//! Scratchpad tabs, editor, and Markdown preview.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{ApplicationCommand, ScratchpadCommand, ScratchpadViewModel};

use super::widgets::{choose_path, plain_row};

/// Widgets kept across updates so typing never rebuilds the editor.
pub(super) struct ScratchpadPanel {
    pub(super) container: gtk::Box,
    pub(super) editor: gtk::TextView,
    title: gtk::Entry,
    tabs: gtk::Box,
    tab_bar: gtk::Box,
    preview: gtk::Label,
    preview_frame: gtk::Frame,
    editor_frame: gtk::Frame,
    preview_toggle: gtk::ToggleButton,
    actions: gtk::Box,
    status: adw::ActionRow,
    expired: adw::ActionRow,
    expired_button: gtk::Button,
    /// Shared with signal handlers, which outlive any one model.
    active_id: Rc<Cell<Option<u64>>>,
    active_title: Rc<RefCell<String>>,
    expired_count: Rc<Cell<usize>>,
    /// Set while the model writes into widgets, so no edit echoes back.
    updating: Rc<Cell<bool>>,
    commands: Sender<ApplicationCommand>,
}

pub(super) fn build_scratchpad_panel(
    model: &ScratchpadViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> ScratchpadPanel {
    let container = gtk::Box::new(Orientation::Vertical, 8);
    let group = adw::PreferencesGroup::builder()
        .title("Scratchpad")
        .description("Private notes saved on this computer, with Markdown preview.")
        .build();
    let status = plain_row().title("Scratchpad").subtitle_lines(0).build();
    group.add(&status);

    let tabs = gtk::Box::new(Orientation::Horizontal, 4);
    tabs.add_css_class("linked");
    tabs.set_valign(Align::Center);
    let tabs_scroller = gtk::ScrolledWindow::builder()
        .child(&tabs)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .build();
    tabs_scroller.update_property(&[Property::Label("Scratchpad tabs")]);
    let new_button = gtk::Button::from_icon_name("list-add-symbolic");
    new_button.set_tooltip_text(Some("New note"));
    new_button.update_property(&[Property::Label("New note")]);
    let tab_bar = gtk::Box::new(Orientation::Horizontal, 6);
    tab_bar.append(&tabs_scroller);
    tab_bar.append(&new_button);
    group.add(&tab_bar);

    let title = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text("Note title")
        .build();
    title.update_property(&[Property::Label("Note title")]);
    let editor = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .monospace(true)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    editor.update_property(&[Property::Label("Note text")]);
    let editor_scroller = gtk::ScrolledWindow::builder()
        .child(&editor)
        .min_content_height(200)
        .max_content_height(480)
        .propagate_natural_height(true)
        .build();
    let editor_frame = gtk::Frame::new(None);
    editor_frame.set_child(Some(&editor_scroller));
    let preview = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .yalign(0.0)
        .selectable(true)
        .use_markup(true)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    preview.update_property(&[Property::Label("Note preview")]);
    let preview_frame = gtk::Frame::new(None);
    preview_frame.set_child(Some(&preview));
    preview_frame.set_visible(false);

    let preview_toggle = gtk::ToggleButton::with_label("Preview");
    let close_button = gtk::Button::with_label("Close note");
    let export_button = gtk::Button::with_label("Export");
    let actions = gtk::Box::new(Orientation::Horizontal, 6);
    actions.append(&title);
    actions.append(&preview_toggle);
    actions.append(&export_button);
    actions.append(&close_button);

    let expired = plain_row().title("Expired notes").build();
    let expired_button = gtk::Button::with_label("Remove");
    expired_button.set_valign(Align::Center);
    expired_button.add_css_class("destructive-action");
    expired.add_suffix(&expired_button);
    expired.set_visible(false);

    container.append(&group);
    container.append(&actions);
    container.append(&editor_frame);
    container.append(&preview_frame);
    container.append(&expired);

    let panel = ScratchpadPanel {
        container,
        editor,
        title,
        tabs,
        tab_bar,
        preview,
        preview_frame,
        editor_frame,
        preview_toggle,
        actions,
        status,
        expired,
        expired_button,
        active_id: Rc::new(Cell::new(None)),
        active_title: Rc::new(RefCell::new(String::new())),
        expired_count: Rc::new(Cell::new(0)),
        updating: Rc::new(Cell::new(false)),
        commands: commands.clone(),
    };
    panel.connect_handlers(window, &new_button, &close_button, &export_button);
    panel.set_model(model);
    panel
}

fn send(commands: &Sender<ApplicationCommand>, command: ScratchpadCommand) {
    let _ = commands.try_send(ApplicationCommand::Scratchpad(command));
}

/// Asks before a destructive scratchpad command.
fn confirm(
    window: &adw::ApplicationWindow,
    heading: &str,
    body: &str,
    action: &str,
    commands: &Sender<ApplicationCommand>,
    command: ScratchpadCommand,
) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(window)
        .heading(heading)
        .body(body)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "confirm" {
            send(&commands, command.clone());
        }
        dialog.close();
    });
    dialog.present();
}

impl ScratchpadPanel {
    fn connect_handlers(
        &self,
        window: &adw::ApplicationWindow,
        new_button: &gtk::Button,
        close_button: &gtk::Button,
        export_button: &gtk::Button,
    ) {
        let commands = self.commands.clone();
        new_button.connect_clicked(move |_| {
            send(&commands, ScratchpadCommand::New("Untitled".to_owned()));
        });

        let commands = self.commands.clone();
        let active = Rc::clone(&self.active_id);
        let active_title = Rc::clone(&self.active_title);
        let window_for_close = window.clone();
        close_button.connect_clicked(move |_| {
            let Some(id) = active.get() else { return };
            confirm(
                &window_for_close,
                "Close note?",
                &format!("“{}” and its text will be deleted.", active_title.borrow()),
                "Close note",
                &commands,
                ScratchpadCommand::Close(id),
            );
        });

        let commands = self.commands.clone();
        let active = Rc::clone(&self.active_id);
        let window_for_export = window.clone();
        export_button.connect_clicked(move |_| {
            let Some(id) = active.get() else { return };
            let commands = commands.clone();
            choose_path(
                &window_for_export,
                "Export note",
                "Export",
                None,
                true,
                move |destination| {
                    send(&commands, ScratchpadCommand::Export { id, destination });
                },
            );
        });

        let commands = self.commands.clone();
        let count = Rc::clone(&self.expired_count);
        let window_for_expired = window.clone();
        self.expired_button.connect_clicked(move |_| {
            confirm(
                &window_for_expired,
                "Remove expired notes?",
                &format!(
                    "{} note(s) past the age limit will be deleted.",
                    count.get()
                ),
                "Remove",
                &commands,
                ScratchpadCommand::RemoveExpired,
            );
        });

        let commands = self.commands.clone();
        let active = Rc::clone(&self.active_id);
        let updating = Rc::clone(&self.updating);
        self.editor.buffer().connect_changed(move |buffer| {
            if updating.get() {
                return;
            }
            if let Some(id) = active.get() {
                let text = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), true)
                    .to_string();
                send(&commands, ScratchpadCommand::Update { id, text });
            }
        });

        // Renames on Enter or when focus leaves, only if the title changed.
        let rename = {
            let commands = self.commands.clone();
            let active = Rc::clone(&self.active_id);
            let active_title = Rc::clone(&self.active_title);
            let entry = self.title.clone();
            move || {
                let Some(id) = active.get() else { return };
                let title = entry.text().trim().to_owned();
                if title.is_empty() || title == *active_title.borrow() {
                    return;
                }
                send(&commands, ScratchpadCommand::Rename { id, title });
            }
        };
        let on_activate = rename.clone();
        self.title.connect_activate(move |_| on_activate());
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| rename());
        self.title.add_controller(focus);

        let preview_frame = self.preview_frame.clone();
        let editor_frame = self.editor_frame.clone();
        self.preview_toggle.connect_toggled(move |toggle| {
            preview_frame.set_visible(toggle.is_active());
            editor_frame.set_visible(!toggle.is_active());
        });
    }

    /// Applies a full model; tabs, title, and text follow the active note.
    pub(super) fn set_model(&self, model: &ScratchpadViewModel) {
        self.updating.set(true);
        self.active_id.set(model.active_id);
        *self.active_title.borrow_mut() = model.active_title.clone();
        self.set_status(model);
        let has_note = model.running && model.active_id.is_some();
        self.actions.set_visible(has_note);
        self.editor_frame
            .set_visible(has_note && !self.preview_toggle.is_active());
        self.preview_frame
            .set_visible(has_note && self.preview_toggle.is_active());
        self.tab_bar.set_visible(model.running);
        if self.title.text() != model.active_title {
            self.title.set_text(&model.active_title);
        }
        let buffer = self.editor.buffer();
        if buffer.text(&buffer.start_iter(), &buffer.end_iter(), true) != model.active_text {
            buffer.set_text(&model.active_text);
        }
        self.preview.set_markup(&model.preview_markup);
        while let Some(child) = self.tabs.first_child() {
            self.tabs.remove(&child);
        }
        for tab in &model.tabs {
            let button = gtk::ToggleButton::with_label(&tab.title);
            button.set_active(tab.active);
            let commands = self.commands.clone();
            let id = tab.id;
            let active = tab.active;
            button.connect_clicked(move |button| {
                if active {
                    // The active tab stays pressed.
                    button.set_active(true);
                } else {
                    send(&commands, ScratchpadCommand::Switch(id));
                }
            });
            self.tabs.append(&button);
        }
        self.expired_count.set(model.expired.len());
        self.expired
            .set_visible(model.running && !model.expired.is_empty());
        self.expired.set_subtitle(&format!(
            "{} note(s) are past the age limit and can be removed.",
            model.expired.len()
        ));
        self.updating.set(false);
    }

    /// Applies the result of typing: preview and status only.
    pub(super) fn set_preview(&self, model: &ScratchpadViewModel) {
        self.preview.set_markup(&model.preview_markup);
        self.set_status(model);
    }

    fn set_status(&self, model: &ScratchpadViewModel) {
        let text = match &model.warning {
            Some(warning) => format!("{} Warning: {warning}", model.status),
            None => model.status.clone(),
        };
        self.status
            .set_subtitle(&gtk::glib::markup_escape_text(&text));
    }
}
