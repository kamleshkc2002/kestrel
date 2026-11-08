//! Hand-off of packages and disk images to the desktop's default app.
//! Classification reads a bounded header; the hand-off only launches the
//! chosen app with the selected file and never installs anything itself.

use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use gio::prelude::AppInfoExt;

/// The largest header read while classifying a file.
const HEADER_BYTES: u64 = 4_096;

/// What a supported file is, or that nothing handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTargetKind {
    DebianPackage,
    RpmPackage,
    FlatpakRef,
    FlatpakBundle,
    SnapPackage,
    DiskImage,
    Unsupported,
}

impl InstallTargetKind {
    /// The plain name shown to the user.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DebianPackage => "Debian package",
            Self::RpmPackage => "RPM package",
            Self::FlatpakRef => "Flatpak reference",
            Self::FlatpakBundle => "Flatpak bundle",
            Self::SnapPackage => "Snap package",
            Self::DiskImage => "Disk image",
            Self::Unsupported => "unsupported file",
        }
    }

    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    /// Whether the default app installs this or opens it.
    pub const fn opens(self) -> bool {
        matches!(self, Self::DiskImage)
    }
}

/// Why a file could not be classified or handed off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffErrorKind {
    /// The path no longer exists.
    NotFound,
    /// The path is not a regular file.
    NotRegular,
    /// The file is empty.
    Empty,
    /// The file could not be read.
    Unreadable,
    /// The contents do not match the type the name claims.
    Mismatch,
    /// The file changed after it was classified.
    Changed,
    /// The app could not be launched.
    LaunchFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffError {
    pub kind: HandoffErrorKind,
    /// Written by Kestrel; carries no path or file name.
    pub message: String,
}

impl HandoffError {
    pub fn new(kind: HandoffErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn not_found() -> Self {
        Self::new(HandoffErrorKind::NotFound, "The file is no longer there.")
    }

    fn unreadable() -> Self {
        Self::new(HandoffErrorKind::Unreadable, "The file could not be read.")
    }

    fn launch_failed() -> Self {
        Self::new(
            HandoffErrorKind::LaunchFailed,
            "The app could not open the file.",
        )
    }
}

impl fmt::Display for HandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HandoffError {}

/// The stable fields of a filesystem object, compared before the hand-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
        }
    }
}

/// A validated file and the type the desktop should handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub kind: InstallTargetKind,
    pub content_type: String,
    pub size: u64,
    /// The selected path, unchanged.
    pub path: PathBuf,
    identity: FileIdentity,
}

impl InstallTarget {
    /// The file name shown in the confirmation; never logged.
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "The file".to_owned())
    }
}

/// The app the desktop would use for a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub app_id: String,
    pub name: String,
    /// The default handler for a package can be an archive manager, which
    /// usually lists the contents instead of installing it.
    pub archive_manager: bool,
}

/// The GIO lookups the hand-off needs; tests replace the host's handlers.
pub trait DesktopApps {
    fn default_for_type(&self, content_type: &str) -> Option<DesktopApp>;
    fn find(&self, app_id: &str) -> Option<DesktopApp>;
    fn launch(&self, app_id: &str, path: &Path) -> Result<(), HandoffError>;
}

/// The parts of a desktop entry the hand-off reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub app_id: String,
    pub name: String,
    pub supported_types: Vec<String>,
}

/// The live GIO registry.
#[derive(Debug, Clone, Copy, Default)]
pub struct GioDesktopApps;

impl DesktopApps for GioDesktopApps {
    fn default_for_type(&self, content_type: &str) -> Option<DesktopApp> {
        gio::AppInfo::default_for_type(content_type, false).map(desktop_app)
    }

    fn find(&self, app_id: &str) -> Option<DesktopApp> {
        gio::AppInfo::all()
            .into_iter()
            .find(|info| info.id().is_some_and(|id| id == app_id))
            .map(desktop_app)
    }

    fn launch(&self, app_id: &str, path: &Path) -> Result<(), HandoffError> {
        let info = gio::AppInfo::all()
            .into_iter()
            .find(|info| info.id().is_some_and(|id| id == app_id))
            .ok_or_else(HandoffError::launch_failed)?;
        info.launch(&[gio::File::for_path(path)], None::<&gio::AppLaunchContext>)
            .map_err(|_| HandoffError::launch_failed())
    }
}

fn desktop_app(info: gio::AppInfo) -> DesktopApp {
    DesktopApp {
        app_id: info.id().map(|id| id.to_string()).unwrap_or_default(),
        name: info.name().to_string(),
        supported_types: info
            .supported_types()
            .into_iter()
            .map(|content_type| content_type.to_string())
            .collect(),
    }
}

/// The supported kind named by a path's extension, without touching the file.
pub fn kind_for_path(path: &Path) -> Option<InstallTargetKind> {
    match extension_kind(path) {
        InstallTargetKind::Unsupported => None,
        kind => Some(kind),
    }
}

/// Checks a file and its contents, returning the type to hand off.
pub fn classify(path: &Path) -> Result<InstallTarget, HandoffError> {
    let file = fs::File::open(path).map_err(open_error)?;
    let metadata = file.metadata().map_err(open_error)?;
    if !metadata.file_type().is_file() {
        return Err(HandoffError::new(
            HandoffErrorKind::NotRegular,
            "That path is not a regular file.",
        ));
    }
    let size = metadata.size();
    if size == 0 {
        return Err(HandoffError::new(
            HandoffErrorKind::Empty,
            "The file is empty.",
        ));
    }
    let mut header = Vec::new();
    file.take(HEADER_BYTES)
        .read_to_end(&mut header)
        .map_err(|_| HandoffError::unreadable())?;
    let (content_type, _) = gio::content_type_guess(Some(path), Some(header.as_slice()));
    let (content_header_type, _) = gio::content_type_guess(None::<&Path>, Some(header.as_slice()));
    let kind = extension_kind(path);
    if kind.is_supported() && !contents_agree(kind, content_header_type.as_str()) {
        return Err(HandoffError::new(
            HandoffErrorKind::Mismatch,
            "The file's contents do not match its name.",
        ));
    }
    Ok(InstallTarget {
        kind,
        content_type: content_type.to_string(),
        size,
        path: path.to_path_buf(),
        identity: FileIdentity::from_metadata(&metadata),
    })
}

/// The app registered for a content type, excluding this application.
pub fn provider_for(content_type: &str) -> Option<Provider> {
    provider_for_with(&GioDesktopApps, content_type)
}

/// The app registered for a content type through an injectable registry.
pub fn provider_for_with(apps: &dyn DesktopApps, content_type: &str) -> Option<Provider> {
    let app = apps.default_for_type(content_type)?;
    if app.app_id.starts_with("io.github.kamleshkc2002.Kestrel") {
        return None;
    }
    let package =
        content_kind(content_type).is_some_and(|kind| kind != InstallTargetKind::DiskImage);
    let archive_manager = package && is_archive_manager(&app);
    Some(Provider {
        app_id: app.app_id,
        name: app.name,
        archive_manager,
    })
}

/// Launches the confirmed app with the selected file after re-checking it.
pub fn hand_off(
    target: &InstallTarget,
    provider: &Provider,
    apps: &dyn DesktopApps,
) -> Result<HandoffOutcome, HandoffError> {
    recheck(target)?;
    if apps.find(&provider.app_id).is_none() {
        return Err(HandoffError::launch_failed());
    }
    apps.launch(&provider.app_id, &target.path)?;
    Ok(HandoffOutcome::Launched {
        provider_name: provider.name.clone(),
    })
}

/// What the hand-off did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffOutcome {
    Launched { provider_name: String },
}

/// The identity check performed before any launch.
fn recheck(target: &InstallTarget) -> Result<(), HandoffError> {
    let metadata = fs::metadata(&target.path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            HandoffError::not_found()
        } else {
            HandoffError::new(
                HandoffErrorKind::Changed,
                "The file changed after it was selected; choose it again.",
            )
        }
    })?;
    if !metadata.file_type().is_file() || FileIdentity::from_metadata(&metadata) != target.identity
    {
        return Err(HandoffError::new(
            HandoffErrorKind::Changed,
            "The file changed after it was selected; choose it again.",
        ));
    }
    Ok(())
}

fn open_error(error: io::Error) -> HandoffError {
    if error.kind() == io::ErrorKind::NotFound {
        HandoffError::not_found()
    } else {
        HandoffError::unreadable()
    }
}

fn extension_kind(path: &Path) -> InstallTargetKind {
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("deb") => InstallTargetKind::DebianPackage,
        Some("rpm") => InstallTargetKind::RpmPackage,
        Some("flatpakref") => InstallTargetKind::FlatpakRef,
        Some("flatpak") => InstallTargetKind::FlatpakBundle,
        Some("snap") => InstallTargetKind::SnapPackage,
        Some("iso" | "img") => InstallTargetKind::DiskImage,
        _ => InstallTargetKind::Unsupported,
    }
}

/// Whether the sniffed type is consistent with the extension.
fn contents_agree(kind: InstallTargetKind, content_type: &str) -> bool {
    if kind == InstallTargetKind::DiskImage {
        return !content_type.starts_with("text/")
            && content_kind(content_type).is_none_or(|found| found == kind);
    }
    content_kind(content_type) == Some(kind)
}

fn content_kind(content_type: &str) -> Option<InstallTargetKind> {
    match content_type {
        "application/vnd.debian.binary-package" | "application/x-archive" => {
            Some(InstallTargetKind::DebianPackage)
        }
        "application/x-rpm" | "application/x-rpm-package" => Some(InstallTargetKind::RpmPackage),
        "application/vnd.flatpak.ref" => Some(InstallTargetKind::FlatpakRef),
        "application/vnd.flatpak" | "application/vnd.xdgapp" => {
            Some(InstallTargetKind::FlatpakBundle)
        }
        "application/vnd.snap" | "application/vnd.squashfs" => Some(InstallTargetKind::SnapPackage),
        "application/vnd.efi.iso"
        | "application/vnd.efi.img"
        | "application/x-cd-image"
        | "application/x-iso9660-image"
        | "application/x-raw-disk-image" => Some(InstallTargetKind::DiskImage),
        _ => None,
    }
}

fn is_archive_manager(app: &DesktopApp) -> bool {
    let supports = |content_type: &str| {
        app.supported_types
            .iter()
            .any(|supported| supported == content_type)
    };
    supports("application/zip")
        && (supports("application/x-tar") || supports("application/x-archive"))
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
    };

    use tempfile::TempDir;

    use super::{
        DesktopApp, DesktopApps, HandoffErrorKind, HandoffOutcome, InstallTarget,
        InstallTargetKind, Provider, classify, hand_off, kind_for_path, provider_for_with,
    };

    fn write(directory: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.path().join(name);
        fs::write(&path, bytes).expect("fixture writes");
        path
    }

    fn rpm_lead() -> Vec<u8> {
        let mut lead = vec![0xed, 0xab, 0xee, 0xdb, 0x03, 0x00, 0x00, 0x00];
        lead.resize(96, 0);
        lead
    }

    fn iso_image() -> Vec<u8> {
        let mut image = vec![0u8; 0x8001];
        image.extend_from_slice(b"CD001");
        image.push(1);
        image
    }

    #[test]
    fn classification_uses_the_extension_and_header_together() {
        let directory = TempDir::new().expect("temporary directory");
        let deb = write(
            &directory,
            "example.deb",
            b"!<arch>\ndebian-binary   1.0           \n",
        );
        let rpm = write(&directory, "example.rpm", &rpm_lead());
        let iso = write(&directory, "example.iso", &iso_image());
        let flatpakref = write(
            &directory,
            "example.flatpakref",
            b"[Flatpak Ref]\nUrl=https://example.invalid/repo\nName=Example\n",
        );
        let renamed = write(&directory, "notes.deb", b"This is not a package.\n");
        let empty = write(&directory, "empty.deb", b"");
        let text = write(&directory, "notes.txt", b"plain text\n");
        let missing = directory.path().join("missing.deb");

        assert_eq!(
            classify(&deb).expect("a deb classifies").kind,
            InstallTargetKind::DebianPackage
        );
        assert_eq!(
            classify(&rpm).expect("an rpm classifies").kind,
            InstallTargetKind::RpmPackage
        );
        assert_eq!(
            classify(&iso).expect("an iso classifies").kind,
            InstallTargetKind::DiskImage
        );
        assert_eq!(
            classify(&flatpakref).expect("a flatpakref classifies").kind,
            InstallTargetKind::FlatpakRef
        );
        assert_eq!(
            classify(&renamed).expect_err("text renamed .deb").kind,
            HandoffErrorKind::Mismatch
        );
        assert_eq!(
            classify(&empty).expect_err("empty file").kind,
            HandoffErrorKind::Empty
        );
        assert_eq!(
            classify(directory.path())
                .expect_err("a directory is not a file")
                .kind,
            HandoffErrorKind::NotRegular
        );
        assert_eq!(
            classify(&missing).expect_err("missing file").kind,
            HandoffErrorKind::NotFound
        );
        assert_eq!(
            classify(&text)
                .expect("an unsupported file still classifies")
                .kind,
            InstallTargetKind::Unsupported
        );
    }

    #[test]
    fn a_symlink_is_checked_through_to_its_target() {
        let directory = TempDir::new().expect("temporary directory");
        let real = write(
            &directory,
            "real.deb",
            b"!<arch>\ndebian-binary   1.0           \n",
        );
        let link = directory.path().join("link.deb");
        symlink(&real, &link).expect("symlink");

        let target = classify(&link).expect("the target is a package");
        assert_eq!(target.kind, InstallTargetKind::DebianPackage);
        assert_eq!(target.path, link);
    }

    #[test]
    fn only_supported_extensions_offer_the_handoff_hint() {
        assert_eq!(
            kind_for_path(Path::new("/tmp/example.deb")),
            Some(InstallTargetKind::DebianPackage)
        );
        assert_eq!(
            kind_for_path(Path::new("/tmp/example.ISO")),
            Some(InstallTargetKind::DiskImage)
        );
        assert_eq!(kind_for_path(Path::new("/tmp/example.txt")), None);
    }

    struct FakeApps {
        apps: Vec<DesktopApp>,
        launched: RefCell<Vec<(String, PathBuf)>>,
    }

    impl FakeApps {
        fn with(app: DesktopApp) -> Self {
            Self {
                apps: vec![app],
                launched: RefCell::new(Vec::new()),
            }
        }
    }

    impl DesktopApps for FakeApps {
        fn default_for_type(&self, _content_type: &str) -> Option<DesktopApp> {
            self.apps.first().cloned()
        }

        fn find(&self, app_id: &str) -> Option<DesktopApp> {
            self.apps.iter().find(|app| app.app_id == app_id).cloned()
        }

        fn launch(&self, app_id: &str, path: &Path) -> Result<(), super::HandoffError> {
            self.launched
                .borrow_mut()
                .push((app_id.to_owned(), path.to_path_buf()));
            Ok(())
        }
    }

    fn installer() -> DesktopApp {
        DesktopApp {
            app_id: "example.installer".to_owned(),
            name: "Software Center".to_owned(),
            supported_types: Vec::new(),
        }
    }

    #[test]
    fn provider_resolution_excludes_this_application_and_flags_archive_managers() {
        let mine = DesktopApp {
            app_id: "io.github.kamleshkc2002.Kestrel.Devel".to_owned(),
            name: "Kestrel (Development)".to_owned(),
            supported_types: Vec::new(),
        };
        let apps = FakeApps::with(mine);
        assert!(
            provider_for_with(&apps, "application/vnd.debian.binary-package").is_none(),
            "this application is never its own provider"
        );

        let archive = DesktopApp {
            supported_types: vec!["application/zip".to_owned(), "application/x-tar".to_owned()],
            ..installer()
        };
        let apps = FakeApps::with(archive);
        let provider = provider_for_with(&apps, "application/vnd.debian.binary-package")
            .expect("an archive manager is still a provider");
        assert!(provider.archive_manager);
    }

    fn classified(directory: &TempDir) -> InstallTarget {
        let path = write(
            directory,
            "example.deb",
            b"!<arch>\ndebian-binary   1.0           \n",
        );
        classify(&path).expect("a deb classifies")
    }

    fn provider() -> Provider {
        Provider {
            app_id: "example.installer".to_owned(),
            name: "Software Center".to_owned(),
            archive_manager: false,
        }
    }

    #[test]
    fn a_handoff_launches_the_provider_with_the_same_file_and_leaves_it_alone() {
        let directory = TempDir::new().expect("temporary directory");
        let target = classified(&directory);
        let original = fs::read(&target.path).expect("fixture reads");
        let apps = FakeApps::with(installer());

        let outcome = hand_off(&target, &provider(), &apps).expect("the hand-off launches");
        assert_eq!(
            outcome,
            HandoffOutcome::Launched {
                provider_name: "Software Center".to_owned()
            }
        );
        assert_eq!(
            apps.launched.borrow().as_slice(),
            [("example.installer".to_owned(), target.path.clone())]
        );
        assert_eq!(fs::read(&target.path).expect("fixture stays"), original);
    }

    #[test]
    fn a_file_that_changed_after_classification_is_not_launched() {
        let directory = TempDir::new().expect("temporary directory");
        let target = classified(&directory);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&target.path)
            .expect("fixture opens");
        std::io::Write::write_all(&mut file, b"more").expect("fixture appends");
        let apps = FakeApps::with(installer());

        let error = hand_off(&target, &provider(), &apps).expect_err("the file changed");
        assert_eq!(error.kind, HandoffErrorKind::Changed);
        assert!(apps.launched.borrow().is_empty());
        assert!(target.path.exists(), "the file is preserved");
    }

    #[test]
    fn a_provider_that_vanished_fails_without_launching() {
        let directory = TempDir::new().expect("temporary directory");
        let target = classified(&directory);
        let apps = FakeApps {
            apps: Vec::new(),
            launched: RefCell::new(Vec::new()),
        };

        let error = hand_off(&target, &provider(), &apps).expect_err("no app is left");
        assert_eq!(error.kind, HandoffErrorKind::LaunchFailed);
        assert!(apps.launched.borrow().is_empty());
    }
}
