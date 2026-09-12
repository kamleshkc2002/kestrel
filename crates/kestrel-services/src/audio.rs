//! Runtime-agnostic policy and state for the PulseAudio-compatible mixer.

use kestrel_core::{CapabilityReport, CapabilityStatus};
use kestrel_platform::audio::{
    AudioBackend, AudioDiscovery, AudioError, AudioServer, OutputDevice, PlaybackStream,
    capability_for_discovery,
};

pub const MIN_VOLUME_PERCENT: u8 = 0;
pub const MAX_VOLUME_PERCENT: u8 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioAvailability {
    Unavailable,
    NoOutputDevices,
    NoActiveStreams,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSnapshot {
    pub availability: AudioAvailability,
    pub server: Option<AudioServer>,
    pub outputs: Vec<OutputDevice>,
    pub streams: Vec<PlaybackStream>,
}

impl AudioSnapshot {
    fn unavailable() -> Self {
        Self {
            availability: AudioAvailability::Unavailable,
            server: None,
            outputs: Vec::new(),
            streams: Vec::new(),
        }
    }

    fn from_discovery(discovery: AudioDiscovery) -> Self {
        let availability = if discovery.outputs.is_empty() {
            AudioAvailability::NoOutputDevices
        } else if discovery.streams.is_empty() {
            AudioAvailability::NoActiveStreams
        } else {
            AudioAvailability::Ready
        };
        Self {
            availability,
            server: Some(discovery.server),
            outputs: discovery.outputs,
            streams: discovery.streams,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCommand {
    SetStreamVolume { stream_id: u32, volume_percent: u8 },
    SetStreamMute { stream_id: u32, muted: bool },
    MoveStream { stream_id: u32, output_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCommandError {
    VolumeOutOfRange { requested: u8 },
    UnknownStream { stream_id: u32 },
    ReadOnlyStream { stream_id: u32 },
    UnknownOutput { output_id: u32 },
    Backend(AudioError),
    Refresh(AudioError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCommandFailure {
    pub error: AudioCommandError,
    pub capability: CapabilityReport,
    pub snapshot: AudioSnapshot,
}

pub type AudioCommandResult = Result<AudioSnapshot, Box<AudioCommandFailure>>;

pub struct AudioMixerService<B> {
    backend: B,
    latest: AudioSnapshot,
    capability: CapabilityReport,
}

impl<B: AudioBackend> AudioMixerService<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            latest: AudioSnapshot::unavailable(),
            capability: CapabilityReport::new(
                kestrel_platform::audio::FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "Audio discovery has not started.".to_string(),
                },
                "Audio mixer has not been started.",
            ),
        }
    }

    pub fn latest(&self) -> &AudioSnapshot {
        &self.latest
    }

    pub fn capability(&self) -> &CapabilityReport {
        &self.capability
    }

    pub fn refresh(&mut self) -> Result<&AudioSnapshot, AudioError> {
        let discovery = self.backend.discover();
        self.capability = capability_for_discovery(&discovery);
        match discovery {
            Ok(discovery) => {
                self.latest = AudioSnapshot::from_discovery(discovery);
                Ok(&self.latest)
            }
            Err(error) => {
                self.latest = AudioSnapshot::unavailable();
                Err(error)
            }
        }
    }

    pub fn execute(&mut self, command: AudioCommand) -> AudioCommandResult {
        let validation = self.validate(command);
        let mutation = validation.and_then(|()| match command {
            AudioCommand::SetStreamVolume {
                stream_id,
                volume_percent,
            } => self
                .backend
                .set_stream_volume(stream_id, volume_percent)
                .map_err(AudioCommandError::Backend),
            AudioCommand::SetStreamMute { stream_id, muted } => self
                .backend
                .set_stream_mute(stream_id, muted)
                .map_err(AudioCommandError::Backend),
            AudioCommand::MoveStream {
                stream_id,
                output_id,
            } => self
                .backend
                .move_stream(stream_id, output_id)
                .map_err(AudioCommandError::Backend),
        });

        let refresh = self.refresh().cloned();
        match (mutation, refresh) {
            (Ok(()), Ok(snapshot)) => Ok(snapshot),
            (Err(error), _) => Err(self.failure(error)),
            (Ok(()), Err(error)) => Err(self.failure(AudioCommandError::Refresh(error))),
        }
    }

    fn validate(&self, command: AudioCommand) -> Result<(), AudioCommandError> {
        match command {
            AudioCommand::SetStreamVolume {
                stream_id,
                volume_percent,
            } => {
                if !(MIN_VOLUME_PERCENT..=MAX_VOLUME_PERCENT).contains(&volume_percent) {
                    return Err(AudioCommandError::VolumeOutOfRange {
                        requested: volume_percent,
                    });
                }
                let stream = self.stream(stream_id)?;
                if !stream.volume_writable {
                    return Err(AudioCommandError::ReadOnlyStream { stream_id });
                }
            }
            AudioCommand::SetStreamMute { stream_id, .. } => {
                self.stream(stream_id)?;
            }
            AudioCommand::MoveStream {
                stream_id,
                output_id,
            } => {
                self.stream(stream_id)?;
                if !self
                    .latest
                    .outputs
                    .iter()
                    .any(|output| output.id == output_id)
                {
                    return Err(AudioCommandError::UnknownOutput { output_id });
                }
            }
        }
        Ok(())
    }

    fn stream(&self, stream_id: u32) -> Result<&PlaybackStream, AudioCommandError> {
        self.latest
            .streams
            .iter()
            .find(|stream| stream.id == stream_id)
            .ok_or(AudioCommandError::UnknownStream { stream_id })
    }

    fn failure(&self, error: AudioCommandError) -> Box<AudioCommandFailure> {
        Box::new(AudioCommandFailure {
            error,
            capability: self.capability.clone(),
            snapshot: self.latest.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use kestrel_platform::audio::{
        AudioBackend, AudioDiscovery, AudioError, AudioErrorKind, AudioServer, FEATURE_ID,
        OutputDevice, PlaybackStream,
    };

    use super::{
        AudioAvailability, AudioCommand, AudioCommandError, AudioMixerService, MAX_VOLUME_PERCENT,
    };

    struct FakeBackend {
        discoveries: VecDeque<Result<AudioDiscovery, AudioError>>,
        mutation_error: Option<AudioError>,
        volume_calls: Vec<(u32, u8)>,
    }

    impl FakeBackend {
        fn new(discoveries: impl IntoIterator<Item = Result<AudioDiscovery, AudioError>>) -> Self {
            Self {
                discoveries: discoveries.into_iter().collect(),
                mutation_error: None,
                volume_calls: Vec::new(),
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn discover(&mut self) -> Result<AudioDiscovery, AudioError> {
            self.discoveries
                .pop_front()
                .expect("fake discovery must be available")
        }

        fn set_stream_volume(
            &mut self,
            stream_id: u32,
            volume_percent: u8,
        ) -> Result<(), AudioError> {
            self.volume_calls.push((stream_id, volume_percent));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn set_stream_mute(&mut self, _stream_id: u32, _muted: bool) -> Result<(), AudioError> {
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn move_stream(&mut self, _stream_id: u32, _output_id: u32) -> Result<(), AudioError> {
            self.mutation_error.clone().map_or(Ok(()), Err)
        }
    }

    fn discovery(volume_percent: u8) -> AudioDiscovery {
        AudioDiscovery {
            server: AudioServer {
                name: Some("PulseAudio (on PipeWire)".to_string()),
                version: Some("1".to_string()),
                protocol_version: Some(35),
                default_output_name: Some("speaker".to_string()),
            },
            outputs: vec![OutputDevice {
                id: 3,
                name: "speaker".to_string(),
                description: "Speakers".to_string(),
                volume_percent: 50,
                muted: false,
            }],
            streams: vec![PlaybackStream {
                id: 7,
                name: "Playback".to_string(),
                application_name: Some("Player".to_string()),
                media_name: None,
                media_role: Some("music".to_string()),
                output_id: 3,
                volume_percent: Some(volume_percent),
                volume_writable: true,
                muted: false,
            }],
        }
    }

    #[test]
    fn refresh_represents_no_active_streams_explicitly() {
        let mut empty = discovery(50);
        empty.streams.clear();
        let mut service = AudioMixerService::new(FakeBackend::new([Ok(empty)]));

        let snapshot = service.refresh().expect("discovery succeeds");

        assert_eq!(snapshot.availability, AudioAvailability::NoActiveStreams);
        assert!(snapshot.streams.is_empty());
    }

    #[test]
    fn successful_mutation_returns_refreshed_state() {
        let backend = FakeBackend::new([Ok(discovery(20)), Ok(discovery(70))]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service
            .execute(AudioCommand::SetStreamVolume {
                stream_id: 7,
                volume_percent: 70,
            })
            .expect("mutation succeeds");

        assert_eq!(snapshot.streams[0].volume_percent, Some(70));
    }

    #[test]
    fn rejected_mutation_still_returns_refreshed_state_and_capability() {
        let error = AudioError {
            kind: AudioErrorKind::Rejected,
            message: "rejected".to_string(),
        };
        let mut backend = FakeBackend::new([Ok(discovery(20)), Ok(discovery(20))]);
        backend.mutation_error = Some(error.clone());
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::SetStreamMute {
                stream_id: 7,
                muted: true,
            })
            .expect_err("mutation is rejected");

        assert_eq!(failure.error, AudioCommandError::Backend(error));
        assert_eq!(failure.snapshot.streams[0].volume_percent, Some(20));
        assert_eq!(failure.capability.feature_id, FEATURE_ID);
    }

    #[test]
    fn volume_above_one_hundred_is_rejected_without_mutation() {
        let backend = FakeBackend::new([Ok(discovery(20)), Ok(discovery(20))]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::SetStreamVolume {
                stream_id: 7,
                volume_percent: MAX_VOLUME_PERCENT + 1,
            })
            .expect_err("out of range volume is rejected");

        assert_eq!(
            failure.error,
            AudioCommandError::VolumeOutOfRange { requested: 101 }
        );
    }

    #[test]
    fn routing_requires_a_discovered_output() {
        let backend = FakeBackend::new([Ok(discovery(20)), Ok(discovery(20))]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::MoveStream {
                stream_id: 7,
                output_id: 99,
            })
            .expect_err("unknown output is rejected");

        assert_eq!(
            failure.error,
            AudioCommandError::UnknownOutput { output_id: 99 }
        );
    }
}
