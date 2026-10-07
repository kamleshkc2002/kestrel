//! Audio mixer and microphone controls.

use super::widgets::plain_row;
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{
    ApplicationCommand, AudioCycleDirection, AudioOutputViewModel, AudioStreamViewModel,
    AudioViewModel, MicrophoneViewModel,
};

/// Target of a volume or mute control.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioLevelTarget {
    Output(u32),
    Stream(u32),
}

impl AudioLevelTarget {
    fn command(self, volume_percent: u8, muted: bool) -> (ApplicationCommand, ApplicationCommand) {
        match self {
            Self::Output(output_id) => (
                ApplicationCommand::Audio(kestrel::AudioCommand::SetOutputVolume {
                    output_id,
                    volume_percent,
                }),
                ApplicationCommand::Audio(kestrel::AudioCommand::SetOutputMute {
                    output_id,
                    muted,
                }),
            ),
            Self::Stream(stream_id) => (
                ApplicationCommand::Audio(kestrel::AudioCommand::SetStreamVolume {
                    stream_id,
                    volume_percent,
                }),
                ApplicationCommand::Audio(kestrel::AudioCommand::SetStreamMute {
                    stream_id,
                    muted,
                }),
            ),
        }
    }
}

/// Builds mute and volume controls.
fn build_audio_level_controls(
    target: AudioLevelTarget,
    label: &str,
    volume_percent: u8,
    muted: bool,
    volume_writable: bool,
    boost_ceiling_percent: u8,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let controls = gtk::Box::new(Orientation::Horizontal, 6);

    let mute = gtk::Switch::builder()
        .active(muted)
        .valign(Align::Center)
        .build();
    let mute_label = format!("Mute {label}");
    mute.update_property(&[
        Property::Label(&mute_label),
        Property::Description(&format!("Mute or unmute {label}")),
    ]);
    mute.set_tooltip_text(Some(&mute_label));
    let sender = commands.clone();
    mute.connect_state_set(move |_, muted| {
        let (_, mute_command) = target.command(volume_percent, muted);
        let _ = sender.try_send(mute_command);
        gtk::glib::Propagation::Proceed
    });
    controls.append(&mute);

    let volume = gtk::SpinButton::with_range(0.0, f64::from(boost_ceiling_percent), 1.0);
    volume.set_value(f64::from(volume_percent));
    volume.set_numeric(true);
    volume.set_width_chars(4);
    volume.set_sensitive(volume_writable);
    volume.set_valign(Align::Center);
    let volume_label = format!("{label} volume");
    volume.update_property(&[
        Property::Label(&volume_label),
        Property::Description(&format!(
            "Volume from 0 to {boost_ceiling_percent} percent, the configured amplification ceiling"
        )),
    ]);
    volume.set_tooltip_text(Some(&if volume_writable {
        format!("{label} volume, up to the {boost_ceiling_percent}% boost ceiling")
    } else {
        format!("{label} does not accept volume changes")
    }));
    let sender = commands.clone();
    volume.connect_value_changed(move |spin| {
        let (volume_command, _) = target.command(spin.value() as u8, muted);
        let _ = sender.try_send(volume_command);
    });
    controls.append(&volume);

    let unit = gtk::Label::new(Some("%"));
    unit.set_valign(Align::Center);
    controls.append(&unit);

    controls
}

fn build_audio_route_row(
    stream: &AudioStreamViewModel,
    outputs: &[AudioOutputViewModel],
    commands: &Sender<ApplicationCommand>,
) -> adw::ComboRow {
    let names = outputs
        .iter()
        .map(|output| output.title.as_str())
        .collect::<Vec<_>>();
    let selected = outputs
        .iter()
        .position(|output| output.id == stream.output_id)
        .unwrap_or(0);
    let row = adw::ComboRow::builder()
        .title(format!("Route {}", stream.title))
        .subtitle(&stream.detail)
        .model(&gtk::StringList::new(&names))
        .selected(selected as u32)
        .build();
    let sender = commands.clone();
    let stream_id = stream.id;
    let target_ids = outputs.iter().map(|output| output.id).collect::<Vec<_>>();
    let current_output_id = stream.output_id;
    row.connect_selected_notify(move |row| {
        let Some(&output_id) = target_ids.get(row.selected() as usize) else {
            return;
        };
        if output_id == current_output_id {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Audio(
            kestrel::AudioCommand::MoveStream {
                stream_id,
                output_id,
            },
        ));
    });
    row
}

/// The audio mixer: master output, per-device grouping, cycling, and streams.
pub(super) fn build_audio_controls(
    audio: &AudioViewModel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(gtk::glib::markup_escape_text(&audio.status).as_str())
        .build();

    if !audio.running {
        let row = plain_row()
            .title("Audio mixer is not running")
            .subtitle(&audio.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("The audio mixer is not running")]);
        group.add(&row);
        container.append(&group);
        return container;
    }

    let master = audio
        .default_output_id
        .and_then(|id| audio.outputs.iter().find(|output| output.id == id));
    let master_row = plain_row()
        .title("Master output")
        .subtitle(match master {
            Some(output) => format!("{} · {}", output.title, output.detail),
            None => "No default output device is available.".to_owned(),
        })
        .subtitle_lines(0)
        .sensitive(master.is_some())
        .build();
    master_row.update_property(&[Property::Label("Master output volume and mute")]);
    if let Some(output) = master {
        master_row.add_suffix(&build_audio_level_controls(
            AudioLevelTarget::Output(output.id),
            "the master output",
            output.volume_percent,
            output.muted,
            true,
            audio.boost_ceiling_percent,
            commands,
        ));
    }
    group.add(&master_row);

    let cycle_row = plain_row()
        .title("Switch output")
        .subtitle(match audio.policy.move_all_streams {
            true => "Cycle the default output; playing streams move with it.",
            false => "Cycle the default output; playing streams keep their device.",
        })
        .subtitle_lines(0)
        .build();
    let cycle = gtk::Box::new(Orientation::Horizontal, 6);
    let can_cycle = audio.outputs.len() > 1;
    for (direction, icon, label) in [
        (
            AudioCycleDirection::Previous,
            "go-previous-symbolic",
            "Switch to the previous output",
        ),
        (
            AudioCycleDirection::Next,
            "go-next-symbolic",
            "Switch to the next output",
        ),
    ] {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .valign(Align::Center)
            .build();
        button.set_sensitive(can_cycle);
        button.update_property(&[
            Property::Label(label),
            Property::Description(&format!(
                "{label}. {}",
                if can_cycle {
                    "Cycles through the discovered outputs."
                } else {
                    "At least two output devices are required."
                }
            )),
        ]);
        button.set_tooltip_text(Some(&if can_cycle {
            label.to_owned()
        } else {
            format!("{label} (a second output device is required)")
        }));
        let sender = commands.clone();
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Audio(
                kestrel::AudioCommand::CycleOutput { direction },
            ));
        });
        cycle.append(&button);
    }
    cycle_row.add_suffix(&cycle);
    group.add(&cycle_row);

    if let Some(message) = &audio.message {
        let row = plain_row()
            .title("Last audio change")
            .subtitle(message)
            .subtitle_lines(0)
            .build();
        row.update_property(&[Property::Label(&format!("Last audio change: {message}"))]);
        group.add(&row);
    }
    container.append(&group);

    if audio.outputs.is_empty() {
        return container;
    }

    for device_group in &audio.output_groups {
        let card = adw::PreferencesGroup::builder()
            .title(&device_group.label)
            .build();
        for output in &device_group.outputs {
            let row = plain_row()
                .title(&output.title)
                .subtitle(&output.name)
                .subtitle_lines(0)
                .build();
            row.update_property(&[Property::Label(&format!(
                "{} ({})",
                output.title, output.detail
            ))]);
            row.add_suffix(&build_audio_level_controls(
                AudioLevelTarget::Output(output.id),
                &output.title,
                output.volume_percent,
                output.muted,
                true,
                audio.boost_ceiling_percent,
                commands,
            ));
            if !output.is_default {
                let make_default = gtk::Button::with_label("Use");
                make_default.set_valign(Align::Center);
                let label = format!("Make {} the default output", output.title);
                make_default.update_property(&[
                    Property::Label(&label),
                    Property::Description(&format!(
                        "Make {} the default output without moving streams unless configured",
                        output.title
                    )),
                ]);
                make_default.set_tooltip_text(Some(&label));
                let sender = commands.clone();
                let output_id = output.id;
                make_default.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::Audio(
                        kestrel::AudioCommand::SetDefaultOutput { output_id },
                    ));
                });
                row.add_suffix(&make_default);
            }
            card.add(&row);
        }
        container.append(&card);
    }

    let streams = adw::PreferencesGroup::builder()
        .title("Playback streams")
        .description(if audio.inactive_streams == 0 {
            "Per-stream volume, mute, and routing.".to_owned()
        } else {
            format!(
                "Per-stream volume, mute, and routing. {} inactive streams are hidden; show them \
                 from Settings → Audio.",
                audio.inactive_streams
            )
        })
        .build();
    if audio.streams.is_empty() {
        let row = plain_row()
            .title("No playing streams")
            .subtitle("Start playback in an application to control its volume.")
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        row.update_property(&[Property::Label("No playback streams are active")]);
        streams.add(&row);
    }
    let routable = audio.outputs.len() > 1;
    for stream in &audio.streams {
        let row = plain_row()
            .title(&stream.title)
            .subtitle(&stream.detail)
            .subtitle_lines(0)
            .build();
        row.update_property(&[Property::Label(&format!(
            "{}: {}",
            stream.title, stream.detail
        ))]);
        row.add_suffix(&build_audio_level_controls(
            AudioLevelTarget::Stream(stream.id),
            &stream.title,
            stream.volume_percent.unwrap_or(0),
            stream.muted,
            stream.volume_writable,
            audio.boost_ceiling_percent,
            commands,
        ));
        streams.add(&row);
        if routable {
            streams.add(&build_audio_route_row(stream, &audio.outputs, commands));
        }
    }
    container.append(&streams);

    container
}

/// Builds microphone controls; mixed mute state leaves the switch usable.
pub(super) fn build_microphone_group(
    microphone: &MicrophoneViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Microphone")
        .description(gtk::glib::markup_escape_text(&microphone.status).as_str())
        .build();
    if !microphone.running {
        let row = plain_row()
            .title("Microphone control is not running")
            .subtitle(&microphone.status)
            .subtitle_lines(0)
            .sensitive(false)
            .build();
        group.add(&row);
        return group;
    }

    let controllable = microphone.muted.is_some() || microphone.mixed;
    let row = plain_row()
        .title("Mute all inputs")
        .subtitle(&microphone.mute_label)
        .build();
    let mute = gtk::Switch::builder()
        .active(microphone.muted.unwrap_or(false))
        .valign(Align::Center)
        .sensitive(controllable)
        .build();
    mute.update_property(&[Property::Label("Mute all microphone inputs")]);
    mute.set_tooltip_text(Some("Also available as Ctrl+Shift+M and from the tray"));
    let sender = commands.clone();
    mute.connect_state_set(move |_, muted| {
        let _ = sender.try_send(ApplicationCommand::Microphone(
            kestrel::MicrophoneCommand::SetMuted(muted),
        ));
        // The next backend refresh supplies the switch state.
        gtk::glib::Propagation::Stop
    });
    row.add_suffix(&mute);
    row.set_activatable_widget(Some(&mute));
    group.add(&row);

    if !microphone.inputs.is_empty() {
        let labels = microphone
            .inputs
            .iter()
            .map(|input| input.label.as_str())
            .collect::<Vec<_>>();
        let model = gtk::StringList::new(&labels);
        let selected = microphone
            .default_input_id
            .and_then(|id| microphone.inputs.iter().position(|input| input.id == id))
            .and_then(|position| u32::try_from(position).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        let combo = adw::ComboRow::builder()
            .title("Default input")
            .subtitle(if microphone.default_input_id.is_some() {
                "The audio server records from this input by default"
            } else {
                "No input is the server default"
            })
            .model(&model)
            .selected(selected)
            .build();
        combo.update_property(&[Property::Label("Default microphone input")]);
        let ids = microphone
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>();
        let sender = commands.clone();
        combo.connect_selected_notify(move |combo| {
            let Some(input_id) = usize::try_from(combo.selected())
                .ok()
                .and_then(|position| ids.get(position))
            else {
                return;
            };
            let _ = sender.try_send(ApplicationCommand::Microphone(
                kestrel::MicrophoneCommand::SetDefaultInput {
                    input_id: *input_id,
                },
            ));
        });
        group.add(&combo);
    }
    if let Some(message) = &microphone.message {
        let notice = plain_row()
            .title("Input disconnected")
            .subtitle(message)
            .subtitle_lines(0)
            .build();
        group.add(&notice);
    }
    group
}
