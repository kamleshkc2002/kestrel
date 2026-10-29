//! Global shortcut runtime features.

use super::*;

impl ApplicationRuntime {
    /// Why `action` cannot run now, judged from its feature's registration.
    pub fn command_gate(&self, action: CommandLineAction) -> Option<CommandGate> {
        let feature_id = action.required_feature()?;
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == feature_id)?;
        let feature = format!("{} ({feature_id})", registration.feature.label);
        if !registration.enabled {
            Some(CommandGate::Disabled { feature })
        } else if !registration.available {
            Some(CommandGate::Unavailable {
                feature,
                summary: registration.capability.summary.clone(),
                remediation: registration.capability.remediation.clone(),
            })
        } else if !registration.running {
            Some(CommandGate::Stopped { feature })
        } else {
            None
        }
    }

    /// Routes shortcut activations to the application command channel.
    pub fn set_command_sender(&mut self, sender: async_channel::Sender<ApplicationCommand>) {
        self.command_sender = Some(sender);
    }

    pub fn global_shortcuts_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == GLOBAL_SHORTCUTS_ID && registration.running
        })
    }

    /// The capability allows registering keys, whether or not the feature runs.
    pub fn global_shortcuts_available(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == GLOBAL_SHORTCUTS_ID && registration.available
        })
    }

    pub fn shortcut_status(&self) -> ShortcutStatus {
        self.global_shortcuts.snapshot()
    }

    pub fn shortcuts_view_model(&self) -> ShortcutsViewModel {
        ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: self.global_shortcuts.snapshot(),
            available: self.global_shortcuts_available(),
            running: self.global_shortcuts_is_running(),
            configured: &self.shortcut_bindings,
            skipped: &self.shortcut_skipped,
        })
    }

    pub(super) fn shortcut_requests(
        configuration: &kestrel_core::ShortcutConfiguration,
    ) -> (Vec<ShortcutRequest>, Vec<String>) {
        let mut requests = Vec::new();
        let mut skipped = Vec::new();
        for binding in &configuration.bindings {
            match (
                CommandLineAction::parse(&binding.command),
                binding.validate(),
            ) {
                (Ok(action), Ok(trigger)) => requests.push(ShortcutRequest {
                    id: binding.command.clone(),
                    description: action.description(),
                    trigger,
                }),
                _ => skipped.push(binding.command.clone()),
            }
        }
        (requests, skipped)
    }

    pub(super) fn start_global_shortcuts(&mut self) {
        let Some(sender) = self.command_sender.clone() else {
            return;
        };
        self.shortcut_attempted = true;
        let Some(provider) = ShortcutDetection::detect().provider() else {
            return;
        };
        let sink: ActivationSink = Arc::new(move |id: &str| {
            if let Ok(action) = CommandLineAction::parse(id) {
                let _ = sender.try_send(action.to_command());
            }
        });
        let _ = self.global_shortcuts.start(
            shortcut_backend_for(provider),
            self.shortcut_requests.clone(),
            sink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_platform::clipboard::FEATURE_ID as CLIPBOARD_HISTORY_ID;
    use kestrel_platform::global_shortcuts::FEATURE_ID as GLOBAL_SHORTCUTS_ID;
    #[test]
    fn bindings_resolve_known_commands_and_skip_the_rest() {
        let configuration = kestrel_core::ShortcutConfiguration {
            bindings: vec![
                kestrel_core::ShortcutBinding {
                    command: "microphone.toggle-mute".to_owned(),
                    trigger: "LOGO+ALT+m".to_owned(),
                },
                kestrel_core::ShortcutBinding {
                    command: "window.explode".to_owned(),
                    trigger: "LOGO+ALT+x".to_owned(),
                },
            ],
        };

        let (requests, skipped) = ApplicationRuntime::shortcut_requests(&configuration);

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "microphone.toggle-mute");
        assert_eq!(
            requests[0].description,
            "Mute or unmute every microphone input"
        );
        assert_eq!(skipped, vec!["window.explode".to_owned()]);
    }
    #[test]
    fn disabled_global_shortcuts_never_register() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        let (sender, _receiver) = async_channel::unbounded();
        runtime.set_command_sender(sender);
        runtime.start();

        assert!(!runtime.global_shortcuts_is_running());
        assert!(!runtime.global_shortcuts.is_started());
        assert_eq!(
            runtime.shortcut_status().phase,
            kestrel_platform::global_shortcuts::ShortcutPhase::Idle
        );
        assert!(
            !FeaturePreset::Balanced.enables(GLOBAL_SHORTCUTS_ID),
            "registration stays an explicit opt-in"
        );
    }
    #[test]
    fn command_gates_follow_feature_state() {
        use crate::command_line::{CommandGate, CommandLineAction};
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("valid feature ID");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();

        assert_eq!(runtime.command_gate(CommandLineAction::ShowWindow), None);
        assert_eq!(
            runtime.command_gate(CommandLineAction::OpenCommandBar),
            None
        );
        let Some(CommandGate::Disabled { feature }) =
            runtime.command_gate(CommandLineAction::QuickPaste)
        else {
            panic!("disabled clipboard history gates quick paste");
        };
        assert!(feature.contains(CLIPBOARD_HISTORY_ID));
    }
}
