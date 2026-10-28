//! Screen recording sessions: consent, optional area choice, recording with
//! pause/resume, and failure-safe takes.
//!
//! One session runs at a time on the `kestrel-recording` worker. A take is
//! written to a hidden `.partial` file and renamed only once the recorder
//! completes it; cancelled and failed sessions remove the partial file, and
//! leftovers from a crash are removed when the service starts.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use kestrel_platform::capture::CaptureCancel;
use kestrel_platform::recording::{
    AudioTrack, CaptureSession, CropRegion, RecordingBackend, RecordingError, RecordingErrorKind,
    RecordingEvent, RecordingPlan, RecordingSource, SystemAudioSource, TakeEditor, available_space,
};
use kestrel_platform::snippets::SystemClock;

use crate::capture::image::{Rect, RgbaImage};

const TAKE_PREFIX: &str = "Kestrel-recording-";
const TAKE_EXTENSION: &str = "webm";
const PARTIAL_SUFFIX: &str = ".partial";
/// Maximum listed takes, newest first.
pub const MAX_LISTED_TAKES: usize = 10;
/// Recording does not start, and stops, below this many free bytes.
pub const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(100);
const SPACE_CHECK: Duration = Duration::from_secs(1);
/// Minimum recording area side in pixels.
pub const MIN_AREA_SIDE: u32 = 16;
/// Age after which an untouched partial file is stale.
const ABANDONED_PARTIAL_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingOptions {
    pub source: RecordingSource,
    pub cursor: bool,
    pub framerate: u32,
    pub audio: Vec<AudioTrack>,
    pub system_audio: SystemAudioSource,
}

/// Elapsed time excluding pauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordingClock {
    accumulated: Duration,
    running_since: Option<Instant>,
}

impl RecordingClock {
    fn started() -> Self {
        Self {
            accumulated: Duration::ZERO,
            running_since: Some(Instant::now()),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.accumulated
            + self
                .running_since
                .map_or(Duration::ZERO, |since| since.elapsed())
    }

    fn pause(&mut self) {
        if let Some(since) = self.running_since.take() {
            self.accumulated += since.elapsed();
        }
    }

    fn resume(&mut self) {
        if self.running_since.is_none() {
            self.running_since = Some(Instant::now());
        }
    }
}

/// One application's playback feeding the System track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTrackState {
    pub identity: String,
    pub playing: bool,
}

/// Audio track state and failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackState {
    pub track: AudioTrack,
    pub error: Option<RecordingError>,
    /// Set for the System track while it records one application.
    pub application: Option<ApplicationTrackState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    /// Set for takes recorded in this run.
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RecordingPhase {
    #[default]
    Idle,
    /// Desktop sharing request is open.
    Requesting {
        source: RecordingSource,
    },
    /// Waiting for area selection.
    ChoosingArea {
        frame: Arc<RgbaImage>,
    },
    Recording {
        source: RecordingSource,
        tracks: Vec<TrackState>,
        paused: bool,
        clock: RecordingClock,
    },
    Stopping,
    Completed {
        take: Take,
        /// Early-stop reason.
        note: Option<RecordingError>,
    },
    Failed {
        error: RecordingError,
    },
}

impl RecordingPhase {
    /// Whether a session holds the worker.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Requesting { .. }
                | Self::ChoosingArea { .. }
                | Self::Recording { .. }
                | Self::Stopping
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExportPhase {
    #[default]
    Idle,
    Exporting {
        name: String,
        percent: u8,
    },
    Exported {
        path: PathBuf,
    },
    Failed {
        error: RecordingError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingSnapshot {
    pub phase: RecordingPhase,
    /// Increments on phase, export, and take-list changes.
    pub generation: u64,
    pub takes: Vec<Take>,
    pub export: ExportPhase,
}

enum Command {
    Area(Option<CropRegion>),
    Pause,
    Resume,
    Stop,
    Cancel,
}

struct Shared {
    snapshot: Mutex<RecordingSnapshot>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut RecordingSnapshot)) {
        let mut snapshot = lock(&self.snapshot);
        change(&mut snapshot);
        snapshot.generation = snapshot.generation.wrapping_add(1);
    }

    fn set_phase(&self, phase: RecordingPhase) {
        self.update(|snapshot| snapshot.phase = phase);
    }

    fn phase(&self) -> RecordingPhase {
        lock(&self.snapshot).phase.clone()
    }
}

pub struct RecordingService {
    backend: Arc<dyn RecordingBackend>,
    editor: Arc<dyn TakeEditor>,
    folder: PathBuf,
    shared: Arc<Shared>,
    commands: Option<Sender<Command>>,
    cancel: CaptureCancel,
    worker: Option<JoinHandle<()>>,
    export_cancel: Arc<AtomicBool>,
    export_worker: Option<JoinHandle<()>>,
}

impl RecordingService {
    /// Removes stale partial files and loads takes from `folder`.
    pub fn new(
        backend: Arc<dyn RecordingBackend>,
        editor: Arc<dyn TakeEditor>,
        folder: PathBuf,
    ) -> Self {
        remove_partials(&folder);
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(RecordingSnapshot {
                takes: list_takes(&folder),
                ..RecordingSnapshot::default()
            }),
        });
        Self {
            backend,
            editor,
            folder,
            shared,
            commands: None,
            cancel: CaptureCancel::new(),
            worker: None,
            export_cancel: Arc::default(),
            export_worker: None,
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn snapshot(&self) -> RecordingSnapshot {
        lock(&self.shared.snapshot).clone()
    }

    /// Asks the desktop to share `options.source` and records once it agrees.
    pub fn start(&mut self, options: RecordingOptions) -> Result<(), String> {
        if self.shared.phase().is_active() {
            return Err("A recording is already in progress.".to_owned());
        }
        self.join_worker();
        if let Err(error) = fs::create_dir_all(&self.folder) {
            return Err(format!("Creating the recordings folder failed: {error}"));
        }
        let (sender, receiver) = mpsc::channel();
        let cancel = CaptureCancel::new();
        self.shared.set_phase(RecordingPhase::Requesting {
            source: options.source,
        });
        let backend = Arc::clone(&self.backend);
        let shared = Arc::clone(&self.shared);
        let folder = self.folder.clone();
        let worker_cancel = cancel.clone();
        let worker = thread::Builder::new()
            .name("kestrel-recording".to_owned())
            .spawn(move || {
                let session = Session {
                    backend: backend.as_ref(),
                    shared: &shared,
                    folder: &folder,
                    commands: &receiver,
                    cancel: &worker_cancel,
                };
                session.run(&options);
            });
        match worker {
            Ok(worker) => {
                self.worker = Some(worker);
                self.commands = Some(sender);
                self.cancel = cancel;
                Ok(())
            }
            Err(error) => {
                self.shared.set_phase(RecordingPhase::Idle);
                Err(format!("Starting the recording worker failed: {error}"))
            }
        }
    }

    /// Answers `ChoosingArea`; `None` cancels the session.
    pub fn choose_area(&self, region: Option<Rect>) -> Result<(), String> {
        let RecordingPhase::ChoosingArea { frame } = self.shared.phase() else {
            return Err("No area is being chosen.".to_owned());
        };
        let crop = region.and_then(|region| crop_region(region, frame.width, frame.height));
        if region.is_some() && crop.is_none() {
            return Err(format!(
                "Choose an area of at least {MIN_AREA_SIDE} × {MIN_AREA_SIDE} pixels."
            ));
        }
        self.send(if region.is_some() {
            Command::Area(crop)
        } else {
            Command::Cancel
        })
    }

    /// Records the whole frame from `ChoosingArea`.
    pub fn record_whole_frame(&self) -> Result<(), String> {
        if !matches!(self.shared.phase(), RecordingPhase::ChoosingArea { .. }) {
            return Err("No area is being chosen.".to_owned());
        }
        self.send(Command::Area(None))
    }

    pub fn pause(&self) -> Result<(), String> {
        self.recording_command(Command::Pause)
    }

    pub fn resume(&self) -> Result<(), String> {
        self.recording_command(Command::Resume)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.recording_command(Command::Stop)
    }

    /// Ends the session and discards anything recorded.
    pub fn cancel(&self) -> Result<(), String> {
        if !self.shared.phase().is_active() {
            return Err("No recording is in progress.".to_owned());
        }
        self.cancel.cancel();
        self.send(Command::Cancel)
    }

    /// Clears a finished or failed outcome and rereads the take list.
    pub fn dismiss(&self) {
        let phase = self.shared.phase();
        if !phase.is_active() {
            let takes = list_takes(&self.folder);
            self.shared.update(|snapshot| {
                snapshot.phase = RecordingPhase::Idle;
                snapshot.takes = takes;
            });
        }
    }

    pub fn delete_take(&self, path: &Path) -> Result<(), String> {
        self.listed(path)?;
        if matches!(
            lock(&self.shared.snapshot).export,
            ExportPhase::Exporting { .. }
        ) {
            return Err("Wait for the export to finish before deleting recordings.".to_owned());
        }
        fs::remove_file(path).map_err(|error| format!("Removing the recording failed: {error}"))?;
        let takes = list_takes(&self.folder);
        self.shared.update(|snapshot| {
            if matches!(&snapshot.phase, RecordingPhase::Completed { take, .. } if take.path == path) {
                snapshot.phase = RecordingPhase::Idle;
            }
            snapshot.takes = takes;
        });
        Ok(())
    }

    fn listed(&self, path: &Path) -> Result<(), String> {
        lock(&self.shared.snapshot)
            .takes
            .iter()
            .any(|take| take.path == path)
            .then_some(())
            .ok_or_else(|| "That recording is not in the list.".to_owned())
    }

    /// Length of a listed take, for the trim timeline.
    pub fn take_duration(&self, path: &Path) -> Result<Duration, String> {
        self.listed(path)?;
        self.editor.duration(path).map_err(|error| error.message)
    }

    /// Writes `start..end` of a listed take to `destination` on the
    /// `kestrel-recording-export` worker, through a hidden partial file
    /// renamed once complete. The take itself is never modified.
    pub fn export(
        &mut self,
        take: &Path,
        start: Duration,
        end: Duration,
        destination: PathBuf,
    ) -> Result<(), String> {
        self.listed(take)?;
        if matches!(
            lock(&self.shared.snapshot).export,
            ExportPhase::Exporting { .. }
        ) {
            return Err("An export is already running.".to_owned());
        }
        if end <= start {
            return Err("Choose an end after the start.".to_owned());
        }
        if destination == take {
            return Err("Export to a new file; the recording itself is kept.".to_owned());
        }
        self.join_export();
        let partial = export_partial(&destination)
            .map_err(|error| format!("Creating the export file failed: {error}"))?;
        let unstarted = partial.clone();
        let name = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.shared.update(|snapshot| {
            snapshot.export = ExportPhase::Exporting {
                name: name.clone(),
                percent: 0,
            };
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let editor = Arc::clone(&self.editor);
        let shared = Arc::clone(&self.shared);
        let worker_cancel = Arc::clone(&cancel);
        let take = take.to_path_buf();
        let worker = thread::Builder::new()
            .name("kestrel-recording-export".to_owned())
            .spawn(move || {
                let progress = |fraction: f64| {
                    let percent = (fraction * 100.0).round().clamp(0.0, 100.0) as u8;
                    shared.update(|snapshot| {
                        if let ExportPhase::Exporting { percent: shown, .. } = &mut snapshot.export
                        {
                            *shown = percent;
                        }
                    });
                };
                let outcome = editor
                    .export(&take, start, end, &partial, &worker_cancel, &progress)
                    .and_then(|()| {
                        fs::rename(&partial, &destination).map_err(|error| {
                            RecordingError::new(
                                RecordingErrorKind::Storage,
                                format!("Saving the export failed: {error}"),
                            )
                        })
                    });
                let export = match outcome {
                    Ok(()) => ExportPhase::Exported { path: destination },
                    Err(error) => {
                        let _ = fs::remove_file(&partial);
                        if error.kind == RecordingErrorKind::Cancelled {
                            ExportPhase::Idle
                        } else {
                            ExportPhase::Failed { error }
                        }
                    }
                };
                shared.update(|snapshot| snapshot.export = export);
            });
        match worker {
            Ok(worker) => {
                self.export_worker = Some(worker);
                self.export_cancel = cancel;
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(&unstarted);
                self.shared
                    .update(|snapshot| snapshot.export = ExportPhase::Idle);
                Err(format!("Starting the export worker failed: {error}"))
            }
        }
    }

    /// Stops the running export, removing its partial file.
    pub fn cancel_export(&mut self) {
        self.export_cancel.store(true, Ordering::Release);
        self.join_export();
    }

    pub fn dismiss_export(&self) {
        self.shared.update(|snapshot| {
            if !matches!(snapshot.export, ExportPhase::Exporting { .. }) {
                snapshot.export = ExportPhase::Idle;
            }
        });
    }

    fn join_export(&mut self) {
        if let Some(worker) = self.export_worker.take() {
            let _ = worker.join();
        }
    }

    /// Completes a running take or abandons a pending session, cancels export, then joins workers.
    pub fn shutdown(&mut self) {
        match self.shared.phase() {
            RecordingPhase::Requesting { .. } | RecordingPhase::ChoosingArea { .. } => {
                self.cancel.cancel();
                let _ = self.send(Command::Cancel);
            }
            RecordingPhase::Recording { .. } => {
                let _ = self.send(Command::Stop);
            }
            _ => {}
        }
        self.cancel_export();
        self.join_worker();
    }

    fn recording_command(&self, command: Command) -> Result<(), String> {
        if !matches!(self.shared.phase(), RecordingPhase::Recording { .. }) {
            return Err("No recording is in progress.".to_owned());
        }
        self.send(command)
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .as_ref()
            .and_then(|commands| commands.send(command).ok())
            .ok_or_else(|| "The recording worker stopped.".to_owned())
    }

    fn join_worker(&mut self) {
        self.commands = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for RecordingService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The part of `region` inside the frame, with even sides for the encoder.
fn crop_region(region: Rect, width: u32, height: u32) -> Option<CropRegion> {
    let left = region.x.max(0) as u32;
    let top = region.y.max(0) as u32;
    let right = (i64::from(region.x) + i64::from(region.width)).clamp(0, i64::from(width)) as u32;
    let bottom =
        (i64::from(region.y) + i64::from(region.height)).clamp(0, i64::from(height)) as u32;
    let crop_width = right.saturating_sub(left) & !1;
    let crop_height = bottom.saturating_sub(top) & !1;
    (crop_width >= MIN_AREA_SIDE && crop_height >= MIN_AREA_SIDE).then_some(CropRegion {
        x: left,
        y: top,
        width: crop_width,
        height: crop_height,
    })
}

enum Outcome {
    Finish(Option<RecordingError>),
    Discard,
    Fail(RecordingError),
}

struct Session<'a> {
    backend: &'a dyn RecordingBackend,
    shared: &'a Shared,
    folder: &'a Path,
    commands: &'a Receiver<Command>,
    cancel: &'a CaptureCancel,
}

impl Session<'_> {
    fn run(&self, options: &RecordingOptions) {
        let mut source = match self
            .backend
            .open(options.source, options.cursor, self.cancel)
        {
            Ok(source) => source,
            Err(error) => return self.end_without_take(error),
        };
        let frame = match source.still() {
            Ok(frame) => frame,
            Err(error) => return self.end_without_take(error),
        };
        let crop = if options.source == RecordingSource::Area {
            match self.choose_area(source.as_ref(), &frame) {
                Ok(crop) => crop,
                Err(error) => return self.end_without_take(error),
            }
        } else {
            None
        };
        if available_space(self.folder).is_some_and(|free| free < MIN_FREE_BYTES) {
            return self.end_without_take(low_space());
        }
        let (width, height) = crop.map_or((frame.width, frame.height), |crop| {
            (crop.width, crop.height)
        });
        let partial = match create_partial(self.folder) {
            Ok(partial) => partial,
            Err(error) => {
                return self.end_without_take(RecordingError::new(
                    RecordingErrorKind::Storage,
                    format!("Creating the recording file failed: {error}"),
                ));
            }
        };
        let plan = RecordingPlan {
            output: partial.clone(),
            width: (width & !1).max(2),
            height: (height & !1).max(2),
            source_size: (frame.width, frame.height),
            crop,
            framerate: options.framerate,
            audio: options.audio.clone(),
            system_audio: options.system_audio.clone(),
        };
        drop(frame);
        let mut recording = match source.record(&plan) {
            Ok(recording) => recording,
            Err(error) => {
                let _ = fs::remove_file(&partial);
                return self.end_without_take(error);
            }
        };
        let mut clock = RecordingClock::started();
        let mut tracks: Vec<TrackState> = options
            .audio
            .iter()
            .map(|track| TrackState {
                track: *track,
                error: None,
                application: match (&options.system_audio, track) {
                    (SystemAudioSource::Application { identity }, AudioTrack::System) => {
                        Some(ApplicationTrackState {
                            identity: identity.clone(),
                            playing: false,
                        })
                    }
                    _ => None,
                },
            })
            .collect();
        let mut paused = false;
        let publish = |tracks: &[TrackState], paused: bool, clock: RecordingClock| {
            self.shared.set_phase(RecordingPhase::Recording {
                source: options.source,
                tracks: tracks.to_vec(),
                paused,
                clock,
            });
        };
        publish(&tracks, paused, clock);
        let mut last_space_check = Instant::now();
        let outcome = loop {
            while let Some(event) = recording.poll(Duration::ZERO) {
                match event {
                    RecordingEvent::TrackFailed { track, error } => {
                        if let Some(state) = tracks.iter_mut().find(|state| state.track == track) {
                            state.error = Some(error);
                        }
                        publish(&tracks, paused, clock);
                    }
                    RecordingEvent::ApplicationAudio { playing } => {
                        if let Some(state) = tracks
                            .iter_mut()
                            .find(|state| state.track == AudioTrack::System)
                        {
                            if let Some(application) = &mut state.application {
                                application.playing = playing;
                            }
                        }
                        publish(&tracks, paused, clock);
                    }
                    RecordingEvent::SourceEnded => {}
                    RecordingEvent::Failed(error) => {
                        recording.abort();
                        let _ = fs::remove_file(&partial);
                        return self.end_without_take(error);
                    }
                }
            }
            if source.ended_by_desktop() {
                break Outcome::Finish(Some(RecordingError::new(
                    RecordingErrorKind::Revoked,
                    "The desktop stopped sharing the screen; the take was saved.",
                )));
            }
            if last_space_check.elapsed() >= SPACE_CHECK {
                last_space_check = Instant::now();
                if available_space(self.folder).is_some_and(|free| free < MIN_FREE_BYTES) {
                    break Outcome::Finish(Some(low_space()));
                }
            }
            match self.commands.recv_timeout(POLL) {
                Ok(Command::Pause) if !paused => match recording.pause() {
                    Ok(()) => {
                        paused = true;
                        clock.pause();
                        publish(&tracks, paused, clock);
                    }
                    Err(error) => break Outcome::Fail(error),
                },
                Ok(Command::Resume) if paused => match recording.resume() {
                    Ok(()) => {
                        paused = false;
                        clock.resume();
                        publish(&tracks, paused, clock);
                    }
                    Err(error) => break Outcome::Fail(error),
                },
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    break Outcome::Finish(None);
                }
                Ok(Command::Cancel) => break Outcome::Discard,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            }
        };
        clock.pause();
        match outcome {
            Outcome::Finish(note) => {
                self.shared.set_phase(RecordingPhase::Stopping);
                match recording.finish() {
                    Ok(()) => self.complete(&partial, clock.elapsed(), note),
                    Err(error) => {
                        let _ = fs::remove_file(&partial);
                        self.end_without_take(error);
                    }
                }
            }
            Outcome::Discard => {
                recording.abort();
                let _ = fs::remove_file(&partial);
                self.shared.set_phase(RecordingPhase::Idle);
            }
            Outcome::Fail(error) => {
                recording.abort();
                let _ = fs::remove_file(&partial);
                self.end_without_take(error);
            }
        }
        // `source` drops after the recorder, which ends the desktop session.
        drop(source);
    }

    fn choose_area(
        &self,
        source: &dyn CaptureSession,
        frame: &kestrel_platform::recording::VideoFrame,
    ) -> Result<Option<CropRegion>, RecordingError> {
        let image =
            RgbaImage::new(frame.width, frame.height, frame.pixels.clone()).map_err(|_| {
                RecordingError::new(
                    RecordingErrorKind::Failed,
                    "The shared picture is too large.",
                )
            })?;
        self.shared.set_phase(RecordingPhase::ChoosingArea {
            frame: Arc::new(image),
        });
        loop {
            if source.ended_by_desktop() {
                return Err(RecordingError::new(
                    RecordingErrorKind::Revoked,
                    "The desktop stopped sharing the screen.",
                ));
            }
            match self.commands.recv_timeout(POLL) {
                Ok(Command::Area(crop)) => return Ok(crop),
                Ok(Command::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(RecordingError::new(
                        RecordingErrorKind::Cancelled,
                        "The recording was cancelled.",
                    ));
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn complete(&self, partial: &Path, duration: Duration, note: Option<RecordingError>) {
        let finished = final_path(self.folder).and_then(|path| {
            fs::rename(partial, &path)?;
            Ok(path)
        });
        let path = match finished {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_file(partial);
                return self.end_without_take(RecordingError::new(
                    RecordingErrorKind::Storage,
                    format!("Saving the recording failed: {error}"),
                ));
            }
        };
        let take = Take {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            bytes: fs::metadata(&path).map_or(0, |metadata| metadata.len()),
            duration: Some(duration),
            path,
        };
        let takes = list_takes(self.folder);
        self.shared.update(|snapshot| {
            snapshot.phase = RecordingPhase::Completed { take, note };
            snapshot.takes = takes;
        });
    }

    /// A session that ends with nothing saved; the user cancelling is quiet.
    fn end_without_take(&self, error: RecordingError) {
        self.shared
            .set_phase(if error.kind == RecordingErrorKind::Cancelled {
                RecordingPhase::Idle
            } else {
                RecordingPhase::Failed { error }
            });
    }
}

fn low_space() -> RecordingError {
    RecordingError::new(
        RecordingErrorKind::Storage,
        "The disk is nearly full, so recording stopped; the take was saved.",
    )
}

fn take_stem() -> String {
    let now = SystemClock.local_time();
    format!(
        "{TAKE_PREFIX}{:04}-{:02}-{:02}-{:02}-{:02}-{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    )
}

/// A fresh hidden partial file beside `destination`.
fn export_partial(destination: &Path) -> io::Result<PathBuf> {
    let folder = destination
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no destination folder"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no destination name"))?
        .to_string_lossy()
        .into_owned();
    for attempt in 0..100 {
        let path = folder.join(if attempt == 0 {
            format!(".{name}{PARTIAL_SUFFIX}")
        } else {
            format!(".{name}-{attempt}{PARTIAL_SUFFIX}")
        });
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name",
    ))
}

/// A fresh hidden partial file, private to the user.
fn create_partial(folder: &Path) -> io::Result<PathBuf> {
    let stem = take_stem();
    for attempt in 0..100 {
        let name = if attempt == 0 {
            format!(".{stem}.{TAKE_EXTENSION}{PARTIAL_SUFFIX}")
        } else {
            format!(".{stem}-{attempt}.{TAKE_EXTENSION}{PARTIAL_SUFFIX}")
        };
        let path = folder.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name",
    ))
}

/// A take name not yet used in `folder`.
fn final_path(folder: &Path) -> io::Result<PathBuf> {
    let stem = take_stem();
    (0..100)
        .map(|attempt| {
            let name = if attempt == 0 {
                format!("{stem}.{TAKE_EXTENSION}")
            } else {
                format!("{stem}-{attempt}.{TAKE_EXTENSION}")
            };
            folder.join(name)
        })
        .find(|path| !path.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "no free file name"))
}

fn is_partial(name: &str) -> bool {
    name.starts_with(&format!(".{TAKE_PREFIX}")) && name.ends_with(PARTIAL_SUFFIX)
}

fn is_take(name: &str) -> bool {
    name.starts_with(TAKE_PREFIX) && name.ends_with(&format!(".{TAKE_EXTENSION}"))
}

/// Removes stale partial takes; active recordings keep files fresh.
fn remove_partials(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_none_or(|age| age >= ABANDONED_PARTIAL_AGE);
        if stale && entry.file_name().to_str().is_some_and(is_partial) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Kestrel takes in `folder`, newest first.
fn list_takes(folder: &Path) -> Vec<Take> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut takes: Vec<(SystemTime, Take)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_str()
                .filter(|name| is_take(name))?
                .to_owned();
            let metadata = entry.metadata().ok().filter(fs::Metadata::is_file)?;
            Some((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                Take {
                    path: entry.path(),
                    name,
                    bytes: metadata.len(),
                    duration: None,
                },
            ))
        })
        .collect();
    takes.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.name.cmp(&left.1.name))
    });
    takes
        .into_iter()
        .take(MAX_LISTED_TAKES)
        .map(|(_, take)| take)
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_platform::recording::{ActiveRecording, VideoFrame};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct Script {
        open_error: Option<RecordingErrorKind>,
        /// Events the recorder reports once recording starts.
        events: Vec<RecordingEvent>,
        finish_error: bool,
    }

    #[derive(Default)]
    struct Log {
        plans: Vec<RecordingPlan>,
        calls: Vec<&'static str>,
        sessions_open: usize,
    }

    #[derive(Clone)]
    struct FakeBackend {
        script: Script,
        log: Arc<Mutex<Log>>,
        revoked: Arc<AtomicBool>,
    }

    impl FakeBackend {
        fn new(script: Script) -> Self {
            Self {
                script,
                log: Arc::default(),
                revoked: Arc::default(),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            lock(&self.log).calls.clone()
        }
    }

    struct FakeSession {
        backend: FakeBackend,
    }

    struct FakeRecording {
        backend: FakeBackend,
        output: PathBuf,
        events: Vec<RecordingEvent>,
    }

    impl RecordingBackend for FakeBackend {
        fn open(
            &self,
            _source: RecordingSource,
            _cursor: bool,
            cancel: &CaptureCancel,
        ) -> Result<Box<dyn CaptureSession>, RecordingError> {
            if let Some(kind) = self.script.open_error {
                return Err(RecordingError::new(kind, "fake refusal"));
            }
            if cancel.is_cancelled() {
                return Err(RecordingError::new(
                    RecordingErrorKind::Cancelled,
                    "cancelled",
                ));
            }
            lock(&self.log).sessions_open += 1;
            Ok(Box::new(FakeSession {
                backend: self.clone(),
            }))
        }
    }

    impl CaptureSession for FakeSession {
        fn still(&mut self) -> Result<VideoFrame, RecordingError> {
            Ok(VideoFrame {
                width: 101,
                height: 61,
                pixels: vec![0; 101 * 61 * 4],
            })
        }

        fn record(
            &mut self,
            plan: &RecordingPlan,
        ) -> Result<Box<dyn ActiveRecording>, RecordingError> {
            assert!(
                plan.output.exists(),
                "the partial file exists before recording"
            );
            fs::write(&plan.output, b"media").expect("write partial");
            lock(&self.backend.log).plans.push(plan.clone());
            Ok(Box::new(FakeRecording {
                backend: self.backend.clone(),
                output: plan.output.clone(),
                events: self.backend.script.events.clone(),
            }))
        }

        fn ended_by_desktop(&self) -> bool {
            self.backend.revoked.load(Ordering::Acquire)
        }
    }

    impl Drop for FakeSession {
        fn drop(&mut self) {
            lock(&self.backend.log).sessions_open -= 1;
        }
    }

    impl ActiveRecording for FakeRecording {
        fn pause(&mut self) -> Result<(), RecordingError> {
            lock(&self.backend.log).calls.push("pause");
            Ok(())
        }

        fn resume(&mut self) -> Result<(), RecordingError> {
            lock(&self.backend.log).calls.push("resume");
            Ok(())
        }

        fn poll(&mut self, _timeout: Duration) -> Option<RecordingEvent> {
            (!self.events.is_empty()).then(|| self.events.remove(0))
        }

        fn finish(self: Box<Self>) -> Result<(), RecordingError> {
            lock(&self.backend.log).calls.push("finish");
            if self.backend.script.finish_error {
                return Err(RecordingError::new(
                    RecordingErrorKind::Storage,
                    "disk full",
                ));
            }
            fs::write(&self.output, b"complete media").expect("complete");
            Ok(())
        }

        fn abort(self: Box<Self>) {
            lock(&self.backend.log).calls.push("abort");
        }
    }

    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    enum Exporting {
        #[default]
        Succeeds,
        Fails,
        /// Writes part of the file, then waits for cancellation.
        Hangs,
    }

    #[derive(Default)]
    struct FakeEditor {
        exporting: Exporting,
    }

    impl TakeEditor for FakeEditor {
        fn duration(&self, _take: &Path) -> Result<Duration, RecordingError> {
            Ok(Duration::from_secs(10))
        }

        fn frame_at(
            &self,
            _take: &Path,
            _position: Duration,
        ) -> Result<VideoFrame, RecordingError> {
            Ok(VideoFrame {
                width: 2,
                height: 2,
                pixels: vec![0; 16],
            })
        }

        fn export(
            &self,
            _take: &Path,
            _start: Duration,
            _end: Duration,
            output: &Path,
            cancel: &AtomicBool,
            progress: &dyn Fn(f64),
        ) -> Result<(), RecordingError> {
            fs::write(output, b"partial").expect("write export");
            progress(0.5);
            match self.exporting {
                Exporting::Succeeds => {
                    fs::write(output, b"trimmed").expect("write export");
                    Ok(())
                }
                Exporting::Fails => Err(RecordingError::new(
                    RecordingErrorKind::Unavailable,
                    "no encoder",
                )),
                Exporting::Hangs => {
                    while !cancel.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(RecordingError::new(
                        RecordingErrorKind::Cancelled,
                        "cancelled",
                    ))
                }
            }
        }
    }

    fn new_service(backend: impl RecordingBackend + 'static, folder: &Path) -> RecordingService {
        RecordingService::new(
            Arc::new(backend),
            Arc::new(FakeEditor::default()),
            folder.to_path_buf(),
        )
    }

    fn exporting_service(exporting: Exporting, folder: &Path) -> RecordingService {
        RecordingService::new(
            Arc::new(FakeBackend::new(Script::default())),
            Arc::new(FakeEditor { exporting }),
            folder.to_path_buf(),
        )
    }

    fn wait_for_export(
        service: &RecordingService,
        done: impl Fn(&ExportPhase) -> bool,
    ) -> ExportPhase {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let export = service.snapshot().export;
            if done(&export) {
                return export;
            }
            assert!(Instant::now() < deadline, "timed out in {export:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn finished(export: &ExportPhase) -> bool {
        !matches!(export, ExportPhase::Exporting { .. })
    }

    #[test]
    fn exports_write_a_new_file_and_keep_the_take() {
        let folder = TempDir::new().expect("temp dir");
        let take = folder.path().join("Kestrel-recording-1.webm");
        fs::write(&take, b"take").expect("take");
        let exports = TempDir::new().expect("export dir");
        let destination = exports.path().join("clip.webm");
        let mut service = exporting_service(Exporting::Succeeds, folder.path());
        assert_eq!(service.take_duration(&take), Ok(Duration::from_secs(10)));
        assert!(
            service
                .take_duration(&exports.path().join("other.webm"))
                .is_err()
        );
        let (one, two, four) = (
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
        );
        assert!(
            service
                .export(&take, two, one, destination.clone())
                .is_err(),
            "empty range"
        );
        assert!(
            service.export(&take, one, two, take.clone()).is_err(),
            "the take is never overwritten"
        );
        service
            .export(&take, one, four, destination.clone())
            .expect("export starts");
        let export = wait_for_export(&service, finished);
        assert_eq!(
            export,
            ExportPhase::Exported {
                path: destination.clone()
            }
        );
        assert_eq!(fs::read(&destination).expect("export"), b"trimmed");
        assert_eq!(
            files(exports.path()),
            ["clip.webm"],
            "no partial file remains"
        );
        assert_eq!(fs::read(&take).expect("take"), b"take");
        service.dismiss_export();
        assert_eq!(service.snapshot().export, ExportPhase::Idle);
    }

    #[test]
    fn failed_and_cancelled_exports_leave_no_output() {
        let folder = TempDir::new().expect("temp dir");
        let take = folder.path().join("Kestrel-recording-1.webm");
        fs::write(&take, b"take").expect("take");
        let exports = TempDir::new().expect("export dir");
        let range = (Duration::ZERO, Duration::from_secs(1));

        let mut failing = exporting_service(Exporting::Fails, folder.path());
        failing
            .export(&take, range.0, range.1, exports.path().join("a.webm"))
            .expect("export starts");
        let export = wait_for_export(&failing, finished);
        assert!(matches!(
            export,
            ExportPhase::Failed { error } if error.kind == RecordingErrorKind::Unavailable
        ));
        assert!(files(exports.path()).is_empty());

        let mut hanging = exporting_service(Exporting::Hangs, folder.path());
        hanging
            .export(&take, range.0, range.1, exports.path().join("b.webm"))
            .expect("export starts");
        wait_for_export(&hanging, |export| {
            matches!(export, ExportPhase::Exporting { percent: 50, .. })
        });
        assert!(
            hanging
                .export(&take, range.0, range.1, exports.path().join("c.webm"))
                .is_err(),
            "one export at a time"
        );
        hanging.shutdown();
        assert_eq!(hanging.snapshot().export, ExportPhase::Idle);
        assert!(files(exports.path()).is_empty());
        assert_eq!(fs::read(&take).expect("take"), b"take");
    }

    fn options(source: RecordingSource, audio: Vec<AudioTrack>) -> RecordingOptions {
        RecordingOptions {
            source,
            cursor: true,
            framerate: 30,
            audio,
            system_audio: SystemAudioSource::Output,
        }
    }

    fn wait_for(
        service: &RecordingService,
        done: impl Fn(&RecordingPhase) -> bool,
    ) -> RecordingPhase {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let phase = service.snapshot().phase;
            if done(&phase) {
                return phase;
            }
            assert!(Instant::now() < deadline, "timed out in {phase:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn recording(phase: &RecordingPhase) -> bool {
        matches!(phase, RecordingPhase::Recording { .. })
    }

    fn files(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .expect("folder")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_stopped_recording_becomes_a_listed_take_and_releases_the_session() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script::default());
        let mut service = new_service(backend.clone(), folder.path());
        service
            .start(options(RecordingSource::Screen, Vec::new()))
            .expect("starts");
        wait_for(&service, recording);
        assert!(
            service
                .start(options(RecordingSource::Screen, Vec::new()))
                .is_err()
        );
        service.pause().expect("pause");
        wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Recording { paused: true, .. })
        });
        service.resume().expect("resume");
        wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Recording { paused: false, .. })
        });
        service.stop().expect("stop");
        let phase = wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Completed { .. })
        });
        let RecordingPhase::Completed { take, note: None } = phase else {
            panic!("completed without a note: {phase:?}");
        };
        assert_eq!(fs::read(&take.path).expect("take"), b"complete media");
        assert_eq!(
            files(folder.path()),
            std::slice::from_ref(&take.name),
            "no partial file remains"
        );
        assert_eq!(service.snapshot().takes, {
            let mut listed = take.clone();
            listed.duration = None;
            vec![listed]
        });
        service.shutdown();
        assert_eq!(backend.calls(), ["pause", "resume", "finish"]);
        assert_eq!(lock(&backend.log).sessions_open, 0);
        let plan = &lock(&backend.log).plans[0];
        assert_eq!((plan.width, plan.height), (100, 60), "even frame size");
        assert_eq!(plan.crop, None);
    }

    #[test]
    fn cancelling_discards_the_partial_file() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script::default());
        let mut service = new_service(backend.clone(), folder.path());
        service
            .start(options(RecordingSource::Window, Vec::new()))
            .expect("starts");
        wait_for(&service, recording);
        service.cancel().expect("cancel");
        wait_for(&service, |phase| *phase == RecordingPhase::Idle);
        service.shutdown();
        assert!(files(folder.path()).is_empty());
        assert_eq!(backend.calls(), ["abort"]);
        assert_eq!(lock(&backend.log).sessions_open, 0);
    }

    #[test]
    fn failures_remove_output_and_keep_earlier_takes() {
        let folder = TempDir::new().expect("temp dir");
        fs::write(folder.path().join("Kestrel-recording-earlier.webm"), b"old").expect("earlier");
        let backend = FakeBackend::new(Script {
            finish_error: true,
            ..Script::default()
        });
        let mut service = new_service(backend, folder.path());
        service
            .start(options(RecordingSource::Screen, Vec::new()))
            .expect("starts");
        wait_for(&service, recording);
        service.stop().expect("stop");
        let phase = wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Failed { .. })
        });
        assert!(matches!(
            phase,
            RecordingPhase::Failed { error } if error.kind == RecordingErrorKind::Storage
        ));
        service.shutdown();
        assert_eq!(files(folder.path()), ["Kestrel-recording-earlier.webm"]);
    }

    #[test]
    fn consent_outcomes_are_distinct() {
        for (kind, expected_idle) in [
            (RecordingErrorKind::Cancelled, true),
            (RecordingErrorKind::Denied, false),
        ] {
            let folder = TempDir::new().expect("temp dir");
            let backend = FakeBackend::new(Script {
                open_error: Some(kind),
                ..Script::default()
            });
            let mut service = new_service(backend, folder.path());
            service
                .start(options(RecordingSource::Screen, Vec::new()))
                .expect("starts");
            let phase = wait_for(&service, |phase| !phase.is_active());
            assert_eq!(phase == RecordingPhase::Idle, expected_idle, "{phase:?}");
            service.shutdown();
            assert!(files(folder.path()).is_empty());
        }
    }

    #[test]
    fn the_desktop_ending_sharing_saves_the_take_with_a_note() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script::default());
        let mut service = new_service(backend.clone(), folder.path());
        service
            .start(options(RecordingSource::Screen, Vec::new()))
            .expect("starts");
        wait_for(&service, recording);
        backend.revoked.store(true, Ordering::Release);
        let phase = wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Completed { .. })
        });
        assert!(matches!(
            phase,
            RecordingPhase::Completed { note: Some(error), .. } if error.kind == RecordingErrorKind::Revoked
        ));
        service.shutdown();
        assert_eq!(lock(&backend.log).sessions_open, 0);
    }

    #[test]
    fn a_failed_track_is_reported_while_recording_continues() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script {
            events: vec![
                RecordingEvent::ApplicationAudio { playing: true },
                RecordingEvent::TrackFailed {
                    track: AudioTrack::Microphone,
                    error: RecordingError::new(RecordingErrorKind::Unavailable, "unplugged"),
                },
            ],
            ..Script::default()
        });
        let mut service = new_service(backend, folder.path());
        let mut request = options(
            RecordingSource::Screen,
            vec![AudioTrack::System, AudioTrack::Microphone],
        );
        request.system_audio = SystemAudioSource::Application {
            identity: "org.example.Player".to_owned(),
        };
        service.start(request).expect("starts");
        let phase = wait_for(
            &service,
            |phase| matches!(phase, RecordingPhase::Recording { tracks, .. } if tracks.iter().any(|t| t.error.is_some())),
        );
        let RecordingPhase::Recording { tracks, .. } = phase else {
            unreachable!();
        };
        assert_eq!(tracks[0].error, None, "system audio continues");
        assert_eq!(
            tracks[0].application,
            Some(ApplicationTrackState {
                identity: "org.example.Player".to_owned(),
                playing: true,
            })
        );
        assert_eq!(tracks[1].track, AudioTrack::Microphone);
        service.stop().expect("stop");
        wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Completed { .. })
        });
    }

    #[test]
    fn an_area_is_chosen_from_the_shared_frame_and_cropped_evenly() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script::default());
        let mut service = new_service(backend.clone(), folder.path());
        service
            .start(options(RecordingSource::Area, Vec::new()))
            .expect("starts");
        let phase = wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::ChoosingArea { .. })
        });
        let RecordingPhase::ChoosingArea { frame } = phase else {
            unreachable!();
        };
        assert_eq!((frame.width, frame.height), (101, 61));
        assert!(
            service
                .choose_area(Some(Rect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 4
                }))
                .is_err()
        );
        service
            .choose_area(Some(Rect {
                x: -10,
                y: 21,
                width: 61,
                height: 200,
            }))
            .expect("area");
        wait_for(&service, recording);
        service.stop().expect("stop");
        wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::Completed { .. })
        });
        service.shutdown();
        let plan = &lock(&backend.log).plans[0];
        assert_eq!(
            plan.crop,
            Some(CropRegion {
                x: 0,
                y: 21,
                width: 50,
                height: 40
            })
        );
        assert_eq!((plan.width, plan.height), (50, 40));
        assert_eq!(plan.source_size, (101, 61));
    }

    #[test]
    fn shutdown_saves_a_running_take_and_abandons_area_choice() {
        let folder = TempDir::new().expect("temp dir");
        let backend = FakeBackend::new(Script::default());
        let mut service = new_service(backend.clone(), folder.path());
        service
            .start(options(RecordingSource::Screen, Vec::new()))
            .expect("starts");
        wait_for(&service, recording);
        service.shutdown();
        assert!(matches!(
            service.snapshot().phase,
            RecordingPhase::Completed { .. }
        ));
        assert_eq!(service.snapshot().takes.len(), 1);

        service.dismiss();
        service
            .start(options(RecordingSource::Area, Vec::new()))
            .expect("starts");
        wait_for(&service, |phase| {
            matches!(phase, RecordingPhase::ChoosingArea { .. })
        });
        service.shutdown();
        assert_eq!(service.snapshot().phase, RecordingPhase::Idle);
        assert_eq!(lock(&backend.log).sessions_open, 0);
        assert_eq!(files(folder.path()).len(), 1, "only the first take");
    }

    #[test]
    fn crash_leftovers_are_removed_and_takes_listed_newest_first() {
        let folder = TempDir::new().expect("temp dir");
        let abandoned = folder.path().join(".Kestrel-recording-a.webm.partial");
        fs::write(&abandoned, b"x").expect("partial");
        fs::File::options()
            .write(true)
            .open(&abandoned)
            .and_then(|file| file.set_modified(SystemTime::now() - Duration::from_secs(120)))
            .expect("backdate");
        let in_progress = folder.path().join(".Kestrel-recording-b.webm.partial");
        fs::write(&in_progress, b"x").expect("fresh partial");
        fs::write(folder.path().join("holiday.webm"), b"x").expect("unrelated");
        for name in ["Kestrel-recording-1.webm", "Kestrel-recording-2.webm"] {
            fs::write(folder.path().join(name), b"x").expect("take");
            thread::sleep(Duration::from_millis(20));
        }
        let service = new_service(FakeBackend::new(Script::default()), folder.path());
        let names: Vec<String> = service
            .snapshot()
            .takes
            .into_iter()
            .map(|take| take.name)
            .collect();
        assert_eq!(
            names,
            ["Kestrel-recording-2.webm", "Kestrel-recording-1.webm"]
        );
        assert!(!abandoned.exists());
        assert!(
            in_progress.exists(),
            "another instance may still be writing it"
        );
        assert!(folder.path().join("holiday.webm").exists());
        service
            .delete_take(&folder.path().join("Kestrel-recording-1.webm"))
            .expect("delete listed take");
        assert!(
            service
                .delete_take(&folder.path().join("holiday.webm"))
                .is_err()
        );
        assert_eq!(service.snapshot().takes.len(), 1);
    }
}
