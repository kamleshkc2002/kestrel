//! Screenshot editor: crop, redact, box, highlight, spotlight, and arrow
//! tools, a growable canvas, and pasted images.
//!
//! Edits stay an `EditPlan` while the window is open; saving sends the plan to
//! the capture service, which applies it to the stored pixels and keeps the
//! source capture. Coordinates are the source image's, so space added on the
//! left or top is negative and resizing the canvas never moves an edit.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, accessible::Property, cairo, gdk, glib};
use kestrel::{
    ApplicationCommand, CanvasExpansion, CaptureId, CaptureRequest, Color,
    DEFAULT_SPOTLIGHT_DIM_PERCENT, EditOperation, EditPlan, MAX_CANVAS_PADDING, MAX_CANVAS_PIXELS,
    MAX_PASTED_IMAGES, MAX_SPOTLIGHT_DIM_PERCENT, MIN_SPOTLIGHT_DIM_PERCENT, Rect, RgbaImage,
};
use kestrel_platform::capture::MAX_CAPTURE_DIMENSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Crop,
    Redact,
    Box,
    Highlight,
    Spotlight,
    Arrow,
}

const TOOLS: [(Tool, &str, &str); 6] = [
    (
        Tool::Redact,
        "Redact",
        "Paint over a region with solid black",
    ),
    (Tool::Box, "Box", "Draw a red outline"),
    (Tool::Highlight, "Highlight", "Tint a region yellow"),
    (
        Tool::Spotlight,
        "Spotlight",
        "Dim everything outside the selected areas",
    ),
    (Tool::Arrow, "Arrow", "Draw a red arrow"),
    (Tool::Crop, "Crop", "Keep only the selected region"),
];

/// Shortest side a pasted image can be resized to, in source pixels.
const MIN_PASTE_SIDE: f64 = 8.0;
/// How close to a corner, in screen pixels, a drag resizes a pasted image.
const HANDLE_RADIUS: f64 = 10.0;

const DEFAULT_HINT: &str = "Drag on the image. Redaction replaces pixels in the saved copy.";
const PENDING_HINT: &str = "Drag the pasted image to move it or a corner to resize it, then Place.";
const CANVAS_TOO_LARGE_HINT: &str =
    "The canvas is too large to save: at most 64 megapixels and 16,384 pixels per side.";

/// Reaches one side of a canvas expansion.
type CanvasSide = fn(&mut CanvasExpansion) -> &mut u32;

/// The canvas sides the Canvas panel edits, by label.
const SIDES: [(&str, CanvasSide); 4] = [
    ("Top", |canvas| &mut canvas.top),
    ("Right", |canvas| &mut canvas.right),
    ("Bottom", |canvas| &mut canvas.bottom),
    ("Left", |canvas| &mut canvas.left),
];

/// Background presets; anything else is Custom.
const BACKGROUNDS: [(&str, Color); 3] = [
    ("White", Color::WHITE),
    ("Black", Color::BLACK),
    ("Transparent", Color::TRANSPARENT),
];

enum Step {
    Operation,
    Crop(Option<Rect>),
    Canvas(CanvasExpansion),
}

/// A pasted image still being moved and resized.
struct PendingPaste {
    image: RgbaImage,
    surface: cairo::ImageSurface,
    rect: Rect,
}

/// What a drag on a pending paste does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adjust {
    /// Moves the image, keeping the grabbed point under the pointer.
    Move { grab: (i32, i32) },
    /// Resizes from the corner that stays fixed.
    Resize { anchor: (i32, i32) },
}

struct EditorState {
    plan: EditPlan,
    /// Drawing surfaces for `plan.pasted`, by index.
    pasted_surfaces: Vec<cairo::ImageSurface>,
    steps: Vec<Step>,
    tool: Tool,
    drag: Option<((i32, i32), (i32, i32))>,
    pending: Option<PendingPaste>,
    adjust: Option<Adjust>,
}

impl EditorState {
    fn new(tool: Tool) -> Self {
        Self {
            plan: EditPlan::default(),
            pasted_surfaces: Vec::new(),
            steps: Vec::new(),
            tool,
            drag: None,
            pending: None,
            adjust: None,
        }
    }

    /// The canvas in source coordinates.
    fn canvas_rect(&self, width: u32, height: u32) -> Rect {
        let canvas = self.plan.canvas;
        Rect {
            x: -(canvas.left as i32),
            y: -(canvas.top as i32),
            width: width + canvas.left + canvas.right,
            height: height + canvas.top + canvas.bottom,
        }
    }

    /// The canvas is within the limits `apply` enforces, so saving cannot be
    /// refused for its size after the window closes.
    fn canvas_fits(&self, width: u32, height: u32) -> bool {
        let canvas = self.canvas_rect(width, height);
        u64::from(canvas.width) * u64::from(canvas.height) <= MAX_CANVAS_PIXELS
            && canvas.width <= MAX_CAPTURE_DIMENSION
            && canvas.height <= MAX_CAPTURE_DIMENSION
    }

    fn edited(&self) -> bool {
        self.plan.crop.is_some()
            || !self.plan.operations.is_empty()
            || !self.plan.canvas.is_empty()
            || self.pending.is_some()
    }

    /// Turns the pending paste into an edit that Undo removes.
    fn place(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.adjust = None;
        let image = self.plan.pasted.len();
        self.plan.pasted.push(pending.image);
        self.pasted_surfaces.push(pending.surface);
        self.plan.operations.push(EditOperation::Paste {
            image,
            rect: pending.rect,
        });
        self.steps.push(Step::Operation);
    }

    /// Cancels a pending paste, or else undoes the last edit.
    fn undo(&mut self) {
        if self.pending.take().is_some() {
            self.adjust = None;
            return;
        }
        match self.steps.pop() {
            Some(Step::Operation) => {
                if let Some(EditOperation::Paste { image, .. }) = self.plan.operations.pop() {
                    self.plan.pasted.truncate(image);
                    self.pasted_surfaces.truncate(image);
                }
            }
            Some(Step::Crop(previous)) => self.plan.crop = previous,
            Some(Step::Canvas(previous)) => self.plan.canvas = previous,
            None => {}
        }
    }
}

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
    let state = Rc::new(RefCell::new(EditorState::new(Tool::Redact)));
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

    let area = gtk::DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    area.update_property(&[Property::Label("Screenshot being edited")]);
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_tooltip_text(Some("Undo"));
    undo.update_property(&[Property::Label("Undo the last edit")]);
    undo.set_sensitive(false);
    let place = gtk::Button::with_label("Place");
    place.add_css_class("suggested-action");
    place.set_tooltip_text(Some("Keep the pasted image here (Enter)"));
    reveal(&place, false);
    // Its own line under the toolbar, so every message stays readable; the
    // line is always present, so a new message never moves the image.
    let hint = gtk::Label::new(Some(DEFAULT_HINT));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_margin_start(12);
    hint.set_margin_end(12);
    hint.set_margin_bottom(6);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let refresh: Rc<dyn Fn()> = {
        let (state, save, undo, place, hint, area) = (
            Rc::clone(&state),
            save.clone(),
            undo.clone(),
            place.clone(),
            hint.clone(),
            area.clone(),
        );
        Rc::new(move || {
            let state = state.borrow();
            let pending = state.pending.is_some();
            let fits = state.canvas_fits(width, height);
            save.set_sensitive(state.edited() && fits);
            undo.set_sensitive(pending || !state.steps.is_empty());
            reveal(&place, pending);
            hint.set_text(if !fits {
                CANVAS_TOO_LARGE_HINT
            } else if pending {
                PENDING_HINT
            } else {
                DEFAULT_HINT
            });
            area.queue_draw();
        })
    };

    let tools = gtk::Box::new(Orientation::Horizontal, 6);
    tools.set_margin_top(6);
    tools.set_margin_bottom(6);
    tools.set_margin_start(12);
    tools.set_margin_end(12);
    let dim = gtk::Scale::with_range(
        Orientation::Horizontal,
        f64::from(MIN_SPOTLIGHT_DIM_PERCENT),
        f64::from(MAX_SPOTLIGHT_DIM_PERCENT),
        5.0,
    );
    dim.set_value(f64::from(DEFAULT_SPOTLIGHT_DIM_PERCENT));
    dim.set_digits(0);
    dim.set_draw_value(true);
    // Beside the slider, so showing it never changes the toolbar's height.
    dim.set_value_pos(gtk::PositionType::Right);
    dim.set_width_request(160);
    dim.set_tooltip_text(Some("How much the area outside the spotlights is dimmed"));
    dim.update_property(&[Property::Label("Spotlight dim strength, in percent")]);
    let dim_box = gtk::Box::new(Orientation::Horizontal, 6);
    dim_box.append(&gtk::Label::new(Some("Dim")));
    dim_box.append(&dim);
    reveal(&dim_box, false);
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
        let (state, dim_box, refresh) = (Rc::clone(&state), dim_box.clone(), Rc::clone(&refresh));
        button.connect_toggled(move |button| {
            if button.is_active() {
                {
                    let mut state = state.borrow_mut();
                    state.tool = tool;
                    state.place();
                }
                reveal(&dim_box, tool == Tool::Spotlight);
                refresh();
            }
        });
        tools.append(&button);
    }
    tools.append(&dim_box);
    tools.append(&canvas_button(&state, &refresh));
    let paste = gtk::Button::with_label("Paste image");
    paste.set_tooltip_text(Some("Paste the image on the clipboard (Ctrl+V)"));
    tools.append(&paste);
    tools.append(&place);
    tools.append(&undo);

    {
        let (state, viewport) = (Rc::clone(&state), Rc::clone(&viewport));
        area.set_draw_func(move |_, context, area_width, area_height| {
            let state = state.borrow();
            let canvas = state.canvas_rect(width, height);
            let scale = (f64::from(area_width) / f64::from(canvas.width))
                .min(f64::from(area_height) / f64::from(canvas.height))
                .min(1.0);
            let current = Viewport {
                scale,
                offset_x: (f64::from(area_width) - f64::from(canvas.width) * scale) / 2.0
                    - f64::from(canvas.x) * scale,
                offset_y: (f64::from(area_height) - f64::from(canvas.height) * scale) / 2.0
                    - f64::from(canvas.y) * scale,
            };
            viewport.set(current);
            let _ = draw(context, &surface, current, &state, canvas, thickness);
        });
    }
    {
        let (state, area) = (Rc::clone(&state), area.clone());
        dim.connect_value_changed(move |dim| {
            state.borrow_mut().plan.spotlight_dim_percent = dim.value().round() as u8;
            area.queue_draw();
        });
    }

    let drag = gtk::GestureDrag::new();
    {
        let (state, viewport, area) = (Rc::clone(&state), Rc::clone(&viewport), area.clone());
        drag.connect_drag_begin(move |_, x, y| {
            let current = viewport.get();
            let point = current.to_image(x, y);
            let radius = (HANDLE_RADIUS / current.scale).ceil() as i32;
            let mut state = state.borrow_mut();
            match state.pending.as_ref().map(|pending| pending.rect) {
                // Another tool waits until the pasted image is placed.
                Some(rect) => state.adjust = adjust_at(rect, point, radius),
                None => state.drag = Some((point, point)),
            }
            area.queue_draw();
        });
    }
    {
        let (state, viewport, area) = (Rc::clone(&state), Rc::clone(&viewport), area.clone());
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let Some((start_x, start_y)) = gesture.start_point() else {
                return;
            };
            let point = viewport
                .get()
                .to_image(start_x + offset_x, start_y + offset_y);
            let mut state = state.borrow_mut();
            let adjust = state.adjust;
            match (adjust, state.pending.as_mut()) {
                (Some(Adjust::Move { grab }), Some(pending)) => {
                    pending.rect.x = point.0 - grab.0;
                    pending.rect.y = point.1 - grab.1;
                }
                (Some(Adjust::Resize { anchor }), Some(pending)) => {
                    pending.rect =
                        resized(anchor, point, (pending.image.width, pending.image.height));
                }
                _ => {
                    if let Some((_, current)) = state.drag.as_mut() {
                        *current = point;
                    }
                }
            }
            area.queue_draw();
        });
    }
    {
        let (state, refresh) = (Rc::clone(&state), Rc::clone(&refresh));
        drag.connect_drag_end(move |_, _, _| {
            {
                let mut state = state.borrow_mut();
                if state.adjust.take().is_none() {
                    if let Some((from, to)) = state.drag.take() {
                        commit(&mut state, from, to, thickness);
                    }
                }
            }
            refresh();
        });
    }
    area.add_controller(drag);

    let paste_image: Rc<dyn Fn()> = {
        let (window, state, hint, refresh) = (
            window.clone(),
            Rc::clone(&state),
            hint.clone(),
            Rc::clone(&refresh),
        );
        Rc::new(move || {
            if state.borrow().plan.pasted.len() >= MAX_PASTED_IMAGES {
                hint.set_text(&format!(
                    "An edit holds at most {MAX_PASTED_IMAGES} pasted images."
                ));
                return;
            }
            let clipboard = window.clipboard();
            let (state, hint, refresh) = (Rc::clone(&state), hint.clone(), Rc::clone(&refresh));
            glib::spawn_future_local(async move {
                let texture = match clipboard.read_texture_future().await {
                    Ok(Some(texture)) => texture,
                    _ => {
                        hint.set_text("The clipboard has no image to paste.");
                        return;
                    }
                };
                let pasted = match texture_image(&texture) {
                    Ok(pasted) => pasted,
                    Err(message) => {
                        hint.set_text(message);
                        return;
                    }
                };
                let Some(surface) = surface_from(&pasted) else {
                    return;
                };
                {
                    let mut state = state.borrow_mut();
                    state.place();
                    let rect = initial_paste_rect(
                        state.canvas_rect(width, height),
                        (pasted.width, pasted.height),
                    );
                    state.pending = Some(PendingPaste {
                        image: pasted,
                        surface,
                        rect,
                    });
                }
                refresh();
            });
        })
    };
    {
        let paste_image = Rc::clone(&paste_image);
        paste.connect_clicked(move |_| paste_image());
    }
    {
        let (state, refresh) = (Rc::clone(&state), Rc::clone(&refresh));
        place.connect_clicked(move |_| {
            state.borrow_mut().place();
            refresh();
        });
    }
    {
        let (state, refresh) = (Rc::clone(&state), Rc::clone(&refresh));
        undo.connect_clicked(move |_| {
            state.borrow_mut().undo();
            refresh();
        });
    }
    let keys = gtk::EventControllerKey::new();
    {
        let (state, refresh) = (Rc::clone(&state), Rc::clone(&refresh));
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let pending = state.borrow().pending.is_some();
            match key {
                gdk::Key::v | gdk::Key::V
                    if modifiers.contains(gdk::ModifierType::CONTROL_MASK) =>
                {
                    paste_image();
                }
                gdk::Key::Return | gdk::Key::KP_Enter if pending => state.borrow_mut().place(),
                gdk::Key::Escape if pending => state.borrow_mut().undo(),
                _ => return glib::Propagation::Proceed,
            }
            refresh();
            glib::Propagation::Stop
        });
    }
    window.add_controller(keys);
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    {
        let (window, state) = (window.clone(), Rc::clone(&state));
        save.connect_clicked(move |_| {
            let plan = {
                let mut state = state.borrow_mut();
                // The service would refuse it after the window closed.
                if !state.canvas_fits(width, height) {
                    return;
                }
                state.place();
                state.plan.clone()
            };
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
    content.append(&hint);
    content.append(&area);
    window.set_content(Some(&content));
    window.present();
}

/// The Canvas button and its panel: space per side and the background.
/// Changes preview at once; closing the panel records one undo step.
fn canvas_button(state: &Rc<RefCell<EditorState>>, refresh: &Rc<dyn Fn()>) -> gtk::MenuButton {
    let grid = gtk::Grid::builder()
        .row_spacing(6)
        .column_spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    // Set while the panel copies the plan into its controls.
    let syncing = Rc::new(Cell::new(false));
    let mut spins = Vec::new();
    for (row, (label, side)) in SIDES.into_iter().enumerate() {
        let name = gtk::Label::new(Some(label));
        name.set_xalign(0.0);
        let spin = gtk::SpinButton::with_range(0.0, f64::from(MAX_CANVAS_PADDING), 10.0);
        spin.set_digits(0);
        spin.update_property(&[Property::Label(&format!(
            "Space added at the {}, in pixels",
            label.to_lowercase()
        ))]);
        let (state, refresh, syncing) = (Rc::clone(state), Rc::clone(refresh), Rc::clone(&syncing));
        spin.connect_value_changed(move |spin| {
            if syncing.get() {
                return;
            }
            *side(&mut state.borrow_mut().plan.canvas) = spin.value().round() as u32;
            refresh();
        });
        grid.attach(&name, 0, row as i32, 1, 1);
        grid.attach(&spin, 1, row as i32, 1, 1);
        spins.push(spin);
    }

    let colours = gtk::Box::new(Orientation::Horizontal, 0);
    colours.add_css_class("linked");
    let custom_colour = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    custom_colour.set_tooltip_text(Some("Choose a custom background colour"));
    let mut presets = Vec::new();
    let mut group: Option<gtk::ToggleButton> = None;
    let choices = BACKGROUNDS
        .iter()
        .map(|(label, colour)| (*label, Some(*colour)))
        .chain([("Custom", None)]);
    for (label, colour) in choices {
        let button = gtk::ToggleButton::with_label(label);
        match &group {
            Some(group) => button.set_group(Some(group)),
            None => group = Some(button.clone()),
        }
        let (state, refresh, syncing, custom_colour) = (
            Rc::clone(state),
            Rc::clone(refresh),
            Rc::clone(&syncing),
            custom_colour.clone(),
        );
        button.connect_toggled(move |button| {
            if syncing.get() || !button.is_active() {
                return;
            }
            state.borrow_mut().plan.canvas.background =
                colour.unwrap_or_else(|| colour_from(&custom_colour.rgba()));
            refresh();
        });
        colours.append(&button);
        presets.push(button);
    }
    {
        let (state, refresh, syncing, custom) = (
            Rc::clone(state),
            Rc::clone(refresh),
            Rc::clone(&syncing),
            presets[BACKGROUNDS.len()].clone(),
        );
        custom_colour.connect_rgba_notify(move |button| {
            if syncing.get() {
                return;
            }
            state.borrow_mut().plan.canvas.background = colour_from(&button.rgba());
            custom.set_active(true);
            refresh();
        });
    }
    let background = gtk::Box::new(Orientation::Horizontal, 6);
    background.append(&colours);
    background.append(&custom_colour);
    let background_label = gtk::Label::new(Some("Background"));
    background_label.set_xalign(0.0);
    grid.attach(&background_label, 0, SIDES.len() as i32, 1, 1);
    grid.attach(&background, 1, SIDES.len() as i32, 1, 1);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&grid));
    let opened = Rc::new(Cell::new(CanvasExpansion::default()));
    {
        let (state, opened) = (Rc::clone(state), Rc::clone(&opened));
        popover.connect_show(move |_| {
            let canvas = state.borrow().plan.canvas;
            opened.set(canvas);
            syncing.set(true);
            let mut copy = canvas;
            for (spin, (_, side)) in spins.iter().zip(SIDES) {
                spin.set_value(f64::from(*side(&mut copy)));
            }
            let preset = BACKGROUNDS
                .iter()
                .position(|(_, colour)| *colour == canvas.background);
            match preset {
                Some(index) => presets[index].set_active(true),
                None => {
                    custom_colour.set_rgba(&rgba_from(canvas.background));
                    presets[BACKGROUNDS.len()].set_active(true);
                }
            }
            syncing.set(false);
        });
    }
    {
        let (state, refresh) = (Rc::clone(state), Rc::clone(refresh));
        popover.connect_closed(move |_| {
            {
                let mut state = state.borrow_mut();
                if state.plan.canvas != opened.get() {
                    state.steps.push(Step::Canvas(opened.get()));
                }
            }
            refresh();
        });
    }
    gtk::MenuButton::builder()
        .label("Canvas")
        .tooltip_text("Add space around the screenshot")
        .popover(&popover)
        .build()
}

fn colour_from(rgba: &gdk::RGBA) -> Color {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color {
        r: channel(rgba.red()),
        g: channel(rgba.green()),
        b: channel(rgba.blue()),
        a: channel(rgba.alpha()),
    }
}

fn rgba_from(colour: Color) -> gdk::RGBA {
    let channel = |value: u8| f32::from(value) / 255.0;
    gdk::RGBA::new(
        channel(colour.r),
        channel(colour.g),
        channel(colour.b),
        channel(colour.a),
    )
}

/// Shows `widget` only when `shown`, keeping its space so the toolbar and the
/// image never move.
fn reveal(widget: &impl IsA<gtk::Widget>, shown: bool) {
    widget.set_opacity(if shown { 1.0 } else { 0.0 });
    widget.set_sensitive(shown);
    widget.set_can_target(shown);
    widget
        .upcast_ref::<gtk::Widget>()
        .update_state(&[gtk::accessible::State::Hidden(!shown)]);
}

/// The clipboard image as straight-alpha RGBA, within the paste limit.
fn texture_image(texture: &gdk::Texture) -> Result<RgbaImage, &'static str> {
    let width = u32::try_from(texture.width()).unwrap_or(0);
    let height = u32::try_from(texture.height()).unwrap_or(0);
    if u64::from(width) * u64::from(height) > MAX_CANVAS_PIXELS {
        return Err("The image on the clipboard is too large to paste.");
    }
    let unreadable = "The image on the clipboard could not be read.";
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in bytes.chunks(stride.max(1)).take(height as usize) {
        pixels.extend_from_slice(line.get(..row).ok_or(unreadable)?);
    }
    RgbaImage::new(width, height, pixels).map_err(|_| unreadable)
}

/// Corners of `rect`, clockwise from the top-left.
fn corners(rect: Rect) -> [(i32, i32); 4] {
    let right = rect.x.saturating_add(rect.width as i32);
    let bottom = rect.y.saturating_add(rect.height as i32);
    [
        (rect.x, rect.y),
        (right, rect.y),
        (right, bottom),
        (rect.x, bottom),
    ]
}

/// What a drag starting at `point` does to a pasted image at `rect`: near a
/// corner it resizes from the opposite one, inside it moves, elsewhere nothing.
fn adjust_at(rect: Rect, point: (i32, i32), radius: i32) -> Option<Adjust> {
    let corners = corners(rect);
    let near = |corner: (i32, i32)| {
        (corner.0 - point.0).abs() <= radius && (corner.1 - point.1).abs() <= radius
    };
    if let Some(index) = corners.iter().position(|corner| near(*corner)) {
        return Some(Adjust::Resize {
            anchor: corners[(index + 2) % 4],
        });
    }
    let inside =
        (rect.x..corners[2].0).contains(&point.0) && (rect.y..corners[2].1).contains(&point.1);
    inside.then_some(Adjust::Move {
        grab: (point.0 - rect.x, point.1 - rect.y),
    })
}

/// The rectangle from `anchor` toward `pointer` with the proportions of an
/// image of `size`, at least [`MIN_PASTE_SIDE`] on each side.
fn resized(anchor: (i32, i32), pointer: (i32, i32), size: (u32, u32)) -> Rect {
    let aspect = f64::from(size.0) / f64::from(size.1);
    let dx = f64::from(pointer.0) - f64::from(anchor.0);
    let dy = f64::from(pointer.1) - f64::from(anchor.1);
    let width = dx
        .abs()
        .max(dy.abs() * aspect)
        .max(MIN_PASTE_SIDE)
        .max(MIN_PASTE_SIDE * aspect);
    let height = width / aspect;
    let x = if dx < 0.0 {
        f64::from(anchor.0) - width
    } else {
        f64::from(anchor.0)
    };
    let y = if dy < 0.0 {
        f64::from(anchor.1) - height
    } else {
        f64::from(anchor.1)
    };
    Rect {
        x: x.round() as i32,
        y: y.round() as i32,
        width: width.round() as u32,
        height: height.round().max(1.0) as u32,
    }
}

/// Where a pasted image starts: centred on the canvas, shrunk to fit 80% of
/// it, never enlarged.
fn initial_paste_rect(canvas: Rect, size: (u32, u32)) -> Rect {
    let scale = (0.8 * f64::from(canvas.width) / f64::from(size.0))
        .min(0.8 * f64::from(canvas.height) / f64::from(size.1))
        .min(1.0);
    let width = (f64::from(size.0) * scale).round().max(1.0);
    let height = (f64::from(size.1) * scale).round().max(1.0);
    Rect {
        x: (f64::from(canvas.x) + (f64::from(canvas.width) - width) / 2.0).round() as i32,
        y: (f64::from(canvas.y) + (f64::from(canvas.height) - height) / 2.0).round() as i32,
        width: width as u32,
        height: height as u32,
    }
}

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
        Tool::Spotlight => EditOperation::Spotlight(rect),
    };
    state.plan.operations.push(operation);
    state.steps.push(Step::Operation);
}

pub(crate) fn surface_from(image: &RgbaImage) -> Option<cairo::ImageSurface> {
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
        // Spotlights draw as the dim layer and pastes with the picture,
        // both beneath every annotation.
        EditOperation::Spotlight(_) | EditOperation::Paste { .. } => Ok(()),
    }
}

/// Draws an image surface scaled into `rect`.
fn draw_surface(
    context: &cairo::Context,
    surface: &cairo::ImageSurface,
    rect: Rect,
) -> Result<(), cairo::Error> {
    if rect.width == 0 || rect.height == 0 {
        return Ok(());
    }
    context.save()?;
    context.translate(f64::from(rect.x), f64::from(rect.y));
    context.scale(
        f64::from(rect.width) / f64::from(surface.width()),
        f64::from(rect.height) / f64::from(surface.height()),
    );
    context.set_source_surface(surface, 0.0, 0.0)?;
    context.paint()?;
    context.restore()
}

/// Fills the canvas with its background; see-through colours sit on a
/// checkerboard of fixed screen size.
fn draw_background(
    context: &cairo::Context,
    canvas: Rect,
    background: Color,
    scale: f64,
) -> Result<(), cairo::Error> {
    if background.a < 255 {
        let tile = cairo::ImageSurface::create(cairo::Format::Rgb24, 16, 16)?;
        {
            let tiles = cairo::Context::new(&tile)?;
            tiles.set_source_rgb(0.8, 0.8, 0.8);
            tiles.paint()?;
            tiles.set_source_rgb(0.6, 0.6, 0.6);
            tiles.rectangle(0.0, 0.0, 8.0, 8.0);
            tiles.rectangle(8.0, 8.0, 8.0, 8.0);
            tiles.fill()?;
        }
        let pattern = cairo::SurfacePattern::create(&tile);
        pattern.set_extend(cairo::Extend::Repeat);
        pattern.set_matrix(cairo::Matrix::new(scale, 0.0, 0.0, scale, 0.0, 0.0));
        context.set_source(&pattern)?;
        rectangle(context, canvas);
        context.fill()?;
    }
    set_color(context, background);
    rectangle(context, canvas);
    context.fill()
}

/// Dims everything outside the spotlights, as `apply` does to the saved copy.
fn draw_spotlight_dim(
    context: &cairo::Context,
    spotlights: &[Rect],
    dim_percent: u8,
    canvas: Rect,
) -> Result<(), cairo::Error> {
    let dim = f64::from(dim_percent.clamp(MIN_SPOTLIGHT_DIM_PERCENT, MAX_SPOTLIGHT_DIM_PERCENT));
    context.push_group();
    context.set_source_rgba(0.0, 0.0, 0.0, dim / 100.0);
    rectangle(context, canvas);
    context.fill()?;
    context.set_operator(cairo::Operator::Clear);
    for rect in spotlights {
        rectangle(context, *rect);
    }
    context.fill()?;
    context.set_operator(cairo::Operator::Over);
    context.pop_group_to_source()?;
    context.paint()
}

/// Outlines a pending paste with corner handles, at fixed screen size.
fn draw_handles(context: &cairo::Context, rect: Rect, scale: f64) -> Result<(), cairo::Error> {
    context.set_line_width(2.0 / scale);
    context.set_source_rgba(0.21, 0.52, 0.89, 1.0);
    rectangle(context, rect);
    context.stroke()?;
    let size = 8.0 / scale;
    for (x, y) in corners(rect) {
        context.rectangle(
            f64::from(x) - size / 2.0,
            f64::from(y) - size / 2.0,
            size,
            size,
        );
    }
    context.set_source_rgb(1.0, 1.0, 1.0);
    context.fill_preserve()?;
    context.set_source_rgba(0.21, 0.52, 0.89, 1.0);
    context.stroke()
}

fn draw(
    context: &cairo::Context,
    surface: &cairo::ImageSurface,
    viewport: Viewport,
    state: &EditorState,
    canvas: Rect,
    thickness: u32,
) -> Result<(), cairo::Error> {
    context.translate(viewport.offset_x, viewport.offset_y);
    context.scale(viewport.scale, viewport.scale);
    draw_background(
        context,
        canvas,
        state.plan.canvas.background,
        viewport.scale,
    )?;
    context.set_source_surface(surface, 0.0, 0.0)?;
    context.paint()?;
    for operation in &state.plan.operations {
        if let EditOperation::Paste { image, rect } = *operation {
            if let Some(pasted) = state.pasted_surfaces.get(image) {
                draw_surface(context, pasted, rect)?;
            }
        }
    }
    // Drawn with the picture, so placing it changes nothing on screen.
    if let Some(pending) = &state.pending {
        draw_surface(context, &pending.surface, pending.rect)?;
    }
    let preview = state.drag.map(|(from, to)| {
        let mut preview = EditorState::new(state.tool);
        commit(&mut preview, from, to, thickness);
        preview.plan
    });
    let dragged = preview
        .as_ref()
        .and_then(|plan| plan.operations.first().copied());
    let spotlights: Vec<Rect> = state
        .plan
        .operations
        .iter()
        .chain(dragged.as_ref())
        .filter_map(|operation| match operation {
            EditOperation::Spotlight(rect) => Some(*rect),
            _ => None,
        })
        .collect();
    if !spotlights.is_empty() {
        draw_spotlight_dim(
            context,
            &spotlights,
            state.plan.spotlight_dim_percent,
            canvas,
        )?;
    }
    for operation in state.plan.operations.iter().chain(dragged.as_ref()) {
        draw_operation(context, operation)?;
    }
    let crop = preview.and_then(|plan| plan.crop).or(state.plan.crop);
    if let Some(crop) = crop {
        context.set_fill_rule(cairo::FillRule::EvenOdd);
        rectangle(context, canvas);
        rectangle(context, crop);
        context.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        context.fill()?;
        context.set_fill_rule(cairo::FillRule::Winding);
        context.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        context.set_line_width(2.0 / viewport.scale);
        rectangle(context, crop);
        context.stroke()?;
    }
    if let Some(pending) = &state.pending {
        draw_handles(context, pending.rect, viewport.scale)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn drags_become_plan_entries_in_any_direction_and_empty_drags_are_dropped() {
        let mut editor = EditorState::new(Tool::Redact);
        commit(&mut editor, (40, 30), (10, 5), 3);
        commit(&mut editor, (7, 7), (7, 20), 3);
        assert_eq!(
            editor.plan.operations,
            vec![EditOperation::Redact(rect(10, 5, 30, 25))]
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
            Some(rect(0, 0, 5, 5)),
            "undo restores the earlier crop"
        );
    }

    #[test]
    fn drags_on_a_pasted_image_resize_from_corners_and_move_from_inside() {
        let image = rect(10, 20, 40, 30);
        assert_eq!(
            adjust_at(image, (11, 21), 3),
            Some(Adjust::Resize { anchor: (50, 50) }),
            "the top-left corner resizes from the bottom-right"
        );
        assert_eq!(
            adjust_at(image, (49, 52), 3),
            Some(Adjust::Resize { anchor: (10, 20) })
        );
        assert_eq!(
            adjust_at(image, (30, 30), 3),
            Some(Adjust::Move { grab: (20, 10) })
        );
        assert_eq!(adjust_at(image, (70, 70), 3), None, "outside does nothing");
    }

    #[test]
    fn resizing_keeps_the_proportions_in_every_direction() {
        // A 4:3 image resized from a top-left anchor at (100, 100).
        assert_eq!(
            resized((100, 100), (140, 101), (4, 3)),
            rect(100, 100, 40, 30)
        );
        assert_eq!(
            resized((100, 100), (99, 70), (4, 3)),
            rect(60, 70, 40, 30),
            "the taller drag wins, up and to the left"
        );
        let tiny = resized((100, 100), (101, 101), (4, 3));
        assert_eq!(
            (tiny.width, tiny.height),
            (11, 8),
            "at least eight pixels each way"
        );
    }

    #[test]
    fn a_pasted_image_starts_centred_and_shrinks_only_to_fit() {
        let canvas = rect(-50, -10, 200, 100);
        assert_eq!(
            initial_paste_rect(canvas, (20, 10)),
            rect(40, 35, 20, 10),
            "a small image keeps its size"
        );
        assert_eq!(
            initial_paste_rect(canvas, (400, 100)),
            rect(-30, 20, 160, 40),
            "a wide image shrinks to 80% of the canvas width"
        );
    }

    #[test]
    fn saving_waits_for_a_canvas_within_the_service_limits() {
        let mut editor = EditorState::new(Tool::Box);
        editor.plan.canvas = CanvasExpansion {
            top: 200,
            left: 100,
            ..CanvasExpansion::default()
        };
        assert!(editor.canvas_fits(1300, 1000));

        editor.plan.canvas = CanvasExpansion {
            top: MAX_CANVAS_PADDING,
            right: MAX_CANVAS_PADDING,
            bottom: MAX_CANVAS_PADDING,
            left: MAX_CANVAS_PADDING,
            ..CanvasExpansion::default()
        };
        assert!(
            !editor.canvas_fits(1300, 1000),
            "9492 × 9192 is over 64 megapixels"
        );

        editor.plan.canvas = CanvasExpansion {
            right: 1000,
            ..CanvasExpansion::default()
        };
        assert!(
            !editor.canvas_fits(16_000, 1),
            "17,000 pixels wide is over the per-side limit"
        );
    }

    #[test]
    fn placing_and_undoing_pastes_and_canvas_changes() {
        let pixel = RgbaImage::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        let surface = surface_from(&pixel).unwrap();
        let mut editor = EditorState::new(Tool::Box);
        editor.pending = Some(PendingPaste {
            image: pixel.clone(),
            surface: surface.clone(),
            rect: rect(1, 1, 4, 4),
        });
        assert!(editor.edited(), "a pending paste counts as an edit");
        editor.undo();
        assert!(
            editor.pending.is_none(),
            "Undo cancels a pending paste first"
        );
        assert!(editor.plan.operations.is_empty());

        editor.pending = Some(PendingPaste {
            image: pixel,
            surface,
            rect: rect(1, 1, 4, 4),
        });
        editor.place();
        assert_eq!(
            editor.plan.operations,
            vec![EditOperation::Paste {
                image: 0,
                rect: rect(1, 1, 4, 4)
            }]
        );
        assert_eq!(editor.plan.pasted.len(), 1);

        let previous = editor.plan.canvas;
        editor.plan.canvas.left = 40;
        editor.steps.push(Step::Canvas(previous));
        editor.undo();
        assert!(editor.plan.canvas.is_empty(), "Undo restores the canvas");
        editor.undo();
        assert!(editor.plan.operations.is_empty());
        assert!(
            editor.plan.pasted.is_empty() && editor.pasted_surfaces.is_empty(),
            "Undo drops the pasted image with its edit"
        );
    }
}
