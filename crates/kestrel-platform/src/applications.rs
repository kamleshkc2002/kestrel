//! Desktop-entry discovery and external launching, without a shell.
//!
//! Only the standard XDG application directories are read, and only the fields
//! the bar needs; nothing is indexed globally and no command line is ever passed
//! through a shell.

use std::{
    env,
    error::Error,
    ffi::OsStr,
    fmt, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const FEATURE_ID: &str = "commands.bar";
/// The largest number of desktop entries the bar will consider.
pub const MAX_APPLICATIONS: usize = 500;
/// The largest number of desktop files visited while scanning.
pub const MAX_APPLICATION_FILES: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationEntry {
    /// The desktop file identifier, used for ranking and de-duplication.
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    /// The parsed command line: program plus arguments, never a shell string.
    pub command: Vec<String>,
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopErrorKind {
    NotAvailable,
    ReadFailed,
    LaunchFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopError {
    pub kind: DesktopErrorKind,
    pub message: String,
}

impl DesktopError {
    fn new(kind: DesktopErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DesktopError {}

/// The XDG application directories, in search order.
pub fn application_directories(
    data_home: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let data_home = data_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(data_home) = data_home {
        roots.push(data_home.join("applications"));
    }
    if let Some(data_dirs) = data_dirs {
        for dir in env::split_paths(data_dirs) {
            if dir.is_absolute() {
                roots.push(dir.join("applications"));
            }
        }
    }
    roots.push(PathBuf::from("/usr/share/applications"));
    roots
}

/// Scans the XDG application directories for launchable entries.
///
/// Entries marked hidden, non-displayable, or without a command line are
/// skipped, and the earlier directory wins for a duplicated identifier. The scan
/// stops at `MAX_APPLICATION_FILES` visited files and returns at most
/// `MAX_APPLICATIONS` entries in a stable order.
pub fn scan_applications(roots: &[PathBuf]) -> Vec<ApplicationEntry> {
    let mut entries: Vec<ApplicationEntry> = Vec::new();
    let mut visited = 0usize;

    for root in roots {
        let Ok(directory) = fs::read_dir(root) else {
            continue;
        };
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in directory.flatten() {
            if visited >= MAX_APPLICATION_FILES {
                break;
            }
            let path = entry.path();
            if path.extension() != Some(OsStr::new("desktop")) {
                continue;
            }
            visited += 1;
            files.push(path);
        }
        files.sort();
        for path in files {
            let Some(id) = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
            else {
                continue;
            };
            if entries.iter().any(|entry| entry.id == id) {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(entry) = parse_desktop_entry(&id, &contents) {
                entries.push(entry);
                if entries.len() >= MAX_APPLICATIONS {
                    return entries;
                }
            }
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    entries
}

/// Parses the `[Desktop Entry]` group of one desktop file.
pub fn parse_desktop_entry(id: &str, contents: &str) -> Option<ApplicationEntry> {
    let mut in_group = false;
    let mut name: Option<String> = None;
    let mut comment: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut entry_type: Option<String> = None;
    let mut hidden = false;
    let mut no_display = false;
    let mut terminal = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => name = Some(value.to_string()),
            "Comment" => comment = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "Type" => entry_type = Some(value.to_string()),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if hidden || no_display || entry_type.as_deref() != Some("Application") {
        return None;
    }
    let name = name?;
    let command = parse_exec(&exec?)?;
    Some(ApplicationEntry {
        id: id.to_string(),
        name,
        comment,
        command,
        terminal,
    })
}

/// Splits an `Exec` value into an argv, dropping desktop field codes.
///
/// Quoting rules from the specification are honoured for the common cases, and
/// every field code (`%U`, `%f`, …) is removed because the bar launches without
/// arguments of its own.
pub fn parse_exec(exec: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut has_current = false;

    let push_current = |argv: &mut Vec<String>, current: &mut String, has_current: &mut bool| {
        if *has_current {
            argv.push(std::mem::take(current));
            *has_current = false;
        }
    };

    let characters: Vec<char> = exec.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        index += 1;
        if escaped {
            current.push(character);
            has_current = true;
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            '%' => {
                // Field codes are dropped, including the argument they would take.
                if let Some(code) = characters.get(index) {
                    if code.is_ascii_alphabetic() {
                        index += 1;
                        if code.is_ascii_uppercase() {
                            push_current(&mut argv, &mut current, &mut has_current);
                        }
                    } else if *code == '%' {
                        current.push('%');
                        has_current = true;
                        index += 1;
                    }
                }
            }
            character if character.is_whitespace() && !quoted => {
                push_current(&mut argv, &mut current, &mut has_current);
            }
            _ => {
                current.push(character);
                has_current = true;
            }
        }
    }
    push_current(&mut argv, &mut current, &mut has_current);

    if argv.is_empty() {
        return None;
    }
    Some(argv)
}

/// Resolves an executable on `PATH` without a shell.
pub fn resolve_executable(name: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    if name.starts_with('/') {
        return is_executable(Path::new(name)).then(|| PathBuf::from(name));
    }
    let path_env = path_env?;
    for root in env::split_paths(path_env) {
        if !root.is_absolute() {
            continue;
        }
        let candidate = root.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Launches a program or opens a URL through the desktop handler.
///
/// The child is detached: nothing is waited for, no output is inherited, and no
/// shell interprets the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalLauncher {
    handler: PathBuf,
    handler_name: String,
}

impl ExternalLauncher {
    /// Uses `xdg-open` when it is present on `PATH`.
    pub fn discover() -> Result<Self, DesktopError> {
        let path_env = env::var_os("PATH");
        Self::discover_with(path_env.as_deref())
    }

    pub fn discover_with(path_env: Option<&OsStr>) -> Result<Self, DesktopError> {
        match resolve_executable("xdg-open", path_env) {
            Some(handler) => Ok(Self {
                handler,
                handler_name: "xdg-open".to_string(),
            }),
            None => Err(DesktopError::new(
                DesktopErrorKind::NotAvailable,
                "no desktop open handler was found on PATH (expected xdg-open)",
            )),
        }
    }

    pub fn handler_name(&self) -> &str {
        &self.handler_name
    }

    /// Starts a program with its own arguments, detached.
    pub fn launch(&self, command: &[String]) -> Result<(), DesktopError> {
        let Some((program, args)) = command.split_first() else {
            return Err(DesktopError::new(
                DesktopErrorKind::LaunchFailed,
                "an empty command cannot be launched",
            ));
        };
        let path_env = env::var_os("PATH");
        let program = resolve_executable(program, path_env.as_deref()).ok_or_else(|| {
            DesktopError::new(
                DesktopErrorKind::NotAvailable,
                format!("{program} was not found on PATH"),
            )
        })?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_with_busy_retry(&mut command)
            .map(|_| ())
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorKind::LaunchFailed,
                    format!("the program could not be started: {error}"),
                )
            })
    }

    /// Opens one target (URL, file, or directory) with the desktop handler.
    pub fn open(&self, target: &str) -> Result<(), DesktopError> {
        if target.is_empty() {
            return Err(DesktopError::new(
                DesktopErrorKind::LaunchFailed,
                "an empty target cannot be opened",
            ));
        }
        let mut command = Command::new(&self.handler);
        command
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_with_busy_retry(&mut command)
            .map(|_| ())
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorKind::LaunchFailed,
                    format!("{target} could not be opened: {error}"),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{
        ExternalLauncher, MAX_APPLICATIONS, application_directories, parse_desktop_entry,
        parse_exec, resolve_executable, scan_applications,
    };

    fn write(dir: &TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, contents).expect("desktop file is written");
        path
    }

    #[test]
    fn exec_field_codes_and_quoting_are_parsed_without_a_shell() {
        assert_eq!(
            parse_exec("/usr/bin/firefox %u"),
            Some(vec!["/usr/bin/firefox".to_string()])
        );
        assert_eq!(
            parse_exec("env FOO=1 gedit --new-window %F"),
            Some(vec![
                "env".to_string(),
                "FOO=1".to_string(),
                "gedit".to_string(),
                "--new-window".to_string()
            ])
        );
        assert_eq!(
            parse_exec("\"/opt/My App/run\" --flag"),
            Some(vec!["/opt/My App/run".to_string(), "--flag".to_string()])
        );
        assert_eq!(
            parse_exec("echo 100%% done"),
            Some(vec![
                "echo".to_string(),
                "100%".to_string(),
                "done".to_string()
            ])
        );
        assert_eq!(parse_exec(""), None);
        assert_eq!(parse_exec("   "), None);
    }

    #[test]
    fn desktop_entries_are_parsed_and_non_launchable_ones_are_skipped() {
        let entry = parse_desktop_entry(
            "firefox.desktop",
            "[Desktop Entry]\nType=Application\nName=Firefox\nComment=Browse the web\n\
             Exec=/usr/bin/firefox %u\nTerminal=false\n",
        )
        .expect("a launchable entry parses");
        assert_eq!(entry.name, "Firefox");
        assert_eq!(entry.comment.as_deref(), Some("Browse the web"));
        assert_eq!(entry.command, vec!["/usr/bin/firefox".to_string()]);
        assert!(!entry.terminal);

        for contents in [
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=/bin/true\nHidden=true\n",
            "[Desktop Entry]\nType=Application\nName=No display\nExec=/bin/true\nNoDisplay=true\n",
            "[Desktop Entry]\nType=Link\nName=A link\n",
            "[Desktop Entry]\nType=Application\nName=No exec\n",
            "[Other Group]\nName=Wrong group\nExec=/bin/true\n",
        ] {
            assert_eq!(
                parse_desktop_entry("x.desktop", contents),
                None,
                "must be skipped: {contents}"
            );
        }
    }

    #[test]
    fn scanning_prefers_the_first_directory_and_stays_bounded() {
        let first = TempDir::new().expect("temp dir");
        let second = TempDir::new().expect("temp dir");
        write(
            &first,
            "alpha.desktop",
            "[Desktop Entry]\nType=Application\nName=Alpha\nExec=/bin/true\n",
        );
        write(
            &first,
            "ignored.txt",
            "[Desktop Entry]\nType=Application\nName=Ignored\nExec=/bin/true\n",
        );
        // A duplicate identifier in a later directory must not replace the first.
        write(
            &second,
            "alpha.desktop",
            "[Desktop Entry]\nType=Application\nName=Alpha Duplicate\nExec=/bin/false\n",
        );
        write(
            &second,
            "beta.desktop",
            "[Desktop Entry]\nType=Application\nName=Beta\nExec=/bin/true\nTerminal=true\n",
        );

        let entries = scan_applications(&[first.path().to_path_buf(), second.path().to_path_buf()]);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Alpha", "the earlier directory wins");
        assert_eq!(entries[1].name, "Beta");
        assert!(entries[1].terminal);
        assert!(entries.len() <= MAX_APPLICATIONS);
    }

    #[test]
    fn application_directories_follow_xdg_order() {
        let roots = application_directories(
            Some(std::ffi::OsStr::new("/home/user/.local/share")),
            Some(std::ffi::OsStr::new("/usr/local/share:/usr/share")),
            Some(std::ffi::OsStr::new("/home/user")),
        );
        assert_eq!(
            roots[0],
            std::path::PathBuf::from("/home/user/.local/share/applications")
        );
        assert_eq!(
            roots[1],
            std::path::PathBuf::from("/usr/local/share/applications")
        );
        assert!(roots.contains(&std::path::PathBuf::from("/usr/share/applications")));

        // A missing data home falls back to HOME, and a relative entry is skipped.
        let roots = application_directories(
            None,
            Some(std::ffi::OsStr::new("relative:/opt/share")),
            Some(std::ffi::OsStr::new("/home/user")),
        );
        assert_eq!(
            roots[0],
            std::path::PathBuf::from("/home/user/.local/share/applications")
        );
        assert!(!roots.iter().any(|root| root.starts_with("relative")));
        assert!(roots.contains(&std::path::PathBuf::from("/opt/share/applications")));
    }

    #[test]
    fn executable_resolution_requires_an_executable_file() {
        let dir = TempDir::new().expect("temp dir");
        let script = write(&dir, "tool", "#!/bin/sh\nexit 0\n");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("mode is set");

        let path_env = std::ffi::OsString::from(dir.path().as_os_str());
        assert_eq!(
            resolve_executable("tool", Some(&path_env)),
            Some(script.clone())
        );
        assert_eq!(resolve_executable("missing", Some(&path_env)), None);
        assert_eq!(
            resolve_executable("/absolute/path/that/does/not/exist", Some(&path_env)),
            None
        );

        let not_executable = write(&dir, "plain", "data");
        assert_eq!(
            resolve_executable(not_executable.to_str().unwrap(), Some(&path_env)),
            None
        );
    }

    #[test]
    fn launcher_reports_a_missing_handler_and_missing_programs() {
        let error = ExternalLauncher::discover_with(Some(std::ffi::OsStr::new("/nonexistent")))
            .expect_err("no handler means no launcher");
        assert_eq!(error.kind, super::DesktopErrorKind::NotAvailable);

        let dir = TempDir::new().expect("temp dir");
        let handler = write(&dir, "xdg-open", "#!/bin/sh\nexit 0\n");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&handler).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&handler, permissions).expect("mode is set");
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());
        let launcher =
            ExternalLauncher::discover_with(Some(&path_env)).expect("the stub handler is found");
        assert_eq!(launcher.handler_name(), "xdg-open");

        let error = launcher
            .launch(&["definitely-not-installed-anywhere".to_string()])
            .expect_err("a missing program is reported");
        assert_eq!(error.kind, super::DesktopErrorKind::NotAvailable);
        assert!(launcher.launch(&[]).is_err());
    }

    #[test]
    fn open_succeeds_with_a_stub_handler() {
        let dir = TempDir::new().expect("temp dir");
        let record = dir.path().join("opened.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" > {}\nexit 0\n",
            record.display()
        );
        let handler = write(&dir, "xdg-open", &script);
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&handler).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&handler, permissions).expect("mode is set");

        let launcher = ExternalLauncher {
            handler,
            handler_name: "xdg-open".to_string(),
        };
        launcher
            .open("https://example.com")
            .expect("the stub opens");

        for _ in 0..100 {
            if record.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let recorded = fs::read_to_string(&record).expect("the stub recorded its arguments");
        assert_eq!(recorded, "1\nhttps://example.com\n");

        assert!(launcher.open("").is_err());
    }
}
