//! `kestrel-record`: the GStreamer half of screen recording.
//!
//! Kestrel starts this helper on demand and speaks the line protocol in
//! `kestrel_platform::recording::helper::protocol` over stdin and stdout. The
//! PipeWire remote arrives as fd 3; the helper exits when its parent closes
//! the pipes, so a crashed app leaves nothing running.

mod export;
mod pipeline;

use std::io::{self, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kestrel_platform::recording::ActiveRecording;
use kestrel_platform::recording::helper::protocol::{
    HelperCommand, HelperReply, PIPE_WIRE_FD, read_command, write_line,
};

use export::{export_range, frame_at, media_duration};
use pipeline::{GstRecording, VideoInput, grab_still, missing_elements};

type Writer = Arc<Mutex<io::Stdout>>;

fn main() -> ExitCode {
    let mut commands = BufReader::new(io::stdin().lock());
    let writer: Writer = Arc::new(Mutex::new(io::stdout()));
    let mut recording: Option<Box<GstRecording>> = None;
    let mut export_cancel: Option<Arc<AtomicBool>> = None;
    while let Ok(Some(command)) = read_command(&mut commands) {
        let keep_running = match command {
            HelperCommand::Probe => match missing_elements() {
                Ok(missing) => reply(
                    &writer,
                    HelperReply::Probe {
                        missing: missing.into_iter().map(str::to_owned).collect(),
                    },
                ),
                Err(error) => reply(&writer, HelperReply::ProbeFailed { error }),
            },
            HelperCommand::Still { node, timeout_ms } => {
                let frame = grab_still(
                    VideoInput::PipeWire {
                        fd: PIPE_WIRE_FD,
                        node,
                    },
                    Duration::from_millis(timeout_ms),
                );
                match frame {
                    Ok(frame) => reply_frame(&writer, frame),
                    Err(error) => reply(&writer, HelperReply::Failed { error }),
                }
            }
            HelperCommand::Start { node, plan } => match start(node, plan) {
                Ok(started) => {
                    recording = Some(started);
                    reply(&writer, HelperReply::Started)
                }
                Err(error) => reply(&writer, HelperReply::Failed { error }),
            },
            HelperCommand::Pause => match recording.as_mut() {
                Some(recording) => match recording.pause() {
                    Ok(()) => reply(&writer, HelperReply::Paused),
                    Err(error) => reply(&writer, HelperReply::Failed { error }),
                },
                None => reply(&writer, no_recording()),
            },
            HelperCommand::Resume => match recording.as_mut() {
                Some(recording) => match recording.resume() {
                    Ok(()) => reply(&writer, HelperReply::Resumed),
                    Err(error) => reply(&writer, HelperReply::Failed { error }),
                },
                None => reply(&writer, no_recording()),
            },
            HelperCommand::Poll { timeout_ms } => {
                let event = recording
                    .as_mut()
                    .and_then(|recording| recording.poll(Duration::from_millis(timeout_ms)));
                reply(&writer, HelperReply::Event { event })
            }
            HelperCommand::Finish => match recording.take() {
                Some(recording) => match recording.finish() {
                    Ok(()) => reply(&writer, HelperReply::Finished),
                    Err(error) => reply(&writer, HelperReply::Failed { error }),
                },
                None => reply(&writer, no_recording()),
            },
            HelperCommand::Duration { path } => match media_duration(&path) {
                Ok(duration) => reply(
                    &writer,
                    HelperReply::Duration {
                        millis: millis(duration),
                    },
                ),
                Err(error) => reply(&writer, HelperReply::Failed { error }),
            },
            HelperCommand::FrameAt { path, position_ms } => {
                match frame_at(&path, Duration::from_millis(position_ms)) {
                    Ok(frame) => reply_frame(&writer, frame),
                    Err(error) => reply(&writer, HelperReply::Failed { error }),
                }
            }
            HelperCommand::Export {
                source,
                start_ms,
                end_ms,
                output,
            } => {
                let cancel = Arc::new(AtomicBool::new(false));
                export_cancel = Some(Arc::clone(&cancel));
                let writer = Arc::clone(&writer);
                thread::spawn(move || {
                    let progress = |percent: f64| {
                        let _ = reply(&writer, HelperReply::Progress { percent });
                    };
                    let result = export_range(
                        &source,
                        Duration::from_millis(start_ms),
                        Duration::from_millis(end_ms),
                        &output,
                        &cancel,
                        &progress,
                    );
                    let response = match result {
                        Ok(()) => HelperReply::Exported,
                        Err(error) => HelperReply::Failed { error },
                    };
                    let _ = reply(&writer, response);
                });
                Ok(())
            }
            HelperCommand::CancelExport => {
                if let Some(cancel) = &export_cancel {
                    cancel.store(true, Ordering::Release);
                }
                Ok(())
            }
        };
        if keep_running.is_err() {
            break;
        }
    }
    if let Some(recording) = recording.take() {
        recording.abort();
    }
    ExitCode::SUCCESS
}

/// Takes the inherited PipeWire remote and starts the recorder with it.
fn start(
    node: u32,
    plan: kestrel_platform::recording::RecordingPlan,
) -> Result<Box<GstRecording>, kestrel_platform::recording::RecordingError> {
    // SAFETY: the parent dups its PipeWire remote to fd 3 before exec.
    let remote = unsafe { OwnedFd::from_raw_fd(PIPE_WIRE_FD) };
    let recording = GstRecording::start(
        VideoInput::PipeWire {
            fd: remote.as_raw_fd(),
            node,
        },
        &plan,
    )?;
    Ok(Box::new(recording.owning(remote)))
}

fn reply(writer: &Mutex<io::Stdout>, response: HelperReply) -> io::Result<()> {
    let mut stdout = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    write_line(&mut *stdout, &response)
}

fn reply_frame(
    writer: &Mutex<io::Stdout>,
    frame: kestrel_platform::recording::VideoFrame,
) -> io::Result<()> {
    let mut stdout = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    write_line(
        &mut *stdout,
        &HelperReply::Frame {
            width: frame.width,
            height: frame.height,
            bytes: u64::try_from(frame.pixels.len()).unwrap_or(u64::MAX),
        },
    )?;
    stdout.write_all(&frame.pixels)?;
    stdout.flush()
}

fn no_recording() -> HelperReply {
    HelperReply::Failed {
        error: kestrel_platform::recording::RecordingError::new(
            kestrel_platform::recording::RecordingErrorKind::Failed,
            "No recording is running.",
        ),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
