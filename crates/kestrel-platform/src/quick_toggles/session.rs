use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};
use zbus::{
    blocking::{Connection, Proxy},
    fdo::ManagedObjects,
    zvariant::{OwnedFd, OwnedObjectPath, OwnedValue},
};

use super::{
    MutationConfirmation, QuickToggleControl, QuickToggleError, QuickToggleErrorKind,
    QuickToggleId, QuickToggleMutation, QuickToggleObservation, ToggleAction, permission_denied,
    supported,
};

const LOGIN_DESTINATION: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const LOGIN_INTERFACE: &str = "org.freedesktop.login1.Manager";
const BLUEZ_ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const UDISKS_BLOCK_INTERFACE: &str = "org.freedesktop.UDisks2.Block";
const UDISKS_DRIVE_INTERFACE: &str = "org.freedesktop.UDisks2.Drive";
const UDISKS_FILESYSTEM_INTERFACE: &str = "org.freedesktop.UDisks2.Filesystem";

#[derive(Debug)]
pub(crate) struct InhibitorLease {
    _fd: OwnedFd,
}

#[derive(Debug, Clone)]
struct EjectableDrive {
    target: String,
    block_path: OwnedObjectPath,
    drive_path: OwnedObjectPath,
    label: String,
    mount_points: Vec<Vec<u8>>,
}

pub(crate) fn keep_awake_capability() -> CapabilityReport {
    let id = QuickToggleId::KeepAwake;
    let connection = match system_connection() {
        Ok(connection) => connection,
        Err(error) => return unavailable_capability(id, "systemd-logind", error),
    };
    let manager = match Proxy::new(&connection, LOGIN_DESTINATION, LOGIN_PATH, LOGIN_INTERFACE) {
        Ok(manager) => manager,
        Err(error) => {
            return dbus_capability_error(
                id,
                "logind is reachable but its inhibitor API is unavailable",
                error,
                Permission::SessionControl,
            );
        }
    };
    let result: Result<u64, _> = manager.get_property("NCurrentInhibitors");
    match result {
        Ok(_) => supported(
            id,
            "The user session can hold a scoped idle and sleep inhibitor.",
            "systemd-logind Inhibit",
        ),
        Err(error) => dbus_capability_error(
            id,
            "logind is reachable but its inhibitor API is unavailable",
            error,
            Permission::SessionControl,
        ),
    }
}

pub(crate) fn observe_keep_awake(
    inhibitor: &Arc<Mutex<Option<InhibitorLease>>>,
) -> Result<QuickToggleObservation, QuickToggleError> {
    let enabled = inhibitor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some();
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled,
            confirmation: None,
        },
        detail: if enabled {
            "Kestrel owns an active idle and sleep inhibitor.".to_owned()
        } else {
            "Kestrel does not currently inhibit idle or sleep.".to_owned()
        },
    })
}

pub(crate) fn set_keep_awake(
    inhibitor: &Arc<Mutex<Option<InhibitorLease>>>,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch("keep awake"));
    };
    let mut lease = inhibitor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *enabled {
        if lease.is_some() {
            return Ok(());
        }
        let connection = system_connection()?;
        let manager = Proxy::new(&connection, LOGIN_DESTINATION, LOGIN_PATH, LOGIN_INTERFACE)
            .map_err(|error| dbus_error("creating the logind manager proxy", error))?;
        let fd: OwnedFd = manager
            .call(
                "Inhibit",
                &(
                    "idle:sleep",
                    "Kestrel",
                    "The user enabled Keep awake",
                    "block",
                ),
            )
            .map_err(|error| dbus_error("acquiring the keep-awake inhibitor", error))?;
        *lease = Some(InhibitorLease { _fd: fd });
    } else {
        lease.take();
    }
    Ok(())
}

pub(crate) fn bluetooth_capability() -> CapabilityReport {
    let id = QuickToggleId::Bluetooth;
    match bluez_adapter() {
        Ok((_connection, path, _powered)) => supported(
            id,
            "A BlueZ adapter exposes its current power state.",
            "BlueZ Adapter1 D-Bus",
        )
        .with_evidence(CapabilityEvidence::new("adapter", object_leaf(&path))),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::NetworkControl,
            "BlueZ denied access to the Bluetooth adapter.",
            "Grant the current graphical user permission to control the BlueZ adapter.",
        ),
        Err(error) => unavailable_capability(id, "BlueZ", error),
    }
}

pub(crate) fn observe_bluetooth() -> Result<QuickToggleObservation, QuickToggleError> {
    let (_connection, path, powered) = bluez_adapter()?;
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled: powered,
            confirmation: powered.then(|| radio_off_confirmation("Bluetooth")),
        },
        detail: format!(
            "Bluetooth adapter {} is {}.",
            object_leaf(&path),
            if powered { "powered" } else { "off" }
        ),
    })
}

pub(crate) fn set_bluetooth(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch("Bluetooth"));
    };
    let (connection, path, _) = bluez_adapter()?;
    let proxy = Proxy::new(
        &connection,
        "org.bluez",
        path.as_str(),
        BLUEZ_ADAPTER_INTERFACE,
    )
    .map_err(|error| dbus_error("creating the BlueZ adapter proxy", error))?;
    proxy
        .set_property("Powered", *enabled)
        .map_err(|error| dbus_error("changing Bluetooth power", error.into()))
}

pub(crate) fn wifi_capability() -> CapabilityReport {
    let id = QuickToggleId::Wifi;
    match network_manager() {
        Ok((_connection, _enabled, hardware_enabled)) if hardware_enabled => supported(
            id,
            "NetworkManager exposes a controllable Wi-Fi radio.",
            "NetworkManager D-Bus",
        ),
        Ok(_) => CapabilityReport::new(
            id.feature_id(),
            CapabilityStatus::Unsupported {
                reason: "NetworkManager reports that no Wi-Fi hardware is enabled".to_owned(),
            },
            "No controllable Wi-Fi radio is available.",
        ),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::NetworkControl,
            "NetworkManager denied access to Wi-Fi state.",
            "Grant the current graphical user permission to control NetworkManager Wi-Fi.",
        ),
        Err(error) => unavailable_capability(id, "NetworkManager", error),
    }
}

pub(crate) fn observe_wifi() -> Result<QuickToggleObservation, QuickToggleError> {
    let (_connection, enabled, hardware_enabled) = network_manager()?;
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled,
            confirmation: enabled.then(|| radio_off_confirmation("Wi-Fi")),
        },
        detail: if hardware_enabled {
            format!(
                "NetworkManager reports Wi-Fi is {}.",
                if enabled { "enabled" } else { "disabled" }
            )
        } else {
            "NetworkManager reports that Wi-Fi hardware is disabled.".to_owned()
        },
    })
}

pub(crate) fn set_wifi(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch("Wi-Fi"));
    };
    let (connection, _, hardware_enabled) = network_manager()?;
    if *enabled && !hardware_enabled {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::Unavailable,
            "NetworkManager reports that the Wi-Fi hardware radio is disabled",
        ));
    }
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .map_err(|error| dbus_error("creating the NetworkManager proxy", error))?;
    proxy
        .set_property("WirelessEnabled", *enabled)
        .map_err(|error| dbus_error("changing Wi-Fi state", error.into()))
}

pub(crate) fn radio_confirmation(
    label: &str,
    mutation: &QuickToggleMutation,
) -> Result<Option<MutationConfirmation>, QuickToggleError> {
    match mutation {
        QuickToggleMutation::SetEnabled(false) => Ok(Some(radio_off_confirmation(label))),
        QuickToggleMutation::SetEnabled(true) => Ok(None),
        _ => Err(invalid_switch(label)),
    }
}

pub(crate) fn screen_lock_capability() -> CapabilityReport {
    let id = QuickToggleId::ScreenLock;
    match current_session() {
        Ok((_connection, session_id)) => supported(
            id,
            "The current logind session can be locked.",
            "systemd-logind LockSession",
        )
        .with_evidence(CapabilityEvidence::new("session", session_id)),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::SessionControl,
            "logind denied access to the current session.",
            "Use the desktop's lock action or restore normal logind session permissions.",
        ),
        Err(error) => unavailable_capability(id, "systemd-logind", error),
    }
}

pub(crate) fn observe_screen_lock() -> Result<QuickToggleObservation, QuickToggleError> {
    let (_connection, session_id) = current_session()?;
    let confirmation = screen_lock_confirmation(&QuickToggleMutation::Activate { target: None })?
        .expect("screen lock always requires confirmation");
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Actions(vec![ToggleAction {
            target: None,
            label: "Lock this session".to_owned(),
            confirmation,
        }]),
        detail: format!("Lock the current graphical session ({session_id})."),
    })
}

pub(crate) fn screen_lock_confirmation(
    mutation: &QuickToggleMutation,
) -> Result<Option<MutationConfirmation>, QuickToggleError> {
    if !matches!(mutation, QuickToggleMutation::Activate { target: None }) {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "screen lock accepts only its advertised action",
        ));
    }
    let (_connection, session_id) = current_session()?;
    Ok(Some(MutationConfirmation {
        scope: format!("Lock the current graphical session ({session_id}) now."),
        token: format!("lock:{session_id}"),
    }))
}

pub(crate) fn lock_screen(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    if !matches!(mutation, QuickToggleMutation::Activate { target: None }) {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "screen lock accepts only its advertised action",
        ));
    }
    let (connection, session_id) = current_session()?;
    let manager = Proxy::new(&connection, LOGIN_DESTINATION, LOGIN_PATH, LOGIN_INTERFACE)
        .map_err(|error| dbus_error("creating the logind manager proxy", error))?;
    manager
        .call::<_, _, ()>("LockSession", &session_id)
        .map_err(|error| dbus_error("locking the current session", error))
}

pub(crate) fn eject_capability() -> CapabilityReport {
    let id = QuickToggleId::Eject;
    match ejectable_drives() {
        Ok(drives) => supported(
            id,
            format!(
                "UDisks2 is available; {} mounted removable drive(s) are currently eligible.",
                drives.len()
            ),
            "UDisks2 D-Bus",
        )
        .with_evidence(CapabilityEvidence::new(
            "eligible-drive-count",
            drives.len().to_string(),
        )),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::RemovableMedia,
            "UDisks2 denied removable-drive discovery.",
            "Grant normal active-session UDisks2 permissions; Kestrel will not elevate itself.",
        ),
        Err(error) => unavailable_capability(id, "UDisks2", error),
    }
}

pub(crate) fn observe_ejectable_drives() -> Result<QuickToggleObservation, QuickToggleError> {
    let drives = ejectable_drives()?;
    let actions = drives
        .iter()
        .map(|drive| ToggleAction {
            target: Some(drive.target.clone()),
            label: format!("Eject {}", drive.label),
            confirmation: eject_drive_confirmation(drive),
        })
        .collect::<Vec<_>>();
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Actions(actions),
        detail: if drives.is_empty() {
            "No mounted removable, non-system drive is currently eligible for ejection.".to_owned()
        } else {
            format!(
                "{} removable drive(s) can be safely unmounted and ejected.",
                drives.len()
            )
        },
    })
}

pub(crate) fn eject_confirmation(
    mutation: &QuickToggleMutation,
) -> Result<Option<MutationConfirmation>, QuickToggleError> {
    let QuickToggleMutation::Activate {
        target: Some(target),
    } = mutation
    else {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "eject requires one advertised removable-drive target",
        ));
    };
    let drive = ejectable_drives()?
        .into_iter()
        .find(|drive| drive.target == *target)
        .ok_or_else(|| {
            QuickToggleError::new(
                QuickToggleErrorKind::Unavailable,
                "the selected removable drive is no longer eligible for ejection",
            )
        })?;
    Ok(Some(eject_drive_confirmation(&drive)))
}

pub(crate) fn eject_drive(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::Activate {
        target: Some(target),
    } = mutation
    else {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "eject requires one advertised removable-drive target",
        ));
    };
    let (connection, drives) = udisks_objects()?;
    let drive = build_ejectable_drives(&drives)?
        .into_iter()
        .find(|drive| drive.target == *target)
        .ok_or_else(|| {
            QuickToggleError::new(
                QuickToggleErrorKind::Unavailable,
                "the selected removable drive is no longer eligible for ejection",
            )
        })?;
    let options: HashMap<String, OwnedValue> = HashMap::new();
    let filesystem = Proxy::new(
        &connection,
        "org.freedesktop.UDisks2",
        drive.block_path.as_str(),
        UDISKS_FILESYSTEM_INTERFACE,
    )
    .map_err(|error| dbus_error("creating the UDisks2 filesystem proxy", error))?;
    filesystem
        .call::<_, _, ()>("Unmount", &options)
        .map_err(|error| dbus_error("unmounting the removable filesystem", error))?;
    let drive_proxy = Proxy::new(
        &connection,
        "org.freedesktop.UDisks2",
        drive.drive_path.as_str(),
        UDISKS_DRIVE_INTERFACE,
    )
    .map_err(|error| dbus_error("creating the UDisks2 drive proxy", error))?;
    drive_proxy
        .call::<_, _, ()>("Eject", &options)
        .map_err(|error| dbus_error("ejecting the removable drive", error))
}

fn system_connection() -> Result<Connection, QuickToggleError> {
    Connection::system().map_err(|error| {
        QuickToggleError::unavailable(format!("connecting to the system bus failed: {error}"))
    })
}

fn current_session() -> Result<(Connection, String), QuickToggleError> {
    let connection = Connection::system().map_err(|error| {
        QuickToggleError::unavailable(format!("connecting to the system bus failed: {error}"))
    })?;
    let manager = Proxy::new(&connection, LOGIN_DESTINATION, LOGIN_PATH, LOGIN_INTERFACE)
        .map_err(|error| dbus_error("creating the logind manager proxy", error))?;
    let path: OwnedObjectPath = manager
        .call("GetSessionByPID", &(std::process::id()))
        .map_err(|error| dbus_error("locating the current logind session", error))?;
    let session = Proxy::new(
        &connection,
        LOGIN_DESTINATION,
        path.as_str(),
        "org.freedesktop.login1.Session",
    )
    .map_err(|error| dbus_error("creating the logind session proxy", error))?;
    let id: String = session
        .get_property("Id")
        .map_err(|error| dbus_error("reading the current logind session ID", error))?;
    Ok((connection, id))
}

fn bluez_adapter() -> Result<(Connection, OwnedObjectPath, bool), QuickToggleError> {
    let connection = Connection::system().map_err(|error| {
        QuickToggleError::unavailable(format!("connecting to the system bus failed: {error}"))
    })?;
    let manager = Proxy::new(
        &connection,
        "org.bluez",
        "/",
        "org.freedesktop.DBus.ObjectManager",
    )
    .map_err(|error| dbus_error("creating the BlueZ object-manager proxy", error))?;
    let objects: ManagedObjects = manager
        .call("GetManagedObjects", &())
        .map_err(|error| dbus_error("discovering BlueZ adapters", error))?;
    for (path, interfaces) in objects {
        let Some(properties) = interfaces
            .iter()
            .find(|(name, _)| name.as_str() == BLUEZ_ADAPTER_INTERFACE)
            .map(|(_, properties)| properties)
        else {
            continue;
        };
        let powered = bool_property(properties, "Powered")?;
        return Ok((connection, path, powered));
    }
    Err(QuickToggleError::unavailable(
        "BlueZ did not expose a Bluetooth adapter",
    ))
}

fn network_manager() -> Result<(Connection, bool, bool), QuickToggleError> {
    let connection = Connection::system().map_err(|error| {
        QuickToggleError::unavailable(format!("connecting to the system bus failed: {error}"))
    })?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .map_err(|error| dbus_error("creating the NetworkManager proxy", error))?;
    let enabled = proxy
        .get_property("WirelessEnabled")
        .map_err(|error| dbus_error("reading Wi-Fi state", error))?;
    let hardware_enabled = proxy
        .get_property("WirelessHardwareEnabled")
        .map_err(|error| dbus_error("reading Wi-Fi hardware state", error))?;
    Ok((connection, enabled, hardware_enabled))
}

fn ejectable_drives() -> Result<Vec<EjectableDrive>, QuickToggleError> {
    let (_connection, objects) = udisks_objects()?;
    build_ejectable_drives(&objects)
}

fn udisks_objects() -> Result<(Connection, ManagedObjects), QuickToggleError> {
    let connection = Connection::system().map_err(|error| {
        QuickToggleError::unavailable(format!("connecting to the system bus failed: {error}"))
    })?;
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.UDisks2",
        "/org/freedesktop/UDisks2",
        "org.freedesktop.DBus.ObjectManager",
    )
    .map_err(|error| dbus_error("creating the UDisks2 object-manager proxy", error))?;
    let objects = manager
        .call("GetManagedObjects", &())
        .map_err(|error| dbus_error("discovering removable drives", error))?;
    Ok((connection, objects))
}

fn build_ejectable_drives(
    objects: &ManagedObjects,
) -> Result<Vec<EjectableDrive>, QuickToggleError> {
    let mut drives = HashMap::<String, (bool, bool)>::new();
    for (path, interfaces) in objects {
        let Some(properties) = interfaces
            .iter()
            .find(|(name, _)| name.as_str() == UDISKS_DRIVE_INTERFACE)
            .map(|(_, properties)| properties)
        else {
            continue;
        };
        let removable = bool_property(properties, "Removable").unwrap_or(false);
        let ejectable = bool_property(properties, "Ejectable").unwrap_or(false);
        drives.insert(path.as_str().to_owned(), (removable, ejectable));
    }

    let mut result = Vec::new();
    for (block_path, interfaces) in objects {
        let Some(block) = interfaces
            .iter()
            .find(|(name, _)| name.as_str() == UDISKS_BLOCK_INTERFACE)
            .map(|(_, properties)| properties)
        else {
            continue;
        };
        let Some(filesystem) = interfaces
            .iter()
            .find(|(name, _)| name.as_str() == UDISKS_FILESYSTEM_INTERFACE)
            .map(|(_, properties)| properties)
        else {
            continue;
        };
        if bool_property(block, "HintSystem").unwrap_or(true) {
            continue;
        }
        let drive_path = object_path_property(block, "Drive")?;
        if drives.get(drive_path.as_str()) != Some(&(true, true)) {
            continue;
        }
        let mount_points = byte_arrays_property(filesystem, "MountPoints")?;
        if mount_points.is_empty() {
            continue;
        }
        let label = string_property(block, "IdLabel")
            .ok()
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| {
                byte_array_property(block, "Device")
                    .ok()
                    .map(|bytes| c_string(&bytes))
                    .filter(|device| !device.is_empty())
                    .unwrap_or_else(|| object_leaf(block_path))
            });
        result.push(EjectableDrive {
            target: format!("{}|{}", block_path.as_str(), drive_path.as_str()),
            block_path: block_path.clone(),
            drive_path,
            label,
            mount_points,
        });
    }
    result.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(result)
}

fn eject_drive_confirmation(drive: &EjectableDrive) -> MutationConfirmation {
    let mut hasher = DefaultHasher::new();
    drive.target.hash(&mut hasher);
    drive.mount_points.hash(&mut hasher);
    MutationConfirmation {
        scope: format!(
            "Unmount {} mounted filesystem(s) on {} and eject that removable, non-system drive.",
            drive.mount_points.len(),
            drive.label
        ),
        token: format!("eject:{:016x}", hasher.finish()),
    }
}

fn radio_off_confirmation(label: &str) -> MutationConfirmation {
    MutationConfirmation {
        scope: format!("Turn off {label}; active connections and devices may disconnect."),
        token: format!("radio-off:{}", label.to_ascii_lowercase()),
    }
}

fn bool_property(
    properties: &HashMap<String, OwnedValue>,
    name: &str,
) -> Result<bool, QuickToggleError> {
    properties
        .get(name)
        .ok_or_else(|| QuickToggleError::protocol(format!("D-Bus property {name} is missing")))?
        .try_clone()
        .map_err(|error| QuickToggleError::protocol(format!("cloning {name} failed: {error}")))?
        .try_into()
        .map_err(|error| QuickToggleError::protocol(format!("decoding {name} failed: {error}")))
}

fn string_property(
    properties: &HashMap<String, OwnedValue>,
    name: &str,
) -> Result<String, QuickToggleError> {
    properties
        .get(name)
        .ok_or_else(|| QuickToggleError::protocol(format!("D-Bus property {name} is missing")))?
        .try_clone()
        .map_err(|error| QuickToggleError::protocol(format!("cloning {name} failed: {error}")))?
        .try_into()
        .map_err(|error| QuickToggleError::protocol(format!("decoding {name} failed: {error}")))
}

fn object_path_property(
    properties: &HashMap<String, OwnedValue>,
    name: &str,
) -> Result<OwnedObjectPath, QuickToggleError> {
    properties
        .get(name)
        .ok_or_else(|| QuickToggleError::protocol(format!("D-Bus property {name} is missing")))?
        .try_clone()
        .map_err(|error| QuickToggleError::protocol(format!("cloning {name} failed: {error}")))?
        .try_into()
        .map_err(|error| QuickToggleError::protocol(format!("decoding {name} failed: {error}")))
}

fn byte_array_property(
    properties: &HashMap<String, OwnedValue>,
    name: &str,
) -> Result<Vec<u8>, QuickToggleError> {
    properties
        .get(name)
        .ok_or_else(|| QuickToggleError::protocol(format!("D-Bus property {name} is missing")))?
        .try_clone()
        .map_err(|error| QuickToggleError::protocol(format!("cloning {name} failed: {error}")))?
        .try_into()
        .map_err(|error| QuickToggleError::protocol(format!("decoding {name} failed: {error}")))
}

fn byte_arrays_property(
    properties: &HashMap<String, OwnedValue>,
    name: &str,
) -> Result<Vec<Vec<u8>>, QuickToggleError> {
    properties
        .get(name)
        .ok_or_else(|| QuickToggleError::protocol(format!("D-Bus property {name} is missing")))?
        .try_clone()
        .map_err(|error| QuickToggleError::protocol(format!("cloning {name} failed: {error}")))?
        .try_into()
        .map_err(|error| QuickToggleError::protocol(format!("decoding {name} failed: {error}")))
}

fn c_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|byte| *byte == 0).next().unwrap_or_default()).into_owned()
}

fn object_leaf(path: &OwnedObjectPath) -> String {
    path.as_str()
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

fn invalid_switch(label: &str) -> QuickToggleError {
    QuickToggleError::new(
        QuickToggleErrorKind::InvalidRequest,
        format!("{label} accepts only an enabled/disabled value"),
    )
}

fn dbus_error(context: &str, error: zbus::Error) -> QuickToggleError {
    let message = error.to_string();
    let kind = if message.contains("AccessDenied")
        || message.contains("NotAuthorized")
        || message.contains("InteractiveAuthorizationRequired")
    {
        QuickToggleErrorKind::PermissionDenied
    } else if message.contains("ServiceUnknown") || message.contains("NameHasNoOwner") {
        QuickToggleErrorKind::Unavailable
    } else {
        QuickToggleErrorKind::Protocol
    };
    QuickToggleError::new(kind, format!("{context}: {error}"))
}

fn dbus_capability_error(
    id: QuickToggleId,
    summary: &str,
    error: zbus::Error,
    permission: Permission,
) -> CapabilityReport {
    let error = dbus_error(summary, error);
    if error.kind == QuickToggleErrorKind::PermissionDenied {
        permission_denied(id, permission, summary, error.message)
    } else {
        unavailable_capability(id, "D-Bus service", error)
    }
}

fn unavailable_capability(
    id: QuickToggleId,
    dependency: &str,
    error: QuickToggleError,
) -> CapabilityReport {
    let status = if error.kind == QuickToggleErrorKind::Unavailable {
        CapabilityStatus::MissingDependency {
            name: dependency.to_owned(),
        }
    } else {
        CapabilityStatus::Unsupported {
            reason: error.message.clone(),
        }
    };
    CapabilityReport::new(
        id.feature_id(),
        status,
        format!("{} is unavailable on this session.", id.label()),
    )
    .with_remediation(error.message)
}
