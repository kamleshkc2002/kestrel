use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, PolicyType, accessible::Property};
use kestrel::{
    AlertKind, ApplicationCommand, ApplicationViewModel, ConfirmationViewModel, FeatureViewModel,
    MonitorViewModel, PanelMoveDirection, PanelSection, QuickToggleCommand,
    QuickToggleControlViewModel, QuickToggleMutation, QuickToggleViewModel,
};
use kestrel_core::AppearancePreference;

const MINIMUM_WINDOW_WIDTH: i32 = 360;
const MINIMUM_WINDOW_HEIGHT: i32 = 360;
const DEFAULT_WINDOW_WIDTH: i32 = 760;
const DEFAULT_WINDOW_HEIGHT: i32 = 640;
const CONTENT_MAXIMUM_WIDTH: i32 = 760;
pub struct WindowView {
    pub window: adw::ApplicationWindow,
    content: gtk::ScrolledWindow,
    refresh_button: gtk::Button,
    toasts: adw::ToastOverlay,
    commands: Sender<ApplicationCommand>,
    monitor_container: std::cell::RefCell<Option<gtk::Box>>,
}

impl WindowView {
    pub fn new(
        application: &adw::Application,
        view_model: &ApplicationViewModel,
        commands: Sender<ApplicationCommand>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Kestrel")
            .default_width(DEFAULT_WINDOW_WIDTH)
            .default_height(DEFAULT_WINDOW_HEIGHT)
            .build();
        window.set_size_request(MINIMUM_WINDOW_WIDTH, MINIMUM_WINDOW_HEIGHT);

        let title = adw::WindowTitle::new("Kestrel", "Feature capabilities");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh capability status")
            .action_name("app.refresh-capabilities")
            .build();
        refresh_button.update_property(&[
            Property::Label("Refresh capability status"),
            Property::Description(
                "Run the registered read-only capability probes and update every feature status.",
            ),
        ]);
        header.pack_end(&refresh_button);

        let content = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .kinetic_scrolling(true)
            .propagate_natural_height(false)
            .vexpand(true)
            .build();
        let page = build_page(view_model, &window, &commands);
        content.set_child(Some(&page.clamp));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));
        window.set_content(Some(&toolbar));

        Self {
            window,
            content,
            refresh_button,
            toasts,
            commands,
            monitor_container: std::cell::RefCell::new(page.monitor_container),
        }
    }

    pub fn set_view_model(&self, view_model: &ApplicationViewModel) {
        let page = build_page(view_model, &self.window, &self.commands);
        self.content.set_child(Some(&page.clamp));
        *self.monitor_container.borrow_mut() = page.monitor_container;
    }

    pub fn set_monitor(&self, monitor: &MonitorViewModel) {
        let Some(container) = self.monitor_container.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = container.first_child() {
            child.unparent();
        }
        container.append(&build_monitor_group(monitor));
    }

    pub fn set_refreshing(&self, refreshing: bool) {
        self.refresh_button.set_sensitive(!refreshing);
        self.refresh_button.set_tooltip_text(Some(if refreshing {
            "Refreshing capability status"
        } else {
            "Refresh capability status"
        }));
    }

    pub fn show_message(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }
}

struct PageBuild {
    clamp: adw::Clamp,
    monitor_container: Option<gtk::Box>,
}

fn build_page(
    view_model: &ApplicationViewModel,
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> PageBuild {
    let page = gtk::Box::new(Orientation::Vertical, 24);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(18);
    page.set_margin_end(18);

    let heading = gtk::Label::new(Some("Kestrel"));
    heading.add_css_class("title-1");
    heading.set_halign(Align::Start);
    heading.set_wrap(true);
    heading.set_xalign(0.0);
    page.append(&heading);

    let description = gtk::Label::new(Some(
        "Kestrel keeps registered features visible, explains unavailable platform paths, and lets you change presentation preferences without hiding warnings.",
    ));
    configure_wrapping_label(&description);
    description.add_css_class("dim-label");
    page.append(&description);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search settings and features")
        .hexpand(true)
        .build();
    page.append(&build_settings_group(view_model, window, commands, &search));
    if !view_model.warnings.is_empty() {
        page.append(&build_warning_group(view_model));
    }
    let mut monitor_container = None;
    for panel in &view_model.panel_sections {
        if !panel.visible {
            continue;
        }
        match panel.section {
            PanelSection::QuickControls if !view_model.quick_toggles.is_empty() => {
                page.append(&build_quick_toggle_group(
                    &view_model.quick_toggles,
                    window,
                    commands,
                ));
            }
            PanelSection::FeatureHub => {
                page.append(&build_feature_hub_group(
                    &view_model.features,
                    commands,
                    &search,
                ));
            }
            PanelSection::Monitoring => {
                let container = gtk::Box::new(Orientation::Vertical, 0);
                container.append(&build_monitor_group(&view_model.monitor));
                page.append(&container);
                monitor_container = Some(container);
            }
            PanelSection::QuickControls => {}
        }
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(CONTENT_MAXIMUM_WIDTH);
    clamp.set_tightening_threshold(MINIMUM_WINDOW_WIDTH);
    clamp.set_child(Some(&page));
    PageBuild {
        clamp,
        monitor_container,
    }
}

fn build_settings_group(
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

    let autostart = adw::ActionRow::builder()
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

    let preset_row = adw::ActionRow::builder()
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

    let panel_group = adw::PreferencesGroup::builder()
        .title("Panel sections")
        .description(
            "Choose visibility and keyboard-accessible order for Quick Controls, Feature Hub, and Monitoring.",
        )
        .build();
    for (index, panel) in view_model.panel_sections.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(panel_title(panel.section))
            .subtitle("Visible in the main panel")
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let visibility = gtk::Switch::builder()
            .active(panel.visible)
            .valign(Align::Center)
            .build();
        let panel_name = panel_title(panel.section);
        let visibility_label = format!("Show {panel_name}");
        let visibility_description = format!("Show or hide the {panel_name} panel");
        visibility.update_property(&[
            Property::Label(&visibility_label),
            Property::Description(&visibility_description),
        ]);
        let sender = commands.clone();
        let section = panel.section;
        visibility.connect_state_set(move |_, visible| {
            let _ = sender.try_send(ApplicationCommand::SetPanelVisibility { section, visible });
            gtk::glib::Propagation::Proceed
        });
        let up = gtk::Button::builder().icon_name("go-up-symbolic").build();
        let up_label = format!("Move {panel_name} up");
        up.update_property(&[Property::Label(&up_label), Property::Description(&up_label)]);
        up.set_tooltip_text(Some(&up_label));
        up.set_sensitive(index > 0);
        let down = gtk::Button::builder().icon_name("go-down-symbolic").build();
        let down_label = format!("Move {panel_name} down");
        down.update_property(&[
            Property::Label(&down_label),
            Property::Description(&down_label),
        ]);
        down.set_tooltip_text(Some(&down_label));
        down.set_sensitive(index + 1 < view_model.panel_sections.len());
        let sender = commands.clone();
        up.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MovePanelSection {
                section,
                direction: PanelMoveDirection::Up,
            });
        });
        let sender = commands.clone();
        down.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MovePanelSection {
                section,
                direction: PanelMoveDirection::Down,
            });
        });
        controls.append(&visibility);
        controls.append(&up);
        controls.append(&down);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&visibility));
        panel_group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!("{} panel visibility order", panel_title(panel.section)),
        ));
    }
    group.add(&panel_group);

    let readout_group = adw::PreferencesGroup::builder()
        .title("Monitoring readouts")
        .description("Choose visible readouts and their order in the Monitoring panel.")
        .build();
    for setting in &view_model.monitor.readout_settings {
        let row = adw::ActionRow::builder()
            .title(setting.label)
            .subtitle(if setting.visible {
                "Show this readout and move it in the configured order."
            } else {
                "Hidden; show it to include it in the Monitoring panel."
            })
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let visibility = gtk::Switch::builder()
            .active(setting.visible)
            .valign(Align::Center)
            .build();
        let visibility_label = format!("Show {} monitoring readout", setting.label);
        visibility.update_property(&[
            Property::Label(&visibility_label),
            Property::Description(&visibility_label),
        ]);
        visibility.set_tooltip_text(Some(&visibility_label));
        let sender = commands.clone();
        let readout = setting.readout;
        visibility.connect_state_set(move |_, visible| {
            let _ =
                sender.try_send(ApplicationCommand::SetMonitorReadoutVisible { readout, visible });
            gtk::glib::Propagation::Proceed
        });
        let up = gtk::Button::builder().icon_name("go-up-symbolic").build();
        let up_label = format!("Move {} readout up", setting.label);
        up.update_property(&[Property::Label(&up_label), Property::Description(&up_label)]);
        up.set_tooltip_text(Some(&up_label));
        up.set_sensitive(setting.can_move_up);
        let sender = commands.clone();
        up.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MoveMonitorReadout {
                readout,
                direction: PanelMoveDirection::Up,
            });
        });
        let down = gtk::Button::builder().icon_name("go-down-symbolic").build();
        let down_label = format!("Move {} readout down", setting.label);
        down.update_property(&[
            Property::Label(&down_label),
            Property::Description(&down_label),
        ]);
        down.set_tooltip_text(Some(&down_label));
        down.set_sensitive(setting.can_move_down);
        let sender = commands.clone();
        down.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::MoveMonitorReadout {
                readout,
                direction: PanelMoveDirection::Down,
            });
        });
        controls.append(&visibility);
        controls.append(&up);
        controls.append(&down);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&visibility));
        row.update_property(&[Property::Label(&format!(
            "Monitoring readout {}",
            setting.label
        ))]);
        readout_group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!("monitoring readouts {} order visibility", setting.label),
        ));
    }
    group.add(&readout_group);

    let alert_group = adw::PreferencesGroup::builder()
        .title("Monitoring alerts")
        .description("Enable sustained threshold alerts and choose each threshold.")
        .build();
    for rule in &view_model.monitor.alert_rules {
        let row = adw::ActionRow::builder()
            .title(format!("{} alerts", rule.label))
            .subtitle(&rule.summary)
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let enabled = gtk::Switch::builder()
            .active(rule.enabled)
            .valign(Align::Center)
            .build();
        let enabled_label = format!("Enable {} alerts", rule.label);
        enabled.update_property(&[
            Property::Label(&enabled_label),
            Property::Description(&enabled_label),
        ]);
        enabled.set_tooltip_text(Some(&enabled_label));
        let sender = commands.clone();
        let kind = rule.kind;
        enabled.connect_state_set(move |_, value| {
            let _ = sender.try_send(ApplicationCommand::SetAlertEnabled {
                kind,
                enabled: value,
            });
            gtk::glib::Propagation::Proceed
        });
        let maximum = if kind == AlertKind::Temperature {
            150.0
        } else {
            100.0
        };
        let spin = adw::SpinRow::with_range(1.0, maximum, 1.0);
        spin.set_value(rule.threshold);
        let unit_label = gtk::Label::new(Some(rule.unit));
        unit_label.set_valign(Align::Center);
        spin.add_suffix(&unit_label);
        spin.set_numeric(true);
        spin.update_property(&[
            Property::Label(&format!("{} alert threshold", rule.label)),
            Property::Description(&rule.summary),
        ]);
        spin.set_tooltip_text(Some(&rule.summary));
        let sender = commands.clone();
        spin.connect_value_notify(move |spin| {
            let _ = sender.try_send(ApplicationCommand::SetAlertThreshold {
                kind,
                threshold: spin.value(),
            });
        });
        controls.append(&enabled);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&enabled));
        alert_group.add(&row);
        alert_group.add(&spin);
        filter_rows.push((
            row.clone().upcast(),
            format!("monitoring alerts {} threshold", rule.label),
        ));
        filter_rows.push((
            spin.clone().upcast(),
            format!("monitoring alerts {} threshold", rule.label),
        ));
    }
    group.add(&alert_group);

    let io_row = adw::ActionRow::builder()
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

fn panel_title(section: PanelSection) -> &'static str {
    match section {
        PanelSection::QuickControls => "Quick Controls",
        PanelSection::FeatureHub => "Feature Hub",
        PanelSection::Monitoring => "Monitoring",
    }
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
        let action = if save {
            gtk::FileChooserAction::Save
        } else {
            gtk::FileChooserAction::Open
        };
        let chooser = gtk::FileChooserNative::builder()
            .title(if save {
                "Export configuration"
            } else {
                "Import configuration"
            })
            .accept_label(if save { "Export" } else { "Import" })
            .cancel_label("Cancel")
            .transient_for(&window)
            .action(action)
            .build();
        let sender = sender.clone();
        chooser.connect_response(move |chooser, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(path) = chooser.file().and_then(|file| file.path()) {
                    let command = if save {
                        ApplicationCommand::ExportConfiguration(path)
                    } else {
                        ApplicationCommand::ImportConfiguration(path)
                    };
                    let _ = sender.try_send(command);
                }
            }
            chooser.destroy();
        });
        chooser.show();
    });
}

fn build_monitor_group(monitor: &MonitorViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Monitoring")
        .description(&monitor.status)
        .build();
    if monitor.readouts.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No monitoring readouts configured")
            .subtitle("Enable at least one readout in Settings → Monitoring readouts.")
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No monitoring readouts are configured")]);
        group.add(&row);
    } else {
        for readout in &monitor.readouts {
            let row = adw::ActionRow::builder()
                .title(readout.label)
                .subtitle(&readout.detail)
                .subtitle_lines(0)
                .sensitive(readout.value.is_some())
                .build();
            if let Some(value) = &readout.value {
                let value_label = gtk::Label::new(Some(value));
                value_label.add_css_class("numeric");
                value_label.set_valign(Align::Center);
                row.add_suffix(&value_label);
            }
            row.update_property(&[Property::Label(&format!(
                "{}: {}",
                readout.label,
                readout.value.as_deref().unwrap_or("Unavailable")
            ))]);
            group.add(&row);
        }
    }
    if !monitor.alerts.is_empty() || !monitor.delivery_failures.is_empty() {
        let heading = adw::ActionRow::builder()
            .title("Active alerts")
            .subtitle("Sustained threshold alerts currently active or with delivery failures.")
            .subtitle_lines(0)
            .build();
        heading.update_property(&[Property::Label("Active alerts")]);
        group.add(&heading);
        for alert in &monitor.alerts {
            let row = adw::ActionRow::builder()
                .title(alert.label)
                .subtitle(&alert.message)
                .subtitle_lines(0)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} alert: {}",
                alert.label, alert.message
            ))]);
            group.add(&row);
        }
        for (kind, message) in &monitor.delivery_failures {
            let row = adw::ActionRow::builder()
                .title(format!("{} alert delivery failed", kind.label()))
                .subtitle(message)
                .subtitle_lines(0)
                .sensitive(false)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} alert delivery failed: {}",
                kind.label(),
                message
            ))]);
            group.add(&row);
        }
    }

    group
}

fn build_feature_hub_group(
    features: &[FeatureViewModel],
    commands: &Sender<ApplicationCommand>,
    search: &gtk::SearchEntry,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Feature Hub")
        .description("Every registered feature exposes enablement, lifecycle, capability, and conservative resource costs.")
        .build();
    let mut rows = Vec::<(adw::ActionRow, String)>::new();
    for feature in features {
        let row = adw::ActionRow::builder()
            .title(&feature.label)
            .subtitle(feature_hub_subtitle(feature))
            .subtitle_lines(0)
            .build();
        let controls = gtk::Box::new(Orientation::Horizontal, 6);
        let enabled = gtk::Switch::builder()
            .active(feature.enabled)
            .sensitive(feature.configurable)
            .valign(Align::Center)
            .build();
        let sender = commands.clone();
        let feature_id = feature.id.clone();
        enabled.connect_state_set(move |_, state| {
            let _ = sender.try_send(ApplicationCommand::SetFeatureEnabled {
                feature_id: feature_id.clone(),
                enabled: state,
            });
            gtk::glib::Propagation::Proceed
        });
        controls.append(&enabled);
        row.add_suffix(&controls);
        row.set_activatable_widget(Some(&enabled));
        let accessible = format!(
            "{}. Registered: {}. Enabled: {}. Available: {}. Running: {}. Configurable: {}. {}",
            feature.label,
            yes_no(feature.registered),
            yes_no(feature.enabled),
            yes_no(feature.available),
            yes_no(feature.running),
            yes_no(feature.configurable),
            feature_hub_subtitle(feature)
        );
        row.update_property(&[Property::Label(&accessible)]);
        group.add(&row);
        rows.push((
            row,
            format!(
                "{} {} {}",
                feature.label,
                feature.id,
                feature_hub_subtitle(feature)
            ),
        ));
    }
    let rows = std::rc::Rc::new(rows);
    let rows_for_search = rows.clone();
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_lowercase();
        for (row, content) in rows_for_search.iter() {
            row.set_visible(query.is_empty() || content.to_lowercase().contains(&query));
        }
    });
    group
}

fn feature_hub_subtitle(feature: &FeatureViewModel) -> String {
    let capability = feature
        .capability
        .status
        .detail
        .as_deref()
        .unwrap_or(feature.capability.status.label);
    let backend = feature
        .capability
        .selected_backend
        .as_deref()
        .map(|value| format!(" Backend: {value}."))
        .unwrap_or_default();
    let remediation = feature
        .capability
        .remediation
        .as_ref()
        .map(|value| format!(" Remediation: {}", value.message))
        .unwrap_or_default();
    format!(
        "{}\nState: {} (registered {}, enabled {}, available {}, running {}, configurable {})\nCost: {}. Capability: {}.{}{}",
        feature.capability.summary,
        feature.lifecycle.label(),
        yes_no(feature.registered),
        yes_no(feature.enabled),
        yes_no(feature.available),
        yes_no(feature.running),
        yes_no(feature.configurable),
        feature.cost.summary(),
        capability,
        backend,
        remediation
    )
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
fn build_quick_toggle_group(
    toggles: &[QuickToggleViewModel],
    window: &adw::ApplicationWindow,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Quick toggles")
        .description(
            "Each control uses an independent user-session adapter. Requirements and externally changed state remain visible.",
        )
        .build();

    for toggle in toggles {
        let subtitle = match &toggle.error {
            Some(error) => format!(
                "{}\nRequirement: {}\nState source: {}\nLast error: {}",
                toggle.detail, toggle.requirement, toggle.source, error
            ),
            None => format!(
                "{}\nRequirement: {}\nState source: {}",
                toggle.detail, toggle.requirement, toggle.source
            ),
        };
        let row = adw::ActionRow::builder()
            .title(toggle.label)
            .subtitle(subtitle)
            .subtitle_lines(0)
            .sensitive(toggle.available)
            .build();

        match &toggle.control {
            Some(QuickToggleControlViewModel::Switch {
                enabled,
                confirmation,
            }) => {
                let control = gtk::Switch::builder()
                    .active(*enabled)
                    .valign(Align::Center)
                    .build();
                let sender = commands.clone();
                let window = window.clone();
                let id = toggle.id;
                let label = toggle.label;
                let confirmation = confirmation.clone();
                control.connect_state_set(move |_, state| {
                    let command = QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetEnabled(state),
                        confirmation_token: confirmation
                            .as_ref()
                            .map(|confirmation| confirmation.token.clone()),
                    };
                    send_with_confirmation(&window, label, confirmation.as_ref(), command, &sender);
                    gtk::glib::Propagation::Stop
                });
                row.add_suffix(&control);
                row.set_activatable_widget(Some(&control));
            }
            Some(QuickToggleControlViewModel::Level { percentage }) => {
                let controls = gtk::Box::new(Orientation::Horizontal, 6);
                controls.set_valign(Align::Center);
                let decrease = gtk::Button::with_label("−");
                decrease.set_tooltip_text(Some(&format!("Decrease {}", toggle.label)));
                decrease.set_sensitive(*percentage > 0);
                let value = gtk::Label::new(Some(&format!("{percentage}%")));
                value.add_css_class("numeric");
                let increase = gtk::Button::with_label("+");
                increase.set_tooltip_text(Some(&format!("Increase {}", toggle.label)));
                increase.set_sensitive(*percentage < 100);

                let sender = commands.clone();
                let id = toggle.id;
                let next = percentage.saturating_sub(10);
                decrease.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::QuickToggle(QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetLevel(next),
                        confirmation_token: None,
                    }));
                });
                let sender = commands.clone();
                let id = toggle.id;
                let next = percentage.saturating_add(10).min(100);
                increase.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::QuickToggle(QuickToggleCommand {
                        id,
                        mutation: QuickToggleMutation::SetLevel(next),
                        confirmation_token: None,
                    }));
                });

                controls.append(&decrease);
                controls.append(&value);
                controls.append(&increase);
                row.add_suffix(&controls);
            }
            Some(QuickToggleControlViewModel::Actions(actions)) => {
                let controls = gtk::Box::new(Orientation::Vertical, 6);
                controls.set_valign(Align::Center);
                for action in actions {
                    let button = gtk::Button::with_label(&action.label);
                    let sender = commands.clone();
                    let window = window.clone();
                    let label = toggle.label;
                    let confirmation = action.confirmation.clone();
                    let command = QuickToggleCommand {
                        id: toggle.id,
                        mutation: QuickToggleMutation::Activate {
                            target: action.target.clone(),
                        },
                        confirmation_token: Some(confirmation.token.clone()),
                    };
                    button.connect_clicked(move |_| {
                        send_with_confirmation(
                            &window,
                            label,
                            Some(&confirmation),
                            command.clone(),
                            &sender,
                        );
                    });
                    controls.append(&button);
                }
                row.add_suffix(&controls);
            }
            None => {}
        }
        let accessible_label = format!(
            "{}. {} Requirement: {}. State source: {}.",
            toggle.label, toggle.detail, toggle.requirement, toggle.source
        );
        row.update_property(&[Property::Label(&accessible_label)]);
        group.add(&row);
    }
    group
}

fn send_with_confirmation(
    window: &adw::ApplicationWindow,
    label: &str,
    confirmation: Option<&ConfirmationViewModel>,
    command: QuickToggleCommand,
    commands: &Sender<ApplicationCommand>,
) {
    let Some(confirmation) = confirmation else {
        let _ = commands.try_send(ApplicationCommand::QuickToggle(command));
        return;
    };
    let dialog = adw::MessageDialog::builder()
        .transient_for(window)
        .heading(format!("Confirm {label}"))
        .body(&confirmation.scope)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", "Continue")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "confirm" {
            let _ = commands.try_send(ApplicationCommand::QuickToggle(command.clone()));
        }
        dialog.close();
    });
    dialog.present();
}

fn build_warning_group(view_model: &ApplicationViewModel) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Configuration warnings")
        .description("Invalid feature settings are isolated and do not prevent other features from starting.")
        .build();

    for warning in &view_model.warnings {
        let row = adw::ActionRow::builder()
            .title(format!("Warning for {}", warning.feature_id))
            .subtitle(&warning.message)
            .subtitle_lines(0)
            .build();
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.add_css_class("warning");
        row.add_prefix(&icon);
        let label = format!(
            "Configuration warning for {}: {}",
            warning.feature_id, warning.message
        );
        row.update_property(&[Property::Label(&label)]);
        group.add(&row);
    }

    group
}

fn configure_wrapping_label(label: &gtk::Label) {
    label.set_halign(Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
}
