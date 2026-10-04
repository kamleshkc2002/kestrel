//! Runtime-agnostic microphone policy and state.

use std::fmt;

use kestrel_core::{CapabilityReport, CapabilityStatus};
use kestrel_platform::audio::{
    AudioError, InputDevice, InputDiscovery, MICROPHONE_FEATURE_ID, MicrophoneBackend,
    microphone_capability_for, microphone_capability_for_error,
};

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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneCommand {
    SetMuted(bool),
    ToggleMute,
    SetInputMute { input_id: u32, muted: bool },
    SetDefaultInput { input_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicrophoneCommandError {
    NoInputs,
    UnknownInput { input_id: u32 },
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
    discovered: Option<InputDiscovery>,
    latest: MicrophoneSnapshot,
    capability: CapabilityReport,
}

impl<B: MicrophoneBackend> MicrophoneService<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
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

        let discovery = match self.backend.discover_inputs() {
            Ok(discovery) => discovery,
            Err(error) => {
                self.discovered = None;
                self.latest = MicrophoneSnapshot::unavailable();
                self.capability = microphone_capability_for_error(&error);
                return Err(error);
            }
        };

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
        let inputs = self
            .discovered
            .as_ref()
            .map(|discovery| discovery.inputs.as_slice())
            .unwrap_or(&[]);
        match command {
            MicrophoneCommand::SetInputMute { input_id, .. }
            | MicrophoneCommand::SetDefaultInput { input_id }
                if !inputs.iter().any(|input| input.id == input_id) =>
            {
                Err(MicrophoneCommandError::UnknownInput { input_id })
            }
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
    }

    impl MicrophoneBackend for FakeBackend {
        fn discover_inputs(&mut self) -> Result<InputDiscovery, AudioError> {
            self.discoveries.pop_front().unwrap_or_else(|| {
                Ok(InputDiscovery {
                    default_input_name: None,
                    inputs: Vec::new(),
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
        fn set_default_input(&mut self, name: &str) -> Result<(), AudioError> {
            self.defaults.push(name.to_string());
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
        }
    }

    #[test]
    fn computes_mute_states() {
        let empty = snapshot_for_discovery(&discovery(vec![]), 0, false);
        assert_eq!(empty.mute, MicrophoneMuteState::NoInputs);
        let mixed = snapshot_for_discovery(
            &discovery(vec![input(1, true, true), input(2, false, false)]),
            0,
            false,
        );
        assert_eq!(
            mixed.mute,
            MicrophoneMuteState::Mixed { muted: 1, total: 2 }
        );
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
}
