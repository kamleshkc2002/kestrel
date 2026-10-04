//! Typed PulseAudio-compatible discovery and control.

use std::{
    cell::{Cell, RefCell},
    error::Error,
    fmt,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use kestrel_core::{
    CapabilityEvidence, CapabilityReport, CapabilityStatus, MAX_AUDIO_BOOST_PERCENT,
};
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
/// Feature identifier for global microphone controls.
pub const MICROPHONE_FEATURE_ID: &str = "audio.microphone";

/// A capture source; sink monitors are excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub id: u32,
    /// Server source name, used to select the default.
    pub name: String,
    /// User-facing label, excluded from capability evidence.
    pub description: String,
    pub volume_percent: u8,
    pub muted: bool,
    /// Whether this is the server's default input.
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDiscovery {
    pub default_input_name: Option<String>,
    pub inputs: Vec<InputDevice>,
}

/// Capture-source operations for the microphone service.
pub trait MicrophoneBackend {
    fn discover_inputs(&mut self) -> Result<InputDiscovery, AudioError>;
    fn set_input_mute(&mut self, input_id: u32, muted: bool) -> Result<(), AudioError>;
    /// Sets the named source as the server-wide default.
    fn set_default_input(&mut self, input_name: &str) -> Result<(), AudioError>;
}

/// Read-only `audio.microphone` probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct MicrophoneProbe;

impl MicrophoneProbe {
    pub fn new() -> Self {
        Self
    }
}

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
    /// Whether this is the server's default output.
    pub is_default: bool,
    /// Hardware card index; absent for virtual devices.
    pub card_id: Option<u32>,
    /// Card label used to group devices.
    pub card_name: Option<String>,
    /// Active port name, when available.
    pub port_name: Option<String>,
    pub port_description: Option<String>,
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
    /// Whether the stream is idle/corked.
    pub corked: bool,
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

/// PulseAudio operations for the audio service.
pub trait AudioBackend {
    fn discover(&mut self) -> Result<AudioDiscovery, AudioError>;
    fn set_stream_volume(&mut self, stream_id: u32, volume_percent: u8) -> Result<(), AudioError>;
    fn set_stream_mute(&mut self, stream_id: u32, muted: bool) -> Result<(), AudioError>;
    fn move_stream(&mut self, stream_id: u32, output_id: u32) -> Result<(), AudioError>;
    /// Sets output volume, subject to the shared boost bound.
    fn set_output_volume(&mut self, output_id: u32, volume_percent: u8) -> Result<(), AudioError>;
    fn set_output_mute(&mut self, output_id: u32, muted: bool) -> Result<(), AudioError>;
    /// Sets the named output as the server-wide default.
    fn set_default_output(&mut self, output_name: &str) -> Result<(), AudioError>;
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
        let cards = session.cards()?;
        let outputs = session.outputs(server.default_output_name.as_deref(), &cards)?;
        let streams = session.streams()?;
        Ok(AudioDiscovery {
            server,
            outputs,
            streams,
        })
    }

    /// Rejects amplification above the shared hard cap.
    fn check_volume_bound(volume_percent: u8) -> Result<(), AudioError> {
        if volume_percent > MAX_AUDIO_BOOST_PERCENT {
            return Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!(
                    "volume {volume_percent}% is above the {MAX_AUDIO_BOOST_PERCENT}% amplification cap"
                ),
            ));
        }
        Ok(())
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

    fn output_channel_count(&self, output_id: u32) -> Result<u8, AudioError> {
        let mut session = PulseSession::connect()?;
        let channels = Rc::new(Cell::new(None));
        let list_failed = Rc::new(Cell::new(false));
        let channels_result = Rc::clone(&channels);
        let failed_result = Rc::clone(&list_failed);
        let operation =
            session
                .context
                .introspect()
                .get_sink_info_list(move |result| match result {
                    ListResult::Item(info) if info.index == output_id => {
                        channels_result.set(Some(info.volume.len()))
                    }
                    ListResult::Item(_) => {}
                    ListResult::Error => failed_result.set(true),
                    ListResult::End => {}
                });
        session.drive_operation(&operation)?;
        if list_failed.get() {
            return Err(AudioError::new(
                AudioErrorKind::Protocol,
                format!("failed to inspect output {output_id}"),
            ));
        }
        channels.get().filter(|count| *count > 0).ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Rejected,
                format!("output {output_id} has no writable volume channels"),
            )
        })
    }
}

impl AudioBackend for PulseAudioBackend {
    fn discover(&mut self) -> Result<AudioDiscovery, AudioError> {
        self.discover_inner()
    }

    fn set_stream_volume(&mut self, stream_id: u32, volume_percent: u8) -> Result<(), AudioError> {
        Self::check_volume_bound(volume_percent)?;
        let channels = self.stream_channel_count(stream_id)?;
        let volumes = channel_volumes(channels, volume_percent);

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

    fn set_output_volume(&mut self, output_id: u32, volume_percent: u8) -> Result<(), AudioError> {
        Self::check_volume_bound(volume_percent)?;
        let channels = self.output_channel_count(output_id)?;
        let volumes = channel_volumes(channels, volume_percent);

        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().set_sink_volume_by_index(
            output_id,
            &volumes,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected volume for output {output_id}"),
            ))
        }
    }

    fn set_output_mute(&mut self, output_id: u32, muted: bool) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().set_sink_mute_by_index(
            output_id,
            muted,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected mute for output {output_id}"),
            ))
        }
    }

    fn set_default_output(&mut self, output_name: &str) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.set_default_sink(
            output_name,
            Box::new(move |success| result.set(Some(success))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected default output {output_name}"),
            ))
        }
    }
}

impl MicrophoneBackend for PulseAudioBackend {
    fn discover_inputs(&mut self) -> Result<InputDiscovery, AudioError> {
        let mut session = PulseSession::connect()?;
        let default_input_name = session.default_source_name()?;
        let inputs = session.sources(default_input_name.as_deref())?;
        Ok(InputDiscovery {
            default_input_name,
            inputs,
        })
    }

    fn set_input_mute(&mut self, input_id: u32, muted: bool) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.introspect().set_source_mute_by_index(
            input_id,
            muted,
            Some(Box::new(move |success| result.set(Some(success)))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                format!("PulseAudio rejected mute for input {input_id}"),
            ))
        }
    }

    fn set_default_input(&mut self, input_name: &str) -> Result<(), AudioError> {
        let succeeded = Rc::new(Cell::new(None));
        let result = Rc::clone(&succeeded);
        let mut session = PulseSession::connect()?;
        let operation = session.context.set_default_source(
            input_name,
            Box::new(move |success| result.set(Some(success))),
        );
        session.drive_operation(&operation)?;
        if succeeded.get() == Some(true) {
            Ok(())
        } else {
            Err(AudioError::new(
                AudioErrorKind::Rejected,
                // Keep the source name out of user-visible errors.
                "PulseAudio rejected the default input change",
            ))
        }
    }
}

impl CapabilityProbe for MicrophoneProbe {
    fn probe(&self) -> CapabilityReport {
        let discovery = PulseAudioBackend::new().discover_inputs();
        microphone_capability_for_discovery(&discovery)
    }
}

pub fn microphone_capability_for(discovery: &InputDiscovery) -> CapabilityReport {
    let input_count = discovery.inputs.len();
    let muted_input_count = discovery.inputs.iter().filter(|input| input.muted).count();
    let status = if input_count == 0 {
        CapabilityStatus::Limited {
            reason: "No microphone or other input device is connected.".to_string(),
        }
    } else {
        CapabilityStatus::Supported
    };
    let mut report = CapabilityReport::new(
        MICROPHONE_FEATURE_ID,
        status,
        if input_count == 0 {
            "No microphone or other input device is connected."
        } else {
            "PulseAudio-compatible server exposes capture inputs."
        },
    )
    .with_selected_backend("PulseAudio-compatible protocol via libpulse")
    .with_alternative("Native PipeWire/WirePlumber graph adapter")
    .with_evidence(CapabilityEvidence::new(
        "input_count",
        input_count.to_string(),
    ))
    .with_evidence(CapabilityEvidence::new(
        "muted_input_count",
        muted_input_count.to_string(),
    ));
    if input_count == 0 {
        report = report.with_remediation("Connect or enable a microphone or other input device.");
    }
    report
}

pub fn microphone_capability_for_error(error: &AudioError) -> CapabilityReport {
    CapabilityReport::new(
        MICROPHONE_FEATURE_ID,
        CapabilityStatus::Unsupported {
            reason: error.message.clone(),
        },
        "PulseAudio-compatible microphone service is unavailable.",
    )
    .with_selected_backend("PulseAudio-compatible protocol via libpulse")
    .with_alternative("Native PipeWire/WirePlumber graph adapter")
    .with_remediation(
        "Start PulseAudio or PipeWire's PulseAudio compatibility service for this user session.",
    )
    .with_evidence(CapabilityEvidence::new(
        "error_kind",
        format!("{:?}", error.kind),
    ))
}

pub fn microphone_capability_for_discovery(
    discovery: &Result<InputDiscovery, AudioError>,
) -> CapabilityReport {
    match discovery {
        Ok(discovery) => microphone_capability_for(discovery),
        Err(error) => microphone_capability_for_error(error),
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

    /// Resolves card indices to labels for device grouping.
    fn cards(&mut self) -> Result<Vec<(u32, String)>, AudioError> {
        let result = Rc::new(RefCell::new(Vec::new()));
        let failed = Rc::new(Cell::new(false));
        let callback_result = Rc::clone(&result);
        let callback_failed = Rc::clone(&failed);
        let operation = self
            .context
            .introspect()
            .get_card_info_list(move |item| match item {
                ListResult::Item(info) => {
                    let label = info
                        .proplist
                        .get_str(properties::DEVICE_DESCRIPTION)
                        .or_else(|| info.name.as_deref().map(str::to_owned))
                        .unwrap_or_else(|| format!("Card {}", info.index));
                    callback_result.borrow_mut().push((info.index, label));
                }
                ListResult::Error => callback_failed.set(true),
                ListResult::End => {}
            });
        self.drive_operation(&operation)?;
        if failed.get() {
            Err(AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio card discovery failed",
            ))
        } else {
            Ok(result.borrow().clone())
        }
    }

    fn outputs(
        &mut self,
        default_output_name: Option<&str>,
        cards: &[(u32, String)],
    ) -> Result<Vec<OutputDevice>, AudioError> {
        let result = Rc::new(RefCell::new(Vec::new()));
        let failed = Rc::new(Cell::new(false));
        let callback_result = Rc::clone(&result);
        let callback_failed = Rc::clone(&failed);
        let callback_default = default_output_name.map(str::to_owned);
        let callback_cards = cards.to_vec();
        let operation = self
            .context
            .introspect()
            .get_sink_info_list(move |item| match item {
                ListResult::Item(info) => {
                    let name = info
                        .name
                        .as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("output-{}", info.index));
                    let card_name = info.card.and_then(|card| {
                        callback_cards
                            .iter()
                            .find(|(index, _)| *index == card)
                            .map(|(_, label)| label.clone())
                    });
                    let (port_name, port_description) = info
                        .active_port
                        .as_ref()
                        .map(|port| {
                            (
                                Some(port.name.as_deref().map(str::to_owned).unwrap_or_default()),
                                port.description.as_deref().map(str::to_owned),
                            )
                        })
                        .unwrap_or((None, None));
                    let is_default = callback_default.as_deref() == Some(name.as_str());
                    callback_result.borrow_mut().push(OutputDevice {
                        id: info.index,
                        name,
                        description: info
                            .description
                            .as_deref()
                            .map(str::to_owned)
                            .or_else(|| info.name.as_deref().map(str::to_owned))
                            .unwrap_or_else(|| format!("Output {}", info.index)),
                        volume_percent: volume_percent(info.volume.avg()),
                        muted: info.mute,
                        is_default,
                        card_id: info.card,
                        card_name,
                        port_name: port_name.filter(|name| !name.is_empty()),
                        port_description,
                    });
                }
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
                            corked: info.corked,
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
    fn default_source_name(&mut self) -> Result<Option<String>, AudioError> {
        let result = Rc::new(RefCell::new(None));
        let callback_result = Rc::clone(&result);
        let operation = self.context.introspect().get_server_info(move |info| {
            *callback_result.borrow_mut() =
                Some(info.default_source_name.as_deref().map(str::to_owned));
        });
        self.drive_operation(&operation)?;
        let server = result.borrow_mut().take();
        server.ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio server information was missing",
            )
        })
    }

    fn sources(
        &mut self,
        default_input_name: Option<&str>,
    ) -> Result<Vec<InputDevice>, AudioError> {
        let result = Rc::new(RefCell::new(Vec::new()));
        let failed = Rc::new(Cell::new(false));
        let callback_result = Rc::clone(&result);
        let callback_failed = Rc::clone(&failed);
        let callback_default = default_input_name.map(str::to_owned);
        let operation = self
            .context
            .introspect()
            .get_source_info_list(move |item| match item {
                ListResult::Item(info) if info.monitor_of_sink.is_none() => {
                    let name = info
                        .name
                        .as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("input-{}", info.index));
                    callback_result.borrow_mut().push(InputDevice {
                        id: info.index,
                        name: name.clone(),
                        description: info
                            .description
                            .as_deref()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("Input {}", info.index)),
                        volume_percent: volume_percent(info.volume.avg()),
                        muted: info.mute,
                        is_default: callback_default.as_deref() == Some(name.as_str()),
                    });
                }
                ListResult::Item(_) => {}
                ListResult::Error => callback_failed.set(true),
                ListResult::End => {}
            });
        self.drive_operation(&operation)?;
        if failed.get() {
            Err(AudioError::new(
                AudioErrorKind::Protocol,
                "PulseAudio input discovery failed",
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

/// Converts server volume to a percentage while preserving amplification.
///
/// Values above the cap remain readable; rounding preserves set/read values.
fn volume_percent(volume: Volume) -> u8 {
    let percent = (u64::from(volume.0).saturating_mul(100) + u64::from(Volume::NORMAL.0) / 2)
        / u64::from(Volume::NORMAL.0);
    percent.min(u64::from(u8::MAX)) as u8
}

/// Builds channel volumes for a percentage, including amplification.
fn channel_volumes(channels: u8, volume_percent: u8) -> ChannelVolumes {
    let raw = (u64::from(Volume::NORMAL.0).saturating_mul(u64::from(volume_percent)) + 50) / 100;
    let mut volumes = ChannelVolumes::default();
    volumes.set(channels, Volume(raw.min(u64::from(u32::MAX)) as u32));
    volumes
}

/// Builds a capability report from borrowed discovery data.
pub fn capability_for(discovery: &AudioDiscovery) -> CapabilityReport {
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

pub fn capability_for_discovery(
    discovery: &Result<AudioDiscovery, AudioError>,
) -> CapabilityReport {
    match discovery {
        Ok(discovery) => capability_for(discovery),
        Err(error) => capability_for_error(error),
    }
}

/// Builds the unavailable capability report for a failed discovery.
pub fn capability_for_error(error: &AudioError) -> CapabilityReport {
    CapabilityReport::new(
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
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        AudioBackend, AudioDiscovery, AudioErrorKind, AudioServer, InputDevice, InputDiscovery,
        OutputDevice, PulseAudioBackend, capability_for_discovery, channel_volumes,
        microphone_capability_for, microphone_capability_for_error, volume_percent,
    };
    use kestrel_core::{CapabilityStatus, MAX_AUDIO_BOOST_PERCENT};
    use libpulse_binding::volume::Volume;

    fn output(output_id: u32) -> OutputDevice {
        OutputDevice {
            id: output_id,
            name: format!("output-{output_id}"),
            description: format!("Output {output_id}"),
            volume_percent: 50,
            muted: false,
            is_default: output_id == 1,
            card_id: Some(0),
            card_name: Some("Built-in Audio".to_string()),
            port_name: Some("analog-output".to_string()),
            port_description: Some("Headphones".to_string()),
        }
    }

    #[test]
    fn volume_conversion_preserves_amplification_up_to_the_type_bound() {
        assert_eq!(volume_percent(Volume::MUTED), 0);
        assert_eq!(volume_percent(Volume::NORMAL), 100);
        assert_eq!(
            volume_percent(Volume(Volume::NORMAL.0 / 2)),
            50,
            "half volume must stay halfway, not saturate"
        );
        assert_eq!(
            volume_percent(Volume(Volume::NORMAL.0 + Volume::NORMAL.0 / 2)),
            150
        );
        assert_eq!(volume_percent(Volume(u32::MAX)), u8::MAX);
    }

    #[test]
    fn channel_volumes_carry_amplification_and_respect_the_hard_cap() {
        let boosted = channel_volumes(2, MAX_AUDIO_BOOST_PERCENT);
        assert!(boosted.avg().0 > Volume::NORMAL.0);

        let unamplified = channel_volumes(2, 100);
        assert_eq!(unamplified.avg().0, Volume::NORMAL.0);
        assert_eq!(unamplified.len(), 2);
    }

    #[test]
    fn every_percentage_round_trips_through_server_volumes() {
        for percent in 0..=MAX_AUDIO_BOOST_PERCENT {
            let volumes = channel_volumes(2, percent);
            assert_eq!(
                volume_percent(volumes.avg()),
                percent,
                "{percent}% must survive a set/read round trip"
            );
        }
    }

    #[test]
    fn adapter_rejects_volume_above_the_amplification_cap_before_connecting() {
        let error = PulseAudioBackend::new()
            .set_stream_volume(1, MAX_AUDIO_BOOST_PERCENT + 1)
            .expect_err("volume above the amplification cap is rejected");

        assert_eq!(error.kind, AudioErrorKind::Rejected);
        assert!(error.message.contains("150% amplification cap"));
    }

    #[test]
    fn reachable_server_without_streams_is_limited_not_unavailable() {
        let report = capability_for_discovery(&Ok(AudioDiscovery {
            server: AudioServer {
                name: Some("PulseAudio (on PipeWire)".to_string()),
                version: Some("1".to_string()),
                protocol_version: Some(35),
                default_output_name: Some("output-1".to_string()),
            },
            outputs: vec![output(1)],
            streams: Vec::new(),
        }));

        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert_eq!(report.evidence[1].value, "0");
    }
    fn input(id: u32, muted: bool) -> InputDevice {
        InputDevice {
            id,
            name: format!("private-source-{id}"),
            description: format!("Private source {id}"),
            volume_percent: 100,
            muted,
            is_default: id == 1,
        }
    }

    #[test]
    fn microphone_capability_reports_supported_and_sanitized_evidence() {
        let report = microphone_capability_for(&InputDiscovery {
            default_input_name: Some("private-source-1".to_string()),
            inputs: vec![input(1, true), input(2, false)],
        });
        assert!(matches!(report.status, CapabilityStatus::Supported));
        assert_eq!(report.evidence[0].key, "input_count");
        assert_eq!(report.evidence[0].value, "2");
        assert_eq!(report.evidence[1].key, "muted_input_count");
        assert_eq!(report.evidence[1].value, "1");
        let evidence = report
            .evidence
            .iter()
            .map(|item| format!("{}{}", item.key, item.value))
            .collect::<String>();
        assert!(!evidence.contains("private-source"));
        assert!(!evidence.contains("Private source"));
    }

    #[test]
    fn microphone_capability_reports_limited_without_inputs() {
        let report = microphone_capability_for(&InputDiscovery {
            default_input_name: None,
            inputs: Vec::new(),
        });
        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert!(report.remediation.is_some());
    }

    #[test]
    fn microphone_capability_reports_unsupported_errors() {
        let report = microphone_capability_for_error(&super::AudioError {
            kind: AudioErrorKind::Unavailable,
            message: "backend unavailable".to_string(),
        });
        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert_eq!(report.evidence[0].key, "error_kind");
        assert!(!report.evidence[0].value.contains("private-source"));
    }
}
