//! Screen recording: choose a source and tracks, control the running
//! recording, and reach recent takes.

use std::time::Duration;

use super::widgets::{plain_row, turn_on_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property, glib};
use kestrel::{
    ApplicationCommand, AudioTrack, RecordingControls, RecordingExportViewModel, RecordingRequest,
    RecordingSetting, RecordingSource, RecordingViewModel,
};

const FRAMERATES: [u32; 4] = [15, 24, 30, 60];
const CLOCK_TICK: Duration = Duration::from_millis(500);

fn send(commands: &Sender<ApplicationCommand>, request: RecordingRequest) {
    let _ = commands.try_send(ApplicationCommand::Recording(request));
}

fn set(commands: &Sender<ApplicationCommand>, setting: RecordingSetting) {
    let _ = commands.try_send(ApplicationCommand::SetRecordingSetting(setting));
}

fn button(label: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.set_valign(Align::Center);
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

pub(super) fn build_recording_group(
    recording: &RecordingViewModel,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Box {
    let container = gtk::Box::new(Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Screen recording")
        .description(glib::markup_escape_text(&recording.status).as_str())
        .build();
    container.append(&group);
    if let Some(turn_on) = &recording.turn_on {
        group.add(&turn_on_row(turn_on, commands));
    }
    if !recording.running {
        return container;
    }
    match recording.controls {
        RecordingControls::Setup => add_setup(&group, recording, commands),
        RecordingControls::Waiting => {
            let row = plain_row().title("Sharing").build();
            let cancel = button("Cancel", false);
            let sender = commands.clone();
            cancel.connect_clicked(move |_| send(&sender, RecordingRequest::Cancel));
            row.add_suffix(&cancel);
            group.add(&row);
        }
        RecordingControls::Recording { paused } => {
            add_controls(&group, recording, paused, commands)
        }
        RecordingControls::Stopping => {}
    }
    if recording.dismissable {
        let row = plain_row().title("Last recording").build();
        let dismiss = button("Dismiss", false);
        let sender = commands.clone();
        dismiss.connect_clicked(move |_| send(&sender, RecordingRequest::Dismiss));
        row.add_suffix(&dismiss);
        group.add(&row);
    }
    container.append(&takes_group(recording, commands));
    container
}

fn add_setup(
    group: &adw::PreferencesGroup,
    recording: &RecordingViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    if recording.sources.is_empty() {
        return;
    }
    let row = plain_row()
        .title("Record")
        .subtitle("The desktop asks which screen or window to share.")
        .build();
    let buttons = gtk::Box::new(Orientation::Horizontal, 6);
    buttons.set_valign(Align::Center);
    for source in &recording.sources {
        let start = button(source.label(), *source == RecordingSource::Screen);
        start.update_property(&[Property::Label(match source {
            RecordingSource::Screen => "Record a screen",
            RecordingSource::Window => "Record a window",
            RecordingSource::Area => "Record an area of a screen",
        })]);
        let sender = commands.clone();
        let source = *source;
        start.connect_clicked(move |_| send(&sender, RecordingRequest::Start(source)));
        buttons.append(&start);
    }
    row.add_suffix(&buttons);
    group.add(&row);

    for option in &recording.tracks_offered {
        let row = adw::SwitchRow::builder()
            .title(option.track.label())
            .active(option.enabled && option.available)
            .sensitive(option.available)
            .build();
        row.set_subtitle(match (option.track, option.available) {
            (_, false) => "Unavailable in this session",
            (AudioTrack::System, true) => "What this computer plays, as its own track",
            (AudioTrack::Microphone, true) => "The default input, as its own track",
        });
        let sender = commands.clone();
        let track = option.track;
        row.connect_active_notify(move |row| {
            set(&sender, RecordingSetting::Track(track, row.is_active()));
        });
        group.add(&row);
        if option.track == AudioTrack::System {
            add_sound_row(group, recording, commands);
        }
    }

    let pointer = adw::SwitchRow::builder()
        .title("Pointer")
        .active(recording.cursor && recording.cursor_available)
        .sensitive(recording.cursor_available)
        .build();
    if !recording.cursor_available {
        pointer.set_subtitle("This desktop cannot draw the pointer into recordings");
    }
    let sender = commands.clone();
    pointer
        .connect_active_notify(move |row| set(&sender, RecordingSetting::Cursor(row.is_active())));
    group.add(&pointer);

    let mut rates = FRAMERATES.to_vec();
    if !rates.contains(&recording.framerate) {
        rates.push(recording.framerate);
        rates.sort_unstable();
    }
    let labels: Vec<String> = rates.iter().map(|rate| format!("{rate} fps")).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let framerate = adw::ComboRow::builder()
        .title("Frame rate")
        .model(&gtk::StringList::new(&label_refs))
        .selected(
            rates
                .iter()
                .position(|rate| *rate == recording.framerate)
                .unwrap_or(0) as u32,
        )
        .build();
    let sender = commands.clone();
    framerate.connect_selected_notify(move |row| {
        if let Some(rate) = rates.get(row.selected() as usize) {
            set(&sender, RecordingSetting::Framerate(*rate));
        }
    });
    group.add(&framerate);
}

fn add_sound_row(
    group: &adw::PreferencesGroup,
    recording: &RecordingViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    let labels: Vec<&str> = recording
        .sound
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    let combo = adw::ComboRow::builder()
        .title("Sound from")
        .model(&gtk::StringList::new(&labels))
        .selected(recording.sound.selected)
        .sensitive(recording.sound.sensitive)
        .build();
    if let Some(reason) = &recording.sound.reason {
        combo.set_subtitle(reason);
    }
    let identities: Vec<Option<String>> = recording
        .sound
        .options
        .iter()
        .map(|option| option.identity.clone())
        .collect();
    // The selection is set above, before this handler exists, so rebuilding
    // the panel never sends the setting back.
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let ready_for_selection = std::rc::Rc::clone(&ready);
    let sender = commands.clone();
    combo.connect_selected_notify(move |combo| {
        if !ready_for_selection.get() {
            return;
        }
        let Some(identity) = usize::try_from(combo.selected())
            .ok()
            .and_then(|position| identities.get(position))
        else {
            return;
        };
        set(
            &sender,
            RecordingSetting::SystemAudioApplication(identity.clone()),
        );
    });
    ready.set(true);
    group.add(&combo);
}

fn add_controls(
    group: &adw::PreferencesGroup,
    recording: &RecordingViewModel,
    paused: bool,
    commands: &Sender<ApplicationCommand>,
) {
    let row = plain_row().title("Recording").build();
    let time = gtk::Label::new(None);
    time.add_css_class("numeric");
    time.set_valign(Align::Center);
    if let Some(clock) = recording.clock {
        time.set_text(&kestrel::view_model::format_clock(clock.elapsed()));
        if !paused {
            let weak = time.downgrade();
            glib::timeout_add_local(CLOCK_TICK, move || {
                let Some(time) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                time.set_text(&kestrel::view_model::format_clock(clock.elapsed()));
                glib::ControlFlow::Continue
            });
        }
    }
    row.add_suffix(&time);
    let toggle = button(if paused { "Resume" } else { "Pause" }, false);
    let sender = commands.clone();
    toggle.connect_clicked(move |_| {
        send(
            &sender,
            if paused {
                RecordingRequest::Resume
            } else {
                RecordingRequest::Pause
            },
        );
    });
    row.add_suffix(&toggle);
    let stop = button("Stop", true);
    stop.update_property(&[Property::Label("Stop and save the recording")]);
    let sender = commands.clone();
    stop.connect_clicked(move |_| send(&sender, RecordingRequest::Stop));
    row.add_suffix(&stop);
    let discard = button("Discard", false);
    discard.add_css_class("destructive-action");
    let sender = commands.clone();
    discard.connect_clicked(move |button| {
        confirm(
            button,
            "Discard this recording?",
            "Recording stops and nothing is saved.",
            "Discard",
            &sender,
            RecordingRequest::Cancel,
        );
    });
    row.add_suffix(&discard);
    group.add(&row);

    for track in &recording.tracks {
        let row = plain_row()
            .title(track.label)
            .subtitle(&track.status)
            .build();
        if track.failed {
            row.add_css_class("warning");
        }
        group.add(&row);
    }
}

fn takes_group(
    recording: &RecordingViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Recent recordings")
        .description(glib::markup_escape_text(&recording.folder.display().to_string()).as_str())
        .build();
    if let Ok(uri) = glib::filename_to_uri(&recording.folder, None) {
        let open = button("Open folder", false);
        let sender = commands.clone();
        let uri = uri.to_string();
        open.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::OpenUri(uri.clone()));
        });
        group.set_header_suffix(Some(&open));
    }
    if let Some(export) = &recording.export {
        group.add(&export_row(export, commands));
    }
    if recording.takes.is_empty() {
        group.add(&plain_row().title("No recordings yet").build());
        return group;
    }
    for take in &recording.takes {
        let row = plain_row().title(&take.name).subtitle(&take.detail).build();
        if let Ok(uri) = glib::filename_to_uri(&take.path, None) {
            let play = button("Play", false);
            play.update_property(&[Property::Label(&format!("Play {}", take.name))]);
            let sender = commands.clone();
            let uri = uri.to_string();
            play.connect_clicked(move |_| {
                let _ = sender.try_send(ApplicationCommand::OpenUri(uri.clone()));
            });
            row.add_suffix(&play);
        }
        let trim = button("Trim", false);
        trim.update_property(&[Property::Label(&format!("Trim {}", take.name))]);
        trim.set_sensitive(
            !recording
                .export
                .as_ref()
                .is_some_and(|export| export.running),
        );
        let sender = commands.clone();
        let path = take.path.clone();
        trim.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::TrimRecording(path.clone()));
        });
        row.add_suffix(&trim);
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_valign(Align::Center);
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some("Delete"));
        delete.update_property(&[Property::Label(&format!("Delete {}", take.name))]);
        let sender = commands.clone();
        let path = take.path.clone();
        let name = take.name.clone();
        delete.connect_clicked(move |button| {
            confirm(
                button,
                "Delete this recording?",
                &format!("{name} is removed from disk."),
                "Delete",
                &sender,
                RecordingRequest::Delete(path.clone()),
            );
        });
        row.add_suffix(&delete);
        group.add(&row);
    }
    group
}

fn export_row(
    export: &RecordingExportViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::ActionRow {
    let row = plain_row()
        .title("Trimmed copy")
        .subtitle(&export.status)
        .build();
    if export.failed {
        row.add_css_class("warning");
    }
    if export.running {
        let cancel = button("Cancel", false);
        cancel.update_property(&[Property::Label("Cancel the export")]);
        let sender = commands.clone();
        cancel.connect_clicked(move |_| send(&sender, RecordingRequest::CancelExport));
        row.add_suffix(&cancel);
        return row;
    }
    if let Some(uri) = export
        .path
        .as_ref()
        .and_then(|path| glib::filename_to_uri(path, None).ok())
    {
        let play = button("Play", false);
        play.update_property(&[Property::Label("Play the trimmed copy")]);
        let sender = commands.clone();
        let uri = uri.to_string();
        play.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::OpenUri(uri.clone()));
        });
        row.add_suffix(&play);
    }
    let dismiss = button("Dismiss", false);
    let sender = commands.clone();
    dismiss.connect_clicked(move |_| send(&sender, RecordingRequest::DismissExport));
    row.add_suffix(&dismiss);
    row
}

fn confirm(
    anchor: &gtk::Button,
    heading: &str,
    body: &str,
    action: &str,
    commands: &Sender<ApplicationCommand>,
    request: RecordingRequest,
) {
    let dialog = adw::MessageDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    if let Some(window) = anchor.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&window));
    }
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    let commands = commands.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "confirm" {
            send(&commands, request.clone());
        }
        dialog.close();
    });
    dialog.present();
}
