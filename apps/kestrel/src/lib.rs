//! Application-level configuration and runtime composition without UI ownership.

mod config;
mod runtime;

pub use config::{
    configuration_path, load, save, ConfigurationLoadError, ConfigurationWarning,
    LoadedConfiguration,
};
pub use runtime::ApplicationRuntime;
