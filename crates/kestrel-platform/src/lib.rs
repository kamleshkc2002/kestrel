//! Narrow, dependency-light seams for future Linux session and OS adapters.
//!
//! This crate translates runtime facts into `kestrel-core` values. It does not
//! define product policy, own GTK widgets, or introduce a particular
//! display-server, D-Bus, portal, audio, or async-runtime implementation.

use kestrel_core::CapabilityReport;
pub mod applications;
pub mod audio;
pub mod clipboard;
pub mod launcher;

pub mod notifications;
pub mod quick_toggles;
pub mod snippets;
pub mod system_monitor;

/// How many times a spawn is retried when the executable is momentarily busy.
const SPAWN_ATTEMPTS: usize = 5;
const SPAWN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// True when `exec` failed because the binary is open for writing elsewhere.
///
/// A package manager replacing a provider binary produces exactly this window.
pub fn is_text_file_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ETXTBSY)
}

/// Spawns a command, retrying briefly while the executable is busy.
///
/// Every adapter that starts a resolved executable goes through this, so a
/// transient `ETXTBSY` becomes a successful run rather than a confusing failure.
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

/// Produces a non-interactive capability report for one feature adapter.
///
/// Concrete implementations may inspect a user session only when their
/// feature-specific Phase 1 work has selected and validated that adapter.
pub trait CapabilityProbe: Send {
    fn probe(&self) -> CapabilityReport;
}

/// A fixed report useful for composition tests and unsupported adapter paths.
#[derive(Debug, Clone)]
pub struct StaticCapabilityProbe {
    report: CapabilityReport,
}

impl StaticCapabilityProbe {
    /// Creates a probe that returns the supplied report without performing I/O.
    pub fn new(report: CapabilityReport) -> Self {
        Self { report }
    }
}

impl CapabilityProbe for StaticCapabilityProbe {
    fn probe(&self) -> CapabilityReport {
        self.report.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityProbe, StaticCapabilityProbe};
    use kestrel_core::{CapabilityReport, CapabilityStatus};

    #[test]
    fn static_probe_returns_a_copy_of_its_report() {
        let probe = StaticCapabilityProbe::new(CapabilityReport::new(
            "capture.screenshot",
            CapabilityStatus::Supported,
            "A test report.",
        ));

        assert_eq!(probe.probe().feature_id, "capture.screenshot");
    }
}
