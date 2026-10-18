//! System monitoring and speed-test runtime features.

use super::*;

impl ApplicationRuntime {
    pub fn sample_monitor(&mut self, observed_at: Duration) -> MonitorTick {
        if !self.system_monitor_is_running() {
            return MonitorTick {
                outcome: RefreshOutcome::Skipped,
                alerts: Vec::new(),
            };
        }
        let outcome = self.system_monitor.refresh(observed_at);
        let alerts = match self.system_monitor.latest() {
            Some(snapshot) if outcome == RefreshOutcome::Updated => self.alerts.evaluate(snapshot),
            _ => Vec::new(),
        };
        MonitorTick { outcome, alerts }
    }

    pub fn record_alert_delivery(&mut self, kind: AlertKind, result: Result<(), String>) {
        match result {
            Ok(()) => self.alerts.clear_delivery_failure(kind),
            Err(message) => self.alerts.record_delivery_failure(kind, message),
        }
    }

    pub fn monitor_view_model(&self, configuration: &ApplicationConfiguration) -> MonitorViewModel {
        MonitorViewModel::from_configuration(
            &configuration.monitoring,
            MonitorPresentation {
                snapshot: self.system_monitor.latest(),
                history: self.system_monitor.history_summary(),
                alerts: self.alerts.snapshot(),
                running: self.system_monitor_is_running(),
            },
        )
        .with_local_addresses(self.local_addresses.as_ref())
    }

    /// Reads local addresses on an explicit request, or forgets them.
    pub fn set_local_addresses_shown(&mut self, shown: bool) {
        self.local_addresses = shown.then(|| {
            kestrel_platform::network_addresses::local_addresses()
                .map_err(|error| format!("Local addresses could not be read: {error}"))
        });
    }

    pub fn system_monitor_snapshot(&self) -> Option<&SystemSnapshot> {
        self.system_monitor.latest()
    }

    pub fn speed_test_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SPEED_TEST_ID && registration.running)
    }

    pub fn start_speed_test(&mut self) -> Result<(), String> {
        self.require_speed_test()?;
        self.speed_test.start().map_err(|error| error.to_string())
    }

    pub fn cancel_speed_test(&mut self) -> Result<(), String> {
        self.require_speed_test()?;
        self.speed_test.cancel().map_err(|error| error.to_string())
    }

    fn require_speed_test(&self) -> Result<(), String> {
        if self.speed_test_is_running() {
            Ok(())
        } else {
            Err(
                "The network speed test is not running; enable network.speed_test in the \
                 Feature Hub"
                    .to_owned(),
            )
        }
    }

    pub fn speed_test_snapshot(&self) -> SpeedTestSnapshot {
        self.speed_test.snapshot()
    }

    pub fn speed_test_view_model(&self) -> SpeedTestViewModel {
        SpeedTestViewModel::from_presentation(SpeedTestPresentation {
            snapshot: self.speed_test.snapshot(),
            running: self.speed_test_is_running(),
        })
    }

    pub(super) fn system_monitor_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == SYSTEM_MONITOR_ID && registration.running
        })
    }

    pub fn monitor_is_running(&self) -> bool {
        self.system_monitor_is_running()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FeaturePreset;
    use kestrel_platform::{audio::MICROPHONE_FEATURE_ID, speed_test::FEATURE_ID as SPEED_TEST_ID};
    use std::time::Duration;
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
    #[test]
    fn monitor_sampling_respects_configured_interval() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        configuration.monitoring.refresh_interval_millis = 60_000;
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();

        assert!(matches!(
            runtime
                .sample_monitor(Duration::from_millis(59_999))
                .outcome,
            kestrel_services::system_monitor::RefreshOutcome::Skipped
        ));
    }
    #[test]
    fn monitor_history_stays_bounded_by_configured_capacity() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.monitoring.history_samples = 1;
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        let _ = runtime.sample_monitor(Duration::from_secs(1));
        let _ = runtime.sample_monitor(Duration::from_secs(2));

        assert_eq!(
            runtime
                .system_monitor
                .history_summary()
                .expect("history has samples")
                .samples,
            1
        );
    }
    #[test]
    fn a_disabled_speed_test_never_starts_or_transfers() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        let error = runtime
            .start_speed_test()
            .expect_err("a stopped feature cannot run a test");

        assert!(error.contains("network.speed_test"), "{error}");
        assert_eq!(
            runtime.speed_test_snapshot().status,
            kestrel_services::speed_test::SpeedTestStatus::Idle
        );
        assert!(runtime.cancel_speed_test().is_err());
    }
    #[test]
    fn only_the_wider_presets_include_the_on_demand_speed_test() {
        assert!(!ApplicationRuntime::preset_enabled(
            FeaturePreset::Essentials,
            SPEED_TEST_ID
        ));
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Balanced,
            SPEED_TEST_ID
        ));
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Essentials,
            MICROPHONE_FEATURE_ID
        ));
    }
}
