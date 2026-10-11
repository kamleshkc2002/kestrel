//! Trim editor: choose a start and end on a take's timeline, preview the
//! frame at either point, and export the range to a new file.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use super::widgets::{choose_path, plain_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, accessible::Property, gdk, glib};
use kestrel::{ApplicationCommand, RecordingRequest, RgbaImage, TrimPreview};

/// Shortest range worth exporting.
const MIN_LENGTH: f64 = 0.5;

fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    let (whole, tenth) = (tenths / 10, tenths % 10);
    if whole >= 3600 {
        format!(
            "{}:{:02}:{:02}.{tenth}",
            whole / 3600,
            (whole % 3600) / 60,
            whole % 60
        )
    } else {
        format!("{}:{:02}.{tenth}", whole / 60, whole % 60)
    }
}

fn texture(image: &RgbaImage) -> gdk::MemoryTexture {
    gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from(&image.pixels[..]),
        image.width as usize * 4,
    )
}

/// Fetches preview frames off the GTK thread, one at a time; a position
/// asked for while one is loading replaces any earlier waiting one.
struct PreviewLoader {
    preview: TrimPreview,
    picture: gtk::Picture,
    status: gtk::Label,
    busy: Cell<bool>,
    waiting: Cell<Option<f64>>,
}

impl PreviewLoader {
    fn show(self: &Rc<Self>, seconds: f64) {
        if self.busy.replace(true) {
            self.waiting.set(Some(seconds));
            return;
        }
        let (sender, receiver) = async_channel::bounded(1);
        let preview = std::sync::Arc::clone(&self.preview);
        let spawned = std::thread::Builder::new()
            .name("kestrel-trim-preview".to_owned())
            .spawn(move || {
                let _ = sender.send_blocking(preview(Duration::from_secs_f64(seconds)));
            });
        if spawned.is_err() {
            self.busy.set(false);
            self.status.set_text("The preview could not start.");
            return;
        }
        let loader = Rc::clone(self);
        glib::spawn_future_local(async move {
            match receiver.recv().await {
                Ok(Ok(image)) => {
                    loader.picture.set_paintable(Some(&texture(&image)));
                    loader
                        .status
                        .set_text(&format!("Frame at {}", clock(seconds)));
                }
                Ok(Err(error)) => loader.status.set_text(&error),
                Err(_) => {}
            }
            loader.busy.set(false);
            if let Some(next) = loader.waiting.take() {
                loader.show(next);
            }
        });
    }
}

pub(super) fn open_trim_dialog(
    window: &adw::ApplicationWindow,
    take: PathBuf,
    duration: Duration,
    preview: TrimPreview,
    commands: Sender<ApplicationCommand>,
) {
    let name = take
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let total = duration.as_secs_f64();
    let dialog = adw::Window::builder()
        .title("Trim recording")
        .modal(true)
        .transient_for(window)
        .default_width(720)
        .default_height(620)
        .build();
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel);
    let export = gtk::Button::with_label("Export…");
    export.add_css_class("suggested-action");
    export.set_sensitive(total > MIN_LENGTH);
    header.pack_end(&export);

    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .height_request(240)
        .vexpand(true)
        .build();
    picture.update_property(&[Property::Label("Frame at the chosen point")]);
    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    let loader = Rc::new(PreviewLoader {
        preview,
        picture: picture.clone(),
        status: status.clone(),
        busy: Cell::new(false),
        waiting: Cell::new(None),
    });

    let group = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(&name).as_str())
        .description(format!(
            "Length {}. The recording itself is kept.",
            clock(total)
        ))
        .build();
    let scale = |initial: f64, label: &str| {
        let scale = gtk::Scale::with_range(Orientation::Horizontal, 0.0, total.max(0.1), 0.1);
        scale.set_value(initial);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        scale.update_property(&[Property::Label(label)]);
        scale
    };
    let start = scale(0.0, "Start of the kept range");
    let end = scale(total, "End of the kept range");
    let start_time = gtk::Label::new(Some(&clock(0.0)));
    let end_time = gtk::Label::new(Some(&clock(total)));
    for label in [&start_time, &end_time] {
        label.add_css_class("numeric");
        label.set_width_chars(9);
    }
    let start_row = plain_row().title("Start").build();
    start_row.add_suffix(&start);
    start_row.add_suffix(&start_time);
    let end_row = plain_row().title("End").build();
    end_row.add_suffix(&end);
    end_row.add_suffix(&end_time);
    let kept = plain_row().title("Kept").subtitle(clock(total)).build();
    group.add(&start_row);
    group.add(&end_row);
    group.add(&kept);

    // Keeps start before end by at least MIN_LENGTH, moving the other handle.
    let adjusting = Rc::new(Cell::new(false));
    let refresh = {
        let (start, end, kept) = (start.clone(), end.clone(), kept.clone());
        let (start_time, end_time) = (start_time.clone(), end_time.clone());
        let export = export.clone();
        move || {
            let (from, to) = (start.value(), end.value());
            start_time.set_text(&clock(from));
            end_time.set_text(&clock(to));
            kept.set_subtitle(&clock(to - from));
            export.set_sensitive(to - from >= MIN_LENGTH);
        }
    };
    {
        let (end, loader, adjusting, refresh) = (
            end.clone(),
            Rc::clone(&loader),
            Rc::clone(&adjusting),
            refresh.clone(),
        );
        start.connect_value_changed(move |start| {
            if adjusting.replace(true) {
                return;
            }
            if end.value() - start.value() < MIN_LENGTH {
                end.set_value((start.value() + MIN_LENGTH).min(total));
                start.set_value(end.value() - MIN_LENGTH);
            }
            adjusting.set(false);
            refresh();
            loader.show(start.value());
        });
    }
    {
        let (start, loader, adjusting, refresh) = (
            start.clone(),
            Rc::clone(&loader),
            Rc::clone(&adjusting),
            refresh.clone(),
        );
        end.connect_value_changed(move |end| {
            if adjusting.replace(true) {
                return;
            }
            if end.value() - start.value() < MIN_LENGTH {
                start.set_value((end.value() - MIN_LENGTH).max(0.0));
                end.set_value(start.value() + MIN_LENGTH);
            }
            adjusting.set(false);
            refresh();
            // The last frame sits just before the end.
            loader.show((end.value() - 0.1).max(0.0));
        });
    }

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    let stem = take
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recording".to_owned());
    let take = RefCell::new(Some(take));
    {
        let (dialog, window) = (dialog.clone(), window.clone());
        let (start, end) = (start.clone(), end.clone());
        export.connect_clicked(move |_| {
            let Some(take) = take.borrow().clone() else {
                return;
            };
            let range = (
                Duration::from_secs_f64(start.value()),
                Duration::from_secs_f64(end.value()),
            );
            let commands = commands.clone();
            let dialog = dialog.clone();
            choose_path(
                &window,
                "Export trimmed recording",
                "Export",
                Some(&format!("{stem}-trimmed.webm")),
                true,
                move |destination| {
                    let _ = commands.try_send(ApplicationCommand::Recording(
                        RecordingRequest::Export {
                            take,
                            start: range.0,
                            end: range.1,
                            destination,
                        },
                    ));
                    dialog.close();
                },
            );
        });
    }

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.append(&header);
    let body = gtk::Box::new(Orientation::Vertical, 12);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.set_margin_bottom(18);
    body.append(&picture);
    body.append(&status);
    body.append(&group);
    content.append(&body);
    dialog.set_content(Some(&content));
    dialog.present();
    loader.show(0.0);
}
