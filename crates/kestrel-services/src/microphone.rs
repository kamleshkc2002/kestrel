//! Runtime-agnostic microphone policy and state.

use std::fmt;

use kestrel_core::{CapabilityReport, CapabilityStatus, MAX_AUDIO_BOOST_PERCENT};
use kestrel_platform::audio::{
    AudioError, CaptureStream, InputDevice, InputDiscovery, MICROPHONE_FEATURE_ID,
    MicrophoneBackend, microphone_capability_for, microphone_capability_for_error,
};

use crate::audio::{DevicePriority, device_set_changed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneMuteState {
    Unknown,
    NoInputs,
    Live,
    Muted,
    Mixed { muted: usize, total: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneAvailability {
    Unavailable,
    NoInputs,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneSnapshot {
    pub availability: MicrophoneAvailability,
    pub inputs: Vec<InputDevice>,
    pub default_input_id: Option<u32>,
    pub mute: MicrophoneMuteState,
    pub lost_inputs: usize,
    pub default_input_lost: bool,
    /// Applications recording from an input, including system-audio capture.
    pub capture_streams: Vec<CaptureStream>,
    /// The input the ranking last selected, by description.
    pub priority_switch: Option<String>,
}

impl MicrophoneSnapshot {
    pub fn unavailable() -> Self {
        Self {
            availability: MicrophoneAvailability::Unavailable,
            inputs: Vec::new(),
            default_input_id: None,
            mute: MicrophoneMuteState::Unknown,
            lost_inputs: 0,
            default_input_lost: false,
            capture_streams: Vec::new(),
            priority_switch: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneCommand {
    SetMuted(bool),
    ToggleMute,
    SetInputMute { input_id: u32, muted: bool },
    SetDefaultInput { input_id: u32 },
    SetInputVolume { input_id: u32, volume_percent: u8 },
    SetCaptureVolume { stream_id: u32, volume_percent: u8 },
    SetCaptureMute { stream_id: u32, muted: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicrophoneCommandError {
    NoInputs,
    UnknownInput { input_id: u32 },
    UnknownStream { stream_id: u32 },
    ReadOnlyStream { stream_id: u32 },
    VolumeOutOfRange { requested: u8 },
    Partial { failed: usize, total: usize },
    Backend(AudioError),
    Refresh(AudioError),
}

impl fmt::Display for MicrophoneCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoInputs => {
                formatter.write_str("no microphone or other input device is available")
            }
            Self::UnknownInput { input_id } => {
                write!(formatter, "input {input_id} is no longer available")
            }
            Self::UnknownStream { stream_id } => {
                write!(formatter, "capture stream {stream_id} has ended")
            }
            Self::ReadOnlyStream { stream_id } => {
                write!(
                    formatter,
                    "capture stream {stream_id} has no adjustable level"
                )
            }
            Self::VolumeOutOfRange { requested } => write!(
                formatter,
                "level {requested}% is above the {MAX_AUDIO_BOOST_PERCENT}% amplification cap"
            ),
            Self::Partial { failed, total } => {
                write!(
                    formatter,
                    "mute change failed for {failed} of {total} inputs"
                )
            }
            Self::Backend(error) | Self::Refresh(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MicrophoneCommandError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneCommandFailure {
    pub error: MicrophoneCommandError,
    pub capability: CapabilityReport,
    pub snapshot: MicrophoneSnapshot,
}

pub type MicrophoneCommandResult = Result<MicrophoneSnapshot, Box<MicrophoneCommandFailure>>;

pub struct MicrophoneService<B> {
    backend: B,
    priority: DevicePriority,
    /// The ranking changed; apply it on the next refresh.
    priority_pending: bool,
    priority_switch: Option<String>,
    discovered: Option<InputDiscovery>,
    latest: MicrophoneSnapshot,
    capability: CapabilityReport,
}

impl<B: MicrophoneBackend> MicrophoneService<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            priority: DevicePriority::default(),
            priority_pending: false,
            priority_switch: None,
            discovered: None,
            latest: MicrophoneSnapshot::unavailable(),
            capability: CapabilityReport::new(
                MICROPHONE_FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "Microphone discovery has not started.".to_string(),
                },
                "Microphone discovery has not been started.",
            ),
        }
    }

    pub fn latest(&self) -> &MicrophoneSnapshot {
        &self.latest
    }

    pub fn capability(&self) -> &CapabilityReport {
        &self.capability
    }

    /// Replaces the input ranking; a changed, followed ranking applies on the
    /// next refresh.
    pub fn set_priority(&mut self, priority: DevicePriority) {
        if priority != self.priority {
            self.priority_pending = priority.follow;
            self.priority = priority;
        }
    }

    pub fn refresh(&mut self) -> Result<&MicrophoneSnapshot, AudioError> {
        let previous = self.discovered.as_ref();
        let previous_ids = previous.map(|discovery| {
            discovery
                .inputs
                .iter()
                .map(|input| input.id)
                .collect::<Vec<_>>()
        });
        let previous_default_id = previous.and_then(|discovery| {
            discovery
                .default_input_name
                .as_deref()
                .and_then(|name| discovery.inputs.iter().find(|input| input.name == name))
                .or_else(|| discovery.inputs.iter().find(|input| input.is_default))
                .map(|input| input.id)
        });

        let mut discovery = match self.backend.discover_inputs() {
            Ok(discovery) => discovery,
            Err(error) => {
                self.discovered = None;
                self.latest = MicrophoneSnapshot::unavailable();
                self.capability = microphone_capability_for_error(&error);
                return Err(error);
            }
        };

        let devices_changed = device_set_changed(
            previous.map(|discovery| discovery.inputs.iter().map(|input| input.name.as_str())),
            discovery.inputs.iter().map(|input| input.name.as_str()),
        );
        if self.priority.follow && (devices_changed || self.priority_pending) {
            self.priority_pending = false;
            let target = self
                .priority
                .preferred(&discovery.inputs, |input| input.name.as_str())
                .filter(|input| !input.is_default)
                .map(|input| (input.name.clone(), input.description.clone()));
            let switched = target.filter(|(name, _)| self.backend.set_default_input(name).is_ok());
            if let Some((_, description)) = switched {
                self.priority_switch = Some(description);
                if let Ok(rediscovered) = self.backend.discover_inputs() {
                    discovery = rediscovered;
                }
            }
        }

        let current_ids = discovery
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>();
        let lost_inputs = previous_ids
            .as_ref()
            .map(|ids| ids.iter().filter(|id| !current_ids.contains(id)).count())
            .unwrap_or(0);
        let default_input_lost = previous_default_id
            .map(|id| !current_ids.contains(&id))
            .unwrap_or(false);
        self.latest = snapshot_for_discovery(&discovery, lost_inputs, default_input_lost);
        self.latest.priority_switch = self.priority_switch.clone();
        self.capability = microphone_capability_for(&discovery);
        self.discovered = Some(discovery);
        Ok(&self.latest)
    }

    pub fn execute(&mut self, command: MicrophoneCommand) -> MicrophoneCommandResult {
        if let Err(error) = self.refresh() {
            return Err(self.failure(MicrophoneCommandError::Refresh(error)));
        }
        if let Err(error) = self.validate(command) {
            return Err(self.failure(error));
        }

        let mutation = self.mutate(command);
        let refresh = self.refresh().cloned();
        match (mutation, refresh) {
            (Ok(()), Ok(snapshot)) => Ok(snapshot),
            (Err(error), _) => Err(self.failure(error)),
            (Ok(()), Err(error)) => Err(self.failure(MicrophoneCommandError::Refresh(error))),
        }
    }

    pub fn reset(&mut self) {
        self.discovered = None;
        self.latest = MicrophoneSnapshot::unavailable();
        self.capability = CapabilityReport::new(
            MICROPHONE_FEATURE_ID,
            CapabilityStatus::Unsupported {
                reason: "Microphone discovery has not started.".to_string(),
            },
            "Microphone discovery has not been started.",
        );
    }

    fn validate(&self, command: MicrophoneCommand) -> Result<(), MicrophoneCommandError> {
        let (inputs, streams) = self
            .discovered
            .as_ref()
            .map(|discovery| (discovery.inputs.as_slice(), discovery.streams.as_slice()))
            .unwrap_or((&[], &[]));
        let stream = |stream_id| {
            streams
                .iter()
                .find(|stream| stream.id == stream_id)
                .ok_or(MicrophoneCommandError::UnknownStream { stream_id })
        };
        let level = |volume_percent| {
            if volume_percent > MAX_AUDIO_BOOST_PERCENT {
                Err(MicrophoneCommandError::VolumeOutOfRange {
                    requested: volume_percent,
                })
            } else {
                Ok(())
            }
        };
        match command {
            MicrophoneCommand::SetInputMute { input_id, .. }
            | MicrophoneCommand::SetDefaultInput { input_id }
            | MicrophoneCommand::SetInputVolume { input_id, .. }
                if !inputs.iter().any(|input| input.id == input_id) =>
            {
                Err(MicrophoneCommandError::UnknownInput { input_id })
            }
            MicrophoneCommand::SetInputVolume { volume_percent, .. } => level(volume_percent),
            MicrophoneCommand::SetCaptureVolume {
                stream_id,
                volume_percent,
            } => {
                if !stream(stream_id)?.volume_writable {
                    return Err(MicrophoneCommandError::ReadOnlyStream { stream_id });
                }
                level(volume_percent)
            }
            MicrophoneCommand::SetCaptureMute { stream_id, .. } => stream(stream_id).map(|_| ()),
            MicrophoneCommand::SetMuted(_) | MicrophoneCommand::ToggleMute if inputs.is_empty() => {
                Err(MicrophoneCommandError::NoInputs)
            }
            _ => Ok(()),
        }
    }

    fn mutate(&mut self, command: MicrophoneCommand) -> Result<(), MicrophoneCommandError> {
        match command {
            MicrophoneCommand::SetMuted(muted) => self.set_all_muted(muted),
            MicrophoneCommand::ToggleMute => {
                let muted = !matches!(self.latest.mute, MicrophoneMuteState::Muted);
                self.set_all_muted(muted)
            }
            MicrophoneCommand::SetInputMute { input_id, muted } => self
                .backend
                .set_input_mute(input_id, muted)
                .map_err(MicrophoneCommandError::Backend),
            MicrophoneCommand::SetDefaultInput { input_id } => {
                let input_name = self
                    .discovered
                    .as_ref()
                    .and_then(|discovery| {
                        discovery.inputs.iter().find(|input| input.id == input_id)
                    })
                    .map(|input| input.name.clone())
                    .expect("validated input id must exist");
                self.backend
                    .set_default_input(&input_name)
                    .map_err(MicrophoneCommandError::Backend)
            }
            MicrophoneCommand::SetInputVolume {
                input_id,
                volume_percent,
            } => self
                .backend
                .set_input_volume(input_id, volume_percent)
                .map_err(MicrophoneCommandError::Backend),
            MicrophoneCommand::SetCaptureVolume {
                stream_id,
                volume_percent,
            } => self
                .backend
                .set_capture_volume(stream_id, volume_percent)
                .map_err(MicrophoneCommandError::Backend),
            MicrophoneCommand::SetCaptureMute { stream_id, muted } => self
                .backend
                .set_capture_mute(stream_id, muted)
                .map_err(MicrophoneCommandError::Backend),
        }
    }

    fn set_all_muted(&mut self, muted: bool) -> Result<(), MicrophoneCommandError> {
        let input_ids = self
            .discovered
            .as_ref()
            .map(|discovery| {
                discovery
                    .inputs
                    .iter()
                    .map(|input| input.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let total = input_ids.len();
        let mut failed = 0;
        for input_id in input_ids {
            if self.backend.set_input_mute(input_id, muted).is_err() {
                failed += 1;
            }
        }
        if failed == 0 {
            Ok(())
        } else {
            Err(MicrophoneCommandError::Partial { failed, total })
        }
    }

    fn failure(&self, error: MicrophoneCommandError) -> Box<MicrophoneCommandFailure> {
        Box::new(MicrophoneCommandFailure {
            error,
            capability: self.capability.clone(),
            snapshot: self.latest.clone(),
        })
    }
}

fn snapshot_for_discovery(
    discovery: &InputDiscovery,
    lost_inputs: usize,
    default_input_lost: bool,
) -> MicrophoneSnapshot {
    let total = discovery.inputs.len();
    let muted = discovery.inputs.iter().filter(|input| input.muted).count();
    let mute = match (total, muted) {
        (0, _) => MicrophoneMuteState::NoInputs,
        (_, 0) => MicrophoneMuteState::Live,
        (total, muted) if muted == total => MicrophoneMuteState::Muted,
        (total, muted) => MicrophoneMuteState::Mixed { muted, total },
    };
    MicrophoneSnapshot {
        availability: if total == 0 {
            MicrophoneAvailability::NoInputs
        } else {
            MicrophoneAvailability::Ready
        },
        inputs: discovery.inputs.clone(),
        default_input_id: discovery
            .inputs
            .iter()
            .find(|input| input.is_default)
            .map(|input| input.id),
        mute,
        lost_inputs,
        default_input_lost,
        capture_streams: discovery.streams.clone(),
        priority_switch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeBackend {
        discoveries: VecDeque<Result<InputDiscovery, AudioError>>,
        mute_failures: Vec<u32>,
        mutes: Vec<(u32, bool)>,
        defaults: Vec<String>,
        input_volumes: Vec<(u32, u8)>,
        capture_volumes: Vec<(u32, u8)>,
        capture_mutes: Vec<(u32, bool)>,
    }

    impl MicrophoneBackend for FakeBackend {
        fn discover_inputs(&mut self) -> Result<InputDiscovery, AudioError> {
            self.discoveries.pop_front().unwrap_or_else(|| {
                Ok(InputDiscovery {
                    default_input_name: None,
                    inputs: Vec::new(),
                    streams: Vec::new(),
                })
            })
        }
        fn set_input_mute(&mut self, id: u32, muted: bool) -> Result<(), AudioError> {
            self.mutes.push((id, muted));
            if self.mute_failures.contains(&id) {
                Err(AudioError {
                    kind: kestrel_platform::audio::AudioErrorKind::Rejected,
                    message: "rejected".into(),
                })
            } else {
                Ok(())
            }
        }
        fn set_input_volume(&mut self, id: u32, volume_percent: u8) -> Result<(), AudioError> {
            self.input_volumes.push((id, volume_percent));
            Ok(())
        }
        fn set_default_input(&mut self, name: &str) -> Result<(), AudioError> {
            self.defaults.push(name.to_string());
            Ok(())
        }
        fn set_capture_volume(&mut self, id: u32, volume_percent: u8) -> Result<(), AudioError> {
            self.capture_volumes.push((id, volume_percent));
            Ok(())
        }
        fn set_capture_mute(&mut self, id: u32, muted: bool) -> Result<(), AudioError> {
            self.capture_mutes.push((id, muted));
            Ok(())
        }
    }

    fn input(id: u32, muted: bool, default: bool) -> InputDevice {
        InputDevice {
            id,
            name: format!("source-{id}"),
            description: format!("Source {id}"),
            volume_percent: 100,
            muted,
            is_default: default,
        }
    }
    fn discovery(inputs: Vec<InputDevice>) -> InputDiscovery {
        InputDiscovery {
            default_input_name: inputs
                .iter()
                .find(|input| input.is_default)
                .map(|input| input.name.clone()),
            inputs,
            streams: Vec::new(),
        }
    }

    #[test]
    fn computes_mute_states() {
        for (label, inputs, expected) in [
            ("no inputs", vec![], MicrophoneMuteState::NoInputs),
            (
                "mixed inputs",
                vec![input(1, true, true), input(2, false, false)],
                MicrophoneMuteState::Mixed { muted: 1, total: 2 },
            ),
        ] {
            let snapshot = snapshot_for_discovery(&discovery(inputs), 0, false);
            assert_eq!(snapshot.mute, expected, "{label}");
        }
    }

    #[test]
    fn refresh_failure_clears_stale_state() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([
                Ok(discovery(vec![input(1, true, true)])),
                Err(AudioError {
                    kind: kestrel_platform::audio::AudioErrorKind::Unavailable,
                    message: "down".into(),
                }),
            ]),
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);
        service.refresh().unwrap();
        assert!(service.refresh().is_err());
        assert_eq!(service.latest().mute, MicrophoneMuteState::Unknown);
    }

    #[test]
    fn toggle_follows_the_backend_even_after_an_external_change() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([
                Ok(discovery(vec![input(1, false, true)])),
                Ok(discovery(vec![input(1, true, true)])),
                // Another application changed the mute state.
                Ok(discovery(vec![input(1, false, true)])),
                Ok(discovery(vec![input(1, true, true)])),
            ]),
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);

        service.execute(MicrophoneCommand::ToggleMute).unwrap();
        assert_eq!(service.latest().mute, MicrophoneMuteState::Muted);
        service.execute(MicrophoneCommand::ToggleMute).unwrap();

        assert_eq!(
            service.backend.mutes,
            vec![(1, true), (1, true)],
            "the second toggle mutes again because the backend reads live, whatever was cached"
        );
    }

    #[test]
    fn lost_default_is_reported_without_fallback_default() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([
                Ok(discovery(vec![
                    input(1, false, true),
                    input(2, false, false),
                ])),
                Ok(discovery(vec![input(2, false, false)])),
            ]),
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);
        service.refresh().unwrap();
        service.refresh().unwrap();
        assert_eq!(service.latest().default_input_id, None);
        assert_eq!(service.latest().lost_inputs, 1);
        assert!(service.latest().default_input_lost);
    }

    #[test]
    fn unknown_input_is_rejected_without_mutating_backend() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([Ok(discovery(vec![input(1, false, true)]))]),
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);
        let failure = service
            .execute(MicrophoneCommand::SetInputMute {
                input_id: 99,
                muted: true,
            })
            .expect_err("unknown input must be rejected");
        assert_eq!(
            failure.error,
            MicrophoneCommandError::UnknownInput { input_id: 99 }
        );
        assert!(service.backend.mutes.is_empty());
    }

    #[test]
    fn partial_global_mute_returns_post_refresh_snapshot() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([
                Ok(discovery(vec![
                    input(1, false, true),
                    input(2, false, false),
                ])),
                Ok(discovery(vec![
                    input(1, true, true),
                    input(2, false, false),
                ])),
            ]),
            mute_failures: vec![2],
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);
        let failure = service
            .execute(MicrophoneCommand::SetMuted(true))
            .expect_err("one rejected input must report partial failure");
        assert_eq!(
            failure.error,
            MicrophoneCommandError::Partial {
                failed: 1,
                total: 2
            }
        );
        assert_eq!(
            failure.snapshot.mute,
            MicrophoneMuteState::Mixed { muted: 1, total: 2 }
        );
    }

    #[test]
    fn reset_clears_discovery_and_claims() {
        let backend = FakeBackend {
            discoveries: VecDeque::from([Ok(discovery(vec![input(1, true, true)]))]),
            ..Default::default()
        };
        let mut service = MicrophoneService::new(backend);
        service.refresh().unwrap();
        service.reset();
        assert_eq!(service.latest(), &MicrophoneSnapshot::unavailable());
        assert!(matches!(
            service.capability().status,
            CapabilityStatus::Unsupported { .. }
        ));
    }

    fn capture(id: u32, input_id: u32, writable: bool) -> CaptureStream {
        CaptureStream {
            id,
            application_name: Some("Recorder".into()),
            application_id: Some("recorder".into()),
            input_id,
            from_monitor: false,
            volume_percent: writable.then_some(100),
            volume_writable: writable,
            muted: false,
            corked: false,
        }
    }

    fn queued<const N: usize>(discoveries: [InputDiscovery; N]) -> FakeBackend {
        FakeBackend {
            discoveries: discoveries.into_iter().map(Ok).collect(),
            ..FakeBackend::default()
        }
    }

    #[test]
    fn followed_input_priority_switches_on_arrival() {
        let backend = queued([
            discovery(vec![input(1, false, true)]),
            discovery(vec![input(1, false, true), input(2, false, false)]),
            discovery(vec![input(1, false, false), input(2, false, true)]),
        ]);
        let mut service = MicrophoneService::new(backend);
        service.set_priority(DevicePriority {
            ranked: vec!["source-2".into(), "source-1".into()],
            follow: true,
        });
        service.refresh().expect("start");
        let snapshot = service.refresh().expect("arrival").clone();
        assert_eq!(service.backend.defaults, ["source-2"]);
        assert_eq!(snapshot.default_input_id, Some(2));
        assert_eq!(snapshot.priority_switch.as_deref(), Some("Source 2"));
    }

    #[test]
    fn input_levels_are_bounded_written_and_read_back() {
        let quiet = InputDevice {
            volume_percent: 40,
            ..input(1, false, true)
        };
        let backend = queued([
            discovery(vec![input(1, false, true)]),
            discovery(vec![quiet]),
            discovery(vec![input(1, false, true)]),
            discovery(vec![input(1, false, true)]),
        ]);
        let mut service = MicrophoneService::new(backend);
        let snapshot = service
            .execute(MicrophoneCommand::SetInputVolume {
                input_id: 1,
                volume_percent: 40,
            })
            .expect("level set");
        assert_eq!(
            snapshot.inputs[0].volume_percent, 40,
            "the server's value is shown"
        );
        assert_eq!(service.backend.input_volumes, [(1, 40)]);
        let error = service
            .execute(MicrophoneCommand::SetInputVolume {
                input_id: 1,
                volume_percent: 200,
            })
            .expect_err("above the cap");
        assert_eq!(
            error.error,
            MicrophoneCommandError::VolumeOutOfRange { requested: 200 }
        );
        assert_eq!(service.backend.input_volumes.len(), 1, "nothing written");
    }

    #[test]
    fn capture_streams_are_listed_and_controlled() {
        let with_streams = |streams| InputDiscovery {
            streams,
            ..discovery(vec![input(1, false, true)])
        };
        let both = || with_streams(vec![capture(7, 1, true), capture(8, 1, false)]);
        let backend = queued([
            both(),
            both(),
            both(),
            // The read-only stream has ended by the last command.
            with_streams(vec![capture(7, 1, true)]),
        ]);
        let mut service = MicrophoneService::new(backend);
        let snapshot = service
            .execute(MicrophoneCommand::SetCaptureMute {
                stream_id: 7,
                muted: true,
            })
            .expect("mute");
        assert_eq!(snapshot.capture_streams.len(), 2);
        assert_eq!(service.backend.capture_mutes, [(7, true)]);
        let error = service
            .execute(MicrophoneCommand::SetCaptureVolume {
                stream_id: 8,
                volume_percent: 50,
            })
            .expect_err("read-only stream");
        assert_eq!(
            error.error,
            MicrophoneCommandError::ReadOnlyStream { stream_id: 8 }
        );
        let error = service
            .execute(MicrophoneCommand::SetCaptureMute {
                stream_id: 8,
                muted: true,
            })
            .expect_err("stream ended");
        assert_eq!(
            error.error,
            MicrophoneCommandError::UnknownStream { stream_id: 8 }
        );
    }
}
