//! UI-agnostic primitives shared by Kestrel services and front ends.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Serialize};

/// The runtime status of a feature on the current Linux session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    Supported,
    Limited { reason: String },
    NeedsPermission { permission: Permission },
    MissingDependency { name: String },
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

/// Values above this are software amplification.
pub const UNAMPLIFIED_AUDIO_VOLUME_PERCENT: u8 = 100;
/// The hard cap for amplified playback volume; no command may exceed it.
pub const MAX_AUDIO_BOOST_PERCENT: u8 = 150;
pub const DEFAULT_AUDIO_BOOST_PERCENT: u8 = 130;
pub const DEFAULT_AUDIO_DISCONNECT_VOLUME_PERCENT: u8 = 100;

pub const DEFAULT_CLIPBOARD_MAX_ITEMS: u32 = 100;
pub const MAX_CLIPBOARD_MAX_ITEMS: u32 = 1000;
pub const MIN_CLIPBOARD_ITEM_BYTES: u32 = 1024;
pub const DEFAULT_CLIPBOARD_ITEM_BYTES: u32 = 1024 * 1024;
pub const MAX_CLIPBOARD_ITEM_BYTES: u32 = 16 * 1024 * 1024;
pub const DEFAULT_CLIPBOARD_IMAGE_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_CLIPBOARD_IMAGE_BYTES: u32 = 64 * 1024 * 1024;
pub const DEFAULT_CLIPBOARD_FILE_ENTRIES: u32 = 64;
pub const MAX_CLIPBOARD_FILE_ENTRIES: u32 = 1024;
pub const DEFAULT_CLIPBOARD_TOTAL_BYTES: u32 = 16 * 1024 * 1024;
pub const MAX_CLIPBOARD_TOTAL_BYTES: u32 = 128 * 1024 * 1024;
pub const DEFAULT_CLIPBOARD_AGE_HOURS: u32 = 24;
pub const MAX_CLIPBOARD_AGE_HOURS: u32 = 24 * 30;
pub const MAX_CLIPBOARD_CLEAR_SECONDS: u64 = 86_400;

pub const MIN_SNIPPET_CONTENT_BYTES: u32 = 1;
pub const DEFAULT_SNIPPET_CONTENT_BYTES: u32 = 64 * 1024;
pub const MAX_SNIPPET_CONTENT_BYTES: u32 = 1024 * 1024;
pub const MIN_SNIPPET_NAME_CHARS: usize = 1;
pub const MAX_SNIPPET_NAME_CHARS: usize = 64;
pub const MAX_SNIPPET_FOLDER_CHARS: usize = 64;
pub const MAX_SNIPPET_TRIGGER_CHARS: usize = 32;
pub const MAX_SNIPPET_VARIABLES: usize = 32;
pub const MIN_SNIPPET_CLIPBOARD_BYTES: u32 = 64;
pub const DEFAULT_SNIPPET_CLIPBOARD_BYTES: u32 = 4096;
pub const MAX_SNIPPET_CLIPBOARD_BYTES: u32 = 64 * 1024;
pub const MIN_SNIPPET_INSERT_TIMEOUT_MILLIS: u64 = 250;
pub const DEFAULT_SNIPPET_INSERT_TIMEOUT_MILLIS: u64 = 2_000;
pub const MAX_SNIPPET_INSERT_TIMEOUT_MILLIS: u64 = 10_000;

// Bounded, keyboard-first command-bar data.
pub const MIN_COMMAND_RESULTS: u32 = 5;
pub const DEFAULT_COMMAND_RESULTS: u32 = 12;
pub const MAX_COMMAND_RESULTS: u32 = 50;
pub const MAX_COMMAND_FILE_ROOTS: usize = 8;
pub const MAX_COMMAND_FILE_DEPTH: u32 = 4;
pub const MAX_COMMAND_FILE_ENTRIES: u32 = 2000;
pub const MAX_COMMAND_SCRIPTS: usize = 16;
pub const MAX_COMMAND_SCRIPT_ARGS: usize = 16;
pub const MIN_COMMAND_SCRIPT_TIMEOUT_MILLIS: u64 = 250;
pub const DEFAULT_COMMAND_SCRIPT_TIMEOUT_MILLIS: u64 = 5_000;
pub const MAX_COMMAND_SCRIPT_TIMEOUT_MILLIS: u64 = 60_000;
pub const MIN_COMMAND_SCRIPT_OUTPUT_BYTES: u32 = 1024;
pub const DEFAULT_COMMAND_SCRIPT_OUTPUT_BYTES: u32 = 64 * 1024;
pub const MAX_COMMAND_SCRIPT_OUTPUT_BYTES: u32 = 1024 * 1024;
pub const MAX_COMMAND_USAGE_ENTRIES: usize = 200;
pub const MAX_COMMAND_ALIAS_CHARS: usize = 32;
/// Speed-test transfer bounds; the provider rejects downloads at 100 MB.
pub const MIN_SPEED_TEST_DOWNLOAD_MEGABYTES: u32 = 1;
pub const DEFAULT_SPEED_TEST_DOWNLOAD_MEGABYTES: u32 = 25;
pub const MAX_SPEED_TEST_DOWNLOAD_MEGABYTES: u32 = 90;
pub const DEFAULT_SPEED_TEST_UPLOAD_MEGABYTES: u32 = 10;
pub const MAX_SPEED_TEST_UPLOAD_MEGABYTES: u32 = 25;
pub const MIN_SPEED_TEST_TIMEOUT_SECONDS: u32 = 5;
pub const DEFAULT_SPEED_TEST_TIMEOUT_SECONDS: u32 = 30;
pub const MAX_SPEED_TEST_TIMEOUT_SECONDS: u32 = 120;
pub const SPEED_TEST_BYTES_PER_MEGABYTE: u64 = 1_000_000;

// Recent-capture bounds.
pub const DEFAULT_CAPTURE_MAX_ENTRIES: u32 = 30;
pub const MAX_CAPTURE_MAX_ENTRIES: u32 = 200;
pub const DEFAULT_CAPTURE_MAX_TOTAL_MEGABYTES: u32 = 256;
pub const MAX_CAPTURE_MAX_TOTAL_MEGABYTES: u32 = 2048;
pub const DEFAULT_CAPTURE_MAX_AGE_HOURS: u32 = 168;
pub const MAX_CAPTURE_MAX_AGE_HOURS: u32 = 720;
pub const CAPTURE_BYTES_PER_MEGABYTE: u64 = 1_048_576;

// Global-shortcut bounds.
pub const MAX_SHORTCUT_BINDINGS: usize = 32;
pub const MAX_SHORTCUT_COMMAND_CHARS: usize = 64;

pub const MIN_COMMAND_QUERY_CHARS: usize = 1;

/// One user-configured local script action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandScriptConfiguration {
    pub name: String,
    /// An absolute path or a bare executable name resolved on `PATH`.
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_command_script_timeout")]
    pub timeout_millis: u64,
    #[serde(default = "default_command_script_output")]
    pub output_bytes: u32,
}

fn default_command_script_timeout() -> u64 {
    DEFAULT_COMMAND_SCRIPT_TIMEOUT_MILLIS
}

fn default_command_script_output() -> u32 {
    DEFAULT_COMMAND_SCRIPT_OUTPUT_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandBarConfiguration {
    #[serde(default = "default_command_results")]
    pub max_results: u32,
    #[serde(default = "default_true")]
    pub enable_applications: bool,
    /// File search is limited to the configured roots.
    #[serde(default)]
    pub enable_files: bool,
    #[serde(default = "default_true")]
    pub enable_scripts: bool,
    #[serde(default = "default_true")]
    pub enable_emoji: bool,
    #[serde(default)]
    pub file_roots: Vec<String>,
    #[serde(default)]
    pub scripts: Vec<CommandScriptConfiguration>,
}

fn default_command_results() -> u32 {
    DEFAULT_COMMAND_RESULTS
}

impl Default for CommandBarConfiguration {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_COMMAND_RESULTS,
            enable_applications: true,
            enable_files: false,
            enable_scripts: true,
            enable_emoji: true,
            file_roots: Vec::new(),
            scripts: Vec::new(),
        }
    }
}

impl CommandBarConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(MIN_COMMAND_RESULTS..=MAX_COMMAND_RESULTS).contains(&self.max_results) {
            return Err(ConfigurationError::InvalidCommandResults {
                results: self.max_results,
            });
        }
        if self.enable_files && self.file_roots.is_empty() {
            return Err(ConfigurationError::MissingCommandFileRoot);
        }
        if self.file_roots.len() > MAX_COMMAND_FILE_ROOTS {
            return Err(ConfigurationError::TooManyCommandFileRoots {
                roots: self.file_roots.len(),
            });
        }
        let mut seen_roots: Vec<&str> = Vec::new();
        for root in &self.file_roots {
            let trimmed = root.trim();
            if trimmed.is_empty() || !trimmed.starts_with('/') {
                return Err(ConfigurationError::InvalidCommandFileRoot { root: root.clone() });
            }
            if seen_roots.contains(&trimmed) {
                return Err(ConfigurationError::DuplicateCommandFileRoot {
                    root: trimmed.to_string(),
                });
            }
            seen_roots.push(trimmed);
        }
        if self.scripts.len() > MAX_COMMAND_SCRIPTS {
            return Err(ConfigurationError::TooManyCommandScripts {
                scripts: self.scripts.len(),
            });
        }
        let mut seen_scripts: Vec<&str> = Vec::new();
        for script in &self.scripts {
            script.validate()?;
            let name = script.name.trim();
            if seen_scripts.contains(&name) {
                return Err(ConfigurationError::DuplicateCommandScriptName {
                    name: name.to_string(),
                });
            }
            seen_scripts.push(name);
        }
        Ok(())
    }
}

impl CommandScriptConfiguration {
    /// Validates one script, so a loader can drop just the malformed entry.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > MAX_COMMAND_ALIAS_CHARS {
            return Err(ConfigurationError::InvalidCommandScriptName {
                name: self.name.clone(),
            });
        }
        let executable = self.executable.trim();
        let bare_name = !executable.is_empty() && !executable.contains('/');
        if executable.is_empty() || ((!executable.starts_with('/')) && !bare_name) {
            return Err(ConfigurationError::InvalidCommandScriptExecutable {
                executable: self.executable.clone(),
            });
        }
        if self.args.len() > MAX_COMMAND_SCRIPT_ARGS {
            return Err(ConfigurationError::InvalidCommandScriptArgs {
                name: name.to_string(),
            });
        }
        if !(MIN_COMMAND_SCRIPT_TIMEOUT_MILLIS..=MAX_COMMAND_SCRIPT_TIMEOUT_MILLIS)
            .contains(&self.timeout_millis)
        {
            return Err(ConfigurationError::InvalidCommandScriptTimeout {
                name: name.to_string(),
                millis: self.timeout_millis,
            });
        }
        if !(MIN_COMMAND_SCRIPT_OUTPUT_BYTES..=MAX_COMMAND_SCRIPT_OUTPUT_BYTES)
            .contains(&self.output_bytes)
        {
            return Err(ConfigurationError::InvalidCommandScriptOutput {
                name: name.to_string(),
                bytes: self.output_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSectionConfiguration {
    pub section: PanelSection,
    pub visible: bool,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StartupConfiguration {
    #[serde(default)]
    pub autostart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostLevel {
    #[default]
    None,
    Low,
    Moderate,
}

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
    pub const fn new(idle: CostLevel, interaction: CostLevel, polling: CostLevel) -> Self {
        Self {
            idle,
            interaction,
            polling,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FeatureConfiguration {
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
    #[serde(default)]
    pub audio: AudioConfiguration,
    #[serde(default)]
    pub speed_test: SpeedTestConfiguration,
    #[serde(default)]
    pub capture: CaptureConfiguration,
    #[serde(default)]
    pub shortcuts: ShortcutConfiguration,
    #[serde(default)]
    pub clipboard: ClipboardConfiguration,
    #[serde(default)]
    pub snippets: SnippetConfiguration,
    #[serde(default)]
    pub command_bar: CommandBarConfiguration,
}

impl Default for ApplicationConfiguration {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_CONFIGURATION_SCHEMA_VERSION,
            features: BTreeMap::new(),
            ui: UiConfiguration::default(),
            startup: StartupConfiguration::default(),
            monitoring: MonitorConfiguration::default(),
            audio: AudioConfiguration::default(),
            speed_test: SpeedTestConfiguration::default(),
            capture: CaptureConfiguration::default(),
            shortcuts: ShortcutConfiguration::default(),
            clipboard: ClipboardConfiguration::default(),
            snippets: SnippetConfiguration::default(),
            command_bar: CommandBarConfiguration::default(),
        }
    }
}

impl ApplicationConfiguration {
    pub fn feature_enabled(&self, feature_id: &str) -> bool {
        self.features
            .get(feature_id)
            .is_some_and(|configuration| configuration.enabled)
    }

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

    /// Captures every entry with separate absent and disabled states.
    pub fn snapshot_features(&self) -> FeatureConfigurationSnapshot {
        FeatureConfigurationSnapshot {
            features: self.features.clone(),
        }
    }

    pub fn restore_feature_snapshot(&mut self, snapshot: FeatureConfigurationSnapshot) {
        self.features = snapshot.features;
    }

    pub fn feature_configuration_snapshot(&self) -> FeatureConfigurationSnapshot {
        self.snapshot_features()
    }

    pub fn restore_features(&mut self, snapshot: &FeatureConfigurationSnapshot) {
        self.features = snapshot.features.clone();
    }

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
        self.audio.validate()?;
        self.speed_test.validate()?;
        self.capture.validate()?;
        self.shortcuts.validate()?;
        self.clipboard.validate()?;
        self.snippets.validate()?;
        self.command_bar.validate()?;

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

// Manual Eq implementation accompanies validation that rejects non-finite values.
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

/// How a master output switch treats streams that are already playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioOutputSwitch {
    #[default]
    DefaultOutput,
    AllStreams,
}

impl AudioOutputSwitch {
    pub const ALL: [AudioOutputSwitch; 2] = [
        AudioOutputSwitch::DefaultOutput,
        AudioOutputSwitch::AllStreams,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::DefaultOutput => "Default output only",
            Self::AllStreams => "Move all streams",
        }
    }
}

/// What Kestrel does to a stream whose output device disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioDisconnectPolicy {
    #[default]
    PreserveVolume,
    ResetVolume,
}

impl AudioDisconnectPolicy {
    pub const ALL: [AudioDisconnectPolicy; 2] = [
        AudioDisconnectPolicy::PreserveVolume,
        AudioDisconnectPolicy::ResetVolume,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::PreserveVolume => "Keep the stream volume",
            Self::ResetVolume => "Reset to the configured volume",
        }
    }
}

/// User intent for the PulseAudio-compatible mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfiguration {
    #[serde(default = "default_audio_boost_percent")]
    pub boost_percent: u8,
    #[serde(default)]
    pub output_switch: AudioOutputSwitch,
    #[serde(default)]
    pub disconnect_policy: AudioDisconnectPolicy,
    #[serde(default = "default_audio_disconnect_volume_percent")]
    pub disconnect_volume_percent: u8,
    #[serde(default)]
    pub include_inactive_streams: bool,
}

fn default_audio_boost_percent() -> u8 {
    DEFAULT_AUDIO_BOOST_PERCENT
}

fn default_audio_disconnect_volume_percent() -> u8 {
    DEFAULT_AUDIO_DISCONNECT_VOLUME_PERCENT
}

impl Default for AudioConfiguration {
    fn default() -> Self {
        Self {
            boost_percent: DEFAULT_AUDIO_BOOST_PERCENT,
            output_switch: AudioOutputSwitch::default(),
            disconnect_policy: AudioDisconnectPolicy::default(),
            disconnect_volume_percent: DEFAULT_AUDIO_DISCONNECT_VOLUME_PERCENT,
            include_inactive_streams: false,
        }
    }
}

impl AudioConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(UNAMPLIFIED_AUDIO_VOLUME_PERCENT..=MAX_AUDIO_BOOST_PERCENT)
            .contains(&self.boost_percent)
        {
            return Err(ConfigurationError::InvalidAudioBoostPercent {
                percent: self.boost_percent,
            });
        }
        if self.disconnect_volume_percent > UNAMPLIFIED_AUDIO_VOLUME_PERCENT {
            return Err(ConfigurationError::InvalidAudioDisconnectVolumePercent {
                percent: self.disconnect_volume_percent,
            });
        }
        Ok(())
    }
}

/// User intent for the on-demand, bounded network speed test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedTestConfiguration {
    #[serde(default = "default_speed_test_download_megabytes")]
    pub download_megabytes: u32,
    #[serde(default = "default_speed_test_upload_megabytes")]
    pub upload_megabytes: u32,
    #[serde(default = "default_speed_test_timeout_seconds")]
    pub timeout_seconds: u32,
}

fn default_speed_test_download_megabytes() -> u32 {
    DEFAULT_SPEED_TEST_DOWNLOAD_MEGABYTES
}

fn default_speed_test_upload_megabytes() -> u32 {
    DEFAULT_SPEED_TEST_UPLOAD_MEGABYTES
}

fn default_speed_test_timeout_seconds() -> u32 {
    DEFAULT_SPEED_TEST_TIMEOUT_SECONDS
}

impl Default for SpeedTestConfiguration {
    fn default() -> Self {
        Self {
            download_megabytes: DEFAULT_SPEED_TEST_DOWNLOAD_MEGABYTES,
            upload_megabytes: DEFAULT_SPEED_TEST_UPLOAD_MEGABYTES,
            timeout_seconds: DEFAULT_SPEED_TEST_TIMEOUT_SECONDS,
        }
    }
}

impl SpeedTestConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(MIN_SPEED_TEST_DOWNLOAD_MEGABYTES..=MAX_SPEED_TEST_DOWNLOAD_MEGABYTES)
            .contains(&self.download_megabytes)
        {
            return Err(ConfigurationError::InvalidSpeedTestDownloadMegabytes {
                megabytes: self.download_megabytes,
            });
        }
        if self.upload_megabytes > MAX_SPEED_TEST_UPLOAD_MEGABYTES {
            return Err(ConfigurationError::InvalidSpeedTestUploadMegabytes {
                megabytes: self.upload_megabytes,
            });
        }
        if !(MIN_SPEED_TEST_TIMEOUT_SECONDS..=MAX_SPEED_TEST_TIMEOUT_SECONDS)
            .contains(&self.timeout_seconds)
        {
            return Err(ConfigurationError::InvalidSpeedTestTimeoutSeconds {
                seconds: self.timeout_seconds,
            });
        }
        Ok(())
    }
}

/// Bounds for the recent-capture history; every limit is at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureConfiguration {
    #[serde(default = "default_capture_max_entries")]
    pub max_entries: u32,
    #[serde(default = "default_capture_max_total_megabytes")]
    pub max_total_megabytes: u32,
    #[serde(default = "default_capture_max_age_hours")]
    pub max_age_hours: u32,
}

fn default_capture_max_entries() -> u32 {
    DEFAULT_CAPTURE_MAX_ENTRIES
}

fn default_capture_max_total_megabytes() -> u32 {
    DEFAULT_CAPTURE_MAX_TOTAL_MEGABYTES
}

fn default_capture_max_age_hours() -> u32 {
    DEFAULT_CAPTURE_MAX_AGE_HOURS
}

impl Default for CaptureConfiguration {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_CAPTURE_MAX_ENTRIES,
            max_total_megabytes: DEFAULT_CAPTURE_MAX_TOTAL_MEGABYTES,
            max_age_hours: DEFAULT_CAPTURE_MAX_AGE_HOURS,
        }
    }
}

impl CaptureConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(1..=MAX_CAPTURE_MAX_ENTRIES).contains(&self.max_entries) {
            return Err(ConfigurationError::InvalidCaptureMaxEntries {
                entries: self.max_entries,
            });
        }
        if !(1..=MAX_CAPTURE_MAX_TOTAL_MEGABYTES).contains(&self.max_total_megabytes) {
            return Err(ConfigurationError::InvalidCaptureTotalMegabytes {
                megabytes: self.max_total_megabytes,
            });
        }
        if !(1..=MAX_CAPTURE_MAX_AGE_HOURS).contains(&self.max_age_hours) {
            return Err(ConfigurationError::InvalidCaptureAgeHours {
                hours: self.max_age_hours,
            });
        }
        Ok(())
    }

    pub const fn max_total_bytes(&self) -> u64 {
        self.max_total_megabytes as u64 * CAPTURE_BYTES_PER_MEGABYTE
    }
}

/// Modifier keys of a global shortcut trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortcutModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

/// A key combination in the freedesktop shortcuts format, e.g. `LOGO+ALT+m`.
///
/// At least one modifier is required, so a global grab never swallows a plain key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutTrigger {
    pub modifiers: ShortcutModifiers,
    /// An XKB keysym name such as `m`, `F12`, or `space`.
    pub key: String,
}

impl ShortcutTrigger {
    pub fn parse(text: &str) -> Result<Self, ConfigurationError> {
        let invalid = || ConfigurationError::InvalidShortcutTrigger {
            trigger: text.to_owned(),
        };
        let mut parts = text.split('+').map(str::trim).collect::<Vec<_>>();
        let key = parts
            .pop()
            .filter(|key| !key.is_empty())
            .ok_or_else(invalid)?;
        if !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(invalid());
        }
        let mut modifiers = ShortcutModifiers::default();
        for part in parts {
            let flag = match part.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => &mut modifiers.ctrl,
                "ALT" => &mut modifiers.alt,
                "SHIFT" => &mut modifiers.shift,
                "LOGO" | "SUPER" => &mut modifiers.logo,
                _ => return Err(invalid()),
            };
            if *flag {
                return Err(invalid());
            }
            *flag = true;
        }
        if modifiers == ShortcutModifiers::default() {
            return Err(invalid());
        }
        Ok(Self {
            modifiers,
            key: key.to_owned(),
        })
    }
}

impl fmt::Display for ShortcutTrigger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (enabled, name) in [
            (self.modifiers.ctrl, "CTRL"),
            (self.modifiers.alt, "ALT"),
            (self.modifiers.shift, "SHIFT"),
            (self.modifiers.logo, "LOGO"),
        ] {
            if enabled {
                write!(formatter, "{name}+")?;
            }
        }
        formatter.write_str(&self.key)
    }
}

/// One global shortcut: a stable command ID and its preferred trigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutBinding {
    pub command: String,
    pub trigger: String,
}

impl ShortcutBinding {
    fn new(command: &str, trigger: &str) -> Self {
        Self {
            command: command.to_owned(),
            trigger: trigger.to_owned(),
        }
    }

    /// Validates the command ID shape and the trigger; the command's meaning is
    /// checked by the application, which owns the command list.
    pub fn validate(&self) -> Result<ShortcutTrigger, ConfigurationError> {
        let shaped = !self.command.is_empty()
            && self.command.len() <= MAX_SHORTCUT_COMMAND_CHARS
            && self.command.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '-' | '_')
            });
        if !shaped {
            return Err(ConfigurationError::InvalidShortcutCommand {
                command: self.command.clone(),
            });
        }
        ShortcutTrigger::parse(&self.trigger)
    }
}

/// Global-shortcut bindings registered while `global.shortcuts` runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutConfiguration {
    #[serde(default = "default_shortcut_bindings")]
    pub bindings: Vec<ShortcutBinding>,
}

fn default_shortcut_bindings() -> Vec<ShortcutBinding> {
    vec![
        ShortcutBinding::new("window.show", "LOGO+ALT+k"),
        ShortcutBinding::new("microphone.toggle-mute", "LOGO+ALT+m"),
        ShortcutBinding::new("audio.output-next", "LOGO+ALT+o"),
    ]
}

impl Default for ShortcutConfiguration {
    fn default() -> Self {
        Self {
            bindings: default_shortcut_bindings(),
        }
    }
}

impl ShortcutConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if self.bindings.len() > MAX_SHORTCUT_BINDINGS {
            return Err(ConfigurationError::TooManyShortcutBindings {
                bindings: self.bindings.len(),
            });
        }
        let mut commands = BTreeSet::new();
        let mut triggers = BTreeSet::new();
        for binding in &self.bindings {
            let trigger = binding.validate()?;
            if !commands.insert(binding.command.as_str()) {
                return Err(ConfigurationError::DuplicateShortcutCommand {
                    command: binding.command.clone(),
                });
            }
            if !triggers.insert(trigger.to_string().to_ascii_lowercase()) {
                return Err(ConfigurationError::DuplicateShortcutTrigger {
                    trigger: binding.trigger.clone(),
                });
            }
        }
        Ok(())
    }
}

/// User intent for the bounded, memory-only clipboard history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardConfiguration {
    #[serde(default = "default_clipboard_max_items")]
    pub max_items: u32,
    #[serde(default = "default_clipboard_item_bytes")]
    pub max_item_bytes: u32,
    #[serde(default = "default_clipboard_image_bytes")]
    pub max_image_bytes: u32,
    #[serde(default = "default_clipboard_file_entries")]
    pub max_file_entries: u32,
    #[serde(default = "default_clipboard_total_bytes")]
    pub max_total_bytes: u32,
    #[serde(default = "default_clipboard_age_hours")]
    pub max_age_hours: u32,
    /// Seconds until the live selection is cleared; `0` disables. Saved entries are kept.
    #[serde(default)]
    pub clear_seconds: u64,
    #[serde(default)]
    pub filter_sensitive: bool,
    #[serde(default = "default_true")]
    pub paste_plain_text: bool,
}

fn default_clipboard_max_items() -> u32 {
    DEFAULT_CLIPBOARD_MAX_ITEMS
}

fn default_clipboard_item_bytes() -> u32 {
    DEFAULT_CLIPBOARD_ITEM_BYTES
}

fn default_clipboard_image_bytes() -> u32 {
    DEFAULT_CLIPBOARD_IMAGE_BYTES
}

fn default_clipboard_file_entries() -> u32 {
    DEFAULT_CLIPBOARD_FILE_ENTRIES
}

fn default_clipboard_total_bytes() -> u32 {
    DEFAULT_CLIPBOARD_TOTAL_BYTES
}

fn default_clipboard_age_hours() -> u32 {
    DEFAULT_CLIPBOARD_AGE_HOURS
}

fn default_true() -> bool {
    true
}

impl Default for ClipboardConfiguration {
    fn default() -> Self {
        Self {
            max_items: DEFAULT_CLIPBOARD_MAX_ITEMS,
            max_item_bytes: DEFAULT_CLIPBOARD_ITEM_BYTES,
            max_image_bytes: DEFAULT_CLIPBOARD_IMAGE_BYTES,
            max_file_entries: DEFAULT_CLIPBOARD_FILE_ENTRIES,
            max_total_bytes: DEFAULT_CLIPBOARD_TOTAL_BYTES,
            max_age_hours: DEFAULT_CLIPBOARD_AGE_HOURS,
            clear_seconds: 0,
            filter_sensitive: false,
            paste_plain_text: true,
        }
    }
}

impl ClipboardConfiguration {
    /// Validates every retention bound and the automatic clear interval.
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(1..=MAX_CLIPBOARD_MAX_ITEMS).contains(&self.max_items) {
            return Err(ConfigurationError::InvalidClipboardMaxItems {
                items: self.max_items,
            });
        }
        if !(MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_ITEM_BYTES).contains(&self.max_item_bytes) {
            return Err(ConfigurationError::InvalidClipboardItemBytes {
                bytes: self.max_item_bytes,
            });
        }
        if !(MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_IMAGE_BYTES).contains(&self.max_image_bytes) {
            return Err(ConfigurationError::InvalidClipboardImageBytes {
                bytes: self.max_image_bytes,
            });
        }
        if !(1..=MAX_CLIPBOARD_FILE_ENTRIES).contains(&self.max_file_entries) {
            return Err(ConfigurationError::InvalidClipboardFileEntries {
                entries: self.max_file_entries,
            });
        }
        if !(MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_TOTAL_BYTES).contains(&self.max_total_bytes) {
            return Err(ConfigurationError::InvalidClipboardTotalBytes {
                bytes: self.max_total_bytes,
            });
        }
        if self.max_item_bytes > self.max_total_bytes || self.max_image_bytes > self.max_total_bytes
        {
            return Err(ConfigurationError::InvalidClipboardTotalBytes {
                bytes: self.max_total_bytes,
            });
        }
        if !(1..=MAX_CLIPBOARD_AGE_HOURS).contains(&self.max_age_hours) {
            return Err(ConfigurationError::InvalidClipboardAgeHours {
                hours: self.max_age_hours,
            });
        }
        if self.clear_seconds > MAX_CLIPBOARD_CLEAR_SECONDS {
            return Err(ConfigurationError::InvalidClipboardClearSeconds {
                seconds: self.clear_seconds,
            });
        }
        Ok(())
    }
}

/// A variable a snippet may reference, rendered locally at insert time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SnippetVariable {
    Date,
    Time,
    DateTime,
    Timezone,
    UtcOffset,
    Clipboard,
}

impl SnippetVariable {
    pub const ALL: [SnippetVariable; 6] = [
        SnippetVariable::Date,
        SnippetVariable::Time,
        SnippetVariable::DateTime,
        SnippetVariable::Timezone,
        SnippetVariable::UtcOffset,
        SnippetVariable::Clipboard,
    ];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Date => "{{date}}",
            Self::Time => "{{time}}",
            Self::DateTime => "{{datetime}}",
            Self::Timezone => "{{timezone}}",
            Self::UtcOffset => "{{utc_offset}}",
            Self::Clipboard => "{{clipboard}}",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Date => "Local date",
            Self::Time => "Local time",
            Self::DateTime => "Local date and time",
            Self::Timezone => "Local time zone",
            Self::UtcOffset => "Local UTC offset",
            Self::Clipboard => "Clipboard text",
        }
    }

    pub const fn reads_clipboard(self) -> bool {
        matches!(self, Self::Clipboard)
    }

    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|variable| variable.token() == token)
    }
}

/// A portable snippet definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    pub content: String,
}

impl Snippet {
    pub fn new(
        name: impl Into<String>,
        folder: Option<String>,
        trigger: Option<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            folder,
            trigger,
            content: content.into(),
        }
    }

    pub fn variables(&self) -> Vec<SnippetVariable> {
        let mut found = Vec::new();
        for token in variable_tokens(&self.content) {
            if let Some(variable) = SnippetVariable::parse(token) {
                if !found.contains(&variable) {
                    found.push(variable);
                }
            }
        }
        found
    }

    pub fn validate(&self, max_content_bytes: u32) -> Result<(), SnippetError> {
        let name = self.name.trim();
        if name.chars().count() < MIN_SNIPPET_NAME_CHARS {
            return Err(SnippetError::EmptyName);
        }
        if name.chars().count() > MAX_SNIPPET_NAME_CHARS {
            return Err(SnippetError::NameTooLong {
                name: self.name.clone(),
            });
        }
        if let Some(folder) = self.folder.as_deref().map(str::trim) {
            if folder.is_empty() {
                return Err(SnippetError::EmptyFolder);
            }
            if folder.chars().count() > MAX_SNIPPET_FOLDER_CHARS {
                return Err(SnippetError::FolderTooLong {
                    folder: folder.to_owned(),
                });
            }
        }
        if let Some(trigger) = self.trigger.as_deref().map(str::trim) {
            if trigger.is_empty() {
                return Err(SnippetError::EmptyTrigger);
            }
            if trigger.chars().count() > MAX_SNIPPET_TRIGGER_CHARS {
                return Err(SnippetError::TriggerTooLong {
                    trigger: trigger.to_owned(),
                });
            }
            if trigger.chars().any(char::is_whitespace) {
                return Err(SnippetError::TriggerContainsWhitespace {
                    trigger: trigger.to_owned(),
                });
            }
        }
        if self.content.len() > max_content_bytes as usize {
            return Err(SnippetError::ContentTooLong {
                bytes: self.content.len(),
                maximum: max_content_bytes,
            });
        }
        let mut variables = 0usize;
        for token in variable_tokens(&self.content) {
            match SnippetVariable::parse(token) {
                Some(variable) => {
                    variables += 1;
                    if variables > MAX_SNIPPET_VARIABLES {
                        return Err(SnippetError::TooManyVariables {
                            maximum: MAX_SNIPPET_VARIABLES,
                        });
                    }
                    let _ = variable;
                }
                None => {
                    return Err(SnippetError::UnknownVariable {
                        token: token.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Collects `{{...}}` tokens from snippet content.
pub fn variable_tokens(content: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start..];
        let Some(end) = after.find("}}") else {
            break;
        };
        tokens.push(&after[..end + 2]);
        rest = &after[end + 2..];
    }
    tokens
}

/// A rejected snippet definition or library operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetError {
    EmptyName,
    NameTooLong { name: String },
    DuplicateName { name: String },
    EmptyFolder,
    FolderTooLong { folder: String },
    EmptyTrigger,
    TriggerTooLong { trigger: String },
    TriggerContainsWhitespace { trigger: String },
    DuplicateTrigger { trigger: String },
    TriggerNotDelimited { trigger: String },
    ContentTooLong { bytes: usize, maximum: u32 },
    TooManyVariables { maximum: usize },
    UnknownVariable { token: String },
    UnknownSnippet { name: String },
}

impl std::fmt::Display for SnippetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("a snippet needs a name"),
            Self::NameTooLong { name } => write!(
                formatter,
                "snippet name \"{name}\" is longer than {MAX_SNIPPET_NAME_CHARS} characters"
            ),
            Self::DuplicateName { name } => {
                write!(formatter, "a snippet named \"{name}\" already exists")
            }
            Self::EmptyFolder => formatter.write_str("a snippet folder needs a name"),
            Self::FolderTooLong { folder } => write!(
                formatter,
                "folder \"{folder}\" is longer than {MAX_SNIPPET_FOLDER_CHARS} characters"
            ),
            Self::EmptyTrigger => formatter.write_str("a snippet trigger cannot be empty"),
            Self::TriggerTooLong { trigger } => write!(
                formatter,
                "trigger \"{trigger}\" is longer than {MAX_SNIPPET_TRIGGER_CHARS} characters"
            ),
            Self::TriggerContainsWhitespace { trigger } => write!(
                formatter,
                "trigger \"{trigger}\" must not contain whitespace"
            ),
            Self::DuplicateTrigger { trigger } => write!(
                formatter,
                "trigger \"{trigger}\" is already used by another snippet"
            ),
            Self::TriggerNotDelimited { trigger } => write!(
                formatter,
                "trigger \"{trigger}\" must start with a non-alphanumeric delimiter"
            ),
            Self::ContentTooLong { bytes, maximum } => write!(
                formatter,
                "snippet content is {bytes} bytes, above the {maximum} byte bound"
            ),
            Self::TooManyVariables { maximum } => {
                write!(
                    formatter,
                    "a snippet may reference at most {maximum} variables"
                )
            }
            Self::UnknownVariable { token } => {
                write!(formatter, "\"{token}\" is not a supported variable")
            }
            Self::UnknownSnippet { name } => {
                write!(formatter, "no snippet named \"{name}\" is stored")
            }
        }
    }
}

impl std::error::Error for SnippetError {}

/// User intent for the snippet library and its insert path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetConfiguration {
    #[serde(default = "default_snippet_content_bytes")]
    pub max_content_bytes: u32,
    #[serde(default = "default_snippet_clipboard_bytes")]
    pub clipboard_variable_bytes: u32,
    #[serde(default = "default_snippet_insert_timeout_millis")]
    pub insert_timeout_millis: u64,
    #[serde(default)]
    pub preferred_provider: SnippetProviderPreference,
    #[serde(default)]
    pub expansion_timing: SnippetExpansionTiming,
}

/// Trigger expansion needs a key-capture provider; manual insertion remains available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnippetExpansionTiming {
    /// Insert only when the user asks.
    #[default]
    Manual,
    /// Expand when a trigger is followed by a delimiter in the focused window.
    Delimiter,
}

impl SnippetExpansionTiming {
    pub const ALL: [SnippetExpansionTiming; 2] = [
        SnippetExpansionTiming::Manual,
        SnippetExpansionTiming::Delimiter,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Manual => "Manual insertion only",
            Self::Delimiter => "Expand on a delimiter (needs key capture)",
        }
    }
}

/// `Auto` selects input-device providers through an explicit preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnippetProviderPreference {
    #[default]
    Auto,
    Wtype,
    Ydotool,
    Xdotool,
}

impl SnippetProviderPreference {
    pub const ALL: [SnippetProviderPreference; 4] = [
        SnippetProviderPreference::Auto,
        SnippetProviderPreference::Wtype,
        SnippetProviderPreference::Ydotool,
        SnippetProviderPreference::Xdotool,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (no input-group access)",
            Self::Wtype => "wtype (Wayland virtual keyboard)",
            Self::Ydotool => "ydotool (uinput; requires input-device access)",
            Self::Xdotool => "xdotool (X11)",
        }
    }

    pub const fn executable(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Wtype => Some("wtype"),
            Self::Ydotool => Some("ydotool"),
            Self::Xdotool => Some("xdotool"),
        }
    }
}

fn default_snippet_content_bytes() -> u32 {
    DEFAULT_SNIPPET_CONTENT_BYTES
}

fn default_snippet_clipboard_bytes() -> u32 {
    DEFAULT_SNIPPET_CLIPBOARD_BYTES
}

fn default_snippet_insert_timeout_millis() -> u64 {
    DEFAULT_SNIPPET_INSERT_TIMEOUT_MILLIS
}

impl Default for SnippetConfiguration {
    fn default() -> Self {
        Self {
            max_content_bytes: DEFAULT_SNIPPET_CONTENT_BYTES,
            clipboard_variable_bytes: DEFAULT_SNIPPET_CLIPBOARD_BYTES,
            insert_timeout_millis: DEFAULT_SNIPPET_INSERT_TIMEOUT_MILLIS,
            preferred_provider: SnippetProviderPreference::default(),
            expansion_timing: SnippetExpansionTiming::default(),
        }
    }
}

impl SnippetConfiguration {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(MIN_SNIPPET_CONTENT_BYTES..=MAX_SNIPPET_CONTENT_BYTES)
            .contains(&self.max_content_bytes)
        {
            return Err(ConfigurationError::InvalidSnippetContentBytes {
                bytes: self.max_content_bytes,
            });
        }
        if !(MIN_SNIPPET_CLIPBOARD_BYTES..=MAX_SNIPPET_CLIPBOARD_BYTES)
            .contains(&self.clipboard_variable_bytes)
        {
            return Err(ConfigurationError::InvalidSnippetClipboardBytes {
                bytes: self.clipboard_variable_bytes,
            });
        }
        if !(MIN_SNIPPET_INSERT_TIMEOUT_MILLIS..=MAX_SNIPPET_INSERT_TIMEOUT_MILLIS)
            .contains(&self.insert_timeout_millis)
        {
            return Err(ConfigurationError::InvalidSnippetInsertTimeout {
                millis: self.insert_timeout_millis,
            });
        }
        Ok(())
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
    InvalidAudioBoostPercent { percent: u8 },
    InvalidAudioDisconnectVolumePercent { percent: u8 },
    InvalidSpeedTestDownloadMegabytes { megabytes: u32 },
    InvalidSpeedTestUploadMegabytes { megabytes: u32 },
    InvalidSpeedTestTimeoutSeconds { seconds: u32 },
    InvalidCaptureMaxEntries { entries: u32 },
    InvalidCaptureTotalMegabytes { megabytes: u32 },
    InvalidCaptureAgeHours { hours: u32 },
    TooManyShortcutBindings { bindings: usize },
    InvalidShortcutCommand { command: String },
    InvalidShortcutTrigger { trigger: String },
    DuplicateShortcutCommand { command: String },
    DuplicateShortcutTrigger { trigger: String },
    InvalidClipboardMaxItems { items: u32 },
    InvalidClipboardItemBytes { bytes: u32 },
    InvalidClipboardImageBytes { bytes: u32 },
    InvalidClipboardFileEntries { entries: u32 },
    InvalidClipboardTotalBytes { bytes: u32 },
    InvalidClipboardAgeHours { hours: u32 },
    InvalidClipboardClearSeconds { seconds: u64 },
    InvalidSnippetContentBytes { bytes: u32 },
    InvalidSnippetClipboardBytes { bytes: u32 },
    InvalidSnippetInsertTimeout { millis: u64 },
    InvalidCommandResults { results: u32 },
    MissingCommandFileRoot,
    TooManyCommandFileRoots { roots: usize },
    InvalidCommandFileRoot { root: String },
    DuplicateCommandFileRoot { root: String },
    TooManyCommandScripts { scripts: usize },
    InvalidCommandScriptName { name: String },
    DuplicateCommandScriptName { name: String },
    InvalidCommandScriptExecutable { executable: String },
    InvalidCommandScriptArgs { name: String },
    InvalidCommandScriptTimeout { name: String, millis: u64 },
    InvalidCommandScriptOutput { name: String, bytes: u32 },
}

// Manual Eq implementation keeps error matching ergonomic; validation rejects
// non-finite thresholds.
impl Eq for ConfigurationError {}

/// Validates a stable namespaced identifier independently of UI and platform.
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

    pub fn with_selected_backend(mut self, backend: impl Into<String>) -> Self {
        self.selected_backend = Some(backend.into());
        self
    }

    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }

    pub fn with_alternative(mut self, alternative: impl Into<String>) -> Self {
        self.alternatives_considered.push(alternative.into());
        self
    }

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
    pub fn new(id: &'static str, label: &'static str, capability: CapabilityStatus) -> Self {
        Self {
            id,
            label,
            capability,
            configurable: true,
            cost: ResourceCost::default(),
        }
    }

    pub fn with_configurable(mut self, configurable: bool) -> Self {
        self.configurable = configurable;
        self
    }

    pub fn with_cost(mut self, cost: ResourceCost) -> Self {
        self.cost = cost;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlertKind, AlertRuleConfiguration, AppearancePreference, ApplicationConfiguration,
        AudioDisconnectPolicy, AudioOutputSwitch, CURRENT_CONFIGURATION_SCHEMA_VERSION,
        CapabilityEvidence, CapabilityReport, CapabilityStatus, ClipboardConfiguration,
        ConfigurationError, CostLevel, DEFAULT_AUDIO_BOOST_PERCENT,
        DEFAULT_AUDIO_DISCONNECT_VOLUME_PERCENT, DEFAULT_CLIPBOARD_AGE_HOURS,
        DEFAULT_CLIPBOARD_FILE_ENTRIES, DEFAULT_CLIPBOARD_IMAGE_BYTES,
        DEFAULT_CLIPBOARD_ITEM_BYTES, DEFAULT_CLIPBOARD_MAX_ITEMS, DEFAULT_CLIPBOARD_TOTAL_BYTES,
        FeatureSpec, MAX_ALERT_COOLDOWN_SECONDS, MAX_ALERT_SUSTAIN_SAMPLES,
        MAX_ALERT_THRESHOLD_PERCENT, MAX_AUDIO_BOOST_PERCENT, MAX_CLIPBOARD_AGE_HOURS,
        MAX_CLIPBOARD_CLEAR_SECONDS, MAX_CLIPBOARD_FILE_ENTRIES, MAX_CLIPBOARD_IMAGE_BYTES,
        MAX_CLIPBOARD_MAX_ITEMS, MAX_MONITOR_HISTORY_SAMPLES, MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
        MAX_SNIPPET_CONTENT_BYTES, MAX_SNIPPET_INSERT_TIMEOUT_MILLIS, MAX_SNIPPET_NAME_CHARS,
        MAX_SNIPPET_VARIABLES, MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS, MIN_ALERT_SUSTAIN_SAMPLES,
        MIN_CLIPBOARD_ITEM_BYTES, MIN_MONITOR_REFRESH_INTERVAL_MILLIS, MIN_SNIPPET_CLIPBOARD_BYTES,
        MonitorReadout, PanelSection, PanelSectionConfiguration, ResourceCost, Snippet,
        SnippetConfiguration, SnippetError, SnippetExpansionTiming, SnippetProviderPreference,
        SnippetVariable, UNAMPLIFIED_AUDIO_VOLUME_PERCENT, UiConfiguration, variable_tokens,
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
    fn audio_boost_ceiling_is_bounded_by_the_hard_cap() {
        for percent in [UNAMPLIFIED_AUDIO_VOLUME_PERCENT, MAX_AUDIO_BOOST_PERCENT] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.audio.boost_percent = percent;
            assert_eq!(configuration.validate(), Ok(()));
        }

        for percent in [
            UNAMPLIFIED_AUDIO_VOLUME_PERCENT - 1,
            MAX_AUDIO_BOOST_PERCENT + 1,
        ] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.audio.boost_percent = percent;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidAudioBoostPercent { percent })
            );
        }
    }

    #[test]
    fn audio_disconnect_volume_stays_within_the_unamplified_range() {
        for percent in [0, UNAMPLIFIED_AUDIO_VOLUME_PERCENT] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.audio.disconnect_volume_percent = percent;
            assert_eq!(configuration.validate(), Ok(()));
        }

        let mut configuration = ApplicationConfiguration::default();
        configuration.audio.disconnect_volume_percent = UNAMPLIFIED_AUDIO_VOLUME_PERCENT + 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidAudioDisconnectVolumePercent { percent: 101 })
        );
    }

    #[test]
    fn audio_defaults_keep_boost_bounded_and_disconnect_policy_conservative() {
        let audio = ApplicationConfiguration::default().audio;

        assert_eq!(audio.boost_percent, DEFAULT_AUDIO_BOOST_PERCENT);
        assert!(audio.boost_percent < MAX_AUDIO_BOOST_PERCENT);
        assert!(audio.boost_percent > UNAMPLIFIED_AUDIO_VOLUME_PERCENT);
        assert_eq!(audio.output_switch, AudioOutputSwitch::DefaultOutput);
        assert_eq!(
            audio.disconnect_policy,
            AudioDisconnectPolicy::PreserveVolume
        );
        assert_eq!(
            audio.disconnect_volume_percent,
            DEFAULT_AUDIO_DISCONNECT_VOLUME_PERCENT
        );
        assert!(!audio.include_inactive_streams);
        assert_eq!(audio.validate(), Ok(()));
    }

    #[test]
    fn clipboard_bounds_are_validated_independently() {
        assert_eq!(ClipboardConfiguration::default().validate(), Ok(()));

        for items in [0, MAX_CLIPBOARD_MAX_ITEMS + 1] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.clipboard.max_items = items;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidClipboardMaxItems { items })
            );
        }

        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_item_bytes = MIN_CLIPBOARD_ITEM_BYTES - 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidClipboardItemBytes {
                bytes: MIN_CLIPBOARD_ITEM_BYTES - 1
            })
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_image_bytes = MAX_CLIPBOARD_IMAGE_BYTES + 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidClipboardImageBytes {
                bytes: MAX_CLIPBOARD_IMAGE_BYTES + 1
            })
        );

        for entries in [0, MAX_CLIPBOARD_FILE_ENTRIES + 1] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.clipboard.max_file_entries = entries;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidClipboardFileEntries { entries })
            );
        }

        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_total_bytes = MIN_CLIPBOARD_ITEM_BYTES;
        configuration.clipboard.max_item_bytes = MIN_CLIPBOARD_ITEM_BYTES * 2;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidClipboardTotalBytes {
                bytes: MIN_CLIPBOARD_ITEM_BYTES
            })
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_age_hours = MAX_CLIPBOARD_AGE_HOURS + 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidClipboardAgeHours {
                hours: MAX_CLIPBOARD_AGE_HOURS + 1
            })
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.clear_seconds = MAX_CLIPBOARD_CLEAR_SECONDS + 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidClipboardClearSeconds {
                seconds: MAX_CLIPBOARD_CLEAR_SECONDS + 1
            })
        );
    }

    #[test]
    fn clipboard_defaults_stay_bounded_memory_only_and_opt_in() {
        let clipboard = ApplicationConfiguration::default().clipboard;

        assert_eq!(clipboard.max_items, DEFAULT_CLIPBOARD_MAX_ITEMS);
        assert_eq!(clipboard.max_item_bytes, DEFAULT_CLIPBOARD_ITEM_BYTES);
        assert_eq!(clipboard.max_image_bytes, DEFAULT_CLIPBOARD_IMAGE_BYTES);
        assert_eq!(clipboard.max_file_entries, DEFAULT_CLIPBOARD_FILE_ENTRIES);
        assert_eq!(clipboard.max_total_bytes, DEFAULT_CLIPBOARD_TOTAL_BYTES);
        assert_eq!(clipboard.max_age_hours, DEFAULT_CLIPBOARD_AGE_HOURS);
        assert_eq!(
            clipboard.clear_seconds, 0,
            "the automatic selection clear is opt-in"
        );
        assert!(
            !clipboard.filter_sensitive,
            "sensitive filtering is opt-in because it has documented false positives"
        );
        assert!(clipboard.paste_plain_text);
        assert_eq!(clipboard.validate(), Ok(()));
    }

    #[test]
    fn snippet_validation_covers_names_folders_triggers_content_and_variables() {
        let bounds = super::DEFAULT_SNIPPET_CONTENT_BYTES;
        let valid = Snippet::new(
            "Address",
            Some("Contact".to_string()),
            Some(";addr".to_string()),
            "123 Example Street\n{{date}}",
        );
        assert_eq!(valid.validate(bounds), Ok(()));
        assert_eq!(valid.variables(), vec![SnippetVariable::Date]);

        assert_eq!(
            Snippet::new("   ", None, None, "x").validate(bounds),
            Err(SnippetError::EmptyName)
        );
        assert_eq!(
            Snippet::new("x".repeat(MAX_SNIPPET_NAME_CHARS + 1), None, None, "x").validate(bounds),
            Err(SnippetError::NameTooLong {
                name: "x".repeat(MAX_SNIPPET_NAME_CHARS + 1)
            })
        );
        assert_eq!(
            Snippet::new("a", Some("   ".to_string()), None, "x").validate(bounds),
            Err(SnippetError::EmptyFolder)
        );
        assert_eq!(
            Snippet::new("a", None, Some(";a b".to_string()), "x").validate(bounds),
            Err(SnippetError::TriggerContainsWhitespace {
                trigger: ";a b".to_string()
            })
        );
        assert_eq!(
            Snippet::new("a", None, Some(String::new()), "x").validate(bounds),
            Err(SnippetError::EmptyTrigger)
        );
        assert_eq!(
            Snippet::new("a", None, None, "x".repeat(20)).validate(8),
            Err(SnippetError::ContentTooLong {
                bytes: 20,
                maximum: 8
            })
        );
        assert_eq!(
            Snippet::new("a", None, None, "{{today}}").validate(bounds),
            Err(SnippetError::UnknownVariable {
                token: "{{today}}".to_string()
            })
        );
        assert_eq!(
            Snippet::new(
                "a",
                None,
                None,
                "{{date}} ".repeat(MAX_SNIPPET_VARIABLES + 1)
            )
            .validate(MAX_SNIPPET_CONTENT_BYTES),
            Err(SnippetError::TooManyVariables {
                maximum: MAX_SNIPPET_VARIABLES
            })
        );
    }

    #[test]
    fn snippet_variables_are_documented_tokens_with_clipboard_marked() {
        assert_eq!(SnippetVariable::ALL.len(), 6);
        assert_eq!(SnippetVariable::Date.token(), "{{date}}");
        assert_eq!(
            SnippetVariable::parse("{{clipboard}}"),
            Some(SnippetVariable::Clipboard)
        );
        assert_eq!(SnippetVariable::parse("{{nope}}"), None);
        assert!(SnippetVariable::Clipboard.reads_clipboard());
        assert!(!SnippetVariable::Date.reads_clipboard());
        let labels = SnippetVariable::ALL
            .iter()
            .map(|variable| (variable.token(), variable.label()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            labels.len(),
            SnippetVariable::ALL.len(),
            "every variable has a unique token and a label"
        );

        assert!(variable_tokens("half {{date").is_empty());
        assert_eq!(
            variable_tokens("{{date}} and {{time}}"),
            vec!["{{date}}", "{{time}}"]
        );
    }

    #[test]
    fn snippet_configuration_bounds_are_enforced() {
        assert_eq!(SnippetConfiguration::default().validate(), Ok(()));

        for bytes in [0, MAX_SNIPPET_CONTENT_BYTES + 1] {
            let mut configuration = ApplicationConfiguration::default();
            configuration.snippets.max_content_bytes = bytes;
            assert_eq!(
                configuration.validate(),
                Err(ConfigurationError::InvalidSnippetContentBytes { bytes })
            );
        }

        let mut configuration = ApplicationConfiguration::default();
        configuration.snippets.clipboard_variable_bytes = MIN_SNIPPET_CLIPBOARD_BYTES - 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidSnippetClipboardBytes {
                bytes: MIN_SNIPPET_CLIPBOARD_BYTES - 1
            })
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration.snippets.insert_timeout_millis = MAX_SNIPPET_INSERT_TIMEOUT_MILLIS + 1;
        assert_eq!(
            configuration.validate(),
            Err(ConfigurationError::InvalidSnippetInsertTimeout {
                millis: MAX_SNIPPET_INSERT_TIMEOUT_MILLIS + 1
            })
        );

        assert_eq!(
            ApplicationConfiguration::default()
                .snippets
                .max_content_bytes,
            super::DEFAULT_SNIPPET_CONTENT_BYTES
        );
        assert_eq!(
            SnippetConfiguration::default().preferred_provider,
            SnippetProviderPreference::Auto,
            "automatic selection never opts into input-group access"
        );
        assert_eq!(SnippetProviderPreference::ALL.len(), 4);
        assert_eq!(SnippetProviderPreference::Auto.executable(), None);
        assert_eq!(
            SnippetProviderPreference::Ydotool.executable(),
            Some("ydotool")
        );
        assert_eq!(
            SnippetConfiguration::default().expansion_timing,
            SnippetExpansionTiming::Manual,
            "trigger expansion is opt-in because it needs key capture"
        );
        assert_eq!(SnippetExpansionTiming::ALL.len(), 2);
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

    #[test]
    fn shortcut_triggers_parse_canonically_and_require_a_modifier() {
        let trigger = super::ShortcutTrigger::parse("logo + Ctrl+F12").expect("valid trigger");
        assert!(trigger.modifiers.logo && trigger.modifiers.ctrl);
        assert!(!trigger.modifiers.alt && !trigger.modifiers.shift);
        assert_eq!(
            trigger.to_string(),
            "CTRL+LOGO+F12",
            "modifiers print in one order"
        );
        assert_eq!(
            super::ShortcutTrigger::parse(&trigger.to_string()),
            Ok(trigger)
        );

        for invalid in [
            "m",
            "CTRL+",
            "CTRL+CTRL+m",
            "HYPER+m",
            "CTRL+m+n",
            "CTRL+m!",
            "",
        ] {
            assert_eq!(
                super::ShortcutTrigger::parse(invalid),
                Err(ConfigurationError::InvalidShortcutTrigger {
                    trigger: invalid.to_owned()
                }),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn shortcut_configuration_rejects_bad_ids_duplicates_and_excess() {
        let binding = |command: &str, trigger: &str| super::ShortcutBinding {
            command: command.to_owned(),
            trigger: trigger.to_owned(),
        };
        assert_eq!(super::ShortcutConfiguration::default().validate(), Ok(()));

        let config = |bindings| super::ShortcutConfiguration { bindings };
        assert!(matches!(
            config(vec![binding("Window.Show", "CTRL+k")]).validate(),
            Err(ConfigurationError::InvalidShortcutCommand { .. })
        ));
        assert!(matches!(
            config(vec![
                binding("window.show", "CTRL+k"),
                binding("window.show", "CTRL+j")
            ])
            .validate(),
            Err(ConfigurationError::DuplicateShortcutCommand { .. })
        ));
        assert!(matches!(
            config(vec![
                binding("window.show", "CTRL+ALT+k"),
                binding("app.quit", "alt+ctrl+K")
            ])
            .validate(),
            Err(ConfigurationError::DuplicateShortcutTrigger { .. }),
        ));
        let many = (0..=super::MAX_SHORTCUT_BINDINGS)
            .map(|index| binding(&format!("command.{index}"), &format!("CTRL+F{}", index + 1)))
            .collect();
        assert!(matches!(
            config(many).validate(),
            Err(ConfigurationError::TooManyShortcutBindings { .. })
        ));
    }
}
