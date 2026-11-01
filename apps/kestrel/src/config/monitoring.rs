//! Monitoring preferences, readouts, and alert rules.

use kestrel_core::{
    AlertKind, MAX_ALERT_COOLDOWN_SECONDS, MAX_ALERT_SUSTAIN_SAMPLES, MAX_ALERT_THRESHOLD_PERCENT,
    MAX_MONITOR_HISTORY_SAMPLES, MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
    MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS, MIN_ALERT_SUSTAIN_SAMPLES,
    MIN_MONITOR_REFRESH_INTERVAL_MILLIS, MonitorReadout,
};
use serde::Deserialize;

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section},
};

const ENABLED_MESSAGE: &str = "Alert enabled value is invalid and the default was retained";
const NETWORK_RATE_MESSAGE: &str =
    "Network rate unit must be \"bytes\" or \"bits\"; the default was retained.";

pub(super) fn parse_monitoring(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "monitoring", value) else {
        return;
    };
    let alerts = section.get("alerts");
    let monitoring = &mut loaded.configuration.monitoring;
    section.in_range(
        "refresh_interval_millis",
        MIN_MONITOR_REFRESH_INTERVAL_MILLIS..=MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
        ("Refresh interval must be", " milliseconds"),
        &mut monitoring.refresh_interval_millis,
    );
    section.in_range(
        "history_samples",
        1..=MAX_MONITOR_HISTORY_SAMPLES,
        ("History samples must be", ""),
        &mut monitoring.history_samples,
    );
    section.read(
        "network_rate_unit",
        NETWORK_RATE_MESSAGE,
        &mut monitoring.network_rate_unit,
    );
    section.read("per_core_cpu", BOOL_MESSAGE, &mut monitoring.per_core_cpu);
    if let Some(tray) = section.get("tray_readouts") {
        match Vec::<MonitorReadout>::deserialize(tray.clone()) {
            Ok(readouts)
                if readouts.len() <= kestrel_core::MAX_TRAY_READOUTS
                    && readouts
                        .iter()
                        .enumerate()
                        .all(|(index, readout)| !readouts[..index].contains(readout)) =>
            {
                monitoring.tray_readouts = readouts;
            }
            _ => section.warn(
                "tray_readouts",
                format!(
                    "Tray readouts must be at most {} different readouts; none are shown.",
                    kestrel_core::MAX_TRAY_READOUTS
                ),
            ),
        }
    }
    if let Some(readouts) = section.get("readouts") {
        if let Some(entries) = readouts.as_array() {
            let mut parsed = Vec::new();
            for (index, entry) in entries.iter().enumerate() {
                let readout = match MonitorReadout::deserialize(entry.clone()) {
                    Ok(readout) => readout,
                    Err(error) => {
                        section.warn(
                            &format!("readouts[{index}]"),
                            format!(
                                "Monitor readout is unknown or invalid and was ignored: {error}"
                            ),
                        );
                        continue;
                    }
                };
                if parsed.contains(&readout) {
                    section.warn(
                        &format!("readouts[{index}]"),
                        "Duplicate monitor readout was ignored; the first entry was retained.",
                    );
                    continue;
                }
                parsed.push(readout);
            }
            if parsed.is_empty() {
                section.warn(
                    "readouts",
                    "Monitor readouts were empty or invalid; the default list was retained.",
                );
            } else {
                monitoring.readouts = parsed;
            }
        } else {
            section.warn(
                "readouts",
                "Monitor readouts must be an array; the default list was retained.",
            );
        }
    }
    parse_monitoring_alerts(loaded, &mut section, alerts);
    section.finish(loaded);
}

fn parse_monitoring_alerts(
    loaded: &mut LoadedConfiguration,
    parent: &mut Section<'_>,
    value: Option<&toml::Value>,
) {
    let Some(value) = value else { return };
    let Some(table) = value.as_table() else {
        parent.warn(
            "alerts",
            "The monitoring alerts value must be a TOML table; defaults were retained.",
        );
        return;
    };
    let mut alerts = Section::nested("monitoring.alerts", table);
    for kind in AlertKind::ALL {
        let name = alert_kind_name(kind);
        if let Some(rule) = alerts.get(name) {
            parse_alert_rule(loaded, &mut alerts, name, kind, rule);
        }
    }
    for name in alerts.table().keys() {
        if !AlertKind::ALL
            .iter()
            .any(|kind| alert_kind_name(*kind) == name.as_str())
        {
            alerts.warn(name, "Unknown alert kind was ignored.");
        }
    }
    parent.absorb(alerts);
}

fn parse_alert_rule(
    loaded: &mut LoadedConfiguration,
    alerts: &mut Section<'_>,
    name: &str,
    kind: AlertKind,
    value: &toml::Value,
) {
    let Some(table) = value.as_table() else {
        alerts.warn(
            name,
            "Alert rule must be a TOML table; defaults were retained.",
        );
        return;
    };
    let location = format!("monitoring.alerts.{name}");
    let mut rule = Section::nested(&location, table);
    let target = loaded.configuration.monitoring.alerts.rule_mut(kind);
    rule.read_detailed("enabled", ENABLED_MESSAGE, &mut target.enabled);
    if let Some(threshold) = rule.get("threshold") {
        let maximum = if kind == AlertKind::Temperature {
            MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS
        } else {
            MAX_ALERT_THRESHOLD_PERCENT
        };
        match f64::deserialize(threshold.clone()) {
            Ok(threshold) if threshold.is_finite() && threshold > 0.0 && threshold <= maximum => {
                target.threshold = threshold;
            }
            _ => rule.warn(
                "threshold",
                format!(
                    "Alert threshold must be finite, greater than zero, and no greater than \
                     {maximum}; the default was retained."
                ),
            ),
        }
    }
    rule.in_range(
        "sustain_samples",
        MIN_ALERT_SUSTAIN_SAMPLES..=MAX_ALERT_SUSTAIN_SAMPLES,
        ("Alert sustain samples must be", ""),
        &mut target.sustain_samples,
    );
    if let Some(cooldown) = rule.get("cooldown_seconds") {
        match u64::deserialize(cooldown.clone()) {
            Ok(seconds) if seconds <= MAX_ALERT_COOLDOWN_SECONDS => {
                target.cooldown_seconds = seconds;
            }
            _ => rule.warn(
                "cooldown_seconds",
                format!(
                    "Alert cooldown must be no greater than {MAX_ALERT_COOLDOWN_SECONDS} \
                     seconds; the default was retained."
                ),
            ),
        }
    }
    alerts.absorb(rule);
}

fn alert_kind_name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Cpu => "cpu",
        AlertKind::Temperature => "temperature",
        AlertKind::Memory => "memory",
        AlertKind::Disk => "disk",
        AlertKind::Battery => "battery",
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::MonitorReadout;

    use crate::config::parse;

    #[test]
    fn malformed_monitoring_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
refresh_interval_millis = 500
history_samples = 0
readouts = ["network", "unknown", "network", "cpu"]
[monitoring.alerts.cpu]
enabled = false
threshold = 101.0
sustain_samples = 7
cooldown_seconds = 42
[monitoring.alerts.temperature]
threshold = 125.0
sustain_samples = "bad"
"#,
        )
        .expect("document itself is valid TOML");
        assert_eq!(loaded.configuration.monitoring.refresh_interval_millis, 500);
        assert_eq!(
            loaded.configuration.monitoring.history_samples,
            kestrel_core::DEFAULT_MONITOR_HISTORY_SAMPLES
        );
        assert_eq!(
            loaded.configuration.monitoring.readouts,
            vec![MonitorReadout::Network, MonitorReadout::Cpu]
        );
        assert!(!loaded.configuration.monitoring.alerts.cpu.enabled);
        assert_eq!(loaded.configuration.monitoring.alerts.cpu.threshold, 90.0);
        assert_eq!(
            loaded.configuration.monitoring.alerts.cpu.sustain_samples,
            7
        );
        assert_eq!(
            loaded.configuration.monitoring.alerts.cpu.cooldown_seconds,
            42
        );
        assert_eq!(
            loaded.configuration.monitoring.alerts.temperature.threshold,
            125.0
        );
        assert_eq!(
            loaded
                .configuration
                .monitoring
                .alerts
                .temperature
                .sustain_samples,
            60
        );
        for location in [
            "monitoring.history_samples",
            "monitoring.readouts[1]",
            "monitoring.readouts[2]",
            "monitoring.alerts.cpu.threshold",
            "monitoring.alerts.temperature.sustain_samples",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location)
            );
        }
    }

    #[test]
    fn monitoring_depth_fields_parse_and_fail_independently() {
        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
network_rate_unit = "bits"
per_core_cpu = true
tray_readouts = ["cpu", "battery"]
"#,
        )
        .expect("document itself is valid TOML");
        let monitoring = &loaded.configuration.monitoring;
        assert_eq!(
            monitoring.network_rate_unit,
            kestrel_core::NetworkRateUnit::Bits
        );
        assert!(monitoring.per_core_cpu);
        assert_eq!(
            monitoring.tray_readouts,
            [MonitorReadout::Cpu, MonitorReadout::Battery]
        );
        assert!(loaded.warnings.is_empty());

        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
refresh_interval_millis = 2000
network_rate_unit = "nibbles"
per_core_cpu = "yes"
tray_readouts = ["cpu", "memory", "swap", "disk"]
"#,
        )
        .expect("document itself is valid TOML");
        let monitoring = &loaded.configuration.monitoring;
        assert_eq!(
            monitoring.refresh_interval_millis, 2000,
            "valid keys still apply"
        );
        assert_eq!(
            monitoring.network_rate_unit,
            kestrel_core::NetworkRateUnit::Bytes
        );
        assert!(!monitoring.per_core_cpu);
        assert!(monitoring.tray_readouts.is_empty());
        for location in [
            "monitoring.network_rate_unit",
            "monitoring.per_core_cpu",
            "monitoring.tray_readouts",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
    }
}
