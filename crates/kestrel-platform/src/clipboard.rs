//! Capability-gated clipboard ownership and privacy events.
//! Text uses `arboard`; rich Wayland entries use bounded PNG and `text/uri-list`
//! payloads in their encoded form.

use std::{
    collections::HashMap,
    env,
    error::Error,
    fmt,
    io::Read,
    os::fd::{AsRawFd, OwnedFd},
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use arboard::Clipboard;
use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use wl_clipboard_rs::{
    copy::{
        ClipboardType as WaylandCopyClipboardType, MimeSource, MimeType, Options, Source, clear,
    },
    paste::{
        ClipboardType as WaylandClipboardType, Error as WaylandError, Seat as WaylandSeat,
        get_contents, get_mime_types,
    },
    watch::{ClipboardType as WaylandWatchClipboardType, Watcher as WaylandWatcher},
};
use x11rb::{
    connection::Connection as X11Connection,
    protocol::{
        Event as X11Event,
        xfixes::{ConnectionExt as X11FixesExt, SelectionEventMask},
        xproto::{Atom, ConnectionExt},
    },
    rust_connection::RustConnection,
};
use zbus::{
    MatchRule,
    blocking::{
        Connection as BusConnection, MessageIterator, Proxy,
        connection::Builder as BusConnectionBuilder,
    },
    message::Type as MessageType,
    zvariant::{OwnedObjectPath, OwnedValue},
};

use crate::{
    CapabilityProbe,
    logind_session::{
        LOGIN_DESTINATION, LOGIN_PATH, MANAGER_INTERFACE, SESSION_INTERFACE, current_session_path,
    },
    wake::{drain_event_fd, event_fd, signal_event_fd, wait as wait_for_descriptors},
};

pub const FEATURE_ID: &str = "clipboard.history";
const PRIVACY_METHOD_TIMEOUT: Duration = Duration::from_millis(500);
pub const PNG_MIME: &str = "image/png";
pub const URI_LIST_MIME: &str = "text/uri-list";
/// KDE's password-manager marker; its payload is `secret` or `persistent`.
pub const KDE_PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";
/// A KDE marker asking not to replace non-empty selections.
pub const KDE_ONLY_REPLACE_EMPTY_MIME: &str = "application/x-kde-onlyReplaceEmpty";
/// The hint payload that marks a write as not fit for history.
pub const KDE_PASSWORD_HINT_SECRET: &str = "secret";
const TEXT_MIMES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
];
/// How long a released selection may remain served.
const OWNERSHIP_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
const RELEASE_POLL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardProvider {
    WaylandDataControl,
    X11,
}

impl ClipboardProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::WaylandDataControl => "Wayland data-control",
            Self::X11 => "X11 CLIPBOARD selection",
        }
    }
}

/// How the worker learns that the selection changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardWatchMode {
    /// The backend reports each selection change, so content is read on demand.
    Events,
    /// The worker re-reads the selection on a fixed interval.
    Polling,
}

impl ClipboardWatchMode {
    pub const fn evidence_value(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::Polling => "polling",
        }
    }
}

/// A cloneable handle that interrupts a blocked change watch from another thread.
#[derive(Clone)]
pub struct ChangeWatchCancel(Arc<dyn Fn() + Send + Sync>);

impl ChangeWatchCancel {
    pub fn new(cancel: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(cancel))
    }

    pub fn cancel(&self) {
        (self.0)();
    }
}

impl fmt::Debug for ChangeWatchCancel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChangeWatchCancel")
    }
}

/// A blocking source of clipboard selection changes.
pub trait ClipboardChangeWatch: Send + 'static {
    /// Blocks until the selection changes.
    ///
    /// Returns `false` once the watch is cancelled or the backend can no longer
    /// report changes.
    fn next_change(&mut self) -> Result<bool, ClipboardError>;

    /// Interrupts a blocked [`ClipboardChangeWatch::next_change`].
    fn cancel_handle(&self) -> ChangeWatchCancel;
}

/// A blocking source of session lock and sleep changes.
pub trait PrivacyChangeWatch: Send + 'static {
    /// Runs until cancelled or the bus closes, calling `changed` for each
    /// session signal edge.
    ///
    /// `Some(event)` is an edge the signal carried; `None` asks the worker to
    /// re-read the current state (an invalidated property or a new owner).
    /// Returns `true` when `changed` asked to stop and `false` when the signal
    /// stream ended or could not be registered.
    fn run(&mut self, changed: &mut dyn FnMut(Option<SessionPrivacyEvent>) -> bool) -> bool;

    /// Interrupts a blocked [`PrivacyChangeWatch::run`].
    fn cancel_handle(&self) -> ChangeWatchCancel;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardEntryKind {
    Text,
    Image,
    Files,
}

impl ClipboardEntryKind {
    pub const ALL: [ClipboardEntryKind; 3] = [
        ClipboardEntryKind::Text,
        ClipboardEntryKind::Image,
        ClipboardEntryKind::Files,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Image => "Image",
            Self::Files => "Files",
        }
    }
}

/// Entry kinds a provider can capture and re-own.
///
/// X11 is text-only; Wayland data-control also carries PNG and URI lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipboardKindSupport {
    pub text: bool,
    pub image: bool,
    pub files: bool,
}

impl ClipboardKindSupport {
    pub const TEXT_ONLY: Self = Self {
        text: true,
        image: false,
        files: false,
    };
    pub const ALL: Self = Self {
        text: true,
        image: true,
        files: true,
    };

    pub const fn supports(self, kind: ClipboardEntryKind) -> bool {
        match kind {
            ClipboardEntryKind::Text => self.text,
            ClipboardEntryKind::Image => self.image,
            ClipboardEntryKind::Files => self.files,
        }
    }

    /// Compact evidence value such as `text,image,files`.
    pub fn evidence_value(self) -> String {
        ClipboardEntryKind::ALL
            .into_iter()
            .filter(|kind| self.supports(*kind))
            .map(|kind| match kind {
                ClipboardEntryKind::Text => "text",
                ClipboardEntryKind::Image => "image",
                ClipboardEntryKind::Files => "files",
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// A captured selection Kestrel owns until replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedSelection {
    Text(String),
    ImagePng(Vec<u8>),
    Files(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardErrorKind {
    Unavailable,
    ContentUnavailable,
    ReadFailed,
    WriteFailed,
    /// The selection exceeds the caller's size bound.
    Oversize,
    PrivacyMonitorFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardError {
    pub kind: ClipboardErrorKind,
    pub message: String,
}

impl ClipboardError {
    fn new(kind: ClipboardErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ClipboardError {}

fn unsupported_kind(kind: ClipboardEntryKind, provider: ClipboardProvider) -> ClipboardError {
    ClipboardError::new(
        ClipboardErrorKind::Unavailable,
        format!(
            "{} entries are unavailable on the {} path",
            kind.label(),
            provider.label()
        ),
    )
}

/// Validates PNG and reads IHDR dimensions from encoded bytes.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

/// Parses bounded local paths; skips comments, blanks, non-file entries, and duplicates.
pub fn parse_uri_list(payload: &str, max_entries: usize) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in payload.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(path) = line.strip_prefix("file://") else {
            continue;
        };
        let decoded = percent_decode(path);
        if decoded.is_empty() || paths.contains(&decoded) {
            continue;
        }
        paths.push(decoded);
        if paths.len() == max_entries {
            break;
        }
    }
    paths
}

/// Encodes local paths as a `text/uri-list`.
pub fn encode_uri_list(paths: &[String]) -> String {
    let mut payload = String::new();
    for path in paths {
        payload.push_str("file://");
        for byte in path.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                    payload.push(byte as char);
                }
                _ => payload.push_str(&format!("%{byte:02X}")),
            }
        }
        payload.push_str("\r\n");
    }
    payload
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/plain") || mime == "UTF8_STRING" || mime == "STRING"
}

/// Highest-priority entry kind among the offered MIME types.
pub fn kind_from_types(types: &[String]) -> Option<ClipboardEntryKind> {
    if types.iter().any(|mime| mime == URI_LIST_MIME) {
        Some(ClipboardEntryKind::Files)
    } else if types.iter().any(|mime| mime == PNG_MIME) {
        Some(ClipboardEntryKind::Image)
    } else if types.iter().any(|mime| is_text_mime(mime)) {
        Some(ClipboardEntryKind::Text)
    } else {
        None
    }
}

/// Whether the offered types mark the write as temporary or generated.
///
/// `hint` is the password-manager marker's payload when it could be read; an
/// unreadable hint is treated as secret, so such a write is never recorded.
pub fn temporary_write(types: &[String], hint: Option<&str>) -> bool {
    if types.iter().any(|mime| mime == KDE_ONLY_REPLACE_EMPTY_MIME) {
        return true;
    }
    let offers_hint = types.iter().any(|mime| mime == KDE_PASSWORD_HINT_MIME);
    if !offers_hint {
        return false;
    }
    hint.is_none_or(|value| value.trim().eq_ignore_ascii_case(KDE_PASSWORD_HINT_SECRET))
}

pub trait ClipboardBackend: Send + 'static {
    fn provider(&self) -> ClipboardProvider;

    /// Takes the change watch when this backend can report selection changes.
    ///
    /// `None` keeps the worker polling.
    fn take_change_watch(
        &mut self,
    ) -> Result<Option<Box<dyn ClipboardChangeWatch>>, ClipboardError> {
        Ok(None)
    }

    /// Entry kinds this provider can capture, own, and restore.
    fn kind_support(&self) -> ClipboardKindSupport {
        ClipboardKindSupport::TEXT_ONLY
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;

    /// Highest-priority kind offered by the live selection.
    ///
    /// `Ok(None)` means type enumeration is unavailable; callers should read text.
    fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
        Ok(None)
    }

    /// Whether [`ClipboardBackend::offered_types`] can list this provider's types.
    fn supports_type_listing(&self) -> bool {
        false
    }

    /// Offered MIME types of the live selection.
    ///
    /// `Ok(None)` means the backend cannot list them, so temporary writes
    /// cannot be recognised.
    fn offered_types(&mut self) -> Result<Option<Vec<String>>, ClipboardError> {
        Ok(None)
    }

    /// The password-manager hint payload, when the selection offers one.
    ///
    /// Only the hint is read: entry content stays unread.
    fn password_manager_hint(
        &mut self,
        max_bytes: usize,
    ) -> Result<Option<String>, ClipboardError> {
        let _ = max_bytes;
        Ok(None)
    }

    /// Reads bounded PNG data, or `None` when no image is offered.
    fn read_image_png(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClipboardError> {
        let _ = max_bytes;
        Err(unsupported_kind(ClipboardEntryKind::Image, self.provider()))
    }

    fn write_image_png(&mut self, bytes: &[u8]) -> Result<(), ClipboardError> {
        let _ = bytes;
        Err(unsupported_kind(ClipboardEntryKind::Image, self.provider()))
    }

    /// Reads bounded local paths, or `None` when no file list is offered.
    fn read_file_list(
        &mut self,
        max_entries: usize,
    ) -> Result<Option<Vec<String>>, ClipboardError> {
        let _ = max_entries;
        Err(unsupported_kind(ClipboardEntryKind::Files, self.provider()))
    }

    fn write_file_list(&mut self, paths: &[String]) -> Result<(), ClipboardError> {
        let _ = paths;
        Err(unsupported_kind(ClipboardEntryKind::Files, self.provider()))
    }

    /// Whether the live selection is still the one Kestrel wrote as `expected`.
    ///
    /// Matching content is not ownership: another source may offer the same
    /// bytes with richer formats. Paste confirms this before typing.
    fn owns_selection(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError>;

    /// Clears the live selection only when it matches `expected`.
    fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError>;
}

struct ServedSelection {
    owned: OwnedSelection,
    handle: Option<JoinHandle<()>>,
}

impl ServedSelection {
    fn is_alive(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

pub struct ArboardClipboardBackend {
    provider: ClipboardProvider,
    clipboard: Clipboard,
    /// Rich selections use a dedicated data-control connection.
    served: Option<ServedSelection>,
    /// The X11 window that owned CLIPBOARD at the last write.
    x11_owner: Option<u32>,
    /// The connection used to read the live X11 CLIPBOARD owner.
    x11: Option<X11SelectionOwner>,
}

impl ArboardClipboardBackend {
    pub fn new(provider: ClipboardProvider) -> Result<Self, ClipboardError> {
        let clipboard = Clipboard::new().map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::Unavailable,
                format!("failed to initialize {}: {error}", provider.label()),
            )
        })?;
        Ok(Self {
            provider,
            clipboard,
            served: None,
            x11_owner: None,
            x11: None,
        })
    }

    /// Reads the live X11 CLIPBOARD owner, keeping one connection open.
    fn x11_live_owner(&mut self) -> Result<Option<u32>, ClipboardError> {
        if self.x11.is_none() {
            self.x11 = Some(X11SelectionOwner::new()?);
        }
        let selection = self
            .x11
            .as_ref()
            .expect("the X11 selection connection was just created");
        selection.owner()
    }

    /// Records the owner window of a write, leaving it unconfirmed on failure.
    fn record_x11_owner(&mut self) {
        self.x11_owner = self.x11_live_owner().ok().flatten();
    }

    /// Releases rich selections, waiting briefly for takeover observation.
    fn release_served(&mut self) {
        let Some(mut served) = self.served.take() else {
            return;
        };
        let _ = clear(
            WaylandCopyClipboardType::Regular,
            wl_clipboard_rs::copy::Seat::All,
        );
        let Some(handle) = served.handle.take() else {
            return;
        };
        let deadline = Instant::now() + OWNERSHIP_RELEASE_TIMEOUT;
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(RELEASE_POLL);
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
        // Detach a still-running loop; it exits after observing takeover.
    }

    /// Serves `owned` until another source replaces the selection.
    fn serve(&mut self, owned: OwnedSelection) -> Result<(), ClipboardError> {
        let mut options = Options::new();
        options.foreground(true);
        let prepared = match &owned {
            // Offer text under common MIME types for broad paste compatibility.
            OwnedSelection::Text(text) => options.prepare_copy_multi(
                TEXT_MIMES
                    .iter()
                    .map(|mime| MimeSource {
                        source: Source::Bytes(text.clone().into_bytes().into_boxed_slice()),
                        mime_type: MimeType::Specific((*mime).to_string()),
                    })
                    .collect(),
            ),
            OwnedSelection::ImagePng(bytes) => options.prepare_copy(
                Source::Bytes(bytes.clone().into_boxed_slice()),
                MimeType::Specific(PNG_MIME.to_string()),
            ),
            OwnedSelection::Files(paths) => options.prepare_copy(
                Source::Bytes(encode_uri_list(paths).into_bytes().into_boxed_slice()),
                MimeType::Specific(URI_LIST_MIME.to_string()),
            ),
        }
        .map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard ownership failed: {error}"),
            )
        })?;
        let handle = thread::Builder::new()
            .name("kestrel-clipboard-serve".to_string())
            .spawn(move || {
                let _ = prepared.serve();
            })
            .map_err(|error| {
                ClipboardError::new(
                    ClipboardErrorKind::WriteFailed,
                    format!("clipboard serving thread failed to start: {error}"),
                )
            })?;
        self.served = Some(ServedSelection {
            owned,
            handle: Some(handle),
        });
        Ok(())
    }

    /// Bounded read of one MIME type from the regular clipboard.
    fn read_mime(
        &mut self,
        mime: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, ClipboardError> {
        match get_contents(
            WaylandClipboardType::Regular,
            WaylandSeat::Unspecified,
            wl_clipboard_rs::paste::MimeType::Specific(mime),
        ) {
            Ok((reader, _)) => {
                let mut bounded = reader.take(max_bytes as u64 + 1);
                let mut payload = Vec::new();
                bounded.read_to_end(&mut payload).map_err(|error| {
                    ClipboardError::new(
                        ClipboardErrorKind::ReadFailed,
                        format!("clipboard {mime} read failed: {error}"),
                    )
                })?;
                if payload.len() > max_bytes {
                    return Err(ClipboardError::new(
                        ClipboardErrorKind::Oversize,
                        format!("clipboard {mime} payload exceeds the {max_bytes} byte bound"),
                    ));
                }
                if payload.is_empty() {
                    return Ok(None);
                }
                Ok(Some(payload))
            }
            Err(WaylandError::ClipboardEmpty) | Err(WaylandError::NoMimeType) => Ok(None),
            Err(error) => Err(ClipboardError::new(
                ClipboardErrorKind::ReadFailed,
                format!("clipboard {mime} read failed: {error}"),
            )),
        }
    }

    /// Releases text via data-control, falling back to arboard.
    ///
    /// The same protocol family removes arboard's compositor-owned selection.
    fn release_text_selection(&mut self) -> Result<(), ClipboardError> {
        self.x11_owner = None;
        if self.provider == ClipboardProvider::WaylandDataControl
            && clear(
                WaylandCopyClipboardType::Regular,
                wl_clipboard_rs::copy::Seat::All,
            )
            .is_ok()
        {
            return Ok(());
        }
        self.clipboard.clear().map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard clear failed: {error}"),
            )
        })
    }

    /// Releases a matching rich selection Kestrel still owns.
    fn clear_served_if_matches(
        &mut self,
        expected: &OwnedSelection,
    ) -> Result<bool, ClipboardError> {
        let Some(served) = self.served.as_ref() else {
            return Ok(false);
        };
        if served.owned != *expected || !served.is_alive() {
            // Another source replaced the selection, or Kestrel's ownership ended.
            self.served = None;
            return Ok(false);
        }
        // Only Kestrel's own data-control source may be cleared.
        self.release_served();
        Ok(true)
    }
}

impl ClipboardBackend for ArboardClipboardBackend {
    fn provider(&self) -> ClipboardProvider {
        self.provider
    }

    fn take_change_watch(
        &mut self,
    ) -> Result<Option<Box<dyn ClipboardChangeWatch>>, ClipboardError> {
        match self.provider {
            ClipboardProvider::WaylandDataControl => Ok(Some(Box::new(WaylandChangeWatch::new()?))),
            ClipboardProvider::X11 => Ok(Some(Box::new(X11ChangeWatch::new()?))),
        }
    }

    fn kind_support(&self) -> ClipboardKindSupport {
        kind_support_for(self.provider)
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        match self.clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(ClipboardError::new(
                ClipboardErrorKind::ReadFailed,
                format!("clipboard text read failed: {error}"),
            )),
        }
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        // Taking ownership ends any rich selection Kestrel served.
        if self.served.is_some() {
            self.release_served();
        }
        if self.provider == ClipboardProvider::WaylandDataControl {
            // Serve and release every Wayland kind through one mechanism.
            return self.serve(OwnedSelection::Text(text.to_owned()));
        }
        self.clipboard.set_text(text.to_owned()).map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard ownership failed: {error}"),
            )
        })?;
        self.record_x11_owner();
        Ok(())
    }

    fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Ok(None);
        }
        match get_mime_types(WaylandClipboardType::Regular, WaylandSeat::Unspecified) {
            Ok(types) => Ok(kind_from_types(&types.into_iter().collect::<Vec<String>>())),
            Err(WaylandError::ClipboardEmpty) | Err(WaylandError::NoMimeType) => Ok(None),
            Err(error) => Err(ClipboardError::new(
                ClipboardErrorKind::ContentUnavailable,
                format!("clipboard type enumeration failed: {error}"),
            )),
        }
    }

    fn supports_type_listing(&self) -> bool {
        self.provider == ClipboardProvider::WaylandDataControl
    }

    fn offered_types(&mut self) -> Result<Option<Vec<String>>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Ok(None);
        }
        match get_mime_types(WaylandClipboardType::Regular, WaylandSeat::Unspecified) {
            Ok(types) => Ok(Some(types.into_iter().collect())),
            Err(WaylandError::ClipboardEmpty) | Err(WaylandError::NoMimeType) => {
                Ok(Some(Vec::new()))
            }
            Err(error) => Err(ClipboardError::new(
                ClipboardErrorKind::ContentUnavailable,
                format!("clipboard type enumeration failed: {error}"),
            )),
        }
    }

    fn password_manager_hint(
        &mut self,
        max_bytes: usize,
    ) -> Result<Option<String>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Ok(None);
        }
        let Some(payload) = self.read_mime(KDE_PASSWORD_HINT_MIME, max_bytes.min(256))? else {
            return Ok(None);
        };
        Ok(Some(String::from_utf8_lossy(&payload).into_owned()))
    }

    fn read_image_png(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Err(unsupported_kind(ClipboardEntryKind::Image, self.provider));
        }
        // Reuse the payload Kestrel is serving; its contents are known.
        if let Some(served) = self.served.as_ref() {
            if served.is_alive() {
                if let OwnedSelection::ImagePng(bytes) = &served.owned {
                    return Ok(Some(bytes.clone()));
                }
                return Ok(None);
            }
        }
        self.read_mime(PNG_MIME, max_bytes)
    }

    fn write_image_png(&mut self, bytes: &[u8]) -> Result<(), ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Err(unsupported_kind(ClipboardEntryKind::Image, self.provider));
        }
        if bytes.is_empty() || png_dimensions(bytes).is_none() {
            return Err(ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                "only well-formed PNG payloads can be placed on the clipboard",
            ));
        }
        self.release_served();
        self.serve(OwnedSelection::ImagePng(bytes.to_vec()))
    }

    fn read_file_list(
        &mut self,
        max_entries: usize,
    ) -> Result<Option<Vec<String>>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Err(unsupported_kind(ClipboardEntryKind::Files, self.provider));
        }
        if let Some(served) = self.served.as_ref() {
            if served.is_alive() {
                if let OwnedSelection::Files(paths) = &served.owned {
                    return Ok(Some(paths.clone()));
                }
                return Ok(None);
            }
        }
        // File lists are text; bound the raw payload before parsing.
        let Some(payload) = self.read_mime(URI_LIST_MIME, max_entries * 4096)? else {
            return Ok(None);
        };
        let parsed = parse_uri_list(&String::from_utf8_lossy(&payload), max_entries);
        Ok((!parsed.is_empty()).then_some(parsed))
    }

    fn write_file_list(&mut self, paths: &[String]) -> Result<(), ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Err(unsupported_kind(ClipboardEntryKind::Files, self.provider));
        }
        if paths.is_empty() {
            return Err(ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                "an empty file list cannot be placed on the clipboard",
            ));
        }
        self.release_served();
        self.serve(OwnedSelection::Files(paths.to_vec()))
    }

    fn owns_selection(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
        match self.provider {
            // The data-control source ends when another client replaces it.
            ClipboardProvider::WaylandDataControl => Ok(self
                .served
                .as_ref()
                .is_some_and(|served| served.is_alive() && served.owned == *expected)),
            // Arboard reports no owner, so the window recorded at write is compared.
            ClipboardProvider::X11 => {
                let OwnedSelection::Text(_) = expected else {
                    return Ok(false);
                };
                let Some(recorded) = self.x11_owner else {
                    return Err(x11_owner_error());
                };
                Ok(self.x11_live_owner()? == Some(recorded))
            }
        }
    }

    fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
        if self.provider == ClipboardProvider::WaylandDataControl {
            return self.clear_served_if_matches(expected);
        }
        // X11 keeps arboard ownership and compares the live selection first.
        let OwnedSelection::Text(text) = expected else {
            return Ok(false);
        };
        if self.read_text()?.as_deref() != Some(text.as_str()) {
            return Ok(false);
        }
        self.release_text_selection()?;
        match self.read_text()? {
            None => Ok(true),
            Some(_) => Err(ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                "the clipboard selection could not be released",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPrivacyEvent {
    Locked,
    PreparingForSleep,
    Unlocked,
    Resumed,
}

fn watch_error(message: impl Into<String>) -> ClipboardError {
    ClipboardError::new(ClipboardErrorKind::ReadFailed, message)
}

fn x11_watch_error(error: impl fmt::Display) -> ClipboardError {
    watch_error(format!("clipboard change watch is unavailable: {error}"))
}

fn wayland_watch_error(error: impl fmt::Display) -> ClipboardError {
    watch_error(format!("clipboard change watch failed: {error}"))
}

/// A bounded Wayland data-control watcher that reports selection changes.
struct WaylandChangeWatch {
    watcher: WaylandWatcher,
    cancel: wl_clipboard_rs::watch::CancelHandle,
}

impl WaylandChangeWatch {
    fn new() -> Result<Self, ClipboardError> {
        let watcher =
            WaylandWatcher::new(WaylandWatchClipboardType::Regular, WaylandSeat::Unspecified)
                .map_err(x11_watch_error)?;
        let cancel = watcher.cancel_handle();
        Ok(Self { watcher, cancel })
    }
}

impl ClipboardChangeWatch for WaylandChangeWatch {
    fn next_change(&mut self) -> Result<bool, ClipboardError> {
        match self.watcher.next_event() {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(error) => Err(wayland_watch_error(error)),
        }
    }

    fn cancel_handle(&self) -> ChangeWatchCancel {
        let cancel = self.cancel.clone();
        ChangeWatchCancel::new(move || cancel.cancel())
    }
}

/// Reads the live X11 CLIPBOARD owner over a dedicated connection.
struct X11SelectionOwner {
    connection: RustConnection,
    clipboard: Atom,
}

impl X11SelectionOwner {
    fn new() -> Result<Self, ClipboardError> {
        let (connection, _) = RustConnection::connect(None).map_err(|_| x11_owner_error())?;
        let clipboard = connection
            .intern_atom(false, b"CLIPBOARD")
            .map_err(|_| x11_owner_error())?
            .reply()
            .map_err(|_| x11_owner_error())?
            .atom;
        Ok(Self {
            connection,
            clipboard,
        })
    }

    fn owner(&self) -> Result<Option<u32>, ClipboardError> {
        let owner = self
            .connection
            .get_selection_owner(self.clipboard)
            .map_err(|_| x11_owner_error())?
            .reply()
            .map_err(|_| x11_owner_error())?
            .owner;
        Ok((owner != x11rb::NONE).then_some(owner))
    }
}

fn x11_owner_error() -> ClipboardError {
    ClipboardError::new(
        ClipboardErrorKind::ReadFailed,
        "the X11 CLIPBOARD owner could not be confirmed",
    )
}

/// An XFixes selection watcher for the X11 compatibility path.
struct X11ChangeWatch {
    connection: RustConnection,
    clipboard: Atom,
    /// The first call reports the current selection, matching the Wayland
    /// watcher's initial event.
    initial: bool,
    cancel: Arc<OwnedFd>,
}

impl X11ChangeWatch {
    fn new() -> Result<Self, ClipboardError> {
        let (connection, screen) = RustConnection::connect(None).map_err(x11_watch_error)?;
        let root = connection.setup().roots[screen].root;
        connection
            .xfixes_query_version(5, 0)
            .map_err(x11_watch_error)?
            .reply()
            .map_err(x11_watch_error)?;
        let clipboard = connection
            .intern_atom(false, b"CLIPBOARD")
            .map_err(x11_watch_error)?
            .reply()
            .map_err(x11_watch_error)?
            .atom;
        let mask = SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE;
        connection
            .xfixes_select_selection_input(root, clipboard, mask)
            .map_err(x11_watch_error)?
            .check()
            .map_err(x11_watch_error)?;
        connection.flush().map_err(x11_watch_error)?;
        Ok(Self {
            connection,
            clipboard,
            initial: true,
            cancel: event_fd().map_err(|error| {
                watch_error(format!(
                    "the clipboard watch wake descriptor could not be created: {error}"
                ))
            })?,
        })
    }

    fn is_selection_change(&self, event: &X11Event) -> bool {
        matches!(
            event,
            X11Event::XfixesSelectionNotify(notify) if notify.selection == self.clipboard
        )
    }
}

impl ClipboardChangeWatch for X11ChangeWatch {
    fn next_change(&mut self) -> Result<bool, ClipboardError> {
        if self.initial {
            self.initial = false;
            return Ok(true);
        }
        loop {
            while let Some(event) = self.connection.poll_for_event().map_err(x11_watch_error)? {
                if self.is_selection_change(&event) {
                    return Ok(true);
                }
            }
            let mut descriptors = [
                libc::pollfd {
                    fd: self.connection.stream().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.cancel.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            wait_for_descriptors(&mut descriptors).map_err(x11_watch_error)?;
            if descriptors[1].revents != 0 {
                drain_event_fd(self.cancel.as_raw_fd());
                return Ok(false);
            }
            if descriptors[0].revents & libc::POLLIN == 0 {
                return Err(x11_watch_error("the X11 display connection closed"));
            }
        }
    }

    fn cancel_handle(&self) -> ChangeWatchCancel {
        let cancel = Arc::clone(&self.cancel);
        ChangeWatchCancel::new(move || signal_event_fd(cancel.as_raw_fd()))
    }
}

pub trait PrivacyEventSource: Send + 'static {
    fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError>;

    /// Takes blocking triggers for session state changes when the bus reports
    /// them. Polling remains the fallback.
    fn take_watches(&mut self) -> Result<Vec<Box<dyn PrivacyChangeWatch>>, ClipboardError> {
        Ok(Vec::new())
    }

    /// Records an edge a watch already delivered, so a later re-read neither
    /// repeats nor drops it.
    fn observe_edge(&mut self, event: SessionPrivacyEvent) {
        let _ = event;
    }
}

pub struct LogindPrivacyMonitor {
    connection: BusConnection,
    session_path: OwnedObjectPath,
    was_locked: bool,
    was_preparing_for_sleep: bool,
}

impl LogindPrivacyMonitor {
    pub fn new() -> Result<Self, ClipboardError> {
        let connection = BusConnectionBuilder::system()
            .map_err(privacy_error)?
            .method_timeout(PRIVACY_METHOD_TIMEOUT)
            .build()
            .map_err(privacy_error)?;
        let session_path = current_session_path(&connection).map_err(privacy_error)?;
        // The first poll re-reads the state, so a session already locked or
        // asleep at startup reports its hold instead of being missed.
        let _ = read_privacy_state(&connection, &session_path)?;
        Ok(Self {
            connection,
            session_path,
            was_locked: false,
            was_preparing_for_sleep: false,
        })
    }
}

impl PrivacyEventSource for LogindPrivacyMonitor {
    fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError> {
        let (locked, preparing_for_sleep) =
            read_privacy_state(&self.connection, &self.session_path)?;
        let mut events = Vec::new();
        if locked && !self.was_locked {
            events.push(SessionPrivacyEvent::Locked);
        }
        if !locked && self.was_locked {
            events.push(SessionPrivacyEvent::Unlocked);
        }
        if preparing_for_sleep && !self.was_preparing_for_sleep {
            events.push(SessionPrivacyEvent::PreparingForSleep);
        }
        if !preparing_for_sleep && self.was_preparing_for_sleep {
            events.push(SessionPrivacyEvent::Resumed);
        }
        self.was_locked = locked;
        self.was_preparing_for_sleep = preparing_for_sleep;
        Ok(events)
    }

    fn take_watches(&mut self) -> Result<Vec<Box<dyn PrivacyChangeWatch>>, ClipboardError> {
        Ok(vec![
            Box::new(LogindPrivacyWatch::new(self.session_path.clone())?),
            Box::new(LogindOwnerWatch::new()?),
        ])
    }

    fn observe_edge(&mut self, event: SessionPrivacyEvent) {
        match event {
            SessionPrivacyEvent::Locked => self.was_locked = true,
            SessionPrivacyEvent::Unlocked => self.was_locked = false,
            SessionPrivacyEvent::PreparingForSleep => self.was_preparing_for_sleep = true,
            SessionPrivacyEvent::Resumed => self.was_preparing_for_sleep = false,
        }
    }
}

/// Reports logind property changes from a dedicated connection.
struct LogindPrivacyWatch {
    connection: BusConnection,
    shutdown: zbus::Connection,
    session_path: OwnedObjectPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivacySignal {
    /// Not a property or signal this watch cares about.
    Ignored,
    /// The property was invalidated; read the current state.
    Reread,
    Edge(SessionPrivacyEvent),
}

impl LogindPrivacyWatch {
    fn new(session_path: OwnedObjectPath) -> Result<Self, ClipboardError> {
        let connection = BusConnectionBuilder::system()
            .map_err(privacy_error)?
            .method_timeout(PRIVACY_METHOD_TIMEOUT)
            .build()
            .map_err(privacy_error)?;
        let shutdown = connection.inner().clone();
        Ok(Self {
            connection,
            shutdown,
            session_path,
        })
    }

    fn decode(&self, message: &zbus::Message) -> PrivacySignal {
        let header = message.header();
        let interface = header.interface().map(|interface| interface.as_str());
        let member = header.member().map(|member| member.as_str());
        match (interface, member) {
            // Sleep is a signal only: the manager property refuses changes.
            (Some(MANAGER_INTERFACE), Some("PrepareForSleep")) => {
                match message.body().deserialize::<bool>() {
                    Ok(true) => PrivacySignal::Edge(SessionPrivacyEvent::PreparingForSleep),
                    Ok(false) => PrivacySignal::Edge(SessionPrivacyEvent::Resumed),
                    Err(_) => PrivacySignal::Reread,
                }
            }
            (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
                let Ok((changed_interface, changed, invalidated)) =
                    message
                        .body()
                        .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                else {
                    return PrivacySignal::Reread;
                };
                let path = header.path().map(|path| path.as_str());
                if changed_interface != SESSION_INTERFACE
                    || path != Some(self.session_path.as_str())
                {
                    return PrivacySignal::Ignored;
                }
                match changed.get("LockedHint") {
                    Some(value) => match value.downcast_ref::<bool>() {
                        Ok(true) => PrivacySignal::Edge(SessionPrivacyEvent::Locked),
                        Ok(false) => PrivacySignal::Edge(SessionPrivacyEvent::Unlocked),
                        Err(_) => PrivacySignal::Reread,
                    },
                    None if invalidated.iter().any(|name| name == "LockedHint") => {
                        PrivacySignal::Reread
                    }
                    None => PrivacySignal::Ignored,
                }
            }
            _ => PrivacySignal::Ignored,
        }
    }
}

impl PrivacyChangeWatch for LogindPrivacyWatch {
    fn run(&mut self, changed: &mut dyn FnMut(Option<SessionPrivacyEvent>) -> bool) -> bool {
        let Ok(rule) = privacy_match_rule() else {
            return false;
        };
        let Ok(messages) = MessageIterator::for_match_rule(rule, &self.connection, Some(32)) else {
            return false;
        };
        // Read the current state once the rule exists so a change during
        // startup is not missed between the worker's first poll and the rule.
        if !changed(None) {
            return true;
        }
        let mut last = None;
        for message in messages {
            let Ok(message) = message else {
                return false;
            };
            let event = match self.decode(&message) {
                PrivacySignal::Ignored => continue,
                PrivacySignal::Reread => None,
                PrivacySignal::Edge(event) => Some(event),
            };
            // Coalesce only an adjacent duplicate; opposite edges both go.
            if event.is_some() && event == last {
                continue;
            }
            last = event;
            if !changed(event) {
                return true;
            }
        }
        false
    }

    fn cancel_handle(&self) -> ChangeWatchCancel {
        let shutdown = self.shutdown.clone();
        ChangeWatchCancel::new(move || {
            let connection = shutdown.clone();
            let _ = async_io::block_on(connection.close());
        })
    }
}

/// Reports when logind leaves the bus, so history can fail closed.
struct LogindOwnerWatch {
    connection: BusConnection,
    shutdown: zbus::Connection,
}

impl LogindOwnerWatch {
    fn new() -> Result<Self, ClipboardError> {
        let connection = BusConnectionBuilder::system()
            .map_err(privacy_error)?
            .method_timeout(PRIVACY_METHOD_TIMEOUT)
            .build()
            .map_err(privacy_error)?;
        let shutdown = connection.inner().clone();
        Ok(Self {
            connection,
            shutdown,
        })
    }
}

impl PrivacyChangeWatch for LogindOwnerWatch {
    fn run(&mut self, changed: &mut dyn FnMut(Option<SessionPrivacyEvent>) -> bool) -> bool {
        let Ok(rule) = logind_owner_match_rule() else {
            return false;
        };
        let Ok(messages) = MessageIterator::for_match_rule(rule, &self.connection, Some(8)) else {
            return false;
        };
        for message in messages {
            let Ok(message) = message else {
                return false;
            };
            match logind_owner_lost(&message) {
                Some(true) => return false,
                // A new owner replaced the old one; read it once more.
                Some(false) if !changed(None) => return true,
                _ => {}
            }
        }
        false
    }

    fn cancel_handle(&self) -> ChangeWatchCancel {
        let shutdown = self.shutdown.clone();
        ChangeWatchCancel::new(move || {
            let connection = shutdown.clone();
            let _ = async_io::block_on(connection.close());
        })
    }
}

fn privacy_match_rule() -> Result<MatchRule<'static>, zbus::Error> {
    // One rule for both the sleep signal and the session property changes; the
    // watch filters the members it needs.
    Ok(MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(LOGIN_DESTINATION)?
        .path_namespace(LOGIN_PATH)?
        .build())
}

fn logind_owner_match_rule() -> Result<MatchRule<'static>, zbus::Error> {
    Ok(MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(LOGIN_DESTINATION)?
        .build())
}

/// Whether a `NameOwnerChanged` release left logind without an owner.
fn logind_owner_lost(message: &zbus::Message) -> Option<bool> {
    let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>() else {
        return None;
    };
    (name == LOGIN_DESTINATION).then_some(new.is_empty())
}

fn read_privacy_state(
    connection: &BusConnection,
    session_path: &OwnedObjectPath,
) -> Result<(bool, bool), ClipboardError> {
    let session = Proxy::new(
        connection,
        LOGIN_DESTINATION,
        session_path.as_str(),
        SESSION_INTERFACE,
    )
    .map_err(privacy_error)?;
    let manager = Proxy::new(connection, LOGIN_DESTINATION, LOGIN_PATH, MANAGER_INTERFACE)
        .map_err(privacy_error)?;
    let locked = session.get_property("LockedHint").map_err(privacy_error)?;
    let preparing_for_sleep = manager
        .get_property("PreparingForSleep")
        .map_err(privacy_error)?;
    Ok((locked, preparing_for_sleep))
}

fn privacy_error(error: impl fmt::Display) -> ClipboardError {
    ClipboardError::new(
        ClipboardErrorKind::PrivacyMonitorFailed,
        format!("logind privacy state is unavailable: {error}"),
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClipboardCapabilityProbe;

impl ClipboardCapabilityProbe {
    pub fn new() -> Self {
        Self
    }
}

impl CapabilityProbe for ClipboardCapabilityProbe {
    fn probe(&self) -> CapabilityReport {
        capability_report(
            discover_provider(),
            LogindPrivacyMonitor::new().map(|_| ()),
            crate::sandbox::flatpak(),
        )
    }
}

pub fn discover_provider() -> Result<ClipboardProvider, ClipboardError> {
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        match get_mime_types(WaylandClipboardType::Regular, WaylandSeat::Unspecified) {
            Ok(_) | Err(WaylandError::ClipboardEmpty) => {
                return Ok(ClipboardProvider::WaylandDataControl);
            }
            Err(WaylandError::MissingProtocol { .. })
            | Err(WaylandError::NoSeats)
            | Err(WaylandError::SocketOpenError(_))
            | Err(WaylandError::WaylandConnection(_))
            | Err(WaylandError::WaylandCommunication(_))
            | Err(WaylandError::PrimarySelectionUnsupported)
            | Err(WaylandError::NoMimeType)
            | Err(WaylandError::SeatNotFound)
            | Err(WaylandError::PipeCreation(_)) => {}
        }
    }
    let (connection, _) = RustConnection::connect(None).map_err(|_| {
        ClipboardError::new(
            ClipboardErrorKind::Unavailable,
            "neither Wayland data-control nor an X11 display is reachable",
        )
    })?;
    let clipboard = connection
        .intern_atom(false, b"CLIPBOARD")
        .map_err(|_| {
            ClipboardError::new(
                ClipboardErrorKind::Unavailable,
                "the X11 CLIPBOARD atom request failed",
            )
        })?
        .reply()
        .map_err(|_| {
            ClipboardError::new(
                ClipboardErrorKind::Unavailable,
                "the X11 CLIPBOARD atom reply failed",
            )
        })?;
    connection
        .get_selection_owner(clipboard.atom)
        .map_err(|_| {
            ClipboardError::new(
                ClipboardErrorKind::Unavailable,
                "the X11 CLIPBOARD selection request failed",
            )
        })?
        .reply()
        .map_err(|_| {
            ClipboardError::new(
                ClipboardErrorKind::Unavailable,
                "the X11 CLIPBOARD selection reply failed",
            )
        })?;
    Ok(ClipboardProvider::X11)
}

pub fn kind_support_for(provider: ClipboardProvider) -> ClipboardKindSupport {
    match provider {
        ClipboardProvider::WaylandDataControl => ClipboardKindSupport::ALL,
        ClipboardProvider::X11 => ClipboardKindSupport::TEXT_ONLY,
    }
}

/// Whether a change watch can be bound for `provider`, without keeping it open.
pub fn watch_mode_for(provider: ClipboardProvider) -> ClipboardWatchMode {
    let available = match provider {
        ClipboardProvider::WaylandDataControl => WaylandChangeWatch::new().is_ok(),
        ClipboardProvider::X11 => X11ChangeWatch::new().is_ok(),
    };
    if available {
        ClipboardWatchMode::Events
    } else {
        ClipboardWatchMode::Polling
    }
}

fn capability_report(
    provider: Result<ClipboardProvider, ClipboardError>,
    privacy: Result<(), ClipboardError>,
    sandboxed: bool,
) -> CapabilityReport {
    match (provider, privacy) {
        (Ok(provider), Ok(())) => {
            let support = kind_support_for(provider);
            let watch = watch_mode_for(provider);
            let mut report = CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                format!(
                    "{} and mandatory lock/sleep clearing are available.",
                    provider.label()
                ),
            )
            .with_selected_backend(provider.label())
            .with_alternative("X11 CLIPBOARD selection")
            .with_evidence(CapabilityEvidence::new("persistent_storage", "false"))
            .with_evidence(CapabilityEvidence::new(
                "source_application_identity",
                "unavailable",
            ))
            .with_evidence(CapabilityEvidence::new("lock_sleep_clear", "available"))
            .with_evidence(CapabilityEvidence::new(
                "watch_mode",
                watch.evidence_value(),
            ))
            .with_evidence(CapabilityEvidence::new(
                "entry_kinds",
                support.evidence_value(),
            ));
            if !support.files || !support.image {
                report = report.with_remediation(
                    "Rich clipboard entries need Wayland data-control; the X11 compatibility \
                     path captures text only.",
                );
            }
            report
        }
        (provider, privacy) => {
            let provider_failed = provider.is_err();
            let reason = provider
                .err()
                .or_else(|| privacy.err())
                .map(|error| error.message)
                .unwrap_or_else(|| "clipboard history prerequisites are unavailable".to_string());
            if sandboxed && provider_failed {
                return crate::sandbox::unsupported(
                    FEATURE_ID,
                    "Privacy-preserving clipboard history is unavailable.",
                    crate::sandbox::SandboxReason::ClipboardAccess,
                )
                .with_evidence(CapabilityEvidence::new(
                    "source_application_identity",
                    "unavailable",
                ));
            }
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: reason.clone(),
                },
                "Privacy-preserving clipboard history is unavailable.",
            )
            .with_remediation(
                "Run the app in a logind graphical session with Wayland data-control or X11 access.",
            )
            .with_evidence(CapabilityEvidence::new(
                "source_application_identity",
                "unavailable",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::CapabilityStatus;

    use super::{
        ClipboardEntryKind, ClipboardError, ClipboardErrorKind, ClipboardKindSupport,
        ClipboardProvider, FEATURE_ID, capability_report, encode_uri_list, kind_support_for,
        parse_uri_list, png_dimensions,
    };

    const PNG_HEADER: [u8; 34] = [
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, // signature
        0x00, 0x00, 0x00, 0x0d, // IHDR length
        b'I', b'H', b'D', b'R', // chunk type
        0x00, 0x00, 0x07, 0x80, // width 1920
        0x00, 0x00, 0x04, 0x38, // height 1080
        0x08, 0x06, 0x00, 0x00, 0x00, // PNG format fields
        0x00, 0x00, 0x00, 0x00, 0x00, // CRC placeholder
    ];

    #[test]
    fn png_dimensions_are_read_from_the_header_without_decoding() {
        assert_eq!(png_dimensions(&PNG_HEADER), Some((1920, 1080)));
        assert_eq!(png_dimensions(b"not a png at all"), None);
        assert_eq!(png_dimensions(&[]), None);

        let mut zero_width = PNG_HEADER;
        zero_width[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(png_dimensions(&zero_width), None);
    }

    #[test]
    fn uri_list_parsing_is_bounded_deduped_and_local_only() {
        let payload = "# comment\r\nfile:///home/user/one.txt\r\nfile:///tmp/two%20words.pdf\r\n\
                       https://example.com/not-a-file\r\nfile:///home/user/one.txt\r\n\r\n";

        let paths = parse_uri_list(payload, 8);
        assert_eq!(
            paths,
            vec![
                "/home/user/one.txt".to_string(),
                "/tmp/two words.pdf".to_string()
            ],
            "remote URLs are skipped, duplicates collapse, and percent escapes decode"
        );
        assert_eq!(parse_uri_list(payload, 1).len(), 1);
        assert!(parse_uri_list("bogus\r\n", 4).is_empty());
    }

    #[test]
    fn uri_list_encoding_round_trips_through_parsing() {
        let paths = vec![
            "/home/user/report final.pdf".to_string(),
            "/tmp/a#b?c".to_string(),
        ];

        let encoded = encode_uri_list(&paths);

        assert!(encoded.ends_with("\r\n"));
        assert!(!encoded.contains(' '));
        assert_eq!(parse_uri_list(&encoded, 8), paths);
    }

    #[test]
    fn kind_support_is_per_provider() {
        assert_eq!(
            kind_support_for(ClipboardProvider::WaylandDataControl),
            ClipboardKindSupport::ALL
        );
        assert_eq!(
            kind_support_for(ClipboardProvider::X11),
            ClipboardKindSupport::TEXT_ONLY
        );
        assert!(ClipboardKindSupport::TEXT_ONLY.supports(ClipboardEntryKind::Text));
        assert!(!ClipboardKindSupport::TEXT_ONLY.supports(ClipboardEntryKind::Image));
        assert_eq!(
            ClipboardKindSupport::ALL.evidence_value(),
            "text,image,files"
        );
        assert_eq!(ClipboardKindSupport::TEXT_ONLY.evidence_value(), "text");
    }

    #[test]
    fn supported_report_is_redacted_and_names_unavailable_source_identity() {
        let report = capability_report(Ok(ClipboardProvider::WaylandDataControl), Ok(()), false);

        assert_eq!(report.feature_id, FEATURE_ID);
        assert_eq!(report.status, CapabilityStatus::Supported);
        assert!(
            report.evidence.iter().any(
                |item| item.key == "source_application_identity" && item.value == "unavailable"
            )
        );
        assert!(
            report
                .evidence
                .iter()
                .any(|item| item.key == "entry_kinds" && item.value == "text,image,files")
        );
        assert!(!report.summary.contains("WAYLAND_DISPLAY"));
        assert!(report.remediation.is_none());
    }

    #[test]
    fn x11_support_stays_text_only_and_reports_remediation() {
        let report = capability_report(Ok(ClipboardProvider::X11), Ok(()), false);

        assert!(
            report
                .evidence
                .iter()
                .any(|item| { item.key == "entry_kinds" && item.value == "text" })
        );
        assert!(
            report
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("text only")),
            "the reduced X11 support must be explained, not hidden"
        );
    }

    #[test]
    fn mandatory_privacy_monitor_failure_disables_history() {
        let report = capability_report(
            Ok(ClipboardProvider::X11),
            Err(ClipboardError {
                kind: ClipboardErrorKind::PrivacyMonitorFailed,
                message: "logind unavailable".to_string(),
            }),
            false,
        );

        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
    }

    #[test]
    fn a_sandboxed_probe_names_missing_clipboard_access() {
        let unavailable = || {
            Err(ClipboardError {
                kind: ClipboardErrorKind::Unavailable,
                message: "neither Wayland data-control nor an X11 display is reachable".to_string(),
            })
        };
        let report = capability_report(unavailable(), Ok(()), true);
        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(
            report
                .evidence
                .iter()
                .any(|item| item.key == "sandbox" && item.value == "clipboard-access")
        );

        // A privacy-monitor fault is not a sandbox restriction.
        let report = capability_report(
            Ok(ClipboardProvider::X11),
            Err(ClipboardError {
                kind: ClipboardErrorKind::PrivacyMonitorFailed,
                message: "logind unavailable".to_string(),
            }),
            true,
        );
        assert!(
            !report
                .evidence
                .iter()
                .any(|item| item.key == "sandbox" && item.value == "clipboard-access")
        );
    }
}
