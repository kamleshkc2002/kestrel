//! Screenshot editor: crop, redact, box, highlight, and arrow tools.
//!
//! Edits stay an `EditPlan` while the window is open; saving sends the plan to
//! the capture service, which applies it to the stored pixels and keeps the
//! source capture.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, accessible::Property, cairo};
use kestrel::{
    ApplicationCommand, CaptureId, CaptureRequest, Color, EditOperation, EditPlan, Rect, RgbaImage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Crop,
    Redact,
    Box,
    Highlight,
    Arrow,
}

const TOOLS: [(Tool, &str, &str); 5] = [
    (
        Tool::Redact,
        "Redact",
        "Paint over a region with solid black",
    ),
    (Tool::Box, "Box", "Draw a red outline"),
    (Tool::Highlight, "Highlight", "Tint a region yellow"),
    (Tool::Arrow, "Arrow", "Draw a red arrow"),
    (Tool::Crop, "Crop", "Keep only the selected region"),
];

/// One undoable change.
enum Step {
    Operation,
    /// The crop that was in place before.
    Crop(Option<Rect>),
}

struct EditorState {
    plan: EditPlan,
    steps: Vec<Step>,
    tool: Tool,
    /// Drag start and current point, in image pixels.
    drag: Option<((i32, i32), (i32, i32))>,
}

/// Maps widget coordinates to image pixels; refreshed on every draw.
#[derive(Clone, Copy)]
struct Viewport {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
}

impl Viewport {
    fn to_image(self, x: f64, y: f64) -> (i32, i32) {
        (
            ((x - self.offset_x) / self.scale).round() as i32,
            ((y - self.offset_y) / self.scale).round() as i32,
        )
    }
}

/// Opens a modal editor for one capture.
pub fn open(
    parent: &adw::ApplicationWindow,
    id: CaptureId,
    image: RgbaImage,
    commands: Sender<ApplicationCommand>,
) {
    let Some(surface) = surface_from(&image) else {
        return;
    };
    let width = image.width;
    let height = image.height;
    let thickness = (width.max(height) / 400).max(3);
    let state = Rc::new(RefCell::new(EditorState {
        plan: EditPlan::default(),
        steps: Vec::new(),
        tool: Tool::Redact,
        drag: None,
    }));
    let viewport = Rc::new(Cell::new(Viewport {
        scale: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
    }));

    let window = adw::Window::builder()
        .title("Edit screenshot")
        .modal(true)
        .transient_for(parent)
        .default_width(960)
        .default_height(720)
        .build();
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save copy");
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    save.update_property(&[Property::Description(
        "Adds the edited image to recent captures and keeps the original",
    )]);
    header.pack_start(&cancel);
    header.pack_end(&save);

    let tools = gtk::Box::new(Orientation::Horizontal, 6);
    tools.set_margin_top(6);
    tools.set_margin_bottom(6);
    tools.set_margin_start(12);
    tools.set_margin_end(12);
    let mut first: Option<gtk::ToggleButton> = None;
    for (tool, label, description) in TOOLS {
        let button = gtk::ToggleButton::with_label(label);
        button.set_tooltip_text(Some(description));
        button.update_property(&[Property::Description(description)]);
        match &first {
            Some(group) => button.set_group(Some(group)),
            None => {
                button.set_active(true);
                first = Some(button.clone());
            }
        }
        let state = Rc::clone(&state);
        button.connect_toggled(move |button| {
            if button.is_active() {
                state.borrow_mut().tool = tool;
            }
        });
        tools.append(&button);
    }
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_tooltip_text(Some("Undo"));
    undo.update_property(&[Property::Label("Undo the last edit")]);
    undo.set_sensitive(false);
    let hint = gtk::Label::new(Some(
        "Drag on the image. Redaction replaces pixels in the saved copy.",
    ));
    hint.add_css_class("dim-label");
    hint.set_hexpand(true);
    hint.set_xalign(1.0);
    tools.append(&undo);
    tools.append(&hint);

    let area = gtk::DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    area.update_property(&[Property::Label("Screenshot being edited")]);
    {
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
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
                current,
                &state.borrow(),
                width,
                height,
                thickness,
            );
        });
    }

    let refresh_controls = {
        let state = Rc::clone(&state);
        let save = save.clone();
        let undo = undo.clone();
        let area = area.clone();
        move || {
            let state = state.borrow();
            let edited = state.plan.crop.is_some() || !state.plan.operations.is_empty();
            save.set_sensitive(edited);
            undo.set_sensitive(!state.steps.is_empty());
            area.queue_draw();
        }
    };

    let drag = gtk::GestureDrag::new();
    {
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
        let area = area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let point = viewport.get().to_image(x, y);
            state.borrow_mut().drag = Some((point, point));
            area.queue_draw();
        });
    }
    {
        let state = Rc::clone(&state);
        let viewport = Rc::clone(&viewport);
        let area = area.clone();
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let Some((start_x, start_y)) = gesture.start_point() else {
                return;
            };
            let point = viewport
                .get()
                .to_image(start_x + offset_x, start_y + offset_y);
            if let Some((_, current)) = state.borrow_mut().drag.as_mut() {
                *current = point;
            }
            area.queue_draw();
        });
    }
    {
        let state = Rc::clone(&state);
        let refresh_controls = refresh_controls.clone();
        drag.connect_drag_end(move |_, _, _| {
            {
                let mut state = state.borrow_mut();
                if let Some((from, to)) = state.drag.take() {
                    commit(&mut state, from, to, thickness);
                }
            }
            refresh_controls();
        });
    }
    area.add_controller(drag);

    {
        let state = Rc::clone(&state);
        let refresh_controls = refresh_controls.clone();
        undo.connect_clicked(move |_| {
            {
                let mut state = state.borrow_mut();
                match state.steps.pop() {
                    Some(Step::Operation) => {
                        state.plan.operations.pop();
                    }
                    Some(Step::Crop(previous)) => state.plan.crop = previous,
                    None => {}
                }
            }
            refresh_controls();
        });
    }
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    {
        let window = window.clone();
        let state = Rc::clone(&state);
        save.connect_clicked(move |_| {
            let plan = state.borrow().plan.clone();
            let _ = commands.try_send(ApplicationCommand::Capture(CaptureRequest::SaveEdit {
                id,
                plan,
            }));
            window.close();
        });
    }

    let content = gtk::Box::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&tools);
    content.append(&area);
    window.set_content(Some(&content));
    window.present();
}

/// Records a finished drag; empty drags are ignored.
fn commit(state: &mut EditorState, from: (i32, i32), to: (i32, i32), thickness: u32) {
    let rect = Rect::normalized(from, to);
    let empty = rect.width == 0 || rect.height == 0;
    let operation = match state.tool {
        Tool::Crop => {
            if !empty {
                let previous = state.plan.crop.replace(rect);
                state.steps.push(Step::Crop(previous));
            }
            return;
        }
        Tool::Arrow if from == to => return,
        Tool::Arrow => EditOperation::Arrow {
            from,
            to,
            color: Color::RED,
            thickness,
        },
        _ if empty => return,
        Tool::Redact => EditOperation::Redact(rect),
        Tool::Box => EditOperation::Rectangle {
            rect,
            color: Color::RED,
            thickness,
        },
        Tool::Highlight => EditOperation::Highlight {
            rect,
            color: Color::YELLOW,
        },
    };
    state.plan.operations.push(operation);
    state.steps.push(Step::Operation);
}

/// Premultiplied ARGB32 copy of the capture for drawing.
fn surface_from(image: &RgbaImage) -> Option<cairo::ImageSurface> {
    let width = i32::try_from(image.width).ok()?;
    let height = i32::try_from(image.height).ok()?;
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    let stride = usize::try_from(surface.stride()).ok()?;
    {
        let mut data = surface.data().ok()?;
        for (row, source_row) in image
            .pixels
            .chunks_exact(image.width as usize * 4)
            .enumerate()
        {
            let target_row = &mut data[row * stride..row * stride + image.width as usize * 4];
            let (targets, _) = target_row.as_chunks_mut::<4>();
            let (sources, _) = source_row.as_chunks::<4>();
            for (target, source) in targets.iter_mut().zip(sources) {
                let alpha = u32::from(source[3]);
                let premultiply = |channel: u8| (u32::from(channel) * alpha / 255) as u8;
                // Cairo stores native-endian 0xAARRGGBB.
                let pixel = u32::from(source[3]) << 24
                    | u32::from(premultiply(source[0])) << 16
                    | u32::from(premultiply(source[1])) << 8
                    | u32::from(premultiply(source[2]));
                *target = pixel.to_ne_bytes();
            }
        }
    }
    Some(surface)
}

fn set_color(context: &cairo::Context, color: Color) {
    context.set_source_rgba(
        f64::from(color.r) / 255.0,
        f64::from(color.g) / 255.0,
        f64::from(color.b) / 255.0,
        f64::from(color.a) / 255.0,
    );
}

fn rectangle(context: &cairo::Context, rect: Rect) {
    context.rectangle(
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.width),
        f64::from(rect.height),
    );
}

fn draw_operation(context: &cairo::Context, operation: &EditOperation) -> Result<(), cairo::Error> {
    match *operation {
        EditOperation::Redact(rect) => {
            set_color(context, Color::BLACK);
            rectangle(context, rect);
            context.fill()
        }
        EditOperation::Rectangle {
            rect,
            color,
            thickness,
        } => {
            set_color(context, color);
            context.set_line_width(f64::from(thickness));
            let inset = f64::from(thickness) / 2.0;
            context.rectangle(
                f64::from(rect.x) + inset,
                f64::from(rect.y) + inset,
                (f64::from(rect.width) - f64::from(thickness)).max(0.0),
                (f64::from(rect.height) - f64::from(thickness)).max(0.0),
            );
            context.stroke()
        }
        EditOperation::Highlight { rect, color } => {
            set_color(context, color);
            rectangle(context, rect);
            context.fill()
        }
        EditOperation::Arrow {
            from,
            to,
            color,
            thickness,
        } => {
            set_color(context, color);
            let (from_x, from_y) = (f64::from(from.0), f64::from(from.1));
            let (to_x, to_y) = (f64::from(to.0), f64::from(to.1));
            let angle = (to_y - from_y).atan2(to_x - from_x);
            let head = f64::from(thickness) * 4.0;
            context.set_line_width(f64::from(thickness));
            context.move_to(from_x, from_y);
            context.line_to(
                to_x - head * 0.8 * angle.cos(),
                to_y - head * 0.8 * angle.sin(),
            );
            context.stroke()?;
            context.move_to(to_x, to_y);
            for side in [-0.45_f64, 0.45] {
                context.line_to(
                    to_x - head * (angle + side).cos(),
                    to_y - head * (angle + side).sin(),
                );
            }
            context.close_path();
            context.fill()
        }
    }
}

fn draw(
    context: &cairo::Context,
    surface: &cairo::ImageSurface,
    viewport: Viewport,
    state: &EditorState,
    width: u32,
    height: u32,
    thickness: u32,
) -> Result<(), cairo::Error> {
    context.translate(viewport.offset_x, viewport.offset_y);
    context.scale(viewport.scale, viewport.scale);
    context.set_source_surface(surface, 0.0, 0.0)?;
    context.paint()?;
    for operation in &state.plan.operations {
        draw_operation(context, operation)?;
    }
    let mut crop = state.plan.crop;
    if let Some((from, to)) = state.drag {
        let mut preview = EditorState {
            plan: EditPlan::default(),
            steps: Vec::new(),
            tool: state.tool,
            drag: None,
        };
        commit(&mut preview, from, to, thickness);
        if let Some(operation) = preview.plan.operations.first() {
            draw_operation(context, operation)?;
        }
        if preview.plan.crop.is_some() {
            crop = preview.plan.crop;
        }
    }
    if let Some(crop) = crop {
        // Dim everything outside the kept region.
        context.set_fill_rule(cairo::FillRule::EvenOdd);
        context.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
        rectangle(context, crop);
        context.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        context.fill()?;
        context.set_fill_rule(cairo::FillRule::Winding);
        context.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        context.set_line_width(2.0 / viewport.scale);
        rectangle(context, crop);
        context.stroke()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tool: Tool) -> EditorState {
        EditorState {
            plan: EditPlan::default(),
            steps: Vec::new(),
            tool,
            drag: None,
        }
    }

    #[test]
    fn drags_become_plan_entries_in_any_direction_and_empty_drags_are_dropped() {
        let mut editor = state(Tool::Redact);
        commit(&mut editor, (40, 30), (10, 5), 3);
        commit(&mut editor, (7, 7), (7, 20), 3);
        assert_eq!(
            editor.plan.operations,
            vec![EditOperation::Redact(Rect {
                x: 10,
                y: 5,
                width: 30,
                height: 25,
            })]
        );

        editor.tool = Tool::Crop;
        commit(&mut editor, (0, 0), (5, 5), 3);
        commit(&mut editor, (1, 1), (9, 9), 3);
        assert_eq!(editor.steps.len(), 3);
        let Some(Step::Crop(previous)) = editor.steps.pop() else {
            panic!("the last step is a crop");
        };
        assert_eq!(
            previous,
            Some(Rect {
                x: 0,
                y: 0,
                width: 5,
                height: 5,
            }),
            "undo restores the earlier crop"
        );
    }
}
