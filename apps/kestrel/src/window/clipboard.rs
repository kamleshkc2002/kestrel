//! Clipboard history panel.

use super::widgets::{SearchGuard, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::ApplicationCommand;

/// Reusable clipboard widgets; search state survives row refreshes.
pub(super) struct ClipboardPanel {
    pub(super) container: gtk::Box,
    pub(super) search: gtk::SearchEntry,
    pub(super) status: gtk::Label,
    pub(super) results: gtk::Box,
    selected: std::rc::Rc<std::cell::RefCell<std::collections::BTreeSet<u64>>>,
}

/// Requests an initial empty search when retained entries lack results.
pub(super) fn request_initial_clipboard_listing(
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if clipboard.running
        && clipboard.search_query.is_empty()
        && clipboard.items.is_empty()
        && clipboard.total_items > 0
    {
        let _ = commands.try_send(ApplicationCommand::ClipboardSearch(String::new()));
    }
}

/// Builds the retained clipboard group.
pub(super) fn build_clipboard_panel(
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) -> ClipboardPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Clipboard")
        .description(
            "Memory-only retention. Content is never written to disk, and the window only \
             renders entries you ask to see.",
        )
        .build();

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search retained entries")
        .hexpand(true)
        .build();
    search.update_property(&[
        Property::Label("Search clipboard history"),
        Property::Description(
            "Searches retained text and file paths plus image labels; the result list updates \
             after you pause typing.",
        ),
    ]);
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row
        .set_subtitle("Text and file paths match case-insensitively; images match on their label.");
    search_row.add_suffix(&search);
    search_row.set_activatable_widget(Some(&search));
    let sender = commands.clone();
    // Guard programmatic field updates from triggering searches.
    let guard: SearchGuard = std::rc::Rc::new(std::cell::Cell::new(false));
    let handler_guard = std::rc::Rc::clone(&guard);
    search.connect_search_changed(move |entry| {
        if handler_guard.get() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::ClipboardSearch(
            entry.text().to_string(),
        ));
    });
    if !clipboard.search_query.is_empty() {
        guard.set(true);
        search.set_text(&clipboard.search_query);
        guard.set(false);
    }
    group.add(&search_row);

    let status = gtk::Label::new(Some(&clipboard.status));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let status_row = adw::ActionRow::new();
    status_row.set_title("Retained state");
    status_row.add_suffix(&status);
    group.add(&status_row);

    let actions = plain_row()
        .title("History actions")
        .subtitle(
            "Clear the live selection without touching entries, or wipe every retained entry.",
        )
        .subtitle_lines(0)
        .build();
    let action_box = gtk::Box::new(Orientation::Horizontal, 6);
    let clear = gtk::Button::with_label("Clear selection");
    clear.set_tooltip_text(Some("Clear the live clipboard; retained entries stay"));
    let sender = commands.clone();
    clear.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::ClearSelection,
        ));
    });
    let wipe = gtk::Button::with_label("Wipe history");
    wipe.set_tooltip_text(Some(
        "Clear the live clipboard and drop every retained entry",
    ));
    let sender = commands.clone();
    wipe.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::Wipe,
        ));
    });
    action_box.append(&clear);
    action_box.append(&wipe);
    actions.add_suffix(&action_box);
    group.add(&actions);

    let results = gtk::Box::new(Orientation::Vertical, 6);
    request_initial_clipboard_listing(clipboard, commands);
    let selected = std::rc::Rc::new(std::cell::RefCell::new(std::collections::BTreeSet::new()));
    let panel = ClipboardPanel {
        container: container.clone(),
        search: search.clone(),
        status,
        results: results.clone(),
        selected: std::rc::Rc::clone(&selected),
    };
    fill_clipboard_results(&panel, clipboard, commands);
    group.add(&results);
    container.append(&group);

    if !clipboard.running {
        let row = plain_row()
            .title("Clipboard history is not running")
            .subtitle(&clipboard.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("Clipboard history is not running")]);
        group.add(&row);
        return panel;
    }

    let delete = gtk::Button::with_label("Delete selected");
    let selected_for_delete = std::rc::Rc::clone(&selected);
    let sender = commands.clone();
    let delete_row = plain_row()
        .title("Multiple selection")
        .subtitle("Tick entries in the list and delete them together.")
        .build();
    delete.set_tooltip_text(Some("Delete every ticked entry"));
    delete.connect_clicked(move |_| {
        let ids = selected_for_delete
            .borrow()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            kestrel::ClipboardCommand::DeleteItems { item_ids: ids },
        ));
    });
    delete_row.add_suffix(&delete);
    group.add(&delete_row);

    panel
}

/// Rebuilds clipboard result rows.
pub(super) fn fill_clipboard_results(
    panel: &ClipboardPanel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if clipboard.items.is_empty() {
        let row = plain_row()
            .title(if clipboard.search_query.trim().is_empty() {
                "No retained entries"
            } else {
                "No entries match the search"
            })
            .subtitle(if clipboard.running {
                "Copy something, or widen the search."
            } else {
                "Enable clipboard.history in the Feature Hub to retain entries."
            })
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No clipboard entries to show")]);
        panel.results.append(&row);
        return;
    }

    for item in &clipboard.items {
        let row = plain_row()
            .title(format!("{} · {}", item.kind, item.detail))
            .subtitle(if item.preview.is_empty() {
                "No inline preview for this entry kind".to_owned()
            } else {
                item.preview.clone()
            })
            .subtitle_lines(2)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{} entry: {}",
            item.kind, item.preview
        ))]);

        let select = gtk::CheckButton::new();
        let select_label = format!("Select entry {}", item.id);
        select.update_property(&[Property::Label(&select_label)]);
        select.set_tooltip_text(Some(&format!("Select entry {} for deletion", item.id)));
        let selected = std::rc::Rc::clone(&panel.selected);
        let id = item.id;
        select.connect_toggled(move |button| {
            let mut selected = selected.borrow_mut();
            if button.is_active() {
                selected.insert(id);
            } else {
                selected.remove(&id);
            }
        });
        row.add_suffix(&select);

        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let pin = gtk::Button::with_label(if item.pinned { "Unpin" } else { "Pin" });
        let pin_label = format!(
            "{} entry {}",
            if item.pinned { "Unpin" } else { "Pin" },
            item.id
        );
        pin.update_property(&[Property::Label(&pin_label)]);
        pin.set_tooltip_text(Some(&pin_label));
        let sender = commands.clone();
        let id = item.id;
        let pinned = item.pinned;
        pin.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::SetPinned {
                    item_id: id,
                    pinned: !pinned,
                },
            ));
        });
        controls.append(&pin);

        let copy = gtk::Button::with_label("Copy");
        copy.update_property(&[Property::Label(&format!("Copy entry {}", item.id))]);
        copy.set_tooltip_text(Some("Put this entry on the clipboard"));
        let sender = commands.clone();
        let id = item.id;
        copy.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::CopyItem {
                    item_id: id,
                    plain_text: false,
                },
            ));
        });
        controls.append(&copy);

        let plain = gtk::Button::with_label("Plain");
        plain.update_property(&[Property::Label(&format!(
            "Copy entry {} as plain text",
            item.id
        ))]);
        plain.set_tooltip_text(Some(
            "Copy without ANSI escapes or trailing whitespace; file entries copy as paths",
        ));
        let sender = commands.clone();
        let id = item.id;
        plain.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::CopyItem {
                    item_id: id,
                    plain_text: true,
                },
            ));
        });
        controls.append(&plain);

        let preview = gtk::Button::with_label("Preview");
        preview.update_property(&[Property::Label(&format!("Preview entry {}", item.id))]);
        preview.set_tooltip_text(Some("Show a bounded, explicit preview of this entry"));
        let sender = commands.clone();
        let id = item.id;
        preview.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ClipboardPreview(id));
        });
        controls.append(&preview);

        let delete = gtk::Button::with_label("Delete");
        delete.update_property(&[Property::Label(&format!("Delete entry {}", item.id))]);
        delete.set_tooltip_text(Some("Drop this retained entry"));
        let sender = commands.clone();
        let id = item.id;
        delete.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::DeleteItems { item_ids: vec![id] },
            ));
        });
        controls.append(&delete);
        row.add_suffix(&controls);
        panel.results.append(&row);

        if let Some(preview) = clipboard
            .preview
            .as_ref()
            .filter(|preview| preview.id == item.id)
        {
            panel
                .results
                .append(&build_clipboard_preview_row(preview, commands));
        }
    }
}

/// Builds the explicitly requested preview row.
fn build_clipboard_preview_row(
    preview: &kestrel::ClipboardPreviewViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let mut description = vec![format!("{} entry", preview.kind)];
    if preview.truncated {
        description.push("preview truncated to the request bound".to_owned());
    }
    if let Some((width, height)) = preview.image_dimensions {
        description.push(format!("{width}×{height}"));
    }
    if let Some(count) = preview.file_count {
        description.push(format!("{count} paths"));
    }
    if preview.text.is_empty() {
        description.push("no inline text for this kind".to_owned());
    }
    let row = plain_row()
        .title(format!("Preview {}", preview.id))
        .subtitle(description.join(" · "))
        .subtitle_lines(0)
        .build();
    row.update_property(&[Property::Label(&format!("Preview of entry {}", preview.id))]);
    if !preview.text.is_empty() {
        let text = gtk::Label::new(Some(&preview.text));
        text.set_wrap(true);
        text.set_xalign(0.0);
        text.set_selectable(true);
        text.set_valign(Align::Center);
        row.add_suffix(&text);
    }
    if preview.editable {
        let edit = gtk::Entry::builder()
            .text(&preview.text)
            .hexpand(true)
            .build();
        edit.update_property(&[Property::Label(&format!("Edit entry {} text", preview.id))]);
        let save = gtk::Button::with_label("Save edit");
        let sender = commands.clone();
        let id = preview.id;
        let edit_for_save = edit.clone();
        save.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Clipboard(
                kestrel::ClipboardCommand::ReplaceText {
                    item_id: id,
                    text: edit_for_save.text().to_string(),
                },
            ));
        });
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        controls.append(&edit);
        controls.append(&save);
        row.add_suffix(&controls);
    }
    row
}
