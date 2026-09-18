//! UI-agnostic primitives shared by Kestrel services and front ends.

use std::collections::{BTreeMap, BTreeSet};

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
pub const CURRENT_CONFIGURATION_SCHEMA_VERSION: u32 = 3;

// Canonical bounds shared by validation and services.
pub const MIN_MONITOR_REFRESH_INTERVAL_MILLIS: u64 = 250;
pub const MAX_MONITOR_REFRESH_INTERVAL_MILLIS: u64 = 60_000;
pub const DEFAULT_MONITOR_REFRESH_INTERVAL_MILLIS: u64 = 1_000;
pub const MAX_MONITOR_HISTORY_SAMPLES: u32 = 600;
pub const DEFAULT_MONITOR_HISTORY_SAMPLES: u32 = 120;
pub const MIN_ALERT_SUSTAIN_SAMPLES: u32 = 1;
pub const MAX_ALERT_SUSTAIN_SAMPLES: u32 = 600;
pub const MAX_ALERT_COOLDOWN_SECONDS: u64 = 86_400;
pub const MAX_ALERT_THRESHOLD_PERCENT: f64 = 100.0;
pub const MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS: f64 = 150.0;

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
    Monitoring,
}

impl PanelSection {
    pub const ALL: [PanelSection; 3] = [
        PanelSection::QuickControls,
        PanelSection::FeatureHub,
        PanelSection::Monitoring,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::QuickControls => "Quick Controls",
            Self::FeatureHub => "Feature Hub",
            Self::Monitoring => "Monitoring",
        }
    }
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
    PanelSection::ALL
        .into_iter()
        .map(|section| PanelSectionConfiguration {
            section,
            visible: true,
        })
        .collect()
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
    #[serde(default)]
    pub monitoring: MonitorConfiguration,
}

impl Default for ApplicationConfiguration {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_CONFIGURATION_SCHEMA_VERSION,
            features: BTreeMap::new(),
            ui: UiConfiguration::default(),
            startup: StartupConfiguration::default(),
            monitoring: MonitorConfiguration::default(),
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

    /// Validates schema, stable feature identifiers, panel section shape, and monitoring.
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

        let refresh_interval_millis = self.monitoring.refresh_interval_millis;
        if !(MIN_MONITOR_REFRESH_INTERVAL_MILLIS..=MAX_MONITOR_REFRESH_INTERVAL_MILLIS)
            .contains(&refresh_interval_millis)
        {
            return Err(ConfigurationError::InvalidMonitorRefreshInterval {
                millis: refresh_interval_millis,
            });
        }

        let history_samples = self.monitoring.history_samples;
        if history_samples == 0 || history_samples > MAX_MONITOR_HISTORY_SAMPLES {
            return Err(ConfigurationError::InvalidMonitorHistorySamples {
                samples: history_samples,
            });
        }

        let mut readouts = BTreeSet::new();
        if self.monitoring.readouts.is_empty()
            || self
                .monitoring
                .readouts
                .iter()
                .any(|readout| !readouts.insert(*readout))
        {
            return Err(ConfigurationError::DuplicateMonitorReadout);
        }

        for kind in AlertKind::ALL {
            let rule = self.monitoring.alerts.rule(kind);
            let maximum = match kind {
                AlertKind::Temperature => MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS,
                AlertKind::Cpu | AlertKind::Memory | AlertKind::Disk | AlertKind::Battery => {
                    MAX_ALERT_THRESHOLD_PERCENT
                }
            };
            if !rule.threshold.is_finite() || rule.threshold <= 0.0 || rule.threshold > maximum {
                return Err(ConfigurationError::InvalidAlertThreshold {
                    kind,
                    threshold: rule.threshold,
                });
            }
            if !(MIN_ALERT_SUSTAIN_SAMPLES..=MAX_ALERT_SUSTAIN_SAMPLES)
                .contains(&rule.sustain_samples)
            {
                return Err(ConfigurationError::InvalidAlertSustainSamples {
                    kind,
                    samples: rule.sustain_samples,
                });
            }
            if rule.cooldown_seconds > MAX_ALERT_COOLDOWN_SECONDS {
                return Err(ConfigurationError::InvalidAlertCooldown {
                    kind,
                    seconds: rule.cooldown_seconds,
                });
            }
        }

        Ok(())
    }
}

impl UiConfiguration {
    /// Validates that panel sections are complete and unambiguous.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        let mut seen = [false; PanelSection::ALL.len()];
        for entry in &self.panel_sections {
            let Some(index) = PanelSection::ALL
                .iter()
                .position(|section| *section == entry.section)
            else {
                return Err(ConfigurationError::MissingPanelSection);
            };
            if seen[index] {
                return Err(ConfigurationError::DuplicatePanelSection);
            }
            seen[index] = true;
        }
        if self.panel_sections.len() != PanelSection::ALL.len()
            || seen.iter().any(|section_seen| !section_seen)
        {
            Err(ConfigurationError::MissingPanelSection)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorReadout {
    Cpu,
    Memory,
    Swap,
    Disk,
    Network,
    Temperature,
    Battery,
    Gpu,
}

impl MonitorReadout {
    pub const ALL: [MonitorReadout; 8] = [
        MonitorReadout::Cpu,
        MonitorReadout::Memory,
        MonitorReadout::Swap,
        MonitorReadout::Disk,
        MonitorReadout::Network,
        MonitorReadout::Temperature,
        MonitorReadout::Battery,
        MonitorReadout::Gpu,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Swap => "Swap",
            Self::Disk => "Disk",
            Self::Network => "Network",
            Self::Temperature => "Temperature",
            Self::Battery => "Battery",
            Self::Gpu => "GPU",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Cpu,
    Temperature,
    Memory,
    Disk,
    Battery,
}

impl AlertKind {
    pub const ALL: [AlertKind; 5] = [
        AlertKind::Cpu,
        AlertKind::Temperature,
        AlertKind::Memory,
        AlertKind::Disk,
        AlertKind::Battery,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Temperature => "Temperature",
            Self::Memory => "Memory",
            Self::Disk => "Disk",
            Self::Battery => "Battery",
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Cpu | Self::Memory | Self::Disk | Self::Battery => "%",
        }
    }

    /// True for kinds that alert when the value rises above the threshold.
    pub const fn alerts_above(self) -> bool {
        !matches!(self, Self::Battery)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRuleConfiguration {
    pub enabled: bool,
    pub threshold: f64,
    pub sustain_samples: u32,
    pub cooldown_seconds: u64,
}

impl AlertRuleConfiguration {
    pub const fn new(
        enabled: bool,
        threshold: f64,
        sustain_samples: u32,
        cooldown_seconds: u64,
    ) -> Self {
        Self {
            enabled,
            threshold,
            sustain_samples,
            cooldown_seconds,
        }
    }
}

impl Default for AlertRuleConfiguration {
    fn default() -> Self {
        Self::new(true, 90.0, 5, 900)
    }
}

// f64 cannot derive Eq, but configuration validation rejects non-finite values.
impl Eq for AlertRuleConfiguration {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorAlertConfiguration {
    pub cpu: AlertRuleConfiguration,
    pub temperature: AlertRuleConfiguration,
    pub memory: AlertRuleConfiguration,
    pub disk: AlertRuleConfiguration,
    pub battery: AlertRuleConfiguration,
}

impl MonitorAlertConfiguration {
    pub fn rule(&self, kind: AlertKind) -> &AlertRuleConfiguration {
        match kind {
            AlertKind::Cpu => &self.cpu,
            AlertKind::Temperature => &self.temperature,
            AlertKind::Memory => &self.memory,
            AlertKind::Disk => &self.disk,
            AlertKind::Battery => &self.battery,
        }
    }

    pub fn rule_mut(&mut self, kind: AlertKind) -> &mut AlertRuleConfiguration {
        match kind {
            AlertKind::Cpu => &mut self.cpu,
            AlertKind::Temperature => &mut self.temperature,
            AlertKind::Memory => &mut self.memory,
            AlertKind::Disk => &mut self.disk,
            AlertKind::Battery => &mut self.battery,
        }
    }
}

impl Default for MonitorAlertConfiguration {
    fn default() -> Self {
        Self {
            cpu: AlertRuleConfiguration::new(true, 90.0, 5, 900),
            temperature: AlertRuleConfiguration::new(true, 90.0, 3, 900),
            memory: AlertRuleConfiguration::new(true, 90.0, 5, 900),
            disk: AlertRuleConfiguration::new(true, 90.0, 3, 3_600),
            battery: AlertRuleConfiguration::new(true, 15.0, 1, 900),
        }
    }
}

impl Eq for MonitorAlertConfiguration {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorConfiguration {
    pub refresh_interval_millis: u64,
    pub history_samples: u32,
    pub readouts: Vec<MonitorReadout>,
    pub alerts: MonitorAlertConfiguration,
}

impl Default for MonitorConfiguration {
    fn default() -> Self {
        Self {
            refresh_interval_millis: DEFAULT_MONITOR_REFRESH_INTERVAL_MILLIS,
            history_samples: DEFAULT_MONITOR_HISTORY_SAMPLES,
            readouts: MonitorReadout::ALL.to_vec(),
            alerts: MonitorAlertConfiguration::default(),
        }
    }
}

/// An invalid portable configuration contract.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigurationError {
    UnsupportedSchemaVersion { version: u32 },
    InvalidFeatureId { feature_id: String },
    DuplicatePanelSection,
    MissingPanelSection,
    InvalidMonitorRefreshInterval { millis: u64 },
    InvalidMonitorHistorySamples { samples: u32 },
    DuplicateMonitorReadout,
    InvalidAlertThreshold { kind: AlertKind, threshold: f64 },
    InvalidAlertSustainSamples { kind: AlertKind, samples: u32 },
    InvalidAlertCooldown { kind: AlertKind, seconds: u64 },
}

// f64 cannot derive Eq; retaining Eq keeps error matching ergonomic while
// validation rejects non-finite thresholds.
impl Eq for ConfigurationError {}

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
        AlertKind, AlertRuleConfiguration, AppearancePreference, ApplicationConfiguration,
        CURRENT_CONFIGURATION_SCHEMA_VERSION, CapabilityEvidence, CapabilityReport,
        CapabilityStatus, ConfigurationError, CostLevel, FeatureSpec, MAX_ALERT_COOLDOWN_SECONDS,
        MAX_ALERT_SUSTAIN_SAMPLES, MAX_ALERT_THRESHOLD_PERCENT, MAX_MONITOR_HISTORY_SAMPLES,
        MAX_MONITOR_REFRESH_INTERVAL_MILLIS, MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS,
        MIN_ALERT_SUSTAIN_SAMPLES, MIN_MONITOR_REFRESH_INTERVAL_MILLIS, MonitorReadout,
        PanelSection, PanelSectionConfiguration, ResourceCost, UiConfiguration,
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
        assert_eq!(configuration.ui.panel_sections.len(), 3);
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

    #[test]
    fn panel_section_default_is_complete_in_all_order() {
        let configuration = UiConfiguration::default();
        let sections = configuration
            .panel_sections
            .iter()
            .map(|entry| entry.section)
            .collect::<Vec<_>>();

        assert_eq!(sections, PanelSection::ALL.to_vec());
        assert!(
            configuration
                .panel_sections
                .iter()
                .all(|entry| entry.visible)
        );
        assert_eq!(configuration.validate(), Ok(()));
    }

    #[test]
    fn panel_section_validation_rejects_duplicates_and_missing_sections() {
        let duplicate = UiConfiguration {
            panel_sections: vec![
                PanelSectionConfiguration {
                    section: PanelSection::QuickControls,
                    visible: true,
                },
                PanelSectionConfiguration {
                    section: PanelSection::FeatureHub,
                    visible: true,
                },
                PanelSectionConfiguration {
                    section: PanelSection::Monitoring,
                    visible: true,
                },
                PanelSectionConfiguration {
                    section: PanelSection::Monitoring,
                    visible: false,
                },
            ],
            ..UiConfiguration::default()
        };
        assert_eq!(
            duplicate.validate(),
            Err(ConfigurationError::DuplicatePanelSection)
        );

        let missing = UiConfiguration {
            panel_sections: PanelSection::ALL[..2]
                .iter()
                .map(|&section| PanelSectionConfiguration {
                    section,
                    visible: true,
                })
                .collect(),
            ..UiConfiguration::default()
        };
        assert_eq!(
            missing.validate(),
            Err(ConfigurationError::MissingPanelSection)
        );
    }

    #[test]
    fn default_configuration_validates() {
        let configuration = ApplicationConfiguration::default();

        assert_eq!(configuration.validate(), Ok(()));
        assert_eq!(
            configuration.monitoring.readouts,
            MonitorReadout::ALL.to_vec()
        );
    }

    #[test]
    fn monitor_refresh_interval_bounds_are_enforced() {
        for millis in [
            MIN_MONITOR_REFRESH_INTERVAL_MILLIS,
            MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
        ] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.monitoring.refresh_interval_millis = millis;
            assert_eq!(configuration.validate(), Ok(()));
        }

        for millis in [
            MIN_MONITOR_REFRESH_INTERVAL_MILLIS - 1,
            MAX_MONITOR_REFRESH_INTERVAL_MILLIS + 1,
        ] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.monitoring.refresh_interval_millis = millis;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidMonitorRefreshInterval { millis })
            );
        }
    }

    #[test]
    fn monitor_history_sample_bounds_are_enforced() {
        for samples in [1, MAX_MONITOR_HISTORY_SAMPLES] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.monitoring.history_samples = samples;
            assert_eq!(configuration.validate(), Ok(()));
        }

        for samples in [0, MAX_MONITOR_HISTORY_SAMPLES + 1] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.monitoring.history_samples = samples;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidMonitorHistorySamples { samples })
            );
        }
    }

    #[test]
    fn monitor_readouts_must_be_non_empty_and_unique() {
        let mut empty = ApplicationConfiguration::default();
        empty.monitoring.readouts.clear();
        assert_eq!(
            empty.validate(),
            Err(ConfigurationError::DuplicateMonitorReadout)
        );

        let mut duplicate = ApplicationConfiguration::default();
        duplicate.monitoring.readouts = vec![MonitorReadout::Cpu, MonitorReadout::Cpu];
        assert_eq!(
            duplicate.validate(),
            Err(ConfigurationError::DuplicateMonitorReadout)
        );
    }

    #[test]
    fn alert_threshold_bounds_are_enforced_per_kind() {
        let mut cpu_at_max = ApplicationConfiguration::default();
        cpu_at_max
            .monitoring
            .alerts
            .rule_mut(AlertKind::Cpu)
            .threshold = MAX_ALERT_THRESHOLD_PERCENT;
        assert_eq!(cpu_at_max.validate(), Ok(()));

        let mut temperature_at_max = ApplicationConfiguration::default();
        temperature_at_max
            .monitoring
            .alerts
            .rule_mut(AlertKind::Temperature)
            .threshold = MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS;
        assert_eq!(temperature_at_max.validate(), Ok(()));

        for threshold in [0.0, -1.0, MAX_ALERT_THRESHOLD_PERCENT + 1.0] {
            let mut configuration = ApplicationConfiguration::default();
            configuration
                .monitoring
                .alerts
                .rule_mut(AlertKind::Cpu)
                .threshold = threshold;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidAlertThreshold {
                    kind: AlertKind::Cpu,
                    threshold,
                })
            );
        }

        let mut over_temperature = ApplicationConfiguration::default();
        over_temperature
            .monitoring
            .alerts
            .rule_mut(AlertKind::Temperature)
            .threshold = MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS + 1.0;
        assert_eq!(
            over_temperature.validate(),
            Err(ConfigurationError::InvalidAlertThreshold {
                kind: AlertKind::Temperature,
                threshold: MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS + 1.0,
            })
        );

        let mut non_finite = ApplicationConfiguration::default();
        non_finite
            .monitoring
            .alerts
            .rule_mut(AlertKind::Memory)
            .threshold = f64::NAN;
        match non_finite.validate() {
            Err(ConfigurationError::InvalidAlertThreshold { kind, threshold }) => {
                assert_eq!(kind, AlertKind::Memory);
                assert!(threshold.is_nan());
            }
            other => panic!("unexpected validation result: {other:?}"),
        }
    }

    #[test]
    fn alert_sustain_sample_bounds_are_enforced() {
        for samples in [MIN_ALERT_SUSTAIN_SAMPLES, MAX_ALERT_SUSTAIN_SAMPLES] {
            let mut configuration = ApplicationConfiguration::default();
            configuration
                .monitoring
                .alerts
                .rule_mut(AlertKind::Disk)
                .sustain_samples = samples;
            assert_eq!(configuration.validate(), Ok(()));
        }

        for samples in [0, MAX_ALERT_SUSTAIN_SAMPLES + 1] {
            let mut configuration = ApplicationConfiguration::default();
            configuration
                .monitoring
                .alerts
                .rule_mut(AlertKind::Disk)
                .sustain_samples = samples;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidAlertSustainSamples {
                    kind: AlertKind::Disk,
                    samples,
                })
            );
        }
    }

    #[test]
    fn alert_cooldown_bound_is_enforced() {
        let mut accepted = ApplicationConfiguration::default();
        accepted
            .monitoring
            .alerts
            .rule_mut(AlertKind::Battery)
            .cooldown_seconds = MAX_ALERT_COOLDOWN_SECONDS;
        assert_eq!(accepted.validate(), Ok(()));

        let mut rejected = ApplicationConfiguration::default();
        rejected
            .monitoring
            .alerts
            .rule_mut(AlertKind::Battery)
            .cooldown_seconds = MAX_ALERT_COOLDOWN_SECONDS + 1;
        assert_eq!(
            rejected.validate(),
            Err(ConfigurationError::InvalidAlertCooldown {
                kind: AlertKind::Battery,
                seconds: MAX_ALERT_COOLDOWN_SECONDS + 1,
            })
        );
    }

    #[test]
    fn alert_kind_labels_and_units_are_stable() {
        assert_eq!(
            AlertKind::ALL
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>(),
            vec!["CPU", "Temperature", "Memory", "Disk", "Battery"]
        );
        assert_eq!(
            AlertKind::ALL
                .iter()
                .map(|kind| kind.unit())
                .collect::<Vec<_>>(),
            vec!["%", "°C", "%", "%", "%"]
        );
        assert!(AlertKind::ALL[..4].iter().all(|kind| kind.alerts_above()));
        assert!(!AlertKind::Battery.alerts_above());
    }

    #[test]
    fn alert_rule_defaults_preserve_shape() {
        let default = AlertRuleConfiguration::default();
        assert_eq!(default, AlertRuleConfiguration::new(true, 90.0, 5, 900));
    }
}
