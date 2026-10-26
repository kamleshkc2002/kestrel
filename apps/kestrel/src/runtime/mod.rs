//! Application runtime orchestration and feature registry.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use crate::{
    ApplicationCommand, ApplicationViewModel, ConfigurationWarning,
    command_line::{CommandGate, CommandLineAction},
    snippets::{save_snippets, snippet_path},
    status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, unavailable_capability},
    view_model::{
        AudioPresentation, AudioViewModel, CameraPresentation, CameraViewModel,
        CapturePresentation, CaptureViewModel, ClipboardPresentation, ClipboardQuery,
        ClipboardViewModel, CommandBarPresentation, CommandBarQuery, ListenersViewModel,
        MicrophonePresentation, MicrophoneViewModel, MonitorPresentation, MonitorViewModel,
        ProcessesViewModel, QuickToggleViewModel, RecordingPresentation, RecordingViewModel,
        ScratchpadPresentation, ScratchpadViewModel, ScreenToolGate, ScreenToolsPresentation,
        ScreenToolsViewModel, ShortcutsPresentation, ShortcutsViewModel, SnippetQuery,
        SnippetsPresentation, SpeedTestPresentation, SpeedTestViewModel, WhatsNewViewModel,
    },
    whats_new::{self, Change},
};
use kestrel_core::{
    AlertKind, ApplicationConfiguration, CapabilityEvidence, CapabilityReport, CapabilityStatus,
    CostLevel, FeatureConfigurationSnapshot, FeatureSpec, ResourceCost,
};
use kestrel_platform::{
    CapabilityProbe, StaticCapabilityProbe,
    applications::{
        ApplicationEntry, ExternalLauncher, application_directories, launch_desktop_action,
        launch_desktop_entry, scan_applications,
    },
    audio::{
        FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID, MicrophoneProbe, PulseAudioBackend,
    },
    camera::{CameraProbe, FEATURE_ID as CAMERA_ID, V4l2Camera, system_cameras},
    capture::{
        CaptureDetection, CaptureMode, CaptureProvider, FEATURE_ID as CAPTURE_ID,
        PortalColorPicker, backend_for as capture_backend_for,
    },
    clipboard::{
        ArboardClipboardBackend, ClipboardBackend, ClipboardCapabilityProbe,
        FEATURE_ID as CLIPBOARD_HISTORY_ID, LogindPrivacyMonitor, discover_provider,
    },
    global_shortcuts::{
        ActivationSink, FEATURE_ID as GLOBAL_SHORTCUTS_ID, GlobalShortcutsProbe, ShortcutDetection,
        ShortcutRequest, ShortcutStatus, backend_for as shortcut_backend_for,
    },
    launcher::{ScriptOutcome, run_script, search_roots},
    processes::{
        FEATURE_ID as PROCESSES_ID, LISTENERS_FEATURE_ID, ListenersProbe, PidfdSignaller,
        ProcessProbe, ProcessSource, ProcfsListeners, ProcfsProcesses, RestartTarget,
        SystemRestarter,
    },
    quick_toggles::{
        ALL_QUICK_TOGGLES, LinuxQuickToggleBackend, QuickToggleCapabilityProbe, QuickToggleError,
        QuickToggleId,
    },
    recording::{
        AudioTrack, FEATURE_ID as RECORDING_ID, GstTakeEditor, PortalRecordingBackend,
        RecordingDetection, TakeEditor,
    },
    screen_tools::{
        ALL_SCREEN_TOOLS, ColorSource, ScreenTool, TesseractAdapter, color_source,
        screen_tool_capability,
    },
    snippets::{
        ExecutableInsertionBackend, InsertionProvider, SnippetInsertionProbe, SystemClock,
        discover_insertion_provider,
    },
    speed_test::{CurlSpeedTestBackend, FEATURE_ID as SPEED_TEST_ID, SpeedTestProbe},
    system_monitor::{FEATURE_ID as SYSTEM_MONITOR_ID, ProcSysMonitor},
    windows::{
        WindowErrorKind, WindowHandle, WindowInfo, WindowProvider, discover_window_provider,
    },
};
use kestrel_services::{
    FeatureRegistry, RegistryError, ServiceRegistration,
    alerts::{AlertEngine, AlertEvent, AlertPolicy},
    audio::{
        AudioCommand, AudioCommandResult, AudioMixerService, AudioPolicy, AudioSnapshot,
        DevicePriority,
    },
    camera::{CameraFrameSlot, CameraPreviewService},
    capture::{CaptureId, CapturePolicy, CaptureService, CaptureSnapshot, image::EditPlan},
    clipboard::{
        ClipboardCommand, ClipboardHistoryService, ClipboardMatch, ClipboardPolicy,
        ClipboardPreview, ClipboardServiceError, ClipboardSnapshot,
    },
    command_bar::{
        CommandIndex, CommandItem, CommandRanking, CommandResult, CommandSource, EnabledProviders,
        SearchInput,
    },
    global_shortcuts::GlobalShortcutService,
    microphone::{
        MicrophoneCommand, MicrophoneCommandResult, MicrophoneService, MicrophoneSnapshot,
    },
    processes::{
        ActionOutcome, ConfirmAction, ListenerService, ProcessInventoryService, ProcessPolicy,
        ProcessSnapshot, confirmation_scope, kill, restart, terminate,
    },
    quick_toggles::{QuickToggleCommand, QuickToggleService, QuickToggleSnapshot},
    recording::{RecordingOptions, RecordingPhase, RecordingService},
    scratchpad::{ScratchpadService, ScratchpadSnapshot},
    screen_tools::{
        ScreenToolSource, ScreenToolsService, ScreenToolsSnapshot, Selection, TesseractRecognizer,
        TextRecognizer,
    },
    snippets::{
        InsertionReport, RenderContext, SnippetInsertionService, SnippetLibrary, SnippetMatch,
        SnippetPolicy, SnippetServiceError,
    },
    speed_test::{SpeedTestPolicy, SpeedTestService, SpeedTestSnapshot},
    system_monitor::{RefreshOutcome, SystemMonitorService, SystemSnapshot},
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
}

const COMMAND_SURFACE_ID: &str = "app.command-surface";

const fn cost(idle: CostLevel, interaction: CostLevel, polling: CostLevel) -> ResourceCost {
    ResourceCost::new(idle, interaction, polling)
}

const COMMAND_SURFACE_COST: ResourceCost = cost(CostLevel::None, CostLevel::Low, CostLevel::None);
const TRAY_COST: ResourceCost = cost(CostLevel::Low, CostLevel::Moderate, CostLevel::None);
const fn quick_toggle_cost(id: QuickToggleId) -> ResourceCost {
    match id {
        QuickToggleId::KeepAwake => cost(CostLevel::Low, CostLevel::Low, CostLevel::None),
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
/// Stable feature identifier for the private scratchpad.
pub const SCRATCHPAD_ID: &str = "notes.scratchpad";
/// Stable feature identifier for the status overlay.
pub const OVERLAY_ID: &str = "overlay.status";

/// Capabilities only the GTK side can observe, reported to the runtime.
pub struct HostCapabilities {
    pub status_notifier: CapabilityReport,
    /// Whether a top-centre overlay surface can be placed.
    pub overlay: CapabilityReport,
}

pub struct ApplicationRuntime {
    registry: FeatureRegistry,
    audio_mixer: AudioMixerService<PulseAudioBackend>,
    microphone: MicrophoneService<PulseAudioBackend>,
    speed_test: SpeedTestService<CurlSpeedTestBackend>,
    capture: CaptureService,
    /// Detection shared with capability probes.
    capture_detection: Arc<Mutex<Option<CaptureDetection>>>,
    capture_limits: kestrel_core::CaptureConfiguration,
    /// Whether capture storage is available.
    capture_storage: bool,
    global_shortcuts: GlobalShortcutService,
    /// Known configured bindings.
    shortcut_requests: Vec<ShortcutRequest>,
    shortcut_bindings: Vec<kestrel_core::ShortcutBinding>,
    /// Unknown configured commands.
    shortcut_skipped: Vec<String>,
    /// One registration attempt per running period, so a declined portal
    /// dialog does not reappear on every refresh.
    shortcut_attempted: bool,
    /// Activations are sent as application commands.
    command_sender: Option<async_channel::Sender<ApplicationCommand>>,
    clipboard_history: ClipboardHistoryService,
    snippet_library: SnippetLibrary,
    snippet_service: SnippetInsertionService<ExecutableInsertionBackend>,
    snippet_policy: SnippetPolicy,
    snippet_expansion_timing: kestrel_core::SnippetExpansionTiming,
    snippet_provider_preference: kestrel_core::SnippetProviderPreference,
    snippet_path: Option<std::path::PathBuf>,
    snippet_warnings: Vec<ConfigurationWarning>,
    /// Read-only clipboard backend.
    session_clipboard: Option<ArboardClipboardBackend>,
    command_index: CommandIndex,
    command_ranking: CommandRanking,
    command_ranking_path: Option<std::path::PathBuf>,
    command_ranking_warnings: Vec<ConfigurationWarning>,
    command_applications: Vec<ApplicationEntry>,
    command_application_items: Vec<CommandItem>,
    command_providers: EnabledProviders,
    command_feature_items: Vec<CommandItem>,
    command_file_roots: Vec<std::path::PathBuf>,
    command_scripts: Vec<kestrel_core::CommandScriptConfiguration>,
    command_max_results: usize,
    command_launcher: Option<ExternalLauncher>,
    window_provider: Option<Box<dyn WindowProvider>>,
    window_provider_unavailable: Option<kestrel_platform::windows::WindowProviderUnavailable>,
    last_external_active: Option<WindowHandle>,
    system_monitor: SystemMonitorService<ProcSysMonitor>,
    process_inventory: ProcessInventoryService,
    listeners: ListenerService,
    process_limits: kestrel_core::ProcessConfiguration,
    /// `pidfd_open` works; checked once because the kernel cannot change.
    process_signalling: bool,
    /// Addresses fetched on request; dropped when hidden.
    local_addresses: Option<Result<Vec<kestrel_platform::network_addresses::LocalAddress>, String>>,
    /// This release's notes, shipped with the build.
    release_notes: Vec<Change>,
    /// Overview offers the notes until they are opened.
    whats_new_pending: bool,
    screen_tools: ScreenToolsService,
    /// Reads text from clipboard images with the screen-tools OCR settings.
    clipboard_ocr: Arc<dyn TextRecognizer>,
    camera: CameraPreviewService,
    recording: RecordingService,
    /// Detection shared with capability probes.
    recording_detection: Arc<Mutex<Option<RecordingDetection>>>,
    recording_settings: kestrel_core::RecordingConfiguration,
    /// Take editor for trim previews.
    recording_editor: Arc<dyn TakeEditor>,
    screen_tool_settings: kestrel_core::ScreenToolsConfiguration,
    alerts: AlertEngine,
    scratchpad: Option<ScratchpadService>,
    scratchpad_path: Option<PathBuf>,
    scratchpad_warning: Option<String>,
    scratchpad_policy: kestrel_core::ScratchpadConfiguration,
    quick_toggles: QuickToggleService,
}
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorTick {
    pub outcome: RefreshOutcome,
    pub alerts: Vec<AlertEvent>,
}

impl ApplicationRuntime {
    pub fn new(configuration: &ApplicationConfiguration) -> Result<Self, RegistryError> {
        Self::new_with_host(
            configuration,
            HostCapabilities {
                status_notifier: unavailable_capability(
                    "StatusNotifierItem registration was not attempted.",
                ),
                overlay: CapabilityReport::new(
                    OVERLAY_ID,
                    CapabilityStatus::Unsupported {
                        reason: "overlay placement was not checked".to_owned(),
                    },
                    "The status overlay needs a desktop session.",
                )
                .with_remediation("Kestrel's window, tray, and shortcuts work without it."),
            },
        )
    }

    /// Builds the runtime with what the host observed about the tray and overlay.
    pub fn new_with_host(
        configuration: &ApplicationConfiguration,
        host: HostCapabilities,
    ) -> Result<Self, RegistryError> {
        let HostCapabilities {
            status_notifier: status_notifier_capability,
            overlay: overlay_capability,
        } = host;
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
        // On unless turned off; it runs only where a tray host exists.
        let status_notifier_enabled = configuration
            .features
            .get(STATUS_NOTIFIER_ID)
            .is_none_or(|feature| feature.enabled);
        let status_notifier = FeatureSpec::new(
            STATUS_NOTIFIER_ID,
            "Tray integration",
            status_notifier_capability.status.clone(),
        )
        .with_cost(TRAY_COST);
        registry.register_probe(
            status_notifier,
            status_notifier_enabled,
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
        let capture_detection = Arc::new(Mutex::new(None));
        let capture = FeatureSpec::new(CAPTURE_ID, "Screenshots", CapabilityStatus::Supported)
            .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            capture,
            configuration.feature_enabled(CAPTURE_ID),
            SharedCaptureProbe(Arc::clone(&capture_detection)),
        )?;
        let camera = FeatureSpec::new(CAMERA_ID, "Camera preview", CapabilityStatus::Supported)
            .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            camera,
            configuration.feature_enabled(CAMERA_ID),
            CameraProbe,
        )?;
        let recording_detection = Arc::new(Mutex::new(None));
        let recording = FeatureSpec::new(
            RECORDING_ID,
            "Screen recording",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
        registry.register_probe(
            recording,
            configuration.feature_enabled(RECORDING_ID),
            SharedRecordingProbe(Arc::clone(&recording_detection)),
        )?;
        for tool in ALL_SCREEN_TOOLS {
            let spec =
                FeatureSpec::new(tool.feature_id(), tool.label(), CapabilityStatus::Supported)
                    .with_cost(cost(CostLevel::None, CostLevel::Moderate, CostLevel::None));
            registry.register_probe(
                spec,
                configuration.feature_enabled(tool.feature_id()),
                ScreenToolProbe {
                    tool,
                    detection: Arc::clone(&capture_detection),
                },
            )?;
        }
        let processes = FeatureSpec::new(PROCESSES_ID, "Processes", CapabilityStatus::Supported)
            .with_cost(cost(CostLevel::None, CostLevel::Low, CostLevel::Moderate));
        registry.register_probe(
            processes,
            configuration.feature_enabled(PROCESSES_ID),
            ProcessProbe::default(),
        )?;
        let listeners = FeatureSpec::new(
            LISTENERS_FEATURE_ID,
            "Listening ports",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::None, CostLevel::Low, CostLevel::Low));
        registry.register_probe(
            listeners,
            configuration.feature_enabled(LISTENERS_FEATURE_ID),
            ListenersProbe::default(),
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
        let scratchpad = FeatureSpec::new(SCRATCHPAD_ID, "Scratchpad", CapabilityStatus::Supported)
            .with_cost(cost(CostLevel::None, CostLevel::Low, CostLevel::None));
        registry.register_probe(
            scratchpad,
            configuration.feature_enabled(SCRATCHPAD_ID),
            StaticCapabilityProbe::new(CapabilityReport::new(
                SCRATCHPAD_ID,
                CapabilityStatus::Supported,
                "Private local scratchpad storage is available.",
            )),
        )?;
        let overlay = FeatureSpec::new(
            OVERLAY_ID,
            "Status overlay",
            overlay_capability.status.clone(),
        )
        .with_cost(cost(CostLevel::Low, CostLevel::Low, CostLevel::None));
        registry.register_probe(
            overlay,
            configuration.feature_enabled(OVERLAY_ID),
            StaticCapabilityProbe::new(overlay_capability),
        )?;
        let global_shortcuts = FeatureSpec::new(
            GLOBAL_SHORTCUTS_ID,
            "Global shortcuts",
            CapabilityStatus::Supported,
        )
        .with_cost(cost(CostLevel::None, CostLevel::Low, CostLevel::None));
        registry.register_probe(
            global_shortcuts,
            configuration.feature_enabled(GLOBAL_SHORTCUTS_ID),
            GlobalShortcutsProbe,
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
        let command_feature_items = Self::feature_items(&registry);
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
        let scratchpad_path = crate::snippets::kestrel_data_directory()
            .map(|directory| directory.join("scratchpad.json"));
        let scratchpad_service = scratchpad_path
            .clone()
            .filter(|_| configuration.feature_enabled(SCRATCHPAD_ID))
            .and_then(|path| ScratchpadService::open(path, configuration.scratchpad).ok());
        let command_launcher = ExternalLauncher::discover().ok();
        let command_ranking_warnings = loaded_ranking.warnings;
        let capture_directory = capture_directory();
        let (shortcut_requests, shortcut_skipped) =
            Self::shortcut_requests(&configuration.shortcuts);

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
            capture: CaptureService::new(
                CapturePolicy::from_configuration(&configuration.capture),
                capture_directory.clone().unwrap_or_default(),
            ),
            capture_detection,
            recording_detection,
            recording_settings: configuration.recording,
            recording_editor: Arc::new(GstTakeEditor),
            recording: RecordingService::new(
                Arc::new(PortalRecordingBackend::new()),
                Arc::new(GstTakeEditor),
                crate::config::recordings_path()
                    .unwrap_or_else(|| std::env::temp_dir().join("Kestrel recordings")),
            ),
            capture_limits: configuration.capture,
            capture_storage: capture_directory.is_some(),
            global_shortcuts: GlobalShortcutService::new(),
            shortcut_requests,
            shortcut_bindings: configuration.shortcuts.bindings.clone(),
            shortcut_skipped,
            shortcut_attempted: false,
            command_sender: None,
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
            command_feature_items,
            command_file_roots,
            command_scripts: configuration.command_bar.scripts.clone(),
            command_max_results,
            command_launcher,
            window_provider: None,
            window_provider_unavailable: None,
            last_external_active: None,
            system_monitor: SystemMonitorService::new(
                monitor_source,
                Duration::from_millis(configuration.monitoring.refresh_interval_millis),
                configuration.monitoring.history_samples,
            )
            .expect("the validated monitor configuration is valid"),
            process_inventory: ProcessInventoryService::new(ProcessPolicy::from_configuration(
                configuration.processes,
            )),
            listeners: ListenerService::new(),
            process_limits: configuration.processes,
            process_signalling: PidfdSignaller::available(),
            local_addresses: None,
            release_notes: whats_new::shipped_changes(),
            whats_new_pending: false,
            screen_tools: ScreenToolsService::new(ocr_recognizer(&configuration.screen_tools)),
            clipboard_ocr: ocr_recognizer(&configuration.screen_tools),
            camera: CameraPreviewService::new(Arc::new(V4l2Camera)),
            screen_tool_settings: configuration.screen_tools.clone(),
            alerts: AlertEngine::new(AlertPolicy::from_configuration(
                &configuration.monitoring.alerts,
            )),
            scratchpad_warning: None,
            scratchpad: scratchpad_service,
            scratchpad_path: scratchpad_path.clone(),
            scratchpad_policy: configuration.scratchpad,
            quick_toggles: QuickToggleService::new(quick_toggle_backend),
        };
        runtime.configure_audio_preferences(&configuration.audio);
        Ok(runtime)
    }

    pub fn start(&mut self) {
        self.registry.start_enabled();
        self.log_lifecycle(&[]);
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
        let before = self.feature_states();
        let feature_ids = self
            .registry
            .registrations()
            .map(|registration| registration.feature.id)
            .collect::<Vec<_>>();
        for feature_id in feature_ids {
            self.registry.refresh_capability(feature_id)?;
        }
        self.registry.start_enabled();
        self.log_lifecycle(&before);
        // A refresh retries a window list that was unavailable.
        self.window_provider_unavailable = None;
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
        let before = self.feature_states();
        self.screen_tool_settings = configuration.screen_tools.clone();
        self.recording_settings = configuration.recording;
        self.screen_tools
            .set_recognizer(ocr_recognizer(&configuration.screen_tools));
        self.clipboard_ocr = ocr_recognizer(&configuration.screen_tools);
        self.system_monitor
            .reconfigure(
                Duration::from_millis(configuration.monitoring.refresh_interval_millis),
                configuration.monitoring.history_samples,
            )
            .expect("the validated monitor configuration is valid");
        self.alerts.set_policy(AlertPolicy::from_configuration(
            &configuration.monitoring.alerts,
        ));
        self.audio_mixer
            .set_policy(AudioPolicy::from_configuration(&configuration.audio))
            .expect("the validated audio policy is valid");
        self.configure_audio_preferences(&configuration.audio);
        if self.audio_mixer_is_running() {
            let _ = self.audio_mixer.refresh();
        }
        if self.microphone_is_running() {
            let _ = self.microphone.refresh();
        }
        self.speed_test
            .set_policy(SpeedTestPolicy::from_configuration(
                &configuration.speed_test,
            ));
        self.capture
            .set_policy(CapturePolicy::from_configuration(&configuration.capture));
        self.capture_limits = configuration.capture;
        self.process_limits = configuration.processes;
        self.process_inventory
            .set_policy(ProcessPolicy::from_configuration(configuration.processes));
        let (requests, skipped) = Self::shortcut_requests(&configuration.shortcuts);
        if requests != self.shortcut_requests {
            self.global_shortcuts.stop();
            self.shortcut_attempted = false;
        }
        self.shortcut_requests = requests;
        self.shortcut_skipped = skipped;
        self.shortcut_bindings = configuration.shortcuts.bindings.clone();
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
        self.scratchpad_policy = configuration.scratchpad;
        if let Some(scratchpad) = self.scratchpad.as_mut() {
            scratchpad.set_policy(configuration.scratchpad);
        }
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
        self.log_lifecycle(&before);
        self.reconcile_resources();
        self.refresh_quick_toggles();
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
            // Presets choose features to use; the tray stays as the user left it.
            .filter(|registration| {
                registration.feature.configurable && registration.feature.id != STATUS_NOTIFIER_ID
            })
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
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }

    pub fn view_model_with_panels(
        &self,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
        clipboard: ClipboardQuery<'_>,
        snippets: SnippetQuery<'_>,
        command_bar: CommandBarQuery<'_>,
    ) -> ApplicationViewModel {
        let mut model = ApplicationViewModel::new(
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
            ShortcutsPresentation {
                status: self.global_shortcuts.snapshot(),
                available: self.global_shortcuts_available(),
                running: self.global_shortcuts_is_running(),
                configured: &self.shortcut_bindings,
                skipped: &self.shortcut_skipped,
            },
            self.capture_presentation(),
            ClipboardPresentation {
                snapshot: Some(self.clipboard_history.latest()),
                running: self.clipboard_history_is_running(),
                search_query: clipboard.query,
                matches: clipboard.matches,
                preview: clipboard.preview,
                ocr_unavailable: clipboard_ocr_unavailable(),
            },
            SnippetsPresentation {
                library: &self.snippet_library,
                policy: self.snippet_policy,
                running: self.snippets_running(),
                provider: self.snippet_provider(),
                unavailable_reason: self.snippet_unavailable_reason(),
                stopped_reason: self.snippets_stopped_reason(),
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
                windows_status: self
                    .window_provider
                    .as_ref()
                    .map(|provider| provider.backend().label().to_owned())
                    .or_else(|| {
                        self.window_provider_unavailable
                            .as_ref()
                            .map(ToString::to_string)
                    })
                    .unwrap_or_else(|| "Window provider unavailable".to_owned()),
                clipboard_status: if self.clipboard_history_is_running() {
                    "clipboard history running".to_owned()
                } else {
                    "clipboard history stopped".to_owned()
                },
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
            },
            warnings,
            configuration,
            can_undo,
        );
        model.monitor = model
            .monitor
            .with_local_addresses(self.local_addresses.as_ref());
        model.scratchpad = self.scratchpad_view_model();
        model.processes = self.processes_view_model("");
        model.screen_tools = self.screen_tools_view_model();
        model.camera = self.camera_view_model();
        model.recording = self.recording_view_model();
        model.listeners = self.listeners_view_model();
        model.apply_tool_actions();
        model.whats_new = WhatsNewViewModel::new(
            &self.release_notes,
            self.whats_new_pending,
            &model.features,
            configuration,
        );
        model
    }

    /// At startup: whether Overview offers this release's notes.
    pub fn start_whats_new(&mut self, configuration: &ApplicationConfiguration) {
        let path = whats_new::state_path();
        let last = path.as_deref().and_then(whats_new::read_last_version);
        let (pending, record) = whats_new::offer_after_update(
            last.as_deref(),
            whats_new::CURRENT_VERSION,
            &self.release_notes,
            configuration.startup.whats_new,
            |feature| whats_new::in_use(configuration, feature),
        );
        self.whats_new_pending = pending;
        if record {
            whats_new::record_current_version(path.as_deref());
        }
    }

    /// The notes were opened; Overview stops offering them.
    pub fn mark_whats_new_read(&mut self) {
        self.whats_new_pending = false;
        whats_new::record_current_version(whats_new::state_path().as_deref());
    }

    fn reconcile_resources(&mut self) {
        self.reconcile_window_provider();
        // Stop services to release claims and join workers.
        if !self.camera_is_running() {
            self.camera.stop();
        }
        if !self.recording_is_running() {
            // Completes a running take before the feature stops.
            self.recording.shutdown();
        }
        if ALL_SCREEN_TOOLS
            .into_iter()
            .any(|tool| self.screen_tool_is_running(tool))
        {
            if !self.screen_tools.is_running() {
                // A failed start leaves every job refusing with a message.
                let _ = self.screen_tools.start();
            }
        } else {
            self.screen_tools.stop();
        }
        if self.processes_is_running() {
            if !self.process_inventory.is_running() {
                self.process_inventory.start(ProcfsProcesses::new("/proc"));
            }
        } else {
            self.process_inventory.stop();
        }
        if !self.listeners_is_running() {
            self.listeners.clear();
        }
        if !self.microphone_is_running() {
            self.microphone.reset();
        }
        if !self.speed_test_is_running() {
            self.speed_test.stop();
        }
        if self.capture_is_running() {
            if !self.capture.is_started() && self.capture_storage {
                // A storage failure is reported through the snapshot.
                let _ = self.capture.start();
            }
        } else {
            self.capture.stop();
        }
        if self.global_shortcuts_is_running() {
            if !self.shortcut_attempted {
                self.start_global_shortcuts();
            }
        } else {
            self.global_shortcuts.stop();
            self.shortcut_attempted = false;
        }
        if self.clipboard_history_is_running() {
            if !self.clipboard_worker_is_running() {
                let _ = self.start_clipboard_history();
            }
        } else {
            self.clipboard_history.stop();
        }

        if self.scratchpad_is_running() {
            if self.scratchpad.is_none() {
                self.scratchpad = self
                    .scratchpad_path
                    .clone()
                    .and_then(|path| ScratchpadService::open(path, self.scratchpad_policy).ok());
                if self.scratchpad.is_some() {
                    self.scratchpad_warning = None;
                }
            }
        } else if let Some(mut service) = self.scratchpad.take() {
            if let Err(error) = service.stop() {
                self.scratchpad_warning = Some(error.to_string());
            }
        }
        if !self.quick_toggle_is_running(QuickToggleId::KeepAwake) {
            self.quick_toggles.stop(QuickToggleId::KeepAwake);
        }
    }

    /// The window list lives while the command bar runs with window results on;
    /// discovery is read-only and retried only after a capability refresh.
    fn reconcile_window_provider(&mut self) {
        if !self.command_bar_running() || !self.command_providers.windows {
            self.window_provider = None;
            self.window_provider_unavailable = None;
            return;
        }
        if self.window_provider.is_some() || self.window_provider_unavailable.is_some() {
            return;
        }
        match discover_window_provider() {
            Ok(provider) => self.window_provider = Some(provider),
            Err(unavailable) => self.window_provider_unavailable = Some(unavailable),
        }
    }

    fn default_enabled(feature_id: &str) -> bool {
        feature_id == STATUS_NOTIFIER_ID
            || ALL_QUICK_TOGGLES
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

mod audio;
mod capture;
mod clipboard;
mod command_bar;
mod lifecycle;
mod probes;
mod processes;
mod quick_toggles;
mod recording;
mod scratchpad;
mod shortcuts;
mod snippets;
mod system;
#[cfg(test)]
mod test_support;
mod window_decision;

use capture::capture_directory;
pub use probes::CommandBarProbe;
use probes::{
    ScreenToolProbe, SharedCaptureProbe, SharedRecordingProbe, clipboard_ocr_unavailable,
    ocr_recognizer,
};
pub use processes::run_process_action;
pub use window_decision::ApplicationWindowDecision;
use window_decision::{decide_application_window_action, is_kestrel_window};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::STATUS_NOTIFIER_ID;
    use kestrel_core::{ApplicationConfiguration, CapabilityReport, CapabilityStatus};
    use kestrel_platform::{
        audio::{FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID},
        clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
        quick_toggles::{ALL_QUICK_TOGGLES, KEEP_AWAKE_ID, SCREEN_LOCK_ID},
        system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID,
    };
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
        let switches = ApplicationRuntime::feature_items(&runtime.registry);
        let has_switch = |feature_id: &str| {
            switches
                .iter()
                .any(|item| item.id == format!("feature:{feature_id}"))
        };
        assert!(
            !has_switch("app.command-surface"),
            "the window is no switch"
        );
        assert!(has_switch(STATUS_NOTIFIER_ID), "the tray can be turned off");
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

        // Shipped notes name real features, and the ones in use lead.
        let notes = whats_new::shipped_changes();
        for change in &notes {
            assert!(
                change.feature == whats_new::APP_WIDE
                    || registrations
                        .iter()
                        .any(|registration| registration.feature.id == change.feature),
                "{} is a registered feature",
                change.feature
            );
        }
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled("capture.screenshot", true)
            .expect("valid feature");
        let features = registrations
            .iter()
            .map(|registration| crate::view_model::FeatureViewModel::from(*registration))
            .collect::<Vec<_>>();
        let offered = WhatsNewViewModel::new(&notes, true, &features, &configuration);
        let order = offered
            .sections
            .iter()
            .map(|section| (section.title.as_str(), section.in_use))
            .collect::<Vec<_>>();
        assert_eq!(order.first(), Some(&("Kestrel", true)));
        assert!(
            order.windows(2).all(|pair| pair[0].1 >= pair[1].1),
            "sections in use come first: {order:?}"
        );
        let screenshot = features
            .iter()
            .find(|feature| feature.id == "capture.screenshot")
            .expect("registered");
        assert!(order.contains(&(screenshot.label.as_str(), true)));
        assert!(offered.banner.is_some());
        assert!(
            WhatsNewViewModel::new(&notes, false, &features, &configuration)
                .banner
                .is_none()
        );
    }
    #[test]
    fn successful_status_notifier_registration_is_reported_as_running() {
        let report = CapabilityReport::new(
            STATUS_NOTIFIER_ID,
            CapabilityStatus::Supported,
            "The tray host accepted Kestrel.",
        )
        .with_selected_backend("StatusNotifierItem");
        let mut runtime = ApplicationRuntime::new_with_host(
            &ApplicationConfiguration::default(),
            HostCapabilities {
                status_notifier: report,
                overlay: CapabilityReport::new(
                    OVERLAY_ID,
                    CapabilityStatus::Supported,
                    "Layer-shell overlays are available.",
                ),
            },
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
        for registration in runtime.registrations().filter(|registration| {
            registration.feature.configurable && registration.feature.id != STATUS_NOTIFIER_ID
        }) {
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
        assert_eq!(
            configuration.features.get(STATUS_NOTIFIER_ID),
            None,
            "presets leave the tray as the user left it"
        );

        runtime
            .undo_preset(&mut configuration, snapshot)
            .expect("preset undo applies");

        assert_eq!(configuration.features, original_features);
    }
}
