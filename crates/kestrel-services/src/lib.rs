//! Feature registry and lifecycle primitives.
//!
//! Services combine feature policy with `kestrel-platform` probes and publish
//! UI-agnostic core reports. Concrete services and their mutable state belong
//! in separate Phase 1 feature slices.

use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::CapabilityProbe;

/// The lifecycle state visible before a concrete feature service is introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceLifecycle {
    Registered,
    Available,
    Unavailable,
}

/// The immutable registration state for one known Kestrel feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRegistration {
    pub feature: FeatureSpec,
    pub capability: CapabilityReport,
    pub lifecycle: ServiceLifecycle,
}

impl ServiceRegistration {
    /// Builds a registration from feature metadata and a matching probe result.
    pub fn new(feature: FeatureSpec, capability: CapabilityReport) -> Result<Self, RegistryError> {
        if feature.id != capability.feature_id {
            return Err(RegistryError::FeatureIdMismatch {
                feature_id: feature.id,
                report_feature_id: capability.feature_id,
            });
        }

        let lifecycle = match capability.status {
            CapabilityStatus::Supported | CapabilityStatus::Limited { .. } => {
                ServiceLifecycle::Available
            }
            CapabilityStatus::NeedsPermission { .. }
            | CapabilityStatus::MissingDependency { .. }
            | CapabilityStatus::Unsupported { .. } => ServiceLifecycle::Unavailable,
        };

        Ok(Self {
            feature,
            capability,
            lifecycle,
        })
    }
}

/// A registration failed because a probe reported for a different feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    FeatureIdMismatch {
        feature_id: &'static str,
        report_feature_id: &'static str,
    },
}

/// Registry state used by the application composition root.
#[derive(Debug, Default)]
pub struct FeatureRegistry {
    registrations: Vec<ServiceRegistration>,
}

impl FeatureRegistry {
    /// Registers one feature using a platform probe without starting a service.
    pub fn register_probe<P: CapabilityProbe>(
        &mut self,
        feature: FeatureSpec,
        probe: &P,
    ) -> Result<(), RegistryError> {
        self.registrations
            .push(ServiceRegistration::new(feature, probe.probe())?);
        Ok(())
    }

    /// Returns all registered features in registration order.
    pub fn registrations(&self) -> &[ServiceRegistration] {
        &self.registrations
    }
}

#[cfg(test)]
mod tests {
    use super::{FeatureRegistry, RegistryError, ServiceLifecycle, ServiceRegistration};
    use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};
    use kestrel_platform::StaticCapabilityProbe;

    #[test]
    fn registration_marks_supported_features_available() {
        let registration = ServiceRegistration::new(
            FeatureSpec::new(
                "system.monitor",
                "System monitor",
                CapabilityStatus::Supported,
            ),
            CapabilityReport::new(
                "system.monitor",
                CapabilityStatus::Supported,
                "A test report.",
            ),
        )
        .expect("feature and report IDs match");

        assert_eq!(registration.lifecycle, ServiceLifecycle::Available);
    }

    #[test]
    fn registry_uses_the_platform_probe_seam() {
        let mut registry = FeatureRegistry::default();
        let probe = StaticCapabilityProbe::new(CapabilityReport::new(
            "audio.mixer",
            CapabilityStatus::Unsupported {
                reason: "No adapter has been selected.".to_string(),
            },
            "A test report.",
        ));

        registry
            .register_probe(
                FeatureSpec::new(
                    "audio.mixer",
                    "Audio mixer",
                    CapabilityStatus::Unsupported {
                        reason: "No adapter has been selected.".to_string(),
                    },
                ),
                &probe,
            )
            .expect("feature and report IDs match");

        assert_eq!(
            registry.registrations()[0].lifecycle,
            ServiceLifecycle::Unavailable
        );
    }

    #[test]
    fn registration_rejects_mismatched_feature_ids() {
        let error = ServiceRegistration::new(
            FeatureSpec::new("audio.mixer", "Audio mixer", CapabilityStatus::Supported),
            CapabilityReport::new(
                "system.monitor",
                CapabilityStatus::Supported,
                "A test report.",
            ),
        )
        .expect_err("mismatched IDs are invalid");

        assert_eq!(
            error,
            RegistryError::FeatureIdMismatch {
                feature_id: "audio.mixer",
                report_feature_id: "system.monitor",
            }
        );
    }
}
