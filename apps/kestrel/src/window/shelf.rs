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
    // The drop area repeats the prompt, so an empty shelf shows it once.
    let status_text = if model.running && model.items.is_empty() {
        ""
    } else {
        model.status.as_str()
    };
    let status = gtk::Label::new(Some(status_text));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    header.append(&status);
    let open_float = gtk::Button::with_label("Open floating shelf");
    open_float.set_valign(Align::Center);
    open_float.set_sensitive(model.running);
    open_float.set_tooltip_text(Some("Open the shelf beside your other windows"));
    let sender = commands.clone();
    open_float.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::OpenShelfFloat);
    });
    header.append(&open_float);
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
    } else {
        container.append(&build_shelf_entries(model, commands, None));
    }
    container.append(&build_shelf_settings(model, commands));
    attach_shelf_drop_target(&container, commands);
    container
}

/// The empty shelf's drop area; the floating shelf reuses it.
pub(crate) fn build_drop_area() -> gtk::Box {
    let area = gtk::Box::new(Orientation::Vertical, 0);
    area.set_hexpand(true);
    area.set_height_request(200);
    area.add_css_class("card");
    let content = gtk::Box::new(Orientation::Vertical, 8);
    content.set_valign(Align::Center);
    content.set_halign(Align::Center);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let icon = gtk::Image::from_icon_name("folder-download-symbolic");
    icon.set_pixel_size(48);
    icon.set_margin_top(8);
    icon.add_css_class("dim-label");
    content.append(&icon);
    let label = gtk::Label::new(Some("Drop files, text, or links here"));
    label.add_css_class("title-4");
    content.append(&label);
    let hint = gtk::Label::new(Some("They stay until you drag them out again."));
    hint.add_css_class("dim-label");
    content.append(&hint);
    area.append(&content);
    area
}

/// What a row's drag is doing, for a surface that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShelfDragEvent {
    Began,
    Ended,
}

pub(crate) type ShelfDragHook = std::rc::Rc<dyn Fn(ShelfDragEvent)>;

/// The list and drop area together; the floating shelf reuses this.
pub(crate) fn build_shelf_entries(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
    drag_hook: Option<&ShelfDragHook>,
) -> gtk::Box {
    let entries = gtk::Box::new(Orientation::Vertical, 12);
    if !model.items.is_empty() {
        match drag_hook {
            Some(hook) => entries.append(&build_shelf_list_with(model, commands, Some(hook))),
            None => entries.append(&build_shelf_list(model, commands)),
        }
    }
    entries.append(&build_drop_area());
    entries
}

/// The entries as rows.
pub(crate) fn build_shelf_list(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::ListBox {
    build_shelf_list_with(model, commands, None)
}

fn build_shelf_list_with(
    model: &ShelfViewModel,
    commands: &Sender<ApplicationCommand>,
    drag_hook: Option<&ShelfDragHook>,
) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(Align::Start);
    for item in &model.items {
        list.append(&build_shelf_row(item, commands, drag_hook));
    }
    list
}

fn build_shelf_row(
    item: &ShelfItemViewModel,
    commands: &Sender<ApplicationCommand>,
    drag_hook: Option<&ShelfDragHook>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&item.name);
    row.set_subtitle(&item.detail);
    row.set_subtitle_lines(2);
    if !item.available {
        row.add_css_class("dim-label");
    }
    let icon = gtk::Image::from_icon_name(kind_icon(item));
    icon.set_pixel_size(20);
    row.add_prefix(&icon);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} entry {}",
        item.kind_label, item.name
    ))]);

    super::install::add_handoff_button(&row, item, commands);

    let copy = icon_button("edit-copy-symbolic", "Copy this entry", true);
    let sender = commands.clone();
    let id = item.id;
    copy.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CopyShelfItem(id));
    });
    if editable_image(item) {
        let edit = icon_button("document-edit-symbolic", "Open in the capture editor", true);
        let sender = commands.clone();
        let id = item.id;
        edit.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::EditShelfImage(id));
        });
        row.add_suffix(&edit);
    }
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
    if let Some(hook) = drag_hook {
        let began = std::rc::Rc::clone(hook);
        source.connect_drag_begin(move |_, _| began(ShelfDragEvent::Began));
        let ended = std::rc::Rc::clone(hook);
        source.connect_drag_end(move |_, _, _| ended(ShelfDragEvent::Ended));
    }
    row.add_controller(source);
    row
}

/// An image entry, or a file entry the editor can load as an image.
fn editable_image(item: &ShelfItemViewModel) -> bool {
    match &item.content {
        ShelfContent::Image { .. } => true,
        ShelfContent::File { path, .. } => path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp"
                )
            }),
        _ => false,
    }
}

fn kind_icon(item: &ShelfItemViewModel) -> &'static str {
    match item.kind {
        ShelfItemKind::File if item.directory => "folder-symbolic",
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
    let screenshots = adw::SwitchRow::builder()
        .title("Add new screenshots")
        .subtitle("Each screenshot you take is added to the shelf.")
        .active(model.limits.add_screenshots)
        .build();
    let sender = commands.clone();
    screenshots.connect_active_notify(move |row| {
        let _ = sender.try_send(ApplicationCommand::SetShelfAddScreenshots(row.is_active()));
    });
    group.add(&screenshots);
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
    let formats = gdk::ContentFormats::builder()
        .add_type(gdk::FileList::static_type())
        .add_type(gdk::Texture::static_type())
        .add_type(glib::types::Type::STRING)
        .build();
    let target = gtk::DropTargetAsync::new(Some(formats), gdk::DragAction::COPY);
    let sender = commands.clone();
    target.connect_drop(move |_, drop, _, _| {
        let drop = drop.clone();
        let sender = sender.clone();
        glib::spawn_future_local(async move {
            let formats = drop.formats();
            // A browser link offers text/uri-list too, so the URI itself
            // decides between a file reference and a link.
            let offered_files = formats.contains_type(gdk::FileList::static_type())
                || formats.contain_mime_type("text/uri-list");
            let input = if offered_files {
                read_file_list(&drop).await
            } else if formats.contains_type(gdk::Texture::static_type()) {
                read_texture(&drop).await
            } else if formats.contains_type(glib::types::Type::STRING) {
                read_text(&drop).await
            } else {
                None
            };
            if let Some(input) = input {
                let _ = sender.try_send(ApplicationCommand::AddToShelf(input));
            }
            drop.finish(gdk::DragAction::COPY);
        });
        true
    });
    widget.add_controller(target);
}

/// A dropped URI list: local paths become file entries, web URIs links.
async fn read_file_list(drop: &gdk::Drop) -> Option<ShelfInput> {
    let value = drop
        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
        .await
        .ok()?;
    let list = value.get::<gdk::FileList>().ok()?;
    let entries = list
        .files()
        .iter()
        .filter_map(uri_entry)
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(ShelfInput::Entries(entries))
}

fn uri_entry(file: &gio::File) -> Option<ShelfContent> {
    if let Some(path) = file.path() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        return Some(ShelfContent::File { path, name });
    }
    let uri = file.uri().to_string();
    matches!(
        kestrel_core::detect_shelf_content(&uri),
        ShelfContent::Link { .. }
    )
    .then_some(ShelfContent::Link { url: uri })
}

async fn read_texture(drop: &gdk::Drop) -> Option<ShelfInput> {
    let value = drop
        .read_value_future(gdk::Texture::static_type(), glib::Priority::DEFAULT)
        .await
        .ok()?;
    let texture = value.get::<gdk::Texture>().ok()?;
    let png = texture.save_to_png_bytes();
    Some(ShelfInput::Image {
        png: png.as_ref().to_vec(),
        width: texture.width().max(0) as u32,
        height: texture.height().max(0) as u32,
    })
}

async fn read_text(drop: &gdk::Drop) -> Option<ShelfInput> {
    let value = drop
        .read_value_future(glib::types::Type::STRING, glib::Priority::DEFAULT)
        .await
        .ok()?;
    let text = value.get::<String>().ok()?;
    (!text.trim().is_empty()).then_some(ShelfInput::Text(text))
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
