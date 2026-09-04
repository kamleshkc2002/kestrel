//! Feature registry and lifecycle primitives.
//!
//! Services combine feature policy with `kestrel-platform` probes and publish
//! UI-agnostic core reports. Concrete feature services can later attach their
//! own resource lifecycle to this capability-aware runtime state.

use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::CapabilityProbe;

pub mod system_monitor;

/// The current lifecycle stage of a registered feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceLifecycle {
    Registered,
    Available,
    Unavailable,
    Running,
}

/// The current state for one known Kestrel feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRegistration {
    pub feature: FeatureSpec,
    pub capability: CapabilityReport,
    pub enabled: bool,
    pub available: bool,
    pub running: bool,
}

impl ServiceRegistration {
    /// Builds a registration from feature metadata and a matching probe result.
    pub fn new(
        feature: FeatureSpec,
        capability: CapabilityReport,
        enabled: bool,
    ) -> Result<Self, RegistryError> {
        if feature.id != capability.feature_id {
            return Err(RegistryError::FeatureIdMismatch {
                feature_id: feature.id,
                report_feature_id: capability.feature_id,
            });
        }

        Ok(Self {
            feature,
            available: is_available(&capability.status),
            capability,
            enabled,
            running: false,
        })
    }

    /// Returns the feature's current lifecycle stage without hiding enablement.
    pub fn lifecycle(&self) -> ServiceLifecycle {
        if self.running {
            ServiceLifecycle::Running
        } else if !self.available {
            ServiceLifecycle::Unavailable
        } else if self.enabled {
            ServiceLifecycle::Available
        } else {
            ServiceLifecycle::Registered
        }
    }

    fn update_capability(&mut self, capability: CapabilityReport) {
        self.available = is_available(&capability.status);
        self.capability = capability;
        if !self.available {
            self.running = false;
        }
    }
}

fn is_available(status: &CapabilityStatus) -> bool {
    matches!(
        status,
        CapabilityStatus::Supported | CapabilityStatus::Limited { .. }
    )
}

/// A registration or lifecycle operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    FeatureIdMismatch {
        feature_id: &'static str,
        report_feature_id: &'static str,
    },
    DuplicateFeature {
        feature_id: &'static str,
    },
    UnknownFeature {
        feature_id: String,
    },
}

/// Registry state used by the application composition root.
#[derive(Default)]
pub struct FeatureRegistry {
    features: Vec<RegisteredFeature>,
}

struct RegisteredFeature {
    registration: ServiceRegistration,
    probe: Box<dyn CapabilityProbe>,
}

impl FeatureRegistry {
    /// Registers a feature, caches its initial probe result, and does not start it.
    pub fn register_probe<P: CapabilityProbe + 'static>(
        &mut self,
        feature: FeatureSpec,
        enabled: bool,
        probe: P,
    ) -> Result<(), RegistryError> {
        if self
            .features
            .iter()
            .any(|registered| registered.registration.feature.id == feature.id)
        {
            return Err(RegistryError::DuplicateFeature {
                feature_id: feature.id,
            });
        }

        let registration = ServiceRegistration::new(feature, probe.probe(), enabled)?;
        self.features.push(RegisteredFeature {
            registration,
            probe: Box::new(probe),
        });
        Ok(())
    }

    /// Returns all registered features in registration order.
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.features
            .iter()
            .map(|registered| &registered.registration)
    }

    /// Refreshes a cached report through the feature's original read-only probe.
    pub fn refresh_capability(
        &mut self,
        feature_id: &str,
    ) -> Result<&ServiceRegistration, RegistryError> {
        let registered = self.find_mut(feature_id)?;
        let report = registered.probe.probe();
        if report.feature_id != registered.registration.feature.id {
            return Err(RegistryError::FeatureIdMismatch {
                feature_id: registered.registration.feature.id,
                report_feature_id: report.feature_id,
            });
        }
        registered.registration.update_capability(report);
        Ok(&registered.registration)
    }

    /// Applies a user's enablement preference without assuming availability.
    pub fn set_enabled(
        &mut self,
        feature_id: &str,
        enabled: bool,
    ) -> Result<&ServiceRegistration, RegistryError> {
        let registration = &mut self.find_mut(feature_id)?.registration;
        registration.enabled = enabled;
        if !enabled {
            registration.running = false;
        }
        Ok(registration)
    }

    /// Starts every enabled feature that currently has an available capability.
    pub fn start_enabled(&mut self) {
        for registered in &mut self.features {
            let registration = &mut registered.registration;
            registration.running = registration.enabled && registration.available;
        }
    }

    fn find_mut(&mut self, feature_id: &str) -> Result<&mut RegisteredFeature, RegistryError> {
        self.features
            .iter_mut()
            .find(|registered| registered.registration.feature.id == feature_id)
            .ok_or_else(|| RegistryError::UnknownFeature {
                feature_id: feature_id.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{FeatureRegistry, RegistryError, ServiceLifecycle, ServiceRegistration};
    use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};
    use kestrel_platform::StaticCapabilityProbe;

    #[test]
    fn registration_tracks_enablement_separately_from_availability() {
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
            false,
        )
        .expect("feature and report IDs match");

        assert!(!registration.enabled);
        assert!(registration.available);
        assert_eq!(registration.lifecycle(), ServiceLifecycle::Registered);
    }

    #[test]
    fn registry_retains_unavailable_features_for_remediation() {
        let mut registry = FeatureRegistry::default();
        let probe = StaticCapabilityProbe::new(
            CapabilityReport::new(
                "audio.mixer",
                CapabilityStatus::Unsupported {
                    reason: "No adapter has been selected.".to_string(),
                },
                "Audio is unavailable.",
            )
            .with_remediation("Install a supported PulseAudio-compatible service."),
        );

        registry
            .register_probe(
                FeatureSpec::new(
                    "audio.mixer",
                    "Audio mixer",
                    CapabilityStatus::Unsupported {
                        reason: "No adapter has been selected.".to_string(),
                    },
                ),
                true,
                probe,
            )
            .expect("feature and report IDs match");
        registry.start_enabled();
        let registration = registry.registrations().next().expect("one registration");

        assert!(!registration.available);
        assert!(!registration.running);
        assert_eq!(registration.lifecycle(), ServiceLifecycle::Unavailable);
        assert!(registration.capability.remediation.is_some());
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
            false,
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

    #[test]
    fn refresh_updates_cached_availability_and_stops_running_features() {
        let mut registry = FeatureRegistry::default();
        let probe = StaticCapabilityProbe::new(CapabilityReport::new(
            "audio.mixer",
            CapabilityStatus::Unsupported {
                reason: "Service disappeared.".to_string(),
            },
            "Audio service is unavailable.",
        ));

        registry
            .register_probe(
                FeatureSpec::new("audio.mixer", "Audio mixer", CapabilityStatus::Supported),
                true,
                probe,
            )
            .expect("feature registers");
        registry.start_enabled();
        let registration = registry
            .refresh_capability("audio.mixer")
            .expect("registered feature refreshes");

        assert!(!registration.available);
        assert!(!registration.running);
        assert_eq!(registration.lifecycle(), ServiceLifecycle::Unavailable);
    }

    #[test]
    fn disabling_a_running_feature_stops_it_without_discarding_its_report() {
        let mut registry = FeatureRegistry::default();
        let probe = StaticCapabilityProbe::new(CapabilityReport::new(
            "system.monitor",
            CapabilityStatus::Supported,
            "Monitoring is available.",
        ));
        registry
            .register_probe(
                FeatureSpec::new(
                    "system.monitor",
                    "System monitor",
                    CapabilityStatus::Supported,
                ),
                true,
                probe,
            )
            .expect("feature registers");
        registry.start_enabled();

        let registration = registry
            .set_enabled("system.monitor", false)
            .expect("registered feature is configurable");

        assert!(!registration.enabled);
        assert!(!registration.running);
        assert_eq!(registration.capability.summary, "Monitoring is available.");
    }
}
