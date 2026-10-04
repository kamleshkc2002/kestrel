//! User-initiated, bounded network diagnostics through `curl`.
//! Curl stderr stays private because it may contain addresses or identifiers.

use std::{
    env, fmt,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};

use crate::{CapabilityProbe, applications::resolve_executable};

pub const FEATURE_ID: &str = "network.speed_test";
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const KILL_GRACE: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT_SECONDS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedTestProvider {
    pub name: &'static str,
    pub host: &'static str,
}

pub const CLOUDFLARE_PROVIDER: SpeedTestProvider = SpeedTestProvider {
    name: "Cloudflare",
    host: "speed.cloudflare.com",
};

impl SpeedTestProvider {
    pub fn download_url(&self, bytes: u64) -> String {
        format!("https://{}/__down?bytes={bytes}", self.host)
    }

    pub fn upload_url(&self) -> String {
        format!("https://{}/__up", self.host)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedTestPlan {
    pub download_bytes: u64,
    pub upload_bytes: u64,
    pub phase_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedTestPhase {
    Latency,
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedTestProgress {
    pub phase: SpeedTestPhase,
    pub transferred_bytes: u64,
    pub phase_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedTestMeasurement {
    pub latency_millis: Option<u32>,
    pub download_bits_per_second: Option<u64>,
    pub downloaded_bytes: u64,
    pub upload_bits_per_second: Option<u64>,
    pub uploaded_bytes: u64,
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedTestErrorKind {
    Unavailable,
    Cancelled,
    TimedOut,
    NameResolution,
    Connection,
    Tls,
    HttpStatus,
    Protocol,
    SpawnFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedTestError {
    pub kind: SpeedTestErrorKind,
    pub message: String,
}

impl SpeedTestError {
    fn new(kind: SpeedTestErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for SpeedTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SpeedTestError {}

#[derive(Debug, Clone)]
pub struct CancellationToken(Arc<AtomicBool>);

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait SpeedTestBackend: Send + Sync + 'static {
    fn provider(&self) -> SpeedTestProvider;
    fn is_available(&self) -> bool;
    fn run(
        &self,
        plan: &SpeedTestPlan,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(SpeedTestProgress),
    ) -> Result<SpeedTestMeasurement, SpeedTestError>;
}

#[derive(Debug, Clone)]
pub struct CurlSpeedTestBackend {
    executable: Option<PathBuf>,
    provider: SpeedTestProvider,
}

impl CurlSpeedTestBackend {
    /// Resolves curl using local `PATH` inspection.
    pub fn discover() -> Self {
        Self {
            executable: resolve_executable("curl", env::var_os("PATH").as_deref()),
            provider: CLOUDFLARE_PROVIDER,
        }
    }

    pub fn with_executable(executable: PathBuf, provider: SpeedTestProvider) -> Self {
        let executable = executable
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .map(|_| executable);
        Self {
            executable,
            provider,
        }
    }
}

impl SpeedTestBackend for CurlSpeedTestBackend {
    fn provider(&self) -> SpeedTestProvider {
        self.provider
    }

    fn is_available(&self) -> bool {
        self.executable.is_some()
    }

    fn run(
        &self,
        plan: &SpeedTestPlan,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(SpeedTestProgress),
    ) -> Result<SpeedTestMeasurement, SpeedTestError> {
        let executable = self.executable.as_ref().ok_or_else(|| {
            SpeedTestError::new(SpeedTestErrorKind::Unavailable, "curl is not available")
        })?;
        let started = Instant::now();
        if cancel.is_cancelled() {
            return Err(SpeedTestError::new(
                SpeedTestErrorKind::Cancelled,
                "Speed test was cancelled.",
            ));
        }

        progress(SpeedTestProgress {
            phase: SpeedTestPhase::Latency,
            transferred_bytes: 0,
            phase_bytes: 0,
        });
        let latency = self.run_latency(executable, plan, cancel)?;

        progress(SpeedTestProgress {
            phase: SpeedTestPhase::Download,
            transferred_bytes: 0,
            phase_bytes: plan.download_bytes,
        });
        let (downloaded, download_first, download_end) =
            self.run_download(executable, plan, cancel, progress)?;

        let (uploaded, upload_speed) = if plan.upload_bytes == 0 {
            (0, None)
        } else {
            progress(SpeedTestProgress {
                phase: SpeedTestPhase::Upload,
                transferred_bytes: 0,
                phase_bytes: plan.upload_bytes,
            });
            self.run_upload(executable, plan, cancel, progress)?
        };

        let download_speed = download_first.and_then(|first| {
            let elapsed = download_end.saturating_duration_since(first);
            rate_bits(downloaded, elapsed)
        });
        Ok(SpeedTestMeasurement {
            latency_millis: latency,
            download_bits_per_second: download_speed,
            downloaded_bytes: downloaded,
            upload_bits_per_second: upload_speed,
            uploaded_bytes: uploaded,
            duration: started.elapsed(),
        })
    }
}

impl CurlSpeedTestBackend {
    fn command(&self, executable: &PathBuf, timeout: Duration) -> Command {
        let mut command = Command::new(executable);
        command
            .arg("--disable")
            .arg("--silent")
            .arg("--fail")
            .arg("--proto")
            .arg("=https")
            .arg("--connect-timeout")
            .arg(CONNECT_TIMEOUT_SECONDS.to_string())
            .arg("--max-time")
            .arg(timeout.as_secs().max(1).to_string())
            .process_group(0);
        command
    }

    fn run_latency(
        &self,
        executable: &PathBuf,
        plan: &SpeedTestPlan,
        cancel: &CancellationToken,
    ) -> Result<Option<u32>, SpeedTestError> {
        let mut command = self.command(executable, plan.phase_timeout);
        command
            .arg("--output")
            .arg("/dev/null")
            .arg("--write-out")
            .arg("%{time_pretransfer} %{time_starttransfer}\n")
            .arg(self.provider.download_url(0))
            .arg(self.provider.download_url(0))
            .arg(self.provider.download_url(0))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let (mut child, group) = spawn(command)?;
        let stdout = child.stdout.take().expect("latency stdout is piped");
        let receiver = spawn_latency_reader(stdout);
        let status = wait_process(&mut child, group, plan.phase_timeout, cancel)?;
        let output = receiver.recv_timeout(KILL_GRACE).unwrap_or_default();
        if !status.success() {
            return Err(error_for_exit(status.code()));
        }
        let mut values = output
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pre = fields.next()?.parse::<f64>().ok()?;
                let start = fields.next()?.parse::<f64>().ok()?;
                Some((start - pre).max(0.0) * 1_000.0)
            })
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        if values.len() != 3 {
            return Err(SpeedTestError::new(
                SpeedTestErrorKind::Protocol,
                "curl returned an invalid latency measurement.",
            ));
        }
        values.sort_by(f64::total_cmp);
        Ok(Some(values[1].round().clamp(0.0, u32::MAX as f64) as u32))
    }

    fn run_download(
        &self,
        executable: &PathBuf,
        plan: &SpeedTestPlan,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(SpeedTestProgress),
    ) -> Result<(u64, Option<Instant>, Instant), SpeedTestError> {
        let mut command = self.command(executable, plan.phase_timeout);
        command
            .arg("--output")
            .arg("-")
            .arg(self.provider.download_url(plan.download_bytes))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let (mut child, group) = spawn(command)?;
        let stdout = child.stdout.take().expect("download stdout is piped");
        let bytes = Arc::new(AtomicU64::new(0));
        let first = Arc::new(Mutex::new(None));
        let receiver =
            spawn_download_reader(stdout, plan.download_bytes, bytes.clone(), first.clone());
        let deadline = Instant::now() + plan.phase_timeout + KILL_GRACE;
        let mut capped = false;
        let status = loop {
            if cancel.is_cancelled() {
                kill_and_reap(&mut child, group);
                let _ = receiver.recv_timeout(KILL_GRACE);
                return Err(SpeedTestError::new(
                    SpeedTestErrorKind::Cancelled,
                    "Speed test was cancelled.",
                ));
            }
            let count = bytes.load(Ordering::Acquire);
            progress(SpeedTestProgress {
                phase: SpeedTestPhase::Download,
                transferred_bytes: count,
                phase_bytes: plan.download_bytes,
            });
            if count >= plan.download_bytes {
                capped = true;
                kill_and_reap(&mut child, group);
                break None;
            }
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(_) => {
                    kill_and_reap(&mut child, group);
                    return Err(SpeedTestError::new(
                        SpeedTestErrorKind::SpawnFailed,
                        "curl could not be waited for.",
                    ));
                }
            }
            if Instant::now() >= deadline {
                kill_and_reap(&mut child, group);
                let _ = receiver.recv_timeout(KILL_GRACE);
                return Err(SpeedTestError::new(
                    SpeedTestErrorKind::TimedOut,
                    "Speed test phase timed out.",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };
        let (downloaded, first_byte) = receiver.recv_timeout(KILL_GRACE).unwrap_or_else(|_| {
            (
                bytes.load(Ordering::Acquire),
                first.lock().ok().and_then(|guard| *guard),
            )
        });
        let end = Instant::now();
        if let Some(status) = status {
            if !status.success() && !capped {
                return Err(error_for_exit(status.code()));
            }
        }
        Ok((downloaded.min(plan.download_bytes), first_byte, end))
    }

    fn run_upload(
        &self,
        executable: &PathBuf,
        plan: &SpeedTestPlan,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(SpeedTestProgress),
    ) -> Result<(u64, Option<u64>), SpeedTestError> {
        let mut command = self.command(executable, plan.phase_timeout);
        // Stream stdin as the request body; suppress `100 Continue` upload delay.
        command
            .arg("--upload-file")
            .arg("-")
            .arg("--request")
            .arg("POST")
            .arg("--header")
            .arg("Content-Type: application/octet-stream")
            .arg("--header")
            .arg("Expect:")
            .arg("--output")
            .arg("/dev/null")
            .arg("--write-out")
            .arg("%{speed_upload} %{size_upload}")
            .arg(self.provider.upload_url())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::piped());
        let (mut child, group) = spawn(command)?;
        let stdin = child.stdin.take().expect("upload stdin is piped");
        let written = Arc::new(AtomicU64::new(0));
        let receiver = spawn_upload_writer(stdin, plan.upload_bytes, Arc::clone(&written));
        let deadline = Instant::now() + plan.phase_timeout + KILL_GRACE;
        let status = loop {
            if cancel.is_cancelled() {
                kill_and_reap(&mut child, group);
                let _ = receiver.recv_timeout(KILL_GRACE);
                return Err(SpeedTestError::new(
                    SpeedTestErrorKind::Cancelled,
                    "Speed test was cancelled.",
                ));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    kill_and_reap(&mut child, group);
                    let _ = receiver.recv_timeout(KILL_GRACE);
                    return Err(SpeedTestError::new(
                        SpeedTestErrorKind::SpawnFailed,
                        "curl could not be waited for.",
                    ));
                }
            }
            let sent = written.load(Ordering::Acquire);
            progress(SpeedTestProgress {
                phase: SpeedTestPhase::Upload,
                transferred_bytes: sent,
                phase_bytes: plan.upload_bytes,
            });
            if Instant::now() >= deadline {
                kill_and_reap(&mut child, group);
                let _ = receiver.recv_timeout(KILL_GRACE);
                return Err(SpeedTestError::new(
                    SpeedTestErrorKind::TimedOut,
                    "Speed test phase timed out.",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };
        let sent = receiver
            .recv_timeout(KILL_GRACE)
            .unwrap_or_else(|_| written.load(Ordering::Acquire));
        let stdout = child.stdout.take().expect("upload stdout is piped");
        let mut output = String::new();
        let mut bounded = stdout.take(256);
        let _ = bounded.read_to_string(&mut output);
        if !status.success() {
            return Err(error_for_exit(status.code()));
        }
        let mut fields = output.split_whitespace();
        let speed = fields.next().and_then(|value| value.parse::<f64>().ok());
        let reported = fields.next().and_then(|value| value.parse::<u64>().ok());
        // HTTP/1.1 chunk framing may inflate curl's byte count; reported bytes must meet the sent-byte count.
        if sent != plan.upload_bytes || !reported.is_some_and(|size| size >= sent) {
            return Err(SpeedTestError::new(
                SpeedTestErrorKind::Protocol,
                "curl returned an invalid upload measurement.",
            ));
        }
        let speed = speed.and_then(|value| {
            value
                .is_finite()
                .then(|| (value.max(0.0) * 8.0).min(u64::MAX as f64) as u64)
        });
        Ok((sent, speed))
    }
}

fn spawn(mut command: Command) -> Result<(Child, libc::pid_t), SpeedTestError> {
    let child = crate::spawn_with_busy_retry(&mut command).map_err(|_| {
        SpeedTestError::new(
            SpeedTestErrorKind::SpawnFailed,
            "curl could not be started.",
        )
    })?;
    let group = child.id() as libc::pid_t;
    Ok((child, group))
}

/// Sends `SIGKILL` to every member of a curl process group.
fn kill_group(group: libc::pid_t) {
    // SAFETY: The negative ID targets this run's group; `ESRCH` is harmless.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

fn kill_and_reap(child: &mut Child, group: libc::pid_t) {
    kill_group(group);
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_process(
    child: &mut Child,
    group: libc::pid_t,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<std::process::ExitStatus, SpeedTestError> {
    let deadline = Instant::now() + timeout + KILL_GRACE;
    loop {
        if cancel.is_cancelled() {
            kill_and_reap(child, group);
            return Err(SpeedTestError::new(
                SpeedTestErrorKind::Cancelled,
                "Speed test was cancelled.",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(child, group);
                return Err(SpeedTestError::new(
                    SpeedTestErrorKind::SpawnFailed,
                    "curl could not be waited for.",
                ));
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap(child, group);
            return Err(SpeedTestError::new(
                SpeedTestErrorKind::TimedOut,
                "Speed test phase timed out.",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Maps curl exit codes to typed failures.
///
/// Curl stderr stays private because it may contain resolved addresses or identifiers.
fn error_for_exit(code: Option<i32>) -> SpeedTestError {
    let (kind, message) = match code {
        Some(6) => (
            SpeedTestErrorKind::NameResolution,
            "The speed-test server name could not be resolved.",
        ),
        Some(7) => (
            SpeedTestErrorKind::Connection,
            "The speed-test server could not be reached.",
        ),
        Some(28) => (
            SpeedTestErrorKind::TimedOut,
            "The speed-test phase did not finish in time.",
        ),
        Some(35 | 51 | 58 | 60) => (
            SpeedTestErrorKind::Tls,
            "A secure connection to the speed-test server could not be verified.",
        ),
        Some(22) => (
            SpeedTestErrorKind::HttpStatus,
            "The speed-test server rejected the request.",
        ),
        _ => (
            SpeedTestErrorKind::Protocol,
            "curl stopped before the speed test finished.",
        ),
    };
    SpeedTestError::new(kind, message)
}

fn rate_bits(bytes: u64, elapsed: Duration) -> Option<u64> {
    let nanos = elapsed.as_nanos();
    (bytes > 0 && nanos > 0).then(|| {
        ((bytes as u128).saturating_mul(8_000_000_000) / nanos).min(u64::MAX as u128) as u64
    })
}

fn spawn_latency_reader(mut stdout: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("kestrel-speed-test-latency".to_string())
        .spawn(move || {
            let mut output = String::new();
            let mut bounded = (&mut stdout).take(4096);
            let _ = bounded.read_to_string(&mut output);
            let _ = sender.send(output);
        })
        .expect("latency reader thread must start");
    receiver
}

fn spawn_download_reader(
    mut stdout: impl Read + Send + 'static,
    limit: u64,
    bytes: Arc<AtomicU64>,
    first: Arc<Mutex<Option<Instant>>>,
) -> mpsc::Receiver<(u64, Option<Instant>)> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("kestrel-speed-test-download".to_string())
        .spawn(move || {
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                let remaining = limit.saturating_sub(bytes.load(Ordering::Acquire));
                if remaining == 0 {
                    break;
                }
                let window = usize::try_from(remaining)
                    .unwrap_or(usize::MAX)
                    .min(chunk.len());
                let count = match stdout.read(&mut chunk[..window]) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if count > 0 {
                    let now = Instant::now();
                    if let Ok(mut guard) = first.lock() {
                        if guard.is_none() {
                            *guard = Some(now);
                        }
                    }
                    bytes.fetch_add(count as u64, Ordering::Release);
                }
            }
            let result = (
                bytes.load(Ordering::Acquire).min(limit),
                first.lock().ok().and_then(|guard| *guard),
            );
            let _ = sender.send(result);
        })
        .expect("download reader thread must start");
    receiver
}

/// Writes exactly `total` zero bytes and publishes progress.
///
/// The bounded pipe ends the writer when curl exits or is killed.
fn spawn_upload_writer(
    mut stdin: impl Write + Send + 'static,
    total: u64,
    written: Arc<AtomicU64>,
) -> mpsc::Receiver<u64> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("kestrel-speed-test-upload".to_string())
        .spawn(move || {
            let zeros = [0_u8; 64 * 1024];
            let mut sent = 0_u64;
            while sent < total {
                let count = usize::try_from(total - sent)
                    .unwrap_or(usize::MAX)
                    .min(zeros.len());
                if stdin.write_all(&zeros[..count]).is_err() {
                    break;
                }
                sent += count as u64;
                written.store(sent, Ordering::Release);
            }
            drop(stdin);
            let _ = sender.send(sent);
        })
        .expect("upload writer thread must start");
    receiver
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SpeedTestProbe;

impl SpeedTestProbe {
    pub fn new() -> Self {
        Self
    }
}

impl CapabilityProbe for SpeedTestProbe {
    fn probe(&self) -> CapabilityReport {
        let backend = CurlSpeedTestBackend::discover();
        if backend.is_available() {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                "curl is available for on-demand network diagnostics.",
            )
            .with_selected_backend("curl")
            .with_evidence(CapabilityEvidence::new("provider", backend.provider.host))
            .with_evidence(CapabilityEvidence::new("transfers", "on_demand_only"))
        } else {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "curl is not installed.".to_string(),
                },
                "Network speed tests are unavailable because curl is not installed.",
            )
            .with_remediation("Install curl and ensure it is available on PATH.")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    use super::{
        CLOUDFLARE_PROVIDER, CancellationToken, CurlSpeedTestBackend, SpeedTestBackend,
        SpeedTestErrorKind, SpeedTestPhase, SpeedTestPlan,
    };

    /// Writes a curl stand-in that logs invocation arguments.
    ///
    /// Latency is the sole phase with no upload or download arguments.
    fn stub(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("curl");
        let log = dir.path().join("invocations");
        fs::create_dir(&log).expect("log directory is created");
        // A file per run avoids newline ambiguity in latency output.
        let script = format!(
            "#!/bin/sh\nn=$(ls {log} | wc -l)\nprintf '%s' \"$*\" > {log}/$n\n{body}\n",
            log = log.display()
        );
        fs::write(&path, script).expect("stub is written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("stub is executable");
        path
    }

    fn invocations(dir: &TempDir) -> Vec<String> {
        let log = dir.path().join("invocations");
        let mut runs = fs::read_dir(&log)
            .expect("log directory exists")
            .map(|entry| {
                let path = entry.expect("log entry").path();
                let index: usize = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse().ok())
                    .expect("runs are numbered");
                (index, fs::read_to_string(&path).expect("run is readable"))
            })
            .collect::<Vec<_>>();
        runs.sort();
        runs.into_iter().map(|(_, arguments)| arguments).collect()
    }

    fn plan(download_bytes: u64, upload_bytes: u64) -> SpeedTestPlan {
        SpeedTestPlan {
            download_bytes,
            upload_bytes,
            phase_timeout: Duration::from_secs(5),
        }
    }

    fn process_is_gone(pid: i32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
                .is_none_or(|state| state == 'Z'),
        }
    }

    fn wait_for_pid(path: &Path) -> i32 {
        for _ in 0..200 {
            if let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the stub never recorded its descendant");
    }

    const LATENCY_LINES: &str = "printf '0.010 0.030\\n0.010 0.050\\n0.010 0.020\\n'";

    #[test]
    fn a_full_run_measures_every_phase_within_the_plan() {
        let dir = TempDir::new().expect("temp dir");
        let uploaded = dir.path().join("uploaded");
        let executable = stub(
            &dir,
            &format!(
                "case \"$*\" in\n\
                 *--upload-file*) n=$(wc -c | tr -d ' '); echo \"$n\" > {uploaded}; \
                 printf '125000.0 %s' \"$n\" ;;\n\
                 *'--output -'*) head -c 3000000 /dev/zero ;;\n\
                 *) {LATENCY_LINES} ;;\n\
                 esac",
                uploaded = uploaded.display()
            ),
        );
        let backend = CurlSpeedTestBackend::with_executable(executable, CLOUDFLARE_PROVIDER);
        let mut phases = Vec::new();

        let measurement = backend
            .run(
                &plan(1_000_000, 200_000),
                &CancellationToken::new(),
                &mut |progress| {
                    phases.push(progress.phase);
                    assert!(
                        progress.transferred_bytes <= progress.phase_bytes.max(1),
                        "progress never exceeds the phase bound: {progress:?}"
                    );
                },
            )
            .expect("the stubbed run succeeds");

        assert_eq!(
            measurement.latency_millis,
            Some(20),
            "the median sample is reported"
        );
        assert_eq!(
            measurement.downloaded_bytes, 1_000_000,
            "a server that sends more than requested is cut off at the cap"
        );
        assert!(measurement.download_bits_per_second.is_some());
        assert_eq!(measurement.uploaded_bytes, 200_000);
        assert_eq!(
            fs::read_to_string(&uploaded)
                .expect("upload was read")
                .trim(),
            "200000",
            "exactly the planned payload reaches the uploader"
        );
        assert_eq!(measurement.upload_bits_per_second, Some(1_000_000));
        for phase in [
            SpeedTestPhase::Latency,
            SpeedTestPhase::Download,
            SpeedTestPhase::Upload,
        ] {
            assert!(phases.contains(&phase), "{phase:?} progress is reported");
        }

        let invocations = invocations(&dir);
        assert_eq!(invocations.len(), 3);
        for arguments in &invocations {
            assert!(
                arguments.starts_with("--disable "),
                "the user's curlrc is never read: {arguments}"
            );
            assert!(arguments.contains("--proto =https"), "{arguments}");
            assert!(
                !arguments.contains("--location"),
                "redirects are never followed"
            );
        }
        assert!(invocations[1].contains("https://speed.cloudflare.com/__down?bytes=1000000"));
        assert!(invocations[2].contains("https://speed.cloudflare.com/__up"));
    }

    #[test]
    fn a_plan_without_upload_never_starts_the_upload_phase() {
        let dir = TempDir::new().expect("temp dir");
        let executable = stub(
            &dir,
            &format!(
                "case \"$*\" in\n*'--output -'*) head -c 1000 /dev/zero ;;\n*) {LATENCY_LINES} ;;\nesac"
            ),
        );
        let backend = CurlSpeedTestBackend::with_executable(executable, CLOUDFLARE_PROVIDER);

        let measurement = backend
            .run(&plan(1_000, 0), &CancellationToken::new(), &mut |_| {})
            .expect("the stubbed run succeeds");

        assert_eq!(measurement.uploaded_bytes, 0);
        assert_eq!(measurement.upload_bits_per_second, None);
        assert_eq!(invocations(&dir).len(), 2, "only latency and download ran");
    }

    #[test]
    fn cancelling_ends_the_transfer_and_its_whole_process_group() {
        let dir = TempDir::new().expect("temp dir");
        let pid_file = dir.path().join("descendant.pid");
        let executable = stub(
            &dir,
            &format!(
                "case \"$*\" in\n*'--output -'*) sleep 30 & echo $! > {pid}; wait ;;\n*) {LATENCY_LINES} ;;\nesac",
                pid = pid_file.display()
            ),
        );
        let backend = CurlSpeedTestBackend::with_executable(executable, CLOUDFLARE_PROVIDER);
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        let watched = pid_file.clone();
        let watcher = std::thread::spawn(move || {
            let pid = wait_for_pid(&watched);
            canceller.cancel();
            pid
        });

        let started = Instant::now();
        let error = backend
            .run(&plan(1_000_000, 0), &cancel, &mut |_| {})
            .expect_err("a cancelled run fails");
        let pid = watcher.join().expect("watcher finishes");

        assert_eq!(error.kind, SpeedTestErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut gone = false;
        for _ in 0..100 {
            if process_is_gone(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone, "no descendant outlives a cancelled run");
    }

    #[test]
    fn a_stalled_phase_times_out_instead_of_waiting_forever() {
        let dir = TempDir::new().expect("temp dir");
        let executable = stub(
            &dir,
            &format!("case \"$*\" in\n*'--output -'*) sleep 30 ;;\n*) {LATENCY_LINES} ;;\nesac"),
        );
        let backend = CurlSpeedTestBackend::with_executable(executable, CLOUDFLARE_PROVIDER);
        let short = SpeedTestPlan {
            phase_timeout: Duration::from_secs(1),
            ..plan(1_000_000, 0)
        };

        let started = Instant::now();
        let error = backend
            .run(&short, &CancellationToken::new(), &mut |_| {})
            .expect_err("the stalled download times out");

        assert_eq!(error.kind, SpeedTestErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn curl_exit_codes_become_typed_failures_without_curl_text() {
        for (code, kind) in [
            (6, SpeedTestErrorKind::NameResolution),
            (7, SpeedTestErrorKind::Connection),
            (28, SpeedTestErrorKind::TimedOut),
            (60, SpeedTestErrorKind::Tls),
            (22, SpeedTestErrorKind::HttpStatus),
            (99, SpeedTestErrorKind::Protocol),
        ] {
            let dir = TempDir::new().expect("temp dir");
            let executable = stub(
                &dir,
                &format!("echo 'Failed to connect to 192.0.2.1 port 443' >&2\nexit {code}"),
            );
            let backend = CurlSpeedTestBackend::with_executable(executable, CLOUDFLARE_PROVIDER);

            let error = backend
                .run(&plan(1_000, 0), &CancellationToken::new(), &mut |_| {})
                .expect_err("a failing curl fails the run");

            assert_eq!(error.kind, kind, "exit code {code}");
            assert!(
                !error.message.contains("192.0.2.1"),
                "curl's stderr never reaches the message"
            );
        }
    }

    #[test]
    fn a_missing_curl_is_reported_as_unavailable() {
        let dir = TempDir::new().expect("temp dir");
        let backend = CurlSpeedTestBackend::with_executable(
            dir.path().join("missing-curl"),
            CLOUDFLARE_PROVIDER,
        );

        assert!(!backend.is_available());
        let error = backend
            .run(&plan(1_000, 0), &CancellationToken::new(), &mut |_| {})
            .expect_err("nothing can run without curl");
        assert_eq!(error.kind, SpeedTestErrorKind::Unavailable);
    }
}
