//! GStreamer pipelines: one-frame grabs and VP8/Opus WebM recordings.
//!
//! Each branch starts with a leaky queue, so a slow encoder drops frames
//! instead of stalling PipeWire, and finishing sends EOS into those queues,
//! which completes the file even when a source already stopped.

use std::os::fd::RawFd;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::{
    APPLICATION_AUDIO_ELEMENTS, AREA_ELEMENT, AUDIO_ELEMENTS, ActiveRecording, AudioTrack,
    RecordingError, RecordingErrorKind, RecordingEvent, RecordingPlan, RecordingSource,
    SystemAudioSource, VIDEO_ELEMENTS, VideoFrame,
};

/// How long finishing may take to flush encoders and write the index.
const FINISH_TIMEOUT: Duration = Duration::from_secs(10);
/// Buffered media per branch before frames are dropped.
const QUEUE_TIME: Duration = Duration::from_secs(2);
/// How often the chosen application's streams are listed again.
const APPLICATION_REFRESH: Duration = Duration::from_secs(1);
/// How long removing a stream may wait for its branch to detach.
const BRANCH_RELEASE_TIMEOUT: Duration = Duration::from_secs(1);
const AUDIO_RATE: i32 = 48_000;
const AUDIO_BITRATE: i32 = 128_000;
/// Keyframe spacing, so takes can be trimmed and seeked.
const KEYFRAME_SECONDS: u32 = 2;
static INIT: LazyLock<Result<(), String>> =
    LazyLock::new(|| gst::init().map_err(|error| error.to_string()));

pub(super) fn init() -> Result<(), String> {
    INIT.clone()
}

/// Needed elements absent from the GStreamer registry.
pub(crate) fn missing_elements() -> Result<Vec<&'static str>, String> {
    init()?;
    let mut missing = Vec::new();
    for element in VIDEO_ELEMENTS
        .iter()
        .chain(std::iter::once(&AREA_ELEMENT))
        .chain(AUDIO_ELEMENTS.iter())
        .chain(APPLICATION_AUDIO_ELEMENTS.iter())
        .copied()
    {
        if gst::ElementFactory::find(element).is_none() && !missing.contains(&element) {
            missing.push(element);
        }
    }
    Ok(missing)
}

/// Why `source` with `audio` cannot be recorded here, checked before asking
/// the desktop to share anything.
pub fn gstreamer_missing(
    source: RecordingSource,
    audio: &[AudioTrack],
    system_audio: &SystemAudioSource,
) -> Option<RecordingError> {
    let missing = match missing_elements() {
        Ok(missing) => missing,
        Err(error) => {
            return Some(RecordingError::new(
                RecordingErrorKind::Unavailable,
                format!("GStreamer could not start: {error}"),
            ));
        }
    };
    let application = matches!(system_audio, SystemAudioSource::Application { .. })
        && audio.contains(&AudioTrack::System);
    let needed = VIDEO_ELEMENTS
        .iter()
        .chain((source == RecordingSource::Area).then_some(&AREA_ELEMENT))
        .chain(AUDIO_ELEMENTS.iter().filter(|_| !audio.is_empty()))
        .chain(APPLICATION_AUDIO_ELEMENTS.iter().filter(|_| application));
    let mut absent: Vec<&str> = Vec::new();
    for element in needed.copied().filter(|element| missing.contains(element)) {
        if !absent.contains(&element) {
            absent.push(element);
        }
    }
    (!absent.is_empty()).then(|| {
        RecordingError::new(
            RecordingErrorKind::Unavailable,
            format!("GStreamer lacks {}.", absent.join(", ")),
        )
    })
}

/// Where video frames come from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum VideoInput {
    /// A portal-shared stream; the fd stays owned by the session.
    PipeWire { fd: RawFd, node: u32 },
    #[cfg(test)]
    Test,
}

pub(super) fn element(factory: &str) -> Result<gst::Element, RecordingError> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        RecordingError::new(
            RecordingErrorKind::Unavailable,
            format!("GStreamer lacks {factory}."),
        )
    })
}

fn video_source(input: VideoInput) -> Result<gst::Element, RecordingError> {
    match input {
        VideoInput::PipeWire { fd, node } => {
            let source = element("pipewiresrc")?;
            source.set_property("fd", fd);
            source.set_property("path", node.to_string());
            source.set_property("do-timestamp", true);
            // Compositors send frames only on damage; repeat the last one.
            if source.find_property("keepalive-time").is_some() {
                source.set_property("keepalive-time", 1000_i32);
            }
            Ok(source)
        }
        #[cfg(test)]
        VideoInput::Test => {
            let source = element("videotestsrc")?;
            source.set_property("is-live", true);
            Ok(source)
        }
    }
}

fn leaky_queue() -> Result<gst::Element, RecordingError> {
    let queue = element("queue")?;
    queue.set_property("max-size-buffers", 0_u32);
    queue.set_property("max-size-bytes", 0_u32);
    queue.set_property("max-size-time", QUEUE_TIME.as_nanos() as u64);
    queue.set_property_from_str("leaky", "downstream");
    Ok(queue)
}

fn caps_filter(caps: gst::Caps) -> Result<gst::Element, RecordingError> {
    let filter = element("capsfilter")?;
    filter.set_property("caps", caps);
    Ok(filter)
}

fn link_all(elements: &[&gst::Element]) -> Result<(), RecordingError> {
    gst::Element::link_many(elements).map_err(|_| {
        RecordingError::new(RecordingErrorKind::Failed, "Linking the recorder failed.")
    })
}

/// The first error on `bus` within `timeout`, as Kestrel text.
fn bus_error(bus: &gst::Bus, timeout: Duration) -> Option<(Option<gst::Object>, RecordingError)> {
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
        &[gst::MessageType::Error],
    )?;
    let gst::MessageView::Error(error) = message.view() else {
        return None;
    };
    Some((message.src().cloned(), classify(&error.error())))
}

pub(super) fn classify(error: &gst::glib::Error) -> RecordingError {
    if let Some(resource) = error.kind::<gst::ResourceError>() {
        match resource {
            gst::ResourceError::NoSpaceLeft
            | gst::ResourceError::OpenWrite
            | gst::ResourceError::Write => {
                return RecordingError::new(
                    RecordingErrorKind::Storage,
                    "Writing the recording failed; the disk may be full.",
                );
            }
            gst::ResourceError::NotFound | gst::ResourceError::OpenRead => {
                return RecordingError::new(
                    RecordingErrorKind::Unavailable,
                    "The source is no longer available.",
                );
            }
            _ => {}
        }
    }
    RecordingError::new(
        RecordingErrorKind::Failed,
        format!("The recorder stopped: {}", error.message()),
    )
}

/// Reads one RGBA frame from `input`.
pub(crate) fn grab_still(
    input: VideoInput,
    timeout: Duration,
) -> Result<VideoFrame, RecordingError> {
    init().map_err(|error| RecordingError::new(RecordingErrorKind::Unavailable, error))?;
    let pipeline = gst::Pipeline::new();
    let source = video_source(input)?;
    let convert = element("videoconvert")?;
    let filter = caps_filter(
        gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build(),
    )?;
    let sink = gst_app::AppSink::builder()
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .build();
    let sink_element: &gst::Element = sink.upcast_ref();
    pipeline
        .add_many([&source, &convert, &filter, sink_element])
        .map_err(|_| {
            RecordingError::new(RecordingErrorKind::Failed, "Building the grabber failed.")
        })?;
    link_all(&[&source, &convert, &filter, sink_element])?;
    let result = (|| {
        pipeline.set_state(gst::State::Playing).map_err(|_| {
            pipeline
                .bus()
                .and_then(|bus| bus_error(&bus, Duration::ZERO))
                .map_or_else(
                    || RecordingError::new(RecordingErrorKind::Failed, "The source did not start."),
                    |(_, error)| error,
                )
        })?;
        let sample = sink
            .try_pull_sample(gst::ClockTime::from_nseconds(timeout.as_nanos() as u64))
            .ok_or_else(|| {
                pipeline
                    .bus()
                    .and_then(|bus| bus_error(&bus, Duration::ZERO))
                    .map_or_else(
                        || {
                            RecordingError::new(
                                RecordingErrorKind::Timeout,
                                "The shared source sent no picture.",
                            )
                        },
                        |(_, error)| error,
                    )
            })?;
        frame_from_sample(&sample)
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

pub(super) fn frame_from_sample(sample: &gst::Sample) -> Result<VideoFrame, RecordingError> {
    let malformed =
        || RecordingError::new(RecordingErrorKind::Failed, "The picture was malformed.");
    let structure = sample
        .caps()
        .and_then(|caps| caps.structure(0))
        .ok_or_else(malformed)?;
    let width = structure.get::<i32>("width").map_err(|_| malformed())?;
    let height = structure.get::<i32>("height").map_err(|_| malformed())?;
    let (width, height) = (
        u32::try_from(width).map_err(|_| malformed())?,
        u32::try_from(height).map_err(|_| malformed())?,
    );
    if width == 0 || height == 0 {
        return Err(malformed());
    }
    let buffer = sample.buffer().ok_or_else(malformed)?;
    let map = buffer.map_readable().map_err(|_| malformed())?;
    let row = width as usize * 4;
    let stride = map.len() / height as usize;
    if stride < row {
        return Err(malformed());
    }
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in map.chunks(stride).take(height as usize) {
        pixels.extend_from_slice(&line[..row]);
    }
    Ok(VideoFrame {
        width,
        height,
        pixels,
    })
}

/// Builds the source chain of one audio track.
pub(crate) type AudioSourceFactory = dyn Fn(AudioTrack) -> Result<gst::Element, RecordingError>;

fn pulse_source(track: AudioTrack) -> Result<gst::Element, RecordingError> {
    let source = element("pulsesrc")?;
    source.set_property("device", track.device());
    source.set_property("client-name", "Kestrel recording");
    source.set_property("do-timestamp", true);
    Ok(source)
}

/// Lists the playback streams of the application being recorded.
trait ApplicationStreams: Send {
    /// Stream indices, which equal the PipeWire object serials.
    fn streams(&mut self, identity: &str) -> Result<Vec<u32>, ()>;
}

struct PulseStreams;

impl ApplicationStreams for PulseStreams {
    fn streams(&mut self, identity: &str) -> Result<Vec<u32>, ()> {
        crate::audio::application_streams(identity).map_err(|_| ())
    }
}

/// Builds the capture source of one application stream.
type StreamSourceFactory = dyn Fn(u32) -> Result<gst::Element, RecordingError> + Send;

fn pipewire_stream(serial: u32) -> Result<gst::Element, RecordingError> {
    let source = element("pipewiresrc")?;
    source.set_property("target-object", serial.to_string());
    source.set_property("do-timestamp", true);
    // A vanished stream must fail, never be replaced by the default source.
    source.set_property(
        "stream-properties",
        gst::Structure::builder("props")
            .field("node.dont-fallback", true)
            .field("node.dont-reconnect", true)
            .build(),
    );
    Ok(source)
}

/// One captured stream, mixed into the System track.
struct ApplicationStream {
    serial: u32,
    branch: gst::Bin,
    mixer_pad: gst::Pad,
}

/// The branch set behind an application System track.
struct ApplicationAudio {
    identity: String,
    /// Holds the mixer, the silence source, and every stream branch.
    bin: gst::Bin,
    mixer: gst::Element,
    streams: Vec<ApplicationStream>,
    playing: bool,
    checked: Instant,
    discovery: Box<dyn ApplicationStreams>,
    source: Box<StreamSourceFactory>,
}

impl ApplicationAudio {
    /// Lists the application's streams and reconciles branches; the detached
    /// branches and the playing state when it changed.
    fn refresh(&mut self) -> (Vec<gst::Bin>, Option<bool>) {
        if self.checked.elapsed() < APPLICATION_REFRESH {
            return (Vec::new(), None);
        }
        self.checked = Instant::now();
        let Ok(found) = self.discovery.streams(&self.identity) else {
            return (Vec::new(), None);
        };
        let current: Vec<u32> = self.streams.iter().map(|stream| stream.serial).collect();
        let (added, removed) = stream_changes(&current, &found);
        let mut detached = Vec::new();
        for serial in removed {
            detached.extend(self.remove(serial));
        }
        for serial in added {
            let _ = self.add(serial);
        }
        let playing = !self.streams.is_empty();
        let change = if playing == self.playing {
            None
        } else {
            self.playing = playing;
            Some(playing)
        };
        (detached, change)
    }

    fn add(&mut self, serial: u32) -> Result<(), RecordingError> {
        let source = (self.source)(serial)?;
        let queue = leaky_queue()?;
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let filter = caps_filter(audio_caps())?;
        let branch = gst::Bin::with_name(&format!("stream-{serial}"));
        branch
            .add_many([&source, &queue, &convert, &resample, &filter])
            .map_err(|_| stream_failed())?;
        link_all(&[&source, &queue, &convert, &resample, &filter])?;
        let output = filter.static_pad("src").ok_or_else(stream_failed)?;
        let ghost = gst::GhostPad::with_target(&output).map_err(|_| stream_failed())?;
        branch.add_pad(&ghost).map_err(|_| stream_failed())?;
        if self.bin.add(&branch).is_err() {
            return Err(stream_failed());
        }
        let Some(mixer_pad) = self.mixer.request_pad_simple("sink_%u") else {
            let _ = self.bin.remove(&branch);
            return Err(stream_failed());
        };
        if ghost.link(&mixer_pad).is_err() {
            self.mixer.release_request_pad(&mixer_pad);
            let _ = self.bin.remove(&branch);
            return Err(stream_failed());
        }
        let _ = branch.sync_state_with_parent();
        self.streams.push(ApplicationStream {
            serial,
            branch,
            mixer_pad,
        });
        Ok(())
    }

    /// Detaches one stream's branch and returns it.
    fn remove(&mut self, serial: u32) -> Option<gst::Bin> {
        let index = self
            .streams
            .iter()
            .position(|stream| stream.serial == serial)?;
        let stream = self.streams.remove(index);
        let mixer = self.mixer.clone();
        let mixer_pad = stream.mixer_pad.clone();
        let fallback_pad = stream.mixer_pad.clone();
        let branch = stream.branch.clone();
        let Some(output) = branch.static_pad("src") else {
            return Some(stream.branch);
        };
        let (detached, done) = std::sync::mpsc::sync_channel(1);
        let probe = output.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |pad, _| {
            let _ = pad.unlink(&mixer_pad);
            mixer.release_request_pad(&mixer_pad);
            let _ = detached.try_send(());
            gst::PadProbeReturn::Remove
        });
        if done.recv_timeout(BRANCH_RELEASE_TIMEOUT).is_err() {
            if let Some(probe) = probe {
                output.remove_probe(probe);
            }
            let _ = branch.set_state(gst::State::Null);
            let _ = output.unlink(&fallback_pad);
            self.mixer.release_request_pad(&fallback_pad);
        }
        let _ = branch.set_state(gst::State::Null);
        let _ = self.bin.remove(&branch);
        Some(branch)
    }
}

/// Serials to add and remove to turn `current` into `found`.
fn stream_changes(current: &[u32], found: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut added = Vec::new();
    for serial in found {
        if !current.contains(serial) && !added.contains(serial) {
            added.push(*serial);
        }
    }
    let mut removed = Vec::new();
    for serial in current {
        if !found.contains(serial) && !removed.contains(serial) {
            removed.push(*serial);
        }
    }
    (added, removed)
}

fn stream_failed() -> RecordingError {
    RecordingError::new(
        RecordingErrorKind::Failed,
        "Building an application audio stream failed.",
    )
}

fn audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("rate", AUDIO_RATE)
        .field("channels", 2)
        .build()
}

struct Track {
    track: AudioTrack,
    bin: gst::Bin,
    /// Where finishing sends EOS.
    entry: gst::Pad,
    /// Feeds the encoder; pausing drops buffers here.
    gate: gst::Pad,
    /// The bin's output, linked to `mux_pad`.
    output: gst::Pad,
    mux_pad: gst::Pad,
    /// Set once an encoded buffer reached the muxer.
    produced: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A pause in progress: when it began, and the probes dropping buffers.
struct Pause {
    started: gst::ClockTime,
    probes: Vec<(gst::Pad, gst::PadProbeId)>,
}

/// A running VP8/Opus WebM recording.
///
/// Pausing keeps the pipeline playing (live PipeWire sources do not survive
/// PAUSED → PLAYING) and drops buffers before each encoder; resuming shifts
/// later running time back by the paused span, so the file has no gap.
pub(crate) struct GstRecording {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
    video_entry: gst::Pad,
    video_gate: gst::Pad,
    video_source: gst::Element,
    tracks: Vec<Track>,
    /// The System track's application streams, when recording one.
    application: Option<ApplicationAudio>,
    /// Failed tracks, whose late errors are ignored.
    dropped: Vec<gst::Bin>,
    /// Events found while starting, delivered by `poll`.
    pending: Vec<RecordingEvent>,
    pause: Option<Pause>,
    paused_total: gst::ClockTime,
    ended: bool,
    /// The PipeWire connection `pipewiresrc` reads; closed after the pipeline.
    remote: Option<std::os::fd::OwnedFd>,
}

impl GstRecording {
    pub(crate) fn start(input: VideoInput, plan: &RecordingPlan) -> Result<Self, RecordingError> {
        Self::start_with(input, plan, &pulse_source)
    }

    /// Keeps `remote` open for as long as the recording runs.
    pub(crate) fn owning(mut self, remote: std::os::fd::OwnedFd) -> Self {
        self.remote = Some(remote);
        self
    }

    fn start_with(
        input: VideoInput,
        plan: &RecordingPlan,
        audio: &AudioSourceFactory,
    ) -> Result<Self, RecordingError> {
        Self::start_with_streams(
            input,
            plan,
            audio,
            Box::new(PulseStreams),
            Box::new(pipewire_stream),
        )
    }

    fn start_with_streams(
        input: VideoInput,
        plan: &RecordingPlan,
        audio: &AudioSourceFactory,
        discovery: Box<dyn ApplicationStreams>,
        stream_source: Box<StreamSourceFactory>,
    ) -> Result<Self, RecordingError> {
        init().map_err(|error| RecordingError::new(RecordingErrorKind::Unavailable, error))?;
        let build_failed =
            || RecordingError::new(RecordingErrorKind::Failed, "Building the recorder failed.");
        let pipeline = gst::Pipeline::new();
        // One clock for every live source keeps the tracks in step.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let bus = pipeline.bus().ok_or_else(build_failed)?;

        let mux = element("webmmux")?;
        let sink = element("filesink")?;
        sink.set_property("location", plan.output.to_string_lossy().as_ref());
        pipeline
            .add_many([&mux, &sink])
            .map_err(|_| build_failed())?;
        link_all(&[&mux, &sink])?;

        let source = video_source(input)?;
        let queue = leaky_queue()?;
        let mut chain = vec![source.clone(), queue.clone()];
        if let Some(crop) = plan.crop {
            let frame = plan.source_size;
            let cropper = element(AREA_ELEMENT)?;
            cropper.set_property("left", crop.x as i32);
            cropper.set_property("top", crop.y as i32);
            cropper.set_property(
                "right",
                frame.0.saturating_sub(crop.x.saturating_add(crop.width)) as i32,
            );
            cropper.set_property(
                "bottom",
                frame.1.saturating_sub(crop.y.saturating_add(crop.height)) as i32,
            );
            chain.push(cropper);
        }
        let scale = element("videoscale")?;
        scale.set_property("add-borders", true);
        chain.push(scale);
        chain.push(element("videorate")?);
        chain.push(caps_filter(
            gst::Caps::builder("video/x-raw")
                .field("width", plan.width as i32)
                .field("height", plan.height as i32)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .field("framerate", gst::Fraction::new(plan.framerate as i32, 1))
                .build(),
        )?);
        chain.push(element("videoconvert")?);
        chain.push(caps_filter(
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .build(),
        )?);
        let encoder = element("vp8enc")?;
        encoder.set_property("deadline", 1_i64);
        encoder.set_property("cpu-used", 8_i32);
        encoder.set_property(
            "threads",
            std::thread::available_parallelism().map_or(2, |count| count.get().min(8)) as i32,
        );
        encoder.set_property("target-bitrate", video_bitrate(plan));
        encoder.set_property(
            "keyframe-max-dist",
            plan.framerate.saturating_mul(KEYFRAME_SECONDS) as i32,
        );
        chain.push(encoder);
        let refs: Vec<&gst::Element> = chain.iter().collect();
        pipeline
            .add_many(refs.iter().copied())
            .map_err(|_| build_failed())?;
        link_all(&refs)?;
        let video_pad = mux
            .request_pad_simple("video_%u")
            .ok_or_else(build_failed)?;
        chain
            .last()
            .and_then(|last| last.static_pad("src"))
            .ok_or_else(build_failed)?
            .link(&video_pad)
            .map_err(|_| build_failed())?;
        let video_entry = queue.static_pad("sink").ok_or_else(build_failed)?;
        let video_gate = chain
            .iter()
            .rev()
            .nth(1)
            .and_then(|element| element.static_pad("src"))
            .ok_or_else(build_failed)?;

        let mut tracks = Vec::new();
        let mut identity = match &plan.system_audio {
            SystemAudioSource::Application { identity }
                if plan.audio.contains(&AudioTrack::System) =>
            {
                Some(identity.clone())
            }
            _ => None,
        };
        let mut discovery = Some(discovery);
        let mut stream_source = Some(stream_source);
        let mut application = None;
        for track in &plan.audio {
            if *track == AudioTrack::System {
                if let Some(identity) = identity.take() {
                    let (built, state) = application_track(
                        &pipeline,
                        &mux,
                        identity,
                        discovery.take().unwrap_or_else(|| Box::new(PulseStreams)),
                        stream_source
                            .take()
                            .unwrap_or_else(|| Box::new(pipewire_stream)),
                    )?;
                    tracks.push(built);
                    application = Some(state);
                    continue;
                }
            }
            tracks.push(audio_track(&pipeline, &mux, *track, audio)?);
        }
        let mut pending = Vec::new();
        if application.is_some() {
            pending.push(RecordingEvent::ApplicationAudio { playing: false });
        }
        let mut recording = Self {
            pipeline,
            bus,
            video_entry,
            video_gate,
            video_source: source,
            tracks,
            application,
            dropped: Vec::new(),
            pending,
            pause: None,
            paused_total: gst::ClockTime::ZERO,
            ended: false,
            remote: None,
        };
        recording.play()?;
        Ok(recording)
    }

    /// Starts playing; a track whose source fails to open is dropped and
    /// reported, and the rest starts again.
    fn play(&mut self) -> Result<(), RecordingError> {
        loop {
            if self.pipeline.set_state(gst::State::Playing).is_ok() {
                return Ok(());
            }
            let error = bus_error(&self.bus, Duration::from_millis(500));
            let _ = self.pipeline.set_state(gst::State::Null);
            while self.bus.pop().is_some() {}
            let Some((source, error)) = error else {
                return Err(RecordingError::new(
                    RecordingErrorKind::Failed,
                    "The recorder did not start.",
                ));
            };
            let Some(source) = source else {
                return Err(error);
            };
            if self.drop_stream(&source) {
                continue;
            }
            let Some(index) = self.track_of(&source) else {
                return Err(error);
            };
            let track = self.tracks.remove(index);
            if track.track == AudioTrack::System {
                self.application = None;
            }
            let _ = self.pipeline.remove(&track.bin);
            if let Some(mux) = track.mux_pad.parent_element() {
                mux.release_request_pad(&track.mux_pad);
            }
            let _ = track.bin.set_state(gst::State::Null);
            self.pending.push(RecordingEvent::TrackFailed {
                track: track.track,
                error: track_error(track.track, &error),
            });
        }
    }

    fn track_of(&self, source: &gst::Object) -> Option<usize> {
        self.tracks.iter().position(|track| {
            source.has_as_ancestor(&track.bin) || source == track.bin.upcast_ref::<gst::Object>()
        })
    }

    /// Ends one failed track; the muxer continues with the others.
    ///
    /// A track that already fed the muxer gets EOS through its own queue,
    /// whose thread carries it after any buffered audio; sending EOS to the
    /// muxer pad directly, or stopping the bin, can deadlock against that
    /// thread blocked in the muxer. A track that fed nothing would hold the
    /// muxer back forever (it waits for data on every pad), so its pad is
    /// released once the output is idle and the bin stopped.
    ///
    /// Either way the error failed the pipeline's state change if it was
    /// still starting, which leaves the file sink paused once it prerolls and
    /// the muxer blocked behind it, so the pipeline is asked for PLAYING again.
    fn drop_track(&mut self, index: usize) -> AudioTrack {
        let track = self.tracks.remove(index);
        if track.track == AudioTrack::System {
            self.application = None;
        }
        if track.produced.load(std::sync::atomic::Ordering::Acquire) {
            track.entry.send_event(gst::event::Eos::new());
            let _ = self.pipeline.set_state(gst::State::Playing);
        } else {
            let (detached, done) = std::sync::mpsc::sync_channel(1);
            let mux_pad = track.mux_pad.clone();
            track
                .output
                .add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                    let _ = pad.unlink(&mux_pad);
                    if let Some(mux) = mux_pad.parent_element() {
                        mux.release_request_pad(&mux_pad);
                    }
                    let _ = detached.try_send(());
                    gst::PadProbeReturn::Remove
                });
            if done.recv_timeout(Duration::from_secs(1)).is_ok() {
                let _ = track.bin.set_state(gst::State::Null);
                let _ = self.pipeline.remove(&track.bin);
                // Ask again once the failed bin is gone.
                let _ = self.pipeline.set_state(gst::State::Playing);
            }
        }
        self.dropped.push(track.bin);
        track.track
    }

    fn is_dropped(&self, source: &gst::Object) -> bool {
        self.dropped
            .iter()
            .any(|bin| source.has_as_ancestor(bin) || source == bin.upcast_ref::<gst::Object>())
    }

    fn is_video(&self, source: &gst::Object) -> bool {
        source == self.video_source.upcast_ref::<gst::Object>()
    }

    /// Lists the chosen application's streams again, when due.
    fn refresh_application(&mut self) -> Option<RecordingEvent> {
        let (detached, playing) = self.application.as_mut()?.refresh();
        self.dropped.extend(detached);
        playing.map(|playing| RecordingEvent::ApplicationAudio { playing })
    }

    /// Removes the branch `source` belongs to; whether it did.
    fn drop_stream(&mut self, source: &gst::Object) -> bool {
        let Some(application) = self.application.as_ref() else {
            return false;
        };
        let Some(serial) = application
            .streams
            .iter()
            .find(|stream| {
                source.has_as_ancestor(stream.branch.upcast_ref::<gst::Object>())
                    || source == stream.branch.upcast_ref::<gst::Object>()
            })
            .map(|stream| stream.serial)
        else {
            return false;
        };
        if let Some(branch) = self
            .application
            .as_mut()
            .and_then(|application| application.remove(serial))
        {
            self.dropped.push(branch);
        }
        true
    }

    /// The playing change since it was last reported.
    fn application_event(&mut self) -> Option<RecordingEvent> {
        let application = self.application.as_mut()?;
        let playing = !application.streams.is_empty();
        if playing == application.playing {
            return None;
        }
        application.playing = playing;
        Some(RecordingEvent::ApplicationAudio { playing })
    }
}

/// Bits per second for VP8 at the plan's size and rate, within 1–12 Mbit/s.
fn video_bitrate(plan: &RecordingPlan) -> i32 {
    let pixels_per_second = u64::from(plan.width)
        .saturating_mul(u64::from(plan.height))
        .saturating_mul(u64::from(plan.framerate));
    (pixels_per_second / 10).clamp(1_000_000, 12_000_000) as i32
}

fn audio_track(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    track: AudioTrack,
    audio: &AudioSourceFactory,
) -> Result<Track, RecordingError> {
    build_audio_track(pipeline, mux, track, audio(track)?)
}

/// An application System track: a mixer fed by a silence source and one
/// branch per captured stream.
fn application_track(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    identity: String,
    discovery: Box<dyn ApplicationStreams>,
    source: Box<StreamSourceFactory>,
) -> Result<(Track, ApplicationAudio), RecordingError> {
    let app_bin = gst::Bin::with_name("app-audio");
    let mixer = element("audiomixer")?;
    let silence = element("audiotestsrc")?;
    silence.set_property_from_str("wave", "silence");
    silence.set_property("is-live", true);
    let silence_caps = caps_filter(audio_caps())?;
    app_bin
        .add_many([&mixer, &silence, &silence_caps])
        .map_err(|_| stream_failed())?;
    link_all(&[&silence, &silence_caps])?;
    let silence_pad = mixer
        .request_pad_simple("sink_%u")
        .ok_or_else(stream_failed)?;
    silence_caps
        .static_pad("src")
        .ok_or_else(stream_failed)?
        .link(&silence_pad)
        .map_err(|_| stream_failed())?;
    let mixer_src = mixer.static_pad("src").ok_or_else(stream_failed)?;
    let ghost = gst::GhostPad::with_target(&mixer_src).map_err(|_| stream_failed())?;
    app_bin.add_pad(&ghost).map_err(|_| stream_failed())?;
    let checked = Instant::now()
        .checked_sub(APPLICATION_REFRESH)
        .unwrap_or_else(Instant::now);
    let app_source: gst::Element = app_bin.clone().upcast();
    let track = build_audio_track(pipeline, mux, AudioTrack::System, app_source)?;
    Ok((
        track,
        ApplicationAudio {
            identity,
            bin: app_bin,
            mixer,
            streams: Vec::new(),
            playing: false,
            checked,
            discovery,
            source,
        },
    ))
}

/// Wraps `source` in the queue → convert → resample → caps → encoder chain.
fn build_audio_track(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    track: AudioTrack,
    source: gst::Element,
) -> Result<Track, RecordingError> {
    let failed = || {
        RecordingError::new(
            RecordingErrorKind::Failed,
            "Building an audio track failed.",
        )
    };
    let bin = gst::Bin::with_name(&format!(
        "track-{}",
        track.label().to_lowercase().replace(' ', "-")
    ));
    let queue = leaky_queue()?;
    let convert = element("audioconvert")?;
    let resample = element("audioresample")?;
    let filter = caps_filter(audio_caps())?;
    let encoder = element("opusenc")?;
    encoder.set_property("bitrate", AUDIO_BITRATE);
    let chain = [&source, &queue, &convert, &resample, &filter, &encoder];
    bin.add_many(chain).map_err(|_| failed())?;
    link_all(&chain)?;
    let output = encoder.static_pad("src").ok_or_else(failed)?;
    let ghost = gst::GhostPad::with_target(&output).map_err(|_| failed())?;
    bin.add_pad(&ghost).map_err(|_| failed())?;
    pipeline.add(&bin).map_err(|_| failed())?;
    let mux_pad = mux.request_pad_simple("audio_%u").ok_or_else(failed)?;
    ghost.link(&mux_pad).map_err(|_| failed())?;
    let produced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&produced);
    ghost.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        seen.store(true, std::sync::atomic::Ordering::Release);
        gst::PadProbeReturn::Remove
    });
    let entry = queue.static_pad("sink").ok_or_else(failed)?;
    let gate = filter.static_pad("src").ok_or_else(failed)?;
    Ok(Track {
        track,
        bin,
        entry,
        gate,
        output: ghost.upcast(),
        mux_pad,
        produced,
    })
}

fn track_error(track: AudioTrack, error: &RecordingError) -> RecordingError {
    RecordingError::new(
        RecordingErrorKind::Unavailable,
        format!("{} stopped: {}", track.label(), error.message),
    )
}

impl ActiveRecording for GstRecording {
    fn pause(&mut self) -> Result<(), RecordingError> {
        if self.pause.is_some() {
            return Ok(());
        }
        let started = self.pipeline.current_running_time().ok_or_else(|| {
            RecordingError::new(RecordingErrorKind::Failed, "The recorder is not running.")
        })?;
        let probes = std::iter::once(self.video_gate.clone())
            .chain(self.tracks.iter().map(|track| track.gate.clone()))
            .filter_map(|pad| {
                let id =
                    pad.add_probe(gst::PadProbeType::BUFFER, |_, _| gst::PadProbeReturn::Drop)?;
                Some((pad, id))
            })
            .collect();
        self.pause = Some(Pause { started, probes });
        Ok(())
    }

    fn resume(&mut self) -> Result<(), RecordingError> {
        let Some(pause) = self.pause.take() else {
            return Ok(());
        };
        let now = self
            .pipeline
            .current_running_time()
            .unwrap_or(pause.started);
        self.paused_total += now.saturating_sub(pause.started);
        let offset = -i64::try_from(self.paused_total.nseconds()).unwrap_or(i64::MAX);
        self.video_gate.set_offset(offset);
        for track in &self.tracks {
            track.gate.set_offset(offset);
        }
        for (pad, id) in pause.probes {
            pad.remove_probe(id);
        }
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> Option<RecordingEvent> {
        if !self.pending.is_empty() {
            return Some(self.pending.remove(0));
        }
        if let Some(event) = self.refresh_application() {
            return Some(event);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self.bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(remaining.as_nanos() as u64),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            )?;
            match message.view() {
                gst::MessageView::Eos(_) => {
                    if let Some(source) = message.src() {
                        if self.drop_stream(source) {
                            if let Some(event) = self.application_event() {
                                return Some(event);
                            }
                            continue;
                        }
                    }
                    self.ended = true;
                    return Some(RecordingEvent::SourceEnded);
                }
                gst::MessageView::Error(error) => {
                    let classified = classify(&error.error());
                    let Some(source) = message.src() else {
                        return Some(RecordingEvent::Failed(classified));
                    };
                    if self.is_dropped(source) {
                        continue;
                    }
                    if self.drop_stream(source) {
                        if let Some(event) = self.application_event() {
                            return Some(event);
                        }
                        continue;
                    }
                    if let Some(index) = self.track_of(source) {
                        let track = self.drop_track(index);
                        return Some(RecordingEvent::TrackFailed {
                            track,
                            error: track_error(track, &classified),
                        });
                    }
                    if self.is_video(source) {
                        return Some(RecordingEvent::SourceEnded);
                    }
                    return Some(RecordingEvent::Failed(classified));
                }
                _ => {}
            }
            if remaining.is_zero() {
                return None;
            }
        }
    }

    fn finish(mut self: Box<Self>) -> Result<(), RecordingError> {
        // EOS passes the buffer-only pause probes; nothing else needs undoing.
        if let Some(pause) = self.pause.take() {
            for (pad, id) in pause.probes {
                pad.remove_probe(id);
            }
        }
        if !self.ended {
            self.video_entry.send_event(gst::event::Eos::new());
            for track in &self.tracks {
                track.entry.send_event(gst::event::Eos::new());
            }
            let deadline = Instant::now() + FINISH_TIMEOUT;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let Some(message) = self.bus.timed_pop_filtered(
                    gst::ClockTime::from_nseconds(remaining.as_nanos() as u64),
                    &[gst::MessageType::Error, gst::MessageType::Eos],
                ) else {
                    let _ = self.pipeline.set_state(gst::State::Null);
                    return Err(RecordingError::new(
                        RecordingErrorKind::Timeout,
                        "Completing the recording took too long.",
                    ));
                };
                match message.view() {
                    gst::MessageView::Eos(_) => break,
                    gst::MessageView::Error(error) => {
                        // Sources stopping on EOS are expected; the file is not.
                        let classified = classify(&error.error());
                        if classified.kind == RecordingErrorKind::Storage {
                            let _ = self.pipeline.set_state(gst::State::Null);
                            return Err(classified);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.pipeline
            .set_state(gst::State::Null)
            .map(|_| ())
            .map_err(|_| {
                RecordingError::new(RecordingErrorKind::Failed, "Stopping the recorder failed.")
            })
    }

    fn abort(self: Box<Self>) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for GstRecording {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Skips when the local GStreamer lacks test or encoding elements.
    pub(in crate::recording) fn ready(extra: &[&str]) -> bool {
        if init().is_err() {
            eprintln!("skipping: GStreamer did not start");
            return false;
        }
        let needed = [
            "videotestsrc",
            "audiotestsrc",
            "identity",
            "videoconvert",
            "videoscale",
        ]
        .into_iter()
        .chain([
            "videorate",
            "vp8enc",
            "webmmux",
            "filesink",
            "videocrop",
            "appsink",
        ])
        .chain(
            AUDIO_ELEMENTS
                .into_iter()
                .filter(|element| *element != "pulsesrc"),
        )
        .chain(extra.iter().copied());
        let missing: Vec<&str> = needed
            .filter(|element| gst::ElementFactory::find(element).is_none())
            .collect();
        if !missing.is_empty() {
            eprintln!("skipping: GStreamer lacks {}", missing.join(", "));
            return false;
        }
        true
    }

    fn plan(directory: &TempDir, audio: Vec<AudioTrack>) -> RecordingPlan {
        RecordingPlan {
            output: directory.path().join("take.webm"),
            width: 160,
            height: 120,
            source_size: (320, 240),
            crop: None,
            framerate: 15,
            audio,
            system_audio: SystemAudioSource::Output,
        }
    }

    /// Track sources for tests: System plays a tone, Microphone fails after
    /// a few buffers.
    fn test_audio(track: AudioTrack) -> Result<gst::Element, RecordingError> {
        let description = match track {
            AudioTrack::System => "audiotestsrc is-live=true",
            AudioTrack::Microphone => "audiotestsrc is-live=true ! identity error-after=5",
        };
        gst::parse::bin_from_description(description, true)
            .map(|bin| bin.upcast())
            .map_err(|_| RecordingError::new(RecordingErrorKind::Failed, "test source"))
    }

    /// Application stream sources: a tone that keeps playing, or one that
    /// fails after a few buffers.
    fn test_stream(_serial: u32) -> Result<gst::Element, RecordingError> {
        gst::parse::bin_from_description("audiotestsrc is-live=true", true)
            .map(|bin| bin.upcast())
            .map_err(|_| RecordingError::new(RecordingErrorKind::Failed, "test stream"))
    }

    fn failing_stream(_serial: u32) -> Result<gst::Element, RecordingError> {
        gst::parse::bin_from_description("audiotestsrc is-live=true ! identity error-after=5", true)
            .map(|bin| bin.upcast())
            .map_err(|_| RecordingError::new(RecordingErrorKind::Failed, "test stream"))
    }

    struct NoStreams;

    impl ApplicationStreams for NoStreams {
        fn streams(&mut self, _identity: &str) -> Result<Vec<u32>, ()> {
            Ok(Vec::new())
        }
    }

    /// Lists one scripted result per call, repeating the last.
    struct ScriptedStreams {
        lists: Vec<Vec<u32>>,
        calls: usize,
    }

    impl ApplicationStreams for ScriptedStreams {
        fn streams(&mut self, _identity: &str) -> Result<Vec<u32>, ()> {
            let list = self
                .lists
                .get(self.calls)
                .or_else(|| self.lists.last())
                .cloned()
                .unwrap_or_default();
            self.calls += 1;
            Ok(list)
        }
    }

    /// Waits until `track` has fed the muxer. A failing Microphone queues its
    /// buffers before the error, so this always happens; waiting before the
    /// first poll fixes which way the recorder ends the track.
    fn wait_until_recorded(recording: &GstRecording, track: AudioTrack) {
        let track = recording
            .tracks
            .iter()
            .find(|candidate| candidate.track == track)
            .expect("track is recording");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !track.produced.load(std::sync::atomic::Ordering::Acquire) {
            assert!(Instant::now() < deadline, "the track never recorded audio");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Records `length` of test video with a System tone track.
    pub(in crate::recording) fn record_test_take(
        directory: &TempDir,
        length: std::time::Duration,
    ) -> std::path::PathBuf {
        let plan = plan(directory, vec![AudioTrack::System]);
        let recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &test_audio)
                .expect("recording starts"),
        );
        std::thread::sleep(length);
        recording.finish().expect("recording completes");
        plan.output
    }

    /// Streams in the WebM file, as GStreamer discovers them.
    fn stream_kinds(path: &Path) -> Vec<String> {
        let pipeline = gst::parse::launch(&format!(
            "filesrc location={} ! matroskademux name=demux",
            path.display()
        ))
        .expect("demux pipeline")
        .downcast::<gst::Pipeline>()
        .expect("pipeline");
        let demux = pipeline.by_name("demux").expect("demux");
        let kinds = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&kinds);
        let pipeline_weak = pipeline.downgrade();
        demux.connect_pad_added(move |_, pad| {
            seen.lock().expect("kinds").push(pad.name().to_string());
            if let Some(pipeline) = pipeline_weak.upgrade() {
                let sink = gst::ElementFactory::make("fakesink")
                    .build()
                    .expect("fakesink");
                pipeline.add(&sink).expect("add sink");
                sink.sync_state_with_parent().expect("sync sink");
                pad.link(&sink.static_pad("sink").expect("sink pad"))
                    .expect("link sink");
            }
        });
        pipeline.set_state(gst::State::Playing).expect("play demux");
        let bus = pipeline.bus().expect("bus");
        let _ = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let _ = pipeline.set_state(gst::State::Null);
        let mut kinds = kinds.lock().expect("kinds").clone();
        kinds.sort();
        kinds
    }

    #[test]
    fn finishing_writes_a_playable_webm_with_each_track() {
        if !ready(&["matroskademux", "fakesink", "audiomixer"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let mut plan = plan(&directory, vec![AudioTrack::System]);
        plan.system_audio = SystemAudioSource::Application {
            identity: "test-application".to_owned(),
        };
        let mut recording = Box::new(
            GstRecording::start_with_streams(
                VideoInput::Test,
                &plan,
                &test_audio,
                Box::new(NoStreams),
                Box::new(test_stream),
            )
            .expect("recording starts"),
        );
        assert_eq!(
            recording.poll(Duration::from_millis(600)),
            Some(RecordingEvent::ApplicationAudio { playing: false }),
            "the track starts while the application plays nothing"
        );
        assert_eq!(recording.poll(Duration::from_millis(400)), None);
        recording.finish().expect("recording completes");
        let size = std::fs::metadata(&plan.output).expect("output").len();
        assert!(size > 1_000, "output has media ({size} bytes)");
        assert_eq!(stream_kinds(&plan.output), ["audio_0", "video_0"]);
    }

    #[test]
    fn application_streams_join_and_leave_without_failing_the_track() {
        if !ready(&["matroskademux", "fakesink", "audiomixer"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let mut plan = plan(&directory, vec![AudioTrack::System]);
        plan.system_audio = SystemAudioSource::Application {
            identity: "test-application".to_owned(),
        };
        let streams = ScriptedStreams {
            lists: vec![Vec::new(), vec![9], Vec::new()],
            calls: 0,
        };
        let mut recording = Box::new(
            GstRecording::start_with_streams(
                VideoInput::Test,
                &plan,
                &test_audio,
                Box::new(streams),
                Box::new(failing_stream),
            )
            .expect("recording starts"),
        );
        assert_eq!(
            recording.poll(Duration::ZERO),
            Some(RecordingEvent::ApplicationAudio { playing: false })
        );
        assert_eq!(recording.poll(Duration::ZERO), None, "no streams yet");
        std::thread::sleep(APPLICATION_REFRESH);
        assert_eq!(
            recording.poll(Duration::from_secs(2)),
            Some(RecordingEvent::ApplicationAudio { playing: true }),
            "the stream joins the track"
        );
        assert_eq!(
            recording.poll(Duration::from_secs(2)),
            Some(RecordingEvent::ApplicationAudio { playing: false }),
            "the failed branch leaves alone"
        );
        assert_eq!(
            recording.poll(Duration::from_millis(400)),
            None,
            "the System track continues"
        );
        recording.finish().expect("recording completes");
        assert_eq!(stream_kinds(&plan.output), ["audio_0", "video_0"]);
    }

    #[test]
    fn stream_changes_report_new_removed_and_duplicate_serials() {
        for (current, found, added, removed) in [
            (vec![], vec![], vec![], vec![]),
            (vec![], vec![1], vec![1], vec![]),
            (vec![1], vec![], vec![], vec![1]),
            (vec![1], vec![1], vec![], vec![]),
            (vec![1, 2], vec![2, 3], vec![3], vec![1]),
            (vec![], vec![4, 4], vec![4], vec![]),
            (vec![4, 4], vec![], vec![], vec![4]),
        ] {
            assert_eq!(
                stream_changes(&current, &found),
                (added, removed),
                "{current:?} -> {found:?}"
            );
        }
    }

    #[test]
    fn a_failing_track_ends_alone_and_the_take_completes() {
        if !ready(&["matroskademux", "fakesink"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let plan = plan(&directory, vec![AudioTrack::System, AudioTrack::Microphone]);
        let mut recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &test_audio)
                .expect("recording starts"),
        );
        wait_until_recorded(&recording, AudioTrack::Microphone);
        let event = recording.poll(Duration::from_secs(3));
        assert!(
            matches!(
                event,
                Some(RecordingEvent::TrackFailed {
                    track: AudioTrack::Microphone,
                    ..
                })
            ),
            "{event:?}"
        );
        assert_eq!(
            recording.poll(Duration::from_millis(400)),
            None,
            "video and system continue"
        );
        recording.finish().expect("recording completes");
        assert_eq!(
            stream_kinds(&plan.output),
            ["audio_0", "audio_1", "video_0"]
        );
    }

    #[test]
    fn a_track_failing_before_the_take_settles_still_lets_it_complete() {
        if !ready(&["matroskademux", "fakesink"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let plan = plan(&directory, vec![AudioTrack::System, AudioTrack::Microphone]);
        // The Microphone's audio is held back until its source has failed, so
        // the error arrives while the file sink is still waiting to preroll.
        let (failed, source_failed) = std::sync::mpsc::sync_channel(1);
        let held = std::sync::Arc::new(std::sync::Mutex::new(None));
        let hold = std::sync::Arc::clone(&held);
        let sources = move |track: AudioTrack| -> Result<gst::Element, RecordingError> {
            if track == AudioTrack::System {
                return test_audio(track);
            }
            let bin = gst::parse::bin_from_description(
                "audiotestsrc is-live=true ! identity error-after=5 ! queue name=hold",
                true,
            )
            .map_err(|_| RecordingError::new(RecordingErrorKind::Failed, "test source"))?;
            let queue = bin.by_name("hold").expect("hold queue");
            // The source sends EOS right after posting its error.
            let failed = failed.clone();
            queue.static_pad("sink").expect("sink pad").add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM,
                move |_, info| {
                    if let Some(gst::PadProbeData::Event(event)) = &info.data {
                        if event.type_() == gst::EventType::Eos {
                            let _ = failed.try_send(());
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
            let output = queue.static_pad("src").expect("src pad");
            // Buffers only: events and the allocation query must pass, or the
            // source never negotiates far enough to fail.
            let block = output
                .add_probe(
                    gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
                    |_, _| gst::PadProbeReturn::Ok,
                )
                .expect("block probe");
            *hold.lock().expect("hold") = Some((output, block));
            Ok(bin.upcast())
        };
        let mut recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &sources).expect("recording starts"),
        );
        source_failed
            .recv_timeout(Duration::from_secs(5))
            .expect("the Microphone source fails");
        let (output, block) = held.lock().expect("hold").take().expect("held output");
        output.remove_probe(block);
        wait_until_recorded(&recording, AudioTrack::Microphone);
        assert!(matches!(
            recording.poll(Duration::from_secs(3)),
            Some(RecordingEvent::TrackFailed {
                track: AudioTrack::Microphone,
                ..
            })
        ));
        assert_eq!(recording.poll(Duration::from_millis(400)), None);
        recording.finish().expect("recording completes");
        assert_eq!(
            stream_kinds(&plan.output),
            ["audio_0", "audio_1", "video_0"]
        );
    }

    #[test]
    fn a_track_failing_on_its_first_buffer_still_lets_the_take_complete() {
        if !ready(&["matroskademux", "fakesink"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let plan = plan(&directory, vec![AudioTrack::System, AudioTrack::Microphone]);
        let early = |track: AudioTrack| -> Result<gst::Element, RecordingError> {
            let description = match track {
                AudioTrack::System => "audiotestsrc is-live=true",
                AudioTrack::Microphone => "audiotestsrc is-live=true ! identity error-after=1",
            };
            gst::parse::bin_from_description(description, true)
                .map(|bin| bin.upcast())
                .map_err(|_| RecordingError::new(RecordingErrorKind::Failed, "test source"))
        };
        let mut recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &early).expect("recording starts"),
        );
        assert!(matches!(
            recording.poll(Duration::from_secs(3)),
            Some(RecordingEvent::TrackFailed {
                track: AudioTrack::Microphone,
                ..
            })
        ));
        assert_eq!(recording.poll(Duration::from_millis(400)), None);
        recording.finish().expect("recording completes");
        assert!(stream_kinds(&plan.output).contains(&"video_0".to_owned()));
    }

    #[test]
    fn a_track_that_cannot_open_is_reported_and_recording_starts() {
        if !ready(&[]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let plan = plan(&directory, vec![AudioTrack::Microphone]);
        let broken = |_: AudioTrack| -> Result<gst::Element, RecordingError> {
            let source = element("filesrc")?;
            source.set_property("location", "/nonexistent/kestrel-audio");
            Ok(source)
        };
        let mut recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &broken).expect("recording starts"),
        );
        assert!(matches!(
            recording.poll(Duration::ZERO),
            Some(RecordingEvent::TrackFailed {
                track: AudioTrack::Microphone,
                ..
            })
        ));
        assert_eq!(recording.poll(Duration::from_millis(300)), None);
        recording.finish().expect("video-only take completes");
        assert!(std::fs::metadata(&plan.output).expect("output").len() > 0);
    }

    #[test]
    fn paused_time_is_left_out_of_the_take() {
        if !ready(&["matroskademux", "fakesink", "decodebin"]) {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let plan = plan(&directory, Vec::new());
        let mut recording = Box::new(
            GstRecording::start_with(VideoInput::Test, &plan, &test_audio)
                .expect("recording starts"),
        );
        std::thread::sleep(Duration::from_millis(700));
        recording.pause().expect("pause");
        std::thread::sleep(Duration::from_millis(1500));
        recording.resume().expect("resume");
        std::thread::sleep(Duration::from_millis(700));
        recording.finish().expect("recording completes");
        let duration = media_duration(&plan.output);
        assert!(
            duration < Duration::from_millis(2_300),
            "pause excluded, got {duration:?}"
        );
        assert!(
            duration > Duration::from_millis(900),
            "both halves kept, got {duration:?}"
        );
    }

    fn media_duration(path: &Path) -> Duration {
        let pipeline = gst::parse::launch(&format!(
            "filesrc location={} ! decodebin ! fakesink sync=false",
            path.display()
        ))
        .expect("decode pipeline");
        pipeline.set_state(gst::State::Paused).expect("preroll");
        let _ = pipeline.state(gst::ClockTime::from_seconds(5));
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .expect("duration");
        let _ = pipeline.set_state(gst::State::Null);
        Duration::from_nanos(duration.nseconds())
    }

    #[test]
    fn grabs_report_the_frame_size() {
        if !ready(&[]) {
            return;
        }
        let frame = grab_still(VideoInput::Test, Duration::from_secs(5)).expect("frame");
        assert_eq!((frame.width, frame.height), (320, 240));
        assert_eq!(frame.pixels.len(), 320 * 240 * 4);
    }
}
