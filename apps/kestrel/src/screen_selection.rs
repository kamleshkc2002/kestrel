//! Selection view over a captured frame: drag a region (screen tools, area
//! recording), or click a pixel to sample, with a magnifier that follows the
//! pointer.
//!
//! The view holds the frame only while open; closing it without a choice
//! cancels.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use gtk::{Orientation, accessible::Property, cairo, gdk};
use kestrel::{Rect, RgbaImage, ScreenTool, Selection};

use crate::capture_editor::surface_from;

const MAGNIFIER_RADIUS: i32 = 5;
const MAGNIFIER_ZOOM: f64 = 12.0;
const MAGNIFIER_OFFSET: f64 = 24.0;

#[derive(Clone, Copy)]
struct Viewport {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
}

impl Viewport {
    fn to_image(self, x: f64, y: f64) -> (i32, i32) {
        (
            ((x - self.offset_x) / self.scale).floor() as i32,
            ((y - self.offset_y) / self.scale).floor() as i32,
        )
    }

    fn to_widget(self, x: i32, y: i32) -> (f64, f64) {
        (
            f64::from(x) * self.scale + self.offset_x,
            f64::from(y) * self.scale + self.offset_y,
        )
    }
}

#[derive(Default)]
struct SelectionState {
    cursor: Option<(i32, i32)>,
    drag: Option<((i32, i32), (i32, i32))>,
    region: Option<Rect>,
}
#[derive(Clone, Copy)]
pub struct SelectionPurpose {
    title: &'static str,
    action: &'static str,
    hint: &'static str,
    label: &'static str,
    whole: &'static str,
    picking: bool,
}

impl SelectionPurpose {
    pub fn for_tool(tool: ScreenTool) -> Self {
        if tool == ScreenTool::Color {
            Self {
                title: tool.label(),
                action: "Pick",
                hint: "Click a pixel, or move with the arrow keys and press Enter.",
                label: "Capture to pick a color from",
                whole: "",
                picking: true,
            }
        } else {
            Self {
                title: tool.label(),
                action: "Read",
                hint: "Drag to select an area, or read the whole capture.",
                label: "Capture to select an area from",
                whole: "Whole capture",
                picking: false,
            }
        }
    }

    pub const RECORD_AREA: Self = Self {
        title: "Record an area",
        action: "Record",
        hint: "Drag the area to record, or record the whole screen.",
        label: "Shared screen to select the recording area from",
        whole: "Whole screen",
        picking: false,
    };
}

pub fn open(
    parent: &adw::ApplicationWindow,
    purpose: SelectionPurpose,
    frame: Arc<RgbaImage>,
    on_choice: impl FnOnce(Option<Selection>) + 'static,
) {
    let on_choice = Cell::new(Some(on_choice));
    let answer = move |choice: Option<Selection>| {
        if let Some(callback) = on_choice.take() {
            callback(choice);
        }
    };
    let Some(surface) = surface_from(&frame) else {
        answer(None);
        return;
    };
    let (width, height) = (frame.width as i32, frame.height as i32);
    let picking = purpose.picking;
    let state = Rc::new(RefCell::new(SelectionState::default()));
    let viewport = Rc::new(Cell::new(Viewport {
        scale: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
    }));

    let window = adw::Window::builder()
        .title(purpose.title)
        .modal(true)
        .transient_for(parent)
        .default_width(960)
        .default_height(720)
        .build();
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel);
    let read = gtk::Button::with_label(purpose.action);
    read.add_css_class("suggested-action");
    read.set_sensitive(!picking);
    header.pack_end(&read);

    let hint = gtk::Label::new(Some(purpose.hint));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_margin_top(6);
    hint.set_margin_bottom(6);
    let readout = gtk::Label::new(None);
    readout.set_selectable(true);
    readout.set_margin_bottom(6);

    let area = gtk::DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .build();
    area.update_property(&[Property::Label(purpose.label)]);
    {
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
        let frame = Arc::clone(&frame);
        area.set_draw_func(move |_, context, area_width, area_height| {
            let scale = (f64::from(area_width) / f64::from(width))
                .min(f64::from(area_height) / f64::from(height))
                .min(1.0);
            let current = Viewport {
                scale,
                offset_x: (f64::from(area_width) - f64::from(width) * scale) / 2.0,
                offset_y: (f64::from(area_height) - f64::from(height) * scale) / 2.0,
            };
            viewport.set(current);
            let _ = draw(
                context,
                &surface,
                &frame,
                current,
                &state.borrow(),
                (area_width, area_height),
            );
        });
    }

    let refresh = {
        let state = Rc::clone(&state);
        let frame = Arc::clone(&frame);
        let readout = readout.clone();
        let read = read.clone();
        let area = area.clone();
        move || {
            let state = state.borrow();
            let text = match (picking, state.cursor, state.region) {
                (true, Some((x, y)), _) => pixel_hex(&frame, x, y)
                    .map(|hex| format!("{hex} at {x}, {y}"))
                    .unwrap_or_default(),
                (false, _, Some(region)) => format!(
                    "{} × {} px at {}, {}",
                    region.width, region.height, region.x, region.y
                ),
                (false, _, None) => format!("{}, {width} × {height} px", purpose.whole),
                _ => String::new(),
            };
            readout.set_text(&text);
            if picking {
                read.set_sensitive(
                    state
                        .cursor
                        .is_some_and(|(x, y)| pixel_hex(&frame, x, y).is_some()),
                );
            }
            area.queue_draw();
        }
    };
    refresh();

    let motion = gtk::EventControllerMotion::new();
    {
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
        let refresh = refresh.clone();
        motion.connect_motion(move |_, x, y| {
            state.borrow_mut().cursor = Some(viewport.get().to_image(x, y));
            refresh();
        });
    }
    area.add_controller(motion);

    let answer = Rc::new(answer);
    let finish = {
        let window = window.clone();
        let answer = Rc::clone(&answer);
        move |choice: Option<Selection>| {
            answer(choice);
            window.close();
        }
    };
    let choose = {
        let state = Rc::clone(&state);
        let frame = Arc::clone(&frame);
        let finish = finish.clone();
        move || {
            let state = state.borrow();
            let selection = if picking {
                let Some((x, y)) = state
                    .cursor
                    .filter(|(x, y)| pixel_hex(&frame, *x, *y).is_some())
                else {
                    return;
                };
                Selection::Pixel {
                    x: x as u32,
                    y: y as u32,
                }
            } else {
                Selection::Region(state.region)
            };
            drop(state);
            finish(Some(selection));
        }
    };

    if picking {
        let click = gtk::GestureClick::new();
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
        let choose = choose.clone();
        click.connect_released(move |_, _, x, y| {
            state.borrow_mut().cursor = Some(viewport.get().to_image(x, y));
            choose();
        });
        area.add_controller(click);
    } else {
        let drag = gtk::GestureDrag::new();
        {
            let state = Rc::clone(&state);
            let viewport = Rc::clone(&viewport);
            drag.connect_drag_begin(move |_, x, y| {
                let point = viewport.get().to_image(x, y);
                state.borrow_mut().drag = Some((point, point));
            });
        }
        {
            let state = Rc::clone(&state);
            let viewport = Rc::clone(&viewport);
            let refresh = refresh.clone();
            drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                let Some((start_x, start_y)) = gesture.start_point() else {
                    return;
                };
                let point = viewport
                    .get()
                    .to_image(start_x + offset_x, start_y + offset_y);
                {
                    let mut state = state.borrow_mut();
                    state.cursor = Some(point);
                    if let Some((_, current)) = state.drag.as_mut() {
                        *current = point;
                    }
                }
                refresh();
            });
        }
        {
            let state = Rc::clone(&state);
            let refresh = refresh.clone();
            drag.connect_drag_end(move |_, _, _| {
                {
                    let mut state = state.borrow_mut();
                    if let Some((from, to)) = state.drag.take() {
                        let rect = Rect::normalized(from, to);
                        state.region = (rect.width >= 4 && rect.height >= 4).then_some(rect);
                    }
                }
                refresh();
            });
        }
        area.add_controller(drag);
    }

    let keys = gtk::EventControllerKey::new();
    {
        let state = Rc::clone(&state);
        let refresh = refresh.clone();
        let choose = choose.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let step = match key {
                gdk::Key::Left => (-1, 0),
                gdk::Key::Right => (1, 0),
                gdk::Key::Up => (0, -1),
                gdk::Key::Down => (0, 1),
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    choose();
                    return gtk::glib::Propagation::Stop;
                }
                _ => return gtk::glib::Propagation::Proceed,
            };
            {
                let mut state = state.borrow_mut();
                let (x, y) = state.cursor.unwrap_or((width / 2, height / 2));
                state.cursor = Some((
                    (x + step.0).clamp(0, width - 1),
                    (y + step.1).clamp(0, height - 1),
                ));
            }
            refresh();
            gtk::glib::Propagation::Stop
        });
    }
    area.add_controller(keys);

    {
        let choose = choose.clone();
        read.connect_clicked(move |_| choose());
    }
    {
        let finish = finish.clone();
        cancel.connect_clicked(move |_| finish(None));
    }
    window.connect_close_request(move |_| {
        answer(None);
        gtk::glib::Propagation::Proceed
    });

    let content = gtk::Box::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&hint);
    content.append(&area);
    content.append(&readout);
    window.set_content(Some(&content));
    window.present();
    area.grab_focus();
}

fn pixel_hex(frame: &RgbaImage, x: i32, y: i32) -> Option<String> {
    let [red, green, blue] = pixel(frame, x, y)?;
    Some(format!("#{red:02x}{green:02x}{blue:02x}"))
}

fn pixel(frame: &RgbaImage, x: i32, y: i32) -> Option<[u8; 3]> {
    let x = u32::try_from(x).ok().filter(|x| *x < frame.width)?;
    let y = u32::try_from(y).ok().filter(|y| *y < frame.height)?;
    let index = (y as usize * frame.width as usize + x as usize) * 4;
    let pixel = frame.pixels.get(index..index + 3)?;
    Some([pixel[0], pixel[1], pixel[2]])
}

fn draw(
    context: &cairo::Context,
    surface: &cairo::ImageSurface,
    frame: &RgbaImage,
    viewport: Viewport,
    state: &SelectionState,
    (area_width, area_height): (i32, i32),
) -> Result<(), cairo::Error> {
    context.save()?;
    context.translate(viewport.offset_x, viewport.offset_y);
    context.scale(viewport.scale, viewport.scale);
    context.set_source_surface(surface, 0.0, 0.0)?;
    context.paint()?;
    let region = state
        .drag
        .map(|(from, to)| Rect::normalized(from, to))
        .or(state.region);
    if let Some(region) = region {
        context.set_fill_rule(cairo::FillRule::EvenOdd);
        context.rectangle(0.0, 0.0, f64::from(frame.width), f64::from(frame.height));
        rectangle(context, region);
        context.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        context.fill()?;
        context.set_fill_rule(cairo::FillRule::Winding);
        context.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        context.set_line_width(2.0 / viewport.scale);
        rectangle(context, region);
        context.stroke()?;
    }
    context.restore()?;
    if let Some((x, y)) = state.cursor.filter(|(x, y)| pixel(frame, *x, *y).is_some()) {
        draw_magnifier(
            context,
            surface,
            viewport,
            (x, y),
            (area_width, area_height),
        )?;
    }
    Ok(())
}

fn draw_magnifier(
    context: &cairo::Context,
    surface: &cairo::ImageSurface,
    viewport: Viewport,
    (x, y): (i32, i32),
    (area_width, area_height): (i32, i32),
) -> Result<(), cairo::Error> {
    let side = f64::from(2 * MAGNIFIER_RADIUS + 1) * MAGNIFIER_ZOOM;
    let (pointer_x, pointer_y) = viewport.to_widget(x, y);
    let mut left = pointer_x + MAGNIFIER_OFFSET;
    let mut top = pointer_y + MAGNIFIER_OFFSET;
    if left + side > f64::from(area_width) {
        left = pointer_x - MAGNIFIER_OFFSET - side;
    }
    if top + side > f64::from(area_height) {
        top = pointer_y - MAGNIFIER_OFFSET - side;
    }
    let left = left.max(0.0);
    let top = top.max(0.0);
    context.save()?;
    context.rectangle(left, top, side, side);
    context.clip();
    context.set_source_rgb(0.2, 0.2, 0.2);
    context.paint()?;
    context.translate(left, top);
    context.scale(MAGNIFIER_ZOOM, MAGNIFIER_ZOOM);
    context.set_source_surface(
        surface,
        f64::from(MAGNIFIER_RADIUS - x),
        f64::from(MAGNIFIER_RADIUS - y),
    )?;
    context.source().set_filter(cairo::Filter::Nearest);
    context.paint()?;
    context.restore()?;
    let center = f64::from(MAGNIFIER_RADIUS) * MAGNIFIER_ZOOM;
    context.set_line_width(2.0);
    context.set_source_rgb(1.0, 1.0, 1.0);
    context.rectangle(left + center, top + center, MAGNIFIER_ZOOM, MAGNIFIER_ZOOM);
    context.stroke()?;
    context.set_source_rgb(0.0, 0.0, 0.0);
    context.rectangle(left, top, side, side);
    context.stroke()?;
    Ok(())
}

fn rectangle(context: &cairo::Context, rect: Rect) {
    context.rectangle(
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.width),
        f64::from(rect.height),
    );
}
