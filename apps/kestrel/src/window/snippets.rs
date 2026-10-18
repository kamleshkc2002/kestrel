//! Snippet library panel and editor.

use super::widgets::{SearchGuard, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, PolicyType, accessible::Property, gdk};
use kestrel::{
    ApplicationCommand,
    text_lines::{LineDirection, move_lines},
};

pub(super) struct SnippetPanel {
    pub(super) container: gtk::Box,
    pub(super) status: gtk::Label,
    pub(super) insertion: gtk::Label,
    draft_label: gtk::Label,
    pub(super) editor: gtk::Box,
    pub(super) results: gtk::Box,
}

pub(super) fn build_snippet_panel(
    snippets: &kestrel::SnippetsViewModel,
    commands: &Sender<ApplicationCommand>,
) -> SnippetPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Text snippets")
        .description(
            "Reusable text with local date, time, time zone, and clipboard variables. \
             Insertion is only offered when a provider is verified.",
        )
        .build();

    let status = gtk::Label::new(Some(&snippets.status));
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    let status_row = adw::ActionRow::new();
    status_row.set_title("Library");
    status_row.add_suffix(&status);
    group.add(&status_row);

    let insertion = gtk::Label::new(Some(&snippets.insertion_status));
    insertion.set_wrap(true);
    insertion.set_xalign(0.0);
    insertion.add_css_class("dim-label");
    let insertion_row = plain_row()
        .title("Insertion")
        .subtitle(snippets.expansion_label)
        .subtitle_lines(0)
        .build();
    insertion_row.add_suffix(&insertion);
    group.add(&insertion_row);

    if let Some(directory) = &snippets.directory {
        let row = plain_row()
            .title("Storage")
            .subtitle(format!("Private file directory: {directory}"))
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("Snippet storage directory")]);
        group.add(&row);
    }
    for warning in &snippets.warnings {
        let row = plain_row()
            .title(&warning.feature_id)
            .subtitle(&warning.message)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{}: {}",
            warning.feature_id, warning.message
        ))]);
        group.add(&row);
    }

    let draft_label = gtk::Label::new(None);
    draft_label.set_wrap(true);
    draft_label.set_xalign(0.0);
    draft_label.add_css_class("dim-label");
    let draft_row = plain_row()
        .title("Editor")
        .subtitle("Name, optional folder, optional trigger, and the snippet text.")
        .subtitle_lines(0)
        .build();
    draft_row.add_suffix(&draft_label);
    group.add(&draft_row);

    let editor = gtk::Box::new(Orientation::Vertical, 6);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search snippets")
        .hexpand(true)
        .build();
    search.update_property(&[
        Property::Label("Search snippets"),
        Property::Description("Searches names, folders, triggers, and snippet text"),
    ]);
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.add_suffix(&search);
    search_row.set_activatable_widget(Some(&search));
    let guard: SearchGuard = std::rc::Rc::new(std::cell::Cell::new(false));
    let handler_guard = std::rc::Rc::clone(&guard);
    let sender = commands.clone();
    search.connect_search_changed(move |entry| {
        if handler_guard.get() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::SnippetSearch(entry.text().to_string()));
    });
    if !snippets.search_query.is_empty() {
        guard.set(true);
        search.set_text(&snippets.search_query);
        guard.set(false);
    }
    group.add(&search_row);
    // Retain widgets across updates.
    let results = gtk::Box::new(Orientation::Vertical, 6);

    if snippets.running
        && snippets.search_query.is_empty()
        && snippets.items.is_empty()
        && snippets.total > 0
    {
        let _ = commands.try_send(ApplicationCommand::SnippetSearch(String::new()));
    }
    let panel = SnippetPanel {
        container: container.clone(),
        status,
        insertion,
        draft_label,
        editor: editor.clone(),
        results: results.clone(),
    };
    group.add(&editor);
    fill_snippet_panel(&panel, snippets, commands);
    group.add(&results);
    container.append(&group);
    panel
}

pub(super) fn fill_snippet_panel(
    panel: &SnippetPanel,
    snippets: &kestrel::SnippetsViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    let draft = snippets
        .draft
        .clone()
        .unwrap_or(kestrel::SnippetDraftViewModel {
            name: String::new(),
            folder: String::new(),
            trigger: String::new(),
            content: String::new(),
            is_new: true,
        });
    panel.draft_label.set_text(if draft.is_new {
        "New snippet"
    } else {
        "Editing a stored snippet"
    });

    let editable = snippets.running;
    let name = entry_row(panel, commands, "Name", &draft.name, editable);
    let folder = entry_row(panel, commands, "Folder", &draft.folder, editable);
    let trigger = entry_row(panel, commands, "Trigger", &draft.trigger, editable);
    let content = text_view_row(panel, commands, &draft.content, editable);

    let actions = gtk::Box::new(Orientation::Horizontal, 6);
    let new_button = gtk::Button::with_label("New");
    new_button.set_sensitive(editable);
    new_button.update_property(&[Property::Label("New snippet")]);
    let sender = commands.clone();
    new_button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::SnippetNew);
    });
    let save = gtk::Button::with_label("Save");
    save.set_sensitive(editable);
    save.update_property(&[Property::Label("Save snippet")]);
    save.set_tooltip_text(Some(
        "Validate and store the snippet in the private snippet file",
    ));
    let sender = commands.clone();
    let name_entry = name.clone();
    let folder_entry = folder.clone();
    let trigger_entry = trigger.clone();
    let content_view = content.clone();
    save.connect_clicked(move |_| {
        let buffer = content_view.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let _ = sender.try_send(ApplicationCommand::Snippet(kestrel::SnippetCommand::Save {
            name: name_entry.text().to_string(),
            folder: folder_entry.text().to_string(),
            trigger: trigger_entry.text().to_string(),
            content: text,
        }));
    });
    actions.append(&new_button);
    actions.append(&save);
    let action_row = adw::ActionRow::new();
    action_row.set_title("Save");
    action_row.set_subtitle("An empty folder or trigger stores no value for that field.");
    action_row.add_suffix(&actions);
    panel.editor.append(&action_row);

    if snippets.items.is_empty() {
        let row = plain_row()
            .title(if snippets.search_query.trim().is_empty() {
                "No snippets yet"
            } else {
                "No snippets match the search"
            })
            .sensitive(false)
            .build();
        // While stopped, the Library row above says how to start it.
        if snippets.running {
            row.set_subtitle("Fill the editor and press Save, or edit the snippet file directly.");
        }
        row.update_property(&[Property::Label("No snippets to show")]);
        panel.results.append(&row);
        return;
    }

    for item in &snippets.items {
        let row = plain_row()
            .title(if let Some(folder) = &item.folder {
                format!("{} · {}", item.name, folder)
            } else {
                item.name.clone()
            })
            .subtitle(&item.preview)
            .subtitle_lines(2)
            .build();
        row.update_property(&[Property::Label(&format!("{}: {}", item.name, item.preview))]);

        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let insert = gtk::Button::with_label("Insert");
        let can_insert = snippets.running && snippets.insertion_available;
        insert.set_sensitive(can_insert);
        insert.update_property(&[Property::Label(&format!("Insert {}", item.name))]);
        insert.set_tooltip_text(Some(if can_insert {
            "Render the variables and type this snippet into the focused window"
        } else if !snippets.running {
            snippets.status.as_str()
        } else {
            "Insertion needs a verified provider"
        }));
        let sender = commands.clone();
        let name = item.name.clone();
        insert.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Snippet(
                kestrel::SnippetCommand::Insert(name.clone()),
            ));
        });
        controls.append(&insert);

        let edit = gtk::Button::with_label("Edit");
        edit.update_property(&[Property::Label(&format!("Edit {}", item.name))]);
        let sender = commands.clone();
        let name = item.name.clone();
        edit.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::SnippetEdit(name.clone()));
        });
        controls.append(&edit);

        let delete = gtk::Button::with_label("Delete");
        delete.set_sensitive(editable);
        delete.update_property(&[Property::Label(&format!("Delete {}", item.name))]);
        let sender = commands.clone();
        let name = item.name.clone();
        delete.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Snippet(
                kestrel::SnippetCommand::Delete(name.clone()),
            ));
        });
        controls.append(&delete);
        row.add_suffix(&controls);
        panel.results.append(&row);
    }
}

fn entry_row(
    panel: &SnippetPanel,
    _commands: &Sender<ApplicationCommand>,
    title: &str,
    value: &str,
    sensitive: bool,
) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .text(value)
        .hexpand(true)
        .sensitive(sensitive)
        .build();
    entry.update_property(&[Property::Label(&format!("Snippet {title}"))]);
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.add_suffix(&entry);
    panel.editor.append(&row);
    entry
}

fn connect_line_movement(view: &gtk::TextView) {
    let buffer = view.buffer();
    buffer.set_enable_undo(true);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, state| {
        let direction = match (key, state.contains(gdk::ModifierType::ALT_MASK)) {
            (gdk::Key::Up, true) => LineDirection::Up,
            (gdk::Key::Down, true) => LineDirection::Down,
            _ => return gtk::glib::Propagation::Proceed,
        };
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        let selection = buffer
            .selection_bounds()
            .map(|(start, end)| (start.offset() as usize, end.offset() as usize))
            .unwrap_or_else(|| {
                let cursor = buffer.cursor_position().max(0) as usize;
                (cursor, cursor)
            });
        let Some(line_move) = move_lines(&text, selection, direction) else {
            return gtk::glib::Propagation::Stop;
        };

        let mut range_start = buffer.iter_at_offset(line_move.range.0 as i32);
        let mut range_end = buffer.iter_at_offset(line_move.range.1 as i32);
        buffer.begin_user_action();
        buffer.delete(&mut range_start, &mut range_end);
        let mut insert_at = buffer.iter_at_offset(line_move.range.0 as i32);
        buffer.insert(&mut insert_at, &line_move.replacement);
        let new_start = buffer.iter_at_offset(line_move.selection.0 as i32);
        let new_end = buffer.iter_at_offset(line_move.selection.1 as i32);
        buffer.select_range(&new_start, &new_end);
        buffer.end_user_action();
        gtk::glib::Propagation::Stop
    });
    view.add_controller(keys);
}

fn text_view_row(
    panel: &SnippetPanel,
    _commands: &Sender<ApplicationCommand>,
    value: &str,
    editable: bool,
) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .editable(editable)
        .build();
    view.buffer().set_text(value);
    connect_line_movement(&view);
    view.update_property(&[Property::Label("Snippet content")]);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .min_content_height(96)
        .child(&view)
        .build();
    let row = adw::ActionRow::new();
    row.set_title("Content");
    row.add_suffix(&scroller);
    panel.editor.append(&row);
    view
}
