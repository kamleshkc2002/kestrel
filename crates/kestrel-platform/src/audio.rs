//! Typed PulseAudio-compatible discovery and control.

use std::{
    cell::{Cell, RefCell},
    error::Error,
    fmt,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{FlagSet as ContextFlagSet, State as ContextState},
    mainloop::standard::{IterateResult, Mainloop},
    operation,
    proplist::properties,
    volume::{ChannelVolumes, Volume},
};

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "audio.mixer";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const IDLE_SLEEP: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioServer {
    pub name: Option<String>,
    pub version: Option<String>,
    pub protocol_version: Option<u32>,
    pub default_output_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub volume_percent: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackStream {
    pub id: u32,
    pub name: String,
    pub application_name: Option<String>,
    pub media_name: Option<String>,
    pub media_role: Option<String>,
    pub output_id: u32,
    pub volume_percent: Option<u8>,
    pub volume_writable: bool,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDiscovery {
    pub server: AudioServer,
    pub outputs: Vec<OutputDevice>,
    pub streams: Vec<PlaybackStream>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioErrorKind {
    Unavailable,
    TimedOut,
    Protocol,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError {
    pub kind: AudioErrorKind,
    pub message: String,
}

impl AudioError {
    fn new(kind: AudioErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AudioError {}

/// Domain-shaped PulseAudio operations used by the audio service.
pub trait AudioBackend {
    fn discover(&mut self) -> Result<AudioDiscovery, AudioError>;
    fn set_stream_volume(&mut self, stream_id: u32, volume_percent: u8) -> Result<(), AudioError>;
    fn set_stream_mute(&mut self, stream_id: u32, muted: bool) -> Result<(), AudioError>;
    fn move_stream(&mut self, stream_id: u32, output_id: u32) -> Result<(), AudioError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PulseAudioBackend;

impl PulseAudioBackend {
    pub fn new() -> Self {
        Self
    }

    fn discover_inner(&self) -> Result<AudioDiscovery, AudioError> {
        let mut session = PulseSession::connect()?;
        let server = session.server_info()?;
        let outputs = session.outputs()?;
        let streams = session.streams()?;
        Ok(AudioDiscovery {
            server,
            outputs,
            streams,
        })
    }

    fn stream_channel_count(&self, stream_id: u32) -> Result<u8, AudioError> {
        let mut session = PulseSession::connect()?;
        let channels = Rc::new(Cell::new(None));
        let list_failed = Rc::new(Cell::new(false));
        let channels_result = Rc::clone(&channels);
        let failed_result = Rc::clone(&list_failed);
        let operation = session.context.introspect().get_sink_input_info(
            stream_id,
            move |result| match result {
                ListResult::Item(info) => channels_result.set(Some(info.volume.len())),
                ListResult::Error => failed_result.set(true),
                ListResult::End => {}
            },
        );
        session.drive_operation(&operation)?;
        if list_failed.get() {
            return Err(AudioError::new(
                AudioErrorKind::Protocol,
                format!("failed to inspect stream {stream_id}"),
            ));
        }
        channels.get().filter(|count| *count > 0).ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Rejected,
                format!("stream {stream_id} has no writable volume channels"),
            )
        })
    }
}

impl AudioBackend for PulseAudioBackend {
    fn discover(&mut self) -> Result<AudioDiscovery, AudioError> {
        self.discover_inner()
    }

    fn set_stream_volume(&mut self, stream_id: u32, volume_percent: u8) -> Result<(), AudioError> {
        if volume_percent > 100 {
            return Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("volume {volume_percent}% is outside the supported 0..=100% range"),
            ));
        }
        let channels = self.stream_channel_count(stream_id)?;
        let raw = u64::from(Volume::NORMAL.0).saturating_mul(u64::from(volume_percent)) / 100;
        let mut volumes = ChannelVolumes::default();
        volumes.set(channels, Volume(raw as u32));

        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().set_sink_input_volume(
            stream_id,
            &volumes,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected volume for stream {stream_id}"),
            ))
        }
    }

    fn set_stream_mute(&mut self, stream_id: u32, muted: bool) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().set_sink_input_mute(
            stream_id,
            muted,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected mute for stream {stream_id}"),
            ))
        }
    }

    fn move_stream(&mut self, stream_id: u32, output_id: u32) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().move_sink_input_by_index(
            stream_id,
            output_id,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected routing stream {stream_id} to output {output_id}"),
            ))
        }
    }
}

impl CapabilityProbe for PulseAudioBackend {
    fn probe(&self) -> CapabilityReport {
        let discovery = self.discover_inner();
        capability_for_discovery(&discovery)
    }
}

struct PulseSession {
    mainloop: Mainloop,
    context: pulse::context::Context,
}

impl PulseSession {
    fn connect() -> Result<Self, AudioError> {
        let mainloop = Mainloop::new().ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Unavailable,
                "failed to create a PulseAudio main loop",
            )
        })?;
        let mut context = pulse::context::Context::new(&mainloop, "Kestrel").ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Unavailable,
                "failed to create a PulseAudio client context",
            )
        })?;
        context
            .connect(None, ContextFlagSet::NOAUTOSPAWN, None)
            .map_err(|error| {
                AudioError::new(
                    AudioErrorKind::Unavailable,
                    format!("failed to connect to the PulseAudio-compatible server: {error}"),
                )
            })?;
        let mut session = Self { mainloop, context };
        session.drive_until(|session| {
            matches!(
                session.context.get_state(),
                ContextState::Ready | ContextState::Failed | ContextState::Terminated
            )
        })?;
        match session.context.get_state() {
            ContextState::Ready => Ok(session),
            _ => Err(AudioError::new(
                AudioErrorKind::Unavailable,
                format!(
                    "PulseAudio-compatible server is unavailable: {}",
                    session.context.errno()
                ),
            )),
        }
    }

    fn iterate(&mut self) -> Result<(), AudioError> {
        match self.mainloop.iterate(false) {
            IterateResult::Success(_) => {
                thread::sleep(IDLE_SLEEP);
                Ok(())
            }
            IterateResult::Quit(_) => Err(AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio main loop quit unexpectedly",
            )),
            IterateResult::Err(error) => Err(AudioError::new(
                AudioErrorKind::Protocol,
                format!("PulseAudio main loop failed: {error}"),
            )),
        }
    }

    fn drive_until(&mut self, mut complete: impl FnMut(&Self) -> bool) -> Result<(), AudioError> {
        let deadline = Instant::now() + OPERATION_TIMEOUT;
        while !complete(self) {
            if Instant::now() >= deadline {
                return Err(AudioError::new(
                    AudioErrorKind::TimedOut,
                    "PulseAudio operation timed out",
                ));
            }
            self.iterate()?;
            if matches!(
                self.context.get_state(),
                ContextState::Failed | ContextState::Terminated
            ) {
                return Err(AudioError::new(
                    AudioErrorKind::Unavailable,
                    format!("PulseAudio connection was lost: {}", self.context.errno()),
                ));
            }
        }
        Ok(())
    }

    fn drive_operation<T: ?Sized>(
        &mut self,
        operation: &pulse::operation::Operation<T>,
    ) -> Result<(), AudioError> {
        self.drive_until(|_| operation.get_state() != operation::State::Running)
    }

    fn server_info(&mut self) -> Result<AudioServer, AudioError> {
        let result = Rc::new(RefCell::new(None));
        let callback_result = Rc::clone(&result);
        let operation = self.context.introspect().get_server_info(move |info| {
            *callback_result.borrow_mut() = Some(AudioServer {
                name: info.server_name.as_deref().map(str::to_owned),
                version: info.server_version.as_deref().map(str::to_owned),
                protocol_version: None,
                default_output_name: info.default_sink_name.as_deref().map(str::to_owned),
            });
        });
        self.drive_operation(&operation)?;
        let mut server = result.borrow_mut().take().ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio server information was missing",
            )
        })?;
        server.protocol_version = self.context.get_server_protocol_version();
        Ok(server)
    }

    fn outputs(&mut self) -> Result<Vec<OutputDevice>, AudioError> {
        let result = Rc::new(RefCell::new(Vec::new()));
        let failed = Rc::new(Cell::new(false));
        let callback_result = Rc::clone(&result);
        let callback_failed = Rc::clone(&failed);
        let operation = self
            .context
            .introspect()
            .get_sink_info_list(move |item| match item {
                ListResult::Item(info) => callback_result.borrow_mut().push(OutputDevice {
                    id: info.index,
                    name: info
                        .name
                        .as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("output-{}", info.index)),
                    description: info
                        .description
                        .as_deref()
                        .map(str::to_owned)
                        .or_else(|| info.name.as_deref().map(str::to_owned))
                        .unwrap_or_else(|| format!("Output {}", info.index)),
                    volume_percent: volume_percent(info.volume.avg()),
                    muted: info.mute,
                }),
                ListResult::Error => callback_failed.set(true),
                ListResult::End => {}
            });
        self.drive_operation(&operation)?;
        if failed.get() {
            Err(AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio output discovery failed",
            ))
        } else {
            Ok(result.borrow_mut().drain(..).collect())
        }
    }

    fn streams(&mut self) -> Result<Vec<PlaybackStream>, AudioError> {
        let result = Rc::new(RefCell::new(Vec::new()));
        let failed = Rc::new(Cell::new(false));
        let callback_result = Rc::clone(&result);
        let callback_failed = Rc::clone(&failed);
        let operation =
            self.context
                .introspect()
                .get_sink_input_info_list(move |item| match item {
                    ListResult::Item(info) => {
                        callback_result.borrow_mut().push(PlaybackStream {
                            id: info.index,
                            name: info
                                .name
                                .as_deref()
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("Stream {}", info.index)),
                            application_name: info.proplist.get_str(properties::APPLICATION_NAME),
                            media_name: info.proplist.get_str(properties::MEDIA_NAME),
                            media_role: info.proplist.get_str(properties::MEDIA_ROLE),
                            output_id: info.sink,
                            volume_percent: info
                                .has_volume
                                .then(|| volume_percent(info.volume.avg())),
                            volume_writable: info.has_volume && info.volume_writable,
                            muted: info.mute,
                        });
                    }
                    ListResult::Error => callback_failed.set(true),
                    ListResult::End => {}
                });
        self.drive_operation(&operation)?;
        if failed.get() {
            Err(AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio stream discovery failed",
            ))
        } else {
            Ok(result.borrow_mut().drain(..).collect())
        }
    }
}

impl Drop for PulseSession {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

fn volume_percent(volume: Volume) -> u8 {
    let percent = u64::from(volume.0).saturating_mul(100) / u64::from(Volume::NORMAL.0);
    percent.min(100) as u8
}

pub fn capability_for_discovery(
    discovery: &Result<AudioDiscovery, AudioError>,
) -> CapabilityReport {
    match discovery {
        Ok(discovery) => {
            let stream_count = discovery.streams.len();
            let output_count = discovery.outputs.len();
            let status = if output_count == 0 {
                CapabilityStatus::Limited {
                    reason: "The audio server has no output devices.".to_string(),
                }
            } else if stream_count == 0 {
                CapabilityStatus::Limited {
                    reason: "There are no active playback streams.".to_string(),
                }
            } else {
                CapabilityStatus::Supported
            };
            let mut report = CapabilityReport::new(
                FEATURE_ID,
                status,
                format!(
                    "PulseAudio-compatible server exposes {output_count} outputs and {stream_count} active streams."
                ),
            )
            .with_selected_backend("PulseAudio-compatible protocol via libpulse")
            .with_alternative("Native PipeWire/WirePlumber graph adapter")
            .with_evidence(CapabilityEvidence::new(
                "output_count",
                output_count.to_string(),
            ))
            .with_evidence(CapabilityEvidence::new(
                "active_stream_count",
                stream_count.to_string(),
            ));
            if output_count == 0 || stream_count == 0 {
                report = report.with_remediation(if output_count == 0 {
                    "Connect or enable an audio output device."
                } else {
                    "Start playback in an application to expose a controllable stream."
                });
            }
            report
        }
        Err(error) => CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Unsupported {
                reason: error.message.clone(),
            },
            "PulseAudio-compatible audio service is unavailable.",
        )
        .with_selected_backend("PulseAudio-compatible protocol via libpulse")
        .with_alternative("Native PipeWire/WirePlumber graph adapter")
        .with_remediation(
            "Start PulseAudio or PipeWire's PulseAudio compatibility service for this user session.",
        )
        .with_evidence(CapabilityEvidence::new(
            "error_kind",
            format!("{:?}", error.kind),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capability_for_discovery, volume_percent, AudioBackend, AudioDiscovery, AudioErrorKind,
        AudioServer, OutputDevice, PulseAudioBackend,
    };
    use kestrel_core::CapabilityStatus;
    use libpulse_binding::volume::Volume;

    #[test]
    fn volume_conversion_is_bounded_to_one_hundred_percent() {
        assert_eq!(volume_percent(Volume::MUTED), 0);
        assert_eq!(volume_percent(Volume::NORMAL), 100);
        assert_eq!(volume_percent(Volume(Volume::NORMAL.0 * 2)), 100);
    }

    #[test]
    fn adapter_rejects_amplified_volume_before_connecting() {
        let error = PulseAudioBackend::new()
            .set_stream_volume(1, 101)
            .expect_err("volume above the service bound is rejected");

        assert_eq!(error.kind, AudioErrorKind::Rejected);
    }

    #[test]
    fn reachable_server_without_streams_is_limited_not_unavailable() {
        let report = capability_for_discovery(&Ok(AudioDiscovery {
            server: AudioServer {
                name: Some("PulseAudio (on PipeWire)".to_string()),
                version: Some("1".to_string()),
                protocol_version: Some(35),
                default_output_name: Some("default".to_string()),
            },
            outputs: vec![OutputDevice {
                id: 1,
                name: "default".to_string(),
                description: "Default output".to_string(),
                volume_percent: 50,
                muted: false,
            }],
            streams: Vec::new(),
        }));

        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert_eq!(report.evidence[1].value, "0");
    }
}
