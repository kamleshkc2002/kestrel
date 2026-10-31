//! Global shortcut presentation.

use super::*;

pub(crate) struct ShortcutsPresentation<'a> {
    pub status: ShortcutStatus,
    /// The `global.shortcuts` capability allows registering keys.
    pub available: bool,
    pub running: bool,
    pub configured: &'a [kestrel_core::ShortcutBinding],
    /// Configured commands that are unknown to this build.
    pub skipped: &'a [String],
}

/// Where desktops keep custom shortcuts, for when Kestrel cannot register keys.
const DESKTOP_SHORTCUT_STEPS: &str = "In your desktop's keyboard settings, add a custom \
    shortcut, paste the command from Copy command, and give it the key shown. GNOME: Settings \
    → Keyboard → Custom Shortcuts. KDE Plasma: System Settings → Shortcuts → Add Command. \
    COSMIC: Settings → Input devices → Keyboard → Keyboard shortcuts → Custom shortcuts.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRowViewModel {
    pub command: String,
    pub description: String,
    /// The key in readable form, such as `Super+Alt+K`.
    pub trigger: String,
    pub state: String,
    /// The shortcut is registered and will fire.
    pub active: bool,
    /// The command line a desktop custom shortcut runs.
    pub copy_command: String,
}

/// An action that can still get a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutChoiceViewModel {
    pub command: String,
    pub description: String,
}

/// Keyboard shortcuts: one row per configured binding, in configuration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutsViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ShortcutRowViewModel>,
    pub notice: Option<String>,
    /// How to set the keys up in the desktop, while Kestrel is not registering them.
    pub desktop_steps: Option<String>,
    /// Actions without a shortcut, in command-list order; empty when the list is full.
    pub choices: Vec<ShortcutChoiceViewModel>,
}

/// A trigger in readable form: `LOGO+ALT+v` is `Super+Alt+V`.
pub fn shortcut_label(trigger: &kestrel_core::ShortcutTrigger) -> String {
    let mut label = String::new();
    for (held, name) in [
        (trigger.modifiers.logo, "Super"),
        (trigger.modifiers.ctrl, "Ctrl"),
        (trigger.modifiers.alt, "Alt"),
        (trigger.modifiers.shift, "Shift"),
    ] {
        if held {
            label.push_str(name);
            label.push('+');
        }
    }
    let mut key = trigger.key.chars();
    if let Some(first) = key.next() {
        label.extend(first.to_uppercase());
        label.push_str(key.as_str());
    }
    label
}

impl ShortcutsViewModel {
    pub(crate) fn from_presentation(presentation: ShortcutsPresentation<'_>) -> Self {
        let status = presentation.status;
        let provider = status
            .provider
            .map_or("desktop shortcuts", |provider| provider.label());
        let unregistrable = matches!(
            &status.phase,
            ShortcutPhase::Failed(error) if error.kind == ShortcutErrorKind::Unavailable
        );
        let summary = if !presentation.available {
            "This desktop does not let apps register global shortcuts, so Kestrel cannot \
             listen for these keys itself. Add each one as a custom shortcut in your desktop's \
             keyboard settings."
                .to_owned()
        } else if !presentation.running {
            "Keyboard shortcuts are off. Turn on Keyboard shortcuts in the Feature Hub so Kestrel \
             registers these keys, or add them as custom shortcuts in your desktop's keyboard \
             settings."
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
                            "Add them as custom shortcuts in your desktop's keyboard settings."
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
        let desktop_steps = (!presentation.available || !presentation.running || unregistrable)
            .then(|| DESKTOP_SHORTCUT_STEPS.to_owned());
        let readable = |trigger: &str| {
            kestrel_core::ShortcutTrigger::parse(trigger)
                .map_or_else(|_| trigger.to_owned(), |parsed| shortcut_label(&parsed))
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
                let configured = readable(&binding.trigger);
                let (state, active, trigger) = match reported.map(|item| &item.state) {
                    Some(BindingState::Bound { trigger }) => (
                        "Active".to_owned(),
                        true,
                        trigger.as_deref().map_or(configured, readable),
                    ),
                    Some(BindingState::Pending) => ("Waiting".to_owned(), false, configured),
                    Some(BindingState::Conflict { reason }) => (
                        format!("In use: {reason}. Choose another key with Change key."),
                        false,
                        configured,
                    ),
                    Some(BindingState::Rejected { reason }) => {
                        (format!("Not registered: {reason}"), false, configured)
                    }
                    None => ("Not registered".to_owned(), false, configured),
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
                    copy_command: format!("kestrel --command {}", binding.command),
                }
            })
            .collect();
        let choices = if presentation.configured.len() >= kestrel_core::MAX_SHORTCUT_BINDINGS {
            Vec::new()
        } else {
            CommandLineAction::all()
                .into_iter()
                .filter(|action| {
                    let id = action.id();
                    !presentation
                        .configured
                        .iter()
                        .any(|binding| binding.command == id)
                })
                .map(|action| ShortcutChoiceViewModel {
                    command: action.id(),
                    description: action.description(),
                })
                .collect()
        };
        let notice = (!presentation.skipped.is_empty()).then(|| {
            format!(
                "Skipped unknown commands: {}. Remove them here or run `kestrel --list-commands` \
                 for valid IDs.",
                presentation.skipped.join(", ")
            )
        });
        Self {
            running: presentation.running,
            status: summary,
            rows,
            notice,
            desktop_steps,
            choices,
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
            shortcut_binding("app.quit", "CTRL+ALT+q"),
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
                BindingStatus {
                    id: "app.quit".to_owned(),
                    requested: "CTRL+ALT+q".to_owned(),
                    state: BindingState::Bound {
                        trigger: Some("ALT+CTRL+q".to_owned()),
                    },
                },
            ],
            generation: 3,
        };

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status,
            available: true,
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
                ("microphone.toggle-mute", "Super+Alt+M", false),
                ("audio.output-next", "Super+Alt+O", false),
                ("app.quit", "Ctrl+Alt+Q", true),
            ],
            "the backend's trigger wins once bound, in readable form"
        );
        assert_eq!(
            view.desktop_steps, None,
            "registered keys need no desktop setup"
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
            available: true,
            running: false,
            configured: &configured,
            skipped: &[],
        });

        assert!(view.desktop_steps.is_some());
        assert!(!view.rows[0].active);
        assert_eq!(view.rows[0].state, "Not registered");
    }

    #[test]
    fn an_unregistrable_desktop_gets_setup_steps_copy_commands_and_free_choices() {
        use kestrel_platform::global_shortcuts::{ShortcutPhase, ShortcutStatus};
        let idle = || ShortcutStatus {
            provider: None,
            phase: ShortcutPhase::Idle,
            bindings: Vec::new(),
            generation: 0,
        };
        let configured = [
            shortcut_binding("clipboard.quick-paste", "ALT+LOGO+v"),
            shortcut_binding("window.show", "CTRL+SHIFT+F12"),
        ];

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: idle(),
            available: false,
            running: false,
            configured: &configured,
            skipped: &[],
        });

        assert!(
            view.desktop_steps
                .as_deref()
                .is_some_and(|steps| steps.contains("COSMIC"))
        );
        let rows = view
            .rows
            .iter()
            .map(|row| (row.trigger.as_str(), row.copy_command.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                ("Super+Alt+V", "kestrel --command clipboard.quick-paste"),
                ("Ctrl+Shift+F12", "kestrel --command window.show"),
            ]
        );
        let choices = view
            .choices
            .iter()
            .map(|choice| choice.command.as_str())
            .collect::<Vec<_>>();
        assert_eq!(choices.len(), CommandLineAction::all().len() - 2);
        assert!(!choices.contains(&"clipboard.quick-paste"));
        assert!(choices.contains(&"command-bar.open"));

        let full = (0..kestrel_core::MAX_SHORTCUT_BINDINGS)
            .map(|index| shortcut_binding(&format!("command.{index}"), "CTRL+x"))
            .collect::<Vec<_>>();
        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: idle(),
            available: true,
            running: true,
            configured: &full,
            skipped: &[],
        });
        assert!(view.choices.is_empty(), "a full list offers nothing to add");
    }
}
