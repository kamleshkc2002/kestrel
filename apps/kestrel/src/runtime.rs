use crate::{
    ApplicationViewModel, ConfigurationWarning,
    snippets::{save_snippets, snippet_path},
    status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, unavailable_capability},
    view_model::{
        AudioPresentation, ClipboardPresentation, ClipboardQuery, ClipboardViewModel,
        CommandBarPresentation, CommandBarQuery, MicrophonePresentation, MicrophoneViewModel,
        MonitorPresentation, MonitorViewModel, SnippetQuery, SnippetsPresentation,
        SpeedTestPresentation, SpeedTestViewModel,
    },
};
use kestrel_core::{
    AlertKind, ApplicationConfiguration, CapabilityEvidence, CapabilityReport, CapabilityStatus,
    CostLevel, FeatureConfigurationSnapshot, FeatureSpec, ResourceCost,
};
use kestrel_platform::{
    CapabilityProbe, StaticCapabilityProbe,
    applications::{
        ApplicationEntry, ExternalLauncher, application_directories, launch_desktop_entry,
        scan_applications,
    },
    audio::{
        FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID, MicrophoneProbe, PulseAudioBackend,
    },
    clipboard::{
        ArboardClipboardBackend, ClipboardBackend, ClipboardCapabilityProbe,
        FEATURE_ID as CLIPBOARD_HISTORY_ID, LogindPrivacyMonitor, discover_provider,
    },
    launcher::{ScriptOutcome, run_script, search_roots},
    quick_toggles::{
        ALL_QUICK_TOGGLES, LinuxQuickToggleBackend, QuickToggleCapabilityProbe, QuickToggleControl,
        QuickToggleError, QuickToggleId,
    },
    snippets::{
        Clock, ExecutableInsertionBackend, InsertionProvider, SnippetInsertionProbe, SystemClock,
        discover_insertion_provider,
    },
    speed_test::{CurlSpeedTestBackend, FEATURE_ID as SPEED_TEST_ID, SpeedTestProbe},
    system_monitor::{FEATURE_ID as SYSTEM_MONITOR_ID, ProcSysMonitor},
};
use kestrel_services::{
    FeatureRegistry, RegistryError, ServiceRegistration,
    alerts::{AlertEngine, AlertEvent, AlertPolicy, AlertSnapshot},
    audio::{AudioCommand, AudioCommandResult, AudioMixerService, AudioPolicy, AudioSnapshot},
    clipboard::{
        ClipboardCommand, ClipboardHistoryService, ClipboardMatch, ClipboardPolicy,
        ClipboardPreview, ClipboardServiceError, ClipboardSnapshot,
    },
    command_bar::{
        CommandIndex, CommandItem, CommandRanking, CommandResult, CommandSource, EnabledProviders,
        SearchInput,
    },
    microphone::{
        MicrophoneCommand, MicrophoneCommandResult, MicrophoneService, MicrophoneSnapshot,
    },
    quick_toggles::{QuickToggleCommand, QuickToggleService, QuickToggleSnapshot},
    snippets::{
        InsertionReport, RenderContext, SnippetInsertionService, SnippetLibrary, SnippetMatch,
        SnippetPolicy, SnippetServiceError,
    },
    speed_test::{SpeedTestPolicy, SpeedTestService, SpeedTestSnapshot},
    system_monitor::{HistorySummary, RefreshOutcome, SystemMonitorService, SystemSnapshot},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeaturePreset {
    Essentials,
    Balanced,
    Everything,
}

impl FeaturePreset {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Essentials => "Essentials",
            Self::Balanced => "Balanced",
            Self::Everything => "Everything",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Essentials => "Core monitoring, audio, power, and session controls.",
            Self::Balanced => {
                "All supported configurable features except clipboard history and global shortcuts."
            }
            Self::Everything => {
                "Every configurable feature, including resource-intensive services."
            }
        }
    }
}

const COMMAND_SURFACE_ID: &str = "app.command-surface";
const GLOBAL_SHORTCUTS_ID: &str = "global.shortcuts";

const fn cost(idle: CostLevel, interaction: CostLevel, polling: CostLevel) -> ResourceCost {
    ResourceCost::new(idle, interaction, polling)
}

const COMMAND_SURFACE_COST: ResourceCost = cost(CostLevel::None, CostLevel::Low, CostLevel::None);
const TRAY_COST: ResourceCost = cost(CostLevel::Low, CostLevel::Moderate, CostLevel::None);
const fn quick_toggle_cost(id: QuickToggleId) -> ResourceCost {
    match id {
        QuickToggleId::KeepAwake => cost(CostLevel::Low, CostLevel::Low, CostLevel::None),
        QuickToggleId::BatteryAlerts => cost(CostLevel::Low, CostLevel::Low, CostLevel::Low),
        QuickToggleId::Appearance
        | QuickToggleId::Brightness
        | QuickToggleId::KeyboardLight
        | QuickToggleId::Bluetooth
        | QuickToggleId::Wifi
        | QuickToggleId::EmptyTrash
        | QuickToggleId::Eject
        | QuickToggleId::HiddenFiles
        | QuickToggleId::DesktopIcons
        | QuickToggleId::ScreenLock => cost(CostLevel::None, CostLevel::Moderate, CostLevel::None),
    }
}

use std::time::Duration;

/// Stable feature identifier for the snippet library.
pub const SNIPPETS_ID: &str = "snippets.text";
/// The stable feature identifier for the command bar.
pub const COMMAND_BAR_ID: &str = "commands.bar";

/// Reports available providers without hiding portable commands.
pub struct CommandBarProbe {
    configuration: kestrel_core::CommandBarConfiguration,
}

impl CommandBarProbe {
    pub fn new(configuration: kestrel_core::CommandBarConfiguration) -> Self {
        Self { configuration }
    }
}

impl CapabilityProbe for CommandBarProbe {
    fn probe(&self) -> CapabilityReport {
        let applications = scan_applications(&application_directories(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("XDG_DATA_DIRS").as_deref(),
            std::env::var_os("HOME").as_deref(),
        ));
        let launcher = ExternalLauncher::discover();
        let roots = self.configuration.file_roots.len();
        let scripts = self.configuration.scripts.len();
        // GIO launches terminal entries; report their count.
        let terminal_entries = applications
            .iter()
            .filter(|application| application.terminal)
            .count();

        let mut report = CapabilityReport::new(
            COMMAND_BAR_ID,
            CapabilityStatus::Supported,
            format!(
                "Portable commands plus {} applications, {} scripts, and {} file roots.",
                if self.configuration.enable_applications {
                    applications.len().to_string()
                } else {
                    "0 (disabled)".to_string()
                },
                if self.configuration.enable_scripts {
                    scripts.to_string()
                } else {
                    "0 (disabled)".to_string()
                },
                if self.configuration.enable_files {
                    roots.to_string()
                } else {
                    "0 (disabled)".to_string()
                }
            ),
        )
        .with_selected_backend("In-process command index")
        .with_evidence(CapabilityEvidence::new("portable_commands", "always"))
        .with_evidence(CapabilityEvidence::new(
            "application_count",
            applications.len().to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "terminal_applications",
            terminal_entries.to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "launch_handler",
            launcher
                .as_ref()
                .map(|launcher| launcher.handler_name().to_string())
                .unwrap_or_else(|_| "unavailable".to_string()),
        ))
        .with_evidence(CapabilityEvidence::new("file_roots", roots.to_string()))
        .with_evidence(CapabilityEvidence::new("scripts", scripts.to_string()));

        if launcher.is_err() {
            report = report.with_remediation(
                "Install xdg-open to open links and files from the command bar. Applications \
                 launch through the desktop's own launcher, and commands that only use \
                 Kestrel data keep working without it.",
            );
        }
        if self.configuration.enable_files && roots == 0 {
            report = report.with_remediation(
                "File search needs at least one configured root; Kestrel never indexes the whole \
                 filesystem.",
            );
        }
        report
    }
}

pub struct ApplicationRuntime {
    registry: FeatureRegistry,
    audio_mixer: AudioMixerService<PulseAudioBackend>,
    microphone: MicrophoneService<PulseAudioBackend>,
    speed_test: SpeedTestService<CurlSpeedTestBackend>,
    clipboard_history: ClipboardHistoryService,
    snippet_library: SnippetLibrary,
    snippet_service: SnippetInsertionService<ExecutableInsertionBackend>,
    snippet_policy: SnippetPolicy,
    snippet_expansion_timing: kestrel_core::SnippetExpansionTiming,
    snippet_provider_preference: kestrel_core::SnippetProviderPreference,
    snippet_path: Option<std::path::PathBuf>,
    snippet_warnings: Vec<ConfigurationWarning>,
    /// Read-only clipboard access for snippet expansion and command copying.
    session_clipboard: Option<ArboardClipboardBackend>,
    command_index: CommandIndex,
    command_ranking: CommandRanking,
    command_ranking_path: Option<std::path::PathBuf>,
    command_ranking_warnings: Vec<ConfigurationWarning>,
    command_applications: Vec<ApplicationEntry>,
    command_application_items: Vec<CommandItem>,
    command_providers: EnabledProviders,
    command_file_roots: Vec<std::path::PathBuf>,
    command_scripts: Vec<kestrel_core::CommandScriptConfiguration>,
    command_max_results: usize,
    command_launcher: Option<ExternalLauncher>,
    system_monitor: SystemMonitorService<ProcSysMonitor>,
    alerts: AlertEngine,
    /// Battery alerts can only be narrowed by the quick toggle.
    battery_alert_configured: bool,
    quick_toggles: QuickToggleService,
}
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorTick {
    pub outcome: RefreshOutcome,
    pub alerts: Vec<AlertEvent>,
}

impl ApplicationRuntime {
    pub fn new(configuration: &ApplicationConfiguration) -> Result<Self, RegistryError> {
        Self::new_with_status_notifier(
            configuration,
            unavailable_capability("StatusNotifierItem registration was not attempted."),
        )
    }

    /// Builds the runtime with optional tray-registration results.
    pub fn new_with_status_notifier(
        configuration: &ApplicationConfiguration,
        status_notifier_capability: CapabilityReport,
    ) -> Result<Self, RegistryError> {
        let mut registry = FeatureRegistry::default();

        let command_surface = FeatureSpec::new(
            COMMAND_SURFACE_ID,
            "Command surface",
            CapabilityStatus::Supported,
        )
        .with_configurable(false)
        .with_cost(COMMAND_SURFACE_COST);
        registry.register_probe(
            command_surface.clone(),
            true,
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    command_surface.id,
                    CapabilityStatus::Supported,
                    "The normal Kestrel window is always available as the command surface.",
                )
                .with_selected_backend("GTK/libadwaita normal window"),
            ),
        )?;
        let status_notifier_available = matches!(
            &status_notifier_capability.status,
            CapabilityStatus::Supported | CapabilityStatus::Limited { .. }
        );
        let status_notifier = FeatureSpec::new(
            STATUS_NOTIFIER_ID,
            "Tray integration",
            status_notifier_capability.status.clone(),
        )
        .with_configurable(false)
        .with_cost(TRAY_COST);
        registry.register_probe(
            status_notifier,
            status_notifier_available,
            StaticCapabilityProbe::new(status_notifier_capability),
        )?;

        let system_monitor = FeatureSpec::new(
            SYSTEM_MONITOR_ID,
            "System monitor",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::Low, CostLevel::Low, CostLevel::Moderate));
        let monitor_source = ProcSysMonitor::default();
        let audio_backend = PulseAudioBackend::new();
        registry.register_probe(
            system_monitor,
            configuration.feature_enabled(SYSTEM_MONITOR_ID),
            monitor_source.clone(),
        )?;
        let audio_mixer =
            FeatureSpec::new(AUDIO_MIXER_ID, "Audio mixer", CapabilityStatus::Supported)
                .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            audio_mixer,
            configuration.feature_enabled(AUDIO_MIXER_ID),
            audio_backend,
        )?;

        let microphone = FeatureSpec::new(
            MICROPHONE_FEATURE_ID,
            "Microphone",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::None, CostLevel::Low, CostLevel::Low));
        registry.register_probe(
            microphone,
            configuration.feature_enabled(MICROPHONE_FEATURE_ID),
            MicrophoneProbe::new(),
        )?;
        let speed_test = FeatureSpec::new(
            SPEED_TEST_ID,
            "Network speed test",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            speed_test,
            configuration.feature_enabled(SPEED_TEST_ID),
            SpeedTestProbe,
        )?;
        let clipboard_history = FeatureSpec::new(
            CLIPBOARD_HISTORY_ID,
            "Clipboard history",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(
            CostLevel::Moderate,
            CostLevel::Moderate,
            CostLevel::Moderate,
        ));
        registry.register_probe(
            clipboard_history,
            configuration.feature_enabled(CLIPBOARD_HISTORY_ID),
            ClipboardCapabilityProbe::new(),
        )?;
        let command_bar =
            FeatureSpec::new(COMMAND_BAR_ID, "Command bar", CapabilityStatus::Supported)
                .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            command_bar,
            configuration.feature_enabled(COMMAND_BAR_ID),
            CommandBarProbe::new(configuration.command_bar.clone()),
        )?;
        let snippets = FeatureSpec::new(SNIPPETS_ID, "Text snippets", CapabilityStatus::Supported)
            .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            snippets,
            configuration.feature_enabled(SNIPPETS_ID),
            SnippetInsertionProbe::new(configuration.snippets.preferred_provider),
        )?;
        let global_shortcuts = FeatureSpec::new(
            GLOBAL_SHORTCUTS_ID,
            "Global shortcuts",
            CapabilityStatus::Unsupported {
                reason: "No portable global-shortcut adapter is registered yet.".to_string(),
            },
        )
        .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            global_shortcuts.clone(),
            configuration.feature_enabled(global_shortcuts.id),
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    global_shortcuts.id,
                    CapabilityStatus::Unsupported {
                        reason: "No portable global-shortcut adapter is registered yet.".to_string(),
                    },
                    "Global shortcuts are unavailable, but Kestrel remains usable from its normal window.",
                )
                .with_remediation(
                    "Use the normal window or configure a desktop shortcut that launches Kestrel.",
                ),
            ),
        )?;
        let quick_toggle_backend = LinuxQuickToggleBackend::new();
        for id in ALL_QUICK_TOGGLES {
            let enabled = configuration
                .features
                .get(id.feature_id())
                .map(|feature| feature.enabled)
                .unwrap_or(true);
            registry.register_probe(
                FeatureSpec::new(id.feature_id(), id.label(), CapabilityStatus::Supported)
                    .with_cost(quick_toggle_cost(id)),
                enabled,
                QuickToggleCapabilityProbe::new(quick_toggle_backend.clone(), id),
            )?;
        }
        let snippet_policy = SnippetPolicy::from_configuration(&configuration.snippets);
        let snippet_path = snippet_path();
        let loaded_snippets = snippet_path
            .as_deref()
            .map(|path| crate::snippets::load_snippets(path, snippet_policy))
            .unwrap_or_default();
        let mut snippet_warnings = loaded_snippets.warnings;
        if snippet_path.is_none() {
            snippet_warnings.push(ConfigurationWarning {
                feature_id: "snippets".to_owned(),
                reason: "No writable data directory was resolved, so snippets cannot be stored. \
                         Set XDG_DATA_HOME or HOME."
                    .to_owned(),
            });
        }
        let snippet_service = Self::build_snippet_service(configuration);
        let command_applications = Self::scan_command_applications(configuration);
        let command_application_items = Self::application_items(&command_applications);
        let command_index = CommandIndex::new(
            Self::snippet_items(&loaded_snippets.library),
            &configuration.command_bar.scripts,
        );
        let command_ranking_path = crate::command_bar::ranking_path();
        let loaded_ranking = command_ranking_path
            .as_deref()
            .map(crate::command_bar::load_ranking)
            .unwrap_or_default();
        let command_providers = Self::providers_from_configuration(configuration);
        let command_file_roots = configuration
            .command_bar
            .file_roots
            .iter()
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>();
        let command_max_results = configuration.command_bar.max_results as usize;
        let command_launcher = ExternalLauncher::discover().ok();
        let command_ranking_warnings = loaded_ranking.warnings;

        let mut runtime = Self {
            registry,
            audio_mixer: AudioMixerService::with_policy(
                audio_backend,
                AudioPolicy::from_configuration(&configuration.audio),
            )
            .expect("the validated audio policy is valid"),
            microphone: MicrophoneService::new(PulseAudioBackend::new()),
            speed_test: SpeedTestService::new(
                CurlSpeedTestBackend::discover(),
                SpeedTestPolicy::from_configuration(&configuration.speed_test),
            ),
            clipboard_history: ClipboardHistoryService::new(ClipboardPolicy::from_configuration(
                &configuration.clipboard,
            ))
            .expect("the validated clipboard policy is valid"),
            snippet_library: loaded_snippets.library,
            snippet_service,
            snippet_policy,
            snippet_expansion_timing: configuration.snippets.expansion_timing,
            snippet_provider_preference: configuration.snippets.preferred_provider,
            snippet_path,
            snippet_warnings,
            session_clipboard: Self::build_snippet_clipboard(),
            command_index,
            command_ranking: loaded_ranking.ranking,
            command_ranking_path,
            command_ranking_warnings,
            command_applications,
            command_application_items,
            command_providers,
            command_file_roots,
            command_scripts: configuration.command_bar.scripts.clone(),
            command_max_results,
            command_launcher,
            system_monitor: SystemMonitorService::new(
                monitor_source,
                Duration::from_millis(configuration.monitoring.refresh_interval_millis),
                configuration.monitoring.history_samples,
            )
            .expect("the validated monitor configuration is valid"),
            alerts: AlertEngine::new(AlertPolicy::from_configuration(
                &configuration.monitoring.alerts,
            )),
            battery_alert_configured: configuration.monitoring.alerts.battery.enabled,
            quick_toggles: QuickToggleService::new(quick_toggle_backend),
        };
        runtime.sync_battery_alert_rule();
        Ok(runtime)
    }

    pub fn start(&mut self) {
        self.registry.start_enabled();
        self.reconcile_resources();
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(Duration::ZERO);
        }
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        if self.microphone_is_running() {
            let _ = self.microphone.refresh();
        }
        self.refresh_quick_toggles();
    }

    pub fn refresh_capabilities(&mut self) -> Result<(), RegistryError> {
        let feature_ids = self
            .registry
            .registrations()
            .map(|registration| registration.feature.id)
            .collect::<Vec<_>>();
        for feature_id in feature_ids {
            self.registry.refresh_capability(feature_id)?;
        }
        self.registry.start_enabled();
        self.reconcile_resources();
        // Re-probe and resample after device loss.
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        if self.microphone_is_running() {
            let _ = self.microphone.refresh();
        }
        self.refresh_quick_toggles();
        Ok(())
    }

    pub fn apply_configuration(
        &mut self,
        configuration: &ApplicationConfiguration,
    ) -> Result<(), RegistryError> {
        self.system_monitor
            .reconfigure(
                Duration::from_millis(configuration.monitoring.refresh_interval_millis),
                configuration.monitoring.history_samples,
            )
            .expect("the validated monitor configuration is valid");
        self.alerts.set_policy(AlertPolicy::from_configuration(
            &configuration.monitoring.alerts,
        ));
        self.battery_alert_configured = configuration.monitoring.alerts.battery.enabled;
        self.audio_mixer
            .set_policy(AudioPolicy::from_configuration(&configuration.audio))
            .expect("the validated audio policy is valid");
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        self.speed_test
            .set_policy(SpeedTestPolicy::from_configuration(
                &configuration.speed_test,
            ));
        if self.microphone_is_running() {
            let _ = self.microphone.refresh();
        }
        if self.clipboard_history_is_running() {
            let _ = self
                .clipboard_history
                .set_policy(ClipboardPolicy::from_configuration(
                    &configuration.clipboard,
                ));
        }
        // Re-derive snippet policy after configuration changes.
        self.snippet_policy = SnippetPolicy::from_configuration(&configuration.snippets);
        self.snippet_expansion_timing = configuration.snippets.expansion_timing;
        self.snippet_provider_preference = configuration.snippets.preferred_provider;
        self.snippet_service = Self::build_snippet_service(configuration);
        self.command_scripts = configuration.command_bar.scripts.clone();
        self.command_providers = Self::providers_from_configuration(configuration);
        self.command_file_roots = configuration
            .command_bar
            .file_roots
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        self.command_max_results = configuration.command_bar.max_results as usize;
        self.command_applications = Self::scan_command_applications(configuration);
        self.rebuild_command_index();
        let desired = self
            .registry
            .registrations()
            .filter(|registration| registration.feature.configurable)
            .map(|registration| {
                (
                    registration.feature.id,
                    configuration
                        .features
                        .get(registration.feature.id)
                        .map(|feature| feature.enabled)
                        .unwrap_or_else(|| Self::default_enabled(registration.feature.id)),
                )
            })
            .collect::<Vec<_>>();
        for (feature_id, enabled) in desired {
            self.registry.set_enabled(feature_id, enabled)?;
        }
        self.registry.start_enabled();
        self.reconcile_resources();
        self.refresh_quick_toggles();
        self.sync_battery_alert_rule();
        Ok(())
    }

    /// Applies a built-in policy and returns the exact prior feature map.
    pub fn apply_preset(
        &mut self,
        configuration: &mut ApplicationConfiguration,
        preset: FeaturePreset,
    ) -> Result<FeatureConfigurationSnapshot, RegistryError> {
        let snapshot = configuration.snapshot_features();
        let configurable = self
            .registry
            .registrations()
            .filter(|registration| registration.feature.configurable)
            .map(|registration| registration.feature.id)
            .collect::<Vec<_>>();
        for feature_id in configurable {
            configuration
                .features
                .entry(feature_id.to_owned())
                .or_default()
                .enabled = Self::preset_enabled(preset, feature_id);
        }
        if let Err(error) = self.apply_configuration(configuration) {
            configuration.restore_features(&snapshot);
            let _ = self.apply_configuration(configuration);
            return Err(error);
        }
        Ok(snapshot)
    }

    /// Restores a consumed preset snapshot and reapplies its enablement.
    pub fn undo_preset(
        &mut self,
        configuration: &mut ApplicationConfiguration,
        snapshot: FeatureConfigurationSnapshot,
    ) -> Result<(), RegistryError> {
        configuration.restore_feature_snapshot(snapshot);
        self.apply_configuration(configuration)
    }
    /// Returns registration snapshots for the active session.
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }

    /// Extracts owned presentation state.
    pub fn view_model(
        &self,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
    ) -> ApplicationViewModel {
        self.view_model_with_panels(
            warnings,
            configuration,
            can_undo,
            ClipboardQuery::default(),
            SnippetQuery::default(),
            CommandBarQuery::default(),
        )
    }

    /// Builds presentation state with panel queries.
    pub fn view_model_with_panels(
        &self,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
        clipboard: ClipboardQuery<'_>,
        snippets: SnippetQuery<'_>,
        command_bar: CommandBarQuery<'_>,
    ) -> ApplicationViewModel {
        ApplicationViewModel::new(
            self.registrations(),
            self.quick_toggles.snapshots(),
            AudioPresentation {
                snapshot: Some(self.audio_mixer.latest()),
                running: self.audio_mixer_is_running(),
                policy: self.audio_mixer.policy(),
            },
            MicrophonePresentation {
                snapshot: self.microphone.latest(),
                running: self.microphone_is_running(),
            },
            SpeedTestPresentation {
                snapshot: self.speed_test.snapshot(),
                running: self.speed_test_is_running(),
            },
            ClipboardPresentation {
                snapshot: Some(self.clipboard_history.latest()),
                running: self.clipboard_history_is_running(),
                search_query: clipboard.query,
                matches: clipboard.matches,
                preview: clipboard.preview,
            },
            SnippetsPresentation {
                library: &self.snippet_library,
                policy: self.snippet_policy,
                running: self.snippets_running(),
                provider: self.snippet_provider(),
                unavailable_reason: self.snippet_unavailable_reason(),
                directory: self.snippet_directory(),
                expansion_timing: self.snippet_expansion_timing,
                provider_preference: self.snippet_provider_preference,
                search_query: snippets.query,
                matches: snippets.matches,
                draft: snippets.draft,
                warnings: &self.snippet_warnings,
            },
            CommandBarPresentation {
                running: self.command_bar_running(),
                max_results: self.command_max_results as u32,
                providers: self.command_providers,
                launcher: self.command_launcher_name(),
                applications: self.command_applications.len(),
                file_roots: self.command_file_roots.len(),
                scripts: self.command_scripts.len(),
                query: command_bar.query,
                results: command_bar.results,
                ranking: &self.command_ranking,
                warnings: &self.command_ranking_warnings,
            },
            MonitorPresentation {
                snapshot: self.system_monitor.latest(),
                history: self.system_monitor.history_summary(),
                alerts: self.alerts.snapshot(),
                running: self.system_monitor_is_running(),
                policy: Some(self.alerts.policy()),
            },
            warnings,
            configuration,
            can_undo,
        )
    }

    pub fn sample_monitor(&mut self, observed_at: Duration) -> MonitorTick {
        if !self.system_monitor_is_running() {
            return MonitorTick {
                outcome: RefreshOutcome::Skipped,
                alerts: Vec::new(),
            };
        }
        let outcome = self.system_monitor.refresh(observed_at);
        let alerts = match self.system_monitor.latest() {
            Some(snapshot) if outcome == RefreshOutcome::Updated => self.alerts.evaluate(snapshot),
            _ => Vec::new(),
        };
        MonitorTick { outcome, alerts }
    }

    pub fn alert_snapshot(&self) -> AlertSnapshot {
        self.alerts.snapshot()
    }

    pub fn record_alert_delivery(&mut self, kind: AlertKind, result: Result<(), String>) {
        match result {
            Ok(()) => self.alerts.clear_delivery_failure(kind),
            Err(message) => self.alerts.record_delivery_failure(kind, message),
        }
    }

    pub fn history_summary(&self) -> Option<HistorySummary> {
        self.system_monitor.history_summary()
    }

    pub fn monitor_view_model(&self, configuration: &ApplicationConfiguration) -> MonitorViewModel {
        MonitorViewModel::from_configuration(
            &configuration.monitoring,
            MonitorPresentation {
                snapshot: self.system_monitor.latest(),
                history: self.system_monitor.history_summary(),
                alerts: self.alerts.snapshot(),
                running: self.system_monitor_is_running(),
                policy: Some(self.alerts.policy()),
            },
        )
    }

    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }

    /// Returns audio state, including unavailable/empty states.
    pub fn audio_snapshot(&self) -> &AudioSnapshot {
        self.audio_mixer.latest()
    }
    pub fn clipboard_is_running(&self) -> bool {
        self.clipboard_history_is_running()
    }

    /// Builds clipboard presentation state for targeted updates.
    pub fn clipboard_view_model(
        &self,
        configuration: &ApplicationConfiguration,
        query: &str,
        matches: &[ClipboardMatch],
        preview: Option<&ClipboardPreview>,
    ) -> ClipboardViewModel {
        ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(self.clipboard_history.latest()),
                running: self.clipboard_history_is_running(),
                search_query: query,
                matches,
                preview,
            },
        )
    }

    pub fn microphone_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == MICROPHONE_FEATURE_ID && registration.running
        })
    }

    /// Returns the latest reading; mute is `Unknown` before sampling.
    pub fn microphone_snapshot(&self) -> &MicrophoneSnapshot {
        self.microphone.latest()
    }

    pub fn execute_microphone_command(
        &mut self,
        command: MicrophoneCommand,
    ) -> Option<MicrophoneCommandResult> {
        self.microphone_is_running()
            .then(|| self.microphone.execute(command))
    }

    /// Refreshes external mute state and reports whether it changed.
    pub fn refresh_microphone(&mut self) -> bool {
        if !self.microphone_is_running() {
            return false;
        }
        let before = self.microphone.latest().clone();
        let _ = self.microphone.refresh();
        self.microphone.latest() != &before
    }

    pub fn microphone_view_model(&self) -> MicrophoneViewModel {
        MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: self.microphone.latest(),
            running: self.microphone_is_running(),
        })
    }

    /// A running registration only makes a test startable.
    pub fn speed_test_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SPEED_TEST_ID && registration.running)
    }

    pub fn start_speed_test(&mut self) -> Result<(), String> {
        self.require_speed_test()?;
        self.speed_test.start().map_err(|error| error.to_string())
    }

    pub fn cancel_speed_test(&mut self) -> Result<(), String> {
        self.require_speed_test()?;
        self.speed_test.cancel().map_err(|error| error.to_string())
    }

    fn require_speed_test(&self) -> Result<(), String> {
        if self.speed_test_is_running() {
            Ok(())
        } else {
            Err(
                "The network speed test is not running; enable network.speed_test in the \
                 Feature Hub"
                    .to_owned(),
            )
        }
    }

    pub fn speed_test_snapshot(&self) -> SpeedTestSnapshot {
        self.speed_test.snapshot()
    }

    pub fn speed_test_view_model(&self) -> SpeedTestViewModel {
        SpeedTestViewModel::from_presentation(SpeedTestPresentation {
            snapshot: self.speed_test.snapshot(),
            running: self.speed_test_is_running(),
        })
    }

    pub fn execute_audio_command(&mut self, command: AudioCommand) -> Option<AudioCommandResult> {
        self.audio_mixer_is_running()
            .then(|| self.audio_mixer.execute(command))
    }

    fn build_snippet_service(
        configuration: &ApplicationConfiguration,
    ) -> SnippetInsertionService<ExecutableInsertionBackend> {
        let discovery = discover_insertion_provider(configuration.snippets.preferred_provider)
            .and_then(|path| {
                ExecutableInsertionBackend::new(path, configuration.snippets.insert_timeout_millis)
            });
        SnippetInsertionService::new(discovery)
    }

    /// Separate read-only access avoids disturbing history ownership.
    fn build_snippet_clipboard() -> Option<ArboardClipboardBackend> {
        discover_provider()
            .ok()
            .and_then(|provider| ArboardClipboardBackend::new(provider).ok())
    }

    fn scan_command_applications(
        configuration: &ApplicationConfiguration,
    ) -> Vec<ApplicationEntry> {
        if !configuration.command_bar.enable_applications {
            return Vec::new();
        }
        scan_applications(&application_directories(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("XDG_DATA_DIRS").as_deref(),
            std::env::var_os("HOME").as_deref(),
        ))
    }

    fn application_items(applications: &[ApplicationEntry]) -> Vec<CommandItem> {
        applications
            .iter()
            .enumerate()
            .map(|(index, application)| {
                CommandItem::new(
                    format!("application:{}", application.id),
                    CommandSource::Application,
                    application.name.clone(),
                    application
                        .comment
                        .clone()
                        .unwrap_or_else(|| "Application".to_string()),
                    kestrel_services::command_bar::CommandAction::OpenApplication { index },
                )
            })
            .collect()
    }

    fn snippet_items(library: &SnippetLibrary) -> Vec<CommandItem> {
        library
            .snippets()
            .iter()
            .map(|snippet| {
                CommandItem::new(
                    format!("snippet:{}", snippet.name),
                    CommandSource::Snippet,
                    snippet.name.clone(),
                    snippet
                        .folder
                        .clone()
                        .unwrap_or_else(|| "Snippet".to_string()),
                    kestrel_services::command_bar::CommandAction::Kestrel(format!(
                        "insert_snippet:{}",
                        snippet.name
                    )),
                )
                .with_keywords(["snippet", "text"])
            })
            .collect()
    }

    /// Enables file search only with a configured root.
    fn providers_from_configuration(configuration: &ApplicationConfiguration) -> EnabledProviders {
        EnabledProviders {
            applications: configuration.command_bar.enable_applications,
            files: configuration.command_bar.enable_files
                && !configuration.command_bar.file_roots.is_empty(),
            scripts: configuration.command_bar.enable_scripts,
            emoji: configuration.command_bar.enable_emoji,
            // Snippets follow their own feature lifecycle.
            snippets: true,
        }
    }

    fn rebuild_command_index(&mut self) {
        self.command_index = CommandIndex::new(
            Self::snippet_items(&self.snippet_library),
            &self.command_scripts,
        );
        self.command_application_items = Self::application_items(&self.command_applications);
    }

    pub fn command_ranking(&self) -> &CommandRanking {
        &self.command_ranking
    }

    pub fn command_ranking_warnings(&self) -> &[ConfigurationWarning] {
        &self.command_ranking_warnings
    }

    pub fn command_applications(&self) -> &[ApplicationEntry] {
        &self.command_applications
    }

    pub fn command_providers(&self) -> EnabledProviders {
        self.command_providers
    }

    pub fn command_launcher_name(&self) -> Option<&str> {
        self.command_launcher
            .as_ref()
            .map(ExternalLauncher::handler_name)
    }

    pub fn command_scripts(&self) -> &[kestrel_core::CommandScriptConfiguration] {
        &self.command_scripts
    }

    pub fn command_bar_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == COMMAND_BAR_ID && registration.running)
    }

    /// Gates effects so stale requests cannot act while stopped.
    fn require_command_bar(&self) -> Result<(), String> {
        if self.command_bar_running() {
            Ok(())
        } else {
            Err("the command bar is not running; enable commands.bar in the Feature Hub".to_owned())
        }
    }

    /// Returns no results when stopped; snippets follow their own lifecycle.
    pub fn command_search<'a>(
        &'a self,
        query: &str,
        now: &'a kestrel_platform::snippets::LocalTime,
    ) -> Vec<CommandResult> {
        if !self.command_bar_running() {
            return Vec::new();
        }
        let providers = EnabledProviders {
            snippets: self.snippets_running(),
            ..self.command_providers
        };
        // File results are gathered only when the provider is on and roots are
        // configured; the walk itself is bounded by depth and entry budget.
        let files = if self.command_providers.files {
            search_roots(
                &self.command_file_roots,
                query,
                kestrel_core::MAX_COMMAND_FILE_DEPTH,
                kestrel_core::MAX_COMMAND_FILE_ENTRIES,
                8,
            )
        } else {
            Vec::new()
        };
        self.command_index.search(SearchInput {
            query,
            max_results: self.command_max_results,
            providers,
            applications: &self.command_application_items,
            files: &files,
            now: Some(now),
            ranking: &self.command_ranking,
            configured_scripts: &self.command_scripts,
        })
    }

    pub fn record_command_use(&mut self, id: &str) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.record_use(id);
        self.persist_command_ranking()
    }

    pub fn set_command_pinned(&mut self, id: &str, pinned: bool) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.set_pinned(id, pinned);
        self.persist_command_ranking()
    }

    pub fn reset_command_ranking(&mut self) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.reset();
        self.persist_command_ranking()
    }

    fn persist_command_ranking(&self) -> Result<(), String> {
        let Some(path) = self.command_ranking_path.as_deref() else {
            return Err(
                "no writable ranking file is available; set XDG_DATA_HOME or HOME".to_owned(),
            );
        };
        crate::command_bar::save_ranking(path, &self.command_ranking)
    }

    /// Shared access prevents copying from disturbing history ownership.
    pub fn copy_to_clipboard(&mut self, text: &str) -> Result<(), String> {
        self.require_command_bar()?;
        let backend = self
            .session_clipboard
            .as_mut()
            .ok_or_else(|| "no clipboard connection is available for this session".to_string())?;
        backend.write_text(text).map_err(|error| error.to_string())
    }

    pub fn run_command_script(&self, index: usize) -> Result<ScriptOutcome, String> {
        self.require_command_bar()?;
        let script = self
            .command_scripts
            .get(index)
            .ok_or_else(|| format!("script {index} is not configured"))?;
        let path_env = std::env::var_os("PATH");
        run_script(script, path_env.as_deref()).map_err(|error| error.to_string())
    }

    /// GIO applies desktop-entry terminal, `TryExec`, and `Exec` rules.
    pub fn launch_command_application(&self, index: usize) -> Result<(), String> {
        self.require_command_bar()?;
        let application = self
            .command_applications
            .get(index)
            .ok_or_else(|| format!("application {index} is no longer available"))?;
        launch_desktop_entry(application).map_err(|error| error.to_string())
    }

    pub fn open_command_target(&self, target: &str) -> Result<(), String> {
        self.require_command_bar()?;
        let launcher = self
            .command_launcher
            .as_ref()
            .ok_or_else(|| "no desktop open handler is available".to_string())?;
        launcher.open(target).map_err(|error| error.to_string())
    }

    pub fn snippet_library(&self) -> &SnippetLibrary {
        &self.snippet_library
    }

    pub fn snippet_policy(&self) -> SnippetPolicy {
        self.snippet_policy
    }

    pub fn snippet_warnings(&self) -> &[ConfigurationWarning] {
        &self.snippet_warnings
    }

    pub fn snippets_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SNIPPETS_ID && registration.running)
    }

    pub fn snippet_provider(&self) -> Option<InsertionProvider> {
        self.snippet_service.provider()
    }

    pub fn snippet_unavailable_reason(&self) -> Option<&str> {
        self.snippet_service.unavailable_reason()
    }

    pub fn snippet_directory(&self) -> Option<String> {
        self.snippet_path
            .as_deref()
            .and_then(|path| path.parent())
            .map(|parent| parent.display().to_string())
    }

    pub fn snippet_matches(&self, query: &str, limit: usize) -> Vec<SnippetMatch> {
        self.snippet_library
            .search(query, limit, self.snippet_policy)
    }

    /// Writes a candidate before replacing live state, so failed saves stay inert.
    pub fn save_snippet(&mut self, snippet: kestrel_core::Snippet) -> Result<(), String> {
        let mut candidate = self.snippet_library.clone();
        candidate
            .upsert(snippet, self.snippet_policy)
            .map_err(|error| error.to_string())?;
        self.persist_snippets(&candidate)?;
        self.snippet_library = candidate;
        self.rebuild_command_index();
        Ok(())
    }

    /// Removal takes effect only after persistence succeeds.
    pub fn delete_snippet(&mut self, name: &str) -> Result<(), String> {
        let mut candidate = self.snippet_library.clone();
        candidate.remove(name).map_err(|error| error.to_string())?;
        self.persist_snippets(&candidate)?;
        self.snippet_library = candidate;
        self.rebuild_command_index();
        Ok(())
    }

    fn persist_snippets(&self, library: &SnippetLibrary) -> Result<(), String> {
        let Some(path) = self.snippet_path.as_deref() else {
            return Err(
                "no writable snippet file is available; set XDG_DATA_HOME or HOME".to_owned(),
            );
        };
        save_snippets(path, library, self.snippet_policy).map_err(|error| error.to_string())
    }

    /// Reads the clipboard once, clips it to the configured bound, and requires
    /// the snippet feature to be running before typing into another application.
    pub fn insert_snippet(&mut self, name: &str) -> Result<InsertionReport, SnippetServiceError> {
        if !self.snippets_running() {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: "snippets.text is not running; enable it in the Feature Hub".to_owned(),
            });
        }
        let snippet = self.snippet_library.get(name).cloned().ok_or_else(|| {
            SnippetServiceError::ExpansionUnavailable {
                reason: format!("no snippet named \"{name}\" is stored"),
            }
        })?;
        let clipboard_available = self.session_clipboard.is_some();
        let clipboard = self
            .session_clipboard
            .as_mut()
            .and_then(|backend| backend.read_text().ok().flatten());
        let clock = SystemClock;
        let local_time = clock.local_time();
        let timezone = clock.timezone_name();
        let context = RenderContext {
            local_time: Some(local_time),
            timezone: Some(timezone.as_str()),
            clipboard: clipboard.as_deref(),
            clipboard_available,
        };
        self.snippet_service
            .insert(&snippet, self.snippet_policy, &context)
    }

    /// Returns retained-item metadata without exposing contents.
    pub fn clipboard_snapshot(&self) -> ClipboardSnapshot {
        self.clipboard_history.latest()
    }

    /// Returns bounded, explicitly requested previews outside snapshots and diagnostics.
    pub fn clipboard_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Option<Result<Vec<ClipboardMatch>, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.search(query, limit))
    }

    /// Returns one bounded entry preview through the running worker.
    pub fn clipboard_preview(
        &self,
        item_id: u64,
        max_bytes: usize,
    ) -> Option<Result<ClipboardPreview, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.preview(item_id, max_bytes))
    }

    /// Applies a clipboard command only while the opt-in service is running.
    pub fn execute_clipboard_command(
        &self,
        command: ClipboardCommand,
    ) -> Option<Result<ClipboardSnapshot, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.execute(command))
    }
    pub fn quick_toggle_snapshots(&self) -> impl Iterator<Item = &QuickToggleSnapshot> {
        self.quick_toggles.snapshots()
    }

    /// Applies a quick-toggle command only while its independent adapter is running.
    pub fn execute_quick_toggle_command(
        &mut self,
        command: QuickToggleCommand,
    ) -> Option<Result<QuickToggleSnapshot, QuickToggleError>> {
        if !self.quick_toggle_is_running(command.id) {
            self.sync_battery_alert_rule();
            return None;
        }
        let result = self.quick_toggles.execute(command).cloned();
        self.sync_battery_alert_rule();
        Some(result)
    }

    fn refresh_quick_toggles(&mut self) {
        for id in ALL_QUICK_TOGGLES {
            if self.quick_toggle_is_running(id) {
                self.quick_toggles.refresh(id);
            }
        }
        self.sync_battery_alert_rule();
    }

    fn reconcile_resources(&mut self) {
        // Stop services to release claims and join workers.
        if !self.microphone_is_running() {
            self.microphone.reset();
        }
        if !self.speed_test_is_running() {
            self.speed_test.stop();
        }
        if self.clipboard_history_is_running() {
            if !self.clipboard_worker_is_running() {
                let _ = self.start_clipboard_history();
            }
        } else {
            self.clipboard_history.stop();
        }

        if !self.quick_toggle_is_running(QuickToggleId::BatteryAlerts) {
            self.quick_toggles.stop(QuickToggleId::BatteryAlerts);
        }

        if !self.quick_toggle_is_running(QuickToggleId::KeepAwake) {
            self.quick_toggles.stop(QuickToggleId::KeepAwake);
        }
    }
    /// Battery alerts can only narrow the user's configured preference.
    fn sync_battery_alert_rule(&mut self) {
        let toggle_enabled = self
            .quick_toggles
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::BatteryAlerts)
            .and_then(|snapshot| snapshot.observation.as_ref())
            .and_then(|observation| match &observation.control {
                QuickToggleControl::Switch { enabled, .. } => Some(*enabled),
                _ => None,
            })
            .unwrap_or(false);
        self.alerts.set_rule_enabled(
            AlertKind::Battery,
            self.battery_alert_configured && toggle_enabled,
        );
    }

    fn start_clipboard_history(&mut self) -> Result<(), ClipboardServiceError> {
        let provider = discover_provider().map_err(ClipboardServiceError::Backend)?;
        let backend =
            ArboardClipboardBackend::new(provider).map_err(ClipboardServiceError::Backend)?;
        let privacy = LogindPrivacyMonitor::new().map_err(ClipboardServiceError::Backend)?;
        self.clipboard_history.start(backend, privacy)
    }

    fn clipboard_history_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == CLIPBOARD_HISTORY_ID && registration.running
        })
    }

    fn clipboard_worker_is_running(&self) -> bool {
        matches!(
            self.clipboard_history.latest().lifecycle,
            kestrel_services::clipboard::ClipboardLifecycle::Running
        )
    }

    fn audio_mixer_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == AUDIO_MIXER_ID && registration.running)
    }

    fn system_monitor_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == SYSTEM_MONITOR_ID && registration.running
        })
    }

    pub fn monitor_is_running(&self) -> bool {
        self.system_monitor_is_running()
    }
    fn quick_toggle_is_running(&self, id: QuickToggleId) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == id.feature_id() && registration.running)
    }

    fn default_enabled(feature_id: &str) -> bool {
        ALL_QUICK_TOGGLES
            .into_iter()
            .any(|id| id.feature_id() == feature_id)
    }

    fn preset_enabled(preset: FeaturePreset, feature_id: &str) -> bool {
        match preset {
            FeaturePreset::Essentials => matches!(
                feature_id,
                SYSTEM_MONITOR_ID
                    | AUDIO_MIXER_ID
                    | MICROPHONE_FEATURE_ID
                    | kestrel_platform::quick_toggles::KEEP_AWAKE_ID
                    | kestrel_platform::quick_toggles::SCREEN_LOCK_ID
            ),
            FeaturePreset::Balanced => {
                feature_id != CLIPBOARD_HISTORY_ID && feature_id != GLOBAL_SHORTCUTS_ID
            }
            FeaturePreset::Everything => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApplicationRuntime, FeaturePreset};
    use crate::STATUS_NOTIFIER_ID;
    use kestrel_core::{AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus};
    use kestrel_platform::{
        audio::{FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID},
        clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
        quick_toggles::{ALL_QUICK_TOGGLES, KEEP_AWAKE_ID, SCREEN_LOCK_ID},
        speed_test::FEATURE_ID as SPEED_TEST_ID,
        system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID,
    };
    use kestrel_services::{
        audio::{AudioAvailability, AudioCommand},
        clipboard::{ClipboardCommand, ClipboardLifecycle},
        microphone::MicrophoneCommand,
    };
    use std::time::Duration;

    use super::COMMAND_BAR_ID;
    use kestrel_core::Snippet;
    use kestrel_platform::{
        applications::ApplicationEntry,
        snippets::{Clock, SystemClock},
    };
    use kestrel_services::{
        command_bar::{CommandAction, CommandSource},
        snippets::{SnippetLibrary, SnippetServiceError},
    };
    use tempfile::TempDir;

    #[test]
    fn startup_keeps_unavailable_features_visible() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .refresh_capabilities()
            .expect("static capability refreshes");

        let registrations = runtime.registrations().collect::<Vec<_>>();
        assert!(
            registrations
                .iter()
                .any(
                    |registration| registration.feature.id == "app.command-surface"
                        && registration.running
                )
        );
        for feature_id in [AUDIO_MIXER_ID, CLIPBOARD_HISTORY_ID, STATUS_NOTIFIER_ID] {
            let registration = registrations
                .iter()
                .find(|registration| registration.feature.id == feature_id)
                .expect("feature remains visible");
            assert!(!registration.running);
        }
        for id in ALL_QUICK_TOGGLES {
            assert!(
                registrations
                    .iter()
                    .any(|registration| registration.feature.id == id.feature_id()),
                "{} remains visible",
                id.feature_id()
            );
        }
        assert!(
            registrations
                .iter()
                .any(|registration| registration.feature.id == SYSTEM_MONITOR_ID)
        );
        for registration in registrations
            .iter()
            .filter(|registration| !registration.available)
        {
            assert!(
                registration.capability.remediation.is_some(),
                "{} must explain remediation when unavailable",
                registration.feature.id
            );
        }
    }

    #[test]
    fn successful_status_notifier_registration_is_reported_as_running() {
        let report = CapabilityReport::new(
            STATUS_NOTIFIER_ID,
            CapabilityStatus::Supported,
            "The tray host accepted Kestrel.",
        )
        .with_selected_backend("StatusNotifierItem");
        let mut runtime = ApplicationRuntime::new_with_status_notifier(
            &ApplicationConfiguration::default(),
            report,
        )
        .expect("runtime builds");

        runtime.start();

        let tray = runtime
            .registrations()
            .find(|registration| registration.feature.id == STATUS_NOTIFIER_ID)
            .expect("tray integration remains visible");
        assert!(tray.running);
        assert_eq!(
            tray.capability.selected_backend.as_deref(),
            Some("StatusNotifierItem")
        );
    }

    #[test]
    fn enabled_monitor_starts_with_a_non_failing_live_snapshot() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");

        runtime.start();

        assert!(runtime.system_monitor_snapshot().is_some());
    }
    #[test]
    fn battery_alert_rule_is_gated_until_toggle_observation() {
        let runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        assert!(
            !runtime
                .alerts
                .policy()
                .rule(AlertKind::Battery)
                .expect("battery rule exists")
                .enabled
        );
    }

    #[test]
    fn monitor_sampling_respects_configured_interval() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        configuration.monitoring.refresh_interval_millis = 60_000;
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();

        assert!(matches!(
            runtime
                .sample_monitor(Duration::from_millis(59_999))
                .outcome,
            kestrel_services::system_monitor::RefreshOutcome::Skipped
        ));
    }

    #[test]
    fn monitor_history_stays_bounded_by_configured_capacity() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.monitoring.history_samples = 1;
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        let _ = runtime.sample_monitor(Duration::from_secs(1));
        let _ = runtime.sample_monitor(Duration::from_secs(2));

        assert_eq!(
            runtime
                .history_summary()
                .expect("history has samples")
                .samples,
            1
        );
    }

    #[test]
    fn disabled_audio_service_does_not_accept_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.audio_snapshot().availability,
            AudioAvailability::Unavailable
        );
        assert!(
            runtime
                .execute_audio_command(AudioCommand::SetStreamMute {
                    stream_id: 1,
                    muted: true,
                })
                .is_none()
        );
    }

    #[test]
    fn disabled_microphone_service_reports_unknown_and_rejects_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        assert_eq!(
            runtime.microphone_snapshot().mute,
            kestrel_services::microphone::MicrophoneMuteState::Unknown
        );
        assert!(
            runtime
                .execute_microphone_command(MicrophoneCommand::ToggleMute)
                .is_none()
        );
    }

    #[test]
    fn a_disabled_speed_test_never_starts_or_transfers() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        let error = runtime
            .start_speed_test()
            .expect_err("a stopped feature cannot run a test");

        assert!(error.contains("network.speed_test"), "{error}");
        assert_eq!(
            runtime.speed_test_snapshot().status,
            kestrel_services::speed_test::SpeedTestStatus::Idle
        );
        assert!(runtime.cancel_speed_test().is_err());
    }

    #[test]
    fn only_the_wider_presets_include_the_on_demand_speed_test() {
        assert!(!ApplicationRuntime::preset_enabled(
            FeaturePreset::Essentials,
            SPEED_TEST_ID
        ));
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Balanced,
            SPEED_TEST_ID
        ));
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Essentials,
            MICROPHONE_FEATURE_ID
        ));
    }

    #[test]
    fn clipboard_history_is_inert_until_explicitly_enabled() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.clipboard_snapshot().lifecycle,
            ClipboardLifecycle::Stopped
        );
        assert!(
            runtime
                .execute_clipboard_command(ClipboardCommand::Wipe)
                .is_none()
        );
    }

    #[test]
    fn preset_policy_is_observable_and_undo_restores_the_exact_feature_map() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, false)
            .expect("feature ID is valid");
        configuration
            .set_feature_enabled(CLIPBOARD_HISTORY_ID, true)
            .expect("feature ID is valid");
        configuration
            .set_feature_enabled("feature.preserved", false)
            .expect("feature ID is valid");
        let original_features = configuration.features.clone();
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");

        let snapshot = runtime
            .apply_preset(&mut configuration, FeaturePreset::Essentials)
            .expect("preset applies");

        assert_eq!(snapshot.features, original_features);
        for registration in runtime
            .registrations()
            .filter(|registration| registration.feature.configurable)
        {
            let expected = matches!(
                registration.feature.id,
                SYSTEM_MONITOR_ID
                    | AUDIO_MIXER_ID
                    | MICROPHONE_FEATURE_ID
                    | KEEP_AWAKE_ID
                    | SCREEN_LOCK_ID
            );
            assert_eq!(
                registration.enabled, expected,
                "{} follows the Essentials policy",
                registration.feature.id
            );
            assert_eq!(
                configuration
                    .features
                    .get(registration.feature.id)
                    .map(|feature| feature.enabled),
                Some(expected),
                "{} is represented in the applied configuration",
                registration.feature.id
            );
        }
        assert_eq!(
            configuration.features.get("feature.preserved"),
            original_features.get("feature.preserved")
        );

        runtime
            .undo_preset(&mut configuration, snapshot)
            .expect("preset undo applies");

        assert_eq!(configuration.features, original_features);
    }

    fn application(id: &str, name: &str, terminal: bool) -> ApplicationEntry {
        ApplicationEntry {
            id: id.to_owned(),
            name: name.to_owned(),
            comment: None,
            command: vec![name.to_lowercase()],
            terminal,
            source: None,
        }
    }

    #[test]
    fn a_disabled_command_bar_neither_searches_nor_acts() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let now = SystemClock.local_time();

        assert!(!runtime.command_bar_running());
        runtime.command_ranking.set_pinned("kestrel:refresh", true);
        assert!(
            runtime.command_search("refresh", &now).is_empty(),
            "a stopped command bar ranks nothing"
        );
        for error in [
            runtime
                .run_command_script(0)
                .expect_err("scripts are gated"),
            runtime
                .launch_command_application(0)
                .expect_err("launching is gated"),
            runtime
                .open_command_target("https://example.com")
                .expect_err("opening is gated"),
            runtime
                .copy_to_clipboard("value")
                .expect_err("copying is gated"),
            runtime
                .set_command_pinned("kestrel:other", true)
                .expect_err("pinning is gated"),
            runtime
                .record_command_use("kestrel:other")
                .expect_err("recording a use is gated"),
            runtime
                .reset_command_ranking()
                .expect_err("resetting is gated"),
        ] {
            assert!(error.contains("not running"), "unexpected error: {error}");
        }
        assert!(
            runtime.command_ranking().is_pinned("kestrel:refresh"),
            "a refused reset leaves the seeded ranking untouched"
        );
        assert!(
            !runtime.command_ranking().is_pinned("kestrel:other"),
            "a refused pin adds nothing"
        );
    }

    #[test]
    fn an_enabled_command_bar_ranks_but_hides_snippets_while_they_are_stopped() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        let now = SystemClock.local_time();
        assert!(runtime.command_bar_running());
        assert!(!runtime.snippets_running());

        runtime
            .snippet_library
            .upsert(
                Snippet::new("Zq Greeting", None, None, "hello"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");
        runtime.rebuild_command_index();

        assert!(
            runtime
                .command_search("refresh", &now)
                .iter()
                .any(|result| result.item.id == "kestrel:refresh"),
            "portable commands answer while the bar runs"
        );
        assert!(
            runtime
                .command_search("zq greeting", &now)
                .iter()
                .all(|result| result.item.source != CommandSource::Snippet),
            "a stopped snippet feature offers nothing to run"
        );
    }

    #[test]
    fn snippet_insertion_requires_the_running_feature() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .snippet_library
            .upsert(
                Snippet::new("Greeting", None, None, "hello"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");

        let error = runtime
            .insert_snippet("Greeting")
            .expect_err("a stopped feature must not type text");

        assert!(matches!(
            &error,
            SnippetServiceError::ExpansionUnavailable { reason } if reason.contains("not running")
        ));
    }

    #[test]
    fn applications_that_need_a_terminal_are_offered_and_launched_through_their_desktop_file() {
        let entries = vec![
            application("top.desktop", "Top", true),
            application("editor.desktop", "Editor", false),
        ];

        let items = ApplicationRuntime::application_items(&entries);

        assert_eq!(
            items.len(),
            2,
            "terminal entries are offered like any other"
        );
        assert_eq!(
            items[0].action,
            CommandAction::OpenApplication { index: 0 },
            "items address the scanned list"
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        // Entries without desktop files cannot be launched through GIO.
        runtime.command_applications = entries;
        let error = runtime
            .launch_command_application(0)
            .expect_err("an entry without a desktop file cannot launch");
        assert!(error.contains("no desktop file"), "{error}");
        assert!(
            runtime
                .launch_command_application(9)
                .expect_err("unknown index")
                .contains("no longer available")
        );
    }

    #[test]
    fn a_failed_snippet_write_leaves_the_library_unchanged() {
        let dir = TempDir::new().expect("temp dir");
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "a regular file").expect("blocker is written");
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.snippet_library = SnippetLibrary::default();
        runtime
            .snippet_library
            .upsert(
                Snippet::new("Keep", None, None, "kept"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");
        // A regular-file parent makes writes fail.
        runtime.snippet_path = Some(blocker.join("snippets.toml"));

        runtime
            .save_snippet(Snippet::new("Rejected", None, None, "nope"))
            .expect_err("the write fails");
        assert!(
            runtime.snippet_library().get("Rejected").is_none(),
            "a snippet that was not saved must not stay active"
        );

        runtime.delete_snippet("Keep").expect_err("the write fails");
        assert!(
            runtime.snippet_library().get("Keep").is_some(),
            "a snippet that was not deleted must stay"
        );

        let path = dir.path().join("data/snippets.toml");
        runtime.snippet_path = Some(path.clone());
        runtime
            .save_snippet(Snippet::new("Greeting", None, None, "hello"))
            .expect("the save succeeds");
        let reloaded = crate::load_snippets(&path, runtime.snippet_policy());
        assert!(reloaded.warnings.is_empty());
        assert_eq!(reloaded.library.names(), vec!["Keep", "Greeting"]);

        runtime.delete_snippet("Keep").expect("the delete succeeds");
        assert!(runtime.snippet_library().get("Keep").is_none());
        assert_eq!(
            crate::load_snippets(&path, runtime.snippet_policy())
                .library
                .names(),
            vec!["Greeting"]
        );
    }
}
