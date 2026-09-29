//! Runtime-agnostic policy and state for the PulseAudio-compatible mixer.

use std::fmt;

use kestrel_core::{
    AudioConfiguration, AudioDisconnectPolicy, AudioOutputSwitch, CapabilityReport,
    CapabilityStatus, MAX_AUDIO_BOOST_PERCENT, UNAMPLIFIED_AUDIO_VOLUME_PERCENT,
};
use kestrel_platform::audio::{
    AudioBackend, AudioDiscovery, AudioError, AudioServer, OutputDevice, PlaybackStream,
    capability_for, capability_for_error,
};

pub const MIN_VOLUME_PERCENT: u8 = 0;
/// The hard amplification cap; a policy ceiling can only be lower.
pub const MAX_VOLUME_PERCENT: u8 = MAX_AUDIO_BOOST_PERCENT;

/// The direction of a master output cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCycleDirection {
    Next,
    Previous,
}

/// User-configured mixer behavior shared by validation and presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPolicy {
    /// The largest volume the UI and commands may request, above 100 for boost.
    pub boost_percent: u8,
    pub output_switch: AudioOutputSwitch,
    pub disconnect_policy: AudioDisconnectPolicy,
    /// The volume reapplied after output loss under `ResetVolume`.
    pub disconnect_volume_percent: u8,
    pub include_inactive_streams: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPolicyError {
    BoostBelowUnamplified { requested: u8 },
    BoostAboveCap { requested: u8 },
    DisconnectVolumeAboveUnamplified { requested: u8 },
}

impl fmt::Display for AudioPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BoostBelowUnamplified { requested } => write!(
                formatter,
                "boost ceiling {requested}% is below the {UNAMPLIFIED_AUDIO_VOLUME_PERCENT}% unamplified volume"
            ),
            Self::BoostAboveCap { requested } => write!(
                formatter,
                "boost ceiling {requested}% is above the {MAX_VOLUME_PERCENT}% hard cap"
            ),
            Self::DisconnectVolumeAboveUnamplified { requested } => write!(
                formatter,
                "disconnect volume {requested}% is above the {UNAMPLIFIED_AUDIO_VOLUME_PERCENT}% unamplified volume"
            ),
        }
    }
}

impl std::error::Error for AudioPolicyError {}

impl Default for AudioPolicy {
    fn default() -> Self {
        Self::from_configuration(&AudioConfiguration::default())
    }
}

impl AudioPolicy {
    /// Projects the portable configuration contract into mixer policy.
    pub fn from_configuration(configuration: &AudioConfiguration) -> Self {
        Self {
            boost_percent: configuration.boost_percent,
            output_switch: configuration.output_switch,
            disconnect_policy: configuration.disconnect_policy,
            disconnect_volume_percent: configuration.disconnect_volume_percent,
            include_inactive_streams: configuration.include_inactive_streams,
        }
    }

    pub fn validate(&self) -> Result<Self, AudioPolicyError> {
        if self.boost_percent > MAX_VOLUME_PERCENT {
            return Err(AudioPolicyError::BoostAboveCap {
                requested: self.boost_percent,
            });
        }
        if self.boost_percent < UNAMPLIFIED_AUDIO_VOLUME_PERCENT {
            return Err(AudioPolicyError::BoostBelowUnamplified {
                requested: self.boost_percent,
            });
        }
        if self.disconnect_volume_percent > UNAMPLIFIED_AUDIO_VOLUME_PERCENT {
            return Err(AudioPolicyError::DisconnectVolumeAboveUnamplified {
                requested: self.disconnect_volume_percent,
            });
        }
        Ok(*self)
    }

    /// The default policy with a different boost ceiling; used by configuration plumbing.
    pub fn with_boost_percent(mut self, boost_percent: u8) -> Self {
        self.boost_percent = boost_percent;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioAvailability {
    Unavailable,
    NoOutputDevices,
    NoActiveStreams,
    Ready,
}

/// What the last master switch did, so the UI can report stream movement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSwitchOutcome {
    pub output_id: u32,
    pub output_name: String,
    pub moved_streams: usize,
    pub failed_moves: usize,
}

/// What Kestrel did when one or more outputs disappeared between refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioReconcileOutcome {
    pub lost_output_ids: Vec<u32>,
    pub rehomed_streams: usize,
    pub volume_resets: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSnapshot {
    pub availability: AudioAvailability,
    pub server: Option<AudioServer>,
    pub outputs: Vec<OutputDevice>,
    /// Streams after the inactive filter; `inactive_streams` counts hidden ones.
    pub streams: Vec<PlaybackStream>,
    pub inactive_streams: usize,
    /// The amplification ceiling currently in effect.
    pub boost_ceiling_percent: u8,
    pub last_switch: Option<AudioSwitchOutcome>,
    pub last_reconcile: Option<AudioReconcileOutcome>,
}

impl AudioSnapshot {
    fn unavailable(policy: AudioPolicy) -> Self {
        Self {
            availability: AudioAvailability::Unavailable,
            server: None,
            outputs: Vec::new(),
            streams: Vec::new(),
            inactive_streams: 0,
            boost_ceiling_percent: policy.boost_percent,
            last_switch: None,
            last_reconcile: None,
        }
    }

    /// The output the server currently treats as default, falling back to the first.
    pub fn default_output(&self) -> Option<&OutputDevice> {
        self.outputs
            .iter()
            .find(|output| output.is_default)
            .or_else(|| self.outputs.first())
    }

    /// Outputs grouped by owning card, preserving discovery order.
    pub fn outputs_by_card(&self) -> Vec<(Option<String>, Vec<&OutputDevice>)> {
        let mut groups: Vec<(Option<String>, Vec<&OutputDevice>)> = Vec::new();
        for output in &self.outputs {
            let key = output.card_name.clone();
            match groups.iter_mut().find(|(label, _)| *label == key) {
                Some((_, devices)) => devices.push(output),
                None => groups.push((key, vec![output])),
            }
        }
        groups
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCommand {
    SetStreamVolume { stream_id: u32, volume_percent: u8 },
    SetStreamMute { stream_id: u32, muted: bool },
    MoveStream { stream_id: u32, output_id: u32 },
    SetOutputVolume { output_id: u32, volume_percent: u8 },
    SetOutputMute { output_id: u32, muted: bool },
    SetDefaultOutput { output_id: u32 },
    CycleOutput { direction: AudioCycleDirection },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCommandError {
    /// The request exceeded the configured ceiling; it is never silently clamped.
    VolumeOutOfRange {
        requested: u8,
        maximum: u8,
    },
    UnknownStream {
        stream_id: u32,
    },
    ReadOnlyStream {
        stream_id: u32,
    },
    UnknownOutput {
        output_id: u32,
    },
    NoAlternateOutput,
    Backend(AudioError),
    Refresh(AudioError),
}

impl fmt::Display for AudioCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VolumeOutOfRange { requested, maximum } => write!(
                formatter,
                "volume {requested}% is outside the configured 0..={maximum}% range"
            ),
            Self::UnknownStream { stream_id } => {
                write!(formatter, "stream {stream_id} is no longer available")
            }
            Self::ReadOnlyStream { stream_id } => {
                write!(
                    formatter,
                    "stream {stream_id} does not accept volume changes"
                )
            }
            Self::UnknownOutput { output_id } => {
                write!(formatter, "output {output_id} is no longer available")
            }
            Self::NoAlternateOutput => {
                formatter.write_str("no other output device is available to switch to")
            }
            Self::Backend(error) | Self::Refresh(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AudioCommandError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCommandFailure {
    pub error: AudioCommandError,
    pub capability: CapabilityReport,
    pub snapshot: AudioSnapshot,
}

pub type AudioCommandResult = Result<AudioSnapshot, Box<AudioCommandFailure>>;

pub struct AudioMixerService<B> {
    backend: B,
    policy: AudioPolicy,
    /// The unfiltered discovery; validation and reconciliation use it, presentation filters it.
    discovered: Option<AudioDiscovery>,
    last_switch: Option<AudioSwitchOutcome>,
    last_reconcile: Option<AudioReconcileOutcome>,
    latest: AudioSnapshot,
    capability: CapabilityReport,
}

impl<B: AudioBackend> AudioMixerService<B> {
    pub fn new(backend: B) -> Self {
        Self::with_policy(backend, AudioPolicy::default())
            .expect("the built-in audio policy is valid")
    }

    /// Builds a mixer with an explicit policy, rejecting an invalid ceiling.
    pub fn with_policy(backend: B, policy: AudioPolicy) -> Result<Self, AudioPolicyError> {
        let policy = policy.validate()?;
        Ok(Self {
            backend,
            policy,
            discovered: None,
            last_switch: None,
            last_reconcile: None,
            latest: AudioSnapshot::unavailable(policy),
            capability: CapabilityReport::new(
                kestrel_platform::audio::FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "Audio discovery has not started.".to_string(),
                },
                "Audio mixer has not been started.",
            ),
        })
    }

    pub fn latest(&self) -> &AudioSnapshot {
        &self.latest
    }

    pub fn capability(&self) -> &CapabilityReport {
        &self.capability
    }

    pub fn policy(&self) -> AudioPolicy {
        self.policy
    }

    /// Applies a validated policy and re-presents the retained discovery.
    pub fn set_policy(&mut self, policy: AudioPolicy) -> Result<(), AudioPolicyError> {
        self.policy = policy.validate()?;
        match self.discovered.as_ref() {
            Some(discovery) => {
                let latest = self.present(discovery);
                self.latest = latest;
            }
            None => self.latest.boost_ceiling_percent = self.policy.boost_percent,
        }
        Ok(())
    }

    /// Re-discovers the graph and repairs routing after output loss.
    pub fn refresh(&mut self) -> Result<&AudioSnapshot, AudioError> {
        let mut discovery = match self.backend.discover() {
            Ok(discovery) => discovery,
            Err(error) => {
                self.capability = capability_for_error(&error);
                self.discovered = None;
                self.last_reconcile = None;
                self.latest = AudioSnapshot::unavailable(self.policy);
                return Err(error);
            }
        };

        let lost = self.lost_output_ids(&discovery);
        if !lost.is_empty() {
            let outcome = self.reconcile_lost_outputs(&lost, &discovery);
            self.last_reconcile = Some(outcome);
            // Re-read the graph so the snapshot never reports stale routing; a
            // failed re-read is a diagnostic, not a feature failure.
            if let Ok(rediscovered) = self.backend.discover() {
                discovery = rediscovered;
            } else {
                if let Some(reconcile) = self.last_reconcile.as_mut() {
                    reconcile.failures += 1;
                }
            }
        }

        self.capability = capability_for(&discovery);
        self.latest = self.present(&discovery);
        self.discovered = Some(discovery);
        Ok(&self.latest)
    }

    pub fn execute(&mut self, command: AudioCommand) -> AudioCommandResult {
        let mutation = self.validate(command).and_then(|()| self.mutate(command));

        let refresh = self.refresh().cloned();
        match (mutation, refresh) {
            (Ok(()), Ok(snapshot)) => Ok(snapshot),
            (Err(error), _) => Err(self.failure(error)),
            (Ok(()), Err(error)) => Err(self.failure(AudioCommandError::Refresh(error))),
        }
    }

    fn mutate(&mut self, command: AudioCommand) -> Result<(), AudioCommandError> {
        match command {
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
            AudioCommand::SetOutputVolume {
                output_id,
                volume_percent,
            } => self
                .backend
                .set_output_volume(output_id, volume_percent)
                .map_err(AudioCommandError::Backend),
            AudioCommand::SetOutputMute { output_id, muted } => self
                .backend
                .set_output_mute(output_id, muted)
                .map_err(AudioCommandError::Backend),
            AudioCommand::SetDefaultOutput { output_id } => {
                self.switch_default_output(output_id);
                Ok(())
            }
            AudioCommand::CycleOutput { direction } => {
                let target = self.cycle_target(direction)?;
                self.switch_default_output(target);
                Ok(())
            }
        }
    }

    /// Switches the server default output and optionally re-homes playing streams.
    fn switch_default_output(&mut self, output_id: u32) {
        let Some(output_name) = self.discovered.as_ref().and_then(|discovery| {
            discovery
                .outputs
                .iter()
                .find(|output| output.id == output_id)
                .map(|output| output.name.clone())
        }) else {
            return;
        };

        if self.backend.set_default_output(&output_name).is_err() {
            return;
        }

        let mut outcome = AudioSwitchOutcome {
            output_id,
            output_name,
            moved_streams: 0,
            failed_moves: 0,
        };
        if self.policy.output_switch == AudioOutputSwitch::AllStreams {
            let stream_ids = self
                .discovered
                .as_ref()
                .map(|discovery| {
                    discovery
                        .streams
                        .iter()
                        .filter(|stream| !stream.corked && stream.output_id != output_id)
                        .map(|stream| stream.id)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for stream_id in stream_ids {
                match self.backend.move_stream(stream_id, output_id) {
                    Ok(()) => outcome.moved_streams += 1,
                    Err(_) => outcome.failed_moves += 1,
                }
            }
        }
        self.last_switch = Some(outcome);
    }

    fn cycle_target(&self, direction: AudioCycleDirection) -> Result<u32, AudioCommandError> {
        let Some(discovery) = self.discovered.as_ref() else {
            return Err(AudioCommandError::NoAlternateOutput);
        };
        if discovery.outputs.len() < 2 {
            return Err(AudioCommandError::NoAlternateOutput);
        }
        let current = discovery
            .outputs
            .iter()
            .position(|output| output.is_default)
            .unwrap_or(0);
        let next = match direction {
            AudioCycleDirection::Next => (current + 1) % discovery.outputs.len(),
            AudioCycleDirection::Previous => {
                (current + discovery.outputs.len() - 1) % discovery.outputs.len()
            }
        };
        Ok(discovery.outputs[next].id)
    }

    fn lost_output_ids(&self, discovery: &AudioDiscovery) -> Vec<u32> {
        let Some(previous) = self.discovered.as_ref() else {
            return Vec::new();
        };
        let mut lost = previous
            .outputs
            .iter()
            .filter(|output| {
                !discovery
                    .outputs
                    .iter()
                    .any(|remaining| remaining.id == output.id)
            })
            .map(|output| output.id)
            .collect::<Vec<_>>();
        lost.sort_unstable();
        lost
    }

    /// Re-homes streams whose output vanished, applying the disconnect policy.
    fn reconcile_lost_outputs(
        &mut self,
        lost: &[u32],
        discovery: &AudioDiscovery,
    ) -> AudioReconcileOutcome {
        let mut outcome = AudioReconcileOutcome {
            lost_output_ids: lost.to_vec(),
            rehomed_streams: 0,
            volume_resets: 0,
            failures: 0,
        };
        let target = discovery
            .outputs
            .iter()
            .find(|output| output.is_default)
            .or_else(|| discovery.outputs.first())
            .map(|output| output.id);
        let affected = discovery
            .streams
            .iter()
            .filter(|stream| lost.contains(&stream.output_id))
            .map(|stream| stream.id)
            .collect::<Vec<_>>();

        for stream_id in affected {
            if let Some(target) = target {
                match self.backend.move_stream(stream_id, target) {
                    Ok(()) => outcome.rehomed_streams += 1,
                    Err(_) => outcome.failures += 1,
                }
            }
            if self.policy.disconnect_policy == AudioDisconnectPolicy::ResetVolume {
                match self
                    .backend
                    .set_stream_volume(stream_id, self.policy.disconnect_volume_percent)
                {
                    Ok(()) => outcome.volume_resets += 1,
                    Err(_) => outcome.failures += 1,
                }
            }
        }
        outcome
    }

    fn validate(&self, command: AudioCommand) -> Result<(), AudioCommandError> {
        match command {
            AudioCommand::SetStreamVolume {
                stream_id,
                volume_percent,
            } => {
                self.check_volume(volume_percent)?;
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
                self.output(output_id)?;
            }
            AudioCommand::SetOutputVolume {
                output_id,
                volume_percent,
            } => {
                self.check_volume(volume_percent)?;
                self.output(output_id)?;
            }
            AudioCommand::SetOutputMute { output_id, .. } => {
                self.output(output_id)?;
            }
            AudioCommand::SetDefaultOutput { output_id } => {
                self.output(output_id)?;
            }
            AudioCommand::CycleOutput { direction } => {
                self.cycle_target(direction)?;
            }
        }
        Ok(())
    }

    /// Rejects values above the configured ceiling instead of clamping them.
    fn check_volume(&self, volume_percent: u8) -> Result<(), AudioCommandError> {
        if volume_percent > self.policy.boost_percent {
            return Err(AudioCommandError::VolumeOutOfRange {
                requested: volume_percent,
                maximum: self.policy.boost_percent,
            });
        }
        Ok(())
    }

    fn stream(&self, stream_id: u32) -> Result<&PlaybackStream, AudioCommandError> {
        self.discovered
            .as_ref()
            .and_then(|discovery| {
                discovery
                    .streams
                    .iter()
                    .find(|stream| stream.id == stream_id)
            })
            .ok_or(AudioCommandError::UnknownStream { stream_id })
    }

    fn output(&self, output_id: u32) -> Result<&OutputDevice, AudioCommandError> {
        self.discovered
            .as_ref()
            .and_then(|discovery| {
                discovery
                    .outputs
                    .iter()
                    .find(|output| output.id == output_id)
            })
            .ok_or(AudioCommandError::UnknownOutput { output_id })
    }

    /// Projects a discovery into owned presentation state under the current policy.
    fn present(&self, discovery: &AudioDiscovery) -> AudioSnapshot {
        let active_count = discovery
            .streams
            .iter()
            .filter(|stream| !stream.corked)
            .count();
        let streams = if self.policy.include_inactive_streams {
            discovery.streams.clone()
        } else {
            discovery
                .streams
                .iter()
                .filter(|stream| !stream.corked)
                .cloned()
                .collect()
        };
        let availability = if discovery.outputs.is_empty() {
            AudioAvailability::NoOutputDevices
        } else if active_count == 0 {
            AudioAvailability::NoActiveStreams
        } else {
            AudioAvailability::Ready
        };
        AudioSnapshot {
            availability,
            server: Some(discovery.server.clone()),
            outputs: discovery.outputs.clone(),
            streams,
            inactive_streams: discovery.streams.len() - active_count,
            boost_ceiling_percent: self.policy.boost_percent,
            last_switch: self.last_switch.clone(),
            last_reconcile: self.last_reconcile.clone(),
        }
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
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use kestrel_core::{AudioDisconnectPolicy, AudioOutputSwitch};
    use kestrel_platform::audio::{
        AudioBackend, AudioDiscovery, AudioError, AudioErrorKind, AudioServer, FEATURE_ID,
        OutputDevice, PlaybackStream,
    };

    use super::{
        AudioAvailability, AudioCommand, AudioCommandError, AudioCycleDirection, AudioMixerService,
        AudioPolicy, AudioPolicyError, MAX_VOLUME_PERCENT,
    };

    #[derive(Default)]
    struct FakeLog {
        volume_calls: Vec<(u32, u8)>,
        mute_calls: Vec<(u32, bool)>,
        move_calls: Vec<(u32, u32)>,
        output_volume_calls: Vec<(u32, u8)>,
        output_mute_calls: Vec<(u32, bool)>,
        default_calls: Vec<String>,
    }

    struct FakeBackend {
        discoveries: VecDeque<Result<AudioDiscovery, AudioError>>,
        mutation_error: Option<AudioError>,
        log: Rc<RefCell<FakeLog>>,
    }

    /// Builds a backend plus a log that stays observable after the service owns it.
    fn fake(
        discoveries: impl IntoIterator<Item = Result<AudioDiscovery, AudioError>>,
    ) -> (FakeBackend, Rc<RefCell<FakeLog>>) {
        let log = Rc::new(RefCell::new(FakeLog::default()));
        (
            FakeBackend {
                discoveries: discoveries.into_iter().collect(),
                mutation_error: None,
                log: Rc::clone(&log),
            },
            log,
        )
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
            self.log
                .borrow_mut()
                .volume_calls
                .push((stream_id, volume_percent));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn set_stream_mute(&mut self, stream_id: u32, muted: bool) -> Result<(), AudioError> {
            self.log.borrow_mut().mute_calls.push((stream_id, muted));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn move_stream(&mut self, stream_id: u32, output_id: u32) -> Result<(), AudioError> {
            self.log
                .borrow_mut()
                .move_calls
                .push((stream_id, output_id));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn set_output_volume(
            &mut self,
            output_id: u32,
            volume_percent: u8,
        ) -> Result<(), AudioError> {
            self.log
                .borrow_mut()
                .output_volume_calls
                .push((output_id, volume_percent));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn set_output_mute(&mut self, output_id: u32, muted: bool) -> Result<(), AudioError> {
            self.log
                .borrow_mut()
                .output_mute_calls
                .push((output_id, muted));
            self.mutation_error.clone().map_or(Ok(()), Err)
        }

        fn set_default_output(&mut self, output_name: &str) -> Result<(), AudioError> {
            self.log
                .borrow_mut()
                .default_calls
                .push(output_name.to_owned());
            self.mutation_error.clone().map_or(Ok(()), Err)
        }
    }

    fn output(id: u32, is_default: bool, volume_percent: u8) -> OutputDevice {
        OutputDevice {
            id,
            name: format!("output-{id}"),
            description: format!("Output {id}"),
            volume_percent,
            muted: false,
            is_default,
            card_id: Some(0),
            card_name: Some("Built-in Audio".to_string()),
            port_name: Some("analog-output".to_string()),
            port_description: Some("Headphones".to_string()),
        }
    }

    fn stream(id: u32, output_id: u32, volume_percent: u8) -> PlaybackStream {
        PlaybackStream {
            id,
            name: "Playback".to_string(),
            application_name: Some("Player".to_string()),
            media_name: None,
            media_role: Some("music".to_string()),
            output_id,
            volume_percent: Some(volume_percent),
            volume_writable: true,
            muted: false,
            corked: false,
        }
    }

    fn corked_stream(id: u32, output_id: u32) -> PlaybackStream {
        PlaybackStream {
            corked: true,
            ..stream(id, output_id, 50)
        }
    }

    fn discovery(
        outputs: Vec<OutputDevice>,
        streams: Vec<PlaybackStream>,
    ) -> Result<AudioDiscovery, AudioError> {
        Ok(AudioDiscovery {
            server: AudioServer {
                name: Some("PulseAudio (on PipeWire)".to_string()),
                version: Some("1".to_string()),
                protocol_version: Some(35),
                default_output_name: Some(
                    outputs
                        .iter()
                        .find(|output| output.is_default)
                        .map(|output| output.name.clone())
                        .unwrap_or_else(|| "output-1".to_string()),
                ),
            },
            outputs,
            streams,
        })
    }

    fn standard(volume_percent: u8) -> Result<AudioDiscovery, AudioError> {
        discovery(
            vec![output(3, true, 50)],
            vec![stream(7, 3, volume_percent)],
        )
    }

    #[test]
    fn refresh_represents_no_active_streams_explicitly() {
        let (backend, log) = fake([discovery(vec![output(3, true, 50)], Vec::new())]);
        let mut service = AudioMixerService::new(backend);

        let snapshot = service.refresh().expect("discovery succeeds");

        assert_eq!(snapshot.availability, AudioAvailability::NoActiveStreams);
        assert!(snapshot.streams.is_empty());
        assert_eq!(
            snapshot.boost_ceiling_percent,
            AudioPolicy::default().boost_percent
        );
        assert!(log.borrow().volume_calls.is_empty());
    }

    #[test]
    fn successful_mutation_returns_refreshed_state() {
        let (backend, log) = fake([standard(20), standard(70)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service
            .execute(AudioCommand::SetStreamVolume {
                stream_id: 7,
                volume_percent: 70,
            })
            .expect("mutation succeeds");

        assert_eq!(snapshot.streams[0].volume_percent, Some(70));
        assert_eq!(log.borrow().volume_calls, vec![(7, 70)]);
    }

    #[test]
    fn rejected_mutation_still_returns_refreshed_state_and_capability() {
        let error = AudioError {
            kind: AudioErrorKind::Rejected,
            message: "rejected".to_string(),
        };
        let (mut backend, _log) = fake([standard(20), standard(20)]);
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
    fn boost_within_the_policy_ceiling_reaches_the_backend() {
        let (backend, log) = fake([standard(100), standard(130)]);
        let mut service =
            AudioMixerService::with_policy(backend, AudioPolicy::default().with_boost_percent(130))
                .expect("policy is valid");
        service.refresh().expect("initial discovery");

        let snapshot = service
            .execute(AudioCommand::SetStreamVolume {
                stream_id: 7,
                volume_percent: 130,
            })
            .expect("boosted volume is accepted");

        assert_eq!(snapshot.streams[0].volume_percent, Some(130));
        assert_eq!(log.borrow().volume_calls, vec![(7, 130)]);
    }

    #[test]
    fn volume_above_the_policy_ceiling_is_rejected_without_mutation() {
        let (backend, log) = fake([standard(20), standard(20)]);
        let mut service =
            AudioMixerService::with_policy(backend, AudioPolicy::default().with_boost_percent(120))
                .expect("policy is valid");
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::SetStreamVolume {
                stream_id: 7,
                volume_percent: 121,
            })
            .expect_err("above the configured ceiling is rejected");

        assert_eq!(
            failure.error,
            AudioCommandError::VolumeOutOfRange {
                requested: 121,
                maximum: 120
            }
        );
        assert_eq!(failure.snapshot.boost_ceiling_percent, 120);
        assert_eq!(failure.snapshot.streams[0].volume_percent, Some(20));
        assert!(
            log.borrow().volume_calls.is_empty(),
            "a rejected request must not reach the backend"
        );
    }

    #[test]
    fn hard_cap_cannot_be_raised_by_policy() {
        let (backend, _log) = fake([standard(20), standard(20)]);
        let result = AudioMixerService::with_policy(
            backend,
            AudioPolicy::default().with_boost_percent(MAX_VOLUME_PERCENT + 1),
        );

        assert_eq!(
            result.err(),
            Some(AudioPolicyError::BoostAboveCap {
                requested: MAX_VOLUME_PERCENT + 1
            })
        );
    }

    #[test]
    fn policy_below_unamplified_volume_is_rejected() {
        let (backend, _log) = fake([standard(20)]);
        let result =
            AudioMixerService::with_policy(backend, AudioPolicy::default().with_boost_percent(99));

        assert_eq!(
            result.err(),
            Some(AudioPolicyError::BoostBelowUnamplified { requested: 99 })
        );
    }

    #[test]
    fn output_loss_rehomes_streams_without_failing_the_refresh() {
        let before = discovery(
            vec![output(3, true, 50), output(4, false, 50)],
            vec![stream(7, 4, 80)],
        );
        let stale = discovery(vec![output(3, true, 50)], vec![stream(7, 4, 80)]);
        let repaired = discovery(vec![output(3, true, 50)], vec![stream(7, 3, 80)]);
        let (backend, log) = fake([before, stale, repaired]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service.refresh().expect("device loss is not a failure");

        assert_eq!(snapshot.streams[0].output_id, 3, "no stale routing remains");
        assert_eq!(
            snapshot.last_reconcile,
            Some(super::AudioReconcileOutcome {
                lost_output_ids: vec![4],
                rehomed_streams: 1,
                volume_resets: 0,
                failures: 0,
            })
        );
        assert_eq!(snapshot.outputs.len(), 1);
        assert_eq!(log.borrow().move_calls, vec![(7, 3)]);
        assert!(log.borrow().volume_calls.is_empty());
    }

    #[test]
    fn reset_disconnect_policy_reapplies_the_configured_volume() {
        let before = discovery(
            vec![output(3, true, 50), output(4, false, 50)],
            vec![stream(7, 4, 80)],
        );
        let stale = discovery(vec![output(3, true, 50)], vec![stream(7, 4, 80)]);
        let repaired = discovery(vec![output(3, true, 50)], vec![stream(7, 3, 60)]);
        let (backend, log) = fake([before, stale, repaired]);
        let policy = AudioPolicy {
            disconnect_policy: AudioDisconnectPolicy::ResetVolume,
            disconnect_volume_percent: 60,
            ..AudioPolicy::default()
        };
        let mut service = AudioMixerService::with_policy(backend, policy).expect("policy is valid");
        service.refresh().expect("initial discovery");

        let snapshot = service.refresh().expect("device loss is reconciled");

        let reconcile = snapshot.last_reconcile.clone().expect("loss recorded");
        assert_eq!(reconcile.volume_resets, 1);
        assert_eq!(reconcile.rehomed_streams, 1);
        assert_eq!(log.borrow().volume_calls, vec![(7, 60)]);
        assert_eq!(log.borrow().move_calls, vec![(7, 3)]);
    }

    #[test]
    fn preserve_disconnect_policy_leaves_the_stream_volume_untouched() {
        let before = discovery(
            vec![output(3, true, 50), output(4, false, 50)],
            vec![stream(7, 4, 80)],
        );
        let stale = discovery(vec![output(3, true, 50)], vec![stream(7, 4, 80)]);
        let repaired = discovery(vec![output(3, true, 50)], vec![stream(7, 3, 80)]);
        let (backend, log) = fake([before, stale, repaired]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service.refresh().expect("device loss is reconciled");

        let reconcile = snapshot.last_reconcile.clone().expect("loss recorded");
        assert_eq!(reconcile.volume_resets, 0);
        assert_eq!(reconcile.rehomed_streams, 1);
        assert!(log.borrow().volume_calls.is_empty());
    }

    #[test]
    fn inactive_streams_are_hidden_from_the_snapshot_but_still_commandable() {
        let with_idle = discovery(
            vec![output(3, true, 50)],
            vec![stream(7, 3, 50), corked_stream(8, 3)],
        );
        let (backend, log) = fake([with_idle.clone(), with_idle.clone(), with_idle]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service.latest();
        assert_eq!(
            snapshot
                .streams
                .iter()
                .map(|stream| stream.id)
                .collect::<Vec<_>>(),
            vec![7]
        );
        assert_eq!(snapshot.inactive_streams, 1);
        assert_eq!(snapshot.availability, AudioAvailability::Ready);

        service
            .execute(AudioCommand::SetStreamMute {
                stream_id: 8,
                muted: true,
            })
            .expect("hidden streams stay addressable by ID");

        assert_eq!(log.borrow().mute_calls, vec![(8, true)]);
    }

    #[test]
    fn include_inactive_policy_lists_corked_streams() {
        let with_idle = discovery(
            vec![output(3, true, 50)],
            vec![stream(7, 3, 50), corked_stream(8, 3)],
        );
        let (backend, _log) = fake([with_idle]);
        let policy = AudioPolicy {
            include_inactive_streams: true,
            ..AudioPolicy::default()
        };
        let mut service = AudioMixerService::with_policy(backend, policy).expect("policy is valid");

        let snapshot = service.refresh().expect("discovery succeeds");

        assert_eq!(
            snapshot
                .streams
                .iter()
                .map(|stream| stream.id)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        assert_eq!(snapshot.inactive_streams, 1);
    }

    #[test]
    fn output_volume_and_mute_require_a_discovered_output() {
        let (backend, log) = fake([standard(50), standard(50), standard(50)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::SetOutputVolume {
                output_id: 99,
                volume_percent: 50,
            })
            .expect_err("unknown output is rejected");
        assert_eq!(
            failure.error,
            AudioCommandError::UnknownOutput { output_id: 99 }
        );

        let failure = service
            .execute(AudioCommand::SetOutputVolume {
                output_id: 3,
                volume_percent: MAX_VOLUME_PERCENT + 1,
            })
            .expect_err("above the ceiling is rejected");
        assert_eq!(
            failure.error,
            AudioCommandError::VolumeOutOfRange {
                requested: MAX_VOLUME_PERCENT + 1,
                maximum: AudioPolicy::default().boost_percent,
            }
        );
        assert!(
            log.borrow().output_volume_calls.is_empty(),
            "rejected output requests must not reach the backend"
        );
    }

    #[test]
    fn output_volume_and_mute_reach_the_backend() {
        let (backend, log) = fake([standard(50), standard(50), standard(50)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        service
            .execute(AudioCommand::SetOutputVolume {
                output_id: 3,
                volume_percent: 120,
            })
            .expect("boosted output volume succeeds");
        service
            .execute(AudioCommand::SetOutputMute {
                output_id: 3,
                muted: true,
            })
            .expect("output mute succeeds");

        assert_eq!(log.borrow().output_volume_calls, vec![(3, 120)]);
        assert_eq!(log.borrow().output_mute_calls, vec![(3, true)]);
    }

    #[test]
    fn cycling_outputs_wraps_and_follows_the_switch_policy() {
        let first = discovery(
            vec![
                output(3, true, 50),
                output(4, false, 50),
                output(5, false, 50),
            ],
            vec![stream(7, 3, 50), stream(9, 5, 50)],
        );
        let second = discovery(
            vec![
                output(3, false, 50),
                output(4, true, 50),
                output(5, false, 50),
            ],
            vec![stream(7, 4, 50), stream(9, 5, 50)],
        );
        let third = discovery(
            vec![
                output(3, false, 50),
                output(4, false, 50),
                output(5, true, 50),
            ],
            vec![stream(7, 4, 50), stream(9, 5, 50)],
        );
        let fourth = discovery(
            vec![
                output(3, true, 50),
                output(4, false, 50),
                output(5, false, 50),
            ],
            vec![stream(7, 4, 50), stream(9, 3, 50)],
        );
        let (backend, log) = fake([first, second, third, fourth]);
        let policy = AudioPolicy {
            output_switch: AudioOutputSwitch::AllStreams,
            ..AudioPolicy::default()
        };
        let mut service = AudioMixerService::with_policy(backend, policy).expect("policy is valid");
        service.refresh().expect("initial discovery");

        service
            .execute(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Next,
            })
            .expect("first cycle succeeds");
        service
            .execute(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Next,
            })
            .expect("second cycle succeeds");
        let snapshot = service
            .execute(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Next,
            })
            .expect("third cycle wraps");

        assert_eq!(
            log.borrow().default_calls,
            vec![
                "output-4".to_string(),
                "output-5".to_string(),
                "output-3".to_string()
            ],
            "cycles advance forward and wrap past the last output"
        );
        assert_eq!(
            snapshot.last_switch,
            Some(super::AudioSwitchOutcome {
                output_id: 3,
                output_name: "output-3".to_string(),
                moved_streams: 2,
                failed_moves: 0,
            })
        );
        assert_eq!(
            log.borrow().move_calls,
            vec![(7, 4), (9, 4), (7, 5), (7, 3), (9, 3)],
            "the all-streams policy re-homes playing streams and skips ones already on the target"
        );
    }

    #[test]
    fn previous_direction_steps_backwards_from_the_default_output() {
        let first = discovery(
            vec![
                output(3, true, 50),
                output(4, false, 50),
                output(5, false, 50),
            ],
            Vec::new(),
        );
        let second = discovery(
            vec![
                output(3, false, 50),
                output(4, false, 50),
                output(5, true, 50),
            ],
            Vec::new(),
        );
        let (backend, log) = fake([first, second]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        service
            .execute(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Previous,
            })
            .expect("previous cycle succeeds");

        assert_eq!(log.borrow().default_calls, vec!["output-5".to_string()]);
    }

    #[test]
    fn default_output_switch_leaves_streams_in_place_by_default() {
        let (backend, log) = fake([standard(50), standard(50)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let snapshot = service
            .execute(AudioCommand::SetDefaultOutput { output_id: 3 })
            .expect("switching to the only output succeeds");

        assert_eq!(
            snapshot.last_switch,
            Some(super::AudioSwitchOutcome {
                output_id: 3,
                output_name: "output-3".to_string(),
                moved_streams: 0,
                failed_moves: 0,
            })
        );
        assert!(log.borrow().move_calls.is_empty());
    }

    #[test]
    fn unknown_default_output_is_rejected() {
        let (backend, log) = fake([standard(50), standard(50)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::SetDefaultOutput { output_id: 99 })
            .expect_err("unknown output is rejected");

        assert_eq!(
            failure.error,
            AudioCommandError::UnknownOutput { output_id: 99 }
        );
        assert!(log.borrow().default_calls.is_empty());
    }

    #[test]
    fn cycling_requires_an_alternate_output() {
        let (backend, log) = fake([standard(50), standard(50)]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let failure = service
            .execute(AudioCommand::CycleOutput {
                direction: AudioCycleDirection::Next,
            })
            .expect_err("a single output cannot be cycled");

        assert_eq!(failure.error, AudioCommandError::NoAlternateOutput);
        assert!(log.borrow().default_calls.is_empty());
    }

    #[test]
    fn routing_requires_a_discovered_output() {
        let (backend, _log) = fake([standard(20), standard(20)]);
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

    #[test]
    fn discovery_failure_clears_routing_state_instead_of_reporting_stale_devices() {
        let (backend, _log) = fake([
            standard(50),
            Err(AudioError {
                kind: AudioErrorKind::Unavailable,
                message: "server went away".to_string(),
            }),
        ]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");

        let error = service
            .refresh()
            .expect_err("the unavailable server is reported");

        assert_eq!(error.kind, AudioErrorKind::Unavailable);
        assert_eq!(
            service.latest().availability,
            AudioAvailability::Unavailable
        );
        assert!(service.latest().outputs.is_empty());
        assert!(service.latest().streams.is_empty());
    }

    #[test]
    fn policy_change_represents_the_retained_discovery_without_rediscovery() {
        let with_idle = discovery(
            vec![output(3, true, 50)],
            vec![stream(7, 3, 50), corked_stream(8, 3)],
        );
        let (backend, _log) = fake([with_idle]);
        let mut service = AudioMixerService::new(backend);
        service.refresh().expect("initial discovery");
        assert_eq!(service.latest().streams.len(), 1);

        let mut policy = service.policy();
        policy.include_inactive_streams = true;
        service.set_policy(policy).expect("policy is valid");

        assert_eq!(service.latest().streams.len(), 2);
    }

    #[test]
    fn outputs_are_grouped_by_owning_card_in_discovery_order() {
        let mut headset = output(6, false, 40);
        headset.card_id = None;
        headset.card_name = None;
        let grouped = discovery(
            vec![output(3, true, 50), output(4, false, 50), headset],
            Vec::new(),
        );
        let (backend, _log) = fake([grouped]);
        let mut service = AudioMixerService::new(backend);

        let snapshot = service.refresh().expect("discovery succeeds");
        let groups = snapshot.outputs_by_card();

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0.as_deref(), Some("Built-in Audio"));
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, None);
        assert_eq!(groups[1].1.len(), 1);
        assert_eq!(snapshot.default_output().map(|output| output.id), Some(3));
    }
}
