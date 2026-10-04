//! Atomic, private replacement for user-data files.
//!
//! Exclusive temporary creation prevents symlink redirection; directory sync makes renames durable.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Owner-only file mode.
pub(crate) const FILE_MODE: u32 = 0o600;
/// Owner-only directory mode.
const DIRECTORY_MODE: u32 = 0o700;
/// Temporary-name attempts before failure.
const TEMPORARY_ATTEMPTS: u32 = 16;

static TEMPORARY_SERIAL: AtomicU64 = AtomicU64::new(0);

/// Atomically replaces `path` with owner-only permissions.
pub(crate) fn write_private_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no parent directory",
        )
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name"))?;
    fs::create_dir_all(parent)?;
    let _ = fs::set_permissions(parent, fs::Permissions::from_mode(DIRECTORY_MODE));

    let (temporary, mut file) = create_temporary(parent, file_name)?;
    let written = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE));

    // Directory sync makes the rename durable where supported.
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Creates a unique owner-only temporary file beside the target.
fn create_temporary(parent: &Path, file_name: &OsStr) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMPORARY_ATTEMPTS {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let serial = TEMPORARY_SERIAL.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name);
        name.push(format!(".tmp{}-{nanos:09}-{serial}", std::process::id()));
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            // A name held by another file or link is skipped.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused temporary file name was available",
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;

    use super::{FILE_MODE, write_private_atomic};

    fn entries(dir: &TempDir) -> Vec<String> {
        let mut names = fs::read_dir(dir.path())
            .expect("directory is readable")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn writes_replace_the_file_privately_and_leave_no_temporary() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nested/data.toml");

        write_private_atomic(&path, b"first").expect("the first write succeeds");
        write_private_atomic(&path, b"second").expect("a replacement succeeds");

        assert_eq!(fs::read_to_string(&path).expect("file exists"), "second");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "the file stays owner-only");
        assert_eq!(
            fs::read_dir(path.parent().expect("parent"))
                .expect("readable")
                .count(),
            1,
            "no temporary file is left behind"
        );
    }

    #[test]
    fn a_pre_created_link_at_a_predictable_temporary_name_is_never_followed() {
        let dir = TempDir::new().expect("temp dir");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep me").expect("victim is written");
        let path = dir.path().join("data.toml");

        // The old predictable name let a planted link redirect writes.
        let planted = dir
            .path()
            .join(format!("data.toml.tmp{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).expect("link is planted");
        let dotted = dir
            .path()
            .join(format!(".data.toml.tmp{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &dotted).expect("second link is planted");

        write_private_atomic(&path, b"payload").expect("the save succeeds");

        assert_eq!(fs::read_to_string(&victim).expect("victim"), "keep me");
        assert_eq!(fs::read_to_string(&path).expect("target"), "payload");
    }

    #[test]
    fn a_target_that_is_a_link_is_replaced_not_written_through() {
        let dir = TempDir::new().expect("temp dir");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep me").expect("victim is written");
        let path = dir.path().join("data.toml");
        std::os::unix::fs::symlink(&victim, &path).expect("target link is planted");

        write_private_atomic(&path, b"payload").expect("the save succeeds");

        assert_eq!(
            fs::read_to_string(&victim).expect("victim"),
            "keep me",
            "the rename replaces the link instead of writing through it"
        );
        assert_eq!(fs::read_to_string(&path).expect("target"), "payload");
        assert!(
            !fs::symlink_metadata(&path)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            entries(&dir).len(),
            2,
            "only the victim and the target remain"
        );
    }

    #[test]
    fn a_failed_write_reports_the_error_and_leaves_no_temporary() {
        let dir = TempDir::new().expect("temp dir");
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "a regular file").expect("blocker is written");

        let error = write_private_atomic(&blocker.join("data.toml"), b"x")
            .expect_err("a file cannot be a parent directory");

        assert!(!error.to_string().is_empty());
        assert_eq!(entries(&dir), vec!["blocker".to_string()]);
    }
}
