//! Application configuration and runtime composition.

mod autostart;
mod command_bar;
pub mod command_line;
mod config;
pub mod logging;
mod private_file;
mod runtime;
pub mod setup;
mod snippets;
mod status_notifier;
pub mod text_lines;
mod tray_icon;
pub mod view_model;
pub mod whats_new;

pub use autostart::{disable, enable};
pub use command_bar::{load_ranking, ranking_path, save_ranking};
pub use config::{
    ConfigurationWarning, LoadedConfiguration, configuration_path, export_file, import_file, load,
    save,
};
pub use kestrel_core::{
    AlertKind, AppearancePreference, AudioDisconnectPolicy, AudioOutputSwitch,
    CaptureAfterScreenshot, ClipboardLayout, CloseAction, MonitorReadout, NetworkRateUnit,
};
pub use kestrel_core::{Snippet, SnippetExpansionTiming, SnippetProviderPreference};
pub use kestrel_platform::capture::CaptureMode;
pub use kestrel_platform::clipboard::ClipboardEntryKind;
pub use kestrel_platform::quick_toggles::{QuickToggleId, QuickToggleMutation};
pub use kestrel_platform::recording::{AudioTrack, RecordingSource};
pub use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
pub use kestrel_platform::upower::{PeripheralCharge, PeripheralKind};
pub use kestrel_services::audio::{AudioCommand, AudioCycleDirection};
pub use kestrel_services::camera::CameraFrameSlot;
pub use kestrel_services::capture::{
    CaptureId,
    image::{
        CanvasExpansion, Color, DEFAULT_SPOTLIGHT_DIM_PERCENT, EditOperation, EditPlan,
        MAX_CANVAS_PADDING, MAX_CANVAS_PIXELS, MAX_PASTED_IMAGES, MAX_SPOTLIGHT_DIM_PERCENT,
        MIN_SPOTLIGHT_DIM_PERCENT, Rect, RgbaImage, decode_png,
    },
};
pub use kestrel_services::clipboard::{ClipboardCommand, ClipboardLifecycle};
pub use kestrel_services::microphone::MicrophoneCommand;
pub use kestrel_services::quick_toggles::QuickToggleCommand;
pub use kestrel_services::screen_tools::Selection;
pub use kestrel_services::snippets::{InsertionReport, SnippetMatch};
pub use runtime::{
    ApplicationRuntime, ApplicationWindowDecision, FeaturePreset, HostCapabilities, OVERLAY_ID,
    run_process_action,
};
pub use snippets::{load_snippets, save_snippets, snippet_path};
pub use status_notifier::{
    FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration, TrayReadout, TrayReadoutSink,
};
pub use view_model::{
    ApplicationViewModel, AudioOutputViewModel, AudioStreamViewModel, AudioViewModel,
    CameraViewModel, CaptureEntryViewModel, CaptureViewModel, ClipboardItemViewModel,
    ClipboardPreviewViewModel, ClipboardViewModel, CommandBarViewModel, CommandProvider,
    ConfirmationViewModel, CoreGroupViewModel, DevicePriorityViewModel, FeatureViewModel,
    ListenersViewModel, MicrophoneViewModel, MonitorViewModel, OutputKind, OverviewTool,
    OverviewToolsViewModel, ProcessRowViewModel, ProcessesViewModel, QuickToggleControlViewModel,
    QuickToggleViewModel, RecordingControls, RecordingExportViewModel, RecordingViewModel,
    ScratchpadViewModel, ScreenToolResultViewModel, ScreenToolsViewModel, SetupPresetViewModel,
    SetupSensitiveViewModel, SetupViewModel, ShortcutsViewModel, SnippetDraft,
    SnippetDraftViewModel, SnippetsViewModel, SpeedTestViewModel, TurnOnViewModel, WhatsNewSection,
    WhatsNewViewModel, output_icon, shortcut_label,
};

/// Command-bar action from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandBarCommand {
    /// Runs a ranked result.
    Run(String),
    Pin {
        id: String,
        pinned: bool,
    },
}

/// Command-bar provider switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandProviderSwitch {
    Applications(bool),
    Files(bool),
    Scripts(bool),
    Emoji(bool),
    Windows(bool),
    Clipboard(bool),
}

/// Snippet-library action from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetCommand {
    /// Renders and types a stored snippet.
    Insert(String),
    /// Validates and stores a snippet.
    Save {
        name: String,
        folder: String,
        trigger: String,
        content: String,
    },
    Delete(String),
}

/// Snippet bound edited from settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetLimit {
    ContentBytes(u32),
    ClipboardBytes(u32),
    InsertTimeoutMillis(u64),
}

/// Recent-capture bound from the settings group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureLimit {
    MaxEntries(u32),
    MaxTotalMegabytes(u32),
    MaxAgeHours(u32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFollowUpAction {
    Open,
    Edit,
    Copy,
}

/// Process inspector setting edited from Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLimit {
    RefreshMillis(u32),
    MaxRows(u32),
}

/// One logging setting changed on the Settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggingSetting {
    Debug(bool),
    RetentionDays(u32),
    MaxTotalMegabytes(u32),
}

/// A status overlay module switched on the Settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayModule {
    Microphone,
    Recording,
    Alerts,
    Volume,
    MicrophoneLevel,
    KeyboardLight,
    Accessories,
}

/// Scratchpad bound edited from Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchpadLimit {
    MaxNotes(u32),
    MaxNoteBytes(u32),
    MaxAgeDays(u32),
}

/// Screen tool setting edited from Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenToolsSetting {
    OcrLanguage(String),
    OcrTimeoutSeconds(u32),
}

/// Screen tool action from the window or the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenToolRequest {
    /// Captures a frame or opens the color picker.
    Begin(ScreenTool),
    /// Hands the selection view's choice over for processing.
    Select(Selection),
    Cancel,
    /// Clears the shown result.
    Dismiss,
}

/// Camera preview action from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraRequest {
    /// Opens this camera; it must be one the window listed.
    Start(std::path::PathBuf),
    Stop,
}

/// Screen recording action from the window or the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingRequest {
    /// Uses the page's preferred source, favoring a whole screen.
    StartDefault,
    /// Asks the desktop to share this source and records it.
    Start(RecordingSource),
    /// The area view's choice; `None` records the whole frame.
    ChooseArea(Option<Rect>),
    Pause,
    Resume,
    /// Completes the take.
    Stop,
    /// Ends the session and discards what was recorded.
    Cancel,
    /// Clears the finished or failed outcome.
    Dismiss,
    /// Removes a listed take.
    Delete(std::path::PathBuf),
    /// Writes `start..end` of a listed take to a new file.
    Export {
        take: std::path::PathBuf,
        start: std::time::Duration,
        end: std::time::Duration,
        destination: std::path::PathBuf,
    },
    /// Stops the running export and removes its partial file.
    CancelExport,
    /// Clears the finished or failed export.
    DismissExport,
}

/// Frame of the take being trimmed at a position, for the trim preview.
pub type TrimPreview =
    std::sync::Arc<dyn Fn(std::time::Duration) -> Result<RgbaImage, String> + Send + Sync>;

/// Recording default changed in the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingSetting {
    Framerate(u32),
    Cursor(bool),
    Track(AudioTrack, bool),
    /// Records one application's playback; the whole output for `None`.
    SystemAudioApplication(Option<String>),
}
/// Bounded clipboard retention value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardLimit {
    Items(u32),
    ItemBytes(u32),
    ImageBytes(u32),
    FileEntries(u32),
    MaxAgeHours(u32),
    ClearSeconds(u64),
}

/// Direction for ordered readouts, device rankings, and pinned applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMoveDirection {
    Up,
    Down,
}

/// A monitoring preference edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorSetting {
    NetworkRateUnit(NetworkRateUnit),
    PerCoreCpu(bool),
    TrayReadout {
        readout: MonitorReadout,
        shown: bool,
    },
}

impl MonitorSetting {
    /// Applies the edit within bounds; false when nothing changed.
    pub fn apply(self, monitoring: &mut kestrel_core::MonitorConfiguration) -> bool {
        match self {
            Self::NetworkRateUnit(unit) => {
                unit != std::mem::replace(&mut monitoring.network_rate_unit, unit)
            }
            Self::PerCoreCpu(enabled) => {
                enabled != std::mem::replace(&mut monitoring.per_core_cpu, enabled)
            }
            Self::TrayReadout { readout, shown } => {
                let tray = &mut monitoring.tray_readouts;
                let present = tray.contains(&readout);
                if shown && !present && tray.len() < kestrel_core::MAX_TRAY_READOUTS {
                    tray.push(readout);
                    true
                } else if !shown && present {
                    tray.retain(|entry| *entry != readout);
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// The server side a device ranking applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDirection {
    Output,
    Input,
}

/// Edit to saved device rankings or pinned mixer applications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPreferenceEdit {
    /// Appends a device name to the ranking.
    Rank {
        direction: AudioDirection,
        name: String,
    },
    Unrank {
        direction: AudioDirection,
        name: String,
    },
    MoveDevice {
        direction: AudioDirection,
        name: String,
        toward: PanelMoveDirection,
    },
    Follow {
        direction: AudioDirection,
        follow: bool,
    },
    /// Appends an application identity to the pinned list.
    Pin(String),
    Unpin(String),
    MovePinned {
        application_id: String,
        toward: PanelMoveDirection,
    },
}

impl AudioPreferenceEdit {
    /// Applies the edit within the configured bounds; false when nothing changed.
    pub fn apply(&self, audio: &mut kestrel_core::AudioConfiguration) -> bool {
        fn ranking(
            audio: &mut kestrel_core::AudioConfiguration,
            direction: AudioDirection,
        ) -> &mut Vec<String> {
            match direction {
                AudioDirection::Output => &mut audio.output_priority,
                AudioDirection::Input => &mut audio.input_priority,
            }
        }
        match self {
            Self::Rank { direction, name } => append_bounded(
                ranking(audio, *direction),
                name,
                kestrel_core::MAX_AUDIO_PRIORITY_DEVICES,
            ),
            Self::Unrank { direction, name } => remove_entry(ranking(audio, *direction), name),
            Self::MoveDevice {
                direction,
                name,
                toward,
            } => move_entry(ranking(audio, *direction), name, *toward),
            Self::Follow { direction, follow } => {
                let current = match direction {
                    AudioDirection::Output => &mut audio.follow_output_priority,
                    AudioDirection::Input => &mut audio.follow_input_priority,
                };
                *current != std::mem::replace(current, *follow)
            }
            Self::Pin(application_id) => append_bounded(
                &mut audio.mixer_favorites,
                application_id,
                kestrel_core::MAX_MIXER_FAVORITES,
            ),
            Self::Unpin(application_id) => remove_entry(&mut audio.mixer_favorites, application_id),
            Self::MovePinned {
                application_id,
                toward,
            } => move_entry(&mut audio.mixer_favorites, application_id, *toward),
        }
    }
}

fn append_bounded(list: &mut Vec<String>, entry: &str, limit: usize) -> bool {
    list.push(entry.to_owned());
    let valid = kestrel_core::valid_audio_identity_list(list, limit);
    if !valid {
        list.pop();
    }
    valid
}

fn remove_entry(list: &mut Vec<String>, entry: &str) -> bool {
    let before = list.len();
    list.retain(|existing| existing != entry);
    list.len() != before
}

pub fn move_entry<T: PartialEq>(list: &mut [T], entry: &T, toward: PanelMoveDirection) -> bool {
    let Some(index) = list.iter().position(|existing| existing == entry) else {
        return false;
    };
    let target = match toward {
        PanelMoveDirection::Up => index.checked_sub(1),
        PanelMoveDirection::Down => (index + 1 < list.len()).then_some(index + 1),
    };
    let Some(target) = target else { return false };
    list.swap(index, target);
    true
}

/// A window control a command can focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    CommandBar,
    QuickPanel,
    Clipboard,
    Capture,
    Processes,
    Scratchpad,
    /// The Monitoring page, where active alerts are listed.
    Monitoring,
    /// The Audio page with the mixer.
    Audio,
    /// The Notes page, where the snippet library leads.
    Snippets,
    /// The speed test on the Monitoring page, scrolled into view.
    SpeedTest,
    /// The camera preview on the Capture page, scrolled into view.
    Camera,
    /// The Feature Hub, where features are switched.
    FeatureHub,
}

/// A process action the user can confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessActionKind {
    /// SIGTERM, then a short wait.
    Terminate,
    /// SIGKILL.
    Kill,
    /// Restart through the process's user unit or desktop launch;
    /// `escalate` sends SIGKILL to scope members that outlive the stop timeout.
    Restart { escalate: bool },
}

/// Process inspector request from the window or a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessRequest {
    Search(String),
    Refresh,
    Confirm {
        identity: kestrel_platform::processes::ProcessIdentity,
        action: ProcessActionKind,
    },
    Run {
        identity: kestrel_platform::processes::ProcessIdentity,
        action: ProcessActionKind,
    },
}

/// Scratchpad tab and editor actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchpadCommand {
    New(String),
    Rename {
        id: u64,
        title: String,
    },
    Switch(u64),
    Close(u64),
    Update {
        id: u64,
        text: String,
    },
    RemoveExpired,
    Export {
        id: u64,
        destination: std::path::PathBuf,
    },
}
/// Command from an application entry surface.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationCommand {
    PresentWindow,
    /// An XDG activation token from a tray host; the next presentation uses it.
    ActivationToken(String),
    /// Presents the window and focuses one control.
    Focus(FocusTarget),
    RefreshCapabilities,
    QuickToggle(QuickToggleCommand),
    /// Flips a quick toggle from its current observed state.
    FlipQuickToggle(QuickToggleId),
    SetFeatureEnabled {
        feature_id: String,
        enabled: bool,
    },
    ApplyPreset(FeaturePreset),
    /// Finishes first-launch setup: the chosen preset, minus the features
    /// left off. No preset changes nothing.
    FinishSetup {
        preset: Option<FeaturePreset>,
        left_off: Vec<String>,
    },
    /// Records setup as done without changing any feature.
    SkipSetup,
    UndoPreset,
    SetAppearance(AppearancePreference),
    SetCloseAction(CloseAction),
    SetAutostart(bool),
    /// Offer this release's notes on Overview after an update.
    SetWhatsNew(bool),
    /// The user opened this release's notes; record the version as seen.
    MarkWhatsNewRead,
    SetMonitorReadoutVisible {
        readout: MonitorReadout,
        visible: bool,
    },
    /// Monitoring units, per-core load, and tray readouts.
    SetMonitorSetting(MonitorSetting),
    /// Reads local addresses into the monitoring panel, or hides them.
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
    /// How long a reading must stay past the threshold before alerting.
    SetAlertSustain {
        kind: AlertKind,
        duration: std::time::Duration,
    },
    /// Mixer mutation.
    Audio(AudioCommand),
    /// Microphone mutation.
    Microphone(MicrophoneCommand),
    /// Starts a user-requested speed test.
    StartSpeedTest,
    /// Cancels the speed test.
    CancelSpeedTest,
    /// Mixer amplification ceiling.
    SetAudioBoostPercent(u8),
    SetAudioOutputSwitch(AudioOutputSwitch),
    SetAudioDisconnectPolicy(AudioDisconnectPolicy),
    SetAudioDisconnectVolumePercent(u8),
    SetAudioIncludeInactiveStreams(bool),
    /// Device rankings and pinned mixer applications.
    AudioPreference(AudioPreferenceEdit),
    /// Clipboard history mutation.
    Clipboard(ClipboardCommand),
    /// Bounded clipboard search.
    ClipboardSearch(String),
    /// Bounded entry preview.
    ClipboardPreview(u64),
    /// Reads the text in an image entry and copies it without retaining it.
    ClipboardCopyImageText(u64),
    SetClipboardLayout(ClipboardLayout),
    SetClipboardLimit(ClipboardLimit),
    SetCaptureLimit(CaptureLimit),
    SetCaptureAfterScreenshot(CaptureAfterScreenshot),
    SetProcessLimit(ProcessLimit),
    SetScratchpadLimit(ScratchpadLimit),
    SetLoggingSetting(LoggingSetting),
    /// Shows or hides one status overlay module.
    SetOverlayModule {
        module: OverlayModule,
        shown: bool,
    },
    /// Sets both accessory battery levels together.
    SetAccessoryLevels {
        warning: u8,
        critical: u8,
    },
    /// Opens the log folder in the file manager.
    OpenLogFolder,
    ClearLogs,
    /// Sets a command ID's global shortcut, or removes it for `None`.
    SetShortcut {
        command: String,
        trigger: Option<kestrel_core::ShortcutTrigger>,
    },
    Processes(ProcessRequest),
    Camera(CameraRequest),
    Recording(RecordingRequest),
    /// Opens the trim editor for a listed take.
    TrimRecording(std::path::PathBuf),
    SetRecordingSetting(RecordingSetting),
    CopyText(String),
    OpenUri(String),
    ScreenTools(ScreenToolRequest),
    SetScreenToolsSetting(ScreenToolsSetting),
    SetClipboardFilterSensitive(bool),
    SetClipboardPastePlainText(bool),
    /// Snippet-library action.
    Snippet(SnippetCommand),
    /// Runs the snippet search whose results the window shows.
    SnippetSearch(String),
    /// Loads one stored snippet into the editor.
    SnippetEdit(String),
    /// Opens an empty editor for a new snippet.
    SnippetNew,
    SetSnippetLimit(SnippetLimit),
    SetSnippetProvider(SnippetProviderPreference),
    /// Scratchpad tab and editor actions.
    Scratchpad(ScratchpadCommand),
    SetSnippetExpansionTiming(SnippetExpansionTiming),
    /// Runs the command bar query whose results the window shows.
    CommandQuery(String),
    CommandBar(CommandBarCommand),
    ResetCommandRanking,
    SetCommandResultLimit(u32),
    SetQuickPanelFavorites(Vec<String>),
    SetCommandProvider(CommandProviderSwitch),
    ImportConfiguration(std::path::PathBuf),
    ExportConfiguration(std::path::PathBuf),
    Capture(CaptureRequest),
    CaptureFollowUp {
        id: CaptureId,
        action: CaptureFollowUpAction,
    },
    Quit,
}

/// Screenshot action from the window or the command line.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureRequest {
    /// `None` uses the provider's preferred mode.
    Begin(Option<CaptureMode>),
    Cancel,
    /// Puts the image on the clipboard.
    Copy(CaptureId),
    Save {
        id: CaptureId,
        destination: std::path::PathBuf,
    },
    /// Opens the editor on the newest capture.
    EditLatest,
    /// Opens the editor on a capture.
    Edit(CaptureId),
    /// Stores the edited image as a new capture.
    SaveEdit {
        id: CaptureId,
        plan: EditPlan,
    },
    Delete(CaptureId),
    Clear,
}

#[cfg(test)]
mod tests {
    use super::{AudioDirection, AudioPreferenceEdit, PanelMoveDirection};
    use kestrel_core::{AudioConfiguration, MAX_MIXER_FAVORITES};

    #[test]
    fn rankings_reorder_within_bounds_and_report_no_ops() {
        let mut audio = AudioConfiguration::default();
        let rank = |name: &str| AudioPreferenceEdit::Rank {
            direction: AudioDirection::Output,
            name: name.to_owned(),
        };
        assert!(rank("speakers").apply(&mut audio));
        assert!(rank("headset").apply(&mut audio));
        assert!(!rank("headset").apply(&mut audio), "already ranked");
        assert!(!rank("").apply(&mut audio), "blank name");
        let up = AudioPreferenceEdit::MoveDevice {
            direction: AudioDirection::Output,
            name: "headset".to_owned(),
            toward: PanelMoveDirection::Up,
        };
        assert!(up.apply(&mut audio));
        assert!(!up.apply(&mut audio), "already first");
        assert_eq!(audio.output_priority, ["headset", "speakers"]);
        assert!(audio.input_priority.is_empty(), "directions are separate");
        assert!(
            AudioPreferenceEdit::Unrank {
                direction: AudioDirection::Output,
                name: "headset".to_owned(),
            }
            .apply(&mut audio)
        );
        assert_eq!(audio.output_priority, ["speakers"]);
    }

    #[test]
    fn tray_readouts_stop_at_the_limit_and_report_no_ops() {
        use super::{MonitorReadout, MonitorSetting};
        let mut monitoring = kestrel_core::MonitorConfiguration::default();
        let tray = |readout, shown| MonitorSetting::TrayReadout { readout, shown };
        assert!(tray(MonitorReadout::Cpu, true).apply(&mut monitoring));
        assert!(
            !tray(MonitorReadout::Cpu, true).apply(&mut monitoring),
            "already shown"
        );
        assert!(tray(MonitorReadout::Memory, true).apply(&mut monitoring));
        assert!(tray(MonitorReadout::Battery, true).apply(&mut monitoring));
        assert!(
            !tray(MonitorReadout::Gpu, true).apply(&mut monitoring),
            "tray is full"
        );
        assert!(tray(MonitorReadout::Memory, false).apply(&mut monitoring));
        assert_eq!(
            monitoring.tray_readouts,
            [MonitorReadout::Cpu, MonitorReadout::Battery]
        );
        assert!(!MonitorSetting::PerCoreCpu(false).apply(&mut monitoring));
        assert!(MonitorSetting::PerCoreCpu(true).apply(&mut monitoring));
    }

    #[test]
    fn pinning_stops_at_the_limit() {
        let mut audio = AudioConfiguration::default();
        for index in 0..MAX_MIXER_FAVORITES {
            assert!(AudioPreferenceEdit::Pin(format!("app-{index}")).apply(&mut audio));
        }
        assert!(!AudioPreferenceEdit::Pin("one-more".to_owned()).apply(&mut audio));
        assert_eq!(audio.mixer_favorites.len(), MAX_MIXER_FAVORITES);
        assert!(audio.validate().is_ok());
    }
}
