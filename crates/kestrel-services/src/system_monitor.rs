//! Runtime-agnostic Linux system-monitor sampling policy.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt,
    time::Duration,
};

use kestrel_platform::{
    system_monitor::{
        CoreKind, CpuCounters, FilesystemUsage, GpuReading, MemoryCounters, Metric,
        NetworkCounters, PowerSupplyReading, RawSystemSample, SourceIssue, SystemMonitorSource,
        TemperatureReading,
    },
    upower::PeripheralBattery,
};

pub const MIN_REFRESH_INTERVAL: Duration =
    Duration::from_millis(kestrel_core::MIN_MONITOR_REFRESH_INTERVAL_MILLIS);
pub const MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRefreshInterval {
    requested: Duration,
}

impl InvalidRefreshInterval {
    pub fn requested(self) -> Duration {
        self.requested
    }
}

impl fmt::Display for InvalidRefreshInterval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "refresh interval {:?} is outside {:?}..={:?}",
            self.requested, MIN_REFRESH_INTERVAL, MAX_REFRESH_INTERVAL
        )
    }
}

impl Error for InvalidRefreshInterval {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHistoryCapacity {
    requested: u32,
}

impl InvalidHistoryCapacity {
    pub fn requested(self) -> u32 {
        self.requested
    }
}

impl fmt::Display for InvalidHistoryCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "history capacity {} is outside 1..={}",
            self.requested,
            kestrel_core::MAX_MONITOR_HISTORY_SAMPLES
        )
    }
}

impl Error for InvalidHistoryCapacity {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMonitorConfigError {
    RefreshInterval(InvalidRefreshInterval),
    HistoryCapacity(InvalidHistoryCapacity),
}

impl fmt::Display for SystemMonitorConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RefreshInterval(error) => error.fmt(formatter),
            Self::HistoryCapacity(error) => error.fmt(formatter),
        }
    }
}

impl Error for SystemMonitorConfigError {}

impl From<InvalidRefreshInterval> for SystemMonitorConfigError {
    fn from(error: InvalidRefreshInterval) -> Self {
        Self::RefreshInterval(error)
    }
}

impl From<InvalidHistoryCapacity> for SystemMonitorConfigError {
    fn from(error: InvalidHistoryCapacity) -> Self {
        Self::HistoryCapacity(error)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub cached_bytes: u64,
    pub buffers_bytes: u64,
    pub used_percent: Option<f64>,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_used_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskUsageSnapshot {
    pub device: String,
    pub mount_point: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub used_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuSnapshot {
    pub device: String,
    pub label: Option<String>,
    pub busy_percent: Option<u8>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoreUsage {
    pub index: u32,
    pub kind: Option<CoreKind>,
    /// `None` until two samples of this core exist.
    pub usage_percent: Option<f64>,
}

/// Bytes moved on non-loopback interfaces since monitoring started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkSessionTotals {
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
    /// Monitor clock time of the baseline sample.
    pub since: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PowerSupplySnapshot {
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
    pub remaining_seconds: Option<u64>,
    pub online: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistorySample {
    pub observed_at: Duration,
    pub cpu_usage_percent: Option<f64>,
    pub memory_used_percent: Option<f64>,
    pub disk_used_percent: Option<f64>,
    pub temperature_celsius_max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistorySummary {
    pub samples: usize,
    pub span: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkSnapshot {
    pub interface: String,
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
    pub receive_bytes_per_second: Option<f64>,
    pub transmit_bytes_per_second: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemSnapshot {
    pub observed_at: Duration,
    pub cpu_usage_percent: Option<f64>,
    pub logical_cpus: Option<usize>,
    pub cores: Vec<CoreUsage>,
    pub memory: Option<MemorySnapshot>,
    pub disk_usage: Vec<DiskUsageSnapshot>,
    pub network: Vec<NetworkSnapshot>,
    pub network_session: Option<NetworkSessionTotals>,
    pub temperatures: Vec<TemperatureReading>,
    pub power_supplies: Vec<PowerSupplySnapshot>,
    pub gpu: Vec<GpuSnapshot>,
    pub peripherals: Vec<PeripheralBattery>,
    pub issues: Vec<SourceIssue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RefreshOutcome {
    Skipped,
    Updated,
}

pub struct SystemMonitorService<S> {
    source: S,
    refresh_interval: Duration,
    history_capacity: u32,
    previous_sample: Option<(Duration, RawSystemSample)>,
    latest: Option<SystemSnapshot>,
    history: VecDeque<HistorySample>,
    network_session: Option<NetworkSessionTotals>,
}

impl<S: SystemMonitorSource> SystemMonitorService<S> {
    pub fn new(
        source: S,
        refresh_interval: Duration,
        history_capacity: u32,
    ) -> Result<Self, SystemMonitorConfigError> {
        if !(MIN_REFRESH_INTERVAL..=MAX_REFRESH_INTERVAL).contains(&refresh_interval) {
            return Err(InvalidRefreshInterval {
                requested: refresh_interval,
            }
            .into());
        }
        if !(1..=kestrel_core::MAX_MONITOR_HISTORY_SAMPLES).contains(&history_capacity) {
            return Err(InvalidHistoryCapacity {
                requested: history_capacity,
            }
            .into());
        }
        Ok(Self {
            source,
            refresh_interval,
            history_capacity,
            previous_sample: None,
            latest: None,
            history: VecDeque::with_capacity(history_capacity as usize),
            network_session: None,
        })
    }

    pub fn reconfigure(
        &mut self,
        refresh_interval: Duration,
        history_capacity: u32,
    ) -> Result<(), SystemMonitorConfigError> {
        if !(MIN_REFRESH_INTERVAL..=MAX_REFRESH_INTERVAL).contains(&refresh_interval) {
            return Err(InvalidRefreshInterval {
                requested: refresh_interval,
            }
            .into());
        }
        if !(1..=kestrel_core::MAX_MONITOR_HISTORY_SAMPLES).contains(&history_capacity) {
            return Err(InvalidHistoryCapacity {
                requested: history_capacity,
            }
            .into());
        }
        self.refresh_interval = refresh_interval;
        self.history_capacity = history_capacity;
        while self.history.len() > history_capacity as usize {
            self.history.pop_front();
        }
        if self.history.capacity() < history_capacity as usize {
            self.history
                .reserve(history_capacity as usize - self.history.len());
        }
        Ok(())
    }

    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    pub fn history_capacity(&self) -> u32 {
        self.history_capacity
    }

    pub fn latest(&self) -> Option<&SystemSnapshot> {
        self.latest.as_ref()
    }

    pub fn history_summary(&self) -> Option<HistorySummary> {
        let first = self.history.front()?;
        let last = self.history.back()?;
        Some(HistorySummary {
            samples: self.history.len(),
            span: last.observed_at.saturating_sub(first.observed_at),
        })
    }

    pub fn refresh(&mut self, observed_at: Duration) -> RefreshOutcome {
        if self
            .previous_sample
            .as_ref()
            .is_some_and(|(previous_at, _)| {
                observed_at
                    .checked_sub(*previous_at)
                    .is_none_or(|elapsed| elapsed < self.refresh_interval)
            })
        {
            return RefreshOutcome::Skipped;
        }

        let raw = self.source.sample();
        let previous = self
            .previous_sample
            .as_ref()
            .map(|(at, sample)| (*at, sample));
        let mut snapshot = build_snapshot(observed_at, &raw, previous);
        self.network_session = advance_session(self.network_session, observed_at, &raw, previous);
        snapshot.network_session = self.network_session;
        let sample = history_sample(&snapshot);
        self.previous_sample = Some((observed_at, raw));
        self.latest = Some(snapshot);
        if self.history.len() == self.history_capacity as usize {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        RefreshOutcome::Updated
    }
}

fn build_snapshot(
    observed_at: Duration,
    raw: &RawSystemSample,
    previous: Option<(Duration, &RawSystemSample)>,
) -> SystemSnapshot {
    let elapsed_seconds = previous
        .and_then(|(previous_at, _)| observed_at.checked_sub(previous_at))
        .map(|elapsed| elapsed.as_secs_f64())
        .filter(|elapsed| *elapsed > 0.0);
    let cpu = available(&raw.cpu);
    let previous_cpu = previous.and_then(|(_, sample)| available(&sample.cpu));
    let memory = available(&raw.memory).map(memory_snapshot);
    let network = available(&raw.network)
        .map(|interfaces| {
            network_snapshots(
                interfaces,
                previous
                    .and_then(|(_, sample)| available(&sample.network))
                    .map(Vec::as_slice),
                elapsed_seconds,
            )
        })
        .unwrap_or_default();
    let disk_usage = available(&raw.disk_usage)
        .map(|entries| entries.iter().map(disk_usage_snapshot).collect())
        .unwrap_or_default();

    SystemSnapshot {
        observed_at,
        cpu_usage_percent: cpu_usage(cpu, previous_cpu),
        logical_cpus: cpu.map(|counters| counters.logical_cpus),
        cores: cpu
            .map(|counters| core_usage(counters, previous_cpu))
            .unwrap_or_default(),
        memory,
        disk_usage,
        network,
        network_session: None,
        temperatures: available(&raw.temperatures).cloned().unwrap_or_default(),
        power_supplies: available(&raw.power_supplies)
            .map(|supplies| supplies.iter().map(power_supply_snapshot).collect())
            .unwrap_or_default(),
        gpu: available(&raw.gpu)
            .map(|gpus| gpus.iter().map(gpu_snapshot).collect())
            .unwrap_or_default(),
        peripherals: available(&raw.peripherals).cloned().unwrap_or_default(),
        issues: collect_issues(raw),
    }
}

/// Adds the latest non-loopback counter deltas; a counter that went backwards
/// restarted from zero, so its current value is the delta.
fn advance_session(
    session: Option<NetworkSessionTotals>,
    observed_at: Duration,
    raw: &RawSystemSample,
    previous: Option<(Duration, &RawSystemSample)>,
) -> Option<NetworkSessionTotals> {
    let Some(current) = available(&raw.network) else {
        return session;
    };
    let mut totals = session.unwrap_or(NetworkSessionTotals {
        received_bytes: 0,
        transmitted_bytes: 0,
        since: observed_at,
    });
    let Some(previous) = previous.and_then(|(_, sample)| available(&sample.network)) else {
        return Some(totals);
    };
    let previous = previous
        .iter()
        .map(|interface| (interface.interface.as_str(), interface))
        .collect::<BTreeMap<_, _>>();
    for interface in current
        .iter()
        .filter(|interface| interface.interface != "lo")
    {
        let Some(prior) = previous.get(interface.interface.as_str()) else {
            continue;
        };
        let delta = |now: u64, before: u64| now.checked_sub(before).unwrap_or(now);
        totals.received_bytes = totals
            .received_bytes
            .saturating_add(delta(interface.received_bytes, prior.received_bytes));
        totals.transmitted_bytes = totals
            .transmitted_bytes
            .saturating_add(delta(interface.transmitted_bytes, prior.transmitted_bytes));
    }
    Some(totals)
}

fn available<T>(metric: &Metric<T>) -> Option<&T> {
    match metric {
        Metric::Available(value) => Some(value),
        Metric::Unavailable(_) => None,
    }
}

fn issue<T>(metric: &Metric<T>) -> Option<SourceIssue> {
    match metric {
        Metric::Available(_) => None,
        Metric::Unavailable(issue) => Some(issue.clone()),
    }
}

fn collect_issues(raw: &RawSystemSample) -> Vec<SourceIssue> {
    [
        issue(&raw.cpu),
        issue(&raw.memory),
        issue(&raw.network),
        issue(&raw.temperatures),
        issue(&raw.power_supplies),
        issue(&raw.disk_usage),
        issue(&raw.gpu),
        issue(&raw.peripherals),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn cpu_usage(current: Option<&CpuCounters>, previous: Option<&CpuCounters>) -> Option<f64> {
    let current = current?;
    let previous = previous?;
    let total_delta = current.total.checked_sub(previous.total)?;
    let idle_delta = current.idle.checked_sub(previous.idle)?;
    if total_delta == 0 || idle_delta > total_delta {
        return None;
    }
    Some(clamp_percent(
        (total_delta - idle_delta) as f64 * 100.0 / total_delta as f64,
    ))
}

fn core_usage(current: &CpuCounters, previous: Option<&CpuCounters>) -> Vec<CoreUsage> {
    let previous = previous
        .into_iter()
        .flat_map(|counters| counters.cores.iter())
        .map(|core| (core.index, core))
        .collect::<BTreeMap<_, _>>();
    current
        .cores
        .iter()
        .map(|core| {
            let usage_percent = previous.get(&core.index).and_then(|prior| {
                let total = core.total.checked_sub(prior.total)?;
                let idle = core.idle.checked_sub(prior.idle)?;
                (total > 0 && idle <= total)
                    .then(|| clamp_percent((total - idle) as f64 * 100.0 / total as f64))
            });
            CoreUsage {
                index: core.index,
                kind: core.kind,
                usage_percent,
            }
        })
        .collect()
}

fn memory_snapshot(counters: &MemoryCounters) -> MemorySnapshot {
    let used_bytes = counters
        .total_bytes
        .saturating_sub(counters.available_bytes);
    let swap_used_bytes = counters
        .swap_total_bytes
        .saturating_sub(counters.swap_free_bytes);
    MemorySnapshot {
        total_bytes: counters.total_bytes,
        used_bytes,
        available_bytes: counters.available_bytes,
        cached_bytes: counters.cached_bytes,
        buffers_bytes: counters.buffers_bytes,
        used_percent: percentage(used_bytes, counters.total_bytes),
        swap_total_bytes: counters.swap_total_bytes,
        swap_used_bytes,
        swap_used_percent: percentage(swap_used_bytes, counters.swap_total_bytes),
    }
}

fn disk_usage_snapshot(entry: &FilesystemUsage) -> DiskUsageSnapshot {
    let used_bytes = entry.total_bytes.saturating_sub(entry.available_bytes);
    DiskUsageSnapshot {
        device: entry.device.clone(),
        mount_point: entry.mount_point.clone(),
        filesystem: entry.filesystem.clone(),
        total_bytes: entry.total_bytes,
        available_bytes: entry.available_bytes,
        used_bytes,
        used_percent: percentage(used_bytes, entry.total_bytes),
    }
}

fn network_snapshots(
    current: &[NetworkCounters],
    previous: Option<&[NetworkCounters]>,
    elapsed_seconds: Option<f64>,
) -> Vec<NetworkSnapshot> {
    let previous = previous
        .into_iter()
        .flatten()
        .map(|interface| (interface.interface.as_str(), interface))
        .collect::<BTreeMap<_, _>>();
    current
        .iter()
        .map(|interface| {
            let prior = previous.get(interface.interface.as_str()).copied();
            NetworkSnapshot {
                interface: interface.interface.clone(),
                received_bytes: interface.received_bytes,
                transmitted_bytes: interface.transmitted_bytes,
                receive_bytes_per_second: counter_rate(
                    interface.received_bytes,
                    prior.map(|value| value.received_bytes),
                    elapsed_seconds,
                ),
                transmit_bytes_per_second: counter_rate(
                    interface.transmitted_bytes,
                    prior.map(|value| value.transmitted_bytes),
                    elapsed_seconds,
                ),
            }
        })
        .collect()
}

fn power_supply_snapshot(reading: &PowerSupplyReading) -> PowerSupplySnapshot {
    let remaining_seconds = match (reading.energy_now_microwatt_hours, reading.power_microwatts) {
        (Some(energy), Some(power)) if energy > 0 && power > 0 => energy
            .checked_mul(3_600)
            .and_then(|energy_seconds| energy_seconds.checked_div(power)),
        _ => None,
    };
    PowerSupplySnapshot {
        name: reading.name.clone(),
        kind: reading.kind.clone(),
        scope: reading.scope.clone(),
        capacity_percent: reading.capacity_percent,
        status: reading.status.clone(),
        health: reading.health.clone(),
        cycle_count: reading.cycle_count,
        energy_now_microwatt_hours: reading.energy_now_microwatt_hours,
        energy_full_microwatt_hours: reading.energy_full_microwatt_hours,
        power_microwatts: reading.power_microwatts,
        remaining_seconds,
        online: reading.online,
    }
}

fn gpu_snapshot(reading: &GpuReading) -> GpuSnapshot {
    GpuSnapshot {
        device: reading.device.clone(),
        label: reading.label.clone(),
        busy_percent: reading.busy_percent,
        memory_used_bytes: reading.memory_used_bytes,
        memory_total_bytes: reading.memory_total_bytes,
    }
}

fn history_sample(snapshot: &SystemSnapshot) -> HistorySample {
    HistorySample {
        observed_at: snapshot.observed_at,
        cpu_usage_percent: snapshot.cpu_usage_percent,
        memory_used_percent: snapshot
            .memory
            .as_ref()
            .and_then(|memory| memory.used_percent),
        disk_used_percent: snapshot
            .disk_usage
            .iter()
            .filter_map(|disk| disk.used_percent)
            .max_by(f64::total_cmp),
        temperature_celsius_max: snapshot
            .temperatures
            .iter()
            .map(|temperature| temperature.millidegrees_celsius as f64 / 1_000.0)
            .max_by(f64::total_cmp),
    }
}

fn percentage(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| clamp_percent(numerator as f64 * 100.0 / denominator as f64))
}

fn clamp_percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

fn counter_rate(current: u64, previous: Option<u64>, elapsed_seconds: Option<f64>) -> Option<f64> {
    Some(current.checked_sub(previous?)? as f64 / elapsed_seconds?)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, time::Duration};

    use kestrel_platform::system_monitor::{
        CpuCounters, FilesystemUsage, GpuReading, MemoryCounters, Metric, NetworkCounters,
        PowerSupplyReading, RawSystemSample, SourceIssue, SystemMonitorSource,
    };

    use super::{
        InvalidHistoryCapacity, MAX_REFRESH_INTERVAL, MIN_REFRESH_INTERVAL, RefreshOutcome,
        SystemMonitorConfigError, SystemMonitorService,
    };

    struct FakeSource {
        samples: RefCell<VecDeque<RawSystemSample>>,
    }

    impl SystemMonitorSource for FakeSource {
        fn sample(&mut self) -> RawSystemSample {
            self.samples
                .borrow_mut()
                .pop_front()
                .expect("a fake sample must be available")
        }
    }

    fn sample(total: u64, idle: u64, received: u64, transmitted: u64) -> RawSystemSample {
        RawSystemSample {
            cpu: Metric::Available(CpuCounters {
                total,
                idle,
                logical_cpus: 4,
                cores: Vec::new(),
            }),
            memory: Metric::Available(MemoryCounters {
                total_bytes: 1_000,
                available_bytes: 250,
                cached_bytes: 100,
                buffers_bytes: 50,
                swap_total_bytes: 100,
                swap_free_bytes: 40,
            }),
            network: Metric::Available(vec![NetworkCounters {
                interface: "eth0".to_string(),
                received_bytes: received,
                transmitted_bytes: transmitted,
            }]),
            temperatures: Metric::Unavailable(SourceIssue {
                source: "temperature".to_string(),
                reason: "not exposed".to_string(),
            }),
            power_supplies: Metric::Available(Vec::new()),
            disk_usage: Metric::Available(vec![FilesystemUsage {
                device: "/dev/test".to_string(),
                mount_point: "/".to_string(),
                filesystem: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 0,
            }]),
            gpu: Metric::Available(vec![GpuReading {
                device: "gpu0".to_string(),
                busy_percent: Some(50),
                label: Some("card0".to_string()),
                memory_used_bytes: None,
                memory_total_bytes: None,
            }]),
            peripherals: Metric::Available(Vec::new()),
        }
    }

    fn source(samples: Vec<RawSystemSample>) -> FakeSource {
        FakeSource {
            samples: RefCell::new(VecDeque::from(samples)),
        }
    }

    fn service(samples: Vec<RawSystemSample>, capacity: u32) -> SystemMonitorService<FakeSource> {
        SystemMonitorService::new(source(samples), Duration::from_secs(1), capacity)
            .expect("valid monitor configuration")
    }

    #[test]
    fn refresh_interval_rejects_out_of_bounds_values() {
        for interval in [
            MIN_REFRESH_INTERVAL - Duration::from_millis(1),
            MAX_REFRESH_INTERVAL + Duration::from_millis(1),
        ] {
            assert!(matches!(
                SystemMonitorService::new(source(Vec::new()), interval, 1),
                Err(SystemMonitorConfigError::RefreshInterval(_))
            ));
        }
    }

    #[test]
    fn invalid_history_capacity_is_rejected() {
        let result = SystemMonitorService::new(source(Vec::new()), Duration::from_secs(1), 0);
        assert!(matches!(
            result,
            Err(SystemMonitorConfigError::HistoryCapacity(
                InvalidHistoryCapacity { requested: 0 }
            ))
        ));
    }

    #[test]
    fn bounds_sampling_and_calculates_counter_deltas() {
        let mut service = service(
            vec![
                sample(100, 80, 1_000, 2_000),
                sample(200, 120, 1_300, 2_600),
            ],
            4,
        );
        assert_eq!(service.refresh(Duration::ZERO), RefreshOutcome::Updated);
        assert_eq!(
            service.refresh(Duration::from_millis(999)),
            RefreshOutcome::Skipped
        );
        assert_eq!(
            service.refresh(Duration::from_secs(2)),
            RefreshOutcome::Updated
        );
        let snapshot = service.latest().expect("second sample must refresh");
        assert_eq!(snapshot.cpu_usage_percent, Some(60.0));
        assert_eq!(snapshot.network[0].receive_bytes_per_second, Some(150.0));
        assert_eq!(snapshot.network[0].transmit_bytes_per_second, Some(300.0));
        assert_eq!(snapshot.memory.as_ref().expect("memory").used_bytes, 750);
        assert_eq!(snapshot.issues.len(), 1);
        assert_eq!(
            service.history_summary().expect("history summary").samples,
            2
        );
    }

    #[test]
    fn counter_resets_produce_missing_rates_instead_of_underflow() {
        let mut service = service(
            vec![sample(100, 80, 1_000, 2_000), sample(10, 8, 100, 200)],
            4,
        );
        service.refresh(Duration::ZERO);
        assert_eq!(
            service.refresh(Duration::from_secs(1)),
            RefreshOutcome::Updated
        );
        let snapshot = service.latest().expect("second sample must refresh");
        assert_eq!(snapshot.cpu_usage_percent, None);
        assert_eq!(snapshot.network[0].receive_bytes_per_second, None);
        assert_eq!(snapshot.network[0].transmit_bytes_per_second, None);
    }

    #[test]
    fn history_is_bounded_and_skips_are_not_recorded() {
        let mut service = service(
            vec![
                sample(100, 80, 1, 2),
                sample(200, 160, 2, 4),
                sample(300, 240, 3, 6),
                sample(400, 320, 4, 8),
            ],
            2,
        );
        service.refresh(Duration::ZERO);
        assert_eq!(
            service.refresh(Duration::from_millis(500)),
            RefreshOutcome::Skipped
        );
        service.refresh(Duration::from_secs(1));
        service.refresh(Duration::from_secs(2));
        service.refresh(Duration::from_secs(3));
        assert_eq!(
            service.history_summary().expect("history summary").samples,
            2
        );
    }

    #[test]
    fn history_summary_reports_samples_and_span() {
        let empty = service(Vec::new(), 2);
        assert_eq!(empty.history_summary(), None);
        let mut service = service(vec![sample(100, 80, 1, 2), sample(200, 120, 2, 4)], 2);
        service.refresh(Duration::from_secs(1));
        service.refresh(Duration::from_secs(2));
        let summary = service.history_summary().expect("history summary");
        assert_eq!(summary.samples, 2);
        assert_eq!(summary.span, Duration::from_secs(1));
    }

    #[test]
    fn disk_percent_is_clamped_and_zero_swap_is_none() {
        let mut raw = sample(100, 80, 1, 2);
        raw.memory = Metric::Available(MemoryCounters {
            total_bytes: 10,
            available_bytes: 0,
            cached_bytes: 0,
            buffers_bytes: 0,
            swap_total_bytes: 0,
            swap_free_bytes: 0,
        });
        if let Metric::Available(disks) = &mut raw.disk_usage {
            disks[0].total_bytes = 10;
            disks[0].available_bytes = 20;
        }
        let mut service = service(vec![raw], 2);
        assert_eq!(service.refresh(Duration::ZERO), RefreshOutcome::Updated);
        let snapshot = service.latest().expect("sample must refresh");
        assert_eq!(
            snapshot.memory.as_ref().expect("memory").swap_used_percent,
            None
        );
        assert_eq!(snapshot.disk_usage[0].used_percent, Some(0.0));
    }

    #[test]
    fn remaining_seconds_requires_positive_power() {
        let mut raw = sample(100, 80, 1, 2);
        raw.power_supplies = Metric::Available(vec![PowerSupplyReading {
            name: "BAT0".to_string(),
            kind: Some("Battery".to_string()),
            scope: Some("System".to_string()),
            capacity_percent: Some(50),
            status: None,
            health: None,
            cycle_count: None,
            energy_now_microwatt_hours: Some(5_000),
            energy_full_microwatt_hours: None,
            power_microwatts: Some(0),
            online: None,
        }]);
        let mut service = service(vec![raw], 2);
        assert_eq!(service.refresh(Duration::ZERO), RefreshOutcome::Updated);
        let snapshot = service.latest().expect("sample must refresh");
        assert_eq!(snapshot.power_supplies[0].remaining_seconds, None);
    }

    #[test]
    fn unavailable_issue_order_is_stable() {
        fn unavailable<T>(name: &str) -> Metric<T> {
            Metric::Unavailable(SourceIssue {
                source: name.to_string(),
                reason: "missing".to_string(),
            })
        }
        let raw = RawSystemSample {
            cpu: unavailable("cpu"),
            memory: unavailable("memory"),
            network: unavailable("network"),
            temperatures: unavailable("temperatures"),
            power_supplies: unavailable("power"),
            disk_usage: unavailable("disk_usage"),
            gpu: unavailable("gpu"),
            peripherals: unavailable("peripherals"),
        };
        let mut service = service(vec![raw], 1);
        assert_eq!(service.refresh(Duration::ZERO), RefreshOutcome::Updated);
        let snapshot = service.latest().expect("sample must refresh");
        let order = snapshot
            .issues
            .iter()
            .map(|issue| issue.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                "cpu",
                "memory",
                "network",
                "temperatures",
                "power",
                "disk_usage",
                "gpu",
                "peripherals"
            ]
        );
    }

    #[test]
    fn per_core_load_needs_two_samples_and_survives_counter_resets() {
        use kestrel_platform::system_monitor::{CoreCounters, CoreKind};
        let with_cores = |cores: [(u64, u64); 2]| {
            let mut raw = sample(100, 50, 0, 0);
            if let Metric::Available(cpu) = &mut raw.cpu {
                cpu.cores = cores
                    .iter()
                    .enumerate()
                    .map(|(index, (total, idle))| CoreCounters {
                        index: index as u32,
                        total: *total,
                        idle: *idle,
                        kind: Some(CoreKind::Performance),
                    })
                    .collect();
            }
            raw
        };
        let mut service = service(
            vec![
                with_cores([(100, 50), (100, 90)]),
                with_cores([(200, 75), (50, 40)]),
            ],
            4,
        );
        service.refresh(Duration::ZERO);
        let first = service.latest().expect("first sample").cores.clone();
        assert!(first.iter().all(|core| core.usage_percent.is_none()));
        service.refresh(Duration::from_secs(1));
        let second = &service.latest().expect("second sample").cores;
        assert_eq!(second[0].usage_percent, Some(75.0));
        assert_eq!(second[1].usage_percent, None, "a reset counter has no rate");
        assert_eq!(second[0].kind, Some(CoreKind::Performance));
    }

    #[test]
    fn session_totals_skip_loopback_count_resets_and_survive_gaps() {
        let with_interfaces = |eth0: (u64, u64), lo: (u64, u64)| {
            let mut raw = sample(100, 50, eth0.0, eth0.1);
            if let Metric::Available(interfaces) = &mut raw.network {
                interfaces.push(NetworkCounters {
                    interface: "lo".to_string(),
                    received_bytes: lo.0,
                    transmitted_bytes: lo.1,
                });
            }
            raw
        };
        let mut gap = sample(100, 50, 0, 0);
        gap.network = Metric::Unavailable(SourceIssue {
            source: "/proc/net/dev".to_string(),
            reason: "unreadable".to_string(),
        });
        let mut service = service(
            vec![
                with_interfaces((1_000, 500), (10, 10)),
                with_interfaces((1_600, 700), (9_000, 9_000)),
                // The interface restarted its counters.
                with_interfaces((100, 50), (9_100, 9_100)),
                gap,
            ],
            8,
        );
        let mut totals = Vec::new();
        for second in 0..4 {
            service.refresh(Duration::from_secs(second));
            let session = service
                .latest()
                .and_then(|snapshot| snapshot.network_session)
                .expect("session totals start with the first readable sample");
            assert_eq!(session.since, Duration::ZERO);
            totals.push((session.received_bytes, session.transmitted_bytes));
        }
        assert_eq!(totals, [(0, 0), (600, 200), (700, 250), (700, 250)]);
    }
}
