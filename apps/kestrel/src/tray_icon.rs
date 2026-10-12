//! Square tray icons that show a readout's label above its value.

use pangocairo::{cairo, pango};

/// Pixel size of the rendered icon; hosts scale it to their panel size.
pub const READOUT_ICON_SIZE: i32 = 64;

/// SNI pixmap: width, height, and ARGB32 bytes in network order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconPixels {
    pub width: i32,
    pub height: i32,
    pub argb: Vec<u8>,
}

/// Light text on a dark rounded plate stays legible on light and dark panels.
pub fn readout_icon(label: &str, value: &str) -> Option<IconPixels> {
    let size = READOUT_ICON_SIZE;
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let context = cairo::Context::new(&surface).ok()?;
        let width = f64::from(size);
        rounded_plate(&context, width, 12.0)?;
        // Label on the top third, value on the bottom two thirds.
        draw_line(&context, label, 5.0, 18.0, width, 0.78)?;
        draw_line(&context, value, 25.0, 34.0, width, 1.0)?;
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().ok()?;
    let mut argb = Vec::with_capacity((size * size * 4) as usize);
    for row in data.chunks(stride).take(size as usize) {
        for pixel in row[..size as usize * 4].as_chunks::<4>().0 {
            // Cairo ARGB32 is a native-endian u32 with premultiplied colour.
            argb.extend_from_slice(&u32::from_ne_bytes(*pixel).to_be_bytes());
        }
    }
    Some(IconPixels {
        width: size,
        height: size,
        argb,
    })
}

fn rounded_plate(context: &cairo::Context, size: f64, radius: f64) -> Option<()> {
    use std::f64::consts::{FRAC_PI_2, PI};
    context.new_sub_path();
    context.arc(size - radius, radius, radius, -FRAC_PI_2, 0.0);
    context.arc(size - radius, size - radius, radius, 0.0, FRAC_PI_2);
    context.arc(radius, size - radius, radius, FRAC_PI_2, PI);
    context.arc(radius, radius, radius, PI, 3.0 * FRAC_PI_2);
    context.close_path();
    context.set_source_rgba(0.16, 0.16, 0.17, 0.94);
    context.fill().ok()
}

/// Centres `text` in the band `top..top + height`, shrinking it to fit the width.
fn draw_line(
    context: &cairo::Context,
    text: &str,
    top: f64,
    height: f64,
    width: f64,
    brightness: f64,
) -> Option<()> {
    if text.is_empty() {
        return Some(());
    }
    let layout = pangocairo::functions::create_layout(context);
    layout.set_text(text);
    let mut font_px = height;
    let mut description = pango::FontDescription::from_string("Sans Bold");
    let ink = loop {
        description.set_absolute_size(font_px * f64::from(pango::SCALE));
        layout.set_font_description(Some(&description));
        let (ink, _) = layout.pixel_extents();
        if f64::from(ink.width()) <= width - 8.0 && f64::from(ink.height()) <= height
            || font_px <= 8.0
        {
            break ink;
        }
        font_px -= 1.0;
    };
    let x = (width - f64::from(ink.width())) / 2.0 - f64::from(ink.x());
    let y = top + (height - f64::from(ink.height())) / 2.0 - f64::from(ink.y());
    context.move_to(x, y);
    context.set_source_rgba(brightness, brightness, brightness, 1.0);
    pangocairo::functions::show_layout(context, &layout);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::{READOUT_ICON_SIZE, readout_icon};

    /// Pixels brighter than the plate: the rendered text.
    fn text_pixels(label: &str, value: &str) -> usize {
        let icon = readout_icon(label, value).expect("cairo renders an image surface");
        assert_eq!(icon.width, READOUT_ICON_SIZE);
        assert_eq!(icon.argb.len(), (icon.width * icon.height * 4) as usize);
        icon.argb
            .chunks(4)
            .filter(|pixel| pixel[0] == 0xff && pixel[1] > 0x90)
            .count()
    }

    #[test]
    fn label_and_value_are_drawn_on_the_plate() {
        assert_eq!(text_pixels("", ""), 0, "an empty plate has no text");
        let label_only = text_pixels("CPU", "");
        let both = text_pixels("CPU", "12%");
        assert!(label_only > 50, "the label is drawn: {label_only}");
        assert!(
            both > label_only + 100,
            "the value is drawn below it: {both}"
        );
    }
}
