//! Network speed-test presentation.

use super::*;

pub(crate) struct SpeedTestPresentation {
    pub snapshot: SpeedTestSnapshot,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeedTestViewModel {
    pub running: bool,
    pub available: bool,
    pub status: String,
    pub disclosure: String,
    pub can_start: bool,
    pub can_cancel: bool,
    pub progress: Option<f64>,
    pub phase_label: Option<String>,
    pub result_lines: Vec<String>,
}

impl SpeedTestViewModel {
    pub(crate) fn from_presentation(presentation: SpeedTestPresentation) -> Self {
        let snapshot = presentation.snapshot;
        let disclosure = snapshot.disclosure();
        let mut progress = None;
        let mut phase_label = None;
        let mut result_lines = Vec::new();
        let status = match &snapshot.status {
            _ if !presentation.running => "The network speed test is not running. Enable \
                                           network.speed_test in the Feature Hub."
                .to_owned(),
            _ if !snapshot.available => {
                "Speed tests need curl. Install curl to run one.".to_owned()
            }
            SpeedTestStatus::Idle => {
                "No test has run. Nothing is sent until you start one.".to_owned()
            }
            SpeedTestStatus::Running(value) => {
                let label = match value.phase {
                    SpeedTestPhase::Latency => "Measuring latency",
                    SpeedTestPhase::Download => "Downloading",
                    SpeedTestPhase::Upload => "Uploading",
                };
                phase_label = Some(label.to_owned());
                progress = (value.phase_bytes > 0).then(|| {
                    (value.transferred_bytes as f64 / value.phase_bytes as f64).clamp(0.0, 1.0)
                });
                format!("{label}…")
            }
            SpeedTestStatus::Completed => {
                if let Some(measurement) = snapshot.last_measurement {
                    if let Some(latency) = measurement.latency_millis {
                        result_lines.push(format!("Latency: {latency} ms"));
                    }
                    if let Some(speed) = measurement.download_bits_per_second {
                        result_lines.push(format!("Download: {} Mbit/s", megabits(speed)));
                    }
                    if let Some(speed) = measurement.upload_bits_per_second {
                        result_lines.push(format!("Upload: {} Mbit/s", megabits(speed)));
                    }
                    result_lines.push(format!(
                        "Transferred: {:.1} MB",
                        measurement
                            .downloaded_bytes
                            .saturating_add(measurement.uploaded_bytes)
                            as f64
                            / 1_000_000.0
                    ));
                }
                "Speed test completed.".to_owned()
            }
            SpeedTestStatus::Cancelled => "Speed test cancelled.".to_owned(),
            SpeedTestStatus::Failed(error) => format!("Speed test failed: {}", error.message),
        };
        Self {
            running: presentation.running,
            available: snapshot.available,
            status,
            disclosure,
            can_start: presentation.running
                && snapshot.available
                && !matches!(snapshot.status, SpeedTestStatus::Running(_)),
            can_cancel: presentation.running
                && matches!(snapshot.status, SpeedTestStatus::Running(_)),
            progress,
            phase_label,
            result_lines,
        }
    }
}

/// Formats bits per second as Mbit/s.
fn megabits(bits_per_second: u64) -> String {
    format!("{:.1}", bits_per_second as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use kestrel_platform::speed_test::{
        CLOUDFLARE_PROVIDER, SpeedTestError, SpeedTestErrorKind, SpeedTestMeasurement,
        SpeedTestPhase, SpeedTestPlan, SpeedTestProgress,
    };

    use std::time::Duration;

    fn speed_test_presentation(
        status: SpeedTestStatus,
        last_measurement: Option<SpeedTestMeasurement>,
        running: bool,
        available: bool,
    ) -> SpeedTestPresentation {
        SpeedTestPresentation {
            snapshot: SpeedTestSnapshot {
                status,
                last_measurement,
                provider: CLOUDFLARE_PROVIDER,
                plan: SpeedTestPlan {
                    download_bytes: 25_000_000,
                    upload_bytes: 10_000_000,
                    phase_timeout: Duration::from_secs(30),
                },
                available,
                generation: 1,
            },
            running,
        }
    }

    #[test]
    fn an_idle_speed_test_discloses_its_destination_before_it_can_start() {
        let view = SpeedTestViewModel::from_presentation(speed_test_presentation(
            SpeedTestStatus::Idle,
            None,
            true,
            true,
        ));

        assert!(view.can_start);
        assert!(!view.can_cancel);
        assert!(view.disclosure.contains("speed.cloudflare.com"));
        assert!(view.disclosure.contains("25 MB"));
        assert!(view.progress.is_none());
    }

    #[test]
    fn a_running_speed_test_reports_progress_and_offers_only_cancel() {
        let view = SpeedTestViewModel::from_presentation(speed_test_presentation(
            SpeedTestStatus::Running(SpeedTestProgress {
                phase: SpeedTestPhase::Download,
                transferred_bytes: 5_000_000,
                phase_bytes: 25_000_000,
            }),
            None,
            true,
            true,
        ));

        assert!(!view.can_start, "one run at a time");
        assert!(view.can_cancel);
        assert_eq!(view.progress, Some(0.2));
        assert_eq!(view.phase_label.as_deref(), Some("Downloading"));
    }

    #[test]
    fn a_speed_test_cannot_start_while_stopped_or_without_curl() {
        for (running, available, reason) in
            [(false, true, "network.speed_test"), (true, false, "curl")]
        {
            let view = SpeedTestViewModel::from_presentation(speed_test_presentation(
                SpeedTestStatus::Idle,
                None,
                running,
                available,
            ));

            assert!(!view.can_start);
            assert!(view.status.contains(reason), "{}", view.status);
        }
    }

    #[test]
    fn completed_and_failed_speed_tests_report_results_and_reasons() {
        let measurement = SpeedTestMeasurement {
            latency_millis: Some(14),
            download_bits_per_second: Some(94_300_000),
            downloaded_bytes: 25_000_000,
            upload_bits_per_second: Some(18_260_000),
            uploaded_bytes: 10_000_000,
            duration: Duration::from_secs(9),
        };
        let completed = SpeedTestViewModel::from_presentation(speed_test_presentation(
            SpeedTestStatus::Completed,
            Some(measurement),
            true,
            true,
        ));
        assert_eq!(
            completed.result_lines,
            vec![
                "Latency: 14 ms".to_owned(),
                "Download: 94.3 Mbit/s".to_owned(),
                "Upload: 18.3 Mbit/s".to_owned(),
                "Transferred: 35.0 MB".to_owned(),
            ]
        );
        assert!(completed.can_start, "a finished test can be repeated");

        let failed = SpeedTestViewModel::from_presentation(speed_test_presentation(
            SpeedTestStatus::Failed(SpeedTestError {
                kind: SpeedTestErrorKind::Connection,
                message: "The speed-test server could not be reached.".to_owned(),
            }),
            Some(measurement),
            true,
            true,
        ));
        assert!(failed.status.contains("could not be reached"));
        assert!(failed.result_lines.is_empty());
    }
}
