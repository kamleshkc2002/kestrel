//! Panel layout, monitoring readout, alert, and process settings.

use crate::window::widgets::{on_spin_row_settled, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{AlertKind, ApplicationCommand, ApplicationViewModel, PanelMoveDirection};
use std::{cell::RefCell, time::Duration};

thread_local! {
    /// Alert rows the user opened; settings changes rebuild the page, and
    /// these reopen so an edit does not snap the row shut.
    static EXPANDED_ALERTS: RefCell<Vec<AlertKind>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn add_monitoring_readouts_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let readout_group = adw::PreferencesGroup::builder()
        .title("Monitoring readouts")
        .description(
            "Choose which readouts the Monitoring panel at the top of this page shows, and in \
             what order. Tray also shows a readout's value as its own tray icon.",
        )
        .build();
    for setting in &view_model.monitor.readout_settings {
        let row = plain_row()
            .title(setting.label)
            .subtitle(match (setting.visible, setting.in_tray) {
                (true, true) => "Shown in the panel and in the tray",
                (true, false) => "Shown in the panel",
                (false, true) => "Hidden from the panel; shown in the tray",
                (false, false) => "Hidden from the panel",
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
        .description("An alert fires when a reading stays past its threshold for the chosen time.")
        .build();
    for rule in &view_model.monitor.alert_rules {
        let kind = rule.kind;
        let row = adw::ExpanderRow::builder()
            .title(format!("{} alerts", rule.label))
            .subtitle(&rule.summary)
            .use_markup(false)
            .expanded(EXPANDED_ALERTS.with_borrow(|open| open.contains(&kind)))
            .build();
        row.connect_expanded_notify(move |row| {
            EXPANDED_ALERTS.with_borrow_mut(|open| {
                open.retain(|open_kind| *open_kind != kind);
                if row.is_expanded() {
                    open.push(kind);
                }
            });
        });
        let enabled = gtk::Switch::builder()
            .active(rule.enabled)
            .valign(Align::Center)
            .build();
        let enabled_label = format!("Enable {} alerts", rule.label);
        enabled.update_property(&[Property::Label(&enabled_label)]);
        enabled.set_tooltip_text(Some(&enabled_label));
        let sender = commands.clone();
        enabled.connect_state_set(move |_, value| {
            let _ = sender.try_send(ApplicationCommand::SetAlertEnabled {
                kind,
                enabled: value,
            });
            gtk::glib::Propagation::Proceed
        });
        row.add_suffix(&enabled);

        let maximum = if kind == AlertKind::Temperature {
            150.0
        } else {
            100.0
        };
        let threshold = adw::SpinRow::with_range(1.0, maximum, 1.0);
        threshold.set_title("Threshold");
        threshold.set_value(rule.threshold);
        threshold.set_numeric(true);
        let unit_label = gtk::Label::new(Some(rule.unit));
        unit_label.set_valign(Align::Center);
        threshold.add_suffix(&unit_label);
        threshold.update_property(&[Property::Label(&format!("{} alert threshold", rule.label))]);
        let sender = commands.clone();
        on_spin_row_settled(&threshold, move |value| {
            let _ = sender.try_send(ApplicationCommand::SetAlertThreshold {
                kind,
                threshold: value,
            });
        });
        row.add_row(&threshold);

        let choices = sustain_choices(rule.sustain);
        let labels = choices
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>();
        let sustained = adw::ComboRow::builder()
            .title("Sustained for")
            .subtitle("Shorter spikes are ignored.")
            .model(&gtk::StringList::new(&labels))
            .selected(
                choices
                    .iter()
                    .position(|(_, span)| *span == rule.sustain)
                    .unwrap_or(0) as u32,
            )
            .build();
        sustained.update_property(&[Property::Label(&format!(
            "How long {} must stay past its threshold",
            rule.label
        ))]);
        let sender = commands.clone();
        let spans = choices.iter().map(|(_, span)| *span).collect::<Vec<_>>();
        sustained.connect_selected_notify(move |combo| {
            if let Some(duration) = spans.get(combo.selected() as usize) {
                let _ = sender.try_send(ApplicationCommand::SetAlertSustain {
                    kind,
                    duration: *duration,
                });
            }
        });
        row.add_row(&sustained);

        alert_group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!(
                "monitoring alerts {} threshold sustained duration spikes",
                rule.label
            )
            .to_lowercase(),
        ));
    }
    group.add(&alert_group);
}

/// Offered alert durations, plus the configured one when it is not a preset.
fn sustain_choices(configured: Duration) -> Vec<(String, Duration)> {
    let mut choices = [0, 5, 15, 30, 60, 120, 300, 600]
        .map(|seconds| {
            let span = Duration::from_secs(seconds);
            (sustain_label(span), span)
        })
        .to_vec();
    if !choices.iter().any(|(_, span)| *span == configured) {
        choices.push((sustain_label(configured), configured));
        choices.sort_by_key(|(_, span)| *span);
    }
    choices
}

fn sustain_label(span: Duration) -> String {
    match span.as_secs() {
        0 => "Immediately".to_owned(),
        seconds if seconds.is_multiple_of(60) => {
            let minutes = seconds / 60;
            format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
        }
        1 => "1 second".to_owned(),
        seconds => format!("{seconds} seconds"),
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
        .title("Process inspector settings")
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
        on_spin_row_settled(&row, move |value| {
            let value = (value as u32).clamp(min, max);
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
