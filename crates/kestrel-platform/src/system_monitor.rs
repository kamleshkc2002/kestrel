//! Read-only Linux system-monitor sources.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs,
    path::{Path, PathBuf},
};

use libc::statvfs;

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};

pub const MAX_FILESYSTEMS: usize = 16;
pub const MAX_DISK_DEVICES: usize = 16;
pub const MAX_GPUS: usize = 8;

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
    pub cached_bytes: u64,
    pub buffers_bytes: u64,
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
    pub scope: Option<String>,
    pub capacity_percent: Option<u8>,
    pub status: Option<String>,
    pub health: Option<String>,
    pub cycle_count: Option<u32>,
    pub energy_now_microwatt_hours: Option<u64>,
    pub energy_full_microwatt_hours: Option<u64>,
    pub power_microwatts: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemUsage {
    pub device: String,
    pub mount_point: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCounters {
    pub device: String,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuReading {
    pub device: String,
    pub busy_percent: Option<u8>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSystemSample {
    pub cpu: Metric<CpuCounters>,
    pub memory: Metric<MemoryCounters>,
    pub network: Metric<Vec<NetworkCounters>>,
    pub temperatures: Metric<Vec<TemperatureReading>>,
    pub power_supplies: Metric<Vec<PowerSupplyReading>>,
    pub disk_usage: Metric<Vec<FilesystemUsage>>,
    pub disk_io: Metric<Vec<DiskCounters>>,
    pub gpu: Metric<Vec<GpuReading>>,
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
            disk_usage: self.read_disk_usage(),
            disk_io: self.read_disk_io(),
            gpu: self.read_gpu(),
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
                ));
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
            cached_bytes: fields.get("Cached").copied().unwrap_or(0),
            buffers_bytes: fields.get("Buffers").copied().unwrap_or(0),
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
                    scope: read_optional(&path.join("scope")),
                    capacity_percent: read_optional(&path.join("capacity"))
                        .and_then(|value| value.parse::<u8>().ok()),
                    status: read_optional(&path.join("status")),
                    health: read_optional(&path.join("health")),
                    cycle_count: read_optional(&path.join("cycle_count"))
                        .and_then(|value| value.parse::<u32>().ok()),
                    energy_now_microwatt_hours: read_optional(&path.join("energy_now"))
                        .and_then(|value| value.parse().ok()),
                    energy_full_microwatt_hours: read_optional(&path.join("energy_full"))
                        .and_then(|value| value.parse().ok()),
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
    fn read_disk_usage(&self) -> Metric<Vec<FilesystemUsage>> {
        let source = "/proc/self/mounts + statvfs";
        let Ok(text) = read_text(&self.proc_root.join("self/mounts"), source) else {
            return Metric::Unavailable(SourceIssue::new(source, "mount list is unreadable"));
        };
        let mut mounts = BTreeMap::<String, (String, String)>::new();
        for line in text.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 3 || !fields[0].starts_with("/dev/") {
                continue;
            }
            let device = fields[0].to_owned();
            let mount_point = decode_mount_field(fields[1]);
            let filesystem = fields[2].to_owned();
            let replace = match mounts.get(&device) {
                None => true,
                Some(current) => mount_point.as_str() < current.0.as_str(),
            };
            if replace {
                mounts.insert(device, (mount_point, filesystem));
            }
        }
        let mut usages = mounts
            .into_iter()
            .map(|(device, (mount_point, filesystem))| (mount_point, device, filesystem))
            .collect::<Vec<_>>();
        usages.sort_by(|left, right| left.0.cmp(&right.0));
        let mut readable = Vec::new();
        for (mount_point, device, filesystem) in usages.into_iter().take(MAX_FILESYSTEMS) {
            let Ok(path) = CString::new(mount_point.as_bytes()) else {
                continue;
            };
            let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
            if unsafe { statvfs(path.as_ptr(), &mut stats) } != 0 {
                continue;
            }
            readable.push(FilesystemUsage {
                device,
                mount_point,
                filesystem,
                total_bytes: (stats.f_blocks as u64).saturating_mul(stats.f_frsize as u64),
                available_bytes: (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64),
            });
        }
        if readable.is_empty() {
            Metric::Unavailable(SourceIssue::new(
                source,
                "no mounted /dev/ filesystem could be read with statvfs",
            ))
        } else {
            Metric::Available(readable)
        }
    }

    fn read_disk_io(&self) -> Metric<Vec<DiskCounters>> {
        let source = "/proc/diskstats";
        let Ok(text) = read_text(&self.proc_root.join("diskstats"), source) else {
            return Metric::Unavailable(SourceIssue::new(source, "disk statistics are unreadable"));
        };
        let block_root = self.sys_root.join("block");
        let mut devices = BTreeSet::new();
        let mut counters = Vec::new();
        for line in text.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 10 {
                continue;
            }
            let device = fields[2];
            if !devices.insert(device.to_owned()) || !block_root.join(device).is_dir() {
                continue;
            }
            let Ok(read_sectors) = fields[5].parse::<u64>() else {
                devices.remove(device);
                continue;
            };
            let Ok(written_sectors) = fields[9].parse::<u64>() else {
                devices.remove(device);
                continue;
            };
            counters.push(DiskCounters {
                device: device.to_owned(),
                read_bytes: read_sectors.saturating_mul(512),
                written_bytes: written_sectors.saturating_mul(512),
            });
        }
        counters.sort_by(|left, right| left.device.cmp(&right.device));
        counters.truncate(MAX_DISK_DEVICES);
        if counters.is_empty() {
            Metric::Unavailable(SourceIssue::new(
                source,
                "no whole-disk statistics are readable",
            ))
        } else {
            Metric::Available(counters)
        }
    }

    fn read_gpu(&self) -> Metric<Vec<GpuReading>> {
        let source = "/sys/class/drm";
        let mut readings = Vec::new();
        let mut any_busy = false;
        for card in sorted_entries(&self.sys_root.join("class/drm"))
            .into_iter()
            .filter(|path| path.is_dir())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix("card"))
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
                    })
            })
            .take(MAX_GPUS)
        {
            let Some(device) = self.canonical_device_identity(&card) else {
                continue;
            };
            let busy_percent = read_optional(&card.join("device/gpu_busy_percent"))
                .and_then(|value| value.parse::<u16>().ok())
                .map(|value| {
                    any_busy = true;
                    value.min(100) as u8
                });
            readings.push(GpuReading {
                device,
                busy_percent,
                label: card
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned),
            });
        }
        if !any_busy {
            Metric::Unavailable(SourceIssue::new(
                source,
                "no GPU exposes gpu_busy_percent; NVIDIA requires an NVML adapter that is not registered yet",
            ))
        } else {
            Metric::Available(readings)
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
        let core_count = [
            sample.cpu.is_available(),
            sample.memory.is_available(),
            sample.network.is_available(),
        ]
        .into_iter()
        .filter(|available| *available)
        .count();
        let optional_count = [
            sample.temperatures.is_available(),
            sample.power_supplies.is_available(),
            sample.disk_usage.is_available(),
            sample.disk_io.is_available(),
            sample.gpu.is_available(),
        ]
        .into_iter()
        .filter(|available| *available)
        .count();
        let status = if core_count == 3 && optional_count == 5 {
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
                "{core_count}/3 core and {optional_count}/5 optional monitor source families are available."
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
        ))
        .with_evidence(CapabilityEvidence::new(
            "disk_usage_sources_available",
            sample.disk_usage.is_available().to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "gpu_sources_available",
            sample.gpu.is_available().to_string(),
        ))
        .with_evidence(CapabilityEvidence::new(
            "per_process_metrics",
            "not_collected",
        ));
        if core_count < 3 || optional_count < 5 {
            report = report.with_remediation(
                "Unavailable metrics stay visible; verify procfs/sysfs mounts and kernel hardware drivers. NVIDIA GPU readings need an NVML adapter, and per-process metrics are not collected.",
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

fn decode_mount_field(field: &str) -> String {
    let mut decoded = Vec::with_capacity(field.len());
    let bytes = field.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if index + 3 < bytes.len()
            && bytes[index] == b'\\'
            && bytes[index + 1].is_ascii_digit()
            && bytes[index + 2].is_ascii_digit()
            && bytes[index + 3].is_ascii_digit()
        {
            let value =
                (bytes[index + 1] - b'0') * 64 + (bytes[index + 2] - b'0') * 8 + bytes[index + 3]
                    - b'0';
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
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
        let mount_point = root.path().join("mounted");
        fs::create_dir_all(proc_root.join("net")).expect("proc fixture");
        fs::create_dir_all(proc_root.join("self")).expect("mount fixture");
        fs::create_dir_all(&mount_point).expect("mount point fixture");
        fs::create_dir_all(sys_root.join("class/hwmon/hwmon7")).expect("hwmon fixture");
        fs::create_dir_all(sys_root.join("devices/platform/coretemp")).expect("device fixture");
        fs::create_dir_all(sys_root.join("class/power_supply/BAT0")).expect("power fixture");
        fs::create_dir_all(sys_root.join("block/sda")).expect("block fixture");
        let gpu_device = sys_root.join("devices/pci0000:00/0000:00:02.0");
        fs::create_dir_all(&gpu_device).expect("GPU device fixture");
        fs::create_dir_all(sys_root.join("class/drm/card0")).expect("DRM fixture");
        fs::write(gpu_device.join("gpu_busy_percent"), "155\n").expect("GPU busy fixture");
        symlink(&gpu_device, sys_root.join("class/drm/card0/device")).expect("GPU device link");
        fs::write(
            proc_root.join("stat"),
            "cpu  10 0 5 80 5 0 0 0\ncpu0 5 0 2 40 2 0 0 0\ncpu1 5 0 3 40 3 0 0 0\n",
        )
        .expect("stat fixture");
        fs::write(
            proc_root.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 400 kB\nCached: 100 kB\nBuffers: 50 kB\nSwapTotal: 200 kB\nSwapFree: 50 kB\n",
        )
        .expect("memory fixture");
        fs::write(
            proc_root.join("net/dev"),
            "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n  lo: 100 1 0 0 0 0 0 0 200 1 0 0 0 0 0 0\n",
        )
        .expect("network fixture");
        fs::write(
            proc_root.join("self/mounts"),
            format!(
                "/dev/test {} ext4 rw 0 0\n/dev/test {} ext4 rw 0 0\nproc /proc proc rw 0 0\n",
                mount_point.display(),
                mount_point.display()
            ),
        )
        .expect("mount fixture");
        fs::write(
            proc_root.join("diskstats"),
            "8 0 sda 8 0 10 0 20 0 30 0 0\nmalformed diskstats\n8 1 sda1 8 0 99 0 20 0 99 0 0\n",
        )
        .expect("diskstats fixture");
        let hwmon = sys_root.join("class/hwmon/hwmon7");
        fs::write(hwmon.join("name"), "coretemp\n").expect("chip name");
        fs::write(hwmon.join("temp1_input"), "42000\n").expect("temperature");
        fs::write(hwmon.join("temp1_label"), "Package id 0\n").expect("label");
        symlink("../../../devices/platform/coretemp", hwmon.join("device")).expect("device link");
        let battery = sys_root.join("class/power_supply/BAT0");
        fs::write(battery.join("type"), "Battery\n").expect("power type");
        fs::write(battery.join("scope"), "System\n").expect("power scope");
        fs::write(battery.join("capacity"), "87\n").expect("capacity");
        fs::write(battery.join("status"), "Discharging\n").expect("status");
        fs::write(battery.join("energy_now"), "1000\n").expect("energy now");
        fs::write(battery.join("energy_full"), "2000\n").expect("energy full");
        fs::write(battery.join("power_now"), "500\n").expect("power now");
        fs::write(battery.join("cycle_count"), "42\n").expect("cycle count");
        (root, ProcSysMonitor::new(proc_root, sys_root))
    }

    #[test]
    fn parses_independent_proc_and_sys_sources() {
        let (_root, mut monitor) = fixture();
        let sample = monitor.sample();

        assert!(matches!(&sample.cpu, Metric::Available(cpu) if cpu.logical_cpus == 2));
        assert!(matches!(
            &sample.memory,
            Metric::Available(memory) if memory.available_bytes == 409_600
        ));
        assert!(matches!(
            &sample.network,
            Metric::Available(interfaces) if interfaces[0].transmitted_bytes == 200
        ));
        assert!(matches!(
            &sample.power_supplies,
            Metric::Available(supplies) if supplies[0].capacity_percent == Some(87)
        ));
        assert!(matches!(
            &sample.memory,
            Metric::Available(memory)
                if memory.cached_bytes == 102_400 && memory.buffers_bytes == 51_200
        ));
        assert!(matches!(
            &sample.power_supplies,
            Metric::Available(supplies)
                if supplies[0].scope.as_deref() == Some("System")
                    && supplies[0].cycle_count == Some(42)
                    && supplies[0].health.is_none()
        ));
        assert!(matches!(
            &sample.disk_usage,
            Metric::Available(filesystems)
                if filesystems.len() == 1
                    && filesystems[0].available_bytes <= filesystems[0].total_bytes
                    && filesystems[0].device == "/dev/test"
        ));
        assert!(matches!(
            &sample.disk_io,
            Metric::Available(disks)
                if disks.len() == 1
                    && disks[0].device == "sda"
                    && disks[0].read_bytes == 10 * 512
                    && disks[0].written_bytes == 30 * 512
        ));
        assert!(matches!(
            &sample.gpu,
            Metric::Available(gpus)
                if gpus.len() == 1 && gpus[0].busy_percent == Some(100)
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
    fn gpu_without_busy_attribute_reports_independent_unavailable_metric() {
        let (_root, mut monitor) = fixture();
        fs::remove_file(
            monitor
                .sys_root
                .join("devices/pci0000:00/0000:00:02.0/gpu_busy_percent"),
        )
        .expect("remove GPU attribute");
        assert!(matches!(monitor.sample().gpu, Metric::Unavailable(_)));
    }

    #[test]
    fn filesystem_statvfs_failures_do_not_hide_readable_mounts() {
        let (root, mut monitor) = fixture();
        let mount_point = root.path().join("mounted");
        fs::write(
            monitor.proc_root.join("self/mounts"),
            format!(
                "/dev/missing /definitely/not/a/fixture ext4 rw 0 0\n/dev/test {} ext4 rw 0 0\n",
                mount_point.display()
            ),
        )
        .expect("mount fixture");
        assert!(matches!(
            monitor.sample().disk_usage,
            Metric::Available(filesystems)
                if filesystems.len() == 1 && filesystems[0].device == "/dev/test"
        ));
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
        assert!(matches!(sample.disk_usage, Metric::Unavailable(_)));
        assert!(matches!(sample.disk_io, Metric::Unavailable(_)));
        assert!(matches!(sample.gpu, Metric::Unavailable(_)));
    }
}
