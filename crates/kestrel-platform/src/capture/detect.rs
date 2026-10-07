//! Read-only screenshot provider detection. Nothing here captures, opens a
//! dialog, or writes to the permission store.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::OwnedValue;

use super::{CaptureMode, CaptureProvider, FEATURE_ID};
use crate::CapabilityProbe;
use crate::applications::resolve_executable;
use crate::global_shortcuts::{APPLICATION_ID, SessionKind};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PERMISSION_STORE_BUS: &str = "org.freedesktop.impl.portal.PermissionStore";
const PERMISSION_STORE_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const NOT_FOUND_ERROR: &str = "org.freedesktop.portal.Error.NotFound";
const SCREENCOPY_GLOBALS: [&str; 2] = [
    "zwlr_screencopy_manager_v1",
    "ext_image_copy_capture_manager_v1",
];

/// The desktop's stored answer for non-interactive Screen captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenPermission {
    Granted,
    Denied,
    /// No stored answer; the desktop asks on the first Screen capture.
    Unset,
    /// The permission store could not be read.
    Unknown,
}

impl ScreenPermission {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Unset => "unset",
            Self::Unknown => "unknown",
        }
    }
}

/// What the session offers for screenshots, observed without capturing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDetection {
    pub session: SessionKind,
    /// The Screenshot portal's `version` property, when the interface is exported.
    pub portal_version: Option<u32>,
    pub screen_permission: ScreenPermission,
    /// Absolute path of `grim`, never executed during detection.
    pub grim: Option<PathBuf>,
    /// Absolute path of `slurp`, never executed during detection.
    pub slurp: Option<PathBuf>,
    /// The Wayland display advertises a screencopy global.
    pub screencopy: bool,
    pub x11_display: bool,
}

impl CaptureDetection {
    /// A detection with no provider evidence.
    pub fn new(session: SessionKind) -> Self {
        Self {
            session,
            portal_version: None,
            screen_permission: ScreenPermission::Unknown,
            grim: None,
            slurp: None,
            screencopy: false,
            x11_display: false,
        }
    }

    pub fn detect() -> Self {
        let bus = Builder::session()
            .and_then(|builder| builder.method_timeout(PROBE_TIMEOUT).build())
            .ok();
        let path_env = env::var_os("PATH");
        Self {
            session: SessionKind::detect(),
            portal_version: bus.as_ref().and_then(portal_version),
            screen_permission: bus
                .as_ref()
                .map_or(ScreenPermission::Unknown, screen_permission),
            grim: resolve_executable("grim", path_env.as_deref()),
            slurp: resolve_executable("slurp", path_env.as_deref()),
            screencopy: env::var_os("WAYLAND_DISPLAY").is_some() && wayland_screencopy(),
            x11_display: env::var_os("DISPLAY").is_some(),
        }
    }

    pub fn provider(&self) -> Option<CaptureProvider> {
        if self.portal_version.is_some() {
            Some(CaptureProvider::Portal)
        } else if self.session == SessionKind::Wayland && self.screencopy && self.grim.is_some() {
            Some(CaptureProvider::Grim)
        } else if self.session == SessionKind::X11 && self.x11_display {
            Some(CaptureProvider::X11)
        } else {
            None
        }
    }

    /// Modes of the selected provider, preferred first.
    pub fn modes(&self) -> Vec<CaptureMode> {
        match self.provider() {
            Some(CaptureProvider::Portal) => vec![CaptureMode::Interactive, CaptureMode::Screen],
            Some(CaptureProvider::Grim) if self.slurp.is_some() => {
                vec![CaptureMode::Area, CaptureMode::Screen]
            }
            Some(CaptureProvider::Grim) => vec![CaptureMode::Screen],
            Some(CaptureProvider::X11) => vec![CaptureMode::Window, CaptureMode::Screen],
            None => Vec::new(),
        }
    }

    pub fn capability(&self) -> CapabilityReport {
        let presence = |found: bool| if found { "present" } else { "absent" };
        let evidence = [
            CapabilityEvidence::new("session_type", self.session.label()),
            CapabilityEvidence::new(
                "portal_version",
                self.portal_version
                    .map_or_else(|| "absent".to_owned(), |version| version.to_string()),
            ),
            CapabilityEvidence::new("screen_permission", self.screen_permission.label()),
            CapabilityEvidence::new("grim", presence(self.grim.is_some())),
            CapabilityEvidence::new("slurp", presence(self.slurp.is_some())),
            CapabilityEvidence::new("screencopy", presence(self.screencopy)),
            CapabilityEvidence::new("x11_display", presence(self.x11_display)),
        ];
        let report = match self.provider() {
            Some(CaptureProvider::Portal) => self.portal_report(),
            Some(CaptureProvider::Grim) if self.slurp.is_none() => CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Limited {
                    reason: "Area capture needs slurp.".to_owned(),
                },
                "Screen captures run through grim; Area capture is unavailable.",
            )
            .with_selected_backend(CaptureProvider::Grim.label())
            .with_remediation("Install slurp to select an area."),
            Some(CaptureProvider::Grim) => CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                "Screen and Area captures run through grim and slurp.",
            )
            .with_selected_backend(CaptureProvider::Grim.label()),
            Some(CaptureProvider::X11) => CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                "Screen and Window captures read the X11 display.",
            )
            .with_selected_backend(CaptureProvider::X11.label()),
            None => CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "This session exports no Screenshot portal, has no grim with a \
                             screencopy-capable compositor, and is not a real X11 session."
                        .to_owned(),
                },
                "Screenshots are unavailable in this session.",
            )
            .with_remediation(
                "Install the desktop's xdg-desktop-portal backend with Screenshot support, or \
                 grim and slurp on wlroots compositors.",
            ),
        };
        evidence
            .into_iter()
            .fold(report, |report, item| report.with_evidence(item))
    }

    fn portal_report(&self) -> CapabilityReport {
        let summary = match self.screen_permission {
            ScreenPermission::Unset => {
                "Screenshots run through the desktop portal; the first Screen capture asks for \
                 permission."
            }
            ScreenPermission::Denied => {
                "Screenshots run through the desktop portal; Screen capture permission is denied, \
                 so only the desktop's capture dialog works."
            }
            ScreenPermission::Granted | ScreenPermission::Unknown => {
                "Screenshots run through the desktop portal."
            }
        };
        let report = CapabilityReport::new(FEATURE_ID, CapabilityStatus::Supported, summary)
            .with_selected_backend(CaptureProvider::Portal.label());
        if self.screen_permission == ScreenPermission::Denied {
            report.with_remediation(
                "Allow screenshots for Kestrel in the desktop's privacy settings.",
            )
        } else {
            report
        }
    }
}

fn portal_version(bus: &Connection) -> Option<u32> {
    let reply = bus
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.portal.Screenshot", "version"),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}

fn screen_permission(bus: &Connection) -> ScreenPermission {
    let reply = bus.call_method(
        Some(PERMISSION_STORE_BUS),
        PERMISSION_STORE_PATH,
        Some(PERMISSION_STORE_BUS),
        "Lookup",
        &("screenshot", "screenshot"),
    );
    match reply {
        Ok(message) => message
            .body()
            .deserialize::<(HashMap<String, Vec<String>>, OwnedValue)>()
            .map_or(ScreenPermission::Unknown, |(entries, _data)| {
                permission_from_entries(&entries)
            }),
        Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == NOT_FOUND_ERROR => {
            ScreenPermission::Unset
        }
        Err(_) => ScreenPermission::Unknown,
    }
}

/// Kestrel's own entry wins, then the unsandboxed (empty) app ID, then any app.
fn permission_from_entries(entries: &HashMap<String, Vec<String>>) -> ScreenPermission {
    let mut others: Vec<&String> = entries
        .keys()
        .filter(|app| app.as_str() != APPLICATION_ID && !app.is_empty())
        .collect();
    others.sort();
    let entry = entries
        .get(APPLICATION_ID)
        .or_else(|| entries.get(""))
        .or_else(|| others.first().and_then(|app| entries.get(*app)));
    match entry.map(|values| values.first().map(String::as_str)) {
        None => ScreenPermission::Unset,
        Some(Some("yes")) => ScreenPermission::Granted,
        Some(Some("no")) => ScreenPermission::Denied,
        Some(_) => ScreenPermission::Unknown,
    }
}

/// One registry round-trip on a dedicated thread, bounded by `PROBE_TIMEOUT`.
fn wayland_screencopy() -> bool {
    let (sender, receiver) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("kestrel-capture-probe".to_owned())
        .spawn(move || {
            let _ = sender.send(wayland_registry_has_screencopy());
        });
    spawned.is_ok() && receiver.recv_timeout(PROBE_TIMEOUT).unwrap_or(false)
}

fn wayland_registry_has_screencopy() -> bool {
    use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_registry};

    struct Globals {
        screencopy: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
        fn event(
            state: &mut Self,
            _registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _data: &(),
            _connection: &Connection,
            _queue: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event {
                state.screencopy |= SCREENCOPY_GLOBALS.contains(&interface.as_str());
            }
        }
    }

    let Ok(connection) = Connection::connect_to_env() else {
        return false;
    };
    let mut queue = connection.new_event_queue();
    let _registry = connection.display().get_registry(&queue.handle(), ());
    let mut globals = Globals { screencopy: false };
    queue.roundtrip(&mut globals).is_ok() && globals.screencopy
}

/// Read-only probe for `capture.screenshot`; never captures or opens a dialog.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureProbe;

impl CapabilityProbe for CaptureProbe {
    fn probe(&self) -> CapabilityReport {
        CaptureDetection::detect().capability()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(
        session: SessionKind,
        change: impl FnOnce(&mut CaptureDetection),
    ) -> CaptureDetection {
        let mut detection = CaptureDetection::new(session);
        change(&mut detection);
        detection
    }

    fn with_grim(detection: &mut CaptureDetection, slurp: bool) {
        detection.grim = Some(PathBuf::from("/usr/bin/grim"));
        detection.slurp = slurp.then(|| PathBuf::from("/usr/bin/slurp"));
        detection.screencopy = true;
    }

    #[test]
    fn provider_and_modes_follow_observed_backends() {
        let cases = [
            (
                detection(SessionKind::Wayland, |d| {
                    d.portal_version = Some(2);
                    with_grim(d, true);
                }),
                Some(CaptureProvider::Portal),
                vec![CaptureMode::Interactive, CaptureMode::Screen],
            ),
            (
                detection(SessionKind::X11, |d| {
                    d.portal_version = Some(1);
                    d.x11_display = true;
                }),
                Some(CaptureProvider::Portal),
                vec![CaptureMode::Interactive, CaptureMode::Screen],
            ),
            (
                detection(SessionKind::Wayland, |d| with_grim(d, true)),
                Some(CaptureProvider::Grim),
                vec![CaptureMode::Area, CaptureMode::Screen],
            ),
            (
                detection(SessionKind::Wayland, |d| with_grim(d, false)),
                Some(CaptureProvider::Grim),
                vec![CaptureMode::Screen],
            ),
            (
                detection(SessionKind::Wayland, |d| {
                    with_grim(d, true);
                    d.screencopy = false;
                }),
                None,
                vec![],
            ),
            (
                detection(SessionKind::X11, |d| with_grim(d, true)),
                None,
                vec![],
            ),
            (
                detection(SessionKind::X11, |d| d.x11_display = true),
                Some(CaptureProvider::X11),
                vec![CaptureMode::Window, CaptureMode::Screen],
            ),
            (
                // Xwayland exposes DISPLAY inside Wayland sessions.
                detection(SessionKind::Wayland, |d| d.x11_display = true),
                None,
                vec![],
            ),
            (detection(SessionKind::Unknown, |_| {}), None, vec![]),
        ];
        for (detection, provider, modes) in cases {
            assert_eq!(detection.provider(), provider, "{detection:?}");
            assert_eq!(detection.modes(), modes, "{detection:?}");
        }
    }

    #[test]
    fn capability_status_tracks_provider_and_permission() {
        let portal = |permission| {
            detection(SessionKind::Wayland, |d| {
                d.portal_version = Some(2);
                d.screen_permission = permission;
            })
            .capability()
        };
        let granted = portal(ScreenPermission::Granted);
        assert_eq!(granted.status, CapabilityStatus::Supported);
        assert_eq!(
            granted.selected_backend.as_deref(),
            Some("Screenshot portal")
        );
        assert_eq!(granted.remediation, None);

        let unset = portal(ScreenPermission::Unset);
        assert_eq!(unset.status, CapabilityStatus::Supported);
        assert!(unset.summary.contains("asks for permission"));

        let denied = portal(ScreenPermission::Denied);
        assert_eq!(denied.status, CapabilityStatus::Supported);
        assert!(
            denied
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("privacy settings"))
        );

        let grim_only = detection(SessionKind::Wayland, |d| with_grim(d, false)).capability();
        assert!(matches!(grim_only.status, CapabilityStatus::Limited { .. }));
        assert!(
            grim_only
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("slurp"))
        );

        let grim = detection(SessionKind::Wayland, |d| with_grim(d, true)).capability();
        assert_eq!(grim.status, CapabilityStatus::Supported);
        assert_eq!(grim.selected_backend.as_deref(), Some("grim"));

        let x11 = detection(SessionKind::X11, |d| d.x11_display = true).capability();
        assert_eq!(x11.status, CapabilityStatus::Supported);
        assert_eq!(x11.selected_backend.as_deref(), Some("X11"));

        let none = detection(SessionKind::Wayland, |_| {}).capability();
        assert!(matches!(none.status, CapabilityStatus::Unsupported { .. }));
        assert_eq!(none.selected_backend, None);
        assert!(
            none.remediation
                .as_deref()
                .is_some_and(|text| text.contains("xdg-desktop-portal") && text.contains("grim"))
        );
    }

    #[test]
    fn capability_evidence_names_executables_without_paths() {
        let report = detection(SessionKind::Wayland, |d| with_grim(d, true)).capability();
        assert_eq!(report.feature_id, FEATURE_ID);
        assert!(report.evidence.iter().all(|item| !item.value.contains('/')));
        let value = |key: &str| {
            report
                .evidence
                .iter()
                .find(|item| item.key == key)
                .map(|item| item.value.as_str())
        };
        assert_eq!(value("grim"), Some("present"));
        assert_eq!(value("slurp"), Some("present"));
        assert_eq!(value("screencopy"), Some("present"));
        assert_eq!(value("session_type"), Some("wayland"));
        assert_eq!(value("portal_version"), Some("absent"));
    }

    #[test]
    fn permission_entries_prefer_kestrel_then_the_host_app() {
        let entries = |pairs: &[(&str, &str)]| -> HashMap<String, Vec<String>> {
            pairs
                .iter()
                .map(|(app, value)| ((*app).to_owned(), vec![(*value).to_owned()]))
                .collect()
        };
        assert_eq!(
            permission_from_entries(&HashMap::new()),
            ScreenPermission::Unset
        );
        assert_eq!(
            permission_from_entries(&entries(&[("", "yes")])),
            ScreenPermission::Granted
        );
        assert_eq!(
            permission_from_entries(&entries(&[("", "yes"), (APPLICATION_ID, "no")])),
            ScreenPermission::Denied
        );
        assert_eq!(
            permission_from_entries(&entries(&[("org.example.Other", "no")])),
            ScreenPermission::Denied
        );
        assert_eq!(
            permission_from_entries(&entries(&[("", "ask")])),
            ScreenPermission::Unknown
        );
    }
}
