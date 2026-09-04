//! Read-only Linux system-monitor sources.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "system.monitor";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIssue {
    pub source: String,
    pub reason: String,
}

impl SourceIssue {
    fn new(source: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metric<T> {
    Available(T),
    Unavailable(SourceIssue),
}

impl<T> Metric<T> {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuCounters {
    pub total: u64,
    pub idle: u64,
    pub logical_cpus: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCounters {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkCounters {
    pub interface: String,
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemperatureReading {
    pub stable_key: String,
    pub label: String,
    pub millidegrees_celsius: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSupplyReading {
    pub name: String,
    pub kind: Option<String>,
    pub capacity_percent: Option<u8>,
    pub status: Option<String>,
    pub power_microwatts: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSystemSample {
    pub cpu: Metric<CpuCounters>,
    pub memory: Metric<MemoryCounters>,
    pub network: Metric<Vec<NetworkCounters>>,
    pub temperatures: Metric<Vec<TemperatureReading>>,
    pub power_supplies: Metric<Vec<PowerSupplyReading>>,
}

pub trait SystemMonitorSource {
    fn sample(&mut self) -> RawSystemSample;
}

#[derive(Debug, Clone)]
pub struct ProcSysMonitor {
    proc_root: PathBuf,
    sys_root: PathBuf,
}

impl Default for ProcSysMonitor {
    fn default() -> Self {
        Self::new("/proc", "/sys")
    }
}

impl ProcSysMonitor {
    pub fn new(proc_root: impl Into<PathBuf>, sys_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            sys_root: sys_root.into(),
        }
    }

    fn read_sample(&self) -> RawSystemSample {
        RawSystemSample {
            cpu: self.read_cpu(),
            memory: self.read_memory(),
            network: self.read_network(),
            temperatures: self.read_temperatures(),
            power_supplies: self.read_power_supplies(),
        }
    }

    fn read_cpu(&self) -> Metric<CpuCounters> {
        let source = "/proc/stat";
        let text = match read_text(&self.proc_root.join("stat"), source) {
            Ok(text) => text,
            Err(issue) => return Metric::Unavailable(issue),
        };
        let mut lines = text.lines().filter(|line| line.starts_with("cpu"));
        let Some(aggregate) = lines.next() else {
            return Metric::Unavailable(SourceIssue::new(source, "aggregate CPU line is missing"));
        };
        let values = match parse_u64_fields(aggregate.split_whitespace().skip(1), source) {
            Ok(values) if values.len() >= 4 => values,
            _ => {
                return Metric::Unavailable(SourceIssue::new(
                    source,
                    "aggregate CPU counters are malformed",
                ))
            }
        };
        let total = values.iter().copied().fold(0_u64, u64::saturating_add);
        let idle = values[3].saturating_add(values.get(4).copied().unwrap_or(0));
        Metric::Available(CpuCounters {
            total,
            idle,
            logical_cpus: lines.filter(|line| cpu_line_is_numbered(line)).count(),
        })
    }

    fn read_memory(&self) -> Metric<MemoryCounters> {
        let source = "/proc/meminfo";
        let text = match read_text(&self.proc_root.join("meminfo"), source) {
            Ok(text) => text,
            Err(issue) => return Metric::Unavailable(issue),
        };
        let fields = parse_memory_fields(&text);
        let Some(total_bytes) = fields.get("MemTotal").copied() else {
            return Metric::Unavailable(SourceIssue::new(source, "MemTotal is missing"));
        };
        let Some(available_bytes) = fields.get("MemAvailable").copied() else {
            return Metric::Unavailable(SourceIssue::new(source, "MemAvailable is missing"));
        };
        Metric::Available(MemoryCounters {
            total_bytes,
            available_bytes,
            swap_total_bytes: fields.get("SwapTotal").copied().unwrap_or(0),
            swap_free_bytes: fields.get("SwapFree").copied().unwrap_or(0),
        })
    }

    fn read_network(&self) -> Metric<Vec<NetworkCounters>> {
        let source = "/proc/net/dev";
        let text = match read_text(&self.proc_root.join("net/dev"), source) {
            Ok(text) => text,
            Err(issue) => return Metric::Unavailable(issue),
        };
        let interfaces = text
            .lines()
            .skip(2)
            .filter_map(|line| {
                let (name, counters) = line.split_once(':')?;
                let values = parse_u64_fields(counters.split_whitespace(), source).ok()?;
                (values.len() >= 16).then(|| NetworkCounters {
                    interface: name.trim().to_string(),
                    received_bytes: values[0],
                    transmitted_bytes: values[8],
                })
            })
            .collect::<Vec<_>>();
        if interfaces.is_empty() {
            Metric::Unavailable(SourceIssue::new(
                source,
                "no network interfaces were parsed",
            ))
        } else {
            Metric::Available(interfaces)
        }
    }

    fn read_temperatures(&self) -> Metric<Vec<TemperatureReading>> {
        let mut readings = self.read_hwmon_temperatures();
        readings.extend(self.read_thermal_zone_temperatures());
        readings.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
        readings.dedup_by(|left, right| left.stable_key == right.stable_key);
        if readings.is_empty() {
            Metric::Unavailable(SourceIssue::new(
                "/sys/class/hwmon + /sys/class/thermal",
                "no temperature sensor with a stable device-qualified identity is readable",
            ))
        } else {
            Metric::Available(readings)
        }
    }

    fn read_hwmon_temperatures(&self) -> Vec<TemperatureReading> {
        let class_root = self.sys_root.join("class/hwmon");
        let mut readings = Vec::new();
        for chip in sorted_entries(&class_root) {
            let Some(chip_name) = read_optional(&chip.join("name")) else {
                continue;
            };
            let Some(device) = self.canonical_device_identity(&chip) else {
                continue;
            };
            for input in sorted_entries(&chip) {
                let filename = input
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                let Some(base) = filename.strip_suffix("_input") else {
                    continue;
                };
                if !base.starts_with("temp") {
                    continue;
                }
                let Ok(value) = read_i64(&input) else {
                    continue;
                };
                let label = read_optional(&chip.join(format!("{base}_label")))
                    .unwrap_or_else(|| base.to_string());
                readings.push(TemperatureReading {
                    stable_key: format!("hwmon/{device}/{chip_name}/{base}/{label}"),
                    label,
                    millidegrees_celsius: value,
                });
            }
        }
        readings
    }

    fn read_thermal_zone_temperatures(&self) -> Vec<TemperatureReading> {
        let class_root = self.sys_root.join("class/thermal");
        let mut readings = Vec::new();
        for zone in sorted_entries(&class_root) {
            let filename = zone
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !filename.starts_with("thermal_zone") {
                continue;
            }
            let Some(kind) = read_optional(&zone.join("type")) else {
                continue;
            };
            let Some(device) = self.canonical_device_identity(&zone) else {
                continue;
            };
            let Ok(value) = read_i64(&zone.join("temp")) else {
                continue;
            };
            readings.push(TemperatureReading {
                stable_key: format!("thermal/{device}/{kind}"),
                label: kind,
                millidegrees_celsius: value,
            });
        }
        readings
    }

    fn canonical_device_identity(&self, class_entry: &Path) -> Option<String> {
        let device = fs::canonicalize(class_entry.join("device")).ok()?;
        let sys_root = fs::canonicalize(&self.sys_root).ok()?;
        let relative = device.strip_prefix(sys_root).ok()?;
        let identity = relative
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        (!identity.is_empty()).then_some(identity)
    }

    fn read_power_supplies(&self) -> Metric<Vec<PowerSupplyReading>> {
        let root = self.sys_root.join("class/power_supply");
        let supplies = sorted_entries(&root)
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_string();
                Some(PowerSupplyReading {
                    name,
                    kind: read_optional(&path.join("type")),
                    capacity_percent: read_optional(&path.join("capacity"))
                        .and_then(|value| value.parse().ok()),
                    status: read_optional(&path.join("status")),
                    power_microwatts: read_optional(&path.join("power_now"))
                        .and_then(|value| value.parse().ok()),
                })
            })
            .collect::<Vec<_>>();
        if supplies.is_empty() {
            Metric::Unavailable(SourceIssue::new(
                "/sys/class/power_supply",
                "no power supplies are exposed",
            ))
        } else {
            Metric::Available(supplies)
        }
    }
}

impl SystemMonitorSource for ProcSysMonitor {
    fn sample(&mut self) -> RawSystemSample {
        self.read_sample()
    }
}

impl CapabilityProbe for ProcSysMonitor {
    fn probe(&self) -> CapabilityReport {
        let sample = self.read_sample();
        let core_available = [
            sample.cpu.is_available(),
            sample.memory.is_available(),
            sample.network.is_available(),
        ];
        let core_count = core_available
            .into_iter()
            .filter(|available| *available)
            .count();
        let optional_count = [
            sample.temperatures.is_available(),
            sample.power_supplies.is_available(),
        ]
        .into_iter()
        .filter(|available| *available)
        .count();
        let status = if core_count == 3 && optional_count == 2 {
            CapabilityStatus::Supported
        } else if core_count > 0 {
            CapabilityStatus::Limited {
                reason: "Some monitor sources are unavailable.".to_string(),
            }
        } else {
            CapabilityStatus::Unsupported {
                reason: "No core procfs monitor sources are readable.".to_string(),
            }
        };
        let mut report = CapabilityReport::new(
            FEATURE_ID,
            status,
            format!(
                "{core_count}/3 core and {optional_count}/2 optional monitor source families are available."
            ),
        )
        .with_selected_backend("/proc + /sys")
        .with_evidence(CapabilityEvidence::new(
            "core_source_families_available",
            core_count.to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "optional_source_families_available",
            optional_count.to_string(),
        ));
        if core_count < 3 || optional_count < 2 {
            report = report.with_remediation(
                "Unavailable metrics remain hidden; verify procfs/sysfs mounts and kernel hardware drivers.",
            );
        }
        report
    }
}

fn read_text(path: &Path, source: &str) -> Result<String, SourceIssue> {
    fs::read_to_string(path)
        .map_err(|error| SourceIssue::new(source, format!("read failed: {}", error.kind())))
}

fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_i64(path: &Path) -> Result<i64, ()> {
    read_optional(path).ok_or(())?.parse().map_err(|_| ())
}

fn parse_u64_fields<'a>(
    fields: impl Iterator<Item = &'a str>,
    source: &str,
) -> Result<Vec<u64>, SourceIssue> {
    fields
        .map(|field| {
            field
                .parse()
                .map_err(|_| SourceIssue::new(source, "numeric counter is malformed"))
        })
        .collect()
}

fn cpu_line_is_numbered(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .and_then(|name| name.strip_prefix("cpu"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|char| char.is_ascii_digit())
        })
}

fn parse_memory_fields(text: &str) -> BTreeMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let kibibytes = value.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((key.to_string(), kibibytes.saturating_mul(1024)))
        })
        .collect()
}

fn sorted_entries(path: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::TempDir;

    use super::{Metric, ProcSysMonitor, SystemMonitorSource};

    fn fixture() -> (TempDir, ProcSysMonitor) {
        let root = TempDir::new().expect("temporary fixture");
        let proc_root = root.path().join("proc");
        let sys_root = root.path().join("sys");
        fs::create_dir_all(proc_root.join("net")).expect("proc fixture");
        fs::create_dir_all(sys_root.join("class/hwmon/hwmon7")).expect("hwmon fixture");
        fs::create_dir_all(sys_root.join("devices/platform/coretemp")).expect("device fixture");
        fs::create_dir_all(sys_root.join("class/power_supply/BAT0")).expect("power fixture");
        fs::write(
            proc_root.join("stat"),
            "cpu  10 0 5 80 5 0 0 0\ncpu0 5 0 2 40 2 0 0 0\ncpu1 5 0 3 40 3 0 0 0\n",
        )
        .expect("stat fixture");
        fs::write(
            proc_root.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 400 kB\nSwapTotal: 200 kB\nSwapFree: 50 kB\n",
        )
        .expect("memory fixture");
        fs::write(
            proc_root.join("net/dev"),
            "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n  lo: 100 1 0 0 0 0 0 0 200 1 0 0 0 0 0 0\n",
        )
        .expect("network fixture");
        let hwmon = sys_root.join("class/hwmon/hwmon7");
        fs::write(hwmon.join("name"), "coretemp\n").expect("chip name");
        fs::write(hwmon.join("temp1_input"), "42000\n").expect("temperature");
        fs::write(hwmon.join("temp1_label"), "Package id 0\n").expect("label");
        symlink("../../../devices/platform/coretemp", hwmon.join("device")).expect("device link");
        let battery = sys_root.join("class/power_supply/BAT0");
        fs::write(battery.join("type"), "Battery\n").expect("power type");
        fs::write(battery.join("capacity"), "87\n").expect("capacity");
        (root, ProcSysMonitor::new(proc_root, sys_root))
    }

    #[test]
    fn parses_independent_proc_and_sys_sources() {
        let (_root, mut monitor) = fixture();
        let sample = monitor.sample();

        assert!(matches!(sample.cpu, Metric::Available(ref cpu) if cpu.logical_cpus == 2));
        assert!(matches!(
            sample.memory,
            Metric::Available(ref memory) if memory.available_bytes == 409_600
        ));
        assert!(matches!(
            sample.network,
            Metric::Available(ref interfaces) if interfaces[0].transmitted_bytes == 200
        ));
        assert!(matches!(
            sample.power_supplies,
            Metric::Available(ref supplies) if supplies[0].capacity_percent == Some(87)
        ));
    }

    #[test]
    fn sensor_key_uses_canonical_device_and_never_hwmon_index() {
        let (_root, mut monitor) = fixture();
        let sample = monitor.sample();
        let Metric::Available(temperatures) = sample.temperatures else {
            panic!("temperature fixture must be available");
        };

        assert_eq!(
            temperatures[0].stable_key,
            "hwmon/devices/platform/coretemp/coretemp/temp1/Package id 0"
        );
        assert!(!temperatures[0].stable_key.contains("hwmon7"));
    }

    #[test]
    fn missing_sources_are_independent_unavailable_metrics() {
        let root = TempDir::new().expect("temporary fixture");
        let mut monitor = ProcSysMonitor::new(root.path().join("proc"), root.path().join("sys"));
        let sample = monitor.sample();

        assert!(matches!(sample.cpu, Metric::Unavailable(_)));
        assert!(matches!(sample.memory, Metric::Unavailable(_)));
        assert!(matches!(sample.network, Metric::Unavailable(_)));
        assert!(matches!(sample.temperatures, Metric::Unavailable(_)));
        assert!(matches!(sample.power_supplies, Metric::Unavailable(_)));
    }
}
