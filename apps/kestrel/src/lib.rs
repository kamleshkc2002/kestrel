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
pub use kestrel_core::{AppearancePreference, PanelSection};
pub use kestrel_platform::quick_toggles::{QuickToggleId, QuickToggleMutation};
pub use kestrel_services::quick_toggles::QuickToggleCommand;
pub use runtime::{ApplicationRuntime, FeaturePreset};
pub use status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration};
pub use view_model::{
    ApplicationViewModel, CapabilityKindViewModel, CapabilityStatusViewModel, CapabilityViewModel,
    ConfigurationWarningViewModel, ConfirmationViewModel, FeatureLifecycleViewModel,
    FeatureViewModel, PanelSectionViewModel, QuickToggleActionViewModel,
    QuickToggleControlViewModel, QuickToggleViewModel, RemediationViewModel,
};

/// Direction for moving one of the two ordered panel sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMoveDirection {
    Up,
    Down,
}

/// Commands accepted from application entry surfaces such as the normal window and tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    ImportConfiguration(std::path::PathBuf),
    ExportConfiguration(std::path::PathBuf),
    Quit,
}
