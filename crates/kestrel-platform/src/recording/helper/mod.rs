//! The recorder helper: a separate executable that owns GStreamer.
//!
//! The main binary starts it on demand and speaks the line protocol in
//! [`protocol`]. The helper inherits its PipeWire remote as fd 3 and is killed
//! and reaped when the recording ends; a helper whose parent exits sees EOF and
//! aborts on its own.

pub mod protocol;

use std::env;
use std::io::{self, BufReader, Read};
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    APPLICATION_AUDIO_ELEMENTS, AREA_ELEMENT, AUDIO_ELEMENTS, ActiveRecording, AudioTrack,
    RecordingError, RecordingErrorKind, RecordingEvent, RecordingPlan, RecordingSource,
    SystemAudioSource, VIDEO_ELEMENTS, VideoFrame,
};
use protocol::{
    HelperCommand, HelperReply, MAX_FRAME_BYTES, PIPE_WIRE_FD, frame_bytes, read_reply, write_line,
};

/// File name of the helper beside the running executable.
pub const HELPER_NAME: &str = "kestrel-record";
/// How long the helper may take to answer a probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Slack over a poll timeout before the helper counts as stalled.
const REPLY_GRACE: Duration = Duration::from_secs(1);
/// How long finishing a recording may take.
const FINISH_TIMEOUT: Duration = Duration::from_secs(20);
/// How often an export checks that it was cancelled.
const EXPORT_POLL: Duration = Duration::from_millis(100);

/// Why the helper cannot record, as the capability report reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderProblem {
    /// The executable is not in this installation.
    Missing,
    /// It exists but did not answer, usually because its libraries are absent.
    Failed,
    /// It answered that GStreamer could not start.
    Gstreamer(String),
}

impl RecorderProblem {
    pub fn message(&self) -> String {
        match self {
            Self::Missing => "The recorder helper is missing from this installation.".to_owned(),
            Self::Failed => {
                "The recorder helper could not start; GStreamer may be missing.".to_owned()
            }
            Self::Gstreamer(error) => format!("GStreamer could not start: {error}"),
        }
    }

    fn into_error(self) -> RecordingError {
        RecordingError::new(RecordingErrorKind::Unavailable, self.message())
    }
}

/// Runs the helper executable that sits beside `kestrel`.
#[derive(Debug, Clone)]
pub struct RecordingHelper {
    path: PathBuf,
}

impl RecordingHelper {
    /// The helper next to the running executable, for the AppImage, an
    /// installed copy, and a cargo build alike.
    pub fn discover() -> Self {
        let path = env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|directory| directory.join(HELPER_NAME)))
            .unwrap_or_else(|| PathBuf::from(HELPER_NAME));
        Self { path }
    }

    /// Uses `path` instead of the discovered executable.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the helper's GStreamer and lists the elements it lacks.
    pub fn probe(&self) -> Result<Vec<&'static str>, RecorderProblem> {
        let mut process = self.spawn(None)?;
        process
            .send(&HelperCommand::Probe)
            .map_err(|_| RecorderProblem::Failed)?;
        match process.reply_within(PROBE_TIMEOUT) {
            Ok(Some(HelperReply::Probe { missing })) => Ok(known_elements(&missing)),
            Ok(Some(HelperReply::ProbeFailed { error })) => Err(RecorderProblem::Gstreamer(error)),
            _ => Err(RecorderProblem::Failed),
        }
    }

    /// Why `source` with `audio` cannot be recorded here, checked before the
    /// desktop is asked to share anything.
    pub fn gstreamer_missing(
        &self,
        source: RecordingSource,
        audio: &[AudioTrack],
        system_audio: &SystemAudioSource,
    ) -> Option<RecordingError> {
        let missing = match self.probe() {
            Ok(missing) => missing,
            Err(problem) => return Some(problem.into_error()),
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

    /// One RGBA frame from `fd`'s PipeWire remote.
    pub fn still(
        &self,
        fd: RawFd,
        node: u32,
        timeout: Duration,
    ) -> Result<VideoFrame, RecordingError> {
        let mut process = self.spawn(Some(fd)).map_err(RecorderProblem::into_error)?;
        process.send(&HelperCommand::Still {
            node,
            timeout_ms: millis(timeout),
        })?;
        match process.reply_within(timeout + REPLY_GRACE) {
            Ok(Some(HelperReply::Frame {
                width,
                height,
                bytes,
            })) => process.read_picture(width, height, bytes),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    /// Starts a recording from `fd`'s PipeWire remote.
    pub fn start_recording(
        &self,
        fd: RawFd,
        node: u32,
        plan: &RecordingPlan,
    ) -> Result<HelperRecording, RecordingError> {
        let mut process = self.spawn(Some(fd)).map_err(RecorderProblem::into_error)?;
        process.send(&HelperCommand::Start {
            node,
            plan: plan.clone(),
        })?;
        match process.reply_within(PROBE_TIMEOUT) {
            Ok(Some(HelperReply::Started)) => Ok(HelperRecording { process }),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    /// The take's length.
    pub fn duration(&self, take: &Path) -> Result<Duration, RecordingError> {
        let mut process = self.spawn(None).map_err(RecorderProblem::into_error)?;
        process.send(&HelperCommand::Duration {
            path: take.to_path_buf(),
        })?;
        match process.reply_within(FINISH_TIMEOUT) {
            Ok(Some(HelperReply::Duration { millis })) => Ok(Duration::from_millis(millis)),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    /// One RGBA frame of a take at `position`.
    pub fn frame_at(&self, take: &Path, position: Duration) -> Result<VideoFrame, RecordingError> {
        let mut process = self.spawn(None).map_err(RecorderProblem::into_error)?;
        process.send(&HelperCommand::FrameAt {
            path: take.to_path_buf(),
            position_ms: millis(position),
        })?;
        match process.reply_within(FINISH_TIMEOUT) {
            Ok(Some(HelperReply::Frame {
                width,
                height,
                bytes,
            })) => process.read_picture(width, height, bytes),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    /// Writes `start..end` of `take` to `output`, reporting progress and
    /// stopping once `cancel` is set. The caller owns `output`.
    pub fn export(
        &self,
        take: &Path,
        start: Duration,
        end: Duration,
        output: &Path,
        cancel: &AtomicBool,
        progress: &dyn Fn(f64),
    ) -> Result<(), RecordingError> {
        let mut process = self.spawn(None).map_err(RecorderProblem::into_error)?;
        process.send(&HelperCommand::Export {
            source: take.to_path_buf(),
            start_ms: millis(start),
            end_ms: millis(end),
            output: output.to_path_buf(),
        })?;
        loop {
            if cancel.load(Ordering::Acquire) {
                let _ = process.send(&HelperCommand::CancelExport);
                return Err(RecordingError::new(
                    RecordingErrorKind::Cancelled,
                    "Exporting was cancelled.",
                ));
            }
            match process.reply_within(EXPORT_POLL) {
                Ok(None) => {}
                Ok(Some(HelperReply::Progress { percent })) => {
                    progress(percent.clamp(0.0, 1.0));
                }
                Ok(Some(HelperReply::Exported)) => return Ok(()),
                Ok(Some(HelperReply::Failed { error })) => return Err(error),
                _ => return Err(helper_stopped()),
            }
        }
    }

    fn spawn(&self, fd: Option<RawFd>) -> Result<HelperProcess, RecorderProblem> {
        let mut command = Command::new(&self.path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(fd) = fd {
            // SAFETY: `inherit_fd` only calls async-signal-safe functions and
            // returns their error without allocating.
            unsafe {
                command.pre_exec(move || inherit_fd(fd));
            }
        }
        let mut child = command.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => RecorderProblem::Missing,
            _ => RecorderProblem::Failed,
        })?;
        let stdin = child.stdin.take().ok_or(RecorderProblem::Failed)?;
        let stdout = child
            .stdout
            .take()
            .map(BufReader::new)
            .ok_or(RecorderProblem::Failed)?;
        Ok(HelperProcess {
            child,
            stdin,
            stdout,
        })
    }
}

impl Default for RecordingHelper {
    fn default() -> Self {
        Self::discover()
    }
}

/// A running recording, driven through the helper.
pub struct HelperRecording {
    process: HelperProcess,
}

impl ActiveRecording for HelperRecording {
    fn pause(&mut self) -> Result<(), RecordingError> {
        self.process.send(&HelperCommand::Pause)?;
        match self.process.reply_within(PROBE_TIMEOUT) {
            Ok(Some(HelperReply::Paused)) => Ok(()),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    fn resume(&mut self) -> Result<(), RecordingError> {
        self.process.send(&HelperCommand::Resume)?;
        match self.process.reply_within(PROBE_TIMEOUT) {
            Ok(Some(HelperReply::Resumed)) => Ok(()),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        }
    }

    fn poll(&mut self, timeout: Duration) -> Option<RecordingEvent> {
        if self
            .process
            .send(&HelperCommand::Poll {
                timeout_ms: millis(timeout),
            })
            .is_err()
        {
            return Some(RecordingEvent::Failed(helper_stopped()));
        }
        match self.process.reply_within(timeout + REPLY_GRACE) {
            Ok(Some(HelperReply::Event { event })) => event,
            Ok(Some(HelperReply::Failed { error })) => Some(RecordingEvent::Failed(error)),
            _ => Some(RecordingEvent::Failed(helper_stopped())),
        }
    }

    fn finish(mut self: Box<Self>) -> Result<(), RecordingError> {
        if self.process.send(&HelperCommand::Finish).is_err() {
            self.process.stop();
            return Err(helper_stopped());
        }
        let result = match self.process.reply_within(FINISH_TIMEOUT) {
            Ok(Some(HelperReply::Finished)) => Ok(()),
            Ok(Some(HelperReply::Failed { error })) => Err(error),
            _ => Err(helper_stopped()),
        };
        self.process.stop();
        result
    }

    fn abort(mut self: Box<Self>) {
        self.process.stop();
    }
}

/// A helper process and its protocol pipes; dropping it kills and reaps it.
struct HelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl HelperProcess {
    fn send(&mut self, command: &HelperCommand) -> Result<(), RecordingError> {
        write_line(&mut self.stdin, command).map_err(|_| helper_stopped())
    }

    /// The next reply, or `None` when none arrived within `timeout`.
    fn reply_within(&mut self, timeout: Duration) -> Result<Option<HelperReply>, RecordingError> {
        if self.stdout.buffer().is_empty()
            && !wait_readable(self.stdout.get_ref(), timeout).map_err(|_| helper_stopped())?
        {
            return Ok(None);
        }
        read_reply(&mut self.stdout).map_err(|_| helper_stopped())
    }

    fn read_picture(
        &mut self,
        width: u32,
        height: u32,
        bytes: u64,
    ) -> Result<VideoFrame, RecordingError> {
        let expected = frame_bytes(width, height).filter(|expected| {
            width > 0 && height > 0 && u64::try_from(*expected).ok() == Some(bytes)
        });
        let Some(bytes) = expected.filter(|bytes| *bytes <= MAX_FRAME_BYTES) else {
            return Err(RecordingError::new(
                RecordingErrorKind::Failed,
                "The recorder helper sent a malformed picture.",
            ));
        };
        let mut pixels = vec![0; bytes];
        self.stdout
            .read_exact(&mut pixels)
            .map_err(|_| helper_stopped())?;
        Ok(VideoFrame {
            width,
            height,
            pixels,
        })
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Hands `fd` to the helper as its well-known PipeWire fd.
fn inherit_fd(fd: RawFd) -> io::Result<()> {
    // SAFETY: dup2 and fcntl only touch the calling process's fd table.
    unsafe {
        if libc::dup2(fd, PIPE_WIRE_FD) < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(PIPE_WIRE_FD, libc::F_SETFD, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Whether `reader` has a byte ready within `timeout`, without consuming it.
fn wait_readable(reader: &ChildStdout, timeout: Duration) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut poll = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let milliseconds = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    loop {
        // SAFETY: `poll` reads and writes only the one `pollfd` passed to it.
        let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if ready > 0 {
            return Ok(true);
        }
        if ready == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Keeps only the element names this build knows about.
fn known_elements(missing: &[String]) -> Vec<&'static str> {
    VIDEO_ELEMENTS
        .iter()
        .chain([AREA_ELEMENT].iter())
        .chain(AUDIO_ELEMENTS.iter())
        .chain(APPLICATION_AUDIO_ELEMENTS.iter())
        .copied()
        .filter(|element| missing.iter().any(|name| name.as_str() == *element))
        .collect()
}

fn helper_stopped() -> RecordingError {
    RecordingError::new(RecordingErrorKind::Failed, "The recorder helper stopped.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_helper_is_reported_without_running_anything() {
        let helper = RecordingHelper::at("/nonexistent/kestrel-record");
        assert_eq!(helper.probe(), Err(RecorderProblem::Missing));
        assert_eq!(
            helper
                .still(0, 1, Duration::from_secs(1))
                .expect_err("missing helper")
                .kind,
            RecordingErrorKind::Unavailable
        );
    }

    #[test]
    fn problems_map_to_unavailable_recording_errors() {
        for problem in [
            RecorderProblem::Missing,
            RecorderProblem::Failed,
            RecorderProblem::Gstreamer("no registry".to_owned()),
        ] {
            let error = problem.clone().into_error();
            assert_eq!(error.kind, RecordingErrorKind::Unavailable);
            assert!(!error.message.contains('/'), "{}", error.message);
        }
        assert!(RecorderProblem::Missing.message().contains("helper"));
        assert!(
            RecorderProblem::Gstreamer("no registry".to_owned())
                .message()
                .contains("GStreamer")
        );
    }

    #[test]
    fn a_helper_that_exits_without_a_reply_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = directory.path().join("kestrel-record");
        std::fs::write(&path, "#!/bin/sh\nexit 127\n").expect("write helper");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make helper executable");
        let helper = RecordingHelper::at(path);
        assert_eq!(helper.probe(), Err(RecorderProblem::Failed));
        let error = helper
            .gstreamer_missing(RecordingSource::Screen, &[], &SystemAudioSource::Output)
            .expect("the helper cannot run");
        assert_eq!(error.kind, RecordingErrorKind::Unavailable);
        assert!(
            error.message.contains("GStreamer"),
            "the missing runtime is named: {}",
            error.message
        );
    }

    #[test]
    fn only_known_elements_are_reported() {
        assert_eq!(
            known_elements(&["vp8enc".to_owned(), "unknown".to_owned()]),
            vec!["vp8enc"]
        );
    }

    /// A stand-in helper that prints `lines` and then waits for EOF.
    fn scripted_helper(lines: &str) -> (tempfile::TempDir, RecordingHelper) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = directory.path().join("kestrel-record");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' {lines}\ncat >/dev/null\n"),
        )
        .expect("write helper");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make helper executable");
        (directory, RecordingHelper::at(path))
    }

    #[test]
    fn a_scripted_helper_drives_a_recording() {
        let args = "'{\"reply\":\"started\"}' \
                    '{\"reply\":\"paused\"}' \
                    '{\"reply\":\"resumed\"}' \
                    '{\"reply\":\"event\",\"event\":null}' \
                    '{\"reply\":\"finished\"}'";
        let (_directory, helper) = scripted_helper(args);
        let plan = RecordingPlan {
            output: PathBuf::from("/tmp/take.webm"),
            width: 64,
            height: 48,
            source_size: (64, 48),
            crop: None,
            framerate: 10,
            audio: Vec::new(),
            system_audio: SystemAudioSource::Output,
        };
        let mut recording = helper
            .start_recording(0, 1, &plan)
            .expect("the helper starts");
        recording.pause().expect("pause");
        recording.resume().expect("resume");
        assert_eq!(recording.poll(Duration::ZERO), None);
        Box::new(recording).finish().expect("finish");
    }

    #[test]
    fn export_reads_progress_and_cancels() {
        let args = "'{\"reply\":\"progress\",\"percent\":0.5}' \
                    '{\"reply\":\"exported\"}'";
        let (_directory, helper) = scripted_helper(args);
        let progress = std::sync::Mutex::new(Vec::new());
        helper
            .export(
                Path::new("/tmp/take.webm"),
                Duration::ZERO,
                Duration::from_secs(1),
                Path::new("/tmp/export.webm"),
                &AtomicBool::new(false),
                &|value| progress.lock().expect("progress").push(value),
            )
            .expect("export");
        assert_eq!(progress.lock().expect("progress").as_slice(), [0.5]);

        let (_directory, helper) = scripted_helper("'{\"reply\":\"exported\"}'");
        let error = helper
            .export(
                Path::new("/tmp/take.webm"),
                Duration::ZERO,
                Duration::from_secs(1),
                Path::new("/tmp/export.webm"),
                &AtomicBool::new(true),
                &|_| {},
            )
            .expect_err("cancelled");
        assert_eq!(error.kind, RecordingErrorKind::Cancelled);
    }
}
