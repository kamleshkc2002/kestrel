//! In-memory RGBA images: PNG decode/encode and edit plans (redact, shapes,
//! spotlight, crop).

use std::fmt;

use kestrel_platform::capture::MAX_CAPTURE_DIMENSION;

/// Maximum operations in one edit plan.
const MAX_EDIT_OPERATIONS: usize = 256;

/// Spotlight dim strength: the share of brightness taken from areas outside
/// every spotlight, in percent.
pub const DEFAULT_SPOTLIGHT_DIM_PERCENT: u8 = 60;
pub const MIN_SPOTLIGHT_DIM_PERCENT: u8 = 10;
pub const MAX_SPOTLIGHT_DIM_PERCENT: u8 = 90;

/// Non-premultiplied 8-bit RGBA pixels, row-major, within capture bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// Validates dimensions against the capture bounds and the pixel buffer length.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        if !valid_dimensions(width, height) {
            return Err(ImageError::InvalidDimensions);
        }
        let expected_len = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|count| count.checked_mul(4));
        if expected_len != Some(pixels.len()) {
            return Err(ImageError::InvalidLength);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

/// Image decode, encode, or edit failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    Png(String),
    InvalidDimensions,
    InvalidLength,
    EmptyCrop,
    TooManyOperations,
    /// The expanded canvas exceeds [`MAX_CANVAS_PADDING`] or [`MAX_CANVAS_PIXELS`].
    CanvasTooLarge,
    TooManyPastes,
    /// A paste refers to an image the plan does not hold.
    UnknownPaste,
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Png(message) => write!(formatter, "PNG image could not be processed: {message}"),
            Self::InvalidDimensions => {
                formatter.write_str("image dimensions are invalid or exceed the limit")
            }
            Self::InvalidLength => formatter.write_str("RGBA pixel data has an invalid length"),
            Self::EmptyCrop => formatter.write_str("crop does not intersect the image"),
            Self::TooManyOperations => {
                formatter.write_str("edit plan contains too many operations")
            }
            Self::CanvasTooLarge => write!(
                formatter,
                "the canvas is too large: at most {MAX_CANVAS_PADDING} pixels per side and \
                 {} megapixels in total",
                MAX_CANVAS_PIXELS / (1024 * 1024)
            ),
            Self::TooManyPastes => write!(
                formatter,
                "an edit holds at most {MAX_PASTED_IMAGES} pasted images"
            ),
            Self::UnknownPaste => formatter.write_str("a pasted image is missing from the edit"),
        }
    }
}

impl std::error::Error for ImageError {}

/// Decodes any PNG colour type to RGBA, rejecting dimensions outside the capture bounds.
pub fn decode_png(bytes: &[u8]) -> Result<RgbaImage, ImageError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(png_error)?;
    let info = reader.info();
    // Checked before allocating the frame buffer.
    if !valid_dimensions(info.width, info.height) {
        return Err(ImageError::InvalidDimensions);
    }
    let size = reader
        .output_buffer_size()
        .ok_or(ImageError::InvalidDimensions)?;
    let mut frame = vec![0; size];
    let output = reader.next_frame(&mut frame).map_err(png_error)?;
    let raw = &frame[..output.buffer_size()];
    let pixel_count = usize::try_from(output.width)
        .ok()
        .zip(usize::try_from(output.height).ok())
        .and_then(|(width, height)| width.checked_mul(height))
        .ok_or(ImageError::InvalidLength)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(
            pixel_count
                .checked_mul(4)
                .ok_or(ImageError::InvalidLength)?,
        )
        .map_err(|_| ImageError::InvalidLength)?;
    match output.color_type {
        png::ColorType::Rgba => pixels.extend_from_slice(raw),
        png::ColorType::Rgb => {
            for rgb in raw.as_chunks::<3>().0 {
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for &value in raw {
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for gray_alpha in raw.as_chunks::<2>().0 {
                let (value, alpha) = (gray_alpha[0], gray_alpha[1]);
                pixels.extend_from_slice(&[value, value, value, alpha]);
            }
        }
        png::ColorType::Indexed => {
            return Err(ImageError::Png("indexed PNG expansion failed".into()));
        }
    }
    RgbaImage::new(output.width, output.height, pixels)
}

/// Encodes 8-bit RGBA PNG with no ancillary metadata chunks.
pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, ImageError> {
    let valid = RgbaImage::new(image.width, image.height, image.pixels.clone())?;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, valid.width, valid.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer.write_image_data(&valid.pixels).map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(bytes)
}

/// Axis-aligned rectangle in image pixels; may extend past the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Rectangle spanning two corner points given in any order.
    pub fn normalized(from: (i32, i32), to: (i32, i32)) -> Self {
        let x = from.0.min(to.0);
        let y = from.1.min(to.1);
        let span = |a: i32, b: i32, low: i32| {
            (i64::from(a.max(b)) - i64::from(low)).min(i64::from(u32::MAX)) as u32
        };
        Self {
            x,
            y,
            width: span(from.0, to.0, x),
            height: span(from.1, to.1, y),
        }
    }

    fn clip(self, width: u32, height: u32) -> Option<Bounds> {
        let left = i64::from(self.x).max(0);
        let top = i64::from(self.y).max(0);
        let right = (i64::from(self.x) + i64::from(self.width)).min(i64::from(width));
        let bottom = (i64::from(self.y) + i64::from(self.height)).min(i64::from(height));
        (right > left && bottom > top).then_some(Bounds {
            left: left as u32,
            top: top as u32,
            right: right as u32,
            bottom: bottom as u32,
        })
    }

    /// The same rectangle moved by `(dx, dy)`, saturating at the coordinate range.
    pub fn shifted(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            ..self
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

/// Straight-alpha RGBA drawing colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const RED: Self = Self {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };

    pub const YELLOW: Self = Self {
        r: 255,
        g: 255,
        b: 0,
        a: 160,
    };

    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOperation {
    /// An opaque, uniform fill: whatever `color`'s alpha, nothing of the
    /// pixels underneath survives.
    Redact {
        rect: Rect,
        color: Color,
    },
    Rectangle {
        rect: Rect,
        color: Color,
        thickness: u32,
    },
    Highlight {
        rect: Rect,
        color: Color,
    },
    Arrow {
        from: (i32, i32),
        to: (i32, i32),
        color: Color,
        thickness: u32,
    },
    /// A clear area in the dim that covers the rest of the image.
    Spotlight(Rect),
    /// The plan's pasted image at `image`, scaled into `rect`.
    Paste {
        image: usize,
        rect: Rect,
    },
}

/// Most space added on one side of the canvas, in pixels.
pub const MAX_CANVAS_PADDING: u32 = 4096;
/// Most pixels in an expanded canvas or a pasted image (256 MiB of RGBA).
pub const MAX_CANVAS_PIXELS: u64 = 64 * 1024 * 1024;
/// Most pasted images in one plan.
pub const MAX_PASTED_IMAGES: usize = 16;

/// Space added around the source image, filled with `background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanvasExpansion {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
    pub background: Color,
}

impl Default for CanvasExpansion {
    fn default() -> Self {
        Self {
            top: 0,
            right: 0,
            bottom: 0,
            left: 0,
            background: Color::WHITE,
        }
    }
}

impl CanvasExpansion {
    pub fn is_empty(&self) -> bool {
        self.top == 0 && self.right == 0 && self.bottom == 0 && self.left == 0
    }
}

/// An edit in source coordinates: space added on the left or top has
/// negative coordinates, so resizing the canvas never moves an edit. Saving
/// expands the canvas, draws pasted images, dims outside the spotlights,
/// applies the annotations in order, then crops; so pasted images are dimmed
/// with the picture and annotations never are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPlan {
    pub crop: Option<Rect>,
    pub operations: Vec<EditOperation>,
    /// Dim strength outside the spotlights, clamped to
    /// [`MIN_SPOTLIGHT_DIM_PERCENT`]..=[`MAX_SPOTLIGHT_DIM_PERCENT`].
    pub spotlight_dim_percent: u8,
    pub canvas: CanvasExpansion,
    /// Images that [`EditOperation::Paste`] refers to by index.
    pub pasted: Vec<RgbaImage>,
}

impl Default for EditPlan {
    fn default() -> Self {
        Self {
            crop: None,
            operations: Vec::new(),
            spotlight_dim_percent: DEFAULT_SPOTLIGHT_DIM_PERCENT,
            canvas: CanvasExpansion::default(),
            pasted: Vec::new(),
        }
    }
}

/// Applies `plan` to a copy of `source`.
pub fn apply(source: &RgbaImage, plan: &EditPlan) -> Result<RgbaImage, ImageError> {
    if plan.operations.len() > MAX_EDIT_OPERATIONS {
        return Err(ImageError::TooManyOperations);
    }
    if plan.pasted.len() > MAX_PASTED_IMAGES {
        return Err(ImageError::TooManyPastes);
    }
    let mut image = expand_canvas(source, plan.canvas)?;
    // Padding is at most MAX_CANVAS_PADDING, so it fits an i32.
    let (dx, dy) = (plan.canvas.left as i32, plan.canvas.top as i32);
    let point = |(x, y): (i32, i32)| (x.saturating_add(dx), y.saturating_add(dy));
    for operation in &plan.operations {
        if let EditOperation::Paste { image: index, rect } = *operation {
            let pasted = plan.pasted.get(index).ok_or(ImageError::UnknownPaste)?;
            draw_image(&mut image, pasted, rect.shifted(dx, dy));
        }
    }
    dim_outside_spotlights(&mut image, plan, dx, dy);
    for operation in &plan.operations {
        match *operation {
            EditOperation::Redact { rect, color } => {
                let fill = [color.r, color.g, color.b, 255];
                for_each_pixel(&mut image, rect.shifted(dx, dy), |pixel, _| {
                    pixel.copy_from_slice(&fill)
                });
            }
            EditOperation::Highlight { rect, color } => {
                for_each_pixel(&mut image, rect.shifted(dx, dy), |pixel, _| {
                    blend(pixel, color)
                });
            }
            EditOperation::Rectangle {
                rect,
                color,
                thickness,
            } => {
                let thickness = i64::from(thickness);
                for_each_pixel(&mut image, rect.shifted(dx, dy), |pixel, (x, y, bounds)| {
                    let on_edge = i64::from(x - bounds.left) < thickness
                        || i64::from(bounds.right - 1 - x) < thickness
                        || i64::from(y - bounds.top) < thickness
                        || i64::from(bounds.bottom - 1 - y) < thickness;
                    if on_edge {
                        blend(pixel, color);
                    }
                });
            }
            EditOperation::Arrow {
                from,
                to,
                color,
                thickness,
            } => draw_arrow(&mut image, point(from), point(to), color, thickness),
            EditOperation::Spotlight(_) | EditOperation::Paste { .. } => {}
        }
    }
    match plan.crop {
        Some(crop) => crop_image(&image, crop.shifted(dx, dy)),
        None => Ok(image),
    }
}

/// The source on a canvas grown by `canvas`, or a copy when nothing is added.
fn expand_canvas(source: &RgbaImage, canvas: CanvasExpansion) -> Result<RgbaImage, ImageError> {
    if canvas.is_empty() {
        return RgbaImage::new(source.width, source.height, source.pixels.clone());
    }
    let sides = [canvas.top, canvas.right, canvas.bottom, canvas.left];
    if sides.iter().any(|side| *side > MAX_CANVAS_PADDING) {
        return Err(ImageError::CanvasTooLarge);
    }
    let width = source.width + canvas.left + canvas.right;
    let height = source.height + canvas.top + canvas.bottom;
    if u64::from(width) * u64::from(height) > MAX_CANVAS_PIXELS || !valid_dimensions(width, height)
    {
        return Err(ImageError::CanvasTooLarge);
    }
    let fill = canvas.background;
    let mut pixels = [fill.r, fill.g, fill.b, fill.a].repeat(width as usize * height as usize);
    let row = source.width as usize * 4;
    for y in 0..source.height {
        let from = offset(source.width, 0, y);
        let to = offset(width, canvas.left, y + canvas.top);
        pixels[to..to + row].copy_from_slice(&source.pixels[from..from + row]);
    }
    RgbaImage::new(width, height, pixels)
}

/// Draws `pasted` scaled into `rect` over what is there: bilinear when
/// enlarging, a box average when shrinking so detail does not alias.
fn draw_image(image: &mut RgbaImage, pasted: &RgbaImage, rect: Rect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let scale_x = f64::from(pasted.width) / f64::from(rect.width);
    let scale_y = f64::from(pasted.height) / f64::from(rect.height);
    let shrinking = scale_x > 1.0 || scale_y > 1.0;
    for_each_pixel(image, rect, |pixel, (x, y, _)| {
        let local_x = (i64::from(x) - i64::from(rect.x)) as f64;
        let local_y = (i64::from(y) - i64::from(rect.y)) as f64;
        let color = if shrinking {
            average(
                pasted,
                (local_x * scale_x, (local_x + 1.0) * scale_x),
                (local_y * scale_y, (local_y + 1.0) * scale_y),
            )
        } else {
            bilinear(
                pasted,
                (local_x + 0.5) * scale_x - 0.5,
                (local_y + 0.5) * scale_y - 0.5,
            )
        };
        blend(pixel, color);
    });
}

/// Alpha-weighted mean colour of the pixels covering `x` and `y` ranges.
fn average(image: &RgbaImage, x: (f64, f64), y: (f64, f64)) -> Color {
    let span = |(start, end): (f64, f64), limit: u32| {
        let first = (start.floor() as u32).min(limit - 1);
        let last = (end.ceil() as u32).clamp(first + 1, limit);
        first..last
    };
    let mut samples = Vec::new();
    for row in span(y, image.height) {
        for column in span(x, image.width) {
            samples.push((column, row, 1.0));
        }
    }
    mix(image, &samples)
}

fn bilinear(image: &RgbaImage, x: f64, y: f64) -> Color {
    let x = x.clamp(0.0, f64::from(image.width - 1));
    let y = y.clamp(0.0, f64::from(image.height - 1));
    let (left, top) = (x.floor() as u32, y.floor() as u32);
    let (right, bottom) = (
        (left + 1).min(image.width - 1),
        (top + 1).min(image.height - 1),
    );
    let (fx, fy) = (x - f64::from(left), y - f64::from(top));
    mix(
        image,
        &[
            (left, top, (1.0 - fx) * (1.0 - fy)),
            (right, top, fx * (1.0 - fy)),
            (left, bottom, (1.0 - fx) * fy),
            (right, bottom, fx * fy),
        ],
    )
}

/// Weighted mix of pixels in premultiplied space, so transparent pixels add no colour.
fn mix(image: &RgbaImage, samples: &[(u32, u32, f64)]) -> Color {
    let (mut red, mut green, mut blue, mut alpha, mut total) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y, weight) in samples {
        let index = offset(image.width, x, y);
        let pixel = &image.pixels[index..index + 4];
        let opacity = f64::from(pixel[3]) / 255.0 * weight;
        red += f64::from(pixel[0]) * opacity;
        green += f64::from(pixel[1]) * opacity;
        blue += f64::from(pixel[2]) * opacity;
        alpha += opacity;
        total += weight;
    }
    if alpha <= 0.0 || total <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |value: f64| (value / alpha).round().clamp(0.0, 255.0) as u8;
    Color {
        r: channel(red),
        g: channel(green),
        b: channel(blue),
        a: (alpha / total * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

/// Darkens every pixel outside the union of the plan's spotlights, once.
fn dim_outside_spotlights(image: &mut RgbaImage, plan: &EditPlan, dx: i32, dy: i32) {
    let mut spotlights = plan
        .operations
        .iter()
        .filter_map(|operation| match operation {
            EditOperation::Spotlight(rect) => {
                Some(rect.shifted(dx, dy).clip(image.width, image.height))
            }
            _ => None,
        })
        .peekable();
    if spotlights.peek().is_none() {
        return;
    }
    let clear: Vec<Bounds> = spotlights.flatten().collect();
    let kept = u16::from(
        100 - plan
            .spotlight_dim_percent
            .clamp(MIN_SPOTLIGHT_DIM_PERCENT, MAX_SPOTLIGHT_DIM_PERCENT),
    );
    for y in 0..image.height {
        for x in 0..image.width {
            let lit = clear.iter().any(|bounds| {
                (bounds.left..bounds.right).contains(&x) && (bounds.top..bounds.bottom).contains(&y)
            });
            if !lit {
                let index = offset(image.width, x, y);
                for channel in &mut image.pixels[index..index + 3] {
                    *channel = (u16::from(*channel) * kept / 100) as u8;
                }
            }
        }
    }
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    let valid = |side: u32| (1..=MAX_CAPTURE_DIMENSION).contains(&side);
    valid(width) && valid(height)
}

fn png_error(error: impl fmt::Display) -> ImageError {
    ImageError::Png(error.to_string())
}

fn offset(width: u32, x: u32, y: u32) -> usize {
    (y as usize * width as usize + x as usize) * 4
}

fn for_each_pixel(
    image: &mut RgbaImage,
    rect: Rect,
    mut visit: impl FnMut(&mut [u8], (u32, u32, Bounds)),
) {
    let Some(bounds) = rect.clip(image.width, image.height) else {
        return;
    };
    for y in bounds.top..bounds.bottom {
        for x in bounds.left..bounds.right {
            let index = offset(image.width, x, y);
            visit(&mut image.pixels[index..index + 4], (x, y, bounds));
        }
    }
}

/// Source-over blend onto one RGBA pixel.
fn blend(pixel: &mut [u8], color: Color) {
    let alpha = u16::from(color.a);
    let inverse = 255 - alpha;
    let mix = |source: u8, target: u8| {
        ((u16::from(source) * alpha + u16::from(target) * inverse) / 255) as u8
    };
    pixel[0] = mix(color.r, pixel[0]);
    pixel[1] = mix(color.g, pixel[1]);
    pixel[2] = mix(color.b, pixel[2]);
    pixel[3] = (alpha + u16::from(pixel[3]) * inverse / 255).min(255) as u8;
}

/// Copies the part of `image` inside `crop`.
pub fn crop_image(image: &RgbaImage, crop: Rect) -> Result<RgbaImage, ImageError> {
    let bounds = crop
        .clip(image.width, image.height)
        .ok_or(ImageError::EmptyCrop)?;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in bounds.top..bounds.bottom {
        let start = offset(image.width, bounds.left, y);
        let end = offset(image.width, bounds.right, y);
        pixels.extend_from_slice(&image.pixels[start..end]);
    }
    RgbaImage::new(width, height, pixels)
}

/// The average colour of the one-pixel ring just outside `rect`, within the
/// image: an Erase fill that blends into a plain background. Black when no
/// ring pixel is inside the image.
pub fn border_colour(image: &RgbaImage, rect: Rect) -> Color {
    let ring = Rect {
        x: rect.x.saturating_sub(1),
        y: rect.y.saturating_sub(1),
        width: rect.width.saturating_add(2),
        height: rect.height.saturating_add(2),
    };
    let Some(outer) = ring.clip(image.width, image.height) else {
        return Color::BLACK;
    };
    let inner = rect.clip(image.width, image.height);
    let mut sums = [0u64; 3];
    let mut count = 0u64;
    for y in outer.top..outer.bottom {
        for x in outer.left..outer.right {
            let inside = inner.is_some_and(|inner| {
                (inner.left..inner.right).contains(&x) && (inner.top..inner.bottom).contains(&y)
            });
            if inside {
                continue;
            }
            let index = offset(image.width, x, y);
            for (sum, value) in sums.iter_mut().zip(&image.pixels[index..index + 3]) {
                *sum += u64::from(*value);
            }
            count += 1;
        }
    }
    if count == 0 {
        return Color::BLACK;
    }
    let average = |sum: u64| (sum / count) as u8;
    Color {
        r: average(sums[0]),
        g: average(sums[1]),
        b: average(sums[2]),
        a: 255,
    }
}

/// Arrow with a filled head.
fn draw_arrow(
    image: &mut RgbaImage,
    from: (i32, i32),
    to: (i32, i32),
    color: Color,
    thickness: u32,
) {
    draw_segment(image, from, to, color, thickness);
    let dx = f64::from(to.0) - f64::from(from.0);
    let dy = f64::from(to.1) - f64::from(from.1);
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 0.0 {
        return;
    }
    let head = (f64::from(thickness.max(2)) * 3.0).max(8.0).min(length);
    let base_x = f64::from(to.0) - dx / length * head;
    let base_y = f64::from(to.1) - dy / length * head;
    let wing = head * 0.55;
    let left_wing = (
        (base_x + dy / length * wing).round() as i32,
        (base_y - dx / length * wing).round() as i32,
    );
    let right_wing = (
        (base_x - dy / length * wing).round() as i32,
        (base_y + dx / length * wing).round() as i32,
    );
    let head_thickness = thickness.max(1);
    draw_segment(image, to, left_wing, color, head_thickness);
    draw_segment(image, to, right_wing, color, head_thickness);
    // Fan of strokes fills the head triangle.
    let lerp = |start: i32, end: i32, ratio: f64| {
        (f64::from(start) + (f64::from(end) - f64::from(start)) * ratio).round() as i32
    };
    for step in 0..=16 {
        let ratio = f64::from(step) / 16.0;
        let edge = (
            lerp(left_wing.0, right_wing.0, ratio),
            lerp(left_wing.1, right_wing.1, ratio),
        );
        draw_segment(image, to, edge, color, head_thickness);
    }
}

/// Round-capped line clipped to the image.
fn draw_segment(
    image: &mut RgbaImage,
    from: (i32, i32),
    to: (i32, i32),
    color: Color,
    thickness: u32,
) {
    let thickness = thickness.min(i32::MAX as u32);
    let radius = (f64::from(thickness.max(1)) / 2.0).max(0.5);
    let margin = thickness.saturating_add(2).min(i32::MAX as u32) as i32;
    let pixel_span = |a: i32, b: i32, last: i32| {
        let low = a.min(b).saturating_sub(margin).max(0).min(last) as u32;
        let high = a.max(b).saturating_add(margin).max(0).min(last) as u32;
        (low, high)
    };
    let (min_x, max_x) = pixel_span(from.0, to.0, image.width as i32 - 1);
    let (min_y, max_y) = pixel_span(from.1, to.1, image.height as i32 - 1);
    let vector_x = f64::from(to.0) - f64::from(from.0);
    let vector_y = f64::from(to.1) - f64::from(from.1);
    let length_squared = vector_x * vector_x + vector_y * vector_y;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let center_x = f64::from(x) + 0.5;
            let center_y = f64::from(y) + 0.5;
            let along = if length_squared == 0.0 {
                0.0
            } else {
                ((center_x - f64::from(from.0)) * vector_x
                    + (center_y - f64::from(from.1)) * vector_y)
                    / length_squared
            }
            .clamp(0.0, 1.0);
            let distance_x = center_x - (f64::from(from.0) + along * vector_x);
            let distance_y = center_y - (f64::from(from.1) + along * vector_y);
            if distance_x * distance_x + distance_y * distance_y <= radius * radius {
                let index = offset(image.width, x, y);
                blend(&mut image.pixels[index..index + 4], color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// What a black-box redaction writes.
    const REDACTED_PIXEL: [u8; 4] = [0, 0, 0, 255];

    /// Every pixel distinct and none black: `[x, y, 0x80, 255]`.
    fn unique_image(width: u32, height: u32) -> RgbaImage {
        let pixels = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, 0x80, 255]))
            .collect();
        RgbaImage::new(width, height, pixels).expect("valid test image")
    }

    fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        let index = offset(image.width, x, y);
        image.pixels[index..index + 4]
            .try_into()
            .expect("four channels")
    }

    fn redact(rect: Rect) -> EditPlan {
        redact_with(rect, Color::BLACK)
    }

    fn redact_with(rect: Rect, color: Color) -> EditPlan {
        EditPlan {
            crop: None,
            operations: vec![EditOperation::Redact { rect, color }],
            ..EditPlan::default()
        }
    }

    fn chunk_types(png: &[u8]) -> Vec<[u8; 4]> {
        let mut types = Vec::new();
        let mut position = 8;
        while position + 8 <= png.len() {
            let length = u32::from_be_bytes(png[position..position + 4].try_into().unwrap());
            types.push(png[position + 4..position + 8].try_into().unwrap());
            position += length as usize + 12;
        }
        types
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A valid PNG whose IHDR claims `width` x `height`, with a correct CRC.
    fn png_with_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = encode_png(&unique_image(1, 1)).unwrap();
        assert_eq!(&png[12..16], b"IHDR");
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        png
    }

    #[test]
    fn redaction_is_opaque_and_no_original_pixel_survives_png_roundtrip() {
        let source = unique_image(32, 24);
        let rect = Rect {
            x: 5,
            y: 3,
            width: 10,
            height: 8,
        };
        let ring = border_colour(&source, rect);
        // (case, fill colour, expected pixel)
        let fills = [
            ("black box", Color::BLACK, [0, 0, 0, 255]),
            ("erase", ring, [ring.r, ring.g, ring.b, 255]),
            (
                "a translucent colour is written opaque",
                Color {
                    r: 200,
                    g: 100,
                    b: 50,
                    a: 10,
                },
                [200, 100, 50, 255],
            ),
        ];
        let secret: HashSet<[u8; 4]> = (3..11)
            .flat_map(|y| (5..15).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&source, x, y))
            .collect();
        for (case, color, expected) in fills {
            let edited = apply(&source, &redact_with(rect, color)).unwrap();
            for y in 3..11 {
                for x in 5..15 {
                    assert_eq!(pixel(&edited, x, y), expected, "{case}: pixel ({x}, {y})");
                }
            }
            let decoded = decode_png(&encode_png(&edited).unwrap()).unwrap();
            assert_eq!(decoded, edited, "{case}");
            // The area is one colour, proven above; any other original pixel
            // from inside it would be a leak.
            for rgba in decoded.pixels.as_chunks::<4>().0 {
                assert!(
                    *rgba == expected || !secret.contains(rgba),
                    "{case}: redacted pixel {rgba:?} survived"
                );
            }
            assert_eq!(pixel(&decoded, 4, 3), pixel(&source, 4, 3), "{case}");
            assert_eq!(pixel(&decoded, 15, 10), pixel(&source, 15, 10), "{case}");
        }

        // Erase takes the colour around the area: a plain background makes
        // the area vanish, and an area with nothing around it falls back to black.
        let plain = RgbaImage::new(6, 6, [30, 60, 90, 255].repeat(36)).unwrap();
        let inside = Rect {
            x: 2,
            y: 2,
            width: 2,
            height: 2,
        };
        assert_eq!(
            border_colour(&plain, inside),
            Color {
                r: 30,
                g: 60,
                b: 90,
                a: 255
            }
        );
        let everything = Rect {
            x: 0,
            y: 0,
            width: 6,
            height: 6,
        };
        assert_eq!(border_colour(&plain, everything), Color::BLACK);
    }

    #[test]
    fn crop_after_redaction_keeps_redaction_at_crop_dimensions() {
        let source = unique_image(20, 20);
        let plan = EditPlan {
            crop: Some(Rect::normalized((12, 13), (2, 3))),
            operations: vec![EditOperation::Redact {
                rect: Rect {
                    x: 4,
                    y: 5,
                    width: 3,
                    height: 2,
                },
                color: Color::BLACK,
            }],
            ..EditPlan::default()
        };
        let cropped = apply(&source, &plan).unwrap();
        assert_eq!((cropped.width, cropped.height), (10, 10));
        for y in 0..10 {
            for x in 0..10 {
                let (source_x, source_y) = (x + 2, y + 3);
                let redacted = (4..7).contains(&source_x) && (5..7).contains(&source_y);
                let expected = if redacted {
                    REDACTED_PIXEL
                } else {
                    pixel(&source, source_x, source_y)
                };
                assert_eq!(pixel(&cropped, x, y), expected, "crop pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn partially_outside_redaction_clamps_to_the_image() {
        let source = unique_image(10, 10);
        let edited = apply(
            &source,
            &redact(Rect {
                x: -5,
                y: -5,
                width: 10,
                height: 10,
            }),
        )
        .unwrap();
        assert_eq!(pixel(&edited, 0, 0), REDACTED_PIXEL);
        assert_eq!(pixel(&edited, 4, 4), REDACTED_PIXEL);
        assert_eq!(pixel(&edited, 5, 4), pixel(&source, 5, 4));
        assert_eq!(pixel(&edited, 4, 5), pixel(&source, 4, 5));
    }

    #[test]
    fn extreme_geometry_clamps_without_panicking() {
        let source = unique_image(16, 12);
        let far = Rect::normalized((i32::MIN, i32::MIN), (i32::MAX, i32::MAX));
        let outside = Rect {
            x: i32::MAX,
            y: i32::MAX,
            width: u32::MAX,
            height: u32::MAX,
        };
        let plan = EditPlan {
            crop: None,
            operations: vec![
                EditOperation::Highlight {
                    rect: outside,
                    color: Color::YELLOW,
                },
                EditOperation::Rectangle {
                    rect: far,
                    color: Color::RED,
                    thickness: u32::MAX,
                },
                EditOperation::Rectangle {
                    rect: Rect {
                        x: -3,
                        y: 40,
                        width: 0,
                        height: 0,
                    },
                    color: Color::RED,
                    thickness: 0,
                },
                EditOperation::Arrow {
                    from: (i32::MIN, i32::MIN),
                    to: (i32::MAX, i32::MAX),
                    color: Color::RED,
                    thickness: u32::MAX,
                },
                EditOperation::Arrow {
                    from: (-1_000, 5),
                    to: (-900, 5),
                    color: Color::RED,
                    thickness: 0,
                },
                EditOperation::Arrow {
                    from: (3, 3),
                    to: (3, 3),
                    color: Color::RED,
                    thickness: 4,
                },
                EditOperation::Redact {
                    rect: outside,
                    color: Color::BLACK,
                },
            ],
            ..EditPlan::default()
        };
        let edited = apply(&source, &plan).unwrap();
        assert_eq!((edited.width, edited.height), (16, 12));

        let blacked = apply(&source, &redact(far)).unwrap();
        assert!(
            blacked
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|rgba| *rgba == REDACTED_PIXEL)
        );
    }

    #[test]
    fn empty_or_outside_crop_is_rejected() {
        let source = unique_image(8, 8);
        for crop in [
            Rect {
                x: 8,
                y: 0,
                width: 4,
                height: 4,
            },
            Rect {
                x: -10,
                y: -10,
                width: 10,
                height: 10,
            },
            Rect {
                x: 2,
                y: 2,
                width: 0,
                height: 3,
            },
        ] {
            let plan = EditPlan {
                crop: Some(crop),
                operations: Vec::new(),
                ..EditPlan::default()
            };
            assert_eq!(
                apply(&source, &plan),
                Err(ImageError::EmptyCrop),
                "{crop:?}"
            );
        }
    }

    #[test]
    fn plans_over_the_operation_limit_are_rejected() {
        let source = unique_image(4, 4);
        let operation = EditOperation::Redact {
            rect: Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            color: Color::BLACK,
        };
        let at_limit = EditPlan {
            crop: None,
            operations: vec![operation; MAX_EDIT_OPERATIONS],
            ..EditPlan::default()
        };
        assert!(apply(&source, &at_limit).is_ok());
        let over_limit = EditPlan {
            crop: None,
            operations: vec![operation; MAX_EDIT_OPERATIONS + 1],
            ..EditPlan::default()
        };
        assert_eq!(
            apply(&source, &over_limit),
            Err(ImageError::TooManyOperations)
        );
    }

    #[test]
    fn spotlights_dim_the_rest_once_and_leave_annotations_bright() {
        let source = RgbaImage::new(20, 10, [200, 100, 50, 255].repeat(200)).unwrap();
        let spot = |x, y| EditOperation::Spotlight(Rect::normalized((x, y), (x + 4, y + 4)));
        let plan = EditPlan {
            crop: None,
            operations: vec![
                // Drawn before the spotlights, outside them.
                EditOperation::Rectangle {
                    rect: Rect::normalized((14, 6), (20, 10)),
                    color: Color::RED,
                    thickness: 1,
                },
                spot(0, 0),
                spot(2, 2),
            ],
            ..EditPlan::default()
        };
        let edited = apply(&source, &plan).unwrap();

        assert_eq!(
            pixel(&edited, 1, 1),
            [200, 100, 50, 255],
            "inside one spotlight"
        );
        assert_eq!(
            pixel(&edited, 3, 3),
            [200, 100, 50, 255],
            "the overlap is not dimmed twice"
        );
        assert_eq!(
            pixel(&edited, 5, 5),
            [200, 100, 50, 255],
            "inside the other"
        );
        assert_eq!(
            pixel(&edited, 10, 1),
            [80, 40, 20, 255],
            "outside every spotlight keeps 40% at the 60% default"
        );
        assert_eq!(
            pixel(&edited, 19, 9),
            [255, 0, 0, 255],
            "an annotation outside the spotlights is never dimmed"
        );
        assert_eq!(
            source.pixels[..4],
            [200, 100, 50, 255],
            "the source is unchanged"
        );

        for (asked, kept) in [(0, 180), (25, 150), (100, 20)] {
            let plan = EditPlan {
                crop: None,
                operations: vec![spot(0, 0)],
                spotlight_dim_percent: asked,
                ..EditPlan::default()
            };
            assert_eq!(
                pixel(&apply(&source, &plan).unwrap(), 10, 1)[0],
                kept,
                "dim {asked}% clamps to 10–90%"
            );
        }
    }

    #[test]
    fn the_canvas_grows_around_the_source_and_edits_stay_in_source_coordinates() {
        let source = RgbaImage::new(2, 2, [10, 20, 30, 255].repeat(4)).unwrap();
        let plan = EditPlan {
            canvas: CanvasExpansion {
                top: 1,
                right: 2,
                bottom: 0,
                left: 3,
                background: Color::WHITE,
            },
            // The canvas's top-left corner, left of and above the source.
            operations: vec![EditOperation::Redact {
                rect: Rect::normalized((-3, -1), (-2, 0)),
                color: Color::BLACK,
            }],
            ..EditPlan::default()
        };
        let grown = apply(&source, &plan).unwrap();
        assert_eq!((grown.width, grown.height), (7, 3));
        assert_eq!(
            pixel(&grown, 0, 0),
            [0, 0, 0, 255],
            "a negative edit lands in the space"
        );
        assert_eq!(pixel(&grown, 6, 0), [255, 255, 255, 255]);
        assert_eq!(
            pixel(&grown, 3, 1),
            [10, 20, 30, 255],
            "the source sits at (3, 1)"
        );
        assert_eq!(pixel(&grown, 4, 2), [10, 20, 30, 255]);
        assert_eq!(pixel(&grown, 5, 1), [255, 255, 255, 255]);

        let transparent = EditPlan {
            canvas: CanvasExpansion {
                bottom: 1,
                background: Color::TRANSPARENT,
                ..CanvasExpansion::default()
            },
            crop: Some(Rect::normalized((0, 1), (2, 3))),
            ..EditPlan::default()
        };
        let cropped = apply(&source, &transparent).unwrap();
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(pixel(&cropped, 0, 0), [10, 20, 30, 255]);
        assert_eq!(
            pixel(&cropped, 0, 1)[3],
            0,
            "the crop, in source coordinates, reaches the transparent space"
        );

        let too_wide = EditPlan {
            canvas: CanvasExpansion {
                left: MAX_CANVAS_PADDING + 1,
                ..CanvasExpansion::default()
            },
            ..EditPlan::default()
        };
        assert_eq!(apply(&source, &too_wide), Err(ImageError::CanvasTooLarge));
        let big = RgbaImage::new(8192, 8000, vec![0; 8192 * 8000 * 4]).unwrap();
        let too_many_pixels = EditPlan {
            canvas: CanvasExpansion {
                bottom: 200,
                ..CanvasExpansion::default()
            },
            ..EditPlan::default()
        };
        assert_eq!(
            apply(&big, &too_many_pixels),
            Err(ImageError::CanvasTooLarge),
            "8192 × 8200 is over 64 megapixels"
        );
    }

    #[test]
    fn pasted_images_scale_blend_and_dim_with_the_picture() {
        let source = RgbaImage::new(12, 6, [200, 200, 200, 255].repeat(72)).unwrap();
        let red = RgbaImage::new(2, 2, [255, 0, 0, 255].repeat(4)).unwrap();
        // Black and white columns: shrunk to one pixel they average to grey.
        let stripes = RgbaImage::new(
            4,
            4,
            [[0, 0, 0, 255], [255, 255, 255, 255]].concat().repeat(8),
        )
        .unwrap();
        let half_clear = RgbaImage::new(1, 1, vec![0, 0, 255, 128]).unwrap();
        let plan = EditPlan {
            pasted: vec![red, stripes, half_clear],
            operations: vec![
                EditOperation::Paste {
                    image: 0,
                    rect: Rect::normalized((0, 0), (4, 4)),
                },
                EditOperation::Paste {
                    image: 1,
                    rect: Rect::normalized((5, 0), (6, 1)),
                },
                EditOperation::Paste {
                    image: 2,
                    rect: Rect::normalized((7, 0), (8, 1)),
                },
            ],
            ..EditPlan::default()
        };
        let edited = apply(&source, &plan).unwrap();
        assert_eq!(pixel(&edited, 3, 3), [255, 0, 0, 255], "enlarged two times");
        assert_eq!(
            pixel(&edited, 4, 4),
            [200, 200, 200, 255],
            "outside the paste"
        );
        assert_eq!(
            pixel(&edited, 5, 0),
            [128, 128, 128, 255],
            "shrunk to the average"
        );
        assert_eq!(
            pixel(&edited, 7, 0),
            [99, 99, 227, 255],
            "half-transparent blue blends over the screenshot"
        );

        let spotlit = EditPlan {
            operations: vec![
                EditOperation::Paste {
                    image: 0,
                    rect: Rect::normalized((8, 2), (12, 6)),
                },
                EditOperation::Spotlight(Rect::normalized((0, 0), (2, 2))),
                EditOperation::Rectangle {
                    rect: Rect::normalized((0, 4), (2, 6)),
                    color: Color::RED,
                    thickness: 1,
                },
            ],
            ..plan.clone()
        };
        let edited = apply(&source, &spotlit).unwrap();
        assert_eq!(
            pixel(&edited, 10, 4),
            [102, 0, 0, 255],
            "the paste dims with the picture"
        );
        assert_eq!(
            pixel(&edited, 0, 5),
            [255, 0, 0, 255],
            "the box stays bright"
        );

        let missing = EditPlan {
            pasted: Vec::new(),
            ..spotlit.clone()
        };
        assert_eq!(apply(&source, &missing), Err(ImageError::UnknownPaste));
        let crowded = EditPlan {
            pasted: vec![source.clone(); MAX_PASTED_IMAGES + 1],
            ..EditPlan::default()
        };
        assert_eq!(apply(&source, &crowded), Err(ImageError::TooManyPastes));
    }

    #[test]
    fn png_roundtrip_is_lossless() {
        let mut image = unique_image(17, 9);
        image.pixels[3] = 0;
        image.pixels[7] = 128;
        assert_eq!(decode_png(&encode_png(&image).unwrap()).unwrap(), image);
    }

    #[test]
    fn encoded_png_has_no_text_or_time_chunks() {
        let png = encode_png(&unique_image(6, 6)).unwrap();
        let types = chunk_types(&png);
        assert_eq!(types.first(), Some(b"IHDR"));
        assert_eq!(types.last(), Some(b"IEND"));
        for forbidden in [b"tEXt", b"zTXt", b"iTXt", b"tIME"] {
            assert!(!types.contains(forbidden), "found {forbidden:?}");
        }
    }

    #[test]
    fn decode_rejects_zero_and_oversized_dimensions() {
        assert!(decode_png(&png_with_header(1, 1)).is_ok());
        assert!(decode_png(&png_with_header(0, 1)).is_err());
        assert!(decode_png(&png_with_header(1, 0)).is_err());
        assert_eq!(
            decode_png(&png_with_header(MAX_CAPTURE_DIMENSION + 1, 1)),
            Err(ImageError::InvalidDimensions)
        );
        assert_eq!(
            decode_png(&png_with_header(1, MAX_CAPTURE_DIMENSION + 1)),
            Err(ImageError::InvalidDimensions)
        );
    }

    #[test]
    fn invalid_pixel_buffers_are_rejected() {
        assert_eq!(
            RgbaImage::new(0, 1, Vec::new()),
            Err(ImageError::InvalidDimensions)
        );
        assert_eq!(
            RgbaImage::new(2, 2, vec![0; 15]),
            Err(ImageError::InvalidLength)
        );
    }
}
