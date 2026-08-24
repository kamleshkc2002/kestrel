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

/// Metadata that every independently enabled Kestrel feature must provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub capability: CapabilityStatus,
}

impl FeatureSpec {
    /// Builds a feature descriptor whose runtime status is supplied by a backend probe.
    pub fn new(
        id: &'static str,
        label: &'static str,
        capability: CapabilityStatus,
    ) -> Self {
        Self {
            id,
            label,
            capability,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityStatus, FeatureSpec};

    #[test]
    fn feature_spec_preserves_its_capability_status() {
        let feature = FeatureSpec::new(
            "audio.mixer",
            "Audio mixer",
            CapabilityStatus::Supported,
        );

        assert_eq!(feature.id, "audio.mixer");
        assert_eq!(feature.capability, CapabilityStatus::Supported);
    }
}
