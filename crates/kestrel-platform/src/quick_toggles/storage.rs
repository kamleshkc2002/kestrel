use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::Path,
    sync::{Arc, Mutex},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};

use super::{
    MutationConfirmation, QuickToggleControl, QuickToggleError, QuickToggleErrorKind,
    QuickToggleId, QuickToggleMutation, QuickToggleObservation, ToggleAction, permission_denied,
    supported,
};
use crate::notifications;

const MAX_TRASH_ENTRIES: usize = 100_000;

#[derive(Debug)]
struct TrashScope {
    items: usize,
    bytes: u64,
    fingerprint: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatteryReading {
    pub name: String,
    pub capacity: Option<u8>,
    pub status: Option<String>,
}

pub(crate) fn trash_capability(data_home: &Path) -> CapabilityReport {
    let id = QuickToggleId::EmptyTrash;
    match scan_trash(data_home) {
        Ok(scope) => supported(
            id,
            format!(
                "The current user's local Trash is accessible ({} items).",
                scope.items
            ),
            "freedesktop.org XDG Trash",
        )
        .with_evidence(CapabilityEvidence::new("scope", "local-user-trash")),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::FileDeletion,
            "The current user's local Trash is not accessible.",
            "Restore user ownership and write access for the local XDG Trash directories.",
        ),
        Err(error) => CapabilityReport::new(
            id.feature_id(),
            CapabilityStatus::Unsupported {
                reason: error.message.clone(),
            },
            "The current user's local Trash cannot be inspected safely.",
        )
        .with_remediation(error.message),
    }
}

pub(crate) fn observe_trash(data_home: &Path) -> Result<QuickToggleObservation, QuickToggleError> {
    let scope = scan_trash(data_home)?;
    let actions = if scope.items == 0 {
        Vec::new()
    } else {
        vec![ToggleAction {
            target: None,
            label: "Empty local Trash".to_owned(),
            confirmation: trash_scope_confirmation(&scope),
        }]
    };
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Actions(actions),
        detail: if scope.items == 0 {
            "The current user's local Trash is empty.".to_owned()
        } else {
            format!(
                "Local Trash contains {} top-level items ({} bytes); remote and other users' Trash locations are excluded.",
                scope.items, scope.bytes
            )
        },
    })
}

pub(crate) fn trash_confirmation(
    data_home: &Path,
    mutation: &QuickToggleMutation,
) -> Result<Option<MutationConfirmation>, QuickToggleError> {
    match mutation {
        QuickToggleMutation::Activate { target: None } => {
            let scope = scan_trash(data_home)?;
            if scope.items == 0 {
                return Err(QuickToggleError::new(
                    QuickToggleErrorKind::InvalidRequest,
                    "the local Trash is already empty",
                ));
            }
            Ok(Some(trash_scope_confirmation(&scope)))
        }
        _ => Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "empty Trash accepts only its advertised action",
        )),
    }
}

pub(crate) fn empty_trash(
    data_home: &Path,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    if !matches!(mutation, QuickToggleMutation::Activate { target: None }) {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "empty Trash accepts only its advertised action",
        ));
    }
    let trash = data_home.join("Trash");
    remove_children(&trash.join("files"))?;
    remove_children(&trash.join("info"))
}

pub(crate) fn battery_alerts_capability(sys_root: &Path) -> CapabilityReport {
    let id = QuickToggleId::BatteryAlerts;
    let batteries = match batteries(sys_root) {
        Ok(batteries) if !batteries.is_empty() => batteries,
        Ok(_) => {
            return CapabilityReport::new(
                id.feature_id(),
                CapabilityStatus::Unsupported {
                    reason: "no system battery is exposed by sysfs".to_owned(),
                },
                "Battery alerts are not applicable on this system.",
            )
            .with_remediation(
                "Connect a supported battery exposed through /sys/class/power_supply, or leave battery alerts disabled on batteryless systems.",
            );
        }
        Err(error) => {
            return CapabilityReport::new(
                id.feature_id(),
                CapabilityStatus::Unsupported {
                    reason: error.message.clone(),
                },
                "Battery state cannot be inspected.",
            )
            .with_remediation(error.message);
        }
    };

    match notification_capabilities() {
        Ok(_) => supported(
            id,
            "Battery state is available and the desktop notification service is reachable.",
            "sysfs power_supply + org.freedesktop.Notifications",
        )
        .with_evidence(CapabilityEvidence::new(
            "battery-count",
            batteries.len().to_string(),
        )),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::Notifications,
            "The notification service rejected access.",
            "Allow notifications for Kestrel in desktop notification settings.",
        ),
        Err(error) => CapabilityReport::new(
            id.feature_id(),
            CapabilityStatus::MissingDependency {
                name: "org.freedesktop.Notifications".to_owned(),
            },
            "Battery state is available, but no notification service is reachable.",
        )
        .with_remediation(error.message),
    }
}

pub(crate) fn observe_battery_alerts(
    sys_root: &Path,
    enabled: &Arc<Mutex<bool>>,
) -> Result<QuickToggleObservation, QuickToggleError> {
    let batteries = batteries(sys_root)?;
    let enabled = *enabled
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let detail = batteries
        .first()
        .map(|battery| {
            let capacity = battery
                .capacity
                .map(|capacity| format!("{capacity}%"))
                .unwrap_or_else(|| "unknown charge".to_owned());
            format!(
                "Alerts are {}; {} is at {} ({}).",
                if enabled { "enabled" } else { "disabled" },
                battery.name,
                capacity,
                battery.status.as_deref().unwrap_or("unknown state")
            )
        })
        .unwrap_or_else(|| "No system battery is available.".to_owned());
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled,
            confirmation: None,
        },
        detail,
    })
}

pub(crate) fn set_battery_alerts(
    enabled_state: &Arc<Mutex<bool>>,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "battery alerts accept only an enabled/disabled value",
        ));
    };
    if *enabled {
        notification_capabilities()?;
    }
    *enabled_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = *enabled;
    Ok(())
}

fn trash_scope_confirmation(scope: &TrashScope) -> MutationConfirmation {
    MutationConfirmation {
        scope: format!(
            "Permanently delete {} top-level items ({} bytes) from this user's local Trash. Remote Trash locations are excluded.",
            scope.items, scope.bytes
        ),
        token: format!(
            "trash:{}:{}:{:016x}",
            scope.items, scope.bytes, scope.fingerprint
        ),
    }
}

fn scan_trash(data_home: &Path) -> Result<TrashScope, QuickToggleError> {
    let files = data_home.join("Trash/files");
    if !files.exists() {
        return Ok(TrashScope {
            items: 0,
            bytes: 0,
            fingerprint: 0,
        });
    }
    let top_level = fs::read_dir(&files)
        .map_err(|error| QuickToggleError::io("reading the local Trash", &error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| QuickToggleError::io("reading a local Trash entry", &error))?;
    let mut hasher = DefaultHasher::new();
    let mut bytes = 0u64;
    let mut visited = 0usize;
    let mut stack = top_level
        .iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    while let Some(path) = stack.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_TRASH_ENTRIES {
            return Err(QuickToggleError::new(
                QuickToggleErrorKind::InvalidRequest,
                format!(
                    "the local Trash exceeds the bounded inspection limit of {MAX_TRASH_ENTRIES} entries"
                ),
            ));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| QuickToggleError::io("inspecting a local Trash entry", &error))?;
        path.strip_prefix(&files).unwrap_or(&path).hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        bytes = bytes.saturating_add(metadata.len());
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            let entries = fs::read_dir(&path)
                .map_err(|error| QuickToggleError::io("reading a trashed directory", &error))?;
            for entry in entries {
                stack.push(
                    entry
                        .map_err(|error| QuickToggleError::io("reading a trashed entry", &error))?
                        .path(),
                );
            }
        }
    }

    Ok(TrashScope {
        items: top_level.len(),
        bytes,
        fingerprint: hasher.finish(),
    })
}

fn remove_children(directory: &Path) -> Result<(), QuickToggleError> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| QuickToggleError::io("opening a Trash directory", &error))?;
    for entry in entries {
        let path = entry
            .map_err(|error| QuickToggleError::io("reading a Trash entry", &error))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| QuickToggleError::io("inspecting a Trash entry", &error))?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path)
                .map_err(|error| QuickToggleError::io("removing a trashed directory", &error))?;
        } else {
            fs::remove_file(&path)
                .map_err(|error| QuickToggleError::io("removing a trashed file", &error))?;
        }
    }
    Ok(())
}

pub(crate) fn batteries(sys_root: &Path) -> Result<Vec<BatteryReading>, QuickToggleError> {
    let root = sys_root.join("class/power_supply");
    let entries = fs::read_dir(&root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            QuickToggleError::unavailable("no power_supply class is exposed by sysfs")
        } else {
            QuickToggleError::io("reading system power supplies", &error)
        }
    })?;
    let mut result = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| QuickToggleError::io("reading a power supply entry", &error))?
            .path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let capacity = fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|value| value.trim().parse::<u8>().ok())
            .map(|value| value.min(100));
        let status = fs::read_to_string(path.join("status"))
            .ok()
            .map(|value| value.trim().to_owned());
        result.push(BatteryReading {
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("battery")
                .to_owned(),
            capacity,
            status,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

fn notification_capabilities() -> Result<Vec<String>, QuickToggleError> {
    notifications::capabilities().map_err(notification_error)
}

fn notification_error(error: crate::notifications::NotificationError) -> QuickToggleError {
    let kind = match error.kind() {
        crate::notifications::NotificationErrorKind::Unavailable => {
            QuickToggleErrorKind::Unavailable
        }
        crate::notifications::NotificationErrorKind::Protocol => QuickToggleErrorKind::Protocol,
        crate::notifications::NotificationErrorKind::PermissionDenied => {
            QuickToggleErrorKind::PermissionDenied
        }
    };
    QuickToggleError::new(kind, error.message)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{battery_alerts_capability, empty_trash, observe_trash, trash_confirmation};
    use crate::quick_toggles::{QuickToggleControl, QuickToggleMutation};
    use kestrel_core::CapabilityStatus;

    #[test]
    fn batteryless_system_reports_actionable_remediation() {
        let directory = TempDir::new().expect("temporary sysfs root");
        fs::create_dir_all(directory.path().join("class/power_supply"))
            .expect("power supply class");

        let report = battery_alerts_capability(directory.path());

        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(report.remediation.is_some());
    }

    #[test]
    fn trash_confirmation_changes_when_scope_changes() {
        let directory = TempDir::new().expect("temporary data home");
        let files = directory.path().join("Trash/files");
        fs::create_dir_all(&files).expect("Trash files");
        fs::write(files.join("first.txt"), "first").expect("first item");
        let mutation = QuickToggleMutation::Activate { target: None };
        let first = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");
        fs::write(files.join("second.txt"), "second").expect("second item");
        let second = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");

        assert_ne!(first.token, second.token);
    }

    #[test]
    fn empty_trash_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("temporary data home");
        let outside = directory.path().join("outside.txt");
        fs::write(&outside, "preserve").expect("outside file");
        let files = directory.path().join("Trash/files");
        let info = directory.path().join("Trash/info");
        fs::create_dir_all(&files).expect("Trash files");
        fs::create_dir_all(&info).expect("Trash info");
        symlink(&outside, files.join("link")).expect("symlink");
        fs::write(info.join("link.trashinfo"), "metadata").expect("trash metadata");

        empty_trash(
            directory.path(),
            &QuickToggleMutation::Activate { target: None },
        )
        .expect("Trash empties");

        assert_eq!(fs::read_to_string(outside).unwrap(), "preserve");
        let observation = observe_trash(directory.path()).expect("Trash observation");
        assert_eq!(observation.control, QuickToggleControl::Actions(Vec::new()));
    }
}
