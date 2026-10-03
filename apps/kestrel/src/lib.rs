//! Application-level configuration and runtime composition without UI ownership.

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
    FeatureViewModel, MonitorReadoutSettingViewModel, MonitorReadoutViewModel, MonitorViewModel,
    PanelSectionViewModel, QuickToggleActionViewModel, QuickToggleControlViewModel,
    QuickToggleViewModel, RemediationViewModel, SnippetBoundsViewModel, SnippetDraft,
    SnippetDraftViewModel, SnippetItemViewModel, SnippetPolicyViewModel, SnippetsViewModel,
};

/// One command bar action requested from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandBarCommand {
    /// Runs the ranked result with this identifier.
    Run(String),
    Pin {
        id: String,
        pinned: bool,
    },
}

/// One command bar provider switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandProviderSwitch {
    Applications(bool),
    Files(bool),
    Scripts(bool),
    Emoji(bool),
}

/// One snippet library action requested from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetCommand {
    /// Renders and types one stored snippet.
    Insert(String),
    /// Validates and stores a definition, replacing one with the same name.
    Save {
        name: String,
        folder: String,
        trigger: String,
        content: String,
    },
    Delete(String),
}

/// One snippet bound edited from the settings controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetLimit {
    ContentBytes(u32),
    ClipboardBytes(u32),
    InsertTimeoutMillis(u64),
}

/// One bounded clipboard retention value edited from the settings controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardLimit {
    Items(u32),
    ItemBytes(u32),
    ImageBytes(u32),
    FileEntries(u32),
    MaxAgeHours(u32),
    ClearSeconds(u64),
}

/// Direction for moving one of the ordered panel sections or monitoring readouts.
///
/// Monitoring readouts use the same ordering semantics as panel sections: `Up`
/// moves a readout toward the beginning of the configured readout order and
/// `Down` moves it toward the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMoveDirection {
    Up,
    Down,
}

/// Commands accepted from application entry surfaces such as the normal window and tray menu.
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
    /// A mixer mutation applied through the opt-in audio service.
    Audio(AudioCommand),
    /// The amplification ceiling for the mixer, between 100% and the hard cap.
    SetAudioBoostPercent(u8),
    SetAudioOutputSwitch(AudioOutputSwitch),
    SetAudioDisconnectPolicy(AudioDisconnectPolicy),
    SetAudioDisconnectVolumePercent(u8),
    SetAudioIncludeInactiveStreams(bool),
    /// A clipboard history mutation (copy, pin, delete, edit, clear, wipe).
    Clipboard(ClipboardCommand),
    /// Runs an explicit, bounded search over retained entries.
    ClipboardSearch(String),
    /// Requests one bounded entry preview for display.
    ClipboardPreview(u64),
    SetClipboardLimit(ClipboardLimit),
    SetClipboardFilterSensitive(bool),
    SetClipboardPastePlainText(bool),
    /// Runs one snippet library action.
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
