//! The applications-menu entry a downloaded AppImage offers to add.
//!
//! Desktops find an app's name and icon through the desktop entry for its
//! application ID. A downloaded AppImage has none, so this writes one under
//! the user data home together with the icon, and recognizes its own entries
//! so a later removal leaves another installation's entry alone.

use std::{
    env,
    ffi::OsStr,
    fmt, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

/// The search path and entry layout are part of the freedesktop desktop-entry
/// specification, not Kestrel policy.
const TEMPLATE: &str = include_str!("../../../data/io.github.kamleshkc2002.Kestrel.desktop");
const ICON: &str = include_str!("../../../data/icons/io.github.kamleshkc2002.Kestrel.svg");
const TEMPLATE_APP_ID: &str = "io.github.kamleshkc2002.Kestrel";
const MANAGED_MARKER: &str = "X-Kestrel-AppImage=true";
const ENTRY_MODE: u32 = 0o644;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure rendering or writing the applications-menu entry.
#[derive(Debug)]
pub enum DesktopEntryError {
    /// No XDG data home could be resolved from the environment.
    MissingDataHome,
    /// XDG data directories and the AppImage path must be absolute.
    NotAbsolute { path: PathBuf },
    /// Desktop entry values must be valid UTF-8.
    NotUtf8 { path: PathBuf },
    /// A control character would invalidate a desktop entry value.
    ContainsControlCharacter { path: PathBuf },
    /// A path has no parent directory.
    MissingParent { path: PathBuf },
    /// Creating a directory failed.
    CreateDirectory { path: PathBuf, source: io::Error },
    /// Writing a temporary file failed.
    Write { path: PathBuf, source: io::Error },
    /// Renaming the finished file into place failed.
    Install {
        from: PathBuf,
        to: PathBuf,
        source: io::Error,
    },
    /// Reading or removing an entry failed.
    Remove { path: PathBuf, source: io::Error },
}

impl fmt::Display for DesktopEntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDataHome => {
                write!(
                    formatter,
                    "cannot resolve the user data directory: set XDG_DATA_HOME or HOME"
                )
            }
            Self::NotAbsolute { .. } => {
                write!(
                    formatter,
                    "the user data directory and AppImage path must be absolute"
                )
            }
            Self::NotUtf8 { .. } => write!(formatter, "an AppImage path is not valid UTF-8"),
            Self::ContainsControlCharacter { .. } => {
                write!(formatter, "an AppImage path contains a control character")
            }
            Self::MissingParent { .. } => {
                write!(
                    formatter,
                    "the applications-menu path has no parent directory"
                )
            }
            Self::CreateDirectory { source, .. } => {
                write!(
                    formatter,
                    "cannot create the applications-menu folder: {source}"
                )
            }
            Self::Write { source, .. } => {
                write!(
                    formatter,
                    "cannot write the applications-menu file: {source}"
                )
            }
            Self::Install { source, .. } => {
                write!(
                    formatter,
                    "cannot install the applications-menu file: {source}"
                )
            }
            Self::Remove { source, .. } => {
                write!(
                    formatter,
                    "cannot remove the applications-menu file: {source}"
                )
            }
        }
    }
}

impl std::error::Error for DesktopEntryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. }
            | Self::Write { source, .. }
            | Self::Install { source, .. }
            | Self::Remove { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// What the XDG application directories hold for one app ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryStatus {
    /// The first entry found, whichever installation wrote it.
    pub path: Option<PathBuf>,
    /// The entry carries Kestrel's AppImage marker.
    pub managed: bool,
    /// The executable a managed entry starts.
    pub executable: Option<PathBuf>,
}

impl EntryStatus {
    pub fn exists(&self) -> bool {
        self.path.is_some()
    }
}

/// The XDG application directories in search order.
///
/// `$XDG_DATA_HOME/applications` comes first, then each absolute directory in
/// `$XDG_DATA_DIRS`, defaulting to `/usr/local/share` and `/usr/share`.
pub fn entry_directories(
    data_home: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let data_home = data_home
        .filter(|value| !value.is_empty() && Path::new(value).is_absolute())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(data_home) = data_home {
        roots.push(data_home.join("applications"));
    }
    let mut data_dirs = data_dirs
        .filter(|value| !value.is_empty())
        .map(|value| {
            env::split_paths(value)
                .filter(|directory| directory.is_absolute())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if data_dirs.is_empty() {
        data_dirs = vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ];
    }
    roots.extend(
        data_dirs
            .into_iter()
            .map(|directory| directory.join("applications")),
    );
    roots
}

/// The application directories for the current process.
pub fn current_entry_directories() -> Vec<PathBuf> {
    entry_directories(
        env::var_os("XDG_DATA_HOME").as_deref(),
        env::var_os("XDG_DATA_DIRS").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

/// The desktop entry for `app_id`, rendered from the packaged template.
///
/// The executable replaces both `Exec` lines, `TryExec` names the file a
/// launcher can check, and the marker lets a later removal recognize the
/// entry as Kestrel's own AppImage entry.
pub fn render_entry(
    app_id: &str,
    name: &str,
    executable: &Path,
) -> Result<String, DesktopEntryError> {
    let executable_text = path_text(executable)?;
    let quoted = quote_exec_argument(executable_text);
    let mut lines = TEMPLATE
        .lines()
        .map(|line| line.replace(TEMPLATE_APP_ID, app_id))
        .collect::<Vec<_>>();
    for line in &mut lines {
        if line == "Name=Kestrel" && name != "Kestrel" {
            *line = format!("Name={name}");
        }
    }
    for line in &mut lines {
        let Some(rest) = line.strip_prefix("Exec=kestrel") else {
            continue;
        };
        if rest.is_empty() || rest.starts_with(' ') {
            *line = format!("Exec={quoted}{rest}");
        }
    }
    lines.retain(|line| !line.starts_with("TryExec="));
    if let Some(position) = lines.iter().position(|line| line.starts_with("Exec=")) {
        lines.insert(position + 1, format!("TryExec={executable_text}"));
    }
    if let Some(position) = lines.iter().position(|line| line == "Type=Application") {
        lines.insert(position + 1, MANAGED_MARKER.to_owned());
    }
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    Ok(rendered)
}

/// The first entry for `app_id` in `directories`.
pub fn find_entry(directories: &[PathBuf], app_id: &str) -> Option<PathBuf> {
    directories
        .iter()
        .map(|directory| directory.join(format!("{app_id}.desktop")))
        .find(|path| path.is_file())
}

/// Whether `directories` already name `app_id`, and whether Kestrel wrote it.
pub fn status(directories: &[PathBuf], app_id: &str) -> EntryStatus {
    let Some(path) = find_entry(directories, app_id) else {
        return EntryStatus {
            path: None,
            managed: false,
            executable: None,
        };
    };
    let contents = fs::read_to_string(&path).ok();
    let managed = contents.as_deref().is_some_and(is_managed);
    let executable = contents
        .as_deref()
        .filter(|_| managed)
        .and_then(managed_executable);
    EntryStatus {
        path: Some(path),
        managed,
        executable,
    }
}

/// Writes the entry and icon under `data_home`; returns the entry path.
pub fn install(
    data_home: &Path,
    app_id: &str,
    name: &str,
    executable: &Path,
) -> Result<PathBuf, DesktopEntryError> {
    if !data_home.is_absolute() {
        return Err(DesktopEntryError::NotAbsolute {
            path: data_home.to_path_buf(),
        });
    }
    let entry = render_entry(app_id, name, executable)?;
    let entry_path = data_home
        .join("applications")
        .join(format!("{app_id}.desktop"));
    let icon_path = icon_path(data_home, app_id);
    write_atomic(&icon_path, ICON.as_bytes(), ENTRY_MODE)?;
    write_atomic(&entry_path, entry.as_bytes(), ENTRY_MODE)?;
    refresh_caches(
        entry_path.parent().unwrap_or(data_home),
        &data_home.join("icons/hicolor"),
    );
    Ok(entry_path)
}

/// Removes Kestrel's own AppImage entry and icon under `data_home`.
///
/// An entry another installation wrote is left alone; `Ok(false)` says so.
pub fn remove_managed(data_home: &Path, app_id: &str) -> Result<bool, DesktopEntryError> {
    if !data_home.is_absolute() {
        return Err(DesktopEntryError::NotAbsolute {
            path: data_home.to_path_buf(),
        });
    }
    let entry_path = data_home
        .join("applications")
        .join(format!("{app_id}.desktop"));
    let contents = match fs::read_to_string(&entry_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(DesktopEntryError::Remove {
                path: entry_path,
                source,
            });
        }
    };
    if !is_managed(&contents) {
        return Ok(false);
    }
    remove_if_present(&entry_path)?;
    remove_if_present(&icon_path(data_home, app_id))?;
    refresh_caches(
        entry_path.parent().unwrap_or(data_home),
        &data_home.join("icons/hicolor"),
    );
    Ok(true)
}

/// The icon path for `app_id` under `data_home`.
pub fn icon_path(data_home: &Path, app_id: &str) -> PathBuf {
    data_home
        .join("icons/hicolor/scalable/apps")
        .join(format!("{app_id}.svg"))
}

/// Whether desktop entry contents are Kestrel's AppImage entry.
pub fn is_managed(contents: &str) -> bool {
    contents.lines().any(|line| line.trim() == MANAGED_MARKER)
}

/// The executable a managed entry starts, from its main `Exec` line.
pub fn managed_executable(contents: &str) -> Option<PathBuf> {
    let value = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("Exec="))?;
    parse_exec_argument(value).map(PathBuf::from)
}

fn path_text(path: &Path) -> Result<&str, DesktopEntryError> {
    if !path.is_absolute() {
        return Err(DesktopEntryError::NotAbsolute {
            path: path.to_path_buf(),
        });
    }
    let Some(text) = path.to_str() else {
        return Err(DesktopEntryError::NotUtf8 {
            path: path.to_path_buf(),
        });
    };
    if text.chars().any(char::is_control) {
        return Err(DesktopEntryError::ContainsControlCharacter {
            path: path.to_path_buf(),
        });
    }
    Ok(text)
}

/// Quotes one `Exec` argument, escaping what the desktop-entry specification
/// reserves inside double quotes.
fn quote_exec_argument(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

fn parse_exec_argument(value: &str) -> Option<String> {
    let value = value.trim_start();
    if let Some(rest) = value.strip_prefix('"') {
        let mut parsed = String::new();
        let mut escaped = false;
        for character in rest.chars() {
            if escaped {
                parsed.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Some(parsed);
            } else {
                parsed.push(character);
            }
        }
        return None;
    }
    value
        .split_whitespace()
        .next()
        .map(|argument| argument.to_owned())
}

fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<(), DesktopEntryError> {
    let parent = path
        .parent()
        .ok_or_else(|| DesktopEntryError::MissingParent {
            path: path.to_path_buf(),
        })?;
    fs::create_dir_all(parent).map_err(|source| DesktopEntryError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or("entry");
    let temporary = parent.join(format!(".{name}.{}.{sequence}.tmp", std::process::id()));
    let result = (|| {
        fs::write(&temporary, contents).map_err(|source| DesktopEntryError::Write {
            path: temporary.clone(),
            source,
        })?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|source| {
            DesktopEntryError::Write {
                path: temporary.clone(),
                source,
            }
        })?;
        fs::rename(&temporary, path).map_err(|source| DesktopEntryError::Install {
            from: temporary.clone(),
            to: path.to_path_buf(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_if_present(path: &Path) -> Result<(), DesktopEntryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(DesktopEntryError::Remove {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Rebuilds the launcher and icon caches when those tools exist; an absent
/// tool only means the desktop refreshes the entry on its own schedule.
fn refresh_caches(applications: &Path, icon_theme: &Path) {
    for (tool, arguments) in [
        (
            "update-desktop-database",
            vec![OsStr::new("-q"), applications.as_os_str()],
        ),
        (
            "gtk-update-icon-cache",
            vec![OsStr::new("-q"), OsStr::new("-t"), icon_theme.as_os_str()],
        ),
    ] {
        let _ = Command::new(tool)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
    };

    use tempfile::TempDir;

    use super::{
        DesktopEntryError, entry_directories, find_entry, install, is_managed, managed_executable,
        remove_managed, render_entry, status,
    };

    #[test]
    fn search_path_starts_at_the_user_data_home_then_follows_data_dirs() {
        for (data_home, data_dirs, home, expected) in [
            (
                Some(OsStr::new("/home/user/.local/share")),
                None,
                Some(OsStr::new("/home/user")),
                vec![
                    PathBuf::from("/home/user/.local/share/applications"),
                    PathBuf::from("/usr/local/share/applications"),
                    PathBuf::from("/usr/share/applications"),
                ],
            ),
            (
                None,
                Some(OsStr::new("/opt/share:/srv/share")),
                Some(OsStr::new("/home/user")),
                vec![
                    PathBuf::from("/home/user/.local/share/applications"),
                    PathBuf::from("/opt/share/applications"),
                    PathBuf::from("/srv/share/applications"),
                ],
            ),
            (
                Some(OsStr::new("relative")),
                Some(OsStr::new("relative:/opt/share")),
                None,
                vec![PathBuf::from("/opt/share/applications")],
            ),
        ] {
            assert_eq!(entry_directories(data_home, data_dirs, home), expected);
        }
    }

    #[test]
    fn an_entry_is_found_in_the_first_directory_that_holds_it() {
        let first = TempDir::new().expect("first directory");
        let second = TempDir::new().expect("second directory");
        let app_id = "io.example.App";
        assert!(find_entry(&[first.path().into(), second.path().into()], app_id).is_none());
        fs::write(
            second.path().join(format!("{app_id}.desktop")),
            "[Desktop Entry]\n",
        )
        .expect("second entry");
        assert_eq!(
            find_entry(&[first.path().into(), second.path().into()], app_id),
            Some(second.path().join(format!("{app_id}.desktop")))
        );
        fs::write(
            first.path().join(format!("{app_id}.desktop")),
            "[Desktop Entry]\n",
        )
        .expect("first entry");
        assert_eq!(
            find_entry(&[first.path().into(), second.path().into()], app_id),
            Some(first.path().join(format!("{app_id}.desktop")))
        );
    }

    #[test]
    fn a_rendered_entry_quotes_the_appimage_and_names_it_for_launchers() {
        let app_id = "io.example.App";
        let executable = Path::new("/home/user/Apps/Kestrel build \"stable\"/Kestrel.AppImage");
        let entry = render_entry(app_id, "Kestrel", executable).expect("entry renders");
        assert!(entry.contains("Type=Application"));
        assert!(entry.contains("Icon=io.example.App"));
        assert!(entry.contains("StartupWMClass=io.example.App"));
        assert!(entry.contains("X-Kestrel-AppImage=true"));
        assert!(
            entry
                .contains("Exec=\"/home/user/Apps/Kestrel build \\\"stable\\\"/Kestrel.AppImage\"")
        );
        assert!(entry.contains(
            "Exec=\"/home/user/Apps/Kestrel build \\\"stable\\\"/Kestrel.AppImage\" --command app.quit"
        ));
        assert!(
            entry.contains("TryExec=/home/user/Apps/Kestrel build \"stable\"/Kestrel.AppImage")
        );
        assert_eq!(managed_executable(&entry), Some(executable.to_path_buf()));
        assert!(is_managed(&entry));
        assert!(!is_managed("[Desktop Entry]\nName=Kestrel\n"));
    }

    #[test]
    fn the_development_name_is_written_when_it_differs() {
        let entry = render_entry(
            "io.example.App.Devel",
            "Kestrel (Development)",
            Path::new("/tmp/Kestrel.AppImage"),
        )
        .expect("entry renders");
        assert!(entry.contains("Name=Kestrel (Development)"));
        assert!(!entry.contains("Name=Kestrel\n"));
    }

    #[test]
    fn non_absolute_and_control_character_paths_are_rejected() {
        assert!(matches!(
            render_entry("io.example.App", "Kestrel", Path::new("relative")),
            Err(DesktopEntryError::NotAbsolute { .. })
        ));
        assert!(matches!(
            render_entry("io.example.App", "Kestrel", Path::new("/tmp/bad\npath")),
            Err(DesktopEntryError::ContainsControlCharacter { .. })
        ));
    }

    #[test]
    fn install_writes_the_entry_and_icon_and_remove_takes_only_its_own() {
        let data = TempDir::new().expect("data home");
        let app_id = "io.example.App";
        let executable = data.path().join("Kestrel.AppImage");
        let entry_path =
            install(data.path(), app_id, "Kestrel", &executable).expect("entry installs");
        assert_eq!(
            entry_path,
            data.path().join(format!("applications/{app_id}.desktop"))
        );
        assert_eq!(
            super::icon_path(data.path(), app_id),
            data.path()
                .join(format!("icons/hicolor/scalable/apps/{app_id}.svg"))
        );
        assert!(entry_path.is_file());
        assert!(super::icon_path(data.path(), app_id).is_file());
        let directories = vec![data.path().join("applications")];
        let reported = status(&directories, app_id);
        assert!(reported.managed);
        assert_eq!(reported.executable, Some(executable.clone()));
        assert!(remove_managed(data.path(), app_id).expect("entry removes"));
        assert!(!entry_path.exists());
        assert!(!super::icon_path(data.path(), app_id).exists());
        assert!(!status(&directories, app_id).exists());
    }

    #[test]
    fn removal_leaves_an_entry_kestrel_did_not_write() {
        let data = TempDir::new().expect("data home");
        let applications = data.path().join("applications");
        fs::create_dir_all(&applications).expect("applications folder");
        let entry = applications.join("io.example.App.desktop");
        fs::write(&entry, "[Desktop Entry]\nName=Other\n").expect("other entry");
        assert!(!remove_managed(data.path(), "io.example.App").expect("removal checks"));
        assert!(entry.exists());
    }
}
