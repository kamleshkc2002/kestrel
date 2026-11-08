//! Shelf presentation: staged entries and how they are kept.

use super::*;
use kestrel_core::ShelfItemKind;
use kestrel_platform::file_actions::{FileIdentity, RevealSupport};
use std::time::{Duration, SystemTime};

pub(crate) struct ShelfPresentation {
    pub running: bool,
    pub keep: bool,
    pub snapshot: Option<ShelfSnapshot>,
    pub limits: kestrel_core::ShelfConfiguration,
    pub warning: Option<String>,
    /// What the desktop offers for showing a file in its folder.
    pub reveal: RevealSupport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfItemViewModel {
    pub id: u64,
    pub kind: ShelfItemKind,
    pub kind_label: &'static str,
    pub name: String,
    /// Path for files, size, and age.
    pub detail: String,
    pub pinned: bool,
    pub available: bool,
    /// A file entry whose path is a directory.
    pub directory: bool,
    /// The hand-off type named by the file extension, without reading it.
    pub install: Option<kestrel_platform::install_handoff::InstallTargetKind>,
    pub content: kestrel_core::ShelfContent,
    /// The file entry's identity when the row was built.
    pub identity: Option<FileIdentity>,
    /// The display name of the app that shows files, when it reports one.
    pub reveal_provider: Option<String>,
    /// The file manager interface can select the file, not only open its folder.
    pub reveal_selects: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfViewModel {
    pub running: bool,
    pub status: String,
    /// Entries survive a restart when this is on.
    pub keep: bool,
    pub items: Vec<ShelfItemViewModel>,
    pub unpinned: usize,
    pub warning: Option<String>,
    pub limits: kestrel_core::ShelfConfiguration,
    pub turn_on: Option<TurnOnViewModel>,
}

impl Default for ShelfViewModel {
    fn default() -> Self {
        Self {
            running: false,
            status: "Shelf is off".to_owned(),
            keep: false,
            items: Vec::new(),
            unpinned: 0,
            warning: None,
            limits: kestrel_core::ShelfConfiguration::default(),
            turn_on: None,
        }
    }
}

impl ShelfViewModel {
    pub(crate) fn from_presentation(presentation: ShelfPresentation) -> Self {
        let now = SystemTime::now();
        let snapshot = presentation.snapshot;
        let reveal_provider = presentation.reveal.provider.clone();
        let reveal_selects = presentation.reveal.selects;
        let warning = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.warning.clone())
            .or(presentation.warning);
        let items = snapshot
            .map(|snapshot| {
                snapshot
                    .entries
                    .into_iter()
                    .map(|entry| {
                        let detail = entry_detail(
                            &entry,
                            now.duration_since(entry.added).unwrap_or_default(),
                        );
                        let install = match &entry.content {
                            kestrel_core::ShelfContent::File { path, .. } => {
                                kestrel_platform::install_handoff::kind_for_path(path)
                            }
                            _ => None,
                        };
                        ShelfItemViewModel {
                            id: entry.id,
                            kind: entry.kind,
                            kind_label: entry.kind.label(),
                            name: entry.name,
                            detail,
                            pinned: entry.pinned,
                            available: entry.available,
                            directory: entry.directory,
                            install,
                            content: entry.content,
                            identity: entry.identity,
                            reveal_provider: reveal_provider.clone(),
                            reveal_selects,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let unpinned = items.iter().filter(|item| !item.pinned).count();
        let status = if !presentation.running {
            "Shelf is off; enable files.shelf in the Feature Hub.".to_owned()
        } else if items.is_empty() {
            "Drop files, text, or links here.".to_owned()
        } else {
            let entries = if items.len() == 1 { "entry" } else { "entries" };
            let keeping = if presentation.keep {
                "kept across restarts"
            } else {
                "cleared on quit"
            };
            format!("{} {entries}; {keeping}.", items.len())
        };
        Self {
            running: presentation.running,
            status,
            keep: presentation.keep,
            items,
            unpinned,
            warning,
            limits: presentation.limits,
            turn_on: None,
        }
    }
}

fn entry_detail(entry: &ShelfEntry, age: Duration) -> String {
    let mut parts = Vec::new();
    if let kestrel_core::ShelfContent::File { path, .. } = &entry.content {
        if entry.available {
            parts.push(path.display().to_string());
        } else {
            parts.push("Missing".to_owned());
            parts.push(path.display().to_string());
            return parts.join(" · ");
        }
    }
    if let Some(size) = entry.size_bytes {
        parts.push(kestrel_core::format_size(size));
    }
    parts.push(format_age(age));
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use kestrel_core::{ShelfContent, ShelfItemKind};

    use super::ShelfPresentation;
    use crate::view_model::ShelfViewModel;
    use kestrel_platform::file_actions::RevealSupport;
    use kestrel_services::shelf::{ShelfEntry, ShelfSnapshot};

    fn entry(content: ShelfContent, available: bool, size: Option<u64>) -> ShelfEntry {
        ShelfEntry {
            id: 1,
            added: UNIX_EPOCH,
            pinned: false,
            kind: content.kind(),
            name: content.display_name(),
            content,
            available,
            directory: false,
            size_bytes: size,
            identity: None,
        }
    }

    #[test]
    fn a_missing_file_says_so_and_a_present_one_shows_its_path() {
        let missing = entry(
            ShelfContent::File {
                path: "/tmp/gone.txt".into(),
                name: "gone.txt".to_owned(),
            },
            false,
            None,
        );
        let present = ShelfEntry {
            id: 2,
            added: UNIX_EPOCH,
            pinned: true,
            ..entry(
                ShelfContent::File {
                    path: "/tmp/here.txt".into(),
                    name: "here.txt".to_owned(),
                },
                true,
                Some(9),
            )
        };
        let model = ShelfViewModel::from_presentation(ShelfPresentation {
            running: true,
            keep: false,
            snapshot: Some(ShelfSnapshot {
                entries: vec![missing, present],
                warning: None,
            }),
            limits: kestrel_core::ShelfConfiguration::default(),
            warning: None,
            reveal: RevealSupport::none(),
        });
        assert_eq!(model.items.len(), 2);
        assert_eq!(model.items[0].detail, "Missing · /tmp/gone.txt");
        assert!(model.items[1].detail.starts_with("/tmp/here.txt · 9 B"));
        assert_eq!(model.items[1].kind, ShelfItemKind::File);
        assert_eq!(model.unpinned, 1);
    }

    #[test]
    fn only_supported_file_names_offer_a_handoff_hint() {
        let package = entry(
            ShelfContent::File {
                path: "/tmp/example.deb".into(),
                name: "example.deb".to_owned(),
            },
            true,
            Some(9),
        );
        let text = ShelfEntry {
            id: 2,
            ..entry(
                ShelfContent::File {
                    path: "/tmp/example.txt".into(),
                    name: "example.txt".to_owned(),
                },
                true,
                Some(9),
            )
        };
        let model = ShelfViewModel::from_presentation(ShelfPresentation {
            running: true,
            keep: false,
            snapshot: Some(ShelfSnapshot {
                entries: vec![package, text],
                warning: None,
            }),
            limits: kestrel_core::ShelfConfiguration::default(),
            warning: None,
        });
        assert_eq!(
            model.items[0].install,
            Some(kestrel_platform::install_handoff::InstallTargetKind::DebianPackage)
        );
        assert_eq!(model.items[1].install, None);
    }

    #[test]
    fn text_detail_carries_size_and_age() {
        let model = ShelfViewModel::from_presentation(ShelfPresentation {
            running: true,
            keep: true,
            snapshot: Some(ShelfSnapshot {
                entries: vec![ShelfEntry {
                    added: SystemTime::now() - Duration::from_secs(3_700),
                    ..entry(ShelfContent::Text("hello there".to_owned()), true, Some(11))
                }],
                warning: None,
            }),
            limits: kestrel_core::ShelfConfiguration::default(),
            warning: None,
            reveal: RevealSupport::none(),
        });
        assert_eq!(model.items[0].detail, "11 B · 1 h old");
        assert!(model.keep);
        assert_eq!(model.items[0].kind_label, "Text");
    }
}
