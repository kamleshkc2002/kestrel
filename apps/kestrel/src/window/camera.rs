//! Camera preview: choose a camera, start and stop, and show its frames.

use std::{cell::Cell, rc::Rc, time::Duration};

use super::widgets::{plain_row, turn_on_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property, gdk, glib};
use kestrel::{ApplicationCommand, CameraFrameSlot, CameraRequest, CameraViewModel};

/// Frame polling while a preview shows; the camera itself paces frames.
const FRAME_POLL: Duration = Duration::from_millis(33);

pub(super) fn build_camera_group(
    camera: &CameraViewModel,
    frames: Option<&CameraFrameSlot>,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Camera")
        .description(glib::markup_escape_text(&camera.status).as_str())
        .build();
    if let Some(turn_on) = &camera.turn_on {
        group.add(&turn_on_row(turn_on, commands));
    }
    if !camera.running || camera.cameras.is_empty() {
        return group;
    }
    let names = camera
        .cameras
        .iter()
        .map(|choice| choice.name.as_str())
        .collect::<Vec<_>>();
    let chooser = adw::ComboRow::builder()
        .title("Camera")
        .model(&gtk::StringList::new(&names))
        .build();
    chooser.set_sensitive(!camera.active);
    group.add(&chooser);

    let row = plain_row().title("Preview").build();
    let button = gtk::Button::with_label(if camera.active { "Stop" } else { "Start" });
    button.set_valign(Align::Center);
    if !camera.active {
        button.add_css_class("suggested-action");
    }
    button.update_property(&[Property::Label(if camera.active {
        "Stop the camera preview"
    } else {
        "Start the camera preview"
    })]);
    let paths = camera
        .cameras
        .iter()
        .map(|choice| choice.path.clone())
        .collect::<Vec<_>>();
    let active = camera.active;
    let sender = commands.clone();
    let chooser_for_click = chooser.clone();
    button.connect_clicked(move |_| {
        let request = if active {
            CameraRequest::Stop
        } else {
            let index = chooser_for_click.selected() as usize;
            let Some(path) = paths.get(index) else {
                return;
            };
            CameraRequest::Start(path.clone())
        };
        let _ = sender.try_send(ApplicationCommand::Camera(request));
    });
    row.add_suffix(&button);
    group.add(&row);

    if let (true, Some(frames)) = (camera.active, frames) {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .height_request(360)
            .can_shrink(true)
            .build();
        picture.update_property(&[Property::Label("Camera preview")]);
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&picture));
        group.add(&frame);
        show_frames(&picture, frames.clone());
    }
    group
}

fn show_frames(picture: &gtk::Picture, frames: CameraFrameSlot) {
    let picture = picture.downgrade();
    let shown = Rc::new(Cell::new(0u64));
    glib::timeout_add_local(FRAME_POLL, move || {
        let Some(picture) = picture.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if let Some((sequence, frame)) = frames.latest() {
            if sequence != shown.get() {
                shown.set(sequence);
                let bytes = glib::Bytes::from(&frame.rgba[..]);
                let texture = gdk::MemoryTexture::new(
                    frame.width as i32,
                    frame.height as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &bytes,
                    frame.width as usize * 4,
                );
                picture.set_paintable(Some(&texture));
            }
        } else if picture.paintable().is_some() {
            picture.set_paintable(None::<&gdk::Paintable>);
        }
        glib::ControlFlow::Continue
    });
}
