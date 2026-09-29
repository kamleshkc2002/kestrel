//! Application-level configuration and runtime composition without UI ownership.

mod autostart;
mod config;
mod runtime;
mod status_notifier;
mod view_model;

pub use autostart::{AutostartError, DESKTOP_FILE_NAME, autostart_path, disable, enable};
pub use config::{
    ConfigurationLoadError, ConfigurationWarning, LoadedConfiguration, configuration_path,
    export_file, export_string, import_file, import_string, load, save,
};
pub use kestrel_core::{
    AlertKind, AppearancePreference, AudioDisconnectPolicy, AudioOutputSwitch, MonitorReadout,
    PanelSection,
};
pub use kestrel_platform::quick_toggles::{QuickToggleId, QuickToggleMutation};
pub use kestrel_services::audio::{AudioCommand, AudioCycleDirection};
pub use kestrel_services::quick_toggles::QuickToggleCommand;
pub use runtime::{ApplicationRuntime, FeaturePreset};
pub use status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration};
pub use view_model::{
    ActiveAlertViewModel, AlertRuleViewModel, ApplicationViewModel, AudioOutputGroupViewModel,
    AudioOutputViewModel, AudioPolicyViewModel, AudioStreamViewModel, AudioViewModel,
    CapabilityKindViewModel, CapabilityStatusViewModel, CapabilityViewModel,
    ConfigurationWarningViewModel, ConfirmationViewModel, FeatureLifecycleViewModel,
    FeatureViewModel, MonitorReadoutSettingViewModel, MonitorReadoutViewModel, MonitorViewModel,
    PanelSectionViewModel, QuickToggleActionViewModel, QuickToggleControlViewModel,
    QuickToggleViewModel, RemediationViewModel,
};

/// Direction for moving one of the ordered panel sections or monitoring readouts.
///
/// Monitoring readouts use the same ordering semantics as panel sections: `Up`
/// moves a readout toward the beginning of the configured readout order and
/// `Down` moves it toward the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMoveDirection {
    Up,
    Down,
}

/// Commands accepted from application entry surfaces such as the normal window and tray menu.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationCommand {
    PresentWindow,
    RefreshCapabilities,
    QuickToggle(QuickToggleCommand),
    SetFeatureEnabled {
        feature_id: String,
        enabled: bool,
    },
    ApplyPreset(FeaturePreset),
    UndoPreset,
    SetAppearance(AppearancePreference),
    SetAutostart(bool),
    SetPanelVisibility {
        section: PanelSection,
        visible: bool,
    },
    MovePanelSection {
        section: PanelSection,
        direction: PanelMoveDirection,
    },
    SetMonitorReadoutVisible {
        readout: MonitorReadout,
        visible: bool,
    },
    MoveMonitorReadout {
        readout: MonitorReadout,
        direction: PanelMoveDirection,
    },
    SetAlertEnabled {
        kind: AlertKind,
        enabled: bool,
    },
    SetAlertThreshold {
        kind: AlertKind,
        threshold: f64,
    },
    /// A mixer mutation applied through the opt-in audio service.
    Audio(AudioCommand),
    /// The amplification ceiling for the mixer, between 100% and the hard cap.
    SetAudioBoostPercent(u8),
    SetAudioOutputSwitch(AudioOutputSwitch),
    SetAudioDisconnectPolicy(AudioDisconnectPolicy),
    SetAudioDisconnectVolumePercent(u8),
    SetAudioIncludeInactiveStreams(bool),
    ImportConfiguration(std::path::PathBuf),
    ExportConfiguration(std::path::PathBuf),
    Quit,
}
