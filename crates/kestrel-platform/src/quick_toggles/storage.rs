use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::Path,
};

use kestrel_core::{
    CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission, format_bytes,
};

use super::{
    MutationConfirmation, QuickToggleControl, QuickToggleError, QuickToggleErrorKind,
    QuickToggleId, QuickToggleMutation, QuickToggleObservation, ToggleAction, permission_denied,
    supported,
};

const MAX_TRASH_ENTRIES: usize = 100_000;

#[derive(Debug)]
struct TrashScope {
    items: usize,
    bytes: u64,
    fingerprint: u64,
}

pub(crate) fn trash_capability(data_home: &Path) -> CapabilityReport {
    let id = QuickToggleId::EmptyTrash;
    match scan_trash(data_home) {
        Ok(scope) => supported(
            id,
            format!(
                "The current user's local Trash is accessible ({} items).",
                scope.items
            ),
            "freedesktop.org XDG Trash",
        )
        .with_evidence(CapabilityEvidence::new("scope", "local-user-trash")),
        Err(error) if error.kind == QuickToggleErrorKind::PermissionDenied => permission_denied(
            id,
            Permission::FileDeletion,
            "The current user's local Trash is not accessible.",
            "Restore user ownership and write access for the local XDG Trash directories.",
        ),
        Err(error) => CapabilityReport::new(
            id.feature_id(),
            CapabilityStatus::Unsupported {
                reason: error.message.clone(),
            },
            "The current user's local Trash cannot be inspected safely.",
        )
        .with_remediation(error.message),
    }
}

pub(crate) fn observe_trash(data_home: &Path) -> Result<QuickToggleObservation, QuickToggleError> {
    let scope = scan_trash(data_home)?;
    let actions = if scope.items == 0 {
        Vec::new()
    } else {
        vec![ToggleAction {
            target: None,
            label: "Empty local Trash".to_owned(),
            confirmation: trash_scope_confirmation(&scope),
        }]
    };
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Actions(actions),
        detail: if scope.items == 0 {
            "The current user's local Trash is empty.".to_owned()
        } else {
            format!(
                "Trash has {} ({}). Remote and other users' Trash folders aren't included.",
                item_count(scope.items),
                format_bytes(scope.bytes as f64)
            )
        },
    })
}

pub(crate) fn trash_confirmation(
    data_home: &Path,
    mutation: &QuickToggleMutation,
) -> Result<Option<MutationConfirmation>, QuickToggleError> {
    match mutation {
        QuickToggleMutation::Activate { target: None } => {
            let scope = scan_trash(data_home)?;
            if scope.items == 0 {
                return Err(QuickToggleError::new(
                    QuickToggleErrorKind::InvalidRequest,
                    "the local Trash is already empty",
                ));
            }
            Ok(Some(trash_scope_confirmation(&scope)))
        }
        _ => Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "empty Trash accepts only its advertised action",
        )),
    }
}

pub(crate) fn empty_trash(
    data_home: &Path,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    if !matches!(mutation, QuickToggleMutation::Activate { target: None }) {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "empty Trash accepts only its advertised action",
        ));
    }
    let trash = data_home.join("Trash");
    remove_children(&trash.join("files"))?;
    remove_children(&trash.join("info"))
}

fn trash_scope_confirmation(scope: &TrashScope) -> MutationConfirmation {
    MutationConfirmation {
        scope: format!(
            "Permanently delete {} ({}) from your Trash. Remote Trash folders aren't included.",
            item_count(scope.items),
            format_bytes(scope.bytes as f64)
        ),
        token: format!(
            "trash:{}:{}:{:016x}",
            scope.items, scope.bytes, scope.fingerprint
        ),
    }
}

fn item_count(items: usize) -> String {
    format!("{items} {}", if items == 1 { "item" } else { "items" })
}

fn scan_trash(data_home: &Path) -> Result<TrashScope, QuickToggleError> {
    let files = data_home.join("Trash/files");
    if !files.exists() {
        return Ok(TrashScope {
            items: 0,
            bytes: 0,
            fingerprint: 0,
        });
    }
    let top_level = fs::read_dir(&files)
        .map_err(|error| QuickToggleError::io("reading the local Trash", &error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| QuickToggleError::io("reading a local Trash entry", &error))?;
    let mut hasher = DefaultHasher::new();
    let mut bytes = 0u64;
    let mut visited = 0usize;
    let mut stack = top_level
        .iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    while let Some(path) = stack.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_TRASH_ENTRIES {
            return Err(QuickToggleError::new(
                QuickToggleErrorKind::InvalidRequest,
                format!(
                    "the local Trash exceeds the bounded inspection limit of {MAX_TRASH_ENTRIES} entries"
                ),
            ));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| QuickToggleError::io("inspecting a local Trash entry", &error))?;
        path.strip_prefix(&files).unwrap_or(&path).hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        bytes = bytes.saturating_add(metadata.len());
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            let entries = fs::read_dir(&path)
                .map_err(|error| QuickToggleError::io("reading a trashed directory", &error))?;
            for entry in entries {
                stack.push(
                    entry
                        .map_err(|error| QuickToggleError::io("reading a trashed entry", &error))?
                        .path(),
                );
            }
        }
    }

    Ok(TrashScope {
        items: top_level.len(),
        bytes,
        fingerprint: hasher.finish(),
    })
}

fn remove_children(directory: &Path) -> Result<(), QuickToggleError> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| QuickToggleError::io("opening a Trash directory", &error))?;
    for entry in entries {
        let path = entry
            .map_err(|error| QuickToggleError::io("reading a Trash entry", &error))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| QuickToggleError::io("inspecting a Trash entry", &error))?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path)
                .map_err(|error| QuickToggleError::io("removing a trashed directory", &error))?;
        } else {
            fs::remove_file(&path)
                .map_err(|error| QuickToggleError::io("removing a trashed file", &error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{empty_trash, observe_trash, trash_confirmation};
    use crate::quick_toggles::{QuickToggleControl, QuickToggleMutation};

    #[test]
    fn trash_scope_shows_readable_sizes_and_plurals() {
        let directory = TempDir::new().expect("temporary data home");
        let files = directory.path().join("Trash/files");
        fs::create_dir_all(&files).expect("Trash files");
        fs::write(files.join("one.txt"), vec![0u8; 2048]).expect("first item");
        let mutation = QuickToggleMutation::Activate { target: None };

        let observation = observe_trash(directory.path()).expect("Trash observation");
        assert!(
            observation.detail.contains("1 item"),
            "{}",
            observation.detail
        );
        assert!(
            observation.detail.contains("2.0 KiB"),
            "{}",
            observation.detail
        );
        assert!(
            !observation.detail.contains("2048"),
            "raw bytes stay hidden: {}",
            observation.detail
        );

        let confirmation = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");
        assert!(
            confirmation.scope.contains("1 item"),
            "{}",
            confirmation.scope
        );
        assert!(
            confirmation.scope.contains("2.0 KiB"),
            "{}",
            confirmation.scope
        );

        fs::write(files.join("two.txt"), vec![0u8; 2048]).expect("second item");

        let observation = observe_trash(directory.path()).expect("Trash observation");
        assert!(
            observation.detail.contains("2 items"),
            "{}",
            observation.detail
        );
        assert!(
            observation.detail.contains("4.0 KiB"),
            "{}",
            observation.detail
        );
        assert!(
            !observation.detail.contains("4096"),
            "raw bytes stay hidden: {}",
            observation.detail
        );

        let confirmation = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");
        assert!(
            confirmation.scope.contains("2 items"),
            "{}",
            confirmation.scope
        );
        assert!(
            confirmation.scope.contains("4.0 KiB"),
            "{}",
            confirmation.scope
        );
    }

    #[test]
    fn trash_confirmation_changes_when_scope_changes() {
        let directory = TempDir::new().expect("temporary data home");
        let files = directory.path().join("Trash/files");
        fs::create_dir_all(&files).expect("Trash files");
        fs::write(files.join("first.txt"), "first").expect("first item");
        let mutation = QuickToggleMutation::Activate { target: None };
        let first = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");
        fs::write(files.join("second.txt"), "second").expect("second item");
        let second = trash_confirmation(directory.path(), &mutation)
            .expect("scope")
            .expect("confirmation");

        assert_ne!(first.token, second.token);
    }

    #[test]
    fn empty_trash_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("temporary data home");
        let outside = directory.path().join("outside.txt");
        fs::write(&outside, "preserve").expect("outside file");
        let files = directory.path().join("Trash/files");
        let info = directory.path().join("Trash/info");
        fs::create_dir_all(&files).expect("Trash files");
        fs::create_dir_all(&info).expect("Trash info");
        symlink(&outside, files.join("link")).expect("symlink");
        fs::write(info.join("link.trashinfo"), "metadata").expect("trash metadata");

        empty_trash(
            directory.path(),
            &QuickToggleMutation::Activate { target: None },
        )
        .expect("Trash empties");

        assert_eq!(fs::read_to_string(outside).unwrap(), "preserve");
        let observation = observe_trash(directory.path()).expect("Trash observation");
        assert_eq!(observation.control, QuickToggleControl::Actions(Vec::new()));
    }
}
