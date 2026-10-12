//! System monitor and speed-test groups.

use super::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{ApplicationCommand, CoreGroupViewModel, MonitorViewModel, SpeedTestViewModel};

pub(super) fn build_monitor_group(
    monitor: &MonitorViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Monitoring")
        .description(gtk::glib::markup_escape_text(&monitor.status).as_str())
        .build();
    if monitor.readouts.is_empty() {
        let row = plain_row()
            .title("No monitoring readouts configured")
            .subtitle("Enable at least one readout in Settings → Monitoring readouts.")
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No monitoring readouts are configured")]);
        group.add(&row);
    } else {
        for readout in &monitor.readouts {
            let row = plain_row()
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
            if readout.readout == kestrel::MonitorReadout::Cpu {
                for cores in monitor.cores.iter().flatten() {
                    group.add(&build_core_row(cores));
                }
            }
        }
    }
    for accessory in &monitor.accessories.rows {
        let row = plain_row()
            .title(&accessory.title)
            .subtitle(&accessory.detail)
            .build();
        let value = accessory
            .percent
            .map_or_else(|| "–".to_owned(), |percent| format!("{percent}%"));
        let value_label = gtk::Label::new(Some(&value));
        value_label.add_css_class("numeric");
        value_label.set_valign(Align::Center);
        row.add_suffix(&value_label);
        row.update_property(&[Property::Label(&format!(
            "{}: {value}, {}",
            accessory.title, accessory.detail
        ))]);
        group.add(&row);
    }
    if let Some(reason) = &monitor.accessories.unavailable {
        group.add(
            &plain_row()
                .title("Accessory batteries unavailable")
                .subtitle(reason)
                .subtitle_lines(0)
                .sensitive(false)
                .build(),
        );
    }
    group.add(&build_addresses_row(monitor, commands));
    if !monitor.alerts.is_empty() || !monitor.delivery_failures.is_empty() {
        let heading = plain_row()
            .title("Active alerts")
            .subtitle("Sustained threshold alerts currently active or with delivery failures.")
            .subtitle_lines(0)
            .build();
        heading.update_property(&[Property::Label("Active alerts")]);
        group.add(&heading);
        for alert in &monitor.alerts {
            let row = plain_row()
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
            let row = plain_row()
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

/// One row per core type with a level bar for each logical CPU.
fn build_core_row(group: &CoreGroupViewModel) -> adw::PreferencesRow {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(8)
        .min_children_per_line(4)
        .column_spacing(12)
        .row_spacing(6)
        .homogeneous(true)
        .build();
    for (index, usage) in &group.cores {
        let cell = gtk::Box::new(Orientation::Horizontal, 6);
        let name = gtk::Label::new(Some(&format!("CPU {index}")));
        name.add_css_class("caption");
        name.set_width_chars(6);
        name.set_xalign(0.0);
        let bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(100.0)
            .value(usage.unwrap_or(0.0))
            .hexpand(true)
            .width_request(40)
            .valign(Align::Center)
            .build();
        let value = usage.map_or_else(|| "–".to_owned(), |usage| format!("{usage:.0}%"));
        let percent = gtk::Label::new(Some(&value));
        percent.add_css_class("caption");
        percent.add_css_class("numeric");
        percent.set_width_chars(4);
        percent.set_xalign(1.0);
        cell.append(&name);
        cell.append(&bar);
        cell.append(&percent);
        cell.update_property(&[Property::Label(&format!("CPU {index}: {value}"))]);
        flow.append(&cell);
    }
    let content = gtk::Box::new(Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let title = gtk::Label::new(Some(group.label));
    title.set_xalign(0.0);
    title.add_css_class("dim-label");
    content.append(&title);
    content.append(&flow);
    let row = adw::PreferencesRow::new();
    row.set_activatable(false);
    row.set_child(Some(&content));
    row.update_property(&[Property::Label(group.label)]);
    row
}

/// Addresses appear only after the user asks and leave when hidden.
fn build_addresses_row(
    monitor: &MonitorViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let (subtitle, shown) = match &monitor.local_addresses {
        None => (
            "Shown on request; never saved or exported.".to_owned(),
            false,
        ),
        Some(Ok(addresses)) if addresses.is_empty() => {
            ("No interface has an address.".to_owned(), true)
        }
        Some(Ok(addresses)) => (addresses.join("\n"), true),
        Some(Err(error)) => (error.clone(), true),
    };
    let row = plain_row()
        .title("Local addresses")
        .subtitle(&subtitle)
        .subtitle_lines(0)
        .build();
    row.set_subtitle_selectable(shown);
    let button = gtk::Button::with_label(if shown { "Hide" } else { "Show" });
    button.set_valign(Align::Center);
    button.update_property(&[Property::Label(if shown {
        "Hide local addresses"
    } else {
        "Show local addresses"
    })]);
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::ShowLocalAddresses(!shown));
    });
    row.add_suffix(&button);
    row
}

pub(super) fn build_speed_test_group(
    speed_test: &SpeedTestViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Network speed test")
        .description(gtk::glib::markup_escape_text(&speed_test.disclosure).as_str())
        .build();
    let row = plain_row()
        .title("Speed test")
        .subtitle(&speed_test.status)
        .subtitle_lines(0)
        .build();
    let start = gtk::Button::with_label("Start");
    start.set_valign(Align::Center);
    start.set_sensitive(speed_test.can_start);
    start.update_property(&[Property::Label("Start network speed test")]);
    let sender = commands.clone();
    start.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::StartSpeedTest);
    });
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_valign(Align::Center);
    cancel.set_sensitive(speed_test.can_cancel);
    cancel.update_property(&[Property::Label("Cancel network speed test")]);
    let sender = commands.clone();
    cancel.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CancelSpeedTest);
    });
    row.add_suffix(&start);
    row.add_suffix(&cancel);
    group.add(&row);
    if let Some(progress) = speed_test.progress {
        let bar = gtk::ProgressBar::builder()
            .fraction(progress)
            .show_text(true)
            .margin_top(6)
            .build();
        bar.set_text(speed_test.phase_label.as_deref());
        group.add(&bar);
    }
    for line in &speed_test.result_lines {
        group.add(&plain_row().title(line).build());
    }
    group
}
