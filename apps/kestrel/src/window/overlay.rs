//! The status overlay: a small pill at the top centre of the screen that
//! appears while one of its modules has something active.

use std::cell::RefCell;

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, MicrophoneCommand, MicrophoneViewModel,
    view_model::{OverlayItem, OverlaySources, OverlayViewModel, overlay_items},
};

use crate::layer_shell;

const NAMESPACE: &str = "kestrel-status";
/// The window stays transparent around the rounded pill.
const STYLE: &str = "window.kestrel-status-window { background: none; } \
                     .kestrel-status { border-radius: 999px; padding: 4px 10px; }";

/// The latest state each module reads; updates arrive separately.
#[derive(Default)]
struct OverlayState {
    overlay: Option<OverlayViewModel>,
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
            state.microphone = Some(view_model.microphone.clone());
        }
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
                OverlayItem::MicrophoneMuted => pill.append(&self.microphone_item()),
            }
        }
        pill
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
