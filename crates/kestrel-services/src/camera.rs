//! Camera preview: one camera open at a time on a worker, latest frame only.
//!
//! Frames go to a shared slot that the window reads; nothing is recorded,
//! and stopping drops the stream, which turns the camera off.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kestrel_platform::camera::{CameraBackend, CameraError, CameraFrame};

/// How long the worker waits for a frame before checking for stop.
const FRAME_WAIT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CameraPhase {
    #[default]
    Idle,
    /// The camera is opening.
    Starting {
        name: String,
    },
    Previewing {
        name: String,
        width: u32,
        height: u32,
    },
    Failed {
        name: String,
        error: CameraError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CameraSnapshot {
    pub phase: CameraPhase,
    /// Advances on every phase change.
    pub generation: u64,
}

/// A frame with its sequence number.
pub type SequencedFrame = (u64, Arc<CameraFrame>);

/// The newest preview frame, shared with the window.
#[derive(Clone, Default)]
pub struct CameraFrameSlot(Arc<Mutex<Option<SequencedFrame>>>);

impl CameraFrameSlot {
    /// The newest frame and its sequence number.
    pub fn latest(&self) -> Option<SequencedFrame> {
        lock(&self.0).clone()
    }

    fn put(&self, frame: CameraFrame) {
        let mut slot = lock(&self.0);
        let sequence = slot
            .as_ref()
            .map_or(1, |(sequence, _)| sequence.wrapping_add(1));
        *slot = Some((sequence, Arc::new(frame)));
    }

    fn clear(&self) {
        *lock(&self.0) = None;
    }
}

struct Shared {
    snapshot: Mutex<CameraSnapshot>,
    stop: AtomicBool,
}

impl Shared {
    fn set_phase(&self, phase: CameraPhase) {
        let mut snapshot = lock(&self.snapshot);
        snapshot.phase = phase;
        snapshot.generation = snapshot.generation.wrapping_add(1);
    }
}

pub struct CameraPreviewService {
    backend: Arc<dyn CameraBackend + Sync>,
    shared: Arc<Shared>,
    frames: CameraFrameSlot,
    worker: Option<JoinHandle<()>>,
}

impl CameraPreviewService {
    pub fn new(backend: Arc<dyn CameraBackend + Sync>) -> Self {
        Self {
            backend,
            shared: Arc::new(Shared {
                snapshot: Mutex::new(CameraSnapshot::default()),
                stop: AtomicBool::new(false),
            }),
            frames: CameraFrameSlot::default(),
            worker: None,
        }
    }

    /// Opens `path` and streams until `stop`; a running preview is stopped first.
    pub fn start(&mut self, path: PathBuf, name: String) -> Result<(), String> {
        self.stop();
        self.shared.stop.store(false, Ordering::Release);
        self.shared
            .set_phase(CameraPhase::Starting { name: name.clone() });
        let backend = Arc::clone(&self.backend);
        let shared = Arc::clone(&self.shared);
        let frames = self.frames.clone();
        let worker = thread::Builder::new()
            .name("kestrel-camera".to_owned())
            .spawn(move || run_preview(backend.as_ref(), &path, name, &shared, &frames));
        match worker {
            Ok(worker) => {
                self.worker = Some(worker);
                Ok(())
            }
            Err(error) => {
                self.shared.set_phase(CameraPhase::Idle);
                Err(format!("Starting the camera worker failed: {error}"))
            }
        }
    }

    /// Joins the worker, which closes the camera, and drops the last frame.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.frames.clear();
        let idle = lock(&self.shared.snapshot).phase == CameraPhase::Idle;
        if !idle {
            self.shared.set_phase(CameraPhase::Idle);
        }
    }

    pub fn snapshot(&self) -> CameraSnapshot {
        lock(&self.shared.snapshot).clone()
    }

    /// The slot the window reads frames from.
    pub fn frames(&self) -> CameraFrameSlot {
        self.frames.clone()
    }
}

impl Drop for CameraPreviewService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_preview(
    backend: &dyn CameraBackend,
    path: &std::path::Path,
    name: String,
    shared: &Shared,
    frames: &CameraFrameSlot,
) {
    let mut stream = match backend.open(path) {
        Ok(stream) => stream,
        Err(error) => {
            shared.set_phase(CameraPhase::Failed { name, error });
            return;
        }
    };
    let mut size = None;
    while !shared.stop.load(Ordering::Acquire) {
        match stream.next_frame(FRAME_WAIT) {
            Ok(Some(frame)) => {
                if size != Some((frame.width, frame.height)) {
                    size = Some((frame.width, frame.height));
                    shared.set_phase(CameraPhase::Previewing {
                        name: name.clone(),
                        width: frame.width,
                        height: frame.height,
                    });
                }
                frames.put(frame);
            }
            Ok(None) => {}
            Err(error) => {
                frames.clear();
                shared.set_phase(CameraPhase::Failed { name, error });
                return;
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_platform::camera::{CameraErrorKind, CameraStream};
    use std::path::Path;
    use std::time::Instant;

    /// Produces numbered frames and records when it is dropped.
    struct FakeStream {
        sent: u32,
        fail_after: Option<u32>,
        closed: Arc<AtomicBool>,
    }

    impl CameraStream for FakeStream {
        fn next_frame(&mut self, _: Duration) -> Result<Option<CameraFrame>, CameraError> {
            thread::sleep(Duration::from_millis(2));
            if self.fail_after == Some(self.sent) {
                return Err(CameraError::new(CameraErrorKind::Unavailable, "unplugged"));
            }
            self.sent += 1;
            Ok(Some(CameraFrame {
                width: 2,
                height: 1,
                rgba: vec![self.sent as u8; 8],
            }))
        }
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
        }
    }

    struct FakeCamera {
        open_error: Option<CameraErrorKind>,
        fail_after: Option<u32>,
        closed: Arc<AtomicBool>,
    }

    impl CameraBackend for FakeCamera {
        fn open(&self, _: &Path) -> Result<Box<dyn CameraStream>, CameraError> {
            if let Some(kind) = self.open_error {
                return Err(CameraError::new(kind, "refused"));
            }
            Ok(Box::new(FakeStream {
                sent: 0,
                fail_after: self.fail_after,
                closed: Arc::clone(&self.closed),
            }))
        }
    }

    fn service(
        open_error: Option<CameraErrorKind>,
        fail_after: Option<u32>,
    ) -> (CameraPreviewService, Arc<AtomicBool>) {
        let closed = Arc::new(AtomicBool::new(false));
        let backend = FakeCamera {
            open_error,
            fail_after,
            closed: Arc::clone(&closed),
        };
        (CameraPreviewService::new(Arc::new(backend)), closed)
    }

    fn wait_for(
        service: &CameraPreviewService,
        done: impl Fn(&CameraPhase) -> bool,
    ) -> CameraPhase {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let phase = service.snapshot().phase;
            if done(&phase) {
                return phase;
            }
            assert!(Instant::now() < deadline, "stuck in {phase:?}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn frames_flow_until_stop_closes_the_camera() {
        let (mut service, closed) = service(None, None);
        let slot = service.frames();
        service
            .start(PathBuf::from("/dev/video0"), "Camera".into())
            .expect("preview starts");
        let phase = wait_for(&service, |phase| {
            matches!(phase, CameraPhase::Previewing { .. })
        });
        assert_eq!(
            phase,
            CameraPhase::Previewing {
                name: "Camera".into(),
                width: 2,
                height: 1
            }
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while slot.latest().is_none_or(|(sequence, _)| sequence < 3) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        service.stop();
        assert!(closed.load(Ordering::Acquire), "the stream was dropped");
        assert!(slot.latest().is_none(), "no frame outlives the preview");
        assert_eq!(service.snapshot().phase, CameraPhase::Idle);
    }

    #[test]
    fn open_and_stream_failures_are_reported_and_drop_frames() {
        let (service, _) = service(Some(CameraErrorKind::Denied), None);
        let mut service = service;
        service
            .start(PathBuf::from("/dev/video0"), "Camera".into())
            .expect("worker starts");
        let phase = wait_for(&service, |phase| {
            matches!(phase, CameraPhase::Failed { .. })
        });
        assert!(matches!(
            phase,
            CameraPhase::Failed {
                error: CameraError {
                    kind: CameraErrorKind::Denied,
                    ..
                },
                ..
            }
        ));

        let (mut service, closed) = self::service(None, Some(2));
        let slot = service.frames();
        service
            .start(PathBuf::from("/dev/video0"), "Camera".into())
            .expect("preview starts");
        let phase = wait_for(&service, |phase| {
            matches!(phase, CameraPhase::Failed { .. })
        });
        assert!(matches!(
            phase,
            CameraPhase::Failed {
                error: CameraError {
                    kind: CameraErrorKind::Unavailable,
                    ..
                },
                ..
            }
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !closed.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "the stream closes after a failure"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(slot.latest().is_none());
    }
}
