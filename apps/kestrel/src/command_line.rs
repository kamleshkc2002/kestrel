//! Command-line entry points: `kestrel --command <id>` forwards one stable,
//! typed action to the running instance, so any desktop's keybinding settings
//! can trigger Kestrel actions.

use std::{ffi::OsString, fmt};

use kestrel_core::MAX_SHORTCUT_COMMAND_CHARS;
use kestrel_platform::{
    applications::FEATURE_ID as COMMAND_BAR_ID,
    audio::{FEATURE_ID as AUDIO_MIXER_ID, MICROPHONE_FEATURE_ID},
    clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID,
    quick_toggles::{ALL_QUICK_TOGGLES, QuickToggleId},
    speed_test::FEATURE_ID as SPEED_TEST_ID,
};
use kestrel_services::{
    audio::{AudioCommand, AudioCycleDirection},
    clipboard::ClipboardCommand,
    microphone::MicrophoneCommand,
};

use crate::{ApplicationCommand, FeaturePreset, FocusTarget};

const TOGGLE_PREFIX: &str = "toggle.";

/// One action a command line can trigger, identified by a stable ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLineAction {
    ShowWindow,
    /// Opens the window with the command bar query focused.
    OpenCommandBar,
    /// Opens the window with the clipboard history search focused.
    QuickPaste,
    RefreshCapabilities,
    Quit,
    NextOutput,
    PreviousOutput,
    ToggleMicrophoneMute,
    MuteMicrophone,
    UnmuteMicrophone,
    StartSpeedTest,
    CancelSpeedTest,
    WipeClipboard,
    ClearClipboardSelection,
    ApplyPreset(FeaturePreset),
    UndoPreset,
    /// Flips a quick toggle from its current observed state.
    QuickToggle(QuickToggleId),
}

const FIXED_ACTIONS: [CommandLineAction; 18] = [
    CommandLineAction::ShowWindow,
    CommandLineAction::OpenCommandBar,
    CommandLineAction::QuickPaste,
    CommandLineAction::RefreshCapabilities,
    CommandLineAction::Quit,
    CommandLineAction::NextOutput,
    CommandLineAction::PreviousOutput,
    CommandLineAction::ToggleMicrophoneMute,
    CommandLineAction::MuteMicrophone,
    CommandLineAction::UnmuteMicrophone,
    CommandLineAction::StartSpeedTest,
    CommandLineAction::CancelSpeedTest,
    CommandLineAction::WipeClipboard,
    CommandLineAction::ClearClipboardSelection,
    CommandLineAction::ApplyPreset(FeaturePreset::Essentials),
    CommandLineAction::ApplyPreset(FeaturePreset::Balanced),
    CommandLineAction::ApplyPreset(FeaturePreset::Everything),
    CommandLineAction::UndoPreset,
];

impl CommandLineAction {
    /// Every action in a stable order, one per quick toggle.
    pub fn all() -> Vec<Self> {
        FIXED_ACTIONS
            .into_iter()
            .chain(ALL_QUICK_TOGGLES.into_iter().map(Self::QuickToggle))
            .collect()
    }

    /// The stable ID used on the command line and in keybinding settings.
    pub fn id(self) -> String {
        let fixed = match self {
            Self::ShowWindow => "window.show",
            Self::OpenCommandBar => "command-bar.open",
            Self::QuickPaste => "clipboard.quick-paste",
            Self::RefreshCapabilities => "capabilities.refresh",
            Self::Quit => "app.quit",
            Self::NextOutput => "audio.output-next",
            Self::PreviousOutput => "audio.output-previous",
            Self::ToggleMicrophoneMute => "microphone.toggle-mute",
            Self::MuteMicrophone => "microphone.mute",
            Self::UnmuteMicrophone => "microphone.unmute",
            Self::StartSpeedTest => "speed-test.start",
            Self::CancelSpeedTest => "speed-test.cancel",
            Self::WipeClipboard => "clipboard.wipe",
            Self::ClearClipboardSelection => "clipboard.clear-selection",
            Self::ApplyPreset(FeaturePreset::Essentials) => "preset.essentials",
            Self::ApplyPreset(FeaturePreset::Balanced) => "preset.balanced",
            Self::ApplyPreset(FeaturePreset::Everything) => "preset.everything",
            Self::UndoPreset => "preset.undo",
            Self::QuickToggle(toggle) => return format!("{TOGGLE_PREFIX}{}", toggle.feature_id()),
        };
        fixed.to_owned()
    }

    pub fn description(self) -> String {
        match self {
            Self::ShowWindow => "Open the Kestrel window".to_owned(),
            Self::OpenCommandBar => "Open the command bar".to_owned(),
            Self::QuickPaste => "Search clipboard history to copy an entry".to_owned(),
            Self::RefreshCapabilities => "Re-run capability probes".to_owned(),
            Self::Quit => "Quit Kestrel".to_owned(),
            Self::NextOutput => "Switch to the next audio output".to_owned(),
            Self::PreviousOutput => "Switch to the previous audio output".to_owned(),
            Self::ToggleMicrophoneMute => "Mute or unmute every microphone input".to_owned(),
            Self::MuteMicrophone => "Mute every microphone input".to_owned(),
            Self::UnmuteMicrophone => "Unmute every microphone input".to_owned(),
            Self::StartSpeedTest => "Start a network speed test".to_owned(),
            Self::CancelSpeedTest => "Cancel the running speed test".to_owned(),
            Self::WipeClipboard => "Wipe clipboard history".to_owned(),
            Self::ClearClipboardSelection => "Clear the live clipboard selection".to_owned(),
            Self::ApplyPreset(preset) => format!("Apply the {} preset", preset.label()),
            Self::UndoPreset => "Undo the last preset".to_owned(),
            Self::QuickToggle(toggle) => format!("Toggle {}", toggle.label()),
        }
    }

    /// Resolves a stable ID.
    pub fn parse(id: &str) -> Result<Self, CommandLineError> {
        let well_formed = !id.is_empty()
            && id.len() <= MAX_SHORTCUT_COMMAND_CHARS
            && id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
            });
        if !well_formed {
            return Err(CommandLineError::MalformedCommandId(bounded(id)));
        }
        if let Some(feature_id) = id.strip_prefix(TOGGLE_PREFIX) {
            return ALL_QUICK_TOGGLES
                .into_iter()
                .find(|toggle| toggle.feature_id() == feature_id)
                .map(Self::QuickToggle)
                .ok_or_else(|| CommandLineError::UnknownCommand(id.to_owned()));
        }
        FIXED_ACTIONS
            .into_iter()
            .find(|action| action.id() == id)
            .ok_or_else(|| CommandLineError::UnknownCommand(id.to_owned()))
    }

    /// The feature that must be running for this action to do anything.
    pub fn required_feature(self) -> Option<&'static str> {
        match self {
            Self::ShowWindow
            | Self::RefreshCapabilities
            | Self::Quit
            | Self::ApplyPreset(_)
            | Self::UndoPreset => None,
            Self::OpenCommandBar => Some(COMMAND_BAR_ID),
            Self::QuickPaste | Self::WipeClipboard | Self::ClearClipboardSelection => {
                Some(CLIPBOARD_HISTORY_ID)
            }
            Self::NextOutput | Self::PreviousOutput => Some(AUDIO_MIXER_ID),
            Self::ToggleMicrophoneMute | Self::MuteMicrophone | Self::UnmuteMicrophone => {
                Some(MICROPHONE_FEATURE_ID)
            }
            Self::StartSpeedTest | Self::CancelSpeedTest => Some(SPEED_TEST_ID),
            Self::QuickToggle(toggle) => Some(toggle.feature_id()),
        }
    }

    /// The application command the running instance executes.
    pub fn to_command(self) -> ApplicationCommand {
        match self {
            Self::ShowWindow => ApplicationCommand::PresentWindow,
            Self::OpenCommandBar => ApplicationCommand::Focus(FocusTarget::CommandBar),
            Self::QuickPaste => ApplicationCommand::Focus(FocusTarget::Clipboard),
            Self::RefreshCapabilities => ApplicationCommand::RefreshCapabilities,
            Self::Quit => ApplicationCommand::Quit,
            Self::NextOutput => ApplicationCommand::Audio(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Next,
            }),
            Self::PreviousOutput => ApplicationCommand::Audio(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Previous,
            }),
            Self::ToggleMicrophoneMute => {
                ApplicationCommand::Microphone(MicrophoneCommand::ToggleMute)
            }
            Self::MuteMicrophone => {
                ApplicationCommand::Microphone(MicrophoneCommand::SetMuted(true))
            }
            Self::UnmuteMicrophone => {
                ApplicationCommand::Microphone(MicrophoneCommand::SetMuted(false))
            }
            Self::StartSpeedTest => ApplicationCommand::StartSpeedTest,
            Self::CancelSpeedTest => ApplicationCommand::CancelSpeedTest,
            Self::WipeClipboard => ApplicationCommand::Clipboard(ClipboardCommand::Wipe),
            Self::ClearClipboardSelection => {
                ApplicationCommand::Clipboard(ClipboardCommand::ClearSelection)
            }
            Self::ApplyPreset(preset) => ApplicationCommand::ApplyPreset(preset),
            Self::UndoPreset => ApplicationCommand::UndoPreset,
            Self::QuickToggle(toggle) => ApplicationCommand::FlipQuickToggle(toggle),
        }
    }
}

/// What one invocation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// No arguments: open the window.
    Present,
    Run(CommandLineAction),
    ListCommands,
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineError {
    UnknownCommand(String),
    /// Empty, overlong, or outside `[a-z0-9._-]`; shown truncated and escaped.
    MalformedCommandId(String),
    MissingCommandId,
    UnexpectedArgument(String),
}

impl CommandLineError {
    pub const EXIT_STATUS: u8 = 2;
}

impl fmt::Display for CommandLineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(
                formatter,
                "unknown command \"{id}\"; run `kestrel --list-commands` for the supported IDs"
            ),
            Self::MalformedCommandId(id) => write!(
                formatter,
                "malformed command ID \"{id}\"; IDs use lowercase letters, digits, '.', '-', \
                 and '_' and are at most {MAX_SHORTCUT_COMMAND_CHARS} characters"
            ),
            Self::MissingCommandId => formatter.write_str("--command needs a command ID"),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument \"{}\"", bounded(argument))
            }
        }
    }
}

impl std::error::Error for CommandLineError {}

/// Why a recognized action cannot run in this instance right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandGate {
    /// The feature is turned off.
    Disabled { feature: String },
    /// The system lacks what the feature needs.
    Unavailable {
        feature: String,
        summary: String,
        remediation: Option<String>,
    },
    /// Enabled and supported, yet not running.
    Stopped { feature: String },
}

impl CommandGate {
    pub const fn exit_status(&self) -> u8 {
        match self {
            Self::Disabled { .. } => 3,
            Self::Unavailable { .. } => 4,
            Self::Stopped { .. } => 5,
        }
    }

    /// Rebuilds the gate a forwarded command ended with; the running
    /// instance's window shows the full reason.
    pub fn from_exit_status(status: i32, feature_id: &str) -> Option<Self> {
        let feature = feature_id.to_owned();
        match status {
            3 => Some(Self::Disabled { feature }),
            4 => Some(Self::Unavailable {
                feature,
                summary: "the Kestrel window shows the reason.".to_owned(),
                remediation: None,
            }),
            5 => Some(Self::Stopped { feature }),
            _ => None,
        }
    }
}

impl fmt::Display for CommandGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled { feature } => write!(
                formatter,
                "{feature} is disabled; enable it in the Feature Hub or apply a preset that \
                 includes it"
            ),
            Self::Unavailable {
                feature,
                summary,
                remediation,
            } => {
                write!(formatter, "{feature} is unavailable: {summary}")?;
                match remediation {
                    Some(remediation) => write!(formatter, " {remediation}"),
                    None => Ok(()),
                }
            }
            Self::Stopped { feature } => write!(
                formatter,
                "{feature} is enabled but not running; run `kestrel --command \
                 capabilities.refresh` or check the Feature Hub"
            ),
        }
    }
}

/// Truncates and escapes echoed input so diagnostics stay bounded and printable.
fn bounded(value: &str) -> String {
    value
        .chars()
        .take(MAX_SHORTCUT_COMMAND_CHARS)
        .flat_map(char::escape_default)
        .collect()
}

/// Parses the arguments after the program name.
pub fn parse_arguments(arguments: &[OsString]) -> Result<Invocation, CommandLineError> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>();
    match arguments
        .iter()
        .map(|argument| argument.as_ref())
        .collect::<Vec<&str>>()
        .as_slice()
    {
        [] => Ok(Invocation::Present),
        ["--help" | "-h"] => Ok(Invocation::Help),
        ["--version" | "-V"] => Ok(Invocation::Version),
        ["--list-commands"] => Ok(Invocation::ListCommands),
        ["--command"] => Err(CommandLineError::MissingCommandId),
        ["--command", id] => CommandLineAction::parse(id).map(Invocation::Run),
        [single] if single.starts_with("--command=") => {
            let id = &single["--command=".len()..];
            if id.is_empty() {
                return Err(CommandLineError::MissingCommandId);
            }
            CommandLineAction::parse(id).map(Invocation::Run)
        }
        [first, ..] => Err(CommandLineError::UnexpectedArgument((*first).to_owned())),
    }
}

/// Usage text for `--help`.
pub fn usage() -> String {
    "Usage:\n  kestrel                  Open the Kestrel window\n  kestrel --command <id>   \
     Run one action in the running instance\n  kestrel --list-commands  List command IDs\n  \
     kestrel --version        Print the version\n"
        .to_owned()
}

/// The `--list-commands` table: one `id<TAB>description` line per action.
pub fn command_list() -> String {
    CommandLineAction::all()
        .into_iter()
        .map(|action| format!("{}\t{}\n", action.id(), action.description()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, ffi::OsString};

    use kestrel_platform::quick_toggles::{ALL_QUICK_TOGGLES, QuickToggleId};

    use super::{CommandGate, CommandLineAction, CommandLineError, Invocation, parse_arguments};
    use crate::FocusTarget;
    use crate::{ApplicationCommand, FeaturePreset};

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn every_listed_id_parses_back_to_its_action() {
        let actions = CommandLineAction::all();
        let ids = actions.iter().map(|action| action.id()).collect::<Vec<_>>();

        assert_eq!(
            ids.iter().collect::<HashSet<_>>().len(),
            ids.len(),
            "IDs are unique"
        );
        assert_eq!(
            actions.len(),
            18 + ALL_QUICK_TOGGLES.len(),
            "every quick toggle has exactly one ID"
        );
        for action in actions {
            assert_eq!(CommandLineAction::parse(&action.id()), Ok(action));
        }
    }

    #[test]
    fn stable_ids_map_to_their_commands() {
        assert_eq!(
            parse_arguments(&arguments(&["--command", "microphone.toggle-mute"])),
            Ok(Invocation::Run(CommandLineAction::ToggleMicrophoneMute))
        );
        assert_eq!(
            CommandLineAction::parse("preset.balanced").map(CommandLineAction::to_command),
            Ok(ApplicationCommand::ApplyPreset(FeaturePreset::Balanced))
        );
        assert_eq!(
            parse_arguments(&arguments(&["--command=toggle.power.keep-awake"])).map(|invocation| {
                match invocation {
                    Invocation::Run(action) => action.to_command(),
                    other => panic!("unexpected {other:?}"),
                }
            }),
            Ok(ApplicationCommand::FlipQuickToggle(
                QuickToggleId::KeepAwake
            ))
        );
    }

    #[test]
    fn invalid_invocations_are_rejected_with_a_reason() {
        assert_eq!(
            parse_arguments(&arguments(&["--command", "microphone.explode"])),
            Err(CommandLineError::UnknownCommand(
                "microphone.explode".to_owned()
            ))
        );
        assert_eq!(
            parse_arguments(&arguments(&["--command", "toggle.radio.unknown"])),
            Err(CommandLineError::UnknownCommand(
                "toggle.radio.unknown".to_owned()
            ))
        );
        assert_eq!(
            parse_arguments(&arguments(&["--command"])),
            Err(CommandLineError::MissingCommandId)
        );
        assert_eq!(
            parse_arguments(&arguments(&["--command="])),
            Err(CommandLineError::MissingCommandId)
        );
        assert_eq!(
            parse_arguments(&arguments(&["--frobnicate"])),
            Err(CommandLineError::UnexpectedArgument(
                "--frobnicate".to_owned()
            ))
        );
        assert_eq!(
            parse_arguments(&arguments(&["--command", "window.show", "extra"])),
            Err(CommandLineError::UnexpectedArgument("--command".to_owned()))
        );
    }

    #[test]
    fn no_arguments_open_the_window_and_flags_select_local_output() {
        assert_eq!(parse_arguments(&[]), Ok(Invocation::Present));
        assert_eq!(
            parse_arguments(&arguments(&["--list-commands"])),
            Ok(Invocation::ListCommands)
        );
        assert_eq!(parse_arguments(&arguments(&["-h"])), Ok(Invocation::Help));
        assert_eq!(
            parse_arguments(&arguments(&["--version"])),
            Ok(Invocation::Version)
        );
    }

    #[test]
    fn entry_point_ids_focus_their_controls() {
        assert_eq!(
            CommandLineAction::parse("command-bar.open").map(CommandLineAction::to_command),
            Ok(ApplicationCommand::Focus(FocusTarget::CommandBar))
        );
        assert_eq!(
            CommandLineAction::parse("clipboard.quick-paste").map(CommandLineAction::to_command),
            Ok(ApplicationCommand::Focus(FocusTarget::Clipboard))
        );
    }

    #[test]
    fn malformed_ids_are_distinct_from_unknown_ones_and_echo_bounded() {
        let long = "a".repeat(200);
        let Err(CommandLineError::MalformedCommandId(echoed)) = CommandLineAction::parse(&long)
        else {
            panic!("an overlong ID is malformed");
        };
        assert_eq!(echoed.len(), kestrel_core::MAX_SHORTCUT_COMMAND_CHARS);
        assert!(matches!(
            CommandLineAction::parse("Window.Show"),
            Err(CommandLineError::MalformedCommandId(_))
        ));
        assert_eq!(
            CommandLineAction::parse("window\x1b[2J"),
            Err(CommandLineError::MalformedCommandId(
                "window\\u{1b}[2J".to_owned()
            )),
            "control characters are escaped before they reach a terminal"
        );
        assert!(matches!(
            CommandLineAction::parse("window.hide"),
            Err(CommandLineError::UnknownCommand(_))
        ));
    }

    #[test]
    fn gates_have_distinct_exit_statuses_that_round_trip() {
        let gates = [
            CommandGate::Disabled {
                feature: "clipboard.history".to_owned(),
            },
            CommandGate::Unavailable {
                feature: "clipboard.history".to_owned(),
                summary: "no selection owner".to_owned(),
                remediation: None,
            },
            CommandGate::Stopped {
                feature: "clipboard.history".to_owned(),
            },
        ];
        let statuses = gates
            .iter()
            .map(CommandGate::exit_status)
            .collect::<HashSet<_>>();
        assert_eq!(statuses.len(), gates.len());
        assert!(!statuses.contains(&0) && !statuses.contains(&CommandLineError::EXIT_STATUS));
        for gate in &gates {
            let rebuilt =
                CommandGate::from_exit_status(i32::from(gate.exit_status()), "clipboard.history")
                    .expect("every gate status is recognized");
            assert_eq!(rebuilt.exit_status(), gate.exit_status());
        }
        assert_eq!(CommandGate::from_exit_status(0, "clipboard.history"), None);
        assert_eq!(CommandGate::from_exit_status(1, "clipboard.history"), None);
    }
}
