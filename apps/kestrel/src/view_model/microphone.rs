//! Microphone input presentation.

use super::*;

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

#[cfg(test)]
mod tests {
    use super::*;

    use kestrel_platform::audio::InputDevice;

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
}
