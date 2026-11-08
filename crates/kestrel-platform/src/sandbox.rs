//! Detection and typed reasons for the restricted sandboxed build.
//!
//! The sandbox exposes a fraction of the host session. Probes that lose a
//! source report [`SandboxReason`] instead of a desktop fault, so the UI names
//! the restriction and, where one exists, the permission that lifts it.

use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use zbus::zvariant::OwnedValue;

use crate::capture::portal::{
    PORTAL_BUS, PORTAL_PATH, Response, register_application, request_path, send_request,
    token_suffix,
};

/// Whether this process runs inside a Flatpak sandbox.
pub fn flatpak() -> bool {
    flatpak_in(Path::new("/"))
}

/// The testable form: `root` stands in for the filesystem root.
pub fn flatpak_in(root: &Path) -> bool {
    root.join(".flatpak-info").is_file()
}

const BACKGROUND_INTERFACE: &str = "org.freedesktop.portal.Background";

/// One restriction the sandbox places on a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxReason {
    /// The sandbox has a PID namespace of its own.
    OwnProcesses,
    /// Socket owners outside the sandbox have no visible process.
    ListenerOwners,
    /// Camera device nodes are not exposed.
    CameraDevices,
    /// Desktop settings live in the host's session, not this one.
    DesktopSettings,
    /// The host Trash is outside the sandbox.
    HostTrash,
    /// System settings are mounted read-only.
    SystemSettings,
    /// The compositor filters clipboard protocols for sandboxed apps.
    ClipboardAccess,
    /// Host applications, windows, and files are out of reach.
    CommandSources,
    /// Files from other apps arrive as document-portal handles.
    ShelfFiles,
    /// The host's root and home filesystems are hidden.
    HostFilesystems,
    /// Autostart needs a portal this session does not export.
    Autostart,
    /// The sandbox cannot own an item bus name.
    TrayName,
}

impl SandboxReason {
    /// A stable category for evidence and tests.
    pub const fn key(self) -> &'static str {
        match self {
            Self::OwnProcesses => "own-processes",
            Self::ListenerOwners => "listener-owners",
            Self::CameraDevices => "camera-devices",
            Self::DesktopSettings => "desktop-settings",
            Self::HostTrash => "host-trash",
            Self::SystemSettings => "system-settings",
            Self::ClipboardAccess => "clipboard-access",
            Self::CommandSources => "command-sources",
            Self::ShelfFiles => "shelf-files",
            Self::HostFilesystems => "host-filesystems",
            Self::Autostart => "autostart",
            Self::TrayName => "tray-name",
        }
    }

    /// The plain explanation, without a trailing period.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::OwnProcesses => "the sandbox only lists Kestrel's own processes",
            Self::ListenerOwners => "sockets owned outside the sandbox have no visible process",
            Self::CameraDevices => "camera devices are not available to the sandbox",
            Self::DesktopSettings => "desktop settings are private to the sandbox",
            Self::HostTrash => "the host Trash is outside the sandbox",
            Self::SystemSettings => "system settings are read-only in the sandbox",
            Self::ClipboardAccess => "the compositor does not give sandboxed apps clipboard access",
            Self::CommandSources => {
                "applications, files, and windows outside the sandbox are not visible"
            }
            Self::ShelfFiles => "files from other apps arrive through the document portal",
            Self::HostFilesystems => "the sandbox hides the host's root and home filesystems",
            Self::Autostart => "autostart needs the desktop's Background portal",
            Self::TrayName => "a sandboxed build cannot own a StatusNotifierItem bus name",
        }
    }

    /// The permission or host package that enables the feature, when one exists.
    pub const fn remediation(self) -> Option<&'static str> {
        match self {
            Self::CameraDevices => Some(
                "A Flatpak build with full device access (--device=all) would expose camera devices.",
            ),
            Self::DesktopSettings => {
                Some("A Flatpak build with dconf access would reach the host's desktop settings.")
            }
            Self::HostTrash => Some(
                "A Flatpak build with home access (--filesystem=home) would reach the host Trash.",
            ),
            Self::SystemSettings => Some(
                "A Flatpak build with writable system access (--filesystem=/sys) would change these settings.",
            ),
            Self::CommandSources => Some("Add the folders to search as command-bar file roots."),
            Self::TrayName => Some(
                "Open the window with `flatpak run io.github.kamleshkc2002.Kestrel --command window.show`.",
            ),
            Self::OwnProcesses
            | Self::ListenerOwners
            | Self::ClipboardAccess
            | Self::ShelfFiles
            | Self::HostFilesystems
            | Self::Autostart => None,
        }
    }
}

/// An Unsupported report for one sandbox restriction.
pub fn unsupported(
    feature_id: &'static str,
    summary: impl Into<String>,
    reason: SandboxReason,
) -> CapabilityReport {
    report(
        feature_id,
        CapabilityStatus::Unsupported {
            reason: reason.reason().to_owned(),
        },
        summary,
        reason,
    )
}

/// A Limited report for one sandbox restriction.
pub fn limited(
    feature_id: &'static str,
    summary: impl Into<String>,
    reason: SandboxReason,
) -> CapabilityReport {
    report(
        feature_id,
        CapabilityStatus::Limited {
            reason: reason.reason().to_owned(),
        },
        summary,
        reason,
    )
}

fn report(
    feature_id: &'static str,
    status: CapabilityStatus,
    summary: impl Into<String>,
    reason: SandboxReason,
) -> CapabilityReport {
    let mut report = CapabilityReport::new(feature_id, status, summary)
        .with_evidence(CapabilityEvidence::new("sandbox", reason.key()));
    if let Some(remediation) = reason.remediation() {
        report = report.with_remediation(remediation);
    }
    report
}

/// Whether the session exports the Background portal that autostart needs.
pub fn background_portal() -> bool {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return false;
    };
    connection
        .call_method(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(BACKGROUND_INTERFACE, "version"),
        )
        .is_ok()
}

/// Asks the desktop's Background portal to start Kestrel at login.
///
/// `EnableAutostart` does not exist in the shipped interface, so this sends
/// `RequestBackground` with `autostart` and waits for its response.
pub fn request_autostart(enable: bool) -> Result<(), String> {
    const AUTOSTART_TIMEOUT: Duration = Duration::from_secs(60);
    let connection = zbus::blocking::Connection::session()
        .map_err(|error| format!("connecting to the session bus failed: {error}"))?;
    let sender = connection
        .unique_name()
        .map(ToString::to_string)
        .ok_or_else(|| "the session bus assigned no unique name".to_owned())?;
    let token = format!("kestrel_autostart_{}", token_suffix());
    let request = request_path(&sender, &token);
    let options: HashMap<String, OwnedValue> = HashMap::from([
        (
            "handle_token".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from(token.as_str())),
        ),
        ("autostart".to_owned(), OwnedValue::from(enable)),
        (
            "reason".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from(
                "Start Kestrel when the session starts.",
            )),
        ),
    ]);
    register_application(&connection);
    let deadline = Instant::now() + AUTOSTART_TIMEOUT;
    let response = send_request(
        &connection,
        &request,
        deadline,
        discard_background_response,
        || {
            let proxy = zbus::blocking::Proxy::new(
                &connection,
                PORTAL_BUS,
                PORTAL_PATH,
                BACKGROUND_INTERFACE,
            )?;
            proxy.call("RequestBackground", &("", options))
        },
    );
    let _ = connection.close();
    match response {
        Ok((code, results)) => response_autostart(code, &results, enable),
        Err(crate::capture::portal::Failure::Timeout) => {
            Err("the desktop did not answer the autostart request".to_owned())
        }
        Err(crate::capture::portal::Failure::Error(error)) => Err(error.message),
    }
}

fn discard_background_response(_response: Response) {}

fn response_autostart(
    code: u32,
    results: &HashMap<String, OwnedValue>,
    enable: bool,
) -> Result<(), String> {
    let granted = results
        .get("autostart")
        .and_then(|value| bool::try_from(value.clone()).ok())
        .unwrap_or(false);
    match code {
        0 if granted == enable => Ok(()),
        0 => Err("the desktop kept the previous autostart setting".to_owned()),
        1 => Err("the autostart request was dismissed".to_owned()),
        _ => Err("the desktop refused the autostart request".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use kestrel_core::CapabilityStatus;
    use tempfile::TempDir;
    use zbus::zvariant::OwnedValue;

    use super::{SandboxReason, flatpak_in, limited, response_autostart, unsupported};

    #[test]
    fn detection_reads_only_the_marker_file() {
        let root = TempDir::new().expect("root");
        assert!(!flatpak_in(root.path()));
        std::fs::write(root.path().join(".flatpak-info"), "[Application]\n").expect("marker");
        assert!(flatpak_in(root.path()));
    }

    #[test]
    fn reports_carry_the_typed_category_and_a_reason() {
        let report = unsupported(
            "system.processes",
            "Processes are unavailable.",
            SandboxReason::OwnProcesses,
        );
        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(
            report
                .evidence
                .iter()
                .any(|item| item.key == "sandbox" && item.value == "own-processes")
        );
        assert!(report.remediation.is_none());

        let report = limited(
            "network.listeners",
            "Open ports are limited.",
            SandboxReason::ListenerOwners,
        );
        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert!(
            report
                .evidence
                .iter()
                .any(|item| item.key == "sandbox" && item.value == "listener-owners")
        );
    }

    #[test]
    fn a_remediation_names_the_permission_that_lifts_the_limit() {
        let report = unsupported(
            "camera.preview",
            "The camera is unavailable.",
            SandboxReason::CameraDevices,
        );
        let remediation = report.remediation.expect("camera remediation");
        assert!(remediation.contains("--device=all"));
        assert!(
            !remediation.contains("video group"),
            "the desktop group remedy does not apply in a sandbox"
        );
    }

    #[test]
    fn autostart_answers_are_checked_against_the_request() {
        let granted = |value| HashMap::from([("autostart".to_owned(), OwnedValue::from(value))]);
        assert!(response_autostart(0, &granted(true), true).is_ok());
        assert!(response_autostart(0, &granted(false), false).is_ok());
        assert!(response_autostart(0, &granted(true), false).is_err());
        assert!(response_autostart(0, &HashMap::new(), true).is_err());
        assert!(response_autostart(1, &granted(true), true).is_err());
        assert!(response_autostart(2, &granted(true), true).is_err());
    }
}
