//! Process inventory, identity-checked signals, restart targets, and
//! listening sockets, all read from procfs for the current user.

mod backend;

pub use backend::{
    ListenersProbe, PidfdSignaller, ProcessProbe, ProcfsListeners, ProcfsProcesses, SystemRestarter,
};

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

pub const FEATURE_ID: &str = "system.processes";
pub const LISTENERS_FEATURE_ID: &str = "network.listeners";

/// Most `/proc/<pid>` entries one scan reads.
pub const MAX_SCANNED_PROCESSES: usize = 8192;
/// Longest name, executable, owner, or unit string kept in a sample.
pub const MAX_FIELD_CHARS: usize = 128;
/// Most listening sockets one refresh reports.
pub const MAX_LISTENERS: usize = 1024;
/// Why no process action is offered when `pidfd_open` is missing.
pub const NO_PIDFD_EXPLANATION: &str =
    "Process actions are unavailable: this kernel has no pidfd support.";

/// A process as it existed when sampled. A PID alone can be reused; the
/// start time (field 22 of `/proc/<pid>/stat`, in clock ticks since boot)
/// tells reused PIDs apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessKind {
    User,
    /// `PF_KTHREAD`; never signalled or restarted.
    KernelThread,
}

/// Where a process runs, from its cgroup v2 path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessScope {
    /// A systemd user service, e.g. `pipewire.service`.
    UserUnit { unit: String },
    /// An app launched by the desktop into its own scope
    /// (`app-<launcher>-<desktop id>-<n>.scope`).
    DesktopLaunch { desktop_id: String, scope: String },
    /// Any other cgroup, or no readable cgroup.
    Other,
}

/// One process; command lines and environment are never read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSample {
    pub identity: ProcessIdentity,
    pub parent_pid: u32,
    /// `comm`, bounded.
    pub name: String,
    /// File name of `/proc/<pid>/exe` when readable, bounded.
    pub executable: Option<String>,
    /// Real UID; `None` when the status file was unreadable.
    pub uid: Option<u32>,
    /// User name for `uid`, bounded.
    pub owner: Option<String>,
    pub kind: ProcessKind,
    /// Single-letter state from `stat` (R, S, D, Z, …).
    pub state: char,
    /// utime + stime in clock ticks.
    pub cpu_ticks: u64,
    pub resident_bytes: u64,
    pub threads: u32,
    pub scope: ProcessScope,
    /// The parent runs in another cgroup, so this process started the
    /// scope; only roots offer a restart.
    pub scope_root: bool,
}

/// One pass over `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessScan {
    pub samples: Vec<ProcessSample>,
    /// Sum of all CPU time in `/proc/stat`, for usage percentages.
    pub total_cpu_ticks: u64,
    /// Online CPUs, for per-process percentages.
    pub cpu_count: u32,
    /// More processes existed than were read.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessErrorKind {
    /// procfs or a needed interface is missing.
    Unavailable,
    PermissionDenied,
    /// The process exited.
    Exited,
    /// The PID now belongs to a different process.
    Reused,
    /// Kestrel declines this target (see [`Ineligible`]).
    Refused,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessError {
    pub kind: ProcessErrorKind,
    /// Kestrel-authored; no command lines, environment, or paths beyond
    /// executable names.
    pub message: String,
}

impl ProcessError {
    pub fn new(kind: ProcessErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProcessError {}

/// Why actions are unavailable for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    KernelThread,
    ForeignUser,
    OwnerUnknown,
    /// Kestrel itself.
    OwnProcess,
    /// PID 1 or the user's systemd manager.
    SessionManager,
}

impl Ineligible {
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::KernelThread => "Kernel threads cannot be signalled.",
            Self::ForeignUser => {
                "This process belongs to another user; only your own processes can be signalled."
            }
            Self::OwnerUnknown => {
                "Ownership could not be read; refresh, and check that /proc is mounted normally."
            }
            Self::OwnProcess => "This is the app's own process; quit it from the window or tray.",
            Self::SessionManager => "Stopping this process would end your session.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    Terminate,
    Kill,
}

/// A restart Kestrel can perform with a known start method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartTarget {
    /// `systemctl --user restart <unit>`.
    UserUnit { unit: String },
    /// Stop the scope's processes, then launch the desktop entry again.
    DesktopApp { desktop_id: String, scope: String },
}

/// Reads processes. Implementations bound every read.
pub trait ProcessSource: Send {
    fn scan(&self, limit: usize) -> Result<ProcessScan, ProcessError>;
    /// Re-reads one process; `Reused` when the start time differs and
    /// `Exited` when it is gone.
    fn sample(&self, identity: ProcessIdentity) -> Result<ProcessSample, ProcessError>;
    /// PIDs in the same cgroup as `identity`, bounded.
    fn scope_members(
        &self,
        identity: ProcessIdentity,
    ) -> Result<Vec<ProcessIdentity>, ProcessError>;
}

/// Sends signals only to a revalidated identity.
pub trait ProcessSignaller: Send {
    fn signal(&self, identity: ProcessIdentity, signal: ProcessSignal) -> Result<(), ProcessError>;
    /// `Ok(true)` once the process has exited, `Ok(false)` at the timeout.
    fn wait_exit(&self, identity: ProcessIdentity, timeout: Duration)
    -> Result<bool, ProcessError>;
}

/// Starts a restart target again after its processes stopped.
pub trait ProcessRestarter: Send {
    fn restart(&self, target: &RestartTarget, timeout: Duration) -> Result<(), ProcessError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ListenerProtocol {
    Tcp,
    Udp,
}

/// A listening TCP socket or a bound UDP socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub protocol: ListenerProtocol,
    pub address: IpAddr,
    pub port: u16,
    /// Owning process when its file descriptors were readable.
    pub owner: Option<ProcessIdentity>,
    pub owner_name: Option<String>,
}
