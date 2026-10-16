use async_channel::Sender;
use gtk::prelude::*;
use kestrel::{ApplicationCommand, CommandBarCommand, CommandBarViewModel};

use crate::layer_shell::{self, SurfacePlacement};

pub struct FloatingSurface {
    pub window: gtk::Window,
    query: gtk::SearchEntry,
    pub status: gtk::Label,
    results: gtk::Box,
}

impl FloatingSurface {
    pub fn open(
        application: &adw::Application,
        parent: Option<&gtk::Window>,
        commands: &Sender<ApplicationCommand>,
        model: &CommandBarViewModel,
        gate: Option<&str>,
        namespace: &str,
    ) -> Self {
        let window = gtk::Window::builder()
            .application(application)
            .title("Kestrel")
            .decorated(false)
            .default_width(640)
            .default_height(420)
            .build();
        if let Some(parent) = parent {
            if parent.is_mapped() {
                window.set_transient_for(Some(parent));
            }
        }
        let placement = layer_shell::observed_placement();
        if matches!(placement, SurfacePlacement::LayerShell { .. }) {
            layer_shell::configure_window(&window, namespace);
        }

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);
        let query = gtk::SearchEntry::builder()
            .placeholder_text("Search commands")
            .hexpand(true)
            .build();
        query.set_visible(gate.is_none());
        let sender = commands.clone();
        query.connect_search_changed(move |entry| {
            let _ = sender.try_send(ApplicationCommand::CommandQuery(entry.text().to_string()));
        });
        root.append(&query);
        let initial_status = gate.map_or_else(|| model.status.clone(), str::to_owned);
        let status = gtk::Label::new(Some(&placement_status(&placement, &initial_status)));
        status.set_xalign(0.0);
        status.set_wrap(true);
        root.append(&status);
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .min_content_height(120)
            .max_content_height(340)
            .build();
        let results = gtk::Box::new(gtk::Orientation::Vertical, 4);
        scroll.set_child(Some(&results));
        root.append(&scroll);
        window.set_child(Some(&root));

        let selection = std::rc::Rc::new(std::cell::Cell::new(None));
        let close = gtk::EventControllerKey::new();
        let weak_window = window.downgrade();
        let selection_for_keys = selection.clone();
        let results_for_keys = results.clone();
        close.connect_key_pressed(move |_, key, modifiers, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
                return gtk::glib::Propagation::Stop;
            }
            let delta = if key == gtk::gdk::Key::Down
                || (key == gtk::gdk::Key::n
                    && modifiers & gtk::gdk::ModifierType::CONTROL_MASK.bits() != 0)
            {
                Some(1_i32)
            } else if key == gtk::gdk::Key::Up
                || (key == gtk::gdk::Key::p
                    && modifiers & gtk::gdk::ModifierType::CONTROL_MASK.bits() != 0)
            {
                Some(-1_i32)
            } else {
                None
            };
            if let Some(delta) = delta {
                let count = results_for_keys.observe_children().n_items() as i32;
                if count > 0 {
                    let current = selection_for_keys
                        .get()
                        .map(|index| index as i32)
                        .unwrap_or(-1);
                    selection_for_keys.set(Some((current + delta).clamp(0, count - 1) as usize));
                }
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Return {
                let child = selection_for_keys
                    .get()
                    .and_then(|index| results_for_keys.observe_children().item(index as u32))
                    .or_else(|| results_for_keys.observe_children().item(0));
                if let Some(button) = child.and_then(|object| object.downcast::<gtk::Button>().ok())
                {
                    button.emit_clicked();
                }
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(close);
        let weak_window = window.downgrade();
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
            }
        });
        let view = Self {
            window,
            query,
            status,
            results,
        };
        if gate.is_none() {
            view.set_model(model, commands);
        }
        gtk::prelude::GtkWindowExt::set_focus(&view.window, Some(&view.query));
        view.window.present();
        view.query.grab_focus();
        view
    }

    pub fn set_model(&self, model: &CommandBarViewModel, commands: &Sender<ApplicationCommand>) {
        self.status.set_text(&model.status);
        while let Some(child) = self.results.first_child() {
            child.unparent();
        }
        for result in &model.results {
            let button = gtk::Button::with_label(&format!("{} · {}", result.source, result.title));
            button.set_halign(gtk::Align::Fill);
            button.set_tooltip_text(Some(&result.detail));
            button.set_sensitive(model.running);
            let sender = commands.clone();
            let id = result.id.clone();
            button.connect_clicked(move |_| {
                let _ = sender.try_send(ApplicationCommand::CommandBar(CommandBarCommand::Run(
                    id.clone(),
                )));
            });
            self.results.append(&button);
        }
    }
    pub fn open_quick_panel(
        application: &adw::Application,
        parent: Option<&gtk::Window>,
        commands: &Sender<ApplicationCommand>,
        favorites: &[String],
        gates: &[Option<String>],
    ) -> Self {
        let window = gtk::Window::builder()
            .application(application)
            .title("Kestrel quick panel")
            .decorated(false)
            .default_width(420)
            .default_height(320)
            .build();
        if let Some(parent) = parent {
            if parent.is_mapped() {
                window.set_transient_for(Some(parent));
            }
        }
        let placement = layer_shell::observed_placement();
        if matches!(placement, SurfacePlacement::LayerShell { .. }) {
            layer_shell::configure_window(&window, "kestrel-quick-panel");
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);
        let title = gtk::Label::new(Some("Quick panel"));
        title.set_xalign(0.0);
        root.append(&title);
        let status = gtk::Label::new(Some(&placement_status(
            &placement,
            if favorites.is_empty() {
                "No favorites yet. Add tools in Settings → Quick panel."
            } else {
                ""
            },
        )));
        status.set_xalign(0.0);
        status.set_wrap(true);
        root.append(&status);
        let results = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for (index, id) in favorites.iter().enumerate() {
            let Ok(action) = kestrel::command_line::CommandLineAction::parse(id) else {
                continue;
            };
            let button = gtk::Button::with_label(&format!("{} — {}", id, action.description()));
            if let Some(gate) = gates.get(index).and_then(Option::as_ref) {
                button.set_sensitive(false);
                button.set_tooltip_text(Some(gate));
            }
            let sender = commands.clone();
            button.connect_clicked(move |_| {
                let _ = sender.try_send(action.to_command());
            });
            results.append(&button);
        }
        root.append(&results);
        window.set_child(Some(&root));
        let close = gtk::EventControllerKey::new();
        let weak = window.downgrade();
        close.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = weak.upgrade() {
                    window.close();
                }
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(close);
        let weak = window.downgrade();
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                if let Some(window) = weak.upgrade() {
                    window.close();
                }
            }
        });
        window.present();
        window.grab_focus();
        Self {
            window,
            query: gtk::SearchEntry::new(),
            status,
            results,
        }
    }
}

fn placement_status(placement: &SurfacePlacement, status: &str) -> String {
    let placement = match placement {
        SurfacePlacement::LayerShell { protocol_version } => {
            format!("Layer-shell surface (protocol {protocol_version})")
        }
        SurfacePlacement::Transient { reason } => reason.clone(),
    };
    if status.is_empty() {
        placement
    } else {
        format!("{placement}\n{status}")
    }
}
