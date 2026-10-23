//! The status overlay: a small pill at the top centre of the screen that
//! appears while one of its modules has something active.

use std::{cell::RefCell, time::Duration};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, glib};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, FocusTarget, MicrophoneCommand, MicrophoneViewModel,
    RecordingRequest, RecordingViewModel,
    view_model::{
        ActiveAlertViewModel, OverlayItem, OverlaySources, OverlayViewModel, format_clock,
        overlay_items,
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

/// The latest state each module reads; updates arrive separately.
#[derive(Default)]
struct OverlayState {
    overlay: Option<OverlayViewModel>,
    recording: Option<RecordingViewModel>,
    alerts: Option<Vec<ActiveAlertViewModel>>,
    microphone: Option<MicrophoneViewModel>,
    shown: Vec<OverlayItem>,
    /// Only while something is shown; dropped when empty, so a hidden
    /// overlay holds no surface.
    window: Option<gtk::Window>,
}

/// Owned by the controller, so it works with the main window closed.
pub struct StatusOverlay {
    commands: Sender<ApplicationCommand>,
    state: RefCell<OverlayState>,
}

impl StatusOverlay {
    pub fn new(commands: Sender<ApplicationCommand>) -> Self {
        Self {
            commands,
            state: RefCell::default(),
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        {
            let mut state = self.state.borrow_mut();
            state.overlay = Some(view_model.overlay.clone());
            state.recording = Some(view_model.recording.clone());
            state.alerts = Some(view_model.monitor.alerts.clone());
            state.microphone = Some(view_model.microphone.clone());
        }
        self.render();
    }

    pub fn set_recording(&self, recording: &RecordingViewModel) {
        self.state.borrow_mut().recording = Some(recording.clone());
        self.render();
    }

    pub fn set_alerts(&self, alerts: &[ActiveAlertViewModel]) {
        self.state.borrow_mut().alerts = Some(alerts.to_vec());
        self.render();
    }

    pub fn set_microphone(&self, microphone: &MicrophoneViewModel) {
        self.state.borrow_mut().microphone = Some(microphone.clone());
        self.render();
    }

    fn render(&self) {
        let mut state = self.state.borrow_mut();
        let items = match &state.overlay {
            Some(overlay) => overlay_items(
                overlay,
                &OverlaySources {
                    recording: state.recording.as_ref(),
                    alerts: state.alerts.as_deref(),
                    microphone: state.microphone.as_ref(),
                },
            ),
            None => Vec::new(),
        };
        if items == state.shown {
            return;
        }
        if items.is_empty() {
            if let Some(window) = state.window.take() {
                window.destroy();
            }
        } else {
            let window = state.window.get_or_insert_with(new_window);
            window.set_child(Some(&self.content(&items)));
            window.present();
        }
        state.shown = items;
    }

    fn content(&self, items: &[OverlayItem]) -> gtk::Box {
        let pill = gtk::Box::new(Orientation::Horizontal, 8);
        pill.add_css_class("osd");
        pill.add_css_class("kestrel-status");
        for item in items {
            match item {
                OverlayItem::Recording { paused, clock } => {
                    pill.append(&self.recording_item(*paused, *clock));
                }
                OverlayItem::Alert { message } => pill.append(&self.alert_item(message)),
                OverlayItem::MicrophoneMuted => pill.append(&self.microphone_item()),
            }
        }
        pill
    }

    fn recording_item(&self, paused: bool, clock: RecordingClock) -> gtk::Box {
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
        item.append(&self.request_button(icon, label, request));
        item.append(&self.request_button(
            "media-playback-stop-symbolic",
            "Stop and save the recording",
            RecordingRequest::Stop,
        ));
        item
    }

    fn request_button(&self, icon: &str, label: &str, request: RecordingRequest) -> gtk::Button {
        let button = gtk::Button::from_icon_name(icon);
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.set_valign(Align::Center);
        button.set_tooltip_text(Some(label));
        button.update_property(&[Property::Label(label)]);
        let commands = self.commands.clone();
        button.connect_clicked(move |_| {
            let _ = commands.try_send(ApplicationCommand::Recording(request.clone()));
        });
        button
    }

    fn alert_item(&self, message: &str) -> gtk::Button {
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
        let commands = self.commands.clone();
        button.connect_clicked(move |_| {
            let _ = commands.try_send(ApplicationCommand::Focus(FocusTarget::Monitoring));
        });
        button
    }

    fn microphone_item(&self) -> gtk::Button {
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
        let commands = self.commands.clone();
        button.connect_clicked(move |_| {
            let _ = commands.try_send(ApplicationCommand::Microphone(MicrophoneCommand::SetMuted(
                false,
            )));
        });
        button
    }
}

fn new_window() -> gtk::Window {
    let window = gtk::Window::builder()
        .title("Kestrel status")
        .decorated(false)
        .resizable(false)
        .build();
    window.add_css_class("kestrel-status-window");
    layer_shell::configure_status_window(&window, NAMESPACE);
    let provider = gtk::CssProvider::new();
    provider.load_from_data(STYLE);
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(&window),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    window
}
