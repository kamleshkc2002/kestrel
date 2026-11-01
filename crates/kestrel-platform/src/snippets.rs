//! Capability-gated text insertion and local time rendering.
//! Providers are verified before use; uinput requires deliberate input-device access.

use std::{
    env,
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    /// Providers in automatic preference order; `ydotool` is omitted.
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

    /// Transport used for capability evidence.
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

/// A verified provider and its executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionPath {
    provider: InsertionProvider,
    executable: PathBuf,
}

impl InsertionPath {
    pub fn provider(&self) -> InsertionProvider {
        self.provider
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionErrorKind {
    MissingDependency,
    SpawnFailed,
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
    fn insert_text(&mut self, text: &str) -> Result<(), InsertionError>;
}

/// `preferred` pins one provider; automatic search uses providers suitable for unattended access.
pub fn discover_insertion_provider_with(
    preferred: SnippetProviderPreference,
    path_env: Option<&OsStr>,
) -> Result<InsertionPath, InsertionError> {
    let candidates: Vec<InsertionProvider> = match InsertionProvider::from_preference(preferred) {
        Some(provider) => vec![provider],
        None => InsertionProvider::AUTO_ORDER.to_vec(),
    };
    for provider in &candidates {
        if let Some(executable) =
            crate::applications::resolve_executable(provider.executable(), path_env)
        {
            return Ok(InsertionPath {
                provider: *provider,
                executable,
            });
        }
    }

    // Report every checked provider, including an explicitly pinned one.
    let checked = candidates
        .iter()
        .map(|provider| provider.executable())
        .collect::<Vec<_>>()
        .join(", ");
    let mut message = format!("no verified insertion provider was found (checked {checked})");
    if preferred == SnippetProviderPreference::Auto
        && crate::applications::resolve_executable("ydotool", path_env).is_some()
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

pub fn discover_insertion_provider(
    preferred: SnippetProviderPreference,
) -> Result<InsertionPath, InsertionError> {
    discover_insertion_provider_with(preferred, env::var_os("PATH").as_deref())
}

/// Stdin keeps text out of `/proc/<pid>/cmdline`; discarded output bounds exposure.
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

    /// Starts the provider, retrying briefly on `ETXTBSY`.
    ///
    /// Package-manager replacement can briefly make the executable busy.
    fn spawn_provider(&self) -> Result<std::process::Child, InsertionError> {
        let mut command = Command::new(&self.executable);
        command
            .args(self.argument_list())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_with_busy_retry(&mut command).map_err(|error| {
            InsertionError::new(
                InsertionErrorKind::SpawnFailed,
                format!("{} could not be started: {error}", self.provider.label()),
            )
        })
    }

    fn argument_list(&self) -> Vec<OsString> {
        match self.provider {
            // `-` makes wtype read text from stdin.
            InsertionProvider::Wtype => vec![OsString::from("-")],
            InsertionProvider::Ydotool => vec![
                OsString::from("type"),
                OsString::from("--file"),
                OsString::from("-"),
            ],
            InsertionProvider::Xdotool => vec![
                OsString::from("type"),
                OsString::from("--clearmodifiers"),
                OsString::from("--file"),
                OsString::from("-"),
            ],
        }
    }

    /// Feeds text on a writer thread so timeout handling stays responsive.
    fn send_text(&self, child: &mut std::process::Child, text: &str) -> Result<(), InsertionError> {
        let Some(mut stdin) = child.stdin.take() else {
            return Err(InsertionError::new(
                InsertionErrorKind::SpawnFailed,
                format!("{} did not expose an input pipe", self.provider.label()),
            ));
        };
        let payload = text.as_bytes().to_vec();
        thread::Builder::new()
            .name("kestrel-insert-input".to_string())
            .spawn(move || {
                // Early pipe closure is reported by the provider's exit status.
                let _ = stdin.write_all(&payload);
            })
            .map(|_| ())
            .map_err(|error| {
                InsertionError::new(
                    InsertionErrorKind::SpawnFailed,
                    format!("{} input could not be sent: {error}", self.provider.label()),
                )
            })
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
        let mut child = self.spawn_provider()?;
        if let Err(error) = self.send_text(&mut child, text) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

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

    /// Local UTC offset formatted as `+HH:MM`.
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

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl SystemClock {
    pub fn timezone_name(&self) -> String {
        // SAFETY: `localtime_r` fills caller-owned storage; copy the zone immediately.
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

    pub fn local_time(&self) -> LocalTime {
        LocalTime::from_system_time(SystemTime::now())
    }
}

impl LocalTime {
    /// Converts an instant to the local calendar, or the Unix epoch if the
    /// time cannot be represented.
    pub fn from_system_time(time: SystemTime) -> Self {
        let seconds = time
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let seconds = libc::time_t::try_from(seconds).unwrap_or_default();
        // SAFETY: `localtime_r` fills caller-owned storage; copy fields before it dies.
        unsafe {
            let mut local: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&seconds, &mut local).is_null() {
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

/// Builds capability evidence for one discovery attempt.
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
        // The library and editor need no provider; only typing a snippet into
        // another application does.
        Err(error) => CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Limited {
                reason: "no insertion provider is installed".to_string(),
            },
            "Snippets can be stored and edited; inserting them into other applications needs an \
             insertion provider.",
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

impl InsertionError {
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use kestrel_core::{CapabilityStatus, SnippetProviderPreference};
    use tempfile::TempDir;

    use super::{
        ExecutableInsertionBackend, InsertionBackend, InsertionErrorKind, InsertionPath,
        InsertionProvider, LocalTime, discover_insertion_provider_with,
        insertion_capability_report,
    };

    /// Writes a stub provider for argument and exit-status tests.
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
    fn automatic_discovery_prefers_supported_provider_order() {
        for (case, first, second, expected) in [
            (
                "X11 provider is preferred over uinput",
                "xdotool",
                "ydotool",
                InsertionProvider::Xdotool,
            ),
            (
                "Wayland provider wins over X11",
                "wtype",
                "xdotool",
                InsertionProvider::Wtype,
            ),
        ] {
            let dir = TempDir::new().expect("temp dir");
            stub(&dir, first, "#!/bin/sh\nexit 0\n");
            stub(&dir, second, "#!/bin/sh\nexit 0\n");

            let path = discover_insertion_provider_with(
                SnippetProviderPreference::Auto,
                Some(&path_env(&dir)),
            )
            .expect("a provider is discovered");

            assert_eq!(path.provider(), expected, "{case}");
        }
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
    fn discovery_ignores_relative_entries() {
        let relative = std::ffi::OsString::from(".");
        let error =
            discover_insertion_provider_with(SnippetProviderPreference::Auto, Some(&relative))
                .expect_err("relative PATH entries are not searched");
        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
    }

    #[test]
    fn insertion_sends_text_over_stdin_and_never_through_argv() {
        let dir = TempDir::new().expect("temp dir");
        let record = dir.path().join("args.txt");
        let input = dir.path().join("input.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat > {}\nexit 0\n",
            record.display(),
            input.display()
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
        assert_eq!(
            args, "-\n",
            "only the stdin marker is passed; the text is not an argument"
        );
        assert!(!args.contains("line text"));
        let typed = fs::read_to_string(&input).expect("the stub recorded its input");
        assert_eq!(typed, "multi\nline text");
        assert_eq!(backend.provider(), InsertionProvider::Wtype);
    }

    #[test]
    fn every_provider_reads_its_text_from_stdin() {
        for (provider, expected) in [
            (InsertionProvider::Wtype, vec!["-"]),
            (InsertionProvider::Ydotool, vec!["type", "--file", "-"]),
            (
                InsertionProvider::Xdotool,
                vec!["type", "--clearmodifiers", "--file", "-"],
            ),
        ] {
            let backend = ExecutableInsertionBackend::new(
                InsertionPath {
                    provider,
                    executable: std::path::PathBuf::from("/nonexistent"),
                },
                500,
            )
            .expect("the backend builds");
            let arguments = backend
                .argument_list()
                .into_iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(arguments, expected, "{} reads stdin", provider.label());
        }
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
        assert!(
            matches!(missing.status, CapabilityStatus::Limited { .. }),
            "the library still runs without an insertion provider"
        );
        assert!(
            missing
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("preferred_provider")),
            "the remediation explains the explicit opt-in"
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
    fn discovery_without_a_path_finds_nothing() {
        let error = discover_insertion_provider_with(SnippetProviderPreference::Auto, None)
            .expect_err("no PATH means no provider");
        assert_eq!(error.kind, InsertionErrorKind::MissingDependency);
    }
}
