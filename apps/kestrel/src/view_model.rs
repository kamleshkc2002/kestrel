use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, CapabilityStatus, CostLevel,
    MonitorConfiguration, MonitorReadout, PanelSection, Permission, ResourceCost,
    SnippetExpansionTiming, SnippetProviderPreference,
};
use kestrel_platform::quick_toggles::{
    MutationConfirmation, QuickToggleControl, QuickToggleId, ToggleAction,
};
use kestrel_platform::snippets::InsertionProvider;
use kestrel_platform::speed_test::SpeedTestPhase;
use kestrel_services::{
    ServiceLifecycle, ServiceRegistration,
    alerts::{ActiveAlert, AlertPolicy, AlertSnapshot},
    audio::{AudioAvailability, AudioPolicy, AudioSnapshot},
    clipboard::{ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
    microphone::{MicrophoneAvailability, MicrophoneMuteState, MicrophoneSnapshot},
    quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    snippets::{SnippetLibrary, SnippetMatch, SnippetPolicy},
    speed_test::{SpeedTestSnapshot, SpeedTestStatus},
    system_monitor::{HistorySummary, SystemSnapshot},
};

use crate::ConfigurationWarning;

/// Inputs for the monitor view.
pub(crate) struct MonitorPresentation<'a> {
    pub snapshot: Option<&'a SystemSnapshot>,
    pub history: Option<HistorySummary>,
    pub alerts: AlertSnapshot,
    pub running: bool,
    /// Effective rules after quick-toggle gates.
    pub policy: Option<&'a AlertPolicy>,
}

/// Monitor panel state.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorViewModel {
    pub running: bool,
    pub status: String,
    pub readouts: Vec<MonitorReadoutViewModel>,
    pub readout_settings: Vec<MonitorReadoutSettingViewModel>,
    pub alerts: Vec<ActiveAlertViewModel>,
    pub delivery_failures: Vec<(AlertKind, String)>,
    pub alert_rules: Vec<AlertRuleViewModel>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAlertViewModel {
    pub kind: AlertKind,
    pub label: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertRuleViewModel {
    pub kind: AlertKind,
    pub label: &'static str,
    pub enabled: bool,
    pub threshold: f64,
    pub unit: &'static str,
    pub summary: String,
}

impl MonitorViewModel {
    /// Builds the monitor presentation.
    pub(crate) fn from_configuration(
        configuration: &MonitorConfiguration,
        presentation: MonitorPresentation<'_>,
    ) -> Self {
        let snapshot = presentation.snapshot;
        let readouts = configuration
            .readouts
            .iter()
            .copied()
            .map(|readout| monitor_readout(snapshot, readout))
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
                MonitorReadoutSettingViewModel {
                    readout,
                    label: readout.label(),
                    visible: position.is_some(),
                    can_move_up: position.is_some_and(|position| position > 0),
                    can_move_down: position.is_some_and(|position| position + 1 < visible_count),
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
                let effective = presentation.policy.and_then(|policy| policy.rule(kind));
                let enabled = effective.map_or(configured.enabled, |rule| rule.enabled);
                let threshold = effective.map_or(configured.threshold, |rule| rule.threshold);
                let sustain_samples =
                    effective.map_or(configured.sustain_samples, |rule| rule.sustain_samples);
                let cooldown_seconds =
                    effective.map_or(configured.cooldown_seconds, |rule| rule.cooldown.as_secs());
                let mut summary = format!(
                    "Alert {} {}{} for {} samples; repeat at most every {} s",
                    if kind.alerts_above() {
                        "at or above"
                    } else {
                        "at or below"
                    },
                    format_threshold(threshold),
                    kind.unit(),
                    sustain_samples,
                    cooldown_seconds
                );
                if configured.enabled && !enabled {
                    summary.push_str("; currently paused by its quick toggle");
                }
                AlertRuleViewModel {
                    kind,
                    label: kind.label(),
                    enabled,
                    threshold,
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
        Self {
            running: presentation.running,
            status,
            readouts,
            readout_settings,
            alerts,
            delivery_failures,
            alert_rules,
        }
    }
}

fn format_threshold(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
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
        kind: alert.kind,
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
fn format_bytes(bytes: f64) -> String {
    if !bytes.is_finite() || bytes < 0.0 {
        return "Unavailable".to_owned();
    }
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
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
        MonitorReadout::Disk => {
            source == "/proc/diskstats" || source.starts_with("/proc/self/mounts")
        }
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
                MonitorReadoutViewModel {
                    readout,
                    label: readout.label(),
                    value: Some(format!("{}/s", format_bytes(rate))),
                    detail: format!("{} interfaces", snapshot.network.len()),
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
            })
            .filter_map(|supply| supply.capacity_percent.map(|capacity| (supply, capacity)))
            .min_by_key(|(_, capacity)| *capacity)
            .map(|(supply, capacity)| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(capacity as f64)),
                detail: supply.name.clone(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "Battery capacity is unavailable.")
            }),
        MonitorReadout::Gpu => snapshot
            .gpu
            .iter()
            .filter_map(|gpu| gpu.busy_percent)
            .max()
            .map(|value| MonitorReadoutViewModel {
                readout,
                label: readout.label(),
                value: Some(percentage(value as f64)),
                detail: "GPU activity".to_owned(),
            })
            .unwrap_or_else(|| {
                unavailable(Some(snapshot), readout, "GPU activity is unavailable.")
            }),
    }
}

/// Inputs for the audio view.
pub(crate) struct AudioPresentation<'a> {
    /// Snapshot, if the opt-in service has produced one.
    pub snapshot: Option<&'a AudioSnapshot>,
    /// Whether the mixer registration is running.
    pub running: bool,
    /// Effective mixer policy.
    pub policy: AudioPolicy,
}

/// Audio mixer panel state.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioViewModel {
    pub running: bool,
    pub status: String,
    /// Last switch or device-loss message.
    pub message: Option<String>,
    pub boost_ceiling_percent: u8,
    pub max_boost_percent: u8,
    pub output_groups: Vec<AudioOutputGroupViewModel>,
    /// Outputs in discovery order.
    pub outputs: Vec<AudioOutputViewModel>,
    pub streams: Vec<AudioStreamViewModel>,
    pub default_output_id: Option<u32>,
    pub inactive_streams: usize,
    pub policy: AudioPolicyViewModel,
}

/// Outputs sharing a hardware card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputGroupViewModel {
    pub label: String,
    pub outputs: Vec<AudioOutputViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputViewModel {
    pub id: u32,
    pub title: String,
    /// Backend name distinguishing similar devices.
    pub name: String,
    pub detail: String,
    pub is_default: bool,
    pub volume_percent: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStreamViewModel {
    pub id: u32,
    pub title: String,
    pub detail: String,
    pub volume_percent: Option<u8>,
    pub volume_writable: bool,
    pub muted: bool,
    pub output_id: u32,
    pub inactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPolicyViewModel {
    pub boost_percent: u8,
    pub max_boost_percent: u8,
    pub move_all_streams: bool,
    pub output_switch_label: &'static str,
    pub reset_volume_on_disconnect: bool,
    pub disconnect_policy_label: &'static str,
    pub disconnect_volume_percent: u8,
    pub include_inactive_streams: bool,
}

impl AudioViewModel {
    pub(crate) fn from_presentation(
        configuration: &kestrel_core::AudioConfiguration,
        presentation: AudioPresentation<'_>,
    ) -> Self {
        let policy = presentation.policy;
        let policy_view = AudioPolicyViewModel {
            boost_percent: policy.boost_percent,
            max_boost_percent: kestrel_core::MAX_AUDIO_BOOST_PERCENT,
            move_all_streams: policy.output_switch == kestrel_core::AudioOutputSwitch::AllStreams,
            output_switch_label: policy.output_switch.label(),
            reset_volume_on_disconnect: policy.disconnect_policy
                == kestrel_core::AudioDisconnectPolicy::ResetVolume,
            disconnect_policy_label: policy.disconnect_policy.label(),
            disconnect_volume_percent: policy.disconnect_volume_percent,
            include_inactive_streams: policy.include_inactive_streams,
        };
        let Some(snapshot) = presentation.snapshot.filter(|_| presentation.running) else {
            return Self {
                running: presentation.running,
                status: if presentation.running {
                    "The audio mixer has not produced a snapshot yet.".to_owned()
                } else {
                    "The audio mixer is not running. Enable audio.mixer in the Feature Hub to \
                     load the PulseAudio-compatible mixer."
                        .to_owned()
                },
                message: None,
                boost_ceiling_percent: configuration.boost_percent,
                max_boost_percent: kestrel_core::MAX_AUDIO_BOOST_PERCENT,
                output_groups: Vec::new(),
                outputs: Vec::new(),
                streams: Vec::new(),
                default_output_id: None,
                inactive_streams: 0,
                policy: policy_view,
            };
        };

        let outputs = snapshot
            .outputs
            .iter()
            .map(|output| AudioOutputViewModel {
                id: output.id,
                title: output.description.clone(),
                name: output.name.clone(),
                detail: audio_output_detail(output),
                is_default: output.is_default,
                volume_percent: output.volume_percent,
                muted: output.muted,
            })
            .collect::<Vec<_>>();
        let output_groups = snapshot
            .outputs_by_card()
            .into_iter()
            .map(|(label, devices)| AudioOutputGroupViewModel {
                label: label.unwrap_or_else(|| "Other outputs".to_owned()),
                outputs: devices
                    .into_iter()
                    .map(|output| AudioOutputViewModel {
                        id: output.id,
                        title: output.description.clone(),
                        name: output.name.clone(),
                        detail: audio_output_detail(output),
                        is_default: output.is_default,
                        volume_percent: output.volume_percent,
                        muted: output.muted,
                    })
                    .collect(),
            })
            .collect();
        let streams = snapshot
            .streams
            .iter()
            .map(|stream| AudioStreamViewModel {
                id: stream.id,
                title: stream
                    .application_name
                    .clone()
                    .or_else(|| stream.media_name.clone())
                    .unwrap_or_else(|| stream.name.clone()),
                detail: audio_stream_detail(stream, &snapshot.outputs),
                volume_percent: stream.volume_percent,
                volume_writable: stream.volume_writable,
                muted: stream.muted,
                output_id: stream.output_id,
                inactive: stream.corked,
            })
            .collect();

        Self {
            running: true,
            status: audio_status(snapshot),
            message: audio_message(snapshot),
            boost_ceiling_percent: snapshot.boost_ceiling_percent,
            max_boost_percent: kestrel_core::MAX_AUDIO_BOOST_PERCENT,
            output_groups,
            outputs,
            streams,
            default_output_id: snapshot.default_output().map(|output| output.id),
            inactive_streams: snapshot.inactive_streams,
            policy: policy_view,
        }
    }
}

pub(crate) struct MicrophonePresentation<'a> {
    pub snapshot: &'a MicrophoneSnapshot,
    pub running: bool,
}

/// One capture input as the window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneInputViewModel {
    pub id: u32,
    pub label: String,
    pub muted: bool,
    pub is_default: bool,
}

/// Microphone state from the latest backend reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneViewModel {
    pub running: bool,
    pub status: String,
    pub mute_label: String,
    /// `Some` only when all inputs agree.
    pub muted: Option<bool>,
    /// Whether inputs have mixed mute states.
    pub mixed: bool,
    pub inputs: Vec<MicrophoneInputViewModel>,
    pub default_input_id: Option<u32>,
    /// Device-loss notice from the last refresh.
    pub message: Option<String>,
}

impl MicrophoneViewModel {
    pub(crate) fn from_presentation(presentation: MicrophonePresentation<'_>) -> Self {
        if !presentation.running {
            return Self {
                running: false,
                status: "The microphone control is not running. Enable audio.microphone in the \
                         Feature Hub."
                    .to_owned(),
                mute_label: "Microphone state is unknown".to_owned(),
                muted: None,
                mixed: false,
                inputs: Vec::new(),
                default_input_id: None,
                message: None,
            };
        }
        let snapshot = presentation.snapshot;
        let (mute_label, muted, mixed) = match snapshot.mute {
            MicrophoneMuteState::Unknown => ("Microphone state is unknown".to_owned(), None, false),
            MicrophoneMuteState::NoInputs => {
                ("No microphone inputs are available".to_owned(), None, false)
            }
            MicrophoneMuteState::Live => ("Microphone is live".to_owned(), Some(false), false),
            MicrophoneMuteState::Muted => ("Microphone is muted".to_owned(), Some(true), false),
            MicrophoneMuteState::Mixed { muted, total } => {
                (format!("{muted} of {total} inputs muted"), None, true)
            }
        };
        let status = match snapshot.availability {
            MicrophoneAvailability::Unavailable => {
                "The PulseAudio-compatible server is unreachable, so the microphone state is \
                 unknown."
                    .to_owned()
            }
            MicrophoneAvailability::NoInputs => {
                "No microphone or other input device is connected.".to_owned()
            }
            MicrophoneAvailability::Ready => match snapshot.inputs.len() {
                1 => "1 input available".to_owned(),
                count => format!("{count} inputs available"),
            },
        };
        let message = if snapshot.default_input_lost {
            Some("The default input disconnected. Choose another input below.".to_owned())
        } else if snapshot.lost_inputs > 0 {
            Some(match snapshot.lost_inputs {
                1 => "An input disconnected.".to_owned(),
                count => format!("{count} inputs disconnected."),
            })
        } else {
            None
        };
        Self {
            running: true,
            status,
            mute_label,
            muted,
            mixed,
            inputs: snapshot
                .inputs
                .iter()
                .map(|input| MicrophoneInputViewModel {
                    id: input.id,
                    label: input.description.clone(),
                    muted: input.muted,
                    is_default: input.is_default,
                })
                .collect(),
            default_input_id: snapshot.default_input_id,
            message,
        }
    }
}

pub(crate) struct SpeedTestPresentation {
    pub snapshot: SpeedTestSnapshot,
    pub running: bool,
}

/// Speed-test panel state.
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

fn audio_status(snapshot: &AudioSnapshot) -> String {
    match snapshot.availability {
        AudioAvailability::Unavailable => {
            "The PulseAudio-compatible server is unreachable; check the audio service for this \
             session."
                .to_owned()
        }
        AudioAvailability::NoOutputDevices => {
            "The server reports no output devices. Connect or enable an output device.".to_owned()
        }
        AudioAvailability::NoActiveStreams => {
            "No application is playing audio. Output devices remain adjustable.".to_owned()
        }
        AudioAvailability::Ready => {
            let hidden = snapshot.inactive_streams;
            let suffix = if hidden == 0 {
                String::new()
            } else {
                format!(" ({hidden} inactive hidden)")
            };
            // Inactive streams are excluded from the playing count.
            let playing = snapshot
                .streams
                .iter()
                .filter(|stream| !stream.corked)
                .count();
            format!(
                "{} outputs · {playing} playing streams{suffix}",
                snapshot.outputs.len()
            )
        }
    }
}

fn audio_message(snapshot: &AudioSnapshot) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(switch) = &snapshot.last_switch {
        let mut text = format!("Switched the default output to {}.", switch.output_name);
        if switch.moved_streams > 0 {
            text.push_str(&format!(" Moved {} streams.", switch.moved_streams));
        }
        if switch.failed_moves > 0 {
            text.push_str(&format!(" {} moves failed.", switch.failed_moves));
        }
        parts.push(text);
    }
    if let Some(reconcile) = &snapshot.last_reconcile {
        let ids = reconcile
            .lost_output_ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut text = format!(
            "Output {ids} disappeared; re-homed {} streams.",
            reconcile.rehomed_streams
        );
        if reconcile.volume_resets > 0 {
            text.push_str(&format!(
                " Reset {} stream volumes.",
                reconcile.volume_resets
            ));
        }
        if reconcile.failures > 0 {
            text.push_str(&format!(" {} repairs failed.", reconcile.failures));
        }
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn audio_output_detail(output: &kestrel_platform::audio::OutputDevice) -> String {
    let mut parts = Vec::new();
    if output.is_default {
        parts.push("Default output".to_owned());
    }
    parts.push(format!("{}%", output.volume_percent));
    if output.muted {
        parts.push("Muted".to_owned());
    }
    if let Some(port) = &output.port_description {
        parts.push(port.clone());
    }
    parts.join(" · ")
}

fn audio_stream_detail(
    stream: &kestrel_platform::audio::PlaybackStream,
    outputs: &[kestrel_platform::audio::OutputDevice],
) -> String {
    let target = outputs
        .iter()
        .find(|output| output.id == stream.output_id)
        .map(|output| output.description.clone())
        .unwrap_or_else(|| "Output no longer available".to_owned());
    let mut parts = vec![format!("Playing on {target}")];
    if let Some(media) = &stream.media_name {
        parts.push(media.clone());
    }
    if stream.corked {
        parts.push("Inactive".to_owned());
    }
    parts.join(" · ")
}

/// Inputs for the clipboard view.
#[derive(Default)]
pub(crate) struct ClipboardPresentation<'a> {
    /// Metadata only; no clipboard content.
    pub snapshot: Option<ClipboardSnapshot>,
    pub running: bool,
    pub search_query: &'a str,
    pub matches: &'a [ClipboardMatch],
    pub preview: Option<&'a ClipboardPreview>,
}

/// Clipboard history panel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardViewModel {
    pub running: bool,
    pub status: String,
    /// Entry kinds supported by the active provider.
    pub kind_support: String,
    pub search_query: String,
    pub items: Vec<ClipboardItemViewModel>,
    pub total_items: usize,
    pub total_bytes: usize,
    pub pinned_items: usize,
    pub rejected_oversize_items: u64,
    pub filtered_sensitive_items: u64,
    pub selection_clears: u64,
    pub wipe_count: u64,
    pub last_filtered_pattern: Option<&'static str>,
    pub preview: Option<ClipboardPreviewViewModel>,
    pub policy: ClipboardPolicyViewModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItemViewModel {
    pub id: u64,
    pub kind: &'static str,
    pub detail: String,
    pub preview: String,
    pub pinned: bool,
    pub text_editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPreviewViewModel {
    pub id: u64,
    pub kind: &'static str,
    pub text: String,
    pub truncated: bool,
    pub editable: bool,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
}

/// Editable policy values plus the bounds the controls must enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicyViewModel {
    pub max_items: u32,
    pub max_item_bytes: u32,
    pub max_image_bytes: u32,
    pub max_file_entries: u32,
    pub max_total_bytes: u32,
    pub max_age_hours: u32,
    pub clear_seconds: u64,
    pub filter_sensitive: bool,
    pub paste_plain_text: bool,
    pub bounds: ClipboardBoundsViewModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardBoundsViewModel {
    pub min_item_bytes: u32,
    pub max_item_bytes: u32,
    pub max_image_bytes: u32,
    pub max_file_entries: u32,
    pub max_items: u32,
    pub max_total_bytes: u32,
    pub max_age_hours: u32,
    pub max_clear_seconds: u64,
}

impl ClipboardViewModel {
    pub(crate) fn from_presentation(
        configuration: &kestrel_core::ClipboardConfiguration,
        presentation: ClipboardPresentation<'_>,
    ) -> Self {
        let bounds = ClipboardBoundsViewModel {
            min_item_bytes: kestrel_core::MIN_CLIPBOARD_ITEM_BYTES,
            max_item_bytes: kestrel_core::MAX_CLIPBOARD_ITEM_BYTES,
            max_image_bytes: kestrel_core::MAX_CLIPBOARD_IMAGE_BYTES,
            max_file_entries: kestrel_core::MAX_CLIPBOARD_FILE_ENTRIES,
            max_items: kestrel_core::MAX_CLIPBOARD_MAX_ITEMS,
            max_total_bytes: kestrel_core::MAX_CLIPBOARD_TOTAL_BYTES,
            max_age_hours: kestrel_core::MAX_CLIPBOARD_AGE_HOURS,
            max_clear_seconds: kestrel_core::MAX_CLIPBOARD_CLEAR_SECONDS,
        };
        let policy = ClipboardPolicyViewModel {
            max_items: configuration.max_items,
            max_item_bytes: configuration.max_item_bytes,
            max_image_bytes: configuration.max_image_bytes,
            max_file_entries: configuration.max_file_entries,
            max_total_bytes: configuration.max_total_bytes,
            max_age_hours: configuration.max_age_hours,
            clear_seconds: configuration.clear_seconds,
            filter_sensitive: configuration.filter_sensitive,
            paste_plain_text: configuration.paste_plain_text,
            bounds,
        };
        let Some(snapshot) = presentation
            .snapshot
            .as_ref()
            .filter(|_| presentation.running)
        else {
            return Self {
                running: presentation.running,
                status: if presentation.running {
                    "The clipboard history has not produced a snapshot yet.".to_owned()
                } else {
                    "Clipboard history is not running. Enable clipboard.history in the Feature \
                     Hub to retain entries."
                        .to_owned()
                },
                kind_support: "unavailable".to_owned(),
                search_query: presentation.search_query.to_owned(),
                items: Vec::new(),
                total_items: 0,
                total_bytes: 0,
                pinned_items: 0,
                rejected_oversize_items: 0,
                filtered_sensitive_items: 0,
                selection_clears: 0,
                wipe_count: 0,
                last_filtered_pattern: None,
                preview: presentation.preview.map(clipboard_preview),
                policy,
            };
        };

        // Render only explicit search matches.
        let items = presentation
            .matches
            .iter()
            .map(|matched| {
                let metadata = snapshot
                    .items
                    .iter()
                    .find(|item| item.id == matched.id)
                    .cloned();
                ClipboardItemViewModel {
                    id: matched.id,
                    kind: matched.kind.label(),
                    detail: metadata
                        .as_ref()
                        .map(clipboard_item_detail)
                        .unwrap_or_else(|| "Retained entry".to_owned()),
                    preview: matched.preview.clone(),
                    pinned: metadata.as_ref().is_some_and(|item| item.pinned),
                    text_editable: matched.kind
                        == kestrel_platform::clipboard::ClipboardEntryKind::Text,
                }
            })
            .collect::<Vec<_>>();

        Self {
            running: true,
            status: clipboard_status(snapshot, presentation.matches.len()),
            kind_support: snapshot.kind_support.evidence_value(),
            search_query: presentation.search_query.to_owned(),
            items,
            total_items: snapshot.items.len(),
            total_bytes: snapshot.total_bytes,
            pinned_items: snapshot.pinned_items,
            rejected_oversize_items: snapshot.rejected_oversize_items,
            filtered_sensitive_items: snapshot.filtered_sensitive_items,
            selection_clears: snapshot.selection_clears,
            wipe_count: snapshot.wipe_count,
            last_filtered_pattern: snapshot.last_filtered_pattern,
            preview: presentation.preview.map(clipboard_preview),
            policy,
        }
    }
}

fn clipboard_status(snapshot: &ClipboardSnapshot, shown: usize) -> String {
    let mut status = format!(
        "{} of {} retained entries shown · {} retained · captured kinds: {}",
        shown,
        snapshot.items.len(),
        format_clipboard_bytes(snapshot.total_bytes),
        snapshot.kind_support.evidence_value()
    );
    if snapshot.pinned_items > 0 {
        status.push_str(&format!(" · {} pinned", snapshot.pinned_items));
    }
    if snapshot.filtered_sensitive_items > 0 {
        status.push_str(&format!(
            " · {} filtered as sensitive",
            snapshot.filtered_sensitive_items
        ));
    }
    if snapshot.selection_clears > 0 {
        status.push_str(&format!(
            " · {} automatic selection clears",
            snapshot.selection_clears
        ));
    }
    if snapshot.rejected_oversize_items > 0 {
        status.push_str(&format!(
            " · {} over the size bound",
            snapshot.rejected_oversize_items
        ));
    }
    if let Some(error) = &snapshot.last_error {
        status.push_str(&format!(" · last error: {error}"));
    }
    status
}

fn clipboard_item_detail(metadata: &kestrel_services::clipboard::ClipboardItemMetadata) -> String {
    let mut parts = vec![
        metadata.kind.label().to_owned(),
        format_clipboard_bytes(metadata.size_bytes),
        format_age(metadata.age),
    ];
    if metadata.pinned {
        parts.push("pinned".to_owned());
    }
    if let Some((width, height)) = metadata.image_dimensions {
        parts.push(format!("{width}×{height}"));
    }
    if let Some(count) = metadata.file_count {
        parts.push(format!("{count} files"));
    }
    parts.join(" · ")
}

fn clipboard_preview(preview: &ClipboardPreview) -> ClipboardPreviewViewModel {
    ClipboardPreviewViewModel {
        id: preview.id,
        kind: preview.kind.label(),
        text: preview.text.clone(),
        truncated: preview.truncated,
        editable: preview.kind == kestrel_platform::clipboard::ClipboardEntryKind::Text,
        image_dimensions: preview.image_dimensions,
        file_count: preview.file_count,
    }
}

fn format_clipboard_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_age(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0 => "just now".to_owned(),
        1..=59 => format!("{seconds} s old"),
        60..=3599 => format!("{} min old", seconds / 60),
        _ => format!("{} h old", seconds / 3600),
    }
}

/// A snippet draft before validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetDraft {
    pub name: String,
    pub folder: String,
    pub trigger: String,
    pub content: String,
}

/// Clipboard query state.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClipboardQuery<'a> {
    pub query: &'a str,
    pub matches: &'a [ClipboardMatch],
    pub preview: Option<&'a ClipboardPreview>,
}

/// Snippet query and draft state.
#[derive(Debug, Clone, Copy, Default)]
pub struct SnippetQuery<'a> {
    pub query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
}

/// Command-bar query state.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandBarQuery<'a> {
    pub query: &'a str,
    pub results: &'a [kestrel_services::command_bar::CommandResult],
}

/// Inputs for the snippet view.
pub(crate) struct SnippetsPresentation<'a> {
    pub library: &'a SnippetLibrary,
    pub policy: SnippetPolicy,
    pub running: bool,
    /// Verified insertion provider, if any.
    pub provider: Option<InsertionProvider>,
    pub unavailable_reason: Option<&'a str>,
    pub directory: Option<String>,
    pub expansion_timing: SnippetExpansionTiming,
    /// Configured preference, which may differ from discovery.
    pub provider_preference: SnippetProviderPreference,
    pub search_query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
    pub warnings: &'a [ConfigurationWarning],
}

/// Snippet library panel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetsViewModel {
    pub running: bool,
    pub status: String,
    pub insertion_status: String,
    pub insertion_available: bool,
    pub provider: Option<&'static str>,
    pub transport: Option<&'static str>,
    pub directory: Option<String>,
    pub expansion_label: &'static str,
    pub expansion_active: bool,
    pub search_query: String,
    pub items: Vec<SnippetItemViewModel>,
    pub total: usize,
    pub folders: Vec<String>,
    pub draft: Option<SnippetDraftViewModel>,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub policy: SnippetPolicyViewModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetItemViewModel {
    pub name: String,
    pub folder: Option<String>,
    pub trigger: Option<String>,
    pub detail: String,
    pub preview: String,
    pub preview_truncated: bool,
    pub variables: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetDraftViewModel {
    pub name: String,
    pub folder: String,
    pub trigger: String,
    pub content: String,
    pub is_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetPolicyViewModel {
    pub max_content_bytes: u32,
    pub clipboard_variable_bytes: u32,
    pub insert_timeout_millis: u64,
    pub preferred_provider: SnippetProviderPreference,
    pub expansion_timing: SnippetExpansionTiming,
    pub bounds: SnippetBoundsViewModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetBoundsViewModel {
    pub min_content_bytes: u32,
    pub max_content_bytes: u32,
    pub min_clipboard_bytes: u32,
    pub max_clipboard_bytes: u32,
    pub min_insert_timeout_millis: u64,
    pub max_insert_timeout_millis: u64,
}

impl SnippetsViewModel {
    pub(crate) fn from_presentation(presentation: SnippetsPresentation<'_>) -> Self {
        let policy = SnippetPolicyViewModel {
            max_content_bytes: presentation.policy.max_content_bytes,
            clipboard_variable_bytes: presentation.policy.clipboard_variable_bytes,
            insert_timeout_millis: presentation.policy.insert_timeout.as_millis() as u64,
            preferred_provider: presentation.provider_preference,
            expansion_timing: presentation.expansion_timing,
            bounds: SnippetBoundsViewModel {
                min_content_bytes: kestrel_core::MIN_SNIPPET_CONTENT_BYTES,
                max_content_bytes: kestrel_core::MAX_SNIPPET_CONTENT_BYTES,
                min_clipboard_bytes: kestrel_core::MIN_SNIPPET_CLIPBOARD_BYTES,
                max_clipboard_bytes: kestrel_core::MAX_SNIPPET_CLIPBOARD_BYTES,
                min_insert_timeout_millis: kestrel_core::MIN_SNIPPET_INSERT_TIMEOUT_MILLIS,
                max_insert_timeout_millis: kestrel_core::MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
            },
        };
        let provider = presentation.provider;
        let insertion_available = provider.is_some();
        let insertion_status = match provider {
            Some(provider) => format!(
                "Insertion uses {} ({})",
                provider.label(),
                provider.transport()
            ),
            None => presentation
                .unavailable_reason
                .map(str::to_owned)
                .unwrap_or_else(|| "no verified insertion provider".to_owned()),
        };
        let expansion_active = insertion_available
            && presentation.expansion_timing == SnippetExpansionTiming::Delimiter
            && false;
        let items = presentation
            .matches
            .iter()
            .map(|matched| SnippetItemViewModel {
                name: matched.name.clone(),
                folder: matched.folder.clone(),
                trigger: matched.trigger.clone(),
                detail: snippet_detail(matched),
                preview: matched.preview.clone(),
                preview_truncated: matched.preview_truncated,
                variables: matched
                    .variables
                    .iter()
                    .map(|variable| variable.label())
                    .collect(),
            })
            .collect::<Vec<_>>();
        let total = presentation.library.len();

        Self {
            running: presentation.running,
            status: if presentation.running {
                let mut status = format!("{} of {total} snippets shown", items.len());
                if !presentation.library.folders().is_empty() {
                    status.push_str(&format!(
                        " · {} folders",
                        presentation.library.folders().len()
                    ));
                }
                status.push_str(&format!(
                    " · content bound {}",
                    format_clipboard_bytes(presentation.policy.max_content_bytes as usize)
                ));
                status
            } else {
                "Text snippets are not running. Enable snippets.text in the Feature Hub.".to_owned()
            },
            insertion_status,
            insertion_available,
            provider: provider.map(InsertionProvider::label),
            transport: provider.map(InsertionProvider::transport),
            directory: presentation.directory,
            expansion_label: presentation.expansion_timing.label(),
            expansion_active,
            search_query: presentation.search_query.to_owned(),
            items,
            total,
            folders: presentation.library.folders(),
            draft: presentation.draft.map(|draft| SnippetDraftViewModel {
                name: draft.name.clone(),
                folder: draft.folder.clone(),
                trigger: draft.trigger.clone(),
                content: draft.content.clone(),
                is_new: draft.name.trim().is_empty(),
            }),
            warnings: presentation
                .warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            policy,
        }
    }
}

fn snippet_detail(matched: &SnippetMatch) -> String {
    let mut parts = Vec::new();
    if let Some(folder) = &matched.folder {
        parts.push(folder.clone());
    }
    if let Some(trigger) = &matched.trigger {
        parts.push(format!("trigger {trigger}"));
    }
    if matched.variables.is_empty() {
        parts.push("no variables".to_owned());
    } else {
        parts.push(
            matched
                .variables
                .iter()
                .map(|variable| variable.token())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    parts.join(" · ")
}

/// Inputs for the command-bar view.
pub(crate) struct CommandBarPresentation<'a> {
    pub running: bool,
    pub max_results: u32,
    pub providers: kestrel_services::command_bar::EnabledProviders,
    /// Providers usable in this session.
    pub launcher: Option<&'a str>,
    pub applications: usize,
    pub file_roots: usize,
    pub scripts: usize,
    /// Query and ranked results.
    pub query: &'a str,
    pub results: &'a [kestrel_services::command_bar::CommandResult],
    pub ranking: &'a kestrel_services::command_bar::CommandRanking,
    pub warnings: &'a [ConfigurationWarning],
}

impl Default for CommandBarPresentation<'_> {
    fn default() -> Self {
        Self {
            running: false,
            max_results: kestrel_core::DEFAULT_COMMAND_RESULTS,
            providers: kestrel_services::command_bar::EnabledProviders::default(),
            launcher: None,
            applications: 0,
            file_roots: 0,
            scripts: 0,
            query: "",
            results: &[],
            ranking: &EMPTY_COMMAND_RANKING,
            warnings: &[],
        }
    }
}

/// Shared empty ranking for default presentations.
static EMPTY_COMMAND_RANKING: kestrel_services::command_bar::CommandRanking =
    kestrel_services::command_bar::CommandRanking::EMPTY;

/// Command-bar panel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBarViewModel {
    pub running: bool,
    pub status: String,
    pub query: String,
    pub max_results: u32,
    pub providers: Vec<CommandProviderViewModel>,
    pub results: Vec<CommandResultViewModel>,
    pub ranking_entries: Vec<CommandRankingViewModel>,
    pub pinned: Vec<String>,
    pub warnings: Vec<ConfigurationWarningViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProviderViewModel {
    pub provider: CommandProvider,
    pub label: &'static str,
    pub enabled: bool,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResultViewModel {
    pub id: String,
    pub source: &'static str,
    pub title: String,
    pub detail: String,
    pub matched_on: &'static str,
    pub pinned: bool,
    pub uses: u32,
    pub action_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRankingViewModel {
    pub id: String,
    pub uses: u32,
    pub pinned: bool,
}

/// Providers available to the command bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandProvider {
    Applications,
    Files,
    Scripts,
    Emoji,
}

impl CommandProvider {
    pub const ALL: [CommandProvider; 4] = [
        CommandProvider::Applications,
        CommandProvider::Files,
        CommandProvider::Scripts,
        CommandProvider::Emoji,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Applications => "Applications",
            Self::Files => "Files (configured roots)",
            Self::Scripts => "Scripts",
            Self::Emoji => "Emoji",
        }
    }

    pub const fn requirement(self) -> &'static str {
        match self {
            Self::Applications => "XDG application directories",
            Self::Files => "At least one configured root; Kestrel never indexes everything",
            Self::Scripts => "Script definitions in the configuration",
            Self::Emoji => "Built in",
        }
    }
}

impl CommandBarViewModel {
    pub(crate) fn from_presentation(presentation: CommandBarPresentation<'_>) -> Self {
        let providers = vec![
            CommandProviderViewModel {
                provider: CommandProvider::Applications,
                label: CommandProvider::Applications.label(),
                enabled: presentation.providers.applications,
                // Applications use the desktop launcher, not `xdg-open`.
                status: if !presentation.providers.applications {
                    "Disabled".to_string()
                } else {
                    format!("{} applications scanned", presentation.applications)
                },
            },
            CommandProviderViewModel {
                provider: CommandProvider::Files,
                label: CommandProvider::Files.label(),
                enabled: presentation.providers.files,
                status: if presentation.file_roots == 0 {
                    "No roots configured; nothing is indexed".to_string()
                } else {
                    format!("{} configured roots", presentation.file_roots)
                },
            },
            CommandProviderViewModel {
                provider: CommandProvider::Scripts,
                label: CommandProvider::Scripts.label(),
                enabled: presentation.providers.scripts,
                status: format!("{} script definitions", presentation.scripts),
            },
            CommandProviderViewModel {
                provider: CommandProvider::Emoji,
                label: CommandProvider::Emoji.label(),
                enabled: presentation.providers.emoji,
                status: "Built in".to_string(),
            },
        ];

        let results = presentation
            .results
            .iter()
            .map(|result| CommandResultViewModel {
                id: result.item.id.clone(),
                source: result.item.source.label(),
                title: result.item.title.clone(),
                detail: result.item.subtitle.clone(),
                matched_on: result.matched_on,
                pinned: result.pinned,
                uses: result.usage,
                action_label: command_action_label(&result.item.action),
            })
            .collect::<Vec<_>>();

        let ranking_entries = presentation
            .ranking
            .entries()
            .into_iter()
            .map(|(id, uses)| CommandRankingViewModel {
                pinned: presentation.ranking.is_pinned(&id),
                id,
                uses,
            })
            .collect::<Vec<_>>();

        Self {
            running: presentation.running,
            status: command_bar_status(&presentation, providers.as_slice()),
            query: presentation.query.to_owned(),
            max_results: presentation.max_results,
            providers,
            results,
            ranking_entries,
            pinned: presentation.ranking.pinned_entries(),
            warnings: presentation
                .warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
        }
    }
}

fn command_action_label(action: &kestrel_services::command_bar::CommandAction) -> &'static str {
    use kestrel_services::command_bar::CommandAction;
    match action {
        CommandAction::Kestrel(_) => "Run",
        CommandAction::InsertText(_) => "Copy",
        CommandAction::RunScript { .. } => "Run script",
        CommandAction::OpenApplication { .. } => "Launch",
        CommandAction::OpenFile(_) => "Open file",
        CommandAction::OpenUrl(_) => "Open link",
    }
}

fn command_bar_status(
    presentation: &CommandBarPresentation<'_>,
    providers: &[CommandProviderViewModel],
) -> String {
    if !presentation.running {
        return "The command bar is not running. Enable commands.bar in the Feature Hub."
            .to_string();
    }
    let enabled = providers.iter().filter(|provider| provider.enabled).count();
    let mut status = format!(
        "{} of {} providers enabled · at most {} results",
        enabled,
        providers.len(),
        presentation.max_results
    );
    if presentation.launcher.is_none() {
        status.push_str(" · links and files cannot be opened (no xdg-open)");
    }
    if !presentation.ranking.entries().is_empty() {
        status.push_str(&format!(
            " · {} learned identifiers, {} pinned",
            presentation.ranking.entries().len(),
            presentation.ranking.pinned_entries().len()
        ));
    }
    if let Some(warning) = presentation.warnings.first() {
        status.push_str(&format!(" · {}", warning.reason));
    }
    status
}

/// State for the main window.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationViewModel {
    pub features: Vec<FeatureViewModel>,
    pub quick_toggles: Vec<QuickToggleViewModel>,
    pub audio: AudioViewModel,
    pub microphone: MicrophoneViewModel,
    pub speed_test: SpeedTestViewModel,
    pub clipboard: ClipboardViewModel,
    pub snippets: SnippetsViewModel,
    pub command_bar: CommandBarViewModel,
    pub monitor: MonitorViewModel,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub appearance: AppearancePreference,
    pub autostart: bool,
    pub panel_sections: Vec<PanelSectionViewModel>,
    pub can_undo: bool,
}

impl ApplicationViewModel {
    // Separate borrowed projections keep the service fields direct.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<'a>(
        registrations: impl Iterator<Item = &'a ServiceRegistration>,
        quick_toggles: impl Iterator<Item = &'a QuickToggleSnapshot>,
        audio: AudioPresentation<'a>,
        microphone: MicrophonePresentation<'a>,
        speed_test: SpeedTestPresentation,
        clipboard: ClipboardPresentation<'a>,
        snippets: SnippetsPresentation<'a>,
        command_bar: CommandBarPresentation<'a>,
        monitor: MonitorPresentation<'a>,
        warnings: &[ConfigurationWarning],
        configuration: &ApplicationConfiguration,
        can_undo: bool,
    ) -> Self {
        let registrations = registrations.collect::<Vec<_>>();
        Self {
            features: registrations
                .iter()
                .map(|registration| {
                    FeatureViewModel::from_registration(registration, configuration)
                })
                .collect(),
            quick_toggles: quick_toggles
                .map(|snapshot| {
                    let registration = registrations
                        .iter()
                        .find(|registration| registration.feature.id == snapshot.id.feature_id())
                        .copied();
                    QuickToggleViewModel::from_snapshot(snapshot, registration)
                })
                .collect(),
            audio: AudioViewModel::from_presentation(&configuration.audio, audio),
            microphone: MicrophoneViewModel::from_presentation(microphone),
            speed_test: SpeedTestViewModel::from_presentation(speed_test),
            clipboard: ClipboardViewModel::from_presentation(&configuration.clipboard, clipboard),
            snippets: SnippetsViewModel::from_presentation(snippets),
            command_bar: CommandBarViewModel::from_presentation(command_bar),
            monitor: MonitorViewModel::from_configuration(&configuration.monitoring, monitor),
            warnings: warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            appearance: configuration.ui.appearance,
            autostart: configuration.startup.autostart,
            panel_sections: configuration
                .ui
                .panel_sections
                .iter()
                .map(PanelSectionViewModel::from)
                .collect(),
            can_undo,
        }
    }
}

/// Panel placement and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSectionViewModel {
    pub section: PanelSection,
    pub visible: bool,
}

impl From<&kestrel_core::PanelSectionConfiguration> for PanelSectionViewModel {
    fn from(configuration: &kestrel_core::PanelSectionConfiguration) -> Self {
        Self {
            section: configuration.section,
            visible: configuration.visible,
        }
    }
}

/// Feature-hub resource costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCostViewModel {
    pub idle: CostLevel,
    pub interaction: CostLevel,
    pub polling: CostLevel,
}

impl From<ResourceCost> for ResourceCostViewModel {
    fn from(cost: ResourceCost) -> Self {
        Self {
            idle: cost.idle,
            interaction: cost.interaction,
            polling: cost.polling,
        }
    }
}

impl ResourceCostViewModel {
    pub fn summary(self) -> String {
        format!(
            "Idle: {}; interaction: {}; polling: {}",
            cost_label(self.idle),
            cost_label(self.interaction),
            cost_label(self.polling)
        )
    }
}

fn cost_label(cost: CostLevel) -> &'static str {
    match cost {
        CostLevel::None => "none",
        CostLevel::Low => "low",
        CostLevel::Moderate => "moderate",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleViewModel {
    pub id: QuickToggleId,
    pub feature_id: &'static str,
    pub label: &'static str,
    pub requirement: &'static str,
    pub available: bool,
    pub detail: String,
    pub source: &'static str,
    pub control: Option<QuickToggleControlViewModel>,
    pub error: Option<String>,
}

impl QuickToggleViewModel {
    fn from_snapshot(
        snapshot: &QuickToggleSnapshot,
        registration: Option<&ServiceRegistration>,
    ) -> Self {
        let detail = snapshot
            .observation
            .as_ref()
            .map(|observation| observation.detail.clone())
            .or_else(|| snapshot.error.as_ref().map(|error| error.message.clone()))
            .or_else(|| registration.map(|registration| registration.capability.summary.clone()))
            .unwrap_or_else(|| "No capability report is available.".to_owned());
        Self {
            id: snapshot.id,
            feature_id: snapshot.id.feature_id(),
            label: snapshot.label,
            requirement: snapshot.requirement,
            available: registration.is_some_and(|registration| registration.running)
                && snapshot.observation.is_some(),
            detail,
            source: match snapshot.source {
                ToggleStateSource::KestrelOwned => "Kestrel-owned state",
                ToggleStateSource::ProviderObserved => "Provider-observed state",
                ToggleStateSource::ChangedElsewhere => "Changed outside Kestrel",
            },
            control: snapshot
                .observation
                .as_ref()
                .map(|observation| QuickToggleControlViewModel::from(&observation.control)),
            error: snapshot
                .observation
                .as_ref()
                .and(snapshot.error.as_ref())
                .map(|error| error.message.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickToggleControlViewModel {
    Switch {
        enabled: bool,
        confirmation: Option<ConfirmationViewModel>,
    },
    Level {
        percentage: u8,
    },
    Actions(Vec<QuickToggleActionViewModel>),
}

impl From<&QuickToggleControl> for QuickToggleControlViewModel {
    fn from(control: &QuickToggleControl) -> Self {
        match control {
            QuickToggleControl::Switch {
                enabled,
                confirmation,
            } => Self::Switch {
                enabled: *enabled,
                confirmation: confirmation.as_ref().map(ConfirmationViewModel::from),
            },
            QuickToggleControl::Level { percentage } => Self::Level {
                percentage: *percentage,
            },
            QuickToggleControl::Actions(actions) => Self::Actions(
                actions
                    .iter()
                    .map(QuickToggleActionViewModel::from)
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationViewModel {
    pub scope: String,
    pub token: String,
}

impl From<&MutationConfirmation> for ConfirmationViewModel {
    fn from(confirmation: &MutationConfirmation) -> Self {
        Self {
            scope: confirmation.scope.clone(),
            token: confirmation.token.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickToggleActionViewModel {
    pub target: Option<String>,
    pub label: String,
    pub confirmation: ConfirmationViewModel,
}

impl From<&ToggleAction> for QuickToggleActionViewModel {
    fn from(action: &ToggleAction) -> Self {
        Self {
            target: action.target.clone(),
            label: action.label.clone(),
            confirmation: ConfirmationViewModel::from(&action.confirmation),
        }
    }
}
/// An action improving a capability state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemediationViewModel {
    pub title: &'static str,
    pub message: String,
}

fn remediation_title(status: &CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::NeedsPermission { .. } => "Permission required",
        CapabilityStatus::MissingDependency { .. } => "Dependency required",
        CapabilityStatus::Unsupported { .. } => "Alternative action",
        CapabilityStatus::Limited { .. } => "Improve support",
        CapabilityStatus::Supported => "Suggested action",
    }
}

/// State for one registered feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureViewModel {
    pub id: String,
    pub label: String,
    pub registered: bool,
    pub enabled: bool,
    pub available: bool,
    pub running: bool,
    pub configurable: bool,
    pub cost: ResourceCostViewModel,
    pub lifecycle: FeatureLifecycleViewModel,
    pub capability: CapabilityViewModel,
}

impl FeatureViewModel {
    fn from_registration(
        registration: &ServiceRegistration,
        _configuration: &ApplicationConfiguration,
    ) -> Self {
        Self {
            id: registration.feature.id.to_string(),
            label: registration.feature.label.to_string(),
            registered: true,
            enabled: registration.enabled,
            available: registration.available,
            running: registration.running,
            configurable: registration.feature.configurable,
            cost: registration.feature.cost.into(),
            lifecycle: registration.lifecycle().into(),
            capability: CapabilityViewModel {
                status: CapabilityStatusViewModel::from(&registration.capability.status),
                summary: registration.capability.summary.clone(),
                selected_backend: registration.capability.selected_backend.clone(),
                remediation: registration.capability.remediation.as_ref().map(|message| {
                    RemediationViewModel {
                        title: remediation_title(&registration.capability.status),
                        message: message.clone(),
                    }
                }),
            },
        }
    }
}

impl From<&ServiceRegistration> for FeatureViewModel {
    fn from(registration: &ServiceRegistration) -> Self {
        Self::from_registration(registration, &ApplicationConfiguration::default())
    }
}

/// User-facing lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureLifecycleViewModel {
    Registered,
    Available,
    Unavailable,
    Running,
}

impl FeatureLifecycleViewModel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Registered => "Disabled",
            Self::Available => "Ready",
            Self::Unavailable => "Unavailable",
            Self::Running => "Running",
        }
    }
}

impl From<ServiceLifecycle> for FeatureLifecycleViewModel {
    fn from(lifecycle: ServiceLifecycle) -> Self {
        match lifecycle {
            ServiceLifecycle::Registered => Self::Registered,
            ServiceLifecycle::Available => Self::Available,
            ServiceLifecycle::Unavailable => Self::Unavailable,
            ServiceLifecycle::Running => Self::Running,
        }
    }
}

/// State for one capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityViewModel {
    pub status: CapabilityStatusViewModel,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub remediation: Option<RemediationViewModel>,
}

/// Capability status and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatusViewModel {
    pub kind: CapabilityKindViewModel,
    pub label: &'static str,
    pub detail: Option<String>,
}
/// Semantic capability state for presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKindViewModel {
    Supported,
    Limited,
    NeedsPermission,
    MissingDependency,
    Unsupported,
}

impl From<&CapabilityStatus> for CapabilityStatusViewModel {
    fn from(status: &CapabilityStatus) -> Self {
        match status {
            CapabilityStatus::Supported => Self {
                kind: CapabilityKindViewModel::Supported,
                label: "Supported",
                detail: None,
            },
            CapabilityStatus::Limited { reason } => Self {
                kind: CapabilityKindViewModel::Limited,
                label: "Limited",
                detail: Some(reason.clone()),
            },
            CapabilityStatus::NeedsPermission { permission } => Self {
                kind: CapabilityKindViewModel::NeedsPermission,
                label: "Needs permission",
                detail: Some(permission_label(*permission).to_string()),
            },
            CapabilityStatus::MissingDependency { name } => Self {
                kind: CapabilityKindViewModel::MissingDependency,
                label: "Missing dependency",
                detail: Some(name.clone()),
            },
            CapabilityStatus::Unsupported { reason } => Self {
                kind: CapabilityKindViewModel::Unsupported,
                label: "Unsupported",
                detail: Some(reason.clone()),
            },
        }
    }
}

fn permission_label(permission: Permission) -> &'static str {
    match permission {
        Permission::ScreenCapture => "Screen capture",
        Permission::GlobalShortcut => "Global shortcut",
        Permission::InputInjection => "Input injection",
        Permission::HardwareControl => "Hardware control",
        Permission::SessionControl => "Session control",
        Permission::DesktopSettings => "Desktop settings",
        Permission::NetworkControl => "Network control",
        Permission::FileDeletion => "File deletion",
        Permission::RemovableMedia => "Removable media",
        Permission::Camera => "Camera",
        Permission::Notifications => "Notifications",
    }
}

/// One isolated configuration warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationWarningViewModel {
    pub feature_id: String,
    pub message: String,
}

impl From<&ConfigurationWarning> for ConfigurationWarningViewModel {
    fn from(warning: &ConfigurationWarning) -> Self {
        Self {
            feature_id: warning.feature_id.clone(),
            message: warning.reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ApplicationViewModel, AudioPresentation, CapabilityKindViewModel,
        CapabilityStatusViewModel, ClipboardPresentation, ClipboardViewModel,
        CommandBarPresentation, CommandBarViewModel, CommandProvider, FeatureLifecycleViewModel,
        FeatureViewModel, MicrophonePresentation, MicrophoneViewModel, MonitorPresentation,
        MonitorViewModel, SnippetsPresentation, SnippetsViewModel, SpeedTestPresentation,
        SpeedTestViewModel,
    };
    use kestrel_core::{
        AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec,
        MonitorConfiguration, MonitorReadout, Permission,
    };
    use kestrel_platform::audio::{AudioServer, InputDevice, OutputDevice, PlaybackStream};
    use kestrel_platform::clipboard::{ClipboardEntryKind, ClipboardKindSupport};
    use kestrel_platform::quick_toggles::QuickToggleId;
    use kestrel_platform::speed_test::{
        CLOUDFLARE_PROVIDER, SpeedTestError, SpeedTestErrorKind, SpeedTestMeasurement,
        SpeedTestPhase, SpeedTestPlan, SpeedTestProgress,
    };
    use kestrel_services::{
        ServiceRegistration,
        audio::{AudioAvailability, AudioPolicy, AudioReconcileOutcome, AudioSnapshot},
        clipboard::{ClipboardItemMetadata, ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
        microphone::{MicrophoneAvailability, MicrophoneMuteState, MicrophoneSnapshot},
        quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
        snippets::{SnippetLibrary, SnippetPolicy},
        speed_test::{SpeedTestSnapshot, SpeedTestStatus},
    };

    use crate::ConfigurationWarning;

    #[test]
    fn extracts_disabled_supported_feature_without_runtime_ownership() {
        let feature = FeatureSpec::new("test.feature", "Test feature", CapabilityStatus::Supported);
        let report = CapabilityReport::new(
            "test.feature",
            CapabilityStatus::Supported,
            "The feature is ready.",
        )
        .with_selected_backend("Test backend");
        let registration =
            ServiceRegistration::new(feature, report, false).expect("matching feature IDs");

        let view_model = FeatureViewModel::from(&registration);

        assert_eq!(view_model.id, "test.feature");
        assert!(!view_model.enabled);
        assert!(view_model.registered);
        assert!(view_model.configurable);
        assert_eq!(view_model.lifecycle, FeatureLifecycleViewModel::Registered);
        assert_eq!(view_model.lifecycle.label(), "Disabled");
        assert_eq!(
            view_model.capability.status.kind,
            CapabilityKindViewModel::Supported
        );
        assert_eq!(view_model.capability.status.label, "Supported");
        assert_eq!(
            view_model.capability.selected_backend.as_deref(),
            Some("Test backend")
        );
    }

    #[test]
    fn preserves_unavailable_capability_detail_and_remediation() {
        let feature = FeatureSpec::new(
            "test.shortcuts",
            "Shortcuts",
            CapabilityStatus::NeedsPermission {
                permission: Permission::GlobalShortcut,
            },
        );
        let report = CapabilityReport::new(
            "test.shortcuts",
            CapabilityStatus::NeedsPermission {
                permission: Permission::GlobalShortcut,
            },
            "Shortcut permission is required.",
        )
        .with_remediation("Grant access or use the normal window.");
        let registration =
            ServiceRegistration::new(feature, report, true).expect("matching feature IDs");

        let view_model = FeatureViewModel::from(&registration);

        assert_eq!(view_model.lifecycle, FeatureLifecycleViewModel::Unavailable);
        assert_eq!(
            view_model.capability.status.kind,
            CapabilityKindViewModel::NeedsPermission
        );
        assert_eq!(view_model.capability.status.label, "Needs permission");
        assert_eq!(
            view_model.capability.status.detail.as_deref(),
            Some("Global shortcut")
        );
        assert_eq!(
            view_model
                .capability
                .remediation
                .as_ref()
                .map(|value| (value.title, value.message.as_str())),
            Some((
                "Permission required",
                "Grant access or use the normal window."
            ))
        );
    }

    #[test]
    fn maps_limited_and_missing_dependency_details() {
        assert_eq!(
            CapabilityStatusViewModel::from(&CapabilityStatus::Limited {
                reason: "Only one provider is available.".to_string(),
            }),
            CapabilityStatusViewModel {
                kind: CapabilityKindViewModel::Limited,
                label: "Limited",
                detail: Some("Only one provider is available.".to_string()),
            }
        );
        assert_eq!(
            CapabilityStatusViewModel::from(&CapabilityStatus::MissingDependency {
                name: "example-service".to_string(),
            }),
            CapabilityStatusViewModel {
                kind: CapabilityKindViewModel::MissingDependency,
                label: "Missing dependency",
                detail: Some("example-service".to_string()),
            }
        );
    }

    #[test]
    fn unavailable_quick_toggle_uses_its_capability_summary() {
        let id = QuickToggleId::Brightness;
        let status = CapabilityStatus::Unsupported {
            reason: "No internal backlight was found.".to_owned(),
        };
        let report = CapabilityReport::new(
            id.feature_id(),
            status.clone(),
            "No backlight adapter exists.",
        );
        let feature = FeatureSpec::new(id.feature_id(), id.label(), status);
        let registration =
            ServiceRegistration::new(feature, report, true).expect("matching feature IDs");
        let snapshot = QuickToggleSnapshot {
            id,
            label: id.label(),
            requirement: id.requirement(),
            observation: None,
            source: ToggleStateSource::ProviderObserved,
            error: None,
        };

        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::once(&registration),
            std::iter::once(&snapshot),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );
        assert!(!view_model.quick_toggles[0].available);
        assert_eq!(
            view_model.quick_toggles[0].detail,
            "No backlight adapter exists."
        );
    }

    #[test]
    fn copies_configuration_warnings_into_owned_presentation_state() {
        let warnings = vec![ConfigurationWarning {
            feature_id: "audio.mixer".to_string(),
            reason: "Invalid volume preference.".to_string(),
        }];
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &warnings,
            &configuration,
            false,
        );

        assert!(view_model.features.is_empty());
        assert_eq!(view_model.warnings[0].feature_id, "audio.mixer");
        assert_eq!(view_model.warnings[0].message, "Invalid volume preference.");
    }

    #[test]
    fn exposes_presentation_preferences_and_reversible_state() {
        use kestrel_core::{AppearancePreference, PanelSection, PanelSectionConfiguration};

        let mut configuration = ApplicationConfiguration::default();
        configuration.ui.appearance = AppearancePreference::Dark;
        configuration.startup.autostart = true;
        configuration.ui.panel_sections = vec![
            PanelSectionConfiguration {
                section: PanelSection::FeatureHub,
                visible: false,
            },
            PanelSectionConfiguration {
                section: PanelSection::QuickControls,
                visible: true,
            },
        ];
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            true,
        );

        assert_eq!(view_model.appearance, AppearancePreference::Dark);
        assert!(view_model.autostart);
        assert!(view_model.can_undo);
        assert_eq!(
            view_model.panel_sections[0].section,
            PanelSection::FeatureHub
        );
        assert!(!view_model.panel_sections[0].visible);
    }
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
                policy: None,
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
    fn alert_rules_expose_the_gated_effective_state() {
        use kestrel_services::alerts::AlertPolicy;

        // Policy carries quick-toggle gates over configuration intent.
        let configuration = MonitorConfiguration::default();
        assert!(configuration.alerts.battery.enabled);
        let mut policy = AlertPolicy::from_configuration(&configuration.alerts);
        policy.set_enabled(AlertKind::Battery, false);
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: Some(&policy),
            },
        );
        let battery = monitor
            .alert_rules
            .iter()
            .find(|rule| rule.kind == AlertKind::Battery)
            .expect("battery rule is always reported");
        assert!(!battery.enabled);
        assert!(battery.summary.contains("paused by its quick toggle"));
        let cpu = monitor
            .alert_rules
            .iter()
            .find(|rule| rule.kind == AlertKind::Cpu)
            .expect("cpu rule is always reported");
        assert!(cpu.enabled);
        assert!(!cpu.summary.contains("quick toggle"));
    }

    #[test]
    fn unavailable_readout_never_borrows_another_family_reason() {
        use kestrel_platform::system_monitor::SourceIssue;
        use kestrel_services::system_monitor::SystemSnapshot;
        use std::time::Duration;

        // Exact source matching prevents `/proc/diskstats` from explaining CPU.
        let snapshot = SystemSnapshot {
            observed_at: Duration::from_secs(1),
            cpu_usage_percent: None,
            logical_cpus: None,
            memory: None,
            disk_usage: Vec::new(),
            disk_activity: Vec::new(),
            network: Vec::new(),
            temperatures: Vec::new(),
            power_supplies: Vec::new(),
            gpu: Vec::new(),
            issues: vec![SourceIssue {
                source: "/proc/diskstats".to_owned(),
                reason: "no whole-disk statistics are readable".to_owned(),
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
                policy: None,
            },
        );

        assert_eq!(monitor.readouts[0].value, None);
        assert!(!monitor.readouts[0].detail.contains("disk"));
        assert_eq!(
            monitor.readouts[1].detail,
            "no whole-disk statistics are readable"
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
            memory: None,
            disk_usage: Vec::new(),
            disk_activity: Vec::new(),
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
                policy: None,
            },
        );
        assert_eq!(monitor.readouts[0].value, None);
        assert_eq!(monitor.readouts[0].detail, "MemAvailable is missing");
    }

    #[test]
    fn active_alerts_and_rules_map_labels_units_and_threshold_summary() {
        use kestrel_services::alerts::{ActiveAlert, AlertSnapshot};
        use std::time::Duration;

        let mut configuration = MonitorConfiguration::default();
        configuration.alerts.temperature.threshold = 80.0;
        configuration.alerts.temperature.sustain_samples = 3;
        configuration.alerts.temperature.cooldown_seconds = 120;
        let monitor = MonitorViewModel::from_configuration(
            &configuration,
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: AlertSnapshot {
                    active: vec![ActiveAlert {
                        kind: AlertKind::Temperature,
                        value: 91.0,
                        threshold: 80.0,
                        subject: Some("CPU package".to_owned()),
                        raised_at: Duration::from_secs(2),
                    }],
                    delivery_failures: Vec::new(),
                },
                running: true,
                policy: None,
            },
        );
        assert_eq!(monitor.alerts[0].label, "Temperature");
        assert_eq!(monitor.alert_rules[1].unit, "°C");
        assert!(monitor.alert_rules[1].summary.contains("80°C"));
        assert!(monitor.alert_rules[1].summary.contains("3 samples"));
        assert!(monitor.alert_rules[1].summary.contains("120 s"));
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
                policy: None,
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

    fn test_speed_presentation() -> SpeedTestPresentation {
        SpeedTestPresentation {
            snapshot: SpeedTestSnapshot {
                status: SpeedTestStatus::Idle,
                last_measurement: None,
                provider: kestrel_platform::speed_test::CLOUDFLARE_PROVIDER,
                plan: kestrel_platform::speed_test::SpeedTestPlan {
                    download_bytes: 1,
                    upload_bytes: 0,
                    phase_timeout: Duration::from_secs(5),
                },
                available: false,
                generation: 0,
            },
            running: false,
        }
    }
    fn audio_output(id: u32, is_default: bool, volume_percent: u8, muted: bool) -> OutputDevice {
        OutputDevice {
            id,
            name: format!("output-{id}"),
            description: format!("Device {id}"),
            volume_percent,
            muted,
            is_default,
            card_id: Some(0),
            card_name: Some("Built-in Audio".to_string()),
            port_name: Some("analog-output".to_string()),
            port_description: Some("Headphones".to_string()),
        }
    }

    fn audio_stream(id: u32, output_id: u32, corked: bool) -> PlaybackStream {
        PlaybackStream {
            id,
            name: format!("Stream {id}"),
            application_name: Some(format!("Application {id}")),
            media_name: Some("A track".to_string()),
            media_role: Some("music".to_string()),
            output_id,
            volume_percent: Some(60),
            volume_writable: true,
            muted: false,
            corked,
        }
    }

    fn audio_snapshot(streams: Vec<PlaybackStream>, inactive: usize) -> AudioSnapshot {
        AudioSnapshot {
            availability: AudioAvailability::Ready,
            server: Some(AudioServer {
                name: Some("PulseAudio (on PipeWire)".to_string()),
                version: Some("1".to_string()),
                protocol_version: Some(35),
                default_output_name: Some("output-3".to_string()),
            }),
            outputs: vec![
                audio_output(3, true, 75, false),
                audio_output(4, false, 40, true),
            ],
            streams,
            inactive_streams: inactive,
            boost_ceiling_percent: 130,
            last_switch: None,
            last_reconcile: Some(AudioReconcileOutcome {
                lost_output_ids: vec![9],
                rehomed_streams: 1,
                volume_resets: 1,
                failures: 0,
            }),
        }
    }

    #[test]
    fn stopped_mixer_presentation_points_at_the_feature_hub() {
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );

        let audio = &view_model.audio;
        assert!(!audio.running);
        assert!(audio.status.contains("Feature Hub"));
        assert!(audio.outputs.is_empty());
        assert!(audio.output_groups.is_empty());
        assert_eq!(
            audio.policy.boost_percent,
            kestrel_core::DEFAULT_AUDIO_BOOST_PERCENT
        );
        assert_eq!(
            audio.policy.max_boost_percent,
            kestrel_core::MAX_AUDIO_BOOST_PERCENT
        );
    }

    #[test]
    fn running_mixer_presents_grouped_devices_streams_and_effective_policy() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = audio_snapshot(vec![audio_stream(7, 3, false)], 1);
        let policy = AudioPolicy {
            boost_percent: 140,
            output_switch: kestrel_core::AudioOutputSwitch::AllStreams,
            disconnect_policy: kestrel_core::AudioDisconnectPolicy::ResetVolume,
            disconnect_volume_percent: 80,
            include_inactive_streams: false,
        };
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: Some(&snapshot),
                running: true,
                policy,
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );

        let audio = &view_model.audio;
        assert!(audio.running);
        assert!(audio.status.contains("2 outputs"));
        assert!(audio.status.contains("1 playing streams"));
        assert!(audio.status.contains("1 inactive hidden"));
        assert_eq!(audio.default_output_id, Some(3));
        assert_eq!(audio.inactive_streams, 1);
        assert_eq!(audio.output_groups.len(), 1);
        assert_eq!(audio.output_groups[0].label, "Built-in Audio");
        assert_eq!(audio.output_groups[0].outputs.len(), 2);
        assert!(
            audio.output_groups[0].outputs[0]
                .detail
                .contains("Default output")
        );
        assert!(audio.output_groups[0].outputs[1].detail.contains("Muted"));
        assert_eq!(
            audio
                .streams
                .iter()
                .map(|stream| stream.id)
                .collect::<Vec<_>>(),
            vec![7],
            "inactive streams stay hidden unless the policy lists them"
        );
        assert!(audio.streams[0].detail.contains("Device 3"));
        assert!(
            audio
                .message
                .as_deref()
                .is_some_and(|message| message.contains("disappeared")),
            "device-loss repairs must be reported"
        );
        assert_eq!(audio.boost_ceiling_percent, 130);
        assert!(audio.policy.move_all_streams);
        assert!(audio.policy.reset_volume_on_disconnect);
        assert_eq!(audio.policy.disconnect_volume_percent, 80);
        assert_eq!(
            audio.policy.output_switch_label,
            kestrel_core::AudioOutputSwitch::AllStreams.label()
        );
    }

    #[test]
    fn inactive_policy_surfaces_corked_streams_in_presentation() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = audio_snapshot(vec![audio_stream(7, 3, false), audio_stream(8, 3, true)], 1);
        let policy = AudioPolicy {
            include_inactive_streams: true,
            ..AudioPolicy::default()
        };
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: Some(&snapshot),
                running: true,
                policy,
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );

        let audio = &view_model.audio;
        assert_eq!(
            audio
                .streams
                .iter()
                .map(|stream| stream.id)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        assert!(audio.streams[1].inactive);
        assert!(
            audio.status.contains("1 playing streams"),
            "the status must count playing streams even when inactive ones are listed"
        );
    }

    #[test]
    fn stream_detail_reports_a_missing_output_instead_of_stale_routing() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = audio_snapshot(vec![audio_stream(7, 99, false)], 0);
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: Some(&snapshot),
                running: true,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );

        assert!(
            view_model.audio.streams[0]
                .detail
                .contains("Output no longer available")
        );
    }

    fn clipboard_snapshot() -> ClipboardSnapshot {
        ClipboardSnapshot {
            lifecycle: kestrel_services::clipboard::ClipboardLifecycle::Running,
            provider: Some(kestrel_platform::clipboard::ClipboardProvider::WaylandDataControl),
            kind_support: ClipboardKindSupport::ALL,
            items: vec![
                ClipboardItemMetadata {
                    id: 7,
                    kind: ClipboardEntryKind::Text,
                    size_bytes: 12,
                    age: Duration::from_secs(3),
                    pinned: true,
                    image_dimensions: None,
                    file_count: None,
                },
                ClipboardItemMetadata {
                    id: 8,
                    kind: ClipboardEntryKind::Files,
                    size_bytes: 24,
                    age: Duration::from_secs(90),
                    pinned: false,
                    image_dimensions: None,
                    file_count: Some(2),
                },
            ],
            total_bytes: 36,
            pinned_items: 1,
            rejected_oversize_items: 2,
            filtered_sensitive_items: 1,
            selection_clears: 3,
            wipe_count: 0,
            last_filtered_pattern: Some("password_assignment"),
            last_error: None,
        }
    }

    fn clipboard_matches() -> Vec<ClipboardMatch> {
        vec![
            ClipboardMatch {
                id: 7,
                kind: ClipboardEntryKind::Text,
                preview: "matched text".to_string(),
            },
            ClipboardMatch {
                id: 8,
                kind: ClipboardEntryKind::Files,
                preview: "/tmp/a, /tmp/b".to_string(),
            },
        ]
    }

    fn snippet_library() -> SnippetLibrary {
        let mut library = SnippetLibrary::default();
        library
            .upsert(
                kestrel_core::Snippet::new(
                    "Address",
                    Some("Contact".to_string()),
                    Some(";addr".to_string()),
                    "Street 1 {{date}} {{clipboard}}",
                ),
                SnippetPolicy::default(),
            )
            .expect("snippet is valid");
        library
    }

    fn snippet_presentation<'a>(
        library: &'a SnippetLibrary,
        matches: &'a [kestrel_services::snippets::SnippetMatch],
    ) -> SnippetsPresentation<'a> {
        SnippetsPresentation {
            library,
            policy: SnippetPolicy::default(),
            running: true,
            provider: Some(kestrel_platform::snippets::InsertionProvider::Wtype),
            unavailable_reason: None,
            directory: Some("/tmp/example/kestrel".to_string()),
            expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
            provider_preference: kestrel_core::SnippetProviderPreference::Auto,
            search_query: "",
            matches,
            draft: None,
            warnings: &[],
        }
    }

    #[test]
    fn snippets_presentation_names_the_provider_and_previews_deterministically() {
        let library = snippet_library();
        let matches = library.search("", 10, SnippetPolicy::default());
        let snippets =
            SnippetsViewModel::from_presentation(snippet_presentation(&library, &matches));

        assert!(snippets.insertion_available);
        assert_eq!(snippets.provider, Some("wtype"));
        assert_eq!(
            snippets.transport,
            Some("Wayland virtual-keyboard protocol")
        );
        assert!(snippets.insertion_status.contains("wtype"));
        assert_eq!(snippets.total, 1);
        assert_eq!(snippets.folders, vec!["Contact"]);
        assert_eq!(snippets.items.len(), 1);
        assert_eq!(snippets.items[0].name, "Address");
        assert_eq!(snippets.items[0].trigger.as_deref(), Some(";addr"));
        assert!(
            snippets.items[0].preview.contains("«date»")
                && snippets.items[0].preview.contains("«clipboard»"),
            "previews keep variables as placeholders instead of reading live values: {}",
            snippets.items[0].preview
        );
        assert!(snippets.items[0].detail.contains("{{date}}"));
        assert!(
            !snippets.expansion_active,
            "manual timing never claims trigger expansion"
        );
    }

    #[test]
    fn snippets_presentation_disables_insertion_without_a_provider() {
        let library = snippet_library();
        let matches = library.search("", 10, SnippetPolicy::default());
        let mut presentation = snippet_presentation(&library, &matches);
        presentation.provider = None;
        presentation.unavailable_reason = Some("no verified insertion provider was found");

        let snippets = SnippetsViewModel::from_presentation(presentation);

        assert!(!snippets.insertion_available);
        assert_eq!(snippets.provider, None);
        assert_eq!(
            snippets.insertion_status,
            "no verified insertion provider was found"
        );
    }

    fn command_presentation<'a>(
        results: &'a [kestrel_services::command_bar::CommandResult],
        ranking: &'a kestrel_services::command_bar::CommandRanking,
    ) -> CommandBarPresentation<'a> {
        CommandBarPresentation {
            running: true,
            max_results: 12,
            providers: kestrel_services::command_bar::EnabledProviders {
                applications: true,
                files: false,
                scripts: true,
                emoji: true,
                snippets: true,
            },
            launcher: Some("xdg-open"),
            applications: 42,
            file_roots: 0,
            scripts: 2,
            query: "2+2",
            results,
            ranking,
            warnings: &[],
        }
    }

    #[test]
    fn command_bar_reports_providers_and_results_without_hiding_portable_ones() {
        let index = kestrel_services::command_bar::CommandIndex::new(Vec::new(), &[]);
        let ranking = kestrel_services::command_bar::CommandRanking::default();
        let now = kestrel_platform::snippets::LocalTime {
            year: 2026,
            month: 10,
            day: 2,
            hour: 9,
            minute: 5,
            second: 7,
            utc_offset_seconds: 7200,
        };
        let results = index.search(kestrel_services::command_bar::SearchInput {
            query: "2+2",
            max_results: 5,
            providers: kestrel_services::command_bar::EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&now),
            ranking: &ranking,
            configured_scripts: &[],
        });
        let command_bar =
            CommandBarViewModel::from_presentation(command_presentation(&results, &ranking));

        assert!(command_bar.running);
        assert_eq!(command_bar.results.len(), 1);
        assert_eq!(command_bar.results[0].source, "Math");
        assert_eq!(command_bar.results[0].title, "2+2 = 4");
        assert_eq!(command_bar.results[0].action_label, "Copy");
        assert_eq!(command_bar.providers.len(), 4);
        assert!(
            command_bar
                .providers
                .iter()
                .any(|provider| provider.provider == CommandProvider::Files && !provider.enabled),
            "a disabled provider is reported, not hidden"
        );
        assert!(
            command_bar.status.contains("launcher") || command_bar.status.contains("providers")
        );
    }

    #[test]
    fn command_bar_exposes_the_learned_ranking_and_handles_a_missing_launcher() {
        let mut ranking = kestrel_services::command_bar::CommandRanking::default();
        ranking.record_use("kestrel:refresh");
        ranking.record_use("kestrel:refresh");
        ranking.set_pinned("snippet:Address", true);
        let mut presentation = command_presentation(&[], &ranking);
        presentation.launcher = None;

        let command_bar = CommandBarViewModel::from_presentation(presentation);

        assert_eq!(command_bar.ranking_entries.len(), 1);
        assert_eq!(command_bar.ranking_entries[0].id, "kestrel:refresh");
        assert_eq!(command_bar.ranking_entries[0].uses, 2);
        assert_eq!(command_bar.pinned, vec!["snippet:Address".to_string()]);
        assert!(
            command_bar.status.contains("no xdg-open"),
            "a missing launcher is reported while portable commands keep working: {}",
            command_bar.status
        );
        assert!(
            command_bar.providers.iter().any(|provider| {
                provider.provider == CommandProvider::Applications
                    && provider.status == "42 applications scanned"
            }),
            "applications launch through GIO, so a missing xdg-open is not reported against them"
        );
    }

    #[test]
    fn stopped_clipboard_presentation_points_at_the_feature_hub() {
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
                policy: None,
            },
            &[],
            &configuration,
            false,
        );

        let clipboard = &view_model.clipboard;
        assert!(!clipboard.running);
        assert!(clipboard.status.contains("Feature Hub"));
        assert!(clipboard.items.is_empty());
        assert_eq!(
            clipboard.policy.max_items,
            configuration.clipboard.max_items
        );
    }

    #[test]
    fn clipboard_rows_come_only_from_explicit_search_matches() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        let matches = clipboard_matches();
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "match",
                matches: &matches,
                preview: None,
            },
        );

        assert_eq!(
            clipboard
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![7, 8],
            "only the explicitly matched entries become rows"
        );
        assert_eq!(clipboard.items[0].preview, "matched text");
        assert!(clipboard.items[0].pinned);
        assert!(clipboard.items[0].text_editable);
        assert!(!clipboard.items[1].text_editable);
        assert!(clipboard.items[1].detail.contains("2 files"));
        assert_eq!(clipboard.search_query, "match");
        assert_eq!(clipboard.total_items, 2);
        assert!(clipboard.status.contains("2 of 2 retained entries shown"));
        assert!(clipboard.status.contains("1 filtered as sensitive"));
        assert!(clipboard.status.contains("3 automatic selection clears"));
        assert!(clipboard.status.contains("2 over the size bound"));
    }

    #[test]
    fn clipboard_rows_are_empty_without_an_explicit_search() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        // No query means no row content.
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "",
                matches: &[],
                preview: None,
            },
        );

        assert!(clipboard.items.is_empty(), "no content without a request");
        assert_eq!(clipboard.total_items, 2, "metadata stays visible");
        assert_eq!(clipboard.pinned_items, 1);
        assert!(
            !format!("{clipboard:?}").contains("matched text"),
            "no content leaks into the presentation without a request"
        );
    }

    #[test]
    fn clipboard_preview_exposes_bounded_text_and_rich_metadata() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        let preview = ClipboardPreview {
            id: 8,
            kind: ClipboardEntryKind::Files,
            text: "/tmp/a\n/tmp/b".to_string(),
            truncated: true,
            size_bytes: 24,
            image_dimensions: None,
            file_count: Some(2),
        };
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "",
                matches: &[],
                preview: Some(&preview),
            },
        );

        let preview = clipboard.preview.expect("the requested preview is carried");
        assert_eq!(preview.text, "/tmp/a\n/tmp/b");
        assert!(preview.truncated);
        assert!(!preview.editable, "file entries are not text-editable");
        assert_eq!(preview.file_count, Some(2));
    }

    #[test]
    fn clipboard_policy_exposes_the_control_bounds() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_items = 42;
        configuration.clipboard.clear_seconds = 30;
        configuration.clipboard.filter_sensitive = true;
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation::default(),
        );

        let policy = clipboard.policy;
        assert_eq!(policy.max_items, 42);
        assert_eq!(policy.clear_seconds, 30);
        assert!(policy.filter_sensitive);
        assert_eq!(
            policy.bounds.min_item_bytes,
            kestrel_core::MIN_CLIPBOARD_ITEM_BYTES
        );
        assert_eq!(
            policy.bounds.max_items,
            kestrel_core::MAX_CLIPBOARD_MAX_ITEMS
        );
        assert_eq!(
            policy.bounds.max_clear_seconds,
            kestrel_core::MAX_CLIPBOARD_CLEAR_SECONDS
        );
    }

    fn microphone_input(id: u32, muted: bool, is_default: bool) -> InputDevice {
        InputDevice {
            id,
            name: format!("source-{id}"),
            description: format!("Input {id}"),
            volume_percent: 100,
            muted,
            is_default,
        }
    }

    fn microphone_snapshot(
        inputs: Vec<InputDevice>,
        mute: MicrophoneMuteState,
    ) -> MicrophoneSnapshot {
        MicrophoneSnapshot {
            availability: if inputs.is_empty() {
                MicrophoneAvailability::NoInputs
            } else {
                MicrophoneAvailability::Ready
            },
            default_input_id: inputs
                .iter()
                .find(|input| input.is_default)
                .map(|input| input.id),
            inputs,
            mute,
            lost_inputs: 0,
            default_input_lost: false,
        }
    }

    #[test]
    fn microphone_mute_is_claimed_only_from_an_agreeing_backend_reading() {
        let unknown = MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: &MicrophoneSnapshot::unavailable(),
            running: true,
        });
        assert_eq!(unknown.muted, None, "no reading, no claim");
        assert!(!unknown.mixed);
        assert!(unknown.mute_label.contains("unknown"));

        let muted = microphone_snapshot(
            vec![
                microphone_input(1, true, true),
                microphone_input(2, true, false),
            ],
            MicrophoneMuteState::Muted,
        );
        let view = MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: &muted,
            running: true,
        });
        assert_eq!(view.muted, Some(true));
        assert_eq!(view.default_input_id, Some(1));
        assert_eq!(view.inputs.len(), 2);

        let mixed = microphone_snapshot(
            vec![
                microphone_input(1, true, true),
                microphone_input(2, false, false),
            ],
            MicrophoneMuteState::Mixed { muted: 1, total: 2 },
        );
        let view = MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: &mixed,
            running: true,
        });
        assert_eq!(
            view.muted, None,
            "a mixed reading is not reported as muted or live"
        );
        assert!(view.mixed);
        assert_eq!(view.mute_label, "1 of 2 inputs muted");
    }

    #[test]
    fn a_stopped_microphone_control_shows_no_inputs_or_state() {
        let snapshot = microphone_snapshot(
            vec![microphone_input(1, true, true)],
            MicrophoneMuteState::Muted,
        );

        let view = MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: &snapshot,
            running: false,
        });

        assert!(!view.running);
        assert_eq!(view.muted, None, "a stale reading is never presented");
        assert!(view.inputs.is_empty());
        assert!(view.status.contains("audio.microphone"));
    }

    #[test]
    fn a_lost_default_input_is_announced_without_a_default_claim() {
        let snapshot = MicrophoneSnapshot {
            lost_inputs: 1,
            default_input_lost: true,
            ..microphone_snapshot(
                vec![microphone_input(2, false, false)],
                MicrophoneMuteState::Live,
            )
        };

        let view = MicrophoneViewModel::from_presentation(MicrophonePresentation {
            snapshot: &snapshot,
            running: true,
        });

        assert_eq!(view.default_input_id, None);
        assert!(
            view.message
                .as_deref()
                .is_some_and(|message| message.contains("default input disconnected"))
        );
    }

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
