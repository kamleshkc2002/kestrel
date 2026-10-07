//! Core X11 `GetImage` capture of the root window, for real X11 sessions.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, ImageFormat, ImageOrder, Screen, VisualClass, Visualtype, Window,
};
use x11rb::rust_connection::RustConnection;

use super::{
    CaptureBackend, CaptureCancel, CaptureError, CaptureErrorKind, CaptureMode, CaptureProvider,
    CapturedImage, MAX_CAPTURE_PNG_BYTES, cancelled, dimensions_in_bounds, encode_rgba_png,
    too_large,
};

pub struct X11CaptureBackend {
    /// `None` uses `$DISPLAY`.
    display: Option<String>,
}

impl X11CaptureBackend {
    pub fn new() -> Self {
        Self { display: None }
    }

    #[cfg(test)]
    pub(crate) fn new_for_display(display: impl Into<String>) -> Self {
        Self {
            display: Some(display.into()),
        }
    }
}

impl Default for X11CaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for X11CaptureBackend {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::X11
    }

    fn modes(&self) -> Vec<CaptureMode> {
        vec![CaptureMode::Window, CaptureMode::Screen]
    }

    fn capture(
        &self,
        mode: CaptureMode,
        cancel: &CaptureCancel,
    ) -> Result<CapturedImage, CaptureError> {
        if matches!(mode, CaptureMode::Area | CaptureMode::Interactive) {
            return Err(unavailable(
                "X11 capture covers the screen or the focused window only.",
            ));
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let (connection, screen_number) = RustConnection::connect(self.display.as_deref())
            .map_err(|_| unavailable("Connecting to the X11 display failed."))?;
        let screen = connection
            .setup()
            .roots
            .get(screen_number)
            .ok_or_else(|| failed("The X11 display has no screen."))?;
        let format = PixelFormat::for_screen(&connection, screen)?;
        let area = match mode {
            CaptureMode::Window => focused_window_area(&connection, screen)?,
            _ => Rect {
                x: 0,
                y: 0,
                width: screen.width_in_pixels,
                height: screen.height_in_pixels,
            },
        };
        dimensions_in_bounds(u32::from(area.width), u32::from(area.height))?;
        let image = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                screen.root,
                area.x,
                area.y,
                area.width,
                area.height,
                u32::MAX,
            )
            .map_err(|_| failed("Requesting the X11 image failed."))?
            .reply()
            .map_err(|_| failed("Reading the X11 image failed."))?;
        if image.depth != screen.root_depth {
            return Err(unavailable("The X11 image has an unsupported depth."));
        }
        let rgba = format.to_rgba(&image.data, area.width, area.height)?;
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let width = u32::from(area.width);
        let height = u32::from(area.height);
        let png = encode_rgba_png(width, height, &rgba)?;
        if png.len() > MAX_CAPTURE_PNG_BYTES {
            return Err(too_large());
        }
        Ok(CapturedImage { png, width, height })
    }
}

fn unavailable(message: &'static str) -> CaptureError {
    CaptureError::new(CaptureErrorKind::Unavailable, message)
}

fn failed(message: &'static str) -> CaptureError {
    CaptureError::new(CaptureErrorKind::Failed, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// The `_NET_ACTIVE_WINDOW` area in root coordinates, clipped to the root.
fn focused_window_area(connection: &RustConnection, screen: &Screen) -> Result<Rect, CaptureError> {
    let no_window = || failed("No focused window");
    let atom = connection
        .intern_atom(true, b"_NET_ACTIVE_WINDOW")
        .map_err(|_| no_window())?
        .reply()
        .map_err(|_| no_window())?
        .atom;
    if atom == 0 {
        return Err(no_window());
    }
    let window: Window = connection
        .get_property(false, screen.root, atom, AtomEnum::WINDOW, 0, 1)
        .map_err(|_| no_window())?
        .reply()
        .map_err(|_| no_window())?
        .value32()
        .and_then(|mut values| values.next())
        .filter(|&window| window != 0)
        .ok_or_else(no_window)?;
    let geometry = connection
        .get_geometry(window)
        .map_err(|_| no_window())?
        .reply()
        .map_err(|_| no_window())?;
    let origin = connection
        .translate_coordinates(window, screen.root, 0, 0)
        .map_err(|_| no_window())?
        .reply()
        .map_err(|_| no_window())?;
    clip_to_root(
        i32::from(origin.dst_x),
        i32::from(origin.dst_y),
        geometry.width,
        geometry.height,
        screen,
    )
    .ok_or_else(no_window)
}

fn clip_to_root(x: i32, y: i32, width: u16, height: u16, screen: &Screen) -> Option<Rect> {
    let left = x.max(0);
    let top = y.max(0);
    let right = (x + i32::from(width)).min(i32::from(screen.width_in_pixels));
    let bottom = (y + i32::from(height)).min(i32::from(screen.height_in_pixels));
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect {
        x: i16::try_from(left).ok()?,
        y: i16::try_from(top).ok()?,
        width: u16::try_from(right - left).ok()?,
        height: u16::try_from(bottom - top).ok()?,
    })
}

/// A TrueColor ZPixmap layout.
#[derive(Debug, Clone, Copy)]
struct PixelFormat {
    bytes_per_pixel: usize,
    scanline_pad: usize,
    lsb_first: bool,
    masks: [u32; 3],
}

impl PixelFormat {
    fn for_screen(connection: &RustConnection, screen: &Screen) -> Result<Self, CaptureError> {
        let unsupported = || unavailable("The X11 display uses an unsupported visual.");
        let setup = connection.setup();
        let visual: &Visualtype = screen
            .allowed_depths
            .iter()
            .filter(|depth| depth.depth == screen.root_depth)
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| visual.visual_id == screen.root_visual)
            .ok_or_else(unsupported)?;
        if visual.class != VisualClass::TRUE_COLOR {
            return Err(unsupported());
        }
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .ok_or_else(unsupported)?;
        let bytes_per_pixel = match format.bits_per_pixel {
            24 => 3,
            32 => 4,
            _ => return Err(unsupported()),
        };
        let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
        if masks.contains(&0) {
            return Err(unsupported());
        }
        Ok(Self {
            bytes_per_pixel,
            scanline_pad: usize::from(format.scanline_pad),
            lsb_first: setup.image_byte_order == ImageOrder::LSB_FIRST,
            masks,
        })
    }

    fn to_rgba(self, data: &[u8], width: u16, height: u16) -> Result<Vec<u8>, CaptureError> {
        let width = usize::from(width);
        let height = usize::from(height);
        let pad_bits = self.scanline_pad.max(8);
        let stride = (width * self.bytes_per_pixel * 8).div_ceil(pad_bits) * pad_bits / 8;
        if data.len() < stride * height {
            return Err(failed("The X11 image is truncated."));
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in data.chunks_exact(stride).take(height) {
            for pixel in row[..width * self.bytes_per_pixel].chunks_exact(self.bytes_per_pixel) {
                let value = if self.lsb_first {
                    pixel
                        .iter()
                        .rev()
                        .fold(0u32, |value, &byte| (value << 8) | u32::from(byte))
                } else {
                    pixel
                        .iter()
                        .fold(0u32, |value, &byte| (value << 8) | u32::from(byte))
                };
                rgba.extend(self.masks.map(|mask| channel(value, mask)));
                rgba.push(u8::MAX);
            }
        }
        Ok(rgba)
    }
}

/// Scales the masked bits to 8 bits.
fn channel(value: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let bits = u64::from((value & mask) >> shift);
    ((bits * 255 + max / 2) / max) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::decode_rgba;
    use std::fs;
    use std::process::{Child, Command, Stdio};
    use std::sync::{LazyLock, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};
    use x11rb::protocol::xproto::{CreateWindowAux, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;

    struct TestServer(Child);

    impl Drop for TestServer {
        fn drop(&mut self) {
            // SIGTERM lets Xvfb remove its socket and lock file.
            if let Ok(pid) = libc::pid_t::try_from(self.0.id()) {
                // SAFETY: signals only the child this guard spawned.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            let _ = self.0.wait();
        }
    }

    /// Starts a private Xvfb; `None` only when Xvfb is not installed.
    fn start_private_server() -> Option<(TestServer, String)> {
        Command::new("Xvfb").arg("-help").output().ok()?;
        // Disjoint from the global-shortcuts live test, which runs in parallel.
        for number in 200..300 {
            let socket = format!("/tmp/.X11-unix/X{number}");
            // Leaves displays owned by other servers alone.
            if fs::metadata(&socket).is_ok()
                || fs::metadata(format!("/tmp/.X{number}-lock")).is_ok()
            {
                continue;
            }
            let display = format!(":{number}");
            let Ok(mut child) = Command::new("Xvfb")
                .arg(&display)
                .args(["-nolisten", "tcp", "-noreset", "-screen", "0", "320x240x24"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                continue;
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                if fs::metadata(&socket).is_ok() && RustConnection::connect(Some(&display)).is_ok()
                {
                    return Some((TestServer(child), display));
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("Xvfb is installed but no private server started");
    }

    const ORANGE: [u8; 4] = [255, 128, 0, 255];

    fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 4] {
        let (width, _, pixels) = decode_rgba(png);
        let offset = ((y * width + x) * 4) as usize;
        pixels[offset..offset + 4]
            .try_into()
            .expect("four channels")
    }

    #[test]
    fn live_x11_captures_screen_and_focused_window() {
        static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((_server, display)) = start_private_server() else {
            eprintln!("skipping: Xvfb is not available");
            return;
        };
        let (connection, screen_number) =
            RustConnection::connect(Some(&display)).expect("connect to the private server");
        let screen = connection.setup().roots[screen_number].clone();
        let window = connection.generate_id().expect("window id");
        connection
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                40,
                30,
                64,
                48,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new().background_pixel(0x00ff_8000),
            )
            .expect("create window")
            .check()
            .expect("window is created");
        connection.map_window(window).expect("map window");
        let active = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .expect("intern atom")
            .reply()
            .expect("atom reply")
            .atom;
        connection
            .change_property32(
                PropMode::REPLACE,
                screen.root,
                active,
                AtomEnum::WINDOW,
                &[window],
            )
            .expect("set active window")
            .check()
            .expect("active window is set");

        let backend = X11CaptureBackend::new_for_display(&display);
        let cancel = CaptureCancel::new();
        let screen_image = backend
            .capture(CaptureMode::Screen, &cancel)
            .expect("screen capture");
        assert_eq!((screen_image.width, screen_image.height), (320, 240));
        assert_eq!(pixel(&screen_image.png, 50, 40), ORANGE);

        let window_image = backend
            .capture(CaptureMode::Window, &cancel)
            .expect("window capture");
        assert_eq!((window_image.width, window_image.height), (64, 48));
        let (width, height, pixels) = decode_rgba(&window_image.png);
        assert_eq!((width, height), (64, 48));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == ORANGE)
        );

        // A window hanging off the bottom-right corner is clipped to the root.
        connection
            .configure_window(
                window,
                &x11rb::protocol::xproto::ConfigureWindowAux::new()
                    .x(300)
                    .y(200),
            )
            .expect("move window")
            .check()
            .expect("window moved");
        let clipped = backend
            .capture(CaptureMode::Window, &cancel)
            .expect("clipped window capture");
        assert_eq!((clipped.width, clipped.height), (20, 40));
        assert_eq!(pixel(&clipped.png, 19, 39), ORANGE);

        connection
            .delete_property(screen.root, active)
            .expect("clear active window")
            .check()
            .expect("active window cleared");
        let error = backend.capture(CaptureMode::Window, &cancel).unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Failed);
        assert_eq!(error.message, "No focused window");

        for mode in [CaptureMode::Area, CaptureMode::Interactive] {
            assert_eq!(
                backend.capture(mode, &cancel).unwrap_err().kind,
                CaptureErrorKind::Unavailable
            );
        }
    }

    #[test]
    fn pixel_conversion_honours_byte_order_masks_and_padding() {
        let msb_24 = PixelFormat {
            bytes_per_pixel: 3,
            scanline_pad: 32,
            lsb_first: false,
            masks: [0xff_0000, 0x00_ff00, 0x00_00ff],
        };
        // One 3-byte pixel per row, padded to 4 bytes.
        let data = [0x11, 0x22, 0x33, 0xee, 0x44, 0x55, 0x66, 0xee];
        assert_eq!(
            msb_24.to_rgba(&data, 1, 2).unwrap(),
            vec![0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 255]
        );

        let lsb_565 = PixelFormat {
            bytes_per_pixel: 4,
            scanline_pad: 32,
            lsb_first: true,
            masks: [0xf800, 0x07e0, 0x001f],
        };
        let white_red = [0xff, 0xff, 0, 0, 0x00, 0xf8, 0, 0];
        assert_eq!(
            lsb_565.to_rgba(&white_red, 2, 1).unwrap(),
            vec![255, 255, 255, 255, 255, 0, 0, 255]
        );
        assert_eq!(
            lsb_565.to_rgba(&white_red[..4], 2, 1).unwrap_err().kind,
            CaptureErrorKind::Failed
        );
    }
}
