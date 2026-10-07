//! In-memory RGBA images: PNG decode/encode and edit plans (redact, shapes, crop).

use std::fmt;

use kestrel_platform::capture::MAX_CAPTURE_DIMENSION;

/// Most operations one edit plan may contain.
const MAX_EDIT_OPERATIONS: usize = 256;

/// Opaque black written over redacted pixels.
const REDACTED_PIXEL: [u8; 4] = [0, 0, 0, 255];

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
    let mut frame = vec![0; reader.output_buffer_size()];
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

    /// The part of this rectangle inside a `width` x `height` image, if any.
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
}

/// Non-empty pixel range inside an image; `right` and `bottom` are exclusive.
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
}

/// One drawing step of an edit plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOperation {
    Redact(Rect),
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
}

/// Operations applied in order in source coordinates, then an optional crop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditPlan {
    pub crop: Option<Rect>,
    pub operations: Vec<EditOperation>,
}

/// Applies `plan` to a copy of `source`.
pub fn apply(source: &RgbaImage, plan: &EditPlan) -> Result<RgbaImage, ImageError> {
    let mut image = RgbaImage::new(source.width, source.height, source.pixels.clone())?;
    if plan.operations.len() > MAX_EDIT_OPERATIONS {
        return Err(ImageError::TooManyOperations);
    }
    for operation in &plan.operations {
        match *operation {
            EditOperation::Redact(rect) => {
                for_each_pixel(&mut image, rect, |pixel, _| {
                    pixel.copy_from_slice(&REDACTED_PIXEL)
                });
            }
            EditOperation::Highlight { rect, color } => {
                for_each_pixel(&mut image, rect, |pixel, _| blend(pixel, color));
            }
            EditOperation::Rectangle {
                rect,
                color,
                thickness,
            } => {
                let thickness = i64::from(thickness);
                for_each_pixel(&mut image, rect, |pixel, (x, y, bounds)| {
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
            } => draw_arrow(&mut image, from, to, color, thickness),
        }
    }
    match plan.crop {
        Some(crop) => crop_image(&image, crop),
        None => Ok(image),
    }
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    let valid = |side: u32| (1..=MAX_CAPTURE_DIMENSION).contains(&side);
    valid(width) && valid(height)
}

fn png_error(error: impl fmt::Display) -> ImageError {
    ImageError::Png(error.to_string())
}

/// Byte offset of pixel `(x, y)` in an image `width` pixels wide.
fn offset(width: u32, x: u32, y: u32) -> usize {
    (y as usize * width as usize + x as usize) * 4
}

/// Calls `visit` with each pixel of `rect` clipped to the image, plus its coordinates and clip.
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

/// Source-over blend of `color` onto one RGBA pixel.
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
fn crop_image(image: &RgbaImage, crop: Rect) -> Result<RgbaImage, ImageError> {
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

/// Shaft from `from` to `to` with a filled head at `to`.
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

/// Round-capped line of `thickness` pixels, clipped to the image.
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
        EditPlan {
            crop: None,
            operations: vec![EditOperation::Redact(rect)],
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
    fn redaction_is_opaque_black_and_no_original_pixel_survives_png_roundtrip() {
        let source = unique_image(32, 24);
        let rect = Rect {
            x: 5,
            y: 3,
            width: 10,
            height: 8,
        };
        let edited = apply(&source, &redact(rect)).unwrap();
        for y in 3..11 {
            for x in 5..15 {
                assert_eq!(pixel(&edited, x, y), REDACTED_PIXEL, "pixel ({x}, {y})");
            }
        }
        let secret: HashSet<[u8; 4]> = (3..11)
            .flat_map(|y| (5..15).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&source, x, y))
            .collect();
        let decoded = decode_png(&encode_png(&edited).unwrap()).unwrap();
        assert_eq!(decoded, edited);
        for rgba in decoded.pixels.as_chunks::<4>().0 {
            assert!(!secret.contains(rgba), "redacted pixel {rgba:?} survived");
        }
        assert_eq!(pixel(&decoded, 4, 3), pixel(&source, 4, 3));
        assert_eq!(pixel(&decoded, 15, 10), pixel(&source, 15, 10));
    }

    #[test]
    fn crop_after_redaction_keeps_redaction_at_crop_dimensions() {
        let source = unique_image(20, 20);
        let plan = EditPlan {
            crop: Some(Rect::normalized((12, 13), (2, 3))),
            operations: vec![EditOperation::Redact(Rect {
                x: 4,
                y: 5,
                width: 3,
                height: 2,
            })],
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
                EditOperation::Redact(outside),
            ],
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
        let operation = EditOperation::Redact(Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });
        let at_limit = EditPlan {
            crop: None,
            operations: vec![operation; MAX_EDIT_OPERATIONS],
        };
        assert!(apply(&source, &at_limit).is_ok());
        let over_limit = EditPlan {
            crop: None,
            operations: vec![operation; MAX_EDIT_OPERATIONS + 1],
        };
        assert_eq!(
            apply(&source, &over_limit),
            Err(ImageError::TooManyOperations)
        );
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
