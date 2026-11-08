//! Bounded, in-memory clipboard history.
//!
//! Retained content stays memory-only; published snapshots expose metadata, and
//! content leaves only through bounded requests or copying an entry back.

use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use kestrel_core::{ClipboardPasteKeys, UrlRules, clean_url, single_http_link};
use kestrel_platform::clipboard::{
    ChangeWatchCancel, ClipboardBackend, ClipboardEntryKind, ClipboardError, ClipboardKindSupport,
    ClipboardProvider, ClipboardWatchMode, OwnedSelection, PrivacyEventSource, SessionPrivacyEvent,
    kind_from_types, png_dimensions, temporary_write,
};
use kestrel_platform::pin_store::{self, PinStoreError, SavedPin};
use kestrel_platform::secret_service::{PinKeyError, PinKeyStore};
use kestrel_platform::snippets::InsertionBackend;
use zeroize::Zeroizing;

use crate::clipboard_pins::{PIN_RETRY_INTERVAL, PinPersistence};
use crate::paste::{PasteObservation, PasteOperation, PasteSettings, PasteStage};

pub use crate::clipboard_pins::{
    PIN_STORE_REMEDY, PinSaveState, PinStoreState, PinStoreStatus, PinWipeReport,
};

pub const DEFAULT_MAX_ITEM_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_ENTRIES: usize = 64;
pub const DEFAULT_MAX_ITEMS: usize = 100;
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Upper bound on a blocked event wait; real deadlines (selection clear,
/// retention expiry, pin load retry, paste staging) schedule earlier wakes.
pub const DEFAULT_SAFETY_INTERVAL: Duration = Duration::from_secs(60);
pub const DEFAULT_IMAGE_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_HISTORY_ITEMS: usize = 1000;
pub const MAX_PREVIEW_BYTES: usize = 4096;
pub const MAX_MATCH_PREVIEW_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPolicy {
    pub max_item_bytes: usize,
    pub max_image_bytes: usize,
    pub max_file_entries: usize,
    pub max_items: usize,
    pub max_total_bytes: usize,
    pub max_age: Duration,
    pub poll_interval: Duration,
    pub image_poll_interval: Duration,
    /// Clears only the live selection after Kestrel takes it over.
    pub selection_clear_after: Option<Duration>,
    pub filter_sensitive: bool,
    pub paste_plain_text: bool,
    /// The keystroke a paste sends; content never reaches the provider.
    pub paste_keys: ClipboardPasteKeys,
    /// Replaces a single copied link with its cleaned form.
    pub clean_links: bool,
    /// The same local rules the manual preview uses.
    pub url_rules: UrlRules,
    /// Keeps pinned text entries in an encrypted store across restarts.
    pub keep_pinned: bool,
    /// How long a pinned entry is kept in memory and in the store.
    pub pinned_max_age: Duration,
    /// The encrypted store's file; present while saving pins is enabled.
    pub pin_file: Option<PathBuf>,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            max_item_bytes: DEFAULT_MAX_ITEM_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_file_entries: DEFAULT_MAX_FILE_ENTRIES,
            max_items: DEFAULT_MAX_ITEMS,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_age: DEFAULT_MAX_AGE,
            poll_interval: DEFAULT_POLL_INTERVAL,
            image_poll_interval: DEFAULT_IMAGE_POLL_INTERVAL,
            selection_clear_after: None,
            filter_sensitive: false,
            paste_plain_text: true,
            paste_keys: ClipboardPasteKeys::CtrlV,
            clean_links: false,
            url_rules: UrlRules::default(),
            keep_pinned: false,
            pinned_max_age: Duration::from_secs(90 * 24 * 60 * 60),
            pin_file: None,
        }
    }
}

impl ClipboardPolicy {
    pub fn validate(self) -> Result<Self, ClipboardServiceError> {
        if self.max_item_bytes == 0
            || self.max_image_bytes == 0
            || self.max_file_entries == 0
            || self.max_items == 0
            || self.max_total_bytes == 0
            || self.max_age.is_zero()
            || self.poll_interval.is_zero()
            || self.image_poll_interval.is_zero()
        {
            return Err(ClipboardServiceError::InvalidPolicy(
                "clipboard limits and polling intervals must be non-zero".to_string(),
            ));
        }
        if self.max_items > MAX_HISTORY_ITEMS {
            return Err(ClipboardServiceError::InvalidPolicy(format!(
                "the item bound must not exceed {MAX_HISTORY_ITEMS}"
            )));
        }
        if self.max_item_bytes > self.max_total_bytes || self.max_image_bytes > self.max_total_bytes
        {
            return Err(ClipboardServiceError::InvalidPolicy(
                "a single entry bound must not exceed the total byte bound".to_string(),
            ));
        }
        if self
            .selection_clear_after
            .is_some_and(|interval| interval.is_zero())
        {
            return Err(ClipboardServiceError::InvalidPolicy(
                "the automatic selection clear interval must be non-zero".to_string(),
            ));
        }
        if self.pinned_max_age.is_zero() {
            return Err(ClipboardServiceError::InvalidPolicy(
                "the saved-pin age must be non-zero; unlimited retention is not supported"
                    .to_string(),
            ));
        }
        if self.keep_pinned && self.pin_file.is_none() {
            return Err(ClipboardServiceError::InvalidPolicy(
                "keeping pinned entries across restarts needs a store path".to_string(),
            ));
        }
        Ok(self)
    }
}

impl ClipboardPolicy {
    pub fn from_configuration(
        configuration: &kestrel_core::ClipboardConfiguration,
        url_cleanup: &kestrel_core::UrlCleanupConfiguration,
    ) -> Self {
        let defaults = Self::default();
        Self {
            max_item_bytes: configuration.max_item_bytes as usize,
            max_image_bytes: configuration.max_image_bytes as usize,
            max_file_entries: configuration.max_file_entries as usize,
            max_items: configuration.max_items as usize,
            max_total_bytes: configuration.max_total_bytes as usize,
            max_age: Duration::from_secs(u64::from(configuration.max_age_hours) * 3600),
            poll_interval: defaults.poll_interval,
            image_poll_interval: defaults.image_poll_interval,
            selection_clear_after: (configuration.clear_seconds > 0)
                .then(|| Duration::from_secs(configuration.clear_seconds)),
            filter_sensitive: configuration.filter_sensitive,
            paste_plain_text: configuration.paste_plain_text,
            paste_keys: configuration.paste_keys,
            clean_links: url_cleanup.automatic,
            url_rules: UrlRules::from_configuration(url_cleanup),
            keep_pinned: configuration.keep_pinned,
            pinned_max_age: Duration::from_secs(
                u64::from(configuration.pinned_max_age_days) * 24 * 60 * 60,
            ),
            pin_file: None,
        }
    }
}

/// A sensitive-content heuristic; false positives are intentional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensitivePattern {
    pub id: &'static str,
    pub description: &'static str,
}

pub const SENSITIVE_PATTERNS: [SensitivePattern; 6] = [
    SensitivePattern {
        id: "private_key_block",
        description: "a PEM private-key block header",
    },
    SensitivePattern {
        id: "aws_access_key",
        description: "an AKIA-prefixed access key identifier",
    },
    SensitivePattern {
        id: "bearer_token",
        description: "a Bearer token of at least 20 characters",
    },
    SensitivePattern {
        id: "password_assignment",
        description: "a password assignment with a value of at least 6 characters",
    },
    SensitivePattern {
        id: "long_base64_token",
        description: "a mixed-case base64/hex run of at least 48 characters",
    },
    SensitivePattern {
        id: "credential_assignment",
        description: "an assignment to a credential name, such as SECRET_KEY, api_key, or one ending in token, with a value of at least 6 characters",
    },
];

pub fn match_sensitive(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        return Some(SENSITIVE_PATTERNS[0].id);
    }
    if has_case_sensitive_token_after(text, "AKIA", 16) {
        return Some(SENSITIVE_PATTERNS[1].id);
    }
    if has_token_after(
        &text.to_ascii_lowercase(),
        "bearer ",
        20,
        is_secret_token_byte,
    ) {
        return Some(SENSITIVE_PATTERNS[2].id);
    }
    if has_password_assignment(text) {
        return Some(SENSITIVE_PATTERNS[3].id);
    }
    if has_credential_assignment(text) {
        return Some(SENSITIVE_PATTERNS[5].id);
    }
    if has_long_mixed_token(text) {
        return Some(SENSITIVE_PATTERNS[4].id);
    }
    None
}

fn is_secret_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

fn has_case_sensitive_token_after(text: &str, prefix: &str, min_length: usize) -> bool {
    has_token_after(text, prefix, min_length, |byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit()
    })
}

fn has_token_after(
    text: &str,
    prefix: &str,
    min_length: usize,
    is_token_byte: impl Fn(u8) -> bool,
) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(found) = text[index..].find(prefix) {
        let start = index + found + prefix.len();
        let run = bytes[start..]
            .iter()
            .take_while(|byte| is_token_byte(**byte))
            .count();
        if run >= min_length {
            return true;
        }
        index = start;
        if index >= text.len() {
            break;
        }
    }
    false
}

fn has_password_assignment(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let mut index = 0;
    while let Some(found) = lowered[index..].find("password") {
        let start = index + found + "password".len();
        let window_end = (start + 32).min(lowered.len());
        let window = &lowered[start..window_end];
        if let Some(separator) = window.find(['=', ':']) {
            let value = window[separator + 1..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            if value.len() >= 6 {
                return true;
            }
        }
        index = start;
        if index >= lowered.len() {
            break;
        }
    }
    false
}

/// Shortest assigned value treated as a credential, as for passwords.
const MIN_CREDENTIAL_VALUE: usize = 6;

fn has_credential_assignment(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        matches!(byte, b'=' | b':')
            && is_assignment_separator(bytes, index)
            && is_credential_name(assigned_name(&text[..index]))
            && assigned_value(&text[index + 1..]).chars().count() >= MIN_CREDENTIAL_VALUE
    })
}

/// Rejects comparisons (`==`, `!=`, `<=`, `>=`), paths (`::`), and URL
/// schemes (`https://`); `:=` counts through its `=`.
fn is_assignment_separator(bytes: &[u8], index: usize) -> bool {
    let previous = index.checked_sub(1).map(|at| bytes[at]);
    let next = bytes.get(index + 1).copied();
    match bytes[index] {
        b'=' => next != Some(b'=') && !matches!(previous, Some(b'=' | b'!' | b'<' | b'>')),
        _ => !matches!(next, Some(b':' | b'=' | b'/')) && previous != Some(b':'),
    }
}

/// The identifier just before a separator, past spaces, quotes, and the `:` of `:=`.
fn assigned_name(before: &str) -> &str {
    let trimmed = before.trim_end_matches(|character: char| {
        character.is_whitespace() || matches!(character, '"' | '\'' | ':')
    });
    let start = trimmed
        .rfind(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
        })
        .map_or(0, |at| at + 1);
    &trimmed[start..]
}

/// The value just after a separator, without its opening quote.
fn assigned_value(after: &str) -> &str {
    let value = after
        .trim_start_matches([' ', '\t'])
        .trim_start_matches(['"', '\'']);
    let end = value
        .find(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | ',' | ';')
        })
        .unwrap_or(value.len());
    &value[..end]
}

/// Credential names hold `secret`, `passwd`, or `passphrase`, a private or API
/// key, or end in `token`; `max_tokens` and `token_count` stay ordinary.
fn is_credential_name(name: &str) -> bool {
    let words = name_words(name);
    let pair = |first: &str, second: &str| {
        words
            .windows(2)
            .any(|window| window[0] == first && window[1] == second)
    };
    words
        .iter()
        .any(|word| matches!(word.as_str(), "secret" | "passwd" | "passphrase" | "apikey"))
        || pair("private", "key")
        || pair("api", "key")
        || words.last().is_some_and(|word| word == "token")
}

/// Lowercase words of an identifier split at separators, camelCase and
/// acronym boundaries, and digit runs: `APIKey2` is `api`, `key`, `2`.
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name.split(|character: char| !character.is_ascii_alphanumeric()) {
        let characters = part.as_bytes();
        let mut start = 0;
        for index in 1..characters.len() {
            let (previous, current) = (characters[index - 1], characters[index]);
            let next_is_lower = characters
                .get(index + 1)
                .is_some_and(u8::is_ascii_lowercase);
            let boundary = (current.is_ascii_uppercase()
                && (previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_ascii_uppercase() && next_is_lower)))
                || current.is_ascii_digit() != previous.is_ascii_digit();
            if boundary {
                words.push(part[start..index].to_ascii_lowercase());
                start = index;
            }
        }
        if start < part.len() {
            words.push(part[start..].to_ascii_lowercase());
        }
    }
    words
}

fn has_long_mixed_token(text: &str) -> bool {
    let mut run = 0usize;
    let mut has_digit = false;
    let mut has_upper = false;
    let mut has_lower = false;
    for character in text.chars() {
        let is_token =
            character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '=' | '-' | '_');
        if is_token {
            run += 1;
            has_digit |= character.is_ascii_digit();
            has_upper |= character.is_ascii_uppercase();
            has_lower |= character.is_ascii_lowercase();
            continue;
        }
        if run >= 48 && has_digit && has_upper && has_lower {
            return true;
        }
        run = 0;
        has_digit = false;
        has_upper = false;
        has_lower = false;
    }
    run >= 48 && has_digit && has_upper && has_lower
}

/// Removes ANSI escape sequences and trailing whitespace for plain-text pasting.
pub fn plain_text(text: &str) -> Zeroizing<String> {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b {
            index += 1;
            match bytes.get(index) {
                Some(b'[') => {
                    index += 1;
                    while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                        index += 1;
                    }
                    index += 1;
                }
                Some(b']') => {
                    index += 1;
                    while index < bytes.len() && bytes[index] != 0x07 && bytes[index] != 0x1b {
                        index += 1;
                    }
                    if bytes.get(index) == Some(&0x1b) {
                        index += 1;
                    }
                    index += 1;
                }
                _ => {}
            }
            continue;
        }
        let character = text[index..].chars().next().unwrap_or_default();
        output.push(character);
        index += character.len_utf8();
    }
    let trimmed = output.trim_end();
    Zeroizing::new(trimmed.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardLifecycle {
    Stopped,
    Running,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItemMetadata {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub size_bytes: usize,
    pub age: Duration,
    pub pinned: bool,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
    /// How this entry relates to the encrypted pin store.
    pub pin_save: PinSaveState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub lifecycle: ClipboardLifecycle,
    pub provider: Option<ClipboardProvider>,
    pub kind_support: ClipboardKindSupport,
    /// Whether the worker reacts to change events or polls the selection.
    pub watch_mode: ClipboardWatchMode,
    pub items: Vec<ClipboardItemMetadata>,
    pub total_bytes: usize,
    pub pinned_items: usize,
    pub rejected_oversize_items: u64,
    pub filtered_sensitive_items: u64,
    /// Count of automatic live-selection clears.
    pub selection_clears: u64,
    pub wipe_count: u64,
    pub last_filtered_pattern: Option<&'static str>,
    pub last_error: Option<String>,
    /// The active or last paste, for the window's status line.
    pub paste: Option<PasteStage>,
    /// Why paste is unavailable, from provider discovery.
    pub paste_unavailable: Option<String>,
    /// Whether the backend can list offered types, so temporary writes are
    /// recognised.
    pub temporary_writes: TemporaryWriteState,
    /// Count of writes skipped because they were marked temporary.
    pub skipped_temporary_writes: u64,
    /// State and size of the encrypted pin store; metadata only.
    pub pin_store: PinStoreStatus,
}

/// What the backend can say about a write's offered types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporaryWriteState {
    /// Offered types are listed and temporary markers are honoured.
    Detected,
    /// The backend cannot list types, so every write is captured.
    Undetectable,
}

impl ClipboardSnapshot {
    fn stopped() -> Self {
        Self {
            lifecycle: ClipboardLifecycle::Stopped,
            provider: None,
            kind_support: ClipboardKindSupport::TEXT_ONLY,
            watch_mode: ClipboardWatchMode::Polling,
            items: Vec::new(),
            total_bytes: 0,
            pinned_items: 0,
            rejected_oversize_items: 0,
            filtered_sensitive_items: 0,
            selection_clears: 0,
            wipe_count: 0,
            last_filtered_pattern: None,
            last_error: None,
            paste: None,
            paste_unavailable: None,
            temporary_writes: TemporaryWriteState::Undetectable,
            skipped_temporary_writes: 0,
            pin_store: PinStoreStatus::new(PinStoreState::Disabled),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardMatch {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewFormat {
    Plain,
    /// A JSON object or array laid out one value per line.
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPreview {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub text: String,
    pub truncated: bool,
    pub format: PreviewFormat,
    pub size_bytes: usize,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
}

/// Content handed out for one action; `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Unlogged<T: zeroize::Zeroize>(Zeroizing<T>);

impl<T: zeroize::Zeroize> Unlogged<T> {
    pub fn new(value: T) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: zeroize::Zeroize> fmt::Debug for Unlogged<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Unlogged(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardCommand {
    Refresh,
    /// Copies an entry back, normalizing text when requested.
    CopyItem {
        item_id: u64,
        plain_text: bool,
    },
    /// Copies an entry, hides the window, and sends one paste keystroke.
    PasteItem {
        item_id: u64,
        plain_text: bool,
    },
    DeleteItems {
        item_ids: Vec<u64>,
    },
    SetPinned {
        item_id: u64,
        pinned: bool,
    },
    ReplaceText {
        item_id: u64,
        text: String,
    },
    /// Clears the live selection; retained entries stay.
    ClearSelection,
    /// Clears the live selection and all retained entries.
    Wipe,
    /// Removes entries up to `through_id`, so entries captured after a
    /// confirmation prompt opened survive; `keep_pinned` keeps pinned ones.
    /// A full clear also releases the live selection when it holds a removed entry.
    ClearEntries {
        through_id: u64,
        keep_pinned: bool,
    },
    /// Puts text on the clipboard without retaining it, such as text read from an image.
    CopyUnstoredText(Unlogged<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardServiceError {
    InvalidPolicy(String),
    AlreadyRunning,
    NotRunning,
    UnknownItem {
        item_id: u64,
    },
    WrongKind {
        item_id: u64,
        kind: ClipboardEntryKind,
    },
    PlainTextUnavailable {
        kind: ClipboardEntryKind,
    },
    Backend(ClipboardError),
    /// A paste is still waiting; only one runs at a time.
    PasteInProgress,
    /// No usable insertion provider, with the discovery reason.
    PasteUnavailable(String),
    /// The encrypted pin store failed.
    PinStore(PinStoreError),
    /// The Secret Service key store failed.
    PinKey(PinKeyError),
    WorkerStopped,
    WorkerTimedOut,
}

impl fmt::Display for ClipboardServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for ClipboardServiceError {}

enum WorkerCommand {
    Execute(
        ClipboardCommand,
        Sender<Result<ClipboardSnapshot, ClipboardServiceError>>,
    ),
    SetPolicy {
        policy: ClipboardPolicy,
        response: Sender<Result<ClipboardSnapshot, ClipboardServiceError>>,
    },
    Query(
        ClipboardQuery,
        Sender<Result<ClipboardQueryResult, ClipboardServiceError>>,
    ),
    /// A fresh observation of focus and modifier state; no response is awaited.
    PasteObservation(PasteObservation),
    /// The backend's change watch reported a selection change.
    SelectionChanged,
    /// The privacy watch reported a session state change.
    PrivacyChanged,
    /// The privacy watch stopped working; the worker returns to polling.
    PrivacyWatchFailed,
    /// The change watch stopped working; the worker returns to polling.
    WatchFailed(String),
    /// Replaces the insertion provider after discovery or a preference change.
    SetPasteProvider {
        provider: Option<Box<dyn InsertionBackend>>,
        unavailable: Option<String>,
    },
    Stop(Sender<()>),
}

/// An explicit, bounded content request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardQuery {
    Search {
        query: String,
        limit: usize,
    },
    Preview {
        item_id: u64,
        max_bytes: usize,
    },
    /// The PNG of an image entry, for local text recognition.
    ImagePng {
        item_id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardQueryResult {
    Matches(Vec<ClipboardMatch>),
    Preview(ClipboardPreview),
    ImagePng(Unlogged<Vec<u8>>),
}

/// The pin-store thread and the status used while it is absent.
#[derive(Clone)]
struct PinSession {
    persistence: Arc<Mutex<Option<PinPersistence>>>,
    fallback: Arc<Mutex<PinStoreStatus>>,
}

impl PinSession {
    fn disabled() -> Self {
        Self {
            persistence: Arc::new(Mutex::new(None)),
            fallback: Arc::new(Mutex::new(PinStoreStatus::new(PinStoreState::Disabled))),
        }
    }

    fn status(&self) -> PinStoreStatus {
        match self
            .persistence
            .lock()
            .expect("pin persistence mutex is not poisoned")
            .as_ref()
        {
            Some(persistence) => persistence.status(),
            None => self
                .fallback
                .lock()
                .expect("pin fallback mutex is not poisoned")
                .clone(),
        }
    }

    fn current(&self) -> Option<PinPersistence> {
        self.persistence
            .lock()
            .expect("pin persistence mutex is not poisoned")
            .clone()
    }

    fn replace(&self, persistence: Option<PinPersistence>, status: PinStoreStatus) {
        *self
            .persistence
            .lock()
            .expect("pin persistence mutex is not poisoned") = persistence;
        *self
            .fallback
            .lock()
            .expect("pin fallback mutex is not poisoned") = status;
    }

    fn request_load(&self) {
        if let Some(persistence) = self.current() {
            persistence.request_load();
        }
    }

    fn take_loaded(&self) -> Option<Vec<SavedPin>> {
        self.current()
            .and_then(|persistence| persistence.take_loaded())
    }

    fn take_resave(&self) -> bool {
        self.current()
            .is_some_and(|persistence| persistence.take_resave())
    }
}

pub struct ClipboardHistoryService {
    policy: Mutex<ClipboardPolicy>,
    sender: Option<Sender<WorkerCommand>>,
    worker: Option<JoinHandle<()>>,
    watches: Vec<WatchJoin>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
    pin_key_store: Option<Arc<dyn PinKeyStore>>,
    pins: PinSession,
}

/// A watch thread and the handle that interrupts it.
struct WatchJoin {
    cancel: ChangeWatchCancel,
    handle: JoinHandle<()>,
}

impl WatchJoin {
    fn finish(self) {
        self.cancel.cancel();
        let _ = self.handle.join();
    }
}

impl ClipboardHistoryService {
    pub fn new(policy: ClipboardPolicy) -> Result<Self, ClipboardServiceError> {
        Ok(Self {
            policy: Mutex::new(policy.validate()?),
            sender: None,
            worker: None,
            watches: Vec::new(),
            snapshot: Arc::new(Mutex::new(ClipboardSnapshot::stopped())),
            pin_key_store: None,
            pins: PinSession::disabled(),
        })
    }

    /// Installs the key store used when saving pins is enabled.
    pub fn set_pin_key_store(&mut self, key_store: Arc<dyn PinKeyStore>) {
        self.pin_key_store = Some(key_store);
    }

    pub fn start<B, P>(
        &mut self,
        mut backend: B,
        mut privacy: P,
    ) -> Result<(), ClipboardServiceError>
    where
        B: ClipboardBackend,
        P: PrivacyEventSource,
    {
        if self.worker.is_some() {
            return Err(ClipboardServiceError::AlreadyRunning);
        }
        let policy = self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned")
            .clone();
        self.rebuild_pin_session(&policy);
        let (sender, receiver) = mpsc::channel();
        let mut watches = Vec::new();
        let mut watch_mode = ClipboardWatchMode::Polling;
        match backend.take_change_watch() {
            Ok(Some(mut watch)) => {
                let sender = sender.clone();
                let cancel = watch.cancel_handle();
                match thread::Builder::new()
                    .name("kestrel-clipboard-watch".to_string())
                    .spawn(move || {
                        while let Ok(true) = watch.next_change() {
                            if sender.send(WorkerCommand::SelectionChanged).is_err() {
                                return;
                            }
                        }
                        let _ = sender.send(WorkerCommand::WatchFailed(
                            "the clipboard change watch stopped unexpectedly".to_string(),
                        ));
                    }) {
                    Ok(handle) => {
                        watches.push(WatchJoin { cancel, handle });
                        watch_mode = ClipboardWatchMode::Events;
                    }
                    Err(error) => {
                        log::warn!(
                            target: kestrel_platform::clipboard::FEATURE_ID,
                            "clipboard change watch thread failed to start: {error}"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::debug!(
                    target: kestrel_platform::clipboard::FEATURE_ID,
                    "clipboard change events are unavailable: {error}"
                );
            }
        }
        let mut privacy_watch = false;
        if let Ok(privacy_watches) = privacy.take_watches() {
            for mut watch in privacy_watches {
                let sender = sender.clone();
                let cancel = watch.cancel_handle();
                match thread::Builder::new()
                    .name("kestrel-clipboard-privacy".to_string())
                    .spawn(move || {
                        if !watch.run(&mut || sender.send(WorkerCommand::PrivacyChanged).is_ok()) {
                            let _ = sender.send(WorkerCommand::PrivacyWatchFailed);
                        }
                    }) {
                    Ok(handle) => {
                        watches.push(WatchJoin { cancel, handle });
                        privacy_watch = true;
                    }
                    Err(error) => {
                        log::warn!(
                            target: kestrel_platform::clipboard::FEATURE_ID,
                            "clipboard privacy watch thread failed to start: {error}"
                        );
                    }
                }
            }
        }
        let snapshot = Arc::clone(&self.snapshot);
        let pins = self.pins.clone();
        let worker_policy = policy.clone();
        let worker = match thread::Builder::new()
            .name("kestrel-clipboard-history".to_string())
            .spawn(move || {
                run_worker(
                    worker_policy,
                    backend,
                    privacy,
                    WatchSetup {
                        clipboard: watch_mode,
                        privacy: privacy_watch,
                    },
                    receiver,
                    snapshot,
                    pins,
                );
            }) {
            Ok(worker) => worker,
            Err(error) => {
                for watch in watches {
                    watch.finish();
                }
                return Err(ClipboardServiceError::InvalidPolicy(format!(
                    "failed to start clipboard worker: {error}"
                )));
            }
        };
        self.sender = Some(sender);
        self.worker = Some(worker);
        self.watches = watches;
        Ok(())
    }

    /// Creates the pin thread when saving is enabled and its key store exists.
    fn rebuild_pin_session(&self, policy: &ClipboardPolicy) {
        if !policy.keep_pinned {
            let previous = self.pins.current();
            self.pins
                .replace(None, PinStoreStatus::new(PinStoreState::Disabled));
            if let Some(previous) = previous {
                previous.shutdown();
            }
            return;
        }
        let (Some(path), Some(key_store)) = (policy.pin_file.clone(), self.pin_key_store.clone())
        else {
            let previous = self.pins.current();
            self.pins
                .replace(None, PinStoreStatus::new(PinStoreState::Unavailable));
            if let Some(previous) = previous {
                previous.shutdown();
            }
            return;
        };
        match PinPersistence::start(path, key_store) {
            Ok(persistence) => {
                let previous = self.pins.current();
                self.pins
                    .replace(Some(persistence), PinStoreStatus::new(PinStoreState::Idle));
                if let Some(previous) = previous {
                    previous.shutdown();
                }
            }
            Err(_) => {
                let previous = self.pins.current();
                self.pins
                    .replace(None, PinStoreStatus::new(PinStoreState::Failed));
                if let Some(previous) = previous {
                    previous.shutdown();
                }
            }
        }
    }

    pub fn latest(&self) -> ClipboardSnapshot {
        self.snapshot
            .lock()
            .expect("clipboard snapshot mutex is not poisoned")
            .clone()
    }

    /// The policy the worker currently applies.
    pub fn policy(&self) -> ClipboardPolicy {
        self.policy
            .lock()
            .expect("clipboard policy mutex is not poisoned")
            .clone()
    }

    /// Applies focus and modifier observations while a paste waits.
    pub fn observe_paste(&self, observation: PasteObservation) {
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.send(WorkerCommand::PasteObservation(observation));
        }
    }

    /// Installs the insertion provider a paste sends through, or why it has none.
    pub fn set_paste_provider(
        &self,
        provider: Option<Box<dyn InsertionBackend>>,
        unavailable: Option<String>,
    ) {
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.send(WorkerCommand::SetPasteProvider {
                provider,
                unavailable,
            });
        }
    }

    pub fn execute(
        &self,
        command: ClipboardCommand,
    ) -> Result<ClipboardSnapshot, ClipboardServiceError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(ClipboardServiceError::NotRunning)?;
        let (response_sender, response_receiver) = mpsc::channel();
        sender
            .send(WorkerCommand::Execute(command, response_sender))
            .map_err(|_| ClipboardServiceError::WorkerStopped)?;
        response_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ClipboardServiceError::WorkerTimedOut,
                RecvTimeoutError::Disconnected => ClipboardServiceError::WorkerStopped,
            })?
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ClipboardMatch>, ClipboardServiceError> {
        match self.query(ClipboardQuery::Search {
            query: query.to_string(),
            limit,
        })? {
            ClipboardQueryResult::Matches(matches) => Ok(matches),
            _ => Err(ClipboardServiceError::WorkerStopped),
        }
    }

    /// Returns one bounded preview for an explicit request.
    pub fn preview(
        &self,
        item_id: u64,
        max_bytes: usize,
    ) -> Result<ClipboardPreview, ClipboardServiceError> {
        match self.query(ClipboardQuery::Preview { item_id, max_bytes })? {
            ClipboardQueryResult::Preview(preview) => Ok(preview),
            _ => Err(ClipboardServiceError::WorkerStopped),
        }
    }

    /// The PNG of an image entry, for one recognition request.
    pub fn image_png(&self, item_id: u64) -> Result<Unlogged<Vec<u8>>, ClipboardServiceError> {
        match self.query(ClipboardQuery::ImagePng { item_id })? {
            ClipboardQueryResult::ImagePng(png) => Ok(png),
            _ => Err(ClipboardServiceError::WorkerStopped),
        }
    }

    /// Applies a policy while retaining existing entries.
    pub fn set_policy(
        &self,
        policy: ClipboardPolicy,
    ) -> Result<ClipboardSnapshot, ClipboardServiceError> {
        let policy = policy.validate()?;
        let previous = self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned")
            .clone();
        let pin_policy_changed =
            previous.keep_pinned != policy.keep_pinned || previous.pin_file != policy.pin_file;
        if pin_policy_changed {
            self.rebuild_pin_session(&policy);
        }
        *self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned") = policy.clone();
        let sender = self
            .sender
            .as_ref()
            .ok_or(ClipboardServiceError::NotRunning)?;
        let (response_sender, response_receiver) = mpsc::channel();
        sender
            .send(WorkerCommand::SetPolicy {
                policy,
                response: response_sender,
            })
            .map_err(|_| ClipboardServiceError::WorkerStopped)?;
        response_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ClipboardServiceError::WorkerTimedOut,
                RecvTimeoutError::Disconnected => ClipboardServiceError::WorkerStopped,
            })?
    }

    /// Deletes the store's file and, with `reset`, its keyring item.
    pub fn wipe_saved_pins(&self, reset: bool) -> Result<PinWipeReport, ClipboardServiceError> {
        if let Some(persistence) = self.pins.current() {
            let report = persistence
                .wipe()
                .map_err(ClipboardServiceError::PinStore)?;
            if reset {
                persistence.request_resave();
            }
            return Ok(report);
        }
        let policy = self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned")
            .clone();
        if let Some(path) = policy.pin_file.as_ref() {
            pin_store::delete_pins(path).map_err(ClipboardServiceError::PinStore)?;
        }
        let key_error = self
            .pin_key_store
            .as_ref()
            .and_then(|key_store| key_store.delete().err());
        Ok(PinWipeReport {
            saved_count: 0,
            saved_bytes: 0,
            key_removed: key_error.is_none(),
            key_error,
        })
    }

    fn query(&self, query: ClipboardQuery) -> Result<ClipboardQueryResult, ClipboardServiceError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(ClipboardServiceError::NotRunning)?;
        let (response_sender, response_receiver) = mpsc::channel();
        sender
            .send(WorkerCommand::Query(query, response_sender))
            .map_err(|_| ClipboardServiceError::WorkerStopped)?;
        response_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ClipboardServiceError::WorkerTimedOut,
                RecvTimeoutError::Disconnected => ClipboardServiceError::WorkerStopped,
            })?
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let (done_sender, done_receiver) = mpsc::channel();
            let _ = sender.send(WorkerCommand::Stop(done_sender));
            let _ = done_receiver.recv_timeout(Duration::from_secs(2));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        for watch in self.watches.drain(..) {
            watch.finish();
        }
        if let Some(persistence) = self.pins.current() {
            persistence.shutdown();
        }
        self.pins
            .replace(None, PinStoreStatus::new(PinStoreState::Disabled));
    }
}

impl Drop for ClipboardHistoryService {
    fn drop(&mut self) {
        self.stop();
    }
}

struct StoredItem {
    id: u64,
    captured_at: Instant,
    pinned: bool,
    /// When the entry was pinned; `None` while unpinned.
    pinned_at: Option<Instant>,
    content: ClipboardEntryContent,
}

enum ClipboardEntryContent {
    Text(Zeroizing<String>),
    ImagePng {
        bytes: Zeroizing<Vec<u8>>,
        dimensions: Option<(u32, u32)>,
    },
    Files(Zeroizing<Vec<String>>),
}

impl ClipboardEntryContent {
    fn kind(&self) -> ClipboardEntryKind {
        match self {
            Self::Text(_) => ClipboardEntryKind::Text,
            Self::ImagePng { .. } => ClipboardEntryKind::Image,
            Self::Files(_) => ClipboardEntryKind::Files,
        }
    }

    fn size_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::ImagePng { bytes, .. } => bytes.len(),
            Self::Files(paths) => paths.iter().map(String::len).sum(),
        }
    }

    fn image_dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Self::ImagePng { dimensions, .. } => *dimensions,
            _ => None,
        }
    }

    fn file_count(&self) -> Option<usize> {
        match self {
            Self::Files(paths) => Some(paths.len()),
            _ => None,
        }
    }

    fn same_content(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(left), Self::Text(right)) => left.as_str() == right.as_str(),
            (Self::ImagePng { bytes: left, .. }, Self::ImagePng { bytes: right, .. }) => {
                left.as_slice() == right.as_slice()
            }
            (Self::Files(left), Self::Files(right)) => left.as_slice() == right.as_slice(),
            _ => false,
        }
    }

    fn to_owned_selection(&self) -> OwnedSelection {
        match self {
            Self::Text(text) => OwnedSelection::Text(text.to_string()),
            Self::ImagePng { bytes, .. } => OwnedSelection::ImagePng(bytes.to_vec()),
            Self::Files(paths) => OwnedSelection::Files(paths.to_vec()),
        }
    }
}

/// Last-seen selection used for change detection.
#[derive(Clone)]
enum ObservedSelection {
    Text(Zeroizing<String>),
    ImagePng(Zeroizing<Vec<u8>>),
    Files(Zeroizing<Vec<String>>),
}

impl ObservedSelection {
    fn from_content(content: &ClipboardEntryContent) -> Self {
        match content {
            ClipboardEntryContent::Text(text) => Self::Text(text.clone()),
            ClipboardEntryContent::ImagePng { bytes, .. } => Self::ImagePng(bytes.clone()),
            ClipboardEntryContent::Files(paths) => Self::Files(paths.clone()),
        }
    }

    fn from_owned(owned: &OwnedSelection) -> Self {
        match owned {
            OwnedSelection::Text(text) => Self::Text(Zeroizing::new(text.clone())),
            OwnedSelection::ImagePng(bytes) => Self::ImagePng(Zeroizing::new(bytes.clone())),
            OwnedSelection::Files(paths) => Self::Files(Zeroizing::new(paths.clone())),
        }
    }

    fn matches_content(&self, content: &ClipboardEntryContent) -> bool {
        match (self, content) {
            (Self::Text(observed), ClipboardEntryContent::Text(text)) => {
                observed.as_str() == text.as_str()
            }
            (Self::ImagePng(observed), ClipboardEntryContent::ImagePng { bytes, .. }) => {
                observed.as_slice() == bytes.as_slice()
            }
            (Self::Files(observed), ClipboardEntryContent::Files(paths)) => {
                observed.as_slice() == paths.as_slice()
            }
            _ => false,
        }
    }
}

struct HistoryStore {
    policy: ClipboardPolicy,
    items: VecDeque<StoredItem>,
    next_id: u64,
    rejected_oversize_items: u64,
    filtered_sensitive_items: u64,
    selection_clears: u64,
    wipe_count: u64,
    skipped_temporary_writes: u64,
    last_filtered_pattern: Option<&'static str>,
    /// Bumped whenever the set of pinned entries changes.
    pin_revision: u64,
}

impl HistoryStore {
    fn new(policy: ClipboardPolicy) -> Self {
        Self {
            policy,
            items: VecDeque::new(),
            next_id: 1,
            rejected_oversize_items: 0,
            filtered_sensitive_items: 0,
            selection_clears: 0,
            wipe_count: 0,
            skipped_temporary_writes: 0,
            last_filtered_pattern: None,
            pin_revision: 0,
        }
    }

    fn capture(&mut self, content: ClipboardEntryContent, now: Instant) -> CaptureOutcome {
        self.prune(now);
        if content.size_bytes() == 0 {
            return CaptureOutcome::Empty;
        }
        if let Some(pattern) = self.sensitive_pattern(&content) {
            self.filtered_sensitive_items = self.filtered_sensitive_items.saturating_add(1);
            self.last_filtered_pattern = Some(pattern);
            return CaptureOutcome::Filtered(pattern);
        }
        if !self.within_bounds(&content) {
            self.rejected_oversize_items = self.rejected_oversize_items.saturating_add(1);
            return CaptureOutcome::Oversize;
        }
        if self
            .items
            .back()
            .is_some_and(|item| item.content.same_content(&content))
        {
            return CaptureOutcome::Duplicate;
        }
        let id = self.next_id;
        self.items.push_back(StoredItem {
            id,
            captured_at: now,
            pinned: false,
            pinned_at: None,
            content,
        });
        self.next_id = self.next_id.saturating_add(1);
        self.enforce_bounds();
        CaptureOutcome::Stored(id)
    }

    fn sensitive_pattern(&self, content: &ClipboardEntryContent) -> Option<&'static str> {
        if !self.policy.filter_sensitive {
            return None;
        }
        match content {
            ClipboardEntryContent::Text(text) => match_sensitive(text),
            _ => None,
        }
    }

    fn within_bounds(&self, content: &ClipboardEntryContent) -> bool {
        match content {
            ClipboardEntryContent::Text(text) => text.len() <= self.policy.max_item_bytes,
            ClipboardEntryContent::ImagePng { bytes, .. } => {
                bytes.len() <= self.policy.max_image_bytes
            }
            ClipboardEntryContent::Files(paths) => {
                paths.len() <= self.policy.max_file_entries
                    && paths.iter().map(String::len).sum::<usize>() <= self.policy.max_item_bytes
            }
        }
    }

    /// Checks a returning selection like a fresh capture and promotes the entry
    /// that already holds it, without storing a duplicate.
    fn restore_returned(&mut self, content: &ClipboardEntryContent, now: Instant) -> ReturnOutcome {
        self.prune(now);
        if content.size_bytes() == 0 {
            return ReturnOutcome::Empty;
        }
        if let Some(pattern) = self.sensitive_pattern(content) {
            self.filtered_sensitive_items = self.filtered_sensitive_items.saturating_add(1);
            self.last_filtered_pattern = Some(pattern);
            return ReturnOutcome::Filtered;
        }
        if !self.within_bounds(content) {
            self.rejected_oversize_items = self.rejected_oversize_items.saturating_add(1);
            return ReturnOutcome::Oversize;
        }
        let Some(item_id) = self
            .items
            .iter()
            .rev()
            .find(|item| item.content.same_content(content))
            .map(|item| item.id)
        else {
            return ReturnOutcome::Missing;
        };
        self.promote(item_id, now);
        ReturnOutcome::Promoted
    }

    /// Evicts oldest unpinned entries while enforcing both bounds; returns
    /// whether a pinned entry had to go.
    fn enforce_bounds(&mut self) -> bool {
        let mut removed_pinned = false;
        while self.items.len() > self.policy.max_items
            || self.total_bytes() > self.policy.max_total_bytes
        {
            let index = self.items.iter().position(|item| !item.pinned).unwrap_or(0);
            let removed = self.items.remove(index);
            if removed.is_some_and(|item| item.pinned) {
                removed_pinned = true;
            }
        }
        if removed_pinned {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        removed_pinned
    }

    /// Age pruning drops old unpinned entries and pins past the saved-pin
    /// age; returns whether a pinned entry expired.
    fn prune(&mut self, now: Instant) -> bool {
        let max_age = self.policy.max_age;
        let pinned_max_age = self.policy.pinned_max_age;
        let mut removed_pinned = false;
        self.items.retain(|item| {
            let keep = match item.pinned_at {
                Some(pinned_at) => now.saturating_duration_since(pinned_at) <= pinned_max_age,
                None => now.saturating_duration_since(item.captured_at) <= max_age,
            };
            if !keep && item.pinned {
                removed_pinned = true;
            }
            keep
        });
        if removed_pinned {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        removed_pinned
    }

    /// The earliest instant at which [`HistoryStore::prune`] drops an entry.
    fn next_expiry(&self) -> Option<Instant> {
        self.items
            .iter()
            .filter_map(|item| match item.pinned_at {
                Some(pinned_at) => pinned_at.checked_add(self.policy.pinned_max_age),
                None => item.captured_at.checked_add(self.policy.max_age),
            })
            .min()
    }

    fn total_bytes(&self) -> usize {
        self.items
            .iter()
            .map(|item| item.content.size_bytes())
            .sum()
    }

    /// The pins to write, in history order, after every global and pin bound.
    fn saved_pins(&self, now: Instant) -> Vec<SavedPin> {
        let now_unix = unix_seconds();
        let mut total = 0usize;
        let mut saved = Vec::new();
        for item in &self.items {
            if !item.pinned || !self.pin_within_bounds(item, now) {
                continue;
            }
            let ClipboardEntryContent::Text(text) = &item.content else {
                continue;
            };
            if saved.len() >= self.policy.max_items {
                break;
            }
            if total.saturating_add(text.len()) > self.policy.max_total_bytes {
                continue;
            }
            total += text.len();
            let pinned_at = item.pinned_at.unwrap_or(item.captured_at);
            let age = now.saturating_duration_since(pinned_at).as_secs();
            saved.push(SavedPin::new(text.as_str(), now_unix.saturating_sub(age)));
        }
        saved
    }

    /// Whether one entry may be written to the store.
    fn pin_within_bounds(&self, item: &StoredItem, now: Instant) -> bool {
        let ClipboardEntryContent::Text(text) = &item.content else {
            return false;
        };
        if text.len() > self.policy.max_item_bytes {
            return false;
        }
        let pinned_at = item.pinned_at.unwrap_or(item.captured_at);
        now.saturating_duration_since(pinned_at) <= self.policy.pinned_max_age
    }

    /// How one entry should be labelled against the store's current state.
    fn pin_save_state(
        &self,
        item: &StoredItem,
        store: &PinStoreStatus,
        now: Instant,
    ) -> PinSaveState {
        if !item.pinned {
            return PinSaveState::NotPinned;
        }
        if !self.policy.keep_pinned {
            return PinSaveState::PersistenceOff;
        }
        if !matches!(item.content, ClipboardEntryContent::Text(_)) {
            return PinSaveState::OnlyText;
        }
        if !self.pin_within_bounds(item, now) {
            let ClipboardEntryContent::Text(text) = &item.content else {
                return PinSaveState::OnlyText;
            };
            if text.len() > self.policy.max_item_bytes {
                return PinSaveState::OverLimit;
            }
            return PinSaveState::Expired;
        }
        match store.state {
            PinStoreState::Saved | PinStoreState::Saving => PinSaveState::Saved,
            PinStoreState::Idle => PinSaveState::Pending,
            PinStoreState::Unavailable => PinSaveState::StoreUnavailable,
            PinStoreState::Locked => PinSaveState::StoreLocked,
            PinStoreState::Unreadable => PinSaveState::StoreUnreadable,
            PinStoreState::ReadFailed => PinSaveState::StoreReadFailed,
            PinStoreState::Held => PinSaveState::StoreHeld,
            PinStoreState::Failed => PinSaveState::StoreFailed,
            PinStoreState::Disabled => PinSaveState::PersistenceOff,
        }
    }

    fn content(&self, item_id: u64) -> Option<&ClipboardEntryContent> {
        self.items
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| &item.content)
    }

    fn delete(&mut self, item_ids: &[u64]) -> usize {
        let before = self.items.len();
        let before_pins = self.items.iter().filter(|item| item.pinned).count();
        self.items.retain(|item| !item_ids.contains(&item.id));
        let after_pins = self.items.iter().filter(|item| item.pinned).count();
        if before_pins != after_pins {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        before - self.items.len()
    }

    /// Moves a reused unpinned entry to the top; pinned entries keep their place.
    fn promote(&mut self, item_id: u64, now: Instant) {
        let Some(index) = self
            .items
            .iter()
            .position(|item| item.id == item_id && !item.pinned)
        else {
            return;
        };
        if let Some(mut item) = self.items.remove(index) {
            item.captured_at = now;
            self.items.push_back(item);
        }
    }

    /// Removes entries with IDs up to `through_id`; returns the removed contents.
    fn clear_through(&mut self, through_id: u64, keep_pinned: bool) -> Vec<StoredItem> {
        let mut removed = Vec::new();
        let mut kept = VecDeque::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            if item.id <= through_id && !(keep_pinned && item.pinned) {
                removed.push(item);
            } else {
                kept.push_back(item);
            }
        }
        self.items = kept;
        if removed.iter().any(|item| item.pinned) {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        removed
    }

    fn set_pinned(
        &mut self,
        item_id: u64,
        pinned: bool,
        now: Instant,
    ) -> Result<(), ClipboardServiceError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
        if item.pinned != pinned {
            item.pinned = pinned;
            item.pinned_at = pinned.then_some(now);
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        self.enforce_bounds();
        Ok(())
    }

    fn replace_text(
        &mut self,
        item_id: u64,
        text: Zeroizing<String>,
        now: Instant,
    ) -> Result<(), ClipboardServiceError> {
        if text.len() > self.policy.max_item_bytes {
            return Err(ClipboardServiceError::InvalidPolicy(format!(
                "edited text exceeds the {} byte item bound",
                self.policy.max_item_bytes
            )));
        }
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
        if item.content.kind() != ClipboardEntryKind::Text {
            return Err(ClipboardServiceError::WrongKind {
                item_id,
                kind: item.content.kind(),
            });
        }
        item.content = ClipboardEntryContent::Text(text);
        item.captured_at = now;
        if item.pinned {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
        Ok(())
    }

    /// Searches retained content; images match only on their kind label.
    fn search(&self, query: &str, limit: usize) -> Vec<ClipboardMatch> {
        let needle = query.trim().to_lowercase();
        let limit = limit.clamp(1, 100);
        self.items
            .iter()
            .rev()
            .filter_map(|item| {
                let preview = match &item.content {
                    ClipboardEntryContent::Text(text) => {
                        if contains_folded(text, &needle) {
                            match_preview(text)
                        } else {
                            return None;
                        }
                    }
                    ClipboardEntryContent::Files(paths) => {
                        if paths.iter().any(|path| contains_folded(path, &needle)) {
                            match_preview(&paths.join(", "))
                        } else {
                            return None;
                        }
                    }
                    ClipboardEntryContent::ImagePng { dimensions, .. } => {
                        let label = format!(
                            "image png {}",
                            dimensions
                                .map(|(width, height)| format!("{width}x{height}"))
                                .unwrap_or_default()
                        );
                        if needle.is_empty() || label.contains(&needle) {
                            label
                        } else {
                            return None;
                        }
                    }
                };
                Some(ClipboardMatch {
                    id: item.id,
                    kind: item.content.kind(),
                    preview,
                })
            })
            .take(limit)
            .collect()
    }

    fn preview(
        &self,
        item_id: u64,
        max_bytes: usize,
    ) -> Result<ClipboardPreview, ClipboardServiceError> {
        let content = self
            .content(item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
        let max_bytes = max_bytes.clamp(1, MAX_PREVIEW_BYTES);
        let size_bytes = content.size_bytes();
        let (text, truncated, format) = match content {
            ClipboardEntryContent::Text(value) => match json_layout(value) {
                Some(pretty) => {
                    let (text, truncated) = bounded_text(&pretty, max_bytes);
                    (text, truncated, PreviewFormat::Json)
                }
                None => {
                    let (text, truncated) = bounded_text(value, max_bytes);
                    (text, truncated, PreviewFormat::Plain)
                }
            },
            ClipboardEntryContent::Files(paths) => {
                let (text, truncated) = bounded_text(&paths.join("\n"), max_bytes);
                (text, truncated, PreviewFormat::Plain)
            }
            ClipboardEntryContent::ImagePng { .. } => (String::new(), false, PreviewFormat::Plain),
        };
        Ok(ClipboardPreview {
            id: item_id,
            kind: content.kind(),
            text,
            truncated,
            format,
            size_bytes,
            image_dimensions: content.image_dimensions(),
            file_count: content.file_count(),
        })
    }

    fn image_png(&self, item_id: u64) -> Result<Unlogged<Vec<u8>>, ClipboardServiceError> {
        match self
            .content(item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?
        {
            ClipboardEntryContent::ImagePng { bytes, .. } => Ok(Unlogged::new(bytes.to_vec())),
            other => Err(ClipboardServiceError::WrongKind {
                item_id,
                kind: other.kind(),
            }),
        }
    }

    fn wipe(&mut self) {
        let had_pins = self.items.iter().any(|item| item.pinned);
        for mut item in self.items.drain(..) {
            match &mut item.content {
                ClipboardEntryContent::Text(text) => text.clear(),
                ClipboardEntryContent::ImagePng { bytes, .. } => bytes.clear(),
                ClipboardEntryContent::Files(paths) => paths.clear(),
            }
        }
        self.wipe_count = self.wipe_count.saturating_add(1);
        if had_pins {
            self.pin_revision = self.pin_revision.saturating_add(1);
        }
    }

    fn snapshot(
        &self,
        lifecycle: ClipboardLifecycle,
        context: SnapshotContext,
        now: Instant,
    ) -> ClipboardSnapshot {
        let items = self
            .items
            .iter()
            .map(|item| ClipboardItemMetadata {
                id: item.id,
                kind: item.content.kind(),
                size_bytes: item.content.size_bytes(),
                age: now.saturating_duration_since(item.captured_at),
                pinned: item.pinned,
                image_dimensions: item.content.image_dimensions(),
                file_count: item.content.file_count(),
                pin_save: self.pin_save_state(item, &context.pin_store, now),
            })
            .collect::<Vec<_>>();
        ClipboardSnapshot {
            lifecycle,
            provider: context.provider,
            kind_support: context.support,
            watch_mode: context.watch_mode,
            total_bytes: items.iter().map(|item| item.size_bytes).sum(),
            pinned_items: items.iter().filter(|item| item.pinned).count(),
            items,
            rejected_oversize_items: self.rejected_oversize_items,
            filtered_sensitive_items: self.filtered_sensitive_items,
            selection_clears: self.selection_clears,
            wipe_count: self.wipe_count,
            last_filtered_pattern: self.last_filtered_pattern,
            last_error: context.last_error,
            paste: context.paste,
            paste_unavailable: context.paste_unavailable,
            temporary_writes: context.temporary_writes,
            skipped_temporary_writes: self.skipped_temporary_writes,
            pin_store: context.pin_store,
        }
    }
}

/// Worker state the store does not own.
struct SnapshotContext {
    provider: Option<ClipboardProvider>,
    support: ClipboardKindSupport,
    watch_mode: ClipboardWatchMode,
    last_error: Option<String>,
    paste: Option<PasteStage>,
    paste_unavailable: Option<String>,
    temporary_writes: TemporaryWriteState,
    pin_store: PinStoreStatus,
}

enum CaptureOutcome {
    Stored(u64),
    Duplicate,
    Filtered(&'static str),
    Oversize,
    Empty,
}

enum ReturnOutcome {
    Promoted,
    Filtered,
    Oversize,
    Empty,
    Missing,
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Case-insensitive containment; ASCII needles match without allocating.
fn contains_folded(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if needle_lower.is_ascii() {
        // ASCII bytes never occur inside multi-byte UTF-8 sequences.
        let needle = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.to_lowercase().contains(needle_lower)
}

/// An object or array laid out one value per line; other text yields `None`.
fn json_layout(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn bounded_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn match_preview(text: &str) -> String {
    let (preview, _) = bounded_text(text, MAX_MATCH_PREVIEW_CHARS * 4);
    preview
        .chars()
        .take(MAX_MATCH_PREVIEW_CHARS)
        .collect::<String>()
        .replace(['\n', '\r'], " ")
}

struct WorkerState<B, P> {
    policy: ClipboardPolicy,
    backend: B,
    privacy: P,
    history: HistoryStore,
    provider: ClipboardProvider,
    support: ClipboardKindSupport,
    /// Whether the worker reacts to change events or polls the selection.
    watch_mode: ClipboardWatchMode,
    /// A change watch event is waiting for a selection read.
    selection_pending: bool,
    /// Logind property changes arrive as signals instead of a periodic read.
    privacy_watch: bool,
    /// The session state must be read after a privacy signal or at startup.
    privacy_poll_due: bool,
    /// Whether offered types can be listed, so temporary writes are recognised.
    type_listing: bool,
    observed: Option<ObservedSelection>,
    owned: Option<OwnedSelection>,
    owned_since: Option<Instant>,
    last_rich_probe: Option<Instant>,
    last_error: Option<String>,
    /// The insertion provider a paste sends through, or why it has none.
    paste_provider: Option<Box<dyn InsertionBackend>>,
    paste_unavailable: Option<String>,
    paste_observation: PasteObservation,
    /// The active or last paste; only one runs at a time.
    paste: Option<PasteOperation>,
    /// Content held before a temporary write, while it may return.
    returned: Option<ReturnWindow>,
    /// The encrypted store shared with the service.
    pins: PinSession,
    /// Whether the store's current contents have been merged.
    pins_loaded: bool,
    /// Whether the store needs to catch up with memory.
    pins_dirty: bool,
    /// The last pin revision written or merged.
    last_pin_revision: u64,
    /// The session is locked; saved pins stay closed until it unlocks.
    session_locked: bool,
    /// The session is preparing for sleep; saved pins stay closed until resume.
    asleep: bool,
    /// Saved pins are waiting for the first privacy poll or a release edge.
    pins_held: bool,
}

/// A selection that may come back after a temporary write replaced it.
struct ReturnWindow {
    content: ObservedSelection,
    deadline: Instant,
}

/// How long a replaced selection may return and still be treated as retained.
const RETURN_WINDOW: Duration = Duration::from_secs(3);

/// How the worker waits for changes.
#[derive(Debug, Clone, Copy)]
struct WatchSetup {
    clipboard: ClipboardWatchMode,
    privacy: bool,
}

fn run_worker<B: ClipboardBackend, P: PrivacyEventSource>(
    policy: ClipboardPolicy,
    backend: B,
    privacy: P,
    watches: WatchSetup,
    receiver: Receiver<WorkerCommand>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
    pins: PinSession,
) {
    let provider = backend.provider();
    let support = backend.kind_support();
    let type_listing = backend.supports_type_listing();
    let history = HistoryStore::new(policy.clone());
    let last_pin_revision = history.pin_revision;
    let mut state = WorkerState {
        policy,
        backend,
        privacy,
        history,
        provider,
        support,
        watch_mode: watches.clipboard,
        selection_pending: false,
        privacy_watch: watches.privacy,
        privacy_poll_due: true,
        type_listing,
        observed: None,
        owned: None,
        owned_since: None,
        last_rich_probe: None,
        last_error: None,
        paste_provider: None,
        paste_unavailable: None,
        paste_observation: PasteObservation::default(),
        paste: None,
        returned: None,
        pins,
        pins_loaded: false,
        pins_dirty: false,
        last_pin_revision,
        session_locked: false,
        asleep: false,
        pins_held: true,
    };
    state.publish(&snapshot, ClipboardLifecycle::Running);
    state.privacy_poll_due = true;
    if state.poll_privacy_if_due(&snapshot) {
        return;
    }

    loop {
        let timeout = state.wait_timeout(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(WorkerCommand::SetPolicy { policy, response }) => {
                let previous = state.policy.clone();
                let pin_target_changed = previous.keep_pinned != policy.keep_pinned
                    || previous.pin_file != policy.pin_file;
                state.history.policy = policy.clone();
                state.policy = policy;
                if pin_target_changed {
                    state.pins_loaded = false;
                    state.pins_dirty = state.policy.keep_pinned && !state.session_held();
                    if state.session_held() {
                        state.pins_held = true;
                    } else {
                        state.pins.request_load();
                    }
                } else if previous.pinned_max_age != state.policy.pinned_max_age {
                    state.pins_dirty = !state.session_held();
                }
                state.history.enforce_bounds();
                state.history.prune(Instant::now());
                state.sync_pin_revision();
                let result = Ok(state.snapshot(ClipboardLifecycle::Running, Instant::now()));
                if let Ok(value) = &result {
                    publish(&snapshot, value.clone());
                }
                let _ = response.send(result);
            }
            Ok(WorkerCommand::Execute(command, response)) => {
                let refresh = matches!(command, ClipboardCommand::Refresh);
                let result = state.execute(command);
                if refresh {
                    state.selection_pending = true;
                }
                if let Ok(value) = &result {
                    publish(&snapshot, value.clone());
                }
                let _ = response.send(result);
            }
            Ok(WorkerCommand::Query(query, response)) => {
                let result = state.query(query);
                let _ = response.send(result);
            }
            Ok(WorkerCommand::PasteObservation(observation)) => {
                state.paste_observation = observation;
            }
            Ok(WorkerCommand::SetPasteProvider {
                provider,
                unavailable,
            }) => {
                state.paste_provider = provider;
                state.paste_unavailable = unavailable;
                state.publish(&snapshot, ClipboardLifecycle::Running);
            }
            Ok(WorkerCommand::SelectionChanged) => {
                state.selection_pending = true;
            }
            Ok(WorkerCommand::PrivacyChanged) => {
                state.privacy_poll_due = true;
            }
            Ok(WorkerCommand::PrivacyWatchFailed) => {
                state.privacy_watch = false;
                state.privacy_poll_due = true;
            }
            Ok(WorkerCommand::WatchFailed(message)) => {
                // The watch is gone, so the worker re-reads on every tick again.
                state.watch_mode = ClipboardWatchMode::Polling;
                state.selection_pending = true;
                state.last_error = Some(message);
            }
            Ok(WorkerCommand::Stop(done)) => {
                state.wipe_owned();
                state.publish(&snapshot, ClipboardLifecycle::Stopped);
                let _ = done.send(());
                break;
            }
            Err(RecvTimeoutError::Disconnected) => {
                state.wipe_owned();
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if state.poll_privacy_if_due(&snapshot) {
            break;
        }
        state.tick();
        if state.watch_mode == ClipboardWatchMode::Polling || state.selection_pending {
            state.selection_pending = false;
            state.poll_selection(Instant::now());
        }
        state.publish(&snapshot, ClipboardLifecycle::Running);
    }
}

impl<B: ClipboardBackend, P: PrivacyEventSource> WorkerState<B, P> {
    /// Reads the session state only when a signal asked for it, or on every
    /// tick when the property watch is unavailable.
    fn poll_privacy_if_due(&mut self, snapshot: &Arc<Mutex<ClipboardSnapshot>>) -> bool {
        if self.privacy_watch && !self.privacy_poll_due {
            return false;
        }
        self.privacy_poll_due = false;
        self.poll_privacy(snapshot)
    }

    fn poll_privacy(&mut self, snapshot: &Arc<Mutex<ClipboardSnapshot>>) -> bool {
        match self.privacy.poll_events() {
            Ok(events) => {
                for event in events {
                    match event {
                        SessionPrivacyEvent::Locked => {
                            self.session_locked = true;
                            self.wipe_owned();
                        }
                        SessionPrivacyEvent::PreparingForSleep => {
                            self.asleep = true;
                            self.wipe_owned();
                        }
                        SessionPrivacyEvent::Unlocked => self.session_locked = false,
                        SessionPrivacyEvent::Resumed => self.asleep = false,
                    }
                }
                self.sync_pin_hold();
                false
            }
            Err(error) => {
                self.last_error = Some(error.message);
                self.wipe_owned();
                self.hold_pins();
                self.publish(snapshot, ClipboardLifecycle::Unavailable);
                true
            }
        }
    }

    fn session_held(&self) -> bool {
        self.session_locked || self.asleep
    }

    /// Keeps saved pins closed during a hold and reopens them once it clears.
    fn sync_pin_hold(&mut self) {
        if self.session_held() {
            self.hold_pins();
        } else if self.pins_held {
            self.pins_held = false;
            self.reload_saved_pins();
        }
    }

    /// Closes saved pins: no queued save, no load, no keyring retry.
    fn hold_pins(&mut self) {
        self.pins_held = true;
        self.pins_loaded = false;
        self.pins_dirty = false;
        self.last_pin_revision = self.history.pin_revision;
        if let Some(persistence) = self.pins.current() {
            persistence.hold();
        }
    }

    fn reload_saved_pins(&mut self) {
        if !self.policy.keep_pinned {
            return;
        }
        self.pins_loaded = false;
        self.pins.request_load();
    }

    /// Merges loaded pins, then writes memory back when it changed.
    fn poll_pins(&mut self, now: Instant) {
        let loaded = self.pins.take_loaded();
        if self.session_held() || self.pins_held {
            return;
        }
        if let Some(loaded) = loaded {
            self.pins_loaded = true;
            if self.merge_saved_pins(loaded, now) {
                self.pins_dirty = true;
            }
        }
        if self.pins.take_resave() {
            self.pins_dirty = true;
        }
    }

    fn merge_saved_pins(&mut self, pins: Vec<SavedPin>, now: Instant) -> bool {
        let now_unix = unix_seconds();
        let mut dropped = false;
        for pin in pins {
            let age = now_unix.saturating_sub(pin.pinned_at_unix());
            let pinned_at = now.checked_sub(Duration::from_secs(age)).unwrap_or(now);
            let text = pin.into_text();
            if text.is_empty()
                || text.len() > self.policy.max_item_bytes
                || now.saturating_duration_since(pinned_at) > self.policy.pinned_max_age
            {
                dropped = true;
                continue;
            }
            let mut found = false;
            for item in self.history.items.iter_mut() {
                if let ClipboardEntryContent::Text(existing) = &item.content {
                    if existing.as_str() == text.as_str() {
                        item.pinned = true;
                        item.pinned_at = Some(pinned_at);
                        found = true;
                        break;
                    }
                }
            }
            if found {
                continue;
            }
            let id = self.history.next_id;
            self.history.next_id = self.history.next_id.saturating_add(1);
            self.history.items.push_back(StoredItem {
                id,
                captured_at: pinned_at,
                pinned: true,
                pinned_at: Some(pinned_at),
                content: ClipboardEntryContent::Text(text),
            });
        }
        if self.history.enforce_bounds() || self.history.prune(now) {
            dropped = true;
        }
        self.last_pin_revision = self.history.pin_revision;
        dropped
    }

    fn sync_pin_revision(&mut self) {
        if self.history.pin_revision != self.last_pin_revision {
            self.last_pin_revision = self.history.pin_revision;
            self.pins_dirty = true;
        }
    }

    fn save_pins(&mut self, now: Instant) {
        if !self.policy.keep_pinned {
            self.pins_dirty = false;
            return;
        }
        if self.session_held() || self.pins_held {
            return;
        }
        if !self.pins_dirty || !self.pins_loaded {
            return;
        }
        if !self.pins.status().state.writable() {
            return;
        }
        let Some(persistence) = self.pins.current() else {
            return;
        };
        persistence.save(self.history.saved_pins(now));
        self.pins_dirty = false;
    }

    /// How long the worker may block before the next timed check.
    ///
    /// Real deadlines win over the safety cap: the automatic selection clear,
    /// the oldest entry's expiry, a pin load retry, and paste staging. The cap
    /// only covers slower maintenance such as pin writes.
    fn wait_timeout(&self, now: Instant) -> Duration {
        if self.watch_mode == ClipboardWatchMode::Polling {
            return self.policy.poll_interval;
        }
        let mut timeout = DEFAULT_SAFETY_INTERVAL;
        if self
            .paste
            .as_ref()
            .is_some_and(|paste| !paste.is_terminal())
        {
            timeout = timeout.min(self.policy.poll_interval);
        }
        if let Some(deadline) = self.selection_clear_deadline() {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        if let Some(deadline) = self.history.next_expiry() {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        if self.policy.keep_pinned
            && !self.pins_held
            && (!self.pins_loaded || self.pins_dirty)
            && self.pins.current().is_some()
        {
            timeout = timeout.min(PIN_RETRY_INTERVAL);
        }
        if !self.privacy_watch {
            // Without property signals the session state keeps the old latency.
            timeout = timeout.min(self.policy.poll_interval);
        }
        timeout
    }

    /// When the automatic selection clear becomes due.
    fn selection_clear_deadline(&self) -> Option<Instant> {
        let interval = self.policy.selection_clear_after?;
        self.owned_since?.checked_add(interval)
    }

    /// Runs maintenance that needs a clock but not a fresh selection.
    fn tick(&mut self) {
        let now = Instant::now();
        self.poll_pins(now);
        self.tick_paste(now);
        self.history.prune(now);
        self.auto_clear_selection(now);
        self.sync_pin_revision();
        self.save_pins(now);
    }

    /// Reads the selection after a change event or on the polling cadence.
    fn poll_selection(&mut self, now: Instant) {
        // One type listing serves both the temporary check and the kind.
        let listed = match self.backend.offered_types() {
            Ok(listed) => listed,
            Err(error) => {
                self.last_error = Some(error.message);
                None
            }
        };
        if let Some(types) = listed.as_deref() {
            if self.marked_temporary(types) {
                self.skip_temporary_write(now);
                return;
            }
        }
        let (offered, enumerated) = match listed.as_deref() {
            Some(types) => (kind_from_types(types), true),
            None => match self.backend.offered_kind() {
                Ok(offered) => (offered, false),
                Err(error) => {
                    self.last_error = Some(error.message);
                    (None, false)
                }
            },
        };
        let kind = match offered {
            Some(kind) if self.support.supports(kind) => Some(kind),
            Some(_) => None,
            // Text is the only kind a backend without type enumeration can carry.
            None if !enumerated && !self.support.image && !self.support.files => {
                Some(ClipboardEntryKind::Text)
            }
            None => None,
        };
        let Some(kind) = kind else {
            if offered.is_none() && (self.support.image || self.support.files) {
                self.observed = None;
            }
            return;
        };

        // Rich payloads use a slower polling cadence.
        if kind != ClipboardEntryKind::Text {
            let due = self.last_rich_poll_due(now);
            if !due {
                return;
            }
            self.last_rich_probe = Some(now);
        }

        let content = match kind {
            ClipboardEntryKind::Text => self.read_text_content(),
            ClipboardEntryKind::Image => self.read_image_content(),
            ClipboardEntryKind::Files => self.read_files_content(),
        };
        let Some(content) = content else {
            return;
        };
        if self.restore_if_returned(&content, now) {
            return;
        }
        if self
            .observed
            .as_ref()
            .is_some_and(|observed| observed.matches_content(&content))
        {
            return;
        }
        if self.policy.clean_links && self.clean_copied_link(&content, now) {
            return;
        }
        let observed = ObservedSelection::from_content(&content);
        match self.history.capture(content, now) {
            CaptureOutcome::Stored(id) => {
                let owned = self
                    .history
                    .content(id)
                    .map(|content| content.to_owned_selection());
                match owned {
                    Some(owned) => match self.write_selection(&owned) {
                        Ok(()) => {
                            self.observed = Some(observed);
                            self.owned = Some(owned);
                            self.owned_since = Some(now);
                            self.last_error = None;
                        }
                        Err(error) => {
                            self.observed = Some(observed);
                            self.last_error = Some(error.message);
                        }
                    },
                    None => self.observed = Some(observed),
                }
            }
            CaptureOutcome::Duplicate => self.observed = Some(observed),
            CaptureOutcome::Filtered(_pattern) => {
                self.observed = Some(observed);
            }
            CaptureOutcome::Oversize | CaptureOutcome::Empty => self.observed = Some(observed),
        }
    }

    /// Whether the offered types mark this write as temporary or generated.
    fn marked_temporary(&mut self, types: &[String]) -> bool {
        let hint = if types
            .iter()
            .any(|mime| mime == kestrel_platform::clipboard::KDE_PASSWORD_HINT_MIME)
        {
            match self.backend.password_manager_hint(64) {
                Ok(hint) => hint,
                Err(error) => {
                    // A hint that cannot be read is treated as secret.
                    self.last_error = Some(error.message);
                    None
                }
            }
        } else {
            None
        };
        temporary_write(types, hint.as_deref())
    }

    /// Skips a temporary write and remembers what it replaced.
    fn skip_temporary_write(&mut self, now: Instant) {
        self.history.skipped_temporary_writes =
            self.history.skipped_temporary_writes.saturating_add(1);
        // Only an explicitly temporary write starts the return window.
        self.returned = self.observed.as_ref().map(|content| ReturnWindow {
            content: content.clone(),
            deadline: now + RETURN_WINDOW,
        });
        self.last_error = None;
    }

    /// Treats a selection that returns within the window as the entry already
    /// retained. Returns whether the selection was handled.
    fn restore_if_returned(&mut self, content: &ClipboardEntryContent, now: Instant) -> bool {
        let Some(window) = self.returned.take() else {
            return false;
        };
        if now >= window.deadline || !window.content.matches_content(content) {
            return false;
        }
        match self.history.restore_returned(content, now) {
            ReturnOutcome::Promoted => {}
            ReturnOutcome::Filtered | ReturnOutcome::Oversize | ReturnOutcome::Empty => {
                self.observed = Some(ObservedSelection::from_content(content));
                return true;
            }
            ReturnOutcome::Missing => {
                // The entry is gone; the returning selection is recorded anew.
                self.observed = None;
                return false;
            }
        }
        let observed = ObservedSelection::from_content(content);
        let owned = content.to_owned_selection();
        match self.write_selection(&owned) {
            Ok(()) => {
                self.observed = Some(observed);
                self.owned = Some(owned);
                self.owned_since = Some(now);
                self.last_error = None;
            }
            Err(error) => {
                self.observed = Some(observed);
                self.last_error = Some(error.message);
            }
        }
        true
    }

    fn last_rich_poll_due(&self, now: Instant) -> bool {
        self.last_rich_probe.is_none_or(|last| {
            now.saturating_duration_since(last) >= self.policy.image_poll_interval
        })
    }

    /// Advances one waiting paste; the check and the keystroke happen here.
    fn tick_paste(&mut self, now: Instant) {
        let Some(paste) = self.paste.as_mut() else {
            return;
        };
        if paste.is_terminal() {
            return;
        }
        let still_owned = self.owned.as_ref() == Some(paste.staged_selection());
        let observation = self.paste_observation;
        let Some(provider) = self.paste_provider.as_mut() else {
            paste.fail(crate::paste::PasteFailure::new(
                None,
                "the insertion provider is no longer available".to_owned(),
            ));
            return;
        };
        paste.advance(
            now,
            observation,
            still_owned,
            &mut self.backend,
            provider.as_mut(),
        );
    }

    fn paste_settings(&self) -> PasteSettings {
        PasteSettings {
            keys: self.policy.paste_keys,
            focus_timeout: crate::paste::DEFAULT_FOCUS_TIMEOUT,
            modifier_timeout: crate::paste::DEFAULT_MODIFIER_TIMEOUT,
            max_image_bytes: self.policy.max_image_bytes,
            max_file_entries: self.policy.max_file_entries,
        }
    }

    fn read_text_content(&mut self) -> Option<ClipboardEntryContent> {
        match self.backend.read_text() {
            Ok(Some(text)) if !text.is_empty() => {
                Some(ClipboardEntryContent::Text(Zeroizing::new(text)))
            }
            Ok(_) => {
                self.observed = None;
                None
            }
            Err(error) => {
                self.last_error = Some(error.message);
                None
            }
        }
    }

    /// Cleans one newly copied link, replaces the selection, and stores only
    /// the cleaned text. Returns whether the selection was handled.
    fn clean_copied_link(&mut self, content: &ClipboardEntryContent, now: Instant) -> bool {
        let ClipboardEntryContent::Text(text) = content else {
            return false;
        };
        let Some(link) = single_http_link(text.as_str()) else {
            return false;
        };
        let Ok(cleaned) = clean_url(link, &self.policy.url_rules) else {
            return false;
        };
        if cleaned.removed.is_empty() {
            return false;
        }
        let cleaned_content = ClipboardEntryContent::Text(Zeroizing::new(cleaned.cleaned.clone()));
        if self.history.sensitive_pattern(&cleaned_content).is_some()
            || !self.history.within_bounds(&cleaned_content)
        {
            return false;
        }
        // A newer copy during the read leaves the newer selection alone.
        match self.backend.read_text() {
            Ok(Some(current)) if current == text.as_str() => {}
            Ok(_) => return true,
            Err(error) => {
                self.last_error = Some(error.message);
                return true;
            }
        }
        match self.history.capture(cleaned_content, now) {
            CaptureOutcome::Stored(_) | CaptureOutcome::Duplicate => {}
            CaptureOutcome::Filtered(_) | CaptureOutcome::Oversize | CaptureOutcome::Empty => {
                return false;
            }
        }
        let removed = cleaned.removed.len();
        let cleaned_text = cleaned.cleaned;
        match self.backend.write_text(&cleaned_text) {
            Ok(()) => {
                self.observed = Some(ObservedSelection::Text(Zeroizing::new(
                    cleaned_text.clone(),
                )));
                self.owned = Some(OwnedSelection::Text(cleaned_text));
                self.owned_since = Some(now);
                self.last_error = None;
                log::debug!(
                    target: "clipboard.history",
                    "cleaned {removed} tracking parameters from a copied link"
                );
            }
            Err(error) => {
                self.observed = Some(ObservedSelection::from_content(content));
                self.last_error = Some(error.message);
            }
        }
        true
    }

    fn read_image_content(&mut self) -> Option<ClipboardEntryContent> {
        match self.backend.read_image_png(self.policy.max_image_bytes) {
            Ok(Some(bytes)) => Some(ClipboardEntryContent::ImagePng {
                dimensions: png_dimensions(&bytes),
                bytes: Zeroizing::new(bytes),
            }),
            Ok(None) => None,
            Err(error) => {
                self.record_read_error(error);
                None
            }
        }
    }

    fn read_files_content(&mut self) -> Option<ClipboardEntryContent> {
        match self.backend.read_file_list(self.policy.max_file_entries) {
            Ok(Some(paths)) if !paths.is_empty() => {
                Some(ClipboardEntryContent::Files(Zeroizing::new(paths)))
            }
            Ok(_) => None,
            Err(error) => {
                self.record_read_error(error);
                None
            }
        }
    }

    /// Oversize selections count as bound rejections.
    fn record_read_error(&mut self, error: ClipboardError) {
        if error.kind == kestrel_platform::clipboard::ClipboardErrorKind::Oversize {
            self.history.rejected_oversize_items =
                self.history.rejected_oversize_items.saturating_add(1);
        } else {
            self.last_error = Some(error.message);
        }
    }

    fn query(&self, query: ClipboardQuery) -> Result<ClipboardQueryResult, ClipboardServiceError> {
        match query {
            ClipboardQuery::Search { query, limit } => Ok(ClipboardQueryResult::Matches(
                self.history.search(&query, limit),
            )),
            ClipboardQuery::Preview { item_id, max_bytes } => self
                .history
                .preview(item_id, max_bytes)
                .map(ClipboardQueryResult::Preview),
            ClipboardQuery::ImagePng { item_id } => self
                .history
                .image_png(item_id)
                .map(ClipboardQueryResult::ImagePng),
        }
    }

    /// Clears only the live selection after the interval elapses.
    fn auto_clear_selection(&mut self, now: Instant) {
        let Some(interval) = self.policy.selection_clear_after else {
            return;
        };
        let Some(since) = self.owned_since else {
            return;
        };
        if now.saturating_duration_since(since) < interval {
            return;
        }
        match self.clear_owned() {
            Ok(true) => {
                self.history.selection_clears = self.history.selection_clears.saturating_add(1);
            }
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.message),
        }
    }

    /// Clears the live selection if Kestrel still owns it.
    fn clear_owned(&mut self) -> Result<bool, ClipboardError> {
        let Some(owned) = self.owned.take() else {
            self.owned_since = None;
            return Ok(false);
        };
        self.owned_since = None;
        let cleared = self.backend.clear_if_matches(&owned)?;
        self.observed = if cleared {
            None
        } else {
            Some(ObservedSelection::from_owned(&owned))
        };
        Ok(cleared)
    }

    fn write_selection(&mut self, owned: &OwnedSelection) -> Result<(), ClipboardError> {
        match owned {
            OwnedSelection::Text(text) => self.backend.write_text(text),
            OwnedSelection::ImagePng(bytes) => self.backend.write_image_png(bytes),
            OwnedSelection::Files(paths) => self.backend.write_file_list(paths),
        }
    }

    /// Releases the owned selection and retained history.
    fn wipe_owned(&mut self) {
        if let Some(owned) = self.owned.take() {
            if let Err(error) = self.backend.clear_if_matches(&owned) {
                self.last_error = Some(error.message);
            }
        }
        self.owned_since = None;
        // Stopping or disabling history clears any pending return.
        self.returned = None;
        self.history.wipe();
        match self.backend.read_text() {
            Ok(Some(text)) => self.observed = Some(ObservedSelection::Text(Zeroizing::new(text))),
            Ok(None) => self.observed = None,
            Err(error) => {
                self.observed = None;
                self.last_error = Some(error.message);
            }
        }
    }

    /// Writes an entry to the clipboard and tracks it for change detection.
    fn stage_item(
        &mut self,
        item_id: u64,
        plain_text: bool,
        now: Instant,
    ) -> Result<OwnedSelection, ClipboardServiceError> {
        let content = self
            .history
            .content(item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
        let normalize = plain_text || self.policy.paste_plain_text;
        // Track the exact payload for ownership and change detection.
        let (observed, owned) = match content {
            ClipboardEntryContent::Text(text) => {
                let payload = if normalize {
                    crate::clipboard::plain_text(text)
                } else {
                    text.clone()
                };
                self.backend
                    .write_text(&payload)
                    .map_err(ClipboardServiceError::Backend)?;
                let owned = OwnedSelection::Text(payload.to_string());
                (ObservedSelection::Text(payload), owned)
            }
            ClipboardEntryContent::Files(paths) => {
                if plain_text {
                    let payload = Zeroizing::new(paths.join("\n"));
                    self.backend
                        .write_text(&payload)
                        .map_err(ClipboardServiceError::Backend)?;
                    let owned = OwnedSelection::Text(payload.to_string());
                    (ObservedSelection::Text(payload), owned)
                } else {
                    self.backend
                        .write_file_list(paths)
                        .map_err(ClipboardServiceError::Backend)?;
                    let owned = OwnedSelection::Files(paths.to_vec());
                    (ObservedSelection::Files(paths.clone()), owned)
                }
            }
            ClipboardEntryContent::ImagePng { bytes, .. } => {
                if plain_text {
                    return Err(ClipboardServiceError::PlainTextUnavailable {
                        kind: ClipboardEntryKind::Image,
                    });
                }
                self.backend
                    .write_image_png(bytes)
                    .map_err(ClipboardServiceError::Backend)?;
                let owned = OwnedSelection::ImagePng(bytes.to_vec());
                (ObservedSelection::ImagePng(bytes.clone()), owned)
            }
        };
        self.observed = Some(observed);
        self.owned = Some(owned.clone());
        self.owned_since = Some(now);
        self.last_error = None;
        self.history.promote(item_id, now);
        Ok(owned)
    }

    fn execute(
        &mut self,
        command: ClipboardCommand,
    ) -> Result<ClipboardSnapshot, ClipboardServiceError> {
        let now = Instant::now();
        match command {
            ClipboardCommand::Refresh => {}
            ClipboardCommand::CopyItem {
                item_id,
                plain_text,
            } => {
                self.stage_item(item_id, plain_text, now)?;
            }
            ClipboardCommand::PasteItem {
                item_id,
                plain_text,
            } => {
                if self
                    .paste
                    .as_ref()
                    .is_some_and(|paste| !paste.is_terminal())
                {
                    return Err(ClipboardServiceError::PasteInProgress);
                }
                if self.paste_provider.is_none() {
                    return Err(ClipboardServiceError::PasteUnavailable(
                        self.paste_unavailable.clone().unwrap_or_else(|| {
                            "no verified insertion provider was found".to_owned()
                        }),
                    ));
                }
                let staged = self.stage_item(item_id, plain_text, now)?;
                self.paste = Some(PasteOperation::staged(
                    item_id,
                    staged,
                    self.paste_settings(),
                    now,
                ));
                self.paste_observation = PasteObservation::default();
            }
            ClipboardCommand::DeleteItems { item_ids } => {
                let ids = item_ids.to_vec();
                if ids.is_empty() {
                    return Err(ClipboardServiceError::UnknownItem { item_id: 0 });
                }
                let removed = self.history.delete(&ids);
                if removed == 0 {
                    return Err(ClipboardServiceError::UnknownItem { item_id: ids[0] });
                }
                self.history.enforce_bounds();
            }
            ClipboardCommand::SetPinned { item_id, pinned } => {
                self.history.set_pinned(item_id, pinned, now)?;
            }
            ClipboardCommand::ReplaceText { item_id, text } => {
                self.history
                    .replace_text(item_id, Zeroizing::new(text), now)?;
            }
            ClipboardCommand::ClearSelection => {
                if self.clear_owned().map_err(ClipboardServiceError::Backend)? {
                    self.history.selection_clears = self.history.selection_clears.saturating_add(1);
                }
            }
            ClipboardCommand::Wipe => {
                self.wipe_owned();
                // Removing every retained entry removes its saved copy too.
                if let Some(persistence) = self.pins.current() {
                    let _ = persistence.wipe();
                }
                self.pins_dirty = false;
            }
            ClipboardCommand::ClearEntries {
                through_id,
                keep_pinned,
            } => {
                let removed = self.history.clear_through(through_id, keep_pinned);
                if !keep_pinned {
                    let holds_removed = self.owned.as_ref().is_some_and(|owned| {
                        let observed = ObservedSelection::from_owned(owned);
                        removed
                            .iter()
                            .any(|item| observed.matches_content(&item.content))
                    });
                    if holds_removed {
                        self.clear_owned().map_err(ClipboardServiceError::Backend)?;
                    }
                    self.history.wipe_count = self.history.wipe_count.saturating_add(1);
                    if let Some(persistence) = self.pins.current() {
                        let _ = persistence.wipe();
                    }
                    self.pins_dirty = false;
                }
                // Removed contents are zeroized as they drop.
                drop(removed);
            }
            ClipboardCommand::CopyUnstoredText(text) => {
                self.backend
                    .write_text(text.expose())
                    .map_err(ClipboardServiceError::Backend)?;
                // Observing the text keeps the next poll from retaining it.
                self.observed = Some(ObservedSelection::Text(Zeroizing::new(
                    text.expose().clone(),
                )));
                self.owned = Some(OwnedSelection::Text(text.expose().clone()));
                self.owned_since = Some(now);
                self.last_error = None;
            }
        }
        self.history.prune(now);
        self.history.enforce_bounds();
        self.sync_pin_revision();
        self.save_pins(now);
        Ok(self.snapshot(ClipboardLifecycle::Running, now))
    }

    fn snapshot(&self, lifecycle: ClipboardLifecycle, now: Instant) -> ClipboardSnapshot {
        self.history.snapshot(
            lifecycle,
            SnapshotContext {
                provider: Some(self.provider),
                support: self.support,
                watch_mode: self.watch_mode,
                last_error: self.last_error.clone(),
                paste: self.paste.as_ref().map(|paste| paste.stage().clone()),
                paste_unavailable: self.paste_unavailable.clone(),
                temporary_writes: if self.type_listing {
                    TemporaryWriteState::Detected
                } else {
                    TemporaryWriteState::Undetectable
                },
                pin_store: self.pin_status(),
            },
            now,
        )
    }

    /// The store view; a privacy hold wins over the store's own state.
    fn pin_status(&self) -> PinStoreStatus {
        let status = self.pins.status();
        if self.policy.keep_pinned && self.session_held() {
            PinStoreStatus {
                state: PinStoreState::Held,
                ..status
            }
        } else {
            status
        }
    }

    fn publish(&self, snapshot: &Arc<Mutex<ClipboardSnapshot>>, lifecycle: ClipboardLifecycle) {
        publish(snapshot, self.snapshot(lifecycle, Instant::now()));
    }
}

fn publish(snapshot: &Arc<Mutex<ClipboardSnapshot>>, value: ClipboardSnapshot) {
    *snapshot
        .lock()
        .expect("clipboard snapshot mutex is not poisoned") = value;
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc::{Receiver, Sender},
        },
        thread,
        time::{Duration, Instant},
    };

    use kestrel_platform::clipboard::{
        ChangeWatchCancel, ClipboardBackend, ClipboardChangeWatch, ClipboardEntryKind,
        ClipboardError, ClipboardErrorKind, ClipboardKindSupport, ClipboardProvider,
        ClipboardWatchMode, OwnedSelection, PrivacyEventSource, SessionPrivacyEvent,
    };
    use kestrel_platform::pin_store::{self, SavedPin};
    use kestrel_platform::secret_service::{
        PIN_KEY_BYTES, PinKey, PinKeyError, PinKeyStore, SecretServiceKeyStore,
    };

    use super::{
        ClipboardCommand, ClipboardHistoryService, ClipboardLifecycle, ClipboardPolicy,
        HistoryStore, MAX_MATCH_PREVIEW_CHARS, PinSaveState, PinStoreState, UrlRules, WorkerState,
        match_sensitive, plain_text,
    };
    use crate::clipboard_pins::{PIN_RETRY_INTERVAL, PinPersistence};

    const PNG_2X2: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6,
        0, 0, 0, 114, 182, 13, 36, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 132, 25, 96, 12, 0, 71, 202, 7, 249, 103, 89, 110, 183, 0, 0, 0, 0, 73, 69, 78, 68,
        174, 66, 96, 130,
    ];

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum FakeSelection {
        Text(String),
        Image(Vec<u8>),
        Files(Vec<String>),
    }

    #[derive(Default)]
    struct FakeState {
        current: Option<FakeSelection>,
        clears: usize,
        writes: Vec<FakeSelection>,
        read_error: Option<ClipboardError>,
        offered: Option<ClipboardEntryKind>,
        /// Offered MIME types when the backend can list them.
        offered_types: Option<Vec<String>>,
        /// The password-manager hint payload, when offered.
        password_hint: Option<String>,
        /// Text reads the worker performed.
        reads: usize,
    }

    #[derive(Clone, Copy)]
    enum WatchStep {
        Changed,
        Stop,
    }

    struct FakeWatchParts {
        receiver: Receiver<WatchStep>,
        cancel: Sender<WatchStep>,
    }

    struct FakeChangeWatch {
        receiver: Receiver<WatchStep>,
        cancel: Sender<WatchStep>,
    }

    impl ClipboardChangeWatch for FakeChangeWatch {
        fn next_change(&mut self) -> Result<bool, ClipboardError> {
            Ok(match self.receiver.recv() {
                Ok(WatchStep::Changed) => true,
                Ok(WatchStep::Stop) | Err(_) => false,
            })
        }

        fn cancel_handle(&self) -> ChangeWatchCancel {
            let cancel = self.cancel.clone();
            ChangeWatchCancel::new(move || {
                let _ = cancel.send(WatchStep::Stop);
            })
        }
    }

    struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
        watch: Option<FakeWatchParts>,
    }

    impl ClipboardBackend for FakeBackend {
        fn provider(&self) -> ClipboardProvider {
            ClipboardProvider::WaylandDataControl
        }

        fn take_change_watch(
            &mut self,
        ) -> Result<Option<Box<dyn ClipboardChangeWatch>>, ClipboardError> {
            Ok(self.watch.take().map(|parts| {
                Box::new(FakeChangeWatch {
                    receiver: parts.receiver,
                    cancel: parts.cancel,
                }) as Box<dyn ClipboardChangeWatch>
            }))
        }

        fn kind_support(&self) -> ClipboardKindSupport {
            ClipboardKindSupport::ALL
        }

        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            state.reads += 1;
            if let Some(error) = state.read_error.take() {
                return Err(error);
            }
            Ok(match state.current.clone() {
                Some(FakeSelection::Text(text)) => Some(text),
                Some(FakeSelection::Files(paths)) => Some(paths.join("\n")),
                Some(FakeSelection::Image(_)) => None,
                None => None,
            })
        }

        fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text(text.to_string()));
            state.writes.push(FakeSelection::Text(text.to_string()));
            Ok(())
        }

        fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
            let state = self.state.lock().expect("fake state");
            if let Some(offered) = state.offered {
                return Ok(Some(offered));
            }
            Ok(match state.current {
                Some(FakeSelection::Text(_)) => Some(ClipboardEntryKind::Text),
                Some(FakeSelection::Image(_)) => Some(ClipboardEntryKind::Image),
                Some(FakeSelection::Files(_)) => Some(ClipboardEntryKind::Files),
                None => None,
            })
        }

        fn supports_type_listing(&self) -> bool {
            self.state
                .lock()
                .expect("fake state")
                .offered_types
                .is_some()
        }

        fn offered_types(&mut self) -> Result<Option<Vec<String>>, ClipboardError> {
            Ok(self.state.lock().expect("fake state").offered_types.clone())
        }

        fn password_manager_hint(
            &mut self,
            max_bytes: usize,
        ) -> Result<Option<String>, ClipboardError> {
            let state = self.state.lock().expect("fake state");
            Ok(state
                .password_hint
                .as_ref()
                .map(|hint| hint.chars().take(max_bytes).collect::<String>()))
        }

        fn read_image_png(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            if let Some(error) = state.read_error.take() {
                return Err(error);
            }
            match state.current.clone() {
                Some(FakeSelection::Image(bytes)) if bytes.len() <= max_bytes => Ok(Some(bytes)),
                Some(FakeSelection::Image(bytes)) => Err(ClipboardError {
                    kind: ClipboardErrorKind::Oversize,
                    message: format!("{} bytes exceed the bound", bytes.len()),
                }),
                _ => Ok(None),
            }
        }

        fn write_image_png(&mut self, bytes: &[u8]) -> Result<(), ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            state.current = Some(FakeSelection::Image(bytes.to_vec()));
            state.writes.push(FakeSelection::Image(bytes.to_vec()));
            Ok(())
        }

        fn read_file_list(
            &mut self,
            max_entries: usize,
        ) -> Result<Option<Vec<String>>, ClipboardError> {
            let state = self.state.lock().expect("fake state");
            match state.current.clone() {
                Some(FakeSelection::Files(paths)) => Ok(Some(
                    paths.into_iter().take(max_entries).collect::<Vec<_>>(),
                )),
                _ => Ok(None),
            }
        }

        fn write_file_list(&mut self, paths: &[String]) -> Result<(), ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            state.current = Some(FakeSelection::Files(paths.to_vec()));
            state.writes.push(FakeSelection::Files(paths.to_vec()));
            Ok(())
        }

        fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            let matches = match (expected, state.current.as_ref()) {
                (OwnedSelection::Text(expected), Some(FakeSelection::Text(current))) => {
                    expected == current
                }
                (OwnedSelection::ImagePng(expected), Some(FakeSelection::Image(current))) => {
                    expected == current
                }
                (OwnedSelection::Files(expected), Some(FakeSelection::Files(current))) => {
                    expected == current
                }
                _ => false,
            };
            if !matches {
                return Ok(false);
            }
            state.current = None;
            state.clears += 1;
            Ok(true)
        }
    }

    struct FakePrivacy {
        events: Arc<Mutex<Vec<SessionPrivacyEvent>>>,
    }

    impl PrivacyEventSource for FakePrivacy {
        fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError> {
            Ok(self.events.lock().expect("fake events").drain(..).collect())
        }
    }

    fn policy() -> ClipboardPolicy {
        ClipboardPolicy {
            max_item_bytes: 32,
            max_image_bytes: 128,
            max_file_entries: 3,
            max_items: 3,
            max_total_bytes: 256,
            max_age: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
            image_poll_interval: Duration::from_millis(5),
            selection_clear_after: None,
            filter_sensitive: false,
            paste_plain_text: false,
            paste_keys: kestrel_core::ClipboardPasteKeys::CtrlV,
            clean_links: false,
            url_rules: UrlRules::default(),
            keep_pinned: false,
            pinned_max_age: Duration::from_secs(90 * 24 * 60 * 60),
            pin_file: None,
        }
    }

    fn new_store() -> HistoryStore {
        HistoryStore::new(policy())
    }

    fn snapshot_of(store: &HistoryStore, now: Instant) -> super::ClipboardSnapshot {
        store.snapshot(
            ClipboardLifecycle::Running,
            super::SnapshotContext {
                provider: None,
                support: ClipboardKindSupport::ALL,
                watch_mode: kestrel_platform::clipboard::ClipboardWatchMode::Polling,
                last_error: None,
                paste: None,
                paste_unavailable: None,
                temporary_writes: super::TemporaryWriteState::Undetectable,
                pin_store: super::PinStoreStatus::new(super::PinStoreState::Disabled),
            },
            now,
        )
    }

    fn text(value: &str) -> super::ClipboardEntryContent {
        super::ClipboardEntryContent::Text(zeroize::Zeroizing::new(value.to_string()))
    }

    #[test]
    fn store_enforces_per_kind_bounds_and_deduplicates() {
        let now = Instant::now();
        let mut store = new_store();

        assert!(matches!(
            store.capture(text("one"), now),
            super::CaptureOutcome::Stored(1)
        ));
        assert!(matches!(
            store.capture(text("one"), now),
            super::CaptureOutcome::Duplicate
        ));
        assert!(matches!(
            store.capture(text("this text is definitely longer than thirty-two"), now),
            super::CaptureOutcome::Oversize
        ));
        assert!(matches!(
            store.capture(text(""), now),
            super::CaptureOutcome::Empty
        ));

        let oversized_image = vec![0u8; 200];
        assert!(matches!(
            store.capture(
                super::ClipboardEntryContent::ImagePng {
                    bytes: zeroize::Zeroizing::new(oversized_image),
                    dimensions: None,
                },
                now
            ),
            super::CaptureOutcome::Oversize
        ));

        let too_many_paths = (0..4).map(|index| format!("/tmp/{index}")).collect();
        assert!(matches!(
            store.capture(
                super::ClipboardEntryContent::Files(zeroize::Zeroizing::new(too_many_paths)),
                now
            ),
            super::CaptureOutcome::Oversize
        ));

        let snapshot = snapshot_of(&store, Instant::now());
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.rejected_oversize_items, 3);
        assert_eq!(snapshot.items[0].kind, ClipboardEntryKind::Text);
    }

    #[test]
    fn store_enforces_count_and_total_byte_bounds_dropping_oldest_first() {
        let now = Instant::now();
        let mut store = new_store();

        for value in ["aaaa", "bbbb", "cccc", "dddd"] {
            store.capture(text(value), now);
        }

        let snapshot = snapshot_of(&store, Instant::now());
        assert_eq!(
            snapshot
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![2, 3, 4],
            "the oldest entry is dropped first"
        );
        assert!(snapshot.total_bytes <= 256);
    }

    #[test]
    fn pinned_entries_survive_pruning_and_eviction_but_not_wipe() {
        let now = Instant::now();
        let mut store = new_store();
        store.capture(text("keep"), now);
        store.set_pinned(1, true, now).expect("pin succeeds");
        store.capture(text("second"), now);
        store.capture(text("third"), now);

        store.prune(now + Duration::from_millis(51));
        assert_eq!(
            store.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1],
            "a pinned entry is exempt from age pruning"
        );

        let mut store = new_store();
        store.capture(text("keep"), now);
        store.set_pinned(1, true, now).expect("pin succeeds");
        for value in ["a", "b", "c", "d"] {
            store.capture(text(value), now);
        }
        let ids = store.items.iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);
        assert!(
            ids.contains(&1),
            "the pinned entry is retained within the bound"
        );

        store.wipe();
        assert!(
            store.items.is_empty(),
            "wipe clears pinned entries as well: lock, sleep, and stop must not leave content behind"
        );
    }

    #[test]
    fn search_matches_text_files_and_image_labels_with_bounded_previews() {
        let now = Instant::now();
        let mut store = new_store();
        store.capture(text("Quarterly Report\nsecond line"), now);
        store.capture(
            super::ClipboardEntryContent::Files(zeroize::Zeroizing::new(vec![
                "/home/user/invoice.pdf".to_string(),
            ])),
            now,
        );
        store.capture(
            super::ClipboardEntryContent::ImagePng {
                bytes: zeroize::Zeroizing::new(PNG_2X2.to_vec()),
                dimensions: super::png_dimensions(PNG_2X2),
            },
            now,
        );

        let matches = store.search("report", 10);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, 1);
        assert_eq!(
            matches[0].preview, "Quarterly Report second line",
            "newlines are flattened in previews"
        );

        let matches = store.search("INVOICE", 10);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, ClipboardEntryKind::Files);

        let matches = store.search("2x2", 10);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, ClipboardEntryKind::Image);

        assert!(store.search("nothing matches this", 10).is_empty());
        assert_eq!(
            store.search("", 10).len(),
            3,
            "an empty query lists the newest entries"
        );
        assert_eq!(store.search("", 1).len(), 1, "the limit is honoured");

        let long = "x".repeat(MAX_MATCH_PREVIEW_CHARS * 2);
        let mut store = HistoryStore::new(ClipboardPolicy {
            max_item_bytes: 4096,
            max_total_bytes: 8192,
            ..policy()
        });
        assert!(matches!(
            store.capture(text(&long), now),
            super::CaptureOutcome::Stored(1)
        ));
        assert_eq!(
            store.search("", 1)[0].preview.chars().count(),
            MAX_MATCH_PREVIEW_CHARS
        );
    }

    #[test]
    fn preview_is_bounded_char_safe_and_reports_unknown_items() {
        let now = Instant::now();
        let mut store = new_store();
        let accented = "é".repeat(16);
        assert_eq!(
            accented.len(),
            32,
            "the fixture sits exactly on the byte bound"
        );
        assert!(matches!(
            store.capture(text(&accented), now),
            super::CaptureOutcome::Stored(1)
        ));

        let preview = store.preview(1, 3).expect("preview succeeds");
        assert!(preview.truncated);
        assert_eq!(
            preview.text.chars().count(),
            1,
            "truncation stays on char boundaries"
        );
        assert!(!preview.text.is_empty());

        let preview = store.preview(1, 4096).expect("preview succeeds");
        assert!(!preview.truncated);
        assert_eq!(preview.size_bytes, 32);
        assert_eq!(
            store.preview(99, 16).expect_err("unknown ids are rejected"),
            super::ClipboardServiceError::UnknownItem { item_id: 99 }
        );
    }

    #[test]
    fn replace_text_only_edits_text_entries_and_refreshes_age() {
        let now = Instant::now();
        let mut store = new_store();
        store.capture(text("before"), now);
        store.capture(
            super::ClipboardEntryContent::ImagePng {
                bytes: zeroize::Zeroizing::new(PNG_2X2.to_vec()),
                dimensions: None,
            },
            now,
        );

        store
            .replace_text(1, zeroize::Zeroizing::new("after".to_string()), now)
            .expect("text edit succeeds");
        assert_eq!(
            store.preview(1, 32).expect("preview").text,
            "after",
            "the edit replaces the retained content"
        );

        assert_eq!(
            store
                .replace_text(2, zeroize::Zeroizing::new("nope".to_string()), now)
                .expect_err("images are not text-editable"),
            super::ClipboardServiceError::WrongKind {
                item_id: 2,
                kind: ClipboardEntryKind::Image
            }
        );

        let too_long = "y".repeat(64);
        assert!(matches!(
            store.replace_text(1, zeroize::Zeroizing::new(too_long), now),
            Err(super::ClipboardServiceError::InvalidPolicy(_))
        ));

        let later = now + Duration::from_millis(20);
        store
            .replace_text(1, zeroize::Zeroizing::new("again".to_string()), later)
            .expect("edit succeeds");
        let snapshot = snapshot_of(&store, later);
        assert_eq!(
            snapshot.items[0].age,
            Duration::ZERO,
            "an edited entry restarts its age"
        );
    }

    #[test]
    fn delete_items_removes_the_selection_and_reports_unknown_ids() {
        let now = Instant::now();
        let mut store = new_store();
        for value in ["one", "two", "three"] {
            store.capture(text(value), now);
        }

        assert_eq!(store.delete(&[1, 3]), 2);
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.delete(&[42]), 0);
    }

    #[test]
    fn sensitive_patterns_cover_documented_heuristics() {
        assert_eq!(
            match_sensitive("-----BEGIN OPENSSH PRIVATE KEY-----\nabc"),
            Some("private_key_block")
        );
        assert_eq!(
            match_sensitive("key = AKIAIOSFODNN7EXAMPLE"),
            Some("aws_access_key")
        );
        assert_eq!(
            match_sensitive("Authorization: Bearer abcdefghijklmnopqrstuvwxyz"),
            Some("bearer_token")
        );
        assert_eq!(
            match_sensitive("password = hunter2secret"),
            Some("password_assignment")
        );
        assert_eq!(
            match_sensitive(&format!(
                "token {}",
                "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4"
            )),
            Some("long_base64_token")
        );

        // 40-character SHAs remain ordinary values; longer digests are secrets.
        assert_eq!(match_sensitive(&"a1b2c3d4e5".repeat(4)), None);
        assert_eq!(
            match_sensitive("A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"),
            Some("long_base64_token")
        );

        assert_eq!(match_sensitive("an ordinary sentence"), None);
        assert_eq!(match_sensitive(""), None);
    }

    #[test]
    fn credential_names_are_recognised_and_ordinary_names_are_not() {
        for text in [
            "SECRET_KEY=django-insecure-abc123",
            "export SECRET_KEY_BASE=9f8e7d6c5b4a",
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG",
            r#"{"secretKey": "s3cr3t-value"}"#,
            "private_key: MIIEvQIBADANBg",
            "GITHUB_TOKEN=ghp_abcdef123456",
            "apiKey := \"abcdef123\"",
        ] {
            assert_eq!(
                match_sensitive(text),
                Some("credential_assignment"),
                "{text}"
            );
        }
        for text in [
            "password_confirmation=hunter2secret",
            "PASSWORD_2: hunter2secret",
        ] {
            assert!(match_sensitive(text).is_some(), "{text}");
        }
        for text in [
            "max_tokens: 4096000",
            "token_count = 1234567",
            "if secret_key == expected_value",
            "see https://example.com/secret_key_rotation",
            "SECRET_KEY=",
            "secret: true",
        ] {
            assert_eq!(match_sensitive(text), None, "{text}");
        }
    }

    #[test]
    fn sensitive_filter_counts_and_retains_nothing() {
        let now = Instant::now();
        let mut policy = policy();
        policy.filter_sensitive = true;
        let mut store = HistoryStore::new(policy);

        assert!(matches!(
            store.capture(text("password = hunter2secret"), now),
            super::CaptureOutcome::Filtered("password_assignment")
        ));
        assert!(store.items.is_empty(), "filtered content is never retained");
        assert!(matches!(
            store.capture(text("ordinary text"), now),
            super::CaptureOutcome::Stored(1)
        ));

        let snapshot = snapshot_of(&store, now);
        assert_eq!(snapshot.filtered_sensitive_items, 1);
        assert_eq!(snapshot.last_filtered_pattern, Some("password_assignment"));
        assert!(
            !format!("{snapshot:?}").contains("hunter2secret"),
            "diagnostics never carry filtered content"
        );
    }

    #[test]
    fn plain_text_strips_escape_sequences_and_trailing_whitespace() {
        for (input, expected) in [
            ("\u{1b}[31mred\u{1b}[0m text  \n\n", "red text"),
            ("\u{1b}]0;title\u{7}body", "body"),
            ("no escapes", "no escapes"),
            ("µnicode ✓  ", "µnicode ✓"),
        ] {
            assert_eq!(plain_text(input).as_str(), expected, "{input:?}");
        }
    }

    fn wait_for_items(service: &ClipboardHistoryService, expected: usize) {
        for _ in 0..200 {
            if service.latest().items.len() == expected {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "clipboard worker did not publish {expected} items: {:?}",
            service.latest()
        );
    }

    type FakeHarness = (
        ClipboardHistoryService,
        Arc<Mutex<FakeState>>,
        Arc<Mutex<Vec<SessionPrivacyEvent>>>,
    );

    fn start_fake(policy: ClipboardPolicy, current: Option<FakeSelection>) -> FakeHarness {
        start_fake_with(
            policy,
            FakeState {
                current,
                ..FakeState::default()
            },
        )
    }

    fn start_fake_with(policy: ClipboardPolicy, state: FakeState) -> FakeHarness {
        let state = Arc::new(Mutex::new(state));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut service = ClipboardHistoryService::new(policy).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                    watch: None,
                },
                FakePrivacy {
                    events: Arc::clone(&events),
                },
            )
            .expect("worker starts");
        (service, state, events)
    }

    type EventFakeHarness = (
        ClipboardHistoryService,
        Arc<Mutex<FakeState>>,
        Sender<WatchStep>,
    );

    fn start_fake_events(policy: ClipboardPolicy, state: FakeState) -> EventFakeHarness {
        let state = Arc::new(Mutex::new(state));
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = ClipboardHistoryService::new(policy).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                    watch: Some(FakeWatchParts {
                        receiver,
                        cancel: sender.clone(),
                    }),
                },
                FakePrivacy {
                    events: Arc::new(Mutex::new(Vec::new())),
                },
            )
            .expect("worker starts");
        (service, state, sender)
    }

    #[test]
    fn change_events_drive_reads_and_the_safety_tick_stays_idle() {
        let (mut service, state, steps) = start_fake_events(policy(), FakeState::default());
        thread::sleep(Duration::from_millis(40));
        assert_eq!(
            state.lock().expect("fake state").reads,
            0,
            "no change event means no selection read"
        );
        assert_eq!(service.latest().watch_mode, ClipboardWatchMode::Events);

        state.lock().expect("fake state").current = Some(FakeSelection::Text("event".to_string()));
        steps.send(WatchStep::Changed).expect("watch receives");
        wait_for_items(&service, 1);
        assert_eq!(
            state.lock().expect("fake state").reads,
            1,
            "one change event reads the selection once"
        );

        thread::sleep(Duration::from_millis(40));
        assert_eq!(
            state.lock().expect("fake state").reads,
            1,
            "the safety tick must not read the selection"
        );
        service.stop();
        assert_eq!(service.latest().lifecycle, ClipboardLifecycle::Stopped);
    }

    #[test]
    fn event_mode_wakes_for_the_selection_clear_deadline() {
        let mut policy = policy();
        // Keep every non-deadline wake far away so only the clear can meet it.
        policy.poll_interval = Duration::from_secs(5);
        policy.selection_clear_after = Some(Duration::from_millis(20));
        let (mut service, state, steps) = start_fake_events(policy, FakeState::default());
        state.lock().expect("fake state").current = Some(FakeSelection::Text("timed".to_string()));
        steps.send(WatchStep::Changed).expect("watch receives");
        wait_for_items(&service, 1);

        let started = Instant::now();
        while state.lock().expect("fake state").current.is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "the automatic clear missed its deadline"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(service.latest().selection_clears, 1);
        service.stop();
    }

    #[test]
    fn worker_captures_and_wipes_owned_content_on_privacy_event() {
        let (mut service, state, events) =
            start_fake(policy(), Some(FakeSelection::Text("secret".to_string())));
        wait_for_items(&service, 1);
        assert_eq!(service.latest().watch_mode, ClipboardWatchMode::Polling);

        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Locked);
        wait_for_items(&service, 0);

        assert!(state.lock().expect("fake state").current.is_none());
        service.stop();
    }

    #[test]
    fn wipe_preserves_a_newer_external_selection() {
        let (mut service, state, _) =
            start_fake(policy(), Some(FakeSelection::Text("first".to_string())));
        wait_for_items(&service, 1);

        state.lock().expect("fake state").current = Some(FakeSelection::Text("newer".to_string()));
        service
            .execute(ClipboardCommand::Wipe)
            .expect("wipe succeeds");

        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("newer".to_string()))
        );
        service.stop();
    }

    #[test]
    fn stop_releases_owned_selection_and_joins_worker() {
        let (mut service, state, _) =
            start_fake(policy(), Some(FakeSelection::Text("owned".to_string())));
        wait_for_items(&service, 1);

        service.stop();

        assert_eq!(service.latest().lifecycle, ClipboardLifecycle::Stopped);
        assert!(state.lock().expect("fake state").current.is_none());
    }

    #[test]
    fn automatic_clear_releases_only_the_live_selection() {
        let mut policy = policy();
        policy.selection_clear_after = Some(Duration::from_millis(20));
        let (mut service, state, _) =
            start_fake(policy, Some(FakeSelection::Text("timed".to_string())));
        wait_for_items(&service, 1);

        let mut cleared = false;
        for _ in 0..200 {
            if state.lock().expect("fake state").current.is_none() {
                cleared = true;
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(cleared, "the automatic clear releases the live selection");

        let snapshot = service.latest();
        assert_eq!(
            snapshot.items.len(),
            1,
            "the automatic clear must not delete saved entries"
        );
        assert_eq!(snapshot.selection_clears, 1);
        assert_eq!(snapshot.lifecycle, ClipboardLifecycle::Running);
        service.stop();
    }

    #[test]
    fn automatic_cleanup_replaces_one_link_and_stores_only_the_cleaned_text() {
        let mut policy = policy();
        policy.max_item_bytes = 4096;
        policy.max_total_bytes = 8192;
        policy.clean_links = true;
        let dirty = "https://www.youtube.com/watch?v=abc&si=share&feature=shared";
        let (mut service, state, _) =
            start_fake(policy, Some(FakeSelection::Text(dirty.to_string())));
        wait_for_items(&service, 1);

        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text(
                "https://www.youtube.com/watch?v=abc".to_string()
            )),
            "the copied link is replaced with the cleaned form"
        );
        let matches = service.search("", 10).expect("search succeeds");
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].preview, "https://www.youtube.com/watch?v=abc",
            "history stores the cleaned version only"
        );
        service.stop();
    }

    #[test]
    fn automatic_cleanup_leaves_lists_and_links_without_tracking_parameters_alone() {
        let mut policy = policy();
        policy.max_item_bytes = 4096;
        policy.max_total_bytes = 8192;
        policy.clean_links = true;
        let list = "https://a.example/?utm_source=x\nhttps://b.example/?utm_source=y";
        let (mut service, state, _) =
            start_fake(policy, Some(FakeSelection::Text(list.to_string())));
        wait_for_items(&service, 1);
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text(list.to_string())),
            "a list is captured unchanged"
        );

        let clean = "https://example.com/?keep=1";
        state.lock().expect("fake state").current = Some(FakeSelection::Text(clean.to_string()));
        wait_for_items(&service, 2);
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text(clean.to_string())),
            "a link without tracking parameters is untouched"
        );
        service.stop();
    }

    #[test]
    fn a_newer_copy_during_cleanup_is_left_untouched() {
        let mut cleaning_policy = policy();
        cleaning_policy.max_item_bytes = 4096;
        cleaning_policy.max_total_bytes = 8192;
        let state = Arc::new(Mutex::new(FakeState {
            current: Some(FakeSelection::Text("newer".to_string())),
            ..FakeState::default()
        }));
        let mut worker = WorkerState {
            policy: cleaning_policy,
            backend: FakeBackend {
                state: Arc::clone(&state),
                watch: None,
            },
            privacy: FakePrivacy {
                events: Arc::new(Mutex::new(Vec::new())),
            },
            history: HistoryStore::new(policy()),
            provider: ClipboardProvider::WaylandDataControl,
            support: ClipboardKindSupport::ALL,
            watch_mode: ClipboardWatchMode::Polling,
            selection_pending: false,
            privacy_watch: false,
            privacy_poll_due: true,
            type_listing: false,
            observed: None,
            owned: None,
            owned_since: None,
            last_rich_probe: None,
            last_error: None,
            paste_provider: None,
            paste_unavailable: None,
            paste_observation: super::PasteObservation::default(),
            paste: None,
            returned: None,
            pins: super::PinSession::disabled(),
            pins_loaded: false,
            pins_dirty: false,
            last_pin_revision: 0,
            session_locked: false,
            asleep: false,
            pins_held: true,
        };

        let stale = text("https://example.com/?utm_source=x");
        assert!(
            worker.clean_copied_link(&stale, Instant::now()),
            "the stale read is still handled"
        );
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("newer".to_string())),
            "the newer selection is not replaced"
        );
        assert!(
            worker.history.items.is_empty(),
            "nothing is stored from the stale read"
        );
    }

    #[test]
    fn manual_selection_clear_keeps_entries_and_wipe_removes_them() {
        let (mut service, state, _) =
            start_fake(policy(), Some(FakeSelection::Text("keep me".to_string())));
        wait_for_items(&service, 1);

        let snapshot = service
            .execute(ClipboardCommand::ClearSelection)
            .expect("clear succeeds");
        assert!(state.lock().expect("fake state").current.is_none());
        assert_eq!(snapshot.items.len(), 1, "entries survive a selection clear");
        assert_eq!(snapshot.selection_clears, 1);

        let snapshot = service
            .execute(ClipboardCommand::Wipe)
            .expect("wipe succeeds");
        assert!(snapshot.items.is_empty());
        assert_eq!(snapshot.wipe_count, 1);
        service.stop();
    }

    #[test]
    fn searching_and_previewing_never_populate_the_snapshot() {
        let (mut service, _state, _) = start_fake(
            policy(),
            Some(FakeSelection::Text("snapshot-secret".to_string())),
        );
        wait_for_items(&service, 1);

        let matches = service.search("secret", 10).expect("search succeeds");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].preview, "snapshot-secret");
        let preview = service
            .preview(matches[0].id, 4096)
            .expect("preview succeeds");
        assert_eq!(preview.text, "snapshot-secret");

        let snapshot = service.latest();
        let debug = format!("{snapshot:?}");
        assert!(
            !debug.contains("snapshot-secret"),
            "the published snapshot and its diagnostics must stay content-free"
        );
        assert_eq!(snapshot.items.len(), 1);
        service.stop();
    }

    #[test]
    fn copying_an_entry_reowns_it_and_plain_text_strips_formatting() {
        let (mut service, state, _) = start_fake(policy(), None);
        state.lock().expect("fake state").current = Some(FakeSelection::Text(
            "\u{1b}[1mbold\u{1b}[0m  \n".to_string(),
        ));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;

        service
            .execute(ClipboardCommand::CopyItem {
                item_id,
                plain_text: true,
            })
            .expect("plain copy succeeds");
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("bold".to_string())),
            "plain-text copying strips ANSI escapes and trailing whitespace"
        );

        service
            .execute(ClipboardCommand::CopyItem {
                item_id,
                plain_text: false,
            })
            .expect("verbatim copy succeeds");
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text(
                "\u{1b}[1mbold\u{1b}[0m  \n".to_string()
            ))
        );
        service.stop();
    }

    #[test]
    fn pinned_entries_survive_a_clear_and_are_reported_in_the_snapshot() {
        let (mut service, _state, _) = start_fake(policy(), None);
        let push = |text: &str| {
            _state.lock().expect("fake state").current =
                Some(FakeSelection::Text(text.to_string()));
        };
        push("first");
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        let snapshot = service
            .execute(ClipboardCommand::SetPinned {
                item_id,
                pinned: true,
            })
            .expect("pin succeeds");
        assert_eq!(snapshot.pinned_items, 1);

        let snapshot = service
            .execute(ClipboardCommand::DeleteItems {
                item_ids: vec![item_id],
            })
            .expect("delete succeeds");
        assert!(snapshot.items.is_empty());
        assert_eq!(snapshot.pinned_items, 0);

        assert_eq!(
            service
                .execute(ClipboardCommand::DeleteItems {
                    item_ids: vec![item_id],
                })
                .expect_err("deleting an unknown entry is reported"),
            super::ClipboardServiceError::UnknownItem { item_id }
        );
        service.stop();
    }

    #[test]
    fn rich_entries_are_captured_reowned_and_bounded() {
        let (mut service, state, _) =
            start_fake(policy(), Some(FakeSelection::Image(PNG_2X2.to_vec())));
        wait_for_items(&service, 1);

        let snapshot = service.latest();
        assert_eq!(snapshot.items[0].kind, ClipboardEntryKind::Image);
        assert_eq!(snapshot.items[0].image_dimensions, Some((2, 2)));
        assert_eq!(
            state.lock().expect("fake state").writes.len(),
            1,
            "a captured image is re-owned so it survives its source application"
        );

        let files = vec!["/tmp/one.txt".to_string(), "/tmp/two.txt".to_string()];
        state.lock().expect("fake state").current = Some(FakeSelection::Files(files.clone()));
        wait_for_items(&service, 2);
        let snapshot = service.latest();
        assert_eq!(snapshot.items[1].kind, ClipboardEntryKind::Files);
        assert_eq!(snapshot.items[1].file_count, Some(2));

        let item_id = snapshot.items[1].id;
        let copied = service
            .execute(ClipboardCommand::CopyItem {
                item_id,
                plain_text: false,
            })
            .expect("file entries copy back exactly");
        assert_eq!(
            copied
                .items
                .iter()
                .map(|item| (item.id, item.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, ClipboardEntryKind::Image),
                (2, ClipboardEntryKind::Files)
            ],
            "copying an entry does not add or remove history"
        );
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Files(files))
        );

        state.lock().expect("fake state").current = Some(FakeSelection::Image(vec![0u8; 512]));
        for _ in 0..200 {
            if service.latest().rejected_oversize_items > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(service.latest().rejected_oversize_items > 0);
        assert!(
            service.latest().last_error.is_none(),
            "oversize is a bound, not a failure"
        );
        service.stop();
    }

    #[test]
    fn plain_text_copy_of_an_image_is_reported_as_unavailable() {
        let (mut service, _state, _) =
            start_fake(policy(), Some(FakeSelection::Image(PNG_2X2.to_vec())));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;

        assert_eq!(
            service
                .execute(ClipboardCommand::CopyItem {
                    item_id,
                    plain_text: true,
                })
                .expect_err("images have no plain-text form"),
            super::ClipboardServiceError::PlainTextUnavailable {
                kind: ClipboardEntryKind::Image
            }
        );
        service.stop();
    }

    #[test]
    fn policy_projects_the_portable_configuration() {
        let configuration = kestrel_core::ClipboardConfiguration {
            max_items: 7,
            max_item_bytes: 4096,
            max_image_bytes: 8192,
            max_file_entries: 5,
            max_total_bytes: 65536,
            max_age_hours: 2,
            clear_seconds: 30,
            filter_sensitive: true,
            paste_plain_text: false,
            history_layout: kestrel_core::ClipboardLayout::Cards,
            paste_keys: kestrel_core::ClipboardPasteKeys::ShiftInsert,
            keep_pinned: false,
            pinned_max_age_days: kestrel_core::DEFAULT_CLIPBOARD_PINNED_MAX_AGE_DAYS,
        };

        let url_cleanup = kestrel_core::UrlCleanupConfiguration {
            automatic: true,
            ..kestrel_core::UrlCleanupConfiguration::default()
        };
        let policy = ClipboardPolicy::from_configuration(&configuration, &url_cleanup);
        assert_eq!(policy.max_items, 7);
        assert_eq!(policy.max_age, Duration::from_secs(7200));
        assert_eq!(policy.selection_clear_after, Some(Duration::from_secs(30)));
        assert!(policy.filter_sensitive);
        assert!(!policy.paste_plain_text);
        assert!(policy.clean_links, "the preference reaches the service");
        assert!(policy.url_rules.global.iter().any(|param| param == "utm_*"));
        assert!(policy.validate().is_ok());

        let zero_clear = ClipboardPolicy::from_configuration(
            &kestrel_core::ClipboardConfiguration {
                clear_seconds: 0,
                ..kestrel_core::ClipboardConfiguration::default()
            },
            &kestrel_core::UrlCleanupConfiguration::default(),
        );
        assert_eq!(
            zero_clear.selection_clear_after, None,
            "a zero interval disables the automatic clear"
        );
    }

    #[test]
    fn set_policy_applies_immediately_without_dropping_entries() {
        let (mut service, _state, _) = start_fake(policy(), None);
        _state.lock().expect("fake state").current = Some(FakeSelection::Text("first".to_string()));
        wait_for_items(&service, 1);

        let mut updated = policy();
        updated.selection_clear_after = Some(Duration::from_millis(10));
        updated.filter_sensitive = true;
        let snapshot = service.set_policy(updated).expect("policy applies");

        assert_eq!(
            snapshot.items.len(),
            1,
            "retained entries survive a policy change"
        );
        assert!(service.latest().items.len() == 1);

        let mut invalid = policy();
        invalid.max_items = super::MAX_HISTORY_ITEMS + 1;
        assert!(matches!(
            service.set_policy(invalid),
            Err(super::ClipboardServiceError::InvalidPolicy(_))
        ));
        service.stop();
    }

    #[test]
    fn policy_validation_rejects_unsafe_bounds() {
        let mut bad = policy();
        bad.max_items = super::MAX_HISTORY_ITEMS + 1;
        assert!(matches!(
            bad.validate(),
            Err(super::ClipboardServiceError::InvalidPolicy(_))
        ));

        let mut bad = policy();
        bad.max_item_bytes = 1024;
        assert!(
            matches!(
                bad.validate(),
                Err(super::ClipboardServiceError::InvalidPolicy(_))
            ),
            "a per-item bound above the total byte bound is rejected"
        );

        let mut bad = policy();
        bad.selection_clear_after = Some(Duration::ZERO);
        assert!(matches!(
            bad.validate(),
            Err(super::ClipboardServiceError::InvalidPolicy(_))
        ));

        assert!(policy().validate().is_ok());
    }

    fn ids(store: &HistoryStore) -> Vec<u64> {
        store.items.iter().map(|item| item.id).collect()
    }

    #[test]
    fn reused_entries_rise_to_the_top_and_pinned_entries_stay_put() {
        let now = Instant::now();
        let mut store = new_store();
        for value in ["first", "second", "third"] {
            store.capture(text(value), now);
        }
        store.promote(1, now);
        assert_eq!(ids(&store), [2, 3, 1], "the reused entry is newest");
        assert_eq!(store.search("", 10)[0].id, 1, "search lists it first");

        store.set_pinned(2, true, now).expect("entry exists");
        store.promote(2, now);
        assert_eq!(ids(&store), [2, 3, 1], "a pinned entry keeps its place");
    }

    #[test]
    fn clearing_through_an_id_keeps_newer_and_optionally_pinned_entries() {
        let now = Instant::now();
        let mut store = new_store();
        for value in ["one", "two", "three"] {
            store.capture(text(value), now);
        }
        store.set_pinned(1, true, now).expect("entry exists");
        let removed = store.clear_through(2, true);
        assert_eq!(removed.len(), 1);
        assert_eq!(ids(&store), [1, 3], "pinned and newer entries remain");
        store.clear_through(3, false);
        assert!(store.items.is_empty());
    }

    #[test]
    fn previews_lay_out_json_and_leave_other_text_alone() {
        let now = Instant::now();
        let mut store = HistoryStore::new(ClipboardPolicy {
            max_item_bytes: 256,
            ..policy()
        });
        store.capture(text(r#"{"tags":[1,true],"name":"kestrel"}"#), now);
        store.capture(text("[1, 2"), now);
        store.capture(text("42"), now);
        let json = store.preview(1, 4096).expect("entry exists");
        assert_eq!(json.format, super::PreviewFormat::Json);
        assert_eq!(
            json.text, "{\n  \"tags\": [\n    1,\n    true\n  ],\n  \"name\": \"kestrel\"\n}",
            "keys keep their copied order"
        );
        for id in [2, 3] {
            let plain = store.preview(id, 4096).expect("entry exists");
            assert_eq!(plain.format, super::PreviewFormat::Plain, "entry {id}");
        }
        let bounded = store.preview(1, 10).expect("entry exists");
        assert!(bounded.truncated && bounded.text.len() <= 10);
    }

    #[test]
    fn case_insensitive_search_handles_non_ascii_text() {
        assert!(super::contains_folded("Größe der DATEI", "datei"));
        assert!(super::contains_folded("GRÖßE", "größe"));
        assert!(!super::contains_folded("Größe", "datei"));
        assert!(super::contains_folded("anything", ""));
    }

    fn long_lived() -> ClipboardPolicy {
        ClipboardPolicy {
            max_age: Duration::from_secs(60),
            ..policy()
        }
    }

    #[test]
    fn a_confirmed_clear_spares_entries_copied_while_the_prompt_was_open() {
        let (mut service, state, _) = start_fake(
            long_lived(),
            Some(FakeSelection::Text("before".to_string())),
        );
        wait_for_items(&service, 1);
        let through_id = service.latest().items[0].id;
        state.lock().expect("fake state").current = Some(FakeSelection::Text("during".into()));
        wait_for_items(&service, 2);

        let snapshot = service
            .execute(ClipboardCommand::ClearEntries {
                through_id,
                keep_pinned: false,
            })
            .expect("clear succeeds");
        assert_eq!(snapshot.items.len(), 1, "the newer entry survives");
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("during".into())),
            "the live clipboard keeps the newer copy"
        );

        let newest = snapshot.items[0].id;
        service
            .execute(ClipboardCommand::ClearEntries {
                through_id: newest,
                keep_pinned: false,
            })
            .expect("clear succeeds");
        assert!(
            state.lock().expect("fake state").current.is_none(),
            "a cleared entry does not stay on the live clipboard"
        );
        service.stop();
    }

    #[test]
    fn copying_back_promotes_and_unstored_text_is_never_retained() {
        let (mut service, state, _) =
            start_fake(long_lived(), Some(FakeSelection::Text("older".to_string())));
        wait_for_items(&service, 1);
        state.lock().expect("fake state").current = Some(FakeSelection::Text("newer".into()));
        wait_for_items(&service, 2);
        let older = service.latest().items[0].id;
        let snapshot = service
            .execute(ClipboardCommand::CopyItem {
                item_id: older,
                plain_text: false,
            })
            .expect("copy succeeds");
        assert_eq!(snapshot.items.last().map(|item| item.id), Some(older));

        let command = ClipboardCommand::CopyUnstoredText(super::Unlogged::new(
            "recognized words".to_string(),
        ));
        assert!(!format!("{command:?}").contains("recognized"));
        service.execute(command).expect("copy succeeds");
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("recognized words".into()))
        );
        thread::sleep(Duration::from_millis(40));
        assert_eq!(service.latest().items.len(), 2, "the text is not retained");
        service.stop();
    }

    fn wait_for(
        service: &ClipboardHistoryService,
        predicate: impl Fn(&super::ClipboardSnapshot) -> bool,
    ) -> super::ClipboardSnapshot {
        for _ in 0..400 {
            let snapshot = service.latest();
            if predicate(&snapshot) {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "clipboard worker did not reach the expected state: {:?}",
            service.latest()
        );
    }

    fn ordinary_types() -> Vec<String> {
        vec!["text/plain;charset=utf-8".to_string()]
    }

    fn hint_types() -> Vec<String> {
        let mut types = ordinary_types();
        types.push(kestrel_platform::clipboard::KDE_PASSWORD_HINT_MIME.to_string());
        types
    }

    fn kept_writes(state: &Arc<Mutex<FakeState>>) -> usize {
        state
            .lock()
            .expect("fake state")
            .writes
            .iter()
            .filter(|write| matches!(write, FakeSelection::Text(text) if text == "kept"))
            .count()
    }

    #[test]
    fn temporary_writes_are_skipped_and_never_read() {
        let (mut service, state, _) = start_fake_with(
            long_lived(),
            FakeState {
                current: Some(FakeSelection::Text("kept".to_string())),
                offered_types: Some(ordinary_types()),
                ..FakeState::default()
            },
        );
        wait_for_items(&service, 1);
        assert_eq!(
            service.latest().temporary_writes,
            super::TemporaryWriteState::Detected
        );

        {
            let mut state = state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text("generated text".to_string()));
            state.offered_types = Some(hint_types());
            state.password_hint = Some("secret".to_string());
        }
        let snapshot = wait_for(&service, |snapshot| snapshot.skipped_temporary_writes == 1);
        assert_eq!(
            snapshot.items.len(),
            1,
            "the temporary write is not retained"
        );
        assert!(
            !format!("{snapshot:?}").contains("generated text"),
            "temporary content never reaches diagnostics"
        );
        assert!(
            !state.lock().expect("fake state").writes.iter().any(
                |write| matches!(write, FakeSelection::Text(text) if text == "generated text")
            ),
            "temporary content is never read or re-owned"
        );

        // The content held before the write returns within the window.
        {
            let mut state = state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text("kept".to_string()));
            state.offered_types = Some(ordinary_types());
        }
        let snapshot = wait_for(&service, |_| kept_writes(&state) >= 2);
        assert_eq!(
            snapshot.items.len(),
            1,
            "a returned entry is not duplicated"
        );
        assert_eq!(snapshot.skipped_temporary_writes, 1);

        // An ordinary copy after the window records normally.
        {
            let mut state = state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text("generated text".to_string()));
        }
        wait_for_items(&service, 2);
        assert_eq!(
            internal_ids(&service.latest()),
            [1, 2],
            "an ordinary copy is retained as a new entry"
        );
        service.stop();
    }

    #[test]
    fn a_persistent_password_hint_and_only_replace_empty_are_honoured() {
        let (mut service, state, _) = start_fake_with(
            long_lived(),
            FakeState {
                current: Some(FakeSelection::Text("ordinary".to_string())),
                offered_types: Some(ordinary_types()),
                ..FakeState::default()
            },
        );
        wait_for_items(&service, 1);

        {
            let mut state = state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text("persistent".to_string()));
            state.offered_types = Some(hint_types());
            state.password_hint = Some("persistent".to_string());
        }
        wait_for_items(&service, 2);

        {
            let mut state = state.lock().expect("fake state");
            state.current = Some(FakeSelection::Text("empty marker".to_string()));
            state.offered_types = Some(vec![
                "text/plain;charset=utf-8".to_string(),
                kestrel_platform::clipboard::KDE_ONLY_REPLACE_EMPTY_MIME.to_string(),
            ]);
            state.password_hint = None;
        }
        let snapshot = wait_for(&service, |snapshot| snapshot.skipped_temporary_writes == 1);
        assert_eq!(
            snapshot.items.len(),
            2,
            "the only-replace-empty marker is not retained"
        );
        service.stop();
    }

    #[test]
    fn a_backend_without_type_listing_reports_it_and_captures_normally() {
        let (mut service, state, _) =
            start_fake(long_lived(), Some(FakeSelection::Text("plain".to_string())));
        wait_for_items(&service, 1);
        assert_eq!(
            service.latest().temporary_writes,
            super::TemporaryWriteState::Undetectable,
            "the feature reports that temporary writes cannot be recognised"
        );

        // An ordinary repeated copy still records normally.
        state.lock().expect("fake state").current = Some(FakeSelection::Text("other".into()));
        wait_for_items(&service, 2);
        service.stop();
    }

    /// IDs of retained entries, oldest first.
    fn internal_ids(snapshot: &super::ClipboardSnapshot) -> Vec<u64> {
        snapshot.items.iter().map(|item| item.id).collect()
    }

    struct FakeProvider {
        keys: Arc<Mutex<Vec<kestrel_core::ClipboardPasteKeys>>>,
    }

    impl kestrel_platform::snippets::InsertionBackend for FakeProvider {
        fn provider(&self) -> kestrel_platform::snippets::InsertionProvider {
            kestrel_platform::snippets::InsertionProvider::Wtype
        }

        fn insert_text(
            &mut self,
            text: &str,
        ) -> Result<(), kestrel_platform::snippets::InsertionError> {
            let _ = text;
            Ok(())
        }

        fn press_keys(
            &mut self,
            keys: kestrel_core::ClipboardPasteKeys,
        ) -> Result<(), kestrel_platform::snippets::InsertionError> {
            self.keys.lock().expect("fake keys").push(keys);
            Ok(())
        }
    }

    #[test]
    fn pasting_stages_waits_for_the_window_and_sends_one_chord() {
        let mut paste_policy = long_lived();
        paste_policy.paste_keys = kestrel_core::ClipboardPasteKeys::CtrlShiftV;
        let (mut service, state, _) = start_fake(
            paste_policy,
            Some(FakeSelection::Text("to paste".to_string())),
        );
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;

        let keys = Arc::new(Mutex::new(Vec::new()));
        service.set_paste_provider(
            Some(Box::new(FakeProvider {
                keys: Arc::clone(&keys),
            })),
            None,
        );

        let snapshot = service
            .execute(ClipboardCommand::PasteItem {
                item_id,
                plain_text: false,
            })
            .expect("paste stages the entry");
        assert!(
            matches!(
                snapshot.paste,
                Some(super::PasteStage::Staged | super::PasteStage::WaitingForFocusLoss)
            ),
            "the entry is staged before the window hides: {:?}",
            snapshot.paste
        );
        assert!(
            keys.lock().expect("fake keys").is_empty(),
            "nothing is sent while the window holds focus"
        );

        service.observe_paste(super::PasteObservation {
            focus_released: true,
            modifiers_released: true,
        });
        let snapshot = wait_for(&service, |snapshot| {
            snapshot
                .paste
                .as_ref()
                .is_some_and(|paste| paste.is_terminal())
        });
        assert_eq!(snapshot.paste, Some(super::PasteStage::Done));
        assert_eq!(
            keys.lock().expect("fake keys").as_slice(),
            [kestrel_core::ClipboardPasteKeys::CtrlShiftV],
            "exactly one keystroke is sent"
        );
        assert_eq!(
            state.lock().expect("fake state").current,
            Some(FakeSelection::Text("to paste".to_string())),
            "the entry stays on the clipboard after the paste"
        );
        service.stop();
    }

    #[test]
    fn pasting_without_a_provider_is_unavailable_and_return_still_copies() {
        let (mut service, _, _) =
            start_fake(policy(), Some(FakeSelection::Text("entry".to_string())));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        service.set_paste_provider(None, Some("no verified insertion provider".to_owned()));
        thread::sleep(Duration::from_millis(20));

        assert_eq!(
            service
                .execute(ClipboardCommand::PasteItem {
                    item_id,
                    plain_text: false,
                })
                .expect_err("paste needs a provider"),
            super::ClipboardServiceError::PasteUnavailable(
                "no verified insertion provider".to_owned()
            )
        );
        service
            .execute(ClipboardCommand::CopyItem {
                item_id,
                plain_text: false,
            })
            .expect("copy needs no provider");
        service.stop();
    }

    #[test]
    fn a_second_paste_is_refused_while_one_waits() {
        let (mut service, _, _) =
            start_fake(policy(), Some(FakeSelection::Text("entry".to_string())));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        let keys = Arc::new(Mutex::new(Vec::new()));
        service.set_paste_provider(Some(Box::new(FakeProvider { keys })), None);

        service
            .execute(ClipboardCommand::PasteItem {
                item_id,
                plain_text: false,
            })
            .expect("first paste stages");
        assert_eq!(
            service
                .execute(ClipboardCommand::PasteItem {
                    item_id,
                    plain_text: false,
                })
                .expect_err("only one paste runs at a time"),
            super::ClipboardServiceError::PasteInProgress
        );
        service.stop();
    }

    #[derive(Default)]
    struct FakeKeyStore {
        key: Mutex<Option<PinKey>>,
        locked: Mutex<bool>,
        unavailable: Mutex<bool>,
        deletes: AtomicUsize,
        reads: AtomicUsize,
    }

    impl FakeKeyStore {
        fn with_key(byte: u8) -> Self {
            Self {
                key: Mutex::new(Some(
                    PinKey::from_bytes(&[byte; PIN_KEY_BYTES]).expect("exact length"),
                )),
                ..Self::default()
            }
        }

        fn set_locked(&self, locked: bool) {
            *self.locked.lock().expect("fake key store") = locked;
        }

        fn set_unavailable(&self, unavailable: bool) {
            *self.unavailable.lock().expect("fake key store") = unavailable;
        }
    }

    impl PinKeyStore for FakeKeyStore {
        fn get(&self) -> Result<Option<PinKey>, PinKeyError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if *self.unavailable.lock().expect("fake key store") {
                return Err(PinKeyError::Unavailable);
            }
            if *self.locked.lock().expect("fake key store") {
                return Err(PinKeyError::Locked);
            }
            Ok(self.key.lock().expect("fake key store").clone())
        }

        fn create(&self) -> Result<PinKey, PinKeyError> {
            if *self.unavailable.lock().expect("fake key store") {
                return Err(PinKeyError::Unavailable);
            }
            if *self.locked.lock().expect("fake key store") {
                return Err(PinKeyError::Locked);
            }
            let key = PinKey::generate().map_err(|_| PinKeyError::Failed)?;
            *self.key.lock().expect("fake key store") = Some(key.clone());
            Ok(key)
        }

        fn delete(&self) -> Result<bool, PinKeyError> {
            if *self.unavailable.lock().expect("fake key store") {
                return Err(PinKeyError::Unavailable);
            }
            if *self.locked.lock().expect("fake key store") {
                return Err(PinKeyError::Locked);
            }
            self.deletes.fetch_add(1, Ordering::Relaxed);
            Ok(self.key.lock().expect("fake key store").take().is_some())
        }
    }

    fn pin_policy(path: PathBuf) -> ClipboardPolicy {
        ClipboardPolicy {
            keep_pinned: true,
            pinned_max_age: Duration::from_secs(90 * 24 * 60 * 60),
            pin_file: Some(path),
            max_item_bytes: 64,
            max_total_bytes: 256,
            ..policy()
        }
    }

    type PinHarness = (
        ClipboardHistoryService,
        Arc<Mutex<FakeState>>,
        Arc<Mutex<Vec<SessionPrivacyEvent>>>,
    );

    fn start_pin_service(policy: ClipboardPolicy, key_store: Arc<dyn PinKeyStore>) -> PinHarness {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut service = ClipboardHistoryService::new(policy).expect("valid policy");
        service.set_pin_key_store(key_store);
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                    watch: None,
                },
                FakePrivacy {
                    events: Arc::clone(&events),
                },
            )
            .expect("worker starts");
        (service, state, events)
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("condition was not met before the timeout");
    }

    #[test]
    fn saved_pins_round_trip_through_the_key_store() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(FakeKeyStore::default());
        let shared: Arc<dyn PinKeyStore> = store.clone();

        let first = PinPersistence::start(path.clone(), shared.clone()).expect("pin worker starts");
        wait_until(|| first.status().state == PinStoreState::Idle);
        first.save(vec![
            SavedPin::new("first saved", 1_700_000_000),
            SavedPin::new("second saved", 1_700_000_100),
        ]);
        wait_until(|| first.status().state == PinStoreState::Saved && path.exists());
        assert_eq!(first.status().saved_count, 2);
        assert!(first.status().saved_bytes > 0, "bytes are reported");
        first.shutdown();

        let second = PinPersistence::start(path, shared).expect("pin worker starts");
        // A finished read nobody merged is dropped by a hold.
        second.request_load();
        wait_until(|| second.status().state == PinStoreState::Saved);
        second.hold();
        thread::sleep(PIN_RETRY_INTERVAL);
        assert!(
            second.take_loaded().is_none(),
            "a hold must drop an unmerged read"
        );

        second.request_load();
        let mut loaded = None;
        wait_until(|| {
            if let Some(pins) = second.take_loaded() {
                loaded = Some(pins);
                true
            } else {
                false
            }
        });
        let loaded = loaded.expect("a load completed");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text(), "first saved");
        assert_eq!(loaded[1].text(), "second saved");
        assert_eq!(second.status().saved_count, 2);

        // Commands run in order, so this read finishes before the wipe starts.
        second.request_load();
        second.wipe().expect("wipe succeeds");
        assert!(
            second.take_loaded().is_none(),
            "a wipe must drop an unmerged read of the deleted pins"
        );
        second.shutdown();
    }

    #[test]
    fn a_wrong_key_or_truncated_file_is_unreadable_and_exposes_no_content() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let writer = PinKey::from_bytes(&[1; PIN_KEY_BYTES]).expect("exact length");
        pin_store::write_pins(&path, &writer, &[SavedPin::new("never shown", 10)])
            .expect("encrypted write");

        let store = Arc::new(FakeKeyStore::with_key(2));
        let shared: Arc<dyn PinKeyStore> = store;
        let persistence = PinPersistence::start(path.clone(), shared).expect("pin worker starts");
        persistence.request_load();
        wait_until(|| persistence.status().state == PinStoreState::Unreadable);
        assert!(persistence.take_loaded().is_none());
        let status = format!("{:?}", persistence.status());
        assert!(!status.contains("never shown"), "{status}");
        persistence.shutdown();

        fs::write(&path, b"KSTRLPIN\x01short").expect("truncated file");
        let store = Arc::new(FakeKeyStore::with_key(1));
        let shared: Arc<dyn PinKeyStore> = store;
        let persistence = PinPersistence::start(path, shared).expect("pin worker starts");
        persistence.request_load();
        wait_until(|| persistence.status().state == PinStoreState::Unreadable);
        assert!(persistence.take_loaded().is_none());
        persistence.shutdown();
    }

    #[test]
    fn a_keyless_or_truncated_store_keeps_history_memory_only() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let writer = PinKey::from_bytes(&[1; PIN_KEY_BYTES]).expect("exact length");
        pin_store::write_pins(&path, &writer, &[SavedPin::new("hidden value", 10)])
            .expect("encrypted write");

        let store = Arc::new(FakeKeyStore::with_key(2));
        let shared: Arc<dyn PinKeyStore> = store;
        let (mut service, _, _) = start_pin_service(pin_policy(path), shared);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Unreadable);
        assert!(service.latest().items.is_empty());
        assert!(!format!("{:?}", service.latest()).contains("hidden value"));
        service.stop();
    }

    #[test]
    fn saved_pins_obey_count_byte_and_age_bounds_before_writing() {
        let now = Instant::now();
        let mut pin_policy = pin_policy(PathBuf::from("/unused"));
        pin_policy.max_items = 3;
        pin_policy.max_item_bytes = 8;
        pin_policy.max_total_bytes = 100;
        pin_policy.pinned_max_age = Duration::from_millis(100);
        let mut store = HistoryStore::new(pin_policy);

        for (id, value) in [(1, "aaaa"), (2, "bbbb"), (3, "cccc")] {
            store.capture(text(value), now);
            store.set_pinned(id, true, now).expect("pin succeeds");
        }
        store.policy.max_total_bytes = 10;
        let saved = store.saved_pins(now);
        assert_eq!(saved.len(), 2, "the total byte bound drops the newest pin");
        assert!(
            saved.iter().map(|pin| pin.text().len()).sum::<usize>() <= 10,
            "saved pins stay inside the total byte bound"
        );

        assert!(
            store
                .saved_pins(now + Duration::from_millis(101))
                .is_empty(),
            "pins past the saved-pin age are not written"
        );

        store.policy.max_total_bytes = 100;
        store.policy.max_items = 1;
        assert_eq!(store.saved_pins(now).len(), 1, "the count bound applies");

        store.policy.max_items = 3;
        store.policy.max_item_bytes = 3;
        assert!(
            store.saved_pins(now).is_empty(),
            "an entry over the per-entry bound is not written"
        );
    }

    #[test]
    fn image_and_file_pins_are_not_saved() {
        let now = Instant::now();
        let mut store = HistoryStore::new(pin_policy(PathBuf::from("/unused")));
        store.capture(
            super::ClipboardEntryContent::ImagePng {
                bytes: zeroize::Zeroizing::new(PNG_2X2.to_vec()),
                dimensions: Some((2, 2)),
            },
            now,
        );
        store.set_pinned(1, true, now).expect("pin succeeds");
        store.capture(
            super::ClipboardEntryContent::Files(zeroize::Zeroizing::new(vec![
                "/tmp/example".to_owned(),
            ])),
            now,
        );
        store.set_pinned(2, true, now).expect("pin succeeds");

        assert!(store.saved_pins(now).is_empty());
        let snapshot = snapshot_of(&store, now);
        assert_eq!(snapshot.items.len(), 2);
        for item in &snapshot.items {
            assert_eq!(
                item.pin_save,
                PinSaveState::OnlyText,
                "images and file lists are kept in memory only"
            );
        }
    }

    #[test]
    fn wipe_removes_the_file_and_calls_the_key_delete() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(FakeKeyStore::with_key(4));
        let shared: Arc<dyn PinKeyStore> = store.clone();
        let (mut service, state, _) = start_pin_service(pin_policy(path.clone()), shared);

        state.lock().expect("fake state").current =
            Some(FakeSelection::Text("durable text".to_string()));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        service
            .execute(ClipboardCommand::SetPinned {
                item_id,
                pinned: true,
            })
            .expect("pin succeeds");
        wait_until(|| path.exists() && service.latest().pin_store.state == PinStoreState::Saved);
        let saved = service.latest();
        assert_eq!(saved.pin_store.saved_count, 1);
        assert_eq!(saved.pin_store.saved_bytes, "durable text".len());
        assert_eq!(saved.items[0].pin_save, PinSaveState::Saved);

        let report = service.wipe_saved_pins(false).expect("wipe succeeds");
        assert_eq!(report.saved_count, 1);
        assert!(report.key_removed);
        assert_eq!(report.key_error, None);
        assert!(!path.exists(), "the store file is gone");
        assert_eq!(store.deletes.load(Ordering::Relaxed), 1);
        service.stop();
    }

    #[test]
    fn an_unavailable_key_store_writes_no_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(FakeKeyStore::default());
        store.set_unavailable(true);
        let shared: Arc<dyn PinKeyStore> = store;
        let (mut service, state, _) = start_pin_service(pin_policy(path.clone()), shared);

        state.lock().expect("fake state").current =
            Some(FakeSelection::Text("kept in memory".to_string()));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        service
            .execute(ClipboardCommand::SetPinned {
                item_id,
                pinned: true,
            })
            .expect("pin succeeds");

        wait_until(|| service.latest().pin_store.state == PinStoreState::Unavailable);
        thread::sleep(Duration::from_millis(60));
        assert!(!path.exists(), "no key means no ciphertext file");
        assert_eq!(
            service.latest().items[0].pin_save,
            PinSaveState::StoreUnavailable
        );
        let report = service
            .wipe_saved_pins(false)
            .expect("wipe removes the file");
        assert!(!report.key_removed);
        assert_eq!(report.key_error, Some(PinKeyError::Unavailable));
        assert!(!path.exists());
        service.stop();
    }

    #[test]
    fn real_keyring_save_and_reload_with_a_new_instance() {
        if std::env::var("KESTREL_SECRET_SERVICE_TEST").as_deref() != Ok("1") {
            return;
        }
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(SecretServiceKeyStore::connect().expect("secret service connects"));
        let _ = store.delete();
        let shared: Arc<dyn PinKeyStore> = store;
        let first = PinPersistence::start(path.clone(), shared).expect("pin worker starts");
        wait_until(|| first.status().state == PinStoreState::Idle);
        first.save(vec![SavedPin::new(
            "secret note 42 for pinning",
            super::unix_seconds(),
        )]);
        wait_until(|| first.status().state == PinStoreState::Saved && path.exists());
        first.shutdown();

        let second_store: Arc<dyn PinKeyStore> =
            Arc::new(SecretServiceKeyStore::connect().expect("secret service reconnects"));
        let second = PinPersistence::start(path, second_store).expect("pin worker starts");
        second.request_load();
        let mut loaded = None;
        wait_until(|| {
            if let Some(pins) = second.take_loaded() {
                loaded = Some(pins);
                true
            } else {
                false
            }
        });
        let loaded = loaded.expect("a load completed");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].text(), "secret note 42 for pinning");

        let report = second.wipe().expect("wipe succeeds");
        assert!(report.key_removed, "the keyring item is deleted");
        assert_eq!(report.key_error, None);
        assert!(
            SecretServiceKeyStore::connect()
                .expect("secret service reconnects")
                .get()
                .expect("get after wipe")
                .is_none(),
            "SearchItems returns no item after the wipe"
        );
        second.shutdown();
    }

    #[test]
    fn lock_wipes_memory_and_resume_reloads_saved_pins() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(FakeKeyStore::with_key(5));
        let shared: Arc<dyn PinKeyStore> = store.clone();
        let (mut service, state, events) = start_pin_service(pin_policy(path.clone()), shared);

        state.lock().expect("fake state").current =
            Some(FakeSelection::Text("survivor".to_string()));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        service
            .execute(ClipboardCommand::SetPinned {
                item_id,
                pinned: true,
            })
            .expect("pin succeeds");
        wait_until(|| path.exists() && service.latest().pin_store.state == PinStoreState::Saved);

        store.set_locked(true);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Locked);
        wait_for_items(&service, 0);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Held);
        for _ in 0..3 {
            thread::sleep(Duration::from_millis(20));
            assert!(
                service.latest().items.is_empty(),
                "a locked session must keep saved pins out of memory"
            );
        }

        store.set_locked(false);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Unlocked);
        wait_until(|| service.latest().items.len() == 1);
        let snapshot = service.latest();
        assert!(snapshot.items[0].pinned, "the saved pin returns pinned");
        let preview = service
            .preview(snapshot.items[0].id, 64)
            .expect("preview succeeds");
        assert_eq!(preview.text, "survivor");
        wait_until(|| service.latest().pin_store.state == PinStoreState::Saved);

        // A reload stuck on a locked keyring stops retrying once the session
        // locks again, so a keyring that opens meanwhile decrypts nothing.
        store.set_locked(true);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Locked);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Held);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Unlocked);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Locked);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Locked);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Held);
        store.set_locked(false);
        let reads = store.reads.load(Ordering::Relaxed);
        thread::sleep(PIN_RETRY_INTERVAL * 4);
        assert_eq!(
            store.reads.load(Ordering::Relaxed),
            reads,
            "a held session must not retry the keyring"
        );
        assert!(service.latest().items.is_empty());

        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Unlocked);
        wait_until(|| service.latest().items.len() == 1);
        service.stop();
    }

    #[test]
    fn sleep_holds_saved_pins_until_resume() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let store = Arc::new(FakeKeyStore::with_key(6));
        let shared: Arc<dyn PinKeyStore> = store.clone();
        let (mut service, state, events) = start_pin_service(pin_policy(path.clone()), shared);

        state.lock().expect("fake state").current =
            Some(FakeSelection::Text("survivor".to_string()));
        wait_for_items(&service, 1);
        let item_id = service.latest().items[0].id;
        service
            .execute(ClipboardCommand::SetPinned {
                item_id,
                pinned: true,
            })
            .expect("pin succeeds");
        wait_until(|| path.exists() && service.latest().pin_store.state == PinStoreState::Saved);

        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::PreparingForSleep);
        wait_for_items(&service, 0);
        wait_until(|| service.latest().pin_store.state == PinStoreState::Held);
        for _ in 0..3 {
            thread::sleep(Duration::from_millis(20));
            assert!(
                service.latest().items.is_empty(),
                "a sleeping session must keep saved pins out of memory"
            );
        }

        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Resumed);
        wait_until(|| service.latest().items.len() == 1);
        assert_eq!(
            service
                .preview(service.latest().items[0].id, 64)
                .expect("preview succeeds")
                .text,
            "survivor"
        );
        service.stop();
    }

    #[test]
    fn a_session_locked_at_start_loads_nothing_until_unlocked() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let writer = PinKey::from_bytes(&[7; PIN_KEY_BYTES]).expect("exact length");
        pin_store::write_pins(
            &path,
            &writer,
            &[SavedPin::new("startup secret", super::unix_seconds())],
        )
        .expect("encrypted write");

        let store = Arc::new(FakeKeyStore::with_key(7));
        let key_store: Arc<dyn PinKeyStore> = store;
        let state = Arc::new(Mutex::new(FakeState::default()));
        let events = Arc::new(Mutex::new(vec![SessionPrivacyEvent::Locked]));
        let mut service = ClipboardHistoryService::new(pin_policy(path)).expect("valid policy");
        service.set_pin_key_store(key_store);
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                    watch: None,
                },
                FakePrivacy {
                    events: Arc::clone(&events),
                },
            )
            .expect("worker starts");

        wait_until(|| service.latest().pin_store.state == PinStoreState::Held);
        for _ in 0..3 {
            thread::sleep(Duration::from_millis(20));
            assert!(
                service.latest().items.is_empty(),
                "a session locked at start must not decrypt saved pins"
            );
        }

        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Unlocked);
        wait_until(|| service.latest().items.len() == 1);
        assert_eq!(
            service
                .preview(service.latest().items[0].id, 64)
                .expect("preview succeeds")
                .text,
            "startup secret"
        );
        service.stop();
    }

    struct FailingPrivacy {
        failed: Arc<AtomicBool>,
    }

    impl PrivacyEventSource for FailingPrivacy {
        fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError> {
            if self.failed.load(Ordering::Relaxed) {
                return Err(ClipboardError {
                    kind: ClipboardErrorKind::PrivacyMonitorFailed,
                    message: "logind unavailable".to_string(),
                });
            }
            Ok(Vec::new())
        }
    }

    #[test]
    fn privacy_monitor_failure_stops_keyring_retries() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(pin_store::PIN_FILE_NAME);
        let writer = PinKey::from_bytes(&[8; PIN_KEY_BYTES]).expect("exact length");
        pin_store::write_pins(
            &path,
            &writer,
            &[SavedPin::new("monitor secret", super::unix_seconds())],
        )
        .expect("encrypted write");

        let store = Arc::new(FakeKeyStore::with_key(8));
        store.set_locked(true);
        let key_store: Arc<dyn PinKeyStore> = store.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(FakeState {
            current: Some(FakeSelection::Text("monitor secret".to_string())),
            ..FakeState::default()
        }));
        let mut service = ClipboardHistoryService::new(pin_policy(path)).expect("valid policy");
        service.set_pin_key_store(key_store);
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                    watch: None,
                },
                FailingPrivacy {
                    failed: Arc::clone(&failed),
                },
            )
            .expect("worker starts");
        wait_until(|| service.latest().pin_store.state == PinStoreState::Locked);
        wait_until(|| service.latest().items.len() == 1);

        // A logind owner loss takes this same error path.
        failed.store(true, Ordering::Relaxed);
        wait_until(|| service.latest().lifecycle == ClipboardLifecycle::Unavailable);
        assert!(
            service.latest().items.is_empty(),
            "a failed privacy monitor must wipe retained content"
        );
        assert!(
            state.lock().expect("fake state").current.is_none(),
            "a failed privacy monitor must release the owned selection"
        );
        store.set_locked(false);
        let reads = store.reads.load(Ordering::Relaxed);
        thread::sleep(PIN_RETRY_INTERVAL * 4);
        assert_eq!(
            store.reads.load(Ordering::Relaxed),
            reads,
            "a failed privacy monitor must stop keyring retries"
        );
        service.stop();
    }
}
