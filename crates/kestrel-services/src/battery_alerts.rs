//! Battery-alert lifecycle and bounded polling policy.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use kestrel_platform::quick_toggles::{BatteryAlertSource, BatteryReading, QuickToggleError};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);
pub const LOW_BATTERY_PERCENT: u8 = 15;
const RECOVERY_PERCENT: u8 = 20;

pub struct BatteryAlertService<S: BatteryAlertSource> {
    source: Arc<S>,
    poll_interval: Duration,
    worker: Option<BatteryAlertWorker>,
    last_error: Arc<Mutex<Option<QuickToggleError>>>,
}

struct BatteryAlertWorker {
    stop: SyncSender<()>,
    handle: JoinHandle<()>,
}

impl<S: BatteryAlertSource> BatteryAlertService<S> {
    pub fn new(source: S, poll_interval: Duration) -> Self {
        Self {
            source: Arc::new(source),
            poll_interval,
            worker: None,
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&mut self) -> Result<(), QuickToggleError> {
        if self.worker.is_some() {
            return Ok(());
        }

        let mut alerted = BTreeSet::new();
        let initial = self.source.battery_readings()?;
        process_readings(self.source.as_ref(), &initial, &mut alerted)?;
        *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let (stop, receiver) = mpsc::sync_channel(1);
        let source = Arc::clone(&self.source);
        let last_error = Arc::clone(&self.last_error);
        let poll_interval = self.poll_interval;
        let handle = thread::Builder::new()
            .name("kestrel-battery-alerts".to_owned())
            .spawn(move || {
                loop {
                    match receiver.recv_timeout(poll_interval) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            let result = source.battery_readings().and_then(|readings| {
                                process_readings(source.as_ref(), &readings, &mut alerted)
                            });
                            *last_error
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) = result.err();
                        }
                    }
                }
            })
            .map_err(|error| {
                QuickToggleError::new(
                    kestrel_platform::quick_toggles::QuickToggleErrorKind::Io,
                    format!("starting the battery-alert worker failed: {error}"),
                )
            })?;
        self.worker = Some(BatteryAlertWorker { stop, handle });
        Ok(())
    }

    pub fn stop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        let _ = worker.stop.send(());
        let _ = worker.handle.join();
    }

    pub fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    pub fn last_error(&self) -> Option<QuickToggleError> {
        self.last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl<S: BatteryAlertSource> Drop for BatteryAlertService<S> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn process_readings<S: BatteryAlertSource>(
    source: &S,
    readings: &[BatteryReading],
    alerted: &mut BTreeSet<String>,
) -> Result<(), QuickToggleError> {
    for reading in readings {
        let discharging = reading
            .status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("discharging"));
        let Some(capacity) = reading.capacity else {
            continue;
        };
        if !discharging || capacity > RECOVERY_PERCENT {
            alerted.remove(&reading.name);
            continue;
        }
        if capacity <= LOW_BATTERY_PERCENT && !alerted.contains(&reading.name) {
            source.notify_low_battery(
                "Low battery",
                &format!(
                    "{} is at {}% and discharging. Connect power.",
                    reading.name, capacity
                ),
            )?;
            alerted.insert(reading.name.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use kestrel_platform::quick_toggles::{BatteryAlertSource, BatteryReading, QuickToggleError};

    use super::BatteryAlertService;

    #[derive(Default)]
    struct FakeSource {
        readings: Mutex<Vec<BatteryReading>>,
        notifications: Mutex<Vec<String>>,
        reads: AtomicUsize,
    }

    impl FakeSource {
        fn with_reading(capacity: u8, status: &str) -> Self {
            Self {
                readings: Mutex::new(vec![BatteryReading {
                    name: "BAT0".to_owned(),
                    capacity: Some(capacity),
                    status: Some(status.to_owned()),
                }]),
                notifications: Mutex::new(Vec::new()),
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl BatteryAlertSource for FakeSource {
        fn battery_readings(&self) -> Result<Vec<BatteryReading>, QuickToggleError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.readings.lock().expect("readings mutex").clone())
        }

        fn notify_low_battery(&self, _summary: &str, body: &str) -> Result<(), QuickToggleError> {
            self.notifications
                .lock()
                .expect("notifications mutex")
                .push(body.to_owned());
            Ok(())
        }
    }

    #[test]
    fn low_discharging_battery_does_not_repeat_while_unchanged() {
        let source = FakeSource::with_reading(10, "Discharging");
        let mut service = BatteryAlertService::new(source, Duration::from_millis(5));

        service.start().expect("monitor starts");
        for _ in 0..100 {
            if service.source.reads.load(Ordering::Relaxed) >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(service.source.reads.load(Ordering::Relaxed) >= 2);

        let notifications = service
            .source
            .notifications
            .lock()
            .expect("notifications mutex");
        assert_eq!(
            notifications.as_slice(),
            ["BAT0 is at 10% and discharging. Connect power."]
        );
        drop(notifications);
        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn charging_battery_does_not_alert() {
        let source = FakeSource::with_reading(5, "Charging");
        let mut service = BatteryAlertService::new(source, Duration::from_millis(5));

        service.start().expect("monitor starts");
        thread::sleep(Duration::from_millis(10));
        service.stop();

        assert!(
            service
                .source
                .notifications
                .lock()
                .expect("notifications mutex")
                .is_empty()
        );
    }
}
