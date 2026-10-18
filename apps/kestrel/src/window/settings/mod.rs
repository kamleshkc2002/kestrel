//! Settings groups shared by the sidebar pages.

mod media;
mod panels;
mod tools;

use crate::window::widgets::{FilterRows, choose_path, icon_button, plain_row, stacked_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation};
use kestrel::{ApplicationCommand, ApplicationViewModel};
use kestrel_core::{AppearancePreference, CloseAction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SettingsPage {
    Monitoring,
    Audio,
    Clipboard,
    CommandBar,
    Capture,
    Notes,
    Processes,
    Settings,
}

/// Builds a page's settings group and returns its rows with their lowercase
/// search keywords; the window's single search handler filters them.
pub(super) fn build_settings_group(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    page: SettingsPage,
) -> (adw::PreferencesGroup, FilterRows) {
    // Feature pages show their nested groups' own titles; the Settings page
    // adds its app-wide rows directly.
    let group = adw::PreferencesGroup::new();
    if page == SettingsPage::Settings {
        group.set_title("General");
    }
    let mut filter_rows = Vec::<(gtk::Widget, String)>::new();

    match page {
        SettingsPage::Monitoring => {
            panels::add_monitoring_readouts_group(&group, view_model, commands, &mut filter_rows);
            panels::add_monitoring_alerts_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Audio => {
            media::add_audio_policy_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Clipboard => {
            tools::add_clipboard_limits_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::CommandBar => {
            tools::add_command_bar_group(&group, view_model, commands, &mut filter_rows);
            add_quick_panel_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Capture => {
            media::add_screenshot_limits_group(&group, view_model, commands, &mut filter_rows);
            media::add_screen_tools_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Notes => {
            tools::add_snippet_limits_group(&group, view_model, commands, &mut filter_rows);
            tools::add_scratchpad_limits_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Processes => {
            panels::add_process_limits_group(&group, view_model, commands, &mut filter_rows);
        }
        SettingsPage::Settings => {
            add_general_rows(&group, view_model, commands, &mut filter_rows);
            let io_row = build_configuration_files_row(window, commands, &mut filter_rows);
            group.add(&io_row);
        }
    }

    for (_, keywords) in &mut filter_rows {
        *keywords = keywords.to_lowercase();
    }
    (group, filter_rows)
}

fn add_quick_panel_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut FilterRows,
) {
    use kestrel::command_line::CommandLineAction;

    let favorites = &view_model.quick_panel_favorites;
    let panel_group = adw::PreferencesGroup::builder()
        .title("Quick panel")
        .description(format!(
            "Up to {} tools for kestrel --command quick-panel.open, in this order.",
            kestrel_core::MAX_QUICK_PANEL_FAVORITES
        ))
        .build();

    let available = CommandLineAction::all()
        .into_iter()
        .filter(|action| !favorites.contains(&action.id()))
        .collect::<Vec<_>>();
    let descriptions = available
        .iter()
        .map(|action| action.description())
        .collect::<Vec<_>>();
    let choices =
        gtk::DropDown::from_strings(&descriptions.iter().map(String::as_str).collect::<Vec<_>>());
    choices.set_valign(Align::Center);
    let add = gtk::Button::with_label("Add");
    add.set_valign(Align::Center);
    add.set_sensitive(
        !available.is_empty() && favorites.len() < kestrel_core::MAX_QUICK_PANEL_FAVORITES,
    );
    let sender = commands.clone();
    let current = favorites.clone();
    let ids = available
        .iter()
        .map(|action| action.id())
        .collect::<Vec<_>>();
    let choices_for_add = choices.clone();
    add.connect_clicked(move |_| {
        let Some(id) = ids.get(choices_for_add.selected() as usize) else {
            return;
        };
        let mut changed = current.clone();
        changed.push(id.clone());
        let _ = sender.try_send(ApplicationCommand::SetQuickPanelFavorites(changed));
    });
    let add_row = plain_row().title("Add a tool").build();
    add_row.add_suffix(&choices);
    add_row.add_suffix(&add);
    filter_rows.push((
        add_row.clone().upcast(),
        "quick panel favorites add tool".to_owned(),
    ));
    panel_group.add(&add_row);

    for (index, id) in favorites.iter().enumerate() {
        let description = CommandLineAction::parse(id)
            .map(|action| action.description())
            .unwrap_or_else(|_| id.clone());
        let row = plain_row().title(&description).subtitle(id).build();
        for (icon, label, target) in [
            ("go-up-symbolic", "Move up", index.checked_sub(1)),
            (
                "go-down-symbolic",
                "Move down",
                Some(index + 1).filter(|next| *next < favorites.len()),
            ),
        ] {
            let button = icon_button(icon, &format!("{label}: {description}"), target.is_some());
            let sender = commands.clone();
            let current = favorites.clone();
            button.connect_clicked(move |_| {
                let Some(target) = target else {
                    return;
                };
                let mut changed = current.clone();
                changed.swap(index, target);
                let _ = sender.try_send(ApplicationCommand::SetQuickPanelFavorites(changed));
            });
            row.add_suffix(&button);
        }
        let remove = icon_button(
            "user-trash-symbolic",
            &format!("Remove {description}"),
            true,
        );
        let sender = commands.clone();
        let current = favorites.clone();
        remove.connect_clicked(move |_| {
            let mut changed = current.clone();
            changed.remove(index);
            let _ = sender.try_send(ApplicationCommand::SetQuickPanelFavorites(changed));
        });
        row.add_suffix(&remove);
        filter_rows.push((
            row.clone().upcast(),
            format!("quick panel favorites {description} {id}"),
        ));
        panel_group.add(&row);
    }
    group.add(&panel_group);
}

fn add_general_rows(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
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

    let close_button = adw::ComboRow::builder()
        .title("Close button")
        .subtitle("Minimizing keeps Kestrel running in the taskbar.")
        .model(&gtk::StringList::new(&[
            "Minimize to the taskbar",
            "Quit Kestrel",
        ]))
        .selected(match view_model.close_action {
            CloseAction::Minimize => 0,
            CloseAction::Quit => 1,
        })
        .build();
    let sender = commands.clone();
    close_button.connect_selected_notify(move |row| {
        let action = if row.selected() == 1 {
            CloseAction::Quit
        } else {
            CloseAction::Minimize
        };
        let _ = sender.try_send(ApplicationCommand::SetCloseAction(action));
    });
    filter_rows.push((
        close_button.clone().upcast(),
        "close button minimize taskbar quit exit window".to_owned(),
    ));
    group.add(&close_button);

    let quit_row = plain_row()
        .title("Quit Kestrel")
        .subtitle("Ctrl+Q quits from any Kestrel window.")
        .build();
    let quit = gtk::Button::with_label("Quit");
    quit.set_valign(Align::Center);
    quit.set_tooltip_text(Some("Quit Kestrel now (Ctrl+Q)"));
    let sender = commands.clone();
    quit.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Quit);
    });
    quit_row.add_suffix(&quit);
    filter_rows.push((quit_row.clone().upcast(), "quit exit close".to_owned()));
    group.add(&quit_row);

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
    let preset_row = stacked_row(
        "Feature presets",
        "Change enablement as one reversible operation.",
        &presets,
    );
    filter_rows.push((
        preset_row.clone().upcast(),
        "feature presets essentials balanced everything undo enablement".to_owned(),
    ));
    group.add(&preset_row);
}

fn build_configuration_files_row(
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) -> adw::PreferencesRow {
    let io_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let import = gtk::Button::with_label("Import");
    connect_file_chooser(&import, window, commands, false);
    let export = gtk::Button::with_label("Export");
    connect_file_chooser(&export, window, commands, true);
    io_buttons.append(&import);
    io_buttons.append(&export);
    let io_row = stacked_row(
        "Configuration files",
        "Import or export configuration. Kestrel only emits the selected path command.",
        &io_buttons,
    );
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
