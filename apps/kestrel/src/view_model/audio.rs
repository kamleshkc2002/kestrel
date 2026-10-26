//! Audio mixer and output presentation.

use super::*;
use kestrel_platform::audio::OutputKind;

pub(crate) struct AudioPresentation<'a> {
    pub snapshot: Option<&'a AudioSnapshot>,
    pub running: bool,
    pub policy: AudioPolicy,
}

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
    pub(super) fn new<'a>(
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
    pub kind: OutputKind,
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
                kind: output.kind,
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
                        kind: output.kind,
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

/// Symbolic icon representing an output class.
pub fn output_icon(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Speakers => "audio-speakers-symbolic",
        OutputKind::Headphones => "audio-headphones-symbolic",
        OutputKind::Display => "video-display-symbolic",
        OutputKind::Bluetooth => "bluetooth-active-symbolic",
        OutputKind::Unknown => "audio-card-symbolic",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::test_support::*;

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
                stopped_reason: None,
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
                stopped_reason: None,
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
            audio.output_groups[0].outputs[0].kind,
            OutputKind::Headphones
        );
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
                stopped_reason: None,
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
        assert!(audio.streams[1].detail.contains("Inactive"));
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
                stopped_reason: None,
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
}
