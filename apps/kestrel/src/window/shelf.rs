//! Shelf page: staged entries with drag and drop.

use super::widgets::{icon_button, plain_row, turn_on_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, gdk, gio, glib};
use kestrel::ApplicationCommand;
use kestrel::view_model::{ShelfItemViewModel, ShelfViewModel};
use kestrel_core::{ShelfContent, ShelfItemKind};
use kestrel_services::shelf::ShelfInput;

/// The shelf's list and drop area, rebuilt from the view model each time.
pub(super) fn build_shelf_page(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let container = gtk::Box::new(Orientation::Vertical, 18);
    let header = gtk::Box::new(Orientation::Horizontal, 12);
    let status = gtk::Label::new(Some(&model.status));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    header.append(&status);
    let clear = gtk::Button::with_label("Clear…");
    clear.set_valign(Align::Center);
    clear.set_sensitive(model.running && !model.items.is_empty());
    clear.set_tooltip_text(Some("Remove entries from the shelf"));
    connect_clear(&clear, model.unpinned, model.items.len(), commands);
    header.append(&clear);
    container.append(&header);

    if let Some(warning) = &model.warning {
        let row = plain_row()
            .title("Shelf warning")
            .subtitle(warning)
            .subtitle_lines(0)
            .build();
        row.add_css_class("warning");
        container.append(&row);
    }

    if !model.running {
        if let Some(turn_on) = &model.turn_on {
            container.append(&turn_on_row(turn_on, commands));
        }
    } else if model.items.is_empty() {
        container.append(&build_drop_area());
    } else {
        container.append(&build_shelf_list(model, commands));
        container.append(&build_drop_area());
    }
    container.append(&build_shelf_settings(model, commands));
    attach_shelf_drop_target(&container, commands);
    container
}

/// The empty shelf's drop area; the floating shelf reuses it.
pub(crate) fn build_drop_area() -> gtk::Box {
    let area = gtk::Box::new(Orientation::Vertical, 8);
    area.set_valign(Align::Center);
    area.set_halign(Align::Center);
    area.set_height_request(160);
    area.set_hexpand(true);
    area.add_css_class("card");
    let icon = gtk::Image::from_icon_name("folder-download-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("dim-label");
    area.append(&icon);
    let label = gtk::Label::new(Some("Drop files, text, or links here"));
    label.add_css_class("title-4");
    area.append(&label);
    let hint = gtk::Label::new(Some("They stay until you drag them out again."));
    hint.add_css_class("dim-label");
    area.append(&hint);
    area
}

/// The entries as rows; part 2's floating shelf reuses this list.
pub(crate) fn build_shelf_list(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(Align::Start);
    for item in &model.items {
        list.append(&build_shelf_row(item, commands));
    }
    list
}

fn build_shelf_row(
    item: &ShelfItemViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&item.name);
    row.set_subtitle(&item.detail);
    row.set_subtitle_lines(2);
    if !item.available {
        row.add_css_class("dim-label");
    }
    let icon = gtk::Image::from_icon_name(kind_icon(item.kind));
    icon.set_pixel_size(20);
    row.add_prefix(&icon);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} entry {}",
        item.kind_label, item.name
    ))]);

    let copy = icon_button("edit-copy-symbolic", "Copy this entry", true);
    let sender = commands.clone();
    let id = item.id;
    copy.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CopyShelfItem(id));
    });
    let pin = icon_button(
        "view-pin-symbolic",
        if item.pinned {
            "Unpin this entry"
        } else {
            "Keep this entry when the shelf is cleared"
        },
        true,
    );
    if item.pinned {
        pin.add_css_class("accent");
    }
    let sender = commands.clone();
    let id = item.id;
    let pinned = item.pinned;
    pin.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::PinShelfItem(id, !pinned));
    });
    let remove = icon_button("user-trash-symbolic", "Remove this entry", true);
    let sender = commands.clone();
    let id = item.id;
    remove.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::RemoveShelfItem(id));
    });
    for button in [copy, pin, remove] {
        row.add_suffix(&button);
    }

    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY);
    let content = item.content.clone();
    source.connect_prepare(move |_, _, _| content_provider(&content));
    row.add_controller(source);
    row
}

fn kind_icon(kind: ShelfItemKind) -> &'static str {
    match kind {
        ShelfItemKind::File => "text-x-generic-symbolic",
        ShelfItemKind::Text => "accessories-text-editor-symbolic",
        ShelfItemKind::Link => "web-browser-symbolic",
        ShelfItemKind::Image => "image-x-generic-symbolic",
    }
}

fn build_shelf_settings(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Shelf settings")
        .build();
    let keep = adw::SwitchRow::builder()
        .title("Keep across restarts")
        .subtitle(
            "Saves links, text, and file locations in a private file. Images are never saved.",
        )
        .active(model.keep)
        .build();
    let sender = commands.clone();
    keep.connect_active_notify(move |row| {
        let _ = sender.try_send(ApplicationCommand::SetShelfKeep(row.is_active()));
    });
    group.add(&keep);
    if !model.keep {
        group.add(
            &plain_row()
                .title("Cleared on quit.")
                .subtitle("Entries last only while the application runs.")
                .sensitive(false)
                .build(),
        );
    }
    group
}

/// Confirms clearing; pinned entries are kept unless "Clear all" is chosen.
fn connect_clear(
    button: &gtk::Button,
    unpinned: usize,
    total: usize,
    commands: &Sender<ApplicationCommand>,
) {
    let commands = commands.clone();
    button.connect_clicked(move |button| {
        if total == 0 {
            return;
        }
        let (heading, body) = if unpinned == 0 {
            (
                "Clear every shelf entry?",
                format!(
                    "All {total} entries are pinned. Every entry is removed, pinned ones included."
                ),
            )
        } else {
            let entries = if total == 1 { "entry" } else { "entries" };
            (
                "Clear unpinned shelf entries?",
                format!(
                    "{unpinned} unpinned of {total} {entries} are removed. Pinned entries stay, \
                     and anything added from now on is kept."
                ),
            )
        };
        let dialog = adw::MessageDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&window));
        }
        let mut responses = vec![("cancel", "Cancel")];
        if unpinned > 0 {
            responses.push(("clear-unpinned", "Clear unpinned"));
        }
        responses.push(("clear-all", "Clear all"));
        dialog.add_responses(&responses);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("clear-all", adw::ResponseAppearance::Destructive);
        let commands = commands.clone();
        dialog.connect_response(None, move |dialog, response| {
            let include_pinned = match response {
                "clear-unpinned" => Some(false),
                "clear-all" => Some(true),
                _ => None,
            };
            if let Some(include_pinned) = include_pinned {
                let _ = commands.try_send(ApplicationCommand::ClearShelf { include_pinned });
            }
            dialog.close();
        });
        dialog.present();
    });
}

/// Accepts everything the shelf stages, reading each value asynchronously so
/// the source finishes delivering it without blocking the window.
pub(crate) fn attach_shelf_drop_target(
    widget: &impl IsA<gtk::Widget>,
    commands: &Sender<ApplicationCommand>,
) {
    attach_files_drop(widget, commands);
    attach_text_drop(widget, commands);
    attach_texture_drop(widget, commands);
}

fn attach_files_drop<T: IsA<gtk::Widget>>(widget: &T, commands: &Sender<ApplicationCommand>) {
    let target = gtk::DropTargetAsync::new(
        Some(gdk::ContentFormats::for_type(gdk::FileList::static_type())),
        gdk::DragAction::COPY,
    );
    let sender = commands.clone();
    target.connect_drop(move |_, drop, _, _| {
        let drop = drop.clone();
        let sender = sender.clone();
        glib::spawn_future_local(async move {
            let value = drop
                .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                .await;
            if let Ok(value) = value {
                if let Ok(list) = value.get::<gdk::FileList>() {
                    let paths = list
                        .files()
                        .iter()
                        .filter_map(gio::File::path)
                        .collect::<Vec<_>>();
                    if !paths.is_empty() {
                        let _ = sender
                            .try_send(ApplicationCommand::AddToShelf(ShelfInput::Files(paths)));
                    }
                }
            }
            drop.finish(gdk::DragAction::COPY);
        });
        true
    });
    widget.add_controller(target);
}

fn attach_text_drop<T: IsA<gtk::Widget>>(widget: &T, commands: &Sender<ApplicationCommand>) {
    let target = gtk::DropTargetAsync::new(
        Some(gdk::ContentFormats::for_type(glib::types::Type::STRING)),
        gdk::DragAction::COPY,
    );
    let sender = commands.clone();
    target.connect_drop(move |_, drop, _, _| {
        let drop = drop.clone();
        let sender = sender.clone();
        glib::spawn_future_local(async move {
            let value = drop
                .read_value_future(glib::types::Type::STRING, glib::Priority::DEFAULT)
                .await;
            if let Ok(value) = value {
                if let Ok(text) = value.get::<String>() {
                    if !text.trim().is_empty() {
                        let _ =
                            sender.try_send(ApplicationCommand::AddToShelf(ShelfInput::Text(text)));
                    }
                }
            }
            drop.finish(gdk::DragAction::COPY);
        });
        true
    });
    widget.add_controller(target);
}

fn attach_texture_drop<T: IsA<gtk::Widget>>(widget: &T, commands: &Sender<ApplicationCommand>) {
    let target = gtk::DropTargetAsync::new(
        Some(gdk::ContentFormats::for_type(gdk::Texture::static_type())),
        gdk::DragAction::COPY,
    );
    let sender = commands.clone();
    target.connect_drop(move |_, drop, _, _| {
        let drop = drop.clone();
        let sender = sender.clone();
        glib::spawn_future_local(async move {
            let value = drop
                .read_value_future(gdk::Texture::static_type(), glib::Priority::DEFAULT)
                .await;
            if let Ok(value) = value {
                if let Ok(texture) = value.get::<gdk::Texture>() {
                    let png = texture.save_to_png_bytes();
                    let _ = sender.try_send(ApplicationCommand::AddToShelf(ShelfInput::Image {
                        png: png.as_ref().to_vec(),
                        width: texture.width().max(0) as u32,
                        height: texture.height().max(0) as u32,
                    }));
                }
            }
            drop.finish(gdk::DragAction::COPY);
        });
        true
    });
    widget.add_controller(target);
}

/// Copies one entry through the window's clipboard.
pub(crate) fn copy_shelf_content(widget: &impl IsA<gtk::Widget>, content: &ShelfContent) -> bool {
    let Some(provider) = content_provider(content) else {
        return false;
    };
    widget.clipboard().set_content(Some(&provider)).is_ok()
}

/// The typed value an entry offers to a copy or drag.
fn content_provider(content: &ShelfContent) -> Option<gdk::ContentProvider> {
    match content {
        ShelfContent::File { path, .. } => {
            let list = gdk::FileList::from_array(&[gio::File::for_path(path)]);
            Some(gdk::ContentProvider::for_value(&list.to_value()))
        }
        ShelfContent::Text(text) => Some(gdk::ContentProvider::for_value(&text.to_value())),
        ShelfContent::Link { url } => {
            let text = gdk::ContentProvider::for_value(&url.to_value());
            let uri_list = gdk::ContentProvider::for_bytes(
                "text/uri-list",
                &glib::Bytes::from(format!("{url}\r\n").as_bytes()),
            );
            Some(gdk::ContentProvider::new_union(&[text, uri_list]))
        }
        ShelfContent::Image { png, .. } => {
            let texture = gdk::Texture::from_bytes(&glib::Bytes::from(png.as_slice())).ok()?;
            Some(gdk::ContentProvider::for_value(&texture.to_value()))
        }
    }
}
