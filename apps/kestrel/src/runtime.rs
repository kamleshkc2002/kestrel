use crate::{
    ApplicationViewModel, ConfigurationWarning,
    snippets::{save_snippets, snippet_path},
    status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, unavailable_capability},
    view_model::{
        AudioPresentation, ClipboardPresentation, ClipboardQuery, ClipboardViewModel,
        MonitorPresentation, MonitorViewModel, SnippetQuery, SnippetsPresentation,
    },
};
use kestrel_core::{
    AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus, CostLevel,
    FeatureConfigurationSnapshot, FeatureSpec, ResourceCost,
};
use kestrel_platform::{
    StaticCapabilityProbe,
    audio::{FEATURE_ID as AUDIO_MIXER_ID, PulseAudioBackend},
    clipboard::{
        ArboardClipboardBackend, ClipboardBackend, ClipboardCapabilityProbe,
        FEATURE_ID as CLIPBOARD_HISTORY_ID, LogindPrivacyMonitor, discover_provider,
    },
    quick_toggles::{
        ALL_QUICK_TOGGLES, LinuxQuickToggleBackend, QuickToggleCapabilityProbe, QuickToggleControl,
        QuickToggleError, QuickToggleId,
    },
    snippets::{
        Clock, ExecutableInsertionBackend, InsertionProvider, SnippetInsertionProbe, SystemClock,
        discover_insertion_provider,
    },
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
    quick_toggles::{QuickToggleCommand, QuickToggleService, QuickToggleSnapshot},
    snippets::{
        InsertionReport, RenderContext, SnippetInsertionService, SnippetLibrary, SnippetMatch,
        SnippetPolicy, SnippetServiceError,
    },
    system_monitor::{HistorySummary, RefreshOutcome, SystemMonitorService, SystemSnapshot},
};
/// A built-in enablement policy for the configurable features.
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

/// UI-independent composition root for startup, enablement, and capability refresh.
/// The stable feature identifier for the snippet library.
pub const SNIPPETS_ID: &str = "snippets.text";

pub struct ApplicationRuntime {
    registry: FeatureRegistry,
    audio_mixer: AudioMixerService<PulseAudioBackend>,
    clipboard_history: ClipboardHistoryService,
    /// Snippet library state, its capability-gated insert path, and its file.
    snippet_library: SnippetLibrary,
    snippet_service: SnippetInsertionService<ExecutableInsertionBackend>,
    snippet_policy: SnippetPolicy,
    snippet_expansion_timing: kestrel_core::SnippetExpansionTiming,
    snippet_provider_preference: kestrel_core::SnippetProviderPreference,
    snippet_path: Option<std::path::PathBuf>,
    snippet_warnings: Vec<ConfigurationWarning>,
    /// Reads the live selection for the `{{clipboard}}` variable.
    snippet_clipboard: Option<ArboardClipboardBackend>,
    system_monitor: SystemMonitorService<ProcSysMonitor>,
    alerts: AlertEngine,
    /// The user's Battery-alert preference; the quick toggle can only narrow it.
    battery_alert_configured: bool,
    quick_toggles: QuickToggleService,
}
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorTick {
    pub outcome: RefreshOutcome,
    pub alerts: Vec<AlertEvent>,
}

impl ApplicationRuntime {
    /// Builds the runtime without requiring a tray host.
    pub fn new(configuration: &ApplicationConfiguration) -> Result<Self, RegistryError> {
        Self::new_with_status_notifier(
            configuration,
            unavailable_capability("StatusNotifierItem registration was not attempted."),
        )
    }

    /// Builds the runtime with the result of the optional StatusNotifierItem registration.
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

        let mut runtime = Self {
            registry,
            audio_mixer: AudioMixerService::with_policy(
                audio_backend,
                AudioPolicy::from_configuration(&configuration.audio),
            )
            .expect("the validated audio policy is valid"),
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
            snippet_clipboard: Self::build_snippet_clipboard(),
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

    /// Starts only the entries that are enabled and currently available.
    pub fn start(&mut self) {
        self.registry.start_enabled();
        self.reconcile_resources();
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(Duration::ZERO);
        }
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
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
        // Device loss is repaired by re-reading the graph, so an explicit refresh
        // must resample audio as well as re-probe capabilities.
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        self.refresh_quick_toggles();
        Ok(())
    }

    /// Applies the current configuration to configurable registrations only.
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
        if self.clipboard_history_is_running() {
            let _ = self
                .clipboard_history
                .set_policy(ClipboardPolicy::from_configuration(
                    &configuration.clipboard,
                ));
        }
        // Provider preference, timeout, and snippet bounds are all re-derived, so
        // a configuration change re-runs discovery and re-tunes the policy.
        self.snippet_policy = SnippetPolicy::from_configuration(&configuration.snippets);
        self.snippet_expansion_timing = configuration.snippets.expansion_timing;
        self.snippet_provider_preference = configuration.snippets.preferred_provider;
        self.snippet_service = Self::build_snippet_service(configuration);
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
    /// Returns UI-independent registration snapshots for the active session.
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }

    /// Extracts owned presentation state without exposing live service resources.
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
        )
    }

    /// Builds the view model with the application's current clipboard query.
    ///
    /// Search results and previews are produced by explicit, bounded requests,
    /// so only the values the user asked to see are rendered.
    /// Builds the view model with the window's clipboard and snippet state.
    pub fn view_model_with_panels(
        &self,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
        clipboard: ClipboardQuery<'_>,
        snippets: SnippetQuery<'_>,
    ) -> ApplicationViewModel {
        ApplicationViewModel::new(
            self.registrations(),
            self.quick_toggles.snapshots(),
            AudioPresentation {
                snapshot: Some(self.audio_mixer.latest()),
                running: self.audio_mixer_is_running(),
                policy: self.audio_mixer.policy(),
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

    /// Returns the latest immutable monitor snapshot, if sampling has started.
    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }

    /// Returns the latest audio snapshot, including structured unavailable/empty states.
    pub fn audio_snapshot(&self) -> &AudioSnapshot {
        self.audio_mixer.latest()
    }
    /// Whether the opt-in clipboard history registration is running.
    pub fn clipboard_is_running(&self) -> bool {
        self.clipboard_history_is_running()
    }

    /// Builds clipboard presentation state alone, for targeted tick updates.
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

    /// Applies an audio command only while the opt-in mixer service is running.
    pub fn execute_audio_command(&mut self, command: AudioCommand) -> Option<AudioCommandResult> {
        self.audio_mixer_is_running()
            .then(|| self.audio_mixer.execute(command))
    }
    /// Discovers an insertion provider and builds the capability-gated service.
    fn build_snippet_service(
        configuration: &ApplicationConfiguration,
    ) -> SnippetInsertionService<ExecutableInsertionBackend> {
        let discovery = discover_insertion_provider(configuration.snippets.preferred_provider)
            .and_then(|path| {
                ExecutableInsertionBackend::new(path, configuration.snippets.insert_timeout_millis)
            });
        SnippetInsertionService::new(discovery)
    }

    /// Opens a second clipboard reader for the `{{clipboard}}` variable.
    ///
    /// Reading never takes ownership, so it cannot disturb the clipboard
    /// history service's selection.
    fn build_snippet_clipboard() -> Option<ArboardClipboardBackend> {
        discover_provider()
            .ok()
            .and_then(|provider| ArboardClipboardBackend::new(provider).ok())
    }

    /// The snippet library as the window sees it.
    pub fn snippet_library(&self) -> &SnippetLibrary {
        &self.snippet_library
    }

    pub fn snippet_policy(&self) -> SnippetPolicy {
        self.snippet_policy
    }

    pub fn snippet_warnings(&self) -> &[ConfigurationWarning] {
        &self.snippet_warnings
    }

    /// Whether the snippet registration is running.
    pub fn snippets_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SNIPPETS_ID && registration.running)
    }

    /// The verified insertion provider, when one exists.
    pub fn snippet_provider(&self) -> Option<InsertionProvider> {
        self.snippet_service.provider()
    }

    /// Why insertion stays disabled, when no provider was verified.
    pub fn snippet_unavailable_reason(&self) -> Option<&str> {
        self.snippet_service.unavailable_reason()
    }

    /// The directory holding the snippet file, for the window's status line.
    pub fn snippet_directory(&self) -> Option<String> {
        self.snippet_path
            .as_deref()
            .and_then(|path| path.parent())
            .map(|parent| parent.display().to_string())
    }

    /// Searches the library; previews render variables as placeholders.
    pub fn snippet_matches(&self, query: &str, limit: usize) -> Vec<SnippetMatch> {
        self.snippet_library
            .search(query, limit, self.snippet_policy)
    }

    /// Saves or replaces a snippet and persists the library.
    pub fn save_snippet(&mut self, snippet: kestrel_core::Snippet) -> Result<(), String> {
        self.snippet_library
            .upsert(snippet, self.snippet_policy)
            .map_err(|error| error.to_string())?;
        self.persist_snippets()
    }

    /// Removes a snippet and persists the library.
    pub fn delete_snippet(&mut self, name: &str) -> Result<(), String> {
        self.snippet_library
            .remove(name)
            .map_err(|error| error.to_string())?;
        self.persist_snippets()
    }

    fn persist_snippets(&self) -> Result<(), String> {
        let Some(path) = self.snippet_path.as_deref() else {
            return Err(
                "no writable snippet file is available; set XDG_DATA_HOME or HOME".to_owned(),
            );
        };
        save_snippets(path, &self.snippet_library, self.snippet_policy)
            .map_err(|error| error.to_string())
    }

    /// Renders one snippet and types it, if a provider is verified.
    ///
    /// The clipboard variable is read here, once, through a read-only
    /// connection, and the render clips it to the configured bound.
    pub fn insert_snippet(&mut self, name: &str) -> Result<InsertionReport, SnippetServiceError> {
        let snippet = self.snippet_library.get(name).cloned().ok_or_else(|| {
            SnippetServiceError::ExpansionUnavailable {
                reason: format!("no snippet named \"{name}\" is stored"),
            }
        })?;
        let clipboard_available = self.snippet_clipboard.is_some();
        let clipboard = self
            .snippet_clipboard
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

    /// Returns metadata about retained clipboard items without exposing their contents.
    pub fn clipboard_snapshot(&self) -> ClipboardSnapshot {
        self.clipboard_history.latest()
    }

    /// Searches retained entries through the running worker.
    ///
    /// The returned previews are bounded and explicitly requested; they never
    /// enter the service snapshot or its diagnostics.
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
    /// Returns owned state for every independently capability-gated quick toggle.
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
    /// The `power.battery-alerts` quick toggle can only narrow the user's configured
    /// Battery-alert preference; it never re-enables a rule the user turned off.
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

    /// Whether the monitor feature currently owns sampling resources.
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
        audio::FEATURE_ID as AUDIO_MIXER_ID,
        clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
        quick_toggles::{ALL_QUICK_TOGGLES, KEEP_AWAKE_ID, SCREEN_LOCK_ID},
        system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID,
    };
    use kestrel_services::{
        audio::{AudioAvailability, AudioCommand},
        clipboard::{ClipboardCommand, ClipboardLifecycle},
    };
    use std::time::Duration;

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
                SYSTEM_MONITOR_ID | AUDIO_MIXER_ID | KEEP_AWAKE_ID | SCREEN_LOCK_ID
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
}
