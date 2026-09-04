//! Narrow, dependency-light seams for future Linux session and OS adapters.
//!
//! This crate translates runtime facts into `kestrel-core` values. It does not
//! define product policy, own GTK widgets, or introduce a particular
//! display-server, D-Bus, portal, audio, or async-runtime implementation.

use kestrel_core::CapabilityReport;

pub mod system_monitor;

/// Produces a non-interactive capability report for one feature adapter.
///
/// Concrete implementations may inspect a user session only when their
/// feature-specific Phase 1 work has selected and validated that adapter.
pub trait CapabilityProbe {
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
