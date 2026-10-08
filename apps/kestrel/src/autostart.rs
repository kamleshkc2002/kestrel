//! User-session XDG autostart integration.
//!
//! Portable config stores intent; this module writes the machine-specific entry.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Desktop-file name in the user's autostart directory.
pub const DESKTOP_FILE_NAME: &str = if kestrel_core::DEVELOPMENT_BUILD {
    "io.github.kamleshkc2002.Kestrel.Devel.desktop"
} else {
    "io.github.kamleshkc2002.Kestrel.desktop"
};

const APPLICATION_NAME: &str = kestrel_core::APPLICATION_NAME;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure resolving or changing the autostart entry.
#[derive(Debug)]
pub enum AutostartError {
    /// No non-empty XDG_CONFIG_HOME or HOME was available.
    MissingConfigHome,
    /// XDG configuration directories must be absolute.
    ConfigHomeNotAbsolute { path: PathBuf },
    /// The autostart path has no parent directory.
    InvalidAutostartPath { path: PathBuf },
    /// The executable path must be absolute.
    ExecutableNotAbsolute { path: PathBuf },
    /// The executable path must use valid UTF-8.
    ExecutableNotUtf8 { path: PathBuf },
    /// A control character would invalidate the desktop Exec value.
    ExecutableContainsControlCharacter { path: PathBuf },
    /// Resolving the running executable failed.
    CurrentExecutable { source: io::Error },
    /// Creating the autostart directory failed.
    CreateDirectory { path: PathBuf, source: io::Error },
    /// Creating the temporary desktop file failed.
    CreateTemporaryFile { path: PathBuf, source: io::Error },
    /// Writing or syncing the temporary file failed.
    WriteTemporaryFile { path: PathBuf, source: io::Error },
    /// Atomic installation failed.
    Install {
        from: PathBuf,
        to: PathBuf,
        source: io::Error,
    },
    /// Removing the desktop file failed.
    Remove { path: PathBuf, source: io::Error },
    /// Temporary names were exhausted.
    TemporaryFileNameExhausted { directory: PathBuf },
}

impl std::fmt::Display for AutostartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingConfigHome => write!(
                formatter,
                "cannot resolve the user configuration directory: set XDG_CONFIG_HOME or HOME"
            ),
            Self::ConfigHomeNotAbsolute { path } => write!(
                formatter,
                "user configuration directory must be absolute: {}",
                path.display()
            ),
            Self::InvalidAutostartPath { path } => {
                write!(
                    formatter,
                    "autostart path has no parent directory: {}",
                    path.display()
                )
            }
            Self::ExecutableNotAbsolute { path } => write!(
                formatter,
                "Kestrel executable path must be absolute: {}",
                path.display()
            ),
            Self::ExecutableNotUtf8 { path } => write!(
                formatter,
                "Kestrel executable path is not valid UTF-8: {}",
                path.display()
            ),
            Self::ExecutableContainsControlCharacter { path } => write!(
                formatter,
                "Kestrel executable path contains a control character: {}",
                path.display()
            ),
            Self::CurrentExecutable { source } => {
                write!(
                    formatter,
                    "cannot resolve the current Kestrel executable: {source}"
                )
            }
            Self::CreateDirectory { path, source } => write!(
                formatter,
                "cannot create user autostart directory {}: {source}",
                path.display()
            ),
            Self::CreateTemporaryFile { path, source } => write!(
                formatter,
                "cannot create temporary autostart file {}: {source}",
                path.display()
            ),
            Self::WriteTemporaryFile { path, source } => write!(
                formatter,
                "cannot write temporary autostart file {}: {source}",
                path.display()
            ),
            Self::Install { from, to, source } => write!(
                formatter,
                "cannot atomically install autostart file {} as {}: {source}",
                from.display(),
                to.display()
            ),
            Self::Remove { path, source } => write!(
                formatter,
                "cannot remove Kestrel autostart file {}: {source}",
                path.display()
            ),
            Self::TemporaryFileNameExhausted { directory } => write!(
                formatter,
                "cannot allocate a temporary autostart filename in {}",
                directory.display()
            ),
        }
    }
}

impl std::error::Error for AutostartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExecutable { source }
            | Self::CreateDirectory { source, .. }
            | Self::CreateTemporaryFile { source, .. }
            | Self::WriteTemporaryFile { source, .. }
            | Self::Install { source, .. }
            | Self::Remove { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Enable Kestrel's user autostart entry using the current environment.
pub fn enable() -> Result<(), AutostartError> {
    let config_home = config_home()?;
    let executable = executable_path()?;
    enable_at(&config_home, &executable)
}

/// Disabling a missing entry succeeds.
pub fn disable() -> Result<(), AutostartError> {
    let config_home = config_home()?;
    disable_at(&config_home)
}

/// Resolves the path, preferring non-empty `XDG_CONFIG_HOME`.
pub fn autostart_path() -> Result<PathBuf, AutostartError> {
    autostart_path_in(&config_home()?)
}

fn config_home() -> Result<PathBuf, AutostartError> {
    let config_home = non_empty_var("XDG_CONFIG_HOME")
        .or_else(|| non_empty_var("HOME").map(|home| home.join(".config")))
        .ok_or(AutostartError::MissingConfigHome)?;
    if config_home.as_os_str().is_empty() || !config_home.is_absolute() {
        return Err(AutostartError::ConfigHomeNotAbsolute { path: config_home });
    }
    Ok(config_home)
}

/// Resolves a path below an explicit absolute config directory.
pub fn autostart_path_in(config_home: &Path) -> Result<PathBuf, AutostartError> {
    if config_home.as_os_str().is_empty() || !config_home.is_absolute() {
        return Err(AutostartError::ConfigHomeNotAbsolute {
            path: config_home.to_path_buf(),
        });
    }
    Ok(config_home.join("autostart").join(DESKTOP_FILE_NAME))
}

/// Resolves the executable, preferring an absolute `APPIMAGE`.
pub fn executable_path() -> Result<PathBuf, AutostartError> {
    if let Some(appimage) = non_empty_var("APPIMAGE") {
        if appimage.is_absolute() {
            validate_executable_path(&appimage)?;
            return Ok(appimage);
        }
    }

    let executable =
        env::current_exe().map_err(|source| AutostartError::CurrentExecutable { source })?;
    validate_executable_path(&executable)?;
    Ok(executable)
}

/// Atomically installs the desktop entry via sibling rename.
pub fn enable_at(config_home: &Path, executable: &Path) -> Result<(), AutostartError> {
    enable_for_config_home(config_home, executable)
}

fn enable_for_config_home(config_home: &Path, executable: &Path) -> Result<(), AutostartError> {
    let path = autostart_path_in(config_home)?;
    validate_executable_path(executable)?;
    let entry = desktop_entry(executable)?;
    install_atomically(&path, entry.as_bytes())?;
    Ok(())
}

/// Removes only Kestrel's entry; a missing file is already disabled.
pub fn disable_at(config_home: &Path) -> Result<(), AutostartError> {
    let path = autostart_path_in(config_home)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AutostartError::Remove { path, source }),
    }
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    let value: OsString = env::var_os(name)?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn validate_executable_path(path: &Path) -> Result<(), AutostartError> {
    if !path.is_absolute() {
        return Err(AutostartError::ExecutableNotAbsolute {
            path: path.to_path_buf(),
        });
    }
    let Some(value) = path.to_str() else {
        return Err(AutostartError::ExecutableNotUtf8 {
            path: path.to_path_buf(),
        });
    };
    if value.chars().any(char::is_control) {
        return Err(AutostartError::ExecutableContainsControlCharacter {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn desktop_entry(executable: &Path) -> Result<String, AutostartError> {
    let encoded = encode_exec_argument(executable)?;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={APPLICATION_NAME}\nExec={encoded}\n"
    ))
}

/// Quote one desktop Exec argument according to the Desktop Entry specification.
fn encode_exec_argument(executable: &Path) -> Result<String, AutostartError> {
    let value = executable
        .to_str()
        .ok_or_else(|| AutostartError::ExecutableNotUtf8 {
            path: executable.to_path_buf(),
        })?;
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '%' => encoded.push_str("%%"),
            '"' | '\\' | '`' | '$' => {
                encoded.push('\\');
                encoded.push(character);
            }
            _ => encoded.push(character),
        }
    }
    encoded.push('"');
    Ok(encoded)
}

fn install_atomically(path: &Path, contents: &[u8]) -> Result<(), AutostartError> {
    let parent = path
        .parent()
        .ok_or_else(|| AutostartError::InvalidAutostartPath {
            path: path.to_path_buf(),
        })?;
    fs::create_dir_all(parent).map_err(|source| AutostartError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })?;

    let (temporary_path, mut temporary_file) = create_temporary_file(parent, path)?;
    if let Err(source) = temporary_file.write_all(contents) {
        let _ = fs::remove_file(&temporary_path);
        return Err(AutostartError::WriteTemporaryFile {
            path: temporary_path,
            source,
        });
    }
    if let Err(source) = temporary_file.sync_all() {
        let _ = fs::remove_file(&temporary_path);
        return Err(AutostartError::WriteTemporaryFile {
            path: temporary_path,
            source,
        });
    }
    drop(temporary_file);

    if let Err(source) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(AutostartError::Install {
            from: temporary_path,
            to: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn create_temporary_file(parent: &Path, target: &Path) -> Result<(PathBuf, File), AutostartError> {
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(DESKTOP_FILE_NAME);
    for _ in 0..128 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(".{target_name}.tmp-{}-{sequence}", std::process::id());
        let path = parent.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(AutostartError::CreateTemporaryFile { path, source });
            }
        }
    }
    Err(AutostartError::TemporaryFileNameExhausted {
        directory: parent.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AutostartError, DESKTOP_FILE_NAME, autostart_path_in, disable_at, enable_at,
        enable_for_config_home,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kestrel-autostart-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create isolated temporary directory");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn enable_writes_valid_entry_and_disable_is_idempotent() {
        let directory = TestDirectory::new();
        let executable = directory.path().join("bin/Kestrel");
        enable_at(directory.path(), &executable).expect("enable autostart");

        let path = autostart_path_in(directory.path()).expect("autostart path");
        let content = fs::read_to_string(&path).expect("read desktop entry");
        assert_eq!(
            content,
            format!(
                "[Desktop Entry]\nType=Application\nName={}\nExec=\"",
                kestrel_core::APPLICATION_NAME
            ) + executable.to_str().expect("UTF-8 test path")
                + "\"\n"
        );

        disable_at(directory.path()).expect("disable autostart");
        disable_at(directory.path()).expect("already-disabled autostart");
        assert!(!path.exists());
    }

    #[test]
    fn exec_argument_escapes_desktop_syntax() {
        let directory = TestDirectory::new();
        let executable = directory.path().join("bin/Kestrel $night\\build%1\"");
        enable_at(directory.path(), &executable).expect("enable autostart");

        let path = autostart_path_in(directory.path()).expect("autostart path");
        let content = fs::read_to_string(path).expect("read desktop entry");
        assert!(content.contains("Exec=\""));
        assert!(content.contains("\\$night\\\\build%%1\\\""));
    }

    #[test]
    fn explicit_paths_never_touch_the_real_user_autostart_directory() {
        let directory = TestDirectory::new();
        let outside = directory.path().join("unrelated.desktop");
        fs::write(&outside, "keep").expect("write unrelated file");

        disable_at(directory.path()).expect("disable missing Kestrel entry");
        assert_eq!(
            fs::read_to_string(outside).expect("read unrelated file"),
            "keep"
        );
        assert_eq!(
            autostart_path_in(directory.path())
                .expect("autostart path")
                .file_name()
                .and_then(|name| name.to_str()),
            Some(DESKTOP_FILE_NAME)
        );
    }

    #[test]
    fn relative_executable_is_an_actionable_typed_error() {
        let directory = TestDirectory::new();
        let error = enable_at(directory.path(), PathBuf::from("Kestrel").as_path())
            .expect_err("relative executable must fail");
        assert!(matches!(
            error,
            AutostartError::ExecutableNotAbsolute { .. }
        ));
    }

    #[test]
    fn resolved_enable_wrapper_targets_one_autostart_directory() {
        let directory = TestDirectory::new();
        let executable = directory.path().join("Kestrel");

        enable_for_config_home(directory.path(), &executable).expect("enable resolved autostart");

        let expected = autostart_path_in(directory.path()).expect("autostart path");
        assert!(expected.is_file());
        assert!(
            !directory
                .path()
                .join("autostart")
                .join("autostart")
                .join(DESKTOP_FILE_NAME)
                .exists()
        );
    }
}
