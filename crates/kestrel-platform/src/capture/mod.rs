//! Screenshot capture: provider detection, a read-only probe, and one-shot
//! capture backends (Screenshot portal, `grim`/`slurp`, X11).

mod detect;
mod grim;
mod portal;
mod x11;

use std::fmt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub use detect::{CaptureDetection, CaptureProbe, ScreenPermission};
pub use grim::GrimCaptureBackend;
pub use portal::PortalCaptureBackend;
pub use x11::X11CaptureBackend;

pub const FEATURE_ID: &str = "capture.screenshot";

/// Largest PNG a backend returns, so a capture cannot exhaust memory.
pub const MAX_CAPTURE_PNG_BYTES: usize = 64 * 1024 * 1024;
/// Largest width or height accepted from any provider.
pub const MAX_CAPTURE_DIMENSION: u32 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureProvider {
    /// `org.freedesktop.portal.Screenshot`.
    Portal,
    /// `grim`, with `slurp` for area selection, on wlroots-style compositors.
    Grim,
    /// Core X11 `GetImage` on a real X11 session.
    X11,
}

impl CaptureProvider {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Portal => "Screenshot portal",
            Self::Grim => "grim",
            Self::X11 => "X11",
        }
    }
}

/// What a capture covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CaptureMode {
    /// Every output, with no selection step.
    Screen,
    /// A rectangle the user drags.
    Area,
    /// The focused window.
    Window,
    /// The desktop's own capture dialog chooses area, window, or screen.
    Interactive,
}

pub const ALL_CAPTURE_MODES: [CaptureMode; 4] = [
    CaptureMode::Interactive,
    CaptureMode::Area,
    CaptureMode::Window,
    CaptureMode::Screen,
];

impl CaptureMode {
    /// Stable token used in file names and configuration.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Area => "area",
            Self::Window => "window",
            Self::Interactive => "interactive",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        ALL_CAPTURE_MODES
            .into_iter()
            .find(|mode| mode.token() == token)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Screen => "Screen",
            Self::Area => "Area",
            Self::Window => "Window",
            Self::Interactive => "Choose in dialog",
        }
    }
}

/// An encoded PNG with its validated dimensions.
#[derive(Clone, PartialEq, Eq)]
pub struct CapturedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl fmt::Debug for CapturedImage {
    // Pixel data stays out of logs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedImage")
            .field("bytes", &self.png.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureErrorKind {
    /// The user dismissed the selection or dialog.
    Cancelled,
    /// The desktop refused, or permission was revoked.
    Denied,
    /// The provider or a needed executable is missing.
    Unavailable,
    /// The provider kept the result elsewhere (for example the clipboard).
    NoImage,
    /// The image exceeds the size bounds.
    TooLarge,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    pub kind: CaptureErrorKind,
    /// Kestrel-authored and free of paths and pixel data.
    pub message: String,
}

impl CaptureError {
    pub fn new(kind: CaptureErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CaptureError {}

type AbortHook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    abort: Mutex<Option<AbortHook>>,
}

/// Cancels one in-flight capture from another thread.
///
/// A backend installs an abort hook (close a D-Bus connection, kill a child)
/// that `cancel` runs once; a hook installed after cancellation runs at once.
#[derive(Clone, Default)]
pub struct CaptureCancel(Arc<CancelInner>);

impl CaptureCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        let hook = self
            .0
            .abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn set_abort(&self, hook: AbortHook) {
        let mut slot = self
            .0
            .abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_cancelled() {
            drop(slot);
            hook();
            return;
        }
        *slot = Some(hook);
    }

    pub fn clear_abort(&self) {
        self.0
            .abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}

/// One-shot screenshot provider. `capture` blocks until the image is ready,
/// the user cancels, or `cancel` aborts it, and leaves no temporary file.
pub trait CaptureBackend: Send {
    fn provider(&self) -> CaptureProvider;
    fn modes(&self) -> Vec<CaptureMode>;
    fn capture(
        &self,
        mode: CaptureMode,
        cancel: &CaptureCancel,
    ) -> Result<CapturedImage, CaptureError>;
}

/// The production backend for `provider`, using executables found by `detection`.
pub fn backend_for(
    provider: CaptureProvider,
    detection: &CaptureDetection,
) -> Box<dyn CaptureBackend> {
    match provider {
        CaptureProvider::Portal => Box::new(PortalCaptureBackend::new()),
        CaptureProvider::Grim => Box::new(GrimCaptureBackend::from_detection(
            detection.grim.clone(),
            detection.slurp.clone(),
        )),
        CaptureProvider::X11 => Box::new(X11CaptureBackend::new()),
    }
}

fn cancelled() -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::Cancelled,
        "Screenshot capture was cancelled.",
    )
}

fn too_large() -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::TooLarge,
        "The screenshot exceeds the supported size.",
    )
}

fn dimensions_in_bounds(width: u32, height: u32) -> Result<(), CaptureError> {
    if width == 0 || height == 0 {
        return Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "The screenshot has no pixels.",
        ));
    }
    if width > MAX_CAPTURE_DIMENSION || height > MAX_CAPTURE_DIMENSION {
        return Err(too_large());
    }
    Ok(())
}

/// Checks the PNG signature and IHDR dimensions.
fn validate_png(png: Vec<u8>) -> Result<CapturedImage, CaptureError> {
    if png.len() > MAX_CAPTURE_PNG_BYTES {
        return Err(too_large());
    }
    let invalid = || CaptureError::new(CaptureErrorKind::Failed, "The screenshot is not a PNG.");
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid());
    }
    let (width, height) = {
        let reader = png::Decoder::new(std::io::Cursor::new(png.as_slice()))
            .read_info()
            .map_err(|_| invalid())?;
        let info = reader.info();
        (info.width, info.height)
    };
    dimensions_in_bounds(width, height)?;
    Ok(CapturedImage { png, width, height })
}

/// Encodes 8-bit RGBA rows as a PNG.
fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, CaptureError> {
    let failed = || CaptureError::new(CaptureErrorKind::Failed, "Encoding the screenshot failed.");
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|_| failed())?;
    writer.write_image_data(rgba).map_err(|_| failed())?;
    writer.finish().map_err(|_| failed())?;
    Ok(png)
}

/// Decodes a PNG to RGBA for assertions.
#[cfg(test)]
fn decode_rgba(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .expect("decode PNG header");
    let size = reader
        .output_buffer_size()
        .expect("frame size fits in memory");
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer).expect("decode PNG frame");
    assert_eq!(frame.color_type, png::ColorType::Rgba);
    buffer.truncate(frame.buffer_size());
    (frame.width, frame.height, buffer)
}

/// A solid-colour PNG for fakes.
#[cfg(test)]
fn solid_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|_| rgba).collect();
    encode_rgba_png(width, height, &pixels).expect("encode test PNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_abort_hook_runs_once_even_when_installed_after_cancel() {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = CaptureCancel::new();
        let counter = Arc::clone(&runs);
        cancel.set_abort(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        cancel.cancel();
        cancel.cancel();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let counter = Arc::clone(&runs);
        cancel.set_abort(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(runs.load(Ordering::SeqCst), 2, "late hook runs immediately");
    }

    #[test]
    fn mode_tokens_round_trip() {
        for mode in ALL_CAPTURE_MODES {
            assert_eq!(CaptureMode::from_token(mode.token()), Some(mode));
        }
    }

    #[test]
    fn png_validation_reads_ihdr_and_rejects_non_png_or_oversized_images() {
        let image = validate_png(solid_png(3, 2, [1, 2, 3, 255])).expect("valid PNG");
        assert_eq!((image.width, image.height), (3, 2));

        let not_png = validate_png(b"GIF89a....".to_vec()).unwrap_err();
        assert_eq!(not_png.kind, CaptureErrorKind::Failed);

        assert_eq!(
            dimensions_in_bounds(MAX_CAPTURE_DIMENSION + 1, 1)
                .unwrap_err()
                .kind,
            CaptureErrorKind::TooLarge
        );
        assert_eq!(
            dimensions_in_bounds(0, 1).unwrap_err().kind,
            CaptureErrorKind::Failed
        );
        assert_eq!(
            validate_png(vec![0; MAX_CAPTURE_PNG_BYTES + 1])
                .unwrap_err()
                .kind,
            CaptureErrorKind::TooLarge
        );
    }
}
