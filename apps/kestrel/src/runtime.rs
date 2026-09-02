use kestrel_core::{ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::StaticCapabilityProbe;
use kestrel_services::{FeatureRegistry, RegistryError, ServiceRegistration};

/// UI-independent composition root for startup, enablement, and capability refresh.
pub struct ApplicationRuntime {
    registry: FeatureRegistry,
}

impl ApplicationRuntime {
    /// Registers the entry surfaces known before concrete feature services land.
    pub fn new(configuration: &ApplicationConfiguration) -> Result<Self, RegistryError> {
        let mut registry = FeatureRegistry::default();

        let command_surface = FeatureSpec::new(
            "app.command-surface",
            "Command surface",
            CapabilityStatus::Supported,
        );
        registry.register_probe(
            command_surface.clone(),
            true,
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    command_surface.id,
                    CapabilityStatus::Supported,
                    "The normal Kestrel window is always available as the command surface.",
                )
                .with_selected_backend("GTK/libadwaita normal window"),
            ),
        )?;

        let global_shortcuts = FeatureSpec::new(
            "global.shortcuts",
            "Global shortcuts",
            CapabilityStatus::Unsupported {
                reason: "No portable global-shortcut adapter is registered yet.".to_string(),
            },
        );
        registry.register_probe(
            global_shortcuts.clone(),
            configuration.feature_enabled(global_shortcuts.id),
            StaticCapabilityProbe::new(
                CapabilityReport::new(
                    global_shortcuts.id,
                    CapabilityStatus::Unsupported {
                        reason: "No portable global-shortcut adapter is registered yet.".to_string(),
                    },
                    "Global shortcuts are unavailable, but Kestrel remains usable from its normal window.",
                )
                .with_remediation(
                    "Use the normal window or configure a desktop shortcut that launches Kestrel.",
                ),
            ),
        )?;

        Ok(Self { registry })
    }

    /// Starts only the entries that are enabled and currently available.
    pub fn start(&mut self) {
        self.registry.start_enabled();
    }

    /// Refreshes every cached capability report through its feature-specific probe.
    pub fn refresh_capabilities(&mut self) -> Result<(), RegistryError> {
        let feature_ids = self
            .registry
            .registrations()
            .map(|registration| registration.feature.id)
            .collect::<Vec<_>>();
        for feature_id in feature_ids {
            self.registry.refresh_capability(feature_id)?;
        }
        Ok(())
    }

    /// Returns UI-independent registration snapshots for the active session.
    pub fn registrations(&self) -> impl Iterator<Item = &ServiceRegistration> {
        self.registry.registrations()
    }
}

#[cfg(test)]
mod tests {
    use super::ApplicationRuntime;
    use kestrel_core::ApplicationConfiguration;

    #[test]
    fn startup_keeps_unavailable_features_visible() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .refresh_capabilities()
            .expect("static capability refreshes");

        let registrations = runtime.registrations().collect::<Vec<_>>();
        assert_eq!(registrations.len(), 2);
        assert!(registrations[0].running);
        assert!(!registrations[1].available);
        assert!(registrations[1].capability.remediation.is_some());
    }
}
