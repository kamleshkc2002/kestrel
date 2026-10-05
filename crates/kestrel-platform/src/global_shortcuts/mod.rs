//! Global shortcuts through the `GlobalShortcuts` portal or, on real X11
//! sessions, `XGrabKey`. Probing only reads the portal version and the session
//! type; sessions and grabs exist only while the feature runs.

mod portal;
mod x11;

use std::{
    env, fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, ShortcutTrigger};
use zbus::{blocking::connection::Builder, zvariant::OwnedValue};

use crate::CapabilityProbe;

pub use portal::PortalShortcutBackend;
pub use x11::X11ShortcutBackend;

pub const FEATURE_ID: &str = "global.shortcuts";
/// The application ID registered with the portal and used as the bus name.
pub const APPLICATION_ID: &str = "io.github.kamleshkc2002.Kestrel";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutProvider {
    Portal,
    X11,
}

impl ShortcutProvider {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Portal => "GlobalShortcuts portal",
            Self::X11 => "X11 key grabs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Wayland,
    X11,
    Unknown,
}

impl SessionKind {
    /// Reads `XDG_SESSION_TYPE`, then the display variables.
    pub fn detect() -> Self {
        Self::from_environment(
            env::var("XDG_SESSION_TYPE").ok().as_deref(),
            env::var_os("WAYLAND_DISPLAY").is_some(),
            env::var_os("DISPLAY").is_some(),
        )
    }

    pub fn from_environment(session_type: Option<&str>, wayland: bool, display: bool) -> Self {
        match session_type {
            Some("wayland") => Self::Wayland,
            Some("x11") => Self::X11,
            _ if wayland => Self::Wayland,
            _ if display => Self::X11,
            _ => Self::Unknown,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
            Self::Unknown => "unknown",
        }
    }
}

/// What the session offers for global shortcuts, without binding anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutDetection {
    pub session: SessionKind,
    /// The portal's `version` property, when the interface is exported.
    pub portal_version: Option<u32>,
    /// A display is reachable for X11 grabs.
    pub x11_display: bool,
}

impl ShortcutDetection {
    pub fn detect() -> Self {
        Self {
            session: SessionKind::detect(),
            portal_version: portal_version(),
            x11_display: env::var_os("DISPLAY").is_some(),
        }
    }

    /// The portal wins where it exists; X11 grabs are used only on real X11,
    /// because Xwayland grabs do not see keys pressed in Wayland clients.
    pub fn provider(&self) -> Option<ShortcutProvider> {
        if self.portal_version.is_some() {
            Some(ShortcutProvider::Portal)
        } else if self.session == SessionKind::X11 && self.x11_display {
            Some(ShortcutProvider::X11)
        } else {
            None
        }
    }
}

fn portal_version() -> Option<u32> {
    let connection = Builder::session()
        .ok()?
        .method_timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.portal.GlobalShortcuts", "version"),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}

/// The capability report for a detection result.
pub fn capability_for(detection: &ShortcutDetection) -> CapabilityReport {
    let evidence = [
        CapabilityEvidence::new("session_type", detection.session.label()),
        CapabilityEvidence::new(
            "portal_version",
            detection
                .portal_version
                .map_or_else(|| "absent".to_owned(), |version| version.to_string()),
        ),
    ];
    let report = match detection.provider() {
        Some(provider) => CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Supported,
            format!(
                "Global shortcuts can be registered through the {}.",
                provider.label()
            ),
        )
        .with_selected_backend(provider.label()),
        None => CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Unsupported {
                reason: "This session exports no GlobalShortcuts portal, and X11 key grabs \
                         need a real X11 session."
                    .to_owned(),
            },
            "Global shortcuts are unavailable in this session.",
        )
        .with_remediation(
            "Bind `kestrel --command <id>` in your desktop's custom-shortcut settings, or use a \
             desktop whose portal backend implements GlobalShortcuts (KDE Plasma, GNOME 48+, \
             Hyprland).",
        ),
    };
    evidence
        .into_iter()
        .fold(report, |report, item| report.with_evidence(item))
        .with_alternative("kestrel --command <id> in desktop shortcut settings")
}

/// Read-only probe for `global.shortcuts`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobalShortcutsProbe;

impl CapabilityProbe for GlobalShortcutsProbe {
    fn probe(&self) -> CapabilityReport {
        capability_for(&ShortcutDetection::detect())
    }
}

/// One shortcut to register: the stable command ID doubles as the shortcut ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRequest {
    pub id: String,
    pub description: String,
    pub trigger: ShortcutTrigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingState {
    Pending,
    /// Registered; `trigger` is the backend's description of the active keys.
    Bound {
        trigger: Option<String>,
    },
    /// Another client already owns the combination.
    Conflict {
        reason: String,
    },
    /// The backend or the user declined this shortcut.
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingStatus {
    pub id: String,
    pub requested: String,
    pub state: BindingState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutErrorKind {
    Unavailable,
    Denied,
    Cancelled,
    Protocol,
    WorkerUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutError {
    pub kind: ShortcutErrorKind,
    pub message: String,
}

impl ShortcutError {
    pub fn new(kind: ShortcutErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ShortcutError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutPhase {
    Idle,
    Starting,
    Active,
    Failed(ShortcutError),
    Stopped,
}

/// Registration state shared between a backend's worker and its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutStatus {
    pub provider: Option<ShortcutProvider>,
    pub phase: ShortcutPhase,
    pub bindings: Vec<BindingStatus>,
    /// Bumped on every change so pollers can skip unchanged state.
    pub generation: u64,
}

impl Default for ShortcutStatus {
    fn default() -> Self {
        Self {
            provider: None,
            phase: ShortcutPhase::Idle,
            bindings: Vec::new(),
            generation: 0,
        }
    }
}

pub type SharedShortcutStatus = Arc<Mutex<ShortcutStatus>>;

/// Called with the command ID of every activated shortcut, from a worker thread.
pub type ActivationSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Locks the shared status, recovering from a poisoned lock.
pub fn lock_status(status: &Mutex<ShortcutStatus>) -> MutexGuard<'_, ShortcutStatus> {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Applies a change and bumps the generation.
pub fn update_status(status: &Mutex<ShortcutStatus>, change: impl FnOnce(&mut ShortcutStatus)) {
    let mut guard = lock_status(status);
    change(&mut guard);
    guard.generation = guard.generation.wrapping_add(1);
}

pub trait ShortcutBackend: Send {
    fn provider(&self) -> ShortcutProvider;

    /// Registers `requests` on a named worker thread and returns at once.
    /// Progress and per-binding results go to `status`; each activation calls
    /// `sink` with the request ID.
    fn start(
        &mut self,
        requests: Vec<ShortcutRequest>,
        sink: ActivationSink,
        status: SharedShortcutStatus,
    ) -> Result<(), ShortcutError>;

    /// Releases every binding and session and joins the worker. Idempotent.
    fn stop(&mut self);
}

/// The production backend for a provider.
pub fn backend_for(provider: ShortcutProvider) -> Box<dyn ShortcutBackend> {
    match provider {
        ShortcutProvider::Portal => Box::new(PortalShortcutBackend::new(APPLICATION_ID)),
        ShortcutProvider::X11 => Box::new(X11ShortcutBackend::new()),
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::CapabilityStatus;

    use super::{SessionKind, ShortcutDetection, ShortcutProvider, capability_for};

    #[test]
    fn session_type_wins_over_display_variables() {
        assert_eq!(
            SessionKind::from_environment(Some("x11"), true, true),
            SessionKind::X11
        );
        assert_eq!(
            SessionKind::from_environment(None, true, true),
            SessionKind::Wayland,
            "Xwayland's DISPLAY does not make a Wayland session X11"
        );
        assert_eq!(
            SessionKind::from_environment(None, false, true),
            SessionKind::X11
        );
        assert_eq!(
            SessionKind::from_environment(Some("tty"), false, false),
            SessionKind::Unknown
        );
    }

    #[test]
    fn provider_prefers_the_portal_and_limits_grabs_to_real_x11() {
        let detection = |session, portal_version, x11_display| ShortcutDetection {
            session,
            portal_version,
            x11_display,
        };
        assert_eq!(
            detection(SessionKind::X11, Some(1), true).provider(),
            Some(ShortcutProvider::Portal)
        );
        assert_eq!(
            detection(SessionKind::X11, None, true).provider(),
            Some(ShortcutProvider::X11)
        );
        assert_eq!(
            detection(SessionKind::Wayland, None, true).provider(),
            None,
            "Xwayland grabs are never used"
        );
    }

    #[test]
    fn an_unsupported_session_reports_evidence_and_the_command_line_path() {
        let report = capability_for(&ShortcutDetection {
            session: SessionKind::Wayland,
            portal_version: None,
            x11_display: true,
        });

        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(
            report
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("kestrel --command"))
        );
        let evidence = report
            .evidence
            .iter()
            .map(|item| (item.key.as_str(), item.value.as_str()))
            .collect::<Vec<_>>();
        assert!(evidence.contains(&("session_type", "wayland")));
        assert!(evidence.contains(&("portal_version", "absent")));
    }
}
