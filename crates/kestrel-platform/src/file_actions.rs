//! Move, rename, PNG export, and reveal for shelf file references.
//!
//! Identity comes from `symlink_metadata`, so a symbolic link is moved as a
//! link and never followed. Every name is created without replacing an
//! existing entry; a failed copy leaves the original in place.

use std::{
    ffi::CString,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use gio::prelude::{AppInfoExt, FileExt};
use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};

pub const FEATURE_ID: &str = "files.shelf";
/// The longest name a file system reliably accepts.
pub const MAX_NAME_BYTES: usize = 255;
/// The desktop's file manager interface for selecting and revealing items.
pub const FILE_MANAGER_SERVICE: &str = "org.freedesktop.FileManager1";
pub const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";
pub const FILE_MANAGER_INTERFACE: &str = "org.freedesktop.FileManager1";

const DIRECTORY_TYPE: &str = "inode/directory";
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// What a path points at, read without following a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

/// The on-disk identity an action was confirmed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// Seconds and nanoseconds since the Unix epoch.
    pub mtime: (i64, i64),
    pub kind: FileKind,
}

impl FileIdentity {
    /// Reads a path's own identity; a link is reported as a link.
    pub fn read(path: &Path) -> Result<Self, FileActionError> {
        let metadata = fs::symlink_metadata(path).map_err(FileActionError::from)?;
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            kind,
        }
    }
}

/// Why a name cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    Separator,
    Nul,
    DotEntry,
    TooLong,
}

impl NameProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a name.",
            Self::Separator => "A name cannot contain a slash.",
            Self::Nul => "A name cannot contain a null character.",
            Self::DotEntry => "A name cannot be a single dot or two dots.",
            Self::TooLong => "A name can be at most 255 bytes.",
        }
    }
}

/// Why an action did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileActionError {
    InvalidName(NameProblem),
    InvalidDestination,
    /// A folder cannot be moved into its own tree.
    DestinationInsideSource,
    Exists,
    PermissionDenied,
    NotFound,
    /// The path changed since the action was confirmed.
    Changed,
    /// A symbolic link cannot cross a file system boundary here.
    CrossDevice,
    /// A folder on another file system cannot be moved here.
    CrossDeviceFolder,
    NotPng,
    OversizePng {
        bytes: u64,
        limit: u64,
    },
    NoFolderApp,
    Io(io::ErrorKind),
}

impl fmt::Display for FileActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(problem) => formatter.write_str(problem.message()),
            Self::InvalidDestination => formatter.write_str("Choose an existing folder."),
            Self::DestinationInsideSource => {
                formatter.write_str("A folder cannot be moved inside itself.")
            }
            Self::Exists => formatter.write_str("Something with that name is already there."),
            Self::PermissionDenied => {
                formatter.write_str("That location does not allow this change.")
            }
            Self::NotFound => formatter.write_str("The file is no longer there."),
            Self::Changed => {
                formatter.write_str("The file changed since you started; try it again.")
            }
            Self::CrossDevice => formatter
                .write_str("A link cannot be moved to another drive from here; move it in your file manager."),
            Self::CrossDeviceFolder => formatter.write_str(
                "A folder cannot be moved to another drive from here; move it in your file manager.",
            ),
            Self::NotPng => formatter.write_str("Only PNG images can be saved here."),
            Self::OversizePng { bytes, limit } => write!(
                formatter,
                "The image is {} and the limit is {}; raise the image limit in the clipboard settings.",
                kestrel_core::format_size(*bytes),
                kestrel_core::format_size(*limit)
            ),
            Self::NoFolderApp => {
                formatter.write_str("No app is available to open the folder.")
            }
            Self::Io(kind) => match kind {
                io::ErrorKind::PermissionDenied => {
                    formatter.write_str("That location does not allow this change.")
                }
                io::ErrorKind::NotFound => formatter.write_str("The file is no longer there."),
                io::ErrorKind::AlreadyExists => {
                    formatter.write_str("Something with that name is already there.")
                }
                _ => formatter.write_str("The change could not be finished."),
            },
        }
    }
}

impl std::error::Error for FileActionError {}

impl From<io::Error> for FileActionError {
    fn from(error: io::Error) -> Self {
        match error.raw_os_error() {
            Some(libc::EEXIST) => Self::Exists,
            Some(libc::EACCES) | Some(libc::EPERM) => Self::PermissionDenied,
            Some(libc::ENOENT) => Self::NotFound,
            Some(libc::ENAMETOOLONG) => Self::InvalidName(NameProblem::TooLong),
            Some(libc::ENOTDIR) => Self::InvalidDestination,
            _ => match error.kind() {
                io::ErrorKind::AlreadyExists => Self::Exists,
                io::ErrorKind::PermissionDenied => Self::PermissionDenied,
                io::ErrorKind::NotFound => Self::NotFound,
                kind => Self::Io(kind),
            },
        }
    }
}

/// Checks a name before it reaches the file system.
pub fn validate_new_name(name: &str) -> Result<(), NameProblem> {
    if name.trim().is_empty() {
        return Err(NameProblem::Empty);
    }
    if name.contains('/') {
        return Err(NameProblem::Separator);
    }
    if name.contains('\0') {
        return Err(NameProblem::Nul);
    }
    if name == "." || name == ".." {
        return Err(NameProblem::DotEntry);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameProblem::TooLong);
    }
    Ok(())
}

/// The file system steps an action takes, replaceable in tests.
pub trait FileSystemOps {
    /// Renames without replacing an existing destination.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
}

/// The real file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemFileSystem;

impl FileSystemOps for SystemFileSystem {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        match renameat2_no_replace(from, to) {
            Err(error) if flag_unsupported(&error) => fallback_rename(from, to),
            result => result,
        }
    }

    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::copy(from, to).map(|_| ())
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        fs::File::open(path)?.sync_all()
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

/// `renameat2(RENAME_NOREPLACE)`: the kernel refuses an existing destination.
fn renameat2_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = path_c_string(from)?;
    let to = path_c_string(to)?;
    // SAFETY: both pointers come from live `CString`s and the call only reads
    // them for the duration of the syscall.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn path_c_string(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// A kernel or file system without `RENAME_NOREPLACE`.
fn flag_unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
    )
}

/// Links a file to its new name before unlinking the old one; a folder is
/// renamed after re-checking that the destination is still absent.
fn fallback_rename(from: &Path, to: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    if metadata.file_type().is_dir() {
        match fs::symlink_metadata(to) {
            Ok(_) => Err(io::Error::from_raw_os_error(libc::EEXIST)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
            Err(error) => Err(error),
        }
    } else {
        fs::hard_link(from, to)?;
        if let Err(error) = fs::remove_file(from) {
            let _ = fs::remove_file(to);
            return Err(error);
        }
        Ok(())
    }
}

/// How a move finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMethod {
    Renamed,
    CopiedAndRemoved,
}

/// A move's result, including the state where the copy exists but the
/// original could not be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved {
        new_path: PathBuf,
        method: MoveMethod,
    },
    /// Both names exist; the user must decide which one to keep.
    CopiedOriginalKept {
        new_path: PathBuf,
        original: PathBuf,
    },
}

/// Renames a path in its current folder after confirming its identity.
pub fn rename_in_place(
    path: &Path,
    new_name: &str,
    expected: &FileIdentity,
) -> Result<PathBuf, FileActionError> {
    rename_in_place_with(&SystemFileSystem, path, new_name, expected)
}

pub fn rename_in_place_with(
    ops: &dyn FileSystemOps,
    path: &Path,
    new_name: &str,
    expected: &FileIdentity,
) -> Result<PathBuf, FileActionError> {
    validate_new_name(new_name).map_err(FileActionError::InvalidName)?;
    let current = FileIdentity::read(path)?;
    if current != *expected {
        return Err(FileActionError::Changed);
    }
    if path.file_name().and_then(|name| name.to_str()) == Some(new_name) {
        return Ok(path.to_path_buf());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(FileActionError::InvalidDestination)?;
    let new_path = parent.join(new_name);
    ops.rename_no_replace(path, &new_path)?;
    Ok(new_path)
}

/// Moves a path into `dest_dir` after confirming both the destination and the
/// path's identity.
pub fn move_to(
    path: &Path,
    dest_dir: &Path,
    expected: &FileIdentity,
) -> Result<MoveOutcome, FileActionError> {
    move_to_with(&SystemFileSystem, path, dest_dir, expected)
}

pub fn move_to_with(
    ops: &dyn FileSystemOps,
    path: &Path,
    dest_dir: &Path,
    expected: &FileIdentity,
) -> Result<MoveOutcome, FileActionError> {
    let name = path
        .file_name()
        .ok_or(FileActionError::InvalidDestination)?;
    let destination = fs::metadata(dest_dir).map_err(FileActionError::from)?;
    if !destination.is_dir() {
        return Err(FileActionError::InvalidDestination);
    }
    let source = FileIdentity::read(path)?;
    if source != *expected {
        return Err(FileActionError::Changed);
    }
    if source.kind == FileKind::Directory {
        let source_folder = fs::canonicalize(path).map_err(FileActionError::from)?;
        let destination = fs::canonicalize(dest_dir).map_err(FileActionError::from)?;
        if destination == source_folder || destination.starts_with(&source_folder) {
            return Err(FileActionError::DestinationInsideSource);
        }
    }
    let new_path = dest_dir.join(name);
    match ops.rename_no_replace(path, &new_path) {
        Ok(()) => Ok(MoveOutcome::Moved {
            new_path,
            method: MoveMethod::Renamed,
        }),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            copy_across_devices(ops, path, dest_dir, name, source, new_path)
        }
        Err(error) => Err(error.into()),
    }
}

fn copy_across_devices(
    ops: &dyn FileSystemOps,
    path: &Path,
    dest_dir: &Path,
    name: &std::ffi::OsStr,
    source: FileIdentity,
    new_path: PathBuf,
) -> Result<MoveOutcome, FileActionError> {
    match source.kind {
        FileKind::File => {}
        FileKind::Directory => return Err(FileActionError::CrossDeviceFolder),
        FileKind::Symlink => return Err(FileActionError::CrossDevice),
    }
    let partial = dest_dir.join(format!(
        ".{}.kestrel-partial-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let copied = ops
        .copy_file(path, &partial)
        .and_then(|()| ops.sync_file(&partial))
        .and_then(|()| match fs::metadata(&partial) {
            Ok(metadata) if metadata.len() == source.size => Ok(()),
            Ok(_) => Err(io::Error::from(io::ErrorKind::InvalidData)),
            Err(error) => Err(error),
        });
    if let Err(error) = copied {
        let _ = fs::remove_file(&partial);
        return Err(error.into());
    }
    if let Err(error) = ops.rename_no_replace(&partial, &new_path) {
        let _ = fs::remove_file(&partial);
        return Err(error.into());
    }
    match ops.remove_file(path) {
        Ok(()) => Ok(MoveOutcome::Moved {
            new_path,
            method: MoveMethod::CopiedAndRemoved,
        }),
        Err(_) => Ok(MoveOutcome::CopiedOriginalKept {
            new_path,
            original: path.to_path_buf(),
        }),
    }
}

/// Writes PNG bytes to a new name, or replaces an existing one when the file
/// dialog already confirmed the overwrite.
pub fn export_png(
    bytes: &[u8],
    dest_path: &Path,
    max_bytes: u64,
    replace: bool,
) -> Result<(), FileActionError> {
    let bytes_len = bytes.len() as u64;
    if bytes_len > max_bytes {
        return Err(FileActionError::OversizePng {
            bytes: bytes_len,
            limit: max_bytes,
        });
    }
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return Err(FileActionError::NotPng);
    }
    let dest_dir = dest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(FileActionError::InvalidDestination)?;
    let name = dest_path
        .file_name()
        .ok_or(FileActionError::InvalidDestination)?;
    let partial = dest_dir.join(format!(
        ".{}.kestrel-export-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if let Err(error) = write_and_sync(&partial, bytes) {
        let _ = fs::remove_file(&partial);
        return Err(error.into());
    }
    let placed = if replace {
        fs::rename(&partial, dest_path)
    } else {
        SystemFileSystem.rename_no_replace(&partial, dest_path)
    };
    if let Err(error) = placed {
        let _ = fs::remove_file(&partial);
        return Err(error.into());
    }
    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(path)?;
            OpenOptions::new().write(true).create_new(true).open(path)?
        }
        Err(error) => return Err(error),
    };
    file.write_all(bytes)?;
    file.sync_all()
}

/// How a reveal finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevealOutcome {
    /// The file manager selected the items.
    Selected,
    /// The folder opened in the named app because selection is unavailable.
    OpenedFolder { provider: Option<String> },
}

/// What the desktop offers for selecting a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealSupport {
    pub report: CapabilityReport,
    /// The default folder app's display name.
    pub provider: Option<String>,
    /// The file manager interface can select and reveal items.
    pub selects: bool,
}

impl RevealSupport {
    /// The state used when nothing was probed.
    pub fn none() -> Self {
        Self {
            report: CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "the file manager interface was not probed".to_owned(),
                },
                "The file's folder opens in your folder app.",
            ),
            provider: None,
            selects: false,
        }
    }
}

/// The display name of the app that opens folders.
pub fn reveal_provider() -> Option<String> {
    gio::AppInfo::default_for_type(DIRECTORY_TYPE, false)
        .map(|info| info.display_name().to_string())
        .filter(|name| !name.is_empty())
}

/// The probed reveal state, read once for the surfaces that name it.
pub fn reveal_support() -> RevealSupport {
    static SUPPORT: LazyLock<RevealSupport> = LazyLock::new(probe_reveal);
    SUPPORT.clone()
}

/// A fresh, read-only probe of the file manager interface.
pub fn probe_reveal() -> RevealSupport {
    let provider = reveal_provider();
    let Some((owned, activatable)) = file_manager_state() else {
        return RevealSupport {
            report: CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "no session bus".to_owned(),
                },
                "The desktop cannot select a file from here; its folder opens instead.",
            )
            .with_remediation("The file's folder opens in your folder app.")
            .with_evidence(CapabilityEvidence::new("session bus", "unavailable")),
            provider,
            selects: false,
        };
    };
    let selects = owned || activatable;
    let report = if selects {
        let status = if owned {
            CapabilityStatus::Supported
        } else {
            CapabilityStatus::Limited {
                reason: "the file manager starts when asked".to_owned(),
            }
        };
        CapabilityReport::new(
            FEATURE_ID,
            status,
            "Your file manager can show a file in its folder.",
        )
        .with_selected_backend(FILE_MANAGER_SERVICE)
        .with_evidence(CapabilityEvidence::new(
            "file manager interface",
            if owned { "running" } else { "activatable" },
        ))
    } else {
        CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Unsupported {
                reason: "no file manager interface".to_owned(),
            },
            "The desktop has no file manager interface that selects a file.",
        )
        .with_remediation("The file's folder opens in your folder app instead.")
        .with_evidence(CapabilityEvidence::new("file manager interface", "absent"))
    };
    RevealSupport {
        report,
        provider,
        selects,
    }
}

fn file_manager_state() -> Option<(bool, bool)> {
    let connection = zbus::blocking::Connection::session().ok()?;
    let dbus = zbus::blocking::fdo::DBusProxy::new(&connection).ok()?;
    let name = zbus::names::BusName::try_from(FILE_MANAGER_SERVICE).ok()?;
    let owned = dbus.name_has_owner(name).unwrap_or(false);
    let activatable = dbus
        .list_activatable_names()
        .map(|names| {
            names
                .iter()
                .any(|name| name.as_str() == FILE_MANAGER_SERVICE)
        })
        .unwrap_or(false);
    Some((owned, activatable))
}

/// Asks the file manager to select the items; opens their folder when the
/// interface is missing.
pub fn show_items(paths: &[PathBuf]) -> Result<RevealOutcome, FileActionError> {
    let support = probe_reveal();
    if support.selects && call_show_items(paths).is_ok() {
        return Ok(RevealOutcome::Selected);
    }
    let folder = paths
        .first()
        .and_then(|path| path.parent())
        .ok_or(FileActionError::InvalidDestination)?;
    open_folder(folder)?;
    Ok(RevealOutcome::OpenedFolder {
        provider: support.provider,
    })
}

fn call_show_items(paths: &[PathBuf]) -> Result<(), ()> {
    let connection = zbus::blocking::Connection::session().map_err(|_| ())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        FILE_MANAGER_SERVICE,
        FILE_MANAGER_PATH,
        FILE_MANAGER_INTERFACE,
    )
    .map_err(|_| ())?;
    let uris = paths
        .iter()
        .map(|path| gio::File::for_path(path).uri().to_string())
        .collect::<Vec<_>>();
    proxy
        .call::<_, _, ()>("ShowItems", &(uris, ""))
        .map_err(|_| ())
}

fn open_folder(folder: &Path) -> Result<(), FileActionError> {
    let info = gio::AppInfo::default_for_type(DIRECTORY_TYPE, false)
        .ok_or(FileActionError::NoFolderApp)?;
    info.launch(
        &[gio::File::for_path(folder)],
        None::<&gio::AppLaunchContext>,
    )
    .map_err(|_| FileActionError::NoFolderApp)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs, io,
        os::unix::fs::MetadataExt,
        path::{Path, PathBuf},
    };

    use super::{
        FileActionError, FileIdentity, FileKind, FileSystemOps, MoveMethod, MoveOutcome,
        NameProblem, SystemFileSystem, export_png, move_to, move_to_with, rename_in_place,
        validate_new_name,
    };

    const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];

    fn fixture(name: &str, contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join(name);
        fs::write(&path, contents).expect("fixture file");
        (directory, path)
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .expect("directory lists")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// The real file system, with an optional first-call failure.
    struct FakeOps {
        cross_device: bool,
        copy_error: Option<i32>,
        remove_error: Option<i32>,
        renames: Cell<usize>,
    }

    impl FakeOps {
        fn cross_device() -> Self {
            Self {
                cross_device: true,
                copy_error: None,
                remove_error: None,
                renames: Cell::new(0),
            }
        }
    }

    impl FileSystemOps for FakeOps {
        fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
            let calls = self.renames.get();
            self.renames.set(calls + 1);
            if self.cross_device && calls == 0 {
                return Err(io::Error::from_raw_os_error(libc::EXDEV));
            }
            SystemFileSystem.rename_no_replace(from, to)
        }

        fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()> {
            match self.copy_error {
                Some(code) => Err(io::Error::from_raw_os_error(code)),
                None => SystemFileSystem.copy_file(from, to),
            }
        }

        fn sync_file(&self, path: &Path) -> io::Result<()> {
            SystemFileSystem.sync_file(path)
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            match self.remove_error {
                Some(code) => Err(io::Error::from_raw_os_error(code)),
                None => SystemFileSystem.remove_file(path),
            }
        }
    }

    #[test]
    fn renaming_keeps_the_contents_under_the_new_name() {
        let (directory, path) = fixture("report.txt", b"hello");
        let expected = FileIdentity::read(&path).expect("identity reads");
        let renamed = rename_in_place(&path, "summary.txt", &expected).expect("rename succeeds");
        assert_eq!(renamed, directory.path().join("summary.txt"));
        assert!(!path.exists());
        assert_eq!(fs::read(&renamed).expect("the file reads"), b"hello");
    }

    #[test]
    fn an_existing_name_is_refused_and_the_original_stays() {
        let (directory, path) = fixture("report.txt", b"hello");
        fs::write(directory.path().join("taken.txt"), b"other").expect("fixture target");
        let expected = FileIdentity::read(&path).expect("identity reads");
        assert_eq!(
            rename_in_place(&path, "taken.txt", &expected),
            Err(FileActionError::Exists)
        );
        assert!(path.exists());
        assert_eq!(
            fs::read(directory.path().join("taken.txt")).expect("the target reads"),
            b"other"
        );
    }

    #[test]
    fn an_invalid_name_is_refused_before_the_file_changes() {
        for (name, problem) in [
            ("", NameProblem::Empty),
            ("   ", NameProblem::Empty),
            ("a/b.txt", NameProblem::Separator),
            ("a\0b", NameProblem::Nul),
            (".", NameProblem::DotEntry),
            ("..", NameProblem::DotEntry),
        ] {
            assert_eq!(validate_new_name(name), Err(problem), "name {name:?}");
        }
        assert_eq!(
            validate_new_name(&"a".repeat(256)),
            Err(NameProblem::TooLong)
        );
        validate_new_name(&"a".repeat(255)).expect("the longest name fits");

        let (_directory, path) = fixture("report.txt", b"hello");
        let expected = FileIdentity::read(&path).expect("identity reads");
        assert_eq!(
            rename_in_place(&path, "a/b", &expected),
            Err(FileActionError::InvalidName(NameProblem::Separator))
        );
        assert!(path.exists());
    }

    #[test]
    fn a_changed_identity_is_detected_before_the_rename() {
        let (_directory, path) = fixture("report.txt", b"hello");
        let expected = FileIdentity::read(&path).expect("identity reads");
        fs::write(&path, b"different contents").expect("the file changes");
        assert_eq!(
            rename_in_place(&path, "other.txt", &expected),
            Err(FileActionError::Changed)
        );
        assert!(path.exists());
    }

    #[test]
    fn moving_within_one_file_system_renames_the_entry() {
        let (source_dir, path) = fixture("photo.png", b"pixels");
        let destination = tempfile::TempDir::new().expect("destination");
        let expected = FileIdentity::read(&path).expect("identity reads");
        let outcome = move_to(&path, destination.path(), &expected).expect("the move succeeds");
        let new_path = destination.path().join("photo.png");
        assert_eq!(
            outcome,
            MoveOutcome::Moved {
                new_path: new_path.clone(),
                method: MoveMethod::Renamed,
            }
        );
        assert!(!path.exists());
        assert_eq!(
            fs::read(&new_path).expect("the moved file reads"),
            b"pixels"
        );
        assert!(source_dir.path().exists());
    }

    #[test]
    fn a_folder_cannot_move_inside_itself() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let folder = directory.path().join("folder");
        let child = folder.join("child");
        fs::create_dir_all(&child).expect("nested folders");
        let expected = FileIdentity::read(&folder).expect("identity reads");
        assert_eq!(
            move_to(&folder, &child, &expected),
            Err(FileActionError::DestinationInsideSource)
        );
        assert_eq!(
            move_to(&folder, &folder, &expected),
            Err(FileActionError::DestinationInsideSource)
        );
        assert!(folder.exists());
    }

    #[test]
    fn a_destination_that_is_not_a_folder_is_refused() {
        let (directory, path) = fixture("report.txt", b"hello");
        let file_destination = directory.path().join("elsewhere.txt");
        fs::write(&file_destination, b"x").expect("destination file");
        let expected = FileIdentity::read(&path).expect("identity reads");
        assert_eq!(
            move_to(&path, &file_destination, &expected),
            Err(FileActionError::InvalidDestination)
        );
    }

    #[test]
    fn export_keeps_an_existing_file_unless_replacing() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let destination = directory.path().join("image.png");
        fs::write(&destination, b"existing").expect("fixture target");
        assert_eq!(
            export_png(PNG, &destination, 1024, false),
            Err(FileActionError::Exists)
        );
        assert_eq!(
            fs::read(&destination).expect("the target reads"),
            b"existing"
        );
        export_png(PNG, &destination, 1024, true).expect("the overwrite succeeds");
        assert_eq!(fs::read(&destination).expect("the target reads"), PNG);
        assert_eq!(names(directory.path()), vec!["image.png".to_owned()]);
    }

    #[test]
    fn export_refuses_oversize_and_non_png_input() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let destination = directory.path().join("image.png");
        assert_eq!(
            export_png(PNG, &destination, 4, false),
            Err(FileActionError::OversizePng {
                bytes: PNG.len() as u64,
                limit: 4,
            })
        );
        assert_eq!(
            export_png(b"not a png", &destination, 1024, false),
            Err(FileActionError::NotPng)
        );
        assert!(!destination.exists());
        assert!(names(directory.path()).is_empty());
    }

    #[test]
    fn a_failed_cross_device_copy_leaves_the_original_and_no_partial_file() {
        let (directory, path) = fixture("large.bin", b"contents");
        let destination = tempfile::TempDir::new().expect("destination");
        let expected = FileIdentity::read(&path).expect("identity reads");
        let ops = FakeOps {
            copy_error: Some(libc::EACCES),
            ..FakeOps::cross_device()
        };
        assert_eq!(
            move_to_with(&ops, &path, destination.path(), &expected),
            Err(FileActionError::PermissionDenied)
        );
        assert!(path.exists(), "the original is untouched");
        assert!(names(destination.path()).is_empty(), "no partial file");
        assert!(directory.path().join("large.bin").exists());
    }

    #[test]
    fn a_failed_removal_after_copying_reports_both_copies() {
        let (_directory, path) = fixture("large.bin", b"contents");
        let destination = tempfile::TempDir::new().expect("destination");
        let expected = FileIdentity::read(&path).expect("identity reads");
        let ops = FakeOps {
            remove_error: Some(libc::EACCES),
            ..FakeOps::cross_device()
        };
        let outcome =
            move_to_with(&ops, &path, destination.path(), &expected).expect("the copy is kept");
        let new_path = destination.path().join("large.bin");
        assert_eq!(
            outcome,
            MoveOutcome::CopiedOriginalKept {
                new_path: new_path.clone(),
                original: path.clone(),
            }
        );
        assert!(path.exists(), "the original is still there");
        assert_eq!(fs::read(&new_path).expect("the copy reads"), b"contents");
    }

    #[test]
    fn a_cross_device_folder_is_reported_with_its_remediation() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let folder = directory.path().join("folder");
        fs::create_dir(&folder).expect("fixture folder");
        let destination = tempfile::TempDir::new().expect("destination");
        let expected = FileIdentity::read(&folder).expect("identity reads");
        let ops = FakeOps::cross_device();
        assert_eq!(
            move_to_with(&ops, &folder, destination.path(), &expected),
            Err(FileActionError::CrossDeviceFolder)
        );
        assert!(folder.exists());
        assert!(
            FileActionError::CrossDeviceFolder
                .to_string()
                .contains("file manager"),
            "the remediation names the file manager"
        );
    }

    #[test]
    fn a_symlink_is_identified_as_a_link_and_moved_as_one() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let target = directory.path().join("target.txt");
        fs::write(&target, b"target contents").expect("fixture target");
        let link = directory.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("fixture link");
        let identity = FileIdentity::read(&link).expect("identity reads");
        assert_eq!(identity.kind, FileKind::Symlink);
        assert_eq!(
            fs::metadata(&link).expect("the target is reachable").ino(),
            fs::metadata(&target).expect("target metadata").ino(),
            "the identity is the link's own"
        );
        let expected = identity;
        let renamed = rename_in_place(&link, "renamed.txt", &expected).expect("the link renames");
        assert!(
            fs::symlink_metadata(&renamed)
                .expect("the link exists")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&renamed).expect("the link reads"), target);
    }
}
