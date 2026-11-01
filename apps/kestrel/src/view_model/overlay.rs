//! Status bar overlay presentation.

use std::time::{Duration, Instant};

use kestrel_platform::upower::{PeripheralCharge, PeripheralKind};

use super::*;

/// How long a control shows after its value changes.
pub const CONTROL_FEEDBACK: Duration = Duration::from_secs(3);

/// The active output's level and mute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeReading {
    pub output_id: u32,
    pub kind: OutputKind,
    pub percent: u8,
    pub muted: bool,
    pub max_percent: u8,
}

/// The default input's level and mute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicrophoneReading {
    pub input_id: u32,
    pub percent: u8,
    pub muted: bool,
}

/// What each control reads; a field stays empty while its backend cannot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlReadings {
    pub volume: Option<VolumeReading>,
    pub microphone: Option<MicrophoneReading>,
    pub keyboard_light: Option<u8>,
}

impl ControlReadings {
    pub fn read(
        audio: Option<&AudioViewModel>,
        microphone: Option<&MicrophoneViewModel>,
        keyboard_light: Option<&QuickToggleViewModel>,
    ) -> Self {
        let volume = audio.and_then(|audio| {
            if !audio.running {
                return None;
            }
            let output = audio
                .outputs
                .iter()
                .find(|output| Some(output.id) == audio.default_output_id)?;
            Some(VolumeReading {
                output_id: output.id,
                kind: output.kind,
                percent: output.volume_percent,
                muted: output.muted,
                max_percent: audio.boost_ceiling_percent.max(100),
            })
        });
        let microphone = microphone.and_then(|microphone| {
            if !microphone.running {
                return None;
            }
            let input = microphone
                .inputs
                .iter()
                .find(|input| Some(input.id) == microphone.default_input_id)?;
            Some(MicrophoneReading {
                input_id: input.id,
                percent: microphone.default_input_volume?,
                muted: input.muted,
            })
        });
        let keyboard_light = keyboard_light.and_then(|toggle| {
            if !toggle.available {
                return None;
            }
            match toggle.control.as_ref()? {
                QuickToggleControlViewModel::Level { percentage } => Some(*percentage),
                _ => None,
            }
        });
        Self {
            volume,
            microphone,
            keyboard_light,
        }
    }
}

/// One accessory's battery, with its kind and charge state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryReading {
    /// The hashed UPower identity, never a serial.
    pub id: String,
    pub kind: PeripheralKind,
    pub percent: u8,
    pub charging: bool,
}

impl AccessoryReading {
    /// The latest readings; `None` while they cannot be read.
    pub fn read(monitor: Option<&MonitorViewModel>) -> Option<Vec<AccessoryReading>> {
        let monitor = monitor?;
        if !monitor.running || monitor.accessories.unavailable.is_some() {
            return None;
        }
        Some(
            monitor
                .accessories
                .rows
                .iter()
                .filter_map(|row| {
                    Some(Self {
                        id: row.id.clone(),
                        kind: row.kind,
                        percent: row.percent?,
                        charging: row.charge == PeripheralCharge::Charging,
                    })
                })
                .collect(),
        )
    }
}

/// How urgent an accessory's battery reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessoryLevel {
    Normal,
    Warning,
    Critical,
}

/// The level a reading reads: a charging device never reads low.
pub fn accessory_level(
    percent: u8,
    charging: bool,
    configuration: &kestrel_core::OverlayConfiguration,
) -> AccessoryLevel {
    if charging {
        return AccessoryLevel::Normal;
    }
    if percent <= configuration.accessory_critical {
        AccessoryLevel::Critical
    } else if percent <= configuration.accessory_warning {
        AccessoryLevel::Warning
    } else {
        AccessoryLevel::Normal
    }
}

/// Shows a control for a while after its reading changes.
#[derive(Default)]
pub struct ControlFeedback {
    readings: ControlReadings,
    volume_deadline: Option<Instant>,
    microphone_deadline: Option<Instant>,
    keyboard_light_deadline: Option<Instant>,
    accessories: Vec<AccessoryReading>,
    accessory_deadlines: Vec<(String, Instant)>,
    accessories_seen: bool,
    held: bool,
}

impl ControlFeedback {
    /// Records new readings, showing each control whose value changed.
    pub fn observe(&mut self, now: Instant, readings: ControlReadings) {
        observe_change(
            self.readings.volume,
            readings.volume,
            now,
            &mut self.volume_deadline,
        );
        observe_change(
            self.readings.microphone,
            readings.microphone,
            now,
            &mut self.microphone_deadline,
        );
        observe_change(
            self.readings.keyboard_light,
            readings.keyboard_light,
            now,
            &mut self.keyboard_light_deadline,
        );
        self.readings = readings;
    }

    /// Records accessory readings, showing each new or newly charging one.
    pub fn observe_accessories(&mut self, now: Instant, readings: Option<Vec<AccessoryReading>>) {
        let Some(readings) = readings else {
            // A missing reading hides every accessory at once.
            self.accessories.clear();
            self.accessory_deadlines.clear();
            self.accessories_seen = false;
            return;
        };
        if !self.accessories_seen {
            // The first reading after a gap only sets the baseline.
            self.accessories = readings;
            self.accessories_seen = true;
            self.accessory_deadlines.clear();
            return;
        }
        let previous = std::mem::take(&mut self.accessories);
        let deadlines = std::mem::take(&mut self.accessory_deadlines);
        for reading in &readings {
            let deadline = match previous.iter().find(|previous| previous.id == reading.id) {
                Some(previous) if previous.charging != reading.charging => {
                    Some(now + CONTROL_FEEDBACK)
                }
                Some(_) => deadlines
                    .iter()
                    .find(|(id, _)| *id == reading.id)
                    .map(|(_, deadline)| *deadline),
                None => Some(now + CONTROL_FEEDBACK),
            };
            if let Some(deadline) = deadline {
                self.accessory_deadlines
                    .push((reading.id.clone(), deadline));
            }
        }
        self.accessories = readings;
    }

    /// Holds shown controls on screen while the pointer is over the overlay.
    pub fn set_held(&mut self, now: Instant, held: bool) {
        if self.held == held {
            return;
        }
        self.held = held;
        let slots = [
            &mut self.volume_deadline,
            &mut self.microphone_deadline,
            &mut self.keyboard_light_deadline,
        ];
        for slot in slots {
            if held {
                // Only controls on screen stay; expired ones are forgotten.
                *slot = slot.filter(|deadline| now < *deadline);
            } else if slot.is_some() {
                *slot = Some(now + CONTROL_FEEDBACK);
            }
        }
        if held {
            self.accessory_deadlines
                .retain(|(_, deadline)| now < *deadline);
        } else {
            for (_, deadline) in &mut self.accessory_deadlines {
                *deadline = now + CONTROL_FEEDBACK;
            }
        }
    }

    /// Whether a control held by the pointer never expires.
    pub fn is_held(&self) -> bool {
        self.held
    }

    /// Only the controls currently shown, with their latest readings.
    pub fn shown(&self, now: Instant) -> ControlReadings {
        let visible = |deadline: Option<Instant>| {
            deadline.is_some_and(|deadline| self.held || now < deadline)
        };
        ControlReadings {
            volume: self
                .readings
                .volume
                .filter(|_| visible(self.volume_deadline)),
            microphone: self
                .readings
                .microphone
                .filter(|_| visible(self.microphone_deadline)),
            keyboard_light: self
                .readings
                .keyboard_light
                .filter(|_| visible(self.keyboard_light_deadline)),
        }
    }

    /// The accessories a change still shows.
    pub fn shown_accessories(&self, now: Instant) -> Vec<AccessoryReading> {
        self.accessories
            .iter()
            .filter(|reading| {
                self.accessory_deadlines
                    .iter()
                    .any(|(id, deadline)| *id == reading.id && (self.held || now < *deadline))
            })
            .cloned()
            .collect()
    }

    /// The earliest deadline still ahead; no timer is needed while held.
    pub fn next_expiry(&self, now: Instant) -> Option<Instant> {
        if self.held {
            return None;
        }
        let controls = [
            self.readings.volume.map(|_| self.volume_deadline),
            self.readings.microphone.map(|_| self.microphone_deadline),
            self.readings
                .keyboard_light
                .map(|_| self.keyboard_light_deadline),
        ]
        .into_iter()
        .flatten()
        .flatten();
        let accessories = self
            .accessory_deadlines
            .iter()
            .map(|(_, deadline)| *deadline);
        controls
            .chain(accessories)
            .filter(|deadline| *deadline > now)
            .min()
    }
}

fn observe_change<T: PartialEq + Copy>(
    previous: Option<T>,
    current: Option<T>,
    now: Instant,
    deadline: &mut Option<Instant>,
) {
    match (previous, current) {
        // A changed value starts the countdown; a control already on screen
        // keeps its own.
        (Some(previous), Some(current)) if previous != current => {
            *deadline = Some(now + CONTROL_FEEDBACK);
        }
        (_, None) => *deadline = None,
        // The first reading after a gap is a baseline, not a change.
        _ => {}
    }
}

/// One thing the status overlay shows, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayItem {
    /// A recording runs; the clock keeps time between updates.
    Recording { paused: bool, clock: RecordingClock },
    /// An active monitoring alert; clicking it opens the Monitoring page.
    Alert { message: String },
    /// An accessory's battery, with its level and charge state.
    Accessory {
        id: String,
        kind: PeripheralKind,
        percent: u8,
        charging: bool,
        level: AccessoryLevel,
    },
    /// The output's type, level, and mute.
    Volume(VolumeReading),
    /// The default input's level and mute.
    MicrophoneLevel(MicrophoneReading),
    /// The keyboard light's level.
    KeyboardLight { percent: u8 },
    /// A timed keep-awake session and the time it has left.
    KeepAwake { remaining: String },
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
            _ => "Turn on Status bar overlay in the Feature Hub to use these.".to_owned(),
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
    /// Accessory batteries; `None` while they cannot be read.
    pub accessories: Option<&'a [AccessoryReading]>,
    /// Accessories a change still shows.
    pub shown_accessories: &'a [AccessoryReading],
    /// The controls currently shown.
    pub controls: ControlReadings,
    /// The keep-awake row, with its session while one runs.
    pub keep_awake: Option<&'a QuickToggleViewModel>,
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
    if let Some(remaining) = sources
        .keep_awake
        .and_then(|toggle| toggle.keep_awake.as_ref())
        .filter(|_| modules.keep_awake)
        .and_then(KeepAwakeControlsViewModel::remaining_short)
    {
        items.push(OverlayItem::KeepAwake { remaining });
    }
    if modules.accessories {
        for reading in sources.accessories.unwrap_or_default() {
            let level = accessory_level(reading.percent, reading.charging, &modules);
            let shown = level != AccessoryLevel::Normal
                || sources
                    .shown_accessories
                    .iter()
                    .any(|shown| shown.id == reading.id);
            if shown {
                items.push(OverlayItem::Accessory {
                    id: reading.id.clone(),
                    kind: reading.kind,
                    percent: reading.percent,
                    charging: reading.charging,
                    level,
                });
            }
        }
    }
    if let Some(volume) = sources.controls.volume.filter(|_| modules.volume) {
        items.push(OverlayItem::Volume(volume));
    }
    let microphone_level = sources
        .controls
        .microphone
        .filter(|_| modules.microphone_level);
    if let Some(microphone) = microphone_level {
        items.push(OverlayItem::MicrophoneLevel(microphone));
    }
    if let Some(percent) = sources
        .controls
        .keyboard_light
        .filter(|_| modules.keyboard_light)
    {
        items.push(OverlayItem::KeyboardLight { percent });
    }
    let all_muted = sources
        .microphone
        .is_some_and(|microphone| microphone.running && microphone.muted == Some(true));
    if modules.microphone && all_muted && microphone_level.is_none() {
        items.push(OverlayItem::MicrophoneMuted);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::{CapabilityReport, FeatureSpec, KeepAwakeDuration, OverlayConfiguration};

    fn overlay(status: CapabilityStatus, enabled: bool) -> OverlayViewModel {
        let spec = FeatureSpec::new(crate::OVERLAY_ID, "Status bar overlay", status.clone());
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

    fn output(id: u32, volume_percent: u8, muted: bool) -> AudioOutputViewModel {
        AudioOutputViewModel {
            id,
            title: format!("Output {id}"),
            name: format!("output-{id}"),
            detail: String::new(),
            is_default: true,
            volume_percent,
            muted,
            kind: OutputKind::Headphones,
        }
    }

    fn audio(default_output_id: Option<u32>, running: bool) -> AudioViewModel {
        AudioViewModel {
            running,
            status: String::new(),
            message: None,
            boost_ceiling_percent: 130,
            max_boost_percent: kestrel_core::MAX_AUDIO_BOOST_PERCENT,
            output_groups: Vec::new(),
            outputs: vec![output(3, 75, false)],
            streams: Vec::new(),
            default_output_id,
            inactive_streams: 0,
            policy: AudioPolicyViewModel {
                boost_percent: 130,
                max_boost_percent: kestrel_core::MAX_AUDIO_BOOST_PERCENT,
                move_all_streams: false,
                output_switch_label: "",
                reset_volume_on_disconnect: false,
                disconnect_policy_label: "",
                disconnect_volume_percent: 100,
                include_inactive_streams: false,
            },
            output_priority: DevicePriorityViewModel::default(),
        }
    }

    fn keyboard_light(available: bool, percentage: u8) -> QuickToggleViewModel {
        QuickToggleViewModel {
            id: crate::QuickToggleId::KeyboardLight,
            feature_id: crate::QuickToggleId::KeyboardLight.feature_id(),
            label: "Keyboard light",
            requirement: "",
            available,
            detail: String::new(),
            source: "",
            control: Some(QuickToggleControlViewModel::Level { percentage }),
            error: None,
            keep_awake: None,
            keep_awake_stamp: None,
        }
    }

    fn input(id: u32, muted: bool) -> MicrophoneInputViewModel {
        MicrophoneInputViewModel {
            id,
            label: format!("Input {id}"),
            muted,
            is_default: true,
        }
    }

    fn microphone_with_level(
        running: bool,
        input_id: Option<u32>,
        volume: Option<u8>,
        muted: bool,
    ) -> MicrophoneViewModel {
        MicrophoneViewModel {
            running,
            inputs: input_id
                .map(|id| vec![input(id, muted)])
                .unwrap_or_default(),
            default_input_id: input_id,
            default_input_volume: volume,
            ..microphone(Some(muted))
        }
    }

    fn readings(
        volume: Option<VolumeReading>,
        microphone_reading: Option<MicrophoneReading>,
        keyboard_light: Option<u8>,
    ) -> ControlReadings {
        ControlReadings {
            volume,
            microphone: microphone_reading,
            keyboard_light,
        }
    }

    fn sample_volume(percent: u8) -> VolumeReading {
        VolumeReading {
            output_id: 3,
            kind: OutputKind::Headphones,
            percent,
            muted: false,
            max_percent: 130,
        }
    }

    fn sample_microphone(percent: u8) -> MicrophoneReading {
        MicrophoneReading {
            input_id: 1,
            percent,
            muted: false,
        }
    }

    fn accessory(id: &str, kind: PeripheralKind, percent: u8, charging: bool) -> AccessoryReading {
        AccessoryReading {
            id: id.to_owned(),
            kind,
            percent,
            charging,
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
                    accessories: None,
                    shown_accessories: &[],
                    controls: ControlReadings::default(),
                    keep_awake: None,
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
                    accessories: None,
                    shown_accessories: &[],
                    controls: ControlReadings::default(),
                    keep_awake: None,
                }
            )
            .is_empty(),
            "no microphone state yet shows nothing"
        );

        let muted_but_shown = overlay_items(
            &running,
            &OverlaySources {
                recording: None,
                alerts: None,
                microphone: Some(&microphone(Some(true))),
                accessories: None,
                shown_accessories: &[],
                controls: readings(None, Some(sample_microphone(20)), None),
                keep_awake: None,
            },
        );
        assert_eq!(
            muted_but_shown,
            [OverlayItem::MicrophoneLevel(sample_microphone(20))],
            "the fader shows the mute state, so the chip stays out"
        );
        let mut level_off = running.clone();
        level_off.modules.microphone_level = false;
        assert_eq!(
            overlay_items(
                &level_off,
                &OverlaySources {
                    recording: None,
                    alerts: None,
                    microphone: Some(&microphone(Some(true))),
                    accessories: None,
                    shown_accessories: &[],
                    controls: readings(None, Some(sample_microphone(20)), None),
                    keep_awake: None,
                },
            ),
            [OverlayItem::MicrophoneMuted],
            "a hidden level module does not hide the chip"
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
        let battery = [
            accessory("mouse", PeripheralKind::Mouse, 15, false),
            accessory("keyboard", PeripheralKind::Keyboard, 8, false),
            accessory("headset", PeripheralKind::Headset, 5, true),
            accessory("phone", PeripheralKind::Phone, 80, false),
        ];
        let changed = [
            accessory("mouse", PeripheralKind::Mouse, 15, false),
            accessory("phone", PeripheralKind::Phone, 80, false),
        ];
        let controls = readings(
            Some(sample_volume(40)),
            Some(sample_microphone(20)),
            Some(70),
        );
        let shown = |overlay: &OverlayViewModel,
                     recording_controls: RecordingControls,
                     readings: ControlReadings| {
            overlay_items(
                overlay,
                &OverlaySources {
                    recording: Some(&recording(recording_controls)),
                    alerts: Some(&alerts),
                    microphone: Some(&microphone(Some(true))),
                    accessories: Some(&battery),
                    shown_accessories: &changed,
                    controls: readings,
                    keep_awake: None,
                },
            )
        };
        let playing = RecordingControls::Recording { paused: true };

        assert_eq!(
            shown(&running, playing, controls),
            [
                OverlayItem::Recording {
                    paused: true,
                    clock: RecordingClock::default()
                },
                OverlayItem::Alert {
                    message: "CPU is 95%".to_owned()
                },
                OverlayItem::Accessory {
                    id: "mouse".to_owned(),
                    kind: PeripheralKind::Mouse,
                    percent: 15,
                    charging: false,
                    level: AccessoryLevel::Warning,
                },
                OverlayItem::Accessory {
                    id: "keyboard".to_owned(),
                    kind: PeripheralKind::Keyboard,
                    percent: 8,
                    charging: false,
                    level: AccessoryLevel::Critical,
                },
                OverlayItem::Accessory {
                    id: "phone".to_owned(),
                    kind: PeripheralKind::Phone,
                    percent: 80,
                    charging: false,
                    level: AccessoryLevel::Normal,
                },
                OverlayItem::Volume(sample_volume(40)),
                OverlayItem::MicrophoneLevel(sample_microphone(20)),
                OverlayItem::KeyboardLight { percent: 70 },
            ],
            "the fader replaces the muted chip"
        );
        assert!(
            !shown(&running, playing, controls)
                .iter()
                .any(|item| matches!(item, OverlayItem::Accessory { id, .. } if id == "headset")),
            "a charging accessory never reads low"
        );
        for idle in [
            RecordingControls::Setup,
            RecordingControls::Waiting,
            RecordingControls::Stopping,
        ] {
            assert!(
                !shown(&running, idle, controls)
                    .iter()
                    .any(|item| matches!(item, OverlayItem::Recording { .. })),
                "{idle:?} has no running time to show"
            );
        }

        let mut modules_off = running.clone();
        modules_off.modules.recording = false;
        modules_off.modules.alerts = false;
        modules_off.modules.microphone = false;
        modules_off.modules.accessories = false;
        assert_eq!(
            shown(&modules_off, playing, controls),
            [
                OverlayItem::Volume(sample_volume(40)),
                OverlayItem::MicrophoneLevel(sample_microphone(20)),
                OverlayItem::KeyboardLight { percent: 70 },
            ],
            "each module hides on its own"
        );

        let mut controls_off = running.clone();
        controls_off.modules.volume = false;
        controls_off.modules.microphone_level = false;
        controls_off.modules.keyboard_light = false;
        assert_eq!(
            shown(&controls_off, playing, controls),
            [
                OverlayItem::Recording {
                    paused: true,
                    clock: RecordingClock::default()
                },
                OverlayItem::Alert {
                    message: "CPU is 95%".to_owned()
                },
                OverlayItem::Accessory {
                    id: "mouse".to_owned(),
                    kind: PeripheralKind::Mouse,
                    percent: 15,
                    charging: false,
                    level: AccessoryLevel::Warning,
                },
                OverlayItem::Accessory {
                    id: "keyboard".to_owned(),
                    kind: PeripheralKind::Keyboard,
                    percent: 8,
                    charging: false,
                    level: AccessoryLevel::Critical,
                },
                OverlayItem::Accessory {
                    id: "phone".to_owned(),
                    kind: PeripheralKind::Phone,
                    percent: 80,
                    charging: false,
                    level: AccessoryLevel::Normal,
                },
                OverlayItem::MicrophoneMuted,
            ],
            "each control needs its own module"
        );
    }

    #[test]
    fn a_keep_awake_item_shows_only_for_a_timed_session_while_its_module_is_on() {
        let running = overlay(CapabilityStatus::Supported, true);
        let toggle = |keep_awake| QuickToggleViewModel {
            id: crate::QuickToggleId::KeepAwake,
            feature_id: crate::QuickToggleId::KeepAwake.feature_id(),
            label: "Keep awake",
            requirement: "",
            available: true,
            detail: String::new(),
            source: "",
            control: None,
            error: None,
            keep_awake,
            keep_awake_stamp: None,
        };
        let session = |remaining, timed| KeepAwakeControlsViewModel {
            duration_choices: Vec::new(),
            selected: KeepAwakeDuration::For(Duration::from_secs(42 * 60)),
            session_text: Some("On for 42 minutes more".to_owned()),
            timed,
            remaining,
        };
        let shown = |overlay: &OverlayViewModel, toggle: &QuickToggleViewModel| {
            overlay_items(
                overlay,
                &OverlaySources {
                    recording: None,
                    alerts: None,
                    microphone: None,
                    accessories: None,
                    shown_accessories: &[],
                    controls: ControlReadings::default(),
                    keep_awake: Some(toggle),
                },
            )
        };
        let timed = toggle(Some(session(Some(Duration::from_secs(42 * 60)), true)));
        assert!(
            shown(&running, &timed)
                .iter()
                .any(|item| matches!(item, OverlayItem::KeepAwake { .. })),
            "a timed session shows its time left"
        );

        let mut module_off = running.clone();
        module_off.modules.keep_awake = false;
        assert!(shown(&module_off, &timed).is_empty());

        let indefinite = toggle(Some(session(None, false)));
        assert!(
            shown(&running, &indefinite).is_empty(),
            "an indefinite session has no time left to show"
        );
        assert!(
            shown(&running, &toggle(None)).is_empty(),
            "no session shows nothing"
        );
    }

    #[test]
    fn control_feedback_shows_a_change_for_a_while_and_never_the_first_reading() {
        let started = Instant::now();
        let mut feedback = ControlFeedback::default();
        let first = readings(Some(sample_volume(40)), None, None);

        feedback.observe(started, first);
        assert_eq!(
            feedback.shown(started),
            ControlReadings::default(),
            "the first reading is a baseline"
        );
        assert_eq!(feedback.next_expiry(started), None);

        let changed = started + Duration::from_secs(1);
        feedback.observe(changed, readings(Some(sample_volume(50)), None, None));
        assert_eq!(
            feedback.shown(changed).volume,
            Some(sample_volume(50)),
            "a change shows the control"
        );
        let expiry = changed + CONTROL_FEEDBACK;
        assert_eq!(feedback.next_expiry(changed), Some(expiry));
        assert!(
            feedback
                .shown(expiry - Duration::from_millis(1))
                .volume
                .is_some(),
            "the control stays until its deadline"
        );
        assert_eq!(feedback.shown(expiry).volume, None, "then it hides");

        let held_at = changed + Duration::from_secs(1);
        feedback.set_held(held_at, true);
        assert_eq!(feedback.next_expiry(held_at), None);
        assert_eq!(
            feedback.shown(expiry + Duration::from_secs(10)).volume,
            Some(sample_volume(50)),
            "holding keeps it on screen"
        );
        let released = expiry + Duration::from_secs(20);
        feedback.set_held(released, false);
        assert_eq!(
            feedback.next_expiry(released),
            Some(released + CONTROL_FEEDBACK),
            "release restarts the countdown"
        );
        assert!(
            feedback
                .shown(released + CONTROL_FEEDBACK - Duration::from_millis(1))
                .volume
                .is_some()
        );
        let later = released + CONTROL_FEEDBACK;
        feedback.set_held(later, true);
        assert_eq!(
            feedback.shown(later).volume,
            None,
            "holding never brings back a hidden control"
        );
        feedback.set_held(later, false);
        assert_eq!(feedback.next_expiry(later), None);

        feedback.observe(released, ControlReadings::default());
        assert_eq!(
            feedback.shown(released),
            ControlReadings::default(),
            "a missing reading hides at once"
        );
        assert_eq!(feedback.next_expiry(released), None);

        feedback.observe(
            released + Duration::from_secs(1),
            readings(None, None, Some(30)),
        );
        assert_eq!(
            feedback.shown(released + Duration::from_secs(1)),
            ControlReadings::default(),
            "the first keyboard light reading never shows"
        );
        // Read gating: a control needs a running backend and a known default.
        assert_eq!(
            ControlReadings::read(None, None, None),
            ControlReadings::default()
        );

        assert_eq!(
            ControlReadings::read(
                Some(&audio(Some(3), false)),
                Some(&microphone_with_level(false, Some(1), Some(50), false)),
                Some(&keyboard_light(false, 60)),
            ),
            ControlReadings::default(),
            "a stopped mixer and an unavailable toggle expose nothing"
        );

        assert_eq!(
            ControlReadings::read(Some(&audio(None, true)), None, None).volume,
            None,
            "no default output, no control"
        );
        assert_eq!(
            ControlReadings::read(Some(&audio(Some(9), true)), None, None).volume,
            None,
            "the default must be one of the outputs"
        );
        let volume = ControlReadings::read(Some(&audio(Some(3), true)), None, None).volume;
        assert_eq!(
            volume,
            Some(VolumeReading {
                output_id: 3,
                kind: OutputKind::Headphones,
                percent: 75,
                muted: false,
                max_percent: 130,
            })
        );

        let microphone_reading = ControlReadings::read(
            None,
            Some(&microphone_with_level(true, Some(1), Some(50), true)),
            None,
        )
        .microphone;
        assert_eq!(
            microphone_reading,
            Some(MicrophoneReading {
                input_id: 1,
                percent: 50,
                muted: true,
            })
        );
        assert_eq!(
            ControlReadings::read(
                None,
                Some(&microphone_with_level(true, Some(1), None, false)),
                None
            )
            .microphone,
            None,
            "no level reading, no fader"
        );
        assert_eq!(
            ControlReadings::read(
                None,
                Some(&microphone_with_level(true, None, Some(50), false)),
                None
            )
            .microphone,
            None,
            "no default input, no fader"
        );

        assert_eq!(
            ControlReadings::read(None, None, Some(&keyboard_light(true, 60))).keyboard_light,
            Some(60)
        );

        // Accessory batteries: a baseline, then connects, charge changes, and
        // removals, with the pointer hold covering them too.
        let battery = |entries: &[(&str, bool)]| {
            entries
                .iter()
                .map(|(id, charging)| accessory(id, PeripheralKind::Mouse, 20, *charging))
                .collect::<Vec<_>>()
        };
        let ids = |shown: Vec<AccessoryReading>| {
            shown
                .into_iter()
                .map(|reading| reading.id)
                .collect::<Vec<_>>()
        };
        let mut feedback = ControlFeedback::default();
        let baseline = released + Duration::from_secs(2);
        feedback.observe_accessories(baseline, Some(battery(&[("a", false)])));
        assert!(
            feedback.shown_accessories(baseline).is_empty(),
            "the first accessory reading is a baseline"
        );
        assert_eq!(feedback.next_expiry(baseline), None);

        let connected = baseline + Duration::from_secs(1);
        feedback.observe_accessories(connected, Some(battery(&[("a", false), ("b", false)])));
        assert_eq!(ids(feedback.shown_accessories(connected)), ["b"]);
        assert_eq!(
            feedback.next_expiry(connected),
            Some(connected + CONTROL_FEEDBACK)
        );
        assert!(
            feedback
                .shown_accessories(connected + CONTROL_FEEDBACK)
                .is_empty()
        );

        let flipped = connected + CONTROL_FEEDBACK + Duration::from_secs(1);
        feedback.observe_accessories(flipped, Some(battery(&[("a", true), ("b", false)])));
        assert_eq!(ids(feedback.shown_accessories(flipped)), ["a"]);

        let removed = flipped + Duration::from_secs(1);
        feedback.observe_accessories(removed, Some(battery(&[("b", false)])));
        assert!(
            feedback.shown_accessories(removed).is_empty(),
            "a vanished accessory is forgotten"
        );
        assert_eq!(feedback.next_expiry(removed), None);

        let hold = removed + Duration::from_secs(1);
        feedback.observe_accessories(hold, Some(battery(&[("b", true), ("a", false)])));
        assert_eq!(ids(feedback.shown_accessories(hold)), ["b", "a"]);
        feedback.set_held(hold, true);
        assert_eq!(feedback.next_expiry(hold), None);
        assert_eq!(
            ids(feedback.shown_accessories(hold + Duration::from_secs(60))),
            ["b", "a"],
            "holding keeps shown accessories on screen"
        );
        let freed = hold + Duration::from_secs(60);
        feedback.set_held(freed, false);
        assert_eq!(feedback.next_expiry(freed), Some(freed + CONTROL_FEEDBACK));
        feedback.set_held(freed + CONTROL_FEEDBACK + Duration::from_secs(1), true);
        assert!(
            feedback
                .shown_accessories(freed + CONTROL_FEEDBACK + Duration::from_secs(1))
                .is_empty(),
            "holding never brings back a hidden accessory"
        );
        feedback.set_held(freed + CONTROL_FEEDBACK + Duration::from_secs(1), false);

        feedback.observe_accessories(freed, None);
        assert!(
            feedback.shown_accessories(freed).is_empty(),
            "a missing reading hides every accessory"
        );
        assert_eq!(feedback.next_expiry(freed), None);

        // Read gating: the monitor must run and UPower must answer.
        let row = |percent| AccessoryViewModel {
            id: "a".to_owned(),
            kind: PeripheralKind::Mouse,
            title: String::new(),
            detail: String::new(),
            percent,
            charge: PeripheralCharge::Discharging,
        };
        let monitor = |running, rows, unavailable: Option<&str>| MonitorViewModel {
            running,
            status: String::new(),
            readouts: Vec::new(),
            readout_settings: Vec::new(),
            alerts: Vec::new(),
            delivery_failures: Vec::new(),
            alert_rules: Vec::new(),
            cores: None,
            accessories: AccessoriesViewModel {
                rows,
                unavailable: unavailable.map(str::to_owned),
            },
            local_addresses: None,
            network_rate_unit: kestrel_core::NetworkRateUnit::Bytes,
            per_core_cpu: false,
            tray: Vec::new(),
        };
        assert_eq!(AccessoryReading::read(None), None);
        assert_eq!(
            AccessoryReading::read(Some(&monitor(false, vec![row(Some(20))], None))),
            None,
            "a stopped monitor reads nothing"
        );
        assert_eq!(
            AccessoryReading::read(Some(&monitor(true, vec![row(Some(20))], Some("no UPower")))),
            None,
            "a provider issue reads nothing"
        );
        assert_eq!(
            AccessoryReading::read(Some(&monitor(true, vec![row(None)], None))),
            Some(Vec::new()),
            "an unknown percentage has no reading"
        );
        assert_eq!(
            AccessoryReading::read(Some(&monitor(true, vec![row(Some(20))], None))),
            Some(vec![accessory("a", PeripheralKind::Mouse, 20, false)])
        );
    }

    #[test]
    fn accessory_levels_follow_the_configured_thresholds() {
        use AccessoryLevel::{Critical, Normal, Warning};
        let configuration = OverlayConfiguration {
            accessory_warning: 30,
            accessory_critical: 15,
            ..OverlayConfiguration::default()
        };
        for (percent, charging, expected) in [
            (0, false, Critical),
            (15, false, Critical),
            (16, false, Warning),
            (30, false, Warning),
            (31, false, Normal),
            (5, false, Critical),
            (5, true, Normal),
            (80, true, Normal),
        ] {
            assert_eq!(
                accessory_level(percent, charging, &configuration),
                expected,
                "{percent}% charging={charging}"
            );
        }
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
                .starts_with("Turn on Status bar overlay")
        );
        assert!(
            overlay(CapabilityStatus::Supported, true)
                .status
                .starts_with("Appears at the top")
        );
    }
}
