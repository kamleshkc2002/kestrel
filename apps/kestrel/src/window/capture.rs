//! Screenshot controls and recent captures.

use super::widgets::{choose_path, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, CaptureEntryViewModel, CaptureRequest, CaptureViewModel};

/// Builds screenshot controls and the recent-capture list.
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
        let row = plain_row().title("Capture").build();
        for mode in &capture.modes {
            let button = gtk::Button::with_label(mode.label);
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
            row.add_suffix(&button);
        }
        if capture.capturing {
            let cancel = gtk::Button::with_label("Cancel");
            cancel.set_valign(Align::Center);
            cancel.update_property(&[Property::Label("Cancel the screenshot")]);
            let sender = commands.clone();
            cancel.connect_clicked(move |_| {
                let _ = sender.try_send(ApplicationCommand::Capture(CaptureRequest::Cancel));
            });
            row.add_suffix(&cancel);
        }
        group.add(&row);
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
        recent.add_row(&build_capture_row(entry, window, commands));
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
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let row = plain_row()
        .title(&entry.title)
        .subtitle(&entry.subtitle)
        .build();
    let texture = entry.thumbnail.as_ref().and_then(|path| {
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(
            path,
            CAPTURE_THUMBNAIL_SIZE,
            CAPTURE_THUMBNAIL_SIZE,
            true,
        )
        .ok()
    });
    if let Some(pixbuf) = texture {
        let picture = gtk::Picture::for_pixbuf(&pixbuf);
        picture.set_size_request(CAPTURE_THUMBNAIL_SIZE, CAPTURE_THUMBNAIL_SIZE);
        picture.set_can_shrink(true);
        row.add_prefix(&picture);
    }
    let id = entry.id;
    let action = |icon: &str, label: &str, request: CaptureRequest| {
        let button = gtk::Button::from_icon_name(icon);
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
    row.add_suffix(&action(
        "edit-copy-symbolic",
        "Copy",
        CaptureRequest::Copy(id),
    ));
    let save = gtk::Button::from_icon_name("document-save-as-symbolic");
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
    row.add_suffix(&save);
    row.add_suffix(&action(
        "document-edit-symbolic",
        "Edit",
        CaptureRequest::Edit(id),
    ));
    row.add_suffix(&action(
        "user-trash-symbolic",
        "Delete",
        CaptureRequest::Delete(id),
    ));
    row
}
