use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, CapabilityStatus, CostLevel,
    MonitorConfiguration, MonitorReadout, PanelSection, Permission, ResourceCost,
    SnippetExpansionTiming, SnippetProviderPreference,
};
use kestrel_platform::camera::CameraInfo;
use kestrel_platform::capture::{CaptureErrorKind, CaptureMode, CaptureProvider};
use kestrel_platform::global_shortcuts::{
    BindingState, ShortcutErrorKind, ShortcutPhase, ShortcutStatus,
};
use kestrel_platform::processes::{
    Ineligible, Listener, ListenerProtocol, NO_PIDFD_EXPLANATION, ProcessIdentity, ProcfsProcesses,
    RestartTarget,
};
use kestrel_platform::quick_toggles::{
    MutationConfirmation, QuickToggleControl, QuickToggleId, ToggleAction,
};
use kestrel_platform::recording::{AudioTrack, RecordingSource};
use kestrel_platform::screen_tools::ScreenTool;
use kestrel_platform::snippets::InsertionProvider;
use kestrel_platform::speed_test::SpeedTestPhase;
use kestrel_services::{
    ServiceLifecycle, ServiceRegistration,
    alerts::{ActiveAlert, AlertPolicy, AlertSnapshot},
    audio::{AudioAvailability, AudioPolicy, AudioSnapshot},
    camera::{CameraPhase, CameraSnapshot},
    capture::{CaptureId, CapturePhase, CaptureSnapshot},
    clipboard::{ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
    microphone::{MicrophoneAvailability, MicrophoneMuteState, MicrophoneSnapshot},
    processes::{ListenerSnapshot, ProcessRow, ProcessSnapshot, search as search_processes},
    quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    recording::{ExportPhase, RecordingClock, RecordingPhase, RecordingSnapshot},
    scratchpad::ScratchpadSnapshot,
    screen_tools::{QrPayload, ScreenToolResult, ScreenToolsPhase, ScreenToolsSnapshot},
    snippets::{SnippetLibrary, SnippetMatch, SnippetPolicy},
    speed_test::{SpeedTestSnapshot, SpeedTestStatus},
    system_monitor::{HistorySummary, SystemSnapshot},
};

use crate::{ConfigurationWarning, ProcessActionKind, command_line::CommandLineAction};

/// Inputs for the monitor view.
pub(crate) struct ScratchpadPresentation {
    pub warning: Option<String>,
    pub running: bool,
    pub snapshot: Option<ScratchpadSnapshot>,
    pub limits: kestrel_core::ScratchpadConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadTabViewModel {
    pub id: u64,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadExpiredViewModel {
    pub id: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadViewModel {
    pub running: bool,
    pub status: String,
    pub tabs: Vec<ScratchpadTabViewModel>,
    pub active_id: Option<u64>,
    pub active_title: String,
    pub active_text: String,
    pub preview_markup: String,
    pub expired: Vec<ScratchpadExpiredViewModel>,
    pub warning: Option<String>,
    pub limits: kestrel_core::ScratchpadConfiguration,
}

impl ScratchpadViewModel {
    pub(crate) fn from_presentation(presentation: ScratchpadPresentation) -> Self {
        let presentation_warning = presentation.warning.clone();
        let (tabs, active_id, active_title, active_text, expired, warning) =
            if let Some(snapshot) = presentation.snapshot {
                let tabs = snapshot
                    .notes
                    .iter()
                    .map(|note| ScratchpadTabViewModel {
                        id: note.id.get(),
                        title: note.title.clone(),
                        active: Some(note.id) == snapshot.active,
                    })
                    .collect();
                let active = snapshot
                    .active
                    .and_then(|id| snapshot.notes.iter().find(|note| note.id == id));
                let expired = snapshot
                    .expired
                    .iter()
                    .filter_map(|id| snapshot.notes.iter().find(|note| note.id == *id))
                    .map(|note| ScratchpadExpiredViewModel {
                        id: note.id.get(),
                        title: note.title.clone(),
                    })
                    .collect();
                (
                    tabs,
                    snapshot.active.map(|id| id.get()),
                    active.map(|note| note.title.clone()).unwrap_or_default(),
                    active.map(|note| note.text.clone()).unwrap_or_default(),
                    expired,
                    snapshot.warning.or(presentation_warning.clone()),
                )
            } else {
                (
                    Vec::new(),
                    None,
                    String::new(),
                    String::new(),
                    Vec::new(),
                    presentation_warning,
                )
            };
        let preview_markup = if presentation.running {
            kestrel_services::scratchpad::render_markdown(&active_text)
        } else {
            String::new()
        };
        let status = if !presentation.running {
            "Scratchpad is off; enable notes.scratchpad in the Feature Hub.".to_owned()
        } else if tabs.is_empty() {
            "No notes yet. Create a tab to begin.".to_owned()
        } else {
            format!(
                "{} note{} available.",
                tabs.len(),
                if tabs.len() == 1 { "" } else { "s" }
            )
        };
        Self {
            running: presentation.running,
            status,
            tabs,
            active_id,
            active_title,
            active_text,
            preview_markup,
            expired,
            warning,
            limits: presentation.limits,
        }
    }
}
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
    pub output_priority: DevicePriorityViewModel,
}

/// A device in a priority list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityDeviceViewModel {
    /// Server device name, the stored identity.
    pub name: String,
    /// Description when connected, else the name.
    pub label: String,
    pub connected: bool,
}

/// Ranked devices first, then connected devices not yet ranked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DevicePriorityViewModel {
    pub follow: bool,
    pub ranked: Vec<PriorityDeviceViewModel>,
    pub unranked: Vec<PriorityDeviceViewModel>,
    /// The ranking is at its size limit.
    pub full: bool,
}

impl DevicePriorityViewModel {
    fn new<'a>(
        ranked: &[String],
        follow: bool,
        connected: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> Self {
        let label = |name: &str| {
            connected
                .clone()
                .find(|(device, _)| *device == name)
                .map(|(_, description)| description.to_owned())
        };
        Self {
            follow,
            ranked: ranked
                .iter()
                .map(|name| PriorityDeviceViewModel {
                    name: name.clone(),
                    connected: label(name).is_some(),
                    label: label(name).unwrap_or_else(|| name.clone()),
                })
                .collect(),
            unranked: connected
                .filter(|(name, _)| !ranked.iter().any(|ranked| ranked == name))
                .map(|(name, description)| PriorityDeviceViewModel {
                    name: name.to_owned(),
                    label: description.to_owned(),
                    connected: true,
                })
                .collect(),
            full: ranked.len() >= kestrel_core::MAX_AUDIO_PRIORITY_DEVICES,
        }
    }
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
    /// Stable application identity, when the stream reports one.
    pub application_id: Option<String>,
    /// Position among pinned applications and how many are pinned.
    pub pinned: Option<(usize, usize)>,
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
        let output_priority = DevicePriorityViewModel::new(
            &configuration.output_priority,
            configuration.follow_output_priority,
            presentation
                .snapshot
                .filter(|_| presentation.running)
                .map(|snapshot| snapshot.outputs.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|output| (output.name.as_str(), output.description.as_str())),
        );
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
                output_priority,
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
        let favorites = &configuration.mixer_favorites;
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
                application_id: stream.application_id.clone(),
                pinned: stream
                    .application_id
                    .as_ref()
                    .and_then(|id| favorites.iter().position(|favorite| favorite == id))
                    .map(|index| (index, favorites.len())),
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
            output_priority,
        }
    }
}

pub(crate) struct MicrophonePresentation<'a> {
    pub snapshot: &'a MicrophoneSnapshot,
    pub running: bool,
}

/// An application recording from an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureStreamViewModel {
    pub id: u32,
    pub title: String,
    pub detail: String,
    pub volume_percent: Option<u8>,
    pub volume_writable: bool,
    pub muted: bool,
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
    /// The default input's level, as the server reports it.
    pub default_input_volume: Option<u8>,
    pub capture_streams: Vec<CaptureStreamViewModel>,
    pub input_priority: DevicePriorityViewModel,
    /// Device-loss or priority notice from the last refresh.
    pub message: Option<String>,
}

impl MicrophoneViewModel {
    pub(crate) fn from_presentation(
        configuration: &kestrel_core::AudioConfiguration,
        presentation: MicrophonePresentation<'_>,
    ) -> Self {
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
                default_input_volume: None,
                capture_streams: Vec::new(),
                input_priority: DevicePriorityViewModel::new(
                    &configuration.input_priority,
                    configuration.follow_input_priority,
                    std::iter::empty(),
                ),
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
            snapshot
                .priority_switch
                .as_ref()
                .map(|input| format!("Switched to {input}, the highest-ranked input."))
        };
        let input_label = |input_id: u32| {
            snapshot
                .inputs
                .iter()
                .find(|input| input.id == input_id)
                .map(|input| input.description.clone())
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
            default_input_volume: snapshot
                .inputs
                .iter()
                .find(|input| input.is_default)
                .map(|input| input.volume_percent),
            capture_streams: snapshot
                .capture_streams
                .iter()
                .map(|stream| CaptureStreamViewModel {
                    id: stream.id,
                    title: stream
                        .application_name
                        .clone()
                        .unwrap_or_else(|| format!("Stream {}", stream.id)),
                    detail: if stream.from_monitor {
                        "Recording system audio".to_owned()
                    } else {
                        match input_label(stream.input_id) {
                            Some(input) => format!("Recording from {input}"),
                            None => "Recording".to_owned(),
                        }
                    },
                    volume_percent: stream.volume_percent,
                    volume_writable: stream.volume_writable,
                    muted: stream.muted,
                })
                .collect(),
            input_priority: DevicePriorityViewModel::new(
                &configuration.input_priority,
                configuration.follow_input_priority,
                snapshot
                    .inputs
                    .iter()
                    .map(|input| (input.name.as_str(), input.description.as_str())),
            ),
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

pub(crate) struct ShortcutsPresentation<'a> {
    pub status: ShortcutStatus,
    pub running: bool,
    pub configured: &'a [kestrel_core::ShortcutBinding],
    /// Configured commands that are unknown to this build.
    pub skipped: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRowViewModel {
    pub command: String,
    pub description: String,
    pub trigger: String,
    pub state: String,
    /// The shortcut is registered and will fire.
    pub active: bool,
}

/// Global shortcuts: one row per configured binding, in configuration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutsViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ShortcutRowViewModel>,
    pub notice: Option<String>,
}

impl ShortcutsViewModel {
    pub(crate) fn from_presentation(presentation: ShortcutsPresentation<'_>) -> Self {
        let status = presentation.status;
        let provider = status
            .provider
            .map_or("desktop shortcuts", |provider| provider.label());
        let summary = if !presentation.running {
            "Global shortcuts are off. Enable global.shortcuts in the Feature Hub, or bind \
             `kestrel --command <id>` in your desktop's shortcut settings."
                .to_owned()
        } else {
            match &status.phase {
                ShortcutPhase::Idle => "Waiting to register shortcuts.".to_owned(),
                ShortcutPhase::Starting => format!("Registering through {provider}…"),
                ShortcutPhase::Active => format!("Registered through {provider}."),
                ShortcutPhase::Failed(error) => {
                    let remedy = match error.kind {
                        ShortcutErrorKind::Denied | ShortcutErrorKind::Cancelled => {
                            "Turn global.shortcuts off and on to ask again."
                        }
                        ShortcutErrorKind::Unavailable => {
                            "Bind `kestrel --command <id>` in your desktop's shortcut settings."
                        }
                        ShortcutErrorKind::Protocol | ShortcutErrorKind::WorkerUnavailable => {
                            "Turn global.shortcuts off and on to retry."
                        }
                    };
                    format!("Registration failed: {error}. {remedy}")
                }
                ShortcutPhase::Stopped => "Shortcuts are released.".to_owned(),
            }
        };
        // A released session keeps its last binding list; report none of it.
        let live = presentation.running && status.phase != ShortcutPhase::Stopped;
        let rows = presentation
            .configured
            .iter()
            .map(|binding| {
                let reported = live
                    .then(|| {
                        status
                            .bindings
                            .iter()
                            .find(|item| item.id == binding.command)
                    })
                    .flatten();
                let (state, active, trigger) = match reported.map(|item| &item.state) {
                    Some(BindingState::Bound { trigger }) => (
                        "Active".to_owned(),
                        true,
                        trigger.clone().unwrap_or_else(|| binding.trigger.clone()),
                    ),
                    Some(BindingState::Pending) => {
                        ("Waiting".to_owned(), false, binding.trigger.clone())
                    }
                    Some(BindingState::Conflict { reason }) => (
                        format!(
                            "In use: {reason}. Choose another trigger for it in \
                             [[shortcuts.bindings]]."
                        ),
                        false,
                        binding.trigger.clone(),
                    ),
                    Some(BindingState::Rejected { reason }) => (
                        format!("Not registered: {reason}"),
                        false,
                        binding.trigger.clone(),
                    ),
                    None => ("Not registered".to_owned(), false, binding.trigger.clone()),
                };
                ShortcutRowViewModel {
                    command: binding.command.clone(),
                    description: CommandLineAction::parse(&binding.command).map_or_else(
                        |_| "Unknown command".to_owned(),
                        |action| action.description(),
                    ),
                    trigger,
                    state,
                    active,
                }
            })
            .collect();
        let notice = (!presentation.skipped.is_empty()).then(|| {
            format!(
                "Skipped unknown commands: {}. Run `kestrel --list-commands` for valid IDs.",
                presentation.skipped.join(", ")
            )
        });
        Self {
            running: presentation.running,
            status: summary,
            rows,
            notice,
        }
    }
}

/// Inputs for the process panel; `current_uid` and `own_pid` decide which
/// rows offer actions.
pub(crate) struct ProcessesPresentation {
    pub snapshot: ProcessSnapshot,
    pub running: bool,
    pub query: String,
    pub current_uid: u32,
    pub own_pid: u32,
    pub limits: kestrel_core::ProcessConfiguration,
    /// `pidfd_open` works; without it no row offers an action.
    pub signalling: bool,
}

pub(crate) struct ListenersPresentation {
    pub snapshot: ListenerSnapshot,
    pub running: bool,
    /// `pidfd_open` works; without it Stop owner is hidden.
    pub signalling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessActionViewModel {
    pub action: ProcessActionKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRowViewModel {
    pub identity: ProcessIdentity,
    pub title: String,
    pub subtitle: String,
    pub actions: Vec<ProcessActionViewModel>,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessesViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ProcessRowViewModel>,
    pub query: String,
    /// Configured cadence and row bound, shown in Settings.
    pub limits: kestrel_core::ProcessConfiguration,
    pub stale_notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerRowViewModel {
    pub title: String,
    pub owner: String,
    pub owner_identity: Option<ProcessIdentity>,
    pub port: u16,
    pub address: String,
    pub pid: Option<u32>,
    pub open_url: Option<String>,
    pub stop_disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenersViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ListenerRowViewModel>,
    /// Stop owner is offered only when process signalling works.
    pub can_stop: bool,
}

impl ProcessesViewModel {
    pub(crate) fn from_presentation(presentation: ProcessesPresentation) -> Self {
        let rows = if presentation.running {
            let limit = presentation.limits.max_rows as usize;
            search_processes(&presentation.snapshot.rows, &presentation.query, limit)
                .into_iter()
                .map(|row| {
                    let mut row = process_row(row, presentation.current_uid, presentation.own_pid);
                    if !presentation.signalling {
                        row.actions.clear();
                    }
                    row
                })
                .collect()
        } else {
            Vec::new()
        };
        let status = if !presentation.running {
            "Processes are off. Enable system.processes in the Feature Hub.".to_owned()
        } else if let Some(error) = &presentation.snapshot.error {
            format!("Process scan failed: {}", error.message)
        } else {
            format!(
                "{} process(es){}.{}",
                presentation.snapshot.total_processes,
                if presentation.snapshot.truncated {
                    "; list truncated"
                } else {
                    ""
                },
                signalling_note(presentation.signalling)
            )
        };
        let stale_notice = presentation.snapshot.stale.then(|| {
            presentation
                .snapshot
                .error
                .as_ref()
                .map(|error| {
                    format!(
                        "Showing the last process list; refresh failed: {}",
                        error.message
                    )
                })
                .unwrap_or_else(|| "Showing a stale process list; refresh failed.".to_owned())
        });
        Self {
            running: presentation.running,
            status,
            rows,
            query: presentation.query,
            limits: presentation.limits,
            stale_notice,
        }
    }
}

fn signalling_note(signalling: bool) -> String {
    if signalling {
        String::new()
    } else {
        format!(" {NO_PIDFD_EXPLANATION}")
    }
}

fn process_row(row: &ProcessRow, current_uid: u32, own_pid: u32) -> ProcessRowViewModel {
    let sample = &row.sample;
    let title = sample
        .executable
        .as_deref()
        .filter(|executable| *executable != sample.name)
        .map_or_else(|| sample.name.clone(), str::to_owned);
    let cpu = row.cpu_percent.map_or_else(
        || "CPU unavailable".to_owned(),
        |value| format!("CPU {value:.1}%"),
    );
    let owner = sample.owner.as_deref().unwrap_or("unknown user");
    let subtitle = format!(
        "PID {} · start {} · {} · {} · {}",
        sample.identity.pid,
        sample.identity.start_ticks,
        owner,
        cpu,
        format_bytes(sample.resident_bytes as f64)
    );
    let eligibility = ProcfsProcesses::eligibility(sample, current_uid, own_pid);
    let mut actions = Vec::new();
    let disabled_reason = eligibility
        .err()
        .map(Ineligible::explanation)
        .map(str::to_owned);
    if disabled_reason.is_none() {
        actions.push(ProcessActionViewModel {
            action: ProcessActionKind::Terminate,
            label: "End with SIGTERM".to_owned(),
        });
        actions.push(ProcessActionViewModel {
            action: ProcessActionKind::Kill,
            label: "Kill with SIGKILL".to_owned(),
        });
        if let Some(target) = ProcfsProcesses::restart_target(sample) {
            let label = match target {
                RestartTarget::UserUnit { unit } => format!("Restart {unit}"),
                RestartTarget::DesktopApp { desktop_id, .. } => format!("Restart {desktop_id}"),
            };
            actions.push(ProcessActionViewModel {
                action: ProcessActionKind::Restart { escalate: false },
                label,
            });
        }
    }
    ProcessRowViewModel {
        identity: sample.identity,
        title,
        subtitle,
        actions,
        disabled_reason,
    }
}

impl ListenersViewModel {
    pub(crate) fn from_presentation(presentation: ListenersPresentation) -> Self {
        let rows: Vec<ListenerRowViewModel> = if presentation.running {
            presentation
                .snapshot
                .listeners
                .iter()
                .map(listener_row)
                .collect()
        } else {
            Vec::new()
        };
        let status = if !presentation.running {
            "Listening ports are off. Enable network.listeners in the Feature Hub.".to_owned()
        } else if let Some(error) = &presentation.snapshot.error {
            format!("Listener scan failed: {}", error.message)
        } else {
            format!(
                "{} listening port(s).{}",
                rows.len(),
                signalling_note(presentation.signalling)
            )
        };
        Self {
            running: presentation.running,
            status,
            rows,
            can_stop: presentation.signalling,
        }
    }
}

fn listener_row(listener: &Listener) -> ListenerRowViewModel {
    let address = listener.address.to_string();
    let protocol = match listener.protocol {
        ListenerProtocol::Tcp => "TCP",
        ListenerProtocol::Udp => "UDP",
    };
    let socket = std::net::SocketAddr::new(listener.address, listener.port);
    let title = format!("{protocol} {socket}");
    let owner = listener
        .owner_name
        .clone()
        .unwrap_or_else(|| "Unknown owner".to_owned());
    // Loopback listeners open at their own address; wildcard ones through localhost.
    let open_url = match listener.protocol {
        ListenerProtocol::Tcp if listener.address.is_unspecified() => {
            Some(format!("http://localhost:{}/", listener.port))
        }
        ListenerProtocol::Tcp if listener.address.is_loopback() => {
            Some(format!("http://{socket}/"))
        }
        _ => None,
    };
    ListenerRowViewModel {
        title,
        owner,
        owner_identity: listener.owner,
        port: listener.port,
        address,
        pid: listener.owner.map(|identity| identity.pid),
        open_url,
        stop_disabled_reason: listener.owner.is_none().then(|| {
            "The owner belongs to another user or has exited; refresh, or stop it from \
             that user's session."
                .to_owned()
        }),
    }
}

pub(crate) struct CameraPresentation {
    pub running: bool,
    /// Why the feature is not running, when it is not.
    pub reason: Option<String>,
    pub snapshot: CameraSnapshot,
    pub cameras: Vec<CameraInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraChoiceViewModel {
    pub path: std::path::PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CameraViewModel {
    pub running: bool,
    pub status: String,
    pub cameras: Vec<CameraChoiceViewModel>,
    /// A preview is opening or showing frames; Stop is offered.
    pub active: bool,
    /// Frames are arriving.
    pub previewing: bool,
}

impl CameraViewModel {
    pub(crate) fn from_presentation(presentation: CameraPresentation) -> Self {
        let cameras = presentation
            .cameras
            .into_iter()
            .map(|camera| CameraChoiceViewModel {
                path: camera.path,
                name: camera.name,
            })
            .collect::<Vec<_>>();
        let phase = presentation.snapshot.phase;
        let status = match (&phase, presentation.running) {
            (_, false) => presentation
                .reason
                .unwrap_or_else(|| "Enable camera.preview in the Feature Hub.".to_owned()),
            (CameraPhase::Idle, true) if cameras.is_empty() => "No camera is connected.".to_owned(),
            (CameraPhase::Idle, true) => {
                "The camera stays off until you start a preview. Frames are never saved.".to_owned()
            }
            (CameraPhase::Starting { name }, true) => format!("Opening {name}…"),
            (
                CameraPhase::Previewing {
                    name,
                    width,
                    height,
                },
                true,
            ) => format!("{name} · {width} × {height}. Frames are never saved."),
            (CameraPhase::Failed { name, error }, true) => format!("{name}: {}", error.message),
        };
        Self {
            running: presentation.running,
            status,
            cameras,
            active: presentation.running
                && matches!(
                    phase,
                    CameraPhase::Starting { .. } | CameraPhase::Previewing { .. }
                ),
            previewing: presentation.running && matches!(phase, CameraPhase::Previewing { .. }),
        }
    }
}

pub(crate) struct RecordingPresentation {
    pub running: bool,
    /// Why the feature is not running, when it is not.
    pub reason: Option<String>,
    pub snapshot: RecordingSnapshot,
    pub sources: Vec<RecordingSource>,
    /// Tracks this session can record.
    pub available_tracks: Vec<AudioTrack>,
    pub cursor_available: bool,
    pub settings: kestrel_core::RecordingConfiguration,
    pub folder: std::path::PathBuf,
}

/// An optional audio track in the recording setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTrackOptionViewModel {
    pub track: AudioTrack,
    pub available: bool,
    pub enabled: bool,
}

/// One track of the running recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTrackViewModel {
    pub label: &'static str,
    pub status: String,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTakeViewModel {
    pub path: std::path::PathBuf,
    pub name: String,
    pub detail: String,
}

/// Which controls the recording panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingControls {
    /// Source buttons and options.
    #[default]
    Setup,
    /// Waiting on the desktop or the area view; Cancel only.
    Waiting,
    Recording {
        paused: bool,
    },
    /// Completing the file; nothing to press.
    Stopping,
}

fn export_view(export: ExportPhase) -> Option<RecordingExportViewModel> {
    let file_name = |path: &std::path::Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match export {
        ExportPhase::Idle => None,
        ExportPhase::Exporting { name, percent } => Some(RecordingExportViewModel {
            status: format!("Exporting {name}… {percent}%"),
            running: true,
            failed: false,
            path: None,
        }),
        ExportPhase::Exported { path } => Some(RecordingExportViewModel {
            status: format!("Exported {}.", file_name(&path)),
            running: false,
            failed: false,
            path: Some(path),
        }),
        ExportPhase::Failed { error } => Some(RecordingExportViewModel {
            status: match error.remediation() {
                Some(remediation) => format!("Export failed: {} {remediation}", error.message),
                None => format!("Export failed: {}", error.message),
            },
            running: false,
            failed: true,
            path: None,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingViewModel {
    pub running: bool,
    pub status: String,
    /// Set while the status reports a problem.
    pub warning: bool,
    pub controls: RecordingControls,
    pub sources: Vec<RecordingSource>,
    pub tracks_offered: Vec<RecordingTrackOptionViewModel>,
    pub cursor_available: bool,
    pub cursor: bool,
    pub framerate: u32,
    /// Running time while recording; stands still while paused.
    pub clock: Option<RecordingClock>,
    pub tracks: Vec<RecordingTrackViewModel>,
    pub takes: Vec<RecordingTakeViewModel>,
    pub folder: std::path::PathBuf,
    /// A finished or failed outcome can be cleared.
    pub dismissable: bool,
    /// The area view should open for the frame the runtime holds.
    pub choosing_area: bool,
    /// The running or last trimmed export.
    pub export: Option<RecordingExportViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingExportViewModel {
    pub status: String,
    /// Cancel is offered while it runs; Dismiss once it ended.
    pub running: bool,
    pub failed: bool,
    /// The finished file, for Play.
    pub path: Option<std::path::PathBuf>,
}

impl RecordingViewModel {
    pub(crate) fn from_presentation(presentation: RecordingPresentation) -> Self {
        let RecordingPresentation {
            running,
            reason,
            snapshot,
            sources,
            available_tracks,
            cursor_available,
            settings,
            folder,
        } = presentation;
        let tracks_offered = AudioTrack::ALL
            .into_iter()
            .map(|track| RecordingTrackOptionViewModel {
                track,
                available: available_tracks.contains(&track),
                enabled: match track {
                    AudioTrack::System => settings.system_audio,
                    AudioTrack::Microphone => settings.microphone,
                },
            })
            .collect();
        let takes = snapshot
            .takes
            .iter()
            .map(|take| RecordingTakeViewModel {
                path: take.path.clone(),
                name: take.name.clone(),
                detail: match take.duration {
                    Some(duration) => {
                        format!(
                            "{} · {}",
                            format_bytes(take.bytes as f64),
                            format_clock(duration)
                        )
                    }
                    None => format_bytes(take.bytes as f64),
                },
            })
            .collect();
        let mut model = Self {
            running,
            sources,
            tracks_offered,
            cursor_available,
            cursor: settings.cursor,
            framerate: settings.framerate,
            takes,
            folder,
            export: export_view(snapshot.export),
            ..Self::default()
        };
        if !running {
            model.status =
                reason.unwrap_or_else(|| "Enable capture.recording in the Feature Hub.".to_owned());
            model.export = None;
            return model;
        }
        match snapshot.phase {
            RecordingPhase::Idle => {
                model.status = "Choose what to record. The desktop asks before sharing, and \
                                recordings stay on this computer."
                    .to_owned();
            }
            RecordingPhase::Requesting { source } => {
                model.controls = RecordingControls::Waiting;
                model.status = format!(
                    "Waiting for the desktop to share a {}…",
                    match source {
                        RecordingSource::Window => "window",
                        RecordingSource::Screen | RecordingSource::Area => "screen",
                    }
                );
            }
            RecordingPhase::ChoosingArea { .. } => {
                model.controls = RecordingControls::Waiting;
                model.choosing_area = true;
                model.status = "Drag the area to record.".to_owned();
            }
            RecordingPhase::Recording {
                source,
                tracks,
                paused,
                clock,
            } => {
                model.controls = RecordingControls::Recording { paused };
                model.clock = Some(clock);
                model.status = format!(
                    "{} recording {}.",
                    source.label(),
                    if paused { "paused" } else { "in progress" }
                );
                model.warning = tracks.iter().any(|track| track.error.is_some());
                model.tracks = tracks
                    .into_iter()
                    .map(|state| RecordingTrackViewModel {
                        label: state.track.label(),
                        failed: state.error.is_some(),
                        status: state
                            .error
                            .map_or_else(|| "Recording".to_owned(), |error| error.message),
                    })
                    .collect();
            }
            RecordingPhase::Stopping => {
                model.controls = RecordingControls::Stopping;
                model.status = "Saving the recording…".to_owned();
            }
            RecordingPhase::Completed { take, note } => {
                model.dismissable = true;
                model.warning = note.is_some();
                model.status = match note {
                    Some(note) => format!("Saved {}. {}", take.name, note.message),
                    None => format!("Saved {}.", take.name),
                };
            }
            RecordingPhase::Failed { error } => {
                model.dismissable = true;
                model.warning = true;
                model.status = match error.remediation() {
                    Some(remediation) => format!("{} {remediation}", error.message),
                    None => error.message,
                };
            }
        }
        model
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_clock(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// One screen tool's gate, from its registration.
pub(crate) struct ScreenToolGate {
    pub tool: ScreenTool,
    pub running: bool,
    /// Why the tool cannot run; `None` when it is running.
    pub reason: Option<String>,
}

pub(crate) struct ScreenToolsPresentation {
    pub snapshot: ScreenToolsSnapshot,
    pub gates: Vec<ScreenToolGate>,
    pub settings: kestrel_core::ScreenToolsConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenToolButtonViewModel {
    pub tool: ScreenTool,
    pub label: &'static str,
    pub enabled: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenToolResultViewModel {
    Text(String),
    Qr(Vec<QrRowViewModel>),
    Color {
        rgb: [u8; 3],
        hex: String,
        css_rgb: String,
        hsl: String,
    },
}

/// One decoded QR code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrRowViewModel {
    pub text: String,
    /// An http(s) address to open.
    pub open: Option<String>,
    /// Text payloads can be copied; binary ones are not shown.
    pub copy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScreenToolsViewModel {
    pub buttons: Vec<ScreenToolButtonViewModel>,
    /// At least one tool is running.
    pub available: bool,
    pub status: String,
    /// A job is collecting or processing; Cancel is offered.
    pub busy: bool,
    /// The selection view should open for this tool.
    pub selecting: Option<ScreenTool>,
    pub result: Option<ScreenToolResultViewModel>,
    /// Seeds the Settings rows.
    pub settings: kestrel_core::ScreenToolsConfiguration,
}

impl ScreenToolsViewModel {
    pub(crate) fn from_presentation(presentation: ScreenToolsPresentation) -> Self {
        let buttons = presentation
            .gates
            .into_iter()
            .map(|gate| ScreenToolButtonViewModel {
                tool: gate.tool,
                label: gate.tool.label(),
                enabled: gate.running,
                reason: gate.reason,
            })
            .collect::<Vec<_>>();
        let available = buttons.iter().any(|button| button.enabled);
        let phase = presentation.snapshot.phase;
        let busy = matches!(
            phase,
            ScreenToolsPhase::Capturing(_)
                | ScreenToolsPhase::Selecting(_)
                | ScreenToolsPhase::Processing(_)
        );
        let (status, selecting, result) = match phase {
            ScreenToolsPhase::Idle if available => (
                "Pick a tool, then the area or pixel to read. Nothing leaves this computer."
                    .to_owned(),
                None,
                None,
            ),
            ScreenToolsPhase::Idle => (
                "Screen tools are off. Enable capture.text, capture.qr, or capture.color in the \
                 Feature Hub."
                    .to_owned(),
                None,
                None,
            ),
            ScreenToolsPhase::Capturing(tool) => (
                format!("{}: waiting for the screen…", tool.label()),
                None,
                None,
            ),
            ScreenToolsPhase::Selecting(tool) => (
                format!("{}: choose in the selection window.", tool.label()),
                Some(tool),
                None,
            ),
            ScreenToolsPhase::Processing(tool) => {
                (format!("{}: reading…", tool.label()), None, None)
            }
            ScreenToolsPhase::Done(tool, result) => {
                let (status, result) = screen_tool_result(result);
                (format!("{}: {status}", tool.label()), None, Some(result))
            }
            ScreenToolsPhase::Failed(tool, error) => {
                (format!("{}: {}", tool.label(), error.message), None, None)
            }
        };
        Self {
            buttons,
            available,
            status,
            busy,
            selecting,
            result,
            settings: presentation.settings,
        }
    }
}

fn screen_tool_result(result: ScreenToolResult) -> (String, ScreenToolResultViewModel) {
    match result {
        ScreenToolResult::Text(text) if text.trim().is_empty() => (
            "no text was found.".to_owned(),
            ScreenToolResultViewModel::Text(text),
        ),
        ScreenToolResult::Text(text) => (
            format!("{} characters.", text.chars().count()),
            ScreenToolResultViewModel::Text(text),
        ),
        ScreenToolResult::Qr(payloads) => {
            let rows = payloads
                .into_iter()
                .map(|payload| match payload {
                    QrPayload::Text(text) => {
                        let lower = text.to_ascii_lowercase();
                        let link = (lower.starts_with("https://") || lower.starts_with("http://"))
                            && !text.chars().any(char::is_whitespace);
                        QrRowViewModel {
                            open: link.then(|| text.clone()),
                            copy: Some(text.clone()),
                            text,
                        }
                    }
                    QrPayload::Binary { bytes } => QrRowViewModel {
                        text: format!("Binary data ({bytes} bytes), not shown"),
                        open: None,
                        copy: None,
                    },
                })
                .collect::<Vec<_>>();
            let status = match rows.len() {
                0 => "no QR code was found.".to_owned(),
                1 => "1 code.".to_owned(),
                count => format!("{count} codes."),
            };
            (status, ScreenToolResultViewModel::Qr(rows))
        }
        ScreenToolResult::Color(color) => (
            color.hex(),
            ScreenToolResultViewModel::Color {
                rgb: [color.red, color.green, color.blue],
                hex: color.hex(),
                css_rgb: color.rgb(),
                hsl: color.hsl(),
            },
        ),
    }
}

/// Inputs for the capture panel.
pub(crate) struct CapturePresentation {
    pub snapshot: CaptureSnapshot,
    pub running: bool,
    pub provider: Option<CaptureProvider>,
    /// Modes the provider offers, in preferred order.
    pub modes: Vec<CaptureMode>,
    /// Thumbnail files aligned with `snapshot.entries`.
    pub thumbnails: Vec<Option<std::path::PathBuf>>,
    pub limits: kestrel_core::CaptureConfiguration,
    pub now: std::time::SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureModeViewModel {
    pub mode: CaptureMode,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEntryViewModel {
    pub id: CaptureId,
    pub title: String,
    pub subtitle: String,
    pub thumbnail: Option<std::path::PathBuf>,
}

/// Screenshot controls and recent captures, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureViewModel {
    pub running: bool,
    pub status: String,
    pub modes: Vec<CaptureModeViewModel>,
    pub capturing: bool,
    pub entries: Vec<CaptureEntryViewModel>,
    pub usage: String,
    pub storage_error: Option<String>,
    pub limits: kestrel_core::CaptureConfiguration,
}

impl CaptureViewModel {
    pub(crate) fn from_presentation(presentation: CapturePresentation) -> Self {
        let snapshot = presentation.snapshot;
        let capturing = matches!(snapshot.phase, CapturePhase::Capturing { .. });
        let status = if !presentation.running {
            "Screenshots are off. Enable capture.screenshot in the Feature Hub.".to_owned()
        } else if let Some(provider) = presentation.provider {
            match &snapshot.phase {
                CapturePhase::Idle => format!("Captures go through {}.", provider.label()),
                CapturePhase::Capturing { mode, .. } => match mode {
                    CaptureMode::Interactive => {
                        "Waiting for the desktop's capture dialog…".to_owned()
                    }
                    CaptureMode::Area => "Drag to select an area; Esc cancels.".to_owned(),
                    CaptureMode::Window | CaptureMode::Screen => "Capturing…".to_owned(),
                },
                CapturePhase::Captured { .. } => "Added to recent captures.".to_owned(),
                CapturePhase::Cancelled => "Capture cancelled; nothing was kept.".to_owned(),
                CapturePhase::Failed { kind, message } => {
                    let remedy = match kind {
                        CaptureErrorKind::Denied => {
                            " Allow screenshots for Kestrel in the desktop's privacy settings, \
                             or use Choose in dialog."
                        }
                        CaptureErrorKind::NoImage => {
                            " Save to a file in the desktop's dialog to add it here."
                        }
                        CaptureErrorKind::Unavailable => {
                            " Refresh capabilities after installing the missing provider."
                        }
                        CaptureErrorKind::Cancelled
                        | CaptureErrorKind::TooLarge
                        | CaptureErrorKind::Timeout
                        | CaptureErrorKind::Failed => "",
                    };
                    format!("Capture failed: {message}.{remedy}")
                }
            }
        } else {
            "No screenshot provider was found; the Feature Hub lists setup steps.".to_owned()
        };
        let modes = if presentation.running {
            presentation
                .modes
                .iter()
                .map(|&mode| CaptureModeViewModel {
                    mode,
                    label: mode.label(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let entries = snapshot
            .entries
            .iter()
            .zip(
                presentation
                    .thumbnails
                    .into_iter()
                    .chain(std::iter::repeat(None)),
            )
            .map(|(entry, thumbnail)| {
                let age = presentation
                    .now
                    .duration_since(entry.created_at)
                    .unwrap_or_default();
                CaptureEntryViewModel {
                    id: entry.id,
                    title: format!(
                        "{} · {}×{}{}",
                        entry.mode.label(),
                        entry.width,
                        entry.height,
                        if entry.edited { " · edited" } else { "" }
                    ),
                    subtitle: format!("{} · {}", format_age(age), format_bytes(entry.bytes as f64)),
                    thumbnail,
                }
            })
            .collect::<Vec<_>>();
        let limits = presentation.limits;
        let usage = format!(
            "{} of {} captures · {} of {} MiB · kept up to {} h",
            entries.len(),
            limits.max_entries,
            format_bytes(snapshot.total_bytes as f64),
            limits.max_total_megabytes,
            limits.max_age_hours
        );
        Self {
            running: presentation.running,
            status,
            modes,
            capturing,
            entries,
            usage,
            storage_error: snapshot.storage_error,
            limits,
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
        let label = snapshot
            .outputs
            .iter()
            .find(|output| output.id == switch.output_id)
            .map_or(switch.output_name.as_str(), |output| {
                output.description.as_str()
            });
        let mut text = match switch.reason {
            kestrel_services::audio::AudioSwitchReason::Manual => {
                format!("Switched the default output to {label}.")
            }
            kestrel_services::audio::AudioSwitchReason::Priority => {
                format!("Switched to {label}, the highest-ranked connected output.")
            }
        };
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
                // Applications launch through desktop-entry handling.
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
    pub shortcuts: ShortcutsViewModel,
    pub capture: CaptureViewModel,
    pub processes: ProcessesViewModel,
    pub screen_tools: ScreenToolsViewModel,
    pub camera: CameraViewModel,
    pub recording: RecordingViewModel,
    pub listeners: ListenersViewModel,
    pub clipboard: ClipboardViewModel,
    pub snippets: SnippetsViewModel,
    pub scratchpad: ScratchpadViewModel,
    pub command_bar: CommandBarViewModel,
    pub monitor: MonitorViewModel,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub appearance: AppearancePreference,
    pub close_action: kestrel_core::CloseAction,
    pub autostart: bool,
    pub panel_sections: Vec<PanelSectionViewModel>,
    pub can_undo: bool,
}
impl Default for ProcessesViewModel {
    fn default() -> Self {
        Self {
            running: false,
            status: "Processes are off.".to_owned(),
            rows: Vec::new(),
            query: String::new(),
            limits: kestrel_core::ProcessConfiguration::default(),
            stale_notice: None,
        }
    }
}

impl Default for ListenersViewModel {
    fn default() -> Self {
        Self {
            running: false,
            status: "Listening ports are off.".to_owned(),
            rows: Vec::new(),
            can_stop: false,
        }
    }
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
        shortcuts: ShortcutsPresentation<'a>,
        capture: CapturePresentation,
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
            microphone: MicrophoneViewModel::from_presentation(&configuration.audio, microphone),
            speed_test: SpeedTestViewModel::from_presentation(speed_test),
            shortcuts: ShortcutsViewModel::from_presentation(shortcuts),
            capture: CaptureViewModel::from_presentation(capture),
            processes: ProcessesViewModel::default(),
            screen_tools: ScreenToolsViewModel::default(),
            camera: CameraViewModel::default(),
            recording: RecordingViewModel::default(),
            listeners: ListenersViewModel::default(),
            clipboard: ClipboardViewModel::from_presentation(&configuration.clipboard, clipboard),
            snippets: SnippetsViewModel::from_presentation(snippets),
            scratchpad: ScratchpadViewModel::from_presentation(ScratchpadPresentation {
                warning: None,
                running: false,
                snapshot: None,
                limits: configuration.scratchpad,
            }),
            command_bar: CommandBarViewModel::from_presentation(command_bar),
            monitor: MonitorViewModel::from_configuration(&configuration.monitoring, monitor),
            warnings: warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            appearance: configuration.ui.appearance,
            close_action: configuration.ui.close_action,
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

    use kestrel_platform::capture::{CaptureErrorKind, CaptureMode, CaptureProvider};

    use super::{
        ApplicationViewModel, AudioPresentation, CapabilityKindViewModel,
        CapabilityStatusViewModel, ClipboardPresentation, ClipboardViewModel,
        CommandBarPresentation, CommandBarViewModel, CommandProvider, FeatureLifecycleViewModel,
        FeatureViewModel, MicrophonePresentation, MicrophoneViewModel, MonitorPresentation,
        MonitorViewModel, ShortcutsPresentation, ShortcutsViewModel, SnippetsPresentation,
        SnippetsViewModel, SpeedTestPresentation, SpeedTestViewModel,
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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

    fn test_capture_presentation() -> super::CapturePresentation {
        super::CapturePresentation {
            snapshot: kestrel_services::capture::CaptureSnapshot::default(),
            running: false,
            provider: None,
            modes: Vec::new(),
            thumbnails: Vec::new(),
            limits: kestrel_core::CaptureConfiguration::default(),
            now: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn test_shortcuts_presentation() -> ShortcutsPresentation<'static> {
        ShortcutsPresentation {
            status: kestrel_platform::global_shortcuts::ShortcutStatus::default(),
            running: false,
            configured: &[],
            skipped: &[],
        }
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
            application_id: Some(format!("application-{id}")),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            test_shortcuts_presentation(),
            test_capture_presentation(),
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
            capture_streams: Vec::new(),
            priority_switch: None,
        }
    }

    #[test]
    fn absent_ranked_inputs_keep_their_rank_and_monitor_capture_reads_as_system_audio() {
        let mut snapshot = microphone_snapshot(
            vec![
                microphone_input(1, false, true),
                microphone_input(2, false, false),
            ],
            MicrophoneMuteState::Live,
        );
        let capture = |id, from_monitor| kestrel_platform::audio::CaptureStream {
            id,
            application_name: Some("Recorder".into()),
            application_id: Some("recorder".into()),
            input_id: 1,
            from_monitor,
            volume_percent: Some(100),
            volume_writable: true,
            muted: false,
            corked: false,
        };
        snapshot.capture_streams = vec![capture(7, false), capture(8, true)];
        let configuration = kestrel_core::AudioConfiguration {
            input_priority: vec!["usb-mic".into(), "source-2".into()],
            follow_input_priority: true,
            ..Default::default()
        };
        let view = MicrophoneViewModel::from_presentation(
            &configuration,
            MicrophonePresentation {
                snapshot: &snapshot,
                running: true,
            },
        );

        let ranked: Vec<_> = view
            .input_priority
            .ranked
            .iter()
            .map(|device| (device.label.as_str(), device.connected))
            .collect();
        assert_eq!(ranked, [("usb-mic", false), ("Input 2", true)]);
        let unranked: Vec<_> = view
            .input_priority
            .unranked
            .iter()
            .map(|device| device.name.as_str())
            .collect();
        assert_eq!(unranked, ["source-1"]);
        let details: Vec<_> = view
            .capture_streams
            .iter()
            .map(|stream| stream.detail.as_str())
            .collect();
        assert_eq!(
            details,
            ["Recording from Input 1", "Recording system audio"]
        );
    }

    #[test]
    fn microphone_mute_is_claimed_only_from_an_agreeing_backend_reading() {
        let unknown = MicrophoneViewModel::from_presentation(
            &Default::default(),
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: true,
            },
        );
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
        let view = MicrophoneViewModel::from_presentation(
            &Default::default(),
            MicrophonePresentation {
                snapshot: &muted,
                running: true,
            },
        );
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
        let view = MicrophoneViewModel::from_presentation(
            &Default::default(),
            MicrophonePresentation {
                snapshot: &mixed,
                running: true,
            },
        );
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

        let view = MicrophoneViewModel::from_presentation(
            &Default::default(),
            MicrophonePresentation {
                snapshot: &snapshot,
                running: false,
            },
        );

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

        let view = MicrophoneViewModel::from_presentation(
            &Default::default(),
            MicrophonePresentation {
                snapshot: &snapshot,
                running: true,
            },
        );

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

    fn shortcut_binding(command: &str, trigger: &str) -> kestrel_core::ShortcutBinding {
        kestrel_core::ShortcutBinding {
            command: command.to_owned(),
            trigger: trigger.to_owned(),
        }
    }

    #[test]
    fn shortcut_rows_follow_backend_states_in_configuration_order() {
        use kestrel_platform::global_shortcuts::{
            BindingState, BindingStatus, ShortcutPhase, ShortcutProvider, ShortcutStatus,
        };
        let configured = [
            shortcut_binding("window.show", "LOGO+ALT+k"),
            shortcut_binding("microphone.toggle-mute", "LOGO+ALT+m"),
            shortcut_binding("audio.output-next", "LOGO+ALT+o"),
        ];
        let status = ShortcutStatus {
            provider: Some(ShortcutProvider::X11),
            phase: ShortcutPhase::Active,
            bindings: vec![
                BindingStatus {
                    id: "microphone.toggle-mute".to_owned(),
                    requested: "ALT+LOGO+m".to_owned(),
                    state: BindingState::Conflict {
                        reason: "ALT+LOGO+m is grabbed by another client".to_owned(),
                    },
                },
                BindingStatus {
                    id: "window.show".to_owned(),
                    requested: "ALT+LOGO+k".to_owned(),
                    state: BindingState::Bound {
                        trigger: Some("Super+Alt+K".to_owned()),
                    },
                },
            ],
            generation: 3,
        };

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status,
            running: true,
            configured: &configured,
            skipped: &["window.explode".to_owned()],
        });

        assert!(view.status.contains("X11 key grabs"));
        let rows = view
            .rows
            .iter()
            .map(|row| (row.command.as_str(), row.trigger.as_str(), row.active))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                ("window.show", "Super+Alt+K", true),
                ("microphone.toggle-mute", "LOGO+ALT+m", false),
                ("audio.output-next", "LOGO+ALT+o", false),
            ],
            "the backend's trigger description wins once bound"
        );
        assert!(view.rows[1].state.starts_with("In use:"));
        assert_eq!(view.rows[2].state, "Not registered");
        assert_eq!(view.rows[0].description, "Open the Kestrel window");
        assert!(
            view.notice
                .is_some_and(|notice| notice.contains("window.explode"))
        );
    }

    #[test]
    fn stopped_shortcuts_claim_no_registration() {
        use kestrel_platform::global_shortcuts::{
            BindingState, BindingStatus, ShortcutPhase, ShortcutStatus,
        };
        let configured = [shortcut_binding("window.show", "LOGO+ALT+k")];
        let stale = ShortcutStatus {
            provider: None,
            phase: ShortcutPhase::Active,
            bindings: vec![BindingStatus {
                id: "window.show".to_owned(),
                requested: "ALT+LOGO+k".to_owned(),
                state: BindingState::Bound { trigger: None },
            }],
            generation: 1,
        };

        let view = ShortcutsViewModel::from_presentation(ShortcutsPresentation {
            status: stale,
            running: false,
            configured: &configured,
            skipped: &[],
        });

        assert!(view.status.contains("kestrel --command"));
        assert!(!view.rows[0].active);
        assert_eq!(view.rows[0].state, "Not registered");
    }

    fn capture_snapshot_with_entries(
        directory: &std::path::Path,
        phase: kestrel_services::capture::CapturePhase,
    ) -> kestrel_services::capture::CaptureSnapshot {
        use kestrel_services::capture::{
            CaptureHistory, CapturePolicy, NewCapture,
            image::{RgbaImage, encode_png},
        };
        let mut history = CaptureHistory::open(
            directory.to_path_buf(),
            CapturePolicy::from_configuration(&kestrel_core::CaptureConfiguration::default()),
        )
        .expect("history opens");
        let png = encode_png(&RgbaImage::new(4, 3, vec![255; 48]).expect("valid image"))
            .expect("png encodes");
        let base = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        for (offset, edited) in [(0, false), (60, true)] {
            history
                .insert(
                    &png,
                    NewCapture {
                        mode: CaptureMode::Area,
                        provider: Some(CaptureProvider::Grim),
                        edited,
                        width: 4,
                        height: 3,
                    },
                    base + std::time::Duration::from_secs(offset),
                )
                .expect("capture stores");
        }
        kestrel_services::capture::CaptureSnapshot {
            phase,
            entries: history.entries().to_vec(),
            total_bytes: history.total_bytes(),
            storage_error: None,
            generation: 3,
        }
    }

    #[test]
    fn capture_panel_lists_newest_first_and_explains_a_denied_capture() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let snapshot = capture_snapshot_with_entries(
            directory.path(),
            kestrel_services::capture::CapturePhase::Failed {
                kind: CaptureErrorKind::Denied,
                message: "The desktop denied screenshot permission".to_owned(),
            },
        );
        let view = super::CaptureViewModel::from_presentation(super::CapturePresentation {
            snapshot,
            running: true,
            provider: Some(CaptureProvider::Portal),
            modes: vec![CaptureMode::Interactive, CaptureMode::Screen],
            thumbnails: Vec::new(),
            limits: kestrel_core::CaptureConfiguration::default(),
            now: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_120),
        });

        assert!(view.status.contains("privacy settings"), "{}", view.status);
        assert_eq!(
            view.modes.iter().map(|mode| mode.mode).collect::<Vec<_>>(),
            [CaptureMode::Interactive, CaptureMode::Screen]
        );
        assert!(!view.capturing);
        assert_eq!(view.entries.len(), 2);
        assert!(view.entries[0].title.ends_with("edited"), "newest first");
        assert!(view.entries[0].subtitle.starts_with("1 min old"));
        assert!(view.entries[1].subtitle.starts_with("2 min old"));
        assert!(view.usage.starts_with("2 of 30 captures"), "{}", view.usage);
    }

    #[test]
    fn a_stopped_capture_feature_offers_no_modes() {
        let view = super::CaptureViewModel::from_presentation(super::CapturePresentation {
            running: false,
            provider: Some(CaptureProvider::X11),
            modes: vec![CaptureMode::Window, CaptureMode::Screen],
            ..test_capture_presentation()
        });
        assert!(view.modes.is_empty());
        assert!(view.status.contains("capture.screenshot"));
    }

    mod processes {
        use crate::view_model::{
            ListenersPresentation, ListenersViewModel, ProcessActionKind, ProcessesPresentation,
            ProcessesViewModel,
        };
        use kestrel_platform::processes::{
            Listener, ListenerProtocol, ProcessIdentity, ProcessKind, ProcessSample, ProcessScope,
        };
        use kestrel_services::processes::{ListenerSnapshot, ProcessRow, ProcessSnapshot};

        const USER: u32 = 1000;
        const KESTREL: u32 = 4242;

        fn sample(pid: u32, name: &str, uid: Option<u32>, scope: ProcessScope) -> ProcessRow {
            ProcessRow {
                sample: ProcessSample {
                    identity: ProcessIdentity {
                        pid,
                        start_ticks: u64::from(pid) * 10,
                    },
                    parent_pid: 1,
                    name: name.to_owned(),
                    executable: Some(name.to_owned()),
                    uid,
                    owner: uid.map(|uid| if uid == USER { "me" } else { "root" }.to_owned()),
                    kind: ProcessKind::User,
                    state: 'S',
                    cpu_ticks: 0,
                    resident_bytes: 4096,
                    threads: 1,
                    scope_root: scope != ProcessScope::Other,
                    scope,
                },
                cpu_percent: Some(1.0),
            }
        }

        fn view(rows: Vec<ProcessRow>, running: bool, query: &str) -> ProcessesViewModel {
            view_with_signalling(rows, running, query, true)
        }

        fn view_with_signalling(
            rows: Vec<ProcessRow>,
            running: bool,
            query: &str,
            signalling: bool,
        ) -> ProcessesViewModel {
            ProcessesViewModel::from_presentation(ProcessesPresentation {
                snapshot: ProcessSnapshot {
                    total_processes: rows.len(),
                    rows,
                    ..ProcessSnapshot::default()
                },
                running,
                query: query.to_owned(),
                current_uid: USER,
                own_pid: KESTREL,
                limits: kestrel_core::ProcessConfiguration::default(),
                signalling,
            })
        }

        fn actions(view: &ProcessesViewModel, pid: u32) -> Vec<ProcessActionKind> {
            view.rows
                .iter()
                .find(|row| row.identity.pid == pid)
                .expect("row is listed")
                .actions
                .iter()
                .map(|action| action.action)
                .collect()
        }

        #[test]
        fn only_own_eligible_processes_offer_actions() {
            let mut kernel = sample(2, "kthreadd", Some(0), ProcessScope::Other);
            kernel.sample.kind = ProcessKind::KernelThread;
            let view = view(
                vec![
                    sample(100, "editor", Some(USER), ProcessScope::Other),
                    sample(200, "sshd", Some(0), ProcessScope::Other),
                    sample(300, "ghost", None, ProcessScope::Other),
                    sample(KESTREL, "kestrel", Some(USER), ProcessScope::Other),
                    sample(1, "systemd", Some(USER), ProcessScope::Other),
                    kernel,
                ],
                true,
                "",
            );

            assert_eq!(
                actions(&view, 100),
                [ProcessActionKind::Terminate, ProcessActionKind::Kill]
            );
            for pid in [200, 300, KESTREL, 1, 2] {
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.identity.pid == pid)
                    .unwrap();
                assert!(row.actions.is_empty(), "PID {pid} must not be actionable");
                assert!(row.disabled_reason.is_some(), "PID {pid} explains why");
            }
            let editor = view
                .rows
                .iter()
                .find(|row| row.identity.pid == 100)
                .unwrap();
            assert!(
                editor.subtitle.contains("start 1000"),
                "{}",
                editor.subtitle
            );
        }

        #[test]
        fn restart_requires_a_unit_or_desktop_launch_scope() {
            let view = view(
                vec![
                    sample(
                        10,
                        "pipewire",
                        Some(USER),
                        ProcessScope::UserUnit {
                            unit: "pipewire.service".to_owned(),
                        },
                    ),
                    sample(
                        11,
                        "firefox",
                        Some(USER),
                        ProcessScope::DesktopLaunch {
                            desktop_id: "firefox.desktop".to_owned(),
                            scope: "app-gnome-firefox-11.scope".to_owned(),
                        },
                    ),
                    sample(12, "bash", Some(USER), ProcessScope::Other),
                ],
                true,
                "",
            );
            let restart = ProcessActionKind::Restart { escalate: false };
            assert!(actions(&view, 10).contains(&restart));
            assert!(actions(&view, 11).contains(&restart));
            assert!(!actions(&view, 12).contains(&restart));
        }

        #[test]
        fn without_pidfd_no_process_or_listener_offers_an_action() {
            let unit = ProcessScope::UserUnit {
                unit: "pipewire.service".to_owned(),
            };
            let processes = view_with_signalling(
                vec![sample(10, "pipewire", Some(USER), unit)],
                true,
                "",
                false,
            );
            assert!(processes.rows[0].actions.is_empty());
            assert!(processes.status.contains("pidfd"), "{}", processes.status);

            let listeners = ListenersViewModel::from_presentation(ListenersPresentation {
                snapshot: ListenerSnapshot::default(),
                running: true,
                signalling: false,
            });
            assert!(!listeners.can_stop);
            assert!(listeners.status.contains("pidfd"), "{}", listeners.status);
        }

        #[test]
        fn search_filters_rows_and_a_stopped_feature_lists_none() {
            let rows = vec![
                sample(100, "editor", Some(USER), ProcessScope::Other),
                sample(200, "compiler", Some(USER), ProcessScope::Other),
            ];
            let filtered = view(rows.clone(), true, "comp");
            assert_eq!(
                filtered
                    .rows
                    .iter()
                    .map(|row| row.identity.pid)
                    .collect::<Vec<_>>(),
                [200]
            );
            let stopped = view(rows, false, "");
            assert!(stopped.rows.is_empty());
            assert!(
                stopped.status.contains("system.processes"),
                "{}",
                stopped.status
            );
        }

        #[test]
        fn listeners_without_a_verified_owner_cannot_be_stopped() {
            let owned = Listener {
                protocol: ListenerProtocol::Tcp,
                address: "127.0.0.1".parse().unwrap(),
                port: 8765,
                owner: Some(ProcessIdentity {
                    pid: 100,
                    start_ticks: 1,
                }),
                owner_name: Some("python3".to_owned()),
            };
            let foreign = Listener {
                protocol: ListenerProtocol::Udp,
                address: "0.0.0.0".parse().unwrap(),
                port: 5353,
                owner: None,
                owner_name: None,
            };
            let view = ListenersViewModel::from_presentation(ListenersPresentation {
                snapshot: ListenerSnapshot {
                    listeners: vec![owned, foreign],
                    ..ListenerSnapshot::default()
                },
                running: true,
                signalling: true,
            });

            let [owned, foreign] = view.rows.as_slice() else {
                panic!("two rows expected");
            };
            assert_eq!(owned.pid, Some(100));
            assert!(owned.stop_disabled_reason.is_none());
            assert_eq!(owned.open_url.as_deref(), Some("http://127.0.0.1:8765/"));
            assert_eq!(foreign.owner_identity, None);
            assert!(foreign.stop_disabled_reason.is_some());
            assert!(view.can_stop);
            assert_eq!(foreign.open_url, None, "UDP has no browser action");
        }
        #[test]
        fn scratchpad_view_model_projects_tabs_preview_and_expiry() {
            use super::super::{ScratchpadPresentation, ScratchpadSnapshot, ScratchpadViewModel};
            let note = kestrel_services::scratchpad::ScratchpadNote {
                id: kestrel_services::scratchpad::NoteId::from(7),
                title: "Idea".to_owned(),
                text: "# Hello & <world>".to_owned(),
                created_at: 1,
                updated_at: 1,
            };
            let model = ScratchpadViewModel::from_presentation(ScratchpadPresentation {
                warning: None,
                running: true,
                snapshot: Some(ScratchpadSnapshot {
                    notes: vec![note],
                    active: Some(kestrel_services::scratchpad::NoteId::from(7)),
                    expired: vec![kestrel_services::scratchpad::NoteId::from(7)],
                    warning: Some("save warning".to_owned()),
                }),
                limits: kestrel_core::ScratchpadConfiguration::default(),
            });
            assert_eq!(model.active_id, Some(7));
            assert_eq!(model.tabs[0].title, "Idea");
            assert!(model.preview_markup.contains("&amp;"));
            assert_eq!(model.expired[0].title, "Idea");
            assert_eq!(model.warning.as_deref(), Some("save warning"));
            let off = ScratchpadViewModel::from_presentation(ScratchpadPresentation {
                warning: None,
                running: false,
                snapshot: None,
                limits: kestrel_core::ScratchpadConfiguration::default(),
            });
            assert!(!off.running);
            assert!(off.tabs.is_empty());
        }
    }

    mod screen_tools {
        use crate::view_model::{
            QrRowViewModel, ScreenToolGate, ScreenToolResultViewModel, ScreenToolsPresentation,
            ScreenToolsViewModel,
        };
        use kestrel_platform::capture::{CaptureError, CaptureErrorKind};
        use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
        use kestrel_services::screen_tools::{
            ColorSample, QrPayload, ScreenToolResult, ScreenToolsPhase, ScreenToolsSnapshot,
        };

        fn view(phase: ScreenToolsPhase, running: &[ScreenTool]) -> ScreenToolsViewModel {
            ScreenToolsViewModel::from_presentation(ScreenToolsPresentation {
                snapshot: ScreenToolsSnapshot {
                    phase,
                    generation: 1,
                },
                gates: ALL_SCREEN_TOOLS
                    .into_iter()
                    .map(|tool| {
                        let running = running.contains(&tool);
                        ScreenToolGate {
                            tool,
                            running,
                            reason: (!running).then(|| format!("{} is off", tool.feature_id())),
                        }
                    })
                    .collect(),
                settings: kestrel_core::ScreenToolsConfiguration::default(),
            })
        }

        #[test]
        fn each_tool_is_gated_on_its_own_feature() {
            let view = view(ScreenToolsPhase::Idle, &[ScreenTool::Qr]);
            let enabled = view
                .buttons
                .iter()
                .map(|button| (button.tool, button.enabled, button.reason.is_some()))
                .collect::<Vec<_>>();
            assert_eq!(
                enabled,
                [
                    (ScreenTool::Text, false, true),
                    (ScreenTool::Qr, true, false),
                    (ScreenTool::Color, false, true),
                ]
            );
            assert!(view.available && !view.busy);
            assert!(!self::view(ScreenToolsPhase::Idle, &[]).available);
        }

        #[test]
        fn a_waiting_frame_asks_for_the_selection_view_and_failures_explain() {
            let selecting = view(
                ScreenToolsPhase::Selecting(ScreenTool::Text),
                &ALL_SCREEN_TOOLS,
            );
            assert_eq!(selecting.selecting, Some(ScreenTool::Text));
            assert!(selecting.busy);
            let failed = view(
                ScreenToolsPhase::Failed(
                    ScreenTool::Text,
                    CaptureError::new(CaptureErrorKind::Denied, "The desktop denied it."),
                ),
                &ALL_SCREEN_TOOLS,
            );
            assert!(failed.status.contains("The desktop denied it."));
            assert_eq!((failed.selecting, failed.result), (None, None));
        }

        #[test]
        fn only_plain_web_addresses_open_and_binary_codes_stay_hidden() {
            let view = view(
                ScreenToolsPhase::Done(
                    ScreenTool::Qr,
                    ScreenToolResult::Qr(vec![
                        QrPayload::Text("HTTPS://example.org/a".into()),
                        QrPayload::Text("https://example.org/a b".into()),
                        QrPayload::Text("javascript:alert(1)".into()),
                        QrPayload::Binary { bytes: 9 },
                    ]),
                ),
                &ALL_SCREEN_TOOLS,
            );
            let Some(ScreenToolResultViewModel::Qr(rows)) = view.result else {
                panic!("QR rows expected");
            };
            let opens = rows
                .iter()
                .map(|row| row.open.is_some())
                .collect::<Vec<_>>();
            assert_eq!(opens, [true, false, false, false]);
            let QrRowViewModel { text, copy, .. } = &rows[3];
            assert_eq!(copy, &None);
            assert!(text.contains("9 bytes"), "{text}");
        }

        #[test]
        fn picked_colors_show_every_format() {
            let view = view(
                ScreenToolsPhase::Done(
                    ScreenTool::Color,
                    ScreenToolResult::Color(ColorSample {
                        red: 51,
                        green: 102,
                        blue: 204,
                    }),
                ),
                &ALL_SCREEN_TOOLS,
            );
            assert_eq!(
                view.result,
                Some(ScreenToolResultViewModel::Color {
                    rgb: [51, 102, 204],
                    hex: "#3366cc".into(),
                    css_rgb: "rgb(51, 102, 204)".into(),
                    hsl: "hsl(220, 60%, 50%)".into(),
                })
            );
        }
    }

    mod camera {
        use crate::view_model::{CameraPresentation, CameraViewModel};
        use kestrel_platform::camera::{CameraError, CameraErrorKind, CameraInfo};
        use kestrel_services::camera::{CameraPhase, CameraSnapshot};

        fn view(running: bool, phase: CameraPhase) -> CameraViewModel {
            CameraViewModel::from_presentation(CameraPresentation {
                running,
                reason: (!running).then(|| "Log in at the computer's own seat.".to_owned()),
                snapshot: CameraSnapshot {
                    phase,
                    generation: 1,
                },
                cameras: vec![CameraInfo {
                    path: "/dev/video0".into(),
                    name: "Camera".into(),
                }],
            })
        }

        #[test]
        fn preview_state_drives_stop_and_frame_display() {
            let idle = view(true, CameraPhase::Idle);
            assert!(!idle.active && !idle.previewing);
            let starting = view(
                true,
                CameraPhase::Starting {
                    name: "Camera".into(),
                },
            );
            assert!(starting.active && !starting.previewing);
            let previewing = view(
                true,
                CameraPhase::Previewing {
                    name: "Camera".into(),
                    width: 640,
                    height: 480,
                },
            );
            assert!(previewing.active && previewing.previewing);
            assert!(
                previewing.status.contains("640 × 480"),
                "{}",
                previewing.status
            );
        }

        #[test]
        fn failures_and_unavailable_features_explain_themselves() {
            let failed = view(
                true,
                CameraPhase::Failed {
                    name: "Camera".into(),
                    error: CameraError::new(CameraErrorKind::Busy, "Another app has it."),
                },
            );
            assert!(!failed.active);
            assert!(failed.status.contains("Another app has it."));
            let off = view(false, CameraPhase::Idle);
            assert!(!off.active);
            assert_eq!(off.status, "Log in at the computer's own seat.");
        }
    }

    mod recording {
        use crate::view_model::{
            RecordingControls, RecordingPresentation, RecordingViewModel, format_clock,
        };
        use kestrel_platform::recording::{
            AudioTrack, RecordingError, RecordingErrorKind, RecordingSource,
        };
        use kestrel_services::recording::{
            RecordingClock, RecordingPhase, RecordingSnapshot, Take, TrackState,
        };
        use std::time::Duration;

        fn view(phase: RecordingPhase, available: Vec<AudioTrack>) -> RecordingViewModel {
            RecordingViewModel::from_presentation(RecordingPresentation {
                running: true,
                reason: None,
                snapshot: RecordingSnapshot {
                    phase,
                    generation: 1,
                    takes: vec![Take {
                        path: "/videos/Kestrel-recording-1.webm".into(),
                        name: "Kestrel-recording-1.webm".into(),
                        bytes: 3 * 1024 * 1024,
                        duration: Some(Duration::from_secs(75)),
                    }],
                    export: kestrel_services::recording::ExportPhase::Idle,
                },
                sources: vec![RecordingSource::Screen, RecordingSource::Window],
                available_tracks: available,
                cursor_available: true,
                settings: kestrel_core::RecordingConfiguration {
                    microphone: true,
                    ..kestrel_core::RecordingConfiguration::default()
                },
                folder: "/videos".into(),
            })
        }

        #[test]
        fn setup_offers_only_available_tracks() {
            let setup = view(RecordingPhase::Idle, vec![AudioTrack::System]);
            assert_eq!(setup.controls, RecordingControls::Setup);
            let microphone = &setup.tracks_offered[1];
            assert_eq!(microphone.track, AudioTrack::Microphone);
            assert!(microphone.enabled && !microphone.available);
            assert!(setup.tracks_offered[0].available);
            assert_eq!(setup.takes[0].detail, "3.0 MiB · 1:15");
        }

        #[test]
        fn running_recordings_show_controls_and_failed_tracks() {
            let recording = view(
                RecordingPhase::Recording {
                    source: RecordingSource::Window,
                    tracks: vec![
                        TrackState {
                            track: AudioTrack::System,
                            error: None,
                        },
                        TrackState {
                            track: AudioTrack::Microphone,
                            error: Some(RecordingError::new(
                                RecordingErrorKind::Unavailable,
                                "Microphone stopped: unplugged",
                            )),
                        },
                    ],
                    paused: true,
                    clock: RecordingClock::default(),
                },
                AudioTrack::ALL.to_vec(),
            );
            assert_eq!(
                recording.controls,
                RecordingControls::Recording { paused: true }
            );
            assert!(recording.warning);
            assert!(!recording.tracks[0].failed);
            assert!(recording.tracks[1].failed);
            assert_eq!(recording.tracks[1].status, "Microphone stopped: unplugged");
        }

        #[test]
        fn outcomes_can_be_dismissed_and_explain_remediation() {
            let denied = view(
                RecordingPhase::Failed {
                    error: RecordingError::new(RecordingErrorKind::Denied, "Refused."),
                },
                Vec::new(),
            );
            assert!(denied.dismissable && denied.warning);
            assert!(
                denied.status.contains("privacy settings"),
                "{}",
                denied.status
            );
            let choosing = view(
                RecordingPhase::ChoosingArea {
                    frame: std::sync::Arc::new(
                        kestrel_services::capture::image::RgbaImage::new(1, 1, vec![0; 4])
                            .expect("image"),
                    ),
                },
                Vec::new(),
            );
            assert!(choosing.choosing_area);
            assert_eq!(choosing.controls, RecordingControls::Waiting);
        }

        #[test]
        fn clock_text_grows_hours_past_an_hour() {
            assert_eq!(format_clock(Duration::from_secs(59)), "0:59");
            assert_eq!(format_clock(Duration::from_secs(3_725)), "1:02:05");
        }

        #[test]
        fn exports_show_progress_then_their_outcome() {
            use kestrel_services::recording::ExportPhase;
            let with_export = |export: ExportPhase| {
                let mut presentation_view = view(RecordingPhase::Idle, Vec::new());
                presentation_view.export = super::super::export_view(export);
                presentation_view
            };
            let running = with_export(ExportPhase::Exporting {
                name: "clip.webm".into(),
                percent: 40,
            })
            .export
            .expect("export row");
            assert!(running.running && !running.failed);
            assert_eq!(running.status, "Exporting clip.webm… 40%");
            let failed = with_export(ExportPhase::Failed {
                error: RecordingError::new(RecordingErrorKind::Unavailable, "No VP8 encoder."),
            })
            .export
            .expect("export row");
            assert!(failed.failed && !failed.running);
            assert!(failed.status.contains("Install"), "{}", failed.status);
            let done = with_export(ExportPhase::Exported {
                path: "/clips/clip.webm".into(),
            })
            .export
            .expect("export row");
            assert_eq!(
                done.path.as_deref(),
                Some(std::path::Path::new("/clips/clip.webm"))
            );
            assert!(with_export(ExportPhase::Idle).export.is_none());
        }
    }
}
