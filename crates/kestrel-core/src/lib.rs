//! UI-agnostic primitives shared by Kestrel services and front ends.

/// The runtime status of a feature on the current Linux session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// The feature can run without additional setup.
    Supported,
    /// The feature runs with documented limitations.
    Limited { reason: String },
    /// The feature requires a portal grant, udev rule, or other permission.
    NeedsPermission { permission: Permission },
    /// The feature requires an optional executable, service, or library.
    MissingDependency { name: String },
    /// The desktop, compositor, hardware, or package mode cannot support the feature.
    Unsupported { reason: String },
}

/// A user-facing category of permission or access requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ScreenCapture,
    GlobalShortcut,
    InputInjection,
    HardwareControl,
    Camera,
    Notifications,
}
/// Non-sensitive evidence that supports a capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    pub key: String,
    pub value: String,
}

impl CapabilityEvidence {
    /// Builds evidence from a stable key and a sanitized value.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// A structured, UI-agnostic explanation of a feature's current availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityReport {
    pub feature_id: &'static str,
    pub status: CapabilityStatus,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub alternatives_considered: Vec<String>,
    pub remediation: Option<String>,
    pub evidence: Vec<CapabilityEvidence>,
}

impl CapabilityReport {
    /// Builds a report from the feature's current status and user-facing summary.
    pub fn new(
        feature_id: &'static str,
        status: CapabilityStatus,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            feature_id,
            status,
            summary: summary.into(),
            selected_backend: None,
            alternatives_considered: Vec::new(),
            remediation: None,
            evidence: Vec::new(),
        }
    }

    /// Adds the adapter actually selected for this report.
    pub fn with_selected_backend(mut self, backend: impl Into<String>) -> Self {
        self.selected_backend = Some(backend.into());
        self
    }

    /// Adds a user-facing action that can address a limited capability.
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }

    /// Records an alternative that was considered but not selected.
    pub fn with_alternative(mut self, alternative: impl Into<String>) -> Self {
        self.alternatives_considered.push(alternative.into());
        self
    }

    /// Records sanitized evidence without performing any I/O.
    pub fn with_evidence(mut self, evidence: CapabilityEvidence) -> Self {
        self.evidence.push(evidence);
        self
    }
}

/// Metadata that every independently enabled Kestrel feature must provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub capability: CapabilityStatus,
}

impl FeatureSpec {
    /// Builds a feature descriptor whose runtime status is supplied by a backend probe.
    pub fn new(id: &'static str, label: &'static str, capability: CapabilityStatus) -> Self {
        Self {
            id,
            label,
            capability,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityEvidence, CapabilityReport, CapabilityStatus, FeatureSpec};

    #[test]
    fn feature_spec_preserves_its_capability_status() {
        let feature = FeatureSpec::new("audio.mixer", "Audio mixer", CapabilityStatus::Supported);

        assert_eq!(feature.id, "audio.mixer");
        assert_eq!(feature.capability, CapabilityStatus::Supported);
    }

    #[test]
    fn capability_report_preserves_status_and_remediation() {
        let report = CapabilityReport::new(
            "clipboard.history",
            CapabilityStatus::Limited {
                reason: "History is disabled by default.".to_string(),
            },
            "Clipboard history requires opt-in.",
        )
        .with_selected_backend("Wayland data-control")
        .with_remediation("Enable history after configuring retention bounds.")
        .with_alternative("X11 selection")
        .with_evidence(CapabilityEvidence::new("clipboard_content_read", "false"));

        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert_eq!(
            report.remediation.as_deref(),
            Some("Enable history after configuring retention bounds.")
        );
        assert_eq!(report.evidence[0].key, "clipboard_content_read");
    }
}
