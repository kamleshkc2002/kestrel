//! Panel layout, monitoring readout, alert, and process settings.

use crate::window::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{
    AlertKind, ApplicationCommand, ApplicationViewModel, PanelMoveDirection, PanelSection,
};

pub(super) fn add_panel_sections_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let panel_group = adw::PreferencesGroup::builder()
        .title("Panel sections")
        .description(
            "Choose visibility and keyboard-accessible order for Quick Controls, Feature Hub, and Monitoring.",
        )
        .build();
    for (index, panel) in view_model.panel_sections.iter().enumerate() {
        let row = plain_row()
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
}

pub(super) fn add_monitoring_readouts_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let readout_group = adw::PreferencesGroup::builder()
        .title("Monitoring readouts")
        .description("Choose visible readouts and their order in the Monitoring panel.")
        .build();
    for setting in &view_model.monitor.readout_settings {
        let row = plain_row()
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
        let tray = gtk::ToggleButton::with_label("Tray");
        tray.set_active(setting.in_tray);
        tray.set_sensitive(setting.can_toggle_tray);
        tray.set_valign(Align::Center);
        let tray_label = format!("Show {} in the tray title and tooltip", setting.label);
        tray.update_property(&[Property::Label(&tray_label)]);
        tray.set_tooltip_text(Some(&if setting.can_toggle_tray {
            tray_label.clone()
        } else {
            format!(
                "The tray shows at most {} readouts",
                kestrel_core::MAX_TRAY_READOUTS
            )
        }));
        let sender = commands.clone();
        tray.connect_toggled(move |button| {
            let _ = sender.try_send(ApplicationCommand::SetMonitorSetting(
                kestrel::MonitorSetting::TrayReadout {
                    readout,
                    shown: button.is_active(),
                },
            ));
        });
        controls.append(&visibility);
        controls.append(&tray);
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
            format!(
                "monitoring readouts {} order visibility tray",
                setting.label
            ),
        ));
    }

    let units = [
        kestrel::NetworkRateUnit::Bytes,
        kestrel::NetworkRateUnit::Bits,
    ];
    let unit_row = adw::ComboRow::builder()
        .title("Network rate unit")
        .subtitle("Bytes use KiB/s; bits use kbit/s and Mbit/s.")
        .model(&gtk::StringList::new(
            &units.iter().map(|unit| unit.label()).collect::<Vec<_>>(),
        ))
        .selected(
            units
                .iter()
                .position(|unit| *unit == view_model.monitor.network_rate_unit)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    unit_row.connect_selected_notify(move |row| {
        if let Some(unit) = units.get(row.selected() as usize) {
            let _ = sender.try_send(ApplicationCommand::SetMonitorSetting(
                kestrel::MonitorSetting::NetworkRateUnit(*unit),
            ));
        }
    });
    readout_group.add(&unit_row);
    filter_rows.push((
        unit_row.clone().upcast(),
        "network rate unit bits bytes".to_owned(),
    ));

    let per_core = adw::SwitchRow::builder()
        .title("Per-core CPU load")
        .subtitle("List each logical CPU under the CPU readout, grouped by core type.")
        .active(view_model.monitor.per_core_cpu)
        .build();
    let sender = commands.clone();
    per_core.connect_active_notify(move |row| {
        let _ = sender.try_send(ApplicationCommand::SetMonitorSetting(
            kestrel::MonitorSetting::PerCoreCpu(row.is_active()),
        ));
    });
    readout_group.add(&per_core);
    filter_rows.push((
        per_core.clone().upcast(),
        "per core cpu load cores".to_owned(),
    ));
    group.add(&readout_group);
}

pub(super) fn add_monitoring_alerts_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let alert_group = adw::PreferencesGroup::builder()
        .title("Monitoring alerts")
        .description("Enable sustained threshold alerts and choose each threshold.")
        .build();
    for rule in &view_model.monitor.alert_rules {
        let row = plain_row()
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
}

fn panel_title(section: PanelSection) -> &'static str {
    match section {
        PanelSection::QuickControls => "Quick Controls",
        PanelSection::FeatureHub => "Feature Hub",
        PanelSection::Monitoring => "Monitoring",
    }
}

pub(super) fn add_process_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let limits = view_model.processes.limits;
    let process_group = adw::PreferencesGroup::builder()
        .title("Processes")
        .description("How often the process list refreshes and how many rows it keeps.")
        .build();
    for (title, subtitle, min, max, step, value, limit) in [
        (
            "Refresh interval",
            "Milliseconds between process scans.",
            kestrel_core::MIN_PROCESS_REFRESH_MILLIS,
            kestrel_core::MAX_PROCESS_REFRESH_MILLIS,
            500.0,
            limits.refresh_millis,
            kestrel::ProcessLimit::RefreshMillis as fn(u32) -> kestrel::ProcessLimit,
        ),
        (
            "Maximum rows",
            "Rows shown, busiest first; search covers every process.",
            kestrel_core::MIN_PROCESS_ROWS,
            kestrel_core::MAX_PROCESS_ROWS,
            10.0,
            limits.max_rows,
            kestrel::ProcessLimit::MaxRows,
        ),
    ] {
        let row = adw::SpinRow::with_range(f64::from(min), f64::from(max), step);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.set_value(f64::from(value));
        row.set_numeric(true);
        row.update_property(&[Property::Label(&format!("Processes: {title}"))]);
        let sender = commands.clone();
        row.connect_value_notify(move |spin| {
            let value = (spin.value() as u32).clamp(min, max);
            let _ = sender.try_send(ApplicationCommand::SetProcessLimit(limit(value)));
        });
        filter_rows.push((
            row.clone().upcast(),
            format!("processes {title} {subtitle}").to_lowercase(),
        ));
        process_group.add(&row);
    }
    group.add(&process_group);
}
