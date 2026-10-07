//! System monitor and speed-test groups.

use super::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, MonitorViewModel, SpeedTestViewModel};

pub(super) fn build_monitor_group(monitor: &MonitorViewModel) -> adw::PreferencesGroup {
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
        }
    }
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

/// Builds the speed-test group.
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
