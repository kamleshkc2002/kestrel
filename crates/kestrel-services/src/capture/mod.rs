//! Screenshot capture service: runs one backend capture at a time on a worker thread
//! and keeps results in a bounded private history.

pub mod history;
pub mod image;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::SystemTime,
};

use kestrel_platform::capture::{
    CaptureBackend, CaptureCancel, CaptureErrorKind, CaptureMode, CaptureProvider, CapturedImage,
};

pub use history::{
    CaptureEntry, CaptureHistory, CaptureId, CapturePolicy, HistoryError, NewCapture,
};
pub use image::{Color, EditOperation, EditPlan, ImageError, Rect, RgbaImage};
use image::{apply, decode_png, encode_png};

/// Rejected capture command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureCommandError {
    NotStarted,
    Busy,
    ModeUnavailable,
    WorkerUnavailable,
}

impl std::fmt::Display for CaptureCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotStarted => "capture service is not started",
            Self::Busy => "a capture is already running",
            Self::ModeUnavailable => "capture mode is unavailable",
            Self::WorkerUnavailable => "capture worker is unavailable",
        })
    }
}

impl std::error::Error for CaptureCommandError {}

/// Failure saving an edited copy of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEditError {
    UnknownCapture,
    Storage(HistoryError),
    Image(ImageError),
}

impl std::fmt::Display for CaptureEditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCapture => {
                formatter.write_str("capture was not found; refresh the capture history")
            }
            Self::Storage(error) => error.fmt(formatter),
            Self::Image(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CaptureEditError {}

/// Progress of the most recent capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturePhase {
    Idle,
    Capturing {
        mode: CaptureMode,
        provider: CaptureProvider,
    },
    Captured {
        id: CaptureId,
    },
    Cancelled,
    Failed {
        kind: CaptureErrorKind,
        message: String,
    },
}

/// Owned service state for presentation; `generation` increases on every change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSnapshot {
    pub phase: CapturePhase,
    pub entries: Vec<CaptureEntry>,
    pub total_bytes: u64,
    pub storage_error: Option<String>,
    pub generation: u64,
}

impl Default for CaptureSnapshot {
    fn default() -> Self {
        Self {
            phase: CapturePhase::Idle,
            entries: Vec::new(),
            total_bytes: 0,
            storage_error: None,
            generation: 0,
        }
    }
}

struct State {
    phase: CapturePhase,
    storage_error: Option<String>,
    generation: u64,
}

impl State {
    fn bump(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }
}

struct Worker {
    cancel: CaptureCancel,
    join: JoinHandle<()>,
}

/// Capture lifecycle owner: history storage plus at most one in-flight capture.
pub struct CaptureService {
    policy: CapturePolicy,
    directory: PathBuf,
    history: Option<Arc<Mutex<CaptureHistory>>>,
    state: Arc<Mutex<State>>,
    worker: Option<Worker>,
    started: bool,
}

impl CaptureService {
    /// A stopped service storing captures in `directory`.
    pub fn new(policy: CapturePolicy, directory: PathBuf) -> Self {
        Self {
            policy,
            directory,
            history: None,
            state: Arc::new(Mutex::new(State {
                phase: CapturePhase::Idle,
                storage_error: None,
                generation: 0,
            })),
            worker: None,
            started: false,
        }
    }

    /// Whether `start` was called without a later `stop`.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Opens the history; a storage failure is published in `storage_error`.
    pub fn start(&mut self) -> Result<(), HistoryError> {
        self.started = true;
        if self.history.is_some() {
            return Ok(());
        }
        match CaptureHistory::open(self.directory.clone(), self.policy) {
            Ok(history) => {
                self.history = Some(Arc::new(Mutex::new(history)));
                if let Ok(mut state) = self.state.lock() {
                    state.storage_error = None;
                    state.phase = CapturePhase::Idle;
                    state.bump();
                }
            }
            Err(error) => {
                if let Ok(mut state) = self.state.lock() {
                    state.storage_error = Some(error.to_string());
                    state.bump();
                }
            }
        }
        Ok(())
    }

    /// Cancels and joins any capture, then releases the history.
    pub fn stop(&mut self) {
        self.started = false;
        if let Some(worker) = self.worker.take() {
            worker.cancel.cancel();
            let _ = worker.join.join();
        }
        self.history = None;
        if let Ok(mut state) = self.state.lock() {
            state.phase = CapturePhase::Idle;
            state.bump();
        }
    }

    /// Starts `backend` capturing `mode` on a worker thread.
    pub fn begin(
        &mut self,
        backend: Box<dyn CaptureBackend>,
        mode: CaptureMode,
    ) -> Result<(), CaptureCommandError> {
        self.reap();
        let history = self
            .history
            .clone()
            .ok_or(CaptureCommandError::NotStarted)?;
        if self.worker.is_some() {
            return Err(CaptureCommandError::Busy);
        }
        if !backend.modes().contains(&mode) {
            return Err(CaptureCommandError::ModeUnavailable);
        }
        let provider = backend.provider();
        let cancel = CaptureCancel::default();
        let worker_cancel = cancel.clone();
        let state = self.state.clone();
        set_phase(&state, CapturePhase::Capturing { mode, provider });
        let join = thread::Builder::new()
            .name("kestrel-capture".into())
            .spawn(move || {
                let phase = run_capture(backend.as_ref(), mode, &worker_cancel, &history);
                set_phase(&state, phase);
            })
            .map_err(|_| {
                set_phase(&self.state, CapturePhase::Idle);
                CaptureCommandError::WorkerUnavailable
            })?;
        self.worker = Some(Worker { cancel, join });
        Ok(())
    }

    /// Aborts the in-flight capture, if any.
    pub fn cancel(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel.cancel();
        }
    }

    /// Current phase, history, and storage status.
    pub fn snapshot(&self) -> CaptureSnapshot {
        let (phase, storage_error, generation) = self
            .state
            .lock()
            .map(|state| {
                (
                    state.phase.clone(),
                    state.storage_error.clone(),
                    state.generation,
                )
            })
            .unwrap_or((CapturePhase::Idle, None, 0));
        let (entries, total_bytes) = self
            .history
            .as_ref()
            .and_then(|history| history.lock().ok())
            .map(|history| (history.entries().to_vec(), history.total_bytes()))
            .unwrap_or_default();
        CaptureSnapshot {
            phase,
            entries,
            total_bytes,
            storage_error,
            generation,
        }
    }

    /// Removes one capture.
    pub fn delete(&mut self, id: CaptureId) {
        self.reap();
        if let Some(history) = &self.history {
            if let Ok(mut history) = history.lock() {
                history.delete(id);
                forget_captured(&self.state, |captured| captured == id);
            }
        }
    }

    /// Removes every capture.
    pub fn clear(&mut self) -> Result<(), HistoryError> {
        self.reap();
        let Some(history) = &self.history else {
            return Ok(());
        };
        let result = history
            .lock()
            .map_err(|_| lock_error())
            .and_then(|mut history| history.clear());
        forget_captured(&self.state, |_| true);
        result
    }

    /// The stored PNG bytes of `id`.
    pub fn read_png(&self, id: CaptureId) -> Result<Vec<u8>, HistoryError> {
        self.locked_history()?.read(id)
    }

    /// File path of `id` for thumbnail rendering.
    pub fn thumbnail_path(&self, id: CaptureId) -> Option<PathBuf> {
        self.history.as_ref()?.lock().ok()?.path(id)
    }

    /// Atomically copies `id` to `destination`.
    pub fn export(&self, id: CaptureId, destination: &Path) -> Result<(), HistoryError> {
        self.locked_history()?.export(id, destination)
    }

    /// Applies `plan` to `id` and stores the result as a new edited capture.
    pub fn save_edit(
        &mut self,
        id: CaptureId,
        plan: &EditPlan,
    ) -> Result<CaptureEntry, CaptureEditError> {
        self.reap();
        let history = self
            .history
            .as_ref()
            .ok_or(CaptureEditError::UnknownCapture)?;
        let mut history = history
            .lock()
            .map_err(|_| CaptureEditError::Storage(lock_error()))?;
        let source = history
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or(CaptureEditError::UnknownCapture)?;
        let bytes = history.read(id).map_err(CaptureEditError::Storage)?;
        let image = decode_png(&bytes).map_err(CaptureEditError::Image)?;
        let edited = apply(&image, plan).map_err(CaptureEditError::Image)?;
        let png = encode_png(&edited).map_err(CaptureEditError::Image)?;
        let meta = NewCapture {
            mode: source.mode,
            provider: source.provider,
            edited: true,
            width: edited.width,
            height: edited.height,
        };
        let entry = history
            .insert(&png, meta, SystemTime::now())
            .map_err(CaptureEditError::Storage)?;
        bump(&self.state);
        Ok(entry)
    }

    /// Replaces the retention policy and applies it immediately.
    pub fn set_policy(&mut self, policy: CapturePolicy) {
        self.policy = policy;
        if let Some(history) = &self.history {
            if let Ok(mut history) = history.lock() {
                history.set_policy(policy, SystemTime::now());
                bump(&self.state);
            }
        }
    }

    /// Applies the age bound; publishes only when something expired.
    pub fn prune(&mut self) {
        if let Some(history) = &self.history {
            if let Ok(mut history) = history.lock() {
                if history.prune(SystemTime::now()) > 0 {
                    bump(&self.state);
                }
            }
        }
    }

    /// Joins a worker that already finished.
    fn reap(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.join.is_finished())
        {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join.join();
            }
        }
    }

    fn locked_history(&self) -> Result<MutexGuard<'_, CaptureHistory>, HistoryError> {
        self.history
            .as_ref()
            .ok_or(HistoryError::UnknownCapture)?
            .lock()
            .map_err(|_| lock_error())
    }
}

impl Drop for CaptureService {
    fn drop(&mut self) {
        self.stop()
    }
}

/// Worker body: captures, then stores the image unless cancelled.
fn run_capture(
    backend: &dyn CaptureBackend,
    mode: CaptureMode,
    cancel: &CaptureCancel,
    history: &Mutex<CaptureHistory>,
) -> CapturePhase {
    let result = backend.capture(mode, cancel);
    if cancel.is_cancelled() {
        return CapturePhase::Cancelled;
    }
    match result {
        Ok(image) => store_capture(image, mode, backend.provider(), cancel, history),
        Err(error) if error.kind == CaptureErrorKind::Cancelled || cancel.is_cancelled() => {
            CapturePhase::Cancelled
        }
        Err(error) => CapturePhase::Failed {
            kind: error.kind,
            message: error.message,
        },
    }
}

fn store_capture(
    image: CapturedImage,
    mode: CaptureMode,
    provider: CaptureProvider,
    cancel: &CaptureCancel,
    history: &Mutex<CaptureHistory>,
) -> CapturePhase {
    let Ok(mut history) = history.lock() else {
        return CapturePhase::Failed {
            kind: CaptureErrorKind::Failed,
            message: "capture storage is unavailable".into(),
        };
    };
    if cancel.is_cancelled() {
        return CapturePhase::Cancelled;
    }
    let meta = NewCapture {
        mode,
        provider: Some(provider),
        edited: false,
        width: image.width,
        height: image.height,
    };
    match history.insert(&image.png, meta, SystemTime::now()) {
        Ok(entry) if cancel.is_cancelled() => {
            history.delete(entry.id);
            CapturePhase::Cancelled
        }
        Ok(entry) => CapturePhase::Captured { id: entry.id },
        Err(error) => CapturePhase::Failed {
            kind: CaptureErrorKind::Failed,
            message: error.to_string(),
        },
    }
}

fn set_phase(state: &Mutex<State>, phase: CapturePhase) {
    if let Ok(mut state) = state.lock() {
        state.phase = phase;
        state.bump();
    }
}

fn bump(state: &Mutex<State>) {
    if let Ok(mut state) = state.lock() {
        state.bump();
    }
}

/// Publishes a change; a `Captured` phase naming a removed capture becomes `Idle`.
fn forget_captured(state: &Mutex<State>, removed: impl Fn(CaptureId) -> bool) {
    if let Ok(mut state) = state.lock() {
        if matches!(state.phase, CapturePhase::Captured { id } if removed(id)) {
            state.phase = CapturePhase::Idle;
        }
        state.bump();
    }
}

fn lock_error() -> HistoryError {
    HistoryError::Io {
        operation: "lock capture history",
        kind: std::io::ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use kestrel_platform::capture::CaptureError;
    use tempfile::TempDir;

    use super::*;

    const SAFETY_TIMEOUT: Duration = Duration::from_secs(10);

    fn policy() -> CapturePolicy {
        CapturePolicy {
            max_entries: 20,
            max_total_bytes: 64 * 1024 * 1024,
            max_age: Duration::from_secs(24 * 3600),
        }
    }

    fn captured_image(width: u32, height: u32) -> CapturedImage {
        let pixels = (0..width * height)
            .flat_map(|index| [index as u8, (index >> 8) as u8, 0x90, 255])
            .collect();
        let png = encode_png(&RgbaImage::new(width, height, pixels).unwrap()).unwrap();
        CapturedImage { png, width, height }
    }

    /// Returns a fixed image at once.
    struct ImageBackend(CapturedImage);

    impl CaptureBackend for ImageBackend {
        fn provider(&self) -> CaptureProvider {
            CaptureProvider::Grim
        }

        fn modes(&self) -> Vec<CaptureMode> {
            vec![CaptureMode::Screen, CaptureMode::Area]
        }

        fn capture(
            &self,
            _mode: CaptureMode,
            _cancel: &CaptureCancel,
        ) -> Result<CapturedImage, CaptureError> {
            Ok(self.0.clone())
        }
    }

    /// Fails at once with a fixed kind.
    struct FailingBackend(CaptureErrorKind);

    impl CaptureBackend for FailingBackend {
        fn provider(&self) -> CaptureProvider {
            CaptureProvider::Portal
        }

        fn modes(&self) -> Vec<CaptureMode> {
            vec![CaptureMode::Screen]
        }

        fn capture(
            &self,
            _mode: CaptureMode,
            _cancel: &CaptureCancel,
        ) -> Result<CapturedImage, CaptureError> {
            Err(CaptureError::new(self.0, "fake failure"))
        }
    }

    /// Blocks until the abort hook fires; then returns `Cancelled`, or a late image.
    struct BlockingBackend {
        late_image: Option<CapturedImage>,
    }

    impl CaptureBackend for BlockingBackend {
        fn provider(&self) -> CaptureProvider {
            CaptureProvider::Portal
        }

        fn modes(&self) -> Vec<CaptureMode> {
            vec![CaptureMode::Screen]
        }

        fn capture(
            &self,
            _mode: CaptureMode,
            cancel: &CaptureCancel,
        ) -> Result<CapturedImage, CaptureError> {
            let (aborted, abort_signal) = mpsc::channel();
            cancel.set_abort(Box::new(move || {
                let _ = aborted.send(());
            }));
            let _ = abort_signal.recv_timeout(SAFETY_TIMEOUT);
            self.late_image
                .clone()
                .ok_or_else(|| CaptureError::new(CaptureErrorKind::Cancelled, "cancelled"))
        }
    }

    fn blocking() -> Box<dyn CaptureBackend> {
        Box::new(BlockingBackend { late_image: None })
    }

    fn started_service(root: &TempDir) -> CaptureService {
        let mut service = CaptureService::new(policy(), capture_dir(root));
        service.start().unwrap();
        service
    }

    fn capture_dir(root: &TempDir) -> PathBuf {
        root.path().join("captures")
    }

    fn png_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|item| {
                item.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".png")
            })
            .count()
    }

    fn wait_for(
        service: &CaptureService,
        done: impl Fn(&CaptureSnapshot) -> bool,
    ) -> CaptureSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = service.snapshot();
            if done(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out in phase {:?}",
                snapshot.phase
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn capture_once(service: &mut CaptureService, image: CapturedImage) -> CaptureId {
        service
            .begin(Box::new(ImageBackend(image)), CaptureMode::Screen)
            .unwrap();
        let snapshot = wait_for(service, |snapshot| {
            matches!(snapshot.phase, CapturePhase::Captured { .. })
        });
        match snapshot.phase {
            CapturePhase::Captured { id } => id,
            other => panic!("unexpected phase {other:?}"),
        }
    }

    #[test]
    fn begin_before_start_is_not_started() {
        let root = TempDir::new().unwrap();
        let mut service = CaptureService::new(policy(), capture_dir(&root));
        let result = service.begin(
            Box::new(ImageBackend(captured_image(2, 2))),
            CaptureMode::Screen,
        );
        assert_eq!(result, Err(CaptureCommandError::NotStarted));
        assert!(!capture_dir(&root).exists());
    }

    #[test]
    fn mode_outside_backend_modes_is_unavailable() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        let result = service.begin(
            Box::new(ImageBackend(captured_image(2, 2))),
            CaptureMode::Window,
        );
        assert_eq!(result, Err(CaptureCommandError::ModeUnavailable));
        assert_eq!(service.snapshot().phase, CapturePhase::Idle);
    }

    #[test]
    fn successful_capture_is_stored_and_published() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        let image = captured_image(6, 4);
        let id = capture_once(&mut service, image.clone());
        let snapshot = service.snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        let entry = &snapshot.entries[0];
        assert_eq!(entry.id, id);
        assert_eq!((entry.width, entry.height), (6, 4));
        assert_eq!(entry.provider, Some(CaptureProvider::Grim));
        assert!(!entry.edited);
        assert_eq!(snapshot.total_bytes, image.png.len() as u64);
        assert_eq!(service.read_png(id).unwrap(), image.png);
        assert_eq!(png_files(&capture_dir(&root)), 1);
        service.stop();
    }

    #[test]
    fn second_begin_during_capture_is_busy() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        service.begin(blocking(), CaptureMode::Screen).unwrap();
        assert_eq!(
            service.begin(blocking(), CaptureMode::Screen),
            Err(CaptureCommandError::Busy)
        );
        service.cancel();
        wait_for(&service, |snapshot| {
            snapshot.phase == CapturePhase::Cancelled
        });
        service.stop();
    }

    #[test]
    fn cancel_during_capture_publishes_cancelled_and_writes_nothing() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        service.begin(blocking(), CaptureMode::Screen).unwrap();
        assert!(matches!(
            service.snapshot().phase,
            CapturePhase::Capturing { .. }
        ));
        service.cancel();
        let snapshot = wait_for(&service, |snapshot| {
            snapshot.phase == CapturePhase::Cancelled
        });
        assert!(snapshot.entries.is_empty());
        assert_eq!(png_files(&capture_dir(&root)), 0);

        // The finished worker is reaped and a new capture may begin.
        capture_once(&mut service, captured_image(2, 2));
        service.stop();
    }

    #[test]
    fn image_delivered_after_cancel_is_discarded() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        let backend = BlockingBackend {
            late_image: Some(captured_image(3, 3)),
        };
        service
            .begin(Box::new(backend), CaptureMode::Screen)
            .unwrap();
        service.cancel();
        let snapshot = wait_for(&service, |snapshot| {
            snapshot.phase == CapturePhase::Cancelled
        });
        assert!(snapshot.entries.is_empty());
        assert_eq!(png_files(&capture_dir(&root)), 0);
        service.stop();
    }

    #[test]
    fn stop_during_capture_returns_promptly() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        service.begin(blocking(), CaptureMode::Screen).unwrap();
        let stopping = Instant::now();
        service.stop();
        assert!(stopping.elapsed() < Duration::from_secs(2));
        assert!(!service.is_started());
        assert_eq!(service.snapshot().phase, CapturePhase::Idle);
        assert_eq!(png_files(&capture_dir(&root)), 0);
    }

    #[test]
    fn backend_error_kind_maps_to_failed_phase() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        service
            .begin(
                Box::new(FailingBackend(CaptureErrorKind::Denied)),
                CaptureMode::Screen,
            )
            .unwrap();
        let snapshot = wait_for(&service, |snapshot| {
            matches!(snapshot.phase, CapturePhase::Failed { .. })
        });
        assert!(matches!(
            snapshot.phase,
            CapturePhase::Failed {
                kind: CaptureErrorKind::Denied,
                ..
            }
        ));
        assert!(snapshot.entries.is_empty());
        service.stop();
    }

    #[test]
    fn redacting_edit_is_stored_as_new_entry_and_source_is_untouched() {
        let root = TempDir::new().unwrap();
        let mut service = started_service(&root);
        let source_id = capture_once(&mut service, captured_image(12, 10));
        let source_png = service.read_png(source_id).unwrap();
        let rect = Rect {
            x: 2,
            y: 3,
            width: 5,
            height: 4,
        };
        let plan = EditPlan {
            crop: None,
            operations: vec![EditOperation::Redact(rect)],
        };
        let edited = service.save_edit(source_id, &plan).unwrap();
        assert!(edited.edited);
        assert_ne!(edited.id, source_id);
        assert_eq!((edited.width, edited.height), (12, 10));
        assert_eq!(service.snapshot().entries.len(), 2);

        let edited_image = decode_png(&service.read_png(edited.id).unwrap()).unwrap();
        for y in 3..7 {
            for x in 2..7 {
                let index = (y * 12 + x) * 4;
                assert_eq!(edited_image.pixels[index..index + 4], [0, 0, 0, 255]);
            }
        }
        assert_eq!(service.read_png(source_id).unwrap(), source_png);

        service.delete(source_id);
        assert_eq!(
            service.save_edit(source_id, &plan),
            Err(CaptureEditError::UnknownCapture)
        );
        service.stop();
    }

    #[test]
    fn generation_tracks_changes_and_idle_prune_keeps_it() {
        let root = TempDir::new().unwrap();
        let mut service = CaptureService::new(policy(), capture_dir(&root));
        let initial = service.snapshot().generation;
        service.start().unwrap();
        let started = service.snapshot().generation;
        assert!(started > initial);

        let id = capture_once(&mut service, captured_image(2, 2));
        let captured = service.snapshot().generation;
        assert!(captured > started);

        service.prune();
        assert_eq!(service.snapshot().generation, captured);

        service.delete(id);
        assert!(service.snapshot().generation > captured);
        service.stop();
    }

    #[test]
    fn storage_failure_on_start_is_published() {
        let root = TempDir::new().unwrap();
        let blocker = capture_dir(&root);
        fs::write(&blocker, b"not a directory").unwrap();
        let mut service = CaptureService::new(policy(), blocker.clone());
        service.start().unwrap();
        let snapshot = service.snapshot();
        assert!(snapshot.storage_error.is_some());
        assert!(snapshot.entries.is_empty());
        assert_eq!(
            service.begin(
                Box::new(ImageBackend(captured_image(2, 2))),
                CaptureMode::Screen
            ),
            Err(CaptureCommandError::NotStarted)
        );
        assert_eq!(fs::read(&blocker).unwrap(), b"not a directory");
    }
}
