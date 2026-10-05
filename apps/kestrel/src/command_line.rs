//! Command-line entry points: `kestrel --command <id>` forwards one stable,
//! typed action to the running instance, so any desktop's keybinding settings
//! can trigger Kestrel actions.

use std::{ffi::OsString, fmt};

use kestrel_platform::quick_toggles::{ALL_QUICK_TOGGLES, QuickToggleId};
use kestrel_services::{
    audio::{AudioCommand, AudioCycleDirection},
    clipboard::ClipboardCommand,
    microphone::MicrophoneCommand,
};

use crate::{ApplicationCommand, FeaturePreset};

const TOGGLE_PREFIX: &str = "toggle.";

/// One action a command line can trigger, identified by a stable ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLineAction {
    ShowWindow,
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

const FIXED_ACTIONS: [CommandLineAction; 16] = [
    CommandLineAction::ShowWindow,
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

    /// The application command the running instance executes.
    pub fn to_command(self) -> ApplicationCommand {
        match self {
            Self::ShowWindow => ApplicationCommand::PresentWindow,
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
    MissingCommandId,
    UnexpectedArgument(String),
}

impl fmt::Display for CommandLineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(
                formatter,
                "unknown command \"{id}\"; run `kestrel --list-commands` for the supported IDs"
            ),
            Self::MissingCommandId => formatter.write_str("--command needs a command ID"),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument \"{argument}\"")
            }
        }
    }
}

impl std::error::Error for CommandLineError {}

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

    use super::{CommandLineAction, CommandLineError, Invocation, parse_arguments};
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
            16 + ALL_QUICK_TOGGLES.len(),
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
}
