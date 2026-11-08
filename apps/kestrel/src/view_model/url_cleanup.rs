//! URL cleanup rules presentation.

use kestrel_core::{UrlCleanupConfiguration, UrlRules};

/// The effective local rules and the automatic-cleanup preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlCleanupViewModel {
    pub automatic: bool,
    /// Extra parameter names that apply to every site.
    pub extra_global: Vec<String>,
    /// Built-ins merged with the configured extras and site overrides.
    pub rules: UrlRules,
}

impl UrlCleanupViewModel {
    pub(crate) fn new(configuration: &UrlCleanupConfiguration) -> Self {
        Self {
            automatic: configuration.automatic,
            extra_global: configuration.extra_global.clone(),
            rules: UrlRules::from_configuration(configuration),
        }
    }
}
