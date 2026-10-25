//! Clipboard history panel.

use super::widgets::{SearchGuard, plain_row, stacked_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, gdk, glib};
use kestrel::{ApplicationCommand, ClipboardCommand, ClipboardItemViewModel, ClipboardLayout};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    rc::Rc,
};

/// Counts behind the clear prompts; a confirmed clear stops at `newest_id`.
#[derive(Clone, Copy, Default)]
struct ClearCounts {
    newest_id: Option<u64>,
    total: usize,
    pinned: usize,
}

/// The rendered results and the entry ID behind each selectable child.
#[derive(Clone)]
enum ResultsView {
    List(gtk::ListBox, Rc<Vec<Option<u64>>>),
    Cards(gtk::FlowBox, Rc<Vec<u64>>),
}

pub(super) struct ClipboardPanel {
    pub(super) container: gtk::Box,
    pub(super) search: gtk::SearchEntry,
    pub(super) status: gtk::Label,
    results: gtk::Box,
    /// Entries ticked for deletion.
    ticked: Rc<RefCell<BTreeSet<u64>>>,
    /// The entry Return pastes; kept by ID so new copies do not move it.
    selected: Rc<Cell<Option<u64>>>,
    view: Rc<RefCell<Option<ResultsView>>>,
    counts: Rc<Cell<ClearCounts>>,
    clear_unpinned: gtk::Button,
    wipe: gtk::Button,
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

fn copy_entry(commands: &Sender<ApplicationCommand>, item_id: u64, plain_text: bool) {
    let _ = commands.try_send(ApplicationCommand::Clipboard(ClipboardCommand::CopyItem {
        item_id,
        plain_text,
    }));
}

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
            "Searches retained text and file paths plus image labels. Ctrl+N and Ctrl+P move \
             through results; Return copies the selected or first result.",
        ),
    ]);
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.set_subtitle(
        "Matches are highlighted. Ctrl+N and Ctrl+P move through results; Return copies one.",
    );
    search_row.add_suffix(&search);
    search_row.set_activatable_widget(Some(&search));
    let sender = commands.clone();
    // Guard programmatic field updates from triggering searches.
    let guard: SearchGuard = Rc::new(Cell::new(false));
    let handler_guard = Rc::clone(&guard);
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

    let counts = Rc::new(Cell::new(ClearCounts::default()));
    let action_box = gtk::Box::new(Orientation::Horizontal, 6);
    let clear = gtk::Button::with_label("Clear selection");
    clear.set_valign(Align::Center);
    clear.set_tooltip_text(Some("Clear the live clipboard; retained entries stay"));
    let sender = commands.clone();
    clear.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            ClipboardCommand::ClearSelection,
        ));
    });
    let clear_unpinned = gtk::Button::with_label("Clear unpinned…");
    clear_unpinned.set_valign(Align::Center);
    clear_unpinned.set_tooltip_text(Some("Remove every entry that is not pinned"));
    connect_clear(&clear_unpinned, &counts, commands, true);
    let wipe = gtk::Button::with_label("Wipe history…");
    wipe.set_valign(Align::Center);
    wipe.add_css_class("destructive-action");
    wipe.set_tooltip_text(Some("Remove every retained entry, pinned ones included"));
    connect_clear(&wipe, &counts, commands, false);
    action_box.append(&clear);
    action_box.append(&clear_unpinned);
    action_box.append(&wipe);
    let actions = stacked_row(
        "History actions",
        "Removing entries asks first; anything copied while it asks is kept.",
        &action_box,
    );
    group.add(&actions);

    let results = gtk::Box::new(Orientation::Vertical, 6);
    request_initial_clipboard_listing(clipboard, commands);
    let panel = ClipboardPanel {
        container: container.clone(),
        search: search.clone(),
        status,
        results: results.clone(),
        ticked: Rc::new(RefCell::new(BTreeSet::new())),
        selected: Rc::new(Cell::new(None)),
        view: Rc::new(RefCell::new(None)),
        counts,
        clear_unpinned,
        wipe,
    };
    connect_navigation(
        &search,
        &panel.view,
        &panel.selected,
        commands,
        /* activates_on_return */ true,
    );
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
    delete.set_valign(Align::Center);
    let ticked = Rc::clone(&panel.ticked);
    let sender = commands.clone();
    let delete_row = plain_row()
        .title("Multiple selection")
        .subtitle("Tick entries and delete them together.")
        .build();
    delete.set_tooltip_text(Some("Delete every ticked entry"));
    delete.connect_clicked(move |_| {
        let ids = ticked.borrow().iter().copied().collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            ClipboardCommand::DeleteItems { item_ids: ids },
        ));
    });
    delete_row.add_suffix(&delete);
    group.add(&delete_row);

    panel
}

/// Confirms a clear with the counts at prompt time; entries captured later survive.
fn connect_clear(
    button: &gtk::Button,
    counts: &Rc<Cell<ClearCounts>>,
    commands: &Sender<ApplicationCommand>,
    keep_pinned: bool,
) {
    let counts = Rc::clone(counts);
    let commands = commands.clone();
    button.connect_clicked(move |button| {
        let snapshot = counts.get();
        let Some(through_id) = snapshot.newest_id else {
            return;
        };
        let removed = if keep_pinned {
            snapshot.total.saturating_sub(snapshot.pinned)
        } else {
            snapshot.total
        };
        if removed == 0 {
            return;
        }
        let entries = if removed == 1 { "entry" } else { "entries" };
        let (heading, body, action) = if keep_pinned {
            (
                "Clear unpinned entries?",
                format!(
                    "{removed} {entries} will be removed. Pinned entries and anything copied \
                     from now on are kept."
                ),
                "Clear unpinned",
            )
        } else {
            (
                "Wipe clipboard history?",
                format!(
                    "{removed} {entries} will be removed, pinned ones included, and the live \
                     clipboard is cleared if it holds one of them. Anything copied from now on \
                     is kept."
                ),
                "Wipe history",
            )
        };
        let dialog = adw::MessageDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&window));
        }
        dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        let commands = commands.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "confirm" {
                let _ = commands.try_send(ApplicationCommand::Clipboard(
                    ClipboardCommand::ClearEntries {
                        through_id,
                        keep_pinned,
                    },
                ));
            }
            dialog.close();
        });
        dialog.present();
    });
}

/// Ctrl+N and Ctrl+P move the selection; Return in the search copies the
/// selected entry, or the first one when nothing is selected.
fn connect_navigation(
    widget: &impl IsA<gtk::Widget>,
    view: &Rc<RefCell<Option<ResultsView>>>,
    selected: &Rc<Cell<Option<u64>>>,
    commands: &Sender<ApplicationCommand>,
    activates_on_return: bool,
) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let navigation_view = Rc::clone(view);
    let navigation_selected = Rc::clone(selected);
    let sender = commands.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let control = state.contains(gdk::ModifierType::CONTROL_MASK);
        let step = match key {
            gdk::Key::n | gdk::Key::N if control => 1,
            gdk::Key::p | gdk::Key::P if control => -1,
            gdk::Key::Return | gdk::Key::KP_Enter if activates_on_return => {
                let target = navigation_selected
                    .get()
                    .or_else(|| first_entry(&navigation_view.borrow()));
                if let Some(item_id) = target {
                    copy_entry(&sender, item_id, false);
                }
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Proceed,
        };
        move_selection(&navigation_view.borrow(), &navigation_selected, step);
        glib::Propagation::Stop
    });
    widget.add_controller(keys);
}

fn first_entry(view: &Option<ResultsView>) -> Option<u64> {
    match view.as_ref()? {
        ResultsView::List(_, ids) => ids.iter().flatten().next().copied(),
        ResultsView::Cards(_, ids) => ids.first().copied(),
    }
}

fn move_selection(view: &Option<ResultsView>, selected: &Cell<Option<u64>>, step: i32) {
    let Some(view) = view else {
        return;
    };
    let entries: Vec<(i32, u64)> = match view {
        ResultsView::List(_, ids) => ids
            .iter()
            .enumerate()
            .filter_map(|(index, id)| id.map(|id| (index as i32, id)))
            .collect(),
        ResultsView::Cards(_, ids) => ids
            .iter()
            .enumerate()
            .map(|(index, id)| (index as i32, *id))
            .collect(),
    };
    if entries.is_empty() {
        return;
    }
    let current = selected
        .get()
        .and_then(|id| entries.iter().position(|(_, entry)| *entry == id));
    let next = match current {
        None => 0,
        Some(position) => (position as i32 + step).clamp(0, entries.len() as i32 - 1) as usize,
    };
    let (index, id) = entries[next];
    select_index(view, index);
    selected.set(Some(id));
}

fn select_index(view: &ResultsView, index: i32) {
    match view {
        ResultsView::List(list, _) => {
            if let Some(row) = list.row_at_index(index) {
                list.select_row(Some(&row));
            }
        }
        ResultsView::Cards(flow, _) => {
            if let Some(child) = flow.child_at_index(index) {
                flow.select_child(&child);
            }
        }
    }
}

/// Escapes `text` for Pango and emboldens the matched byte ranges.
fn highlighted_markup(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut markup = String::with_capacity(text.len() + ranges.len() * 7);
    let mut cursor = 0;
    for &(start, end) in ranges {
        if start < cursor
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        markup.push_str(&glib::markup_escape_text(&text[cursor..start]));
        markup.push_str("<b>");
        markup.push_str(&glib::markup_escape_text(&text[start..end]));
        markup.push_str("</b>");
        cursor = end;
    }
    markup.push_str(&glib::markup_escape_text(&text[cursor..]));
    markup
}

fn preview_markup(item: &ClipboardItemViewModel) -> String {
    if item.preview.is_empty() {
        glib::markup_escape_text("No inline preview for this entry kind").to_string()
    } else {
        highlighted_markup(&item.preview, &item.highlights)
    }
}

pub(super) fn fill_clipboard_results(
    panel: &ClipboardPanel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    panel.counts.set(ClearCounts {
        newest_id: clipboard.newest_id,
        total: clipboard.total_items,
        pinned: clipboard.pinned_items,
    });
    panel
        .clear_unpinned
        .set_sensitive(clipboard.running && clipboard.total_items > clipboard.pinned_items);
    panel
        .wipe
        .set_sensitive(clipboard.running && clipboard.total_items > 0);

    // A refresh rebuilds the results; keyboard focus returns to the selected entry.
    let had_focus = panel
        .results
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus.is_ancestor(&panel.results));
    while let Some(child) = panel.results.first_child() {
        child.unparent();
    }
    panel.view.replace(None);

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
        panel.selected.set(None);
        return;
    }

    let view = match clipboard.layout {
        ClipboardLayout::List => build_list(panel, clipboard, commands),
        ClipboardLayout::Cards => build_cards(panel, clipboard, commands),
    };
    connect_navigation(
        match &view {
            ResultsView::List(list, _) => list.upcast_ref::<gtk::Widget>(),
            ResultsView::Cards(flow, _) => flow.upcast_ref::<gtk::Widget>(),
        },
        &panel.view,
        &panel.selected,
        commands,
        false,
    );

    let kept = panel.selected.get().and_then(|id| {
        let index = match &view {
            ResultsView::List(_, ids) => ids.iter().position(|entry| *entry == Some(id)),
            ResultsView::Cards(_, ids) => ids.iter().position(|entry| *entry == id),
        }?;
        Some(index as i32)
    });
    match kept {
        Some(index) => {
            select_index(&view, index);
            if had_focus {
                match &view {
                    ResultsView::List(list, _) => {
                        if let Some(row) = list.row_at_index(index) {
                            row.grab_focus();
                        }
                    }
                    ResultsView::Cards(flow, _) => {
                        if let Some(child) = flow.child_at_index(index) {
                            child.grab_focus();
                        }
                    }
                }
            }
        }
        None => panel.selected.set(None),
    }
    panel.view.replace(Some(view));
}

fn build_list(
    panel: &ClipboardPanel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) -> ResultsView {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .build();
    list.add_css_class("boxed-list");
    list.update_property(&[Property::Label("Clipboard entries")]);
    let mut ids = Vec::new();
    for item in &clipboard.items {
        let row = adw::ActionRow::builder()
            .use_markup(true)
            .title(glib::markup_escape_text(&format!("{} · {}", item.kind, item.detail)).as_str())
            .subtitle(preview_markup(item).as_str())
            .subtitle_lines(2)
            .activatable(true)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{} entry: {}",
            item.kind, item.preview
        ))]);
        row.add_suffix(&tick_button(panel, item));
        row.add_suffix(&entry_controls(item, commands, false));
        list.append(&row);
        ids.push(Some(item.id));
        if let Some(preview) = clipboard
            .preview
            .as_ref()
            .filter(|preview| preview.id == item.id)
        {
            let preview_row = build_clipboard_preview_row(preview, clipboard, commands);
            preview_row.set_selectable(false);
            preview_row.set_activatable(false);
            list.append(&preview_row);
            ids.push(None);
        }
    }
    let ids = Rc::new(ids);
    let selected = Rc::clone(&panel.selected);
    let selection_ids = Rc::clone(&ids);
    list.connect_row_selected(move |_, row| {
        if let Some(id) =
            row.and_then(|row| selection_ids.get(row.index() as usize).copied().flatten())
        {
            selected.set(Some(id));
        }
    });
    let sender = commands.clone();
    let activation_ids = Rc::clone(&ids);
    list.connect_row_activated(move |_, row| {
        if let Some(id) = activation_ids.get(row.index() as usize).copied().flatten() {
            copy_entry(&sender, id, false);
        }
    });
    panel.results.append(&list);
    ResultsView::List(list, ids)
}

fn build_cards(
    panel: &ClipboardPanel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) -> ResultsView {
    // A vertical flow with one child per line lays cards out side by side.
    let flow = gtk::FlowBox::builder()
        .orientation(Orientation::Vertical)
        .min_children_per_line(1)
        .max_children_per_line(1)
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .column_spacing(12)
        .homogeneous(true)
        .build();
    flow.update_property(&[Property::Label("Clipboard entries")]);
    let mut ids = Vec::new();
    for item in &clipboard.items {
        let card = gtk::Box::new(Orientation::Vertical, 6);
        card.add_css_class("card");
        card.set_size_request(240, 190);
        card.set_margin_top(4);
        card.set_margin_bottom(4);
        let inner = gtk::Box::new(Orientation::Vertical, 6);
        inner.set_margin_top(10);
        inner.set_margin_bottom(10);
        inner.set_margin_start(12);
        inner.set_margin_end(12);
        inner.set_vexpand(true);

        let header = gtk::Box::new(Orientation::Horizontal, 6);
        let title = gtk::Label::new(Some(&format!("{} · {}", item.kind, item.detail)));
        title.add_css_class("caption-heading");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&title);
        header.append(&tick_button(panel, item));
        inner.append(&header);

        let preview = gtk::Label::new(None);
        preview.set_markup(&preview_markup(item));
        preview.set_wrap(true);
        preview.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        preview.set_lines(4);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.set_xalign(0.0);
        preview.set_yalign(0.0);
        preview.set_vexpand(true);
        preview.set_max_width_chars(28);
        inner.append(&preview);

        let source = gtk::Label::new(Some("Source app unknown"));
        source.add_css_class("caption");
        source.add_css_class("dim-label");
        source.set_xalign(0.0);
        source.set_tooltip_text(Some(
            "The clipboard backend does not report which application copied this entry.",
        ));
        inner.append(&source);
        inner.append(&entry_controls(item, commands, true));
        card.append(&inner);
        card.update_property(&[Property::Label(&format!(
            "{} entry: {}",
            item.kind, item.preview
        ))]);
        flow.append(&card);
        ids.push(item.id);
    }
    let ids = Rc::new(ids);
    let selected = Rc::clone(&panel.selected);
    let selection_ids = Rc::clone(&ids);
    flow.connect_selected_children_changed(move |flow| {
        if let Some(id) = flow
            .selected_children()
            .first()
            .and_then(|child| selection_ids.get(child.index() as usize).copied())
        {
            selected.set(Some(id));
        }
    });
    let sender = commands.clone();
    let activation_ids = Rc::clone(&ids);
    flow.connect_child_activated(move |_, child| {
        if let Some(id) = activation_ids.get(child.index() as usize).copied() {
            copy_entry(&sender, id, false);
        }
    });
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&flow)
        .build();
    panel.results.append(&scroller);
    if let Some(preview) = clipboard
        .preview
        .as_ref()
        .filter(|preview| ids.contains(&preview.id))
    {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.append(&build_clipboard_preview_row(preview, clipboard, commands));
        panel.results.append(&list);
    }
    ResultsView::Cards(flow, ids)
}

fn tick_button(panel: &ClipboardPanel, item: &ClipboardItemViewModel) -> gtk::CheckButton {
    let select = gtk::CheckButton::new();
    select.set_valign(Align::Center);
    select.set_active(panel.ticked.borrow().contains(&item.id));
    select.update_property(&[Property::Label(&format!("Tick entry {}", item.id))]);
    select.set_tooltip_text(Some("Tick for deletion"));
    let ticked = Rc::clone(&panel.ticked);
    let id = item.id;
    select.connect_toggled(move |button| {
        let mut ticked = ticked.borrow_mut();
        if button.is_active() {
            ticked.insert(id);
        } else {
            ticked.remove(&id);
        }
    });
    select
}

/// Pin, copy, plain copy, preview, and delete; cards use compact icon buttons.
fn entry_controls(
    item: &ClipboardItemViewModel,
    commands: &Sender<ApplicationCommand>,
    compact: bool,
) -> gtk::Box {
    let controls = gtk::Box::new(Orientation::Horizontal, if compact { 2 } else { 6 });
    controls.set_valign(Align::Center);
    let button = |label: &str, icon: &str, description: String| {
        let button = if compact {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button
        } else {
            gtk::Button::with_label(label)
        };
        button.set_valign(Align::Center);
        button.set_tooltip_text(Some(&description));
        button.update_property(&[Property::Label(&description)]);
        button
    };
    let id = item.id;

    let pin = button(
        if item.pinned { "Unpin" } else { "Pin" },
        if item.pinned {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        },
        format!("{} entry {id}", if item.pinned { "Unpin" } else { "Pin" }),
    );
    let sender = commands.clone();
    let pinned = item.pinned;
    pin.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(ClipboardCommand::SetPinned {
            item_id: id,
            pinned: !pinned,
        }));
    });
    controls.append(&pin);

    let copy = button("Copy", "edit-copy-symbolic", format!("Copy entry {id}"));
    let sender = commands.clone();
    copy.connect_clicked(move |_| copy_entry(&sender, id, false));
    controls.append(&copy);

    if !item.is_image {
        let plain = button(
            "Plain",
            "text-x-generic-symbolic",
            format!("Copy entry {id} as plain text"),
        );
        let sender = commands.clone();
        plain.connect_clicked(move |_| copy_entry(&sender, id, true));
        controls.append(&plain);
    }

    let preview = button(
        "Preview",
        "view-reveal-symbolic",
        format!("Preview entry {id}"),
    );
    let sender = commands.clone();
    preview.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::ClipboardPreview(id));
    });
    controls.append(&preview);

    let delete = button(
        "Delete",
        "user-trash-symbolic",
        format!("Delete entry {id}"),
    );
    let sender = commands.clone();
    delete.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Clipboard(
            ClipboardCommand::DeleteItems { item_ids: vec![id] },
        ));
    });
    controls.append(&delete);
    controls
}

/// Builds the explicitly requested preview row.
fn build_clipboard_preview_row(
    preview: &kestrel::ClipboardPreviewViewModel,
    clipboard: &kestrel::ClipboardViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let mut description = vec![format!("{} entry", preview.kind)];
    if preview.json {
        description.push("JSON laid out one value per line".to_owned());
    }
    if preview.truncated {
        description.push("preview truncated to the request bound".to_owned());
    }
    if let Some((width, height)) = preview.image_dimensions {
        description.push(format!("{width}×{height}"));
    }
    if let Some(count) = preview.file_count {
        description.push(format!("{count} paths"));
    }
    if preview.is_image {
        if let Some(reason) = &clipboard.ocr_unavailable {
            description.push(reason.clone());
        }
    } else if preview.text.is_empty() {
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
        if preview.json {
            text.add_css_class("monospace");
        } else {
            text.set_wrap(true);
        }
        text.set_xalign(0.0);
        text.set_selectable(true);
        text.set_valign(Align::Center);
        row.add_suffix(&text);
    }
    if preview.is_image {
        let copy_text = gtk::Button::with_label("Copy text");
        copy_text.set_valign(Align::Center);
        copy_text.set_sensitive(clipboard.ocr_unavailable.is_none());
        copy_text.set_tooltip_text(Some(match &clipboard.ocr_unavailable {
            Some(reason) => reason.as_str(),
            None => "Read the text in this image locally and copy it; the text is not retained",
        }));
        copy_text.update_property(&[Property::Label(&format!(
            "Copy the text in image entry {}",
            preview.id
        ))]);
        let sender = commands.clone();
        let id = preview.id;
        copy_text.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ClipboardCopyImageText(id));
        });
        row.add_suffix(&copy_text);
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
                ClipboardCommand::ReplaceText {
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

#[cfg(test)]
mod tests {
    use super::highlighted_markup;

    #[test]
    fn highlighted_markup_escapes_and_handles_ranges() {
        for (text, ranges, expected) in [
            (
                "a <b> & token",
                vec![(8, 13)],
                "a &lt;b&gt; &amp; <b>token</b>",
            ),
            ("plain", vec![], "plain"),
            ("abc", vec![(0, 1), (0, 2)], "<b>a</b>bc"),
        ] {
            assert_eq!(
                highlighted_markup(text, &ranges),
                expected,
                "ranges: {ranges:?}"
            );
        }
    }
}
