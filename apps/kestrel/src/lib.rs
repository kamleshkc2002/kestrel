//! Application configuration and runtime composition.

mod autostart;
mod command_bar;
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
pub use kestrel_platform::clipboard::ClipboardEntryKind;
pub use kestrel_platform::quick_toggles::{QuickToggleId, QuickToggleMutation};
pub use kestrel_services::audio::{AudioCommand, AudioCycleDirection};
pub use kestrel_services::clipboard::{ClipboardCommand, ClipboardLifecycle};
pub use kestrel_services::microphone::MicrophoneCommand;
pub use kestrel_services::quick_toggles::QuickToggleCommand;
pub use kestrel_services::snippets::{InsertionReport, SnippetMatch};
pub use runtime::{ApplicationRuntime, FeaturePreset};
pub use snippets::{
    CURRENT_SNIPPET_SCHEMA_VERSION, LoadedSnippets, SnippetFile, SnippetStoreError, load_snippets,
    save_snippets, snippet_path,
};
pub use status_notifier::{FEATURE_ID as STATUS_NOTIFIER_ID, StatusNotifierIntegration};
pub use view_model::{
    ActiveAlertViewModel, AlertRuleViewModel, ApplicationViewModel, AudioOutputGroupViewModel,
    AudioOutputViewModel, AudioPolicyViewModel, AudioStreamViewModel, AudioViewModel,
    CapabilityKindViewModel, CapabilityStatusViewModel, CapabilityViewModel,
    ClipboardBoundsViewModel, ClipboardItemViewModel, ClipboardPolicyViewModel,
    ClipboardPreviewViewModel, ClipboardViewModel, CommandBarViewModel, CommandProvider,
    CommandProviderViewModel, CommandRankingViewModel, CommandResultViewModel,
    ConfigurationWarningViewModel, ConfirmationViewModel, FeatureLifecycleViewModel,
    FeatureViewModel, MicrophoneInputViewModel, MicrophoneViewModel,
    MonitorReadoutSettingViewModel, MonitorReadoutViewModel, MonitorViewModel,
    PanelSectionViewModel, QuickToggleActionViewModel, QuickToggleControlViewModel,
    QuickToggleViewModel, RemediationViewModel, SnippetBoundsViewModel, SnippetDraft,
    SnippetDraftViewModel, SnippetItemViewModel, SnippetPolicyViewModel, SnippetsViewModel,
    SpeedTestViewModel,
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

/// Command from an application entry surface.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationCommand {
    PresentWindow,
    RefreshCapabilities,
    QuickToggle(QuickToggleCommand),
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
    SetSnippetExpansionTiming(SnippetExpansionTiming),
    /// Runs the command bar query whose results the window shows.
    CommandQuery(String),
    CommandBar(CommandBarCommand),
    ResetCommandRanking,
    SetCommandResultLimit(u32),
    SetCommandProvider(CommandProviderSwitch),
    ImportConfiguration(std::path::PathBuf),
    ExportConfiguration(std::path::PathBuf),
    Quit,
}
