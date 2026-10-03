//! Capability-gated text insertion and local time rendering.
//!
//! Injection is never assumed. Kestrel probes for a documented provider, and
//! when none is verified the feature reports the dependency and refuses to
//! insert instead of guessing at a mechanism. The uinput-based provider is
//! never selected automatically, because it needs input-device access that a
//! user must grant deliberately.

use std::{
    env,
    error::Error,
    ffi::{OsStr, OsString},
    fmt, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use kestrel_core::{
    CapabilityEvidence, CapabilityReport, CapabilityStatus, SnippetProviderPreference,
};

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "snippets.text";
const POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionProvider {
    Wtype,
    Ydotool,
    Xdotool,
}

impl InsertionProvider {
    /// Providers in automatic preference order.
    ///
    /// The least-privileged verified mechanism wins: a compositor protocol, then
    /// the X11 compatibility path. `ydotool` is intentionally absent.
    pub const AUTO_ORDER: [InsertionProvider; 2] =
        [InsertionProvider::Wtype, InsertionProvider::Xdotool];

    pub const fn executable(self) -> &'static str {
        match self {
            Self::Wtype => "wtype",
            Self::Ydotool => "ydotool",
            Self::Xdotool => "xdotool",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wtype => "wtype",
            Self::Ydotool => "ydotool",
            Self::Xdotool => "xdotool",
        }
    }

    /// The transport this provider uses, for capability evidence.
    pub const fn transport(self) -> &'static str {
        match self {
            Self::Wtype => "Wayland virtual-keyboard protocol",
            Self::Ydotool => "uinput virtual input",
            Self::Xdotool => "X11 XTEST",
        }
    }

    pub const fn requirement(self) -> &'static str {
        match self {
            Self::Wtype => "wtype and a compositor that implements the virtual-keyboard protocol",
            Self::Ydotool => "ydotool with its daemon and access to /dev/uinput",
            Self::Xdotool => "xdotool and an X11 display",
        }
    }

    /// Whether this provider needs broad input-device access.
    pub const fn needs_input_device_access(self) -> bool {
        matches!(self, Self::Ydotool)
    }

    pub const fn from_preference(
        preference: SnippetProviderPreference,
    ) -> Option<InsertionProvider> {
        match preference {
            SnippetProviderPreference::Auto => None,
            SnippetProviderPreference::Wtype => Some(InsertionProvider::Wtype),
            SnippetProviderPreference::Ydotool => Some(InsertionProvider::Ydotool),
            SnippetProviderPreference::Xdotool => Some(InsertionProvider::Xdotool),
        }
    }
}

/// A verified provider and the executable that implements it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionPath {
    provider: InsertionProvider,
    executable: PathBuf,
}

impl InsertionPath {
    pub fn provider(&self) -> InsertionProvider {
        self.provider
    }

    /// The resolved executable path. Kept out of capability evidence and
    /// configuration export because resolved paths are machine-specific.
    pub fn executable(&self) -> &Path {
        &self.executable
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionErrorKind {
    /// No verified insertion provider is available.
    MissingDependency,
    /// The provider executable could not be started.
    SpawnFailed,
    /// The provider started but its exit status could not be collected.
    WaitFailed,
    TimedOut,
    Rejected,
    EmptyText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionError {
    pub kind: InsertionErrorKind,
    pub message: String,
}

impl InsertionError {
    fn new(kind: InsertionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for InsertionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for InsertionError {}

pub trait InsertionBackend: Send + 'static {
    fn provider(&self) -> InsertionProvider;
    /// Types `text` into the focused window.
    fn insert_text(&mut self, text: &str) -> Result<(), InsertionError>;
}

/// Finds a provider executable without invoking a shell.
///
/// `preferred` pins one provider; otherwise the automatic order is used and the
/// uinput provider is skipped. `path_env` and the returned path are explicit so
/// the search is deterministic and testable.
pub fn discover_insertion_provider_with(
    preferred: SnippetProviderPreference,
    path_env: Option<&OsStr>,
) -> Result<InsertionPath, InsertionError> {
    let candidates: Vec<InsertionProvider> = match InsertionProvider::from_preference(preferred) {
        Some(provider) => vec![provider],
        None => InsertionProvider::AUTO_ORDER.to_vec(),
    };
    let search_roots = search_roots(path_env);
    for provider in &candidates {
        if let Some(executable) = find_executable(provider.executable(), &search_roots) {
            return Ok(InsertionPath {
                provider: *provider,
                executable,
            });
        }
    }

    // Report what was actually checked, including the opt-in provider when the
    // user pinned it.
    let checked = candidates
        .iter()
        .map(|provider| provider.executable())
        .collect::<Vec<_>>()
        .join(", ");
    let mut message = format!("no verified insertion provider was found (checked {checked})");
    if preferred == SnippetProviderPreference::Auto
        && find_executable("ydotool", &search_roots).is_some()
    {
        message.push_str(
            "; ydotool is present but needs input-device access, so it is only used when \
             [snippets] preferred_provider = \"ydotool\"",
        );
    }
    Err(InsertionError::new(
        InsertionErrorKind::MissingDependency,
        message,
    ))
}

/// Probes for a provider using the environment's `PATH`.
pub fn discover_insertion_provider(
    preferred: SnippetProviderPreference,
) -> Result<InsertionPath, InsertionError> {
    discover_insertion_provider_with(preferred, env::var_os("PATH").as_deref())
}

/// Splits `PATH` into search roots, dropping empty and relative entries.
fn search_roots(path_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(path_env) = path_env else {
        return Vec::new();
    };
    env::split_paths(path_env)
        .filter(|root| root.is_absolute())
        .collect()
}

/// Resolves one executable name against the search roots.
fn find_executable(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    for root in roots {
        let candidate = root.join(name);
        let Ok(metadata) = candidate.metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

/// Runs a verified provider for one insertion.
///
/// The payload travels as one argument, never through a shell, and provider
/// output is discarded: only the exit status is reported, which bounds both the
/// output size and the failure surface.
pub struct ExecutableInsertionBackend {
    provider: InsertionProvider,
    executable: PathBuf,
    timeout: Duration,
}

impl ExecutableInsertionBackend {
    pub fn new(path: InsertionPath, timeout_millis: u64) -> Result<Self, InsertionError> {
        if timeout_millis == 0 {
            return Err(InsertionError::new(
                InsertionErrorKind::Rejected,
                "the insertion timeout must be non-zero",
            ));
        }
        Ok(Self {
            provider: path.provider,
            executable: path.executable,
            timeout: Duration::from_millis(timeout_millis),
        })
    }

    /// Starts the provider, retrying briefly when the executable is busy.
    ///
    /// `exec` fails with `ETXTBSY` while the binary is open for writing, which
    /// happens when a package manager replaces or upgrades the provider. A short
    /// bounded retry turns that transient window into a successful insert
    /// instead of a confusing failure.
    fn spawn_provider(&self, text: &str) -> Result<std::process::Child, InsertionError> {
        let mut command = Command::new(&self.executable);
        command
            .args(self.argument_list(text))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_with_busy_retry(&mut command).map_err(|error| {
            InsertionError::new(
                InsertionErrorKind::SpawnFailed,
                format!("{} could not be started: {error}", self.provider.label()),
            )
        })
    }

    fn argument_list(&self, text: &str) -> Vec<OsString> {
        match self.provider {
            // `--` stops option parsing so snippet text can start with a dash.
            InsertionProvider::Wtype => vec![OsString::from("--"), OsString::from(text)],
            InsertionProvider::Ydotool => vec![
                OsString::from("type"),
                OsString::from("--"),
                OsString::from(text),
            ],
            InsertionProvider::Xdotool => vec![
                OsString::from("type"),
                OsString::from("--clearmodifiers"),
                OsString::from("--"),
                OsString::from(text),
            ],
        }
    }
}

impl InsertionBackend for ExecutableInsertionBackend {
    fn provider(&self) -> InsertionProvider {
        self.provider
    }

    fn insert_text(&mut self, text: &str) -> Result<(), InsertionError> {
        if text.is_empty() {
            return Err(InsertionError::new(
                InsertionErrorKind::EmptyText,
                "an empty snippet cannot be inserted",
            ));
        }
        let mut child = self.spawn_provider(text)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(InsertionError::new(
                        InsertionErrorKind::Rejected,
                        format!("{} refused the insertion ({status})", self.provider.label()),
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(InsertionError::new(
                        InsertionErrorKind::WaitFailed,
                        format!("{} could not be waited for: {error}", self.provider.label()),
                    ));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InsertionError::new(
                    InsertionErrorKind::TimedOut,
                    format!(
                        "{} did not finish within {} ms",
                        self.provider.label(),
                        self.timeout.as_millis()
                    ),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Local wall-clock fields for one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub utc_offset_seconds: i32,
}

impl LocalTime {
    pub fn iso_date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn iso_time(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    pub fn iso_datetime(&self) -> String {
        format!("{} {}", self.iso_date(), self.iso_time())
    }

    /// The local UTC offset as `+HH:MM`.
    pub fn utc_offset(&self) -> String {
        let sign = if self.utc_offset_seconds < 0 {
            '-'
        } else {
            '+'
        };
        let total = self.utc_offset_seconds.unsigned_abs();
        format!("{sign}{:02}:{:02}", total / 3600, (total % 3600) / 60)
    }
}

/// Supplies the local time used to render snippet variables.
pub trait Clock: Send + Sync {
    /// The local time zone name, or a neutral fallback when unavailable.
    fn timezone_name(&self) -> String;
    fn local_time(&self) -> LocalTime;
}

/// The session's local time through the C library, without a date dependency.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn timezone_name(&self) -> String {
        // SAFETY: `localtime_r` fills a caller-owned `tm`; the zone pointer it
        // exposes is read immediately and copied.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut local: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut local).is_null() {
                return "local".to_string();
            }
            if local.tm_zone.is_null() {
                return "local".to_string();
            }
            std::ffi::CStr::from_ptr(local.tm_zone)
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|_| "local".to_string())
        }
    }

    fn local_time(&self) -> LocalTime {
        // SAFETY: as above; every field is copied out before the buffer dies.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut local: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut local).is_null() {
                return LocalTime {
                    year: 1970,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    utc_offset_seconds: 0,
                };
            }
            LocalTime {
                year: local.tm_year + 1900,
                month: (local.tm_mon + 1).clamp(1, 12) as u32,
                day: local.tm_mday.clamp(1, 31) as u32,
                hour: local.tm_hour.clamp(0, 23) as u32,
                minute: local.tm_min.clamp(0, 59) as u32,
                second: local.tm_sec.clamp(0, 60) as u32,
                utc_offset_seconds: local.tm_gmtoff as i32,
            }
        }
    }
}

/// Probes whether a verified insertion provider exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct SnippetInsertionProbe {
    preferred: SnippetProviderPreference,
}

impl SnippetInsertionProbe {
    pub fn new(preferred: SnippetProviderPreference) -> Self {
        Self { preferred }
    }
}

impl CapabilityProbe for SnippetInsertionProbe {
    fn probe(&self) -> CapabilityReport {
        insertion_capability_report(discover_insertion_provider(self.preferred))
    }
}

/// The capability report for one discovery attempt.
pub fn insertion_capability_report(
    discovered: Result<InsertionPath, InsertionError>,
) -> CapabilityReport {
    match discovered {
        Ok(path) => {
            let provider = path.provider();
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                format!(
                    "Snippets can be inserted through {} ({}).",
                    provider.label(),
                    provider.transport()
                ),
            )
            .with_selected_backend(provider.label())
            .with_alternative(provider.requirement())
            .with_evidence(CapabilityEvidence::new(
                "insertion_provider",
                provider.label(),
            ))
            .with_evidence(CapabilityEvidence::new(
                "insertion_transport",
                provider.transport(),
            ))
            .with_evidence(CapabilityEvidence::new(
                "input_device_access",
                if provider.needs_input_device_access() {
                    "required"
                } else {
                    "not_required"
                },
            ))
            .with_evidence(CapabilityEvidence::new("storage", "private_file"))
        }
        Err(error) => CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::MissingDependency {
                name: "an insertion provider".to_string(),
            },
            "Snippet insertion is unavailable until a provider is installed.",
        )
        .with_remediation(
            "Install wtype for Wayland or xdotool for X11. ydotool needs input-device access and \
             is only used when [snippets] preferred_provider = \"ydotool\".",
        )
        .with_evidence(CapabilityEvidence::new("insertion_provider", "none"))
        .with_evidence(CapabilityEvidence::new("insertion_error", error.kind()))
        .with_evidence(CapabilityEvidence::new("storage", "private_file")),
    }
}

fn io_kind(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::TimedOut => "timed_out",
        _ => "io",
    }
}

impl InsertionError {
    /// A stable machine-readable kind for capability evidence.
    fn kind(&self) -> &'static str {
        match self.kind {
            InsertionErrorKind::MissingDependency => "missing_dependency",
            InsertionErrorKind::SpawnFailed => "spawn_failed",
            InsertionErrorKind::WaitFailed => "wait_failed",
            InsertionErrorKind::TimedOut => "timed_out",
            InsertionErrorKind::Rejected => "rejected",
            InsertionErrorKind::EmptyText => "empty_text",
        }
    }
}

/// Reads a path's executable bit, used by callers that validate a configured
/// provider before using it.
pub fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Maps an I/O failure onto insertion evidence without leaking a path.
pub fn insertion_io_kind(error: &io::Error) -> &'static str {
    io_kind(error)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };

    use kestrel_core::{CapabilityStatus, SnippetProviderPreference};
    use tempfile::TempDir;

    use super::{
        Clock, ExecutableInsertionBackend, InsertionBackend, InsertionErrorKind, InsertionPath,
        InsertionProvider, LocalTime, SnippetInsertionProbe, SystemClock,
        discover_insertion_provider, discover_insertion_provider_with, insertion_capability_report,
        is_executable,
    };
    use crate::CapabilityProbe;

    /// Writes a stub provider that records its arguments and exit behaviour.
    fn stub(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, body).expect("stub is written");
        let mut permissions = fs::metadata(&path).expect("stub metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("stub is executable");
        path
    }

    fn path_env(dir: &TempDir) -> std::ffi::OsString {
        std::ffi::OsString::from(dir.path().as_os_str())
    }

    #[test]
    fn discovery_prefers_the_least_privileged_verified_provider() {
        let dir = TempDir::new().expect("temp dir");
        stub(&dir, "xdotool", "#!/bin/sh\nexit 0\n");
        stub(&dir, "ydotool", "#!/bin/sh\nexit 0\n");

        let path = discover_insertion_provider_with(
            SnippetProviderPreference::Auto,
            Some(&path_env(&dir)),
        )
        .expect("xdotool is discovered");

        assert_eq!(
            path.provider(),
            InsertionProvider::Xdotool,
            "automatic selection uses the X11 path and never the uinput provider"
        );
    }

    #[test]
    fn wtype_wins_over_xdotool_when_both_are_present() {
        let dir = TempDir::new().expect("temp dir");
        stub(&dir, "wtype", "#!/bin/sh\nexit 0\n");
        stub(&dir, "xdotool", "#!/bin/sh\nexit 0\n");

        let path = discover_insertion_provider_with(
            SnippetProviderPreference::Auto,
            Some(&path_env(&dir)),
        )
        .expect("a provider is discovered");

        assert_eq!(path.provider(), InsertionProvider::Wtype);
    }

    #[test]
    fn uinput_provider_requires_an_explicit_preference() {
        let dir = TempDir::new().expect("temp dir");
        stub(&dir, "ydotool", "#!/bin/sh\nexit 0\n");

        let error = discover_insertion_provider_with(
            SnippetProviderPreference::Auto,
            Some(&path_env(&dir)),
        )
        .expect_err("the uinput provider is not selected automatically");
        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
        assert!(
            error.message.contains("preferred_provider"),
            "the opt-in path is explained: {}",
            error.message
        );

        let path = discover_insertion_provider_with(
            SnippetProviderPreference::Ydotool,
            Some(&path_env(&dir)),
        )
        .expect("an explicit preference is honoured");
        assert_eq!(path.provider(), InsertionProvider::Ydotool);
    }

    #[test]
    fn a_pinned_provider_that_is_absent_is_reported() {
        let dir = TempDir::new().expect("temp dir");
        stub(&dir, "wtype", "#!/bin/sh\nexit 0\n");

        let error = discover_insertion_provider_with(
            SnippetProviderPreference::Xdotool,
            Some(&path_env(&dir)),
        )
        .expect_err("a pinned provider must exist");

        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
        assert!(error.message.contains("xdotool"));
    }

    #[test]
    fn discovery_ignores_non_executable_and_relative_entries() {
        let dir = TempDir::new().expect("temp dir");
        let not_executable = dir.path().join("wtype");
        fs::write(&not_executable, "#!/bin/sh\nexit 0\n").expect("file is written");
        assert!(!is_executable(&not_executable));

        let relative = std::ffi::OsString::from(".");
        let error =
            discover_insertion_provider_with(SnippetProviderPreference::Auto, Some(&relative))
                .expect_err("relative PATH entries are not searched");
        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
    }

    #[test]
    fn insertion_runs_the_provider_without_a_shell_and_reports_its_status() {
        let dir = TempDir::new().expect("temp dir");
        let record = dir.path().join("args.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            record.display()
        );
        let executable = stub(&dir, "wtype", &script);
        let mut backend = ExecutableInsertionBackend::new(
            InsertionPath {
                provider: InsertionProvider::Wtype,
                executable,
            },
            2_000,
        )
        .expect("the backend builds");

        backend
            .insert_text("multi\nline text")
            .expect("the stub accepts the insertion");

        let args = fs::read_to_string(&record).expect("the stub recorded its arguments");
        assert_eq!(args, "--\nmulti\nline text\n");
        assert_eq!(backend.provider(), InsertionProvider::Wtype);
    }

    #[test]
    fn insertion_reports_a_rejected_run_and_a_timeout() {
        let dir = TempDir::new().expect("temp dir");
        let failing = stub(&dir, "xdotool", "#!/bin/sh\nexit 3\n");
        let mut backend = ExecutableInsertionBackend::new(
            InsertionPath {
                provider: InsertionProvider::Xdotool,
                executable: failing,
            },
            500,
        )
        .expect("the backend builds");
        let error = backend
            .insert_text("text")
            .expect_err("a failing provider is reported");
        assert_eq!(
            error.kind,
            InsertionErrorKind::Rejected,
            "a provider that ran and refused must be reported as a rejection: {error}"
        );

        let slow = stub(&dir, "slow-xdotool", "#!/bin/sh\nsleep 5\n");
        let mut backend = ExecutableInsertionBackend::new(
            InsertionPath {
                provider: InsertionProvider::Xdotool,
                executable: slow,
            },
            150,
        )
        .expect("the backend builds");
        let error = backend
            .insert_text("text")
            .expect_err("a slow provider times out");
        assert_eq!(error.kind, InsertionErrorKind::TimedOut);

        assert!(
            matches!(
                ExecutableInsertionBackend::new(
                    InsertionPath {
                        provider: InsertionProvider::Xdotool,
                        executable: dir.path().join("xdotool"),
                    },
                    0
                ),
                Err(error) if error.kind == InsertionErrorKind::Rejected
            ),
            "a zero timeout is refused"
        );
    }

    #[test]
    fn a_busy_provider_binary_is_classified_for_retry() {
        let busy = std::io::Error::from_raw_os_error(libc::ETXTBSY);
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);

        assert!(crate::is_text_file_busy(&busy));
        assert!(!crate::is_text_file_busy(&missing));
    }

    #[test]
    fn empty_text_and_missing_executables_are_reported() {
        let dir = TempDir::new().expect("temp dir");
        let missing = dir.path().join("does-not-exist");
        let mut backend = ExecutableInsertionBackend::new(
            InsertionPath {
                provider: InsertionProvider::Wtype,
                executable: missing,
            },
            500,
        )
        .expect("the backend builds");

        assert_eq!(
            backend.insert_text("").expect_err("empty text").kind,
            InsertionErrorKind::EmptyText
        );
        assert_eq!(
            backend
                .insert_text("text")
                .expect_err("a missing executable is reported")
                .kind,
            InsertionErrorKind::SpawnFailed
        );
    }

    #[test]
    fn capability_reports_name_the_provider_evidence_without_a_path() {
        let dir = TempDir::new().expect("temp dir");
        let wtype = stub(&dir, "wtype", "#!/bin/sh\nexit 0\n");
        let report = insertion_capability_report(Ok(InsertionPath {
            provider: InsertionProvider::Wtype,
            executable: wtype.clone(),
        }));

        assert_eq!(report.status, CapabilityStatus::Supported);
        assert!(report.evidence.iter().any(|item| {
            item.key == "insertion_transport" && item.value == "Wayland virtual-keyboard protocol"
        }));
        assert!(
            report
                .evidence
                .iter()
                .any(|item| { item.key == "input_device_access" && item.value == "not_required" })
        );
        assert!(
            !report
                .evidence
                .iter()
                .any(|item| item.value.contains(&wtype.display().to_string())),
            "resolved paths stay out of capability evidence"
        );

        let missing = insertion_capability_report(Err(super::InsertionError::new(
            InsertionErrorKind::MissingDependency,
            "none",
        )));
        assert!(matches!(
            missing.status,
            CapabilityStatus::MissingDependency { .. }
        ));
        assert!(
            missing
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("preferred_provider")),
            "the remediation explains the explicit opt-in"
        );
    }

    #[test]
    fn probe_is_send_and_reports_a_definite_status() {
        let probe = SnippetInsertionProbe::new(SnippetProviderPreference::Auto);
        let report = probe.probe();
        assert_eq!(report.feature_id, super::FEATURE_ID);
        assert!(
            matches!(
                report.status,
                CapabilityStatus::Supported | CapabilityStatus::MissingDependency { .. }
            ),
            "the probe answers definitely: {:?}",
            report.status
        );
        let shared: Arc<Mutex<SnippetInsertionProbe>> = Arc::new(Mutex::new(probe));
        assert_eq!(
            shared.lock().expect("lock").probe().feature_id,
            super::FEATURE_ID
        );
    }

    #[test]
    fn local_time_formats_iso_fields_and_offsets() {
        let time = LocalTime {
            year: 2026,
            month: 10,
            day: 2,
            hour: 9,
            minute: 5,
            second: 7,
            utc_offset_seconds: 7200,
        };
        assert_eq!(time.iso_date(), "2026-10-02");
        assert_eq!(time.iso_time(), "09:05:07");
        assert_eq!(time.iso_datetime(), "2026-10-02 09:05:07");
        assert_eq!(time.utc_offset(), "+02:00");
        assert_eq!(
            LocalTime {
                utc_offset_seconds: -18000,
                ..time
            }
            .utc_offset(),
            "-05:00"
        );
    }

    #[test]
    fn system_clock_reports_plausible_local_fields() {
        let clock = SystemClock;
        let time = clock.local_time();
        assert!((2024..2100).contains(&time.year), "year {}", time.year);
        assert!((1..=12).contains(&time.month));
        assert!((1..=31).contains(&time.day));
        assert!(time.hour <= 23 && time.minute <= 59 && time.second <= 60);
        assert!(
            time.utc_offset_seconds.abs() <= 14 * 3600,
            "offset {} looks implausible",
            time.utc_offset_seconds
        );
        assert!(!clock.timezone_name().is_empty());
    }

    #[test]
    fn discovery_without_a_path_finds_nothing() {
        let error = discover_insertion_provider_with(SnippetProviderPreference::Auto, None)
            .expect_err("no PATH means no provider");
        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
    }

    #[test]
    fn provider_metadata_is_complete() {
        for provider in [
            InsertionProvider::Wtype,
            InsertionProvider::Ydotool,
            InsertionProvider::Xdotool,
        ] {
            assert!(!provider.label().is_empty());
            assert!(!provider.transport().is_empty());
            assert!(!provider.requirement().is_empty());
        }
        assert!(InsertionProvider::Ydotool.needs_input_device_access());
        assert!(!InsertionProvider::Wtype.needs_input_device_access());
        assert_eq!(InsertionProvider::AUTO_ORDER.len(), 2);
        assert!(
            !InsertionProvider::AUTO_ORDER.contains(&InsertionProvider::Ydotool),
            "the automatic order never includes the uinput provider"
        );
        assert_eq!(
            InsertionProvider::from_preference(SnippetProviderPreference::Auto),
            None
        );
    }

    #[test]
    fn discovery_uses_the_process_path_when_no_preference_is_pinned() {
        // The real environment decides the outcome; the call must not panic and
        // must agree with its own report.
        let discovered = discover_insertion_provider(SnippetProviderPreference::Auto);
        let report = insertion_capability_report(discovered);
        assert!(matches!(
            report.status,
            CapabilityStatus::Supported | CapabilityStatus::MissingDependency { .. }
        ));
    }
}
