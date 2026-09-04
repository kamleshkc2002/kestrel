use kestrel_core::{ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::{
    system_monitor::{ProcSysMonitor, FEATURE_ID as SYSTEM_MONITOR_ID},
    StaticCapabilityProbe,
};
use kestrel_services::{
    system_monitor::{
        RefreshOutcome, SystemMonitorService, SystemSnapshot, DEFAULT_REFRESH_INTERVAL,
    },
    FeatureRegistry, RegistryError, ServiceRegistration,
};
use std::time::Duration;

/// UI-independent composition root for startup, enablement, and capability refresh.
pub struct ApplicationRuntime {
    registry: FeatureRegistry,
    system_monitor: SystemMonitorService<ProcSysMonitor>,
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

        let system_monitor = FeatureSpec::new(
            SYSTEM_MONITOR_ID,
            "System monitor",
            CapabilityStatus::Supported,
        );
        let monitor_source = ProcSysMonitor::default();
        registry.register_probe(
            system_monitor,
            configuration.feature_enabled(SYSTEM_MONITOR_ID),
            monitor_source.clone(),
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
        Ok(Self {
            registry,
            system_monitor: SystemMonitorService::new(monitor_source, DEFAULT_REFRESH_INTERVAL)
                .expect("the built-in monitor refresh interval is valid"),
        })
    }

    /// Starts only the entries that are enabled and currently available.
    pub fn start(&mut self) {
        self.registry.start_enabled();
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(Duration::ZERO);
        }
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

    /// Samples monitor metrics when the feature is running and its interval elapsed.
    pub fn refresh_system_monitor(&mut self, observed_at: Duration) -> RefreshOutcome {
        if self.system_monitor_is_running() {
            self.system_monitor.refresh(observed_at)
        } else {
            RefreshOutcome::Skipped
        }
    }

    /// Returns the latest immutable monitor snapshot, if sampling has started.
    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }

    fn system_monitor_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == SYSTEM_MONITOR_ID && registration.running
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ApplicationRuntime;
    use kestrel_core::ApplicationConfiguration;
    use kestrel_platform::system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID;

    #[test]
    fn startup_keeps_unavailable_features_visible() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .refresh_capabilities()
            .expect("static capability refreshes");

        let registrations = runtime.registrations().collect::<Vec<_>>();
        assert_eq!(registrations.len(), 3);
        assert!(registrations[0].running);
        assert_eq!(registrations[1].feature.id, SYSTEM_MONITOR_ID);
        assert!(!registrations[2].available);
        assert!(registrations[2].capability.remediation.is_some());
    }

    #[test]
    fn enabled_monitor_starts_with_a_non_failing_live_snapshot() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");

        runtime.start();

        assert!(runtime.system_monitor_snapshot().is_some());
    }
}
