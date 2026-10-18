//! Shared widget helpers.

use adw::prelude::*;
use gtk::{Align, Orientation, accessible::Property, gdk};
use kestrel::text_lines::{LineDirection, move_lines};

/// Blocks searches triggered by programmatic updates.
pub(super) type SearchGuard = std::rc::Rc<std::cell::Cell<bool>>;

/// Rows the sidebar search can hide, with their lowercase keywords.
pub(super) type FilterRows = Vec<(gtk::Widget, String)>;

pub(super) fn connect_line_movement(view: &gtk::TextView) {
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

pub(super) fn icon_button(icon: &str, label: &str, sensitive: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .valign(Align::Center)
        .sensitive(sensitive)
        .tooltip_text(label)
        .build();
    button.add_css_class("flat");
    button.update_property(&[Property::Label(label)]);
    button
}

/// Asks for a path with the desktop's file dialog; `save` picks a destination.
/// The dialog keeps itself alive until the user answers.
pub(super) fn choose_path(
    window: &adw::ApplicationWindow,
    title: &str,
    accept: &str,
    initial_name: Option<&str>,
    save: bool,
    on_path: impl FnOnce(std::path::PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .accept_label(accept)
        .modal(true)
        .build();
    if let Some(name) = initial_name {
        dialog.set_initial_name(Some(name));
    }
    let callback = move |result: Result<gtk::gio::File, gtk::glib::Error>| {
        if let Some(path) = result.ok().and_then(|file| file.path()) {
            on_path(path);
        }
    };
    if save {
        dialog.save(Some(window), gtk::gio::Cancellable::NONE, callback);
    } else {
        dialog.open(Some(window), gtk::gio::Cancellable::NONE, callback);
    }
}

/// Rows show runtime text (device names, clipboard previews, remediation with
/// `<id>` or `&`). `use-markup` is cleared before any text is set; a GTK builder
/// would apply the text first and parse it as markup once.
#[derive(Default)]
pub(super) struct PlainRow {
    title: String,
    subtitle: Option<String>,
    subtitle_lines: Option<i32>,
    sensitive: Option<bool>,
}

pub(super) fn plain_row() -> PlainRow {
    PlainRow::default()
}

impl PlainRow {
    pub(super) fn title(mut self, title: impl AsRef<str>) -> Self {
        title.as_ref().clone_into(&mut self.title);
        self
    }

    pub(super) fn subtitle(mut self, subtitle: impl AsRef<str>) -> Self {
        self.subtitle = Some(subtitle.as_ref().to_owned());
        self
    }

    pub(super) fn subtitle_lines(mut self, lines: i32) -> Self {
        self.subtitle_lines = Some(lines);
        self
    }

    pub(super) fn sensitive(mut self, sensitive: bool) -> Self {
        self.sensitive = Some(sensitive);
        self
    }

    pub(super) fn build(self) -> adw::ActionRow {
        let row = adw::ActionRow::builder().use_markup(false).build();
        row.set_title(&self.title);
        if let Some(subtitle) = &self.subtitle {
            row.set_subtitle(subtitle);
        }
        if let Some(lines) = self.subtitle_lines {
            row.set_subtitle_lines(lines);
        }
        if let Some(sensitive) = self.sensitive {
            row.set_sensitive(sensitive);
        }
        row
    }
}

pub(super) fn configure_wrapping_label(label: &gtk::Label) {
    label.set_halign(Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
}

/// A row whose controls sit on their own line below the title and description,
/// so several buttons never squeeze the text into a narrow column.
pub(super) fn stacked_row(
    title: &str,
    subtitle: &str,
    controls: &impl IsA<gtk::Widget>,
) -> adw::PreferencesRow {
    let row = adw::PreferencesRow::builder()
        .title(title)
        .activatable(false)
        .build();
    let content = gtk::Box::new(Orientation::Vertical, 4);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let heading = gtk::Label::new(Some(title));
    configure_wrapping_label(&heading);
    let description = gtk::Label::new(Some(subtitle));
    configure_wrapping_label(&description);
    description.add_css_class("dim-label");
    description.add_css_class("caption");
    content.append(&heading);
    content.append(&description);
    controls.set_halign(Align::Start);
    controls.set_margin_top(6);
    content.append(controls);
    row.set_child(Some(&content));
    row
}
