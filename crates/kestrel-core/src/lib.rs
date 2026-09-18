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
    SessionControl,
    DesktopSettings,
    NetworkControl,
    FileDeletion,
    RemovableMedia,
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
pub const CURRENT_CONFIGURATION_SCHEMA_VERSION: u32 = 2;

/// The user's preferred appearance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// A section that can be shown in the application panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSection {
    QuickControls,
    FeatureHub,
}

/// Visibility and ordering for one panel section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSectionConfiguration {
    pub section: PanelSection,
    pub visible: bool,
}

/// Presentation preferences owned by the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfiguration {
    #[serde(default)]
    pub appearance: AppearancePreference,
    #[serde(default = "default_panel_sections")]
    pub panel_sections: Vec<PanelSectionConfiguration>,
}

fn default_panel_sections() -> Vec<PanelSectionConfiguration> {
    vec![
        PanelSectionConfiguration {
            section: PanelSection::QuickControls,
            visible: true,
        },
        PanelSectionConfiguration {
            section: PanelSection::FeatureHub,
            visible: true,
        },
    ]
}

impl Default for UiConfiguration {
    fn default() -> Self {
        Self {
            appearance: AppearancePreference::default(),
            panel_sections: default_panel_sections(),
        }
    }
}

/// Startup preferences owned by the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StartupConfiguration {
    #[serde(default)]
    pub autostart: bool,
}

/// A resource-use level for a feature lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostLevel {
    #[default]
    None,
    Low,
    Moderate,
}

/// Static resource-use metadata for a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceCost {
    #[serde(default)]
    pub idle: CostLevel,
    #[serde(default)]
    pub interaction: CostLevel,
    #[serde(default)]
    pub polling: CostLevel,
}

impl ResourceCost {
    /// Builds resource metadata for idle, interaction, and polling work.
    pub const fn new(idle: CostLevel, interaction: CostLevel, polling: CostLevel) -> Self {
        Self {
            idle,
            interaction,
            polling,
        }
    }
}
/// Non-sensitive, per-feature preferences persisted by the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FeatureConfiguration {
    /// Whether the user has opted into starting this feature.
    #[serde(default)]
    pub enabled: bool,
}

/// An exact, reversible copy of the complete feature preference map.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeatureConfigurationSnapshot {
    pub features: BTreeMap<String, FeatureConfiguration>,
}

/// Versioned, portable application configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationConfiguration {
    /// The schema version from which the configuration was loaded.
    pub schema_version: u32,
    /// Preferences keyed by stable, namespaced feature IDs.
    #[serde(default)]
    pub features: BTreeMap<String, FeatureConfiguration>,
    #[serde(default)]
    pub ui: UiConfiguration,
    #[serde(default)]
    pub startup: StartupConfiguration,
}

impl Default for ApplicationConfiguration {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_CONFIGURATION_SCHEMA_VERSION,
            features: BTreeMap::new(),
            ui: UiConfiguration::default(),
            startup: StartupConfiguration::default(),
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

    /// Captures every feature entry, including the distinction between absent
    /// and disabled entries.
    pub fn snapshot_features(&self) -> FeatureConfigurationSnapshot {
        FeatureConfigurationSnapshot {
            features: self.features.clone(),
        }
    }

    /// Restores a previously captured feature map exactly.
    pub fn restore_feature_snapshot(&mut self, snapshot: FeatureConfigurationSnapshot) {
        self.features = snapshot.features;
    }

    /// Alias for callers that use the noun form of the snapshot operation.
    pub fn feature_configuration_snapshot(&self) -> FeatureConfigurationSnapshot {
        self.snapshot_features()
    }

    /// Restores a snapshot by reference, retaining the reusable snapshot.
    pub fn restore_features(&mut self, snapshot: &FeatureConfigurationSnapshot) {
        self.features = snapshot.features.clone();
    }

    /// Validates schema, stable feature identifiers, and panel section shape.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if self.schema_version != CURRENT_CONFIGURATION_SCHEMA_VERSION {
            return Err(ConfigurationError::UnsupportedSchemaVersion {
                version: self.schema_version,
            });
        }

        for feature_id in self.features.keys() {
            validate_feature_id(feature_id)?;
        }
        self.ui.validate()?;
        Ok(())
    }
}

impl UiConfiguration {
    /// Validates that panel sections are complete and unambiguous.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        let mut quick_controls = false;
        let mut feature_hub = false;
        for entry in &self.panel_sections {
            match entry.section {
                PanelSection::QuickControls if !quick_controls => quick_controls = true,
                PanelSection::FeatureHub if !feature_hub => feature_hub = true,
                PanelSection::QuickControls | PanelSection::FeatureHub => {
                    return Err(ConfigurationError::DuplicatePanelSection);
                }
            }
        }
        if self.panel_sections.len() != 2 || !quick_controls || !feature_hub {
            Err(ConfigurationError::MissingPanelSection)
        } else {
            Ok(())
        }
    }
}

/// An invalid portable configuration contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationError {
    UnsupportedSchemaVersion { version: u32 },
    InvalidFeatureId { feature_id: String },
    DuplicatePanelSection,
    MissingPanelSection,
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
    pub configurable: bool,
    pub cost: ResourceCost,
}

impl FeatureSpec {
    /// Builds a feature descriptor with configurable, resource-free defaults.
    pub fn new(id: &'static str, label: &'static str, capability: CapabilityStatus) -> Self {
        Self {
            id,
            label,
            capability,
            configurable: true,
            cost: ResourceCost::default(),
        }
    }

    /// Marks whether the feature exposes user configuration.
    pub fn with_configurable(mut self, configurable: bool) -> Self {
        self.configurable = configurable;
        self
    }

    /// Supplies static resource-use metadata.
    pub fn with_cost(mut self, cost: ResourceCost) -> Self {
        self.cost = cost;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppearancePreference, ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION,
        CapabilityEvidence, CapabilityReport, CapabilityStatus, ConfigurationError, CostLevel,
        FeatureSpec, ResourceCost,
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

    #[test]
    fn defaults_cover_presentation_and_static_cost_contracts() {
        let configuration = ApplicationConfiguration::default();
        assert_eq!(configuration.ui.appearance, AppearancePreference::System);
        assert_eq!(configuration.ui.panel_sections.len(), 2);
        assert!(
            configuration
                .ui
                .panel_sections
                .iter()
                .all(|section| section.visible)
        );
        assert!(!configuration.startup.autostart);

        let feature = FeatureSpec::new(
            "system.monitor",
            "System monitor",
            CapabilityStatus::Supported,
        )
        .with_configurable(false)
        .with_cost(ResourceCost {
            idle: CostLevel::Low,
            interaction: CostLevel::Moderate,
            polling: CostLevel::Low,
        });
        assert!(!feature.configurable);
        assert_eq!(feature.cost.polling, CostLevel::Low);
    }

    #[test]
    fn feature_snapshot_restores_absence_and_values_exactly() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled("audio.mixer", true)
            .expect("valid feature ID");
        let snapshot = configuration.snapshot_features();
        configuration.features.clear();
        configuration
            .set_feature_enabled("clipboard.history", false)
            .expect("valid feature ID");
        configuration.restore_feature_snapshot(snapshot);
        assert!(configuration.feature_enabled("audio.mixer"));
        assert!(!configuration.features.contains_key("clipboard.history"));
    }
}
