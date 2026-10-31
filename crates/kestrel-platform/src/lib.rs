//! Dependency-light seams for Linux session and OS adapters.
//! Translates runtime facts into `kestrel-core` values while leaving product policy,
//! UI, display server, bus, portal, audio, and async runtime to callers.

use kestrel_core::CapabilityReport;
pub mod applications;
pub mod audio;
pub mod camera;
pub mod capture;
pub mod clipboard;
pub mod global_shortcuts;
pub mod launcher;
pub mod logind_session;
pub mod network_addresses;
pub mod notifications;
pub mod nvml;
pub mod processes;
pub mod quick_toggles;
pub mod recording;
pub mod screen_tools;
pub mod snippets;
pub mod speed_test;
pub mod system_monitor;
#[cfg(test)]
mod test_bus;
pub mod tray_host;
pub mod upower;
pub mod windows;

/// Number of retries while an executable is momentarily busy.
const SPAWN_ATTEMPTS: usize = 5;
const SPAWN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether `exec` failed because another process is writing the binary.
pub fn is_text_file_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ETXTBSY)
}

/// Spawns a command, retrying briefly on `ETXTBSY`.
pub(crate) fn spawn_with_busy_retry(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if is_text_file_busy(&error) && attempt < SPAWN_ATTEMPTS => {
                std::thread::sleep(SPAWN_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Produces a non-interactive capability report for one adapter.
///
/// Session inspection occurs only after feature-specific adapter validation.
pub trait CapabilityProbe: Send {
    fn probe(&self) -> CapabilityReport;
}

/// Fixed report for composition tests and unsupported adapters.
#[derive(Debug, Clone)]
pub struct StaticCapabilityProbe {
    report: CapabilityReport,
}

impl StaticCapabilityProbe {
    pub fn new(report: CapabilityReport) -> Self {
        Self { report }
    }
}

impl CapabilityProbe for StaticCapabilityProbe {
    fn probe(&self) -> CapabilityReport {
        self.report.clone()
    }
}
