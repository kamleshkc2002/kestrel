//! Capability-gated clipboard ownership and session privacy events.
//!
//! Text is read, owned, and released through `arboard`, which keeps selection
//! ownership for the lifetime of its connection. Rich entries (images and file
//! lists) are transferred as bounded payloads over the same Wayland
//! data-control protocol so Kestrel never has to decode or re-encode them:
//! an image entry is the original PNG bytes, and a file entry is the
//! `text/uri-list` payload.

use std::{
    env,
    error::Error,
    fmt,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
};
use x11rb::{protocol::xproto::ConnectionExt, rust_connection::RustConnection};
use zbus::{
    blocking::{Connection as BusConnection, Proxy, connection::Builder as BusConnectionBuilder},
    zvariant::OwnedObjectPath,
};

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "clipboard.history";
const PRIVACY_METHOD_TIMEOUT: Duration = Duration::from_millis(500);
/// The canonical PNG MIME type offered and requested on Wayland.
pub const PNG_MIME: &str = "image/png";
/// The canonical file-list MIME type offered and requested on Wayland.
pub const URI_LIST_MIME: &str = "text/uri-list";
/// The text MIME types Kestrel offers when it owns a text selection.
const TEXT_MIMES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
];
/// How long a released selection may take to stop being served.
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

/// The kinds of clipboard entry Kestrel can read, own, and restore.
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

    /// The evidence key this kind reports through capability reports.
    pub const fn evidence_key(self) -> &'static str {
        match self {
            Self::Text => "text_entries",
            Self::Image => "image_entries",
            Self::Files => "file_entries",
        }
    }
}

/// Which entry kinds one provider can capture and re-own.
///
/// Support is per capability: Wayland data-control can carry PNG images and
/// `text/uri-list` payloads, while the X11 compatibility path is text-only
/// because richer formats would require decoding and re-encoding pixels.
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

    /// A compact evidence value such as `text,image,files`.
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

/// A selection Kestrel captured and therefore owns until something replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedSelection {
    Text(String),
    ImagePng(Vec<u8>),
    Files(Vec<String>),
}

impl OwnedSelection {
    pub const fn kind(&self) -> ClipboardEntryKind {
        match self {
            Self::Text(_) => ClipboardEntryKind::Text,
            Self::ImagePng(_) => ClipboardEntryKind::Image,
            Self::Files(_) => ClipboardEntryKind::Files,
        }
    }

    pub fn size_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::ImagePng(bytes) => bytes.len(),
            Self::Files(paths) => paths.iter().map(String::len).sum(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardErrorKind {
    Unavailable,
    ContentUnavailable,
    ReadFailed,
    WriteFailed,
    /// The selection exists but is larger than the caller's bound.
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

/// Validates the PNG signature and returns the IHDR dimensions without decoding.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

/// Parses a `text/uri-list` payload into bounded local file paths.
///
/// Comments (`#`) and blank lines are ignored, non-`file://` entries are
/// skipped rather than fetched, duplicates are dropped, and at most
/// `max_entries` paths are returned.
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

/// Encodes local paths as a `text/uri-list` payload.
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

pub trait ClipboardBackend: Send + 'static {
    fn provider(&self) -> ClipboardProvider;

    /// The entry kinds this provider can capture, own, and restore.
    fn kind_support(&self) -> ClipboardKindSupport {
        ClipboardKindSupport::TEXT_ONLY
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;

    /// The highest-priority kind the live selection currently offers.
    ///
    /// `Ok(None)` means the provider cannot enumerate selection types, so the
    /// caller must fall back to reading text.
    fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
        Ok(None)
    }

    /// Reads at most `max_bytes` of PNG data; `None` when no image is offered.
    fn read_image_png(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClipboardError> {
        let _ = max_bytes;
        Err(unsupported_kind(ClipboardEntryKind::Image, self.provider()))
    }

    fn write_image_png(&mut self, bytes: &[u8]) -> Result<(), ClipboardError> {
        let _ = bytes;
        Err(unsupported_kind(ClipboardEntryKind::Image, self.provider()))
    }

    /// Reads at most `max_entries` local paths; `None` when no file list is offered.
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

    /// Clears the live selection only when it still is exactly `expected`.
    fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError>;
}

/// A selection Kestrel is currently serving on Wayland, plus its serving thread.
struct ServedSelection {
    owned: OwnedSelection,
    handle: Option<JoinHandle<()>>,
    _wake: Arc<AtomicBool>,
}

impl ServedSelection {
    /// True while Kestrel still owns the selection (nobody took it over).
    fn is_alive(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

pub struct ArboardClipboardBackend {
    provider: ClipboardProvider,
    clipboard: Clipboard,
    /// Rich selections are served by a dedicated data-control connection.
    served: Option<ServedSelection>,
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
        })
    }

    /// Releases any served rich selection, waiting briefly for the server loop
    /// to observe the takeover. Never blocks indefinitely.
    fn release_served(&mut self) {
        let Some(mut served) = self.served.take() else {
            return;
        };
        let cleared = clear(
            WaylandCopyClipboardType::Regular,
            wl_clipboard_rs::copy::Seat::All,
        );
        let _ = cleared;
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
        // A still-running serve loop is detached rather than joined: it exits
        // as soon as it observes the takeover, and blocking here would stall the
        // clipboard worker.
    }

    /// Serves `owned` over data-control until something replaces the selection.
    fn serve(&mut self, owned: OwnedSelection) -> Result<(), ClipboardError> {
        let mut options = Options::new();
        options.foreground(true);
        let prepared = match &owned {
            // Text is offered under every common plain-text MIME type so that
            // any consumer can paste it.
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
        let wake = Arc::new(AtomicBool::new(false));
        let thread_wake = Arc::clone(&wake);
        let handle = thread::Builder::new()
            .name("kestrel-clipboard-serve".to_string())
            .spawn(move || {
                let _ = prepared.serve();
                thread_wake.store(true, Ordering::Relaxed);
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
            _wake: wake,
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

    /// Releases the text selection through data-control, falling back to arboard.
    ///
    /// The text selection is created by arboard's own data-control connection;
    /// clearing it with the same protocol family is the path that actually
    /// removes the source from the compositor.
    fn release_text_selection(&mut self) -> Result<(), ClipboardError> {
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

    /// Releases a rich selection Kestrel still owns when it matches `expected`.
    fn clear_served_if_matches(
        &mut self,
        expected: &OwnedSelection,
    ) -> Result<bool, ClipboardError> {
        let Some(served) = self.served.as_ref() else {
            return Ok(false);
        };
        if served.owned != *expected || !served.is_alive() {
            // Something else replaced the selection, or Kestrel never owned it.
            self.served = None;
            return Ok(false);
        }
        // `release_served` tears down Kestrel's own data-control source, which
        // is the only selection Kestrel is allowed to clear. A foreign source
        // that still exposes the same bytes (an X11 bridge, for example) is not
        // Kestrel's to release, and re-published content is visible to the user
        // because the next poll captures it again.
        self.release_served();
        Ok(true)
    }
}

impl ClipboardBackend for ArboardClipboardBackend {
    fn provider(&self) -> ClipboardProvider {
        self.provider
    }

    fn kind_support(&self) -> ClipboardKindSupport {
        match self.provider {
            ClipboardProvider::WaylandDataControl => ClipboardKindSupport::ALL,
            ClipboardProvider::X11 => ClipboardKindSupport::TEXT_ONLY,
        }
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
        // Taking over the selection also ends whatever Kestrel served before.
        if self.served.is_some() {
            self.release_served();
        }
        if self.provider == ClipboardProvider::WaylandDataControl {
            // One ownership mechanism for every kind: Kestrel serves the
            // selection it owns and releases it through the same path.
            return self.serve(OwnedSelection::Text(text.to_owned()));
        }
        self.clipboard.set_text(text.to_owned()).map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard ownership failed: {error}"),
            )
        })
    }

    fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Ok(None);
        }
        match get_mime_types(WaylandClipboardType::Regular, WaylandSeat::Unspecified) {
            Ok(types) => {
                let offered = if types.contains(URI_LIST_MIME) {
                    Some(ClipboardEntryKind::Files)
                } else if types.contains(PNG_MIME) {
                    Some(ClipboardEntryKind::Image)
                } else if types.iter().any(|mime| is_text_mime(mime)) {
                    Some(ClipboardEntryKind::Text)
                } else {
                    None
                };
                Ok(offered)
            }
            Err(WaylandError::ClipboardEmpty) | Err(WaylandError::NoMimeType) => Ok(None),
            Err(error) => Err(ClipboardError::new(
                ClipboardErrorKind::ContentUnavailable,
                format!("clipboard type enumeration failed: {error}"),
            )),
        }
    }

    fn read_image_png(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClipboardError> {
        if self.provider != ClipboardProvider::WaylandDataControl {
            return Err(unsupported_kind(ClipboardEntryKind::Image, self.provider));
        }
        // Never read a selection Kestrel itself is serving: the payload is already known.
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
        // A file list is text; bound the raw payload generously before parsing.
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

    fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
        if self.provider == ClipboardProvider::WaylandDataControl {
            return self.clear_served_if_matches(expected);
        }
        // The X11 compatibility path keeps arboard's ownership and compares the
        // live selection before releasing it.
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
}

pub trait PrivacyEventSource: Send + 'static {
    fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError>;
}

pub struct LogindPrivacyMonitor {
    connection: BusConnection,
    session_path: OwnedObjectPath,
    was_locked: bool,
    was_preparing_for_sleep: bool,
}

impl LogindPrivacyMonitor {
    pub fn new() -> Result<Self, ClipboardError> {
        let session_id = env::var("XDG_SESSION_ID").map_err(|_| {
            ClipboardError::new(
                ClipboardErrorKind::PrivacyMonitorFailed,
                "XDG_SESSION_ID is unavailable",
            )
        })?;
        let connection = BusConnectionBuilder::system()
            .map_err(privacy_error)?
            .method_timeout(PRIVACY_METHOD_TIMEOUT)
            .build()
            .map_err(privacy_error)?;
        let manager = Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .map_err(privacy_error)?;
        let session_path: OwnedObjectPath = manager
            .call("GetSession", &(session_id))
            .map_err(privacy_error)?;
        let (was_locked, was_preparing_for_sleep) = read_privacy_state(&connection, &session_path)?;
        Ok(Self {
            connection,
            session_path,
            was_locked,
            was_preparing_for_sleep,
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
        if preparing_for_sleep && !self.was_preparing_for_sleep {
            events.push(SessionPrivacyEvent::PreparingForSleep);
        }
        self.was_locked = locked;
        self.was_preparing_for_sleep = preparing_for_sleep;
        Ok(events)
    }
}

fn read_privacy_state(
    connection: &BusConnection,
    session_path: &OwnedObjectPath,
) -> Result<(bool, bool), ClipboardError> {
    let session = Proxy::new(
        connection,
        "org.freedesktop.login1",
        session_path.as_str(),
        "org.freedesktop.login1.Session",
    )
    .map_err(privacy_error)?;
    let manager = Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
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
        capability_report(discover_provider(), LogindPrivacyMonitor::new().map(|_| ()))
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

/// The entry-kind support reported for a provider.
pub fn kind_support_for(provider: ClipboardProvider) -> ClipboardKindSupport {
    match provider {
        ClipboardProvider::WaylandDataControl => ClipboardKindSupport::ALL,
        ClipboardProvider::X11 => ClipboardKindSupport::TEXT_ONLY,
    }
}

fn capability_report(
    provider: Result<ClipboardProvider, ClipboardError>,
    privacy: Result<(), ClipboardError>,
) -> CapabilityReport {
    match (provider, privacy) {
        (Ok(provider), Ok(())) => {
            let support = kind_support_for(provider);
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
            let reason = provider
                .err()
                .or_else(|| privacy.err())
                .map(|error| error.message)
                .unwrap_or_else(|| "clipboard history prerequisites are unavailable".to_string());
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: reason.clone(),
                },
                "Privacy-preserving clipboard history is unavailable.",
            )
            .with_remediation(
                "Run Kestrel in a logind graphical session with Wayland data-control or X11 access.",
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
        ClipboardProvider, FEATURE_ID, OwnedSelection, capability_report, encode_uri_list,
        kind_support_for, parse_uri_list, png_dimensions,
    };

    const PNG_HEADER: [u8; 34] = [
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, // signature
        0x00, 0x00, 0x00, 0x0d, // IHDR length
        b'I', b'H', b'D', b'R', // chunk type
        0x00, 0x00, 0x07, 0x80, // width 1920
        0x00, 0x00, 0x04, 0x38, // height 1080
        0x08, 0x06, 0x00, 0x00, 0x00, // bit depth, color type, compression, filter, interlace
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
    fn owned_selection_reports_kind_and_size() {
        assert_eq!(
            OwnedSelection::Text("abcd".to_string()).kind(),
            ClipboardEntryKind::Text
        );
        assert_eq!(OwnedSelection::ImagePng(vec![0; 12]).size_bytes(), 12);
        assert_eq!(
            OwnedSelection::Files(vec!["/a".to_string(), "/bb".to_string()]).size_bytes(),
            5
        );
    }

    #[test]
    fn supported_report_is_redacted_and_names_unavailable_source_identity() {
        let report = capability_report(Ok(ClipboardProvider::WaylandDataControl), Ok(()));

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
        let report = capability_report(Ok(ClipboardProvider::X11), Ok(()));

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
        );

        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
    }
}
