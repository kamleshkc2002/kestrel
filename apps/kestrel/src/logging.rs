//! Private, bounded diagnostic log files.
//!
//! Lines go to `$XDG_STATE_HOME/<data directory>/logs` in 0600 files inside a
//! 0700 folder. Each day starts a new file, and so does a file that reaches a
//! quarter of the size cap. Files past the retention age go first, then the
//! oldest until the folder fits the cap. Home-folder paths and network
//! addresses are replaced before a line is written.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{Ipv4Addr, Ipv6Addr},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use kestrel_core::LoggingConfiguration;
use kestrel_platform::snippets::{LocalTime, SystemClock};
use log::{Level, LevelFilter, Log, Metadata, Record};

const FILE_PREFIX: &str = "kestrel-";
const FILE_SUFFIX: &str = ".log";
const MIN_FILE_BYTES: u64 = 64 * 1024;
const HOME_PATH: &str = "<home path>";
const ADDRESS: &str = "<address>";

/// The process logger; `log` needs a reference that lives as long as the process.
static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// What to record and how long to keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPolicy {
    pub debug: bool,
    pub retention: Duration,
    pub max_total_bytes: u64,
}

impl LogPolicy {
    pub fn from_configuration(configuration: &LoggingConfiguration) -> Self {
        Self {
            debug: configuration.debug,
            retention: Duration::from_secs(u64::from(configuration.retention_days) * 86_400),
            max_total_bytes: u64::from(configuration.max_total_megabytes) * 1024 * 1024,
        }
    }

    fn file_bytes(&self) -> u64 {
        (self.max_total_bytes / 4).max(MIN_FILE_BYTES)
    }
}

/// `$XDG_STATE_HOME/<data directory>/logs`, falling back to `~/.local/state`.
pub fn log_directory() -> Option<PathBuf> {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(
        state_home
            .join(kestrel_core::DATA_DIRECTORY_NAME)
            .join("logs"),
    )
}

/// Writes log lines; opens a file only when the first line arrives.
pub struct FileLogger {
    directory: PathBuf,
    writer: Mutex<Writer>,
    policy: Mutex<LogPolicy>,
    debug: AtomicBool,
    /// `KESTREL_LOG=debug` keeps debug detail on for this run.
    forced_debug: bool,
    /// Warnings and errors also go to stderr when it is a terminal.
    mirror: bool,
    home: Option<String>,
}

struct Writer {
    file: Option<File>,
    day: String,
    index: u32,
    written: u64,
}

impl FileLogger {
    pub fn new(directory: PathBuf, policy: LogPolicy, forced_debug: bool, mirror: bool) -> Self {
        let home = std::env::var("HOME")
            .ok()
            .filter(|home| home.len() > 1)
            .map(|home| home.trim_end_matches('/').to_owned());
        Self {
            directory,
            writer: Mutex::new(Writer {
                file: None,
                day: String::new(),
                index: 0,
                written: 0,
            }),
            policy: Mutex::new(policy),
            debug: AtomicBool::new(policy.debug || forced_debug),
            forced_debug,
            mirror,
            home,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// True while `KESTREL_LOG=debug` overrides the setting.
    pub fn debug_forced(&self) -> bool {
        self.forced_debug
    }

    /// Applies a changed setting at once and trims to the new limits.
    pub fn apply(&self, policy: LogPolicy) {
        self.debug
            .store(policy.debug || self.forced_debug, Ordering::Relaxed);
        *lock(&self.policy) = policy;
        let _ = self.prune(SystemTime::now());
    }

    /// Deletes expired files, then the oldest until the folder fits the cap.
    pub fn prune(&self, now: SystemTime) -> io::Result<usize> {
        let policy = *lock(&self.policy);
        let mut writer = lock(&self.writer);
        // A deleted file must not keep taking lines; the next line reopens.
        writer.file = None;
        prune_directory(&self.directory, policy, now)
    }

    /// Deletes every log file and returns how many went.
    pub fn clear(&self) -> io::Result<usize> {
        let mut writer = lock(&self.writer);
        writer.file = None;
        writer.index = 0;
        let mut removed = 0;
        for file in log_files(&self.directory)? {
            fs::remove_file(&file.path)?;
            removed += 1;
        }
        Ok(removed)
    }

    fn write_line(&self, line: &str, day: &str) -> io::Result<()> {
        let file_bytes = lock(&self.policy).file_bytes();
        let mut writer = lock(&self.writer);
        if writer.day != day {
            writer.day = day.to_owned();
            writer.index = 0;
            writer.file = None;
        }
        if writer.file.is_none() {
            self.open_current(&mut writer, file_bytes)?;
        }
        let Some(file) = writer.file.as_mut() else {
            return Ok(());
        };
        file.write_all(line.as_bytes())?;
        writer.written = writer.written.saturating_add(line.len() as u64);
        if writer.written >= file_bytes {
            writer.file = None;
            writer.index += 1;
        }
        Ok(())
    }

    /// Creates the private log folder if needed.
    pub fn ensure_directory(&self) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
    }

    fn open_current(&self, writer: &mut Writer, file_bytes: u64) -> io::Result<()> {
        self.ensure_directory()?;
        loop {
            let path = self.directory.join(file_name(&writer.day, writer.index));
            let written = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if written < file_bytes {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&path)?;
                writer.file = Some(file);
                writer.written = written;
                return Ok(());
            }
            writer.index += 1;
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let own = metadata.target().starts_with("kestrel") || !metadata.target().contains("::");
        match metadata.level() {
            Level::Error | Level::Warn => true,
            Level::Info => own,
            Level::Debug => own && self.debug.load(Ordering::Relaxed),
            Level::Trace => false,
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = sanitize(&record.args().to_string(), self.home.as_deref());
        let target = if record.target().contains("::") {
            "kestrel"
        } else {
            record.target()
        };
        let time = SystemClock.local_time();
        let day = format!("{:04}-{:02}-{:02}", time.year, time.month, time.day);
        let line = format!(
            "{day}T{} {:<5} {target} {message}\n",
            clock_time(&time),
            record.level()
        );
        if self.mirror && record.level() <= Level::Warn {
            eprintln!("kestrel: {} {message}", record.level());
        }
        let _ = self.write_line(&line, &day);
    }

    fn flush(&self) {
        if let Some(file) = lock(&self.writer).file.as_mut() {
            let _ = file.flush();
        }
    }
}

/// Installs the file logger for this process; `None` without a home folder.
pub fn install(policy: LogPolicy) -> Option<&'static FileLogger> {
    let directory = log_directory()?;
    let forced_debug = std::env::var("KESTREL_LOG").is_ok_and(|value| value == "debug");
    // SAFETY: `isatty` only inspects the descriptor.
    let mirror = unsafe { libc::isatty(libc::STDERR_FILENO) } == 1;
    LOGGER
        .set(FileLogger::new(directory, policy, forced_debug, mirror))
        .ok()?;
    let logger = LOGGER.get()?;
    log::set_logger(logger).ok()?;
    log::set_max_level(LevelFilter::Debug);
    Some(logger)
}

/// The installed logger, once `install` succeeded.
pub fn logger() -> Option<&'static FileLogger> {
    LOGGER.get()
}

/// Replaces home-folder paths and network addresses.
pub fn sanitize(message: &str, home: Option<&str>) -> String {
    redact_addresses(&redact_home_paths(message, home))
}

fn redact_home_paths(message: &str, home: Option<&str>) -> String {
    let mut output = String::with_capacity(message.len());
    let mut rest = message;
    while !rest.is_empty() {
        let at_token_start = output
            .chars()
            .last()
            .is_none_or(|previous| is_path_delimiter(previous) || previous == '=');
        let home_match = home.is_some_and(|home| {
            rest.starts_with(home)
                && rest[home.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| next == '/' || is_path_delimiter(next))
        });
        if at_token_start && (home_match || rest.starts_with("~/")) {
            let end = rest.find(is_path_delimiter).unwrap_or(rest.len());
            output.push_str(HOME_PATH);
            rest = &rest[end..];
            continue;
        }
        let mut characters = rest.chars();
        if let Some(character) = characters.next() {
            output.push(character);
        }
        rest = characters.as_str();
    }
    output
}

fn is_path_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\'' | '"' | ')' | ']' | '>' | ',' | ';')
}

fn redact_addresses(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut run = String::new();
    for character in message.chars() {
        if character.is_ascii_hexdigit() || matches!(character, ':' | '.') {
            run.push(character);
        } else {
            push_run(&mut output, &run);
            run.clear();
            output.push(character);
        }
    }
    push_run(&mut output, &run);
    output
}

/// Appends a run of address-like characters, redacted when it is one.
fn push_run(output: &mut String, run: &str) {
    let candidate = run.trim_end_matches(['.', ':']);
    let tail = &run[candidate.len()..];
    let without_port = candidate
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        .map(|(host, _)| host);
    let is_address = candidate.parse::<Ipv4Addr>().is_ok()
        || (candidate.contains(':') && candidate.parse::<Ipv6Addr>().is_ok())
        || without_port.is_some_and(|host| host.parse::<Ipv4Addr>().is_ok());
    if is_address {
        output.push_str(ADDRESS);
        output.push_str(tail);
    } else {
        output.push_str(run);
    }
}

struct LogFile {
    path: PathBuf,
    modified: SystemTime,
    bytes: u64,
}

fn log_files(directory: &Path) -> io::Result<Vec<LogFile>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(FILE_PREFIX) || !name.ends_with(FILE_SUFFIX) {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        files.push(LogFile {
            path: entry.path(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            bytes: metadata.len(),
        });
    }
    files.sort_by_key(|file| file.modified);
    Ok(files)
}

fn prune_directory(directory: &Path, policy: LogPolicy, now: SystemTime) -> io::Result<usize> {
    let mut removed = 0;
    let mut kept = Vec::new();
    for file in log_files(directory)? {
        let age = now.duration_since(file.modified).unwrap_or(Duration::ZERO);
        if age > policy.retention {
            fs::remove_file(&file.path)?;
            removed += 1;
        } else {
            kept.push(file);
        }
    }
    let mut total: u64 = kept.iter().map(|file| file.bytes).sum();
    for file in kept {
        if total <= policy.max_total_bytes {
            break;
        }
        fs::remove_file(&file.path)?;
        total = total.saturating_sub(file.bytes);
        removed += 1;
    }
    Ok(removed)
}

fn file_name(day: &str, index: u32) -> String {
    if index == 0 {
        format!("{FILE_PREFIX}{day}{FILE_SUFFIX}")
    } else {
        format!("{FILE_PREFIX}{day}-{index}{FILE_SUFFIX}")
    }
}

fn clock_time(time: &LocalTime) -> String {
    format!(
        "{:02}:{:02}:{:02}{}",
        time.hour,
        time.minute,
        time.second,
        time.utc_offset()
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, SystemTime},
    };

    use log::{Level, Log, Record};
    use tempfile::TempDir;

    use super::{FileLogger, LogPolicy, sanitize};

    fn policy(debug: bool) -> LogPolicy {
        LogPolicy {
            debug,
            retention: Duration::from_secs(7 * 86_400),
            max_total_bytes: 20 * 1024 * 1024,
        }
    }

    fn emit(logger: &FileLogger, level: Level, target: &str, message: &str) {
        logger.log(
            &Record::builder()
                .args(format_args!("{message}"))
                .level(level)
                .target(target)
                .build(),
        );
    }

    fn contents(directory: &std::path::Path) -> String {
        let mut names = fs::read_dir(directory)
            .map(|entries| {
                entries
                    .map(|entry| entry.expect("entry").path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
            .iter()
            .map(|path| fs::read_to_string(path).expect("log file"))
            .collect()
    }

    #[test]
    fn home_paths_and_addresses_never_reach_a_line() {
        let home = Some("/home/ana");
        let cases = [
            (
                "could not write /home/ana/Documents/secret.txt: denied",
                "secret",
            ),
            ("export to '/home/ana/My Notes.md' failed", "My Notes"),
            ("path=~/private/key.pem missing", "key.pem"),
            ("connect 192.168.1.20:8080 refused", "192.168.1.20"),
            ("listening on [fe80::1ff:fe23:4567:890a] now", "fe80"),
            ("peer 10.0.0.7.", "10.0.0.7"),
        ];
        for (message, secret) in cases {
            let line = sanitize(message, home);
            assert!(!line.contains(secret), "{message:?} became {line:?}");
        }
        assert_eq!(
            sanitize("took 14:07:13 at /usr/share/kestrel, v0.4.34", home),
            "took 14:07:13 at /usr/share/kestrel, v0.4.34",
            "times, system paths, and versions are kept"
        );
        assert_eq!(
            sanitize("/home/anabel/file", home),
            "/home/anabel/file",
            "another user's similarly named home is not mistaken for this one"
        );
    }

    #[test]
    fn debug_detail_appears_only_while_switched_on() {
        let directory = TempDir::new().expect("temp dir");
        let logger = FileLogger::new(directory.path().to_owned(), policy(false), false, false);

        emit(&logger, Level::Debug, "kestrel", "hidden detail");
        emit(&logger, Level::Info, "audio.mixer", "feature started");
        logger.apply(policy(true));
        emit(&logger, Level::Debug, "kestrel", "visible detail");
        emit(
            &logger,
            Level::Info,
            "zbus::connection",
            "third-party chatter",
        );
        emit(
            &logger,
            Level::Warn,
            "zbus::connection",
            "third-party warning",
        );

        let text = contents(directory.path());
        assert!(!text.contains("hidden detail"));
        assert!(text.contains("INFO  audio.mixer feature started"));
        assert!(text.contains("DEBUG kestrel visible detail"));
        assert!(!text.contains("third-party chatter"));
        assert!(text.contains("WARN  kestrel third-party warning"));
    }

    #[test]
    fn forced_debug_survives_the_setting_turning_off() {
        let directory = TempDir::new().expect("temp dir");
        let logger = FileLogger::new(directory.path().to_owned(), policy(false), true, false);
        logger.apply(policy(false));
        emit(&logger, Level::Debug, "kestrel", "forced detail");
        assert!(contents(directory.path()).contains("forced detail"));
    }

    #[test]
    fn files_are_private_and_rotate_at_a_quarter_of_the_cap() {
        let directory = TempDir::new().expect("temp dir");
        let logs = directory.path().join("logs");
        let small = LogPolicy {
            max_total_bytes: 256 * 1024,
            ..policy(false)
        };
        let logger = FileLogger::new(logs.clone(), small, false, false);
        let line = "x".repeat(1024);
        for _ in 0..70 {
            emit(&logger, Level::Warn, "kestrel", &line);
        }

        let mode =
            |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&logs), 0o700);
        let files = fs::read_dir(&logs)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            files.len(),
            2,
            "64 KiB files: the 70th line starts a second"
        );
        for file in &files {
            assert_eq!(mode(file), 0o600);
        }
    }

    #[test]
    fn pruning_drops_expired_files_then_the_oldest_over_the_cap() {
        let directory = TempDir::new().expect("temp dir");
        let now = SystemTime::now();
        let make = |name: &str, age_days: u64, bytes: usize| {
            let path = directory.path().join(name);
            fs::write(&path, vec![b'x'; bytes]).unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(age_days * 86_400))
                .unwrap();
        };
        make("kestrel-2026-10-01.log", 9, 10);
        make("kestrel-2026-10-08.log", 3, 600);
        make("kestrel-2026-10-09.log", 2, 600);
        make("kestrel-2026-10-10.log", 1, 600);
        make("notes.txt", 30, 10);
        let logger = FileLogger::new(
            directory.path().to_owned(),
            LogPolicy {
                max_total_bytes: 1300,
                ..policy(false)
            },
            false,
            false,
        );

        let removed = logger.prune(now).expect("prune");

        let mut left = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(removed, 2);
        assert_eq!(
            left,
            vec![
                "kestrel-2026-10-09.log",
                "kestrel-2026-10-10.log",
                "notes.txt"
            ],
            "the expired file and the oldest over the cap go; other files stay"
        );
    }

    #[test]
    fn clearing_removes_every_log_and_logging_continues() {
        let directory = TempDir::new().expect("temp dir");
        let logger = FileLogger::new(directory.path().to_owned(), policy(false), false, false);
        emit(&logger, Level::Warn, "kestrel", "before");

        assert_eq!(logger.clear().expect("clear"), 1);
        emit(&logger, Level::Warn, "kestrel", "after");

        let text = contents(directory.path());
        assert!(!text.contains("before"));
        assert!(text.contains("after"));
    }
}
