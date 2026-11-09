//! Screenshot controls and recent captures.

use super::widgets::{
    action_box, choose_path, plain_row, stacked_row, stacked_row_with_prefix, turn_on_row,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, CaptureEntryViewModel, CaptureRequest, CaptureViewModel};

pub(super) fn build_capture_group(
    capture: &CaptureViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Screenshots")
        .description(gtk::glib::markup_escape_text(&capture.status).as_str())
        .build();
    if capture.running {
        // One button per mode; wrapping keeps the row inside a narrow window.
        let modes = action_box(4);
        for mode in &capture.modes {
            let button = gtk::Button::with_label(mode.label);
            button.set_halign(Align::Start);
            button.set_valign(Align::Center);
            button.set_sensitive(!capture.capturing);
            button.update_property(&[Property::Label(&format!("Capture: {}", mode.label))]);
            let sender = commands.clone();
            let mode = mode.mode;
            button.connect_clicked(move |_| {
                let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::Begin(Some(
                    mode,
                ))));
            });
            modes.append(&button);
        }
        if capture.capturing {
            let cancel = gtk::Button::with_label("Cancel");
            cancel.set_halign(Align::Start);
            cancel.set_valign(Align::Center);
            cancel.update_property(&[Property::Label("Cancel the screenshot")]);
            let sender = commands.clone();
            cancel.connect_clicked(move |_| {
                let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::Cancel));
            });
            modes.append(&cancel);
        }
        group.add(&stacked_row(
            "Capture",
            "Choose how to capture the screen.",
            &modes,
        ));
    }
    if let Some(turn_on) = &capture.turn_on {
        group.add(&turn_on_row(turn_on, commands));
    }
    if let Some(error) = &capture.storage_error {
        group.add(
            &plain_row()
                .title("Capture storage is unavailable")
                .subtitle(error)
                .subtitle_lines(0)
                .build(),
        );
    }
    if !capture.running {
        return group;
    }
    let recent = adw::ExpanderRow::builder()
        .title("Recent captures")
        .subtitle(&capture.usage)
        .expanded(!capture.entries.is_empty())
        .build();
    if capture.entries.is_empty() {
        recent.add_row(&plain_row().title("No captures yet").build());
    }
    for entry in &capture.entries {
        recent.add_row(&build_capture_row(
            entry,
            capture.ocr_unavailable.as_deref(),
            window,
            commands,
        ));
    }
    if !capture.entries.is_empty() {
        let clear = gtk::Button::with_label("Clear all");
        clear.add_css_class("destructive-action");
        clear.set_valign(Align::Center);
        clear.update_property(&[Property::Label("Delete every recent capture")]);
        let sender = commands.clone();
        clear.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::Clear));
        });
        recent.add_suffix(&clear);
    }
    group.add(&recent);
    group
}

const CAPTURE_THUMBNAIL_SIZE: i32 = 72;

fn build_capture_row(
    entry: &CaptureEntryViewModel,
    ocr_unavailable: Option<&str>,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesRow {
    let actions = action_box(5);
    let id = entry.id;
    let action = |icon: &str, label: &str, request: CaptureRequest| {
        let button = gtk::Button::from_icon_name(icon);
        button.set_halign(Align::Start);
        button.set_valign(Align::Center);
        button.add_css_class("flat");
        button.set_tooltip_text(Some(label));
        button.update_property(&[Property::Label(label)]);
        let sender = commands.clone();
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Capture(request.clone()));
        });
        button
    };
    actions.append(&action(
        "edit-copy-symbolic",
        "Copy",
        CaptureRequest::Copy(id),
    ));
    let copy_text = gtk::Button::with_label("Copy text");
    copy_text.set_halign(Align::Start);
    copy_text.set_valign(Align::Center);
    copy_text.add_css_class("flat");
    copy_text.set_sensitive(ocr_unavailable.is_none());
    copy_text.set_tooltip_text(Some(match ocr_unavailable {
        Some(reason) => reason,
        None => "Read this capture's text locally and copy it",
    }));
    copy_text.update_property(&[Property::Label("Copy text from this capture")]);
    let sender = commands.clone();
    copy_text.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::CopyText(id)));
    });
    actions.append(&copy_text);
    let save = gtk::Button::from_icon_name("document-save-as-symbolic");
    save.set_halign(Align::Start);
    save.set_valign(Align::Center);
    save.add_css_class("flat");
    save.set_tooltip_text(Some("Save as…"));
    save.update_property(&[Property::Label("Save as")]);
    let parent = window.clone();
    let sender = commands.clone();
    save.connect_clicked(move |_| {
        let sender = sender.clone();
        let name = format!("Screenshot-{}.png", id.get());
        choose_path(
            &parent,
            "Save screenshot",
            "Save",
            Some(&name),
            true,
            move |destination| {
                let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::Save {
                    id,
                    destination,
                }));
            },
        );
    });
    actions.append(&save);
    actions.append(&action(
        "document-edit-symbolic",
        "Edit",
        CaptureRequest::Edit(id),
    ));
    actions.append(&action(
        "user-trash-symbolic",
        "Delete",
        CaptureRequest::Delete(id),
    ));
    let thumbnail = entry.thumbnail.as_ref().and_then(|path| {
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(
            path,
            CAPTURE_THUMBNAIL_SIZE,
            CAPTURE_THUMBNAIL_SIZE,
            true,
        )
        .ok()
    });
    match thumbnail {
        Some(pixbuf) => {
            let image = gtk::Image::from_pixbuf(Some(&pixbuf));
            image.set_pixel_size(CAPTURE_THUMBNAIL_SIZE);
            image.set_valign(Align::Start);
            stacked_row_with_prefix(&entry.title, &entry.subtitle, &image, &actions)
        }
        None => stacked_row(&entry.title, &entry.subtitle, &actions),
    }
}
