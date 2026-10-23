//! Status overlay presentation.

use super::*;

/// One thing the status overlay shows, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayItem {
    /// A recording runs; the clock keeps time between updates.
    Recording { paused: bool, clock: RecordingClock },
    /// An active monitoring alert; clicking it opens the Monitoring page.
    Alert { message: String },
    /// Every microphone input is muted; clicking it unmutes them.
    MicrophoneMuted,
}

/// The status overlay's state and module switches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayViewModel {
    pub running: bool,
    pub available: bool,
    /// What the Settings group says about the overlay.
    pub status: String,
    pub modules: kestrel_core::OverlayConfiguration,
}

impl OverlayViewModel {
    pub(crate) fn new(
        registration: Option<&ServiceRegistration>,
        modules: kestrel_core::OverlayConfiguration,
    ) -> Self {
        let running = registration.is_some_and(|registration| registration.running);
        let available = registration.is_some_and(|registration| registration.available);
        let status = match registration {
            Some(registration) if !registration.available => {
                let reason = match &registration.capability.status {
                    CapabilityStatus::Unsupported { reason }
                    | CapabilityStatus::Limited { reason } => reason.as_str(),
                    _ => registration.capability.summary.as_str(),
                };
                format!("Not available here: {reason}.")
            }
            Some(registration) if registration.running => {
                "Appears at the top of the screen while one of these is active.".to_owned()
            }
            _ => "Turn on Status overlay in the Feature Hub to use these.".to_owned(),
        };
        Self {
            running,
            available,
            status,
            modules,
        }
    }
}

/// The latest state the overlay's modules read.
pub struct OverlaySources<'a> {
    pub recording: Option<&'a RecordingViewModel>,
    pub alerts: Option<&'a [ActiveAlertViewModel]>,
    pub microphone: Option<&'a MicrophoneViewModel>,
}

/// What the overlay shows, in order; empty while it is off, so it hides.
pub fn overlay_items(overlay: &OverlayViewModel, sources: &OverlaySources<'_>) -> Vec<OverlayItem> {
    let mut items = Vec::new();
    if !overlay.running {
        return items;
    }
    let modules = overlay.modules;
    if let Some(recording) = sources.recording.filter(|_| modules.recording) {
        if let RecordingControls::Recording { paused } = recording.controls {
            items.push(OverlayItem::Recording {
                paused,
                clock: recording.clock.unwrap_or_default(),
            });
        }
    }
    if modules.alerts {
        items.extend(
            sources
                .alerts
                .unwrap_or_default()
                .iter()
                .map(|alert| OverlayItem::Alert {
                    message: alert.message.clone(),
                }),
        );
    }
    let all_muted = sources
        .microphone
        .is_some_and(|microphone| microphone.running && microphone.muted == Some(true));
    if modules.microphone && all_muted {
        items.push(OverlayItem::MicrophoneMuted);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::{CapabilityReport, FeatureSpec, OverlayConfiguration};

    fn overlay(status: CapabilityStatus, enabled: bool) -> OverlayViewModel {
        let spec = FeatureSpec::new(crate::OVERLAY_ID, "Status overlay", status.clone());
        let report = CapabilityReport::new(crate::OVERLAY_ID, status, "Overlay.");
        let mut registration =
            ServiceRegistration::new(spec, report, enabled).expect("matching feature IDs");
        registration.running = enabled && registration.available;
        OverlayViewModel::new(Some(&registration), OverlayConfiguration::default())
    }

    fn microphone(muted: Option<bool>) -> MicrophoneViewModel {
        MicrophoneViewModel {
            running: true,
            status: String::new(),
            mute_label: String::new(),
            muted,
            mixed: muted.is_none(),
            inputs: Vec::new(),
            default_input_id: None,
            default_input_volume: None,
            capture_streams: Vec::new(),
            input_priority: DevicePriorityViewModel::default(),
            message: None,
        }
    }

    #[test]
    fn the_overlay_shows_a_muted_microphone_only_while_it_runs_and_everything_is_muted() {
        let shown = |overlay: &OverlayViewModel, muted| {
            overlay_items(
                overlay,
                &OverlaySources {
                    recording: None,
                    alerts: None,
                    microphone: Some(&microphone(muted)),
                },
            )
        };
        let running = overlay(CapabilityStatus::Supported, true);
        assert_eq!(shown(&running, Some(true)), [OverlayItem::MicrophoneMuted]);
        assert!(
            shown(&running, Some(false)).is_empty(),
            "unmuted shows nothing"
        );
        assert!(
            shown(&running, None).is_empty(),
            "mixed mute states are not all muted"
        );

        let mut module_off = running.clone();
        module_off.modules.microphone = false;
        assert!(shown(&module_off, Some(true)).is_empty());
        assert!(shown(&overlay(CapabilityStatus::Supported, false), Some(true)).is_empty());
        assert!(
            overlay_items(
                &running,
                &OverlaySources {
                    recording: None,
                    alerts: None,
                    microphone: None,
                }
            )
            .is_empty(),
            "no microphone state yet shows nothing"
        );
    }

    #[test]
    fn recordings_and_alerts_show_in_order_while_active_and_their_module_is_on() {
        let running = overlay(CapabilityStatus::Supported, true);
        let recording = |controls| RecordingViewModel {
            running: true,
            controls,
            ..RecordingViewModel::default()
        };
        let alerts = [ActiveAlertViewModel {
            label: "CPU",
            message: "CPU is 95%".to_owned(),
        }];
        let shown = |overlay: &OverlayViewModel, controls| {
            overlay_items(
                overlay,
                &OverlaySources {
                    recording: Some(&recording(controls)),
                    alerts: Some(&alerts),
                    microphone: Some(&microphone(Some(true))),
                },
            )
        };

        assert_eq!(
            shown(&running, RecordingControls::Recording { paused: true }),
            [
                OverlayItem::Recording {
                    paused: true,
                    clock: RecordingClock::default()
                },
                OverlayItem::Alert {
                    message: "CPU is 95%".to_owned()
                },
                OverlayItem::MicrophoneMuted,
            ]
        );
        for idle in [
            RecordingControls::Setup,
            RecordingControls::Waiting,
            RecordingControls::Stopping,
        ] {
            assert!(
                !shown(&running, idle)
                    .iter()
                    .any(|item| matches!(item, OverlayItem::Recording { .. })),
                "{idle:?} has no running time to show"
            );
        }

        let mut modules_off = running.clone();
        modules_off.modules.recording = false;
        modules_off.modules.alerts = false;
        assert_eq!(
            shown(&modules_off, RecordingControls::Recording { paused: false }),
            [OverlayItem::MicrophoneMuted]
        );
    }

    #[test]
    fn the_settings_line_explains_unavailable_off_and_on() {
        let unavailable = overlay(
            CapabilityStatus::Unsupported {
                reason: "the compositor does not advertise wlr-layer-shell".to_owned(),
            },
            true,
        );
        assert!(!unavailable.running && !unavailable.available);
        assert_eq!(
            unavailable.status,
            "Not available here: the compositor does not advertise wlr-layer-shell."
        );
        assert!(
            overlay(CapabilityStatus::Supported, false)
                .status
                .starts_with("Turn on Status overlay")
        );
        assert!(
            overlay(CapabilityStatus::Supported, true)
                .status
                .starts_with("Appears at the top")
        );
    }
}
