//! Opt-in private storage for shelf entries.
//!
//! Only file references, text, and links are written; image bytes stay in
//! memory. An unreadable file is moved aside rather than overwritten.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kestrel_core::{ShelfContent, ShelfItem, ShelfLimits, shelf_content_fits, shelf_evict_index};
use serde::{Deserialize, Serialize};

/// The stored document's schema version.
const FILE_VERSION: u32 = 1;
const FILE_NAME: &str = "shelf.toml";

#[derive(Debug, Serialize, Deserialize)]
struct StoredShelf {
    version: u32,
    #[serde(default)]
    items: Vec<StoredItem>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredItem {
    id: u64,
    /// Seconds since the Unix epoch.
    added: u64,
    #[serde(default)]
    pinned: bool,
    content: StoredContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredContent {
    File { path: String, name: String },
    Text { text: String },
    Link { url: String },
}

/// The shelf's private file beside the other Kestrel data files.
pub fn shelf_store_path() -> Option<PathBuf> {
    crate::snippets::kestrel_data_directory().map(|directory| directory.join(FILE_NAME))
}

/// Reads stored entries within `limits`; a bad file is preserved and empty.
pub fn load(path: &Path, limits: &ShelfLimits) -> (Vec<ShelfItem>, Option<String>) {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(_) => {
            preserve_corrupt(path);
            return (
                Vec::new(),
                Some(
                    "The saved shelf could not be read. It was kept as shelf.toml.corrupt-<time> \
                     in the data folder, and the shelf starts empty."
                        .to_owned(),
                ),
            );
        }
    };
    let document = match std::str::from_utf8(&bytes)
        .ok()
        .and_then(|contents| toml::from_str::<StoredShelf>(contents).ok())
    {
        Some(document) if document.version == FILE_VERSION => document,
        Some(_) | None => {
            preserve_corrupt(path);
            return (
                Vec::new(),
                Some(
                    "The saved shelf is damaged. It was kept as shelf.toml.corrupt-<time> in the \
                     data folder, and the shelf starts empty."
                        .to_owned(),
                ),
            );
        }
    };
    let mut items = Vec::with_capacity(document.items.len());
    for item in document.items {
        let content = match item.content {
            StoredContent::File { path, name } => {
                if path.is_empty() {
                    continue;
                }
                ShelfContent::File {
                    path: PathBuf::from(path),
                    name,
                }
            }
            StoredContent::Text { text } => ShelfContent::Text(text),
            StoredContent::Link { url } => ShelfContent::Link { url },
        };
        if shelf_content_fits(&content, limits).is_err() {
            continue;
        }
        items.push(ShelfItem {
            id: item.id,
            added: UNIX_EPOCH
                .checked_add(Duration::from_secs(item.added))
                .unwrap_or(UNIX_EPOCH),
            pinned: item.pinned,
            content,
        });
    }
    while items.len() > limits.max_items as usize {
        match shelf_evict_index(&items) {
            Some(index) => {
                items.remove(index);
            }
            None => {
                items.truncate(limits.max_items as usize);
                break;
            }
        }
    }
    (items, None)
}

/// Writes persistable entries privately and atomically; images are skipped.
pub fn save(path: &Path, items: &[ShelfItem]) -> Result<(), String> {
    let stored = StoredShelf {
        version: FILE_VERSION,
        items: items
            .iter()
            .filter_map(|item| {
                let content = match &item.content {
                    ShelfContent::File { path, name } => StoredContent::File {
                        path: path.display().to_string(),
                        name: name.clone(),
                    },
                    ShelfContent::Text(text) => StoredContent::Text { text: text.clone() },
                    ShelfContent::Link { url } => StoredContent::Link { url: url.clone() },
                    ShelfContent::Image { .. } => return None,
                };
                Some(StoredItem {
                    id: item.id,
                    added: unix_seconds(item.added),
                    pinned: item.pinned,
                    content,
                })
            })
            .collect(),
    };
    let contents = toml::to_string_pretty(&stored)
        .map_err(|error| format!("The shelf was not saved: {error}"))?;
    crate::private_file::write_private_atomic(path, contents.as_bytes())
        .map_err(|error| format!("The shelf was not saved: {error}"))
}

/// Deletes the stored file; a file that is already gone is fine.
pub fn remove_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("The shelf file was not deleted: {error}")),
    }
}

/// Moves an unreadable file aside so a later save never overwrites it.
fn preserve_corrupt(path: &Path) {
    let destination = path.with_file_name(format!(
        "{FILE_NAME}.corrupt-{}",
        unix_millis(SystemTime::now())
    ));
    if fs::rename(path, &destination).is_ok() {
        let _ = crate::private_file::restrict_existing(&destination);
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, UNIX_EPOCH},
    };

    use kestrel_core::{ShelfContent, ShelfItem, ShelfLimits};

    use super::{load, remove_file, save};

    fn item(id: u64, pinned: bool, content: ShelfContent) -> ShelfItem {
        ShelfItem {
            id,
            added: UNIX_EPOCH + Duration::from_secs(1_700_000_000 + id),
            pinned,
            content,
        }
    }

    #[test]
    fn a_round_trip_skips_images_and_keeps_pins_and_times() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("shelf.toml");
        let items = vec![
            item(
                1,
                true,
                ShelfContent::File {
                    path: "/tmp/report.txt".into(),
                    name: "report.txt".to_owned(),
                },
            ),
            item(2, false, ShelfContent::Text("hello".to_owned())),
            item(
                3,
                false,
                ShelfContent::Link {
                    url: "https://example.com/a".to_owned(),
                },
            ),
            item(
                4,
                false,
                ShelfContent::Image {
                    png: vec![1, 2, 3],
                    width: 1,
                    height: 1,
                },
            ),
        ];
        save(&path, &items).expect("the shelf saves");
        let mode = fs::metadata(&path)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the shelf file is private to its owner"
        );
        let (loaded, warning) = load(&path, &ShelfLimits::default());
        assert!(warning.is_none());
        assert_eq!(loaded.len(), 3, "the image is not stored");
        assert_eq!(loaded[0], items[0]);
        assert_eq!(loaded[1], items[1]);
        assert_eq!(loaded[2], items[2]);
    }

    #[test]
    fn a_corrupt_file_is_preserved_and_the_shelf_starts_empty() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("shelf.toml");
        fs::write(&path, b"this is not toml [").expect("fixture file");
        let (loaded, warning) = load(&path, &ShelfLimits::default());
        assert!(loaded.is_empty());
        assert!(warning.is_some(), "a damaged file produces a warning");
        assert!(!path.exists(), "the corrupt file is moved aside");
        let preserved = fs::read_dir(directory.path())
            .expect("directory lists")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .find(|name| name.starts_with("shelf.toml.corrupt-"))
            .expect("a preserved copy exists");
        assert_eq!(
            fs::read(directory.path().join(&preserved)).expect("the copy is readable"),
            b"this is not toml ["
        );
    }

    #[test]
    fn entries_beyond_the_limits_are_dropped_on_load() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("shelf.toml");
        let items = (1..=5)
            .map(|id| item(id, id == 1, ShelfContent::Text(format!("entry {id}"))))
            .collect::<Vec<_>>();
        save(&path, &items).expect("the shelf saves");
        let limits = ShelfLimits {
            max_items: 2,
            ..ShelfLimits::default()
        };
        let (loaded, warning) = load(&path, &limits);
        assert!(warning.is_none());
        assert_eq!(loaded.len(), 2);
        assert!(
            loaded.iter().any(|item| item.pinned),
            "the pinned entry survives trimming"
        );
    }

    #[test]
    fn turning_storage_off_deletes_the_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("shelf.toml");
        save(
            &path,
            &[item(1, false, ShelfContent::Text("kept".to_owned()))],
        )
        .expect("the shelf saves");
        assert!(path.exists());
        remove_file(&path).expect("the shelf file is deleted");
        assert!(!path.exists());
        remove_file(&path).expect("deleting a missing file succeeds");
    }
}
