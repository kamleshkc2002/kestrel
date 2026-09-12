//! Capability-gated clipboard ownership and session privacy events.

use std::{env, error::Error, fmt, time::Duration};

use arboard::Clipboard;
use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use wl_clipboard_rs::paste::{
    ClipboardType as WaylandClipboardType, Error as WaylandError, Seat as WaylandSeat,
    get_mime_types,
};
use x11rb::{protocol::xproto::ConnectionExt, rust_connection::RustConnection};
use zbus::{
    blocking::{Connection as BusConnection, Proxy, connection::Builder as BusConnectionBuilder},
    zvariant::OwnedObjectPath,
};

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "clipboard.history";
const PRIVACY_METHOD_TIMEOUT: Duration = Duration::from_millis(500);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardErrorKind {
    Unavailable,
    ContentUnavailable,
    ReadFailed,
    WriteFailed,
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

pub trait ClipboardBackend: Send + 'static {
    fn provider(&self) -> ClipboardProvider;
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;
    fn clear_if_matches(&mut self, expected: &str) -> Result<bool, ClipboardError>;
}

pub struct ArboardClipboardBackend {
    provider: ClipboardProvider,
    clipboard: Clipboard,
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
        })
    }
}

impl ClipboardBackend for ArboardClipboardBackend {
    fn provider(&self) -> ClipboardProvider {
        self.provider
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
        self.clipboard.set_text(text.to_owned()).map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard ownership failed: {error}"),
            )
        })
    }

    fn clear_if_matches(&mut self, expected: &str) -> Result<bool, ClipboardError> {
        if self.read_text()?.as_deref() != Some(expected) {
            return Ok(false);
        }
        self.clipboard.clear().map_err(|error| {
            ClipboardError::new(
                ClipboardErrorKind::WriteFailed,
                format!("clipboard clear failed: {error}"),
            )
        })?;
        Ok(true)
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

fn capability_report(
    provider: Result<ClipboardProvider, ClipboardError>,
    privacy: Result<(), ClipboardError>,
) -> CapabilityReport {
    match (provider, privacy) {
        (Ok(provider), Ok(())) => CapabilityReport::new(
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
        .with_evidence(CapabilityEvidence::new("lock_sleep_clear", "available")),
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
        ClipboardError, ClipboardErrorKind, ClipboardProvider, FEATURE_ID, capability_report,
    };

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
        assert!(!report.summary.contains("WAYLAND_DISPLAY"));
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
