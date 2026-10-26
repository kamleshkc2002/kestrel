//! The status overlay: a small pill at the top centre of the screen that
//! appears while one of its modules has something active.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, glib};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, AudioCommand, AudioViewModel, FocusTarget,
    MicrophoneCommand, MicrophoneViewModel, QuickToggleCommand, QuickToggleId, QuickToggleMutation,
    QuickToggleViewModel, RecordingRequest, RecordingViewModel, output_icon,
    view_model::{
        ActiveAlertViewModel, ControlFeedback, ControlReadings, MicrophoneReading, OverlayItem,
        OverlaySources, OverlayViewModel, VolumeReading, format_clock, overlay_items,
    },
};
use kestrel_services::recording::RecordingClock;

use super::widgets::on_spin_button_settled;
use crate::layer_shell;

const NAMESPACE: &str = "kestrel-status";
/// The window stays transparent around the rounded pill.
const STYLE: &str = "window.kestrel-status-window { background: none; } \
                     .kestrel-status { border-radius: 999px; padding: 4px 10px; }";
/// How often a running recording's time is redrawn.
const CLOCK_TICK: Duration = Duration::from_millis(500);
/// A slider sends once it stops moving, so a drag is one operation.
const SLIDER_SETTLE: Duration = Duration::from_millis(150);
/// The widest control keeps the pill from growing with the screen.
const SLIDER_WIDTH: i32 = 160;

/// The latest state each module reads; updates arrive separately.
#[derive(Default)]
struct OverlayState {
    overlay: Option<OverlayViewModel>,
    recording: Option<RecordingViewModel>,
    alerts: Option<Vec<ActiveAlertViewModel>>,
    microphone: Option<MicrophoneViewModel>,
    audio: Option<AudioViewModel>,
    keyboard_light: Option<QuickToggleViewModel>,
    feedback: ControlFeedback,
    layout: Vec<ItemLayout>,
    controls: Controls,
    /// Only while something is shown; dropped when empty, so a hidden
    /// overlay holds no surface.
    window: Option<gtk::Window>,
    /// The style is added to the display once, not per window.
    styled: bool,
    /// Supersedes hide timers that have not fired yet.
    generation: u64,
}

/// Owned by the controller, so it works with the main window closed.
pub struct StatusOverlay {
    commands: Sender<ApplicationCommand>,
    state: Rc<RefCell<OverlayState>>,
}

impl StatusOverlay {
    pub fn new(commands: Sender<ApplicationCommand>) -> Self {
        Self {
            commands,
            state: Rc::default(),
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        {
            let mut state = self.state.borrow_mut();
            state.overlay = Some(view_model.overlay.clone());
            state.recording = Some(view_model.recording.clone());
            state.alerts = Some(view_model.monitor.alerts.clone());
            state.microphone = Some(view_model.microphone.clone());
            state.audio = Some(view_model.audio.clone());
            state.keyboard_light = view_model
                .quick_toggles
                .iter()
                .find(|toggle| toggle.id == QuickToggleId::KeyboardLight)
                .cloned();
        }
        self.observe();
    }

    pub fn set_recording(&self, recording: &RecordingViewModel) {
        self.state.borrow_mut().recording = Some(recording.clone());
        self.observe();
    }

    pub fn set_alerts(&self, alerts: &[ActiveAlertViewModel]) {
        self.state.borrow_mut().alerts = Some(alerts.to_vec());
        self.observe();
    }

    pub fn set_microphone(&self, microphone: &MicrophoneViewModel) {
        self.state.borrow_mut().microphone = Some(microphone.clone());
        self.observe();
    }

    pub fn set_audio(&self, audio: &AudioViewModel) {
        self.state.borrow_mut().audio = Some(audio.clone());
        self.observe();
    }

    pub fn set_keyboard_light(&self, keyboard_light: &QuickToggleViewModel) {
        self.state.borrow_mut().keyboard_light = Some(keyboard_light.clone());
        self.observe();
    }

    /// Takes the latest readings, then shows whatever just changed.
    fn observe(&self) {
        {
            let mut state = self.state.borrow_mut();
            let readings = ControlReadings::read(
                state.audio.as_ref(),
                state.microphone.as_ref(),
                state.keyboard_light.as_ref(),
            );
            state.feedback.observe(Instant::now(), readings);
        }
        render(&self.state, &self.commands);
    }
}

/// A control set that can be matched against the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemLayout {
    Recording { paused: bool },
    Alert { message: String },
    Volume { output_id: u32 },
    MicrophoneLevel { input_id: u32 },
    KeyboardLight,
    MicrophoneMuted,
}

impl ItemLayout {
    fn of(item: &OverlayItem) -> Self {
        match item {
            OverlayItem::Recording { paused, .. } => Self::Recording { paused: *paused },
            OverlayItem::Alert { message } => Self::Alert {
                message: message.clone(),
            },
            OverlayItem::Volume(reading) => Self::Volume {
                output_id: reading.output_id,
            },
            OverlayItem::MicrophoneLevel(reading) => Self::MicrophoneLevel {
                input_id: reading.input_id,
            },
            OverlayItem::KeyboardLight { .. } => Self::KeyboardLight,
            OverlayItem::MicrophoneMuted => Self::MicrophoneMuted,
        }
    }
}

fn render(state: &Rc<RefCell<OverlayState>>, commands: &Sender<ApplicationCommand>) {
    let now = Instant::now();
    let items = {
        let state = state.borrow();
        match state.overlay.as_ref() {
            Some(overlay) => overlay_items(
                overlay,
                &OverlaySources {
                    recording: state.recording.as_ref(),
                    alerts: state.alerts.as_deref(),
                    microphone: state.microphone.as_ref(),
                    controls: state.feedback.shown(now),
                },
            ),
            None => Vec::new(),
        }
    };
    let weak = Rc::downgrade(state);
    let (expiry, generation) = {
        let mut state = state.borrow_mut();
        let expiry = if items.is_empty() {
            // Nothing can hold a pointer over a hidden window.
            state.feedback.set_held(now, false);
            if let Some(window) = state.window.take() {
                window.destroy();
            }
            state.layout.clear();
            state.controls = Controls::default();
            None
        } else {
            let mut show = state.window.is_none();
            if show {
                let window = new_window();
                if !state.styled {
                    let provider = gtk::CssProvider::new();
                    provider.load_from_data(STYLE);
                    gtk::style_context_add_provider_for_display(
                        &gtk::prelude::WidgetExt::display(&window),
                        &provider,
                        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                    );
                    state.styled = true;
                }
                state.window = Some(window);
            }
            let layout = items.iter().map(ItemLayout::of).collect::<Vec<_>>();
            if layout == state.layout {
                let held = state.feedback.is_held();
                update_controls(&state.controls, &items, held);
            } else {
                let (pill, controls) = build_pill(&items, commands, &weak);
                if let Some(window) = state.window.as_ref() {
                    window.set_child(Some(&pill));
                }
                state.layout = layout;
                state.controls = controls;
                show = true;
            }
            if let (true, Some(window)) = (show, state.window.as_ref()) {
                window.present();
            }
            state.feedback.next_expiry(now)
        };
        state.generation = state.generation.wrapping_add(1);
        (expiry, state.generation)
    };
    let Some(expiry) = expiry else {
        return;
    };
    let delay = expiry.saturating_duration_since(Instant::now());
    let commands = commands.clone();
    glib::timeout_add_local_once(delay, move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        if state.borrow().generation == generation {
            render(&state, &commands);
        }
    });
}

/// Builds the pill for a new item layout.
fn build_pill(
    items: &[OverlayItem],
    commands: &Sender<ApplicationCommand>,
    state: &Weak<RefCell<OverlayState>>,
) -> (gtk::Box, Controls) {
    let pill = gtk::Box::new(Orientation::Horizontal, 8);
    pill.add_css_class("osd");
    pill.add_css_class("kestrel-status");
    let mut controls = Controls::default();
    for item in items {
        match item {
            OverlayItem::Recording { paused, clock } => {
                pill.append(&recording_item(*paused, *clock, commands));
            }
            OverlayItem::Alert { message } => pill.append(&alert_item(message, commands)),
            OverlayItem::Volume(reading) => {
                let volume = VolumeControls::new(*reading, commands);
                pill.append(&volume.container);
                controls.volume = Some(volume);
            }
            OverlayItem::MicrophoneLevel(reading) => {
                let microphone = MicrophoneControls::new(*reading, commands);
                pill.append(&microphone.container);
                controls.microphone = Some(microphone);
            }
            OverlayItem::KeyboardLight { percent } => {
                let keyboard_light = KeyboardLightControls::new(*percent, commands);
                pill.append(&keyboard_light.container);
                controls.keyboard_light = Some(keyboard_light);
            }
            OverlayItem::MicrophoneMuted => pill.append(&microphone_item(commands)),
        }
    }
    let motion = gtk::EventControllerMotion::new();
    {
        let state = state.clone();
        let commands = commands.clone();
        motion.connect_enter(move |_, _, _| {
            if let Some(state) = state.upgrade() {
                set_held(&state, &commands, true);
            }
        });
    }
    {
        let state = state.clone();
        let commands = commands.clone();
        motion.connect_leave(move |_| {
            if let Some(state) = state.upgrade() {
                set_held(&state, &commands, false);
            }
        });
    }
    pill.add_controller(motion);
    (pill, controls)
}

fn set_held(state: &Rc<RefCell<OverlayState>>, commands: &Sender<ApplicationCommand>, held: bool) {
    state.borrow_mut().feedback.set_held(Instant::now(), held);
    render(state, commands);
}

#[derive(Default)]
struct Controls {
    volume: Option<VolumeControls>,
    microphone: Option<MicrophoneControls>,
    keyboard_light: Option<KeyboardLightControls>,
}

fn update_controls(controls: &Controls, items: &[OverlayItem], held: bool) {
    for item in items {
        match item {
            OverlayItem::Volume(reading) => {
                if let Some(control) = &controls.volume {
                    control.update(*reading, held);
                }
            }
            OverlayItem::MicrophoneLevel(reading) => {
                if let Some(control) = &controls.microphone {
                    control.update(*reading, held);
                }
            }
            OverlayItem::KeyboardLight { percent } => {
                if let Some(control) = &controls.keyboard_light {
                    control.update(*percent, held);
                }
            }
            _ => {}
        }
    }
}

struct VolumeControls {
    container: gtk::Box,
    icon: gtk::Image,
    slider: LevelSlider,
    mute: MuteControl,
}

impl VolumeControls {
    fn new(reading: VolumeReading, commands: &Sender<ApplicationCommand>) -> Self {
        let container = gtk::Box::new(Orientation::Horizontal, 6);
        let icon = gtk::Image::from_icon_name(output_icon(reading.kind));
        icon.set_valign(Align::Center);
        icon.update_property(&[Property::Label("Output")]);
        container.append(&icon);
        let output_id = reading.output_id;
        let slider = LevelSlider::new(
            reading.max_percent,
            reading.percent,
            &format!("Output volume, up to {} percent", reading.max_percent),
            commands,
            Rc::new(move |volume_percent| {
                ApplicationCommand::Audio(AudioCommand::SetOutputVolume {
                    output_id,
                    volume_percent,
                })
            }),
        );
        container.append(&slider.scale);
        let mute = MuteControl::new(
            reading.muted,
            ("audio-volume-high-symbolic", "audio-volume-muted-symbolic"),
            ("Mute output", "Unmute output"),
            commands,
            Rc::new(move |muted| {
                ApplicationCommand::Audio(AudioCommand::SetOutputMute { output_id, muted })
            }),
        );
        container.append(&mute.button);
        Self {
            container,
            icon,
            slider,
            mute,
        }
    }

    fn update(&self, reading: VolumeReading, held: bool) {
        self.icon.set_icon_name(Some(output_icon(reading.kind)));
        self.mute.update(reading.muted);
        self.slider.set_max(reading.max_percent);
        if !held {
            self.slider.set_value(reading.percent);
        }
    }
}

struct MicrophoneControls {
    container: gtk::Box,
    icon: gtk::Image,
    scale: gtk::Scale,
    spin: gtk::SpinButton,
    mute: MuteControl,
    /// A render writes both widgets.
    updating: Rc<Cell<bool>>,
    /// A programmatic spin write armed the settle timer.
    spin_gate: Rc<Cell<bool>>,
    scale_generation: Rc<Cell<u64>>,
    /// The scale's debounce has not fired yet.
    pending: Rc<Cell<bool>>,
    /// The spin's settle timer has not fired yet.
    spin_pending: Rc<Cell<bool>>,
}

impl MicrophoneControls {
    fn new(reading: MicrophoneReading, commands: &Sender<ApplicationCommand>) -> Self {
        let container = gtk::Box::new(Orientation::Horizontal, 6);
        let icon = gtk::Image::from_icon_name(microphone_icon(reading.muted));
        icon.set_valign(Align::Center);
        icon.update_property(&[Property::Label("Microphone")]);
        container.append(&icon);

        let scale = gtk::Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
        configure_scale(
            &scale,
            reading.percent,
            "Microphone level, 0 to 100 percent",
        );
        container.append(&scale);

        let spin = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
        spin.set_value(f64::from(reading.percent));
        spin.set_numeric(true);
        spin.set_width_chars(3);
        spin.set_valign(Align::Center);
        let spin_label = "Microphone level percentage";
        spin.update_property(&[Property::Label(spin_label)]);
        spin.set_tooltip_text(Some(spin_label));
        container.append(&spin);

        let updating = Rc::new(Cell::new(false));
        let scale_updating = Rc::new(Cell::new(false));
        let spin_updating = Rc::new(Cell::new(false));
        let spin_gate = Rc::new(Cell::new(false));
        let scale_generation = Rc::new(Cell::new(0u64));
        let pending = Rc::new(Cell::new(false));
        let spin_pending = Rc::new(Cell::new(false));
        let input_id = reading.input_id;

        {
            let updating = Rc::clone(&updating);
            let scale_updating = Rc::clone(&scale_updating);
            let spin_updating = Rc::clone(&spin_updating);
            let spin_gate = Rc::clone(&spin_gate);
            let scale_generation = Rc::clone(&scale_generation);
            let pending = Rc::clone(&pending);
            let spin = spin.clone();
            let commands = commands.clone();
            scale.connect_value_changed(move |scale| {
                if updating.get() || scale_updating.get() {
                    return;
                }
                let value = level_of(scale.value());
                // Mirror the level without the spin sending its own command.
                spin_updating.set(true);
                spin_gate.set(true);
                if level_of(spin.value()) != value {
                    spin.set_value(f64::from(value));
                }
                spin_updating.set(false);
                let generation = scale_generation.get().wrapping_add(1);
                scale_generation.set(generation);
                pending.set(true);
                let pending = Rc::clone(&pending);
                let scale_generation = Rc::clone(&scale_generation);
                let commands = commands.clone();
                glib::timeout_add_local_once(SLIDER_SETTLE, move || {
                    if scale_generation.get() != generation {
                        return;
                    }
                    pending.set(false);
                    let _ = commands.try_send(ApplicationCommand::Microphone(
                        MicrophoneCommand::SetInputVolume {
                            input_id,
                            volume_percent: value,
                        },
                    ));
                });
            });
        }

        {
            let updating = Rc::clone(&updating);
            let scale_updating = Rc::clone(&scale_updating);
            let spin_updating = Rc::clone(&spin_updating);
            let spin_gate = Rc::clone(&spin_gate);
            let scale_generation = Rc::clone(&scale_generation);
            let spin_pending = Rc::clone(&spin_pending);
            let scale = scale.clone();
            // Runs before the settle helper: a user change cancels a stale
            // programmatic marker and mirrors the level into the scale.
            spin.connect_value_changed(move |spin| {
                if updating.get() || spin_updating.get() {
                    return;
                }
                spin_gate.set(false);
                spin_pending.set(true);
                let value = level_of(spin.value());
                scale_updating.set(true);
                scale_generation.set(scale_generation.get().wrapping_add(1));
                if level_of(scale.value()) != value {
                    scale.set_value(f64::from(value));
                }
                scale_updating.set(false);
            });
        }

        {
            let spin_gate = Rc::clone(&spin_gate);
            let spin_pending = Rc::clone(&spin_pending);
            let commands = commands.clone();
            on_spin_button_settled(&spin, move |value| {
                spin_pending.set(false);
                if spin_gate.replace(false) {
                    return;
                }
                let _ = commands.try_send(ApplicationCommand::Microphone(
                    MicrophoneCommand::SetInputVolume {
                        input_id,
                        volume_percent: level_of(value),
                    },
                ));
            });
        }

        let mute = MuteControl::new(
            reading.muted,
            (
                "microphone-sensitivity-high-symbolic",
                "microphone-sensitivity-muted-symbolic",
            ),
            ("Mute microphone", "Unmute microphone"),
            commands,
            Rc::new(move |muted| {
                ApplicationCommand::Microphone(MicrophoneCommand::SetInputMute { input_id, muted })
            }),
        );
        container.append(&mute.button);

        Self {
            container,
            icon,
            scale,
            spin,
            mute,
            updating,
            spin_gate,
            scale_generation,
            pending,
            spin_pending,
        }
    }

    fn update(&self, reading: MicrophoneReading, held: bool) {
        self.icon
            .set_icon_name(Some(microphone_icon(reading.muted)));
        self.mute.update(reading.muted);
        // Leave the value alone while the pointer holds the pill or a change
        // is still on its way to the mixer.
        if held || self.pending.get() || self.spin_pending.get() || self.spin.has_focus() {
            return;
        }
        self.updating.set(true);
        self.scale_generation
            .set(self.scale_generation.get().wrapping_add(1));
        if level_of(self.scale.value()) != reading.percent {
            self.scale.set_value(f64::from(reading.percent));
        }
        if level_of(self.spin.value()) != reading.percent {
            self.spin_gate.set(true);
            self.spin.set_value(f64::from(reading.percent));
        }
        self.updating.set(false);
    }
}

struct KeyboardLightControls {
    container: gtk::Box,
    slider: LevelSlider,
    toggle: LightButton,
}

impl KeyboardLightControls {
    fn new(percent: u8, commands: &Sender<ApplicationCommand>) -> Self {
        let container = gtk::Box::new(Orientation::Horizontal, 6);
        let toggle = LightButton::new(percent, commands);
        container.append(&toggle.button);
        let slider = LevelSlider::new(
            100,
            percent,
            "Keyboard light level, 0 to 100 percent",
            commands,
            Rc::new(|percent| {
                ApplicationCommand::QuickToggle(QuickToggleCommand {
                    id: QuickToggleId::KeyboardLight,
                    mutation: QuickToggleMutation::SetLevel(percent),
                    confirmation_token: None,
                })
            }),
        );
        container.append(&slider.scale);
        Self {
            container,
            slider,
            toggle,
        }
    }

    fn update(&self, percent: u8, held: bool) {
        self.toggle.update(percent);
        if !held {
            self.slider.set_value(percent);
        }
    }
}

fn microphone_icon(muted: bool) -> &'static str {
    if muted {
        "microphone-sensitivity-muted-symbolic"
    } else {
        "audio-input-microphone-symbolic"
    }
}

fn level_of(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn configure_scale(scale: &gtk::Scale, value: u8, label: &str) {
    scale.set_value(f64::from(value));
    scale.set_draw_value(false);
    scale.set_size_request(SLIDER_WIDTH, -1);
    scale.set_valign(Align::Center);
    scale.update_property(&[Property::Label(label)]);
    scale.set_tooltip_text(Some(label));
}

/// A level slider that sends one command once dragging stops.
struct LevelSlider {
    scale: gtk::Scale,
    generation: Rc<Cell<u64>>,
    updating: Rc<Cell<bool>>,
    /// A debounce is still waiting to send.
    pending: Rc<Cell<bool>>,
}

impl LevelSlider {
    fn new(
        max: u8,
        value: u8,
        label: &str,
        commands: &Sender<ApplicationCommand>,
        command: Rc<dyn Fn(u8) -> ApplicationCommand>,
    ) -> Self {
        let scale = gtk::Scale::with_range(Orientation::Horizontal, 0.0, f64::from(max), 1.0);
        configure_scale(&scale, value, label);
        let generation = Rc::new(Cell::new(0u64));
        let updating = Rc::new(Cell::new(false));
        let pending = Rc::new(Cell::new(false));
        let sender = commands.clone();
        let changed_generation = Rc::clone(&generation);
        let changed_updating = Rc::clone(&updating);
        let changed_pending = Rc::clone(&pending);
        scale.connect_value_changed(move |scale| {
            if changed_updating.get() {
                return;
            }
            let value = level_of(scale.value());
            let generation = changed_generation.get().wrapping_add(1);
            changed_generation.set(generation);
            changed_pending.set(true);
            let changed_generation = Rc::clone(&changed_generation);
            let changed_pending = Rc::clone(&changed_pending);
            let command = Rc::clone(&command);
            let sender = sender.clone();
            glib::timeout_add_local_once(SLIDER_SETTLE, move || {
                if changed_generation.get() != generation {
                    return;
                }
                changed_pending.set(false);
                let _ = sender.try_send(command(value));
            });
        });
        Self {
            scale,
            generation,
            updating,
            pending,
        }
    }

    fn set_value(&self, value: u8) {
        // A render must not replace a change still on its way to the mixer.
        if self.pending.get() || level_of(self.scale.value()) == value {
            return;
        }
        self.updating.set(true);
        // A pending drag must not send a level the render just replaced.
        self.generation.set(self.generation.get().wrapping_add(1));
        self.scale.set_value(f64::from(value));
        self.updating.set(false);
    }

    fn set_max(&self, max: u8) {
        let adjustment = self.scale.adjustment();
        if adjustment.upper() != f64::from(max) {
            adjustment.set_upper(f64::from(max));
        }
    }
}

/// A mute toggle whose icon and label follow the mute state.
struct MuteControl {
    button: gtk::Button,
    muted: Rc<Cell<bool>>,
    icons: (&'static str, &'static str),
    labels: (&'static str, &'static str),
}

impl MuteControl {
    fn new(
        muted: bool,
        icons: (&'static str, &'static str),
        labels: (&'static str, &'static str),
        commands: &Sender<ApplicationCommand>,
        command: Rc<dyn Fn(bool) -> ApplicationCommand>,
    ) -> Self {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.set_valign(Align::Center);
        let state = Rc::new(Cell::new(muted));
        let sender = commands.clone();
        let pressed = Rc::clone(&state);
        button.connect_clicked(move |_| {
            let next = !pressed.get();
            pressed.set(next);
            let _ = sender.try_send(command(next));
        });
        let control = Self {
            button,
            muted: state,
            icons,
            labels,
        };
        control.update(muted);
        control
    }

    fn update(&self, muted: bool) {
        if self.muted.get() != muted {
            self.muted.set(muted);
        }
        let icon = if muted { self.icons.1 } else { self.icons.0 };
        let label = if muted { self.labels.1 } else { self.labels.0 };
        self.button.set_icon_name(icon);
        self.button.set_tooltip_text(Some(label));
        self.button.update_property(&[Property::Label(label)]);
    }
}

/// The icon button that switches the keyboard light off and back on.
struct LightButton {
    button: gtk::Button,
    level: Rc<Cell<u8>>,
    last_nonzero: Rc<Cell<u8>>,
}

impl LightButton {
    fn new(percent: u8, commands: &Sender<ApplicationCommand>) -> Self {
        let button = gtk::Button::from_icon_name("keyboard-brightness-symbolic");
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.set_valign(Align::Center);
        let level = Rc::new(Cell::new(percent));
        let last_nonzero = Rc::new(Cell::new(if percent > 0 { percent } else { 100 }));
        let sender = commands.clone();
        let pressed_level = Rc::clone(&level);
        let pressed_last = Rc::clone(&last_nonzero);
        button.connect_clicked(move |_| {
            let value = if pressed_level.get() > 0 {
                0
            } else {
                pressed_last.get()
            };
            let _ = sender.try_send(ApplicationCommand::QuickToggle(QuickToggleCommand {
                id: QuickToggleId::KeyboardLight,
                mutation: QuickToggleMutation::SetLevel(value),
                confirmation_token: None,
            }));
        });
        let control = Self {
            button,
            level,
            last_nonzero,
        };
        control.update(percent);
        control
    }

    fn update(&self, percent: u8) {
        if self.level.get() != percent {
            self.level.set(percent);
        }
        if percent > 0 && self.last_nonzero.get() != percent {
            self.last_nonzero.set(percent);
        }
        let label = if percent > 0 {
            "Turn off the keyboard light"
        } else {
            "Turn on the keyboard light"
        };
        self.button.set_tooltip_text(Some(label));
        self.button.update_property(&[Property::Label(label)]);
        if percent > 0 {
            self.button.remove_css_class("dim-label");
        } else {
            self.button.add_css_class("dim-label");
        }
    }
}

fn recording_item(
    paused: bool,
    clock: RecordingClock,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let item = gtk::Box::new(Orientation::Horizontal, 4);
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.add_css_class(if paused { "dim-label" } else { "error" });
    item.append(&dot);
    let time = gtk::Label::new(Some(&format_clock(clock.elapsed())));
    time.add_css_class("numeric");
    item.append(&time);
    if paused {
        item.append(&gtk::Label::new(Some("Paused")));
    } else {
        // The label holds the timer: once the chip is replaced or hidden,
        // the next tick finds no label and stops.
        let weak = time.downgrade();
        glib::timeout_add_local(CLOCK_TICK, move || match weak.upgrade() {
            Some(time) => {
                time.set_text(&format_clock(clock.elapsed()));
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }
    let (icon, label, request) = if paused {
        (
            "media-playback-start-symbolic",
            "Resume recording",
            RecordingRequest::Resume,
        )
    } else {
        (
            "media-playback-pause-symbolic",
            "Pause recording",
            RecordingRequest::Pause,
        )
    };
    item.append(&request_button(icon, label, request, commands));
    item.append(&request_button(
        "media-playback-stop-symbolic",
        "Stop and save the recording",
        RecordingRequest::Stop,
        commands,
    ));
    item
}

fn request_button(
    icon: &str,
    label: &str,
    request: RecordingRequest,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_valign(Align::Center);
    button.set_tooltip_text(Some(label));
    button.update_property(&[Property::Label(label)]);
    let commands = commands.clone();
    button.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::Recording(request.clone()));
    });
    button
}

fn alert_item(message: &str, commands: &Sender<ApplicationCommand>) -> gtk::Button {
    let content = gtk::Box::new(Orientation::Horizontal, 6);
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.add_css_class("warning");
    content.append(&icon);
    content.append(&gtk::Label::new(Some(message)));
    let button = gtk::Button::builder()
        .child(&content)
        .valign(Align::Center)
        .tooltip_text("Open Monitoring")
        .build();
    button.add_css_class("flat");
    button.update_property(&[Property::Label(&format!("{message}. Open Monitoring"))]);
    let commands = commands.clone();
    button.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::Focus(FocusTarget::Monitoring));
    });
    button
}

fn microphone_item(commands: &Sender<ApplicationCommand>) -> gtk::Button {
    let content = gtk::Box::new(Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name("microphone-disabled-symbolic"));
    content.append(&gtk::Label::new(Some("Microphone muted")));
    let button = gtk::Button::builder()
        .child(&content)
        .valign(Align::Center)
        .tooltip_text("Unmute every microphone input")
        .build();
    button.add_css_class("flat");
    button.update_property(&[Property::Label("Microphone muted. Unmute every input")]);
    let commands = commands.clone();
    button.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::Microphone(MicrophoneCommand::SetMuted(
            false,
        )));
    });
    button
}

fn new_window() -> gtk::Window {
    let window = gtk::Window::builder()
        .title("Kestrel status")
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("kestrel-status-window");
    layer_shell::configure_status_window(&window, NAMESPACE);

    window
}
