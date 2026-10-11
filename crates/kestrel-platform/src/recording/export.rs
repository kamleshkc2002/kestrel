//! Reading and trimming takes: duration, one frame at a position, and a
//! re-encoded copy of a time range.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::pipeline::{classify, element, frame_from_sample, init};
use super::{RecordingError, RecordingErrorKind, VideoFrame, element_remediation};

/// Elements reading and re-encoding a take.
pub const EXPORT_ELEMENTS: [&str; 11] = [
    "decodebin",
    "matroskademux",
    "vp8dec",
    "opusdec",
    "videoconvert",
    "vp8enc",
    "audioconvert",
    "audioresample",
    "opusenc",
    "webmmux",
    "filesink",
];
/// How long opening a take may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an export may go without decoding anything before it fails.
const EXPORT_STALL: Duration = Duration::from_secs(20);
const EXPORT_POLL: Duration = Duration::from_millis(100);
const EXPORT_VIDEO_BITRATE: i32 = 6_000_000;
const EXPORT_AUDIO_BITRATE: i32 = 128_000;

fn unreadable() -> RecordingError {
    RecordingError::new(
        RecordingErrorKind::Failed,
        "The recording could not be read.",
    )
}

/// Why exporting cannot run here, with the packages to install.
pub fn export_missing() -> Option<RecordingError> {
    if let Err(error) = init() {
        return Some(RecordingError::new(
            RecordingErrorKind::Unavailable,
            format!("GStreamer could not start: {error}"),
        ));
    }
    let missing: Vec<&str> = EXPORT_ELEMENTS
        .into_iter()
        .filter(|element| gst::ElementFactory::find(element).is_none())
        .collect();
    (!missing.is_empty()).then(|| {
        RecordingError::new(
            RecordingErrorKind::Unavailable,
            format!(
                "Exporting needs {}. {}",
                missing.join(", "),
                element_remediation(&missing)
            ),
        )
    })
}

/// A pipeline reading `path`, with each decoded stream sent to the sink
/// `attach` builds for it. `decodebin` holds PAUSED until every stream is
/// linked, so prerolling waits for the sinks added here.
fn open(
    path: &Path,
    attach: impl Fn(&gst::Pipeline, &gst::Pad) -> Result<(), RecordingError> + Send + Sync + 'static,
) -> Result<gst::Pipeline, RecordingError> {
    init().map_err(|error| RecordingError::new(RecordingErrorKind::Unavailable, error))?;
    let pipeline = gst::Pipeline::new();
    let source = element("filesrc")?;
    source.set_property("location", path.to_string_lossy().as_ref());
    let demux = element("decodebin")?;
    pipeline
        .add_many([&source, &demux])
        .map_err(|_| unreadable())?;
    source.link(&demux).map_err(|_| unreadable())?;
    let weak = pipeline.downgrade();
    demux.connect_pad_added(move |_, pad| {
        if let Some(pipeline) = weak.upgrade() {
            // A stream that cannot be handled fails the pipeline through
            // not-linked, which the caller sees on the bus.
            let _ = attach(&pipeline, pad);
        }
    });
    Ok(pipeline)
}

/// Waits for the pipeline to reach PAUSED, or for an error.
fn preroll(pipeline: &gst::Pipeline) -> Result<(), RecordingError> {
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| first_error(pipeline).unwrap_or_else(unreadable))?;
    let (result, _, _) =
        pipeline.state(gst::ClockTime::from_nseconds(OPEN_TIMEOUT.as_nanos() as u64));
    result
        .map(|_| ())
        .map_err(|_| first_error(pipeline).unwrap_or_else(unreadable))
}

fn first_error(pipeline: &gst::Pipeline) -> Option<RecordingError> {
    let message = pipeline.bus()?.pop_filtered(&[gst::MessageType::Error])?;
    let gst::MessageView::Error(error) = message.view() else {
        return None;
    };
    Some(classify(&error.error()))
}

fn fakesink(pipeline: &gst::Pipeline, pad: &gst::Pad) -> Result<(), RecordingError> {
    let sink = element("fakesink")?;
    sink.set_property("sync", false);
    pipeline.add(&sink).map_err(|_| unreadable())?;
    sink.sync_state_with_parent().map_err(|_| unreadable())?;
    let input = sink.static_pad("sink").ok_or_else(unreadable)?;
    pad.link(&input).map(|_| ()).map_err(|_| unreadable())
}

fn is_video(pad: &gst::Pad) -> bool {
    pad.current_caps()
        .and_then(|caps| {
            caps.structure(0)
                .map(|structure| structure.name().starts_with("video/"))
        })
        .unwrap_or(false)
}

/// The take's length.
pub fn media_duration(path: &Path) -> Result<Duration, RecordingError> {
    let pipeline = open(path, fakesink)?;
    let result = preroll(&pipeline).and_then(|()| {
        pipeline
            .query_duration::<gst::ClockTime>()
            .map(|duration| Duration::from_nanos(duration.nseconds()))
            .ok_or_else(unreadable)
    });
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// The video frame shown at `position`, as RGBA.
pub fn frame_at(path: &Path, position: Duration) -> Result<VideoFrame, RecordingError> {
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build(),
        )
        .max_buffers(1)
        .sync(false)
        .build();
    let video_sink = sink.clone();
    let pipeline = open(path, move |pipeline, pad| {
        if !is_video(pad) {
            return fakesink(pipeline, pad);
        }
        let convert = element("videoconvert")?;
        let output: &gst::Element = video_sink.upcast_ref();
        pipeline
            .add_many([&convert, output])
            .map_err(|_| unreadable())?;
        convert.link(output).map_err(|_| unreadable())?;
        for element in [&convert, output] {
            element.sync_state_with_parent().map_err(|_| unreadable())?;
        }
        let input = convert.static_pad("sink").ok_or_else(unreadable)?;
        pad.link(&input).map(|_| ()).map_err(|_| unreadable())
    })?;
    let result = preroll(&pipeline).and_then(|()| {
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(position.as_nanos() as u64),
            )
            .map_err(|_| unreadable())?;
        let sample = sink
            .try_pull_preroll(gst::ClockTime::from_nseconds(OPEN_TIMEOUT.as_nanos() as u64))
            .ok_or_else(|| first_error(&pipeline).unwrap_or_else(unreadable))?;
        frame_from_sample(&sample)
    });
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Re-encodes `start..end` of `source` into a WebM file at `output`, every
/// stream kept. The take is decoded from the beginning; buffers outside the
/// range are dropped and the rest shifted to start at zero (the muxer
/// refuses seeks). Reports progress from 0 to 1; returns `Cancelled` once
/// `cancel` is set. The caller owns `output` and removes it on failure.
pub fn export_range(
    source: &Path,
    start: Duration,
    end: Duration,
    output: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(), RecordingError> {
    if end <= start {
        return Err(RecordingError::new(
            RecordingErrorKind::Failed,
            "The trimmed range is empty.",
        ));
    }
    if let Some(missing) = export_missing() {
        return Err(missing);
    }
    let mux = element("webmmux")?;
    let sink = element("filesink")?;
    sink.set_property("location", output.to_string_lossy().as_ref());
    let stream_mux = mux.clone();
    let (start_ns, end_ns) = (start.as_nanos() as u64, end.as_nanos() as u64);
    let reached = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stream_reached = std::sync::Arc::clone(&reached);
    let pipeline = open(source, move |pipeline, pad| {
        let chain = if is_video(pad) {
            let encoder = element("vp8enc")?;
            encoder.set_property("deadline", 1_i64);
            encoder.set_property("cpu-used", 4_i32);
            encoder.set_property("target-bitrate", EXPORT_VIDEO_BITRATE);
            vec![element("videoconvert")?, encoder]
        } else {
            let encoder = element("opusenc")?;
            encoder.set_property("bitrate", EXPORT_AUDIO_BITRATE);
            vec![element("audioconvert")?, element("audioresample")?, encoder]
        };
        let refs: Vec<&gst::Element> = chain.iter().collect();
        pipeline
            .add_many(refs.iter().copied())
            .map_err(|_| unreadable())?;
        gst::Element::link_many(refs.iter().copied()).map_err(|_| unreadable())?;
        let template = if is_video(pad) {
            "video_%u"
        } else {
            "audio_%u"
        };
        let mux_pad = stream_mux
            .request_pad_simple(template)
            .ok_or_else(unreadable)?;
        chain
            .last()
            .and_then(|last| last.static_pad("src"))
            .ok_or_else(unreadable)?
            .link(&mux_pad)
            .map_err(|_| unreadable())?;
        for element in &chain {
            element.sync_state_with_parent().map_err(|_| unreadable())?;
        }
        let input = chain[0].static_pad("sink").ok_or_else(unreadable)?;
        input.set_offset(-i64::try_from(start_ns).unwrap_or(i64::MAX));
        let reached = std::sync::Arc::clone(&stream_reached);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let Some(pts) = info.buffer().and_then(|buffer| buffer.pts()) else {
                return gst::PadProbeReturn::Ok;
            };
            let pts = pts.nseconds();
            reached.fetch_max(pts, Ordering::Relaxed);
            if (start_ns..end_ns).contains(&pts) {
                gst::PadProbeReturn::Ok
            } else {
                gst::PadProbeReturn::Drop
            }
        });
        pad.link(&input).map(|_| ()).map_err(|_| unreadable())
    })?;
    pipeline.add_many([&mux, &sink]).map_err(|_| unreadable())?;
    mux.link(&sink).map_err(|_| unreadable())?;
    let result = if cancel.load(Ordering::Acquire) {
        Err(cancelled())
    } else {
        run_export(&pipeline, &reached, end, cancel, progress)
    };
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn cancelled() -> RecordingError {
    RecordingError::new(RecordingErrorKind::Cancelled, "Exporting was cancelled.")
}

fn run_export(
    pipeline: &gst::Pipeline,
    reached: &std::sync::atomic::AtomicU64,
    end: Duration,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(), RecordingError> {
    // No bounded preroll: reaching a late start decodes everything before it.
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| first_error(pipeline).unwrap_or_else(unreadable))?;
    let bus = pipeline.bus().ok_or_else(unreadable)?;
    let mut last_progress = Instant::now();
    // Decoding position and when it last moved; a stuck pipeline fails.
    let mut moved = (0, Instant::now());
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::from_nseconds(EXPORT_POLL.as_nanos() as u64),
            &[gst::MessageType::Error, gst::MessageType::Eos],
        ) {
            match message.view() {
                gst::MessageView::Eos(_) => {
                    progress(1.0);
                    return Ok(());
                }
                gst::MessageView::Error(error) => return Err(classify(&error.error())),
                _ => {}
            }
        }
        if last_progress.elapsed() >= Duration::from_millis(250) {
            last_progress = Instant::now();
            let position = reached.load(Ordering::Relaxed);
            if position != moved.0 {
                moved = (position, Instant::now());
            } else if moved.1.elapsed() >= EXPORT_STALL {
                return Err(RecordingError::new(
                    RecordingErrorKind::Timeout,
                    "Exporting stopped making progress.",
                ));
            }
            let done = Duration::from_nanos(position);
            progress((done.as_secs_f64() / end.as_secs_f64()).clamp(0.0, 1.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::pipeline::tests::{ready, record_test_take};
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[test]
    fn a_trimmed_copy_keeps_the_range_and_every_stream() {
        if !ready(&EXPORT_ELEMENTS) || export_missing().is_some() {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let source = record_test_take(&directory, Duration::from_millis(2500));
        let full = media_duration(&source).expect("duration");
        assert!(full > Duration::from_millis(1800), "{full:?}");
        let output = directory.path().join("trimmed.webm");
        let seen = Mutex::new(Vec::new());
        export_range(
            &source,
            Duration::from_millis(500),
            Duration::from_millis(1500),
            &output,
            &AtomicBool::new(false),
            &|value| seen.lock().expect("progress").push(value),
        )
        .expect("export");
        let trimmed = media_duration(&output).expect("trimmed duration");
        assert!(
            trimmed > Duration::from_millis(800) && trimmed < Duration::from_millis(1300),
            "{trimmed:?}"
        );
        assert_eq!(seen.lock().expect("progress").last(), Some(&1.0));
        let frame = frame_at(&output, Duration::from_millis(200)).expect("frame");
        assert_eq!((frame.width, frame.height), (160, 120));
        assert!(
            media_duration(&source).is_ok(),
            "the source take is untouched"
        );
    }

    #[test]
    fn cancelling_and_bad_input_fail_without_touching_the_source() {
        if !ready(&EXPORT_ELEMENTS) || export_missing().is_some() {
            return;
        }
        let directory = TempDir::new().expect("temp dir");
        let source = record_test_take(&directory, Duration::from_millis(1500));
        let output = directory.path().join("cancelled.webm");
        let error = export_range(
            &source,
            Duration::ZERO,
            Duration::from_millis(1000),
            &output,
            &AtomicBool::new(true),
            &|_| {},
        )
        .expect_err("cancelled");
        assert_eq!(error.kind, RecordingErrorKind::Cancelled);
        assert!(
            export_range(
                &source,
                Duration::from_secs(1),
                Duration::from_secs(1),
                &output,
                &AtomicBool::new(false),
                &|_| {}
            )
            .is_err()
        );
        let garbage = directory.path().join("garbage.webm");
        std::fs::write(&garbage, b"not a recording").expect("garbage");
        assert!(media_duration(&garbage).is_err());
        assert!(media_duration(&source).is_ok());
    }
}
