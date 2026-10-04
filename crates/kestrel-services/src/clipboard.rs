//! Bounded, in-memory clipboard history.
//!
//! Retained content stays memory-only; published snapshots expose metadata, and
//! content leaves only through bounded requests or copying an entry back.

use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use kestrel_platform::clipboard::{
    ClipboardBackend, ClipboardEntryKind, ClipboardError, ClipboardKindSupport, ClipboardProvider,
    OwnedSelection, PrivacyEventSource, png_dimensions,
};
use zeroize::Zeroizing;

pub const DEFAULT_MAX_ITEM_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_ENTRIES: usize = 64;
pub const DEFAULT_MAX_ITEMS: usize = 100;
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub const DEFAULT_IMAGE_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_HISTORY_ITEMS: usize = 1000;
pub const MAX_PREVIEW_BYTES: usize = 4096;
pub const MAX_MATCH_PREVIEW_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        Ok(self)
    }
}

impl ClipboardPolicy {
    pub fn from_configuration(configuration: &kestrel_core::ClipboardConfiguration) -> Self {
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
        }
    }
}

/// A sensitive-content heuristic; false positives are intentional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensitivePattern {
    pub id: &'static str,
    pub description: &'static str,
}

pub const SENSITIVE_PATTERNS: [SensitivePattern; 5] = [
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

/// Metadata for one retained entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItemMetadata {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub size_bytes: usize,
    pub age: Duration,
    pub pinned: bool,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub lifecycle: ClipboardLifecycle,
    pub provider: Option<ClipboardProvider>,
    pub kind_support: ClipboardKindSupport,
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
}

impl ClipboardSnapshot {
    fn stopped() -> Self {
        Self {
            lifecycle: ClipboardLifecycle::Stopped,
            provider: None,
            kind_support: ClipboardKindSupport::TEXT_ONLY,
            items: Vec::new(),
            total_bytes: 0,
            pinned_items: 0,
            rejected_oversize_items: 0,
            filtered_sensitive_items: 0,
            selection_clears: 0,
            wipe_count: 0,
            last_filtered_pattern: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardMatch {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub preview: String,
}

/// A bounded preview returned for an explicit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPreview {
    pub id: u64,
    pub kind: ClipboardEntryKind,
    pub text: String,
    pub truncated: bool,
    pub size_bytes: usize,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardCommand {
    Refresh,
    /// Copies an entry back, normalizing text when requested.
    CopyItem {
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
    SetPolicy(
        ClipboardPolicy,
        Sender<Result<ClipboardSnapshot, ClipboardServiceError>>,
    ),
    Query(
        ClipboardQuery,
        Sender<Result<ClipboardQueryResult, ClipboardServiceError>>,
    ),
    Stop(Sender<()>),
}

/// An explicit, bounded content request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardQuery {
    Search { query: String, limit: usize },
    Preview { item_id: u64, max_bytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardQueryResult {
    Matches(Vec<ClipboardMatch>),
    Preview(ClipboardPreview),
}

pub struct ClipboardHistoryService {
    policy: Mutex<ClipboardPolicy>,
    sender: Option<Sender<WorkerCommand>>,
    worker: Option<JoinHandle<()>>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
}

impl ClipboardHistoryService {
    pub fn new(policy: ClipboardPolicy) -> Result<Self, ClipboardServiceError> {
        Ok(Self {
            policy: Mutex::new(policy.validate()?),
            sender: None,
            worker: None,
            snapshot: Arc::new(Mutex::new(ClipboardSnapshot::stopped())),
        })
    }

    pub fn start<B, P>(&mut self, backend: B, privacy: P) -> Result<(), ClipboardServiceError>
    where
        B: ClipboardBackend,
        P: PrivacyEventSource,
    {
        if self.worker.is_some() {
            return Err(ClipboardServiceError::AlreadyRunning);
        }
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::clone(&self.snapshot);
        let policy = *self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned");
        let worker = thread::Builder::new()
            .name("kestrel-clipboard-history".to_string())
            .spawn(move || run_worker(policy, backend, privacy, receiver, snapshot))
            .map_err(|error| {
                ClipboardServiceError::InvalidPolicy(format!(
                    "failed to start clipboard worker: {error}"
                ))
            })?;
        self.sender = Some(sender);
        self.worker = Some(worker);
        Ok(())
    }

    pub fn latest(&self) -> ClipboardSnapshot {
        self.snapshot
            .lock()
            .expect("clipboard snapshot mutex is not poisoned")
            .clone()
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
            ClipboardQueryResult::Preview(_) => Err(ClipboardServiceError::WorkerStopped),
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
            ClipboardQueryResult::Matches(_) => Err(ClipboardServiceError::WorkerStopped),
        }
    }

    /// Applies a policy while retaining existing entries.
    pub fn set_policy(
        &self,
        policy: ClipboardPolicy,
    ) -> Result<ClipboardSnapshot, ClipboardServiceError> {
        let policy = policy.validate()?;
        *self
            .policy
            .lock()
            .expect("clipboard policy mutex is not poisoned") = policy;
        let sender = self
            .sender
            .as_ref()
            .ok_or(ClipboardServiceError::NotRunning)?;
        let (response_sender, response_receiver) = mpsc::channel();
        sender
            .send(WorkerCommand::SetPolicy(policy, response_sender))
            .map_err(|_| ClipboardServiceError::WorkerStopped)?;
        response_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ClipboardServiceError::WorkerTimedOut,
                RecvTimeoutError::Disconnected => ClipboardServiceError::WorkerStopped,
            })?
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
    last_filtered_pattern: Option<&'static str>,
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
            last_filtered_pattern: None,
        }
    }

    /// Stores new, bounded, unfiltered content and returns its ID.
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

    /// Evicts oldest unpinned entries while enforcing both bounds.
    fn enforce_bounds(&mut self) {
        while self.items.len() > self.policy.max_items
            || self.total_bytes() > self.policy.max_total_bytes
        {
            let index = self.items.iter().position(|item| !item.pinned).unwrap_or(0);
            self.items.remove(index);
        }
    }

    /// Age pruning skips pinned entries.
    fn prune(&mut self, now: Instant) {
        let max_age = self.policy.max_age;
        self.items.retain(|item| {
            item.pinned || now.saturating_duration_since(item.captured_at) <= max_age
        });
    }

    fn total_bytes(&self) -> usize {
        self.items
            .iter()
            .map(|item| item.content.size_bytes())
            .sum()
    }

    fn content(&self, item_id: u64) -> Option<&ClipboardEntryContent> {
        self.items
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| &item.content)
    }

    fn delete(&mut self, item_ids: &[u64]) -> usize {
        let before = self.items.len();
        self.items.retain(|item| !item_ids.contains(&item.id));
        before - self.items.len()
    }

    fn set_pinned(&mut self, item_id: u64, pinned: bool) -> Result<(), ClipboardServiceError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
        item.pinned = pinned;
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
                        let lowered = text.to_lowercase();
                        if needle.is_empty() || lowered.contains(&needle) {
                            match_preview(text)
                        } else {
                            return None;
                        }
                    }
                    ClipboardEntryContent::Files(paths) => {
                        let lowered = paths.join("\n").to_lowercase();
                        if needle.is_empty() || lowered.contains(&needle) {
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
        let (text, truncated) = match content {
            ClipboardEntryContent::Text(value) => bounded_text(value, max_bytes),
            ClipboardEntryContent::Files(paths) => bounded_text(&paths.join("\n"), max_bytes),
            ClipboardEntryContent::ImagePng { .. } => (String::new(), false),
        };
        Ok(ClipboardPreview {
            id: item_id,
            kind: content.kind(),
            text,
            truncated,
            size_bytes,
            image_dimensions: content.image_dimensions(),
            file_count: content.file_count(),
        })
    }

    fn wipe(&mut self) {
        for mut item in self.items.drain(..) {
            match &mut item.content {
                ClipboardEntryContent::Text(text) => text.clear(),
                ClipboardEntryContent::ImagePng { bytes, .. } => bytes.clear(),
                ClipboardEntryContent::Files(paths) => paths.clear(),
            }
        }
        self.wipe_count = self.wipe_count.saturating_add(1);
    }

    fn snapshot(
        &self,
        lifecycle: ClipboardLifecycle,
        provider: Option<ClipboardProvider>,
        support: ClipboardKindSupport,
        last_error: Option<String>,
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
            })
            .collect::<Vec<_>>();
        ClipboardSnapshot {
            lifecycle,
            provider,
            kind_support: support,
            total_bytes: items.iter().map(|item| item.size_bytes).sum(),
            pinned_items: items.iter().filter(|item| item.pinned).count(),
            items,
            rejected_oversize_items: self.rejected_oversize_items,
            filtered_sensitive_items: self.filtered_sensitive_items,
            selection_clears: self.selection_clears,
            wipe_count: self.wipe_count,
            last_filtered_pattern: self.last_filtered_pattern,
            last_error,
        }
    }
}

enum CaptureOutcome {
    Stored(u64),
    Duplicate,
    Filtered(&'static str),
    Oversize,
    Empty,
}

/// A bounded, character-safe preview of `text`.
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
    observed: Option<ObservedSelection>,
    owned: Option<OwnedSelection>,
    owned_since: Option<Instant>,
    last_rich_probe: Option<Instant>,
    last_error: Option<String>,
}

fn run_worker<B: ClipboardBackend, P: PrivacyEventSource>(
    policy: ClipboardPolicy,
    backend: B,
    privacy: P,
    receiver: Receiver<WorkerCommand>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
) {
    let provider = backend.provider();
    let support = backend.kind_support();
    let mut state = WorkerState {
        policy,
        backend,
        privacy,
        history: HistoryStore::new(policy),
        provider,
        support,
        observed: None,
        owned: None,
        owned_since: None,
        last_rich_probe: None,
        last_error: None,
    };
    state.publish(&snapshot, ClipboardLifecycle::Running);

    loop {
        let poll_interval = state.policy.poll_interval;
        match receiver.recv_timeout(poll_interval) {
            Ok(WorkerCommand::SetPolicy(policy, response)) => {
                state.policy = policy;
                state.history.policy = policy;
                state.history.enforce_bounds();
                let result = Ok(state.snapshot(ClipboardLifecycle::Running, Instant::now()));
                if let Ok(value) = &result {
                    publish(&snapshot, value.clone());
                }
                let _ = response.send(result);
            }
            Ok(WorkerCommand::Execute(command, response)) => {
                let result = state.execute(command);
                if let Ok(value) = &result {
                    publish(&snapshot, value.clone());
                }
                let _ = response.send(result);
            }
            Ok(WorkerCommand::Query(query, response)) => {
                let result = state.query(query);
                let _ = response.send(result);
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

        if state.poll_privacy(&snapshot) {
            break;
        }
        state.tick();
        state.publish(&snapshot, ClipboardLifecycle::Running);
    }
}

impl<B: ClipboardBackend, P: PrivacyEventSource> WorkerState<B, P> {
    fn poll_privacy(&mut self, snapshot: &Arc<Mutex<ClipboardSnapshot>>) -> bool {
        match self.privacy.poll_events() {
            Ok(events) if !events.is_empty() => {
                self.wipe_owned();
                false
            }
            Ok(_) => false,
            Err(error) => {
                self.last_error = Some(error.message);
                self.wipe_owned();
                self.publish(snapshot, ClipboardLifecycle::Unavailable);
                true
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.history.prune(now);
        self.auto_clear_selection(now);
        let offered = match self.backend.offered_kind() {
            Ok(offered) => offered,
            Err(error) => {
                self.last_error = Some(error.message);
                None
            }
        };
        let kind = match offered {
            Some(kind) if self.support.supports(kind) => Some(kind),
            Some(_) => None,
            // Type-enumeration fallback uses text.
            None if !self.support.image && !self.support.files => Some(ClipboardEntryKind::Text),
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
            let due = self.last_rich_probe.is_none_or(|last| {
                now.saturating_duration_since(last) >= self.policy.image_poll_interval
            });
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
        if self
            .observed
            .as_ref()
            .is_some_and(|observed| observed.matches_content(&content))
        {
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
                self.owned = Some(owned);
                self.owned_since = Some(now);
                self.last_error = None;
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
                self.history.set_pinned(item_id, pinned)?;
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
            ClipboardCommand::Wipe => self.wipe_owned(),
        }
        self.history.prune(now);
        self.history.enforce_bounds();
        Ok(self.snapshot(ClipboardLifecycle::Running, now))
    }

    fn snapshot(&self, lifecycle: ClipboardLifecycle, now: Instant) -> ClipboardSnapshot {
        self.history.snapshot(
            lifecycle,
            Some(self.provider),
            self.support,
            self.last_error.clone(),
            now,
        )
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
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use kestrel_platform::clipboard::{
        ClipboardBackend, ClipboardEntryKind, ClipboardError, ClipboardErrorKind,
        ClipboardKindSupport, ClipboardProvider, OwnedSelection, PrivacyEventSource,
        SessionPrivacyEvent,
    };

    use super::{
        ClipboardCommand, ClipboardHistoryService, ClipboardLifecycle, ClipboardPolicy,
        HistoryStore, MAX_MATCH_PREVIEW_CHARS, SENSITIVE_PATTERNS, match_sensitive, plain_text,
    };

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
    }

    struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
    }

    impl ClipboardBackend for FakeBackend {
        fn provider(&self) -> ClipboardProvider {
            ClipboardProvider::WaylandDataControl
        }

        fn kind_support(&self) -> ClipboardKindSupport {
            ClipboardKindSupport::ALL
        }

        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
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
        }
    }

    fn new_store() -> HistoryStore {
        HistoryStore::new(policy())
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

        let snapshot = store.snapshot(
            ClipboardLifecycle::Running,
            None,
            ClipboardKindSupport::ALL,
            None,
            Instant::now(),
        );
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

        let snapshot = store.snapshot(
            ClipboardLifecycle::Running,
            None,
            ClipboardKindSupport::ALL,
            None,
            Instant::now(),
        );
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
        store.set_pinned(1, true).expect("pin succeeds");
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
        store.set_pinned(1, true).expect("pin succeeds");
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
        let snapshot = store.snapshot(
            ClipboardLifecycle::Running,
            None,
            ClipboardKindSupport::ALL,
            None,
            later,
        );
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
        assert_eq!(SENSITIVE_PATTERNS.len(), 5);

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

        let snapshot = store.snapshot(
            ClipboardLifecycle::Running,
            None,
            ClipboardKindSupport::ALL,
            None,
            now,
        );
        assert_eq!(snapshot.filtered_sensitive_items, 1);
        assert_eq!(snapshot.last_filtered_pattern, Some("password_assignment"));
        assert!(
            !format!("{snapshot:?}").contains("hunter2secret"),
            "diagnostics never carry filtered content"
        );
    }

    #[test]
    fn plain_text_strips_escape_sequences_and_trailing_whitespace() {
        assert_eq!(
            plain_text("\u{1b}[31mred\u{1b}[0m text  \n\n").as_str(),
            "red text"
        );
        assert_eq!(plain_text("\u{1b}]0;title\u{7}body").as_str(), "body");
        assert_eq!(plain_text("no escapes").as_str(), "no escapes");
        assert_eq!(plain_text("µnicode ✓  ").as_str(), "µnicode ✓");
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
        let state = Arc::new(Mutex::new(FakeState {
            current,
            ..FakeState::default()
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut service = ClipboardHistoryService::new(policy).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                },
                FakePrivacy {
                    events: Arc::clone(&events),
                },
            )
            .expect("worker starts");
        (service, state, events)
    }

    #[test]
    fn worker_captures_and_wipes_owned_content_on_privacy_event() {
        let (mut service, state, events) =
            start_fake(policy(), Some(FakeSelection::Text("secret".to_string())));
        wait_for_items(&service, 1);

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
        };

        let policy = ClipboardPolicy::from_configuration(&configuration);
        assert_eq!(policy.max_items, 7);
        assert_eq!(policy.max_age, Duration::from_secs(7200));
        assert_eq!(policy.selection_clear_after, Some(Duration::from_secs(30)));
        assert!(policy.filter_sensitive);
        assert!(!policy.paste_plain_text);
        assert!(policy.validate().is_ok());

        let zero_clear =
            ClipboardPolicy::from_configuration(&kestrel_core::ClipboardConfiguration {
                clear_seconds: 0,
                ..kestrel_core::ClipboardConfiguration::default()
            });
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
}
