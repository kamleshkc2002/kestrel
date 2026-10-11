//! Application configuration and runtime composition.

mod autostart;
mod command_bar;
pub mod command_line;
mod config;
mod private_file;
mod runtime;
mod snippets;
mod status_notifier;
pub mod view_model;

pub use autostart::{AutostartError, DESKTOP_FILE_NAME, autostart_path, disable, enable};
pub use command_bar::{
    CURRENT_RANKING_SCHEMA_VERSION, LoadedRanking, RankingEntry, RankingFile, load_ranking,
    ranking_path, save_ranking,
};
pub use config::{
    ConfigurationLoadError, ConfigurationWarning, LoadedConfiguration, configuration_path,
    export_file, export_string, import_file, import_string, load, save,
};
pub use kestrel_core::{
    AlertKind, AppearancePreference, AudioDisconnectPolicy, AudioOutputSwitch, MonitorReadout,
    PanelSection,
};
pub use kestrel_core::{Snippet, SnippetExpansionTiming, SnippetProviderPreference};
pub use kestrel_platform::capture::CaptureMode;
pub use kestrel_platform::clipboard::ClipboardEntryKind;
pub use kestrel_platform::quick_toggles::{QuickToggleId, QuickToggleMutation};
pub use kestrel_platform::recording::{AudioTrack, RecordingSource};
pub use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
pub use kestrel_services::audio::{AudioCommand, AudioCycleDirection};
pub use kestrel_services::camera::CameraFrameSlot;
pub use kestrel_services::capture::{
    CaptureId,
    image::{Color, EditOperation, EditPlan, Rect, RgbaImage, decode_png},
};
pub use kestrel_services::clipboard::{ClipboardCommand, ClipboardLifecycle};
pub use kestrel_services::microphone::MicrophoneCommand;
pub use kestrel_services::quick_toggles::QuickToggleCommand;
pub use kestrel_services::screen_tools::Selection;
pub use kestrel_services::snippets::{InsertionReport, SnippetMatch};
pub use runtime::{ApplicationRuntime, FeaturePreset, run_process_action};
pub use snippets::{
    CURRENT_SNIPPET_SCHEMA_VERSION, LoadedSnippets, SnippetFile, SnippetStoreError, load_snippets,
    save_snippets, snippet_path,
};
pub use status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration};
pub use view_model::{
    ActiveAlertViewModel, AlertRuleViewModel, ApplicationViewModel, AudioOutputGroupViewModel,
    AudioOutputViewModel, AudioPolicyViewModel, AudioStreamViewModel, AudioViewModel,
    CameraChoiceViewModel, CameraViewModel, CapabilityKindViewModel, CapabilityStatusViewModel,
    CapabilityViewModel, CaptureEntryViewModel, CaptureModeViewModel, CaptureViewModel,
    ClipboardBoundsViewModel, ClipboardItemViewModel, ClipboardPolicyViewModel,
    ClipboardPreviewViewModel, ClipboardViewModel, CommandBarViewModel, CommandProvider,
    CommandProviderViewModel, CommandRankingViewModel, CommandResultViewModel,
    ConfigurationWarningViewModel, ConfirmationViewModel, FeatureLifecycleViewModel,
    FeatureViewModel, ListenerRowViewModel, ListenersViewModel, MicrophoneInputViewModel,
    MicrophoneViewModel, MonitorReadoutSettingViewModel, MonitorReadoutViewModel, MonitorViewModel,
    PanelSectionViewModel, ProcessActionViewModel, ProcessRowViewModel, ProcessesViewModel,
    QrRowViewModel, QuickToggleActionViewModel, QuickToggleControlViewModel, QuickToggleViewModel,
    RecordingControls, RecordingTakeViewModel, RecordingTrackOptionViewModel,
    RecordingTrackViewModel, RecordingViewModel, RemediationViewModel, ScratchpadExpiredViewModel,
    ScratchpadTabViewModel, ScratchpadViewModel, ScreenToolButtonViewModel,
    ScreenToolResultViewModel, ScreenToolsViewModel, ShortcutRowViewModel, ShortcutsViewModel,
    SnippetBoundsViewModel, SnippetDraft, SnippetDraftViewModel, SnippetItemViewModel,
    SnippetPolicyViewModel, SnippetsViewModel, SpeedTestViewModel,
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

/// Process inspector setting edited from Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLimit {
    RefreshMillis(u32),
    MaxRows(u32),
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
}

/// Recording default changed in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingSetting {
    Framerate(u32),
    Cursor(bool),
    Track(AudioTrack, bool),
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

/// Direction for ordered panel sections or readouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMoveDirection {
    Up,
    Down,
}

/// A window control a command can focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    CommandBar,
    Clipboard,
    Processes,
    Scratchpad,
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
    UndoPreset,
    SetAppearance(AppearancePreference),
    SetAutostart(bool),
    SetPanelVisibility {
        section: PanelSection,
        visible: bool,
    },
    MovePanelSection {
        section: PanelSection,
        direction: PanelMoveDirection,
    },
    SetMonitorReadoutVisible {
        readout: MonitorReadout,
        visible: bool,
    },
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
    /// Clipboard history mutation.
    Clipboard(ClipboardCommand),
    /// Bounded clipboard search.
    ClipboardSearch(String),
    /// Bounded entry preview.
    ClipboardPreview(u64),
    SetClipboardLimit(ClipboardLimit),
    SetCaptureLimit(CaptureLimit),
    SetProcessLimit(ProcessLimit),
    SetScratchpadLimit(ScratchpadLimit),
    Processes(ProcessRequest),
    Camera(CameraRequest),
    Recording(RecordingRequest),
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
    SetCommandProvider(CommandProviderSwitch),
    ImportConfiguration(std::path::PathBuf),
    ExportConfiguration(std::path::PathBuf),
    Capture(CaptureRequest),
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
