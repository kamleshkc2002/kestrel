//! In-memory shelf state and bounded add, clear, pin, and expiry policy.
//!
//! Storage lives with the application so this service stays I/O-free apart
//! from the cheap availability check a snapshot makes per file entry.

use std::{fs, path::PathBuf, time::SystemTime};

use kestrel_core::{
    ShelfAddOutcome, ShelfContent, ShelfError, ShelfItem, ShelfItemKind, ShelfLimits, shelf_expire,
    shelf_place,
};
use kestrel_platform::file_actions::FileIdentity;

pub const FEATURE_ID: &str = "files.shelf";

/// Content arriving from a drop, the clipboard, or a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShelfInput {
    /// One entry per dropped file.
    Files(Vec<PathBuf>),
    /// A mixed drop already classified: local files and web links.
    Entries(Vec<ShelfContent>),
    Text(String),
    /// A decoded PNG with the size it was rendered at.
    Image {
        png: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// One entry as the window presents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfEntry {
    pub id: u64,
    pub added: SystemTime,
    pub pinned: bool,
    pub kind: ShelfItemKind,
    pub name: String,
    pub content: ShelfContent,
    /// A file entry whose path still exists.
    pub available: bool,
    /// A file entry whose path is a directory.
    pub directory: bool,
    pub size_bytes: Option<u64>,
    /// The file entry's identity, captured with the snapshot so an action can
    /// confirm the same file before it changes anything.
    pub identity: Option<FileIdentity>,
}

/// Entries and the warning a failed load left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfSnapshot {
    pub entries: Vec<ShelfEntry>,
    pub warning: Option<String>,
}

/// Bounded shelf state; the newest entry is first.
#[derive(Debug, Clone)]
pub struct ShelfService {
    items: Vec<ShelfItem>,
    limits: ShelfLimits,
    next_id: u64,
    warning: Option<String>,
}

impl ShelfService {
    pub fn new(limits: ShelfLimits) -> Result<Self, ShelfError> {
        limits.validate().map_err(|_| ShelfError::InvalidLimits)?;
        Ok(Self {
            items: Vec::new(),
            limits,
            next_id: 1,
            warning: None,
        })
    }

    /// Replaces the entries with stored ones, keeping their order and pins.
    pub fn restore(&mut self, items: Vec<ShelfItem>) {
        self.next_id = items
            .iter()
            .map(|item| item.id.saturating_add(1))
            .max()
            .unwrap_or(1);
        self.items = items;
    }

    /// New bounds apply to later adds and expiry; stored entries stay.
    pub fn set_limits(&mut self, limits: ShelfLimits) {
        self.limits = limits;
    }

    pub fn limits(&self) -> ShelfLimits {
        self.limits
    }

    pub fn set_warning(&mut self, warning: Option<String>) {
        self.warning = warning;
    }

    pub fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    /// Adds every file reference, then text or an image; returns the new count.
    pub fn add(&mut self, input: ShelfInput, now: SystemTime) -> Result<usize, ShelfError> {
        match input {
            ShelfInput::Files(paths) => self.add_files(paths, now),
            ShelfInput::Entries(entries) => {
                let mut added = 0;
                for content in entries {
                    self.place(content, now)?;
                    added += 1;
                }
                Ok(added)
            }
            ShelfInput::Text(text) => {
                let content = kestrel_core::detect_shelf_content(&text);
                self.place(content, now).map(|_| 1)
            }
            ShelfInput::Image { png, width, height } => self
                .place(ShelfContent::Image { png, width, height }, now)
                .map(|_| 1),
        }
    }

    /// Each path becomes one entry; a path that does not fit stops the batch.
    pub fn add_files(&mut self, paths: Vec<PathBuf>, now: SystemTime) -> Result<usize, ShelfError> {
        let mut added = 0;
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            self.place(ShelfContent::File { path, name }, now)?;
            added += 1;
        }
        Ok(added)
    }

    fn place(
        &mut self,
        content: ShelfContent,
        now: SystemTime,
    ) -> Result<ShelfAddOutcome, ShelfError> {
        let outcome = shelf_place(&mut self.items, self.next_id, content, now, &self.limits)?;
        if !outcome.existing {
            self.next_id = self.next_id.saturating_add(1);
        }
        Ok(outcome)
    }

    pub fn remove(&mut self, id: u64) -> Result<(), ShelfError> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(ShelfError::UnknownItem)?;
        self.items.remove(index);
        Ok(())
    }

    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> Result<(), ShelfError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(ShelfError::UnknownItem)?;
        item.pinned = pinned;
        Ok(())
    }

    /// Points a file entry at its new path after a move or rename.
    pub fn update_file(&mut self, id: u64, path: PathBuf, name: String) -> Result<(), ShelfError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(ShelfError::UnknownItem)?;
        match &mut item.content {
            ShelfContent::File {
                path: current,
                name: current_name,
            } => {
                *current = path;
                *current_name = name;
                Ok(())
            }
            _ => Err(ShelfError::NotAFile),
        }
    }

    /// Removes unpinned entries, or every entry; returns how many left.
    pub fn clear(&mut self, include_pinned: bool) -> usize {
        let before = self.items.len();
        if include_pinned {
            self.items.clear();
        } else {
            self.items.retain(|item| item.pinned);
        }
        before - self.items.len()
    }

    /// Removes entries past the age limit; pinned entries stay.
    pub fn expire(&mut self, now: SystemTime) -> usize {
        shelf_expire(&mut self.items, &self.limits, now).len()
    }

    pub fn unpinned_count(&self) -> usize {
        self.items.iter().filter(|item| !item.pinned).count()
    }

    /// Entries with their per-file availability checked once per refresh.
    pub fn snapshot(&self) -> ShelfSnapshot {
        let entries = self
            .items
            .iter()
            .map(|item| {
                let (available, directory, size_bytes) = match &item.content {
                    ShelfContent::File { path, .. } => match fs::metadata(path) {
                        Ok(metadata) => (
                            true,
                            metadata.is_dir(),
                            metadata.is_file().then_some(metadata.len()),
                        ),
                        Err(_) => (false, false, None),
                    },
                    _ => (true, false, item.content.size_bytes()),
                };
                let identity = match &item.content {
                    ShelfContent::File { path, .. } => FileIdentity::read(path).ok(),
                    _ => None,
                };
                ShelfEntry {
                    id: item.id,
                    added: item.added,
                    pinned: item.pinned,
                    kind: item.content.kind(),
                    name: item.content.display_name(),
                    content: item.content.clone(),
                    available,
                    directory,
                    size_bytes,
                    identity,
                }
            })
            .collect();
        ShelfSnapshot {
            entries,
            warning: self.warning.clone(),
        }
    }

    /// Entries that may be written to disk; images never are.
    pub fn persistable(&self) -> Vec<ShelfItem> {
        self.items
            .iter()
            .filter(|item| !matches!(item.content, ShelfContent::Image { .. }))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, UNIX_EPOCH},
    };

    use kestrel_core::{ShelfContent, ShelfError, ShelfItem, ShelfLimits};

    use super::{ShelfInput, ShelfService};

    fn service() -> ShelfService {
        ShelfService::new(ShelfLimits::default()).expect("default limits are valid")
    }

    fn at(seconds: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn text_and_links_are_classified_and_bounded() {
        let mut shelf = service();
        assert_eq!(
            shelf
                .add(ShelfInput::Text("https://example.com".to_owned()), at(1))
                .expect("a link fits"),
            1
        );
        assert_eq!(
            shelf.items()[0].content.kind(),
            kestrel_core::ShelfItemKind::Link
        );
        let mut tiny = ShelfService::new(ShelfLimits {
            max_text_bytes: 4,
            ..ShelfLimits::default()
        })
        .expect("limits are valid");
        assert_eq!(
            tiny.add(ShelfInput::Text("too long".to_owned()), at(1)),
            Err(ShelfError::OversizeText { bytes: 8, limit: 4 })
        );
    }

    #[test]
    fn expiry_skips_pinned_entries_and_clear_distinguishes_them() {
        let mut shelf = service();
        shelf
            .add(ShelfInput::Text("one".to_owned()), at(0))
            .expect("fits");
        shelf
            .add(ShelfInput::Text("two".to_owned()), at(0))
            .expect("fits");
        let pinned = shelf.items()[0].id;
        shelf.set_pinned(pinned, true).expect("the entry exists");

        shelf.set_limits(ShelfLimits {
            max_age_hours: 1,
            ..ShelfLimits::default()
        });
        assert_eq!(shelf.expire(at(10_000)), 1);
        assert_eq!(shelf.items().len(), 1);
        assert_eq!(shelf.items()[0].id, pinned);
        assert_eq!(shelf.unpinned_count(), 0);

        assert_eq!(shelf.clear(false), 0, "nothing unpinned is left");
        shelf
            .add(ShelfInput::Text("three".to_owned()), at(20_000))
            .expect("fits");
        assert_eq!(shelf.clear(false), 1);
        assert_eq!(shelf.items().len(), 1);
        assert_eq!(shelf.items()[0].id, pinned);
        assert_eq!(shelf.clear(true), 1);
        assert!(shelf.items().is_empty());
        assert_eq!(shelf.remove(pinned), Err(ShelfError::UnknownItem));
    }

    #[test]
    fn a_missing_file_entry_reports_unavailable_without_a_size() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let present = directory.path().join("present.txt");
        fs::write(&present, b"hello").expect("fixture file");
        let missing = directory.path().join("missing.txt");
        let folder = directory.path().to_path_buf();
        let mut shelf = service();
        shelf
            .add(
                ShelfInput::Files(vec![present.clone(), missing.clone(), folder.clone()]),
                at(1),
            )
            .expect("file references always fit");
        let snapshot = shelf.snapshot();
        assert_eq!(snapshot.entries.len(), 3);
        let present_entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.available && !entry.directory)
            .expect("the present file is available");
        assert_eq!(present_entry.size_bytes, Some(5));
        assert!(
            present_entry.identity.is_some(),
            "an available file carries the identity an action confirms"
        );
        let missing_entry = snapshot
            .entries
            .iter()
            .find(|entry| !entry.available)
            .expect("the missing file is reported");
        assert_eq!(missing_entry.size_bytes, None);
        assert_eq!(missing_entry.identity, None);
        assert_eq!(missing_entry.name, "missing.txt");
        let folder_entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.directory)
            .expect("the directory is reported as one");
        assert!(folder_entry.available);
        assert_eq!(folder_entry.size_bytes, None);
    }

    #[test]
    fn a_moved_file_entry_points_at_its_new_path_and_text_refuses_the_change() {
        let mut shelf = service();
        shelf
            .add(
                ShelfInput::Files(vec![PathBuf::from("/tmp/before.txt")]),
                at(1),
            )
            .expect("a file reference fits");
        let id = shelf.items()[0].id;
        shelf
            .update_file(id, PathBuf::from("/tmp/after.txt"), "after.txt".to_owned())
            .expect("a file entry updates");
        assert_eq!(
            shelf.items()[0].content,
            ShelfContent::File {
                path: PathBuf::from("/tmp/after.txt"),
                name: "after.txt".to_owned(),
            }
        );
        shelf
            .add(ShelfInput::Text("kept".to_owned()), at(2))
            .expect("text fits");
        let text_id = shelf
            .items()
            .iter()
            .find(|item| item.content.kind() == kestrel_core::ShelfItemKind::Text)
            .map(|item| item.id)
            .expect("the text entry exists");
        assert_eq!(
            shelf.update_file(text_id, PathBuf::from("/tmp/x"), "x".to_owned()),
            Err(ShelfError::NotAFile)
        );
    }

    #[test]
    fn a_mixed_drop_keeps_local_files_and_web_links_apart() {
        let mut shelf = service();
        shelf
            .add(
                ShelfInput::Entries(vec![
                    ShelfContent::File {
                        path: "/tmp/one".into(),
                        name: "one".to_owned(),
                    },
                    ShelfContent::Link {
                        url: "https://example.com/page".to_owned(),
                    },
                ]),
                at(1),
            )
            .expect("both entries fit");
        assert_eq!(
            shelf
                .items()
                .iter()
                .map(|item| item.content.kind())
                .collect::<Vec<_>>(),
            vec![
                kestrel_core::ShelfItemKind::Link,
                kestrel_core::ShelfItemKind::File
            ]
        );
    }

    #[test]
    fn images_are_never_persistable_and_ids_do_not_repeat() {
        let mut shelf = service();
        shelf
            .add(ShelfInput::Text("text".to_owned()), at(1))
            .expect("fits");
        shelf
            .add(
                ShelfInput::Image {
                    png: vec![1, 2, 3],
                    width: 1,
                    height: 1,
                },
                at(2),
            )
            .expect("fits");
        assert_eq!(shelf.persistable().len(), 1);
        assert_eq!(shelf.items().len(), 2);
        shelf.remove(shelf.items()[0].id).expect("exists");
        shelf
            .add(ShelfInput::Text("later".to_owned()), at(3))
            .expect("fits");
        let ids = shelf.items().iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![3, 1]);
    }

    #[test]
    fn restoring_entries_keeps_their_pins_and_advances_ids() {
        let mut shelf = service();
        shelf.restore(vec![
            ShelfItem {
                id: 4,
                added: at(10),
                pinned: true,
                content: ShelfContent::Text("kept".to_owned()),
            },
            ShelfItem {
                id: 7,
                added: at(5),
                pinned: false,
                content: ShelfContent::Link {
                    url: "https://example.com".to_owned(),
                },
            },
        ]);
        assert_eq!(shelf.snapshot().entries[0].name, "kept");
        assert!(shelf.items()[0].pinned);
        shelf
            .add(ShelfInput::Text("new".to_owned()), at(20))
            .expect("fits");
        assert_eq!(shelf.items()[0].id, 8);
    }
}
