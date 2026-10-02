use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, CapabilityStatus, CostLevel,
    MonitorConfiguration, MonitorReadout, PanelSection, Permission, ResourceCost,
};
use kestrel_platform::quick_toggles::{
    MutationConfirmation, QuickToggleControl, QuickToggleId, ToggleAction,
};
use kestrel_services::{
    ServiceLifecycle, ServiceRegistration,
    alerts::{ActiveAlert, AlertPolicy, AlertSnapshot},
    audio::{AudioAvailability, AudioPolicy, AudioSnapshot},
    clipboard::{ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
    quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
    system_monitor::{HistorySummary, SystemSnapshot},
};

use crate::ConfigurationWarning;

/// Owned inputs used to construct monitoring presentation state.
pub(crate) struct MonitorPresentation<'a> {
    pub snapshot: Option<&'a SystemSnapshot>,
    pub history: Option<HistorySummary>,
    pub alerts: AlertSnapshot,
    pub running: bool,
    /// Effective alert rules, including gates applied outside the configuration
    /// (such as the `power.battery-alerts` quick toggle). The configuration stays
    /// the user's intent; the policy is what the engine actually enforces.
    pub policy: Option<&'a AlertPolicy>,
}

/// Immutable presentation state for one system-monitor panel.
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
    /// Single source of truth for monitoring presentation.
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
        // Settings rows follow the configured order first, then the readouts that are
        // currently hidden, so the move arrows describe the list they actually reorder.
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
    // Exact platform source strings: a substring match would attribute another
    // family's failure (for example `/proc/diskstats`) to this readout.
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

/// Owned inputs used to construct audio mixer presentation state.
pub(crate) struct AudioPresentation<'a> {
    /// The mixer snapshot; `None` before the opt-in service has produced one.
    pub snapshot: Option<&'a AudioSnapshot>,
    /// Whether the opt-in mixer registration is currently running.
    pub running: bool,
    /// The effective mixer policy, which the snapshot does not carry.
    pub policy: AudioPolicy,
}

/// Immutable presentation state for the audio mixer panel.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioViewModel {
    pub running: bool,
    pub status: String,
    /// The last output switch or device-loss reconciliation, when one happened.
    pub message: Option<String>,
    pub boost_ceiling_percent: u8,
    pub max_boost_percent: u8,
    pub output_groups: Vec<AudioOutputGroupViewModel>,
    /// The flat output list in discovery order, used by routing selectors.
    pub outputs: Vec<AudioOutputViewModel>,
    pub streams: Vec<AudioStreamViewModel>,
    pub default_output_id: Option<u32>,
    pub inactive_streams: usize,
    pub policy: AudioPolicyViewModel,
}

/// One group of output devices that share a hardware card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputGroupViewModel {
    pub label: String,
    pub outputs: Vec<AudioOutputViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputViewModel {
    pub id: u32,
    pub title: String,
    /// The backend device name, shown so two similar devices stay distinguishable.
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
            // The snapshot may list inactive streams, so count only playing ones here.
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

/// Owned inputs used to construct clipboard presentation state.
#[derive(Default)]
pub(crate) struct ClipboardPresentation<'a> {
    /// Metadata-only service snapshot; it never carries clipboard content.
    pub snapshot: Option<ClipboardSnapshot>,
    pub running: bool,
    /// The explicit search that produced `matches`, echoed for the search field.
    pub search_query: &'a str,
    /// Bounded previews returned by an explicit search request.
    pub matches: &'a [ClipboardMatch],
    /// The one entry preview the user explicitly asked for.
    pub preview: Option<&'a ClipboardPreview>,
}

/// Immutable presentation state for the clipboard history panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardViewModel {
    pub running: bool,
    pub status: String,
    /// Which entry kinds the active provider can capture.
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

        // Rows come from the explicit search result, so the window only ever
        // renders content the user asked to see.
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

/// Immutable presentation state for the normal Kestrel window.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationViewModel {
    pub features: Vec<FeatureViewModel>,
    pub quick_toggles: Vec<QuickToggleViewModel>,
    pub audio: AudioViewModel,
    pub clipboard: ClipboardViewModel,
    pub monitor: MonitorViewModel,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub appearance: AppearancePreference,
    pub autostart: bool,
    pub panel_sections: Vec<PanelSectionViewModel>,
    pub can_undo: bool,
}

impl ApplicationViewModel {
    // Each presentation is an independent borrowed projection of one service;
    // grouping them would only move the same fields behind another struct.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<'a>(
        registrations: impl Iterator<Item = &'a ServiceRegistration>,
        quick_toggles: impl Iterator<Item = &'a QuickToggleSnapshot>,
        audio: AudioPresentation<'a>,
        clipboard: ClipboardPresentation<'a>,
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
            clipboard: ClipboardViewModel::from_presentation(&configuration.clipboard, clipboard),
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

/// Presentation state for configured panel placement and visibility.
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

/// Conservative resource-use labels disclosed by the feature hub.
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
/// A distinct action users can take to improve a capability state.
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

/// Presentation state for one registered feature.
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

/// User-facing lifecycle state, independent of service enum formatting.
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

/// Presentation state for one capability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityViewModel {
    pub status: CapabilityStatusViewModel,
    pub summary: String,
    pub selected_backend: Option<String>,
    pub remediation: Option<RemediationViewModel>,
}

/// Stable, user-facing capability status and its optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityStatusViewModel {
    pub kind: CapabilityKindViewModel,
    pub label: &'static str,
    pub detail: Option<String>,
}
/// Semantic capability state used by presentation layers without string matching.
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

/// Presentation state for one isolated configuration warning.
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
        FeatureLifecycleViewModel, FeatureViewModel, MonitorPresentation, MonitorViewModel,
    };
    use kestrel_core::{
        AlertKind, ApplicationConfiguration, CapabilityReport, CapabilityStatus, FeatureSpec,
        MonitorConfiguration, MonitorReadout, Permission,
    };
    use kestrel_platform::audio::{AudioServer, OutputDevice, PlaybackStream};
    use kestrel_platform::clipboard::{ClipboardEntryKind, ClipboardKindSupport};
    use kestrel_platform::quick_toggles::QuickToggleId;
    use kestrel_services::{
        ServiceRegistration,
        audio::{AudioAvailability, AudioPolicy, AudioReconcileOutcome, AudioSnapshot},
        clipboard::{ClipboardItemMetadata, ClipboardMatch, ClipboardPreview, ClipboardSnapshot},
        quick_toggles::{QuickToggleSnapshot, ToggleStateSource},
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
        // Settings rows follow the configured order first, then the hidden readouts, and
        // their arrows describe the list the move commands actually reorder.
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

        // The configuration keeps the user's intent; the policy carries the gate that
        // the engine enforces (for example the battery-alerts quick toggle).
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

        // `/proc/diskstats` contains "stat"; a substring match would show the disk
        // failure as the CPU explanation.
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
            ClipboardPresentation::default(),
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
        // The service snapshot is metadata-only: no query means no row content.
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
}
