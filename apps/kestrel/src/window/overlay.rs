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
    MicrophoneCommand, MicrophoneViewModel, PeripheralKind, QuickToggleCommand, QuickToggleId,
    QuickToggleMutation, QuickToggleViewModel, RecordingRequest, RecordingViewModel, output_icon,
    view_model::{
        AccessoryLevel, AccessoryReading, ActiveAlertViewModel, ControlFeedback, ControlReadings,
        MicrophoneReading, MonitorViewModel, OverlayItem, OverlaySources, OverlayViewModel,
        VolumeReading, accessory_icon, format_clock, overlay_items,
    },
};
use kestrel_services::recording::RecordingClock;

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
    accessories: Option<Vec<AccessoryReading>>,
    microphone: Option<MicrophoneViewModel>,
    audio: Option<AudioViewModel>,
    keyboard_light: Option<QuickToggleViewModel>,
    keep_awake: Option<QuickToggleViewModel>,
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
            state.accessories = AccessoryReading::read(Some(&view_model.monitor));
            state.microphone = Some(view_model.microphone.clone());
            state.audio = Some(view_model.audio.clone());
            state.keyboard_light = view_model
                .quick_toggles
                .iter()
                .find(|toggle| toggle.id == QuickToggleId::KeyboardLight)
                .cloned();
            state.keep_awake = view_model
                .quick_toggles
                .iter()
                .find(|toggle| toggle.id == QuickToggleId::KeepAwake)
                .cloned();
        }
        self.observe();
    }

    pub fn set_recording(&self, recording: &RecordingViewModel) {
        self.state.borrow_mut().recording = Some(recording.clone());
        self.observe();
    }

    pub fn set_monitor(&self, monitor: &MonitorViewModel) {
        {
            let mut state = self.state.borrow_mut();
            state.alerts = Some(monitor.alerts.clone());
            state.accessories = AccessoryReading::read(Some(monitor));
        }
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

    pub fn set_keep_awake(&self, keep_awake: &QuickToggleViewModel) {
        self.state.borrow_mut().keep_awake = Some(keep_awake.clone());
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
            let now = Instant::now();
            let accessories = state.accessories.clone();
            state.feedback.observe(now, readings);
            state.feedback.observe_accessories(now, accessories);
        }
        render(&self.state, &self.commands);
    }
}

/// A control set that can be matched against the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemLayout {
    Recording {
        paused: bool,
        clock: RecordingClock,
    },
    Alert {
        message: String,
    },
    Accessory {
        id: String,
        percent: u8,
        charging: bool,
        level: AccessoryLevel,
    },
    Volume {
        output_id: u32,
    },
    MicrophoneLevel {
        input_id: u32,
    },
    KeyboardLight,
    KeepAwake {
        remaining: String,
    },
    MicrophoneMuted,
}

impl ItemLayout {
    fn of(item: &OverlayItem) -> Self {
        match item {
            OverlayItem::Recording { paused, clock } => Self::Recording {
                paused: *paused,
                clock: *clock,
            },
            OverlayItem::Alert { message } => Self::Alert {
                message: message.clone(),
            },
            OverlayItem::Accessory {
                id,
                percent,
                charging,
                level,
                ..
            } => Self::Accessory {
                id: id.clone(),
                percent: *percent,
                charging: *charging,
                level: *level,
            },
            OverlayItem::Volume(reading) => Self::Volume {
                output_id: reading.output_id,
            },
            OverlayItem::MicrophoneLevel(reading) => Self::MicrophoneLevel {
                input_id: reading.input_id,
            },
            OverlayItem::KeyboardLight { .. } => Self::KeyboardLight,
            OverlayItem::KeepAwake { remaining } => Self::KeepAwake {
                remaining: remaining.clone(),
            },
            OverlayItem::MicrophoneMuted => Self::MicrophoneMuted,
        }
    }
}

fn render(state: &Rc<RefCell<OverlayState>>, commands: &Sender<ApplicationCommand>) {
    let now = Instant::now();
    let items = {
        let state = state.borrow();
        match state.overlay.as_ref() {
            Some(overlay) => {
                let shown_accessories = state.feedback.shown_accessories(now);
                overlay_items(
                    overlay,
                    &OverlaySources {
                        recording: state.recording.as_ref(),
                        alerts: state.alerts.as_deref(),
                        microphone: state.microphone.as_ref(),
                        accessories: state.accessories.as_deref(),
                        shown_accessories: &shown_accessories,
                        controls: state.feedback.shown(now),
                        keep_awake: state.keep_awake.as_ref(),
                    },
                )
            }
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
            OverlayItem::Accessory {
                kind,
                percent,
                charging,
                level,
                ..
            } => pill.append(&accessory_item(
                *kind, *percent, *charging, *level, commands,
            )),
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
            OverlayItem::KeepAwake { remaining } => {
                pill.append(&keep_awake_item(remaining, commands));
            }
            OverlayItem::MicrophoneMuted => pill.append(&microphone_item(commands)),
        }
    }
    let motion = gtk::EventControllerMotion::new();
    {
        let state = state.clone();
        let commands = commands.clone();
        motion.connect_enter(move |_, _, _| defer_held(&state, &commands, true));
    }
    {
        let state = state.clone();
        let commands = commands.clone();
        motion.connect_leave(move |_| defer_held(&state, &commands, false));
    }
    pill.add_controller(motion);
    (pill, controls)
}

/// Crossing events can arrive while a render swaps the pill, so the hold
/// waits until the render has finished.
fn defer_held(
    state: &Weak<RefCell<OverlayState>>,
    commands: &Sender<ApplicationCommand>,
    held: bool,
) {
    let state = state.clone();
    let commands = commands.clone();
    glib::idle_add_local_once(move || {
        let Some(state) = state.upgrade() else {
            return;
        };
        {
            let mut state = state.borrow_mut();
            state.feedback.set_held(Instant::now(), held);
            // A click can focus the percentage only while the pointer is over
            // the pill, so the overlay never takes the keyboard by appearing.
            if let Some(window) = state.window.as_ref() {
                layer_shell::set_status_keyboard(window, held);
            }
        }
        render(&state, &commands);
    });
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
        dim_while_muted(&[slider.scale.upcast_ref()], reading.muted);
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
        dim_while_muted(&[self.slider.scale.upcast_ref()], reading.muted);
        self.slider.set_max(reading.max_percent);
        if !held {
            self.slider.set_value(reading.percent);
        }
    }
}

struct MicrophoneControls {
    container: gtk::Box,
    mute: MuteControl,
    slider: LevelSlider,
    spin: gtk::SpinButton,
}

impl MicrophoneControls {
    fn new(reading: MicrophoneReading, commands: &Sender<ApplicationCommand>) -> Self {
        let container = gtk::Box::new(Orientation::Horizontal, 6);
        let input_id = reading.input_id;
        let mute = MuteControl::new(
            reading.muted,
            (
                "audio-input-microphone-symbolic",
                "microphone-sensitivity-muted-symbolic",
            ),
            ("Mute microphone", "Unmute microphone"),
            commands,
            Rc::new(move |muted| {
                ApplicationCommand::Microphone(MicrophoneCommand::SetInputMute { input_id, muted })
            }),
        );
        container.append(&mute.button);
        let slider = LevelSlider::new(
            100,
            reading.percent,
            "Microphone level, 0 to 100 percent",
            commands,
            Rc::new(move |volume_percent| {
                ApplicationCommand::Microphone(MicrophoneCommand::SetInputVolume {
                    input_id,
                    volume_percent,
                })
            }),
        );
        container.append(&slider.scale);
        // Sharing the scale's adjustment keeps both in step, and a typed
        // percentage goes out through the slider's debounce.
        let spin = gtk::SpinButton::new(Some(&slider.scale.adjustment()), 1.0, 0);
        spin.set_numeric(true);
        spin.set_width_chars(3);
        spin.set_valign(Align::Center);
        let spin_label = "Microphone level percentage";
        spin.update_property(&[Property::Label(spin_label)]);
        spin.set_tooltip_text(Some(spin_label));
        container.append(&spin);
        dim_while_muted(
            &[slider.scale.upcast_ref(), spin.upcast_ref()],
            reading.muted,
        );
        Self {
            container,
            mute,
            slider,
            spin,
        }
    }

    fn update(&self, reading: MicrophoneReading, held: bool) {
        self.mute.update(reading.muted);
        dim_while_muted(
            &[self.slider.scale.upcast_ref(), self.spin.upcast_ref()],
            reading.muted,
        );
        // A percentage being typed keeps its text until it is entered.
        let typing = self
            .spin
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        if !held && !typing {
            self.slider.set_value(reading.percent);
        }
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

/// A muted output or input dims its level, so the state reads at a glance.
fn dim_while_muted(widgets: &[&gtk::Widget], muted: bool) {
    for widget in widgets {
        if muted {
            widget.add_css_class("dim-label");
        } else {
            widget.remove_css_class("dim-label");
        }
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

/// An accessory's battery, amber while low and red while critical.
fn accessory_item(
    kind: PeripheralKind,
    percent: u8,
    charging: bool,
    level: AccessoryLevel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Button {
    let content = gtk::Box::new(Orientation::Horizontal, 6);
    let icon = gtk::Image::from_icon_name(accessory_icon(kind));
    icon.set_valign(Align::Center);
    content.append(&icon);
    if charging {
        content.append(&gtk::Image::from_icon_name(
            "battery-level-100-charged-symbolic",
        ));
    }
    let value = gtk::Label::new(Some(&format!("{percent}%")));
    value.add_css_class("numeric");
    value.set_valign(Align::Center);
    let (suffix, class) = match level {
        AccessoryLevel::Warning => (", low", Some("warning")),
        AccessoryLevel::Critical => (", critically low", Some("error")),
        AccessoryLevel::Normal => ("", None),
    };
    if let Some(class) = class {
        value.add_css_class(class);
    }
    content.append(&value);
    let label = format!("{} battery {percent} percent{suffix}", kind.label());
    let button = gtk::Button::builder()
        .child(&content)
        .valign(Align::Center)
        .tooltip_text("Open Monitoring")
        .build();
    button.add_css_class("flat");
    button.update_property(&[Property::Label(&label)]);
    let commands = commands.clone();
    button.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::Focus(FocusTarget::Monitoring));
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

/// A timed keep-awake session's time left, with a stop control.
fn keep_awake_item(remaining: &str, commands: &Sender<ApplicationCommand>) -> gtk::Box {
    let item = gtk::Box::new(Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name("preferences-desktop-screensaver-symbolic");
    icon.set_valign(Align::Center);
    item.append(&icon);
    let time = gtk::Label::new(Some(remaining));
    time.add_css_class("numeric");
    time.set_valign(Align::Center);
    item.append(&time);

    let stop = gtk::Button::from_icon_name("media-playback-stop-symbolic");
    stop.add_css_class("flat");
    stop.add_css_class("circular");
    stop.set_valign(Align::Center);
    stop.set_tooltip_text(Some("Stop keeping awake"));
    let label = format!("Keep awake, {remaining} left");
    stop.update_property(&[Property::Label(&label)]);
    let commands = commands.clone();
    stop.connect_clicked(move |_| {
        let _ = commands.try_send(ApplicationCommand::StopKeepAwake);
    });
    item.append(&stop);
    item
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
