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
        ProcessesViewModel, RecordingPresentation, RecordingViewModel, ScratchpadPresentation,
        ScratchpadViewModel, ScreenToolGate, ScreenToolsPresentation, ScreenToolsViewModel,
        ShortcutsPresentation, ShortcutsViewModel, SnippetQuery, SnippetsPresentation,
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
        ALL_QUICK_TOGGLES, LinuxQuickToggleBackend, QuickToggleCapabilityProbe, QuickToggleControl,
        QuickToggleError, QuickToggleId,
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
        Clock, ExecutableInsertionBackend, InsertionProvider, SnippetInsertionProbe, SystemClock,
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
    alerts::{AlertEngine, AlertEvent, AlertPolicy, AlertSnapshot},
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

struct SharedCaptureProbe(Arc<Mutex<Option<CaptureDetection>>>);

impl CapabilityProbe for SharedCaptureProbe {
    fn probe(&self) -> CapabilityReport {
        let detection = CaptureDetection::detect();
        let report = detection.capability();
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(detection);
        report
    }
}

struct SharedRecordingProbe(Arc<Mutex<Option<RecordingDetection>>>);

impl CapabilityProbe for SharedRecordingProbe {
    fn probe(&self) -> CapabilityReport {
        let detection = RecordingDetection::detect();
        let report = detection.capability();
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(detection);
        report
    }
}

struct ScreenToolProbe {
    tool: ScreenTool,
    detection: Arc<Mutex<Option<CaptureDetection>>>,
}

impl CapabilityProbe for ScreenToolProbe {
    fn probe(&self) -> CapabilityReport {
        // The capture probe registers first, so its detection is current.
        let mut detection = self
            .detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let detection = detection.get_or_insert_with(CaptureDetection::detect);
        screen_tool_capability(
            self.tool,
            detection,
            TesseractAdapter::discover().executable(),
        )
    }
}

/// Why text cannot be read from clipboard images, when Tesseract is missing.
fn clipboard_ocr_unavailable() -> Option<String> {
    TesseractAdapter::discover()
        .executable()
        .is_none()
        .then(|| {
            "Install Tesseract (the tesseract-ocr package) to copy text from images.".to_owned()
        })
}

fn ocr_recognizer(settings: &kestrel_core::ScreenToolsConfiguration) -> Arc<dyn TextRecognizer> {
    Arc::new(TesseractRecognizer {
        adapter: TesseractAdapter::discover(),
        language: settings.ocr_language.clone(),
        timeout: Duration::from_secs(u64::from(settings.ocr_timeout_seconds)),
    })
}

fn capture_directory() -> Option<PathBuf> {
    crate::snippets::kestrel_data_directory().map(|directory| directory.join("captures"))
}

/// Stable feature identifier for the snippet library.
pub const SNIPPETS_ID: &str = "snippets.text";
/// The stable feature identifier for the command bar.
pub const COMMAND_BAR_ID: &str = "commands.bar";
/// Stable feature identifier for the private scratchpad.
pub const SCRATCHPAD_ID: &str = "notes.scratchpad";

/// Reports available providers while preserving portable commands.
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

/// What running an application result does, given its open windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationWindowDecision {
    Hide(WindowHandle),
    Restore(WindowHandle),
    Launch,
}

/// Kestrel's own windows never appear as results or count as the front window.
fn is_kestrel_window(app_id: Option<&str>) -> bool {
    app_id.is_some_and(|app_id| {
        app_id.eq_ignore_ascii_case(kestrel_core::APPLICATION_ID)
            || app_id.eq_ignore_ascii_case("kestrel")
    })
}

fn normalized_window_id(value: &str) -> String {
    value
        .strip_suffix(".desktop")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

pub fn decide_application_window_action(
    windows: &[WindowInfo],
    last_external_active: Option<WindowHandle>,
    application: &ApplicationEntry,
) -> ApplicationWindowDecision {
    let desktop_id = normalized_window_id(&application.id);
    let startup_class = application
        .startup_wm_class
        .as_deref()
        .map(normalized_window_id);
    let matched = windows.iter().find(|window| {
        window.app_id.as_deref().is_some_and(|app_id| {
            let app_id = normalized_window_id(app_id);
            app_id == desktop_id || startup_class.as_deref() == Some(app_id.as_str())
        })
    });
    let Some(window) = matched else {
        return ApplicationWindowDecision::Launch;
    };
    if window.minimized {
        ApplicationWindowDecision::Restore(window.handle)
    } else if last_external_active == Some(window.handle) || window.active {
        ApplicationWindowDecision::Hide(window.handle)
    } else {
        ApplicationWindowDecision::Restore(window.handle)
    }
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
    /// Quick toggle may narrow battery alerts.
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
            battery_alert_configured: configuration.monitoring.alerts.battery.enabled,
            quick_toggles: QuickToggleService::new(quick_toggle_backend),
        };
        runtime.configure_audio_preferences(&configuration.audio);
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
        self.battery_alert_configured = configuration.monitoring.alerts.battery.enabled;
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
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }

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
                policy: Some(self.alerts.policy()),
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
        model
    }

    pub fn scratchpad_view_model(&self) -> ScratchpadViewModel {
        ScratchpadViewModel::from_presentation(ScratchpadPresentation {
            running: self.scratchpad_is_running(),
            warning: self.scratchpad_warning.clone(),
            snapshot: self.scratchpad_snapshot(SystemTime::now()),
            limits: self.scratchpad_policy,
        })
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
        .with_local_addresses(self.local_addresses.as_ref())
    }

    /// Reads local addresses on an explicit request, or forgets them.
    pub fn set_local_addresses_shown(&mut self, shown: bool) {
        self.local_addresses = shown.then(|| {
            kestrel_platform::network_addresses::local_addresses()
                .map_err(|error| format!("Local addresses could not be read: {error}"))
        });
    }

    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }

    /// Returns audio state, including unavailable/empty states.
    pub fn audio_snapshot(&self) -> &AudioSnapshot {
        self.audio_mixer.latest()
    }

    /// Re-reads the mixer, applying the output ranking to arrivals and
    /// losses; true when anything changed.
    pub fn refresh_audio_mixer(&mut self) -> bool {
        if !self.audio_mixer_is_running() {
            return false;
        }
        let before = self.audio_mixer.latest().clone();
        let _ = self.audio_mixer.refresh();
        self.audio_mixer.latest() != &before
    }

    fn configure_audio_preferences(&mut self, audio: &kestrel_core::AudioConfiguration) {
        self.audio_mixer.set_priority(DevicePriority {
            ranked: audio.output_priority.clone(),
            follow: audio.follow_output_priority,
        });
        self.audio_mixer
            .set_favorites(audio.mixer_favorites.clone());
        self.microphone.set_priority(DevicePriority {
            ranked: audio.input_priority.clone(),
            follow: audio.follow_input_priority,
        });
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
                ocr_unavailable: clipboard_ocr_unavailable(),
            },
        )
    }

    /// Reads the text in an image entry and puts it on the clipboard without
    /// retaining it; returns the number of characters copied.
    pub fn copy_clipboard_image_text(&self, item_id: u64) -> Result<usize, String> {
        if !self.clipboard_history_is_running() {
            return Err("Clipboard history is not running".to_owned());
        }
        if let Some(reason) = clipboard_ocr_unavailable() {
            return Err(reason);
        }
        let png = self
            .clipboard_history
            .image_png(item_id)
            .map_err(|error| format!("The image could not be read: {error}"))?;
        let text = self
            .clipboard_ocr
            .recognize(
                png.expose(),
                &kestrel_platform::capture::CaptureCancel::new(),
            )
            .map_err(|error| format!("Text recognition failed: {}", error.message))?;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err("No text was found in the image".to_owned());
        }
        let characters = text.chars().count();
        self.clipboard_history
            .execute(ClipboardCommand::CopyUnstoredText(
                kestrel_services::clipboard::Unlogged::new(text),
            ))
            .map_err(|error| format!("The text could not be copied: {error}"))?;
        Ok(characters)
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

    pub fn microphone_view_model(
        &self,
        audio: &kestrel_core::AudioConfiguration,
    ) -> MicrophoneViewModel {
        MicrophoneViewModel::from_presentation(
            audio,
            MicrophonePresentation {
                snapshot: self.microphone.latest(),
                running: self.microphone_is_running(),
            },
        )
    }

    pub fn audio_view_model(&self, audio: &kestrel_core::AudioConfiguration) -> AudioViewModel {
        AudioViewModel::from_presentation(
            audio,
            AudioPresentation {
                snapshot: Some(self.audio_mixer.latest()),
                running: self.audio_mixer_is_running(),
                policy: self.audio_mixer.policy(),
            },
        )
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

    pub fn capture_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == CAPTURE_ID && registration.running)
    }
    pub fn processes_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == PROCESSES_ID && registration.running)
    }

    fn require_processes(&self) -> Result<(), String> {
        self.processes_is_running().then_some(()).ok_or_else(|| {
            "Processes are not running; enable system.processes in the Feature Hub.".to_owned()
        })
    }

    pub fn listeners_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == LISTENERS_FEATURE_ID && registration.running
        })
    }

    fn require_listeners(&self) -> Result<(), String> {
        self.listeners_is_running().then_some(()).ok_or_else(|| {
            "Listening ports are not running; enable network.listeners in the Feature Hub."
                .to_owned()
        })
    }

    fn require_capture(&self) -> Result<(), String> {
        if !self.capture_is_running() {
            return Err(
                "Screenshots are not running; enable capture.screenshot in the Feature Hub."
                    .to_owned(),
            );
        }
        if !self.capture_storage {
            return Err(
                "No data directory is available for captures; set XDG_DATA_HOME or HOME."
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn capture_provider(&self) -> (Option<CaptureProvider>, Vec<CaptureMode>) {
        self.capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|detection| (detection.provider(), detection.modes()))
            .unwrap_or_default()
    }

    /// Starts a capture; `None` picks the provider's preferred mode.
    pub fn begin_capture(&mut self, mode: Option<CaptureMode>) -> Result<(), String> {
        self.require_capture()?;
        let detection = self
            .capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                "Screenshot providers have not been probed yet; refresh capabilities.".to_owned()
            })?;
        let provider = detection.provider().ok_or_else(|| {
            "No screenshot provider is available; the Feature Hub lists setup steps.".to_owned()
        })?;
        let mode = match mode {
            Some(mode) => mode,
            None => *detection
                .modes()
                .first()
                .ok_or_else(|| "The screenshot provider offers no capture mode.".to_owned())?,
        };
        self.capture
            .begin(capture_backend_for(provider, &detection), mode)
            .map_err(|error| error.to_string())
    }

    pub fn cancel_capture(&self) {
        self.capture.cancel();
    }

    pub fn delete_capture(&mut self, id: CaptureId) -> Result<(), String> {
        self.require_capture()?;
        self.capture.delete(id);
        Ok(())
    }

    pub fn clear_captures(&mut self) -> Result<(), String> {
        self.require_capture()?;
        self.capture.clear().map_err(|error| error.to_string())
    }

    pub fn capture_png(&self, id: CaptureId) -> Result<Vec<u8>, String> {
        self.require_capture()?;
        self.capture.read_png(id).map_err(|error| error.to_string())
    }

    pub fn export_capture(&self, id: CaptureId, destination: &Path) -> Result<(), String> {
        self.require_capture()?;
        self.capture
            .export(id, destination)
            .map_err(|error| error.to_string())
    }

    /// Stores the edited image as a new capture; the source stays.
    pub fn save_capture_edit(&mut self, id: CaptureId, plan: &EditPlan) -> Result<(), String> {
        self.require_capture()?;
        self.capture
            .save_edit(id, plan)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Applies the age bound; cheap when nothing expired.
    pub fn prune_captures(&mut self) {
        if self.capture_is_running() {
            self.capture.prune();
        }
    }

    pub fn capture_snapshot(&self) -> CaptureSnapshot {
        self.capture.snapshot()
    }

    pub fn process_snapshot(&self) -> ProcessSnapshot {
        self.process_inventory.snapshot()
    }

    pub fn processes_view_model(&self, query: &str) -> ProcessesViewModel {
        ProcessesViewModel::from_presentation(crate::view_model::ProcessesPresentation {
            snapshot: self.process_snapshot(),
            running: self.processes_is_running(),
            query: query.to_owned(),
            current_uid: current_uid(),
            own_pid: std::process::id(),
            limits: self.process_limits,
            signalling: self.process_signalling,
        })
    }

    pub fn refresh_processes(&self) -> Result<(), String> {
        self.require_processes()?;
        self.process_inventory.refresh_now();
        Ok(())
    }

    pub fn listeners_view_model(&self) -> ListenersViewModel {
        ListenersViewModel::from_presentation(crate::view_model::ListenersPresentation {
            snapshot: self.listeners.snapshot(),
            running: self.listeners_is_running(),
            signalling: self.process_signalling,
        })
    }

    pub fn listener_generation(&self) -> u64 {
        self.listeners.generation()
    }

    pub fn refresh_listeners(&mut self) -> Result<(), String> {
        self.require_listeners()?;
        self.listeners.refresh(
            &ProcfsListeners::new("/proc"),
            kestrel_platform::processes::MAX_LISTENERS,
        );
        Ok(())
    }

    pub fn process_confirmation(
        &self,
        identity: kestrel_platform::processes::ProcessIdentity,
        action: crate::ProcessActionKind,
    ) -> Result<ProcessConfirmation, String> {
        self.require_processes()?;
        if !self.process_signalling {
            return Err(kestrel_platform::processes::NO_PIDFD_EXPLANATION.to_owned());
        }
        let source = ProcfsProcesses::new("/proc");
        let sample = source.sample(identity).map_err(describe_sample_error)?;
        ProcfsProcesses::eligibility(&sample, current_uid(), std::process::id())
            .map_err(|reason| reason.explanation().to_owned())?;
        let (scope, can_escalate) = match action {
            crate::ProcessActionKind::Terminate => {
                (confirmation_scope(&sample, ConfirmAction::Terminate), false)
            }
            crate::ProcessActionKind::Kill => {
                (confirmation_scope(&sample, ConfirmAction::Kill), false)
            }
            crate::ProcessActionKind::Restart { .. } => {
                let target = ProcfsProcesses::restart_target(&sample)
                    .ok_or_else(|| "This process has no supported restart target.".to_owned())?;
                let members = source
                    .scope_members(identity)
                    .map_err(describe_sample_error)?
                    .len();
                let can_escalate = matches!(target, RestartTarget::DesktopApp { .. });
                (
                    confirmation_scope(&sample, ConfirmAction::Restart(&target, members)),
                    can_escalate,
                )
            }
        };
        Ok(ProcessConfirmation {
            scope,
            can_escalate,
        })
    }
}

/// Text for a process action confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessConfirmation {
    /// Executable, PID, owner, and signal or restart scope.
    pub scope: String,
    /// A desktop restart may send SIGKILL to members still running.
    pub can_escalate: bool,
}

fn describe_sample_error(error: kestrel_platform::processes::ProcessError) -> String {
    use kestrel_platform::processes::ProcessErrorKind;
    match error.kind {
        ProcessErrorKind::Reused => ActionOutcome::Reused,
        ProcessErrorKind::Exited => ActionOutcome::AlreadyExited,
        _ => ActionOutcome::Failed(error),
    }
    .to_string()
}

/// Real UID of this process; actions apply only to its processes.
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Runs a confirmed process action. Callers hold no runtime lock: a
/// termination waits up to its grace period.
pub fn run_process_action(
    identity: kestrel_platform::processes::ProcessIdentity,
    action: crate::ProcessActionKind,
) -> String {
    let source = ProcfsProcesses::new("/proc");
    let signaller = PidfdSignaller::for_current_user(ProcfsProcesses::new("/proc"));
    let own_pid = std::process::id();
    let uid = current_uid();
    let eligibility = |sample: &kestrel_platform::processes::ProcessSample| {
        ProcfsProcesses::eligibility(sample, uid, own_pid)
    };
    let outcome = match action {
        crate::ProcessActionKind::Terminate => terminate(
            &source,
            &signaller,
            eligibility,
            identity,
            PROCESS_TERMINATE_GRACE,
        ),
        crate::ProcessActionKind::Kill => kill(&source, &signaller, eligibility, identity),
        crate::ProcessActionKind::Restart { escalate } => restart(
            &source,
            &signaller,
            &SystemRestarter::new(),
            eligibility,
            ProcfsProcesses::restart_target,
            identity,
            PROCESS_STOP_TIMEOUT,
            escalate,
        ),
    };
    outcome.to_string()
}

const PROCESS_TERMINATE_GRACE: Duration = Duration::from_secs(3);
const PROCESS_STOP_TIMEOUT: Duration = Duration::from_secs(5);

impl ApplicationRuntime {
    pub fn camera_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == CAMERA_ID && registration.running)
    }

    pub fn camera_view_model(&self) -> CameraViewModel {
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == CAMERA_ID);
        let running = registration.is_some_and(|registration| registration.running);
        let reason = registration.filter(|_| !running).map(|registration| {
            if registration.enabled {
                registration
                    .capability
                    .remediation
                    .clone()
                    .unwrap_or_else(|| registration.capability.summary.clone())
            } else {
                "Camera preview is off. Enable camera.preview in the Feature Hub.".to_owned()
            }
        });
        CameraViewModel::from_presentation(CameraPresentation {
            running,
            reason,
            snapshot: self.camera.snapshot(),
            cameras: if running {
                system_cameras()
            } else {
                Vec::new()
            },
        })
    }

    pub fn camera_generation(&self) -> u64 {
        self.camera.snapshot().generation
    }

    /// The slot the window reads preview frames from.
    pub fn camera_frames(&self) -> CameraFrameSlot {
        self.camera.frames()
    }

    /// Opens the camera at `path`; only listed cameras are accepted.
    pub fn start_camera(&mut self, path: &std::path::Path) -> Result<(), String> {
        if !self.camera_is_running() {
            return Err(
                "Camera preview is not running; enable camera.preview in the Feature Hub."
                    .to_owned(),
            );
        }
        let camera = system_cameras()
            .into_iter()
            .find(|camera| camera.path == path)
            .ok_or_else(|| "That camera is no longer connected; refresh the list.".to_owned())?;
        self.camera.start(camera.path, camera.name)
    }

    pub fn stop_camera(&mut self) {
        self.camera.stop();
    }

    pub fn recording_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == RECORDING_ID && registration.running)
    }

    pub fn recording_generation(&self) -> u64 {
        self.recording.snapshot().generation
    }

    pub fn recording_view_model(&self) -> RecordingViewModel {
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == RECORDING_ID);
        let running = registration.is_some_and(|registration| registration.running);
        let reason = registration.filter(|_| !running).map(|registration| {
            if registration.enabled {
                registration
                    .capability
                    .remediation
                    .clone()
                    .unwrap_or_else(|| registration.capability.summary.clone())
            } else {
                "Screen recording is off. Enable capture.recording in the Feature Hub.".to_owned()
            }
        });
        let detection = self
            .recording_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        RecordingViewModel::from_presentation(RecordingPresentation {
            running,
            reason,
            snapshot: self.recording.snapshot(),
            sources: detection
                .as_ref()
                .map(RecordingDetection::sources)
                .unwrap_or_default(),
            available_tracks: AudioTrack::ALL
                .into_iter()
                .filter(|track| {
                    detection
                        .as_ref()
                        .is_some_and(|detection| detection.track_available(*track))
                })
                .collect(),
            cursor_available: detection
                .as_ref()
                .is_some_and(RecordingDetection::cursor_available),
            settings: self.recording_settings,
            folder: self.recording.folder().to_path_buf(),
        })
    }

    /// The frame to choose an area from, while one is being chosen.
    pub fn recording_area_frame(&self) -> Option<Arc<kestrel_services::capture::image::RgbaImage>> {
        match self.recording.snapshot().phase {
            RecordingPhase::ChoosingArea { frame } => Some(frame),
            _ => None,
        }
    }

    pub fn recording_request(&mut self, request: crate::RecordingRequest) -> Result<(), String> {
        use crate::RecordingRequest;
        if !self.recording_is_running() {
            return Err(
                "Screen recording is not running; enable capture.recording in the Feature Hub."
                    .to_owned(),
            );
        }
        match request {
            RecordingRequest::Start(source) => {
                let detection = self
                    .recording_detection
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| "Recording support has not been checked yet.".to_owned())?;
                if !detection.sources().contains(&source) {
                    return Err(format!(
                        "This desktop cannot record a {}.",
                        source.label().to_lowercase()
                    ));
                }
                let settings = self.recording_settings;
                let audio = AudioTrack::ALL
                    .into_iter()
                    .filter(|track| match track {
                        AudioTrack::System => settings.system_audio,
                        AudioTrack::Microphone => settings.microphone,
                    })
                    .filter(|track| detection.track_available(*track))
                    .collect();
                self.recording.start(RecordingOptions {
                    source,
                    cursor: settings.cursor && detection.cursor_available(),
                    framerate: settings.framerate,
                    audio,
                })
            }
            RecordingRequest::ChooseArea(Some(region)) => self.recording.choose_area(Some(region)),
            RecordingRequest::ChooseArea(None) => self.recording.record_whole_frame(),
            RecordingRequest::Pause => self.recording.pause(),
            RecordingRequest::Resume => self.recording.resume(),
            RecordingRequest::Stop => self.recording.stop(),
            RecordingRequest::Cancel => self.recording.cancel(),
            RecordingRequest::Dismiss => {
                self.recording.dismiss();
                Ok(())
            }
            RecordingRequest::Delete(path) => self.recording.delete_take(&path),
            RecordingRequest::Export {
                take,
                start,
                end,
                destination,
            } => self.recording.export(&take, start, end, destination),
            RecordingRequest::CancelExport => {
                self.recording.cancel_export();
                Ok(())
            }
            RecordingRequest::DismissExport => {
                self.recording.dismiss_export();
                Ok(())
            }
        }
    }

    /// The length of a listed take and a preview function over it, for the
    /// trim editor.
    pub fn recording_trim(
        &self,
        take: &Path,
    ) -> Result<(std::time::Duration, crate::TrimPreview), String> {
        if !self.recording_is_running() {
            return Err(
                "Screen recording is not running; enable capture.recording in the Feature Hub."
                    .to_owned(),
            );
        }
        let duration = self.recording.take_duration(take)?;
        let editor = Arc::clone(&self.recording_editor);
        let take = take.to_path_buf();
        let preview: crate::TrimPreview = Arc::new(move |position| {
            let frame = editor
                .frame_at(&take, position)
                .map_err(|error| error.message)?;
            kestrel_services::capture::image::RgbaImage::new(
                frame.width,
                frame.height,
                frame.pixels,
            )
            .map_err(|_| "The picture is too large to preview.".to_owned())
        });
        Ok((duration, preview))
    }
}

impl ApplicationRuntime {
    pub fn screen_tool_is_running(&self, tool: ScreenTool) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == tool.feature_id() && registration.running
        })
    }

    pub fn screen_tools_snapshot(&self) -> ScreenToolsSnapshot {
        self.screen_tools.snapshot()
    }

    pub fn screen_tools_view_model(&self) -> ScreenToolsViewModel {
        let gates = ALL_SCREEN_TOOLS
            .into_iter()
            .map(|tool| {
                let registration = self
                    .registry
                    .registrations()
                    .find(|registration| registration.feature.id == tool.feature_id());
                let running = registration.is_some_and(|registration| registration.running);
                let reason = match registration {
                    _ if running => None,
                    Some(registration) if !registration.enabled => {
                        Some(format!("Enable {} in the Feature Hub.", tool.feature_id()))
                    }
                    Some(registration) => Some(
                        registration
                            .capability
                            .remediation
                            .clone()
                            .unwrap_or_else(|| registration.capability.summary.clone()),
                    ),
                    None => Some(format!("{} is not registered.", tool.feature_id())),
                };
                ScreenToolGate {
                    tool,
                    running,
                    reason,
                }
            })
            .collect();
        ScreenToolsViewModel::from_presentation(ScreenToolsPresentation {
            snapshot: self.screen_tools.snapshot(),
            gates,
            settings: self.screen_tool_settings.clone(),
        })
    }

    /// Captures a frame for `tool`, or opens the desktop's color picker.
    pub fn begin_screen_tool(&self, tool: ScreenTool) -> Result<(), String> {
        if !self.screen_tool_is_running(tool) {
            return Err(format!(
                "{} is not running; enable {} in the Feature Hub.",
                tool.label(),
                tool.feature_id()
            ));
        }
        let detection = self
            .capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                "Screenshot providers have not been probed yet; refresh capabilities.".to_owned()
            })?;
        let source = match (tool, color_source(&detection)) {
            (ScreenTool::Color, Some(ColorSource::Picker)) => {
                ScreenToolSource::Picker(Box::new(PortalColorPicker::new()))
            }
            _ => {
                let provider = detection.provider().ok_or_else(|| {
                    "No screenshot provider is available; the Feature Hub lists setup steps."
                        .to_owned()
                })?;
                let modes = detection.modes();
                // Prefer a provider-side selection; the selection view refines it.
                let mode = [CaptureMode::Area, CaptureMode::Interactive]
                    .into_iter()
                    .find(|mode| modes.contains(mode))
                    .unwrap_or(CaptureMode::Screen);
                ScreenToolSource::Frame {
                    backend: capture_backend_for(provider, &detection),
                    mode,
                }
            }
        };
        self.screen_tools.begin(tool, source)
    }

    /// The frame waiting in the selection view.
    pub fn screen_tool_frame(&self) -> Option<Arc<kestrel_services::capture::RgbaImage>> {
        self.screen_tools.frame()
    }

    pub fn select_screen_region(&self, selection: Selection) -> Result<(), String> {
        self.screen_tools.select(selection)
    }

    pub fn cancel_screen_tool(&self) {
        self.screen_tools.cancel();
    }

    pub fn dismiss_screen_tool(&self) {
        self.screen_tools.dismiss();
    }
}

impl ApplicationRuntime {
    pub fn capture_view_model(&self) -> CaptureViewModel {
        CaptureViewModel::from_presentation(self.capture_presentation())
    }

    fn capture_presentation(&self) -> CapturePresentation {
        let snapshot = self.capture.snapshot();
        let thumbnails = snapshot
            .entries
            .iter()
            .map(|entry| self.capture.thumbnail_path(entry.id))
            .collect();
        let (provider, modes) = self.capture_provider();
        CapturePresentation {
            snapshot,
            running: self.capture_is_running(),
            provider,
            modes,
            thumbnails,
            limits: self.capture_limits,
            now: SystemTime::now(),
        }
    }

    /// Why `action` cannot run now, judged from its feature's registration.
    pub fn command_gate(&self, action: CommandLineAction) -> Option<CommandGate> {
        let feature_id = action.required_feature()?;
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == feature_id)?;
        let feature = format!("{} ({feature_id})", registration.feature.label);
        if !registration.enabled {
            Some(CommandGate::Disabled { feature })
        } else if !registration.available {
            Some(CommandGate::Unavailable {
                feature,
                summary: registration.capability.summary.clone(),
                remediation: registration.capability.remediation.clone(),
            })
        } else if !registration.running {
            Some(CommandGate::Stopped { feature })
        } else {
            None
        }
    }

    /// Routes shortcut activations to the application command channel.
    pub fn set_command_sender(&mut self, sender: async_channel::Sender<ApplicationCommand>) {
        self.command_sender = Some(sender);
    }

    pub fn global_shortcuts_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == GLOBAL_SHORTCUTS_ID && registration.running
        })
    }

    pub fn shortcut_status(&self) -> ShortcutStatus {
        self.global_shortcuts.snapshot()
    }

    pub fn shortcuts_view_model(&self) -> ShortcutsViewModel {
        ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: self.global_shortcuts.snapshot(),
            running: self.global_shortcuts_is_running(),
            configured: &self.shortcut_bindings,
            skipped: &self.shortcut_skipped,
        })
    }

    fn shortcut_requests(
        configuration: &kestrel_core::ShortcutConfiguration,
    ) -> (Vec<ShortcutRequest>, Vec<String>) {
        let mut requests = Vec::new();
        let mut skipped = Vec::new();
        for binding in &configuration.bindings {
            match (
                CommandLineAction::parse(&binding.command),
                binding.validate(),
            ) {
                (Ok(action), Ok(trigger)) => requests.push(ShortcutRequest {
                    id: binding.command.clone(),
                    description: action.description(),
                    trigger,
                }),
                _ => skipped.push(binding.command.clone()),
            }
        }
        (requests, skipped)
    }

    fn start_global_shortcuts(&mut self) {
        let Some(sender) = self.command_sender.clone() else {
            return;
        };
        self.shortcut_attempted = true;
        let Some(provider) = ShortcutDetection::detect().provider() else {
            return;
        };
        let sink: ActivationSink = Arc::new(move |id: &str| {
            if let Ok(action) = CommandLineAction::parse(id) {
                let _ = sender.try_send(action.to_command());
            }
        });
        let _ = self.global_shortcuts.start(
            shortcut_backend_for(provider),
            self.shortcut_requests.clone(),
            sink,
        );
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
        let mut items = Vec::new();
        for (index, application) in applications.iter().enumerate() {
            items.push(CommandItem::new(
                format!("application:{}", application.id),
                CommandSource::Application,
                application.name.clone(),
                application
                    .comment
                    .clone()
                    .unwrap_or_else(|| "Application".to_string()),
                kestrel_services::command_bar::CommandAction::OpenApplication { index },
            ));
            for action in &application.actions {
                items.push(CommandItem::new(
                    format!("app-action:{}:{}", application.id, action.id),
                    CommandSource::AppAction,
                    format!("{}: {}", application.name, action.name),
                    "Desktop application action",
                    kestrel_services::command_bar::CommandAction::OpenApplicationAction {
                        index,
                        action_id: action.id.clone(),
                    },
                ));
            }
        }
        items
    }

    fn feature_items(registry: &FeatureRegistry) -> Vec<CommandItem> {
        registry
            .registrations()
            .map(|registration| {
                let enabled = registration.enabled;
                let label = registration.feature.label.to_string();
                CommandItem::new(
                    format!("feature:{}", registration.feature.id),
                    CommandSource::Feature,
                    format!("Turn {} {label}", if enabled { "off" } else { "on" }),
                    format!("Feature {}", registration.feature.id),
                    kestrel_services::command_bar::CommandAction::SetFeatureEnabled {
                        feature_id: registration.feature.id.to_owned(),
                        enabled: !enabled,
                    },
                )
                .with_keywords([
                    "feature",
                    registration.feature.id,
                    registration.feature.label,
                ])
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
            windows: configuration.command_bar.enable_windows,
            clipboard: configuration.command_bar.enable_clipboard,
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
        self.command_feature_items = Self::feature_items(&self.registry);
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

    /// Gates effects while stopped, keeping stale requests inactive.
    fn require_command_bar(&self) -> Result<(), String> {
        if self.command_bar_running() {
            Ok(())
        } else {
            Err("the command bar is not running; enable commands.bar in the Feature Hub".to_owned())
        }
    }

    /// Records the front window before the bar takes focus; compositors may
    /// stop reporting it as active while an overlay holds the keyboard.
    pub fn remember_front_window(&mut self) {
        let Some(provider) = self.window_provider.as_mut() else {
            return;
        };
        let Ok(windows) = provider.windows() else {
            return;
        };
        if let Some(front) = windows
            .iter()
            .find(|window| window.active && !is_kestrel_window(window.app_id.as_deref()))
        {
            self.last_external_active = Some(front.handle);
        }
    }

    fn window_items(&mut self) -> Vec<CommandItem> {
        let Some(provider) = self.window_provider.as_mut() else {
            return Vec::new();
        };
        let backend = provider.backend();
        let snapshot = match provider.windows() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // The connection is not reused; a capability refresh reconnects.
                self.window_provider = None;
                self.window_provider_unavailable =
                    Some(kestrel_platform::windows::WindowProviderUnavailable {
                        summary: format!("The window list stopped responding: {error}"),
                        remediation: "Refresh capabilities to reconnect.".to_owned(),
                    });
                return Vec::new();
            }
        };
        let mut items = Vec::new();
        for window in snapshot {
            if is_kestrel_window(window.app_id.as_deref()) {
                continue;
            }
            if window.active {
                self.last_external_active = Some(window.handle);
            }
            let app = window
                .app_id
                .clone()
                .unwrap_or_else(|| "Unknown app".to_owned());
            items.push(CommandItem::new(
                format!("window:{}", window.handle.0),
                CommandSource::Window,
                window.title,
                format!("{app} · {}", backend.label()),
                kestrel_services::command_bar::CommandAction::ActivateWindow(window.handle.0),
            ));
        }
        items
    }

    fn clipboard_items(&self, query: &str) -> Vec<CommandItem> {
        let Some(Ok(matches)) = self.clipboard_search(query, 8) else {
            return Vec::new();
        };
        matches
            .into_iter()
            .map(|entry| {
                let preview = entry
                    .preview
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect::<String>();
                CommandItem::new(
                    format!("clipboard:{}", entry.id),
                    CommandSource::Clipboard,
                    preview,
                    format!("Clipboard entry {}", entry.id),
                    kestrel_services::command_bar::CommandAction::CopyClipboardEntry {
                        item_id: entry.id,
                    },
                )
            })
            .collect()
    }

    /// Returns no results when stopped; snippets follow their own lifecycle.
    pub fn command_search(
        &mut self,
        query: &str,
        now: &kestrel_platform::snippets::LocalTime,
    ) -> Vec<CommandResult> {
        if !self.command_bar_running() {
            return Vec::new();
        }
        let providers = EnabledProviders {
            snippets: self.snippets_running(),
            ..self.command_providers
        };
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
        let windows = if providers.windows {
            self.window_items()
        } else {
            Vec::new()
        };
        let clipboard = if providers.clipboard {
            self.clipboard_items(query)
        } else {
            Vec::new()
        };
        self.command_index.search(SearchInput {
            query,
            max_results: self.command_max_results,
            providers,
            applications: &self.command_application_items,
            files: &files,
            windows: &windows,
            clipboard: &clipboard,
            features: &self.command_feature_items,
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

    pub fn activate_command_window(&mut self, handle: u64) -> Result<(), String> {
        self.require_command_bar()?;
        let provider = self
            .window_provider
            .as_mut()
            .ok_or_else(|| "window provider is unavailable".to_owned())?;
        let backend = provider.backend();
        provider
            .activate(WindowHandle(handle))
            .map_err(|error| match error.kind {
                WindowErrorKind::Unsupported => {
                    format!("{} cannot activate windows", backend.label())
                }
                _ => error.to_string(),
            })
    }

    pub fn launch_command_application_action(
        &self,
        index: usize,
        action_id: &str,
    ) -> Result<(), String> {
        self.require_command_bar()?;
        let application = self
            .command_applications
            .get(index)
            .ok_or_else(|| format!("application {index} is no longer available"))?;
        launch_desktop_action(application, action_id).map_err(|error| error.to_string())
    }

    /// Hides the application when it is in front, restores it when it has a
    /// window, and launches it otherwise.
    pub fn launch_command_application(
        &mut self,
        index: usize,
    ) -> Result<ApplicationWindowDecision, String> {
        self.require_command_bar()?;
        let application = self
            .command_applications
            .get(index)
            .cloned()
            .ok_or_else(|| format!("application {index} is no longer available"))?;
        // Without a readable window list the application is simply launched.
        let snapshot = self
            .window_provider
            .as_mut()
            .and_then(|provider| provider.windows().ok())
            .unwrap_or_default();
        let mut external = Vec::with_capacity(snapshot.len());
        for window in snapshot {
            if is_kestrel_window(window.app_id.as_deref()) {
                continue;
            }
            if window.active {
                self.last_external_active = Some(window.handle);
            }
            external.push(window);
        }
        let decision =
            decide_application_window_action(&external, self.last_external_active, &application);
        let provider = self.window_provider.as_mut();
        match (decision, provider) {
            (ApplicationWindowDecision::Hide(handle), Some(provider)) => {
                if !provider.backend().can_minimize() {
                    return Err(format!(
                        "{} cannot minimize windows",
                        provider.backend().label()
                    ));
                }
                provider
                    .minimize(handle)
                    .map_err(|error| error.to_string())?;
            }
            (ApplicationWindowDecision::Restore(handle), Some(provider)) => {
                provider
                    .activate(handle)
                    .map_err(|error| error.to_string())?;
            }
            _ => {
                launch_desktop_entry(&application).map_err(|error| error.to_string())?;
                return Ok(ApplicationWindowDecision::Launch);
            }
        }
        Ok(decision)
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

    pub fn scratchpad_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SCRATCHPAD_ID && registration.running)
    }

    pub fn scratchpad_snapshot(&self, now: SystemTime) -> Option<ScratchpadSnapshot> {
        self.scratchpad
            .as_ref()
            .map(|service| service.snapshot(now))
    }

    pub fn scratchpad_service(&mut self) -> Result<&mut ScratchpadService, String> {
        if !self.scratchpad_is_running() {
            return Err(
                "Scratchpad is not running; enable notes.scratchpad in the Feature Hub.".to_owned(),
            );
        }
        self.scratchpad.as_mut().ok_or_else(|| {
            "No writable scratchpad data directory was resolved; set XDG_DATA_HOME or HOME."
                .to_owned()
        })
    }

    pub fn scratchpad_create(&mut self, title: String) -> Result<u64, String> {
        self.scratchpad_service()?
            .create(title, SystemTime::now())
            .map(|id| id.get())
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_rename(&mut self, id: u64, title: String) -> Result<(), String> {
        self.scratchpad_service()?
            .rename(
                kestrel_services::scratchpad::NoteId::from(id),
                title,
                SystemTime::now(),
            )
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_switch(&mut self, id: u64) -> Result<(), String> {
        self.scratchpad_service()?
            .switch(kestrel_services::scratchpad::NoteId::from(id))
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_close(&mut self, id: u64) -> Result<(), String> {
        self.scratchpad_service()?
            .close(kestrel_services::scratchpad::NoteId::from(id))
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_update(&mut self, id: u64, text: String) -> Result<(), String> {
        self.scratchpad_service()?
            .update_text(
                kestrel_services::scratchpad::NoteId::from(id),
                text,
                SystemTime::now(),
            )
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_remove_expired(&mut self) -> Result<usize, String> {
        self.scratchpad_service()?
            .remove_expired(SystemTime::now())
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_export(&mut self, id: u64, destination: &Path) -> Result<(), String> {
        self.scratchpad_service()?
            .export(kestrel_services::scratchpad::NoteId::from(id), destination)
            .map_err(|error| error.to_string())
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

    /// Returns retained-item metadata while keeping contents private.
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
        if !self.quick_toggle_is_running(QuickToggleId::BatteryAlerts) {
            self.quick_toggles.stop(QuickToggleId::BatteryAlerts);
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

    pub fn audio_mixer_is_running(&self) -> bool {
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
    use super::{
        ApplicationRuntime, ApplicationWindowDecision, FeaturePreset,
        decide_application_window_action,
    };
    use crate::STATUS_NOTIFIER_ID;
    use kestrel_core::{AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus};
    use kestrel_platform::{
        audio::{FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID},
        capture::FEATURE_ID as CAPTURE_ID,
        clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
        global_shortcuts::FEATURE_ID as GLOBAL_SHORTCUTS_ID,
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
        windows::{WindowHandle, WindowInfo},
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
    fn bindings_resolve_known_commands_and_skip_the_rest() {
        let configuration = kestrel_core::ShortcutConfiguration {
            bindings: vec![
                kestrel_core::ShortcutBinding {
                    command: "microphone.toggle-mute".to_owned(),
                    trigger: "LOGO+ALT+m".to_owned(),
                },
                kestrel_core::ShortcutBinding {
                    command: "window.explode".to_owned(),
                    trigger: "LOGO+ALT+x".to_owned(),
                },
            ],
        };

        let (requests, skipped) = ApplicationRuntime::shortcut_requests(&configuration);

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "microphone.toggle-mute");
        assert_eq!(
            requests[0].description,
            "Mute or unmute every microphone input"
        );
        assert_eq!(skipped, vec!["window.explode".to_owned()]);
    }

    #[test]
    fn command_gates_follow_feature_state() {
        use crate::command_line::{CommandGate, CommandLineAction};
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("valid feature ID");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();

        assert_eq!(runtime.command_gate(CommandLineAction::ShowWindow), None);
        assert_eq!(
            runtime.command_gate(CommandLineAction::OpenCommandBar),
            None
        );
        let Some(CommandGate::Disabled { feature }) =
            runtime.command_gate(CommandLineAction::QuickPaste)
        else {
            panic!("disabled clipboard history gates quick paste");
        };
        assert!(feature.contains(CLIPBOARD_HISTORY_ID));
    }

    #[test]
    fn disabled_screenshots_refuse_every_capture_action() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert!(!runtime.capture_is_running());
        let error = runtime
            .begin_capture(None)
            .expect_err("a disabled feature never captures");
        assert!(error.contains(CAPTURE_ID), "{error}");
        assert!(runtime.clear_captures().is_err());
        assert!(runtime.capture_snapshot().entries.is_empty());
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Balanced,
            CAPTURE_ID
        ));
    }

    #[test]
    fn disabled_global_shortcuts_never_register() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        let (sender, _receiver) = async_channel::unbounded();
        runtime.set_command_sender(sender);
        runtime.start();

        assert!(!runtime.global_shortcuts_is_running());
        assert!(!runtime.global_shortcuts.is_started());
        assert_eq!(
            runtime.shortcut_status().phase,
            kestrel_platform::global_shortcuts::ShortcutPhase::Idle
        );
        assert!(
            !ApplicationRuntime::preset_enabled(FeaturePreset::Balanced, GLOBAL_SHORTCUTS_ID),
            "registration stays an explicit opt-in"
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

    #[test]
    fn application_window_decision_hides_restores_and_launches() {
        let application = application("org.example.Editor.desktop", "Editor", false);
        let window = WindowInfo {
            handle: WindowHandle(9),
            title: "Editor".to_owned(),
            app_id: Some("org.example.editor".to_owned()),
            active: true,
            minimized: false,
        };
        let windows = [window.clone()];
        assert_eq!(
            decide_application_window_action(&windows, Some(WindowHandle(9)), &application),
            ApplicationWindowDecision::Hide(WindowHandle(9))
        );
        let mut restored = window;
        restored.active = false;
        restored.minimized = true;
        assert_eq!(
            decide_application_window_action(&[restored], Some(WindowHandle(1)), &application),
            ApplicationWindowDecision::Restore(WindowHandle(9))
        );
        assert_eq!(
            decide_application_window_action(&[], None, &application),
            ApplicationWindowDecision::Launch
        );
    }

    fn application(id: &str, name: &str, terminal: bool) -> ApplicationEntry {
        ApplicationEntry {
            id: id.to_owned(),
            name: name.to_owned(),
            comment: None,
            startup_wm_class: None,
            command: vec![name.to_lowercase()],
            terminal,
            actions: Vec::new(),
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
        // Entries need desktop files for GIO launching.
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

    #[test]
    fn disabling_process_features_joins_the_worker_and_refuses_actions() {
        use super::{LISTENERS_FEATURE_ID, PROCESSES_ID};
        let mut configuration = ApplicationConfiguration::default();
        for feature in [PROCESSES_ID, LISTENERS_FEATURE_ID] {
            configuration
                .set_feature_enabled(feature, true)
                .expect("valid feature ID");
        }
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        assert!(runtime.process_inventory.is_running());
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback binds");
        runtime.refresh_listeners().expect("listeners are enabled");
        assert!(!runtime.listeners.snapshot().listeners.is_empty());

        runtime
            .apply_configuration(&ApplicationConfiguration::default())
            .expect("configuration applies");
        drop(socket);

        assert!(!runtime.process_inventory.is_running(), "worker joined");
        assert!(runtime.listeners.snapshot().listeners.is_empty());
        assert!(runtime.refresh_processes().is_err());
        assert!(runtime.refresh_listeners().is_err());
        let own = kestrel_platform::processes::ProcessIdentity {
            pid: std::process::id(),
            start_ticks: 0,
        };
        let refused = runtime
            .process_confirmation(own, crate::ProcessActionKind::Kill)
            .expect_err("a disabled feature confirms nothing");
        assert!(refused.contains(PROCESSES_ID), "{refused}");
        assert!(runtime.processes_view_model("").rows.is_empty());
    }

    #[test]
    fn disabled_screen_tools_refuse_jobs_and_keep_no_worker() {
        use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        let error = runtime
            .begin_screen_tool(ScreenTool::Text)
            .expect_err("a disabled tool never captures");
        assert!(error.contains("capture.text"), "{error}");
        assert!(!runtime.screen_tools.is_running());
        let view = runtime.screen_tools_view_model();
        assert!(view.buttons.iter().all(|button| !button.enabled));
        assert!(view.buttons.iter().all(|button| button.reason.is_some()));
        for tool in ALL_SCREEN_TOOLS {
            assert!(ApplicationRuntime::preset_enabled(
                FeaturePreset::Balanced,
                tool.feature_id()
            ));
        }
    }

    #[test]
    fn disabled_scratchpad_refuses_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let error = runtime
            .scratchpad_create("test".to_owned())
            .expect_err("disabled feature");
        assert!(error.contains("notes.scratchpad"));
    }

    #[test]
    fn a_disabled_camera_never_opens() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let error = runtime
            .start_camera(std::path::Path::new("/dev/video0"))
            .expect_err("disabled feature");
        assert!(error.contains("camera.preview"), "{error}");
        assert_eq!(
            runtime.camera.snapshot().phase,
            kestrel_services::camera::CameraPhase::Idle
        );
        assert!(runtime.camera_view_model().cameras.is_empty());
    }
}
