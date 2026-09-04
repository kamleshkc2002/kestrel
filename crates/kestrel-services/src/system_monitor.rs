//! Runtime-agnostic sampling policy for the Linux system monitor.

use std::{collections::BTreeMap, error::Error, fmt, time::Duration};

use kestrel_platform::system_monitor::{
    CpuCounters, MemoryCounters, Metric, NetworkCounters, PowerSupplyReading, RawSystemSample,
    SourceIssue, SystemMonitorSource, TemperatureReading,
};

pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
pub const MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

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

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
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
    pub memory: Option<MemorySnapshot>,
    pub network: Vec<NetworkSnapshot>,
    pub temperatures: Vec<TemperatureReading>,
    pub power_supplies: Vec<PowerSupplyReading>,
    pub issues: Vec<SourceIssue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RefreshOutcome {
    Skipped,
    Updated(SystemSnapshot),
}

pub struct SystemMonitorService<S> {
    source: S,
    refresh_interval: Duration,
    previous_sample: Option<(Duration, RawSystemSample)>,
    latest: Option<SystemSnapshot>,
}

impl<S: SystemMonitorSource> SystemMonitorService<S> {
    pub fn new(source: S, refresh_interval: Duration) -> Result<Self, InvalidRefreshInterval> {
        if !(MIN_REFRESH_INTERVAL..=MAX_REFRESH_INTERVAL).contains(&refresh_interval) {
            return Err(InvalidRefreshInterval {
                requested: refresh_interval,
            });
        }
        Ok(Self {
            source,
            refresh_interval,
            previous_sample: None,
            latest: None,
        })
    }

    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    pub fn latest(&self) -> Option<&SystemSnapshot> {
        self.latest.as_ref()
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
        let snapshot = build_snapshot(
            observed_at,
            &raw,
            self.previous_sample
                .as_ref()
                .map(|(at, sample)| (*at, sample)),
        );
        self.previous_sample = Some((observed_at, raw));
        self.latest = Some(snapshot.clone());
        RefreshOutcome::Updated(snapshot)
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

    SystemSnapshot {
        observed_at,
        cpu_usage_percent: cpu_usage(cpu, previous_cpu),
        logical_cpus: cpu.map(|counters| counters.logical_cpus),
        memory,
        network,
        temperatures: available(&raw.temperatures).cloned().unwrap_or_default(),
        power_supplies: available(&raw.power_supplies).cloned().unwrap_or_default(),
        issues: collect_issues(raw),
    }
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
    Some((total_delta - idle_delta) as f64 * 100.0 / total_delta as f64)
}

fn memory_snapshot(counters: &MemoryCounters) -> MemorySnapshot {
    MemorySnapshot {
        total_bytes: counters.total_bytes,
        used_bytes: counters
            .total_bytes
            .saturating_sub(counters.available_bytes),
        available_bytes: counters.available_bytes,
        swap_total_bytes: counters.swap_total_bytes,
        swap_used_bytes: counters
            .swap_total_bytes
            .saturating_sub(counters.swap_free_bytes),
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

fn counter_rate(current: u64, previous: Option<u64>, elapsed_seconds: Option<f64>) -> Option<f64> {
    Some(current.checked_sub(previous?)? as f64 / elapsed_seconds?)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, time::Duration};

    use kestrel_platform::system_monitor::{
        CpuCounters, MemoryCounters, Metric, NetworkCounters, RawSystemSample, SourceIssue,
        SystemMonitorSource,
    };

    use super::{RefreshOutcome, SystemMonitorService, MAX_REFRESH_INTERVAL, MIN_REFRESH_INTERVAL};

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
            }),
            memory: Metric::Available(MemoryCounters {
                total_bytes: 1_000,
                available_bytes: 250,
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
        }
    }

    #[test]
    fn validates_refresh_interval_bounds() {
        assert!(SystemMonitorService::new(
            FakeSource {
                samples: RefCell::new(VecDeque::new())
            },
            MIN_REFRESH_INTERVAL - Duration::from_millis(1)
        )
        .is_err());
        assert!(SystemMonitorService::new(
            FakeSource {
                samples: RefCell::new(VecDeque::new())
            },
            MAX_REFRESH_INTERVAL + Duration::from_millis(1)
        )
        .is_err());
    }

    #[test]
    fn bounds_sampling_and_calculates_counter_deltas() {
        let source = FakeSource {
            samples: RefCell::new(VecDeque::from([
                sample(100, 80, 1_000, 2_000),
                sample(200, 120, 1_300, 2_600),
            ])),
        };
        let mut service =
            SystemMonitorService::new(source, Duration::from_secs(1)).expect("valid interval");

        assert!(matches!(
            service.refresh(Duration::ZERO),
            RefreshOutcome::Updated(_)
        ));
        assert_eq!(
            service.refresh(Duration::from_millis(999)),
            RefreshOutcome::Skipped
        );
        let RefreshOutcome::Updated(snapshot) = service.refresh(Duration::from_secs(2)) else {
            panic!("second sample must refresh");
        };

        assert_eq!(snapshot.cpu_usage_percent, Some(60.0));
        assert_eq!(snapshot.network[0].receive_bytes_per_second, Some(150.0));
        assert_eq!(snapshot.network[0].transmit_bytes_per_second, Some(300.0));
        assert_eq!(snapshot.memory.expect("memory").used_bytes, 750);
        assert_eq!(snapshot.issues.len(), 1);
    }

    #[test]
    fn counter_resets_produce_missing_rates_instead_of_underflow() {
        let source = FakeSource {
            samples: RefCell::new(VecDeque::from([
                sample(100, 80, 1_000, 2_000),
                sample(10, 8, 100, 200),
            ])),
        };
        let mut service =
            SystemMonitorService::new(source, Duration::from_secs(1)).expect("valid interval");
        service.refresh(Duration::ZERO);
        let RefreshOutcome::Updated(snapshot) = service.refresh(Duration::from_secs(1)) else {
            panic!("second sample must refresh");
        };

        assert_eq!(snapshot.cpu_usage_percent, None);
        assert_eq!(snapshot.network[0].receive_bytes_per_second, None);
        assert_eq!(snapshot.network[0].transmit_bytes_per_second, None);
    }
}
