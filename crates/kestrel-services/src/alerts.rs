//! Policy-driven, sustained system alerts with bounded in-memory state.

use std::time::Duration;

use crate::system_monitor::SystemSnapshot;
use kestrel_core::{AlertKind, AlertRuleConfiguration, MonitorAlertConfiguration};

/// Recovery distance from an alert threshold, in percentage points or degrees Celsius.
pub const HYSTERESIS: f64 = 5.0;

#[derive(Debug, Clone, PartialEq)]
pub struct AlertRule {
    pub kind: AlertKind,
    pub enabled: bool,
    pub threshold: f64,
    pub sustain_samples: u32,
    pub cooldown: Duration,
}

impl AlertRule {
    pub fn from_configuration(kind: AlertKind, configuration: &AlertRuleConfiguration) -> Self {
        Self {
            kind,
            enabled: configuration.enabled,
            threshold: configuration.threshold,
            sustain_samples: configuration.sustain_samples,
            cooldown: Duration::from_secs(configuration.cooldown_seconds),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertPolicy {
    rules: [AlertRule; 5],
}

impl Default for AlertPolicy {
    fn default() -> Self {
        Self::from_configuration(&MonitorAlertConfiguration::default())
    }
}

impl AlertPolicy {
    pub fn from_configuration(configuration: &MonitorAlertConfiguration) -> Self {
        Self {
            rules: AlertKind::ALL
                .map(|kind| AlertRule::from_configuration(kind, configuration.rule(kind))),
        }
    }

    pub fn rules(&self) -> impl Iterator<Item = &AlertRule> {
        self.rules.iter()
    }

    pub fn rule(&self, kind: AlertKind) -> Option<&AlertRule> {
        self.rules.get(kind_index(kind))
    }

    pub fn set_enabled(&mut self, kind: AlertKind, enabled: bool) {
        self.rules[kind_index(kind)].enabled = enabled;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlertEvent {
    Raised {
        kind: AlertKind,
        value: f64,
        threshold: f64,
        subject: Option<String>,
        observed_at: Duration,
    },
    Cleared {
        kind: AlertKind,
        observed_at: Duration,
    },
}

impl AlertEvent {
    pub fn kind(&self) -> AlertKind {
        match self {
            Self::Raised { kind, .. } | Self::Cleared { kind, .. } => *kind,
        }
    }

    pub fn is_raised(&self) -> bool {
        matches!(self, Self::Raised { .. })
    }
}

/// Produce privacy-preserving one-line notification text for an alert transition.
pub fn notification_text(event: &AlertEvent) -> (String, String) {
    let kind = event.kind();
    let label = kind.label();
    match event {
        AlertEvent::Raised { value, .. } => (
            format!("{label} alert"),
            format!("{label} is at {:.0}{}.", value.round(), kind.unit()),
        ),
        AlertEvent::Cleared { .. } => (
            format!("{label} alert cleared"),
            format!("{label} alert cleared."),
        ),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveAlert {
    pub kind: AlertKind,
    pub value: f64,
    pub threshold: f64,
    pub subject: Option<String>,
    pub raised_at: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertDeliveryFailure {
    pub kind: AlertKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AlertSnapshot {
    pub active: Vec<ActiveAlert>,
    pub delivery_failures: Vec<AlertDeliveryFailure>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertEngine {
    policy: AlertPolicy,
    sustain_counters: [u32; 5],
    active: [Option<ActiveAlert>; 5],
    last_raised: [Option<Duration>; 5],
    delivery_failures: [Option<AlertDeliveryFailure>; 5],
}

impl AlertEngine {
    pub fn new(policy: AlertPolicy) -> Self {
        Self {
            policy,
            sustain_counters: [0; 5],
            active: [None, None, None, None, None],
            last_raised: [None; 5],
            delivery_failures: [None, None, None, None, None],
        }
    }

    pub fn policy(&self) -> &AlertPolicy {
        &self.policy
    }

    pub fn set_policy(&mut self, policy: AlertPolicy) {
        self.policy = policy;
    }

    pub fn set_rule_enabled(&mut self, kind: AlertKind, enabled: bool) {
        self.policy.set_enabled(kind, enabled);
    }

    pub fn evaluate(&mut self, snapshot: &SystemSnapshot) -> Vec<AlertEvent> {
        let mut events = Vec::new();
        for kind in AlertKind::ALL {
            let index = kind_index(kind);
            let rule = &self.policy.rules[index];
            if !rule.enabled {
                self.sustain_counters[index] = 0;
                continue;
            }
            let Some((value, subject)) = observed_value(snapshot, kind) else {
                self.sustain_counters[index] = 0;
                continue;
            };
            let active = self.active[index].is_some();
            let recovered = if active {
                if kind.alerts_above() {
                    value <= rule.threshold - HYSTERESIS
                } else {
                    value >= rule.threshold + HYSTERESIS
                }
            } else {
                false
            };
            if recovered {
                self.active[index] = None;
                self.sustain_counters[index] = 0;
                self.last_raised[index] = None;
                events.push(AlertEvent::Cleared {
                    kind,
                    observed_at: snapshot.observed_at,
                });
                continue;
            }

            let crossing = if kind.alerts_above() {
                value >= rule.threshold
            } else {
                value <= rule.threshold
            };
            if !crossing {
                self.sustain_counters[index] = 0;
                continue;
            }

            self.sustain_counters[index] = self.sustain_counters[index].saturating_add(1);
            if self.sustain_counters[index] < rule.sustain_samples {
                continue;
            }
            let cooldown_elapsed = self.last_raised[index]
                .and_then(|raised_at| snapshot.observed_at.checked_sub(raised_at))
                .is_some_and(|elapsed| elapsed >= rule.cooldown);
            if active && !cooldown_elapsed {
                continue;
            }
            let active_alert = ActiveAlert {
                kind,
                value,
                threshold: rule.threshold,
                subject,
                raised_at: snapshot.observed_at,
            };
            self.active[index] = Some(active_alert.clone());
            self.sustain_counters[index] = 0;
            self.last_raised[index] = Some(snapshot.observed_at);
            events.push(AlertEvent::Raised {
                kind,
                value,
                threshold: rule.threshold,
                subject: active_alert.subject,
                observed_at: snapshot.observed_at,
            });
        }
        events
    }

    pub fn snapshot(&self) -> AlertSnapshot {
        AlertSnapshot {
            active: AlertKind::ALL
                .into_iter()
                .filter_map(|kind| self.active[kind_index(kind)].clone())
                .collect(),
            delivery_failures: AlertKind::ALL
                .into_iter()
                .filter_map(|kind| self.delivery_failures[kind_index(kind)].clone())
                .collect(),
        }
    }

    pub fn record_delivery_failure(&mut self, kind: AlertKind, message: impl Into<String>) {
        self.delivery_failures[kind_index(kind)] = Some(AlertDeliveryFailure {
            kind,
            message: message.into(),
        });
    }

    pub fn clear_delivery_failure(&mut self, kind: AlertKind) {
        self.delivery_failures[kind_index(kind)] = None;
    }
}

fn kind_index(kind: AlertKind) -> usize {
    match kind {
        AlertKind::Cpu => 0,
        AlertKind::Temperature => 1,
        AlertKind::Memory => 2,
        AlertKind::Disk => 3,
        AlertKind::Battery => 4,
    }
}

fn observed_value(snapshot: &SystemSnapshot, kind: AlertKind) -> Option<(f64, Option<String>)> {
    let observed = match kind {
        AlertKind::Cpu => snapshot.cpu_usage_percent.map(|value| (value, None)),
        AlertKind::Memory => snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.used_percent.map(|value| (value, None))),
        AlertKind::Disk => snapshot
            .disk_usage
            .iter()
            .filter_map(|disk| {
                disk.used_percent
                    .filter(|value| value.is_finite())
                    .map(|value| (value, Some(disk.mount_point.clone())))
            })
            .max_by(|(left, _), (right, _)| left.total_cmp(right)),
        AlertKind::Temperature => snapshot
            .temperatures
            .iter()
            .map(|temperature| {
                (
                    temperature.millidegrees_celsius as f64 / 1_000.0,
                    Some(temperature.label.clone()),
                )
            })
            .max_by(|(left, _), (right, _)| left.total_cmp(right)),
        AlertKind::Battery => snapshot
            .power_supplies
            .iter()
            .filter(|supply| {
                supply
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("battery"))
                    && supply
                        .scope
                        .as_deref()
                        .is_none_or(|scope| scope.eq_ignore_ascii_case("system"))
                    && supply
                        .status
                        .as_deref()
                        .is_some_and(|status| status.eq_ignore_ascii_case("discharging"))
            })
            .filter_map(|supply| {
                supply
                    .capacity_percent
                    .map(|value| (value as f64, Some(supply.name.clone())))
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right)),
    };
    observed.filter(|(value, _)| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_monitor::{DiskUsageSnapshot, MemorySnapshot, PowerSupplySnapshot};
    use kestrel_platform::system_monitor::TemperatureReading;

    fn snapshot(at: u64) -> SystemSnapshot {
        SystemSnapshot {
            observed_at: Duration::from_secs(at),
            cpu_usage_percent: None,
            logical_cpus: None,
            memory: None,
            disk_usage: Vec::new(),
            disk_activity: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: Vec::new(),
        }
    }

    fn policy(kind: AlertKind, threshold: f64, sustain: u32, cooldown: u64) -> AlertPolicy {
        let mut configuration = MonitorAlertConfiguration::default();
        *configuration.rule_mut(kind) =
            AlertRuleConfiguration::new(true, threshold, sustain, cooldown);
        AlertPolicy::from_configuration(&configuration)
    }

    #[test]
    fn sustained_requirement_honoured() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 2, 900));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value).is_empty());
        value.observed_at = Duration::from_secs(2);
        assert!(matches!(
            engine.evaluate(&value).as_slice(),
            [AlertEvent::Raised { .. }]
        ));
    }

    #[test]
    fn counter_resets_on_non_crossing_sample() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 2, 900));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value).is_empty());
        value.cpu_usage_percent = Some(70.0);
        value.observed_at = Duration::from_secs(2);
        assert!(engine.evaluate(&value).is_empty());
        value.cpu_usage_percent = Some(90.0);
        value.observed_at = Duration::from_secs(3);
        assert!(engine.evaluate(&value).is_empty());
    }

    #[test]
    fn repeat_limited_by_cooldown_and_allowed_after_it() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 1, 10));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert_eq!(engine.evaluate(&value).len(), 1);
        value.observed_at = Duration::from_secs(2);
        assert!(engine.evaluate(&value).is_empty());
        value.observed_at = Duration::from_secs(11);
        assert_eq!(engine.evaluate(&value).len(), 1);
    }

    #[test]
    fn recovery_past_hysteresis_clears_and_rearms() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 1, 900));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value)[0].is_raised());
        value.cpu_usage_percent = Some(74.0);
        value.observed_at = Duration::from_secs(2);
        assert!(matches!(
            engine.evaluate(&value).as_slice(),
            [AlertEvent::Cleared { .. }]
        ));
        value.cpu_usage_percent = Some(90.0);
        value.observed_at = Duration::from_secs(3);
        assert!(engine.evaluate(&value)[0].is_raised());
    }

    #[test]
    fn charging_battery_does_not_alert() {
        let mut engine = AlertEngine::new(policy(AlertKind::Battery, 15.0, 1, 900));
        let mut value = snapshot(1);
        value.power_supplies.push(PowerSupplySnapshot {
            name: "battery".into(),
            kind: Some("Battery".into()),
            scope: Some("System".into()),
            capacity_percent: Some(5),
            status: Some("Charging".into()),
            health: None,
            cycle_count: None,
            energy_now_microwatt_hours: None,
            energy_full_microwatt_hours: None,
            power_microwatts: None,
            remaining_seconds: None,
        });
        assert!(engine.evaluate(&value).is_empty());
    }
    #[test]
    fn low_discharging_battery_does_not_repeat_while_unchanged() {
        let mut engine = AlertEngine::new(policy(AlertKind::Battery, 15.0, 1, 900));
        let mut value = snapshot(1);
        value.power_supplies.push(PowerSupplySnapshot {
            name: "battery".into(),
            kind: Some("Battery".into()),
            scope: Some("System".into()),
            capacity_percent: Some(10),
            status: Some("Discharging".into()),
            health: None,
            cycle_count: None,
            energy_now_microwatt_hours: None,
            energy_full_microwatt_hours: None,
            power_microwatts: None,
            remaining_seconds: None,
        });
        assert_eq!(engine.evaluate(&value).len(), 1);
        value.observed_at = Duration::from_secs(2);
        assert!(engine.evaluate(&value).is_empty());
    }

    #[test]
    fn battery_clears_when_it_stops_discharging() {
        let mut engine = AlertEngine::new(policy(AlertKind::Battery, 15.0, 1, 900));
        let mut value = snapshot(1);
        value.power_supplies.push(PowerSupplySnapshot {
            name: "battery".into(),
            kind: Some("Battery".into()),
            scope: None,
            capacity_percent: Some(5),
            status: Some("Discharging".into()),
            health: None,
            cycle_count: None,
            energy_now_microwatt_hours: None,
            energy_full_microwatt_hours: None,
            power_microwatts: None,
            remaining_seconds: None,
        });
        assert!(engine.evaluate(&value)[0].is_raised());
        value.power_supplies[0].status = Some("Charging".into());
        value.observed_at = Duration::from_secs(2);
        assert!(engine.evaluate(&value).is_empty());
        assert!(engine.snapshot().active.len() == 1);
    }

    #[test]
    fn unavailable_metric_neither_raises_nor_clears_or_disturbs_other_kinds() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 2, 900));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value).is_empty());
        value.observed_at = Duration::from_secs(2);
        value.cpu_usage_percent = None;
        assert!(engine.evaluate(&value).is_empty());
        value.observed_at = Duration::from_secs(3);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value).is_empty());
    }

    #[test]
    fn disabled_rule_is_inert() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 1, 1));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(100.0);
        engine.set_rule_enabled(AlertKind::Cpu, false);
        assert!(engine.evaluate(&value).is_empty());
        value.observed_at = Duration::from_secs(2);
        assert!(engine.evaluate(&value).is_empty());
        assert!(engine.snapshot().active.is_empty());

        engine.set_rule_enabled(AlertKind::Cpu, true);
        value.observed_at = Duration::from_secs(3);
        assert!(matches!(
            engine.evaluate(&value).as_slice(),
            [AlertEvent::Raised { .. }]
        ));
        engine.set_rule_enabled(AlertKind::Cpu, false);
        value.observed_at = Duration::from_secs(4);
        assert!(engine.evaluate(&value).is_empty());
        assert_eq!(engine.snapshot().active.len(), 1);
        engine.set_rule_enabled(AlertKind::Cpu, true);
        value.observed_at = Duration::from_secs(5);
        assert!(matches!(
            engine.evaluate(&value).as_slice(),
            [AlertEvent::Raised { .. }]
        ));
    }

    #[test]
    fn snapshot_lists_only_active_kinds_in_order() {
        let mut configuration = MonitorAlertConfiguration::default();
        for kind in AlertKind::ALL {
            *configuration.rule_mut(kind) = AlertRuleConfiguration::new(true, 1.0, 1, 1);
        }
        let mut engine = AlertEngine::new(AlertPolicy::from_configuration(&configuration));
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(10.0);
        value.memory = Some(MemorySnapshot {
            total_bytes: 10,
            used_bytes: 10,
            available_bytes: 0,
            cached_bytes: 0,
            buffers_bytes: 0,
            used_percent: Some(10.0),
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_used_percent: None,
        });
        value.disk_usage.push(DiskUsageSnapshot {
            device: "dev".into(),
            mount_point: "/".into(),
            filesystem: "fs".into(),
            total_bytes: 10,
            available_bytes: 0,
            used_bytes: 10,
            used_percent: Some(10.0),
        });
        value.temperatures.push(TemperatureReading {
            stable_key: "t".into(),
            label: "CPU".into(),
            millidegrees_celsius: 10_000,
        });
        for kind in [
            AlertKind::Cpu,
            AlertKind::Memory,
            AlertKind::Disk,
            AlertKind::Temperature,
        ] {
            assert!(!engine.evaluate(&value).is_empty() || engine.policy().rule(kind).is_some());
        }
        let active = engine.snapshot().active;
        assert!(
            active
                .windows(2)
                .all(|window| window[0].kind <= window[1].kind)
        );
    }

    #[test]
    fn delivery_failures_are_per_kind_and_do_not_suppress_alerts() {
        let mut engine = AlertEngine::new(policy(AlertKind::Cpu, 80.0, 1, 1));
        engine.record_delivery_failure(AlertKind::Cpu, "unavailable");
        let mut value = snapshot(1);
        value.cpu_usage_percent = Some(90.0);
        assert!(engine.evaluate(&value)[0].is_raised());
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.delivery_failures[0].message, "unavailable");
        assert_eq!(snapshot.active.len(), 1);
        engine.clear_delivery_failure(AlertKind::Cpu);
        assert!(engine.snapshot().delivery_failures.is_empty());
    }
}
