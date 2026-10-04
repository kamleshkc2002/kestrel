//! Lifecycle and policy for user-initiated network speed tests.
//!
//! A service owns one named worker at a time. The worker is the only code that
//! invokes the backend; all state visible to the UI is copied through a small
//! mutex-protected snapshot.

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::Duration,
};

use kestrel_core::{SPEED_TEST_BYTES_PER_MEGABYTE, SpeedTestConfiguration};
use kestrel_platform::speed_test::{
    CancellationToken, SpeedTestBackend, SpeedTestError, SpeedTestErrorKind, SpeedTestMeasurement,
    SpeedTestPhase, SpeedTestPlan, SpeedTestProgress, SpeedTestProvider,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedTestPolicy {
    pub plan: SpeedTestPlan,
}

impl SpeedTestPolicy {
    pub fn from_configuration(configuration: &SpeedTestConfiguration) -> Self {
        Self {
            plan: SpeedTestPlan {
                download_bytes: u64::from(configuration.download_megabytes)
                    .saturating_mul(SPEED_TEST_BYTES_PER_MEGABYTE),
                upload_bytes: u64::from(configuration.upload_megabytes)
                    .saturating_mul(SPEED_TEST_BYTES_PER_MEGABYTE),
                phase_timeout: Duration::from_secs(u64::from(configuration.timeout_seconds)),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeedTestStatus {
    Idle,
    Running(SpeedTestProgress),
    Completed,
    Cancelled,
    Failed(SpeedTestError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedTestSnapshot {
    pub status: SpeedTestStatus,
    pub last_measurement: Option<SpeedTestMeasurement>,
    pub provider: SpeedTestProvider,
    pub plan: SpeedTestPlan,
    pub available: bool,
    pub generation: u64,
}

impl SpeedTestSnapshot {
    pub fn disclosure(&self) -> String {
        let download = self.plan.download_bytes / SPEED_TEST_BYTES_PER_MEGABYTE;
        let upload = self.plan.upload_bytes / SPEED_TEST_BYTES_PER_MEGABYTE;
        format!(
            "Contacts {} ({}) only when you start a test. Downloads up to {} MB and uploads up to {} MB; {} sees your IP address. Each phase stops after {} seconds.",
            self.provider.host,
            self.provider.name,
            download,
            upload,
            self.provider.name,
            self.plan.phase_timeout.as_secs(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedTestServiceError {
    AlreadyRunning,
    Unavailable,
    NotRunning,
    /// The operating system refused to start the worker thread.
    WorkerUnavailable,
}

impl fmt::Display for SpeedTestServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyRunning => "A speed test is already running.",
            Self::Unavailable => "The speed test backend is unavailable.",
            Self::NotRunning => "No speed test is running.",
            Self::WorkerUnavailable => "The speed test could not start a worker thread.",
        })
    }
}

impl std::error::Error for SpeedTestServiceError {}

#[derive(Debug)]
struct SharedState {
    status: SpeedTestStatus,
    last_measurement: Option<SpeedTestMeasurement>,
    generation: u64,
}

pub struct SpeedTestService<B: SpeedTestBackend> {
    backend: Arc<B>,
    policy: SpeedTestPolicy,
    shared: Arc<Mutex<SharedState>>,
    worker: Option<JoinHandle<()>>,
    cancel: Option<CancellationToken>,
}

fn lock_state(shared: &Mutex<SharedState>) -> MutexGuard<'_, SharedState> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl<B: SpeedTestBackend> SpeedTestService<B> {
    pub fn new(backend: B, policy: SpeedTestPolicy) -> Self {
        Self {
            backend: Arc::new(backend),
            policy,
            shared: Arc::new(Mutex::new(SharedState {
                status: SpeedTestStatus::Idle,
                last_measurement: None,
                generation: 0,
            })),
            worker: None,
            cancel: None,
        }
    }

    pub fn set_policy(&mut self, policy: SpeedTestPolicy) {
        self.reap_finished();
        self.policy = policy;
    }

    pub fn start(&mut self) -> Result<(), SpeedTestServiceError> {
        self.reap_finished();
        if self.worker.is_some() || self.is_running() {
            return Err(SpeedTestServiceError::AlreadyRunning);
        }
        if !self.backend.is_available() {
            return Err(SpeedTestServiceError::Unavailable);
        }

        let token = CancellationToken::new();
        let backend = Arc::clone(&self.backend);
        let shared = Arc::clone(&self.shared);
        let plan = self.policy.plan;
        set_status(
            &shared,
            SpeedTestStatus::Running(SpeedTestProgress {
                phase: SpeedTestPhase::Latency,
                transferred_bytes: 0,
                phase_bytes: 0,
            }),
        );
        let worker_token = token.clone();
        let worker_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("kestrel-speed-test".to_string())
            .spawn(move || {
                let result = backend.run(&plan, &worker_token, &mut |progress| {
                    set_status(&worker_shared, SpeedTestStatus::Running(progress));
                });
                match result {
                    Ok(measurement) => {
                        let mut state = lock_state(&worker_shared);
                        state.last_measurement = Some(measurement);
                        set_status_locked(&mut state, SpeedTestStatus::Completed);
                    }
                    Err(error) if error.kind == SpeedTestErrorKind::Cancelled => {
                        set_status(&worker_shared, SpeedTestStatus::Cancelled);
                    }
                    Err(error) => {
                        set_status(&worker_shared, SpeedTestStatus::Failed(error));
                    }
                }
            });
        match spawned {
            Ok(worker) => {
                self.cancel = Some(token);
                self.worker = Some(worker);
                Ok(())
            }
            Err(_) => {
                // Nothing ran, so the run is reported as never having started.
                set_status(&shared, SpeedTestStatus::Idle);
                Err(SpeedTestServiceError::WorkerUnavailable)
            }
        }
    }

    pub fn cancel(&mut self) -> Result<(), SpeedTestServiceError> {
        self.reap_finished();
        let Some(token) = &self.cancel else {
            return Err(SpeedTestServiceError::NotRunning);
        };
        if !self.is_running() {
            return Err(SpeedTestServiceError::NotRunning);
        }
        token.cancel();
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        matches!(lock_state(&self.shared).status, SpeedTestStatus::Running(_))
    }

    pub fn snapshot(&self) -> SpeedTestSnapshot {
        let provider = self.backend.provider();
        let available = self.backend.is_available();
        let state = lock_state(&self.shared);
        SpeedTestSnapshot {
            status: state.status.clone(),
            last_measurement: state.last_measurement,
            provider,
            plan: self.policy.plan,
            available,
            generation: state.generation,
        }
    }

    pub fn stop(&mut self) {
        if let Some(token) = &self.cancel {
            token.cancel();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
            self.cancel = None;
            if self.is_running() {
                set_status(&self.shared, SpeedTestStatus::Cancelled);
            }
        }
    }

    fn reap_finished(&mut self) {
        let running = self.is_running();
        let should_join = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished() || !running);
        if should_join {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            self.cancel = None;
        }
    }
}

impl<B: SpeedTestBackend> Drop for SpeedTestService<B> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn set_status(shared: &Mutex<SharedState>, status: SpeedTestStatus) {
    let mut state = lock_state(shared);
    set_status_locked(&mut state, status);
}

fn set_status_locked(state: &mut SharedState, status: SpeedTestStatus) {
    if state.status != status {
        state.status = status;
        state.generation = state.generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc::{self, Receiver, RecvTimeoutError, Sender},
        },
        time::{Duration, Instant},
    };

    use kestrel_core::SpeedTestConfiguration;
    use kestrel_platform::speed_test::{
        CLOUDFLARE_PROVIDER, CancellationToken, SpeedTestBackend, SpeedTestError,
        SpeedTestErrorKind, SpeedTestMeasurement, SpeedTestPhase, SpeedTestPlan, SpeedTestProgress,
        SpeedTestProvider,
    };

    use super::{SpeedTestPolicy, SpeedTestService, SpeedTestServiceError, SpeedTestStatus};

    /// A backend whose run blocks until the test releases an outcome, while
    /// honouring cancellation the way the real adapter does.
    struct GatedBackend {
        available: bool,
        outcomes: Mutex<Receiver<Result<SpeedTestMeasurement, SpeedTestError>>>,
        runs: Arc<AtomicUsize>,
    }

    impl SpeedTestBackend for GatedBackend {
        fn provider(&self) -> SpeedTestProvider {
            CLOUDFLARE_PROVIDER
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn run(
            &self,
            plan: &SpeedTestPlan,
            cancel: &CancellationToken,
            progress: &mut dyn FnMut(SpeedTestProgress),
        ) -> Result<SpeedTestMeasurement, SpeedTestError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            progress(SpeedTestProgress {
                phase: SpeedTestPhase::Download,
                transferred_bytes: plan.download_bytes / 2,
                phase_bytes: plan.download_bytes,
            });
            let outcomes = self.outcomes.lock().expect("outcome lock");
            loop {
                if cancel.is_cancelled() {
                    return Err(SpeedTestError {
                        kind: SpeedTestErrorKind::Cancelled,
                        message: "cancelled".to_owned(),
                    });
                }
                match outcomes.recv_timeout(Duration::from_millis(5)) {
                    Ok(outcome) => return outcome,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        // A dropped test sender behaves like a hung server.
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }

    fn service(
        available: bool,
    ) -> (
        SpeedTestService<GatedBackend>,
        Sender<Result<SpeedTestMeasurement, SpeedTestError>>,
        Arc<AtomicUsize>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let runs = Arc::new(AtomicUsize::new(0));
        let backend = GatedBackend {
            available,
            outcomes: Mutex::new(receiver),
            runs: Arc::clone(&runs),
        };
        let policy = SpeedTestPolicy::from_configuration(&SpeedTestConfiguration::default());
        (SpeedTestService::new(backend, policy), sender, runs)
    }

    fn measurement(downloaded_bytes: u64) -> SpeedTestMeasurement {
        SpeedTestMeasurement {
            latency_millis: Some(12),
            download_bits_per_second: Some(80_000_000),
            downloaded_bytes,
            upload_bits_per_second: Some(20_000_000),
            uploaded_bytes: 1_000,
            duration: Duration::from_secs(3),
        }
    }

    fn wait_until(
        service: &SpeedTestService<GatedBackend>,
        done: impl Fn(&SpeedTestStatus) -> bool,
    ) -> SpeedTestStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = service.snapshot().status;
            if done(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "status never settled: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn building_the_service_never_starts_a_test() {
        let (service, _sender, runs) = service(true);
        std::thread::sleep(Duration::from_millis(20));

        let snapshot = service.snapshot();
        assert_eq!(snapshot.status, SpeedTestStatus::Idle);
        assert!(snapshot.last_measurement.is_none());
        assert_eq!(runs.load(Ordering::SeqCst), 0, "nothing runs automatically");
    }

    #[test]
    fn a_started_test_completes_and_keeps_its_measurement() {
        let (mut service, sender, _runs) = service(true);
        let before = service.snapshot().generation;

        service.start().expect("an idle service starts");
        wait_until(
            &service,
            |status| matches!(status, SpeedTestStatus::Running(progress) if progress.phase == SpeedTestPhase::Download),
        );
        assert_eq!(
            service.start(),
            Err(SpeedTestServiceError::AlreadyRunning),
            "one run at a time"
        );
        sender
            .send(Ok(measurement(25)))
            .expect("outcome is delivered");

        wait_until(&service, |status| *status == SpeedTestStatus::Completed);
        let snapshot = service.snapshot();
        assert_eq!(snapshot.last_measurement, Some(measurement(25)));
        assert!(snapshot.generation > before, "pollers can see the change");
        assert_eq!(service.cancel(), Err(SpeedTestServiceError::NotRunning));
    }

    #[test]
    fn a_failed_run_reports_why_and_keeps_the_previous_measurement() {
        let (mut service, sender, _runs) = service(true);
        service.start().expect("first run starts");
        sender
            .send(Ok(measurement(10)))
            .expect("outcome is delivered");
        wait_until(&service, |status| *status == SpeedTestStatus::Completed);

        service
            .start()
            .expect("a finished run can be followed by another");
        let failure = SpeedTestError {
            kind: SpeedTestErrorKind::Connection,
            message: "The speed-test server could not be reached.".to_owned(),
        };
        sender
            .send(Err(failure.clone()))
            .expect("outcome is delivered");

        assert_eq!(
            wait_until(&service, |status| matches!(
                status,
                SpeedTestStatus::Failed(_)
            )),
            SpeedTestStatus::Failed(failure)
        );
        assert_eq!(service.snapshot().last_measurement, Some(measurement(10)));
    }

    #[test]
    fn cancelling_ends_the_run_without_a_measurement() {
        let (mut service, _sender, _runs) = service(true);
        service.start().expect("an idle service starts");

        service.cancel().expect("a running test can be cancelled");

        wait_until(&service, |status| *status == SpeedTestStatus::Cancelled);
        assert!(service.snapshot().last_measurement.is_none());
        assert!(!service.is_running());
    }

    #[test]
    fn stop_interrupts_a_running_test_and_joins_its_worker() {
        let (mut service, _sender, _runs) = service(true);
        service.start().expect("an idle service starts");

        let started = Instant::now();
        service.stop();

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(service.snapshot().status, SpeedTestStatus::Cancelled);
        assert_eq!(service.cancel(), Err(SpeedTestServiceError::NotRunning));
    }

    #[test]
    fn an_unavailable_backend_refuses_to_start() {
        let (mut service, _sender, runs) = service(false);

        assert_eq!(service.start(), Err(SpeedTestServiceError::Unavailable));
        assert_eq!(service.snapshot().status, SpeedTestStatus::Idle);
        assert!(!service.snapshot().available);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_disclosure_names_the_destination_the_sizes_and_the_time_bound() {
        let (service, _sender, _runs) = service(true);

        let disclosure = service.snapshot().disclosure();

        for expected in [
            "speed.cloudflare.com",
            "Cloudflare",
            "25 MB",
            "10 MB",
            "30 seconds",
        ] {
            assert!(disclosure.contains(expected), "{expected} in {disclosure}");
        }
    }
}
