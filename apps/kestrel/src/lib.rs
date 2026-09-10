//! Application-level configuration and runtime composition without UI ownership.

mod config;
mod runtime;
mod view_model;

pub use config::{
    configuration_path, load, save, ConfigurationLoadError, ConfigurationWarning,
    LoadedConfiguration,
};
pub use runtime::ApplicationRuntime;
pub use view_model::{
    ApplicationViewModel, CapabilityKindViewModel, CapabilityStatusViewModel, CapabilityViewModel,
    ConfigurationWarningViewModel, FeatureLifecycleViewModel, FeatureViewModel,
    RemediationViewModel,
};
