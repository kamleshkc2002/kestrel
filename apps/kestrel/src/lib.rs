//! Application-level configuration and runtime composition without UI ownership.

mod config;
mod runtime;
mod status_notifier;
mod view_model;

pub use config::{
    ConfigurationLoadError, ConfigurationWarning, LoadedConfiguration, configuration_path, load,
    save,
};
pub use runtime::ApplicationRuntime;
pub use status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration};
pub use view_model::{
    ApplicationViewModel, CapabilityKindViewModel, CapabilityStatusViewModel, CapabilityViewModel,
    ConfigurationWarningViewModel, FeatureLifecycleViewModel, FeatureViewModel,
    RemediationViewModel,
};

/// Commands accepted from application entry surfaces such as the normal window and tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationCommand {
    PresentWindow,
    RefreshCapabilities,
    Quit,
}
