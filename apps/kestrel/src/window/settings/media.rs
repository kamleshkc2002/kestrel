//! Audio policy and screenshot limit settings.

use crate::window::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, ApplicationViewModel};

/// Adds audio mixer policy controls.
pub(super) fn add_audio_policy_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let audio = &view_model.audio;
    let audio_group = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(
            "Mixer policy: how loud amplification may go, what an output switch moves, and how \
             lost output devices are handled.",
        )
        .build();
    let boost = adw::SpinRow::with_range(
        f64::from(kestrel_core::UNAMPLIFIED_AUDIO_VOLUME_PERCENT),
        f64::from(audio.policy.max_boost_percent),
        5.0,
    );
    boost.set_title("Boost ceiling");
    boost.set_subtitle(&format!(
        "The largest volume the mixer requests. Above {}% PulseAudio amplifies in software; the \
         hard cap is {}%.",
        kestrel_core::UNAMPLIFIED_AUDIO_VOLUME_PERCENT,
        audio.policy.max_boost_percent
    ));
    boost.set_value(f64::from(audio.policy.boost_percent));
    boost.set_numeric(true);
    let boost_unit = gtk::Label::new(Some("%"));
    boost_unit.set_valign(Align::Center);
    boost.add_suffix(&boost_unit);
    boost.update_property(&[
        Property::Label("Audio boost ceiling"),
        Property::Description("The largest volume the mixer requests, bounded by the hard cap"),
    ]);
    let sender = commands.clone();
    boost.connect_value_notify(move |spin| {
        let _ = sender.try_send(ApplicationCommand::SetAudioBoostPercent(spin.value() as u8));
    });
    audio_group.add(&boost);
    filter_rows.push((
        boost.clone().upcast(),
        "audio mixer boost ceiling amplification volume".to_owned(),
    ));

    let move_streams = plain_row()
        .title("Move streams with the output")
        .subtitle(format!(
            "When switching the default output, also move playing streams. Current mode: {}.",
            audio.policy.output_switch_label
        ))
        .subtitle_lines(0)
        .build();
    let move_streams_switch = gtk::Switch::builder()
        .active(audio.policy.move_all_streams)
        .valign(Align::Center)
        .build();
    let move_streams_label = "Move playing streams when the default output changes";
    move_streams_switch.update_property(&[
        Property::Label(move_streams_label),
        Property::Description("Move every playing stream to the newly selected output"),
    ]);
    let sender = commands.clone();
    move_streams_switch.connect_state_set(move |_, moved| {
        let mode = if moved {
            kestrel::AudioOutputSwitch::AllStreams
        } else {
            kestrel::AudioOutputSwitch::DefaultOutput
        };
        let _ = sender.try_send(ApplicationCommand::SetAudioOutputSwitch(mode));
        gtk::glib::Propagation::Proceed
    });
    move_streams.add_suffix(&move_streams_switch);
    move_streams.set_activatable_widget(Some(&move_streams_switch));
    audio_group.add(&move_streams);
    filter_rows.push((
        move_streams.clone().upcast(),
        "audio output switch move streams routing".to_owned(),
    ));

    let disconnect = plain_row()
        .title("Reset volume after output loss")
        .subtitle(format!(
            "Reapply a fixed volume when a stream loses its output device. Current policy: {}.",
            audio.policy.disconnect_policy_label
        ))
        .subtitle_lines(0)
        .build();
    let disconnect_switch = gtk::Switch::builder()
        .active(audio.policy.reset_volume_on_disconnect)
        .valign(Align::Center)
        .build();
    let disconnect_label = "Reset a stream volume when its output disappears";
    disconnect_switch.update_property(&[
        Property::Label(disconnect_label),
        Property::Description("Choose whether output loss preserves or resets the stream volume"),
    ]);
    let sender = commands.clone();
    disconnect_switch.connect_state_set(move |_, reset| {
        let policy = if reset {
            kestrel::AudioDisconnectPolicy::ResetVolume
        } else {
            kestrel::AudioDisconnectPolicy::PreserveVolume
        };
        let _ = sender.try_send(ApplicationCommand::SetAudioDisconnectPolicy(policy));
        gtk::glib::Propagation::Proceed
    });
    disconnect.add_suffix(&disconnect_switch);
    disconnect.set_activatable_widget(Some(&disconnect_switch));
    audio_group.add(&disconnect);
    filter_rows.push((
        disconnect.clone().upcast(),
        "audio disconnect policy output loss volume".to_owned(),
    ));

    let disconnect_volume = adw::SpinRow::with_range(0.0, 100.0, 5.0);
    disconnect_volume.set_title("Disconnect volume");
    disconnect_volume
        .set_subtitle("Applied to a stream after output loss when the reset policy is active.");
    disconnect_volume.set_value(f64::from(audio.policy.disconnect_volume_percent));
    disconnect_volume.set_numeric(true);
    let disconnect_unit = gtk::Label::new(Some("%"));
    disconnect_unit.set_valign(Align::Center);
    disconnect_volume.add_suffix(&disconnect_unit);
    disconnect_volume.update_property(&[
        Property::Label("Disconnect volume"),
        Property::Description("The volume reapplied after output loss, from 0 to 100 percent"),
    ]);
    let sender = commands.clone();
    disconnect_volume.connect_value_notify(move |spin| {
        let _ = sender.try_send(ApplicationCommand::SetAudioDisconnectVolumePercent(
            spin.value() as u8,
        ));
    });
    audio_group.add(&disconnect_volume);
    filter_rows.push((
        disconnect_volume.clone().upcast(),
        "audio disconnect volume output loss".to_owned(),
    ));

    let inactive = plain_row()
        .title("Show inactive streams")
        .subtitle("List idle or corked streams next to playing ones.")
        .build();
    let inactive_switch = gtk::Switch::builder()
        .active(audio.policy.include_inactive_streams)
        .valign(Align::Center)
        .build();
    let inactive_label = "Show inactive playback streams";
    inactive_switch.update_property(&[
        Property::Label(inactive_label),
        Property::Description("Show or hide corked and idle playback streams"),
    ]);
    inactive_switch.set_tooltip_text(Some(inactive_label));
    let sender = commands.clone();
    inactive_switch.connect_state_set(move |_, include| {
        let _ = sender.try_send(ApplicationCommand::SetAudioIncludeInactiveStreams(include));
        gtk::glib::Propagation::Proceed
    });
    inactive.add_suffix(&inactive_switch);
    inactive.set_activatable_widget(Some(&inactive_switch));
    audio_group.add(&inactive);
    filter_rows.push((
        inactive.clone().upcast(),
        "audio inactive streams corked idle filter".to_owned(),
    ));

    group.add(&audio_group);
}

/// Adds screenshot retention limits.
pub(super) fn add_screenshot_limits_group(
    group: &adw::PreferencesGroup,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut Vec<(gtk::Widget, String)>,
) {
    let capture_limits = view_model.capture.limits;
    let capture_group = adw::PreferencesGroup::builder()
        .title("Screenshots")
        .description(
            "Recent captures are private files in Kestrel's data directory. The oldest are \
             removed first when a limit is reached.",
        )
        .build();
    for (title, subtitle, max, value, limit) in [
        (
            "Recent captures",
            "How many captures are kept.",
            kestrel_core::MAX_CAPTURE_MAX_ENTRIES,
            capture_limits.max_entries,
            kestrel::CaptureLimit::MaxEntries as fn(u32) -> kestrel::CaptureLimit,
        ),
        (
            "Storage limit",
            "Total size of kept captures, in MiB.",
            kestrel_core::MAX_CAPTURE_MAX_TOTAL_MEGABYTES,
            capture_limits.max_total_megabytes,
            kestrel::CaptureLimit::MaxTotalMegabytes,
        ),
        (
            "Capture age",
            "Hours a capture is kept.",
            kestrel_core::MAX_CAPTURE_MAX_AGE_HOURS,
            capture_limits.max_age_hours,
            kestrel::CaptureLimit::MaxAgeHours,
        ),
    ] {
        let row = adw::SpinRow::with_range(1.0, f64::from(max), 1.0);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.set_value(f64::from(value));
        row.set_numeric(true);
        row.update_property(&[Property::Label(&format!("Screenshots: {title}"))]);
        let sender = commands.clone();
        row.connect_value_notify(move |spin| {
            let value = spin.value().max(1.0) as u32;
            let _ = sender.try_send(ApplicationCommand::SetCaptureLimit(limit(value)));
        });
        filter_rows.push((
            row.clone().upcast(),
            format!("screenshots capture {title} {subtitle}").to_lowercase(),
        ));
        capture_group.add(&row);
    }
    group.add(&capture_group);
}
