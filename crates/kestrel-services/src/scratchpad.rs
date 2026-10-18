//! Private, bounded tabbed scratchpad with debounced persistence.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kestrel_core::ScratchpadConfiguration;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

pub const FEATURE_ID: &str = "notes.scratchpad";
const FILE_VERSION: u32 = 1;
const AUTOSAVE_DELAY: Duration = Duration::from_millis(750);
const MAX_TITLE_BYTES: usize = 256;
const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteId(u64);
impl NoteId {
    pub fn get(self) -> u64 {
        self.0
    }
}
impl From<u64> for NoteId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchpadNote {
    pub id: NoteId,
    pub title: String,
    pub text: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadSnapshot {
    pub notes: Vec<ScratchpadNote>,
    pub active: Option<NoteId>,
    pub expired: Vec<NoteId>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchpadError {
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    InvalidNote,
    UnknownNote,
    LimitExceeded,
    NoNotes,
    SaveFailed(String),
}
impl fmt::Display for ScratchpadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, .. } => write!(
                f,
                "scratchpad {operation} failed; check permissions and available space"
            ),
            Self::InvalidNote => f.write_str("scratchpad note is invalid"),
            Self::UnknownNote => f.write_str("scratchpad note was not found; refresh the tabs"),
            Self::LimitExceeded => {
                f.write_str("scratchpad limit exceeded; shorten the note or remove a tab")
            }
            Self::NoNotes => f.write_str("scratchpad has no notes"),
            Self::SaveFailed(reason) => write!(
                f,
                "scratchpad could not be saved: {reason}; check the data directory permissions and available space"
            ),
        }
    }
}
impl std::error::Error for ScratchpadError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskState {
    version: u32,
    notes: Vec<ScratchpadNote>,
    active: Option<NoteId>,
}

#[derive(Debug, Clone)]
struct State {
    notes: Vec<ScratchpadNote>,
    active: Option<NoteId>,
    next_id: u64,
}
impl State {
    fn empty() -> Self {
        Self {
            notes: Vec::new(),
            active: None,
            next_id: 1,
        }
    }
    fn disk(&self) -> DiskState {
        DiskState {
            version: FILE_VERSION,
            notes: self.notes.clone(),
            active: self.active,
        }
    }
}

#[derive(Debug)]
struct SaveMessage {
    state: State,
    flush: Option<mpsc::Sender<Result<(), ScratchpadError>>>,
}

enum WorkerMessage {
    Save(SaveMessage),
    Stop(mpsc::Sender<Result<(), ScratchpadError>>),
}

/// A bounded scratchpad whose state is persisted privately in one JSON file.
pub struct ScratchpadService {
    policy: ScratchpadConfiguration,
    state: State,
    warning: Option<String>,
    /// The latest autosave failure; a later successful save clears it.
    autosave_error: Arc<Mutex<Option<String>>>,
    sender: mpsc::Sender<WorkerMessage>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ScratchpadService {
    pub fn open(path: PathBuf, policy: ScratchpadConfiguration) -> Result<Self, ScratchpadError> {
        policy
            .validate()
            .map_err(|_| ScratchpadError::LimitExceeded)?;
        let (state, warning) = load_state(&path, &policy)?;
        let (sender, receiver) = mpsc::channel();
        let worker_path = path;
        let autosave_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&autosave_error);
        let worker = thread::Builder::new()
            .name("kestrel-scratchpad".to_string())
            .spawn(move || worker_loop(worker_path, receiver, worker_error))
            .map_err(|error| ScratchpadError::SaveFailed(error.to_string()))?;
        Ok(Self {
            policy,
            state,
            warning,
            autosave_error,
            sender,
            worker: Some(worker),
        })
    }

    /// New bounds apply to later edits and expiry; stored notes stay.
    pub fn set_policy(&mut self, policy: ScratchpadConfiguration) {
        self.policy = policy;
    }

    pub fn snapshot(&self, now: SystemTime) -> ScratchpadSnapshot {
        let expired = if self.policy.max_age_days == 0 {
            Vec::new()
        } else {
            let age = Duration::from_secs(u64::from(self.policy.max_age_days) * 86_400);
            self.state
                .notes
                .iter()
                .filter(|note| {
                    now.duration_since(
                        UNIX_EPOCH
                            .checked_add(Duration::from_millis(note.updated_at))
                            .unwrap_or(UNIX_EPOCH),
                    )
                    .is_ok_and(|elapsed| elapsed > age)
                })
                .map(|note| note.id)
                .collect()
        };
        ScratchpadSnapshot {
            notes: self.state.notes.clone(),
            active: self.state.active,
            expired,
            warning: self.warning.clone().or_else(|| {
                self.autosave_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            }),
        }
    }
    pub fn notes(&self) -> &[ScratchpadNote] {
        &self.state.notes
    }

    pub fn create(
        &mut self,
        title: impl Into<String>,
        now: SystemTime,
    ) -> Result<NoteId, ScratchpadError> {
        if self.state.notes.len() >= self.policy.max_notes as usize {
            return Err(ScratchpadError::LimitExceeded);
        }
        let title = clean_title(title.into());
        if title.is_empty() {
            return Err(ScratchpadError::InvalidNote);
        }
        let stamp = millis(now);
        let id = NoteId(self.state.next_id);
        self.state.next_id = self.state.next_id.saturating_add(1);
        self.state.notes.push(ScratchpadNote {
            id,
            title,
            text: String::new(),
            created_at: stamp,
            updated_at: stamp,
        });
        self.state.active = Some(id);
        self.schedule_save();
        Ok(id)
    }
    pub fn rename(
        &mut self,
        id: NoteId,
        title: impl Into<String>,
        now: SystemTime,
    ) -> Result<(), ScratchpadError> {
        let title = clean_title(title.into());
        if title.is_empty() {
            return Err(ScratchpadError::InvalidNote);
        }
        let note = self.note_mut(id)?;
        note.title = title;
        note.updated_at = millis(now);
        self.schedule_save();
        Ok(())
    }
    pub fn switch(&mut self, id: NoteId) -> Result<(), ScratchpadError> {
        if !self.state.notes.iter().any(|note| note.id == id) {
            return Err(ScratchpadError::UnknownNote);
        }
        self.state.active = Some(id);
        self.schedule_save();
        Ok(())
    }
    pub fn close(&mut self, id: NoteId) -> Result<(), ScratchpadError> {
        let index = self
            .state
            .notes
            .iter()
            .position(|note| note.id == id)
            .ok_or(ScratchpadError::UnknownNote)?;
        self.state.notes.remove(index);
        self.state.active = self
            .state
            .notes
            .get(index)
            .or_else(|| self.state.notes.last())
            .map(|note| note.id);
        self.schedule_save();
        Ok(())
    }
    pub fn update_text(
        &mut self,
        id: NoteId,
        text: impl Into<String>,
        now: SystemTime,
    ) -> Result<(), ScratchpadError> {
        let text = text.into();
        self.validate_text(id, &text)?;
        let note = self.note_mut(id)?;
        note.text = text;
        note.updated_at = millis(now);
        self.schedule_save();
        Ok(())
    }
    pub fn remove_expired(&mut self, now: SystemTime) -> Result<usize, ScratchpadError> {
        let ids = self.snapshot(now).expired;
        if ids.is_empty() {
            return Ok(0);
        }
        self.state.notes.retain(|note| !ids.contains(&note.id));
        if self
            .state
            .active
            .is_some_and(|id| !self.state.notes.iter().any(|note| note.id == id))
        {
            self.state.active = self.state.notes.first().map(|note| note.id);
        }
        self.schedule_save();
        Ok(ids.len())
    }
    pub fn export(&self, id: NoteId, destination: &Path) -> Result<(), ScratchpadError> {
        let note = self
            .state
            .notes
            .iter()
            .find(|note| note.id == id)
            .ok_or(ScratchpadError::UnknownNote)?;
        fs::write(destination, note.text.as_bytes()).map_err(|error| ScratchpadError::Io {
            operation: "export",
            kind: error.kind(),
        })
    }
    pub fn flush(&mut self) -> Result<(), ScratchpadError> {
        let (reply, receive) = mpsc::channel();
        let result = self
            .sender
            .send(WorkerMessage::Save(SaveMessage {
                state: self.state.clone(),
                flush: Some(reply),
            }))
            .map_err(|_| ScratchpadError::SaveFailed("save worker stopped".into()))
            .and_then(|_| {
                receive.recv().unwrap_or_else(|_| {
                    Err(ScratchpadError::SaveFailed("save worker stopped".into()))
                })
            });
        if let Err(error) = &result {
            self.warning = Some(error.to_string());
        }
        result
    }
    pub fn stop(&mut self) -> Result<(), ScratchpadError> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let (reply, receive) = mpsc::channel();
        let result = self
            .sender
            .send(WorkerMessage::Stop(reply))
            .map_err(|_| ScratchpadError::SaveFailed("save worker stopped".into()))
            .and_then(|_| {
                receive.recv().unwrap_or_else(|_| {
                    Err(ScratchpadError::SaveFailed("save worker stopped".into()))
                })
            });
        if let Err(error) = &result {
            self.warning = Some(error.to_string());
        }
        let _ = worker.join();
        result
    }
    fn schedule_save(&self) {
        let _ = self.sender.send(WorkerMessage::Save(SaveMessage {
            state: self.state.clone(),
            flush: None,
        }));
    }
    fn note_mut(&mut self, id: NoteId) -> Result<&mut ScratchpadNote, ScratchpadError> {
        self.state
            .notes
            .iter_mut()
            .find(|note| note.id == id)
            .ok_or(ScratchpadError::UnknownNote)
    }
    fn validate_text(&self, id: NoteId, text: &str) -> Result<(), ScratchpadError> {
        if text.len() > self.policy.max_note_bytes as usize || text.len() > MAX_INPUT_BYTES {
            return Err(ScratchpadError::LimitExceeded);
        }
        if !self.state.notes.iter().any(|note| note.id == id) {
            return Err(ScratchpadError::UnknownNote);
        }
        let total: usize = self
            .state
            .notes
            .iter()
            .filter(|note| note.id != id)
            .map(|note| note.text.len())
            .sum::<usize>()
            + text.len();
        if total > self.policy.max_note_bytes as usize * self.policy.max_notes as usize {
            return Err(ScratchpadError::LimitExceeded);
        }
        Ok(())
    }
}
impl Drop for ScratchpadService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn worker_loop(
    path: PathBuf,
    receiver: mpsc::Receiver<WorkerMessage>,
    autosave_error: Arc<Mutex<Option<String>>>,
) {
    let record = |result: &Result<(), ScratchpadError>| {
        *autosave_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            result.as_ref().err().map(ToString::to_string);
    };
    let mut pending: Option<SaveMessage> = None;
    loop {
        let message = if pending.is_some() {
            match receiver.recv_timeout(AUTOSAVE_DELAY) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(save) = pending.take() {
                        record(&save_state(&path, &save.state));
                    }
                    None
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        };
        let Some(message) = message else { continue };
        match message {
            WorkerMessage::Save(save) => {
                if let Some(reply) = save.flush {
                    let result = save_state(&path, &save.state);
                    record(&result);
                    let _ = reply.send(result);
                } else {
                    pending = Some(save);
                }
            }
            WorkerMessage::Stop(reply) => {
                let result = pending
                    .take()
                    .map_or(Ok(()), |save| save_state(&path, &save.state));
                let _ = reply.send(result);
                return;
            }
        }
    }
}

fn load_state(
    path: &Path,
    policy: &ScratchpadConfiguration,
) -> Result<(State, Option<String>), ScratchpadError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((State::empty(), None)),
        Err(_) => {
            preserve_corrupt(path);
            return Ok((State::empty(), Some("Scratchpad data could not be read and was preserved; a new note can be created.".into())));
        }
    };
    let parsed: DiskState = match serde_json::from_slice::<DiskState>(&data) {
        Ok(parsed) if parsed.version == FILE_VERSION => parsed,
        Ok(_) | Err(_) => {
            preserve_corrupt(path);
            return Ok((
                State::empty(),
                Some(
                    "Scratchpad data was corrupt and was preserved; a new note can be created."
                        .into(),
                ),
            ));
        }
    };
    let mut state = State::empty();
    let mut total_bytes = 0usize;
    for note in parsed.notes {
        if state.notes.len() >= policy.max_notes as usize
            || note.title.is_empty()
            || note.title.len() > MAX_TITLE_BYTES
            || note.text.len() > policy.max_note_bytes as usize
            || note.text.len() > MAX_INPUT_BYTES
            || total_bytes.saturating_add(note.text.len())
                > policy.max_note_bytes as usize * policy.max_notes as usize
        {
            continue;
        }
        total_bytes = total_bytes.saturating_add(note.text.len());
        state.next_id = state.next_id.max(note.id.0.saturating_add(1));
        state.notes.push(note);
    }
    state.active = parsed
        .active
        .filter(|id| state.notes.iter().any(|note| note.id == *id))
        .or_else(|| state.notes.first().map(|note| note.id));
    Ok((state, None))
}
fn save_state(path: &Path, state: &State) -> Result<(), ScratchpadError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| ScratchpadError::Io {
        operation: "create data directory",
        kind: error.kind(),
    })?;
    set_mode(parent, 0o700).map_err(|error| ScratchpadError::Io {
        operation: "secure data directory",
        kind: error.kind(),
    })?;
    let temp = parent.join(format!(
        ".scratchpad.json.tmp-{}",
        millis(SystemTime::now())
    ));
    let bytes = serde_json::to_vec_pretty(&state.disk())
        .map_err(|error| ScratchpadError::SaveFailed(error.to_string()))?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| ScratchpadError::Io {
                operation: "create temporary file",
                kind: error.kind(),
            })?;
        set_mode(&temp, 0o600).map_err(|error| ScratchpadError::Io {
            operation: "secure temporary file",
            kind: error.kind(),
        })?;
        file.write_all(&bytes)
            .map_err(|error| ScratchpadError::Io {
                operation: "write",
                kind: error.kind(),
            })?;
        file.sync_all().map_err(|error| ScratchpadError::Io {
            operation: "sync",
            kind: error.kind(),
        })?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| ScratchpadError::Io {
            operation: "replace",
            kind: error.kind(),
        })?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn preserve_corrupt(path: &Path) {
    let millis = millis(SystemTime::now());
    let destination = path.with_file_name(format!("scratchpad.json.corrupt-{millis}"));
    if fs::rename(path, &destination).is_ok() {
        let _ = set_mode(&destination, 0o600);
    }
}
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn clean_title(title: String) -> String {
    bounded_prefix(title.trim(), MAX_TITLE_BYTES).to_owned()
}

/// Renders bounded Markdown into escaped Pango markup.
pub fn render_markdown(markdown: &str) -> String {
    let input = bounded_prefix(markdown, MAX_INPUT_BYTES);
    let mut output = String::new();
    let mut list_depth = 0usize;
    for event in Parser::new_ext(input, Options::all()) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                output.push_str(if level == HeadingLevel::H1 {
                    "<big><b>"
                } else {
                    "<b>"
                });
            }
            Event::End(TagEnd::Heading(level)) => {
                output.push_str("</b>");
                if level == HeadingLevel::H1 {
                    output.push_str("</big>");
                }
                output.push_str("\n\n");
            }
            Event::End(TagEnd::Paragraph) => {
                output.push_str(if list_depth > 0 { "" } else { "\n\n" });
            }
            Event::Start(Tag::BlockQuote(_)) => output.push_str("<i>"),
            Event::End(TagEnd::BlockQuote(_)) => output.push_str("</i>\n"),
            Event::Start(Tag::Emphasis) => output.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => output.push_str("</i>"),
            Event::Start(Tag::Strong) => output.push_str("<b>"),
            Event::End(TagEnd::Strong) => output.push_str("</b>"),
            Event::Start(Tag::Strikethrough) => output.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => output.push_str("</s>"),
            Event::Start(Tag::CodeBlock(kind)) => {
                output.push_str("<tt>");
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    output.push('\n');
                }
            }
            Event::End(TagEnd::CodeBlock) => output.push_str("</tt>\n"),
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    output.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                output.push_str(&"  ".repeat(list_depth.saturating_sub(1)));
                output.push_str("• ");
            }
            Event::End(TagEnd::Item) => output.push('\n'),
            Event::Start(Tag::Link { .. }) => output.push_str("<u>"),
            Event::End(TagEnd::Link) => output.push_str("</u>"),
            Event::Text(text) => escape_pango(&text, &mut output),
            Event::Code(text) => {
                output.push_str("<tt>");
                escape_pango(&text, &mut output);
                output.push_str("</tt>");
            }
            Event::SoftBreak | Event::HardBreak => output.push('\n'),
            Event::Rule => output.push_str("────────\n"),
            Event::Html(text) | Event::InlineHtml(text) | Event::FootnoteReference(text) => {
                escape_pango(&text, &mut output);
            }
            Event::TaskListMarker(checked) => {
                output.push_str(if checked { "☑ " } else { "☐ " })
            }
            _ => {}
        }
    }
    output.trim_end().to_owned()
}
fn escape_pango(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}
fn bounded_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;
    fn policy() -> ScratchpadConfiguration {
        ScratchpadConfiguration::default()
    }
    #[test]
    fn markdown_escapes_markup_and_renders_constructs() {
        let rendered =
            render_markdown("# <x>\n\n*em* **strong** `code`\n\n[link](https://example.com)");
        assert!(rendered.contains("&lt;x&gt;"));
        assert!(rendered.contains("<i>em</i>"));
        assert!(rendered.contains("<b>strong</b>"));
        assert!(rendered.contains("<u>link</u>"));
    }
    #[test]
    fn storage_round_trip_and_bounds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scratchpad.json");
        let mut service = ScratchpadService::open(path.clone(), policy()).unwrap();
        let id = service.create("one", SystemTime::now()).unwrap();
        service.update_text(id, "hello", SystemTime::now()).unwrap();
        service.flush().unwrap();
        drop(service);
        let service = ScratchpadService::open(path, policy()).unwrap();
        assert_eq!(service.notes()[0].text, "hello");
    }
    #[test]
    fn corrupt_file_is_preserved() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scratchpad.json");
        fs::write(&path, b"broken").unwrap();
        let service = ScratchpadService::open(path, policy()).unwrap();
        assert!(service.snapshot(SystemTime::now()).warning.is_some());
        assert!(fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("corrupt-")
        }));
    }
    #[test]
    fn debounce_flushes_on_drop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scratchpad.json");
        let mut service = ScratchpadService::open(path.clone(), policy()).unwrap();
        let id = service.create("one", SystemTime::now()).unwrap();
        service.update_text(id, "saved", SystemTime::now()).unwrap();
        drop(service);
        let service = ScratchpadService::open(path, policy()).unwrap();
        assert_eq!(service.notes()[0].text, "saved");
    }
    #[test]
    fn expired_notes_are_listed_and_removed_explicitly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scratchpad.json");
        let mut config = policy();
        config.max_age_days = 1;
        let mut service = ScratchpadService::open(path, config).unwrap();
        let id = service
            .create("old", UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            service
                .snapshot(UNIX_EPOCH + Duration::from_secs(90_000))
                .expired,
            vec![id]
        );
        assert_eq!(
            service
                .remove_expired(UNIX_EPOCH + Duration::from_secs(90_000))
                .unwrap(),
            1
        );
    }

    #[test]
    fn saved_notes_are_private_and_edits_stay_within_bounds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("kestrel");
        let path = data.join("scratchpad.json");
        let mut config = policy();
        config.max_notes = 1;
        config.max_note_bytes = 1024;
        let mut service = ScratchpadService::open(path.clone(), config).unwrap();
        let id = service.create("one", SystemTime::now()).unwrap();
        assert_eq!(
            service.create("two", SystemTime::now()),
            Err(ScratchpadError::LimitExceeded)
        );
        assert_eq!(
            service.update_text(id, "x".repeat(1025), SystemTime::now()),
            Err(ScratchpadError::LimitExceeded)
        );
        service.update_text(id, "kept", SystemTime::now()).unwrap();
        service.flush().unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data), 0o700);
        assert_eq!(service.notes()[0].text, "kept");
    }

    #[test]
    fn autosave_failures_are_reported_until_a_save_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scratchpad.json");
        let mut service = ScratchpadService::open(path.clone(), policy()).unwrap();
        // A non-empty directory at the target makes the final rename fail.
        fs::create_dir(&path).unwrap();
        fs::write(path.join("blocker"), b"").unwrap();
        let id = service.create("one", SystemTime::now()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while service.snapshot(SystemTime::now()).warning.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "autosave failure surfaces"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        fs::remove_dir_all(&path).unwrap();
        service.update_text(id, "again", SystemTime::now()).unwrap();
        service.flush().unwrap();
        assert_eq!(service.snapshot(SystemTime::now()).warning, None);
    }

    #[test]
    fn markdown_keeps_paragraphs_apart() {
        assert_eq!(
            render_markdown("first\n\nsecond\n\n- a\n- b"),
            "first\n\nsecond\n\n• a\n• b"
        );
    }
}
