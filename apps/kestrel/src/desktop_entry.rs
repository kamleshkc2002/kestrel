//! The applications-menu entry a downloaded AppImage offers to add.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use kestrel_core::{APPLICATION_ID, APPLICATION_NAME};
use kestrel_platform::desktop_entry::{
    DesktopEntryError, EntryStatus, current_entry_directories, install, remove_managed, status,
};

/// In the state directory: whether the offer was declined.
const STATE_FILE: &str = "desktop-entry";

/// Why the entry could not be added or removed.
#[derive(Debug)]
pub enum OfferError {
    /// This process is not a downloaded AppImage.
    MissingAppImage,
    /// No XDG data home could be resolved.
    MissingDataHome,
    /// Writing or removing the entry failed.
    Desktop(DesktopEntryError),
}

impl std::fmt::Display for OfferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAppImage => write!(
                formatter,
                "the applications-menu entry is only offered when running from an AppImage"
            ),
            Self::MissingDataHome => write!(
                formatter,
                "cannot resolve the user data directory: set XDG_DATA_HOME or HOME"
            ),
            Self::Desktop(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for OfferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Desktop(error) => Some(error),
            _ => None,
        }
    }
}

/// This run's AppImage and the applications-menu entry it already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    appimage: Option<PathBuf>,
    directories: Vec<PathBuf>,
    data_home: Option<PathBuf>,
    status: EntryStatus,
    dismissed: bool,
    state_path: Option<PathBuf>,
}

impl Default for DesktopEntry {
    fn default() -> Self {
        Self {
            appimage: None,
            directories: Vec::new(),
            data_home: None,
            status: EntryStatus {
                path: None,
                managed: false,
                executable: None,
            },
            dismissed: false,
            state_path: None,
        }
    }
}

impl DesktopEntry {
    /// The current process's AppImage, directories, and remembered answer.
    pub fn from_environment() -> Self {
        Self::from_parts(
            appimage_from_environment(),
            current_entry_directories(),
            kestrel_platform::xdg::data_home(),
            state_path(),
        )
    }

    fn from_parts(
        appimage: Option<PathBuf>,
        directories: Vec<PathBuf>,
        data_home: Option<PathBuf>,
        state_path: Option<PathBuf>,
    ) -> Self {
        let mut entry = Self {
            appimage,
            directories,
            data_home,
            status: EntryStatus {
                path: None,
                managed: false,
                executable: None,
            },
            dismissed: false,
            state_path,
        };
        entry.reload();
        entry
    }

    fn reload(&mut self) {
        self.status = status(&self.directories, APPLICATION_ID);
        self.dismissed = read_dismissed(self.state_path.as_deref());
    }

    /// Whether the entry is Kestrel's and this process is an AppImage; only
    /// then can it be offered at all.
    fn available(&self) -> bool {
        self.appimage.is_some() && !kestrel_platform::sandbox::flatpak()
    }

    /// Whether Overview offers the banner.
    pub fn offers_banner(&self) -> bool {
        self.available() && !self.status.exists() && !self.dismissed
    }

    /// Whether Settings offers to add the entry.
    pub fn can_add(&self) -> bool {
        self.available() && !self.status.exists()
    }

    /// Whether Settings offers to remove an entry Kestrel wrote.
    pub fn can_remove(&self) -> bool {
        self.available() && self.status.managed
    }

    /// Whether Settings shows an applications-menu row at all.
    pub fn in_settings(&self) -> bool {
        self.can_add() || self.can_remove()
    }

    /// Writes the entry and icon, and forgets an earlier dismissal.
    pub fn add(&mut self) -> Result<(), OfferError> {
        let Some(appimage) = self.appimage.as_deref() else {
            return Err(OfferError::MissingAppImage);
        };
        let Some(data_home) = self.data_home.as_deref() else {
            return Err(OfferError::MissingDataHome);
        };
        install(data_home, APPLICATION_ID, APPLICATION_NAME, appimage)
            .map_err(OfferError::Desktop)?;
        let _ = write_dismissed(self.state_path.as_deref(), false);
        self.reload();
        Ok(())
    }

    /// Hides the offer for good; Settings still offers to add the entry.
    pub fn dismiss(&mut self) -> io::Result<()> {
        write_dismissed(self.state_path.as_deref(), true)?;
        self.dismissed = true;
        Ok(())
    }

    /// Removes Kestrel's entry and icon; another installation's entry stays.
    pub fn remove(&mut self) -> Result<(), OfferError> {
        let Some(data_home) = self.data_home.as_deref() else {
            return Err(OfferError::MissingDataHome);
        };
        remove_managed(data_home, APPLICATION_ID).map_err(OfferError::Desktop)?;
        // A removal is an answer: do not ask again on the next start.
        let _ = write_dismissed(self.state_path.as_deref(), true);
        self.reload();
        Ok(())
    }

    /// Rewrites Kestrel's entry when the AppImage path changed, so a moved
    /// AppImage keeps a working launcher. Another installation's entry stays.
    pub fn refresh_entry(&mut self) -> Result<(), OfferError> {
        if !self.status.managed {
            return Ok(());
        }
        let Some(appimage) = self.appimage.as_deref() else {
            return Ok(());
        };
        if self.status.executable.as_deref() == Some(appimage) {
            return Ok(());
        }
        self.add()
    }
}

/// An absolute `$APPIMAGE`, set by the AppImage runtime.
fn appimage_from_environment() -> Option<PathBuf> {
    env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn state_path() -> Option<PathBuf> {
    Some(crate::config::state_directory()?.join(STATE_FILE))
}

/// A missing or unreadable answer reads as a first run.
fn read_dismissed(path: Option<&Path>) -> bool {
    path.and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|contents| contents.lines().any(|line| line.trim() == "dismissed=true"))
}

/// Records the answer; `false` removes the file so the offer can return.
fn write_dismissed(path: Option<&Path>, dismissed: bool) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    if dismissed {
        crate::private_file::write_private_atomic(path, b"dismissed=true\n")
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use kestrel_core::APPLICATION_ID;
    use kestrel_platform::desktop_entry::{install, status};
    use tempfile::TempDir;

    use super::{DesktopEntry, read_dismissed, write_dismissed};

    struct Layout {
        _directory: TempDir,
        data_home: std::path::PathBuf,
        state: std::path::PathBuf,
        directories: Vec<std::path::PathBuf>,
    }

    impl Layout {
        fn new() -> Self {
            let directory = TempDir::new().expect("temporary directory");
            let data_home = directory.path().join("data");
            let state = directory.path().join("state/desktop-entry");
            let directories = vec![data_home.join("applications")];
            Self {
                _directory: directory,
                data_home,
                state,
                directories,
            }
        }

        fn entry(&self, appimage: Option<&Path>) -> DesktopEntry {
            DesktopEntry::from_parts(
                appimage.map(Path::to_path_buf),
                self.directories.clone(),
                Some(self.data_home.clone()),
                Some(self.state.clone()),
            )
        }
    }

    #[test]
    fn the_offer_needs_an_appimage_a_missing_entry_and_an_unanswered_question() {
        let layout = Layout::new();
        let appimage = Path::new("/tmp/Kestrel.AppImage");
        let offered = layout.entry(Some(appimage));
        assert!(offered.offers_banner());
        assert!(offered.can_add());
        assert!(!offered.can_remove());
        assert!(offered.in_settings());

        let not_an_appimage = layout.entry(None);
        assert!(!not_an_appimage.offers_banner());
        assert!(!not_an_appimage.in_settings());

        write_dismissed(Some(&layout.state), true).expect("answer records");
        let dismissed = layout.entry(Some(appimage));
        assert!(!dismissed.offers_banner());
        assert!(dismissed.can_add());

        let elsewhere = TempDir::new().expect("installed directory");
        fs::write(
            elsewhere.path().join(format!("{APPLICATION_ID}.desktop")),
            "[Desktop Entry]\nName=Kestrel\n",
        )
        .expect("installed entry");
        let installed = DesktopEntry::from_parts(
            Some(appimage.to_path_buf()),
            vec![elsewhere.path().into()],
            Some(layout.data_home.clone()),
            Some(layout.state.clone()),
        );
        assert!(!installed.offers_banner());
        assert!(!installed.can_add());
        assert!(!installed.can_remove());
    }

    #[test]
    fn adding_writes_the_entry_and_removing_takes_it_away_again() {
        let layout = Layout::new();
        fs::create_dir_all(layout.state.parent().expect("state folder")).expect("state folder");
        write_dismissed(Some(&layout.state), true).expect("answer records");
        let mut entry = layout.entry(Some(Path::new("/tmp/Kestrel.AppImage")));
        assert!(entry.dismissed);
        entry.add().expect("entry adds");
        assert!(!entry.can_add());
        assert!(entry.can_remove());
        assert!(!read_dismissed(Some(&layout.state)));
        assert!(
            layout
                .data_home
                .join(format!("applications/{APPLICATION_ID}.desktop"))
                .is_file()
        );
        assert!(
            layout
                .data_home
                .join(format!("icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"))
                .is_file()
        );
        entry.remove().expect("entry removes");
        assert!(entry.can_add());
        assert!(!entry.can_remove());
        assert!(read_dismissed(Some(&layout.state)));
        assert!(!entry.offers_banner());
    }

    #[test]
    fn a_moved_appimage_updates_the_launcher_it_wrote() {
        let layout = Layout::new();
        let old = Path::new("/tmp/old/Kestrel.AppImage");
        install(&layout.data_home, APPLICATION_ID, "Kestrel", old).expect("old entry installs");
        let new = Path::new("/tmp/new/Kestrel.AppImage");
        let mut entry = layout.entry(Some(new));
        assert!(entry.can_remove());
        entry.refresh_entry().expect("entry updates");
        assert_eq!(
            status(&layout.directories, APPLICATION_ID).executable,
            Some(new.to_path_buf())
        );
        assert_eq!(entry.status.executable, Some(new.to_path_buf()));
    }

    #[test]
    fn a_state_file_that_cannot_be_read_reads_as_unanswered() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("desktop-entry");
        assert!(!read_dismissed(Some(&path)));
        fs::write(&path, "other=value\n").expect("state file");
        assert!(!read_dismissed(Some(&path)));
        write_dismissed(Some(&path), true).expect("answer records");
        assert!(read_dismissed(Some(&path)));
        write_dismissed(Some(&path), false).expect("answer clears");
        assert!(!path.exists());
    }
}
