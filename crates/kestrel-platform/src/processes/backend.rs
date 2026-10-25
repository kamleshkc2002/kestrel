use std::{
    collections::{HashMap, HashSet},
    env, fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use kestrel_core::{CapabilityReport, CapabilityStatus};

use super::*;
use crate::{CapabilityProbe, applications};

const PF_KTHREAD: u64 = 0x0020_0000;
const MAX_SCOPE_MEMBERS: usize = 1024;
const MAX_FDS: usize = 4096;

#[derive(Debug, Clone)]
pub struct ProcfsProcesses {
    proc_root: PathBuf,
    cgroup_root: PathBuf,
    passwd_file: Option<PathBuf>,
}

impl ProcfsProcesses {
    pub fn new(proc_root: impl AsRef<Path>) -> Self {
        Self {
            proc_root: proc_root.as_ref().to_path_buf(),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            passwd_file: None,
        }
    }

    pub fn with_cgroup_root(mut self, root: impl AsRef<Path>) -> Self {
        self.cgroup_root = root.as_ref().to_path_buf();
        self
    }

    pub fn with_passwd_file(mut self, path: impl AsRef<Path>) -> Self {
        self.passwd_file = Some(path.as_ref().to_path_buf());
        self
    }

    fn passwd_name(&self, uid: u32, cache: &mut HashMap<u32, Option<String>>) -> Option<String> {
        if let Some(name) = cache.get(&uid) {
            return name.clone();
        }
        let result = if let Some(path) = &self.passwd_file {
            fs::read_to_string(path).ok().and_then(|text| {
                text.lines().find_map(|line| {
                    let fields: Vec<&str> = line.split(':').collect();
                    (fields.len() > 2 && fields[2].parse::<u32>().ok() == Some(uid))
                        .then(|| bounded(fields[0]))
                })
            })
        } else {
            let mut buf = vec![0u8; 16 * 1024];
            let mut pwd = unsafe { std::mem::zeroed::<libc::passwd>() };
            let mut result_ptr = std::ptr::null_mut();
            let rc = unsafe {
                libc::getpwuid_r(
                    uid,
                    &mut pwd,
                    buf.as_mut_ptr() as *mut libc::c_char,
                    buf.len(),
                    &mut result_ptr,
                )
            };
            if rc == 0 && !result_ptr.is_null() && !pwd.pw_name.is_null() {
                Some(
                    unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
                        .to_string_lossy()
                        .chars()
                        .take(MAX_FIELD_CHARS)
                        .collect(),
                )
            } else {
                None
            }
        };
        cache.insert(uid, result.clone());
        result
    }

    fn read_sample_inner(
        &self,
        pid: u32,
        cache: &mut HashMap<u32, Option<String>>,
    ) -> Result<ProcessSample, ProcessError> {
        let dir = self.proc_root.join(pid.to_string());
        let stat = fs::read_to_string(dir.join("stat"))
            .map_err(|e| map_proc_error(e, ProcessErrorKind::Exited, "process exited"))?;
        let (name, fields) = parse_stat(&stat).ok_or_else(|| {
            ProcessError::new(
                ProcessErrorKind::Failed,
                "process metadata could not be parsed",
            )
        })?;
        let state = fields.first().and_then(|s| s.chars().next()).unwrap_or('?');
        let parent_pid = parse_u64(fields.get(1)).unwrap_or(0) as u32;
        let flags = parse_u64(fields.get(6)).unwrap_or(0);
        let cpu_ticks = parse_u64(fields.get(11))
            .unwrap_or(0)
            .saturating_add(parse_u64(fields.get(12)).unwrap_or(0));
        let threads = parse_u64(fields.get(17)).unwrap_or(0).min(u32::MAX as u64) as u32;
        let start_ticks = parse_u64(fields.get(19)).unwrap_or(0);
        let rss = parse_i64(fields.get(21)).unwrap_or(0).max(0) as u64;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        let uid = read_uid(&dir.join("status"));
        let owner = uid.and_then(|uid| self.passwd_name(uid, cache));
        let executable = fs::read_link(dir.join("exe"))
            .ok()
            .and_then(|path| path.file_name().map(|s| bounded(&s.to_string_lossy())));
        let cgroup = read_cgroup(&dir);
        let scope = cgroup
            .as_deref()
            .map(parse_scope)
            .unwrap_or(ProcessScope::Other);
        let scope_root = scope != ProcessScope::Other
            && read_cgroup(&self.proc_root.join(parent_pid.to_string())) != cgroup;
        Ok(ProcessSample {
            identity: ProcessIdentity { pid, start_ticks },
            parent_pid,
            name: bounded(&name),
            executable,
            uid,
            owner,
            kind: if flags & PF_KTHREAD != 0 {
                ProcessKind::KernelThread
            } else {
                ProcessKind::User
            },
            state,
            cpu_ticks,
            resident_bytes: rss.saturating_mul(page_size),
            threads,
            scope,
            scope_root,
        })
    }

    fn total_cpu(&self) -> (u64, u32) {
        let Ok(text) = fs::read_to_string(self.proc_root.join("stat")) else {
            return (0, 0);
        };
        let mut total = 0u64;
        let mut count = 0u32;
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(name) = fields.next() else { continue };
            if name == "cpu" {
                total = fields.filter_map(|v| v.parse::<u64>().ok()).sum();
            } else if name
                .strip_prefix("cpu")
                .is_some_and(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
            {
                count = count.saturating_add(1);
            }
        }
        (total, count)
    }

    pub fn eligibility(
        sample: &ProcessSample,
        current_uid: u32,
        own_pid: u32,
    ) -> Result<(), Ineligible> {
        if sample.kind == ProcessKind::KernelThread {
            return Err(Ineligible::KernelThread);
        }
        let Some(uid) = sample.uid else {
            return Err(Ineligible::OwnerUnknown);
        };
        if uid != current_uid {
            return Err(Ineligible::ForeignUser);
        }
        if sample.identity.pid == own_pid {
            return Err(Ineligible::OwnProcess);
        }
        if sample.identity.pid == 1 || (sample.name == "systemd" && sample.parent_pid == 1) {
            return Err(Ineligible::SessionManager);
        }
        Ok(())
    }

    /// Restart targets exist only for the process that started its scope;
    /// a descendant would restart its whole app.
    pub fn restart_target(sample: &ProcessSample) -> Option<RestartTarget> {
        if !sample.scope_root {
            return None;
        }
        match &sample.scope {
            ProcessScope::UserUnit { unit } => Some(RestartTarget::UserUnit { unit: unit.clone() }),
            ProcessScope::DesktopLaunch { desktop_id, scope } => Some(RestartTarget::DesktopApp {
                desktop_id: desktop_id.clone(),
                scope: scope.clone(),
            }),
            ProcessScope::Other => None,
        }
    }

    fn cgroup_path(
        &self,
        identity: ProcessIdentity,
    ) -> Result<(PathBuf, ProcessScope), ProcessError> {
        let sample = self.sample(identity)?;
        let text = fs::read_to_string(self.proc_root.join(identity.pid.to_string()).join("cgroup"))
            .map_err(|_| {
                ProcessError::new(
                    ProcessErrorKind::Refused,
                    "the process cgroup could not be read",
                )
            })?;
        let path = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap_or("");
        Ok((
            self.cgroup_root.join(path.trim_start_matches('/')),
            sample.scope,
        ))
    }
}

impl ProcessSource for ProcfsProcesses {
    fn scan(&self, limit: usize) -> Result<ProcessScan, ProcessError> {
        if !self.proc_root.is_dir() {
            return Err(ProcessError::new(
                ProcessErrorKind::Unavailable,
                "procfs is unavailable",
            ));
        }
        let cap = limit.min(MAX_SCANNED_PROCESSES);
        let mut pids = Vec::new();
        let Ok(entries) = fs::read_dir(&self.proc_root) else {
            return Err(ProcessError::new(
                ProcessErrorKind::Unavailable,
                "procfs could not be read",
            ));
        };
        for entry in entries.flatten() {
            if let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        let truncated = pids.len() > cap;
        let mut cache = HashMap::new();
        let mut samples = Vec::new();
        for pid in pids.into_iter().take(cap) {
            if let Ok(sample) = self.read_sample_inner(pid, &mut cache) {
                samples.push(sample);
            }
        }
        let (total_cpu_ticks, cpu_count) = self.total_cpu();
        Ok(ProcessScan {
            samples,
            total_cpu_ticks,
            cpu_count,
            truncated,
        })
    }

    fn sample(&self, identity: ProcessIdentity) -> Result<ProcessSample, ProcessError> {
        let mut cache = HashMap::new();
        let sample = self.read_sample_inner(identity.pid, &mut cache)?;
        if sample.identity.start_ticks != identity.start_ticks {
            return Err(ProcessError::new(
                ProcessErrorKind::Reused,
                "the process identity changed",
            ));
        }
        Ok(sample)
    }

    fn scope_members(
        &self,
        identity: ProcessIdentity,
    ) -> Result<Vec<ProcessIdentity>, ProcessError> {
        let (path, scope) = self.cgroup_path(identity)?;
        if matches!(scope, ProcessScope::Other) {
            return Err(ProcessError::new(
                ProcessErrorKind::Refused,
                "the process has no restartable scope",
            ));
        }
        let text = fs::read_to_string(path.join("cgroup.procs")).map_err(|_| {
            ProcessError::new(
                ProcessErrorKind::PermissionDenied,
                "scope members could not be read",
            )
        })?;
        let mut result = Vec::new();
        let mut cache = HashMap::new();
        for pid in text
            .lines()
            .take(MAX_SCOPE_MEMBERS)
            .filter_map(|line| line.trim().parse::<u32>().ok())
        {
            if let Ok(sample) = self.read_sample_inner(pid, &mut cache) {
                result.push(sample.identity);
            }
        }
        Ok(result)
    }
}

fn parse_stat(text: &str) -> Option<(String, Vec<&str>)> {
    let open = text.find('(')?;
    let close = text.rfind(") ")?;
    let name = text[open + 1..close].to_string();
    Some((name, text[close + 2..].split_whitespace().collect()))
}
fn parse_u64(value: Option<&&str>) -> Option<u64> {
    value?.parse().ok()
}
fn parse_i64(value: Option<&&str>) -> Option<i64> {
    value?.parse().ok()
}
fn bounded(value: &str) -> String {
    value.chars().take(MAX_FIELD_CHARS).collect()
}
fn read_cgroup(process_dir: &Path) -> Option<String> {
    fs::read_to_string(process_dir.join("cgroup"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
}
fn read_uid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}
fn map_proc_error(error: io::Error, default: ProcessErrorKind, message: &str) -> ProcessError {
    let kind = match error.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => ProcessErrorKind::PermissionDenied,
        Some(libc::ENOENT) => ProcessErrorKind::Exited,
        _ => default,
    };
    ProcessError::new(kind, message)
}

/// Launchers that prefix XDG app units (`app-<launcher>-<id>…`).
const KNOWN_LAUNCHERS: [&str; 6] = ["gnome", "kde", "cosmic", "flatpak", "xfce", "lxqt"];

/// Classifies a cgroup v2 path. Desktop launches follow systemd's XDG
/// naming: `app[-<launcher>]-<id>-<random>.scope` or
/// `app[-<launcher>]-<id>[@<random>].service`. User services sit under
/// `user@<uid>.service`; units in `session.slice` (compositor, shell) are
/// never offered for restart because restarting them ends the session.
fn parse_scope(path: &str) -> ProcessScope {
    let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some((component, desktop_id)) = components
        .iter()
        .rev()
        .find_map(|component| app_unit_id(component).map(|id| (*component, id)))
    {
        return ProcessScope::DesktopLaunch {
            desktop_id: bounded(&desktop_id),
            scope: bounded(component),
        };
    }
    let Some(manager) = components
        .iter()
        .position(|c| c.starts_with("user@") && c.ends_with(".service"))
    else {
        return ProcessScope::Other;
    };
    let mut slice = None;
    for component in &components[manager + 1..] {
        if component.ends_with(".slice") {
            slice = Some(*component);
        } else if component.ends_with(".service") {
            if slice == Some("session.slice") {
                return ProcessScope::Other;
            }
            return ProcessScope::UserUnit {
                unit: bounded(component),
            };
        } else {
            break;
        }
    }
    ProcessScope::Other
}

/// The desktop ID of an XDG app unit name, unescaped.
fn app_unit_id(component: &str) -> Option<String> {
    let rest = component.strip_prefix("app-")?;
    let body = match rest.strip_suffix(".scope") {
        Some(body) => body.rsplit_once('-')?.0,
        None => {
            let body = rest.strip_suffix(".service")?;
            body.split_once('@').map_or(body, |(id, _)| id)
        }
    };
    // IDs escape their own dashes, so unescaped dashes separate fields.
    let parts: Vec<&str> = body.split('-').filter(|part| !part.is_empty()).collect();
    let id = match parts.as_slice() {
        [] => return None,
        [id] => (*id).to_owned(),
        [launcher, rest @ ..] if KNOWN_LAUNCHERS.contains(launcher) => rest.join("-"),
        [_, id] => (*id).to_owned(),
        all => all.join("-"),
    };
    let id = unescape_systemd(&id);
    (!id.is_empty()).then_some(id)
}

/// Decodes systemd `\xNN` escapes byte-wise, so multi-byte UTF-8 survives.
fn unescape_systemd(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x') && i + 3 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Signals through a pidfd. The pidfd is opened first and the start time is
/// checked afterwards, so the signal can only reach the sampled process.
#[derive(Debug, Clone)]
pub struct PidfdSignaller {
    source: ProcfsProcesses,
    current_uid: u32,
    own_pid: u32,
}

impl PidfdSignaller {
    pub fn new(source: ProcfsProcesses, current_uid: u32, own_pid: u32) -> Self {
        Self {
            source,
            current_uid,
            own_pid,
        }
    }

    /// The real UID and PID of this process, for production signallers.
    pub fn for_current_user(source: ProcfsProcesses) -> Self {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        Self::new(source, uid, std::process::id())
    }

    fn open(identity: ProcessIdentity) -> Result<OwnedFd, ProcessError> {
        let pid = libc::pid_t::try_from(identity.pid)
            .map_err(|_| ProcessError::new(ProcessErrorKind::Failed, "invalid process ID"))?;
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(errno_error(last_errno()));
        }
        // SAFETY: the kernel returned a fresh descriptor this guard now owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }

    /// Whether the kernel supports `pidfd_open`; without it no action is offered.
    pub fn available() -> bool {
        let pid = std::process::id() as libc::pid_t;
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return false;
        }
        // SAFETY: the kernel returned a fresh descriptor; dropping it closes it.
        drop(unsafe { OwnedFd::from_raw_fd(fd as RawFd) });
        true
    }
}

impl ProcessSignaller for PidfdSignaller {
    fn signal(&self, identity: ProcessIdentity, signal: ProcessSignal) -> Result<(), ProcessError> {
        let fd = Self::open(identity)?;
        let sample = self.source.sample(identity)?;
        ProcfsProcesses::eligibility(&sample, self.current_uid, self.own_pid)
            .map_err(|why| ProcessError::new(ProcessErrorKind::Refused, why.explanation()))?;
        let number = match signal {
            ProcessSignal::Terminate => libc::SIGTERM,
            ProcessSignal::Kill => libc::SIGKILL,
        };
        // SAFETY: the fd is a live pidfd; a null siginfo and zero flags are valid.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                number,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(errno_error(last_errno()))
        }
    }

    fn wait_exit(
        &self,
        identity: ProcessIdentity,
        timeout: Duration,
    ) -> Result<bool, ProcessError> {
        let fd = match Self::open(identity) {
            Ok(fd) => fd,
            Err(error) if error.kind == ProcessErrorKind::Exited => return Ok(true),
            Err(error) => return Err(error),
        };
        // A different start time means the sampled process is already gone.
        match self.source.sample(identity) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind,
                    ProcessErrorKind::Exited | ProcessErrorKind::Reused
                ) =>
            {
                return Ok(true);
            }
            Err(error) => return Err(error),
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            let mut pollfd = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd entry for the duration of the call.
            let rc = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if rc > 0 {
                return Ok(pollfd.revents & libc::POLLIN != 0);
            }
            if rc == 0 {
                return Ok(false);
            }
            if last_errno() != libc::EINTR {
                return Err(ProcessError::new(
                    ProcessErrorKind::Failed,
                    "Waiting for the process to exit failed.",
                ));
            }
        }
    }
}

fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn errno_error(errno: i32) -> ProcessError {
    match errno {
        libc::ESRCH => ProcessError::new(ProcessErrorKind::Exited, "The process already exited."),
        libc::EPERM => ProcessError::new(
            ProcessErrorKind::PermissionDenied,
            "Only your own processes can be signalled; Kestrel uses no root helper.",
        ),
        libc::ENOSYS => ProcessError::new(ProcessErrorKind::Unavailable, NO_PIDFD_EXPLANATION),
        _ => ProcessError::new(ProcessErrorKind::Failed, "Sending the signal failed."),
    }
}

#[derive(Debug, Clone)]
pub struct SystemRestarter {
    systemctl: Option<PathBuf>,
}
impl Default for SystemRestarter {
    fn default() -> Self {
        Self::new()
    }
}
impl SystemRestarter {
    pub fn new() -> Self {
        Self {
            systemctl: applications::resolve_executable(
                "systemctl",
                env::var_os("PATH").as_deref(),
            ),
        }
    }
}
fn validate_unit_name(unit: &str) -> bool {
    unit.strip_suffix(".service").is_some_and(|base| {
        !base.is_empty()
            && base
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ":_.@-".contains(c))
    })
}
impl ProcessRestarter for SystemRestarter {
    fn restart(&self, target: &RestartTarget, timeout: Duration) -> Result<(), ProcessError> {
        match target {
            RestartTarget::UserUnit { unit } => {
                if !validate_unit_name(unit) {
                    return Err(ProcessError::new(
                        ProcessErrorKind::Refused,
                        "invalid service unit name",
                    ));
                }
                let Some(program) = &self.systemctl else {
                    return Err(ProcessError::new(
                        ProcessErrorKind::Unavailable,
                        "systemctl is unavailable",
                    ));
                };
                let mut child = Command::new(program)
                    .args(["--user", "restart", unit])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| {
                        ProcessError::new(
                            ProcessErrorKind::Unavailable,
                            "systemctl could not be started",
                        )
                    })?;
                let deadline = Instant::now() + timeout;
                loop {
                    if let Some(status) = child.try_wait().map_err(|_| {
                        ProcessError::new(
                            ProcessErrorKind::Failed,
                            "restart status could not be read",
                        )
                    })? {
                        return if status.success() {
                            Ok(())
                        } else {
                            Err(ProcessError::new(
                                ProcessErrorKind::Failed,
                                "service restart failed",
                            ))
                        };
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(ProcessError::new(
                            ProcessErrorKind::Timeout,
                            "service restart timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            RestartTarget::DesktopApp { desktop_id, .. } => {
                let roots = applications::application_directories(
                    env::var_os("XDG_DATA_HOME").as_deref(),
                    env::var_os("XDG_DATA_DIRS").as_deref(),
                    env::var_os("HOME").as_deref(),
                );
                let Some(entry) = applications::scan_applications(&roots)
                    .into_iter()
                    .find(|entry| entry.id == *desktop_id)
                else {
                    return Err(ProcessError::new(
                        ProcessErrorKind::Failed,
                        "desktop application was not found",
                    ));
                };
                applications::launch_desktop_entry(&entry).map_err(|_| {
                    ProcessError::new(
                        ProcessErrorKind::Failed,
                        "desktop application could not be launched",
                    )
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcfsListeners {
    proc_root: PathBuf,
}
impl ProcfsListeners {
    pub fn new(proc_root: impl AsRef<Path>) -> Self {
        Self {
            proc_root: proc_root.as_ref().to_path_buf(),
        }
    }
}
impl ProcfsListeners {
    pub fn listeners(&self, limit: usize) -> Result<Vec<Listener>, ProcessError> {
        let mut sockets = Vec::new();
        for (file, protocol, state) in [
            ("tcp", ListenerProtocol::Tcp, "0A"),
            ("tcp6", ListenerProtocol::Tcp, "0A"),
            ("udp", ListenerProtocol::Udp, "07"),
            ("udp6", ListenerProtocol::Udp, "07"),
        ] {
            let Ok(text) = fs::read_to_string(self.proc_root.join("net").join(file)) else {
                continue;
            };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() < 10 || fields[3] != state {
                    continue;
                }
                let Some((address, port)) = parse_address(fields[1]) else {
                    continue;
                };
                let inode = fields[9].parse::<u64>().unwrap_or(0);
                sockets.push((protocol, address, port, inode));
            }
        }
        if sockets.is_empty() && !self.proc_root.join("net/tcp").exists() {
            return Err(ProcessError::new(
                ProcessErrorKind::Unavailable,
                "procfs network tables are unavailable",
            ));
        }
        let owners = self.socket_owners();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (protocol, address, port, inode) in sockets {
            if !seen.insert((protocol, address, port)) {
                continue;
            }
            let owner = owners.get(&inode).cloned();
            out.push(Listener {
                protocol,
                address,
                port,
                owner: owner.as_ref().map(|v| v.0),
                owner_name: owner.and_then(|v| v.1),
            });
        }
        out.sort_by_key(|v| (v.protocol, v.port, v.address));
        out.truncate(limit.min(MAX_LISTENERS));
        Ok(out)
    }
}
impl ProcfsListeners {
    fn socket_owners(&self) -> HashMap<u64, (ProcessIdentity, Option<String>)> {
        let mut map = HashMap::new();
        let Ok(entries) = fs::read_dir(&self.proc_root) else {
            return map;
        };
        let mut pids = entries
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
            .collect::<Vec<_>>();
        pids.sort_unstable();
        for pid in pids.into_iter().take(MAX_SCANNED_PROCESSES) {
            let Some(identity) = read_identity(&self.proc_root, pid) else {
                continue;
            };
            let name = read_name(&self.proc_root, pid);
            let Ok(fds) = fs::read_dir(self.proc_root.join(pid.to_string()).join("fd")) else {
                continue;
            };
            for fd in fds.flatten().take(MAX_FDS) {
                let Ok(link) = fs::read_link(fd.path()) else {
                    continue;
                };
                let Some(inode) = link
                    .to_string_lossy()
                    .strip_prefix("socket:[")
                    .and_then(|x| x.strip_suffix(']'))
                    .and_then(|x| x.parse().ok())
                else {
                    continue;
                };
                map.entry(inode).or_insert((identity, name.clone()));
            }
        }
        map
    }
}
fn read_identity(root: &Path, pid: u32) -> Option<ProcessIdentity> {
    let stat = fs::read_to_string(root.join(pid.to_string()).join("stat")).ok()?;
    let (_, f) = parse_stat(&stat)?;
    Some(ProcessIdentity {
        pid,
        start_ticks: parse_u64(f.get(19))?,
    })
}
fn read_name(root: &Path, pid: u32) -> Option<String> {
    let stat = fs::read_to_string(root.join(pid.to_string()).join("stat")).ok()?;
    Some(bounded(&parse_stat(&stat)?.0))
}
fn parse_address(value: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    if addr.len() == 8 {
        let raw = u32::from_str_radix(addr, 16).ok()?;
        Some((IpAddr::V4(Ipv4Addr::from(raw.to_le_bytes())), port))
    } else if addr.len() == 32 {
        let mut bytes = [0u8; 16];
        for i in 0..4 {
            let word = u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16).ok()?;
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        Some((IpAddr::V6(Ipv6Addr::from(bytes)), port))
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ProcessProbe {
    source: ProcfsProcesses,
}
impl ProcessProbe {
    pub fn new(proc_root: impl AsRef<Path>) -> Self {
        Self {
            source: ProcfsProcesses::new(proc_root),
        }
    }
}
impl Default for ProcessProbe {
    fn default() -> Self {
        Self::new(PathBuf::from("/proc"))
    }
}
impl CapabilityProbe for ProcessProbe {
    fn probe(&self) -> CapabilityReport {
        if !self.source.proc_root.join("self/stat").is_file() {
            return CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "procfs is unavailable".into(),
                },
                "Process inspection is unavailable.",
            );
        }
        if PidfdSignaller::available() {
            return CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Supported,
                "Process inspection and identity-checked actions are available.",
            );
        }
        CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Limited {
                reason: "pidfd is unavailable".into(),
            },
            "Process inspection is available; actions are hidden without pidfd support.",
        )
        .with_remediation("Use a Linux kernel with pidfd_open support.")
    }
}
#[derive(Debug, Clone)]
pub struct ListenersProbe {
    proc_root: PathBuf,
}
impl ListenersProbe {
    pub fn new(proc_root: impl AsRef<Path>) -> Self {
        Self {
            proc_root: proc_root.as_ref().to_path_buf(),
        }
    }
}
impl Default for ListenersProbe {
    fn default() -> Self {
        Self::new(PathBuf::from("/proc"))
    }
}
impl CapabilityProbe for ListenersProbe {
    fn probe(&self) -> CapabilityReport {
        if self.proc_root.join("net/tcp").is_file() {
            CapabilityReport::new(
                LISTENERS_FEATURE_ID,
                CapabilityStatus::Supported,
                "Listening ports are available.",
            )
        } else {
            CapabilityReport::new(
                LISTENERS_FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "procfs network tables are unavailable".into(),
                },
                "Listening ports are unavailable.",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::process::Child;

    use tempfile::TempDir;

    use super::*;

    struct FixtureProcess<'a> {
        pid: u32,
        comm: &'a str,
        ppid: u32,
        flags: u64,
        start: u64,
        uid: Option<u32>,
        cgroup: &'a str,
        exe: Option<&'a str>,
    }

    impl FixtureProcess<'_> {
        fn user(pid: u32, comm: &str) -> FixtureProcess<'_> {
            FixtureProcess {
                pid,
                comm,
                ppid: 1,
                flags: 0,
                start: u64::from(pid) * 10,
                uid: Some(1000),
                cgroup: "/user.slice/user-1000.slice/session-2.scope",
                exe: None,
            }
        }

        fn write(&self, root: &Path) {
            let dir = root.join(self.pid.to_string());
            fs::create_dir_all(dir.join("fd")).expect("process dir");
            // utime 7, stime 5, 3 threads, 4 resident pages.
            fs::write(
                dir.join("stat"),
                format!(
                    "{} ({}) S {} 1 1 0 -1 {} 0 0 0 0 7 5 0 0 20 0 3 0 {} 1000 4 0\n",
                    self.pid, self.comm, self.ppid, self.flags, self.start
                ),
            )
            .expect("stat");
            if let Some(uid) = self.uid {
                fs::write(
                    dir.join("status"),
                    format!("Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
                )
                .expect("status");
            }
            fs::write(dir.join("cgroup"), format!("0::{}\n", self.cgroup)).expect("cgroup");
            if let Some(exe) = self.exe {
                symlink(exe, dir.join("exe")).expect("exe link");
            }
        }
    }

    fn fixture_root() -> TempDir {
        let root = TempDir::new().expect("temp dir");
        fs::write(
            root.path().join("stat"),
            "cpu  10 20 30 40\ncpu0 5 10 15 20\ncpu1 5 10 15 20\nintr 1\n",
        )
        .expect("proc stat");
        fs::write(
            root.path().join("passwd"),
            "root:x:0:0::/root:/bin/sh\nalex:x:1000:1000::/home/alex:/bin/sh\n",
        )
        .expect("passwd");
        root
    }

    fn source(root: &TempDir) -> ProcfsProcesses {
        ProcfsProcesses::new(root.path()).with_passwd_file(root.path().join("passwd"))
    }

    fn page_size() -> u64 {
        // SAFETY: sysconf has no preconditions.
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64
    }

    #[test]
    fn scan_reads_identity_owner_kind_and_resources() {
        let root = fixture_root();
        FixtureProcess {
            exe: Some("/usr/lib/firefox/firefox-bin"),
            ..FixtureProcess::user(100, "my (odd) name")
        }
        .write(root.path());
        FixtureProcess {
            flags: 0x0020_0000,
            ppid: 2,
            uid: Some(0),
            ..FixtureProcess::user(20, "kworker/0:1")
        }
        .write(root.path());
        FixtureProcess {
            uid: None,
            ..FixtureProcess::user(300, "hidden")
        }
        .write(root.path());
        fs::create_dir(root.path().join("self")).expect("non-numeric entry");

        let scan = source(&root).scan(MAX_SCANNED_PROCESSES).expect("scan");

        assert_eq!(
            (scan.total_cpu_ticks, scan.cpu_count, scan.truncated),
            (100, 2, false)
        );
        let by_pid = |pid: u32| {
            scan.samples
                .iter()
                .find(|sample| sample.identity.pid == pid)
                .expect("sampled")
        };
        let odd = by_pid(100);
        assert_eq!(
            odd.name, "my (odd) name",
            "comm is read up to the last parenthesis"
        );
        assert_eq!(odd.identity.start_ticks, 1000);
        assert_eq!(odd.executable.as_deref(), Some("firefox-bin"));
        assert_eq!(odd.owner.as_deref(), Some("alex"));
        assert_eq!(odd.kind, ProcessKind::User);
        assert_eq!((odd.cpu_ticks, odd.threads), (12, 3));
        assert_eq!(odd.resident_bytes, 4 * page_size());
        assert_eq!(by_pid(20).kind, ProcessKind::KernelThread);
        assert_eq!(by_pid(20).owner.as_deref(), Some("root"));
        assert_eq!(
            (by_pid(300).uid, by_pid(300).owner.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn scan_caps_the_process_count_and_bounds_names() {
        let root = fixture_root();
        let long = "n".repeat(300);
        for pid in 10..15 {
            FixtureProcess::user(pid, &long).write(root.path());
        }

        let scan = source(&root).scan(3).expect("scan");

        assert!(scan.truncated);
        assert_eq!(scan.samples.len(), 3);
        assert!(
            scan.samples
                .iter()
                .all(|sample| sample.name.chars().count() == MAX_FIELD_CHARS)
        );
    }

    #[test]
    fn sampling_tells_exited_and_reused_processes_apart() {
        let root = fixture_root();
        FixtureProcess::user(42, "worker").write(root.path());
        let source = source(&root);

        let live = ProcessIdentity {
            pid: 42,
            start_ticks: 420,
        };
        assert_eq!(source.sample(live).expect("same process").name, "worker");
        let reused = ProcessIdentity {
            pid: 42,
            start_ticks: 7,
        };
        assert_eq!(
            source.sample(reused).unwrap_err().kind,
            ProcessErrorKind::Reused
        );
        let gone = ProcessIdentity {
            pid: 43,
            start_ticks: 430,
        };
        assert_eq!(
            source.sample(gone).unwrap_err().kind,
            ProcessErrorKind::Exited
        );
    }

    #[test]
    fn scopes_follow_systemd_unit_and_xdg_app_naming() {
        let unit = |unit: &str| ProcessScope::UserUnit { unit: unit.into() };
        let app = |id: &str, scope: &str| ProcessScope::DesktopLaunch {
            desktop_id: id.into(),
            scope: scope.into(),
        };
        let base = "/user.slice/user-1000.slice/user@1000.service";
        for (path, expected) in [
            (
                format!("{base}/app.slice/pipewire.service"),
                unit("pipewire.service"),
            ),
            (
                format!("{base}/background.slice/tracker-miner-fs-3.service"),
                unit("tracker-miner-fs-3.service"),
            ),
            (
                format!("{base}/session.slice/org.gnome.Shell@wayland.service"),
                ProcessScope::Other,
            ),
            (format!("{base}/init.scope"), ProcessScope::Other),
            (
                format!("{base}/app.slice/app-gnome-org.gnome.Terminal-4242.scope"),
                app(
                    "org.gnome.Terminal",
                    "app-gnome-org.gnome.Terminal-4242.scope",
                ),
            ),
            (
                format!("{base}/app.slice/app-org.kde.dolphin@a1b2c3.service"),
                app("org.kde.dolphin", "app-org.kde.dolphin@a1b2c3.service"),
            ),
            (
                format!("{base}/app.slice/app-flatpak-org.mozilla.firefox-12.scope"),
                app(
                    "org.mozilla.firefox",
                    "app-flatpak-org.mozilla.firefox-12.scope",
                ),
            ),
            (
                format!("{base}/app.slice/app-gnome-google\\x2dchrome-9f.scope"),
                app("google-chrome", "app-gnome-google\\x2dchrome-9f.scope"),
            ),
            (
                format!("{base}/app.slice/app-kde-caf\\xc3\\xa9-1.scope"),
                app("café", "app-kde-caf\\xc3\\xa9-1.scope"),
            ),
            (
                "/user.slice/user-1000.slice/session-2.scope".to_owned(),
                ProcessScope::Other,
            ),
            ("/system.slice/cron.service".to_owned(), ProcessScope::Other),
        ] {
            assert_eq!(parse_scope(&path), expected, "{path}");
        }
    }

    #[test]
    fn eligibility_refuses_kernel_foreign_unknown_own_and_session_processes() {
        let sample =
            |pid: u32, name: &str, ppid: u32, uid: Option<u32>, kind: ProcessKind| ProcessSample {
                identity: ProcessIdentity {
                    pid,
                    start_ticks: 1,
                },
                parent_pid: ppid,
                name: name.into(),
                executable: None,
                uid,
                owner: None,
                kind,
                state: 'S',
                cpu_ticks: 0,
                resident_bytes: 0,
                threads: 1,
                scope: ProcessScope::Other,
                scope_root: false,
            };
        let check = |sample: ProcessSample| ProcfsProcesses::eligibility(&sample, 1000, 77);
        let user = ProcessKind::User;
        assert_eq!(check(sample(500, "editor", 400, Some(1000), user)), Ok(()));
        assert_eq!(
            check(sample(
                2,
                "kthreadd",
                0,
                Some(1000),
                ProcessKind::KernelThread
            )),
            Err(Ineligible::KernelThread)
        );
        assert_eq!(
            check(sample(500, "editor", 400, None, user)),
            Err(Ineligible::OwnerUnknown)
        );
        assert_eq!(
            check(sample(500, "sshd", 1, Some(0), user)),
            Err(Ineligible::ForeignUser)
        );
        assert_eq!(
            check(sample(77, "kestrel", 400, Some(1000), user)),
            Err(Ineligible::OwnProcess)
        );
        assert_eq!(
            check(sample(1, "systemd", 0, Some(1000), user)),
            Err(Ineligible::SessionManager)
        );
        assert_eq!(
            check(sample(900, "systemd", 1, Some(1000), user)),
            Err(Ineligible::SessionManager),
            "the user's service manager"
        );
    }

    #[test]
    fn scope_members_come_from_the_cgroup_and_only_the_root_restarts() {
        let root = fixture_root();
        let cgroups = TempDir::new().expect("cgroup root");
        let scope_path = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.example.App-1.scope";
        // 51 is a child 50 spawned inside the same scope.
        for (pid, ppid) in [(50, 1), (51, 50)] {
            FixtureProcess {
                cgroup: scope_path,
                ppid,
                ..FixtureProcess::user(pid, "app")
            }
            .write(root.path());
        }
        FixtureProcess::user(60, "loose").write(root.path());
        let scope_dir = cgroups.path().join(scope_path.trim_start_matches('/'));
        fs::create_dir_all(&scope_dir).expect("scope dir");
        // 52 exited after the cgroup was read.
        fs::write(scope_dir.join("cgroup.procs"), "50\n51\n52\n").expect("procs");
        let source = source(&root).with_cgroup_root(cgroups.path());

        let members = source
            .scope_members(ProcessIdentity {
                pid: 50,
                start_ticks: 500,
            })
            .expect("members");
        assert_eq!(
            members,
            vec![
                ProcessIdentity {
                    pid: 50,
                    start_ticks: 500
                },
                ProcessIdentity {
                    pid: 51,
                    start_ticks: 510
                },
            ]
        );
        assert_eq!(
            source
                .scope_members(ProcessIdentity {
                    pid: 60,
                    start_ticks: 600
                })
                .unwrap_err()
                .kind,
            ProcessErrorKind::Refused
        );
        let target = |pid: u32| {
            let identity = ProcessIdentity {
                pid,
                start_ticks: u64::from(pid) * 10,
            };
            ProcfsProcesses::restart_target(&source.sample(identity).expect("sample"))
        };
        assert!(matches!(
            target(50),
            Some(RestartTarget::DesktopApp { desktop_id, .. }) if desktop_id == "org.example.App"
        ));
        assert_eq!(target(51), None, "a descendant would restart its whole app");
    }

    #[test]
    fn listeners_keep_listening_sockets_and_resolve_their_owner() {
        let root = fixture_root();
        FixtureProcess::user(70, "devserver").write(root.path());
        symlink("socket:[555]", root.path().join("70/fd/3")).expect("socket fd");
        symlink("/dev/null", root.path().join("70/fd/0")).expect("plain fd");
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        let net = root.path().join("net");
        fs::create_dir(&net).expect("net dir");
        fs::write(
            net.join("tcp"),
            format!(
                "{header}   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1\n   1: 0100007F:A000 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 556 1\n"
            ),
        )
        .expect("tcp");
        fs::write(
            net.join("tcp6"),
            format!("{header}   0: 0000000000000000FFFF00000100007F:0016 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 777 1\n"),
        )
        .expect("tcp6");
        fs::write(
            net.join("udp"),
            format!("{header}   0: 00000000:14E9 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 888 2\n"),
        )
        .expect("udp");

        let listeners = ProcfsListeners::new(root.path())
            .listeners(MAX_LISTENERS)
            .expect("listeners");

        let summary: Vec<_> = listeners
            .iter()
            .map(|listener| {
                (
                    listener.protocol,
                    listener.address,
                    listener.port,
                    listener.owner.map(|owner| owner.pid),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    ListenerProtocol::Tcp,
                    "::ffff:127.0.0.1".parse().unwrap(),
                    22,
                    None
                ),
                (
                    ListenerProtocol::Tcp,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    8080,
                    Some(70)
                ),
                (
                    ListenerProtocol::Udp,
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    5353,
                    None
                ),
            ],
            "established sockets are excluded"
        );
        assert_eq!(listeners[1].owner_name.as_deref(), Some("devserver"));
    }

    #[test]
    fn unit_names_require_service_suffix_and_safe_characters() {
        for (name, valid) in [
            ("org.example.Service.service", true),
            ("foo@bar.service", true),
            ("foo", false),
            (".service", false),
            ("foo/bar.service", false),
            ("foo bar.service", false),
        ] {
            assert_eq!(validate_unit_name(name), valid, "{name}");
        }
    }

    fn pidfd_available() -> bool {
        PidfdSignaller::open(ProcessIdentity {
            pid: std::process::id(),
            start_ticks: 0,
        })
        .is_ok()
    }

    fn spawn_sleeper() -> (Child, ProcessIdentity) {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let identity = read_identity(Path::new("/proc"), child.id()).expect("child identity");
        (child, identity)
    }

    #[test]
    fn live_terminate_reaches_an_own_child_and_wait_sees_it_exit() {
        if !pidfd_available() {
            eprintln!("skipping: pidfd is unavailable");
            return;
        }
        let (mut child, identity) = spawn_sleeper();
        let signaller = PidfdSignaller::for_current_user(ProcfsProcesses::new("/proc"));

        signaller
            .signal(identity, ProcessSignal::Terminate)
            .expect("own child is eligible");

        assert_eq!(
            signaller.wait_exit(identity, Duration::from_secs(2)),
            Ok(true)
        );
        let _ = child.wait();
        assert_eq!(
            signaller.wait_exit(identity, Duration::from_millis(10)),
            Ok(true),
            "a reaped process counts as exited"
        );
    }

    #[test]
    fn live_reused_identity_is_refused_and_the_process_survives() {
        if !pidfd_available() {
            eprintln!("skipping: pidfd is unavailable");
            return;
        }
        let (mut child, identity) = spawn_sleeper();
        let signaller = PidfdSignaller::for_current_user(ProcfsProcesses::new("/proc"));
        let stale = ProcessIdentity {
            start_ticks: identity.start_ticks + 1,
            ..identity
        };

        let error = signaller.signal(stale, ProcessSignal::Kill).unwrap_err();

        assert_eq!(error.kind, ProcessErrorKind::Reused);
        assert!(
            child.try_wait().expect("child status").is_none(),
            "no signal was sent"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn live_init_is_refused_before_any_signal() {
        if !pidfd_available() {
            eprintln!("skipping: pidfd is unavailable");
            return;
        }
        let Some(init) = read_identity(Path::new("/proc"), 1) else {
            eprintln!("skipping: /proc/1 is not visible");
            return;
        };
        let signaller = PidfdSignaller::for_current_user(ProcfsProcesses::new("/proc"));

        let error = signaller
            .signal(init, ProcessSignal::Terminate)
            .unwrap_err();

        assert_eq!(error.kind, ProcessErrorKind::Refused);
    }

    #[test]
    fn live_listener_reports_this_process_as_owner() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = socket.local_addr().expect("address").port();

        let listeners = ProcfsListeners::new("/proc")
            .listeners(MAX_LISTENERS)
            .expect("listeners");

        let ours = listeners
            .iter()
            .find(|listener| listener.protocol == ListenerProtocol::Tcp && listener.port == port)
            .expect("bound socket is listed");
        assert_eq!(ours.owner.map(|owner| owner.pid), Some(std::process::id()));
    }
}
