//! Command bar presentation.

use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct CommandBarQuery<'a> {
    pub query: &'a str,
    pub results: &'a [kestrel_services::command_bar::CommandResult],
}

/// Inputs for the command-bar view.
pub(crate) struct CommandBarPresentation<'a> {
    pub running: bool,
    pub max_results: u32,
    pub providers: kestrel_services::command_bar::EnabledProviders,
    pub launcher: Option<&'a str>,
    pub applications: usize,
    pub file_roots: usize,
    pub scripts: usize,
    pub windows_status: String,
    pub clipboard_status: String,
    pub query: &'a str,
    pub results: &'a [kestrel_services::command_bar::CommandResult],
    pub ranking: &'a kestrel_services::command_bar::CommandRanking,
    pub warnings: &'a [ConfigurationWarning],
}

impl Default for CommandBarPresentation<'_> {
    fn default() -> Self {
        Self {
            running: false,
            max_results: kestrel_core::DEFAULT_COMMAND_RESULTS,
            providers: kestrel_services::command_bar::EnabledProviders::default(),
            launcher: None,
            applications: 0,
            file_roots: 0,
            scripts: 0,
            windows_status: "Unavailable".to_owned(),
            clipboard_status: "Clipboard history stopped".to_owned(),
            query: "",
            results: &[],
            ranking: &EMPTY_COMMAND_RANKING,
            warnings: &[],
        }
    }
}

static EMPTY_COMMAND_RANKING: kestrel_services::command_bar::CommandRanking =
    kestrel_services::command_bar::CommandRanking::EMPTY;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBarViewModel {
    pub running: bool,
    pub status: String,
    pub query: String,
    pub max_results: u32,
    pub providers: Vec<CommandProviderViewModel>,
    pub results: Vec<CommandResultViewModel>,
    pub ranking_entries: Vec<CommandRankingViewModel>,
    pub pinned: Vec<String>,
    pub warnings: Vec<ConfigurationWarningViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProviderViewModel {
    pub provider: CommandProvider,
    pub label: &'static str,
    pub enabled: bool,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResultViewModel {
    pub id: String,
    pub source: &'static str,
    pub title: String,
    pub detail: String,
    pub matched_on: &'static str,
    pub pinned: bool,
    pub uses: u32,
    pub action_label: &'static str,
    pub swatch: Option<[u8; 3]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRankingViewModel {
    pub id: String,
    pub uses: u32,
    pub pinned: bool,
}

/// Providers available to the command bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandProvider {
    Applications,
    Files,
    Scripts,
    Emoji,
    Windows,
    Clipboard,
}

impl CommandProvider {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Applications => "Applications",
            Self::Files => "Files (configured roots)",
            Self::Scripts => "Scripts",
            Self::Emoji => "Emoji",
            Self::Windows => "Windows",
            Self::Clipboard => "Clipboard",
        }
    }

    pub const fn requirement(self) -> &'static str {
        match self {
            Self::Applications => "XDG application directories",
            Self::Files => "At least one configured root; Kestrel never indexes everything",
            Self::Scripts => "Script definitions in the configuration",
            Self::Emoji => "Built in",
            Self::Windows => "Compositor window-list protocol",
            Self::Clipboard => "clipboard.history feature",
        }
    }
}

impl CommandBarViewModel {
    pub(crate) fn from_presentation(presentation: CommandBarPresentation<'_>) -> Self {
        let providers = vec![
            CommandProviderViewModel {
                provider: CommandProvider::Applications,
                label: CommandProvider::Applications.label(),
                enabled: presentation.providers.applications,
                // Applications launch through desktop-entry handling.
                status: if !presentation.providers.applications {
                    "Disabled".to_string()
                } else {
                    format!("{} applications scanned", presentation.applications)
                },
            },
            CommandProviderViewModel {
                provider: CommandProvider::Files,
                label: CommandProvider::Files.label(),
                enabled: presentation.providers.files,
                status: if presentation.file_roots == 0 {
                    "No roots configured; nothing is indexed".to_string()
                } else {
                    format!("{} configured roots", presentation.file_roots)
                },
            },
            CommandProviderViewModel {
                provider: CommandProvider::Scripts,
                label: CommandProvider::Scripts.label(),
                enabled: presentation.providers.scripts,
                status: format!("{} script definitions", presentation.scripts),
            },
            CommandProviderViewModel {
                provider: CommandProvider::Emoji,
                label: CommandProvider::Emoji.label(),
                enabled: presentation.providers.emoji,
                status: "Built in".to_string(),
            },
            CommandProviderViewModel {
                provider: CommandProvider::Windows,
                label: CommandProvider::Windows.label(),
                enabled: presentation.providers.windows,
                status: if !presentation.providers.windows {
                    "Disabled".to_string()
                } else {
                    presentation.windows_status.clone()
                },
            },
            CommandProviderViewModel {
                provider: CommandProvider::Clipboard,
                label: CommandProvider::Clipboard.label(),
                enabled: presentation.providers.clipboard,
                status: if !presentation.providers.clipboard {
                    "Disabled".to_string()
                } else {
                    presentation.clipboard_status.clone()
                },
            },
        ];

        let results = presentation
            .results
            .iter()
            .map(|result| CommandResultViewModel {
                id: result.item.id.clone(),
                source: result.item.source.label(),
                title: result.item.title.clone(),
                detail: result.item.subtitle.clone(),
                matched_on: result.matched_on,
                pinned: result.pinned,
                uses: result.usage,
                action_label: command_action_label(&result.item.action),
                swatch: result.item.swatch,
            })
            .collect::<Vec<_>>();

        let ranking_entries = presentation
            .ranking
            .entries()
            .into_iter()
            .map(|(id, uses)| CommandRankingViewModel {
                pinned: presentation.ranking.is_pinned(&id),
                id,
                uses,
            })
            .collect::<Vec<_>>();

        Self {
            running: presentation.running,
            status: command_bar_status(&presentation),
            query: presentation.query.to_owned(),
            max_results: presentation.max_results,
            providers,
            results,
            ranking_entries,
            pinned: presentation.ranking.pinned_entries(),
            warnings: presentation
                .warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
        }
    }
}

fn command_action_label(action: &kestrel_services::command_bar::CommandAction) -> &'static str {
    use kestrel_services::command_bar::CommandAction;
    match action {
        CommandAction::Kestrel(_) => "Run",
        CommandAction::InsertText(_) => "Copy",
        CommandAction::RunScript { .. } => "Run script",
        CommandAction::OpenApplication { .. } => "Launch",
        CommandAction::OpenApplicationAction { .. } => "Run action",
        CommandAction::ActivateWindow(_) => "Activate",
        CommandAction::CopyClipboardEntry { .. } => "Copy",
        CommandAction::SetFeatureEnabled { .. } => "Toggle",
        CommandAction::OpenFile(_) => "Open file",
        CommandAction::OpenUrl(_) => "Open link",
    }
}

/// What the command bar rows don't already show: whether it runs, a missing
/// launcher, and the first ranking warning. Empty when there is nothing else.
fn command_bar_status(presentation: &CommandBarPresentation<'_>) -> String {
    if !presentation.running {
        return "The command bar is not running. Enable commands.bar in the Feature Hub."
            .to_string();
    }
    let mut notes = Vec::new();
    if presentation.launcher.is_none() {
        notes.push("Links and files cannot be opened (no xdg-open)".to_owned());
    }
    if let Some(warning) = presentation.warnings.first() {
        notes.push(warning.reason.clone());
    }
    notes.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_presentation<'a>(
        results: &'a [kestrel_services::command_bar::CommandResult],
        ranking: &'a kestrel_services::command_bar::CommandRanking,
    ) -> CommandBarPresentation<'a> {
        CommandBarPresentation {
            running: true,
            max_results: 12,
            providers: kestrel_services::command_bar::EnabledProviders {
                applications: true,
                files: false,
                scripts: true,
                emoji: true,
                windows: true,
                clipboard: true,
                snippets: true,
            },
            launcher: Some("xdg-open"),
            applications: 42,
            file_roots: 0,
            scripts: 2,
            windows_status: "X11 EWMH".to_owned(),
            clipboard_status: "clipboard history running".to_owned(),
            query: "2+2",
            results,
            ranking,
            warnings: &[],
        }
    }

    #[test]
    fn command_bar_reports_providers_and_results_without_hiding_portable_ones() {
        let index = kestrel_services::command_bar::CommandIndex::new(Vec::new(), &[]);
        let ranking = kestrel_services::command_bar::CommandRanking::default();
        let now = kestrel_platform::snippets::LocalTime {
            year: 2026,
            month: 10,
            day: 2,
            hour: 9,
            minute: 5,
            second: 7,
            utc_offset_seconds: 7200,
        };
        let results = index.search(kestrel_services::command_bar::SearchInput {
            query: "2+2",
            max_results: 5,
            providers: kestrel_services::command_bar::EnabledProviders::default(),
            applications: &[],
            files: &[],
            windows: &[],
            clipboard: &[],
            features: &[],
            now: Some(&now),
            ranking: &ranking,
            configured_scripts: &[],
        });
        let command_bar =
            CommandBarViewModel::from_presentation(command_presentation(&results, &ranking));

        assert!(command_bar.running);
        assert_eq!(command_bar.results.len(), 1);
        assert_eq!(command_bar.results[0].source, "Math");
        assert_eq!(command_bar.results[0].title, "2+2 = 4");
        assert_eq!(command_bar.results[0].action_label, "Copy");
        assert_eq!(command_bar.providers.len(), 6);
        assert!(
            command_bar
                .providers
                .iter()
                .any(|provider| provider.provider == CommandProvider::Files && !provider.enabled),
            "a disabled provider is reported, not hidden"
        );
        assert!(
            command_bar.status.is_empty(),
            "provider and result counts stay in their own rows: {}",
            command_bar.status
        );
    }

    #[test]
    fn command_bar_exposes_the_learned_ranking_and_handles_a_missing_launcher() {
        let mut ranking = kestrel_services::command_bar::CommandRanking::default();
        ranking.record_use("kestrel:refresh");
        ranking.record_use("kestrel:refresh");
        ranking.set_pinned("snippet:Address", true);
        let mut presentation = command_presentation(&[], &ranking);
        presentation.launcher = None;

        let command_bar = CommandBarViewModel::from_presentation(presentation);

        assert_eq!(command_bar.ranking_entries.len(), 1);
        assert_eq!(command_bar.ranking_entries[0].id, "kestrel:refresh");
        assert_eq!(command_bar.ranking_entries[0].uses, 2);
        assert_eq!(command_bar.pinned, vec!["snippet:Address".to_string()]);
        assert!(
            command_bar.status.contains("no xdg-open"),
            "a missing launcher is reported while portable commands keep working: {}",
            command_bar.status
        );
        assert!(
            command_bar.providers.iter().any(|provider| {
                provider.provider == CommandProvider::Applications
                    && provider.status == "42 applications scanned"
            }),
            "applications launch through GIO, so a missing xdg-open is not reported against them"
        );
    }
}
