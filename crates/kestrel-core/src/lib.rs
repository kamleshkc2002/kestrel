//! UI-agnostic primitives shared by Kestrel services and front ends.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The runtime status of a feature on the current Linux session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// The feature can run without additional setup.
    Supported,
    /// The feature runs with documented limitations.
    Limited { reason: String },
    /// The feature requires a portal grant, udev rule, or other permission.
    NeedsPermission { permission: Permission },
    /// The feature requires an optional executable, service, or library.
    MissingDependency { name: String },
    /// The desktop, compositor, hardware, or package mode cannot support the feature.
    Unsupported { reason: String },
}

/// A user-facing category of permission or access requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ScreenCapture,
    GlobalShortcut,
    InputInjection,
    HardwareControl,
    Camera,
    Notifications,
}
/// Non-sensitive evidence that supports a capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    pub key: String,
    pub value: String,
}

impl CapabilityEvidence {
    /// Builds evidence from a stable key and a sanitized value.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// The current version of Kestrel's non-sensitive configuration schema.
pub const CURRENT_CONFIGURATION_SCHEMA_VERSION: u32 = 1;

/// Non-sensitive, per-feature preferences persisted by the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FeatureConfiguration {
    /// Whether the user has opted into starting this feature.
    #[serde(default)]
    pub enabled: bool,
}

/// Versioned, portable application configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationConfiguration {
    /// The schema version from which the configuration was loaded.
    pub schema_version: u32,
    /// Preferences keyed by stable, namespaced feature IDs.
    #[serde(default)]
    pub features: BTreeMap<String, FeatureConfiguration>,
}

impl Default for ApplicationConfiguration {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_CONFIGURATION_SCHEMA_VERSION,
            features: BTreeMap::new(),
        }
    }
}

impl ApplicationConfiguration {
    /// Returns whether a known feature should be started by default.
    pub fn feature_enabled(&self, feature_id: &str) -> bool {
        self.features
            .get(feature_id)
            .is_some_and(|configuration| configuration.enabled)
    }

    /// Records a user's enablement preference for a stable feature ID.
    pub fn set_feature_enabled(
        &mut self,
        feature_id: impl Into<String>,
        enabled: bool,
    ) -> Result<(), ConfigurationError> {
        let feature_id = feature_id.into();
        validate_feature_id(&feature_id)?;
        self.features
            .insert(feature_id, FeatureConfiguration { enabled });
        Ok(())
    }

    /// Validates schema and stable feature identifiers without performing I/O.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if self.schema_version != CURRENT_CONFIGURATION_SCHEMA_VERSION {
            return Err(ConfigurationError::UnsupportedSchemaVersion {
                version: self.schema_version,
            });
        }

        for feature_id in self.features.keys() {
            validate_feature_id(feature_id)?;
        }

        Ok(())
    }
}

/// An invalid portable configuration contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationError {
    UnsupportedSchemaVersion { version: u32 },
    InvalidFeatureId { feature_id: String },
}

/// Validates a stable namespaced identifier without depending on a UI or platform.
pub fn validate_feature_id(feature_id: &str) -> Result<(), ConfigurationError> {
    let valid = !feature_id.is_empty()
        && feature_id.split('.').all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        });

    if valid {
        Ok(())
    } else {
        Err(ConfigurationError::InvalidFeatureId {
            feature_id: feature_id.to_owned(),
        })
    }
}

/// A structured, UI-agnostic explanation of a feature's current availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityReport {
    pub feature_id: &'static str,
    pub status: CapabilityStatus,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub alternatives_considered: Vec<String>,
    pub remediation: Option<String>,
    pub evidence: Vec<CapabilityEvidence>,
}

impl CapabilityReport {
    /// Builds a report from the feature's current status and user-facing summary.
    pub fn new(
        feature_id: &'static str,
        status: CapabilityStatus,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            feature_id,
            status,
            summary: summary.into(),
            selected_backend: None,
            alternatives_considered: Vec::new(),
            remediation: None,
            evidence: Vec::new(),
        }
    }

    /// Adds the adapter actually selected for this report.
    pub fn with_selected_backend(mut self, backend: impl Into<String>) -> Self {
        self.selected_backend = Some(backend.into());
        self
    }

    /// Adds a user-facing action that can address a limited capability.
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }

    /// Records an alternative that was considered but not selected.
    pub fn with_alternative(mut self, alternative: impl Into<String>) -> Self {
        self.alternatives_considered.push(alternative.into());
        self
    }

    /// Records sanitized evidence without performing any I/O.
    pub fn with_evidence(mut self, evidence: CapabilityEvidence) -> Self {
        self.evidence.push(evidence);
        self
    }
}

/// Metadata that every independently enabled Kestrel feature must provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub capability: CapabilityStatus,
}

impl FeatureSpec {
    /// Builds a feature descriptor whose runtime status is supplied by a backend probe.
    pub fn new(id: &'static str, label: &'static str, capability: CapabilityStatus) -> Self {
        Self {
            id,
            label,
            capability,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationConfiguration, CapabilityEvidence, CapabilityReport, CapabilityStatus,
        ConfigurationError, FeatureSpec, CURRENT_CONFIGURATION_SCHEMA_VERSION,
    };

    #[test]
    fn feature_spec_preserves_its_capability_status() {
        let feature = FeatureSpec::new("audio.mixer", "Audio mixer", CapabilityStatus::Supported);

        assert_eq!(feature.id, "audio.mixer");
        assert_eq!(feature.capability, CapabilityStatus::Supported);
    }

    #[test]
    fn capability_report_preserves_status_and_remediation() {
        let report = CapabilityReport::new(
            "clipboard.history",
            CapabilityStatus::Limited {
                reason: "History is disabled by default.".to_string(),
            },
            "Clipboard history requires opt-in.",
        )
        .with_selected_backend("Wayland data-control")
        .with_remediation("Enable history after configuring retention bounds.")
        .with_alternative("X11 selection")
        .with_evidence(CapabilityEvidence::new("clipboard_content_read", "false"));

        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert_eq!(
            report.remediation.as_deref(),
            Some("Enable history after configuring retention bounds.")
        );
        assert_eq!(report.evidence[0].key, "clipboard_content_read");
    }

    #[test]
    fn configuration_tracks_enablement_by_stable_feature_id() {
        let mut configuration = ApplicationConfiguration::default();

        configuration
            .set_feature_enabled("audio.mixer", true)
            .expect("valid feature ID");

        assert!(configuration.feature_enabled("audio.mixer"));
        assert!(!configuration.feature_enabled("clipboard.history"));
        assert_eq!(
            configuration.schema_version,
            CURRENT_CONFIGURATION_SCHEMA_VERSION
        );
    }

    #[test]
    fn configuration_rejects_malformed_feature_ids() {
        let mut configuration = ApplicationConfiguration::default();

        let error = configuration
            .set_feature_enabled("not a feature", true)
            .expect_err("spaces are not valid in stable feature IDs");

        assert_eq!(
            error,
            ConfigurationError::InvalidFeatureId {
                feature_id: "not a feature".to_string(),
            }
        );
    }
}
