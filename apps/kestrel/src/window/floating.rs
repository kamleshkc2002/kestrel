//! Floating command bar and quick panel surfaces.
//!
//! Each surface is an undecorated window: a layer-shell overlay where the
//! compositor offers one, otherwise a normal window. Closing it destroys the
//! window, and the compositor or window manager refocuses the previous one.

use std::{cell::RefCell, rc::Rc};

use super::{command_bar::color_swatch, widgets::plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, accessible::Property, gdk, glib};
use kestrel::{ApplicationCommand, CommandBarCommand, CommandBarViewModel, FocusTarget};

use crate::layer_shell::{self, SurfacePlacement};

const COMMAND_BAR_WIDTH: i32 = 640;
const QUICK_PANEL_WIDTH: i32 = 440;
const MAX_RESULTS_HEIGHT: i32 = 360;
const COMMAND_BAR_HINT: &str = "Up and Down select · Return runs · Escape closes";
/// Focus may leave briefly when input devices change; dismissal waits this long.
const DISMISS_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// One quick-panel tool, resolved by the controller.
pub struct QuickPanelEntry {
    pub id: String,
    pub description: String,
    /// Why the tool cannot run now, with remediation.
    pub gate: Option<String>,
    pub command: ApplicationCommand,
}

pub struct FloatingSurface {
    pub kind: FocusTarget,
    pub window: gtk::Window,
    status: gtk::Label,
    results: gtk::ListBox,
    /// Result IDs in row order; empty for the quick panel and gated bar.
    result_ids: Rc<RefCell<Vec<String>>>,
}

impl FloatingSurface {
    /// Opens the command bar; `gate` replaces search with remediation.
    pub fn command_bar(
        application: &adw::Application,
        parent: Option<&gtk::Window>,
        startup_id: Option<&str>,
        commands: &Sender<ApplicationCommand>,
        model: &CommandBarViewModel,
        gate: Option<&str>,
    ) -> Self {
        let window = surface_window(
            application,
            parent,
            "Kestrel command bar",
            "kestrel-command-bar",
            COMMAND_BAR_WIDTH,
        );
        let root = surface_root(&window);
        let status = status_label(gate.unwrap_or(COMMAND_BAR_HINT));
        let results = results_list(&root);
        let surface = Self {
            kind: FocusTarget::CommandBar,
            window,
            status,
            results,
            result_ids: Rc::new(RefCell::new(Vec::new())),
        };

        if gate.is_some() {
            root.prepend(&surface.status);
            surface.results.set_visible(false);
            surface.window.set_child(Some(&root));
            present(&surface.window, startup_id);
            return surface;
        }

        let query = gtk::SearchEntry::builder()
            .placeholder_text("Run a command, search applications, compute a value")
            .hexpand(true)
            .build();
        query.update_property(&[Property::Label("Command bar search")]);
        let sender = commands.clone();
        query.connect_search_changed(move |entry| {
            let _ = sender.try_send(ApplicationCommand::CommandQuery(entry.text().to_string()));
        });
        root.prepend(&surface.status);
        root.prepend(&query);
        surface.window.set_child(Some(&root));

        let sender = commands.clone();
        let ids = Rc::clone(&surface.result_ids);
        surface.results.connect_row_activated(move |_, row| {
            run_result(&sender, &ids, row.index());
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let results = surface.results.clone();
        let sender = commands.clone();
        let ids = Rc::clone(&surface.result_ids);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Down => move_selection(&results, 1),
                gdk::Key::n if control => move_selection(&results, 1),
                gdk::Key::Up => move_selection(&results, -1),
                gdk::Key::p if control => move_selection(&results, -1),
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    let index = results.selected_row().map_or(0, |row| row.index());
                    run_result(&sender, &ids, index);
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        surface.window.add_controller(keys);

        surface.set_command_bar(model);
        // Focus before mapping so the first typed key reaches the field.
        GtkWindowExt::set_focus(&surface.window, Some(&query));
        present(&surface.window, startup_id);
        surface
    }

    /// Opens the favorites palette in saved order.
    pub fn quick_panel(
        application: &adw::Application,
        parent: Option<&gtk::Window>,
        startup_id: Option<&str>,
        commands: &Sender<ApplicationCommand>,
        entries: Vec<QuickPanelEntry>,
    ) -> Self {
        let window = surface_window(
            application,
            parent,
            "Kestrel quick panel",
            "kestrel-quick-panel",
            QUICK_PANEL_WIDTH,
        );
        let root = surface_root(&window);
        let title = gtk::Label::new(Some("Quick panel"));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        root.append(&title);
        let status = status_label(if entries.is_empty() {
            "No favorites yet. Add tools under Settings → Quick panel."
        } else {
            "Return runs the selected tool · Escape closes"
        });
        root.append(&status);
        let results = results_list(&root);
        window.set_child(Some(&root));

        let mut commands_by_row = Vec::with_capacity(entries.len());
        for entry in entries {
            let row = plain_row()
                .title(&entry.description)
                .subtitle(entry.gate.as_deref().unwrap_or(&entry.id))
                .subtitle_lines(0)
                .sensitive(entry.gate.is_none())
                .build();
            row.set_activatable(entry.gate.is_none());
            row.update_property(&[Property::Label(&entry.description)]);
            results.append(&row);
            commands_by_row.push(entry.command);
        }
        let sender = commands.clone();
        let weak_window = window.downgrade();
        results.connect_row_activated(move |_, row| {
            let Some(command) = usize::try_from(row.index())
                .ok()
                .and_then(|index| commands_by_row.get(index))
            else {
                return;
            };
            let _ = sender.try_send(command.clone());
            if let Some(window) = weak_window.upgrade() {
                window.close();
            }
        });
        if let Some(first) = first_sensitive_row(&results) {
            results.select_row(Some(&first));
            GtkWindowExt::set_focus(&window, Some(&first));
        }
        present(&window, startup_id);
        Self {
            kind: FocusTarget::QuickPanel,
            window,
            status,
            results,
            result_ids: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Replaces the command-bar results; the first result is preselected.
    pub fn set_command_bar(&self, model: &CommandBarViewModel) {
        if self.kind != FocusTarget::CommandBar {
            return;
        }
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let mut ids = self.result_ids.borrow_mut();
        ids.clear();
        for result in &model.results {
            let row = plain_row()
                .title(format!("{} · {}", result.source, result.title))
                .subtitle(&result.detail)
                .build();
            row.set_activatable(model.running);
            if let Some(rgb) = result.swatch {
                row.add_prefix(&color_swatch(rgb));
            }
            row.update_property(&[Property::Label(&format!(
                "{} result {}",
                result.source, result.title
            ))]);
            self.results.append(&row);
            ids.push(result.id.clone());
        }
        self.results.set_visible(!ids.is_empty());
        if let Some(first) = self.results.row_at_index(0) {
            self.results.select_row(Some(&first));
        }
    }

    /// Shows the outcome of the last run, success or typed error.
    pub fn show_outcome(&self, message: &str) {
        self.status.set_text(message);
    }
}

fn surface_window(
    application: &adw::Application,
    parent: Option<&gtk::Window>,
    title: &str,
    namespace: &str,
    width: i32,
) -> gtk::Window {
    let window = gtk::Window::builder()
        .application(application)
        .title(title)
        .decorated(false)
        .resizable(false)
        .default_width(width)
        .build();
    window.add_css_class("background");
    if let SurfacePlacement::LayerShell { .. } = layer_shell::observed_placement() {
        layer_shell::configure_window(&window, namespace);
    } else if let Some(parent) = parent.filter(|parent| parent.is_mapped()) {
        window.set_transient_for(Some(parent));
    }

    let escape = gtk::EventControllerKey::new();
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_window = window.downgrade();
    escape.connect_key_pressed(move |_, key, _, _| {
        if key != gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
        glib::Propagation::Stop
    });
    window.add_controller(escape);
    // Activating another window dismisses the surface.
    window.connect_is_active_notify(|window| {
        if window.is_active() {
            return;
        }
        let weak_window = window.downgrade();
        glib::timeout_add_local_once(DISMISS_DELAY, move || {
            if let Some(window) = weak_window.upgrade() {
                if !window.is_active() && window.is_visible() {
                    window.close();
                }
            }
        });
    });
    window
}

fn surface_root(window: &gtk::Window) -> gtk::Box {
    let root = gtk::Box::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_width_request(window.default_width() - 24);
    root
}

fn status_label(text: &str) -> gtk::Label {
    let status = gtk::Label::new(Some(text));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.update_property(&[Property::Label("Status")]);
    status
}

fn results_list(root: &gtk::Box) -> gtk::ListBox {
    let results = gtk::ListBox::new();
    results.set_selection_mode(gtk::SelectionMode::Single);
    results.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(MAX_RESULTS_HEIGHT)
        .child(&results)
        .build();
    root.append(&scroll);
    results
}

fn present(window: &gtk::Window, startup_id: Option<&str>) {
    if let Some(startup_id) = startup_id {
        window.set_startup_id(startup_id);
    }
    window.present();
}

/// Moves the highlight; focus stays in the search field so typing continues.
fn move_selection(results: &gtk::ListBox, delta: i32) {
    let mut index = results.selected_row().map_or(-1, |row| row.index()) + delta;
    while index >= 0 {
        let Some(row) = results.row_at_index(index) else {
            return;
        };
        if row.is_sensitive() {
            results.select_row(Some(&row));
            return;
        }
        index += delta;
    }
}

fn first_sensitive_row(results: &gtk::ListBox) -> Option<gtk::ListBoxRow> {
    (0..)
        .map_while(|index| results.row_at_index(index))
        .find(|row| row.is_sensitive())
}

fn run_result(commands: &Sender<ApplicationCommand>, ids: &RefCell<Vec<String>>, index: i32) {
    let id = usize::try_from(index)
        .ok()
        .and_then(|index| ids.borrow().get(index).cloned());
    if let Some(id) = id {
        let _ = commands.try_send(ApplicationCommand::CommandBar(CommandBarCommand::Run(id)));
    }
}
