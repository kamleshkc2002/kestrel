//! Global shortcut presentation.

use super::*;

pub(crate) struct ShortcutsPresentation<'a> {
    pub status: ShortcutStatus,
    pub running: bool,
    pub configured: &'a [kestrel_core::ShortcutBinding],
    /// Configured commands that are unknown to this build.
    pub skipped: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRowViewModel {
    pub command: String,
    pub description: String,
    pub trigger: String,
    pub state: String,
    /// The shortcut is registered and will fire.
    pub active: bool,
}

/// Global shortcuts: one row per configured binding, in configuration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutsViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ShortcutRowViewModel>,
    pub notice: Option<String>,
}

impl ShortcutsViewModel {
    pub(crate) fn from_presentation(presentation: ShortcutsPresentation<'_>) -> Self {
        let status = presentation.status;
        let provider = status
            .provider
            .map_or("desktop shortcuts", |provider| provider.label());
        let summary = if !presentation.running {
            "Global shortcuts are off. Enable global.shortcuts in the Feature Hub, or bind \
             `kestrel --command <id>` in your desktop's shortcut settings."
                .to_owned()
        } else {
            match &status.phase {
                ShortcutPhase::Idle => "Waiting to register shortcuts.".to_owned(),
                ShortcutPhase::Starting => format!("Registering through {provider}…"),
                ShortcutPhase::Active => format!("Registered through {provider}."),
                ShortcutPhase::Failed(error) => {
                    let remedy = match error.kind {
                        ShortcutErrorKind::Denied | ShortcutErrorKind::Cancelled => {
                            "Turn global.shortcuts off and on to ask again."
                        }
                        ShortcutErrorKind::Unavailable => {
                            "Bind `kestrel --command <id>` in your desktop's shortcut settings."
                        }
                        ShortcutErrorKind::Protocol | ShortcutErrorKind::WorkerUnavailable => {
                            "Turn global.shortcuts off and on to retry."
                        }
                    };
                    format!("Registration failed: {error}. {remedy}")
                }
                ShortcutPhase::Stopped => "Shortcuts are released.".to_owned(),
            }
        };
        // A released session keeps its last binding list; report none of it.
        let live = presentation.running && status.phase != ShortcutPhase::Stopped;
        let rows = presentation
            .configured
            .iter()
            .map(|binding| {
                let reported = live
                    .then(|| {
                        status
                            .bindings
                            .iter()
                            .find(|item| item.id == binding.command)
                    })
                    .flatten();
                let (state, active, trigger) = match reported.map(|item| &item.state) {
                    Some(BindingState::Bound { trigger }) => (
                        "Active".to_owned(),
                        true,
                        trigger.clone().unwrap_or_else(|| binding.trigger.clone()),
                    ),
                    Some(BindingState::Pending) => {
                        ("Waiting".to_owned(), false, binding.trigger.clone())
                    }
                    Some(BindingState::Conflict { reason }) => (
                        format!(
                            "In use: {reason}. Choose another trigger for it in \
                             [[shortcuts.bindings]]."
                        ),
                        false,
                        binding.trigger.clone(),
                    ),
                    Some(BindingState::Rejected { reason }) => (
                        format!("Not registered: {reason}"),
                        false,
                        binding.trigger.clone(),
                    ),
                    None => ("Not registered".to_owned(), false, binding.trigger.clone()),
                };
                ShortcutRowViewModel {
                    command: binding.command.clone(),
                    description: CommandLineAction::parse(&binding.command).map_or_else(
                        |_| "Unknown command".to_owned(),
                        |action| action.description(),
                    ),
                    trigger,
                    state,
                    active,
                }
            })
            .collect();
        let notice = (!presentation.skipped.is_empty()).then(|| {
            format!(
                "Skipped unknown commands: {}. Run `kestrel --list-commands` for valid IDs.",
                presentation.skipped.join(", ")
            )
        });
        Self {
            running: presentation.running,
            status: summary,
            rows,
            notice,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut_binding(command: &str, trigger: &str) -> kestrel_core::ShortcutBinding {
        kestrel_core::ShortcutBinding {
            command: command.to_owned(),
            trigger: trigger.to_owned(),
        }
    }

    #[test]
    fn shortcut_rows_follow_backend_states_in_configuration_order() {
        use kestrel_platform::global_shortcuts::{
            BindingState, BindingStatus, ShortcutPhase, ShortcutProvider, ShortcutStatus,
        };
        let configured = [
            shortcut_binding("window.show", "LOGO+ALT+k"),
            shortcut_binding("microphone.toggle-mute", "LOGO+ALT+m"),
            shortcut_binding("audio.output-next", "LOGO+ALT+o"),
        ];
        let status = ShortcutStatus {
            provider: Some(ShortcutProvider::X11),
            phase: ShortcutPhase::Active,
            bindings: vec![
                BindingStatus {
                    id: "microphone.toggle-mute".to_owned(),
                    requested: "ALT+LOGO+m".to_owned(),
                    state: BindingState::Conflict {
                        reason: "ALT+LOGO+m is grabbed by another client".to_owned(),
                    },
                },
                BindingStatus {
                    id: "window.show".to_owned(),
                    requested: "ALT+LOGO+k".to_owned(),
                    state: BindingState::Bound {
                        trigger: Some("Super+Alt+K".to_owned()),
                    },
                },
            ],
            generation: 3,
        };

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status,
            running: true,
            configured: &configured,
            skipped: &["window.explode".to_owned()],
        });

        assert!(view.status.contains("X11 key grabs"));
        let rows = view
            .rows
            .iter()
            .map(|row| (row.command.as_str(), row.trigger.as_str(), row.active))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                ("window.show", "Super+Alt+K", true),
                ("microphone.toggle-mute", "LOGO+ALT+m", false),
                ("audio.output-next", "LOGO+ALT+o", false),
            ],
            "the backend's trigger description wins once bound"
        );
        assert!(view.rows[1].state.starts_with("In use:"));
        assert_eq!(view.rows[2].state, "Not registered");
        assert_eq!(view.rows[0].description, "Open the Kestrel window");
        assert!(
            view.notice
                .is_some_and(|notice| notice.contains("window.explode"))
        );
    }

    #[test]
    fn stopped_shortcuts_claim_no_registration() {
        use kestrel_platform::global_shortcuts::{
            BindingState, BindingStatus, ShortcutPhase, ShortcutStatus,
        };
        let configured = [shortcut_binding("window.show", "LOGO+ALT+k")];
        let stale = ShortcutStatus {
            provider: None,
            phase: ShortcutPhase::Active,
            bindings: vec![BindingStatus {
                id: "window.show".to_owned(),
                requested: "ALT+LOGO+k".to_owned(),
                state: BindingState::Bound { trigger: None },
            }],
            generation: 1,
        };

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: stale,
            running: false,
            configured: &configured,
            skipped: &[],
        });

        assert!(view.status.contains("kestrel --command"));
        assert!(!view.rows[0].active);
        assert_eq!(view.rows[0].state, "Not registered");
    }
}
