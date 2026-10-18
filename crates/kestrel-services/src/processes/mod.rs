//! Process inventory, actions, and listening-port service state.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kestrel_core::ProcessConfiguration;
use kestrel_platform::processes::{
    Ineligible, Listener, MAX_LISTENERS, MAX_SCANNED_PROCESSES, ProcessError, ProcessErrorKind,
    ProcessIdentity, ProcessRestarter, ProcessSample, ProcessScan, ProcessSignal, ProcessSignaller,
    ProcessSource, ProcfsListeners, RestartTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessPolicy {
    pub refresh: Duration,
}

impl ProcessPolicy {
    pub fn from_configuration(configuration: ProcessConfiguration) -> Self {
        configuration.into()
    }
}

impl From<ProcessConfiguration> for ProcessPolicy {
    fn from(configuration: ProcessConfiguration) -> Self {
        Self {
            refresh: Duration::from_millis(configuration.refresh_millis as u64),
        }
    }
}

impl Default for ProcessPolicy {
    fn default() -> Self {
        ProcessConfiguration::default().into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessRow {
    pub sample: ProcessSample,
    pub cpu_percent: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcessSnapshot {
    pub rows: Vec<ProcessRow>,
    pub total_processes: usize,
    pub truncated: bool,
    pub error: Option<ProcessError>,
    pub stale: bool,
    pub generation: u64,
}

struct InventoryShared {
    state: Mutex<InventoryState>,
    wake: Condvar,
}

struct InventoryState {
    policy: ProcessPolicy,
    snapshot: ProcessSnapshot,
    stop: bool,
    wake: bool,
    running: bool,
}

pub struct ProcessInventoryService {
    shared: Arc<InventoryShared>,
    worker: Option<JoinHandle<()>>,
    policy: ProcessPolicy,
}

fn lock_state(shared: &InventoryShared) -> MutexGuard<'_, InventoryState> {
    match shared.state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl ProcessInventoryService {
    pub fn new(policy: ProcessPolicy) -> Self {
        let shared = Arc::new(InventoryShared {
            state: Mutex::new(InventoryState {
                policy,
                snapshot: ProcessSnapshot::default(),
                stop: false,
                wake: false,
                running: false,
            }),
            wake: Condvar::new(),
        });
        Self {
            shared,
            worker: None,
            policy,
        }
    }

    /// Starts sampling; repeated starts are ignored.
    pub fn start<S: ProcessSource + 'static>(&mut self, source: S) {
        self.reap_finished();
        if self.worker.is_some() {
            return;
        }
        {
            let mut state = lock_state(&self.shared);
            state.policy = self.policy;
            state.stop = false;
            state.wake = false;
            state.running = true;
        }
        let shared = Arc::clone(&self.shared);
        let worker = thread::Builder::new()
            .name("kestrel-processes".to_string())
            .spawn(move || run_inventory(Box::new(source), shared));
        match worker {
            Ok(worker) => self.worker = Some(worker),
            Err(_) => {
                let mut state = lock_state(&self.shared);
                state.running = false;
            }
        }
    }

    pub fn stop(&mut self) {
        {
            let mut state = lock_state(&self.shared);
            state.stop = true;
            state.wake = true;
            self.shared.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        lock_state(&self.shared).running = false;
    }

    pub fn is_running(&self) -> bool {
        lock_state(&self.shared).running
    }

    pub fn set_policy(&mut self, policy: ProcessPolicy) {
        self.policy = policy;
        let mut state = lock_state(&self.shared);
        state.policy = policy;
        state.wake = true;
        self.shared.wake.notify_all();
    }

    pub fn snapshot(&self) -> ProcessSnapshot {
        lock_state(&self.shared).snapshot.clone()
    }

    pub fn refresh_now(&self) {
        let mut state = lock_state(&self.shared);
        state.wake = true;
        self.shared.wake.notify_all();
    }

    fn reap_finished(&mut self) {
        let finished = self.worker.as_ref().is_some_and(JoinHandle::is_finished);
        if finished {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for ProcessInventoryService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_inventory(source: Box<dyn ProcessSource>, shared: Arc<InventoryShared>) {
    let mut previous: HashMap<ProcessIdentity, u64> = HashMap::new();
    let mut previous_total = None;
    loop {
        if lock_state(&shared).stop {
            break;
        }
        match source.scan(MAX_SCANNED_PROCESSES) {
            Ok(scan) => {
                let (rows, current) = process_rows(&scan, &previous, previous_total);
                previous = current;
                previous_total = Some(scan.total_cpu_ticks);
                publish_success(&shared, rows, scan);
            }
            Err(error) => publish_error(&shared, error),
        }
        let state = lock_state(&shared);
        if state.stop {
            break;
        }
        let cadence = state.policy.refresh;

        let (mut state, _) = match shared
            .wake
            .wait_timeout_while(state, cadence, |state| !state.stop && !state.wake)
        {
            Ok(result) => result,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.stop {
            break;
        }
        state.wake = false;
    }
    lock_state(&shared).running = false;
}

fn process_rows(
    scan: &ProcessScan,
    previous: &HashMap<ProcessIdentity, u64>,
    previous_total: Option<u64>,
) -> (Vec<ProcessRow>, HashMap<ProcessIdentity, u64>) {
    let total_delta = previous_total.and_then(|old| scan.total_cpu_ticks.checked_sub(old));
    let cpu_count = scan.cpu_count as f64;
    let mut current = HashMap::with_capacity(scan.samples.len());
    let mut rows = scan
        .samples
        .iter()
        .map(|sample| {
            let cpu_percent = previous
                .get(&sample.identity)
                .and_then(|old| sample.cpu_ticks.checked_sub(*old))
                .and_then(|delta| {
                    total_delta
                        .filter(|total| *total > 0)
                        .map(|total| (delta, total))
                })
                .and_then(|(delta, total)| {
                    if cpu_count == 0.0 {
                        None
                    } else {
                        let value = (delta as f64 / total as f64) * cpu_count * 100.0;
                        Some(value.min(f32::MAX as f64) as f32)
                    }
                });
            current.insert(sample.identity, sample.cpu_ticks);
            ProcessRow {
                sample: sample.clone(),
                cpu_percent,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .cpu_percent
            .unwrap_or(f32::NEG_INFINITY)
            .total_cmp(&left.cpu_percent.unwrap_or(f32::NEG_INFINITY))
            .then_with(|| right.sample.resident_bytes.cmp(&left.sample.resident_bytes))
    });
    (rows, current)
}

fn publish_success(shared: &InventoryShared, rows: Vec<ProcessRow>, scan: ProcessScan) {
    let mut state = lock_state(shared);
    let next = ProcessSnapshot {
        rows,
        total_processes: scan.samples.len(),
        truncated: scan.truncated,
        error: None,
        stale: false,
        generation: state.snapshot.generation,
    };
    if !same_content(&state.snapshot, &next) {
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
    state.snapshot.rows = next.rows;
    state.snapshot.total_processes = next.total_processes;
    state.snapshot.truncated = next.truncated;
    state.snapshot.error = next.error;
    state.snapshot.stale = next.stale;
}

fn publish_error(shared: &InventoryShared, error: ProcessError) {
    let mut state = lock_state(shared);
    let changed = state.snapshot.error.as_ref() != Some(&error) || !state.snapshot.stale;
    state.snapshot.error = Some(error);
    state.snapshot.stale = true;
    if changed {
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
}

fn same_content(left: &ProcessSnapshot, right: &ProcessSnapshot) -> bool {
    left.rows == right.rows
        && left.total_processes == right.total_processes
        && left.truncated == right.truncated
        && left.error == right.error
        && left.stale == right.stale
}

/// Case-insensitive search, limited to `limit` CPU-ordered matches.
pub fn search<'a>(rows: &'a [ProcessRow], query: &str, limit: usize) -> Vec<&'a ProcessRow> {
    let query = query.chars().take(128).collect::<String>().to_lowercase();
    rows.iter()
        .filter(|row| {
            let pid = row.sample.identity.pid.to_string();
            [
                row.sample.name.as_str(),
                row.sample.executable.as_deref().unwrap_or(""),
                row.sample.owner.as_deref().unwrap_or(""),
                pid.as_str(),
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(&query))
        })
        .take(limit)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    Stopped,
    StillRunning,
    AlreadyExited,
    Restarted,
    PartiallyStopped { remaining: usize },
    Refused(Ineligible),
    Reused,
    Failed(ProcessError),
}

impl fmt::Display for ActionOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("Process stopped."),
            Self::StillRunning => {
                formatter.write_str("Process is still running; try Kill if it is safe.")
            }
            Self::AlreadyExited => {
                formatter.write_str("Process already exited; refresh the process list.")
            }
            Self::Restarted => formatter.write_str("Process restarted."),
            Self::PartiallyStopped { remaining } => {
                write!(
                    formatter,
                    "{remaining} process(es) are still running; use Force restart or Kill."
                )
            }
            Self::Refused(reason) => formatter.write_str(reason.explanation()),
            Self::Reused => {
                formatter.write_str("The PID was reused; refresh and select the process again.")
            }
            Self::Failed(error) => write!(formatter, "The action failed: {}.", error.message),
        }
    }
}

fn sample_for_action(
    source: &dyn ProcessSource,
    identity: ProcessIdentity,
) -> Result<ProcessSample, ActionOutcome> {
    match source.sample(identity) {
        Ok(sample) => Ok(sample),
        Err(error) if error.kind == ProcessErrorKind::Exited => Err(ActionOutcome::AlreadyExited),
        Err(error) if error.kind == ProcessErrorKind::Reused => Err(ActionOutcome::Reused),
        Err(error) => Err(ActionOutcome::Failed(error)),
    }
}

fn signal_once(
    source: &dyn ProcessSource,
    signaller: &dyn ProcessSignaller,
    eligibility: &impl Fn(&ProcessSample) -> Result<(), Ineligible>,
    identity: ProcessIdentity,
    signal: ProcessSignal,
    wait: Duration,
) -> ActionOutcome {
    let sample = match sample_for_action(source, identity) {
        Ok(sample) => sample,
        Err(outcome) => return outcome,
    };
    if let Err(reason) = eligibility(&sample) {
        return ActionOutcome::Refused(reason);
    }
    if let Err(error) = signaller.signal(identity, signal) {
        return ActionOutcome::Failed(error);
    }
    match signaller.wait_exit(identity, wait) {
        Ok(true) => ActionOutcome::Stopped,
        Ok(false) => ActionOutcome::StillRunning,
        Err(error) => ActionOutcome::Failed(error),
    }
}

pub fn terminate(
    source: &dyn ProcessSource,
    signaller: &dyn ProcessSignaller,
    eligibility: impl Fn(&ProcessSample) -> Result<(), Ineligible>,
    identity: ProcessIdentity,
    grace: Duration,
) -> ActionOutcome {
    signal_once(
        source,
        signaller,
        &eligibility,
        identity,
        ProcessSignal::Terminate,
        grace,
    )
}

pub fn kill(
    source: &dyn ProcessSource,
    signaller: &dyn ProcessSignaller,
    eligibility: impl Fn(&ProcessSample) -> Result<(), Ineligible>,
    identity: ProcessIdentity,
) -> ActionOutcome {
    signal_once(
        source,
        signaller,
        &eligibility,
        identity,
        ProcessSignal::Kill,
        KILL_WAIT,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn restart(
    source: &dyn ProcessSource,
    signaller: &dyn ProcessSignaller,
    restarter: &dyn ProcessRestarter,
    eligibility: impl Fn(&ProcessSample) -> Result<(), Ineligible>,
    restart_target: impl Fn(&ProcessSample) -> Option<RestartTarget>,
    identity: ProcessIdentity,
    stop_timeout: Duration,
    escalate: bool,
) -> ActionOutcome {
    let sample = match sample_for_action(source, identity) {
        Ok(sample) => sample,
        Err(outcome) => return outcome,
    };
    if let Err(reason) = eligibility(&sample) {
        return ActionOutcome::Refused(reason);
    }
    let Some(target) = restart_target(&sample) else {
        return ActionOutcome::Failed(ProcessError::new(
            ProcessErrorKind::Refused,
            "This process has no supported restart target; use its desktop or user-service launcher.",
        ));
    };
    if matches!(target, RestartTarget::UserUnit { .. }) {
        return match restarter.restart(&target, stop_timeout) {
            Ok(()) => ActionOutcome::Restarted,
            Err(error) => ActionOutcome::Failed(error),
        };
    }

    let members = match source.scope_members(identity) {
        Ok(members) => members,
        Err(error) => return ActionOutcome::Failed(error),
    };
    let mut unique = Vec::new();
    for member in members {
        if !unique.contains(&member) {
            unique.push(member);
        }
    }
    if !unique.contains(&identity) {
        unique.push(identity);
    }
    // SIGTERM all members before one shared deadline.
    let deadline = Instant::now()
        .checked_add(stop_timeout)
        .unwrap_or_else(Instant::now);
    let mut signalled = Vec::new();
    for member in unique {
        let member_sample = match source.sample(member) {
            Ok(sample) => sample,
            Err(error)
                if matches!(
                    error.kind,
                    ProcessErrorKind::Exited | ProcessErrorKind::Reused
                ) =>
            {
                continue;
            }
            Err(error) => return ActionOutcome::Failed(error),
        };
        if eligibility(&member_sample).is_err() {
            continue;
        }
        match signaller.signal(member, ProcessSignal::Terminate) {
            Ok(()) => signalled.push(member),
            Err(error) if error.kind == ProcessErrorKind::Exited => {}
            Err(error) => return ActionOutcome::Failed(error),
        }
    }
    let remaining = match wait_all(signaller, signalled, deadline) {
        Ok(remaining) => remaining,
        Err(error) => return ActionOutcome::Failed(error),
    };
    if !remaining.is_empty() && !escalate {
        return ActionOutcome::PartiallyStopped {
            remaining: remaining.len(),
        };
    }
    if !remaining.is_empty() {
        let mut killed = Vec::new();
        for member in remaining {
            match signaller.signal(member, ProcessSignal::Kill) {
                Ok(()) => killed.push(member),
                Err(error) if error.kind == ProcessErrorKind::Exited => {}
                Err(error) => return ActionOutcome::Failed(error),
            }
        }
        let kill_deadline = Instant::now() + KILL_WAIT;
        match wait_all(signaller, killed, kill_deadline) {
            Ok(still_running) if still_running.is_empty() => {}
            Ok(still_running) => {
                return ActionOutcome::PartiallyStopped {
                    remaining: still_running.len(),
                };
            }
            Err(error) => return ActionOutcome::Failed(error),
        }
    }
    match restarter.restart(&target, RELAUNCH_TIMEOUT) {
        Ok(()) => ActionOutcome::Restarted,
        Err(error) => ActionOutcome::Failed(error),
    }
}

/// SIGKILL wait bound.
const KILL_WAIT: Duration = Duration::from_secs(1);
/// Relaunch timeout bound.
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(10);

fn wait_all(
    signaller: &dyn ProcessSignaller,
    members: Vec<ProcessIdentity>,
    deadline: Instant,
) -> Result<Vec<ProcessIdentity>, ProcessError> {
    let mut remaining = Vec::new();
    for member in members {
        let timeout = deadline.saturating_duration_since(Instant::now());
        match signaller.wait_exit(member, timeout) {
            Ok(true) => {}
            Ok(false) => remaining.push(member),
            Err(error) if error.kind == ProcessErrorKind::Exited => {}
            Err(error) => return Err(error),
        }
    }
    Ok(remaining)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction<'a> {
    Terminate,
    Kill,
    Restart(&'a RestartTarget, usize),
}

pub fn confirmation_scope(sample: &ProcessSample, action: ConfirmAction<'_>) -> String {
    let executable = sample.executable.as_deref().unwrap_or(&sample.name);
    let owner = sample.owner.as_deref().unwrap_or("unknown user");
    match action {
        ConfirmAction::Terminate => format!(
            "Send SIGTERM to {executable} (PID {}, user {owner}).{}",
            sample.identity.pid,
            thread_note(sample.threads)
        ),
        ConfirmAction::Kill => format!(
            "Send SIGKILL to {executable} (PID {}, user {owner}).{}",
            sample.identity.pid,
            thread_note(sample.threads)
        ),
        ConfirmAction::Restart(target, members) => match target {
            RestartTarget::UserUnit { unit } => {
                format!("Restart the {unit} user unit with systemctl --user restart.")
            }
            RestartTarget::DesktopApp { desktop_id, scope } => {
                format!(
                    "Send SIGTERM to {members} process(es) in {scope}, then launch \
                     {desktop_id} from its desktop entry."
                )
            }
        },
    }
}

fn thread_note(threads: u32) -> String {
    if threads > 1 {
        format!(" Its {threads} threads stop with it.")
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListenerSnapshot {
    pub listeners: Vec<Listener>,
    pub error: Option<ProcessError>,
    pub generation: u64,
}
#[derive(Debug, Default)]
pub struct ListenerService {
    snapshot: ListenerSnapshot,
}

impl ListenerService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn refresh(&mut self, source: &ProcfsListeners, limit: usize) {
        match source.listeners(limit.min(MAX_LISTENERS)) {
            Ok(mut listeners) => {
                listeners.truncate(MAX_LISTENERS);
                if self.snapshot.listeners != listeners || self.snapshot.error.is_some() {
                    self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                }
                self.snapshot.listeners = listeners;
                self.snapshot.error = None;
            }
            Err(error) => {
                if self.snapshot.error.as_ref() != Some(&error) {
                    self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                }
                self.snapshot.error = Some(error);
            }
        }
    }

    pub fn snapshot(&self) -> ListenerSnapshot {
        self.snapshot.clone()
    }

    /// Increments when listeners or the error changes.
    pub fn generation(&self) -> u64 {
        self.snapshot.generation
    }

    /// Clearing listeners also advances generation.
    pub fn clear(&mut self) {
        if self.snapshot.listeners.is_empty() && self.snapshot.error.is_none() {
            return;
        }
        self.snapshot.listeners.clear();
        self.snapshot.error = None;
        self.snapshot.generation = self.snapshot.generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_platform::processes::ProcessKind;
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn sample(pid: u32, ticks: u64, memory: u64) -> ProcessSample {
        ProcessSample {
            identity: ProcessIdentity {
                pid,
                start_ticks: 1,
            },
            parent_pid: 1,
            name: format!("proc-{pid}"),
            executable: Some(format!("exe-{pid}")),
            uid: Some(1000),
            owner: Some("kamlesh".into()),
            kind: ProcessKind::User,
            state: 'S',
            cpu_ticks: ticks,
            resident_bytes: memory,
            threads: 2,
            scope: kestrel_platform::processes::ProcessScope::Other,
            scope_root: false,
        }
    }

    fn scan(samples: Vec<ProcessSample>, total: u64) -> ProcessScan {
        ProcessScan {
            samples,
            total_cpu_ticks: total,
            cpu_count: 2,
            truncated: false,
        }
    }

    struct FakeSource {
        scans: Mutex<VecDeque<Result<ProcessScan, ProcessError>>>,
        last_scan: Mutex<Option<Result<ProcessScan, ProcessError>>>,
        samples: Mutex<HashMap<ProcessIdentity, VecDeque<Result<ProcessSample, ProcessError>>>>,
        members: Vec<ProcessIdentity>,
    }

    impl FakeSource {
        fn new(scans: Vec<Result<ProcessScan, ProcessError>>) -> Self {
            Self {
                scans: Mutex::new(scans.into()),
                last_scan: Mutex::new(None),
                samples: Mutex::new(HashMap::new()),
                members: Vec::new(),
            }
        }

        fn with_sample(mut self, value: ProcessSample) -> Self {
            self.samples.get_mut().unwrap().insert(
                value.identity,
                VecDeque::from([Ok(value.clone()), Ok(value)]),
            );
            self
        }
        fn with_members(mut self, values: &[ProcessSample]) -> Self {
            self.members = values.iter().map(|sample| sample.identity).collect();
            let samples = self.samples.get_mut().unwrap();
            for value in values {
                samples.insert(
                    value.identity,
                    VecDeque::from([Ok(value.clone()), Ok(value.clone())]),
                );
            }
            self
        }
    }

    impl ProcessSource for FakeSource {
        fn scan(&self, _limit: usize) -> Result<ProcessScan, ProcessError> {
            let value = lock(&self.scans)
                .pop_front()
                .or_else(|| lock(&self.last_scan).clone())
                .unwrap_or_else(|| Ok(scan(Vec::new(), 0)));
            *lock(&self.last_scan) = Some(value.clone());
            value
        }

        fn sample(&self, identity: ProcessIdentity) -> Result<ProcessSample, ProcessError> {
            lock(&self.samples)
                .get_mut(&identity)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| Err(ProcessError::new(ProcessErrorKind::Exited, "exited")))
        }

        fn scope_members(
            &self,
            _identity: ProcessIdentity,
        ) -> Result<Vec<ProcessIdentity>, ProcessError> {
            Ok(self.members.clone())
        }
    }

    struct FakeSignaller {
        signals: Mutex<Vec<(ProcessIdentity, ProcessSignal)>>,
        waits: Mutex<VecDeque<Result<bool, ProcessError>>>,
    }

    impl FakeSignaller {
        fn new(waits: Vec<Result<bool, ProcessError>>) -> Self {
            Self {
                signals: Mutex::new(Vec::new()),
                waits: Mutex::new(waits.into()),
            }
        }
    }

    impl ProcessSignaller for FakeSignaller {
        fn signal(
            &self,
            identity: ProcessIdentity,
            signal: ProcessSignal,
        ) -> Result<(), ProcessError> {
            lock(&self.signals).push((identity, signal));
            Ok(())
        }

        fn wait_exit(
            &self,
            _identity: ProcessIdentity,
            _timeout: Duration,
        ) -> Result<bool, ProcessError> {
            lock(&self.waits).pop_front().unwrap_or(Ok(true))
        }
    }

    struct FakeRestarter {
        calls: AtomicUsize,
    }

    impl ProcessRestarter for FakeRestarter {
        fn restart(&self, _target: &RestartTarget, _timeout: Duration) -> Result<(), ProcessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn inventory_sorts_by_cpu_and_search_reaches_rows_past_the_cap() {
        let first = vec![sample(10, 10, 10), sample(20, 8, 30)];
        let second = vec![sample(10, 20, 10), sample(20, 12, 30)];
        let source = FakeSource::new(vec![Ok(scan(first, 100)), Ok(scan(second, 140))]);
        let mut service = ProcessInventoryService::new(ProcessPolicy {
            refresh: Duration::from_millis(20),
        });
        service.start(source);
        let deadline = Instant::now() + Duration::from_secs(1);
        while service
            .snapshot()
            .rows
            .first()
            .is_none_or(|row| row.cpu_percent.is_none())
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let snapshot = service.snapshot();
        assert_eq!(snapshot.rows.len(), 2);
        assert_eq!(snapshot.rows[0].sample.identity.pid, 10);
        assert_eq!(snapshot.rows[0].cpu_percent, Some(50.0));
        let pids = |query| {
            search(&snapshot.rows, query, 1)
                .iter()
                .map(|row| row.sample.identity.pid)
                .collect::<Vec<_>>()
        };
        assert_eq!(pids(""), [10], "the cap keeps the busiest row");
        assert_eq!(pids("EXE-20"), [20], "search covers rows past the cap");
        service.stop();
    }

    #[test]
    fn inventory_error_keeps_stale_rows_and_stop_wakes_cadence() {
        let value = sample(10, 1, 1);
        let error = ProcessError::new(ProcessErrorKind::Unavailable, "procfs unavailable");
        let source = FakeSource::new(vec![Ok(scan(vec![value], 10)), Err(error.clone())]);
        let mut service = ProcessInventoryService::new(ProcessPolicy {
            refresh: Duration::from_secs(10),
        });
        service.start(source);
        service.refresh_now();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !service.snapshot().stale {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(service.snapshot().rows.len(), 1);
        assert_eq!(service.snapshot().error, Some(error));
        let started = Instant::now();
        service.stop();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!service.is_running());
    }

    #[test]
    fn actions_revalidate_and_send_exact_signals() {
        let value = sample(42, 1, 1);
        let source = FakeSource::new(Vec::new()).with_sample(value.clone());
        let signaller = FakeSignaller::new(vec![Ok(false), Ok(true)]);
        let identity = value.identity;
        assert_eq!(
            terminate(
                &source,
                &signaller,
                |_| Ok(()),
                identity,
                Duration::from_secs(1)
            ),
            ActionOutcome::StillRunning
        );
        assert_eq!(
            kill(&source, &signaller, |_| Ok(()), identity),
            ActionOutcome::Stopped
        );
        assert_eq!(
            lock(&signaller.signals).as_slice(),
            &[
                (identity, ProcessSignal::Terminate),
                (identity, ProcessSignal::Kill)
            ]
        );
    }

    #[test]
    fn restart_user_unit_only_calls_restarter() {
        let value = sample(42, 1, 1);
        let source = FakeSource::new(Vec::new()).with_sample(value.clone());
        let signaller = FakeSignaller::new(Vec::new());
        let restarter = FakeRestarter {
            calls: AtomicUsize::new(0),
        };
        let outcome = restart(
            &source,
            &signaller,
            &restarter,
            |_| Ok(()),
            |_| {
                Some(RestartTarget::UserUnit {
                    unit: "pipewire.service".into(),
                })
            },
            value.identity,
            Duration::from_secs(1),
            false,
        );
        assert_eq!(outcome, ActionOutcome::Restarted);
        assert_eq!(restarter.calls.load(Ordering::SeqCst), 1);
        assert!(lock(&signaller.signals).is_empty());
    }

    #[test]
    fn desktop_restart_partially_stops_without_relaunch_when_not_escalating() {
        let first = sample(1, 1, 1);
        let second = sample(2, 1, 1);
        let source = FakeSource::new(Vec::new()).with_members(&[first.clone(), second.clone()]);
        let signaller = FakeSignaller::new(vec![Ok(false), Ok(true)]);
        let restarter = FakeRestarter {
            calls: AtomicUsize::new(0),
        };
        let target = RestartTarget::DesktopApp {
            desktop_id: "org.example.App".into(),
            scope: "app-example.scope".into(),
        };
        let outcome = restart(
            &source,
            &signaller,
            &restarter,
            |_| Ok(()),
            |_| Some(target.clone()),
            first.identity,
            Duration::from_secs(1),
            false,
        );
        assert_eq!(outcome, ActionOutcome::PartiallyStopped { remaining: 1 });
        assert_eq!(
            lock(&signaller.signals).as_slice(),
            &[
                (first.identity, ProcessSignal::Terminate),
                (second.identity, ProcessSignal::Terminate),
            ]
        );
        assert_eq!(restarter.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn desktop_restart_escalates_stragglers_then_relaunches() {
        let first = sample(1, 1, 1);
        let second = sample(2, 1, 1);
        let source = FakeSource::new(Vec::new()).with_members(&[first.clone(), second.clone()]);
        let signaller = FakeSignaller::new(vec![Ok(false), Ok(true), Ok(true)]);
        let restarter = FakeRestarter {
            calls: AtomicUsize::new(0),
        };
        let target = RestartTarget::DesktopApp {
            desktop_id: "org.example.App".into(),
            scope: "app-example.scope".into(),
        };
        assert_eq!(
            restart(
                &source,
                &signaller,
                &restarter,
                |_| Ok(()),
                |_| Some(target.clone()),
                first.identity,
                Duration::from_secs(1),
                true,
            ),
            ActionOutcome::Restarted
        );
        assert_eq!(
            lock(&signaller.signals).as_slice(),
            &[
                (first.identity, ProcessSignal::Terminate),
                (second.identity, ProcessSignal::Terminate),
                (first.identity, ProcessSignal::Kill),
            ]
        );
        assert_eq!(restarter.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn confirmation_names_identity_and_scope() {
        let value = sample(4242, 1, 1);
        let text = confirmation_scope(&value, ConfirmAction::Terminate);
        assert!(text.contains("exe-4242"));
        assert!(text.contains("4242"));
        assert!(text.contains("kamlesh"));
        let target = RestartTarget::DesktopApp {
            desktop_id: "org.mozilla.firefox".into(),
            scope: "app-gnome-firefox.scope".into(),
        };
        let text = confirmation_scope(&value, ConfirmAction::Restart(&target, 7));
        assert!(text.contains("7"));
        assert!(text.contains("org.mozilla.firefox"));
    }

    #[test]
    fn cpu_usage_needs_the_same_identity_and_forward_counters() {
        let mut reused = sample(30, 50, 1);
        reused.identity.start_ticks = 99;
        let previous = HashMap::from([
            (sample(10, 0, 0).identity, 10),
            (sample(20, 0, 0).identity, 40),
            (sample(30, 0, 0).identity, 10),
        ]);
        let current = scan(vec![sample(10, 30, 1), sample(20, 5, 1), reused], 200);

        let (rows, carried) = process_rows(&current, &previous, Some(100));

        let cpu = |pid: u32| {
            rows.iter()
                .find(|row| row.sample.identity.pid == pid)
                .expect("row")
                .cpu_percent
        };
        assert_eq!(cpu(10), Some(40.0));
        assert_eq!(cpu(20), None, "a counter that went backwards has no rate");
        assert_eq!(cpu(30), None, "a reused PID starts over");
        assert_eq!(carried.len(), 3, "only present identities are kept");
        let (rows, _) = process_rows(&current, &previous, Some(300));
        assert!(
            rows.iter().all(|row| row.cpu_percent.is_none()),
            "total counter reset"
        );
    }

    #[test]
    fn identical_ticks_keep_the_generation() {
        let value = sample(10, 1, 1);
        let source = FakeSource::new(vec![Ok(scan(vec![value], 10))]);
        let mut service = ProcessInventoryService::new(ProcessPolicy {
            refresh: Duration::from_millis(5),
        });
        service.start(source);
        let deadline = Instant::now() + Duration::from_secs(1);
        while service.snapshot().rows.is_empty() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        thread::sleep(Duration::from_millis(30));
        let settled = service.snapshot().generation;
        thread::sleep(Duration::from_millis(50));
        assert_eq!(service.snapshot().generation, settled);
        service.stop();
    }

    #[test]
    fn gone_reused_or_ineligible_targets_are_never_signalled() {
        let present = sample(42, 1, 1);
        let gone = sample(43, 1, 1);
        let source = FakeSource::new(Vec::new()).with_sample(present.clone());
        let mut reused_source = FakeSource::new(Vec::new());
        reused_source.samples.get_mut().unwrap().insert(
            present.identity,
            VecDeque::from([Err(ProcessError::new(ProcessErrorKind::Reused, "reused"))]),
        );
        let signaller = FakeSignaller::new(Vec::new());
        let grace = Duration::from_secs(1);

        assert_eq!(
            terminate(&source, &signaller, |_| Ok(()), gone.identity, grace),
            ActionOutcome::AlreadyExited
        );
        assert_eq!(
            terminate(
                &reused_source,
                &signaller,
                |_| Ok(()),
                present.identity,
                grace
            ),
            ActionOutcome::Reused
        );
        assert_eq!(
            kill(
                &source,
                &signaller,
                |_| Err(Ineligible::ForeignUser),
                present.identity
            ),
            ActionOutcome::Refused(Ineligible::ForeignUser)
        );
        assert!(lock(&signaller.signals).is_empty());
    }
}
