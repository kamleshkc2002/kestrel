mod capture_editor;
mod layer_shell;
mod screen_selection;
mod window;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use adw::{glib, prelude::*};
use async_channel::{Receiver, Sender};
use kestrel::command_line::{
    CommandGate, CommandLineAction, CommandLineError, Invocation, command_list, parse_arguments,
    usage,
};
use kestrel::view_model::{
    ClipboardQuery, CommandBarQuery, MicrophoneViewModel, ShortcutsViewModel, SnippetQuery,
    SpeedTestViewModel,
};
use kestrel::{
    AlertKind, AppearancePreference, ApplicationCommand, ApplicationRuntime, ApplicationViewModel,
    ClipboardLifecycle, ClipboardViewModel, CloseAction, ConfigurationWarning, FeaturePreset,
    LoadedConfiguration, MicrophoneCommand, MonitorReadout, MonitorViewModel, PanelMoveDirection,
    SnippetCommand, SnippetDraft, SnippetLimit, SnippetMatch, SnippetProviderPreference,
    StatusNotifierIntegration, configuration_path, disable as disable_autostart,
    enable as enable_autostart, export_file, import_file, load, save, setup,
};
use kestrel::{
    ApplicationWindowDecision, CaptureId, CaptureRequest, CommandBarCommand, CommandProviderSwitch,
    FocusTarget,
};
use kestrel_core::{ApplicationConfiguration, FeatureConfigurationSnapshot};
use kestrel_platform::notifications::{
    AlertNotification, AlertNotifier, CaptureNotificationAction, DesktopNotifier, NotificationEvent,
};
use kestrel_services::alerts::notification_text;

use window::{FloatingSurface, QuickPanelEntry, StatusOverlay, WindowView};

const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// Poll interval for mixer and microphone state, which other mixers and
/// hardware keys change externally.
const AUDIO_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
/// Poll interval while the overlay shows a control, so external changes appear
/// soon after they happen.
const AUDIO_FEEDBACK_INTERVAL: Duration = Duration::from_millis(500);
/// Poll interval for the applications offered to the System track.
const RECORDING_APPS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

type RefreshResult = Result<(ApplicationViewModel, String), String>;
/// Which deferred reads this tick performs.
#[derive(Debug, Clone, Copy, Default)]
struct TickDue {
    audio: bool,
    listeners: bool,
    keyboard_light: bool,
    recording_apps: bool,
}
struct TickUpdate {
    monitor: Option<MonitorViewModel>,
    clipboard: Option<ClipboardViewModel>,
    /// Command results re-run because clipboard entries in them changed.
    command_bar: Option<kestrel::CommandBarViewModel>,
    audio: Option<kestrel::AudioViewModel>,
    microphone: Option<MicrophoneViewModel>,
    speed_test: Option<SpeedTestViewModel>,
    shortcuts: Option<ShortcutsViewModel>,
    capture: Option<kestrel::CaptureViewModel>,
    capture_notification: Option<(CaptureId, u32)>,
    capture_follow_up: Option<(CaptureId, kestrel::CaptureAfterScreenshot)>,
    processes: Option<kestrel::ProcessesViewModel>,
    listeners: Option<kestrel::ListenersViewModel>,
    screen_tools: Option<kestrel::ScreenToolsViewModel>,
    camera: Option<kestrel::CameraViewModel>,
    recording: Option<kestrel::RecordingViewModel>,
    keyboard_light: Option<kestrel::QuickToggleViewModel>,
}

type SharedState = Arc<Mutex<ControllerState>>;

/// Maximum entries returned per clipboard search.
const CLIPBOARD_SEARCH_LIMIT: usize = 50;
/// Maximum bytes in one clipboard preview.
const CLIPBOARD_PREVIEW_BYTES: usize = 2048;
/// Maximum snippets returned per search.
const SNIPPET_SEARCH_LIMIT: usize = 100;

struct ControllerState {
    runtime: ApplicationRuntime,
    commands: Sender<ApplicationCommand>,
    configuration: ApplicationConfiguration,
    warnings: Vec<ConfigurationWarning>,
    preset_snapshot: Option<FeatureConfigurationSnapshot>,
    clipboard_query: String,
    clipboard_matches: Vec<kestrel_services::clipboard::ClipboardMatch>,
    clipboard_preview: Option<kestrel_services::clipboard::ClipboardPreview>,
    clipboard_fingerprint: Option<ClipboardFingerprint>,
    speed_test_generation: u64,
    shortcut_generation: u64,
    capture_generation: u64,
    last_capture_follow_up: Option<CaptureId>,
    process_query: String,
    process_generation: u64,
    listener_generation: u64,
    screen_tools_generation: u64,
    camera_generation: u64,
    recording_generation: u64,
    snippet_query: String,
    snippet_matches: Vec<SnippetMatch>,
    snippet_draft: Option<SnippetDraft>,
    command_query: String,
    command_results: Vec<kestrel_services::command_bar::CommandResult>,
}

impl ControllerState {
    fn view_model(&self) -> ApplicationViewModel {
        let mut model = self.runtime.view_model_with_panels(
            &self.warnings,
            &self.configuration,
            self.preset_snapshot.is_some(),
            ClipboardQuery {
                query: &self.clipboard_query,
                matches: &self.clipboard_matches,
                preview: self.clipboard_preview.as_ref(),
            },
            SnippetQuery {
                query: &self.snippet_query,
                matches: &self.snippet_matches,
                draft: self.snippet_draft.as_ref(),
            },
            CommandBarQuery {
                query: &self.command_query,
                results: &self.command_results,
            },
        );
        model.processes = self.runtime.processes_view_model(&self.process_query);
        model
    }

    fn refresh_command_results(&mut self) {
        if self.command_query.trim().is_empty() {
            self.command_results.clear();
            return;
        }
        let clock = kestrel_platform::snippets::SystemClock;
        let now = clock.local_time();
        self.command_results = self
            .runtime
            .command_search(&self.command_query.clone(), &now);
    }

    fn run_command_result(&mut self, id: &str) -> String {
        use kestrel_services::command_bar::CommandAction;
        // Reject stale results before acting.
        if !self.runtime.command_bar_running() {
            return "The command bar is not running; enable commands.bar in the Feature Hub"
                .to_owned();
        }
        let Some(result) = self
            .command_results
            .iter()
            .find(|result| result.item.id == id)
            .cloned()
        else {
            return format!("No command result with id \"{id}\"");
        };

        let outcome = match &result.item.action {
            CommandAction::InsertText(text) => match self.runtime.copy_to_clipboard(text) {
                Ok(()) => format!("Copied \"{}\" to the clipboard", result.item.title),
                Err(error) => format!("The value was not copied: {error}"),
            },
            CommandAction::RunScript { index } => match self.runtime.run_command_script(*index) {
                Ok(outcome) => {
                    let mut message = format!(
                        "{} finished with {:?} in {:?}",
                        outcome.name, outcome.exit_code, outcome.duration
                    );
                    if outcome.stdout_truncated || outcome.stderr_truncated {
                        message.push_str(" (output truncated to the configured bound)");
                    }
                    message
                }
                Err(error) => format!("Script failed: {error}"),
            },
            CommandAction::OpenApplication { index } => {
                match self.runtime.launch_command_application(*index) {
                    Ok(ApplicationWindowDecision::Hide(_)) => format!("Hid {}", result.item.title),
                    Ok(ApplicationWindowDecision::Restore(_)) => {
                        format!("Brought {} to the front", result.item.title)
                    }
                    Ok(ApplicationWindowDecision::Launch) => {
                        format!("Opened {}", result.item.title)
                    }
                    Err(error) => format!("{}: {error}", result.item.title),
                }
            }
            CommandAction::OpenApplicationAction { index, action_id } => {
                match self
                    .runtime
                    .launch_command_application_action(*index, action_id)
                {
                    Ok(()) => format!("Ran {}", result.item.title),
                    Err(error) => format!("The application action was not run: {error}"),
                }
            }
            CommandAction::ActivateWindow(handle) => {
                match self.runtime.activate_command_window(*handle) {
                    Ok(()) => "Window activated".to_owned(),
                    Err(error) => format!("The window was not activated: {error}"),
                }
            }
            CommandAction::CopyClipboardEntry { item_id } => {
                match self.runtime.execute_clipboard_command(
                    kestrel_services::clipboard::ClipboardCommand::CopyItem {
                        item_id: *item_id,
                        plain_text: true,
                    },
                ) {
                    Some(Ok(_)) => format!("Copied clipboard entry {item_id}"),
                    Some(Err(error)) => format!("Clipboard entry was not copied: {error}"),
                    None => "Clipboard history is not running".to_owned(),
                }
            }
            CommandAction::SetFeatureEnabled {
                feature_id,
                enabled,
            } => {
                let _ = self
                    .commands
                    .try_send(ApplicationCommand::SetFeatureEnabled {
                        feature_id: feature_id.clone(),
                        enabled: *enabled,
                    });
                format!(
                    "{} {}",
                    feature_id,
                    if *enabled {
                        "enable requested"
                    } else {
                        "disable requested"
                    }
                )
            }
            CommandAction::OpenFile(path) => {
                match self
                    .runtime
                    .open_command_target(&path.display().to_string())
                {
                    Ok(()) => format!("Opened {}", path.display()),
                    Err(error) => format!("The file was not opened: {error}"),
                }
            }
            CommandAction::OpenUrl(url) => match self.runtime.open_command_target(url) {
                Ok(()) => format!("Opened {url}"),
                Err(error) => format!("The link was not opened: {error}"),
            },
            CommandAction::Kestrel(action) => match self.run_kestrel_command(action) {
                Some(message) => message,
                None => format!("No Kestrel action named \"{action}\""),
            },
        };

        if !matches!(
            result.item.source,
            kestrel_services::command_bar::CommandSource::Window
                | kestrel_services::command_bar::CommandSource::Clipboard
        ) {
            let _ = self.runtime.record_command_use(id);
        }
        self.refresh_command_results();
        outcome
    }

    /// Flips a quick toggle from its observed state; an unobserved toggle turns on.
    fn flip_quick_toggle(&mut self, id: kestrel::QuickToggleId) -> String {
        use kestrel_platform::quick_toggles::QuickToggleControl;
        let currently_enabled = self
            .runtime
            .quick_toggle_snapshots()
            .find(|snapshot| snapshot.id == id)
            .and_then(|snapshot| snapshot.observation.as_ref())
            .is_some_and(|observation| match &observation.control {
                QuickToggleControl::Switch { enabled, .. } => *enabled,
                QuickToggleControl::Level { percentage } => *percentage > 0,
                QuickToggleControl::Actions(_) => false,
            });
        let command = kestrel::QuickToggleCommand {
            id,
            mutation: kestrel::QuickToggleMutation::SetEnabled(!currently_enabled),
            confirmation_token: None,
        };
        let label = id.label();
        match self.runtime.execute_quick_toggle_command(command) {
            Some(Ok(_)) => format!("{label} toggled"),
            Some(Err(error)) => format!("{label} failed: {error}"),
            None => format!("{label} is not available"),
        }
    }

    fn run_kestrel_command(&mut self, action: &str) -> Option<String> {
        if let Some(name) = action.strip_prefix("insert_snippet:") {
            return Some(match self.runtime.insert_snippet(name) {
                Ok(report) => format!(
                    "Inserted \"{name}\" ({} bytes via {})",
                    report.bytes,
                    report.provider.label()
                ),
                Err(error) => format!("Insert failed: {error}"),
            });
        }

        if let Ok(command) = CommandLineAction::parse(action) {
            if command == CommandLineAction::CaptureEditLast {
                let present = self
                    .commands
                    .try_send(ApplicationCommand::PresentWindow)
                    .is_ok();
                let edit = self.commands.try_send(command.to_command()).is_ok();
                return Some(if present && edit {
                    "capture.edit-last queued".to_owned()
                } else {
                    "capture.edit-last could not be queued".to_owned()
                });
            }
            let queued = self.commands.try_send(command.to_command()).is_ok();
            return Some(if queued {
                format!("{action} queued")
            } else {
                format!("{action} could not be queued")
            });
        }

        let command = match action {
            "open_window" => ApplicationCommand::PresentWindow,
            "refresh" => ApplicationCommand::RefreshCapabilities,
            "preset_essentials" => {
                ApplicationCommand::ApplyPreset(kestrel::FeaturePreset::Essentials)
            }
            "preset_balanced" => ApplicationCommand::ApplyPreset(kestrel::FeaturePreset::Balanced),
            "preset_everything" => {
                ApplicationCommand::ApplyPreset(kestrel::FeaturePreset::Everything)
            }
            "undo_preset" => ApplicationCommand::UndoPreset,
            "wipe_clipboard" => {
                ApplicationCommand::Clipboard(kestrel_services::clipboard::ClipboardCommand::Wipe)
            }
            "clear_selection" => ApplicationCommand::Clipboard(
                kestrel_services::clipboard::ClipboardCommand::ClearSelection,
            ),
            "reset_ranking" => ApplicationCommand::ResetCommandRanking,
            "microphone_toggle" => ApplicationCommand::Microphone(MicrophoneCommand::ToggleMute),
            "speed_test" => ApplicationCommand::StartSpeedTest,
            _ => return None,
        };

        let queued = self.commands.try_send(command).is_ok();
        Some(if queued {
            format!("{action} queued")
        } else {
            format!("{action} could not be queued")
        })
    }

    fn refresh_snippet_matches(&mut self) {
        self.snippet_matches = self
            .runtime
            .snippet_matches(&self.snippet_query, SNIPPET_SEARCH_LIMIT);
    }

    fn refresh_clipboard_matches(&mut self) {
        self.clipboard_preview = None;
        match self
            .runtime
            .clipboard_search(&self.clipboard_query, CLIPBOARD_SEARCH_LIMIT)
        {
            Some(Ok(matches)) => self.clipboard_matches = matches,
            Some(Err(_)) | None => self.clipboard_matches.clear(),
        }
    }
}

fn capture_follow_up_decision<T: Copy + PartialEq>(
    captured: Option<T>,
    last_handled: Option<T>,
    after: kestrel::CaptureAfterScreenshot,
) -> Option<(T, kestrel::CaptureAfterScreenshot)> {
    captured
        .filter(|id| last_handled != Some(*id))
        .map(|id| (id, after))
}

/// Samples state and notifies while the controller lock is released.
fn run_tick(
    state: &SharedState,
    notifier: &dyn AlertNotifier,
    capture_notifier: &DesktopNotifier,
    tray: Option<&kestrel::TrayReadoutSink>,
    observed_at: Duration,
    due: TickDue,
) -> TickUpdate {
    let (
        updated,
        alerts,
        audio,
        microphone,
        speed_test,
        shortcuts,
        capture,
        capture_to_notify,
        capture_follow_up,
        processes,
        listeners,
        screen_tools,
        camera,
        recording,
        keyboard_light,
    ) = {
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tick = guard.runtime.sample_monitor(observed_at);
        let controller = &mut *guard;
        let audio = (due.audio && controller.runtime.refresh_audio_mixer()).then(|| {
            controller
                .runtime
                .audio_view_model(&controller.configuration.audio)
        });
        let microphone = (due.audio && controller.runtime.refresh_microphone()).then(|| {
            controller
                .runtime
                .microphone_view_model(&controller.configuration.audio)
        });
        let keyboard_light = due
            .keyboard_light
            .then(|| controller.runtime.refresh_keyboard_light())
            .flatten();
        let generation = guard.runtime.speed_test_snapshot().generation;
        let speed_test = (generation != guard.speed_test_generation).then(|| {
            guard.speed_test_generation = generation;
            guard.runtime.speed_test_view_model()
        });
        // Portal registration finishes asynchronously, possibly after a dialog.
        let generation = guard.runtime.shortcut_status().generation;
        let shortcuts = (generation != guard.shortcut_generation).then(|| {
            guard.shortcut_generation = generation;
            guard.runtime.shortcuts_view_model()
        });
        guard.runtime.prune_captures();
        let snapshot = guard.runtime.capture_snapshot();
        let captured = match &snapshot.phase {
            kestrel_services::capture::CapturePhase::Captured { id } => Some(*id),
            _ => None,
        };
        let decision = capture_follow_up_decision(
            captured,
            guard.last_capture_follow_up,
            guard.configuration.capture.after_screenshot,
        );
        if let Some((id, _)) = decision {
            guard.last_capture_follow_up = Some(id);
        }
        let (capture_to_notify, capture_follow_up) = match decision {
            Some((id, kestrel::CaptureAfterScreenshot::Notify)) => (Some(id), None),
            Some((id, kestrel::CaptureAfterScreenshot::Edit)) => {
                (None, Some((id, kestrel::CaptureAfterScreenshot::Edit)))
            }
            Some((_, kestrel::CaptureAfterScreenshot::Nothing)) | None => (None, None),
        };
        let generation = snapshot.generation;
        let capture = (generation != guard.capture_generation).then(|| {
            guard.capture_generation = generation;
            guard.runtime.capture_view_model()
        });
        let process_generation = guard.runtime.process_snapshot().generation;
        let processes = (process_generation != guard.process_generation).then(|| {
            guard.process_generation = process_generation;
            guard.runtime.processes_view_model(&guard.process_query)
        });
        let listeners = if due.listeners && guard.runtime.listeners_is_running() {
            let _ = guard.runtime.refresh_listeners();
            let generation = guard.runtime.listener_generation();
            (generation != guard.listener_generation).then(|| {
                guard.listener_generation = generation;
                guard.runtime.listeners_view_model()
            })
        } else {
            None
        };
        let generation = guard.runtime.screen_tools_snapshot().generation;
        let screen_tools = (generation != guard.screen_tools_generation).then(|| {
            guard.screen_tools_generation = generation;
            guard.runtime.screen_tools_view_model()
        });
        let generation = guard.runtime.camera_generation();
        let camera = (generation != guard.camera_generation).then(|| {
            guard.camera_generation = generation;
            guard.runtime.camera_view_model()
        });
        let generation = guard.runtime.recording_generation();
        let applications_changed =
            due.recording_apps && guard.runtime.refresh_recording_applications();
        let recording =
            (generation != guard.recording_generation || applications_changed).then(|| {
                guard.recording_generation = generation;
                guard.runtime.recording_view_model()
            });
        (
            tick.outcome == kestrel_services::system_monitor::RefreshOutcome::Updated,
            tick.alerts,
            audio,
            microphone,
            speed_test,
            shortcuts,
            capture,
            capture_to_notify,
            capture_follow_up,
            processes,
            listeners,
            screen_tools,
            camera,
            recording,
            keyboard_light,
        )
    };
    let monitor = updated.then(|| deliver_alerts(state, notifier, alerts));
    if let (Some(tray), Some(monitor)) = (tray, monitor.as_ref()) {
        tray.publish(&monitor.tray);
    }
    let capture_notification = capture_to_notify.and_then(|id| {
        capture_notifier
            .notify_capture()
            .ok()
            .map(|notification_id| (id, notification_id))
    });
    let (clipboard, command_bar) = clipboard_tick_update(state);
    TickUpdate {
        monitor,
        clipboard,
        command_bar,
        audio,
        microphone,
        speed_test,
        shortcuts,
        capture,
        capture_notification,
        capture_follow_up,
        processes,
        listeners,
        screen_tools,
        camera,
        recording,
        keyboard_light,
    }
}

/// Delivers alerts outside the controller lock.
fn deliver_alerts(
    state: &SharedState,
    notifier: &dyn AlertNotifier,
    alerts: Vec<kestrel_services::alerts::AlertEvent>,
) -> MonitorViewModel {
    let deliveries = alerts
        .iter()
        .map(|event| {
            let (summary, body) = notification_text(event);
            let result = notifier
                .notify(&AlertNotification { summary, body })
                .map_err(|error| error.to_string());
            (event.kind(), result)
        })
        .collect::<Vec<_>>();

    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (kind, result) in deliveries {
        guard.runtime.record_alert_delivery(kind, result);
    }
    guard.runtime.monitor_view_model(&guard.configuration)
}

fn clipboard_tick_update(
    state: &SharedState,
) -> (
    Option<ClipboardViewModel>,
    Option<kestrel::CommandBarViewModel>,
) {
    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = guard.runtime.clipboard_snapshot();
    let fingerprint = ClipboardFingerprint::of(&snapshot);
    if guard.clipboard_fingerprint == Some(fingerprint) {
        return (None, None);
    }
    guard.clipboard_fingerprint = Some(fingerprint);
    // Refresh the explicit query so background captures appear in results.
    let refreshed = {
        let query = &guard.clipboard_query;
        guard
            .runtime
            .clipboard_search(query, CLIPBOARD_SEARCH_LIMIT)
    };
    if let Some(Ok(matches)) = refreshed {
        guard.clipboard_matches = matches;
    }
    if let Some(preview) = guard.clipboard_preview.as_ref() {
        if !guard
            .clipboard_matches
            .iter()
            .any(|matched| matched.id == preview.id)
        {
            guard.clipboard_preview = None;
        }
    }
    // A wipe, lock, or sleep must not leave clipboard previews in command results.
    let shows_clipboard = guard.command_results.iter().any(|result| {
        result.item.source == kestrel_services::command_bar::CommandSource::Clipboard
    });
    let command_bar = shows_clipboard.then(|| {
        guard.refresh_command_results();
        guard.view_model().command_bar
    });
    let clipboard = guard.runtime.clipboard_view_model(
        &guard.configuration,
        &guard.clipboard_query,
        &guard.clipboard_matches,
        guard.clipboard_preview.as_ref(),
    );
    (Some(clipboard), command_bar)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TargetedPanel {
    #[default]
    Full,
    Clipboard,
    Snippets,
    Commands,
    /// Address requests keep the monitoring scroll position.
    Monitor,
    /// Keeps mute updates from resetting scroll.
    Microphone,
    /// Mixer and microphone panels after preference edits.
    Audio,
    SpeedTest,
    Capture,
    Processes,
    Scratchpad,
}

/// Fingerprinting avoids rebuilding presentation on every worker poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClipboardFingerprint {
    lifecycle: ClipboardLifecycle,
    items: usize,
    total_bytes: usize,
    pinned: usize,
    rejected: u64,
    filtered: u64,
    clears: u64,
    wipes: u64,
    has_error: bool,
}

impl ClipboardFingerprint {
    fn of(snapshot: &kestrel_services::clipboard::ClipboardSnapshot) -> Self {
        Self {
            lifecycle: snapshot.lifecycle,
            items: snapshot.items.len(),
            total_bytes: snapshot.total_bytes,
            pinned: snapshot.pinned_items,
            rejected: snapshot.rejected_oversize_items,
            filtered: snapshot.filtered_sensitive_items,
            clears: snapshot.selection_clears,
            wipes: snapshot.wipe_count,
            has_error: snapshot.last_error.is_some(),
        }
    }
}

enum CaptureOperation {
    Begin(Option<kestrel::CaptureMode>),
    Cancel,
    Save {
        id: CaptureId,
        destination: PathBuf,
    },
    SaveEdit {
        id: CaptureId,
        plan: kestrel::EditPlan,
    },
    Delete(CaptureId),
    Clear,
}

enum ControllerOperation {
    Refresh,
    QuickToggle(kestrel::QuickToggleCommand),
    FlipQuickToggle(kestrel::QuickToggleId),
    SetFeatureEnabled {
        feature_id: String,
        enabled: bool,
    },
    ApplyPreset(FeaturePreset),
    FinishSetup {
        preset: Option<FeaturePreset>,
        left_off: Vec<String>,
    },
    SkipSetup,
    UndoPreset,
    SetAppearance(AppearancePreference),
    SetCloseAction(CloseAction),
    SetAutostart(bool),
    SetWhatsNew(bool),
    MarkWhatsNewRead,
    SetMonitorReadoutVisible {
        readout: MonitorReadout,
        visible: bool,
    },
    SetMonitorSetting(kestrel::MonitorSetting),
    ShowLocalAddresses(bool),
    MoveMonitorReadout {
        readout: MonitorReadout,
        direction: PanelMoveDirection,
    },
    SetAlertEnabled {
        kind: AlertKind,
        enabled: bool,
    },
    SetAlertThreshold {
        kind: AlertKind,
        threshold: f64,
    },
    SetAlertSustain {
        kind: AlertKind,
        duration: Duration,
    },
    Audio(kestrel::AudioCommand),
    Microphone(MicrophoneCommand),
    StartSpeedTest,
    CancelSpeedTest,
    Capture(CaptureOperation),
    SetAudioBoostPercent(u8),
    SetAudioOutputSwitch(kestrel::AudioOutputSwitch),
    SetAudioDisconnectPolicy(kestrel::AudioDisconnectPolicy),
    SetAudioDisconnectVolumePercent(u8),
    SetAudioIncludeInactiveStreams(bool),
    AudioPreference(kestrel::AudioPreferenceEdit),
    Clipboard(kestrel::ClipboardCommand),
    ClipboardSearch(String),
    ClipboardPreview(u64),
    ClipboardCopyImageText(u64),
    SetClipboardLayout(kestrel::ClipboardLayout),
    SetClipboardLimit(kestrel::ClipboardLimit),
    SetCaptureLimit(kestrel::CaptureLimit),
    SetCaptureAfterScreenshot(kestrel::CaptureAfterScreenshot),
    SetProcessLimit(kestrel::ProcessLimit),
    SetScratchpadLimit(kestrel::ScratchpadLimit),
    SetLoggingSetting(kestrel::LoggingSetting),
    SetOverlayModule {
        module: kestrel::OverlayModule,
        shown: bool,
    },
    SetAccessoryLevels {
        warning: u8,
        critical: u8,
    },
    ClearLogs,
    SetShortcut {
        command: String,
        trigger: Option<kestrel_core::ShortcutTrigger>,
    },
    SetScreenToolsSetting(kestrel::ScreenToolsSetting),
    SetRecordingSetting(kestrel::RecordingSetting),
    RefreshProcesses,
    SetClipboardFilterSensitive(bool),
    SetClipboardPastePlainText(bool),
    Snippet(SnippetCommand),
    SnippetSearch(String),
    SnippetEdit(String),
    SnippetNew,
    SetSnippetLimit(SnippetLimit),
    SetSnippetProvider(SnippetProviderPreference),
    SetSnippetExpansionTiming(kestrel::SnippetExpansionTiming),
    CommandQuery(String),
    CommandBar(CommandBarCommand),
    ResetCommandRanking,
    SetCommandResultLimit(u32),
    SetQuickPanelFavorites(Vec<String>),
    SetCommandProvider(CommandProviderSwitch),
    ImportConfiguration(PathBuf),
    ExportConfiguration(PathBuf),
    Scratchpad(kestrel::ScratchpadCommand),
}

struct ControllerContext {
    config_path: Option<PathBuf>,
    /// First launch: setup waits for the window to be presented.
    offer_setup: bool,
    refresh_results: Sender<RefreshResult>,
    tick_results: Sender<TickUpdate>,
    commands: Sender<ApplicationCommand>,
    notifier: Arc<dyn AlertNotifier>,
    capture_notifier: Arc<DesktopNotifier>,
    status_notifier: Option<StatusNotifierIntegration>,
}

struct ApplicationController {
    state: SharedState,
    config_path: Option<PathBuf>,
    /// First launch: setup opens once the window is first presented.
    setup_pending: Cell<bool>,
    window: RefCell<Option<WindowView>>,
    floating: RefCell<Option<FloatingSurface>>,
    /// Works with or without the main window.
    overlay: StatusOverlay,
    refreshing: Cell<bool>,
    targeted: Cell<TargetedPanel>,
    monitoring: Cell<bool>,
    /// Mirrors registration state; disabled features spawn no tick workers.
    monitor_running: Cell<bool>,
    clipboard_running: Cell<bool>,
    microphone_running: Cell<bool>,
    audio_mixer_running: Cell<bool>,
    speed_test_running: Cell<bool>,
    shortcuts_running: Cell<bool>,
    capture_running: Cell<bool>,
    processes_running: Cell<bool>,
    listeners_running: Cell<bool>,
    screen_tools_running: Cell<bool>,
    camera_running: Cell<bool>,
    recording_running: Cell<bool>,
    /// The status overlay runs; its modules then poll for control feedback.
    overlay_running: Cell<bool>,
    overlay_modules: Cell<kestrel_core::OverlayConfiguration>,
    keyboard_light_running: Cell<bool>,
    /// A selection view is open; at most one at a time.
    selection_open: Cell<bool>,
    /// A frame waits for a selection view.
    selection_pending: Cell<Option<kestrel::ScreenTool>>,
    /// The recording area view is open.
    recording_area_open: Cell<bool>,
    listener_last_refresh: Cell<Instant>,
    monitor_started_at: Instant,
    /// Bounds audio backend polling to its interval.
    audio_last_refresh: Cell<Instant>,
    /// Bounds the keyboard light poll to the feedback interval.
    keyboard_light_last_refresh: Cell<Instant>,
    /// Bounds the System audio application poll to its interval.
    recording_apps_last_refresh: Cell<Instant>,
    /// Keeps only the newest query received while a worker is busy.
    pending_command_query: RefCell<Option<String>>,
    notifier: Arc<dyn AlertNotifier>,
    capture_notifier: Arc<DesktopNotifier>,
    notification_captures: RefCell<HashMap<u32, CaptureId>>,
    /// The tray entry while the tray feature runs; started and stopped with it.
    status_notifier: RefCell<Option<StatusNotifierIntegration>>,
    tray: RefCell<Option<kestrel::TrayReadoutSink>>,
    /// The latest XDG activation token from a tray host, used once.
    activation_token: RefCell<Option<String>>,
    refresh_results: Sender<RefreshResult>,
    tick_results: Sender<TickUpdate>,
    commands: Sender<ApplicationCommand>,
}
impl ApplicationController {
    fn new(
        runtime: ApplicationRuntime,
        configuration: ApplicationConfiguration,
        warnings: Vec<ConfigurationWarning>,
        context: ControllerContext,
    ) -> Rc<Self> {
        let ControllerContext {
            config_path,
            offer_setup,
            refresh_results,
            tick_results,
            commands,
            notifier,
            capture_notifier,
            status_notifier,
        } = context;
        let monitor_running = runtime.monitor_is_running();
        let clipboard_running = runtime.clipboard_is_running();
        let microphone_running = runtime.microphone_is_running();
        let audio_mixer_running = runtime.audio_mixer_is_running();
        let speed_test_running = runtime.speed_test_is_running();
        let shortcuts_running = runtime.global_shortcuts_is_running();
        let processes_running = runtime.processes_is_running();
        let listeners_running = runtime.listeners_is_running();
        let capture_running = runtime.capture_is_running();
        let screen_tools_running = kestrel::ALL_SCREEN_TOOLS
            .into_iter()
            .any(|tool| runtime.screen_tool_is_running(tool));
        let camera_running = runtime.camera_is_running();
        let recording_running = runtime.recording_is_running();
        let overlay_running = runtime.registrations().any(|registration| {
            registration.feature.id == kestrel::OVERLAY_ID && registration.running
        });
        let keyboard_light_running = runtime.registrations().any(|registration| {
            registration.feature.id == kestrel::QuickToggleId::KeyboardLight.feature_id()
                && registration.running
        });
        let overlay_modules = configuration.overlay;
        let state_commands = commands.clone();
        Rc::new(Self {
            state: Arc::new(Mutex::new(ControllerState {
                runtime,
                commands: state_commands,
                configuration,
                warnings,
                preset_snapshot: None,
                clipboard_query: String::new(),
                clipboard_matches: Vec::new(),
                clipboard_preview: None,
                clipboard_fingerprint: None,
                speed_test_generation: 0,
                shortcut_generation: 0,
                capture_generation: 0,
                last_capture_follow_up: None,
                process_query: String::new(),
                process_generation: 0,
                listener_generation: 0,
                screen_tools_generation: 0,
                camera_generation: 0,
                recording_generation: 0,
                snippet_query: String::new(),
                snippet_matches: Vec::new(),
                snippet_draft: None,
                command_query: String::new(),
                command_results: Vec::new(),
            })),
            config_path,
            setup_pending: Cell::new(offer_setup),
            window: RefCell::new(None),
            floating: RefCell::new(None),
            overlay: StatusOverlay::new(commands.clone()),
            refreshing: Cell::new(false),
            targeted: Cell::new(TargetedPanel::Full),
            monitoring: Cell::new(false),
            monitor_running: Cell::new(monitor_running),
            clipboard_running: Cell::new(clipboard_running),
            microphone_running: Cell::new(microphone_running),
            audio_mixer_running: Cell::new(audio_mixer_running),
            speed_test_running: Cell::new(speed_test_running),
            shortcuts_running: Cell::new(shortcuts_running),
            capture_running: Cell::new(capture_running),
            processes_running: Cell::new(processes_running),
            listeners_running: Cell::new(listeners_running),
            screen_tools_running: Cell::new(screen_tools_running),
            camera_running: Cell::new(camera_running),
            recording_running: Cell::new(recording_running),
            overlay_running: Cell::new(overlay_running),
            overlay_modules: Cell::new(overlay_modules),
            keyboard_light_running: Cell::new(keyboard_light_running),
            selection_open: Cell::new(false),
            selection_pending: Cell::new(None),
            recording_area_open: Cell::new(false),
            listener_last_refresh: Cell::new(Instant::now()),
            monitor_started_at: Instant::now(),
            audio_last_refresh: Cell::new(Instant::now()),
            keyboard_light_last_refresh: Cell::new(Instant::now()),
            recording_apps_last_refresh: Cell::new(Instant::now()),
            pending_command_query: RefCell::new(None),
            notifier,
            capture_notifier,
            notification_captures: RefCell::new(HashMap::new()),
            tray: RefCell::new(
                status_notifier
                    .as_ref()
                    .and_then(StatusNotifierIntegration::readout_sink),
            ),
            status_notifier: RefCell::new(status_notifier),
            activation_token: RefCell::new(None),
            refresh_results,
            tick_results,
            commands,
        })
    }

    fn present(self: &Rc<Self>, application: &adw::Application) {
        let existing = self
            .window
            .borrow()
            .as_ref()
            .map(|view| view.window.clone());
        let token = self.activation_token.take();
        if let Some(window) = existing {
            // Wayland compositors raise a window only with a fresh activation token.
            if let Some(token) = &token {
                window.set_startup_id(token);
            }
            // The close button may have minimized it; present alone does not
            // restore a minimized X11 window.
            window.unminimize();
            window.present();
            return;
        }

        let view_model = self.current_view_model();
        let frames = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .camera_frames();
        let view = WindowView::new(application, &view_model, self.commands.clone(), frames);
        let weak_controller = Rc::downgrade(self);
        view.window.connect_close_request(move |window| {
            let Some(controller) = weak_controller.upgrade() else {
                return glib::Propagation::Proceed;
            };
            // The camera never keeps running behind a closed or minimized window.
            controller.camera_request(kestrel::CameraRequest::Stop);
            let close_action = controller
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .configuration
                .ui
                .close_action;
            if close_action == CloseAction::Minimize {
                window.minimize();
                return glib::Propagation::Stop;
            }
            controller.window.borrow_mut().take();
            glib::Propagation::Proceed
        });
        if let Some(token) = &token {
            view.window.set_startup_id(token);
        }
        view.window.present();
        self.window.replace(Some(view));
        if self.setup_pending.replace(false) {
            let window = self
                .window
                .borrow()
                .as_ref()
                .map(|view| view.window.clone());
            if let Some(window) = window {
                let view_model = self.current_view_model();
                window::open_setup(&window, &view_model, &self.commands);
            }
        }
    }

    fn focus(self: &Rc<Self>, application: &adw::Application, target: FocusTarget) {
        if matches!(target, FocusTarget::CommandBar | FocusTarget::QuickPanel) {
            self.open_floating(application, target);
            return;
        }
        self.present(application);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        let _ = view.focus(target);
    }

    /// Opens one floating surface without raising the main window.
    fn open_floating(self: &Rc<Self>, application: &adw::Application, target: FocusTarget) {
        let startup_id = self.activation_token.take();
        let existing = self
            .floating
            .borrow()
            .as_ref()
            .map(|surface| (surface.kind, surface.window.clone()));
        if let Some((kind, window)) = existing {
            if kind == target {
                present_with(&window, startup_id.as_deref());
                return;
            }
            // Release the slot before closing; the close handler borrows it.
            self.floating.borrow_mut().take();
            window.close();
        }

        let parent = self
            .window
            .borrow()
            .as_ref()
            .map(|view| view.window.clone().upcast::<gtk::Window>());
        let surface = if target == FocusTarget::QuickPanel {
            let entries = self.quick_panel_entries();
            FloatingSurface::quick_panel(
                application,
                parent.as_ref(),
                startup_id.as_deref(),
                &self.commands,
                entries,
            )
        } else {
            let gate = self
                .command_gate(CommandLineAction::OpenCommandBar)
                .map(|gate| gate.to_string());
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .runtime
                .remember_front_window();
            FloatingSurface::command_bar(
                application,
                parent.as_ref(),
                startup_id.as_deref(),
                &self.commands,
                &self.current_view_model().command_bar,
                gate.as_deref(),
            )
        };
        let weak_controller = Rc::downgrade(self);
        surface.window.connect_close_request(move |closing| {
            let Some(controller) = weak_controller.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let owned = controller
                .floating
                .borrow()
                .as_ref()
                .is_some_and(|surface| &surface.window == closing);
            if owned {
                controller.floating.borrow_mut().take();
            }
            if target == FocusTarget::CommandBar {
                // Transient search state ends with the surface.
                let _ = controller
                    .commands
                    .try_send(ApplicationCommand::CommandQuery(String::new()));
            }
            glib::Propagation::Proceed
        });
        self.floating.replace(Some(surface));
    }

    /// Favorites in saved order, each with its current gate.
    fn quick_panel_entries(&self) -> Vec<QuickPanelEntry> {
        let favorites = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .configuration
            .quick_panel
            .favorites
            .clone();
        favorites
            .into_iter()
            .filter_map(|id| {
                let action = CommandLineAction::parse(&id).ok()?;
                Some(QuickPanelEntry {
                    gate: self.command_gate(action).map(|gate| gate.to_string()),
                    description: action.description(),
                    command: action.to_command(),
                    id,
                })
            })
            .collect()
    }

    fn show_message(&self, message: &str) {
        if let Some(view) = self.window.borrow().as_ref() {
            view.show_message(message);
        }
    }

    fn command_gate(&self, action: CommandLineAction) -> Option<CommandGate> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .command_gate(action)
    }

    fn capture_png(&self, id: CaptureId) -> Result<Vec<u8>, String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .capture_png(id)
    }

    fn copy_capture(&self, id: CaptureId) {
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        let message = self
            .capture_png(id)
            .and_then(|png| {
                adw::gdk::Texture::from_bytes(&glib::Bytes::from_owned(png))
                    .map_err(|_| "The capture could not be decoded".to_owned())
            })
            .map(|texture| {
                view.window.clipboard().set_texture(&texture);
                "Capture copied to the clipboard".to_owned()
            })
            .unwrap_or_else(|error| error);
        view.show_message(&message);
    }

    fn open_capture_editor(&self, id: CaptureId) {
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        match self
            .capture_png(id)
            .and_then(|png| kestrel::decode_png(&png).map_err(|error| error.to_string()))
        {
            Ok(image) => {
                let text = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .runtime
                    .editor_text_recognizer();
                capture_editor::open(&view.window, id, image, text, self.commands.clone());
            }
            Err(error) => view.show_message(&error),
        }
    }

    fn handle_capture_follow_up(
        self: &Rc<Self>,
        application: &adw::Application,
        id: CaptureId,
        action: CaptureNotificationAction,
    ) {
        self.focus(application, FocusTarget::Capture);
        match action {
            CaptureNotificationAction::Open => {}
            CaptureNotificationAction::Edit => self.open_capture_editor(id),
            CaptureNotificationAction::Copy => self.copy_capture(id),
        }
    }

    fn request_refresh(&self) {
        self.request_operation(ControllerOperation::Refresh, "kestrel-capability-refresh");
    }

    fn request_quick_toggle(&self, command: kestrel::QuickToggleCommand) {
        self.request_operation(
            ControllerOperation::QuickToggle(command),
            "kestrel-quick-toggle",
        );
    }

    fn request_operation(&self, operation: ControllerOperation, worker_name: &'static str) {
        if self.refreshing.replace(true) {
            if let Some(view) = self.window.borrow().as_ref() {
                view.show_message("Another Kestrel operation is still running");
            }
            return;
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_refreshing(true);
        }

        let state = Arc::clone(&self.state);
        let config_path = self.config_path.clone();
        let results = self.refresh_results.clone();
        let worker = thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                let started = Instant::now();
                let result = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .run(operation, config_path.as_deref());
                log::debug!(
                    "{worker_name} finished in {} ms",
                    started.elapsed().as_millis()
                );
                // Failure text can name processes or devices; it stays in the toast.
                if result.is_err() {
                    log::warn!("{worker_name} failed");
                }
                let _ = results.send_blocking(result);
            });
        if let Err(error) = worker {
            log::warn!("{worker_name} could not start");
            self.finish_refresh(Err(format!(
                "Could not start the {worker_name} worker: {error}"
            )));
        }
    }

    fn request_clipboard_operation(
        &self,
        operation: ControllerOperation,
        worker_name: &'static str,
    ) {
        self.request_targeted_operation(operation, worker_name, TargetedPanel::Clipboard);
    }

    /// Queues command-bar work; the newest query survives a busy worker.
    fn request_command_operation(&self, operation: ControllerOperation, worker_name: &'static str) {
        if let ControllerOperation::CommandQuery(query) = &operation {
            if self.refreshing.get() {
                *self.pending_command_query.borrow_mut() = Some(query.clone());
                return;
            }
        }
        self.request_targeted_operation(operation, worker_name, TargetedPanel::Commands);
    }

    fn dispatch_pending_command_query(&self) {
        let Some(query) = self.pending_command_query.borrow_mut().take() else {
            return;
        };
        self.request_command_operation(
            ControllerOperation::CommandQuery(query),
            "kestrel-command-query",
        );
    }

    fn request_snippet_operation(&self, operation: ControllerOperation, worker_name: &'static str) {
        self.request_targeted_operation(operation, worker_name, TargetedPanel::Snippets);
    }

    /// Targeted updates preserve panel queries and focus.
    fn request_targeted_operation(
        &self,
        operation: ControllerOperation,
        worker_name: &'static str,
        panel: TargetedPanel,
    ) {
        // Check the operation gate before recording the targeted panel.
        if self.refreshing.get() {
            self.request_operation(operation, worker_name);
            return;
        }
        self.targeted.set(panel);
        self.request_operation(operation, worker_name);
    }

    fn finish_refresh(&self, result: RefreshResult) {
        self.apply_refresh_result(result);
        self.dispatch_pending_command_query();
    }

    fn apply_refresh_result(&self, result: RefreshResult) {
        self.refreshing.set(false);
        let targeted = self.targeted.replace(TargetedPanel::Full);
        // The floating surfaces work without the main window.
        if let Some(surface) = self.floating.borrow().as_ref() {
            match &result {
                Ok((view_model, message)) => {
                    surface.set_command_bar(&view_model.command_bar);
                    if !message.is_empty() {
                        surface.show_outcome(message);
                    }
                }
                Err(message) => surface.show_outcome(message),
            }
        }
        // Polling, the overlay, and the colour scheme follow every result,
        // including those from the tray or command bar with no window open.
        let (view_model, message) = match result {
            Ok((view_model, message)) => (view_model, (!message.is_empty()).then_some(message)),
            Err(message) => (self.current_view_model(), Some(message)),
        };
        apply_color_scheme(view_model.appearance);
        self.track_running(&view_model);
        self.overlay.set_view_model(&view_model);
        self.reconcile_tray(&view_model);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        view.set_refreshing(false);
        apply_targeted_panel(view, &view_model, targeted);
        if let Some(message) = message {
            view.show_message(&message);
        }
    }

    /// Starts or stops the tray entry with the tray feature.
    fn reconcile_tray(&self, view_model: &ApplicationViewModel) {
        let wanted = view_model
            .features
            .iter()
            .any(|feature| feature.id == kestrel::STATUS_NOTIFIER_ID && feature.running);
        let present = self.status_notifier.borrow().is_some();
        if wanted && !present {
            let integration = StatusNotifierIntegration::start(self.commands.clone());
            let sink = integration.readout_sink();
            if sink.is_none() {
                log::warn!(
                    target: kestrel::STATUS_NOTIFIER_ID,
                    "The tray host did not accept the tray entry"
                );
            }
            *self.tray.borrow_mut() = sink;
            *self.status_notifier.borrow_mut() = Some(integration);
        } else if !wanted && present {
            let _ = self.stop_tray();
        }
    }

    /// Removes the tray entry and its readout icons off the GTK thread.
    fn stop_tray(&self) -> Option<thread::JoinHandle<()>> {
        let sink = self.tray.borrow_mut().take();
        let integration = self.status_notifier.borrow_mut().take();
        thread::Builder::new()
            .name("kestrel-tray-stop".to_owned())
            .spawn(move || {
                if let Some(sink) = sink {
                    sink.close();
                }
                drop(integration);
            })
            .ok()
    }

    /// Records which features run, which decides what each tick polls.
    fn track_running(&self, view_model: &ApplicationViewModel) {
        self.monitor_running.set(view_model.monitor.running);
        self.clipboard_running.set(view_model.clipboard.running);
        self.microphone_running.set(view_model.microphone.running);
        self.audio_mixer_running.set(view_model.audio.running);
        self.speed_test_running.set(view_model.speed_test.running);
        self.shortcuts_running.set(view_model.shortcuts.running);
        self.capture_running.set(view_model.capture.running);
        self.processes_running.set(view_model.processes.running);
        self.listeners_running.set(view_model.listeners.running);
        self.screen_tools_running
            .set(view_model.screen_tools.available);
        self.camera_running.set(view_model.camera.running);
        self.recording_running.set(view_model.recording.running);
        self.overlay_running.set(view_model.overlay.running);
        self.overlay_modules.set(view_model.overlay.modules);
        self.keyboard_light_running
            .set(view_model.features.iter().any(|feature| {
                feature.id == kestrel::QuickToggleId::KeyboardLight.feature_id() && feature.running
            }));
    }

    fn request_tick(&self) {
        if self.refreshing.get() || self.monitoring.replace(true) {
            return;
        }
        let now = Instant::now();
        let overlay_modules = self.overlay_modules.get();
        let overlay_feedback = self.overlay_running.get()
            && (overlay_modules.volume || overlay_modules.microphone_level);
        // Hardware keys, other mixers, and device hotplug change audio externally.
        let audio_due = (self.microphone_running.get() || self.audio_mixer_running.get()) && {
            let interval = if overlay_feedback {
                AUDIO_FEEDBACK_INTERVAL
            } else {
                AUDIO_REFRESH_INTERVAL
            };
            let due = now.duration_since(self.audio_last_refresh.get()) >= interval;
            if due {
                self.audio_last_refresh.set(now);
            }
            due
        };
        let keyboard_light_due = self.overlay_running.get()
            && overlay_modules.keyboard_light
            && self.keyboard_light_running.get()
            && {
                let due = now.duration_since(self.keyboard_light_last_refresh.get())
                    >= AUDIO_FEEDBACK_INTERVAL;
                if due {
                    self.keyboard_light_last_refresh.set(now);
                }
                due
            };
        let listeners_due = self.listeners_running.get() && {
            let due =
                now.duration_since(self.listener_last_refresh.get()) >= Duration::from_secs(2);
            if due {
                self.listener_last_refresh.set(now);
            }
            due
        };
        let recording_apps_due = self.recording_running.get() && {
            let due = now.duration_since(self.recording_apps_last_refresh.get())
                >= RECORDING_APPS_REFRESH_INTERVAL;
            if due {
                self.recording_apps_last_refresh.set(now);
            }
            due
        };
        if !self.monitor_running.get()
            && !self.clipboard_running.get()
            && !self.speed_test_running.get()
            && !self.shortcuts_running.get()
            && !self.capture_running.get()
            && !self.processes_running.get()
            && !self.listeners_running.get()
            && !self.screen_tools_running.get()
            && !self.camera_running.get()
            && !self.recording_running.get()
            && !audio_due
            && !keyboard_light_due
            && !recording_apps_due
        {
            self.monitoring.set(false);
            return;
        }
        let state = Arc::clone(&self.state);
        let notifier = Arc::clone(&self.notifier);
        let capture_notifier = Arc::clone(&self.capture_notifier);
        let tray = self.tray.borrow().clone();
        let results = self.tick_results.clone();
        let observed_at = self.monitor_started_at.elapsed();
        let due = TickDue {
            audio: audio_due,
            listeners: listeners_due,
            keyboard_light: keyboard_light_due,
            recording_apps: recording_apps_due,
        };
        let worker = thread::Builder::new()
            .name("kestrel-tick".to_owned())
            .spawn(move || {
                let update = run_tick(
                    &state,
                    notifier.as_ref(),
                    capture_notifier.as_ref(),
                    tray.as_ref(),
                    observed_at,
                    due,
                );
                let _ = results.send_blocking(update);
            });
        if worker.is_err() {
            self.monitoring.set(false);
        }
    }

    fn finish_tick(&self, update: TickUpdate) {
        self.monitoring.set(false);
        let TickUpdate {
            monitor,
            clipboard,
            command_bar,
            audio,
            microphone,
            speed_test,
            shortcuts,
            capture,
            capture_notification,
            capture_follow_up,
            processes,
            listeners,
            screen_tools,
            camera,
            recording,
            keyboard_light,
        } = update;
        if let Some((id, notification_id)) = capture_notification {
            self.notification_captures
                .borrow_mut()
                .insert(notification_id, id);
        }
        for event in self.capture_notifier.take_events() {
            match event {
                NotificationEvent::ActionInvoked {
                    notification_id,
                    action,
                } => {
                    if let Some(id) = self.notification_captures.borrow().get(&notification_id) {
                        let action = match action {
                            CaptureNotificationAction::Open => kestrel::CaptureFollowUpAction::Open,
                            CaptureNotificationAction::Edit => kestrel::CaptureFollowUpAction::Edit,
                            CaptureNotificationAction::Copy => kestrel::CaptureFollowUpAction::Copy,
                        };
                        let _ = self
                            .commands
                            .try_send(ApplicationCommand::CaptureFollowUp { id: *id, action });
                    }
                }
                NotificationEvent::NotificationClosed { notification_id } => {
                    self.notification_captures
                        .borrow_mut()
                        .remove(&notification_id);
                }
            }
        }
        if let Some((id, after)) = capture_follow_up {
            if after == kestrel::CaptureAfterScreenshot::Edit {
                let _ = self.commands.try_send(ApplicationCommand::CaptureFollowUp {
                    id,
                    action: kestrel::CaptureFollowUpAction::Edit,
                });
            }
        }
        if let Some(screen_tools) = &screen_tools {
            self.screen_tools_running.set(screen_tools.available);
            self.selection_pending.set(screen_tools.selecting);
            let needs_window = screen_tools.selecting.is_some() || screen_tools.result.is_some();
            if needs_window && self.window.borrow().is_none() {
                // A command-line request finished with no window; the next tick shows it.
                let _ = self.commands.try_send(ApplicationCommand::PresentWindow);
            }
        }
        self.open_pending_selection();
        if let Some(recording) = &recording {
            self.recording_running.set(recording.running);
            if !recording.choosing_area {
                // The worker took the answer; a later choice opens a new view.
                self.recording_area_open.set(false);
            } else if self.window.borrow().is_none() {
                let _ = self.commands.try_send(ApplicationCommand::PresentWindow);
            }
        }
        self.open_recording_area();
        if let Some(microphone) = &microphone {
            self.overlay.set_microphone(microphone);
        }
        if let Some(audio) = &audio {
            self.overlay.set_audio(audio);
        }
        if let Some(keyboard_light) = &keyboard_light {
            self.overlay.set_keyboard_light(keyboard_light);
        }
        if let Some(recording) = &recording {
            self.overlay.set_recording(recording);
        }
        if let Some(monitor) = &monitor {
            self.overlay.set_monitor(monitor);
        }
        if let (Some(command_bar), Some(surface)) = (&command_bar, self.floating.borrow().as_ref())
        {
            surface.set_command_bar(command_bar);
        }
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        if let Some(screen_tools) = screen_tools {
            view.set_screen_tools(&screen_tools);
        }
        if let Some(camera) = camera {
            self.camera_running.set(camera.running);
            view.set_camera(&camera);
        }
        if let Some(recording) = recording {
            view.set_recording(&recording);
        }
        if let Some(monitor) = monitor {
            self.monitor_running.set(monitor.running);
            view.set_monitor(&monitor);
        }
        if let Some(clipboard) = clipboard {
            self.clipboard_running.set(clipboard.running);
            view.set_clipboard(&clipboard);
        }
        if let Some(command_bar) = command_bar {
            view.set_command_bar(&command_bar);
        }
        if let Some(audio) = audio {
            view.set_audio(&audio);
        }
        if let Some(microphone) = microphone {
            view.set_microphone(&microphone);
        }
        if let Some(speed_test) = speed_test {
            view.set_speed_test(&speed_test);
        }
        if let Some(shortcuts) = shortcuts {
            view.set_shortcuts(&shortcuts);
        }
        if let Some(capture) = capture {
            self.capture_running.set(capture.running);
            view.set_capture(&capture);
        }
        if let Some(processes) = processes {
            self.processes_running.set(processes.running);
            view.set_processes(&processes);
        }
        if let Some(listeners) = listeners {
            self.listeners_running.set(listeners.running);
            view.set_listeners(&listeners);
        }
    }

    /// Runs a screen tool request on the GTK thread; every call only signals the worker.
    fn screen_tool_request(&self, request: kestrel::ScreenToolRequest) {
        if matches!(
            request,
            kestrel::ScreenToolRequest::Select(_) | kestrel::ScreenToolRequest::Cancel
        ) {
            // Only the selection view sends these, once, as it closes.
            self.selection_open.set(false);
        }
        let (result, screen_tools) = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = match request {
                kestrel::ScreenToolRequest::Begin(tool) => guard.runtime.begin_screen_tool(tool),
                kestrel::ScreenToolRequest::Select(selection) => {
                    guard.runtime.select_screen_region(selection)
                }
                kestrel::ScreenToolRequest::Cancel => {
                    guard.runtime.cancel_screen_tool();
                    Ok(())
                }
                kestrel::ScreenToolRequest::Dismiss => {
                    guard.runtime.dismiss_screen_tool();
                    Ok(())
                }
            };
            (result, guard.runtime.screen_tools_view_model())
        };
        self.screen_tools_running.set(screen_tools.available);
        self.selection_pending.set(screen_tools.selecting);
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_screen_tools(&screen_tools);
            if let Err(error) = result {
                view.show_message(&error);
            }
        }
    }

    /// Starts or stops the preview on the GTK thread; stopping joins the
    /// camera worker, which waits at most one frame interval.
    fn camera_request(&self, request: kestrel::CameraRequest) {
        let (result, camera) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = match request {
                kestrel::CameraRequest::Start(path) => guard.runtime.start_camera(&path),
                kestrel::CameraRequest::Stop => {
                    guard.runtime.stop_camera();
                    Ok(())
                }
            };
            (result, guard.runtime.camera_view_model())
        };
        self.camera_running.set(camera.running);
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_camera(&camera);
            if let Err(error) = result {
                view.show_message(&error);
            }
        }
    }

    /// Runs a recording request on the GTK thread; requests only signal the
    /// recording worker.
    fn recording_request(&self, request: kestrel::RecordingRequest) {
        // The area view stays marked open until a tick shows the worker left
        // ChoosingArea, so it is never opened twice for one frame.
        let (result, recording) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = guard.runtime.recording_request(request);
            (result, guard.runtime.recording_view_model())
        };
        self.recording_running.set(recording.running);
        self.overlay.set_recording(&recording);
        if !recording.choosing_area {
            self.recording_area_open.set(false);
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_recording(&recording);
            if let Err(error) = result {
                view.show_message(&error);
            }
        }
    }

    fn open_trim(&self, take: std::path::PathBuf) {
        let trim = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .recording_trim(&take);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        match trim {
            Ok((duration, preview)) => view.open_trim(take, duration, preview),
            Err(error) => view.show_message(&error),
        }
    }

    /// Opens the area view while the recording waits for an area.
    fn open_recording_area(&self) {
        if self.recording_area_open.get() {
            return;
        }
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        let frame = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .recording_area_frame();
        let Some(frame) = frame else {
            return;
        };
        self.recording_area_open.set(true);
        let commands = self.commands.clone();
        screen_selection::open(
            &view.window,
            screen_selection::SelectionPurpose::RECORD_AREA,
            frame,
            move |choice| {
                let request = match choice {
                    Some(kestrel::Selection::Region(region)) => {
                        kestrel::RecordingRequest::ChooseArea(region)
                    }
                    Some(kestrel::Selection::Pixel { .. }) | None => {
                        kestrel::RecordingRequest::Cancel
                    }
                };
                let _ = commands.try_send(ApplicationCommand::Recording(request));
            },
        );
    }

    /// Opens the selection view for a waiting frame once a window exists.
    fn open_pending_selection(&self) {
        let Some(tool) = self.selection_pending.get() else {
            return;
        };
        if self.selection_open.get() {
            return;
        }
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        let frame = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .screen_tool_frame();
        let Some(frame) = frame else {
            return;
        };
        self.selection_open.set(true);
        let commands = self.commands.clone();
        screen_selection::open(
            &view.window,
            screen_selection::SelectionPurpose::for_tool(tool),
            frame,
            move |choice| {
                let request = choice.map_or(
                    kestrel::ScreenToolRequest::Cancel,
                    kestrel::ScreenToolRequest::Select,
                );
                let _ = commands.try_send(ApplicationCommand::ScreenTools(request));
            },
        );
    }

    fn confirm_process(
        &self,
        identity: kestrel_platform::processes::ProcessIdentity,
        action: kestrel::ProcessActionKind,
    ) {
        let result = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .runtime
            .process_confirmation(identity, action);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        let confirmation = match result {
            Ok(confirmation) => confirmation,
            Err(error) => {
                view.show_message(&error);
                return;
            }
        };
        let (heading, confirm) = match action {
            kestrel::ProcessActionKind::Terminate => ("End process?", "End"),
            kestrel::ProcessActionKind::Kill => ("Kill process?", "Kill"),
            kestrel::ProcessActionKind::Restart { .. } => ("Restart?", "Restart"),
        };
        let dialog = adw::MessageDialog::builder()
            .transient_for(&view.window)
            .heading(heading)
            .body(confirmation.scope)
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("confirm", confirm)]);
        if confirmation.can_escalate {
            // Escalation sends SIGKILL to members still running after the deadline.
            dialog.add_response("force", "Force restart");
            dialog.set_response_appearance("force", adw::ResponseAppearance::Destructive);
        }
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        let commands = self.commands.clone();
        dialog.connect_response(None, move |dialog, response| {
            let action = match (response, action) {
                ("confirm", action) => Some(action),
                ("force", kestrel::ProcessActionKind::Restart { .. }) => {
                    Some(kestrel::ProcessActionKind::Restart { escalate: true })
                }
                _ => None,
            };
            if let Some(action) = action {
                let _ = commands.try_send(ApplicationCommand::Processes(
                    kestrel::ProcessRequest::Run { identity, action },
                ));
            }
            dialog.close();
        });
        dialog.present();
    }

    /// Filters the process list on the GTK thread; rows are already bounded.
    fn search_processes(&self, query: String) {
        let processes = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.process_query = query;
            guard.runtime.processes_view_model(&guard.process_query)
        };
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_processes(&processes);
        }
    }

    fn update_scratchpad(&self, command: kestrel::ScratchpadCommand) {
        let model = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let kestrel::ScratchpadCommand::Update { id, text } = command else {
                return;
            };
            if let Err(error) = guard.runtime.scratchpad_update(id, text) {
                drop(guard);
                self.show_message(&error);
                return;
            }
            guard.runtime.scratchpad_view_model()
        };
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_scratchpad_preview(&model);
        }
    }

    /// Runs a confirmed action on a worker without holding the controller
    /// lock, since a termination waits for its grace period.
    fn request_process_action(
        &self,
        identity: kestrel_platform::processes::ProcessIdentity,
        action: kestrel::ProcessActionKind,
    ) {
        if self.refreshing.replace(true) {
            if let Some(view) = self.window.borrow().as_ref() {
                view.show_message("Another Kestrel operation is still running");
            }
            return;
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_refreshing(true);
        }
        self.targeted.set(TargetedPanel::Processes);
        let state = Arc::clone(&self.state);
        let results = self.refresh_results.clone();
        let worker = thread::Builder::new()
            .name("kestrel-process-action".to_owned())
            .spawn(move || {
                let running = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .runtime
                    .processes_is_running();
                let message = if running {
                    kestrel::run_process_action(identity, action)
                } else {
                    "Processes are not running; enable system.processes in the Feature Hub."
                        .to_owned()
                };
                let guard = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = guard.runtime.refresh_processes();
                let _ = results.send_blocking(Ok((guard.view_model(), message)));
            });
        if let Err(error) = worker {
            log::warn!("kestrel-process-action could not start");
            self.finish_refresh(Err(format!(
                "Could not start the process action worker: {error}"
            )));
        }
    }
    fn current_view_model(&self) -> ApplicationViewModel {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view_model()
    }
}

impl ControllerState {
    fn run(&mut self, operation: ControllerOperation, config_path: Option<&Path>) -> RefreshResult {
        let logging_before = self.configuration.logging;
        let message = match operation {
            ControllerOperation::Refresh => {
                self.runtime
                    .refresh_capabilities()
                    .map_err(|error| format!("Capability refresh failed: {error:?}"))?;
                "Capability and toggle state refreshed".to_owned()
            }
            ControllerOperation::QuickToggle(command) => {
                let label = command.id.label();
                match self.runtime.execute_quick_toggle_command(command) {
                    Some(Ok(_)) => format!("{label} updated"),
                    Some(Err(error)) => format!("{label} failed: {error}"),
                    None => format!("{label} is not available"),
                }
            }
            ControllerOperation::FlipQuickToggle(id) => self.flip_quick_toggle(id),
            ControllerOperation::SetFeatureEnabled {
                feature_id,
                enabled,
            } => {
                let label = self
                    .runtime
                    .registrations()
                    .find(|registration| registration.feature.id == feature_id)
                    .map_or_else(
                        || feature_id.clone(),
                        |registration| registration.feature.label.to_owned(),
                    );
                let old = self.configuration.clone();
                let mut candidate = old.clone();
                if let Err(error) = candidate.set_feature_enabled(feature_id.clone(), enabled) {
                    return Err(format!("Could not change {label}: {error:?}"));
                }
                self.runtime
                    .apply_configuration(&candidate)
                    .map_err(|error| {
                        self.restore_configuration(old.clone());
                        format!("Could not apply the change to {label}: {error:?}")
                    })?;
                if let Err(error) = persist(config_path, &candidate) {
                    self.restore_configuration(old);
                    return Err(format!("Feature change was not saved: {error}"));
                }
                self.configuration = candidate;
                self.preset_snapshot = None;
                format!("{label} is {}", if enabled { "on" } else { "off" })
            }
            ControllerOperation::ApplyPreset(preset) => {
                let old = self.configuration.clone();
                let snapshot = self
                    .runtime
                    .apply_preset(&mut self.configuration, preset)
                    .map_err(|error| {
                        self.restore_configuration(old.clone());
                        format!("Could not apply the {} preset: {error:?}", preset.label())
                    })?;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.restore_configuration(old);
                    return Err(format!("Preset was not saved: {error}"));
                }
                self.preset_snapshot = Some(snapshot);
                format!("{} preset applied", preset.label())
            }
            ControllerOperation::FinishSetup { preset, left_off } => {
                let old = self.configuration.clone();
                let snapshot = self
                    .runtime
                    .apply_setup(&mut self.configuration, preset, &left_off)
                    .map_err(|error| {
                        self.restore_configuration(old.clone());
                        format!("Could not apply setup: {error:?}")
                    })?;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.restore_configuration(old);
                    return Err(format!("Setup was not saved: {error}"));
                }
                if preset.is_some() {
                    self.preset_snapshot = Some(snapshot);
                }
                setup::record_done(setup::state_path().as_deref());
                match preset {
                    Some(preset) => format!("{} is on. Undo it in Settings.", preset.label()),
                    None => {
                        let _ = self
                            .commands
                            .try_send(ApplicationCommand::Focus(FocusTarget::FeatureHub));
                        String::new()
                    }
                }
            }
            ControllerOperation::SkipSetup => {
                setup::record_done(setup::state_path().as_deref());
                String::new()
            }
            ControllerOperation::UndoPreset => {
                let Some(snapshot) = self.preset_snapshot.take() else {
                    return Err("There is no feature preset to undo".to_owned());
                };
                let old = self.configuration.clone();
                let snapshot_for_error = snapshot.clone();
                if let Err(error) = self.runtime.undo_preset(&mut self.configuration, snapshot) {
                    self.preset_snapshot = Some(snapshot_for_error);
                    self.restore_configuration(old);
                    return Err(format!("Could not undo the feature preset: {error:?}"));
                }
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.preset_snapshot = Some(snapshot_for_error);
                    self.restore_configuration(old);
                    return Err(format!("Preset undo was not saved: {error}"));
                }
                "Feature preset undone".to_owned()
            }
            ControllerOperation::SetAppearance(appearance) => {
                let old = self.configuration.clone();
                self.configuration.ui.appearance = appearance;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("Appearance change was not saved: {error}"));
                }

                "Appearance updated".to_owned()
            }
            ControllerOperation::SetCloseAction(close_action) => {
                let old = self.configuration.clone();
                self.configuration.ui.close_action = close_action;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("The close button setting was not saved: {error}"));
                }
                match close_action {
                    CloseAction::Minimize => "The close button minimizes Kestrel".to_owned(),
                    CloseAction::Quit => "The close button quits Kestrel".to_owned(),
                }
            }
            ControllerOperation::SetAutostart(enabled) => {
                self.set_autostart(enabled, config_path)?;
                format!("Autostart {}", if enabled { "enabled" } else { "disabled" })
            }
            ControllerOperation::SetWhatsNew(enabled) => {
                let old = self.configuration.clone();
                self.configuration.startup.whats_new = enabled;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("The what's new setting was not saved: {error}"));
                }
                if enabled {
                    "Overview offers what's new after an update".to_owned()
                } else {
                    "Overview no longer offers what's new after an update".to_owned()
                }
            }
            ControllerOperation::MarkWhatsNewRead => {
                self.runtime.mark_whats_new_read();
                String::new()
            }
            ControllerOperation::SetMonitorReadoutVisible { readout, visible } => {
                let mut candidate = self.configuration.clone();
                if visible {
                    if !candidate.monitoring.readouts.contains(&readout) {
                        candidate.monitoring.readouts.push(readout);
                    }
                } else {
                    candidate
                        .monitoring
                        .readouts
                        .retain(|entry| *entry != readout);
                }
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "{} readout {}",
                        readout.label(),
                        if visible { "shown" } else { "hidden" }
                    ),
                )?
            }
            ControllerOperation::SetMonitorSetting(setting) => {
                let mut candidate = self.configuration.clone();
                if !setting.apply(&mut candidate.monitoring) {
                    return Ok((
                        self.view_model(),
                        "Monitoring settings unchanged".to_owned(),
                    ));
                }
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Monitoring settings updated".to_owned(),
                )?
            }
            ControllerOperation::ShowLocalAddresses(shown) => {
                self.runtime.set_local_addresses_shown(shown);
                String::new()
            }
            ControllerOperation::MoveMonitorReadout { readout, direction } => {
                let mut candidate = self.configuration.clone();
                if !kestrel::move_entry(&mut candidate.monitoring.readouts, &readout, direction) {
                    return Ok((
                        self.view_model(),
                        format!("{} readout is already at that boundary", readout.label()),
                    ));
                }
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Monitor readout order updated".to_owned(),
                )?
            }
            ControllerOperation::SetAlertEnabled { kind, enabled } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.alerts.rule_mut(kind).enabled = enabled;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "{} alerts {}",
                        kind.label(),
                        if enabled { "enabled" } else { "disabled" }
                    ),
                )?
            }
            ControllerOperation::SetAlertThreshold { kind, threshold } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.alerts.rule_mut(kind).threshold = threshold;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("{} alert threshold updated", kind.label()),
                )?
            }
            ControllerOperation::SetAlertSustain { kind, duration } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.set_alert_sustain(kind, duration);
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("{} alert duration updated", kind.label()),
                )?
            }
            ControllerOperation::Audio(command) => {
                let summary = audio_command_summary(command);
                match self.runtime.execute_audio_command(command) {
                    Some(Ok(_)) => format!("{summary} updated"),
                    Some(Err(failure)) => format!("{summary} failed: {}", failure.error),
                    None => "The audio mixer is not running; enable audio.mixer first".to_owned(),
                }
            }
            ControllerOperation::Microphone(command) => {
                match self.runtime.execute_microphone_command(command) {
                    Some(Ok(_)) => "Microphone state updated".to_owned(),
                    Some(Err(failure)) => format!("Microphone update failed: {}", failure.error),
                    None => "The microphone control is not running; enable audio.microphone first"
                        .to_owned(),
                }
            }
            ControllerOperation::StartSpeedTest => self
                .runtime
                .start_speed_test()
                .map(|()| "Network speed test started".to_owned())
                .unwrap_or_else(|error| error),
            ControllerOperation::CancelSpeedTest => self
                .runtime
                .cancel_speed_test()
                .map(|()| "Network speed test cancellation requested".to_owned())
                .unwrap_or_else(|error| error),
            ControllerOperation::RefreshProcesses => self
                .runtime
                .refresh_processes()
                .map(|()| "Process list refresh requested".to_owned())
                .unwrap_or_else(|error| error),
            ControllerOperation::Capture(operation) => match operation {
                CaptureOperation::Begin(mode) => self
                    .runtime
                    .begin_capture(mode)
                    .map(|()| "Screenshot started".to_owned())
                    .unwrap_or_else(|error| error),
                CaptureOperation::Cancel => {
                    self.runtime.cancel_capture();
                    "Screenshot cancellation requested".to_owned()
                }
                CaptureOperation::Save { id, destination } => self
                    .runtime
                    .export_capture(id, &destination)
                    .map(|()| {
                        format!(
                            "Saved {}",
                            destination
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|error| error),
                CaptureOperation::SaveEdit { id, plan } => self
                    .runtime
                    .save_capture_edit(id, &plan)
                    .map(|()| "Edited copy added to recent captures".to_owned())
                    .unwrap_or_else(|error| error),
                CaptureOperation::Delete(id) => self
                    .runtime
                    .delete_capture(id)
                    .map(|()| "Capture deleted".to_owned())
                    .unwrap_or_else(|error| error),
                CaptureOperation::Clear => self
                    .runtime
                    .clear_captures()
                    .map(|()| "Recent captures cleared".to_owned())
                    .unwrap_or_else(|error| error),
            },
            ControllerOperation::SetAudioBoostPercent(percent) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.boost_percent = percent;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Audio boost ceiling set to {percent}%"),
                )?
            }
            ControllerOperation::SetAudioOutputSwitch(mode) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.output_switch = mode;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Output switching set to {}", mode.label()),
                )?
            }
            ControllerOperation::SetAudioDisconnectPolicy(policy) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.disconnect_policy = policy;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Disconnect policy set to {}", policy.label()),
                )?
            }
            ControllerOperation::SetAudioDisconnectVolumePercent(percent) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.disconnect_volume_percent = percent;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Disconnect volume set to {percent}%"),
                )?
            }
            ControllerOperation::SetAudioIncludeInactiveStreams(include) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.include_inactive_streams = include;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Inactive streams {}",
                        if include { "shown" } else { "hidden" }
                    ),
                )?
            }
            ControllerOperation::AudioPreference(edit) => {
                let mut candidate = self.configuration.clone();
                if !edit.apply(&mut candidate.audio) {
                    return Ok((self.view_model(), "Audio preferences unchanged".to_owned()));
                }
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Audio preferences updated".to_owned(),
                )?
            }
            ControllerOperation::Clipboard(command) => {
                let summary = clipboard_command_summary(&command);
                match self.runtime.execute_clipboard_command(command) {
                    Some(Ok(snapshot)) => {
                        self.refresh_clipboard_matches();
                        format!("{summary} · {} entries retained", snapshot.items.len())
                    }
                    Some(Err(error)) => format!("{summary} failed: {error}"),
                    None => "Clipboard history is not running; enable clipboard.history first"
                        .to_owned(),
                }
            }
            ControllerOperation::ClipboardSearch(query) => {
                self.clipboard_query = query.clone();
                self.clipboard_preview = None;
                match self
                    .runtime
                    .clipboard_search(&query, CLIPBOARD_SEARCH_LIMIT)
                {
                    Some(Ok(matches)) => {
                        let shown = matches.len();
                        self.clipboard_matches = matches;
                        if query.trim().is_empty() {
                            // The initial listing shows its count in the window.
                            String::new()
                        } else {
                            format!("{shown} entries match \"{query}\"")
                        }
                    }
                    Some(Err(error)) => {
                        self.clipboard_matches.clear();
                        format!("Clipboard search failed: {error}")
                    }
                    None => {
                        self.clipboard_matches.clear();
                        "Clipboard history is not running".to_owned()
                    }
                }
            }
            ControllerOperation::ClipboardCopyImageText(item_id) => {
                match self.runtime.copy_clipboard_image_text(item_id) {
                    Ok(characters) => {
                        format!("Copied {characters} characters of text from the image")
                    }
                    Err(message) => message,
                }
            }
            ControllerOperation::SetClipboardLayout(layout) => {
                let mut candidate = self.configuration.clone();
                candidate.clipboard.history_layout = layout;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Clipboard history shown as {}",
                        layout.label().to_lowercase()
                    ),
                )?
            }
            ControllerOperation::ClipboardPreview(item_id) => {
                self.clipboard_preview = None;
                match self
                    .runtime
                    .clipboard_preview(item_id, CLIPBOARD_PREVIEW_BYTES)
                {
                    Some(Ok(preview)) => {
                        let truncated = if preview.truncated {
                            " (truncated)"
                        } else {
                            ""
                        };
                        self.clipboard_preview = Some(preview);
                        format!("Entry preview loaded{truncated}")
                    }
                    Some(Err(error)) => format!("Entry preview failed: {error}"),
                    None => "Clipboard history is not running".to_owned(),
                }
            }
            ControllerOperation::SetClipboardLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    kestrel::ClipboardLimit::Items(items) => {
                        candidate.clipboard.max_items = items;
                        format!("Clipboard retains at most {items} entries")
                    }
                    kestrel::ClipboardLimit::ItemBytes(bytes) => {
                        candidate.clipboard.max_item_bytes = bytes;
                        format!("Clipboard text entries are bounded to {bytes} bytes")
                    }
                    kestrel::ClipboardLimit::ImageBytes(bytes) => {
                        candidate.clipboard.max_image_bytes = bytes;
                        format!("Clipboard image entries are bounded to {bytes} bytes")
                    }
                    kestrel::ClipboardLimit::FileEntries(entries) => {
                        candidate.clipboard.max_file_entries = entries;
                        format!("Clipboard file entries are bounded to {entries} paths")
                    }
                    kestrel::ClipboardLimit::MaxAgeHours(hours) => {
                        candidate.clipboard.max_age_hours = hours;
                        format!("Clipboard entries expire after {hours} hours")
                    }
                    kestrel::ClipboardLimit::ClearSeconds(seconds) => {
                        candidate.clipboard.clear_seconds = seconds;
                        if seconds == 0 {
                            "Automatic selection clearing is disabled".to_owned()
                        } else {
                            format!("The live selection is cleared after {seconds} seconds")
                        }
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetCaptureLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    kestrel::CaptureLimit::MaxEntries(entries) => {
                        candidate.capture.max_entries = entries;
                        format!("Recent captures keep at most {entries} images")
                    }
                    kestrel::CaptureLimit::MaxTotalMegabytes(megabytes) => {
                        candidate.capture.max_total_megabytes = megabytes;
                        format!("Recent captures use at most {megabytes} MiB")
                    }
                    kestrel::CaptureLimit::MaxAgeHours(hours) => {
                        candidate.capture.max_age_hours = hours;
                        format!("Captures are removed after {hours} hours")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetCaptureAfterScreenshot(after) => {
                let mut candidate = self.configuration.clone();
                candidate.capture.after_screenshot = after;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("After a screenshot: {}", after.label()),
                )?
            }
            ControllerOperation::SetProcessLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    kestrel::ProcessLimit::RefreshMillis(millis) => {
                        candidate.processes.refresh_millis = millis;
                        format!("Processes refresh every {millis} ms")
                    }
                    kestrel::ProcessLimit::MaxRows(rows) => {
                        candidate.processes.max_rows = rows;
                        format!("Processes show at most {rows} rows")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetLoggingSetting(setting) => {
                let mut candidate = self.configuration.clone();
                let message = match setting {
                    kestrel::LoggingSetting::Debug(true) => {
                        candidate.logging.debug = true;
                        "Debug logging is on".to_owned()
                    }
                    kestrel::LoggingSetting::Debug(false) => {
                        candidate.logging.debug = false;
                        "Debug logging is off".to_owned()
                    }
                    kestrel::LoggingSetting::RetentionDays(days) => {
                        candidate.logging.retention_days = days;
                        format!("Logs are kept for {days} days")
                    }
                    kestrel::LoggingSetting::MaxTotalMegabytes(megabytes) => {
                        candidate.logging.max_total_megabytes = megabytes;
                        format!("Logs use at most {megabytes} MB")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetOverlayModule { module, shown } => {
                let mut candidate = self.configuration.clone();
                let name = match module {
                    kestrel::OverlayModule::Microphone => {
                        candidate.overlay.microphone = shown;
                        "The muted microphone"
                    }
                    kestrel::OverlayModule::Recording => {
                        candidate.overlay.recording = shown;
                        "A running recording"
                    }
                    kestrel::OverlayModule::Alerts => {
                        candidate.overlay.alerts = shown;
                        "The alerts module"
                    }
                    kestrel::OverlayModule::Volume => {
                        candidate.overlay.volume = shown;
                        "The volume control"
                    }
                    kestrel::OverlayModule::MicrophoneLevel => {
                        candidate.overlay.microphone_level = shown;
                        "The microphone level control"
                    }
                    kestrel::OverlayModule::KeyboardLight => {
                        candidate.overlay.keyboard_light = shown;
                        "The keyboard light control"
                    }
                    kestrel::OverlayModule::Accessories => {
                        candidate.overlay.accessories = shown;
                        "The accessory batteries module"
                    }
                };
                let message = format!(
                    "{name} {} in the status overlay",
                    if shown { "shows" } else { "no longer shows" }
                );
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetAccessoryLevels { warning, critical } => {
                if !kestrel_core::accessory_levels_valid(critical, warning) {
                    return Err(
                        "The critical level must be lower than the warning level".to_owned()
                    );
                }
                let mut candidate = self.configuration.clone();
                candidate.overlay.accessory_warning = warning;
                candidate.overlay.accessory_critical = critical;
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Accessory battery levels updated".to_owned(),
                )?
            }
            ControllerOperation::ClearLogs => match kestrel::logging::logger() {
                Some(logger) => match logger.clear() {
                    Ok(removed) => format!("Removed {removed} log file(s)"),
                    Err(error) => return Err(format!("The logs were not cleared: {error}")),
                },
                None => "Logging is unavailable without a home folder".to_owned(),
            },
            ControllerOperation::SetShortcut { command, trigger } => {
                let describe = |command: &str| {
                    CommandLineAction::parse(command)
                        .map_or_else(|_| command.to_owned(), |action| action.description())
                };
                let mut candidate = self.configuration.clone();
                if let Err(error) = candidate.shortcuts.set_binding(&command, trigger.as_ref()) {
                    return Err(match error {
                        kestrel_core::ShortcutEditError::InUse { command: holder } => format!(
                            "{} is already set for “{}”",
                            trigger
                                .as_ref()
                                .map(kestrel::shortcut_label)
                                .unwrap_or_default(),
                            describe(&holder)
                        ),
                        kestrel_core::ShortcutEditError::Full => format!(
                            "Kestrel keeps at most {} shortcuts; remove one first",
                            kestrel_core::MAX_SHORTCUT_BINDINGS
                        ),
                    });
                }
                let message = match &trigger {
                    Some(trigger) => format!(
                        "{} now runs “{}”",
                        kestrel::shortcut_label(trigger),
                        describe(&command)
                    ),
                    None => format!("Removed the shortcut for “{}”", describe(&command)),
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetScratchpadLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    kestrel::ScratchpadLimit::MaxNotes(notes) => {
                        candidate.scratchpad.max_notes = notes;
                        format!("The scratchpad keeps at most {notes} notes")
                    }
                    kestrel::ScratchpadLimit::MaxNoteBytes(bytes) => {
                        candidate.scratchpad.max_note_bytes = bytes;
                        format!("Scratchpad notes hold at most {} KiB", bytes / 1024)
                    }
                    kestrel::ScratchpadLimit::MaxAgeDays(0) => {
                        candidate.scratchpad.max_age_days = 0;
                        "Scratchpad notes never expire".to_owned()
                    }
                    kestrel::ScratchpadLimit::MaxAgeDays(days) => {
                        candidate.scratchpad.max_age_days = days;
                        format!("Scratchpad notes expire after {days} days")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetScreenToolsSetting(setting) => {
                let mut candidate = self.configuration.clone();
                let message = match setting {
                    kestrel::ScreenToolsSetting::OcrLanguage(language) => {
                        let message = format!("Text is read with the {language} language data");
                        candidate.screen_tools.ocr_language = language;
                        message
                    }
                    kestrel::ScreenToolsSetting::OcrTimeoutSeconds(seconds) => {
                        candidate.screen_tools.ocr_timeout_seconds = seconds;
                        format!("Reading text stops after {seconds} s")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetRecordingSetting(setting) => {
                let mut candidate = self.configuration.clone();
                let message = match setting {
                    kestrel::RecordingSetting::Framerate(framerate) => {
                        candidate.recording.framerate = framerate;
                        format!("Recordings use {framerate} frames per second")
                    }
                    kestrel::RecordingSetting::Cursor(cursor) => {
                        candidate.recording.cursor = cursor;
                        format!(
                            "The pointer is {} recordings",
                            if cursor { "drawn into" } else { "left out of" }
                        )
                    }
                    kestrel::RecordingSetting::Track(track, enabled) => {
                        match track {
                            kestrel::AudioTrack::System => {
                                candidate.recording.system_audio = enabled
                            }
                            kestrel::AudioTrack::Microphone => {
                                candidate.recording.microphone = enabled;
                            }
                        }
                        format!(
                            "{} is {} recordings",
                            track.label(),
                            if enabled { "added to" } else { "left out of" }
                        )
                    }
                    kestrel::RecordingSetting::SystemAudioApplication(application) => {
                        let message = if application.is_some() {
                            "System audio records one application"
                        } else {
                            "System audio records all apps"
                        };
                        candidate.recording.system_audio_app = application;
                        message.to_owned()
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetClipboardFilterSensitive(filter) => {
                let mut candidate = self.configuration.clone();
                candidate.clipboard.filter_sensitive = filter;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Sensitive-pattern filtering {}",
                        if filter { "enabled" } else { "disabled" }
                    ),
                )?
            }
            ControllerOperation::SetClipboardPastePlainText(plain) => {
                let mut candidate = self.configuration.clone();
                candidate.clipboard.paste_plain_text = plain;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Quick paste copies {}",
                        if plain {
                            "plain text"
                        } else {
                            "the original form"
                        }
                    ),
                )?
            }
            ControllerOperation::Snippet(command) => match command {
                SnippetCommand::Insert(name) => match self.runtime.insert_snippet(&name) {
                    Ok(report) => {
                        let mut message = format!(
                            "Inserted \"{name}\" ({} bytes via {})",
                            report.bytes,
                            report.provider.label()
                        );
                        if report.clipboard_truncated {
                            message.push_str(" · clipboard variable clipped to the bound");
                        }
                        if report.clipboard_empty {
                            message.push_str(" · the live selection was empty");
                        }
                        message
                    }
                    Err(error) => format!("Insert failed: {error}"),
                },
                SnippetCommand::Save {
                    name,
                    folder,
                    trigger,
                    content,
                } => {
                    let snippet = kestrel_core::Snippet::new(
                        name.trim().to_string(),
                        optional_field(folder),
                        optional_field(trigger),
                        content,
                    );
                    let label = snippet.name.clone();
                    match self.runtime.save_snippet(snippet) {
                        Ok(()) => {
                            self.refresh_snippet_matches();
                            format!("Snippet \"{label}\" saved")
                        }
                        Err(error) => format!("Snippet was not saved: {error}"),
                    }
                }
                SnippetCommand::Delete(name) => match self.runtime.delete_snippet(&name) {
                    Ok(()) => {
                        self.refresh_snippet_matches();
                        if self
                            .snippet_draft
                            .as_ref()
                            .is_some_and(|draft| draft.name == name)
                        {
                            self.snippet_draft = None;
                        }
                        format!("Snippet \"{name}\" deleted")
                    }
                    Err(error) => format!("Snippet was not deleted: {error}"),
                },
            },
            ControllerOperation::SnippetSearch(query) => {
                self.snippet_query = query.clone();
                self.refresh_snippet_matches();
                if query.trim().is_empty() {
                    String::new()
                } else {
                    format!("{} snippets match \"{query}\"", self.snippet_matches.len())
                }
            }
            ControllerOperation::SnippetEdit(name) => {
                match self.runtime.snippet_library().get(&name) {
                    Some(snippet) => {
                        self.snippet_draft = Some(SnippetDraft {
                            name: snippet.name.clone(),
                            folder: snippet.folder.clone().unwrap_or_default(),
                            trigger: snippet.trigger.clone().unwrap_or_default(),
                            content: snippet.content.clone(),
                        });
                        format!("Editing \"{name}\"")
                    }
                    None => format!("No snippet named \"{name}\" is stored"),
                }
            }
            ControllerOperation::SnippetNew => {
                self.snippet_draft = Some(SnippetDraft::default());
                "New snippet editor opened".to_owned()
            }
            ControllerOperation::SetSnippetLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    SnippetLimit::ContentBytes(bytes) => {
                        candidate.snippets.max_content_bytes = bytes;
                        format!("Snippet content is bounded to {bytes} bytes")
                    }
                    SnippetLimit::ClipboardBytes(bytes) => {
                        candidate.snippets.clipboard_variable_bytes = bytes;
                        format!("The clipboard variable is bounded to {bytes} bytes")
                    }
                    SnippetLimit::InsertTimeoutMillis(millis) => {
                        candidate.snippets.insert_timeout_millis = millis;
                        format!("Insertion times out after {millis} ms")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetSnippetProvider(preference) => {
                let mut candidate = self.configuration.clone();
                candidate.snippets.preferred_provider = preference;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Insertion provider preference: {}", preference.label()),
                )?
            }
            ControllerOperation::SetSnippetExpansionTiming(timing) => {
                let mut candidate = self.configuration.clone();
                candidate.snippets.expansion_timing = timing;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Expansion timing: {}", timing.label()),
                )?
            }
            ControllerOperation::CommandQuery(query) => {
                self.command_query = query.clone();
                self.refresh_command_results();
                if query.trim().is_empty() {
                    String::new()
                } else {
                    format!("{} command results", self.command_results.len())
                }
            }
            ControllerOperation::CommandBar(command) => match command {
                CommandBarCommand::Run(id) => self.run_command_result(&id),
                CommandBarCommand::Pin { id, pinned } => {
                    match self.runtime.set_command_pinned(&id, pinned) {
                        Ok(()) => {
                            self.refresh_command_results();
                            format!("{} \"{id}\"", if pinned { "Pinned" } else { "Unpinned" })
                        }
                        Err(error) => format!("The pin was not saved: {error}"),
                    }
                }
            },
            ControllerOperation::ResetCommandRanking => {
                match self.runtime.reset_command_ranking() {
                    Ok(()) => {
                        self.refresh_command_results();
                        "Command ranking reset".to_owned()
                    }
                    Err(error) => format!("The ranking was not reset: {error}"),
                }
            }
            ControllerOperation::SetCommandResultLimit(results) => {
                let mut candidate = self.configuration.clone();
                candidate.command_bar.max_results = results;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("The command bar shows at most {results} results"),
                )?
            }
            ControllerOperation::SetQuickPanelFavorites(favorites) => {
                let mut candidate = self.configuration.clone();
                candidate.quick_panel.favorites = favorites;
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Quick panel favorites updated".to_owned(),
                )?
            }
            ControllerOperation::SetCommandProvider(switch) => {
                let mut candidate = self.configuration.clone();
                let message = match switch {
                    CommandProviderSwitch::Applications(enabled) => {
                        candidate.command_bar.enable_applications = enabled;
                        format!("Application results {}", if enabled { "on" } else { "off" })
                    }
                    CommandProviderSwitch::Files(enabled) => {
                        candidate.command_bar.enable_files = enabled;
                        if enabled && candidate.command_bar.file_roots.is_empty() {
                            "File search stays off until a root is configured".to_owned()
                        } else {
                            format!("File results {}", if enabled { "on" } else { "off" })
                        }
                    }
                    CommandProviderSwitch::Scripts(enabled) => {
                        candidate.command_bar.enable_scripts = enabled;
                        format!("Script results {}", if enabled { "on" } else { "off" })
                    }
                    CommandProviderSwitch::Emoji(enabled) => {
                        candidate.command_bar.enable_emoji = enabled;
                        format!("Emoji results {}", if enabled { "on" } else { "off" })
                    }
                    CommandProviderSwitch::Windows(enabled) => {
                        candidate.command_bar.enable_windows = enabled;
                        format!("Window results {}", if enabled { "on" } else { "off" })
                    }
                    CommandProviderSwitch::Clipboard(enabled) => {
                        candidate.command_bar.enable_clipboard = enabled;
                        format!("Clipboard results {}", if enabled { "on" } else { "off" })
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::Scratchpad(command) => match command {
                kestrel::ScratchpadCommand::New(title) => self
                    .runtime
                    .scratchpad_create(title)
                    .map(|_| "Scratchpad tab created".to_owned()),
                kestrel::ScratchpadCommand::Rename { id, title } => self
                    .runtime
                    .scratchpad_rename(id, title)
                    .map(|_| "Scratchpad tab renamed".to_owned()),
                kestrel::ScratchpadCommand::Switch(id) => self
                    .runtime
                    .scratchpad_switch(id)
                    .map(|_| "Scratchpad tab selected".to_owned()),
                kestrel::ScratchpadCommand::Close(id) => self
                    .runtime
                    .scratchpad_close(id)
                    .map(|_| "Scratchpad tab closed".to_owned()),
                kestrel::ScratchpadCommand::Update { id, text } => self
                    .runtime
                    .scratchpad_update(id, text)
                    .map(|_| String::new()),
                kestrel::ScratchpadCommand::RemoveExpired => self
                    .runtime
                    .scratchpad_remove_expired()
                    .map(|count| format!("Removed {count} expired scratchpad note(s)")),
                kestrel::ScratchpadCommand::Export { id, destination } => self
                    .runtime
                    .scratchpad_export(id, &destination)
                    .map(|_| format!("Saved {}", destination.display())),
            }
            .unwrap_or_else(|error| error),
            ControllerOperation::ImportConfiguration(path) => {
                let imported = import_file(&path)
                    .map_err(|error| format!("Could not import {}: {error}", path.display()))?;
                let old_configuration = self.configuration.clone();
                let old_warnings = std::mem::replace(&mut self.warnings, imported.warnings);
                let candidate = imported.configuration;
                if let Err(error) = self.runtime.apply_configuration(&candidate) {
                    self.warnings = old_warnings;
                    self.restore_configuration(old_configuration.clone());
                    return Err(format!(
                        "Imported configuration could not be applied: {error:?}"
                    ));
                }
                let old_autostart = old_configuration.startup.autostart;
                let new_autostart = candidate.startup.autostart;
                if old_autostart != new_autostart {
                    if let Err(error) = set_autostart_side_effect(new_autostart) {
                        self.warnings = old_warnings;
                        self.restore_configuration(old_configuration.clone());
                        return Err(format!("Imported autostart preference failed: {error}"));
                    }
                }
                if let Err(error) = persist(config_path, &candidate) {
                    if old_autostart != new_autostart {
                        let _ = set_autostart_side_effect(old_autostart);
                    }
                    self.warnings = old_warnings;
                    self.restore_configuration(old_configuration);
                    return Err(format!("Imported configuration was not saved: {error}"));
                }
                self.configuration = candidate;
                self.preset_snapshot = None;
                format!("Imported configuration from {}", path.display())
            }
            ControllerOperation::ExportConfiguration(path) => {
                export_file(&path, &self.configuration)
                    .map_err(|error| format!("Could not export {}: {error}", path.display()))?;
                format!("Exported configuration to {}", path.display())
            }
        };
        if self.configuration.logging != logging_before {
            if let Some(logger) = kestrel::logging::logger() {
                logger.apply(kestrel::logging::LogPolicy::from_configuration(
                    &self.configuration.logging,
                ));
            }
        }
        Ok((self.view_model(), message))
    }
    fn commit_configuration(
        &mut self,
        candidate: ApplicationConfiguration,
        config_path: Option<&Path>,
        message: String,
    ) -> Result<String, String> {
        candidate
            .validate()
            .map_err(|error| format!("Invalid configuration: {error:?}"))?;
        let old = self.configuration.clone();
        self.runtime
            .apply_configuration(&candidate)
            .map_err(|error| {
                self.restore_configuration(old.clone());
                format!("Could not apply the configuration: {error:?}")
            })?;
        if let Err(error) = persist(config_path, &candidate) {
            self.restore_configuration(old);
            return Err(format!("Configuration change was not saved: {error}"));
        }
        self.configuration = candidate;
        Ok(message)
    }

    fn restore_configuration(&mut self, configuration: ApplicationConfiguration) {
        self.configuration = configuration.clone();
        let _ = self.runtime.apply_configuration(&configuration);
    }

    fn set_autostart(&mut self, enabled: bool, config_path: Option<&Path>) -> Result<(), String> {
        let old = self.configuration.clone();
        if old.startup.autostart != enabled {
            set_autostart_side_effect(enabled)
                .map_err(|error| format!("Could not change autostart: {error}"))?;
        }
        self.configuration.startup.autostart = enabled;
        if let Err(error) = persist(config_path, &self.configuration) {
            if old.startup.autostart != enabled {
                let _ = set_autostart_side_effect(old.startup.autostart);
            }
            self.configuration = old;
            return Err(format!("Autostart change was not saved: {error}"));
        }

        Ok(())
    }
}

fn persist(path: Option<&Path>, configuration: &ApplicationConfiguration) -> Result<(), String> {
    let path = path.ok_or_else(|| {
        "no writable configuration path is available; correct the existing configuration file \
         and restart Kestrel"
            .to_owned()
    })?;
    save(path, configuration).map_err(|error| format!("{}: {error}", path.display()))
}

fn set_autostart_side_effect(enabled: bool) -> Result<(), String> {
    if enabled {
        enable_autostart().map_err(|error| error.to_string())
    } else {
        disable_autostart().map_err(|error| error.to_string())
    }
}

fn audio_command_summary(command: kestrel::AudioCommand) -> &'static str {
    match command {
        kestrel::AudioCommand::SetStreamVolume { .. } => "Stream volume",
        kestrel::AudioCommand::SetStreamMute { .. } => "Stream mute",
        kestrel::AudioCommand::MoveStream { .. } => "Stream routing",
        kestrel::AudioCommand::SetOutputVolume { .. } => "Output volume",
        kestrel::AudioCommand::SetOutputMute { .. } => "Output mute",
        kestrel::AudioCommand::SetDefaultOutput { .. } => "Default output",
        kestrel::AudioCommand::CycleOutput { .. } => "Output cycle",
    }
}

fn optional_field(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn apply_targeted_panel(
    view: &WindowView,
    view_model: &ApplicationViewModel,
    targeted: TargetedPanel,
) {
    match targeted {
        TargetedPanel::Full => view.set_view_model(view_model),
        TargetedPanel::Clipboard => view.set_clipboard(&view_model.clipboard),
        TargetedPanel::Snippets => view.set_snippets(&view_model.snippets),
        TargetedPanel::Commands => view.set_command_bar(&view_model.command_bar),
        TargetedPanel::Monitor => view.set_monitor(&view_model.monitor),
        TargetedPanel::Microphone => view.set_microphone(&view_model.microphone),
        TargetedPanel::Audio => {
            view.set_audio(&view_model.audio);
            view.set_microphone(&view_model.microphone);
        }
        TargetedPanel::SpeedTest => view.set_speed_test(&view_model.speed_test),
        TargetedPanel::Capture => view.set_capture(&view_model.capture),
        TargetedPanel::Processes => {
            view.set_processes(&view_model.processes);
            view.set_listeners(&view_model.listeners);
        }
        TargetedPanel::Scratchpad => view.set_scratchpad(&view_model.scratchpad),
    }
}

fn clipboard_command_summary(command: &kestrel::ClipboardCommand) -> &'static str {
    match command {
        kestrel::ClipboardCommand::Refresh => "Clipboard refresh",
        kestrel::ClipboardCommand::CopyItem { .. } => "Clipboard copy",
        kestrel::ClipboardCommand::DeleteItems { .. } => "Clipboard delete",
        kestrel::ClipboardCommand::SetPinned { .. } => "Clipboard pin",
        kestrel::ClipboardCommand::ReplaceText { .. } => "Clipboard edit",
        kestrel::ClipboardCommand::ClearSelection => "Clipboard selection clear",
        kestrel::ClipboardCommand::Wipe => "Clipboard wipe",
        kestrel::ClipboardCommand::ClearEntries {
            keep_pinned: true, ..
        } => "Clipboard clear unpinned",
        kestrel::ClipboardCommand::ClearEntries { .. } => "Clipboard wipe",
        kestrel::ClipboardCommand::CopyUnstoredText(_) => "Image text copy",
    }
}

/// Presents `window`, applying a fresh activation token when one arrived.
fn present_with(window: &gtk::Window, startup_id: Option<&str>) {
    if let Some(startup_id) = startup_id {
        window.set_startup_id(startup_id);
    }
    window.present();
}

fn apply_color_scheme(preference: AppearancePreference) {
    let scheme = match preference {
        AppearancePreference::System => adw::ColorScheme::Default,
        AppearancePreference::Light => adw::ColorScheme::ForceLight,
        AppearancePreference::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

fn load_startup_configuration(
    config_path: Option<PathBuf>,
) -> (LoadedConfiguration, Option<PathBuf>) {
    let Some(path) = config_path else {
        return (LoadedConfiguration::default(), None);
    };
    match load(&path) {
        Ok(loaded) => (loaded, Some(path)),
        Err(error) => {
            let mut loaded = LoadedConfiguration {
                existed: true,
                ..LoadedConfiguration::default()
            };
            loaded.warnings.push(ConfigurationWarning {
                feature_id: "configuration".to_owned(),
                reason: format!(
                    "Configuration could not be loaded: {error}. \
                     Writes are disabled until the file is corrected and Kestrel is restarted."
                ),
            });
            (loaded, None)
        }
    }
}

fn main() -> glib::ExitCode {
    let arguments = std::env::args().collect::<Vec<_>>();
    // Answer help, version, listing, and invalid IDs locally, before touching
    // D-Bus or building the runtime.
    let requested = match parse_arguments(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(Invocation::Help) => {
            print!("{}", usage());
            return glib::ExitCode::SUCCESS;
        }
        Ok(Invocation::Version) => {
            println!("kestrel {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        Ok(Invocation::ListCommands) => {
            print!("{}", command_list());
            return glib::ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("kestrel: {error}");
            eprint!("{}", usage());
            return glib::ExitCode::from(CommandLineError::EXIT_STATUS);
        }
        Ok(invocation @ (Invocation::Present | Invocation::Run(_))) => invocation,
    };

    let (mut loaded, config_path) = load_startup_configuration(configuration_path());
    let offer_setup = config_path.is_some()
        && setup::should_offer_setup(
            loaded.existed,
            setup::is_done(setup::state_path().as_deref()),
        );
    let logger = kestrel::logging::install(kestrel::logging::LogPolicy::from_configuration(
        &loaded.configuration.logging,
    ));
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_panic(info);
    }));
    let initial_appearance = loaded.configuration.ui.appearance;
    let application = adw::Application::builder()
        .application_id(kestrel_core::APPLICATION_ID)
        .flags(adw::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    application.connect_startup(move |_| {
        apply_color_scheme(initial_appearance);
    });
    if let Err(error) = application.register(None::<&adw::gio::Cancellable>) {
        log::error!("could not register with the session bus: {error}");
        return glib::ExitCode::FAILURE;
    }
    if application.is_remote() {
        // Another instance owns the session: forward this command line to it
        // without building a second runtime or tray icon.
        let exit = application.run_with_args(&arguments);
        if let Invocation::Run(action) = requested {
            if let Some(gate) = action
                .required_feature()
                .and_then(|feature| CommandGate::from_exit_status(exit.get(), feature))
            {
                eprintln!("kestrel: {}: {gate}", action.id());
            }
        }
        return exit;
    }

    log::info!("Kestrel {} started", env!("CARGO_PKG_VERSION"));
    for warning in &loaded.warnings {
        if logger.is_some() {
            log::warn!(target: "configuration", "{}: {}", warning.feature_id, warning.reason);
        } else {
            eprintln!("kestrel: {}: {}", warning.feature_id, warning.reason);
        }
    }
    if let Some(logger) = logger {
        let _ = logger.prune(std::time::SystemTime::now());
        glib::timeout_add_seconds_local(60 * 60, move || {
            let _ = logger.prune(std::time::SystemTime::now());
            glib::ControlFlow::Continue
        });
    }

    if loaded.configuration.startup.autostart {
        let result = thread::Builder::new()
            .name("kestrel-startup-autostart".to_owned())
            .spawn(setup_startup_autostart)
            .and_then(|worker| {
                worker
                    .join()
                    .map_err(|_| std::io::Error::other("worker panicked"))
            });
        if let Err(error) = result.and_then(|result| result) {
            loaded.configuration.startup.autostart = false;
            loaded.warnings.push(ConfigurationWarning {
                feature_id: "startup.autostart".to_owned(),
                reason: format!("Autostart could not be enabled at startup and was reset: {error}"),
            });
            if let Some(path) = config_path.as_deref() {
                let _ = save(path, &loaded.configuration);
            }
        }
    }

    let (commands, command_receiver) = async_channel::unbounded();
    // The tray starts only when it is on; otherwise its host is only looked for.
    let tray_on = loaded
        .configuration
        .features
        .get(kestrel::STATUS_NOTIFIER_ID)
        .is_none_or(|feature| feature.enabled);
    let status_notifier = tray_on.then(|| StatusNotifierIntegration::start(commands.clone()));
    let tray_capability = status_notifier.as_ref().map_or_else(
        StatusNotifierIntegration::probe,
        StatusNotifierIntegration::capability,
    );

    let mut runtime = ApplicationRuntime::new_with_host(
        &loaded.configuration,
        kestrel::HostCapabilities {
            status_notifier: tray_capability,
            overlay: layer_shell::overlay_capability(&layer_shell::observed_placement()),
        },
    )
    .expect("built-in features have valid IDs");
    runtime.set_command_sender(commands.clone());
    runtime.start();
    runtime
        .refresh_capabilities()
        .expect("built-in capability probes are internally consistent");
    runtime.start_whats_new(&loaded.configuration);

    let (refresh_results, refresh_receiver) = async_channel::unbounded();
    let (tick_results, tick_receiver) = async_channel::unbounded();
    let capture_notifier = Arc::new(DesktopNotifier::new());
    let notifier: Arc<dyn AlertNotifier> = capture_notifier.clone();
    let controller = ApplicationController::new(
        runtime,
        loaded.configuration,
        loaded.warnings,
        ControllerContext {
            config_path,
            offer_setup,
            refresh_results,
            tick_results,
            commands: commands.clone(),
            notifier,
            capture_notifier,
            status_notifier,
        },
    );
    controller
        .overlay
        .set_view_model(&controller.current_view_model());
    let command_line_commands = commands.clone();
    install_command_actions(&application, commands);
    dispatch_commands(&application, &controller, command_receiver);
    dispatch_refresh_results(&controller, refresh_receiver);
    dispatch_tick_results(&controller, tick_receiver);
    let weak_monitor_controller = Rc::downgrade(&controller);
    glib::timeout_add_local(TICK_INTERVAL, move || {
        if let Some(controller) = weak_monitor_controller.upgrade() {
            controller.request_tick();
        }
        glib::ControlFlow::Continue
    });

    let weak_controller = Rc::downgrade(&controller);
    application.connect_activate(move |application| {
        if let Some(controller) = weak_controller.upgrade() {
            controller.present(application);
        }
    });
    let weak_controller = Rc::downgrade(&controller);
    application.connect_command_line(move |application, command_line| {
        let arguments = command_line.arguments();
        let Ok(invocation) = parse_arguments(arguments.get(1..).unwrap_or_default()) else {
            return glib::ExitCode::from(CommandLineError::EXIT_STATUS);
        };
        let Some(controller) = weak_controller.upgrade() else {
            return glib::ExitCode::FAILURE;
        };
        match invocation {
            Invocation::Present => controller.present(application),
            Invocation::Run(action) => {
                // A command that started Kestrel opens the window so the
                // instance stays alive and the result is visible.
                if !command_line.is_remote() {
                    controller.present(application);
                }
                if let Some(gate) = controller.command_gate(action) {
                    // The remote client reports the gate from the exit status.
                    let message = format!("{}: {gate}", action.id());
                    if !command_line.is_remote() {
                        log::warn!("{message}");
                    }
                    if action == CommandLineAction::OpenCommandBar {
                        // The surface opens in its remediation state.
                        let _ = command_line_commands.try_send(action.to_command());
                    } else {
                        controller.show_message(&message);
                    }
                    return glib::ExitCode::from(gate.exit_status());
                }
                let _ = command_line_commands.try_send(action.to_command());
            }
            Invocation::Help | Invocation::Version | Invocation::ListCommands => {}
        }
        glib::ExitCode::SUCCESS
    });

    let exit = application.run_with_args(&arguments);
    if let Some(stopping) = controller.stop_tray() {
        let _ = stopping.join();
    }
    exit
}

fn setup_startup_autostart() -> Result<(), std::io::Error> {
    enable_autostart().map_err(|error| std::io::Error::other(error.to_string()))
}

fn install_command_actions(application: &adw::Application, commands: Sender<ApplicationCommand>) {
    add_command_action(
        application,
        "present-window",
        ApplicationCommand::PresentWindow,
        &commands,
    );
    add_command_action(
        application,
        "refresh-capabilities",
        ApplicationCommand::RefreshCapabilities,
        &commands,
    );
    add_command_action(
        application,
        "toggle-microphone-mute",
        ApplicationCommand::Microphone(MicrophoneCommand::ToggleMute),
        &commands,
    );
    application.set_accels_for_action("app.toggle-microphone-mute", &["<Primary><Shift>m"]);
    add_command_action(application, "quit", ApplicationCommand::Quit, &commands);
    application.set_accels_for_action("app.refresh-capabilities", &["<Primary>r"]);
    application.set_accels_for_action("app.quit", &["<Primary>q"]);
}

fn add_command_action(
    application: &adw::Application,
    name: &str,
    command: ApplicationCommand,
    commands: &Sender<ApplicationCommand>,
) {
    let action = adw::gio::SimpleAction::new(name, None);
    let commands = commands.clone();
    action.connect_activate(move |_, _| {
        let _ = commands.try_send(command.clone());
    });
    application.add_action(&action);
}

fn dispatch_commands(
    application: &adw::Application,
    controller: &Rc<ApplicationController>,
    receiver: Receiver<ApplicationCommand>,
) {
    let application = application.clone();
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(command) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            match command {
                ApplicationCommand::PresentWindow => controller.present(&application),
                ApplicationCommand::ActivationToken(token) => {
                    controller.activation_token.replace(Some(token));
                }
                ApplicationCommand::Focus(target) => controller.focus(&application, target),
                ApplicationCommand::RefreshCapabilities => controller.request_refresh(),
                ApplicationCommand::QuickToggle(command) => {
                    controller.request_quick_toggle(command)
                }
                ApplicationCommand::FlipQuickToggle(id) => controller.request_operation(
                    ControllerOperation::FlipQuickToggle(id),
                    "kestrel-quick-toggle",
                ),
                ApplicationCommand::SetFeatureEnabled {
                    feature_id,
                    enabled,
                } => controller.request_operation(
                    ControllerOperation::SetFeatureEnabled {
                        feature_id,
                        enabled,
                    },
                    "kestrel-feature-change",
                ),
                ApplicationCommand::ApplyPreset(preset) => controller.request_operation(
                    ControllerOperation::ApplyPreset(preset),
                    "kestrel-preset-change",
                ),
                ApplicationCommand::FinishSetup { preset, left_off } => controller
                    .request_operation(
                        ControllerOperation::FinishSetup { preset, left_off },
                        "kestrel-setup-finish",
                    ),
                ApplicationCommand::SkipSetup => controller
                    .request_operation(ControllerOperation::SkipSetup, "kestrel-setup-skip"),
                ApplicationCommand::UndoPreset => controller
                    .request_operation(ControllerOperation::UndoPreset, "kestrel-preset-undo"),
                ApplicationCommand::SetAppearance(appearance) => controller.request_operation(
                    ControllerOperation::SetAppearance(appearance),
                    "kestrel-appearance-change",
                ),
                ApplicationCommand::SetCloseAction(close_action) => controller.request_operation(
                    ControllerOperation::SetCloseAction(close_action),
                    "kestrel-close-action-change",
                ),
                ApplicationCommand::SetAutostart(enabled) => controller.request_operation(
                    ControllerOperation::SetAutostart(enabled),
                    "kestrel-autostart-change",
                ),
                ApplicationCommand::SetWhatsNew(enabled) => controller.request_operation(
                    ControllerOperation::SetWhatsNew(enabled),
                    "kestrel-whats-new-change",
                ),
                ApplicationCommand::MarkWhatsNewRead => controller.request_operation(
                    ControllerOperation::MarkWhatsNewRead,
                    "kestrel-whats-new-read",
                ),
                ApplicationCommand::SetMonitorReadoutVisible { readout, visible } => controller
                    .request_operation(
                        ControllerOperation::SetMonitorReadoutVisible { readout, visible },
                        "kestrel-monitor-readout-visibility",
                    ),
                ApplicationCommand::SetMonitorSetting(setting) => controller.request_operation(
                    ControllerOperation::SetMonitorSetting(setting),
                    "kestrel-monitor-setting",
                ),
                ApplicationCommand::ShowLocalAddresses(shown) => controller
                    .request_targeted_operation(
                        ControllerOperation::ShowLocalAddresses(shown),
                        "kestrel-local-addresses",
                        TargetedPanel::Monitor,
                    ),
                ApplicationCommand::MoveMonitorReadout { readout, direction } => controller
                    .request_operation(
                        ControllerOperation::MoveMonitorReadout { readout, direction },
                        "kestrel-monitor-readout-movement",
                    ),
                ApplicationCommand::SetAlertEnabled { kind, enabled } => controller
                    .request_operation(
                        ControllerOperation::SetAlertEnabled { kind, enabled },
                        "kestrel-alert-enabled",
                    ),
                ApplicationCommand::SetAlertThreshold { kind, threshold } => controller
                    .request_operation(
                        ControllerOperation::SetAlertThreshold { kind, threshold },
                        "kestrel-alert-threshold",
                    ),
                ApplicationCommand::SetAlertSustain { kind, duration } => controller
                    .request_operation(
                        ControllerOperation::SetAlertSustain { kind, duration },
                        "kestrel-alert-sustain",
                    ),
                ApplicationCommand::Audio(command) => controller
                    .request_operation(ControllerOperation::Audio(command), "kestrel-audio"),
                ApplicationCommand::SetAudioBoostPercent(percent) => controller.request_operation(
                    ControllerOperation::SetAudioBoostPercent(percent),
                    "kestrel-audio-boost",
                ),
                ApplicationCommand::SetAudioOutputSwitch(mode) => controller.request_operation(
                    ControllerOperation::SetAudioOutputSwitch(mode),
                    "kestrel-audio-output-switch",
                ),
                ApplicationCommand::SetAudioDisconnectPolicy(policy) => controller
                    .request_operation(
                        ControllerOperation::SetAudioDisconnectPolicy(policy),
                        "kestrel-audio-disconnect",
                    ),
                ApplicationCommand::SetAudioDisconnectVolumePercent(percent) => controller
                    .request_operation(
                        ControllerOperation::SetAudioDisconnectVolumePercent(percent),
                        "kestrel-audio-disconnect-volume",
                    ),
                ApplicationCommand::SetAudioIncludeInactiveStreams(include) => controller
                    .request_operation(
                        ControllerOperation::SetAudioIncludeInactiveStreams(include),
                        "kestrel-audio-inactive",
                    ),
                ApplicationCommand::AudioPreference(edit) => controller.request_targeted_operation(
                    ControllerOperation::AudioPreference(edit),
                    "kestrel-audio-preference",
                    TargetedPanel::Audio,
                ),
                ApplicationCommand::Microphone(command) => controller.request_targeted_operation(
                    ControllerOperation::Microphone(command),
                    "kestrel-microphone",
                    TargetedPanel::Microphone,
                ),
                ApplicationCommand::StartSpeedTest => controller.request_targeted_operation(
                    ControllerOperation::StartSpeedTest,
                    "kestrel-speed-test-start",
                    TargetedPanel::SpeedTest,
                ),
                ApplicationCommand::CancelSpeedTest => controller.request_targeted_operation(
                    ControllerOperation::CancelSpeedTest,
                    "kestrel-speed-test-cancel",
                    TargetedPanel::SpeedTest,
                ),
                ApplicationCommand::CaptureFollowUp { id, action } => {
                    let action = match action {
                        kestrel::CaptureFollowUpAction::Open => CaptureNotificationAction::Open,
                        kestrel::CaptureFollowUpAction::Edit => CaptureNotificationAction::Edit,
                        kestrel::CaptureFollowUpAction::Copy => CaptureNotificationAction::Copy,
                    };
                    controller.handle_capture_follow_up(&application, id, action);
                }
                ApplicationCommand::Capture(request) => {
                    let operation = match request {
                        CaptureRequest::Copy(id) => {
                            controller.copy_capture(id);
                            continue;
                        }
                        CaptureRequest::EditLatest => {
                            controller.focus(&application, FocusTarget::Capture);
                            let id = controller
                                .state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .runtime
                                .capture_snapshot()
                                .entries
                                .first()
                                .map(|entry| entry.id);
                            if let Some(id) = id {
                                controller.open_capture_editor(id);
                            } else {
                                controller.show_message("No captured screenshots are available");
                            }
                            continue;
                        }
                        CaptureRequest::Edit(id) => {
                            controller.focus(&application, FocusTarget::Capture);
                            controller.open_capture_editor(id);
                            continue;
                        }
                        CaptureRequest::Begin(mode) => CaptureOperation::Begin(mode),
                        CaptureRequest::Cancel => CaptureOperation::Cancel,
                        CaptureRequest::Save { id, destination } => {
                            CaptureOperation::Save { id, destination }
                        }
                        CaptureRequest::SaveEdit { id, plan } => {
                            CaptureOperation::SaveEdit { id, plan }
                        }
                        CaptureRequest::Delete(id) => CaptureOperation::Delete(id),
                        CaptureRequest::Clear => CaptureOperation::Clear,
                    };
                    controller.request_targeted_operation(
                        ControllerOperation::Capture(operation),
                        "kestrel-capture",
                        TargetedPanel::Capture,
                    );
                }
                ApplicationCommand::Processes(request) => match request {
                    kestrel::ProcessRequest::Search(query) => controller.search_processes(query),
                    kestrel::ProcessRequest::Refresh => controller.request_targeted_operation(
                        ControllerOperation::RefreshProcesses,
                        "kestrel-processes",
                        TargetedPanel::Processes,
                    ),
                    kestrel::ProcessRequest::Confirm { identity, action } => {
                        controller.confirm_process(identity, action);
                    }
                    kestrel::ProcessRequest::Run { identity, action } => {
                        controller.request_process_action(identity, action);
                    }
                },
                ApplicationCommand::SetProcessLimit(limit) => controller.request_operation(
                    ControllerOperation::SetProcessLimit(limit),
                    "kestrel-process-limit",
                ),
                ApplicationCommand::SetScratchpadLimit(limit) => controller.request_operation(
                    ControllerOperation::SetScratchpadLimit(limit),
                    "kestrel-scratchpad-limit",
                ),
                ApplicationCommand::SetLoggingSetting(setting) => controller.request_operation(
                    ControllerOperation::SetLoggingSetting(setting),
                    "kestrel-logging-setting",
                ),
                ApplicationCommand::ClearLogs => controller
                    .request_operation(ControllerOperation::ClearLogs, "kestrel-clear-logs"),
                ApplicationCommand::SetOverlayModule { module, shown } => controller
                    .request_operation(
                        ControllerOperation::SetOverlayModule { module, shown },
                        "kestrel-overlay-module",
                    ),
                ApplicationCommand::SetAccessoryLevels { warning, critical } => controller
                    .request_operation(
                        ControllerOperation::SetAccessoryLevels { warning, critical },
                        "kestrel-accessory-levels",
                    ),
                ApplicationCommand::SetShortcut { command, trigger } => controller
                    .request_operation(
                        ControllerOperation::SetShortcut { command, trigger },
                        "kestrel-shortcut-setting",
                    ),
                ApplicationCommand::OpenLogFolder => {
                    let Some(logger) = kestrel::logging::logger() else {
                        controller.show_message("Logging is unavailable without a home folder");
                        continue;
                    };
                    if let Err(error) = logger.ensure_directory() {
                        controller
                            .show_message(&format!("The log folder was not created: {error}"));
                        continue;
                    }
                    let uri = gtk::gio::File::for_path(logger.directory()).uri();
                    let window = controller.window.borrow();
                    let parent = window.as_ref().map(|view| view.window.clone());
                    gtk::UriLauncher::new(&uri).launch(
                        parent.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        |_| {},
                    );
                }
                ApplicationCommand::ScreenTools(request) => controller.screen_tool_request(request),
                ApplicationCommand::Camera(request) => controller.camera_request(request),
                ApplicationCommand::Recording(request) => controller.recording_request(request),
                ApplicationCommand::TrimRecording(take) => controller.open_trim(take),
                ApplicationCommand::SetRecordingSetting(setting) => controller.request_operation(
                    ControllerOperation::SetRecordingSetting(setting),
                    "kestrel-recording-setting",
                ),
                ApplicationCommand::SetScreenToolsSetting(setting) => controller.request_operation(
                    ControllerOperation::SetScreenToolsSetting(setting),
                    "kestrel-screen-tools-setting",
                ),
                ApplicationCommand::CopyText(text) => {
                    if let Some(view) = controller.window.borrow().as_ref() {
                        view.window.clipboard().set_text(&text);
                    }
                }
                ApplicationCommand::OpenUri(uri) => {
                    let window = controller.window.borrow();
                    let parent = window.as_ref().map(|view| view.window.clone());
                    gtk::UriLauncher::new(&uri).launch(
                        parent.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        |_| {},
                    );
                }
                ApplicationCommand::Clipboard(command) => controller.request_clipboard_operation(
                    ControllerOperation::Clipboard(command),
                    "kestrel-clipboard",
                ),
                ApplicationCommand::ClipboardSearch(query) => controller
                    .request_clipboard_operation(
                        ControllerOperation::ClipboardSearch(query),
                        "kestrel-clipboard-search",
                    ),
                ApplicationCommand::ClipboardPreview(item_id) => controller
                    .request_clipboard_operation(
                        ControllerOperation::ClipboardPreview(item_id),
                        "kestrel-clipboard-preview",
                    ),
                ApplicationCommand::ClipboardCopyImageText(item_id) => controller
                    .request_targeted_operation(
                        ControllerOperation::ClipboardCopyImageText(item_id),
                        "kestrel-clipboard-image-text",
                        TargetedPanel::Clipboard,
                    ),
                ApplicationCommand::SetClipboardLayout(layout) => controller.request_operation(
                    ControllerOperation::SetClipboardLayout(layout),
                    "kestrel-clipboard-layout",
                ),
                ApplicationCommand::SetClipboardLimit(limit) => controller.request_operation(
                    ControllerOperation::SetClipboardLimit(limit),
                    "kestrel-clipboard-limit",
                ),
                ApplicationCommand::SetCaptureLimit(limit) => controller.request_operation(
                    ControllerOperation::SetCaptureLimit(limit),
                    "kestrel-capture-limit",
                ),
                ApplicationCommand::SetCaptureAfterScreenshot(after) => controller
                    .request_operation(
                        ControllerOperation::SetCaptureAfterScreenshot(after),
                        "kestrel-capture-after-screenshot",
                    ),
                ApplicationCommand::SetClipboardFilterSensitive(filter) => controller
                    .request_operation(
                        ControllerOperation::SetClipboardFilterSensitive(filter),
                        "kestrel-clipboard-filter",
                    ),
                ApplicationCommand::SetClipboardPastePlainText(plain) => controller
                    .request_operation(
                        ControllerOperation::SetClipboardPastePlainText(plain),
                        "kestrel-clipboard-paste",
                    ),
                ApplicationCommand::Snippet(command) => controller.request_snippet_operation(
                    ControllerOperation::Snippet(command),
                    "kestrel-snippet",
                ),
                ApplicationCommand::SnippetSearch(query) => controller.request_snippet_operation(
                    ControllerOperation::SnippetSearch(query),
                    "kestrel-snippet-search",
                ),
                ApplicationCommand::SnippetEdit(name) => controller.request_snippet_operation(
                    ControllerOperation::SnippetEdit(name),
                    "kestrel-snippet-edit",
                ),
                ApplicationCommand::SnippetNew => controller.request_snippet_operation(
                    ControllerOperation::SnippetNew,
                    "kestrel-snippet-new",
                ),
                ApplicationCommand::SetSnippetLimit(limit) => controller.request_operation(
                    ControllerOperation::SetSnippetLimit(limit),
                    "kestrel-snippet-limit",
                ),
                ApplicationCommand::SetSnippetProvider(preference) => controller.request_operation(
                    ControllerOperation::SetSnippetProvider(preference),
                    "kestrel-snippet-provider",
                ),
                ApplicationCommand::SetSnippetExpansionTiming(timing) => controller
                    .request_operation(
                        ControllerOperation::SetSnippetExpansionTiming(timing),
                        "kestrel-snippet-expansion",
                    ),
                ApplicationCommand::CommandQuery(query) => controller.request_command_operation(
                    ControllerOperation::CommandQuery(query),
                    "kestrel-command-query",
                ),
                ApplicationCommand::CommandBar(command) => controller.request_command_operation(
                    ControllerOperation::CommandBar(command),
                    "kestrel-command-run",
                ),
                ApplicationCommand::ResetCommandRanking => controller.request_command_operation(
                    ControllerOperation::ResetCommandRanking,
                    "kestrel-command-reset",
                ),
                ApplicationCommand::SetCommandResultLimit(results) => controller.request_operation(
                    ControllerOperation::SetCommandResultLimit(results),
                    "kestrel-command-limit",
                ),
                ApplicationCommand::SetQuickPanelFavorites(favorites) => controller
                    .request_operation(
                        ControllerOperation::SetQuickPanelFavorites(favorites),
                        "kestrel-quick-panel-settings",
                    ),
                ApplicationCommand::SetCommandProvider(switch) => controller.request_operation(
                    ControllerOperation::SetCommandProvider(switch),
                    "kestrel-command-provider",
                ),
                ApplicationCommand::ImportConfiguration(path) => controller.request_operation(
                    ControllerOperation::ImportConfiguration(path),
                    "kestrel-configuration-import",
                ),
                ApplicationCommand::ExportConfiguration(path) => controller.request_operation(
                    ControllerOperation::ExportConfiguration(path),
                    "kestrel-configuration-export",
                ),
                ApplicationCommand::Scratchpad(command) => match command {
                    kestrel::ScratchpadCommand::Update { .. } => {
                        controller.update_scratchpad(command)
                    }
                    command => controller.request_targeted_operation(
                        ControllerOperation::Scratchpad(command),
                        "kestrel-scratchpad",
                        TargetedPanel::Scratchpad,
                    ),
                },
                ApplicationCommand::Quit => application.quit(),
            }
        }
    });
}

fn dispatch_refresh_results(
    controller: &Rc<ApplicationController>,
    receiver: Receiver<RefreshResult>,
) {
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(result) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.finish_refresh(result);
        }
    });
}
fn dispatch_tick_results(controller: &Rc<ApplicationController>, receiver: Receiver<TickUpdate>) {
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(monitor) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.finish_tick(monitor);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{load_startup_configuration, persist};
    use kestrel::{MonitorReadout, PanelMoveDirection};
    use kestrel_core::ApplicationConfiguration;

    #[test]
    fn capture_follow_up_runs_once_for_new_screenshot() {
        use super::capture_follow_up_decision;
        use kestrel::CaptureAfterScreenshot;

        assert_eq!(
            capture_follow_up_decision(Some(41u64), None, CaptureAfterScreenshot::Notify),
            Some((41, CaptureAfterScreenshot::Notify))
        );
        assert_eq!(
            capture_follow_up_decision(Some(41u64), Some(41), CaptureAfterScreenshot::Notify),
            None
        );
    }

    #[test]
    fn edited_copy_does_not_retrigger_original_capture_follow_up() {
        use super::capture_follow_up_decision;
        use kestrel::CaptureAfterScreenshot;

        assert_eq!(
            capture_follow_up_decision(Some(42u64), Some(42), CaptureAfterScreenshot::Edit),
            None
        );
    }
    #[test]
    fn monitor_readout_movement_is_bounded_and_deterministic() {
        let mut readouts = vec![MonitorReadout::Cpu, MonitorReadout::Memory];
        assert!(!kestrel::move_entry(
            &mut readouts,
            &MonitorReadout::Cpu,
            PanelMoveDirection::Up
        ));
        assert!(kestrel::move_entry(
            &mut readouts,
            &MonitorReadout::Cpu,
            PanelMoveDirection::Down
        ));
        assert_eq!(readouts, vec![MonitorReadout::Memory, MonitorReadout::Cpu]);
        assert!(!kestrel::move_entry(
            &mut readouts,
            &MonitorReadout::Cpu,
            PanelMoveDirection::Down
        ));
    }

    #[test]
    fn invalid_startup_configuration_disables_persistence() {
        let path = std::env::temp_dir().join(format!(
            "kestrel-invalid-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let original = b"[";
        std::fs::write(&path, original).expect("invalid fixture can be written");

        let (loaded, writable_path) = load_startup_configuration(Some(path.clone()));

        assert!(writable_path.is_none());
        assert!(loaded.warnings.iter().any(|warning| {
            warning.feature_id == "configuration" && warning.reason.contains("Writes are disabled")
        }));
        assert!(
            persist(
                writable_path.as_deref(),
                &ApplicationConfiguration::default()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(&path).expect("invalid fixture remains readable"),
            original
        );

        std::fs::remove_file(path).expect("invalid fixture can be removed");
    }

    /// Disabled ticks spawn no worker or result; enabled ticks still sample.
    #[test]
    fn monitor_ticks_are_inert_while_the_feature_is_disabled() {
        use super::{ApplicationController, ControllerContext};
        use async_channel::Sender;
        use kestrel::ApplicationRuntime;
        use kestrel_platform::notifications::{
            AlertNotification, AlertNotifier, DesktopNotifier, NotificationError,
        };
        use kestrel_platform::system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID;
        use std::rc::Rc;
        use std::sync::Arc;

        struct SilentNotifier;

        impl AlertNotifier for SilentNotifier {
            fn notify(&self, _: &AlertNotification) -> Result<(), NotificationError> {
                Ok(())
            }
        }

        fn controller_for(
            configuration: &ApplicationConfiguration,
            tick_results: Sender<super::TickUpdate>,
        ) -> Rc<ApplicationController> {
            let mut runtime =
                ApplicationRuntime::new(configuration).expect("built-in features have valid IDs");
            runtime.start();
            let (commands, _command_receiver) = async_channel::unbounded();
            let (refresh_results, _refresh_receiver) = async_channel::unbounded();
            ApplicationController::new(
                runtime,
                configuration.clone(),
                Vec::new(),
                ControllerContext {
                    config_path: None,
                    offer_setup: false,
                    refresh_results,
                    tick_results,
                    commands,
                    notifier: Arc::new(SilentNotifier),
                    capture_notifier: Arc::new(DesktopNotifier::new()),
                    status_notifier: None,
                },
            )
        }

        let (disabled_results, disabled_receiver) = async_channel::unbounded();
        let disabled = controller_for(&ApplicationConfiguration::default(), disabled_results);
        disabled.request_tick();
        assert!(
            !disabled.monitoring.get(),
            "a disabled monitor must not start a sampling worker"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            disabled_receiver.try_recv().is_err(),
            "a disabled monitor must not publish a sample"
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let (enabled_results, enabled_receiver) = async_channel::unbounded();
        let enabled = controller_for(&configuration, enabled_results);
        enabled.request_tick();
        assert!(
            enabled.monitoring.get(),
            "an enabled monitor samples on the next tick"
        );
        assert!(
            enabled_receiver.recv_blocking().is_ok(),
            "an enabled monitor publishes its tick result"
        );
    }
}
