//! Screen tools: one-shot frames for reading text and QR codes, and color
//! samples.
//!
//! A job captures one frame, which waits in memory while the user selects a
//! region or pixel. The frame is released as soon as a selection is handed
//! over, the job is cancelled, or the service stops; results stay in memory
//! until dismissed.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kestrel_platform::capture::{
    CaptureBackend, CaptureCancel, CaptureError, CaptureErrorKind, CaptureMode, PortalColorPicker,
};
use kestrel_platform::screen_tools::{ScreenTool, TesseractAdapter};

use crate::capture::Rect;
use crate::capture::image::{RgbaImage, crop_image, decode_png, encode_png};

/// Most QR codes kept from one frame.
pub const MAX_QR_PAYLOADS: usize = 16;
/// Largest QR payload kept as text.
pub const MAX_QR_PAYLOAD_BYTES: usize = 4096;

/// One sampled sRGB pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorSample {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl ColorSample {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }

    pub fn rgb(self) -> String {
        format!("rgb({}, {}, {})", self.red, self.green, self.blue)
    }

    pub fn hsl(self) -> String {
        let (hue, saturation, lightness) = self.hsl_components();
        format!("hsl({hue}, {saturation}%, {lightness}%)")
    }

    /// Hue in degrees, saturation and lightness in rounded percent.
    pub fn hsl_components(self) -> (u16, u8, u8) {
        let channels = [self.red, self.green, self.blue].map(|value| f64::from(value) / 255.0);
        let [red, green, blue] = channels;
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let lightness = (max + min) / 2.0;
        let delta = max - min;
        if delta == 0.0 {
            return (0, 0, (lightness * 100.0).round() as u8);
        }
        let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
        let sector = if max == red {
            ((green - blue) / delta).rem_euclid(6.0)
        } else if max == green {
            (blue - red) / delta + 2.0
        } else {
            (red - green) / delta + 4.0
        };
        (
            ((sector * 60.0).round() as u16) % 360,
            (saturation * 100.0).round() as u8,
            (lightness * 100.0).round() as u8,
        )
    }
}

/// One decoded QR code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrPayload {
    Text(String),
    /// Binary or oversized content, kept as a size only.
    Binary {
        bytes: usize,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub enum ScreenToolResult {
    Text(String),
    Qr(Vec<QrPayload>),
    Color(ColorSample),
}

impl fmt::Debug for ScreenToolResult {
    // Recognized content stays out of logs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => write!(formatter, "Text({} bytes)", text.len()),
            Self::Qr(payloads) => write!(formatter, "Qr({} codes)", payloads.len()),
            Self::Color(_) => formatter.write_str("Color"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ScreenToolsPhase {
    #[default]
    Idle,
    /// The provider or picker is collecting the frame or color.
    Capturing(ScreenTool),
    /// A frame waits for the user's region or pixel.
    Selecting(ScreenTool),
    Processing(ScreenTool),
    Done(ScreenTool, ScreenToolResult),
    Failed(ScreenTool, CaptureError),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScreenToolsSnapshot {
    pub phase: ScreenToolsPhase,
    /// Advances on every phase change.
    pub generation: u64,
}

/// What the user chose in the selection view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Text or QR from this region; `None` uses the whole frame.
    Region(Option<Rect>),
    /// The color of this pixel.
    Pixel { x: u32, y: u32 },
}

/// The desktop's own color picker.
pub trait ColorPicker: Send {
    fn pick(&self, cancel: &CaptureCancel) -> Result<[u8; 3], CaptureError>;
}

impl ColorPicker for PortalColorPicker {
    fn pick(&self, cancel: &CaptureCancel) -> Result<[u8; 3], CaptureError> {
        PortalColorPicker::pick(self, cancel)
    }
}

/// Local text recognition for one PNG.
pub trait TextRecognizer: Send + Sync {
    fn recognize(&self, png: &[u8], cancel: &CaptureCancel) -> Result<String, CaptureError>;
}

/// Tesseract with the configured language and time limit.
pub struct TesseractRecognizer {
    pub adapter: TesseractAdapter,
    pub language: String,
    pub timeout: Duration,
}

impl TextRecognizer for TesseractRecognizer {
    fn recognize(&self, png: &[u8], cancel: &CaptureCancel) -> Result<String, CaptureError> {
        self.adapter
            .recognize(png, &self.language, self.timeout, cancel)
    }
}

/// Where a job's input comes from.
pub enum ScreenToolSource {
    Frame {
        backend: Box<dyn CaptureBackend>,
        mode: CaptureMode,
    },
    Picker(Box<dyn ColorPicker>),
}

enum Job {
    Acquire {
        id: u64,
        tool: ScreenTool,
        source: ScreenToolSource,
        cancel: CaptureCancel,
    },
    Process {
        id: u64,
        tool: ScreenTool,
        frame: Arc<RgbaImage>,
        region: Option<Rect>,
        cancel: CaptureCancel,
    },
}

struct State {
    snapshot: ScreenToolsSnapshot,
    /// Identifies the current job; results of replaced jobs are dropped.
    job: u64,
    frame: Option<Arc<RgbaImage>>,
    cancel: Option<CaptureCancel>,
    recognizer: Arc<dyn TextRecognizer>,
}

impl State {
    fn set_phase(&mut self, phase: ScreenToolsPhase) {
        self.snapshot.phase = phase;
        self.snapshot.generation = self.snapshot.generation.wrapping_add(1);
    }

    /// Ends the current job and releases its frame.
    fn reset(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.frame = None;
        self.job = self.job.wrapping_add(1);
        if self.snapshot.phase != ScreenToolsPhase::Idle {
            self.set_phase(ScreenToolsPhase::Idle);
        }
    }
}

pub struct ScreenToolsService {
    state: Arc<Mutex<State>>,
    jobs: Option<mpsc::Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl ScreenToolsService {
    pub fn new(recognizer: Arc<dyn TextRecognizer>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                snapshot: ScreenToolsSnapshot::default(),
                job: 0,
                frame: None,
                cancel: None,
                recognizer,
            })),
            jobs: None,
            worker: None,
        }
    }

    pub fn set_recognizer(&self, recognizer: Arc<dyn TextRecognizer>) {
        lock(&self.state).recognizer = recognizer;
    }

    pub fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.worker.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::channel();
        let state = Arc::clone(&self.state);
        let worker = thread::Builder::new()
            .name("kestrel-screen-tools".to_owned())
            .spawn(move || run_worker(&receiver, &state))
            .map_err(|error| format!("Starting the screen tools worker failed: {error}"))?;
        self.jobs = Some(sender);
        self.worker = Some(worker);
        Ok(())
    }

    /// Cancels any job, joins the worker, and drops frames and results.
    pub fn stop(&mut self) {
        lock(&self.state).reset();
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        lock(&self.state).reset();
    }

    /// Starts a job, replacing any current one.
    pub fn begin(&self, tool: ScreenTool, source: ScreenToolSource) -> Result<(), String> {
        let jobs = self
            .jobs
            .as_ref()
            .ok_or_else(|| "Screen tools are not running.".to_owned())?;
        let cancel = CaptureCancel::new();
        let id = {
            let mut state = lock(&self.state);
            state.reset();
            state.cancel = Some(cancel.clone());
            state.set_phase(ScreenToolsPhase::Capturing(tool));
            state.job
        };
        jobs.send(Job::Acquire {
            id,
            tool,
            source,
            cancel,
        })
        .map_err(|_| "The screen tools worker stopped.".to_owned())
    }

    /// The frame waiting for a selection.
    pub fn frame(&self) -> Option<Arc<RgbaImage>> {
        let state = lock(&self.state);
        matches!(state.snapshot.phase, ScreenToolsPhase::Selecting(_))
            .then(|| state.frame.clone())
            .flatten()
    }

    /// Hands the selection over; the service keeps no reference to the frame.
    pub fn select(&self, selection: Selection) -> Result<(), String> {
        let mut state = lock(&self.state);
        let ScreenToolsPhase::Selecting(tool) = state.snapshot.phase else {
            return Err("No screen selection is waiting.".to_owned());
        };
        let frame = state
            .frame
            .take()
            .ok_or_else(|| "The selected frame was released.".to_owned())?;
        match (tool, selection) {
            (ScreenTool::Color, Selection::Pixel { x, y }) => {
                let phase = match pixel(&frame, x, y) {
                    Some(color) => ScreenToolsPhase::Done(tool, ScreenToolResult::Color(color)),
                    None => ScreenToolsPhase::Failed(
                        tool,
                        CaptureError::new(
                            CaptureErrorKind::Failed,
                            "The chosen pixel is outside the capture.",
                        ),
                    ),
                };
                state.cancel = None;
                state.set_phase(phase);
                Ok(())
            }
            (ScreenTool::Text | ScreenTool::Qr, Selection::Region(region)) => {
                let jobs = self
                    .jobs
                    .as_ref()
                    .ok_or_else(|| "Screen tools are not running.".to_owned())?;
                let cancel = state.cancel.clone().unwrap_or_default();
                state.set_phase(ScreenToolsPhase::Processing(tool));
                let id = state.job;
                drop(state);
                jobs.send(Job::Process {
                    id,
                    tool,
                    frame,
                    region,
                    cancel,
                })
                .map_err(|_| "The screen tools worker stopped.".to_owned())
            }
            _ => {
                state.frame = Some(frame);
                Err("That selection does not fit this tool.".to_owned())
            }
        }
    }

    /// Cancels the current job and releases its frame.
    pub fn cancel(&self) {
        lock(&self.state).reset();
    }

    /// Clears a finished result.
    pub fn dismiss(&self) {
        let mut state = lock(&self.state);
        if matches!(
            state.snapshot.phase,
            ScreenToolsPhase::Done(..) | ScreenToolsPhase::Failed(..)
        ) {
            state.reset();
        }
    }

    pub fn snapshot(&self) -> ScreenToolsSnapshot {
        lock(&self.state).snapshot.clone()
    }
}

impl Drop for ScreenToolsService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run_worker(jobs: &mpsc::Receiver<Job>, state: &Mutex<State>) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Acquire {
                id,
                tool,
                source,
                cancel,
            } => {
                let outcome = match source {
                    ScreenToolSource::Frame { backend, mode } => backend
                        .capture(mode, &cancel)
                        .and_then(|image| {
                            decode_png(&image.png).map_err(|_| {
                                CaptureError::new(
                                    CaptureErrorKind::Failed,
                                    "The captured image could not be decoded.",
                                )
                            })
                        })
                        .map(|frame| Acquired::Frame(Arc::new(frame))),
                    ScreenToolSource::Picker(picker) => {
                        picker.pick(&cancel).map(|[red, green, blue]| {
                            Acquired::Color(ColorSample { red, green, blue })
                        })
                    }
                };
                let mut state = lock(state);
                if state.job != id {
                    continue;
                }
                match outcome {
                    Ok(Acquired::Frame(frame)) => {
                        state.frame = Some(frame);
                        state.set_phase(ScreenToolsPhase::Selecting(tool));
                    }
                    Ok(Acquired::Color(color)) => {
                        state.cancel = None;
                        state.set_phase(ScreenToolsPhase::Done(
                            tool,
                            ScreenToolResult::Color(color),
                        ));
                    }
                    Err(error) => finish_with_error(&mut state, tool, error),
                }
            }
            Job::Process {
                id,
                tool,
                frame,
                region,
                cancel,
            } => {
                let recognizer = Arc::clone(&lock(state).recognizer);
                let outcome = process(tool, &frame, region, recognizer.as_ref(), &cancel);
                drop(frame);
                let mut state = lock(state);
                if state.job != id {
                    continue;
                }
                state.cancel = None;
                match outcome {
                    Ok(result) => state.set_phase(ScreenToolsPhase::Done(tool, result)),
                    Err(error) => finish_with_error(&mut state, tool, error),
                }
            }
        }
    }
}

enum Acquired {
    Frame(Arc<RgbaImage>),
    Color(ColorSample),
}

fn finish_with_error(state: &mut State, tool: ScreenTool, error: CaptureError) {
    state.cancel = None;
    if error.kind == CaptureErrorKind::Cancelled {
        state.reset();
    } else {
        state.set_phase(ScreenToolsPhase::Failed(tool, error));
    }
}

fn process(
    tool: ScreenTool,
    frame: &RgbaImage,
    region: Option<Rect>,
    recognizer: &dyn TextRecognizer,
    cancel: &CaptureCancel,
) -> Result<ScreenToolResult, CaptureError> {
    let cropped;
    let image = match region {
        Some(region) => {
            cropped = crop_image(frame, region).map_err(|_| {
                CaptureError::new(
                    CaptureErrorKind::Failed,
                    "The selected area is outside the capture.",
                )
            })?;
            &cropped
        }
        None => frame,
    };
    match tool {
        ScreenTool::Text => {
            let png = encode_png(image).map_err(|_| {
                CaptureError::new(
                    CaptureErrorKind::Failed,
                    "The selected area could not be prepared for OCR.",
                )
            })?;
            recognizer
                .recognize(&png, cancel)
                .map(ScreenToolResult::Text)
        }
        ScreenTool::Qr => Ok(ScreenToolResult::Qr(decode_qr(image))),
        ScreenTool::Color => Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "Colors are sampled from a pixel.",
        )),
    }
}

fn pixel(frame: &RgbaImage, x: u32, y: u32) -> Option<ColorSample> {
    if x >= frame.width || y >= frame.height {
        return None;
    }
    let index = (y as usize * frame.width as usize + x as usize) * 4;
    let [red, green, blue, _] = frame.pixels.get(index..index + 4)?.try_into().ok()?;
    Some(ColorSample { red, green, blue })
}

/// Decodes every QR code in the image, bounded in count and size.
pub fn decode_qr(image: &RgbaImage) -> Vec<QrPayload> {
    let width = image.width as usize;
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, image.height as usize, |x, y| {
            let index = (y * width + x) * 4;
            let [red, green, blue] =
                [0, 1, 2].map(|offset| u32::from(image.pixels[index + offset]));
            ((red * 299 + green * 587 + blue * 114) / 1000) as u8
        });
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| {
            let mut bytes = Vec::new();
            grid.decode_to(&mut bytes).ok()?;
            Some(if bytes.len() > MAX_QR_PAYLOAD_BYTES {
                QrPayload::Binary { bytes: bytes.len() }
            } else {
                match String::from_utf8(bytes) {
                    Ok(text) => QrPayload::Text(text),
                    Err(error) => QrPayload::Binary {
                        bytes: error.into_bytes().len(),
                    },
                }
            })
        })
        .take(MAX_QR_PAYLOADS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_platform::capture::{CaptureProvider, CapturedImage};
    use std::time::Instant;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::new(width, height, rgba.repeat((width * height) as usize)).expect("image")
    }

    struct FakeBackend(Result<RgbaImage, CaptureErrorKind>);

    impl CaptureBackend for FakeBackend {
        fn provider(&self) -> CaptureProvider {
            CaptureProvider::X11
        }
        fn modes(&self) -> Vec<CaptureMode> {
            vec![CaptureMode::Screen]
        }
        fn capture(
            &self,
            _: CaptureMode,
            _: &CaptureCancel,
        ) -> Result<CapturedImage, CaptureError> {
            let image = self
                .0
                .clone()
                .map_err(|kind| CaptureError::new(kind, "fake"))?;
            Ok(CapturedImage {
                png: encode_png(&image).expect("encode"),
                width: image.width,
                height: image.height,
            })
        }
    }

    /// Reports the size of the PNG it was given.
    struct SizeRecognizer;

    impl TextRecognizer for SizeRecognizer {
        fn recognize(&self, png: &[u8], _: &CaptureCancel) -> Result<String, CaptureError> {
            let image = decode_png(png).expect("valid png");
            Ok(format!("{}x{}", image.width, image.height))
        }
    }

    struct FixedPicker([u8; 3]);

    impl ColorPicker for FixedPicker {
        fn pick(&self, _: &CaptureCancel) -> Result<[u8; 3], CaptureError> {
            Ok(self.0)
        }
    }

    fn service() -> ScreenToolsService {
        let mut service = ScreenToolsService::new(Arc::new(SizeRecognizer));
        service.start().expect("worker starts");
        service
    }

    fn frame_source(image: Result<RgbaImage, CaptureErrorKind>) -> ScreenToolSource {
        ScreenToolSource::Frame {
            backend: Box::new(FakeBackend(image)),
            mode: CaptureMode::Screen,
        }
    }

    fn wait_for(
        service: &ScreenToolsService,
        done: impl Fn(&ScreenToolsPhase) -> bool,
    ) -> ScreenToolsPhase {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let phase = service.snapshot().phase;
            if done(&phase) {
                return phase;
            }
            assert!(Instant::now() < deadline, "stuck in {phase:?}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn selecting(phase: &ScreenToolsPhase) -> bool {
        matches!(phase, ScreenToolsPhase::Selecting(_))
    }

    fn finished(phase: &ScreenToolsPhase) -> bool {
        matches!(
            phase,
            ScreenToolsPhase::Done(..) | ScreenToolsPhase::Failed(..) | ScreenToolsPhase::Idle
        )
    }

    #[test]
    fn colors_format_as_hex_rgb_and_hsl() {
        let color = |red, green, blue| ColorSample { red, green, blue };
        assert_eq!(color(255, 0, 0).hex(), "#ff0000");
        assert_eq!(color(255, 0, 0).hsl(), "hsl(0, 100%, 50%)");
        assert_eq!(color(0, 0, 0).hsl(), "hsl(0, 0%, 0%)");
        assert_eq!(color(255, 255, 255).hsl(), "hsl(0, 0%, 100%)");
        assert_eq!(color(128, 128, 128).hsl(), "hsl(0, 0%, 50%)");
        assert_eq!(color(51, 102, 204).rgb(), "rgb(51, 102, 204)");
        assert_eq!(color(51, 102, 204).hsl(), "hsl(220, 60%, 50%)");
        // Red-dominant colors with blue above green wrap to the top of the hue circle.
        assert_eq!(color(255, 0, 128).hsl(), "hsl(330, 100%, 50%)");
    }

    #[test]
    fn a_selected_region_is_cropped_before_recognition() {
        let service = service();
        service
            .begin(
                ScreenTool::Text,
                frame_source(Ok(solid(40, 30, [9, 9, 9, 255]))),
            )
            .expect("job starts");
        wait_for(&service, selecting);
        let frame = service.frame().expect("frame waits");
        assert_eq!((frame.width, frame.height), (40, 30));
        service
            .select(Selection::Region(Some(Rect {
                x: 5,
                y: 5,
                width: 10,
                height: 4,
            })))
            .expect("selection accepted");
        assert_eq!(service.frame(), None, "the service let go of the frame");
        let phase = wait_for(&service, finished);
        assert_eq!(
            phase,
            ScreenToolsPhase::Done(ScreenTool::Text, ScreenToolResult::Text("10x4".into()))
        );
    }

    #[test]
    fn colors_come_from_the_chosen_pixel_or_the_picker() {
        let mut image = solid(4, 4, [0, 0, 0, 255]);
        image.pixels[(2 * 4 + 3) * 4..(2 * 4 + 3) * 4 + 3].copy_from_slice(&[10, 20, 30]);
        let service = service();
        service
            .begin(ScreenTool::Color, frame_source(Ok(image)))
            .expect("job starts");
        wait_for(&service, selecting);
        assert!(service.select(Selection::Region(None)).is_err());
        service
            .select(Selection::Pixel { x: 3, y: 2 })
            .expect("pixel accepted");
        let color = ColorSample {
            red: 10,
            green: 20,
            blue: 30,
        };
        assert_eq!(
            service.snapshot().phase,
            ScreenToolsPhase::Done(ScreenTool::Color, ScreenToolResult::Color(color))
        );

        service
            .begin(
                ScreenTool::Color,
                ScreenToolSource::Picker(Box::new(FixedPicker([1, 2, 3]))),
            )
            .expect("picker starts");
        let phase = wait_for(&service, |phase| {
            matches!(phase, ScreenToolsPhase::Done(..))
        });
        assert_eq!(
            phase,
            ScreenToolsPhase::Done(
                ScreenTool::Color,
                ScreenToolResult::Color(ColorSample {
                    red: 1,
                    green: 2,
                    blue: 3
                })
            )
        );
    }

    #[test]
    fn cancelling_a_selection_releases_the_frame() {
        let service = service();
        service
            .begin(
                ScreenTool::Qr,
                frame_source(Ok(solid(8, 8, [1, 1, 1, 255]))),
            )
            .expect("job starts");
        wait_for(&service, selecting);
        let frame = service.frame().expect("frame waits");
        service.cancel();
        assert_eq!(Arc::strong_count(&frame), 1, "only the test still holds it");
        assert_eq!(service.snapshot().phase, ScreenToolsPhase::Idle);
        assert!(service.select(Selection::Region(None)).is_err());
    }

    #[test]
    fn dismissal_is_idle_and_denial_is_reported() {
        let service = service();
        service
            .begin(
                ScreenTool::Text,
                frame_source(Err(CaptureErrorKind::Cancelled)),
            )
            .expect("job starts");
        let phase = wait_for(&service, |phase| {
            !matches!(phase, ScreenToolsPhase::Capturing(_))
        });
        assert_eq!(phase, ScreenToolsPhase::Idle);

        service
            .begin(
                ScreenTool::Text,
                frame_source(Err(CaptureErrorKind::Denied)),
            )
            .expect("job starts");
        let phase = wait_for(&service, finished);
        assert!(matches!(
            phase,
            ScreenToolsPhase::Failed(
                ScreenTool::Text,
                CaptureError {
                    kind: CaptureErrorKind::Denied,
                    ..
                }
            )
        ));
        service.dismiss();
        assert_eq!(service.snapshot().phase, ScreenToolsPhase::Idle);
    }

    #[test]
    fn stopping_joins_the_worker_and_forgets_everything() {
        let mut service = service();
        service
            .begin(
                ScreenTool::Qr,
                frame_source(Ok(solid(8, 8, [1, 1, 1, 255]))),
            )
            .expect("job starts");
        wait_for(&service, selecting);
        service.stop();
        assert!(!service.is_running());
        assert_eq!(service.frame(), None);
        assert_eq!(service.snapshot().phase, ScreenToolsPhase::Idle);
        assert!(
            service
                .begin(ScreenTool::Qr, frame_source(Err(CaptureErrorKind::Failed)))
                .is_err()
        );
    }

    fn qr_image(payload: &[u8]) -> RgbaImage {
        let code = qrcode::QrCode::new(payload).expect("qr encodes");
        let modules = code.width();
        let colors = code.to_colors();
        let (scale, quiet) = (4, 4);
        let side = (modules + 2 * quiet) * scale;
        let mut pixels = Vec::with_capacity(side * side * 4);
        for y in 0..side {
            for x in 0..side {
                let (mx, my) = (x / scale, y / scale);
                let dark = (quiet..quiet + modules).contains(&mx)
                    && (quiet..quiet + modules).contains(&my)
                    && colors[(my - quiet) * modules + (mx - quiet)] == qrcode::Color::Dark;
                let value = if dark { 0 } else { 255 };
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        RgbaImage::new(side as u32, side as u32, pixels).expect("image")
    }

    #[test]
    fn qr_codes_decode_locally_and_binary_payloads_stay_opaque() {
        assert_eq!(
            decode_qr(&qr_image(b"https://example.org/kestrel")),
            [QrPayload::Text("https://example.org/kestrel".into())]
        );
        assert_eq!(
            decode_qr(&qr_image(&[0xff, 0xfe, 0x00, 0x81])),
            [QrPayload::Binary { bytes: 4 }]
        );
        assert!(decode_qr(&solid(32, 32, [255, 255, 255, 255])).is_empty());
    }
}
