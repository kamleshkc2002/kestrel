//! Kestrel-owned user files: the exact paths an uninstall may list and remove.
//!
//! Every path here comes from a folder Kestrel names itself under the XDG
//! directories, from the autostart and desktop entries it writes, or from
//! the running binary. Removal unlinks each path in place, so a symlink is
//! removed as a link and never followed.

use std::{
    env, fmt, fs, io,
    path::{Path, PathBuf},
};

use kestrel_core::{APPLICATION_ID, DATA_DIRECTORY_NAME, DEVELOPMENT_BUILD};

use crate::xdg;

/// What an item is, which decides its wording and default selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UninstallItemKind {
    Configuration,
    Data,
    State,
    Cache,
    Recordings,
    Autostart,
    DesktopEntry,
    Icon,
    LauncherScripts,
    AppImage,
    PreviousAppImage,
    LocalBinary,
    /// The running copy lives somewhere Kestrel did not install it.
    Executable,
}

impl UninstallItemKind {
    /// Folder and metadata items start selected; recordings and the binary
    /// are the user's choice, since they hold the user's own content and
    /// running the binary is how the flow was reached.
    pub fn starts_selected(self) -> bool {
        !matches!(
            self,
            Self::Recordings
                | Self::AppImage
                | Self::PreviousAppImage
                | Self::LocalBinary
                | Self::Executable
        )
    }
}

/// One removable path, its kind, and the bytes it holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallItem {
    pub kind: UninstallItemKind,
    pub path: PathBuf,
    pub size: u64,
}

impl UninstallItem {
    pub fn new(kind: UninstallItemKind, path: PathBuf) -> Self {
        let size = path_size(&path);
        Self { kind, path, size }
    }
}

/// How this copy of Kestrel was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryInstallation {
    /// A single-file AppImage, optionally with the previous version beside it.
    AppImage {
        path: PathBuf,
        previous: Option<PathBuf>,
    },
    /// `~/.local/bin/kestrel` from `scripts/install-local.sh`.
    Local { path: PathBuf },
    /// The system's package manager owns the file.
    PackageManager,
    /// A development or copied build under the user's home.
    Executable { path: PathBuf },
    /// The running executable could not be resolved; it was already removed.
    Missing,
}

/// Everything an uninstall can remove, with the binary kept separate so a
/// package-managed install can be explained without offering to delete it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallPreview {
    pub items: Vec<UninstallItem>,
    pub binary: BinaryInstallation,
}

/// The directories the preview resolves from; explicit for tests.
#[derive(Debug, Clone)]
pub struct UninstallScope {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub state_home: PathBuf,
    pub cache_home: PathBuf,
    pub recordings_directory: PathBuf,
    pub home: PathBuf,
    pub executable: Option<PathBuf>,
    pub appimage: Option<PathBuf>,
}

impl UninstallScope {
    /// The current user's layout and running binary.
    pub fn from_environment() -> Option<Self> {
        Some(Self {
            config_home: xdg::config_home()?,
            data_home: xdg::data_home()?,
            state_home: xdg::state_home()?,
            cache_home: xdg::cache_home()?,
            recordings_directory: xdg::videos_directory()?.join(if DEVELOPMENT_BUILD {
                "Kestrel (Development)"
            } else {
                "Kestrel"
            }),
            home: xdg::home()?,
            executable: current_executable(),
            appimage: env::var_os("APPIMAGE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        })
    }
}

/// Enumerates Kestrel's paths for one scope.
#[derive(Debug, Clone)]
pub struct Uninstall {
    scope: UninstallScope,
}

impl Uninstall {
    pub fn new(scope: UninstallScope) -> Self {
        Self { scope }
    }

    pub fn from_environment() -> Option<Self> {
        Some(Self::new(UninstallScope::from_environment()?))
    }

    /// The folders and binary this preview resolves against.
    pub fn scope(&self) -> &UninstallScope {
        &self.scope
    }

    pub fn preview(&self) -> UninstallPreview {
        let scope = &self.scope;
        let mut items = Vec::new();
        let candidates = [
            (
                UninstallItemKind::Configuration,
                scope.config_home.join(DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::Data,
                scope.data_home.join(DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::State,
                scope.state_home.join(DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::Cache,
                scope.cache_home.join(DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::Recordings,
                scope.recordings_directory.clone(),
            ),
            (
                UninstallItemKind::Autostart,
                scope
                    .config_home
                    .join("autostart")
                    .join(format!("{APPLICATION_ID}.desktop")),
            ),
            (
                UninstallItemKind::DesktopEntry,
                scope
                    .data_home
                    .join("applications")
                    .join(format!("{APPLICATION_ID}.desktop")),
            ),
            (
                UninstallItemKind::Icon,
                scope
                    .data_home
                    .join("icons/hicolor/scalable/apps")
                    .join(format!("{APPLICATION_ID}.svg")),
            ),
            (
                UninstallItemKind::LauncherScripts,
                scope.data_home.join("vicinae/scripts/kestrel"),
            ),
        ];
        for (kind, path) in candidates {
            if items.iter().any(|item: &UninstallItem| item.path == path) {
                continue;
            }
            items.push(UninstallItem::new(kind, path));
        }

        let binary = self.binary();
        match &binary {
            BinaryInstallation::AppImage { path, previous } => {
                items.push(UninstallItem::new(
                    UninstallItemKind::AppImage,
                    path.clone(),
                ));
                if let Some(previous) = previous {
                    items.push(UninstallItem::new(
                        UninstallItemKind::PreviousAppImage,
                        previous.clone(),
                    ));
                }
            }
            BinaryInstallation::Local { path } => items.push(UninstallItem::new(
                UninstallItemKind::LocalBinary,
                path.clone(),
            )),
            BinaryInstallation::Executable { path } => items.push(UninstallItem::new(
                UninstallItemKind::Executable,
                path.clone(),
            )),
            BinaryInstallation::PackageManager | BinaryInstallation::Missing => {}
        }
        UninstallPreview { items, binary }
    }

    fn binary(&self) -> BinaryInstallation {
        if let Some(appimage) = &self.scope.appimage {
            let previous = previous_binary(appimage);
            return BinaryInstallation::AppImage {
                path: appimage.clone(),
                previous: previous.exists().then_some(previous),
            };
        }
        let Some(executable) = &self.scope.executable else {
            return BinaryInstallation::Missing;
        };
        let local = self.scope.home.join(".local/bin/kestrel");
        if executable == &local {
            return BinaryInstallation::Local {
                path: executable.clone(),
            };
        }
        if executable.starts_with(&self.scope.home) || !executable.is_absolute() {
            return BinaryInstallation::Executable {
                path: executable.clone(),
            };
        }
        // Distribution packages install under the system prefix; anything
        // else is a copy Kestrel did not place and only explains.
        if executable.starts_with("/usr/") {
            return BinaryInstallation::PackageManager;
        }
        BinaryInstallation::Executable {
            path: executable.clone(),
        }
    }
}

/// How the current process was started, with the kernel's `(deleted)` mark
/// removed so an already-uninstalled binary still names its old path.
fn current_executable() -> Option<PathBuf> {
    let path = env::current_exe().ok()?;
    let text = path.to_string_lossy();
    Some(match text.strip_suffix(" (deleted)") {
        Some(stripped) => PathBuf::from(stripped),
        None => path,
    })
}

fn previous_binary(path: &Path) -> PathBuf {
    let mut previous = path.as_os_str().to_os_string();
    previous.push(".previous");
    PathBuf::from(previous)
}

/// Bytes in a path without following symlinks; a missing path is zero.
fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return 0;
        };
        return entries
            .flatten()
            .map(|entry| path_size(&entry.path()))
            .sum();
    }
    if metadata.file_type().is_symlink() {
        return 0;
    }
    metadata.len()
}

/// Why one selected path was not removed.
#[derive(Debug)]
pub enum UninstallError {
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for UninstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot remove {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for UninstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// Removes exactly the item's path, or a directory's contents and the
/// directory itself. A missing path is already removed.
pub fn remove(item: &UninstallItem) -> Result<(), UninstallError> {
    remove_exact(&item.path)
}

fn remove_exact(path: &Path) -> Result<(), UninstallError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(UninstallError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| UninstallError::Io {
            path: path.to_owned(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| UninstallError::Io {
                path: path.to_owned(),
                source,
            })?;
            remove_exact(&entry.path())?;
        }
        return match fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(UninstallError::Io {
                path: path.to_owned(),
                source,
            }),
        };
    }
    // A symlink keeps its own type here, so this unlinks it rather than
    // touching whatever it points at.
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(UninstallError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
    };

    use tempfile::TempDir;

    use super::{
        BinaryInstallation, Uninstall, UninstallItem, UninstallItemKind, UninstallScope, remove,
    };

    struct Layout {
        _directory: TempDir,
        root: PathBuf,
        home: PathBuf,
        config: PathBuf,
        data: PathBuf,
        state: PathBuf,
        cache: PathBuf,
        recordings: PathBuf,
        executable: PathBuf,
    }

    impl Layout {
        fn new() -> Self {
            let directory = TempDir::new().expect("temporary directory");
            let root = directory.path().to_owned();
            let home = root.join("home");
            let config = root.join("config");
            let data = root.join("data");
            let state = root.join("state");
            let cache = root.join("cache");
            let recordings = root.join("videos/Kestrel");
            let executable = home.join(".local/bin/kestrel");
            for directory in [
                config.join(kestrel_core::DATA_DIRECTORY_NAME),
                data.join(kestrel_core::DATA_DIRECTORY_NAME),
                state.join(kestrel_core::DATA_DIRECTORY_NAME),
                cache.join(kestrel_core::DATA_DIRECTORY_NAME),
                recordings.clone(),
                home.join(".local/bin"),
            ] {
                fs::create_dir_all(directory).expect("folder");
            }
            for file in [
                config
                    .join(kestrel_core::DATA_DIRECTORY_NAME)
                    .join("config.toml"),
                data.join(kestrel_core::DATA_DIRECTORY_NAME)
                    .join("scratchpad.json"),
                state
                    .join(kestrel_core::DATA_DIRECTORY_NAME)
                    .join("last-version"),
                cache
                    .join(kestrel_core::DATA_DIRECTORY_NAME)
                    .join("cached.bin"),
                recordings.join("take.webm"),
                config
                    .join("autostart")
                    .join(format!("{}.desktop", kestrel_core::APPLICATION_ID)),
                data.join("applications")
                    .join(format!("{}.desktop", kestrel_core::APPLICATION_ID)),
                data.join("icons/hicolor/scalable/apps")
                    .join(format!("{}.svg", kestrel_core::APPLICATION_ID)),
                data.join("vicinae/scripts/kestrel/kestrel-window-show.sh"),
                executable.clone(),
            ] {
                if let Some(parent) = file.parent() {
                    fs::create_dir_all(parent).expect("parent folder");
                }
                fs::write(file, b"kestrel").expect("file");
            }
            Self {
                _directory: directory,
                root,
                home,
                config,
                data,
                state,
                cache,
                recordings,
                executable,
            }
        }

        fn uninstall(&self) -> Uninstall {
            Uninstall::new(UninstallScope {
                config_home: self.config.clone(),
                data_home: self.data.clone(),
                state_home: self.state.clone(),
                cache_home: self.cache.clone(),
                recordings_directory: self.recordings.clone(),
                home: self.home.clone(),
                executable: Some(self.executable.clone()),
                appimage: None,
            })
        }

        fn item(&self, kind: UninstallItemKind) -> UninstallItem {
            self.uninstall()
                .preview()
                .items
                .into_iter()
                .find(|item| item.kind == kind)
                .expect("item is listed")
        }
    }

    #[test]
    fn enumeration_covers_every_owned_path_and_the_installed_binary() {
        let layout = Layout::new();
        let preview = layout.uninstall().preview();

        for (kind, path) in [
            (
                UninstallItemKind::Configuration,
                layout.config.join(kestrel_core::DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::Data,
                layout.data.join(kestrel_core::DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::State,
                layout.state.join(kestrel_core::DATA_DIRECTORY_NAME),
            ),
            (
                UninstallItemKind::Cache,
                layout.cache.join(kestrel_core::DATA_DIRECTORY_NAME),
            ),
            (UninstallItemKind::Recordings, layout.recordings.clone()),
            (
                UninstallItemKind::Autostart,
                layout
                    .config
                    .join("autostart")
                    .join(format!("{}.desktop", kestrel_core::APPLICATION_ID)),
            ),
            (
                UninstallItemKind::DesktopEntry,
                layout
                    .data
                    .join("applications")
                    .join(format!("{}.desktop", kestrel_core::APPLICATION_ID)),
            ),
            (
                UninstallItemKind::Icon,
                layout
                    .data
                    .join("icons/hicolor/scalable/apps")
                    .join(format!("{}.svg", kestrel_core::APPLICATION_ID)),
            ),
            (
                UninstallItemKind::LauncherScripts,
                layout.data.join("vicinae/scripts/kestrel"),
            ),
            (UninstallItemKind::LocalBinary, layout.executable.clone()),
        ] {
            let item = preview
                .items
                .iter()
                .find(|item| item.kind == kind)
                .unwrap_or_else(|| panic!("{kind:?} is listed"));
            assert_eq!(item.path, path, "{kind:?}");
            assert!(item.size > 0, "{kind:?} has a size");
            let expected_selected = !matches!(
                kind,
                UninstallItemKind::Recordings
                    | UninstallItemKind::AppImage
                    | UninstallItemKind::PreviousAppImage
                    | UninstallItemKind::LocalBinary
                    | UninstallItemKind::Executable
            );
            assert_eq!(
                item.kind.starts_selected(),
                expected_selected,
                "{kind:?} default"
            );
        }
        assert!(!UninstallItemKind::Recordings.starts_selected());
        assert!(!UninstallItemKind::LocalBinary.starts_selected());
        assert!(UninstallItemKind::Configuration.starts_selected());
        assert_eq!(
            preview.binary,
            BinaryInstallation::Local {
                path: layout.executable.clone()
            }
        );
    }

    #[test]
    fn deletion_removes_only_the_selected_item() {
        let layout = Layout::new();
        remove(&layout.item(UninstallItemKind::Data)).expect("remove data");

        assert!(!layout.data.join(kestrel_core::DATA_DIRECTORY_NAME).exists());
        assert!(
            layout
                .config
                .join(kestrel_core::DATA_DIRECTORY_NAME)
                .join("config.toml")
                .exists()
        );
        assert!(layout.executable.exists(), "the binary was not selected");
    }

    #[test]
    fn deletion_unlinks_symlinks_without_following_them() {
        let layout = Layout::new();
        let outside = layout.root.join("outside");
        fs::create_dir_all(&outside).expect("outside folder");
        let kept = outside.join("keep.txt");
        fs::write(&kept, b"not Kestrel's").expect("outside file");
        symlink(
            &outside,
            layout
                .data
                .join(kestrel_core::DATA_DIRECTORY_NAME)
                .join("escape"),
        )
        .expect("inner symlink");

        remove(&layout.item(UninstallItemKind::Data)).expect("remove data");
        assert!(!layout.data.join(kestrel_core::DATA_DIRECTORY_NAME).exists());
        assert!(
            kept.exists(),
            "a symlink inside the folder was not followed"
        );

        let linked = layout.state.join("linked-state");
        symlink(&outside, &linked).expect("item symlink");
        remove(&UninstallItem::new(
            UninstallItemKind::State,
            linked.clone(),
        ))
        .expect("remove linked state");
        assert!(linked.symlink_metadata().is_err(), "the link is gone");
        assert!(kept.exists(), "the item path itself was not followed");
    }

    #[test]
    fn a_missing_path_is_already_removed() {
        let layout = Layout::new();
        let missing = layout.root.join("never-created");
        remove(&UninstallItem::new(UninstallItemKind::Cache, missing))
            .expect("missing path is fine");
    }

    #[test]
    fn an_appimage_lists_both_versions_and_leaves_the_choice_to_the_user() {
        let layout = Layout::new();
        let appimage = layout.root.join("Apps/Kestrel.AppImage");
        fs::create_dir_all(appimage.parent().expect("folder")).expect("apps folder");
        fs::write(&appimage, b"appimage").expect("appimage");
        let previous = PathBuf::from(format!("{}.previous", appimage.display()));
        fs::write(&previous, b"previous").expect("previous appimage");

        let uninstall = Uninstall::new(UninstallScope {
            executable: Some(PathBuf::from("/tmp/.mount/usr/bin/kestrel")),
            appimage: Some(appimage.clone()),
            ..layout.uninstall().scope().clone()
        });
        let preview = uninstall.preview();

        assert_eq!(
            preview.binary,
            BinaryInstallation::AppImage {
                path: appimage.clone(),
                previous: Some(previous.clone())
            }
        );
        for kind in [
            UninstallItemKind::AppImage,
            UninstallItemKind::PreviousAppImage,
        ] {
            let item = preview
                .items
                .iter()
                .find(|item| item.kind == kind)
                .unwrap_or_else(|| panic!("{kind:?} is listed"));
            assert!(!item.kind.starts_selected(), "{kind:?} starts unchecked");
        }
        assert!(
            preview
                .items
                .iter()
                .any(|item| item.path == appimage && item.size > 0)
        );
        assert!(
            preview
                .items
                .iter()
                .any(|item| item.path == previous && item.size > 0)
        );
    }

    #[test]
    fn a_system_binary_is_left_to_the_package_manager() {
        let layout = Layout::new();
        let uninstall = Uninstall::new(UninstallScope {
            executable: Some(Path::new("/usr/bin/kestrel").to_owned()),
            ..layout.uninstall().scope().clone()
        });
        let preview = uninstall.preview();

        assert_eq!(preview.binary, BinaryInstallation::PackageManager);
        assert!(
            !preview
                .items
                .iter()
                .any(|item| item.path == layout.executable)
        );
    }
}
