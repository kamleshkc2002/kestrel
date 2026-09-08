//! Bounded, in-memory clipboard history with deterministic ownership cleanup.

use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use kestrel_platform::clipboard::{
    ClipboardBackend, ClipboardError, ClipboardProvider, PrivacyEventSource,
};
use zeroize::Zeroizing;

pub const DEFAULT_MAX_ITEM_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_ITEMS: usize = 100;
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    pub max_item_bytes: usize,
    pub max_items: usize,
    pub max_age: Duration,
    pub poll_interval: Duration,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            max_item_bytes: DEFAULT_MAX_ITEM_BYTES,
            max_items: DEFAULT_MAX_ITEMS,
            max_age: DEFAULT_MAX_AGE,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

impl ClipboardPolicy {
    pub fn validate(self) -> Result<Self, ClipboardServiceError> {
        if self.max_item_bytes == 0
            || self.max_items == 0
            || self.max_age.is_zero()
            || self.poll_interval.is_zero()
        {
            return Err(ClipboardServiceError::InvalidPolicy(
                "clipboard limits and polling interval must be non-zero".to_string(),
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardLifecycle {
    Stopped,
    Running,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItemMetadata {
    pub id: u64,
    pub size_bytes: usize,
    pub age: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub lifecycle: ClipboardLifecycle,
    pub provider: Option<ClipboardProvider>,
    pub items: Vec<ClipboardItemMetadata>,
    pub total_bytes: usize,
    pub rejected_oversize_items: u64,
    pub wipe_count: u64,
    pub last_error: Option<String>,
}

impl ClipboardSnapshot {
    fn stopped() -> Self {
        Self {
            lifecycle: ClipboardLifecycle::Stopped,
            provider: None,
            items: Vec::new(),
            total_bytes: 0,
            rejected_oversize_items: 0,
            wipe_count: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardCommand {
    Refresh,
    SelectItem { item_id: u64 },
    Wipe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardServiceError {
    InvalidPolicy(String),
    AlreadyRunning,
    NotRunning,
    UnknownItem { item_id: u64 },
    Backend(ClipboardError),
    WorkerStopped,
    WorkerTimedOut,
}

impl fmt::Display for ClipboardServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for ClipboardServiceError {}

enum WorkerCommand {
    Execute(
        ClipboardCommand,
        Sender<Result<ClipboardSnapshot, ClipboardServiceError>>,
    ),
    Stop(Sender<()>),
}

pub struct ClipboardHistoryService {
    policy: ClipboardPolicy,
    sender: Option<Sender<WorkerCommand>>,
    worker: Option<JoinHandle<()>>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
}

impl ClipboardHistoryService {
    pub fn new(policy: ClipboardPolicy) -> Result<Self, ClipboardServiceError> {
        Ok(Self {
            policy: policy.validate()?,
            sender: None,
            worker: None,
            snapshot: Arc::new(Mutex::new(ClipboardSnapshot::stopped())),
        })
    }

    pub fn start<B, P>(&mut self, backend: B, privacy: P) -> Result<(), ClipboardServiceError>
    where
        B: ClipboardBackend,
        P: PrivacyEventSource,
    {
        if self.worker.is_some() {
            return Err(ClipboardServiceError::AlreadyRunning);
        }
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::clone(&self.snapshot);
        let policy = self.policy;
        let worker = thread::Builder::new()
            .name("kestrel-clipboard-history".to_string())
            .spawn(move || run_worker(policy, backend, privacy, receiver, snapshot))
            .map_err(|error| {
                ClipboardServiceError::InvalidPolicy(format!(
                    "failed to start clipboard worker: {error}"
                ))
            })?;
        self.sender = Some(sender);
        self.worker = Some(worker);
        Ok(())
    }

    pub fn latest(&self) -> ClipboardSnapshot {
        self.snapshot
            .lock()
            .expect("clipboard snapshot mutex is not poisoned")
            .clone()
    }

    pub fn execute(
        &self,
        command: ClipboardCommand,
    ) -> Result<ClipboardSnapshot, ClipboardServiceError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(ClipboardServiceError::NotRunning)?;
        let (response_sender, response_receiver) = mpsc::channel();
        sender
            .send(WorkerCommand::Execute(command, response_sender))
            .map_err(|_| ClipboardServiceError::WorkerStopped)?;
        response_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ClipboardServiceError::WorkerTimedOut,
                RecvTimeoutError::Disconnected => ClipboardServiceError::WorkerStopped,
            })?
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let (done_sender, done_receiver) = mpsc::channel();
            let _ = sender.send(WorkerCommand::Stop(done_sender));
            let _ = done_receiver.recv_timeout(Duration::from_secs(2));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ClipboardHistoryService {
    fn drop(&mut self) {
        self.stop();
    }
}

struct StoredItem {
    id: u64,
    captured_at: Instant,
    content: Zeroizing<String>,
}

struct HistoryStore {
    policy: ClipboardPolicy,
    items: VecDeque<StoredItem>,
    next_id: u64,
    rejected_oversize_items: u64,
    wipe_count: u64,
}

impl HistoryStore {
    fn new(policy: ClipboardPolicy) -> Self {
        Self {
            policy,
            items: VecDeque::new(),
            next_id: 1,
            rejected_oversize_items: 0,
            wipe_count: 0,
        }
    }

    fn capture(&mut self, content: Zeroizing<String>, now: Instant) -> bool {
        self.prune(now);
        if content.is_empty() {
            return false;
        }
        if content.len() > self.policy.max_item_bytes {
            self.rejected_oversize_items = self.rejected_oversize_items.saturating_add(1);
            return false;
        }
        if self
            .items
            .back()
            .is_some_and(|item| item.content.as_str() == content.as_str())
        {
            return false;
        }
        self.items.push_back(StoredItem {
            id: self.next_id,
            captured_at: now,
            content,
        });
        self.next_id = self.next_id.saturating_add(1);
        while self.items.len() > self.policy.max_items {
            self.items.pop_front();
        }
        true
    }

    fn prune(&mut self, now: Instant) {
        while self.items.front().is_some_and(|item| {
            now.saturating_duration_since(item.captured_at) > self.policy.max_age
        }) {
            self.items.pop_front();
        }
    }

    fn content(&self, item_id: u64) -> Option<Zeroizing<String>> {
        self.items
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| item.content.clone())
    }

    fn wipe(&mut self) {
        self.items.clear();
        self.wipe_count = self.wipe_count.saturating_add(1);
    }

    fn snapshot(
        &self,
        lifecycle: ClipboardLifecycle,
        provider: Option<ClipboardProvider>,
        last_error: Option<String>,
        now: Instant,
    ) -> ClipboardSnapshot {
        let items = self
            .items
            .iter()
            .map(|item| ClipboardItemMetadata {
                id: item.id,
                size_bytes: item.content.len(),
                age: now.saturating_duration_since(item.captured_at),
            })
            .collect::<Vec<_>>();
        ClipboardSnapshot {
            lifecycle,
            provider,
            total_bytes: items.iter().map(|item| item.size_bytes).sum(),
            items,
            rejected_oversize_items: self.rejected_oversize_items,
            wipe_count: self.wipe_count,
            last_error,
        }
    }
}

fn run_worker<B: ClipboardBackend, P: PrivacyEventSource>(
    policy: ClipboardPolicy,
    mut backend: B,
    mut privacy: P,
    receiver: Receiver<WorkerCommand>,
    snapshot: Arc<Mutex<ClipboardSnapshot>>,
) {
    let provider = backend.provider();
    let mut history = HistoryStore::new(policy);
    let mut last_owned: Option<Zeroizing<String>> = None;
    let mut last_observed: Option<Zeroizing<String>> = None;
    let mut last_error = None;
    publish(
        &snapshot,
        history.snapshot(
            ClipboardLifecycle::Running,
            Some(provider),
            None,
            Instant::now(),
        ),
    );

    loop {
        match receiver.recv_timeout(policy.poll_interval) {
            Ok(WorkerCommand::Execute(command, response)) => {
                let result = execute_command(
                    command,
                    &mut backend,
                    &mut history,
                    &mut last_owned,
                    &mut last_observed,
                    provider,
                    &mut last_error,
                );
                if let Ok(value) = &result {
                    publish(&snapshot, value.clone());
                }
                let _ = response.send(result);
            }
            Ok(WorkerCommand::Stop(done)) => {
                wipe_owned(
                    &mut backend,
                    &mut history,
                    &mut last_owned,
                    &mut last_observed,
                    &mut last_error,
                );
                publish(
                    &snapshot,
                    history.snapshot(
                        ClipboardLifecycle::Stopped,
                        None,
                        last_error.clone(),
                        Instant::now(),
                    ),
                );
                let _ = done.send(());
                break;
            }
            Err(RecvTimeoutError::Disconnected) => {
                wipe_owned(
                    &mut backend,
                    &mut history,
                    &mut last_owned,
                    &mut last_observed,
                    &mut last_error,
                );
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        match privacy.poll_events() {
            Ok(events) if !events.is_empty() => {
                wipe_owned(
                    &mut backend,
                    &mut history,
                    &mut last_owned,
                    &mut last_observed,
                    &mut last_error,
                );
            }
            Ok(_) => {}
            Err(error) => {
                last_error = Some(error.message);
                wipe_owned(
                    &mut backend,
                    &mut history,
                    &mut last_owned,
                    &mut last_observed,
                    &mut last_error,
                );
                publish(
                    &snapshot,
                    history.snapshot(
                        ClipboardLifecycle::Unavailable,
                        Some(provider),
                        last_error.clone(),
                        Instant::now(),
                    ),
                );
                break;
            }
        }

        let now = Instant::now();
        history.prune(now);
        match backend.read_text() {
            Ok(Some(content))
                if last_observed
                    .as_ref()
                    .is_none_or(|observed| observed.as_str() != content) =>
            {
                let content = Zeroizing::new(content);
                last_observed = Some(content.clone());
                if content.len() <= policy.max_item_bytes && !content.is_empty() {
                    match backend.write_text(&content) {
                        Ok(()) => {
                            if history.capture(content.clone(), now) {
                                last_owned = Some(content);
                            }
                            last_error = None;
                        }
                        Err(error) => last_error = Some(error.message),
                    }
                } else {
                    history.capture(content, now);
                }
            }
            Ok(None) => last_observed = None,
            Ok(_) => {}
            Err(error) => last_error = Some(error.message),
        }
        publish(
            &snapshot,
            history.snapshot(
                ClipboardLifecycle::Running,
                Some(provider),
                last_error.clone(),
                now,
            ),
        );
    }
}

fn execute_command<B: ClipboardBackend>(
    command: ClipboardCommand,
    backend: &mut B,
    history: &mut HistoryStore,
    last_owned: &mut Option<Zeroizing<String>>,
    last_observed: &mut Option<Zeroizing<String>>,
    provider: ClipboardProvider,
    last_error: &mut Option<String>,
) -> Result<ClipboardSnapshot, ClipboardServiceError> {
    match command {
        ClipboardCommand::Refresh => {}
        ClipboardCommand::SelectItem { item_id } => {
            let content = history
                .content(item_id)
                .ok_or(ClipboardServiceError::UnknownItem { item_id })?;
            backend
                .write_text(&content)
                .map_err(ClipboardServiceError::Backend)?;
            *last_observed = Some(content.clone());
            *last_owned = Some(content);
            *last_error = None;
        }
        ClipboardCommand::Wipe => {
            wipe_owned(backend, history, last_owned, last_observed, last_error);
        }
    }
    let now = Instant::now();
    history.prune(now);
    Ok(history.snapshot(
        ClipboardLifecycle::Running,
        Some(provider),
        last_error.clone(),
        now,
    ))
}

fn wipe_owned<B: ClipboardBackend>(
    backend: &mut B,
    history: &mut HistoryStore,
    last_owned: &mut Option<Zeroizing<String>>,
    last_observed: &mut Option<Zeroizing<String>>,
    last_error: &mut Option<String>,
) {
    if let Some(owned) = last_owned.as_ref() {
        if let Err(error) = backend.clear_if_matches(owned) {
            *last_error = Some(error.message);
        }
    }
    last_owned.take();
    history.wipe();
    match backend.read_text() {
        Ok(Some(content)) => *last_observed = Some(Zeroizing::new(content)),
        Ok(None) => *last_observed = None,
        Err(error) => {
            *last_observed = None;
            *last_error = Some(error.message);
        }
    }
}

fn publish(snapshot: &Arc<Mutex<ClipboardSnapshot>>, value: ClipboardSnapshot) {
    *snapshot
        .lock()
        .expect("clipboard snapshot mutex is not poisoned") = value;
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use kestrel_platform::clipboard::{
        ClipboardBackend, ClipboardError, ClipboardProvider, PrivacyEventSource,
        SessionPrivacyEvent,
    };
    use zeroize::Zeroizing;

    use super::{
        ClipboardCommand, ClipboardHistoryService, ClipboardLifecycle, ClipboardPolicy,
        HistoryStore,
    };

    #[derive(Default)]
    struct FakeState {
        current: Option<String>,
        clears: usize,
    }

    struct FakeBackend {
        state: Arc<Mutex<FakeState>>,
    }

    impl ClipboardBackend for FakeBackend {
        fn provider(&self) -> ClipboardProvider {
            ClipboardProvider::WaylandDataControl
        }

        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            Ok(self.state.lock().expect("fake state").current.clone())
        }

        fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
            self.state.lock().expect("fake state").current = Some(text.to_string());
            Ok(())
        }

        fn clear_if_matches(&mut self, expected: &str) -> Result<bool, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            if state.current.as_deref() != Some(expected) {
                return Ok(false);
            }
            state.current = None;
            state.clears += 1;
            Ok(true)
        }
    }

    struct FakePrivacy {
        events: Arc<Mutex<Vec<SessionPrivacyEvent>>>,
    }

    impl PrivacyEventSource for FakePrivacy {
        fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError> {
            Ok(self.events.lock().expect("fake events").drain(..).collect())
        }
    }

    fn policy() -> ClipboardPolicy {
        ClipboardPolicy {
            max_item_bytes: 8,
            max_items: 2,
            max_age: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
        }
    }

    #[test]
    fn store_enforces_size_deduplication_and_count_bounds() {
        let now = Instant::now();
        let mut store = HistoryStore::new(policy());

        assert!(store.capture(Zeroizing::new("one".to_string()), now));
        assert!(!store.capture(Zeroizing::new("one".to_string()), now));
        assert!(!store.capture(Zeroizing::new("oversized".to_string()), now));
        assert!(store.capture(Zeroizing::new("two".to_string()), now));
        assert!(store.capture(Zeroizing::new("three".to_string()), now));

        let snapshot = store.snapshot(ClipboardLifecycle::Running, None, None, Instant::now());
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.rejected_oversize_items, 1);
    }

    #[test]
    fn store_prunes_expired_items() {
        let now = Instant::now();
        let mut store = HistoryStore::new(policy());
        store.capture(Zeroizing::new("one".to_string()), now);

        store.prune(now + Duration::from_millis(51));

        assert!(store.items.is_empty());
    }

    #[test]
    fn worker_captures_and_wipes_owned_content_on_privacy_event() {
        let state = Arc::new(Mutex::new(FakeState {
            current: Some("secret".to_string()),
            clears: 0,
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut service = ClipboardHistoryService::new(policy()).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                },
                FakePrivacy {
                    events: Arc::clone(&events),
                },
            )
            .expect("worker starts");
        wait_for_items(&service, 1);
        events
            .lock()
            .expect("fake events")
            .push(SessionPrivacyEvent::Locked);
        wait_for_items(&service, 0);

        assert!(state.lock().expect("fake state").current.is_none());
        service.stop();
    }

    #[test]
    fn wipe_preserves_a_newer_external_selection() {
        let state = Arc::new(Mutex::new(FakeState {
            current: Some("first".to_string()),
            clears: 0,
        }));
        let mut service = ClipboardHistoryService::new(policy()).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                },
                FakePrivacy {
                    events: Arc::new(Mutex::new(Vec::new())),
                },
            )
            .expect("worker starts");
        wait_for_items(&service, 1);
        state.lock().expect("fake state").current = Some("newer".to_string());
        service
            .execute(ClipboardCommand::Wipe)
            .expect("wipe succeeds");

        assert_eq!(
            state.lock().expect("fake state").current.as_deref(),
            Some("newer")
        );
        service.stop();
    }

    #[test]
    fn stop_releases_owned_selection_and_joins_worker() {
        let state = Arc::new(Mutex::new(FakeState {
            current: Some("owned".to_string()),
            clears: 0,
        }));
        let mut service = ClipboardHistoryService::new(policy()).expect("valid policy");
        service
            .start(
                FakeBackend {
                    state: Arc::clone(&state),
                },
                FakePrivacy {
                    events: Arc::new(Mutex::new(Vec::new())),
                },
            )
            .expect("worker starts");
        wait_for_items(&service, 1);

        service.stop();

        assert_eq!(service.latest().lifecycle, ClipboardLifecycle::Stopped);
        assert!(state.lock().expect("fake state").current.is_none());
    }

    fn wait_for_items(service: &ClipboardHistoryService, expected: usize) {
        for _ in 0..100 {
            if service.latest().items.len() == expected {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("clipboard worker did not publish {expected} items");
    }
}
