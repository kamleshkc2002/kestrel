//! Debounced, encrypted persistence for pinned clipboard text.
//!
//! One named thread owns the key store and the file. Saves are coalesced for
//! a short interval, loads are retried when the keyring is locked, and any
//! unreadable store is reported as a state without exposing partial content.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use kestrel_platform::{
    pin_store::{self, PinStoreError, SavedPin},
    secret_service::{PinKeyError, PinKeyStore},
};

/// Pinned changes settle for this long before one encrypted write.
#[cfg(not(test))]
pub const PIN_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
#[cfg(test)]
pub const PIN_SAVE_DEBOUNCE: Duration = Duration::from_millis(20);
/// A locked or missing keyring is retried at this interval.
#[cfg(not(test))]
pub const PIN_RETRY_INTERVAL: Duration = Duration::from_secs(10);
#[cfg(test)]
pub const PIN_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// The remedy shown while pins cannot be saved.
pub const PIN_STORE_REMEDY: &str =
    "Unlock or install a keyring (Secret Service) to keep pinned entries";
/// How long a wipe may take before it is reported as unfinished.
const WIPE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the encrypted pin store is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinStoreState {
    /// Keeping pins across restarts is off.
    Disabled,
    /// The store is usable and holds nothing yet.
    Idle,
    /// A write is waiting for the debounce interval.
    Saving,
    /// The file matches memory.
    Saved,
    /// No Secret Service is available.
    Unavailable,
    /// The keyring or its collection is locked.
    Locked,
    /// The file is truncated, corrupt, or encrypted with another key.
    Unreadable,
    /// The store could not be read because its file was inaccessible.
    ReadFailed,
    /// The session is locked or asleep; saved pins stay closed until it ends.
    Held,
    /// The file could not be written.
    Failed,
}

impl PinStoreState {
    /// Whether a write can be attempted in this state.
    pub const fn writable(self) -> bool {
        matches!(self, Self::Idle | Self::Saving | Self::Saved)
    }
}

/// Counts behind the Clipboard page's saved-pins status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinStoreStatus {
    pub state: PinStoreState,
    pub saved_count: usize,
    pub saved_bytes: usize,
}

impl PinStoreStatus {
    pub fn new(state: PinStoreState) -> Self {
        Self {
            state,
            saved_count: 0,
            saved_bytes: 0,
        }
    }

    pub fn saved(state: PinStoreState, count: usize, bytes: usize) -> Self {
        Self {
            state,
            saved_count: count,
            saved_bytes: bytes,
        }
    }
}

/// What an explicit deletion removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinWipeReport {
    pub saved_count: usize,
    pub saved_bytes: usize,
    /// Whether the keyring item is gone as well.
    pub key_removed: bool,
    /// Why the keyring item could not be deleted, when it could not.
    pub key_error: Option<PinKeyError>,
}

/// How one pinned entry relates to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSaveState {
    NotPinned,
    Saved,
    OnlyText,
    OverLimit,
    Expired,
    PersistenceOff,
    /// The store is usable but has not written these pins yet.
    Pending,
    StoreUnavailable,
    StoreLocked,
    StoreUnreadable,
    StoreReadFailed,
    StoreHeld,
    StoreFailed,
}

impl PinSaveState {
    /// The Clipboard page's wording for this entry.
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotPinned => "",
            Self::Saved => "Saved",
            Self::OnlyText => "Not saved: only text pins are kept",
            Self::OverLimit => "Not saved: over the history limits",
            Self::Expired => "Not saved: older than the saved-pin age limit",
            Self::PersistenceOff => "Not saved: keeping pins across restarts is off",
            Self::Pending => "Not saved: waiting for the store",
            Self::StoreUnavailable => "Not saved: no keyring is available",
            Self::StoreLocked => "Not saved: the keyring is locked",
            Self::StoreUnreadable => "Not saved: saved pins could not be read",
            Self::StoreReadFailed => "Not saved: the saved-pins file could not be read",
            Self::StoreHeld => "Not saved: saved pins reload after you unlock",
            Self::StoreFailed => "Not saved: the store could not be written",
        }
    }
}

enum PinCommand {
    Save(Vec<SavedPin>),
    Load,
    Hold,
    Wipe {
        response: Sender<Result<PinWipeReport, PinStoreError>>,
    },
    Stop,
}

/// The latest finished read, waiting to be merged; a hold empties it.
type LoadedSlot = Arc<Mutex<Option<Vec<SavedPin>>>>;

struct PinPersistenceInner {
    sender: Sender<PinCommand>,
    status: Arc<Mutex<PinStoreStatus>>,
    loaded: LoadedSlot,
    worker: Mutex<Option<JoinHandle<()>>>,
    resave: Mutex<bool>,
}

/// Handle to the pin-store thread; clones share the thread and its status.
#[derive(Clone)]
pub struct PinPersistence {
    inner: Arc<PinPersistenceInner>,
}

impl PinPersistence {
    /// Starts the thread; it reads nothing until `request_load`.
    pub fn start(path: PathBuf, key_store: Arc<dyn PinKeyStore>) -> Result<Self, std::io::Error> {
        let (sender, receiver) = mpsc::channel();
        let loaded: LoadedSlot = Arc::new(Mutex::new(None));
        let thread_loaded = Arc::clone(&loaded);
        let status = Arc::new(Mutex::new(PinStoreStatus::new(PinStoreState::Idle)));
        let thread_status = Arc::clone(&status);
        let worker = thread::Builder::new()
            .name("kestrel-clipboard-pins".to_owned())
            .spawn(move || {
                run_pin_worker(path, key_store, receiver, thread_loaded, thread_status);
            })?;
        Ok(Self {
            inner: Arc::new(PinPersistenceInner {
                sender,
                status,
                loaded,
                worker: Mutex::new(Some(worker)),
                resave: Mutex::new(false),
            }),
        })
    }

    pub fn status(&self) -> PinStoreStatus {
        self.inner
            .status
            .lock()
            .expect("pin store status mutex is not poisoned")
            .clone()
    }

    /// Queues pins for one debounced, encrypted write.
    pub fn save(&self, pins: Vec<SavedPin>) {
        let _ = self.inner.sender.send(PinCommand::Save(pins));
    }

    /// Asks for a fresh read; used at start, after a privacy wipe, and on retry.
    pub fn request_load(&self) {
        let _ = self.inner.sender.send(PinCommand::Load);
    }

    /// Drops a queued save, any outstanding load or keyring retry, and any
    /// unmerged read; nothing is read or written until the next `request_load`.
    pub fn hold(&self) {
        let _ = self.inner.sender.send(PinCommand::Hold);
        set_loaded(&self.inner.loaded, None);
    }

    /// The latest finished read waiting to be merged; `Some` marks a finished
    /// read, including one that found nothing.
    pub fn take_loaded(&self) -> Option<Vec<SavedPin>> {
        self.inner
            .loaded
            .lock()
            .expect("pin store load mutex is not poisoned")
            .take()
    }

    /// Marks the store for re-saving after an explicit reset.
    pub fn request_resave(&self) {
        *self
            .inner
            .resave
            .lock()
            .expect("pin store resave mutex is not poisoned") = true;
    }

    /// Takes a pending reset-triggered re-save.
    pub fn take_resave(&self) -> bool {
        std::mem::take(
            &mut *self
                .inner
                .resave
                .lock()
                .expect("pin store resave mutex is not poisoned"),
        )
    }

    /// Deletes the file and the keyring item.
    pub fn wipe(&self) -> Result<PinWipeReport, PinStoreError> {
        let (response, receiver) = mpsc::channel();
        self.inner
            .sender
            .send(PinCommand::Wipe { response })
            .map_err(|_| PinStoreError::Io)?;
        match receiver.recv_timeout(WIPE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(PinStoreError::Io),
        }
    }

    /// Stops the thread and waits for it.
    pub fn shutdown(&self) {
        let _ = self.inner.sender.send(PinCommand::Stop);
        if let Some(worker) = self
            .inner
            .worker
            .lock()
            .expect("pin store worker mutex is not poisoned")
            .take()
        {
            let _ = worker.join();
        }
    }
}

impl Drop for PinPersistenceInner {
    fn drop(&mut self) {
        let _ = self.sender.send(PinCommand::Stop);
    }
}

fn run_pin_worker(
    path: PathBuf,
    key_store: Arc<dyn PinKeyStore>,
    receiver: Receiver<PinCommand>,
    loaded: LoadedSlot,
    status: Arc<Mutex<PinStoreStatus>>,
) {
    let mut pending: Option<Vec<SavedPin>> = None;
    let mut deadline: Option<Instant> = None;
    let mut needs_load = false;
    let mut retry_at = Instant::now();

    loop {
        let now = Instant::now();
        if needs_load && now >= retry_at {
            match load(&path, key_store.as_ref()) {
                LoadOutcome::Loaded(pins) => {
                    let bytes = pins.iter().map(|pin| pin.text().len()).sum();
                    set_status(
                        &status,
                        PinStoreStatus::saved(PinStoreState::Saved, pins.len(), bytes),
                    );
                    set_loaded(&loaded, Some(pins));
                    needs_load = false;
                }
                LoadOutcome::Empty => {
                    set_status(&status, PinStoreStatus::new(PinStoreState::Idle));
                    set_loaded(&loaded, Some(Vec::new()));
                    needs_load = false;
                }
                LoadOutcome::Unavailable(state) => {
                    set_status(&status, PinStoreStatus::new(state));
                    retry_at = now + PIN_RETRY_INTERVAL;
                }
            }
        }

        let mut wait = Duration::from_secs(3600);
        if let Some(deadline) = deadline {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        if needs_load {
            wait = wait.min(retry_at.saturating_duration_since(Instant::now()));
        }

        match receiver.recv_timeout(wait) {
            Ok(PinCommand::Save(pins)) => {
                let bytes = pins.iter().map(|pin| pin.text().len()).sum();
                set_status(
                    &status,
                    PinStoreStatus::saved(PinStoreState::Saving, pins.len(), bytes),
                );
                pending = Some(pins);
                deadline = Some(Instant::now() + PIN_SAVE_DEBOUNCE);
            }
            Ok(PinCommand::Hold) => {
                // A read that finished while the hold was queued is dropped here.
                pending = None;
                deadline = None;
                needs_load = false;
                set_loaded(&loaded, None);
            }
            Ok(PinCommand::Load) => {
                // A reload follows a lock or a fresh start; queued content must
                // not survive it.
                pending = None;
                deadline = None;
                needs_load = true;
                retry_at = Instant::now();
            }
            Ok(PinCommand::Wipe { response }) => {
                pending = None;
                deadline = None;
                needs_load = false;
                let report = wipe(&path, key_store.as_ref(), &status);
                let _ = response.send(report);
            }
            Ok(PinCommand::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            deadline = None;
            if let Some(pins) = pending.take() {
                write(&path, key_store.as_ref(), &pins, &status);
            }
        }
    }
}

enum LoadOutcome {
    Loaded(Vec<SavedPin>),
    Empty,
    Unavailable(PinStoreState),
}

fn load(path: &std::path::Path, key_store: &dyn PinKeyStore) -> LoadOutcome {
    if !pin_store::pins_file_present(path) {
        return LoadOutcome::Empty;
    }
    let key = match key_store.get() {
        Ok(Some(key)) => key,
        // A file without its key cannot be decrypted; treat it like key loss.
        Ok(None) => return LoadOutcome::Unavailable(PinStoreState::Unreadable),
        Err(error) => return LoadOutcome::Unavailable(read_key_state(error)),
    };
    match pin_store::read_pins(path, &key) {
        Ok(Some(pins)) => LoadOutcome::Loaded(pins),
        Ok(None) => LoadOutcome::Empty,
        Err(PinStoreError::Unreadable | PinStoreError::UnsupportedVersion) => {
            LoadOutcome::Unavailable(PinStoreState::Unreadable)
        }
        Err(_) => LoadOutcome::Unavailable(PinStoreState::ReadFailed),
    }
}

fn write(
    path: &std::path::Path,
    key_store: &dyn PinKeyStore,
    pins: &[SavedPin],
    status: &Arc<Mutex<PinStoreStatus>>,
) {
    if pins.is_empty() {
        match pin_store::delete_pins(path) {
            Ok(_) => set_status(status, PinStoreStatus::new(PinStoreState::Saved)),
            Err(_) => set_status(status, PinStoreStatus::new(PinStoreState::Failed)),
        }
        return;
    }
    let key = match key_store.get() {
        Ok(Some(key)) => key,
        Ok(None) => match key_store.create() {
            Ok(key) => key,
            Err(error) => {
                set_status(status, PinStoreStatus::new(key_state(error)));
                return;
            }
        },
        Err(error) => {
            set_status(status, PinStoreStatus::new(key_state(error)));
            return;
        }
    };
    let bytes = pins.iter().map(|pin| pin.text().len()).sum();
    match pin_store::write_pins(path, &key, pins) {
        Ok(_) => set_status(
            status,
            PinStoreStatus::saved(PinStoreState::Saved, pins.len(), bytes),
        ),
        Err(PinStoreError::Unreadable | PinStoreError::UnsupportedVersion) => {
            set_status(status, PinStoreStatus::new(PinStoreState::Unreadable));
        }
        Err(_) => set_status(status, PinStoreStatus::new(PinStoreState::Failed)),
    }
}

fn wipe(
    path: &std::path::Path,
    key_store: &dyn PinKeyStore,
    status: &Arc<Mutex<PinStoreStatus>>,
) -> Result<PinWipeReport, PinStoreError> {
    let previous = status
        .lock()
        .expect("pin store status mutex is not poisoned")
        .clone();
    pin_store::delete_pins(path)?;
    if pin_store::pins_file_present(path) {
        return Err(PinStoreError::Io);
    }
    let key_result = key_store.delete();
    let key_error = key_result.err();
    set_status(status, PinStoreStatus::new(PinStoreState::Idle));
    Ok(PinWipeReport {
        saved_count: previous.saved_count,
        saved_bytes: previous.saved_bytes,
        key_removed: key_error.is_none(),
        key_error,
    })
}

fn key_state(error: PinKeyError) -> PinStoreState {
    match error {
        PinKeyError::Unavailable => PinStoreState::Unavailable,
        PinKeyError::Locked => PinStoreState::Locked,
        PinKeyError::Failed => PinStoreState::Failed,
    }
}

fn read_key_state(error: PinKeyError) -> PinStoreState {
    match error {
        PinKeyError::Unavailable => PinStoreState::Unavailable,
        PinKeyError::Locked => PinStoreState::Locked,
        PinKeyError::Failed => PinStoreState::ReadFailed,
    }
}

fn set_loaded(slot: &LoadedSlot, value: Option<Vec<SavedPin>>) {
    *slot.lock().expect("pin store load mutex is not poisoned") = value;
}

fn set_status(status: &Arc<Mutex<PinStoreStatus>>, value: PinStoreStatus) {
    *status
        .lock()
        .expect("pin store status mutex is not poisoned") = value;
}
