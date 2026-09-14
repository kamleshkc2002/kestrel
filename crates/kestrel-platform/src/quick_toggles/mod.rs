//! Capability-gated, user-session-safe quick-toggle adapters.

mod backlight;
mod desktop;
mod session;
mod storage;

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use kestrel_core::{CapabilityReport, CapabilityStatus};

use crate::CapabilityProbe;

pub const KEEP_AWAKE_ID: &str = "power.keep-awake";
pub const APPEARANCE_ID: &str = "desktop.appearance";
pub const BRIGHTNESS_ID: &str = "display.brightness";
pub const KEYBOARD_LIGHT_ID: &str = "keyboard.backlight";
pub const BLUETOOTH_ID: &str = "radio.bluetooth";
pub const WIFI_ID: &str = "radio.wifi";
pub const EMPTY_TRASH_ID: &str = "files.empty-trash";
pub const EJECT_ID: &str = "storage.eject";
pub const HIDDEN_FILES_ID: &str = "files.show-hidden";
pub const DESKTOP_ICONS_ID: &str = "desktop.icons";
pub const SCREEN_LOCK_ID: &str = "session.lock";
pub const BATTERY_ALERTS_ID: &str = "power.battery-alerts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuickToggleId {
    KeepAwake,
    Appearance,
    Brightness,
    KeyboardLight,
    Bluetooth,
    Wifi,
    EmptyTrash,
    Eject,
    HiddenFiles,
    DesktopIcons,
    ScreenLock,
    BatteryAlerts,
}

pub const ALL_QUICK_TOGGLES: [QuickToggleId; 12] = [
    QuickToggleId::KeepAwake,
    QuickToggleId::Appearance,
    QuickToggleId::Brightness,
    QuickToggleId::KeyboardLight,
    QuickToggleId::Bluetooth,
    QuickToggleId::Wifi,
    QuickToggleId::EmptyTrash,
    QuickToggleId::Eject,
    QuickToggleId::HiddenFiles,
    QuickToggleId::DesktopIcons,
    QuickToggleId::ScreenLock,
    QuickToggleId::BatteryAlerts,
];

impl QuickToggleId {
    pub const fn feature_id(self) -> &'static str {
        match self {
            Self::KeepAwake => KEEP_AWAKE_ID,
            Self::Appearance => APPEARANCE_ID,
            Self::Brightness => BRIGHTNESS_ID,
            Self::KeyboardLight => KEYBOARD_LIGHT_ID,
            Self::Bluetooth => BLUETOOTH_ID,
            Self::Wifi => WIFI_ID,
            Self::EmptyTrash => EMPTY_TRASH_ID,
            Self::Eject => EJECT_ID,
            Self::HiddenFiles => HIDDEN_FILES_ID,
            Self::DesktopIcons => DESKTOP_ICONS_ID,
            Self::ScreenLock => SCREEN_LOCK_ID,
            Self::BatteryAlerts => BATTERY_ALERTS_ID,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::KeepAwake => "Keep awake",
            Self::Appearance => "Dark appearance",
            Self::Brightness => "Internal brightness",
            Self::KeyboardLight => "Keyboard light",
            Self::Bluetooth => "Bluetooth",
            Self::Wifi => "Wi-Fi",
            Self::EmptyTrash => "Empty Trash",
            Self::Eject => "Eject removable disks",
            Self::HiddenFiles => "Show hidden files",
            Self::DesktopIcons => "Desktop icons",
            Self::ScreenLock => "Lock screen",
            Self::BatteryAlerts => "Battery alerts",
        }
    }

    pub const fn requirement(self) -> &'static str {
        match self {
            Self::KeepAwake => "A logind user-session inhibitor; no elevated privileges.",
            Self::Appearance => "A writable desktop appearance GSettings key.",
            Self::Brightness => "A writable internal backlight exposed by sysfs.",
            Self::KeyboardLight => "A writable keyboard-backlight LED exposed by sysfs.",
            Self::Bluetooth => "A BlueZ adapter and permission to change its Powered property.",
            Self::Wifi => "NetworkManager and permission to change WirelessEnabled.",
            Self::EmptyTrash => "The current user's local XDG Trash; remote trash is excluded.",
            Self::Eject => "A mounted, removable, non-system UDisks2 drive.",
            Self::HiddenFiles => "A writable supported file-manager GSettings key.",
            Self::DesktopIcons => "A writable supported desktop-icons GSettings key.",
            Self::ScreenLock => "A current logind graphical session.",
            Self::BatteryAlerts => "A system battery and a desktop notification service.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationConfirmation {
    pub scope: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleAction {
    pub target: Option<String>,
    pub label: String,
    pub confirmation: MutationConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickToggleControl {
    Switch {
        enabled: bool,
        confirmation: Option<MutationConfirmation>,
    },
    Level {
        percentage: u8,
    },
    Actions(Vec<ToggleAction>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleObservation {
    pub control: QuickToggleControl,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickToggleMutation {
    SetEnabled(bool),
    SetLevel(u8),
    Activate { target: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickToggleErrorKind {
    Unavailable,
    PermissionDenied,
    InvalidRequest,
    ConfirmationRequired,
    Protocol,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleError {
    pub kind: QuickToggleErrorKind,
    pub message: String,
}

impl QuickToggleError {
    pub fn new(kind: QuickToggleErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(QuickToggleErrorKind::Unavailable, message)
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::new(QuickToggleErrorKind::Protocol, message)
    }

    pub(crate) fn io(context: &str, error: &std::io::Error) -> Self {
        let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
            QuickToggleErrorKind::PermissionDenied
        } else {
            QuickToggleErrorKind::Io
        };
        Self::new(kind, format!("{context}: {error}"))
    }
}

impl fmt::Display for QuickToggleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for QuickToggleError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryReading {
    pub name: String,
    pub capacity: Option<u8>,
    pub status: Option<String>,
}

pub trait BatteryAlertSource: Send + Sync + 'static {
    fn battery_readings(&self) -> Result<Vec<BatteryReading>, QuickToggleError>;
    fn notify_low_battery(&self, summary: &str, body: &str) -> Result<(), QuickToggleError>;
}

pub trait QuickToggleBackend: Send {
    fn capability(&self, id: QuickToggleId) -> CapabilityReport;
    fn observe(&self, id: QuickToggleId) -> Result<QuickToggleObservation, QuickToggleError>;
    fn confirmation(
        &self,
        id: QuickToggleId,
        mutation: &QuickToggleMutation,
    ) -> Result<Option<MutationConfirmation>, QuickToggleError>;
    fn apply(
        &mut self,
        id: QuickToggleId,
        mutation: QuickToggleMutation,
        confirmation_token: Option<&str>,
    ) -> Result<QuickToggleObservation, QuickToggleError>;
}

#[derive(Clone)]
pub struct LinuxQuickToggleBackend {
    sys_root: PathBuf,
    data_home: PathBuf,
    inhibitor: Arc<Mutex<Option<session::InhibitorLease>>>,
    battery_alerts: Arc<Mutex<bool>>,
}

impl fmt::Debug for LinuxQuickToggleBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxQuickToggleBackend")
            .field("sys_root", &self.sys_root)
            .field("data_home", &self.data_home)
            .finish_non_exhaustive()
    }
}

impl Default for LinuxQuickToggleBackend {
    fn default() -> Self {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self::new_with_roots("/sys", data_home)
    }
}

impl LinuxQuickToggleBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_with_roots(sys_root: impl Into<PathBuf>, data_home: impl Into<PathBuf>) -> Self {
        Self {
            sys_root: sys_root.into(),
            data_home: data_home.into(),
            inhibitor: Arc::new(Mutex::new(None)),
            battery_alerts: Arc::new(Mutex::new(false)),
        }
    }

    fn validate_confirmation(
        &self,
        id: QuickToggleId,
        mutation: &QuickToggleMutation,
        supplied: Option<&str>,
    ) -> Result<(), QuickToggleError> {
        let expected = self.confirmation(id, mutation)?;
        match expected {
            Some(expected) if supplied != Some(expected.token.as_str()) => {
                Err(QuickToggleError::new(
                    QuickToggleErrorKind::ConfirmationRequired,
                    format!(
                        "{} requires confirmation for: {}",
                        id.label(),
                        expected.scope
                    ),
                ))
            }
            _ => Ok(()),
        }
    }
}

impl BatteryAlertSource for LinuxQuickToggleBackend {
    fn battery_readings(&self) -> Result<Vec<BatteryReading>, QuickToggleError> {
        storage::batteries(&self.sys_root)
    }

    fn notify_low_battery(&self, summary: &str, body: &str) -> Result<(), QuickToggleError> {
        storage::notify_low_battery(summary, body)
    }
}

impl QuickToggleBackend for LinuxQuickToggleBackend {
    fn capability(&self, id: QuickToggleId) -> CapabilityReport {
        match id {
            QuickToggleId::KeepAwake => session::keep_awake_capability(),
            QuickToggleId::Appearance => desktop::appearance_capability(),
            QuickToggleId::Brightness => backlight::capability(
                id,
                &self.sys_root.join("class/backlight"),
                backlight::DeviceKind::Display,
            ),
            QuickToggleId::KeyboardLight => backlight::capability(
                id,
                &self.sys_root.join("class/leds"),
                backlight::DeviceKind::Keyboard,
            ),
            QuickToggleId::Bluetooth => session::bluetooth_capability(),
            QuickToggleId::Wifi => session::wifi_capability(),
            QuickToggleId::EmptyTrash => storage::trash_capability(&self.data_home),
            QuickToggleId::Eject => session::eject_capability(),
            QuickToggleId::HiddenFiles => desktop::hidden_files_capability(),
            QuickToggleId::DesktopIcons => desktop::desktop_icons_capability(),
            QuickToggleId::ScreenLock => session::screen_lock_capability(),
            QuickToggleId::BatteryAlerts => storage::battery_alerts_capability(&self.sys_root),
        }
    }

    fn observe(&self, id: QuickToggleId) -> Result<QuickToggleObservation, QuickToggleError> {
        match id {
            QuickToggleId::KeepAwake => session::observe_keep_awake(&self.inhibitor),
            QuickToggleId::Appearance => desktop::observe_appearance(),
            QuickToggleId::Brightness => backlight::observe(
                &self.sys_root.join("class/backlight"),
                backlight::DeviceKind::Display,
            ),
            QuickToggleId::KeyboardLight => backlight::observe(
                &self.sys_root.join("class/leds"),
                backlight::DeviceKind::Keyboard,
            ),
            QuickToggleId::Bluetooth => session::observe_bluetooth(),
            QuickToggleId::Wifi => session::observe_wifi(),
            QuickToggleId::EmptyTrash => storage::observe_trash(&self.data_home),
            QuickToggleId::Eject => session::observe_ejectable_drives(),
            QuickToggleId::HiddenFiles => desktop::observe_hidden_files(),
            QuickToggleId::DesktopIcons => desktop::observe_desktop_icons(),
            QuickToggleId::ScreenLock => session::observe_screen_lock(),
            QuickToggleId::BatteryAlerts => {
                storage::observe_battery_alerts(&self.sys_root, &self.battery_alerts)
            }
        }
    }

    fn confirmation(
        &self,
        id: QuickToggleId,
        mutation: &QuickToggleMutation,
    ) -> Result<Option<MutationConfirmation>, QuickToggleError> {
        match id {
            QuickToggleId::Bluetooth => session::radio_confirmation("Bluetooth", mutation),
            QuickToggleId::Wifi => session::radio_confirmation("Wi-Fi", mutation),
            QuickToggleId::EmptyTrash => storage::trash_confirmation(&self.data_home, mutation),
            QuickToggleId::Eject => session::eject_confirmation(mutation),
            QuickToggleId::ScreenLock => session::screen_lock_confirmation(mutation),
            _ => Ok(None),
        }
    }

    fn apply(
        &mut self,
        id: QuickToggleId,
        mutation: QuickToggleMutation,
        confirmation_token: Option<&str>,
    ) -> Result<QuickToggleObservation, QuickToggleError> {
        self.validate_confirmation(id, &mutation, confirmation_token)?;
        match id {
            QuickToggleId::KeepAwake => session::set_keep_awake(&self.inhibitor, &mutation)?,
            QuickToggleId::Appearance => desktop::set_appearance(&mutation)?,
            QuickToggleId::Brightness => backlight::set_level(
                &self.sys_root.join("class/backlight"),
                backlight::DeviceKind::Display,
                &mutation,
            )?,
            QuickToggleId::KeyboardLight => backlight::set_level(
                &self.sys_root.join("class/leds"),
                backlight::DeviceKind::Keyboard,
                &mutation,
            )?,
            QuickToggleId::Bluetooth => session::set_bluetooth(&mutation)?,
            QuickToggleId::Wifi => session::set_wifi(&mutation)?,
            QuickToggleId::EmptyTrash => storage::empty_trash(&self.data_home, &mutation)?,
            QuickToggleId::Eject => session::eject_drive(&mutation)?,
            QuickToggleId::HiddenFiles => desktop::set_hidden_files(&mutation)?,
            QuickToggleId::DesktopIcons => desktop::set_desktop_icons(&mutation)?,
            QuickToggleId::ScreenLock => session::lock_screen(&mutation)?,
            QuickToggleId::BatteryAlerts => {
                storage::set_battery_alerts(&self.battery_alerts, &mutation)?
            }
        }
        self.observe(id)
    }
}

#[derive(Clone)]
pub struct QuickToggleCapabilityProbe {
    backend: LinuxQuickToggleBackend,
    id: QuickToggleId,
}

impl QuickToggleCapabilityProbe {
    pub fn new(backend: LinuxQuickToggleBackend, id: QuickToggleId) -> Self {
        Self { backend, id }
    }
}

impl CapabilityProbe for QuickToggleCapabilityProbe {
    fn probe(&self) -> CapabilityReport {
        self.backend.capability(self.id)
    }
}

pub(crate) fn supported(
    id: QuickToggleId,
    summary: impl Into<String>,
    backend: impl Into<String>,
) -> CapabilityReport {
    CapabilityReport::new(id.feature_id(), CapabilityStatus::Supported, summary)
        .with_selected_backend(backend)
}

pub(crate) fn permission_denied(
    id: QuickToggleId,
    permission: kestrel_core::Permission,
    summary: impl Into<String>,
    remediation: impl Into<String>,
) -> CapabilityReport {
    CapabilityReport::new(
        id.feature_id(),
        CapabilityStatus::NeedsPermission { permission },
        summary,
    )
    .with_remediation(remediation)
}

pub(crate) fn path_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_owned()
}
