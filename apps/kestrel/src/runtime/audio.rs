//! Audio mixer and microphone runtime features.

use super::*;

impl ApplicationRuntime {
    /// Returns audio state, including unavailable/empty states.
    pub fn audio_snapshot(&self) -> &AudioSnapshot {
        self.audio_mixer.latest()
    }

    /// Re-reads the mixer, applying the output ranking to arrivals and
    /// losses; true when anything changed.
    pub fn refresh_audio_mixer(&mut self) -> bool {
        if !self.audio_mixer_is_running() {
            return false;
        }
        let before = self.audio_mixer.latest().clone();
        let _ = self.audio_mixer.refresh();
        self.audio_mixer.latest() != &before
    }

    pub(super) fn configure_audio_preferences(&mut self, audio: &kestrel_core::AudioConfiguration) {
        self.audio_mixer.set_priority(DevicePriority {
            ranked: audio.output_priority.clone(),
            follow: audio.follow_output_priority,
        });
        self.audio_mixer
            .set_favorites(audio.mixer_favorites.clone());
        self.microphone.set_priority(DevicePriority {
            ranked: audio.input_priority.clone(),
            follow: audio.follow_input_priority,
        });
    }

    pub fn microphone_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == MICROPHONE_FEATURE_ID && registration.running
        })
    }

    /// Returns the latest reading; mute is `Unknown` before sampling.
    pub fn microphone_snapshot(&self) -> &MicrophoneSnapshot {
        self.microphone.latest()
    }

    pub fn execute_microphone_command(
        &mut self,
        command: MicrophoneCommand,
    ) -> Option<MicrophoneCommandResult> {
        self.microphone_is_running()
            .then(|| self.microphone.execute(command))
    }

    /// Refreshes external mute state and reports whether it changed.
    pub fn refresh_microphone(&mut self) -> bool {
        if !self.microphone_is_running() {
            return false;
        }
        let before = self.microphone.latest().clone();
        let _ = self.microphone.refresh();
        self.microphone.latest() != &before
    }

    pub fn microphone_view_model(
        &self,
        audio: &kestrel_core::AudioConfiguration,
    ) -> MicrophoneViewModel {
        MicrophoneViewModel::from_presentation(
            audio,
            MicrophonePresentation {
                snapshot: self.microphone.latest(),
                running: self.microphone_is_running(),
            },
        )
    }

    pub fn audio_view_model(&self, audio: &kestrel_core::AudioConfiguration) -> AudioViewModel {
        AudioViewModel::from_presentation(
            audio,
            AudioPresentation {
                snapshot: Some(self.audio_mixer.latest()),
                running: self.audio_mixer_is_running(),
                policy: self.audio_mixer.policy(),
            },
        )
    }

    /// A running registration only makes a test startable.
    pub fn execute_audio_command(&mut self, command: AudioCommand) -> Option<AudioCommandResult> {
        self.audio_mixer_is_running()
            .then(|| self.audio_mixer.execute(command))
    }

    pub fn audio_mixer_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == AUDIO_MIXER_ID && registration.running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_services::audio::{AudioAvailability, AudioCommand};
    use kestrel_services::microphone::MicrophoneCommand;
    #[test]
    fn disabled_audio_service_does_not_accept_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.audio_snapshot().availability,
            AudioAvailability::Unavailable
        );
        assert!(
            runtime
                .execute_audio_command(AudioCommand::SetStreamMute {
                    stream_id: 1,
                    muted: true,
                })
                .is_none()
        );
    }
    #[test]
    fn disabled_microphone_service_reports_unknown_and_rejects_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        assert_eq!(
            runtime.microphone_snapshot().mute,
            kestrel_services::microphone::MicrophoneMuteState::Unknown
        );
        assert!(
            runtime
                .execute_microphone_command(MicrophoneCommand::ToggleMute)
                .is_none()
        );
    }
}
