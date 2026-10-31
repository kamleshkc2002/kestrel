//! Monitoring presentation and readouts.

use super::*;
use kestrel_platform::upower::{PeripheralCharge, PeripheralKind};

pub(crate) struct MonitorPresentation<'a> {
    pub snapshot: Option<&'a SystemSnapshot>,
    pub history: Option<HistorySummary>,
    pub alerts: AlertSnapshot,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorViewModel {
    pub running: bool,
    pub status: String,
    pub readouts: Vec<MonitorReadoutViewModel>,
    pub readout_settings: Vec<MonitorReadoutSettingViewModel>,
    pub alerts: Vec<ActiveAlertViewModel>,
    pub delivery_failures: Vec<(AlertKind, String)>,
    pub alert_rules: Vec<AlertRuleViewModel>,
    /// Present when per-core load is enabled and the CPU readout is shown.
    pub cores: Option<Vec<CoreGroupViewModel>>,
    pub accessories: AccessoriesViewModel,
    /// Present only after an explicit request; never stored or exported.
    pub local_addresses: Option<Result<Vec<String>, String>>,
    pub network_rate_unit: kestrel_core::NetworkRateUnit,
    pub per_core_cpu: bool,
    /// One tray item per configured tray readout, in configured order.
    pub tray: Vec<crate::TrayReadout>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoreGroupViewModel {
    pub label: &'static str,
    /// Logical CPU number and load, `None` until a rate exists.
    pub cores: Vec<(u32, Option<f64>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessoriesViewModel {
    pub rows: Vec<AccessoryViewModel>,
    /// Why accessory batteries cannot be read, when they cannot.
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryViewModel {
    /// The hashed UPower identity, never a serial.
    pub id: String,
    pub kind: PeripheralKind,
    pub title: String,
    pub detail: String,
    pub percent: Option<u8>,
    pub charge: PeripheralCharge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorReadoutViewModel {
    pub readout: MonitorReadout,
    pub label: &'static str,
    pub value: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorReadoutSettingViewModel {
    pub readout: MonitorReadout,
    pub label: &'static str,
    pub visible: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    pub in_tray: bool,
    /// False once the tray holds its maximum and this readout is not in it.
    pub can_toggle_tray: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAlertViewModel {
    pub label: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertRuleViewModel {
    pub kind: AlertKind,
    pub label: &'static str,
    pub enabled: bool,
    pub threshold: f64,
    /// How long a reading must stay past the threshold.
    pub sustain: std::time::Duration,
    pub unit: &'static str,
    pub summary: String,
}

impl MonitorViewModel {
    pub(crate) fn from_configuration(
        configuration: &MonitorConfiguration,
        presentation: MonitorPresentation<'_>,
    ) -> Self {
        let snapshot = presentation.snapshot;
        let readouts = configuration
            .readouts
            .iter()
            .copied()
            .map(|readout| monitor_readout(snapshot, readout, configuration))
            .collect();
        // Configured readouts come first; hidden ones follow.
        let visible_count = configuration.readouts.len();
        let mut ordered = configuration.readouts.clone();
        ordered.extend(
            MonitorReadout::ALL
                .into_iter()
                .filter(|readout| !configuration.readouts.contains(readout)),
        );
        let readout_settings = ordered
            .into_iter()
            .map(|readout| {
                let position = configuration
                    .readouts
                    .iter()
                    .position(|entry| *entry == readout);
                let in_tray = configuration.tray_readouts.contains(&readout);
                MonitorReadoutSettingViewModel {
                    readout,
                    label: readout.label(),
                    visible: position.is_some(),
                    can_move_up: position.is_some_and(|position| position > 0),
                    can_move_down: position.is_some_and(|position| position + 1 < visible_count),
                    in_tray,
                    can_toggle_tray: in_tray
                        || configuration.tray_readouts.len() < kestrel_core::MAX_TRAY_READOUTS,
                }
            })
            .collect();
        let alerts = presentation
            .alerts
            .active
            .iter()
            .map(active_alert)
            .collect();
        let delivery_failures = presentation
            .alerts
            .delivery_failures
            .iter()
            .map(|failure| (failure.kind, failure.message.clone()))
            .collect();
        let alert_rules = AlertKind::ALL
            .into_iter()
            .map(|kind| {
                let configured = configuration.alerts.rule(kind);
                let kestrel_core::AlertRuleConfiguration {
                    enabled,
                    threshold,
                    sustain_samples,
                    cooldown_seconds,
                } = *configured;
                // One sample means the first reading past the threshold alerts.
                let sustain = if sustain_samples <= 1 {
                    std::time::Duration::ZERO
                } else {
                    kestrel_core::sustain_duration(
                        sustain_samples,
                        configuration.refresh_interval_millis,
                    )
                };
                let summary = format!(
                    "Alerts when {} {}{} {}; repeats at most every {}",
                    if kind.alerts_above() {
                        "at or above"
                    } else {
                        "at or below"
                    },
                    format_threshold(threshold),
                    kind.unit(),
                    if sustain_samples <= 1 {
                        "on the first reading".to_owned()
                    } else {
                        format!("for {}", format_span(sustain))
                    },
                    format_span(std::time::Duration::from_secs(cooldown_seconds))
                );
                AlertRuleViewModel {
                    kind,
                    label: kind.label(),
                    enabled,
                    threshold,
                    sustain,
                    unit: kind.unit(),
                    summary,
                }
            })
            .collect();
        let status = monitor_status(
            presentation.running,
            snapshot,
            presentation.history.as_ref(),
        );
        let cores = (configuration.per_core_cpu
            && configuration.readouts.contains(&MonitorReadout::Cpu))
        .then(|| snapshot.map(core_groups))
        .flatten()
        .filter(|groups| !groups.is_empty());
        let tray = configuration
            .tray_readouts
            .iter()
            .map(|readout| {
                let view = monitor_readout(snapshot, *readout, configuration);
                let value = view.value.unwrap_or_else(|| "–".to_owned());
                crate::TrayReadout {
                    key: readout.label().to_ascii_lowercase(),
                    label: readout.label(),
                    icon_label: match readout {
                        MonitorReadout::Cpu => "CPU",
                        MonitorReadout::Memory => "MEM",
                        MonitorReadout::Swap => "SWAP",
                        MonitorReadout::Disk => "DISK",
                        MonitorReadout::Network => "NET",
                        MonitorReadout::Temperature => "TEMP",
                        MonitorReadout::Battery => "BAT",
                        MonitorReadout::Gpu => "GPU",
                    },
                    compact: compact_reading(&value),
                    value,
                    detail: view.detail,
                }
            })
            .collect();
        Self {
            running: presentation.running,
            status,
            readouts,
            readout_settings,
            alerts,
            delivery_failures,
            alert_rules,
            cores,
            accessories: snapshot.map(accessories).unwrap_or_default(),
            local_addresses: None,
            network_rate_unit: configuration.network_rate_unit,
            per_core_cpu: configuration.per_core_cpu,
            tray,
        }
    }
    /// Adds addresses fetched on an explicit request.
    pub(crate) fn with_local_addresses(
        mut self,
        addresses: Option<&Result<Vec<kestrel_platform::network_addresses::LocalAddress>, String>>,
    ) -> Self {
        self.local_addresses = addresses.map(|result| {
            result.as_ref().map_err(Clone::clone).map(|addresses| {
                addresses
                    .iter()
                    .map(|entry| {
                        format!(
                            "{} · {}/{}",
                            entry.interface, entry.address, entry.prefix_length
                        )
                    })
                    .collect()
            })
        });
        self
    }
}

/// Shortens a formatted reading to fit a square tray icon.
fn compact_reading(value: &str) -> String {
    if let Some(degrees) = value.strip_suffix("°C") {
        return format!("{degrees}°");
    }
    let Some((number, unit)) = value.split_once(' ') else {
        return value.to_owned();
    };
    let suffix = match unit {
        "bit/s" => "b",
        "kbit/s" => "Kb",
        "Mbit/s" => "Mb",
        "Gbit/s" => "Gb",
        "Tbit/s" => "Tb",
        "B/s" => "B",
        "KiB/s" => "KB",
        "MiB/s" => "MB",
        "GiB/s" => "GB",
        "TiB/s" => "TB",
        _ => return value.to_owned(),
    };
    let number = match number.parse::<f64>() {
        Ok(parsed) if parsed >= 10.0 => format!("{parsed:.0}"),
        _ => number.to_owned(),
    };
    format!("{number}{suffix}")
}

fn core_groups(snapshot: &SystemSnapshot) -> Vec<CoreGroupViewModel> {
    use kestrel_platform::system_monitor::CoreKind;
    let group = |label, kind: Option<CoreKind>| CoreGroupViewModel {
        label,
        cores: snapshot
            .cores
            .iter()
            .filter(|core| core.kind == kind)
            .map(|core| (core.index, core.usage_percent))
            .collect(),
    };
    [
        group("Performance cores", Some(CoreKind::Performance)),
        group("Efficiency cores", Some(CoreKind::Efficiency)),
        group("Cores", None),
    ]
    .into_iter()
    .filter(|group| !group.cores.is_empty())
    .collect()
}

fn accessories(snapshot: &SystemSnapshot) -> AccessoriesViewModel {
    AccessoriesViewModel {
        rows: snapshot
            .peripherals
            .iter()
            .map(|peripheral| AccessoryViewModel {
                id: peripheral.id.clone(),
                kind: peripheral.kind,
                title: match &peripheral.model {
                    Some(model) => format!("{} · {model}", peripheral.kind.label()),
                    None => peripheral.kind.label().to_owned(),
                },
                detail: match peripheral.charge {
                    PeripheralCharge::Charging => "Charging",
                    PeripheralCharge::Discharging => "On battery",
                    PeripheralCharge::Full => "Fully charged",
                    PeripheralCharge::Unknown => "Charge state unknown",
                }
                .to_owned(),
                percent: peripheral.percentage,
                charge: peripheral.charge,
            })
            .collect(),
        unavailable: snapshot
            .issues
            .iter()
            .find(|issue| issue.source == "UPower")
            .map(|issue| issue.reason.clone()),
    }
}

/// What the accessory batteries module needs to show a device.
pub fn accessory_module_status(monitor: &MonitorViewModel) -> &str {
    if !monitor.running {
        return "Needs Monitoring, which reads accessory batteries.";
    }
    monitor.accessories.unavailable.as_deref().unwrap_or(
        "While a device runs low, and briefly when one connects or starts or stops charging.",
    )
}

/// The icon for an accessory's kind.
pub fn accessory_icon(kind: PeripheralKind) -> &'static str {
    match kind {
        PeripheralKind::Mouse => "input-mouse-symbolic",
        PeripheralKind::Keyboard => "input-keyboard-symbolic",
        PeripheralKind::Headset => "audio-headset-symbolic",
        PeripheralKind::Headphones => "audio-headphones-symbolic",
        PeripheralKind::Speakers => "audio-speakers-symbolic",
        PeripheralKind::GamingInput => "input-gaming-symbolic",
        PeripheralKind::Pen | PeripheralKind::Tablet => "input-tablet-symbolic",
        PeripheralKind::Touchpad => "input-touchpad-symbolic",
        PeripheralKind::Phone => "phone-symbolic",
        PeripheralKind::Other => "battery-symbolic",
    }
}

fn format_threshold(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// A whole span in the largest unit that divides it: "45 s", "2 min", "1 h".
fn format_span(span: std::time::Duration) -> String {
    let seconds = span.as_secs();
    if seconds >= 3_600 && seconds.is_multiple_of(3_600) {
        format!("{} h", seconds / 3_600)
    } else if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else if seconds > 0 {
        format!("{seconds} s")
    } else {
        format!("{} ms", span.as_millis())
    }
}

fn monitor_status(
    running: bool,
    snapshot: Option<&SystemSnapshot>,
    history: Option<&HistorySummary>,
) -> String {
    if snapshot.is_none() {
        return if running {
            "Monitoring is unavailable: no system snapshot has been sampled.".to_owned()
        } else {
            "Monitoring is unavailable: the monitor service is not running.".to_owned()
        };
    }
    if !running {
        return "Monitoring is unavailable: the monitor service is not running.".to_owned();
    }
    history
        .map(|summary| {
            format!(
                "{} samples retained over {} s.",
                summary.samples,
                summary.span.as_secs()
            )
        })
        .unwrap_or_else(|| "Monitoring is running; history is not available yet.".to_owned())
}

fn active_alert(alert: &ActiveAlert) -> ActiveAlertViewModel {
    let value = format!("{:.0}", alert.value);
    let subject = alert
        .subject
        .as_deref()
        .map(|subject| format!(" ({subject})"))
        .unwrap_or_default();
    ActiveAlertViewModel {
        label: alert.kind.label(),
        message: format!(
            "{}{} is {}{}",
            alert.kind.label(),
            subject,
            value,
            alert.kind.unit()
        ),
    }
}

/// A network rate in the configured unit.
fn format_rate(bytes_per_second: f64, unit: kestrel_core::NetworkRateUnit) -> String {
    match unit {
        kestrel_core::NetworkRateUnit::Bytes => {
            format!("{}/s", kestrel_core::format_bytes(bytes_per_second))
        }
        kestrel_core::NetworkRateUnit::Bits => kestrel_core::scaled(
            bytes_per_second * 8.0,
            1000.0,
            &["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"],
        ),
    }
}

fn percentage(value: f64) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

fn temperature(value: f64) -> String {
    format!("{:.0}°C", value)
}

fn issue_reason(snapshot: &SystemSnapshot, readout: MonitorReadout) -> Option<String> {
    // Match exact platform sources to avoid cross-family errors.
    let matches = |source: &str| match readout {
        MonitorReadout::Cpu => source == "/proc/stat",
        MonitorReadout::Memory | MonitorReadout::Swap => source == "/proc/meminfo",
        MonitorReadout::Network => source == "/proc/net/dev",
        MonitorReadout::Temperature => source == "/sys/class/hwmon + /sys/class/thermal",
        MonitorReadout::Battery => source == "/sys/class/power_supply",
        MonitorReadout::Disk => source.starts_with("/proc/self/mounts"),
        MonitorReadout::Gpu => source == "/sys/class/drm",
    };
    snapshot
        .issues
        .iter()
        .find(|issue| matches(&issue.source))
        .map(|issue| issue.reason.clone())
}

fn unavailable(
    snapshot: Option<&SystemSnapshot>,
    readout: MonitorReadout,
    fallback: &str,
) -> MonitorReadoutViewModel {
    let detail = snapshot
        .and_then(|snapshot| issue_reason(snapshot, readout))
        .unwrap_or_else(|| fallback.to_owned());
    MonitorReadoutViewModel {
        readout,
        label: readout.label(),
        value: None,
        detail,
    }
}

fn monitor_readout(
    snapshot: Option<&SystemSnapshot>,
    readout: MonitorReadout,
    configuration: &MonitorConfiguration,
) -> MonitorReadoutViewModel {
    let Some(snapshot) = snapshot else {
        return unavailable(None, readout, "No system snapshot is available yet.");
    };
    match readout {
        MonitorReadout::Cpu => snapshot
            .cpu_usage_percent
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .logical_cpus
                    .map(|cpus| format!("{cpus} logical CPUs"))
                    .unwrap_or_else(|| "Current CPU utilization".to_owned()),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "CPU usage is unavailable.")),
        MonitorReadout::Memory => snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.used_percent)
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .memory
                    .as_ref()
                    .map(|memory| {
                        format!(
                            "{} used of {}",
                            format_bytes(memory.used_bytes as f64),
                            format_bytes(memory.total_bytes as f64)
                        )
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "Memory usage is unavailable.")
            }),
        MonitorReadout::Swap => snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.swap_used_percent)
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: snapshot
                    .memory
                    .as_ref()
                    .map(|memory| {
                        format!(
                            "{} used of {}",
                            format_bytes(memory.swap_used_bytes as f64),
                            format_bytes(memory.swap_total_bytes as f64)
                        )
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "Swap usage is unavailable.")),
        MonitorReadout::Disk => snapshot
            .disk_usage
            .iter()
            .filter_map(|disk| disk.used_percent.map(|value| (disk, value)))
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(disk, value)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value)),
                detail: format!("{} mounted at {}", disk.filesystem, disk.mount_point),
            })
            .unwrap_or_else(|| unavailable(Some(snapshot), readout, "Disk usage is unavailable.")),
        MonitorReadout::Network => {
            let rates = snapshot
                .network
                .iter()
                .flat_map(|network| {
                    [
                        network.receive_bytes_per_second,
                        network.transmit_bytes_per_second,
                    ]
                })
                .flatten()
                .collect::<Vec<_>>();
            let rate = rates.iter().copied().fold(0.0, f64::max);
            if !rates.is_empty() {
                let mut detail = format!("{} interfaces", snapshot.network.len());
                if let Some(session) = snapshot.network_session {
                    detail.push_str(&format!(
                        " · {} down, {} up since monitoring started",
                        format_bytes(session.received_bytes as f64),
                        format_bytes(session.transmitted_bytes as f64)
                    ));
                }
                MonitorReadoutViewModel {
                    readout,
                    label: readout.label(),
                    value: Some(format_rate(rate, configuration.network_rate_unit)),
                    detail,
                }
            } else {
                unavailable(Some(snapshot), readout, "Network rate is unavailable.")
            }
        }
        MonitorReadout::Temperature => snapshot
            .temperatures
            .iter()
            .max_by_key(|reading| reading.millidegrees_celsius)
            .map(|reading| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(temperature(reading.millidegrees_celsius as f64 / 1000.0)),
                detail: reading.label.clone(),
            })
            .unwrap_or_else(|| {
                unavailable(
                    Some(snapshot),
                    readout,
                    "Temperature readings are unavailable.",
                )
            }),
        MonitorReadout::Battery => snapshot
            .power_supplies
            .iter()
            .filter(|supply| {
                supply
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("battery"))
                    && !supply
                        .scope
                        .as_deref()
                        .is_some_and(|scope| scope.eq_ignore_ascii_case("device"))
            })
            .filter_map(|supply| supply.capacity_percent.map(|capacity| (supply, capacity)))
            .min_by_key(|(_, capacity)| *capacity)
            .map(|(supply, capacity)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(capacity as f64)),
                detail: match power_source(snapshot, supply.status.as_deref()) {
                    Some(state) => format!("{} · {state}", supply.name),
                    None => supply.name.clone(),
                },
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "Battery capacity is unavailable.")
            }),
        MonitorReadout::Gpu => snapshot
            .gpu
            .iter()
            .filter_map(|gpu| gpu.busy_percent.map(|busy| (gpu, busy)))
            .max_by_key(|(_, busy)| *busy)
            .map(|(gpu, value)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value as f64)),
                detail: {
                    let name = gpu
                        .label
                        .clone()
                        .unwrap_or_else(|| "GPU activity".to_owned());
                    match (gpu.memory_used_bytes, gpu.memory_total_bytes) {
                        (Some(used), Some(total)) if total > 0 => format!(
                            "{name} · {} of {} memory",
                            format_bytes(used as f64),
                            format_bytes(total as f64)
                        ),
                        _ => name,
                    }
                },
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "GPU activity is unavailable.")
            }),
    }
}

/// Charging state with the adapter's role; a connected adapter with charging
/// paused counts as external power.
fn power_source(snapshot: &SystemSnapshot, status: Option<&str>) -> Option<&'static str> {
    let adapter_online = snapshot.power_supplies.iter().any(|supply| {
        supply
            .kind
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("mains"))
            && supply.online == Some(true)
    });
    let status = status?.to_ascii_lowercase();
    Some(match status.as_str() {
        "charging" => "Charging",
        "discharging" => "On battery",
        "full" if adapter_online => "Fully charged, on external power",
        "full" => "Fully charged",
        "not charging" if adapter_online => "On external power, charging paused",
        "not charging" => "Not charging",
        _ if adapter_online => "On external power",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitoring_configured_order_and_hidden_readouts_are_honoured() {
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Gpu, MonitorReadout::Cpu],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
            },
        );
        assert_eq!(
            monitor
                .readouts
                .iter()
                .map(|readout| readout.readout)
                .collect::<Vec<_>>(),
            vec![MonitorReadout::Gpu, MonitorReadout::Cpu]
        );
        assert!(
            !monitor
                .readout_settings
                .iter()
                .find(|setting| setting.readout == MonitorReadout::Memory)
                .expect("all settings are represented")
                .visible
        );
        // Configured rows precede hidden rows; arrows follow that order.
        let settings = &monitor.readout_settings;
        assert_eq!(settings[0].readout, MonitorReadout::Gpu);
        assert_eq!(settings[1].readout, MonitorReadout::Cpu);
        assert!(!settings[0].can_move_up && settings[0].can_move_down);
        assert!(settings[1].can_move_up && !settings[1].can_move_down);
        assert_eq!(settings.len(), MonitorReadout::ALL.len());
        for hidden in settings.iter().skip(2) {
            assert!(!hidden.visible && !hidden.can_move_up && !hidden.can_move_down);
        }
    }

    #[test]
    fn rates_switch_at_the_rounded_boundary() {
        for (value, unit, expected) in [
            (125_000.0, kestrel_core::NetworkRateUnit::Bits, "1.0 Mbit/s"),
            (124.9, kestrel_core::NetworkRateUnit::Bits, "999 bit/s"),
            (2048.0, kestrel_core::NetworkRateUnit::Bytes, "2.0 KiB/s"),
        ] {
            assert_eq!(super::format_rate(value, unit), expected);
        }
    }

    #[test]
    fn monitor_depth_reads_power_source_session_totals_cores_and_tray() {
        use kestrel_platform::system_monitor::CoreKind;
        use kestrel_services::system_monitor::{
            CoreUsage, NetworkSessionTotals, NetworkSnapshot, PowerSupplySnapshot, SystemSnapshot,
        };
        use std::time::Duration;

        let supply = |name: &str, kind: &str, status: Option<&str>, online| PowerSupplySnapshot {
            name: name.to_owned(),
            kind: Some(kind.to_owned()),
            scope: None,
            capacity_percent: (kind == "Battery").then_some(80),
            status: status.map(str::to_owned),
            health: None,
            cycle_count: None,
            energy_now_microwatt_hours: None,
            energy_full_microwatt_hours: None,
            power_microwatts: None,
            remaining_seconds: None,
            online,
        };
        let core = |index, kind| CoreUsage {
            index,
            kind,
            usage_percent: Some(f64::from(index) * 10.0),
        };
        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(5),
            cpu_usage_percent: Some(12.0),
            logical_cpus: Some(3),
            cores: vec![
                core(0, Some(CoreKind::Performance)),
                core(1, Some(CoreKind::Efficiency)),
                core(2, Some(CoreKind::Efficiency)),
            ],
            network_session: Some(NetworkSessionTotals {
                received_bytes: 3 * 1024 * 1024,
                transmitted_bytes: 512,
                since: Duration::ZERO,
            }),
            peripherals: Vec::new(),
            memory: None,
            disk_usage: Vec::new(),
            network: vec![NetworkSnapshot {
                interface: "eth0".to_owned(),
                received_bytes: 0,
                transmitted_bytes: 0,
                receive_bytes_per_second: Some(125_000.0),
                transmit_bytes_per_second: Some(10.0),
            }],
            temperatures: Vec::new(),
            power_supplies: vec![
                supply("ADP0", "Mains", None, Some(true)),
                supply("BAT0", "Battery", Some("Not charging"), None),
            ],
            gpu: Vec::new(),
            issues: Vec::new(),
        };
        let configuration = MonitorConfiguration {
            readouts: vec![
                MonitorReadout::Cpu,
                MonitorReadout::Network,
                MonitorReadout::Battery,
            ],
            network_rate_unit: kestrel_core::NetworkRateUnit::Bits,
            per_core_cpu: true,
            tray_readouts: vec![MonitorReadout::Network, MonitorReadout::Battery],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: Some(&snapshot),
                history: None,
                alerts: Default::default(),
                running: true,
            },
        );

        assert_eq!(monitor.readouts[1].value.as_deref(), Some("1.0 Mbit/s"));
        assert!(
            monitor.readouts[1]
                .detail
                .contains("3.0 MiB down, 512 B up since monitoring started")
        );
        assert_eq!(
            monitor.readouts[2].detail,
            "BAT0 · On external power, charging paused"
        );
        let groups = monitor
            .cores
            .expect("per-core load is enabled")
            .iter()
            .map(|group| (group.label, group.cores.len()))
            .collect::<Vec<_>>();
        assert_eq!(groups, [("Performance cores", 1), ("Efficiency cores", 2)]);
        let tray = monitor
            .tray
            .iter()
            .map(|item| {
                (
                    item.key.as_str(),
                    item.value.as_str(),
                    item.compact.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tray,
            [
                ("network", "1.0 Mbit/s", "1.0Mb"),
                ("battery", "80%", "80%"),
            ],
            "one tray item per readout, in configured order"
        );
        assert_eq!(
            monitor.tray[1].detail,
            "BAT0 · On external power, charging paused"
        );
        assert_eq!(monitor.local_addresses, None, "addresses need a request");

        let module_status = |running, snapshot: &SystemSnapshot| {
            let monitor = MonitorViewModel::from_configuration(
                &configuration,
                MonitorPresentation {
                    snapshot: Some(snapshot),
                    history: None,
                    alerts: Default::default(),
                    running,
                },
            );
            accessory_module_status(&monitor).to_owned()
        };
        let mut without_upower = snapshot.clone();
        without_upower
            .issues
            .push(kestrel_platform::system_monitor::SourceIssue {
                source: "UPower".to_owned(),
                reason: "UPower is not running.".to_owned(),
            });
        let normal = module_status(true, &snapshot);
        let needs_monitor = module_status(false, &snapshot);
        let unavailable = module_status(true, &without_upower);
        assert_eq!(unavailable, "UPower is not running.");
        assert_ne!(normal, needs_monitor);
        assert_ne!(normal, unavailable);
    }

    #[test]
    fn tray_icon_readings_fit_a_square_icon() {
        for (reading, compact) in [
            ("12%", "12%"),
            ("54°C", "54°"),
            ("1.6 Mbit/s", "1.6Mb"),
            ("12.3 Mbit/s", "12Mb"),
            ("999 bit/s", "999b"),
            ("512 B/s", "512B"),
            ("2.0 KiB/s", "2.0KB"),
            ("–", "–"),
        ] {
            assert_eq!(super::compact_reading(reading), compact, "{reading}");
        }
    }

    #[test]
    fn unavailable_readout_never_borrows_another_family_reason() {
        use kestrel_platform::system_monitor::SourceIssue;
        use kestrel_services::system_monitor::SystemSnapshot;
        use std::time::Duration;

        // Exact source matching prevents the disk source from explaining CPU.
        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(1),
            cpu_usage_percent: None,
            logical_cpus: None,
            cores: Vec::new(),
            network_session: None,
            peripherals: Vec::new(),
            memory: None,
            disk_usage: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: vec![SourceIssue {
                source: "/proc/self/mounts + statvfs".to_owned(),
                reason: "no mounted filesystem is readable".to_owned(),
            }],
        };
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Cpu, MonitorReadout::Disk],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: Some(&snapshot),
                history: None,
                alerts: Default::default(),
                running: true,
            },
        );

        assert_eq!(monitor.readouts[0].value, None);
        assert!(!monitor.readouts[0].detail.contains("disk"));
        assert_eq!(
            monitor.readouts[1].detail,
            "no mounted filesystem is readable"
        );
    }

    #[test]
    fn monitoring_unavailable_readout_keeps_source_reason() {
        use kestrel_platform::system_monitor::SourceIssue;
        use kestrel_services::system_monitor::SystemSnapshot;
        use std::time::Duration;

        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(1),
            cpu_usage_percent: None,
            logical_cpus: None,
            cores: Vec::new(),
            network_session: None,
            peripherals: Vec::new(),
            memory: None,
            disk_usage: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: vec![SourceIssue {
                source: "/proc/meminfo".to_owned(),
                reason: "MemAvailable is missing".to_owned(),
            }],
        };
        let configuration = MonitorConfiguration {
            readouts: vec![MonitorReadout::Memory],
            ..MonitorConfiguration::default()
        };
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: Some(&snapshot),
                history: None,
                alerts: Default::default(),
                running: true,
            },
        );
        assert_eq!(monitor.readouts[0].value, None);
        assert_eq!(monitor.readouts[0].detail, "MemAvailable is missing");
    }

    #[test]
    fn active_alerts_and_rules_map_labels_units_and_threshold_summary() {
        use kestrel_services::alerts::{ActiveAlert, AlertSnapshot};
        use std::time::Duration;

        let mut configuration = MonitorConfiguration {
            refresh_interval_millis: 2_000,
            ..MonitorConfiguration::default()
        };
        configuration.alerts.temperature.threshold = 80.0;
        configuration.alerts.temperature.sustain_samples = 30;
        configuration.alerts.temperature.cooldown_seconds = 120;
        configuration.alerts.battery.sustain_samples = 1;
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: AlertSnapshot {
                    active: vec![ActiveAlert {
                        kind: AlertKind::Temperature,
                        value: 91.0,
                        subject: Some("CPU package".to_owned()),
                    }],
                    delivery_failures: Vec::new(),
                },
                running: true,
            },
        );
        assert_eq!(monitor.alerts[0].label, "Temperature");
        assert_eq!(monitor.alert_rules[1].unit, "°C");
        assert!(monitor.alert_rules[1].summary.contains("80°C"));
        let temperature = &monitor.alert_rules[1];
        assert_eq!(
            temperature.sustain,
            Duration::from_secs(60),
            "30 samples at 2 s"
        );
        assert!(temperature.summary.contains("for 1 min"));
        assert!(temperature.summary.contains("every 2 min"));
        let battery = &monitor.alert_rules[4];
        assert_eq!(battery.sustain, Duration::ZERO, "one sample alerts at once");
        assert!(battery.summary.contains("on the first reading"));
    }

    #[test]
    fn empty_snapshot_yields_explicit_unavailable_state() {
        let monitor = MonitorViewModel::from_configuration(
            &MonitorConfiguration::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
            },
        );
        assert!(!monitor.running);
        assert!(monitor.status.contains("unavailable"));
        assert!(
            monitor
                .readouts
                .iter()
                .all(|readout| readout.value.is_none())
        );
    }
}
