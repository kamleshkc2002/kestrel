//! Screen tools: read text, read QR codes, and pick colors.

use super::widgets::{plain_row, turn_on_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{
    ApplicationCommand, ScreenToolRequest, ScreenToolResultViewModel, ScreenToolsViewModel,
};

pub(super) fn build_screen_tools_group(
    tools: &ScreenToolsViewModel,
    commands: &Sender<ApplicationCommand>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Screen tools")
        .description(gtk::glib::markup_escape_text(&tools.status).as_str())
        .build();
    let row = plain_row().title("Read from the screen").build();
    for button in &tools.buttons {
        let widget = gtk::Button::with_label(button.label);
        widget.set_valign(Align::Center);
        widget.set_sensitive(button.enabled && !tools.busy);
        if let Some(reason) = &button.reason {
            widget.set_tooltip_text(Some(reason));
            widget.update_property(&[Property::Description(reason)]);
        }
        let sender = commands.clone();
        let tool = button.tool;
        widget.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ScreenTools(ScreenToolRequest::Begin(
                tool,
            )));
        });
        row.add_suffix(&widget);
    }
    if tools.busy {
        row.add_suffix(&request_button(
            "Cancel",
            ScreenToolRequest::Cancel,
            commands,
        ));
    }
    group.add(&row);
    for button in tools.buttons.iter().filter(|button| !button.enabled) {
        if let Some(turn_on) = &button.turn_on {
            group.add(&turn_on_row(turn_on, commands));
        } else if let Some(reason) = button.reason.as_deref() {
            group.add(
                &plain_row()
                    .title(format!("{} is unavailable", button.label))
                    .subtitle(reason)
                    .subtitle_lines(0)
                    .build(),
            );
        }
    }
    if let Some(result) = &tools.result {
        add_result(&group, result, commands);
        let dismiss = gtk::Button::with_label("Clear result");
        dismiss.add_css_class("flat");
        dismiss.set_halign(Align::End);
        dismiss.set_margin_top(6);
        let sender = commands.clone();
        dismiss.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::ScreenTools(ScreenToolRequest::Dismiss));
        });
        group.add(&dismiss);
    }
    group
}

fn add_result(
    group: &adw::PreferencesGroup,
    result: &ScreenToolResultViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    match result {
        ScreenToolResultViewModel::Text(text) => {
            let view = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(8)
                .bottom_margin(8)
                .left_margin(8)
                .right_margin(8)
                .build();
            view.buffer().set_text(text);
            view.update_property(&[Property::Label("Recognized text")]);
            let scroller = gtk::ScrolledWindow::builder()
                .child(&view)
                .min_content_height(80)
                .max_content_height(240)
                .propagate_natural_height(true)
                .build();
            let frame = gtk::Frame::new(None);
            frame.set_child(Some(&scroller));
            group.add(&frame);
            let row = plain_row().title("Recognized text").build();
            row.add_suffix(&copy_button("Copy text", text.clone(), commands));
            group.add(&row);
        }
        ScreenToolResultViewModel::Qr(codes) => {
            for code in codes {
                let row = plain_row().title(&code.text).build();
                row.set_title_lines(0);
                if let Some(uri) = &code.open {
                    let button = gtk::Button::with_label("Open");
                    button.set_valign(Align::Center);
                    button.add_css_class("flat");
                    button.set_tooltip_text(Some(&format!("Open {uri}")));
                    let sender = commands.clone();
                    let uri = uri.clone();
                    button.connect_clicked(move |_| {
                        let _ = sender.try_send(ApplicationCommand::OpenUri(uri.clone()));
                    });
                    row.add_suffix(&button);
                }
                if let Some(text) = &code.copy {
                    row.add_suffix(&copy_button("Copy", text.clone(), commands));
                }
                group.add(&row);
            }
        }
        ScreenToolResultViewModel::Color {
            rgb,
            hex,
            css_rgb,
            hsl,
        } => {
            let swatch = gtk::DrawingArea::builder()
                .content_width(48)
                .content_height(48)
                .valign(Align::Center)
                .build();
            let [red, green, blue] = rgb.map(|channel| f64::from(channel) / 255.0);
            swatch.set_draw_func(move |_, context, width, height| {
                context.set_source_rgb(red, green, blue);
                context.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
                let _ = context.fill();
            });
            swatch.update_property(&[Property::Label(&format!("Picked color {hex}"))]);
            let row = plain_row().title("Picked color").build();
            row.add_prefix(&swatch);
            group.add(&row);
            for value in [hex, css_rgb, hsl] {
                let row = plain_row().title(value).build();
                row.add_suffix(&copy_button("Copy", value.clone(), commands));
                group.add(&row);
            }
        }
    }
}

fn copy_button(label: &str, text: String, commands: &Sender<ApplicationCommand>) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.set_valign(Align::Center);
    button.add_css_class("flat");
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CopyText(text.clone()));
    });
    button
}

fn request_button(
    label: &str,
    request: ScreenToolRequest,
    commands: &Sender<ApplicationCommand>,
) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.set_valign(Align::Center);
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::ScreenTools(request));
    });
    button
}
