//! Local camera preview through V4L2.
//!
//! Cameras are listed from sysfs without opening them; a device opens only
//! when a preview starts, and dropping the stream turns the camera off.

use std::ffi::CString;
use std::fmt;
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};
use linuxvideo::format::{PixFormat, PixelFormat};
use linuxvideo::stream::ReadStream;
use linuxvideo::{BufType, CapabilityFlags, Device};
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::CapabilityProbe;

pub const FEATURE_ID: &str = "camera.preview";

/// Most cameras listed.
pub const MAX_CAMERAS: usize = 16;
/// Requested preview size; drivers may pick the nearest they support.
pub const PREVIEW_WIDTH: u32 = 640;
pub const PREVIEW_HEIGHT: u32 = 480;
/// Largest frame accepted from a driver, so a camera cannot exhaust memory.
pub const MAX_FRAME_DIMENSION: u32 = 4096;
const MAX_NAME_CHARS: usize = 128;

/// One video capture node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraInfo {
    /// `/dev/videoN`.
    pub path: PathBuf,
    /// The driver's name for the camera, bounded.
    pub name: String,
}

/// Lists camera nodes under `sysfs` (normally `/sys/class/video4linux`).
///
/// Only index 0 nodes are listed: UVC cameras expose metadata on further
/// nodes of the same device.
pub fn list_cameras(sysfs: &Path, dev: &Path) -> Vec<CameraInfo> {
    let Ok(entries) = fs::read_dir(sysfs) else {
        return Vec::new();
    };
    let mut cameras = entries
        .flatten()
        .filter_map(|entry| {
            let node = entry.file_name();
            let node = node.to_str()?.to_owned();
            if !node.starts_with("video") {
                return None;
            }
            let index = fs::read_to_string(entry.path().join("index")).ok()?;
            if index.trim() != "0" {
                return None;
            }
            let name = fs::read_to_string(entry.path().join("name")).unwrap_or_default();
            let name = name.trim();
            let name = if name.is_empty() { node.as_str() } else { name };
            Some(CameraInfo {
                path: dev.join(&node),
                name: name.chars().take(MAX_NAME_CHARS).collect(),
            })
        })
        .collect::<Vec<_>>();
    cameras.sort_by(|left, right| left.path.cmp(&right.path));
    cameras.truncate(MAX_CAMERAS);
    cameras
}

/// The cameras on this system.
pub fn system_cameras() -> Vec<CameraInfo> {
    list_cameras(Path::new("/sys/class/video4linux"), Path::new("/dev"))
}

/// Whether this user may open `path` for reading and writing; nothing is opened.
pub fn can_open(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string; access(2) only checks permissions.
    unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

/// Capability from a camera list and the permission check, without opening a device.
pub fn camera_capability(
    cameras: &[CameraInfo],
    accessible: impl Fn(&Path) -> bool,
) -> CapabilityReport {
    let usable = cameras
        .iter()
        .filter(|camera| accessible(&camera.path))
        .count();
    let evidence = CapabilityEvidence::new("cameras", cameras.len().to_string());
    let report = if cameras.is_empty() {
        CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Unsupported {
                reason: "no camera was found".into(),
            },
            "No camera is connected.",
        )
        .with_remediation("Connect a camera, or check that its driver is loaded, then refresh.")
    } else if usable == 0 {
        CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::NeedsPermission {
                permission: Permission::Camera,
            },
            "This user may not open the camera.",
        )
        .with_remediation(
            "Log in at the computer's own seat, or add your user to the video group, then log in again.",
        )
    } else {
        CapabilityReport::new(
            FEATURE_ID,
            CapabilityStatus::Supported,
            "The camera opens only while a preview runs.",
        )
        .with_selected_backend("V4L2")
    };
    report.with_evidence(evidence)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CameraProbe;

impl CapabilityProbe for CameraProbe {
    fn probe(&self) -> CapabilityReport {
        camera_capability(&system_cameras(), can_open)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraErrorKind {
    /// The device node is gone.
    Unavailable,
    /// The user may not open the device.
    Denied,
    /// Another application holds the camera.
    Busy,
    /// The camera offers no format Kestrel can show.
    UnsupportedFormat,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraError {
    pub kind: CameraErrorKind,
    /// Kestrel-authored; carries no pixel data.
    pub message: String,
}

impl CameraError {
    pub fn new(kind: CameraErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn from_io(error: &io::Error, action: &str) -> Self {
        match error.raw_os_error() {
            Some(libc::ENOENT | libc::ENODEV | libc::ENXIO) => Self::new(
                CameraErrorKind::Unavailable,
                "The camera was disconnected; reconnect it and refresh.",
            ),
            Some(libc::EACCES | libc::EPERM) => Self::new(
                CameraErrorKind::Denied,
                "Access to the camera was denied; log in at the computer's own seat or join \
                 the video group.",
            ),
            Some(libc::EBUSY) => Self::new(
                CameraErrorKind::Busy,
                "Another application is using the camera; close it and try again.",
            ),
            _ => Self::new(CameraErrorKind::Failed, format!("{action} failed: {error}")),
        }
    }
}

impl fmt::Display for CameraError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CameraError {}

/// One decoded preview frame: 8-bit RGBA, row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl fmt::Debug for CameraFrame {
    // Pixel data stays out of logs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CameraFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// An open camera producing frames until dropped.
pub trait CameraStream: Send {
    /// The next frame, or `None` when none arrived within `timeout`.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame>, CameraError>;
}

/// Opens cameras when a preview starts.
pub trait CameraBackend: Send {
    fn open(&self, path: &Path) -> Result<Box<dyn CameraStream>, CameraError>;
}

/// The production V4L2 backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct V4l2Camera;

impl CameraBackend for V4l2Camera {
    fn open(&self, path: &Path) -> Result<Box<dyn CameraStream>, CameraError> {
        let device = Device::open(path)
            .map_err(|error| CameraError::from_io(&error, "Opening the camera"))?;
        let capabilities = device
            .capabilities()
            .map_err(|error| CameraError::from_io(&error, "Reading the camera capabilities"))?;
        let flags = capabilities.device_capabilities();
        if !flags.contains(CapabilityFlags::VIDEO_CAPTURE)
            || !flags.contains(CapabilityFlags::STREAMING)
        {
            return Err(CameraError::new(
                CameraErrorKind::UnsupportedFormat,
                "This video device does not stream camera frames.",
            ));
        }
        let offered = device
            .formats(BufType::VIDEO_CAPTURE)
            .filter_map(Result::ok)
            .map(|format| format.pixel_format())
            .collect::<Vec<_>>();
        let pixel_format = [PixelFormat::MJPG, PixelFormat::JPEG, PixelFormat::YUYV]
            .into_iter()
            .find(|format| offered.contains(format))
            .ok_or_else(|| {
                CameraError::new(
                    CameraErrorKind::UnsupportedFormat,
                    "The camera offers neither MJPEG nor YUYV frames.",
                )
            })?;
        let capture = device
            .video_capture(PixFormat::new(PREVIEW_WIDTH, PREVIEW_HEIGHT, pixel_format))
            .map_err(|error| CameraError::from_io(&error, "Choosing the camera format"))?;
        let format = capture.format();
        let (width, height, bytes_per_line, pixel_format) = (
            format.width(),
            format.height(),
            format.bytes_per_line(),
            format.pixel_format(),
        );
        if width == 0 || height == 0 || width > MAX_FRAME_DIMENSION || height > MAX_FRAME_DIMENSION
        {
            return Err(CameraError::new(
                CameraErrorKind::UnsupportedFormat,
                "The camera chose an unsupported frame size.",
            ));
        }
        let stream = capture
            .into_stream()
            .map_err(|error| CameraError::from_io(&error, "Starting the camera"))?;
        Ok(Box::new(V4l2Stream {
            stream,
            width,
            height,
            bytes_per_line,
            pixel_format,
        }))
    }
}

struct V4l2Stream {
    stream: ReadStream,
    width: u32,
    height: u32,
    bytes_per_line: u32,
    pixel_format: PixelFormat,
}

impl CameraStream for V4l2Stream {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame>, CameraError> {
        let mut poll = libc::pollfd {
            fd: self.stream.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd for an fd this stream owns.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(CameraError::from_io(&error, "Waiting for the camera"));
        }
        if ready == 0 {
            return Ok(None);
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(CameraError::new(
                CameraErrorKind::Unavailable,
                "The camera stopped; reconnect it and start the preview again.",
            ));
        }
        let (width, height, bytes_per_line, pixel_format) = (
            self.width,
            self.height,
            self.bytes_per_line,
            self.pixel_format,
        );
        self.stream
            .dequeue(|buffer| {
                if buffer.is_error() {
                    return Ok(None);
                }
                Ok(decode(&buffer, width, height, bytes_per_line, pixel_format))
            })
            .map_err(|error| CameraError::from_io(&error, "Reading a camera frame"))
    }
}

/// Decodes one buffer; corrupt frames are skipped.
fn decode(
    data: &[u8],
    width: u32,
    height: u32,
    bytes_per_line: u32,
    pixel_format: PixelFormat,
) -> Option<CameraFrame> {
    if pixel_format == PixelFormat::YUYV {
        yuyv_to_rgba(data, width, height, bytes_per_line)
    } else {
        jpeg_to_rgba(data)
    }
}

/// Decodes an MJPEG frame; webcams often omit Huffman tables, which the decoder supplies.
pub fn jpeg_to_rgba(data: &[u8]) -> Option<CameraFrame> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_FRAME_DIMENSION as usize)
        .set_max_height(MAX_FRAME_DIMENSION as usize);
    let mut decoder = JpegDecoder::new_with_options(data, options);
    let rgba = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    (rgba.len() == width as usize * height as usize * 4).then_some(CameraFrame {
        width,
        height,
        rgba,
    })
}

/// Converts packed YUYV 4:2:2 (BT.601, limited range) to RGBA.
pub fn yuyv_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    bytes_per_line: u32,
) -> Option<CameraFrame> {
    let (width, height) = (width as usize, height as usize);
    let stride = (bytes_per_line as usize).max(width * 2);
    if width % 2 != 0 || data.len() < stride * height.saturating_sub(1) + width * 2 {
        return None;
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let line = &data[row * stride..row * stride + width * 2];
        for &[y0, u, y1, v] in line.as_chunks::<4>().0 {
            rgba.extend_from_slice(&yuv_pixel(y0, u, v));
            rgba.extend_from_slice(&yuv_pixel(y1, u, v));
        }
    }
    Some(CameraFrame {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

fn yuv_pixel(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let clamp = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(298 * c + 409 * e),
        clamp(298 * c - 100 * d - 208 * e),
        clamp(298 * c + 516 * d),
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn node(sysfs: &Path, name: &str, index: &str, label: &str) {
        let dir = sysfs.join(name);
        fs::create_dir_all(&dir).expect("node dir");
        fs::write(dir.join("index"), format!("{index}\n")).expect("index");
        fs::write(dir.join("name"), format!("{label}\n")).expect("name");
    }

    #[test]
    fn only_primary_capture_nodes_are_listed_in_order() {
        let sysfs = TempDir::new().expect("sysfs");
        node(sysfs.path(), "video2", "0", "USB Camera");
        node(sysfs.path(), "video1", "1", "Integrated Camera");
        node(sysfs.path(), "video0", "0", "Integrated Camera");
        node(sysfs.path(), "vbi0", "0", "Teletext");
        let cameras = list_cameras(sysfs.path(), Path::new("/dev"));
        assert_eq!(
            cameras,
            [
                CameraInfo {
                    path: PathBuf::from("/dev/video0"),
                    name: "Integrated Camera".into(),
                },
                CameraInfo {
                    path: PathBuf::from("/dev/video2"),
                    name: "USB Camera".into(),
                },
            ]
        );
        assert!(list_cameras(&sysfs.path().join("missing"), Path::new("/dev")).is_empty());
    }

    #[test]
    fn capability_distinguishes_missing_denied_and_usable_cameras() {
        let camera = CameraInfo {
            path: PathBuf::from("/dev/video0"),
            name: "Camera".into(),
        };
        assert!(matches!(
            camera_capability(&[], |_| true).status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert_eq!(
            camera_capability(std::slice::from_ref(&camera), |_| false).status,
            CapabilityStatus::NeedsPermission {
                permission: Permission::Camera
            }
        );
        assert_eq!(
            camera_capability(&[camera], |_| true).status,
            CapabilityStatus::Supported
        );
    }

    #[test]
    fn yuyv_converts_with_padding_and_rejects_short_buffers() {
        // Two pixels per row, one byte of row padding: white then black, twice.
        let row = [235, 128, 16, 128, 0];
        let data = [row, row].concat();
        let frame = yuyv_to_rgba(&data, 2, 2, 5).expect("frame");
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(&frame.rgba[..8], &[255, 255, 255, 255, 0, 0, 0, 255]);
        assert_eq!(yuyv_to_rgba(&data[..6], 2, 2, 5), None);
        assert_eq!(yuyv_to_rgba(&data, 3, 1, 5), None, "odd widths cannot pair");
    }

    #[test]
    fn corrupt_jpeg_frames_are_skipped() {
        assert_eq!(jpeg_to_rgba(b"not a jpeg"), None);
        assert_eq!(jpeg_to_rgba(&[0xff, 0xd8, 0xff]), None);
    }

    #[test]
    fn opening_errors_are_typed() {
        let error = |code| CameraError::from_io(&io::Error::from_raw_os_error(code), "x").kind;
        assert_eq!(error(libc::ENOENT), CameraErrorKind::Unavailable);
        assert_eq!(error(libc::EACCES), CameraErrorKind::Denied);
        assert_eq!(error(libc::EBUSY), CameraErrorKind::Busy);
        assert_eq!(error(libc::EIO), CameraErrorKind::Failed);
        let missing = V4l2Camera
            .open(Path::new("/nonexistent/video0"))
            .err()
            .map(|error| error.kind);
        assert_eq!(missing, Some(CameraErrorKind::Unavailable));
    }
}
