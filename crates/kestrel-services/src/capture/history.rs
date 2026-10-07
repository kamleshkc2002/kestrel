//! On-disk capture history: private PNG files bounded by count, total bytes, and age.

use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kestrel_core::CaptureConfiguration;
use kestrel_platform::capture::{
    CaptureMode, CaptureProvider, MAX_CAPTURE_DIMENSION, MAX_CAPTURE_PNG_BYTES,
};

use super::image::decode_png;

/// Retention bounds for stored captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePolicy {
    pub max_entries: u32,
    pub max_total_bytes: u64,
    pub max_age: Duration,
}

impl CapturePolicy {
    /// Retention bounds from the user's capture configuration.
    pub fn from_configuration(configuration: &CaptureConfiguration) -> Self {
        Self {
            max_entries: configuration.max_entries,
            max_total_bytes: configuration.max_total_bytes(),
            max_age: Duration::from_secs(u64::from(configuration.max_age_hours) * 3600),
        }
    }
}

/// Stable identifier of one stored capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureId(u64);

impl CaptureId {
    /// The numeric value used in the file name.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Metadata of one stored capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEntry {
    pub id: CaptureId,
    pub created_at: SystemTime,
    pub mode: CaptureMode,
    /// Known only for captures taken in this session.
    pub provider: Option<CaptureProvider>,
    pub edited: bool,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

/// Metadata supplied with a PNG passed to [`CaptureHistory::insert`].
#[derive(Debug, Clone, Copy)]
pub struct NewCapture {
    pub mode: CaptureMode,
    pub provider: Option<CaptureProvider>,
    pub edited: bool,
    pub width: u32,
    pub height: u32,
}

/// Capture storage failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    TooLarge,
    UnknownCapture,
    InvalidImage,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, .. } => write!(
                formatter,
                "storage {operation} failed; check the capture directory permissions and available space"
            ),
            Self::TooLarge => formatter.write_str(
                "capture exceeds the configured storage limit; reduce the image size or increase the limit",
            ),
            Self::UnknownCapture => {
                formatter.write_str("capture was not found; refresh the capture history")
            }
            Self::InvalidImage => formatter.write_str("capture is not a valid PNG; capture it again"),
        }
    }
}

impl std::error::Error for HistoryError {}

/// Capture PNGs in one private directory, listed newest first.
///
/// Files are named `<created-ms>-<id>-<mode>[-edited].png`; other files are left alone.
pub struct CaptureHistory {
    dir: PathBuf,
    policy: CapturePolicy,
    records: Vec<CaptureEntry>,
    paths: HashMap<CaptureId, PathBuf>,
    next_id: u64,
}

impl CaptureHistory {
    /// Opens or creates `dir` as mode 0700, loads valid captures, removes temporary and
    /// invalid capture files, and applies `policy`.
    pub fn open(dir: PathBuf, policy: CapturePolicy) -> Result<Self, HistoryError> {
        fs::create_dir_all(&dir).map_err(|error| io_error("create capture directory", error))?;
        set_mode(&dir, 0o700).map_err(|error| io_error("secure capture directory", error))?;
        let mut records = Vec::new();
        let mut paths = HashMap::new();
        let mut next_id = 0;
        let listing =
            fs::read_dir(&dir).map_err(|error| io_error("scan capture directory", error))?;
        for item in listing {
            let path = item
                .map_err(|error| io_error("scan capture directory", error))?
                .path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name.ends_with(".tmp") {
                let _ = fs::remove_file(&path);
                continue;
            }
            let Some(name) = CaptureName::parse(file_name) else {
                continue;
            };
            next_id = next_id.max(name.id.0.saturating_add(1));
            let Some(entry) = load_entry(&path, name) else {
                let _ = fs::remove_file(&path);
                continue;
            };
            if paths.contains_key(&entry.id) {
                continue;
            }
            paths.insert(entry.id, path);
            records.push(entry);
        }
        sort_newest_first(&mut records);
        let mut history = Self {
            dir,
            policy,
            records,
            paths,
            next_id,
        };
        history.enforce_policy(SystemTime::now());
        Ok(history)
    }

    /// Stored captures, newest first.
    pub fn entries(&self) -> &[CaptureEntry] {
        &self.records
    }

    /// Sum of stored PNG sizes.
    pub fn total_bytes(&self) -> u64 {
        self.records.iter().map(|entry| entry.bytes).sum()
    }

    /// Validates and stores `png` as a 0600 file, then evicts older captures over the
    /// count or byte bound. The new capture itself is never evicted.
    pub fn insert(
        &mut self,
        png: &[u8],
        meta: NewCapture,
        now: SystemTime,
    ) -> Result<CaptureEntry, HistoryError> {
        if png.len() as u64 > self.policy.max_total_bytes || png.len() > MAX_CAPTURE_PNG_BYTES {
            return Err(HistoryError::TooLarge);
        }
        let image = decode_png(png).map_err(|_| HistoryError::InvalidImage)?;
        if image.width != meta.width || image.height != meta.height {
            return Err(HistoryError::InvalidImage);
        }
        let id = CaptureId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let name = CaptureName {
            created_ms: millis(now),
            id,
            mode: meta.mode,
            edited: meta.edited,
        };
        let path = self.dir.join(name.file_name());
        write_private(&path, png)?;
        let entry = CaptureEntry {
            id,
            created_at: now,
            mode: meta.mode,
            provider: meta.provider,
            edited: meta.edited,
            bytes: png.len() as u64,
            width: meta.width,
            height: meta.height,
        };
        self.paths.insert(id, path);
        self.records.push(entry.clone());
        sort_newest_first(&mut self.records);
        self.evict(Some(id));
        Ok(entry)
    }

    /// Removes captures older than the age bound; returns how many were removed.
    pub fn prune(&mut self, now: SystemTime) -> usize {
        let before = self.records.len();
        let max_age = self.policy.max_age;
        let mut kept = Vec::with_capacity(before);
        for entry in self.records.drain(..) {
            // Timestamps in the future count as fresh.
            let fresh = now
                .duration_since(entry.created_at)
                .map_or(true, |age| age <= max_age);
            if fresh {
                kept.push(entry);
            } else if let Some(path) = self.paths.remove(&entry.id) {
                let _ = fs::remove_file(path);
            }
        }
        self.records = kept;
        before - self.records.len()
    }

    /// Replaces the policy and applies every bound immediately.
    pub fn set_policy(&mut self, policy: CapturePolicy, now: SystemTime) {
        self.policy = policy;
        self.enforce_policy(now);
    }

    /// The stored PNG bytes of `id`.
    pub fn read(&self, id: CaptureId) -> Result<Vec<u8>, HistoryError> {
        let path = self.paths.get(&id).ok_or(HistoryError::UnknownCapture)?;
        let data = fs::read(path).map_err(|error| io_error("read capture", error))?;
        if data.len() > MAX_CAPTURE_PNG_BYTES {
            return Err(HistoryError::TooLarge);
        }
        Ok(data)
    }

    /// File path of `id`, for thumbnails.
    pub fn path(&self, id: CaptureId) -> Option<PathBuf> {
        self.paths.get(&id).cloned()
    }

    /// Removes `id` and its file; unknown ids are ignored.
    pub fn delete(&mut self, id: CaptureId) {
        if let Some(index) = self.records.iter().position(|entry| entry.id == id) {
            self.records.remove(index);
            if let Some(path) = self.paths.remove(&id) {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Removes every capture and temporary file; reports the first removal failure.
    pub fn clear(&mut self) -> Result<(), HistoryError> {
        let mut failure = None;
        self.records.clear();
        for (_, path) in self.paths.drain() {
            if let Err(error) = fs::remove_file(path) {
                if error.kind() != io::ErrorKind::NotFound && failure.is_none() {
                    failure = Some(io_error("clear captures", error));
                }
            }
        }
        if let Ok(listing) = fs::read_dir(&self.dir) {
            for item in listing.flatten() {
                let path = item.path();
                let is_temporary = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".tmp"));
                if !is_temporary {
                    continue;
                }
                if let Err(error) = fs::remove_file(path) {
                    if failure.is_none() {
                        failure = Some(io_error("clear temporary captures", error));
                    }
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Atomically writes a copy of `id` to `destination`; on failure an existing
    /// destination is unchanged and no temporary file remains.
    pub fn export(&self, id: CaptureId, destination: &Path) -> Result<(), HistoryError> {
        let data = self.read(id)?;
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture");
        let temporary = parent.join(format!(".{name}.tmp"));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| io_error("create export temporary", error))?;
            // The exported file keeps the user's default permissions.
            file.write_all(&data)
                .map_err(|error| io_error("write export", error))?;
            file.sync_all()
                .map_err(|error| io_error("sync export", error))?;
            drop(file);
            fs::rename(&temporary, destination).map_err(|error| io_error("commit export", error))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn enforce_policy(&mut self, now: SystemTime) {
        self.prune(now);
        self.evict(None);
    }

    /// Removes the oldest captures, except `protected`, until count and byte bounds hold.
    fn evict(&mut self, protected: Option<CaptureId>) {
        while !self.records.is_empty()
            && (self.records.len() as u32 > self.policy.max_entries
                || self.total_bytes() > self.policy.max_total_bytes)
        {
            let oldest = self
                .records
                .iter()
                .rposition(|entry| Some(entry.id) != protected)
                .unwrap_or(self.records.len() - 1);
            let entry = self.records.remove(oldest);
            if let Some(path) = self.paths.remove(&entry.id) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Parsed capture file name.
#[derive(Debug, Clone, Copy)]
struct CaptureName {
    created_ms: u128,
    id: CaptureId,
    mode: CaptureMode,
    edited: bool,
}

impl CaptureName {
    fn parse(file_name: &str) -> Option<Self> {
        let parts: Vec<_> = file_name.strip_suffix(".png")?.split('-').collect();
        let edited = match parts.len() {
            3 => false,
            4 if parts[3] == "edited" => true,
            _ => return None,
        };
        Some(Self {
            created_ms: parts[0].parse().ok()?,
            id: CaptureId(parts[1].parse().ok()?),
            mode: CaptureMode::from_token(parts[2])?,
            edited,
        })
    }

    fn file_name(self) -> String {
        format!(
            "{}-{}-{}{}.png",
            self.created_ms,
            self.id.0,
            self.mode.token(),
            if self.edited { "-edited" } else { "" }
        )
    }
}

fn io_error(operation: &'static str, error: io::Error) -> HistoryError {
    HistoryError::Io {
        operation,
        kind: error.kind(),
    }
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn time_from_millis(millis: u128) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(u64::try_from(millis).ok()?))
}

fn sort_newest_first(records: &mut [CaptureEntry]) {
    records.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// Entry for a capture file whose size, PNG header, and timestamp are valid.
fn load_entry(path: &Path, name: CaptureName) -> Option<CaptureEntry> {
    let bytes = fs::metadata(path).ok()?.len();
    if bytes > MAX_CAPTURE_PNG_BYTES as u64 {
        return None;
    }
    // Only the header is read so opening stays fast with many captures.
    let (width, height) = png_dimensions(path)?;
    Some(CaptureEntry {
        id: name.id,
        created_at: time_from_millis(name.created_ms)?,
        mode: name.mode,
        provider: None,
        edited: name.edited,
        bytes,
        width,
        height,
    })
}

/// Width and height from a PNG header, within the capture bounds.
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = fs::File::open(path).ok()?;
    let reader = png::Decoder::new(io::BufReader::new(file))
        .read_info()
        .ok()?;
    let info = reader.info();
    let valid = |side: u32| (1..=MAX_CAPTURE_DIMENSION).contains(&side);
    (valid(info.width) && valid(info.height)).then_some((info.width, info.height))
}

/// Writes `data` to `path` with mode 0600 through a temporary file and rename.
fn write_private(path: &Path, data: &[u8]) -> Result<(), HistoryError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture");
    let temporary = path.with_file_name(format!("{file_name}.tmp"));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| io_error("create temporary capture", error))?;
        set_mode(&temporary, 0o600).map_err(|error| io_error("secure temporary capture", error))?;
        file.write_all(data)
            .map_err(|error| io_error("write capture", error))?;
        file.sync_all()
            .map_err(|error| io_error("sync capture", error))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|error| io_error("commit capture", error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::super::image::{RgbaImage, encode_png};
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn policy(max_entries: u32, max_total_bytes: u64) -> CapturePolicy {
        CapturePolicy {
            max_entries,
            max_total_bytes,
            max_age: DAY,
        }
    }

    fn roomy() -> CapturePolicy {
        policy(100, 64 * 1024 * 1024)
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let pixels = (0..width * height)
            .flat_map(|index| [index as u8, (index >> 8) as u8, 0x40, 255])
            .collect();
        encode_png(&RgbaImage::new(width, height, pixels).unwrap()).unwrap()
    }

    fn meta(width: u32, height: u32) -> NewCapture {
        NewCapture {
            mode: CaptureMode::Screen,
            provider: Some(CaptureProvider::Portal),
            edited: false,
            width,
            height,
        }
    }

    /// One hour ago plus `seconds`, at millisecond precision so file names round-trip.
    fn moment(seconds: u64) -> SystemTime {
        let now_ms = millis(SystemTime::now()) as u64;
        UNIX_EPOCH + Duration::from_millis(now_ms) - Duration::from_secs(3600)
            + Duration::from_secs(seconds)
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn ids(history: &CaptureHistory) -> Vec<CaptureId> {
        history.entries().iter().map(|entry| entry.id).collect()
    }

    fn capture_dir(root: &TempDir) -> PathBuf {
        root.path().join("captures")
    }

    #[test]
    fn directory_is_created_private_and_existing_directory_is_tightened() {
        let root = TempDir::new().unwrap();
        let fresh = root.path().join("fresh/nested");
        CaptureHistory::open(fresh.clone(), roomy()).unwrap();
        assert_eq!(mode_of(&fresh), 0o700);

        let loose = root.path().join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();
        CaptureHistory::open(loose.clone(), roomy()).unwrap();
        assert_eq!(mode_of(&loose), 0o700);
    }

    #[test]
    fn capture_files_are_private_and_hold_the_png_bytes() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let bytes = png(5, 4);
        let entry = history.insert(&bytes, meta(5, 4), moment(0)).unwrap();
        let path = history.path(entry.id).unwrap();
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(history.read(entry.id).unwrap(), bytes);
        assert_eq!(file_names(&capture_dir(&root)).len(), 1);
    }

    #[test]
    fn mismatched_or_invalid_images_are_rejected_without_writing() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        assert_eq!(
            history.insert(&png(5, 4), meta(4, 5), moment(0)),
            Err(HistoryError::InvalidImage)
        );
        assert_eq!(
            history.insert(b"not a png", meta(1, 1), moment(0)),
            Err(HistoryError::InvalidImage)
        );
        assert!(history.entries().is_empty());
        assert!(file_names(&capture_dir(&root)).is_empty());
    }

    #[test]
    fn count_bound_evicts_oldest_and_keeps_newest() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), policy(2, u64::MAX)).unwrap();
        let bytes = png(3, 3);
        let oldest = history.insert(&bytes, meta(3, 3), moment(10)).unwrap();
        let middle = history.insert(&bytes, meta(3, 3), moment(20)).unwrap();
        let oldest_path = history.path(oldest.id).unwrap();
        let newest = history.insert(&bytes, meta(3, 3), moment(30)).unwrap();
        assert_eq!(ids(&history), vec![newest.id, middle.id]);
        assert!(!oldest_path.exists());
        assert_eq!(file_names(&capture_dir(&root)).len(), 2);
    }

    #[test]
    fn byte_bound_evicts_until_under_limit_and_spares_the_new_entry() {
        let root = TempDir::new().unwrap();
        let bytes = png(8, 8);
        let size = bytes.len() as u64;
        let limit = size * 2 + size / 2;
        let mut history = CaptureHistory::open(capture_dir(&root), policy(100, limit)).unwrap();
        let first = history.insert(&bytes, meta(8, 8), moment(10)).unwrap();
        let second = history.insert(&bytes, meta(8, 8), moment(20)).unwrap();
        let third = history.insert(&bytes, meta(8, 8), moment(30)).unwrap();
        assert_eq!(ids(&history), vec![third.id, second.id]);
        assert!(history.total_bytes() <= limit);
        assert!(history.read(first.id).is_err());

        // Older than everything stored, yet kept because it was just inserted.
        let backdated = history.insert(&bytes, meta(8, 8), moment(0)).unwrap();
        assert_eq!(ids(&history), vec![third.id, backdated.id]);
        assert!(history.total_bytes() <= limit);
        assert_eq!(file_names(&capture_dir(&root)).len(), 2);
    }

    #[test]
    fn png_larger_than_byte_bound_is_rejected_without_writing() {
        let root = TempDir::new().unwrap();
        let bytes = png(8, 8);
        let mut history =
            CaptureHistory::open(capture_dir(&root), policy(100, bytes.len() as u64 - 1)).unwrap();
        assert_eq!(
            history.insert(&bytes, meta(8, 8), moment(0)),
            Err(HistoryError::TooLarge)
        );
        assert!(history.entries().is_empty());
        assert!(file_names(&capture_dir(&root)).is_empty());
    }

    #[test]
    fn age_bound_removes_expired_entries_on_prune() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let bytes = png(2, 2);
        let old = history.insert(&bytes, meta(2, 2), moment(0)).unwrap();
        let old_path = history.path(old.id).unwrap();
        let recent = history.insert(&bytes, meta(2, 2), moment(0) + DAY).unwrap();
        assert_eq!(history.prune(moment(0) + DAY + Duration::from_secs(1)), 1);
        assert_eq!(ids(&history), vec![recent.id]);
        assert!(!old_path.exists());
        assert_eq!(history.prune(moment(0) + DAY + Duration::from_secs(1)), 0);
    }

    #[test]
    fn age_bound_removes_expired_entries_on_open() {
        let root = TempDir::new().unwrap();
        let dir = capture_dir(&root);
        let short = CapturePolicy {
            max_age: Duration::from_secs(1800),
            ..roomy()
        };
        let mut history = CaptureHistory::open(dir.clone(), short).unwrap();
        let bytes = png(2, 2);
        history.insert(&bytes, meta(2, 2), moment(0)).unwrap();
        let fresh = history.insert(&bytes, meta(2, 2), moment(3000)).unwrap();
        drop(history);

        let reopened = CaptureHistory::open(dir.clone(), short).unwrap();
        assert_eq!(ids(&reopened), vec![fresh.id]);
        assert_eq!(file_names(&dir).len(), 1);
    }

    #[test]
    fn smaller_policy_evicts_immediately() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let bytes = png(4, 4);
        for second in 0..4 {
            history.insert(&bytes, meta(4, 4), moment(second)).unwrap();
        }
        let newest = history.entries()[0].id;
        history.set_policy(policy(1, u64::MAX), moment(10));
        assert_eq!(ids(&history), vec![newest]);
        assert_eq!(file_names(&capture_dir(&root)).len(), 1);

        history.set_policy(policy(100, bytes.len() as u64 - 1), moment(10));
        assert!(history.entries().is_empty());
        assert!(file_names(&capture_dir(&root)).is_empty());
    }

    #[test]
    fn reopen_restores_ids_order_and_dimensions() {
        let root = TempDir::new().unwrap();
        let dir = capture_dir(&root);
        let mut history = CaptureHistory::open(dir.clone(), roomy()).unwrap();
        history.insert(&png(4, 3), meta(4, 3), moment(1)).unwrap();
        history
            .insert(
                &png(5, 2),
                NewCapture {
                    mode: CaptureMode::Area,
                    edited: true,
                    ..meta(5, 2)
                },
                moment(2),
            )
            .unwrap();
        history
            .insert(
                &png(6, 1),
                NewCapture {
                    mode: CaptureMode::Window,
                    ..meta(6, 1)
                },
                moment(2),
            )
            .unwrap();
        let without_provider = |entries: &[CaptureEntry]| {
            entries
                .iter()
                .map(|entry| CaptureEntry {
                    provider: None,
                    ..entry.clone()
                })
                .collect::<Vec<_>>()
        };
        let before = without_provider(history.entries());
        let highest = before.iter().map(|entry| entry.id).max().unwrap();
        drop(history);

        let mut reopened = CaptureHistory::open(dir, roomy()).unwrap();
        assert_eq!(without_provider(reopened.entries()), before);
        let next = reopened.insert(&png(1, 1), meta(1, 1), moment(3)).unwrap();
        assert!(next.id > highest);
    }

    #[test]
    fn open_ignores_unrelated_files_and_removes_leftovers() {
        let root = TempDir::new().unwrap();
        let dir = capture_dir(&root);
        fs::create_dir(&dir).unwrap();
        let valid = CaptureName {
            created_ms: millis(moment(0)),
            id: CaptureId(4),
            mode: CaptureMode::Screen,
            edited: false,
        };
        fs::write(dir.join(valid.file_name()), png(3, 2)).unwrap();
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        fs::write(dir.join("holiday.png"), png(1, 1)).unwrap();
        fs::write(dir.join("1-2-sideways.png"), b"keep").unwrap();
        fs::write(dir.join("1-2-screen-copy.png"), b"keep").unwrap();
        fs::write(dir.join("123-9-area.png.tmp"), b"partial").unwrap();
        fs::write(dir.join(".export.png.tmp"), b"partial").unwrap();
        let corrupt = CaptureName {
            id: CaptureId(7),
            ..valid
        };
        fs::write(dir.join(corrupt.file_name()), b"not a png").unwrap();

        let history = CaptureHistory::open(dir.clone(), roomy()).unwrap();
        assert_eq!(ids(&history), vec![CaptureId(4)]);
        assert_eq!(
            (history.entries()[0].width, history.entries()[0].height),
            (3, 2)
        );
        let mut expected = vec![
            valid.file_name(),
            "1-2-screen-copy.png".to_owned(),
            "1-2-sideways.png".to_owned(),
            "holiday.png".to_owned(),
            "notes.txt".to_owned(),
        ];
        expected.sort();
        assert_eq!(file_names(&dir), expected);
        assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"keep");
    }

    #[test]
    fn duplicate_ids_on_disk_keep_metadata_and_file_together() {
        let root = TempDir::new().unwrap();
        let dir = capture_dir(&root);
        fs::create_dir(&dir).unwrap();
        let first = CaptureName {
            created_ms: millis(moment(0)),
            id: CaptureId(3),
            mode: CaptureMode::Screen,
            edited: false,
        };
        let second = CaptureName {
            mode: CaptureMode::Area,
            ..first
        };
        fs::write(dir.join(first.file_name()), png(2, 1)).unwrap();
        fs::write(dir.join(second.file_name()), png(1, 3)).unwrap();

        let history = CaptureHistory::open(dir, roomy()).unwrap();
        assert_eq!(ids(&history), vec![CaptureId(3)]);
        let entry = &history.entries()[0];
        let stored = decode_png(&history.read(entry.id).unwrap()).unwrap();
        assert_eq!((stored.width, stored.height), (entry.width, entry.height));
        assert_eq!(entry.bytes, history.read(entry.id).unwrap().len() as u64);
    }

    #[test]
    fn clear_removes_captures_and_temporaries_only() {
        let root = TempDir::new().unwrap();
        let dir = capture_dir(&root);
        let mut history = CaptureHistory::open(dir.clone(), roomy()).unwrap();
        history.insert(&png(2, 2), meta(2, 2), moment(0)).unwrap();
        history.insert(&png(2, 2), meta(2, 2), moment(1)).unwrap();
        fs::write(dir.join("orphan.png.tmp"), b"partial").unwrap();
        fs::write(dir.join("notes.txt"), b"keep").unwrap();

        history.clear().unwrap();
        assert!(history.entries().is_empty());
        assert_eq!(history.total_bytes(), 0);
        assert_eq!(file_names(&dir), vec!["notes.txt".to_owned()]);
    }

    #[test]
    fn delete_removes_the_file() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let kept = history.insert(&png(2, 2), meta(2, 2), moment(0)).unwrap();
        let deleted = history.insert(&png(2, 2), meta(2, 2), moment(1)).unwrap();
        let path = history.path(deleted.id).unwrap();
        history.delete(deleted.id);
        assert!(!path.exists());
        assert_eq!(ids(&history), vec![kept.id]);
        assert_eq!(history.read(deleted.id), Err(HistoryError::UnknownCapture));
    }

    #[test]
    fn read_of_unknown_id_is_unknown_capture() {
        let root = TempDir::new().unwrap();
        let history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        assert_eq!(
            history.read(CaptureId(42)),
            Err(HistoryError::UnknownCapture)
        );
    }

    #[test]
    fn export_writes_identical_bytes() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let bytes = png(7, 3);
        let entry = history.insert(&bytes, meta(7, 3), moment(0)).unwrap();
        let out = TempDir::new().unwrap();
        let destination = out.path().join("shot.png");
        fs::write(&destination, b"previous").unwrap();
        history.export(entry.id, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(file_names(out.path()), vec!["shot.png".to_owned()]);
    }

    #[test]
    fn failed_export_of_unknown_id_leaves_destination_unchanged() {
        let root = TempDir::new().unwrap();
        let history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let out = TempDir::new().unwrap();
        let destination = out.path().join("shot.png");
        fs::write(&destination, b"previous").unwrap();
        assert_eq!(
            history.export(CaptureId(9), &destination),
            Err(HistoryError::UnknownCapture)
        );
        assert_eq!(fs::read(&destination).unwrap(), b"previous");
        assert_eq!(file_names(out.path()), vec!["shot.png".to_owned()]);
    }

    #[test]
    fn failed_export_to_read_only_directory_leaves_destination_unchanged() {
        let root = TempDir::new().unwrap();
        let mut history = CaptureHistory::open(capture_dir(&root), roomy()).unwrap();
        let entry = history.insert(&png(2, 2), meta(2, 2), moment(0)).unwrap();
        let out = TempDir::new().unwrap();
        let destination = out.path().join("shot.png");
        fs::write(&destination, b"previous").unwrap();
        fs::set_permissions(out.path(), fs::Permissions::from_mode(0o555)).unwrap();
        // Privileged users bypass directory permissions; the case cannot be staged.
        let privileged = fs::write(out.path().join("probe"), b"").is_ok();
        let result = (!privileged).then(|| history.export(entry.id, &destination));
        fs::set_permissions(out.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let Some(result) = result else {
            return;
        };
        assert!(matches!(result, Err(HistoryError::Io { .. })), "{result:?}");
        assert_eq!(fs::read(&destination).unwrap(), b"previous");
        assert_eq!(file_names(out.path()), vec!["shot.png".to_owned()]);
    }
}
