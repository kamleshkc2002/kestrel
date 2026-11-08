//! Line-delimited recorder helper protocol.
//!
//! Every control message is one JSON object on its own line. A picture is a
//! `Frame` reply line followed by exactly the RGBA bytes it announces, so the
//! bytes never pass through JSON.

use std::io::{self, BufRead, Write};
use std::os::fd::RawFd;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::super::{RecordingError, RecordingEvent, RecordingPlan};

/// The fd a spawned helper reads its PipeWire remote from.
pub const PIPE_WIRE_FD: RawFd = 3;
/// Longest control line either side accepts.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Longest picture either side accepts, four bytes per pixel.
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// One request from Kestrel to the recorder helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HelperCommand {
    /// Loads GStreamer and lists the elements its registry lacks.
    Probe,
    /// Reads one RGBA frame from the inherited PipeWire remote.
    Still {
        node: u32,
        timeout_ms: u64,
    },
    /// Starts recording from the inherited PipeWire remote.
    Start {
        node: u32,
        plan: RecordingPlan,
    },
    Pause,
    Resume,
    /// Waits up to `timeout_ms` for the next recording event.
    Poll {
        timeout_ms: u64,
    },
    Finish,
    /// The take's length.
    Duration {
        path: PathBuf,
    },
    /// One RGBA frame of a take at `position_ms`.
    FrameAt {
        path: PathBuf,
        position_ms: u64,
    },
    /// Re-encodes `start_ms..end_ms` of `source` into `output`.
    Export {
        source: PathBuf,
        start_ms: u64,
        end_ms: u64,
        output: PathBuf,
    },
    CancelExport,
}

/// One answer from the recorder helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum HelperReply {
    /// The elements absent from the helper's GStreamer registry.
    Probe {
        missing: Vec<String>,
    },
    /// The helper's GStreamer could not start.
    ProbeFailed {
        error: String,
    },
    Started,
    Paused,
    Resumed,
    /// The next recording event, or `None` when none arrived in time.
    Event {
        event: Option<RecordingEvent>,
    },
    Finished,
    Duration {
        millis: u64,
    },
    /// `bytes` of RGBA pixels follow the line.
    Frame {
        width: u32,
        height: u32,
        bytes: u64,
    },
    Progress {
        percent: f64,
    },
    Exported,
    Failed {
        error: RecordingError,
    },
}

/// Bytes of RGBA pixels a frame of this size has.
pub fn frame_bytes(width: u32, height: u32) -> Option<usize> {
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    (width > 0 && height > 0 && bytes <= MAX_FRAME_BYTES).then_some(bytes)
}

/// Writes one message and flushes it.
pub fn write_line(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the message was too long",
        ));
    }
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads commands until the sender closes the stream.
pub fn read_command(reader: &mut impl BufRead) -> io::Result<Option<HelperCommand>> {
    read_message(reader)
}

/// Reads replies until the helper closes the stream.
pub fn read_reply(reader: &mut impl BufRead) -> io::Result<Option<HelperReply>> {
    read_message(reader)
}

fn read_message<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let Some(line) = read_line_bounded(reader, MAX_MESSAGE_BYTES)? else {
        return Ok(None);
    };
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Reads one line without ever buffering more than `limit` bytes.
pub fn read_line_bounded(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let (chunk, consumed) = match newline {
            Some(index) => (&buffer[..=index], index + 1),
            None => (buffer, buffer.len()),
        };
        if line.len() + chunk.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the message was too long",
            ));
        }
        line.extend_from_slice(chunk);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;
    use crate::recording::{AudioTrack, CropRegion, RecordingErrorKind, SystemAudioSource};

    fn round_trip(command: HelperCommand) {
        let mut encoded = Vec::new();
        write_line(&mut encoded, &command).expect("encode");
        let mut reader = BufReader::new(Cursor::new(encoded));
        assert_eq!(read_command(&mut reader).expect("decode"), Some(command));
        assert_eq!(read_command(&mut reader).expect("end"), None);
    }

    fn round_trip_reply(reply: HelperReply) {
        let mut encoded = Vec::new();
        write_line(&mut encoded, &reply).expect("encode");
        let mut reader = BufReader::new(Cursor::new(encoded));
        assert_eq!(read_reply(&mut reader).expect("decode"), Some(reply));
        assert_eq!(read_reply(&mut reader).expect("end"), None);
    }

    fn plan() -> RecordingPlan {
        RecordingPlan {
            output: PathBuf::from("/tmp/take.webm"),
            width: 640,
            height: 480,
            source_size: (1280, 960),
            crop: Some(CropRegion {
                x: 8,
                y: 16,
                width: 320,
                height: 240,
            }),
            framerate: 30,
            audio: vec![AudioTrack::System, AudioTrack::Microphone],
            system_audio: SystemAudioSource::Application {
                identity: "firefox".to_owned(),
            },
        }
    }

    #[test]
    fn every_command_survives_a_line_round_trip() {
        for command in [
            HelperCommand::Probe,
            HelperCommand::Still {
                node: 42,
                timeout_ms: 5_000,
            },
            HelperCommand::Start {
                node: 42,
                plan: plan(),
            },
            HelperCommand::Pause,
            HelperCommand::Resume,
            HelperCommand::Poll { timeout_ms: 100 },
            HelperCommand::Finish,
            HelperCommand::Duration {
                path: PathBuf::from("/tmp/take.webm"),
            },
            HelperCommand::FrameAt {
                path: PathBuf::from("/tmp/take.webm"),
                position_ms: 1_500,
            },
            HelperCommand::Export {
                source: PathBuf::from("/tmp/take.webm"),
                start_ms: 500,
                end_ms: 2_500,
                output: PathBuf::from("/tmp/trimmed.webm"),
            },
            HelperCommand::CancelExport,
        ] {
            round_trip(command);
        }
    }

    #[test]
    fn every_reply_survives_a_line_round_trip() {
        for reply in [
            HelperReply::Probe {
                missing: vec!["vp8enc".to_owned()],
            },
            HelperReply::ProbeFailed {
                error: "no registry".to_owned(),
            },
            HelperReply::Started,
            HelperReply::Paused,
            HelperReply::Resumed,
            HelperReply::Event { event: None },
            HelperReply::Event {
                event: Some(RecordingEvent::ApplicationAudio { playing: true }),
            },
            HelperReply::Finished,
            HelperReply::Duration { millis: 2_500 },
            HelperReply::Frame {
                width: 16,
                height: 8,
                bytes: 512,
            },
            HelperReply::Progress { percent: 0.5 },
            HelperReply::Exported,
            HelperReply::Failed {
                error: RecordingError::new(
                    RecordingErrorKind::Storage,
                    "Writing the recording failed.",
                ),
            },
        ] {
            round_trip_reply(reply);
        }
    }

    #[test]
    fn failures_keep_their_kind_and_message() {
        let error =
            RecordingError::new(RecordingErrorKind::Storage, "Writing the recording failed.");
        let mut encoded = Vec::new();
        write_line(
            &mut encoded,
            &HelperReply::Failed {
                error: error.clone(),
            },
        )
        .expect("encode");
        let mut reader = BufReader::new(Cursor::new(encoded));
        let Some(HelperReply::Failed { error: decoded }) = read_reply(&mut reader).expect("decode")
        else {
            panic!("a failed reply decodes as Failed");
        };
        assert_eq!(decoded, error);
        assert_eq!(decoded.kind, RecordingErrorKind::Storage);
    }

    #[test]
    fn lines_split_across_reads_are_reassembled() {
        let mut reader =
            BufReader::with_capacity(1, Cursor::new(b"{\"command\":\"pause\"}\n".to_vec()));
        assert_eq!(
            read_command(&mut reader).expect("decode"),
            Some(HelperCommand::Pause)
        );
        assert_eq!(read_command(&mut reader).expect("end"), None);
    }

    #[test]
    fn an_oversized_line_is_refused_instead_of_buffered() {
        let encoded = vec![b'a'; MAX_MESSAGE_BYTES + 2];
        let mut reader = BufReader::new(Cursor::new(encoded));
        assert!(read_command(&mut reader).is_err());
    }

    #[test]
    fn frame_sizes_are_bounded() {
        assert_eq!(frame_bytes(16, 8), Some(512));
        assert_eq!(frame_bytes(0, 8), None);
        assert_eq!(frame_bytes(u32::MAX, 2), None);
    }
}
