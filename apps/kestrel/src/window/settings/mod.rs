//! Settings group with searchable rows.

mod media;
mod panels;
mod tools;

use crate::window::widgets::{choose_path, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation};
use kestrel::{ApplicationCommand, ApplicationViewModel};
use kestrel_core::AppearancePreference;

pub(super) fn build_settings_group(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    search: &gtk::SearchEntry,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Settings")
        .description("Presentation and startup preferences. Search filters this content and the feature hub.")
        .build();
    let mut filter_rows = Vec::<(gtk::Widget, String)>::new();

    add_general_rows(&group, view_model, commands, search, &mut filter_rows);
    panels::add_panel_sections_group(&group, view_model, commands, &mut filter_rows);
    panels::add_monitoring_readouts_group(&group, view_model, commands, &mut filter_rows);
    panels::add_monitoring_alerts_group(&group, view_model, commands, &mut filter_rows);
    media::add_audio_policy_group(&group, view_model, commands, &mut filter_rows);
    media::add_screenshot_limits_group(&group, view_model, commands, &mut filter_rows);
    tools::add_clipboard_limits_group(&group, view_model, commands, &mut filter_rows);
    tools::add_snippet_limits_group(&group, view_model, commands, &mut filter_rows);
    tools::add_command_bar_group(&group, view_model, commands, &mut filter_rows);

    let io_row = build_configuration_files_row(window, commands, &mut filter_rows);
    let filter_rows = std::rc::Rc::new(filter_rows);
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_lowercase();
        for (row, content) in filter_rows.iter() {
            row.set_visible(query.is_empty() || content.contains(&query));
        }
    });
    group.add(&io_row);
    group
}

/// Adds search, appearance, autostart, and preset rows.
fn add_general_rows(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    search: &gtk::SearchEntry,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.add_suffix(search);
    search_row.set_activatable_widget(Some(search));
    group.add(&search_row);

    let appearance = adw::ComboRow::builder()
        .title("Appearance")
        .subtitle("Choose the application color scheme.")
        .model(&gtk::StringList::new(&["System", "Light", "Dark"]))
        .selected(match view_model.appearance {
            AppearancePreference::System => 0,
            AppearancePreference::Light => 1,
            AppearancePreference::Dark => 2,
        })
        .build();
    let sender = commands.clone();
    appearance.connect_selected_notify(move |row| {
        let preference = match row.selected() {
            1 => AppearancePreference::Light,
            2 => AppearancePreference::Dark,
            _ => AppearancePreference::System,
        };
        let _ = sender.try_send(ApplicationCommand::SetAppearance(preference));
    });
    filter_rows.push((
        appearance.clone().upcast(),
        "appearance color scheme system light dark".to_owned(),
    ));
    group.add(&appearance);

    let autostart = plain_row()
        .title("Start automatically")
        .subtitle("Launch Kestrel when your desktop session starts.")
        .build();
    let autostart_switch = gtk::Switch::builder()
        .active(view_model.autostart)
        .valign(Align::Center)
        .build();
    let sender = commands.clone();
    autostart_switch.connect_state_set(move |_, enabled| {
        let _ = sender.try_send(ApplicationCommand::SetAutostart(enabled));
        gtk::glib::Propagation::Proceed
    });
    autostart.add_suffix(&autostart_switch);
    autostart.set_activatable_widget(Some(&autostart_switch));
    filter_rows.push((
        autostart.clone().upcast(),
        "start automatically autostart startup session".to_owned(),
    ));
    group.add(&autostart);

    let preset_row = plain_row()
        .title("Feature presets")
        .subtitle("Change enablement as one reversible operation.")
        .subtitle_lines(0)
        .build();
    let presets = gtk::Box::new(Orientation::Horizontal, 6);
    for (label, preset) in [
        ("Essentials", kestrel::FeaturePreset::Essentials),
        ("Balanced", kestrel::FeaturePreset::Balanced),
        ("Everything", kestrel::FeaturePreset::Everything),
    ] {
        let button = gtk::Button::with_label(label);
        button.set_tooltip_text(Some(&format!("Apply the {label} feature preset")));
        let sender = commands.clone();
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ApplyPreset(preset));
        });
        presets.append(&button);
    }
    let undo = gtk::Button::with_label("Undo");
    undo.set_sensitive(view_model.can_undo);
    let sender = commands.clone();
    undo.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::UndoPreset);
    });
    presets.append(&undo);
    preset_row.add_suffix(&presets);
    filter_rows.push((
        preset_row.clone().upcast(),
        "feature presets essentials balanced everything undo enablement".to_owned(),
    ));
    group.add(&preset_row);
}

/// Builds the configuration import and export row.
fn build_configuration_files_row(
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) -> adw::ActionRow {
    let io_row = plain_row()
        .title("Configuration files")
        .subtitle("Import or export configuration. Kestrel only emits the selected path command.")
        .subtitle_lines(0)
        .build();
    let io_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let import = gtk::Button::with_label("Import");
    connect_file_chooser(&import, window, commands, false);
    let export = gtk::Button::with_label("Export");
    connect_file_chooser(&export, window, commands, true);
    io_buttons.append(&import);
    io_buttons.append(&export);
    io_row.add_suffix(&io_buttons);
    filter_rows.push((
        io_row.clone().upcast(),
        "configuration files import export".to_owned(),
    ));
    io_row
}

fn connect_file_chooser(
    button: &gtk::Button,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    save: bool,
) {
    let window = window.clone();
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let sender = sender.clone();
        let (title, accept) = if save {
            ("Export configuration", "Export")
        } else {
            ("Import configuration", "Import")
        };
        choose_path(&window, title, accept, None, save, move |path| {
            let command = if save {
                ApplicationCommand::ExportConfiguration(path)
            } else {
                ApplicationCommand::ImportConfiguration(path)
            };
            let _ = sender.try_send(command);
        });
    });
}
