//! Settings groups shared by the sidebar pages.

mod media;
mod panels;
mod tools;

use crate::window::widgets::{
    FilterRows, choose_path, icon_button, on_spin_row_settled, plain_row, stacked_row,
};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation};
use kestrel::{ApplicationCommand, ApplicationViewModel, view_model::accessory_module_status};
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
) -> (adw::PreferencesGroup, FilterRows, Option<adw::ActionRow>) {
    // Feature pages show their nested groups' own titles; the Settings page
    // adds its app-wide rows directly.
    let group = adw::PreferencesGroup::new();
    if page == SettingsPage::Settings {
        group.set_title("General");
    }
    let mut filter_rows = Vec::<(gtk::Widget, String)>::new();
    let mut accessory_row = None;

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
            add_general_rows(&group, view_model, window, commands, &mut filter_rows);
            let io_row = build_configuration_files_row(window, commands, &mut filter_rows);
            group.add(&io_row);
            add_logging_group(&group, view_model, window, commands, &mut filter_rows);
            add_whats_new_group(&group, view_model, window, commands, &mut filter_rows);
            accessory_row = add_overlay_group(&group, view_model, commands, &mut filter_rows);
        }
    }

    for (_, keywords) in &mut filter_rows {
        *keywords = keywords.to_lowercase();
    }
    (group, filter_rows, accessory_row)
}

fn add_logging_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let logging = &view_model.logging;
    let logs_group = adw::PreferencesGroup::builder()
        .title("Diagnostic logs")
        .description(
            "Warnings, errors, and features starting or stopping are kept on this computer. \
             Logs never record clipboard, snippet, or note text, searches, window titles, \
             network addresses, or paths in your home folder, and never leave this computer.",
        )
        .build();

    let debug = plain_row()
        .title("Debug logging")
        .subtitle(if logging.debug_forced {
            "On for this run because KESTREL_LOG=debug is set."
        } else {
            "Also records operation names and timings, for tracking down a problem."
        })
        .subtitle_lines(0)
        .build();
    let debug_switch = gtk::Switch::builder()
        .active(logging.debug)
        .valign(Align::Center)
        .build();
    let sender = commands.clone();
    debug_switch.connect_state_set(move |_, enabled| {
        let _ = sender.try_send(ApplicationCommand::SetLoggingSetting(
            kestrel::LoggingSetting::Debug(enabled),
        ));
        gtk::glib::Propagation::Proceed
    });
    debug.add_suffix(&debug_switch);
    debug.set_activatable_widget(Some(&debug_switch));
    filter_rows.push((
        debug.clone().upcast(),
        "debug logging logs diagnostics verbose".to_owned(),
    ));
    logs_group.add(&debug);

    for (title, subtitle, (min, max), value, setting) in [
        (
            "Keep logs for",
            "Days before a log file is deleted.",
            (
                kestrel_core::MIN_LOG_RETENTION_DAYS,
                kestrel_core::MAX_LOG_RETENTION_DAYS,
            ),
            logging.retention_days,
            kestrel::LoggingSetting::RetentionDays as fn(u32) -> kestrel::LoggingSetting,
        ),
        (
            "Log size limit",
            "Total size of all log files, in MB; the oldest go first.",
            (
                kestrel_core::MIN_LOG_TOTAL_MEGABYTES,
                kestrel_core::MAX_LOG_TOTAL_MEGABYTES,
            ),
            logging.max_total_megabytes,
            kestrel::LoggingSetting::MaxTotalMegabytes,
        ),
    ] {
        let row = adw::SpinRow::with_range(f64::from(min), f64::from(max), 1.0);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.set_value(f64::from(value));
        row.set_numeric(true);
        let sender = commands.clone();
        on_spin_row_settled(&row, move |value| {
            let value = (value as u32).clamp(min, max);
            let _ = sender.try_send(ApplicationCommand::SetLoggingSetting(setting(value)));
        });
        filter_rows.push((
            row.clone().upcast(),
            format!("logs logging retention ttl size {title} {subtitle}"),
        ));
        logs_group.add(&row);
    }

    let buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let open = gtk::Button::with_label("Open folder");
    let clear = gtk::Button::with_label("Clear logs…");
    clear.add_css_class("destructive-action");
    open.set_sensitive(logging.directory.is_some());
    clear.set_sensitive(logging.directory.is_some());
    let sender = commands.clone();
    open.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::OpenLogFolder);
    });
    let sender = commands.clone();
    let parent = window.clone();
    clear.connect_clicked(move |_| {
        let dialog = adw::MessageDialog::builder()
            .transient_for(&parent)
            .heading("Clear the logs?")
            .body("Every log file is deleted. Logging continues with a new file.")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("clear", "Clear logs")]);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        let sender = sender.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "clear" {
                let _ = sender.try_send(ApplicationCommand::ClearLogs);
            }
            dialog.close();
        });
        dialog.present();
    });
    buttons.append(&open);
    buttons.append(&clear);
    let folder = logging
        .directory
        .as_deref()
        .unwrap_or("Unavailable without a home folder");
    let files_row = stacked_row("Log files", folder, &buttons);
    filter_rows.push((
        files_row.clone().upcast(),
        "log files folder open clear logs diagnostics".to_owned(),
    ));
    logs_group.add(&files_row);

    let report = plain_row()
        .title("Report a problem")
        .subtitle(
            "Opens the bug report form on GitHub with your Kestrel version and desktop \
             filled in. Attach logs from the folder above if you can.",
        )
        .subtitle_lines(0)
        .build();
    let report_button = gtk::Button::with_label("Open");
    report_button.set_valign(Align::Center);
    let sender = commands.clone();
    report_button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::ReportProblem);
    });
    report.add_suffix(&report_button);
    report.set_activatable_widget(Some(&report_button));
    filter_rows.push((
        report.clone().upcast(),
        "report problem bug issue github diagnostics logs".to_owned(),
    ));
    logs_group.add(&report);
    group.add(&logs_group);
}

fn add_overlay_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) -> Option<adw::ActionRow> {
    let overlay = &view_model.overlay;
    let overlay_group = adw::PreferencesGroup::builder()
        .title("Status bar overlay")
        .description(gtk::glib::markup_escape_text(&overlay.status).as_str())
        .build();
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Recording",
            "While a screen recording runs: its time, with pause and stop.",
        ),
        (overlay.modules.recording, kestrel::OverlayModule::Recording),
        "status overlay recording time pause stop",
        commands,
        filter_rows,
    ));
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Keep awake",
            "While a timed Keep awake session runs: the time left, with stop.",
        ),
        (
            overlay.modules.keep_awake,
            kestrel::OverlayModule::KeepAwake,
        ),
        "status overlay keep awake time left",
        commands,
        filter_rows,
    ));
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Alerts",
            "Each active monitoring alert with its reading; click it to open Monitoring.",
        ),
        (overlay.modules.alerts, kestrel::OverlayModule::Alerts),
        "status overlay alerts monitoring cpu memory temperature",
        commands,
        filter_rows,
    ));
    let accessory_row = overlay_module_row(
        overlay,
        (
            "Accessory batteries",
            accessory_module_status(&view_model.monitor),
        ),
        (
            overlay.modules.accessories,
            kestrel::OverlayModule::Accessories,
        ),
        "status overlay accessory batteries battery mouse keyboard headset low",
        commands,
        filter_rows,
    );
    overlay_group.add(&accessory_row);
    let levels = (
        overlay.modules.accessory_warning,
        overlay.modules.accessory_critical,
    );
    for (title, subtitle, (minimum, maximum), value, keywords, pair) in [
        (
            "Warning level",
            "Accessories at or below this percentage show as low, in amber.",
            (5, kestrel_core::MAX_ACCESSORY_WARNING_LEVEL),
            levels.0,
            "battery accessory mouse keyboard headset low warning level",
            (|level: u8, (_, critical): (u8, u8)| (level, critical))
                as fn(u8, (u8, u8)) -> (u8, u8),
        ),
        (
            "Critical level",
            "Accessories at or below this percentage show as critically low, in red.",
            (
                kestrel_core::MIN_ACCESSORY_CRITICAL_LEVEL,
                kestrel_core::MAX_ACCESSORY_CRITICAL_LEVEL,
            ),
            levels.1,
            "battery accessory mouse keyboard headset low critical level",
            (|level: u8, (warning, _): (u8, u8)| (warning, level)) as fn(u8, (u8, u8)) -> (u8, u8),
        ),
    ] {
        let row = adw::SpinRow::with_range(f64::from(minimum), f64::from(maximum), 1.0);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.set_value(f64::from(value));
        row.set_numeric(true);
        let percent = gtk::Label::new(Some("%"));
        percent.set_valign(Align::Center);
        row.add_suffix(&percent);
        let sender = commands.clone();
        on_spin_row_settled(&row, move |value| {
            let level = (value as u8).clamp(minimum, maximum);
            let (warning, critical) = pair(level, levels);
            let _ = sender.try_send(ApplicationCommand::SetAccessoryLevels { warning, critical });
        });
        filter_rows.push((
            row.clone().upcast(),
            format!("{keywords} {title} {subtitle}"),
        ));
        overlay_group.add(&row);
    }
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Microphone muted",
            "While every microphone input is muted; click it to unmute.",
        ),
        (
            overlay.modules.microphone,
            kestrel::OverlayModule::Microphone,
        ),
        "status overlay microphone muted indicator",
        commands,
        filter_rows,
    ));
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Volume",
            "When the output volume or mute changes: the output type, a level slider, and mute.",
        ),
        (overlay.modules.volume, kestrel::OverlayModule::Volume),
        "status overlay volume output speaker mute level slider",
        commands,
        filter_rows,
    ));
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Microphone level",
            "When the microphone's mute or level changes: a fader, mute, and an editable percentage.",
        ),
        (
            overlay.modules.microphone_level,
            kestrel::OverlayModule::MicrophoneLevel,
        ),
        "status overlay microphone input level fader mute percentage",
        commands,
        filter_rows,
    ));
    overlay_group.add(&overlay_module_row(
        overlay,
        (
            "Keyboard light",
            "When the keyboard light changes: a slider whose icon turns it off and on.",
        ),
        (
            overlay.modules.keyboard_light,
            kestrel::OverlayModule::KeyboardLight,
        ),
        "status overlay keyboard light backlight brightness slider",
        commands,
        filter_rows,
    ));
    group.add(&overlay_group);
    Some(accessory_row)
}

/// One module's switch; usable only where the overlay can run.
fn overlay_module_row(
    overlay: &kestrel::view_model::OverlayViewModel,
    (title, subtitle): (&str, &str),
    (shown, module): (bool, kestrel::OverlayModule),
    keywords: &str,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) -> adw::ActionRow {
    let row = plain_row()
        .title(title)
        .subtitle(subtitle)
        .subtitle_lines(0)
        .sensitive(overlay.available)
        .build();
    let toggle = gtk::Switch::builder()
        .active(shown)
        .valign(Align::Center)
        .build();
    let sender = commands.clone();
    toggle.connect_state_set(move |_, shown| {
        let _ = sender.try_send(ApplicationCommand::SetOverlayModule { module, shown });
        gtk::glib::Propagation::Proceed
    });
    row.add_suffix(&toggle);
    row.set_activatable_widget(Some(&toggle));
    filter_rows.push((row.clone().upcast(), keywords.to_owned()));
    row
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
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let appearance = adw::ComboRow::builder()
        .title("Kestrel window style")
        .subtitle(if view_model.appearance_overridden {
            "Your GTK style sheet sets this window's colours, so Light and Dark may not change them."
        } else {
            "Only this window. System follows your desktop."
        })
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
        "appearance kestrel window style color scheme system light dark theme".to_owned(),
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

    let setup_row = plain_row()
        .title("Run setup again")
        .subtitle("Choose a preset and review what each feature uses.")
        .build();
    let open = gtk::Button::builder()
        .label("Open")
        .valign(Align::Center)
        .build();
    let window_for_setup = window.clone();
    let view_model_for_setup = view_model.clone();
    let commands_for_setup = commands.clone();
    open.connect_clicked(move |_| {
        super::open_setup(
            &window_for_setup,
            &view_model_for_setup,
            &commands_for_setup,
        );
    });
    setup_row.add_suffix(&open);
    setup_row.set_activatable_widget(Some(&open));
    filter_rows.push((
        setup_row.clone().upcast(),
        "run setup again first launch presets welcome".to_owned(),
    ));
    group.add(&setup_row);
}

fn add_whats_new_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let notes_group = adw::PreferencesGroup::builder().title("What's new").build();
    let notes = plain_row()
        .title(format!("Kestrel {}", view_model.whats_new.version))
        .subtitle("What changed in this release, by feature.")
        .build();
    let show = gtk::Button::builder()
        .label("Show")
        .valign(Align::Center)
        .build();
    let window_for_notes = window.clone();
    let whats_new = view_model.whats_new.clone();
    show.connect_clicked(move |_| {
        crate::window::whats_new::open_whats_new(&window_for_notes, &whats_new);
    });
    notes.add_suffix(&show);
    notes.set_activatable_widget(Some(&show));
    filter_rows.push((
        notes.clone().upcast(),
        "what's new whats new release notes changes update version".to_owned(),
    ));
    notes_group.add(&notes);

    let offer = plain_row()
        .title("Offer what's new after updates")
        .subtitle("Overview shows a banner once when an update changes a feature you use.")
        .build();
    let offer_switch = gtk::Switch::builder()
        .active(view_model.whats_new.offered)
        .valign(Align::Center)
        .build();
    let sender = commands.clone();
    offer_switch.connect_state_set(move |_, enabled| {
        let _ = sender.try_send(ApplicationCommand::SetWhatsNew(enabled));
        gtk::glib::Propagation::Proceed
    });
    offer.add_suffix(&offer_switch);
    offer.set_activatable_widget(Some(&offer_switch));
    filter_rows.push((
        offer.clone().upcast(),
        "what's new whats new banner update release notes".to_owned(),
    ));
    notes_group.add(&offer);
    group.add(&notes_group);
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
