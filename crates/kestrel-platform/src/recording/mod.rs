//! Screen recording: `org.freedesktop.portal.ScreenCast` sessions recorded
//! through GStreamer (`pipewiresrc` → VP8, optional Opus tracks → WebM).

mod export;
mod pipeline;
mod portal;

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::OwnedValue;

pub use export::{EXPORT_ELEMENTS, export_missing, export_range, frame_at, media_duration};

/// Reads and trims completed takes.
pub trait TakeEditor: Send + Sync {
    fn duration(&self, take: &Path) -> Result<Duration, RecordingError>;
    fn frame_at(&self, take: &Path, position: Duration) -> Result<VideoFrame, RecordingError>;
    /// Writes `start..end` of `take` to `output`; see [`export_range`].
    fn export(
        &self,
        take: &Path,
        start: Duration,
        end: Duration,
        output: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &dyn Fn(f64),
    ) -> Result<(), RecordingError>;
}

/// Takes read and re-encoded through GStreamer.
#[derive(Debug, Clone, Copy, Default)]
pub struct GstTakeEditor;

impl TakeEditor for GstTakeEditor {
    fn duration(&self, take: &Path) -> Result<Duration, RecordingError> {
        media_duration(take)
    }

    fn frame_at(&self, take: &Path, position: Duration) -> Result<VideoFrame, RecordingError> {
        frame_at(take, position)
    }

    fn export(
        &self,
        take: &Path,
        start: Duration,
        end: Duration,
        output: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &dyn Fn(f64),
    ) -> Result<(), RecordingError> {
        export_range(take, start, end, output, cancel, progress)
    }
}
pub use pipeline::gstreamer_missing;
pub use portal::{PortalScreenCast, ScreenCastSession};

use crate::CapabilityProbe;
use crate::audio::{AudioBackend, MicrophoneBackend, PulseAudioBackend};
use crate::capture::CaptureCancel;

pub const FEATURE_ID: &str = "capture.recording";
const BACKEND_LABEL: &str = "ScreenCast portal + GStreamer";

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// `AvailableSourceTypes` bits.
const SOURCE_MONITOR: u32 = 1;
const SOURCE_WINDOW: u32 = 2;
/// `AvailableCursorModes` bit for a cursor drawn into the frames.
pub(crate) const CURSOR_EMBEDDED: u32 = 2;
/// Elements every recording needs.
pub const VIDEO_ELEMENTS: [&str; 7] = [
    "pipewiresrc",
    "videoconvert",
    "videoscale",
    "videorate",
    "vp8enc",
    "webmmux",
    "filesink",
];
/// Area recordings crop a monitor stream.
pub const AREA_ELEMENT: &str = "videocrop";
/// Elements each audio track needs.
pub const AUDIO_ELEMENTS: [&str; 4] = ["pulsesrc", "audioconvert", "audioresample", "opusenc"];

/// What a recording captures; Area crops a monitor to a region chosen after consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordingSource {
    Screen,
    Window,
    Area,
}

impl RecordingSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Screen => "Screen",
            Self::Window => "Window",
            Self::Area => "Area",
        }
    }
}

/// An optional audio track, recorded separately from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AudioTrack {
    /// What the default output plays (its PulseAudio monitor).
    System,
    /// The default input.
    Microphone,
}

impl AudioTrack {
    pub const ALL: [Self; 2] = [Self::System, Self::Microphone];

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System audio",
            Self::Microphone => "Microphone",
        }
    }

    /// PulseAudio device the track reads.
    pub(crate) const fn device(self) -> &'static str {
        match self {
            Self::System => "@DEFAULT_MONITOR@",
            Self::Microphone => "@DEFAULT_SOURCE@",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingErrorKind {
    /// The user dismissed the desktop's sharing dialog.
    Cancelled,
    /// The desktop refused to share the screen.
    Denied,
    /// The desktop stopped sharing during the session.
    Revoked,
    /// The portal, PipeWire, or a GStreamer element is missing.
    Unavailable,
    /// The take could not be written.
    Storage,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingError {
    pub kind: RecordingErrorKind,
    /// Kestrel-authored and free of paths.
    pub message: String,
}

impl RecordingError {
    pub fn new(kind: RecordingErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// What the user can do next, when there is something.
    pub fn remediation(&self) -> Option<&'static str> {
        match self.kind {
            RecordingErrorKind::Denied => {
                Some("Allow screen sharing for Kestrel in the desktop's privacy settings.")
            }
            RecordingErrorKind::Revoked => Some("Start a new recording to share again."),
            RecordingErrorKind::Unavailable => Some(
                "Install xdg-desktop-portal with a ScreenCast backend, PipeWire, and the GStreamer \
                 PipeWire and Good plugins.",
            ),
            RecordingErrorKind::Storage => Some("Free disk space or check the recordings folder."),
            RecordingErrorKind::Cancelled
            | RecordingErrorKind::Timeout
            | RecordingErrorKind::Failed => None,
        }
    }
}

impl fmt::Display for RecordingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RecordingError {}

/// A region of the source frame, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One frame of the shared source, RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// How to record an opened source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingPlan {
    /// Written while recording; renamed by the caller once finished.
    pub output: PathBuf,
    /// Encoded frame size, even on both sides; the source is scaled into it.
    pub width: u32,
    pub height: u32,
    /// Size of the shared source's frames, which `crop` refers to.
    pub source_size: (u32, u32),
    pub crop: Option<CropRegion>,
    pub framerate: u32,
    pub audio: Vec<AudioTrack>,
}

/// Something that happened while recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingEvent {
    /// One audio track stopped; the rest of the recording continues.
    TrackFailed {
        track: AudioTrack,
        error: RecordingError,
    },
    /// The video source ended (for example, the shared window closed).
    SourceEnded,
    /// The recording cannot continue.
    Failed(RecordingError),
}

/// Opens consented capture sessions.
pub trait RecordingBackend: Send + Sync {
    /// Asks the desktop to share `source`; blocks until the user answers or
    /// `cancel` fires.
    fn open(
        &self,
        source: RecordingSource,
        cursor: bool,
        cancel: &CaptureCancel,
    ) -> Result<Box<dyn CaptureSession>, RecordingError>;
}

/// A shared source; dropping it ends the desktop session.
pub trait CaptureSession: Send {
    /// One current frame, for sizing and area selection.
    fn still(&mut self) -> Result<VideoFrame, RecordingError>;
    fn record(&mut self, plan: &RecordingPlan) -> Result<Box<dyn ActiveRecording>, RecordingError>;
    /// Whether the desktop ended the session.
    fn ended_by_desktop(&self) -> bool;
}

/// A running recording; dropping it without `finish` discards the output's tail.
pub trait ActiveRecording: Send {
    fn pause(&mut self) -> Result<(), RecordingError>;
    fn resume(&mut self) -> Result<(), RecordingError>;
    /// Waits up to `timeout` for the next event.
    fn poll(&mut self, timeout: Duration) -> Option<RecordingEvent>;
    /// Ends every track and completes the output file.
    fn finish(self: Box<Self>) -> Result<(), RecordingError>;
    /// Stops at once; the caller removes the output.
    fn abort(self: Box<Self>);
}

/// The ScreenCast portal recorded through GStreamer.
pub struct PortalRecordingBackend {
    portal: PortalScreenCast,
}

impl PortalRecordingBackend {
    pub fn new() -> Self {
        Self {
            portal: PortalScreenCast::new(),
        }
    }
}

impl Default for PortalRecordingBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingBackend for PortalRecordingBackend {
    fn open(
        &self,
        source: RecordingSource,
        cursor: bool,
        cancel: &CaptureCancel,
    ) -> Result<Box<dyn CaptureSession>, RecordingError> {
        if let Some(missing) = gstreamer_missing(source, &[]) {
            return Err(missing);
        }
        Ok(Box::new(self.portal.open(source, cursor, cancel)?))
    }
}

/// Audio sources observed through PulseAudio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioAvailability {
    Available,
    /// The server has no such device.
    Absent,
    /// No PulseAudio-compatible server answered.
    NoServer,
}

impl AudioAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Absent => "absent",
            Self::NoServer => "no server",
        }
    }
}

/// What the session offers for recording, observed without opening a dialog
/// or a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingDetection {
    /// The ScreenCast portal's `version`, when exported.
    pub portal_version: Option<u32>,
    pub source_types: u32,
    pub cursor_modes: u32,
    pub pipewire_socket: bool,
    /// Why GStreamer could not start, when it could not.
    pub gstreamer_error: Option<String>,
    /// Needed elements this GStreamer lacks.
    pub missing_elements: Vec<&'static str>,
    pub system_audio: AudioAvailability,
    pub microphone: AudioAvailability,
}

impl RecordingDetection {
    pub fn detect() -> Self {
        let bus = Builder::session()
            .and_then(|builder| builder.method_timeout(PROBE_TIMEOUT).build())
            .ok();
        let property = |name: &str| bus.as_ref().and_then(|bus| screencast_property(bus, name));
        let (gstreamer_error, missing_elements) = match pipeline::missing_elements() {
            Ok(missing) => (None, missing),
            Err(error) => (Some(error), Vec::new()),
        };
        let mut pulse = PulseAudioBackend::new();
        let system_audio = match pulse.discover() {
            Ok(discovery) if discovery.server.default_output_name.is_some() => {
                AudioAvailability::Available
            }
            Ok(_) => AudioAvailability::Absent,
            Err(_) => AudioAvailability::NoServer,
        };
        let microphone = match pulse.discover_inputs() {
            Ok(discovery) if !discovery.inputs.is_empty() => AudioAvailability::Available,
            Ok(_) => AudioAvailability::Absent,
            Err(_) => AudioAvailability::NoServer,
        };
        Self {
            portal_version: property("version"),
            source_types: property("AvailableSourceTypes").unwrap_or(0),
            cursor_modes: property("AvailableCursorModes").unwrap_or(0),
            pipewire_socket: pipewire_socket().is_some_and(|socket| socket.exists()),
            gstreamer_error,
            missing_elements,
            system_audio,
            microphone,
        }
    }

    /// Sources this session can record, in menu order.
    pub fn sources(&self) -> Vec<RecordingSource> {
        if self.portal_version.is_none() {
            return Vec::new();
        }
        let mut sources = Vec::new();
        if self.source_types & SOURCE_MONITOR != 0 {
            sources.push(RecordingSource::Screen);
        }
        if self.source_types & SOURCE_WINDOW != 0 {
            sources.push(RecordingSource::Window);
        }
        if self.source_types & SOURCE_MONITOR != 0 && !self.missing_elements.contains(&AREA_ELEMENT)
        {
            sources.push(RecordingSource::Area);
        }
        sources
    }

    /// Whether `track` can be offered.
    pub fn track_available(&self, track: AudioTrack) -> bool {
        let observed = match track {
            AudioTrack::System => self.system_audio,
            AudioTrack::Microphone => self.microphone,
        };
        observed == AudioAvailability::Available
            && !AUDIO_ELEMENTS
                .iter()
                .any(|element| self.missing_elements.contains(element))
    }

    pub fn cursor_available(&self) -> bool {
        self.cursor_modes & CURSOR_EMBEDDED != 0
    }

    fn missing_video(&self) -> Vec<&'static str> {
        VIDEO_ELEMENTS
            .iter()
            .copied()
            .filter(|element| self.missing_elements.contains(element))
            .collect()
    }

    pub fn capability(&self) -> CapabilityReport {
        let presence = |found: bool| if found { "present" } else { "absent" };
        let sources = self.sources();
        let evidence = [
            CapabilityEvidence::new(
                "screencast_portal_version",
                self.portal_version
                    .map_or_else(|| "absent".to_owned(), |version| version.to_string()),
            ),
            CapabilityEvidence::new(
                "sources",
                if sources.is_empty() {
                    "none".to_owned()
                } else {
                    sources
                        .iter()
                        .map(|source| source.label().to_lowercase())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ),
            CapabilityEvidence::new("cursor_capture", presence(self.cursor_available())),
            CapabilityEvidence::new("pipewire_socket", presence(self.pipewire_socket)),
            CapabilityEvidence::new(
                "gstreamer",
                if self.gstreamer_error.is_some() {
                    "failed to start".to_owned()
                } else if self.missing_elements.is_empty() {
                    "complete".to_owned()
                } else {
                    format!("missing {}", self.missing_elements.join(", "))
                },
            ),
            CapabilityEvidence::new("system_audio", self.system_audio.label()),
            CapabilityEvidence::new("microphone", self.microphone.label()),
        ];
        let missing_video = self.missing_video();
        let report = if self.portal_version.is_none() {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "This session exports no ScreenCast portal.".to_owned(),
                },
                "Screen recording is unavailable in this session.",
            )
            .with_remediation(
                "Install the desktop's xdg-desktop-portal backend with ScreenCast support.",
            )
        } else if let Some(error) = &self.gstreamer_error {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: format!("GStreamer could not start: {error}"),
                },
                "Screen recording needs GStreamer.",
            )
            .with_remediation("Install GStreamer 1.x with its Base and Good plugins.")
        } else if !missing_video.is_empty() {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: format!("GStreamer lacks {}.", missing_video.join(", ")),
                },
                "Screen recording needs more GStreamer plugins.",
            )
            .with_remediation(element_remediation(&missing_video))
        } else if !self.pipewire_socket {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "PipeWire is not running in this session.".to_owned(),
                },
                "Screen recording needs PipeWire.",
            )
            .with_remediation("Start PipeWire for your user session.")
        } else if sources.is_empty() {
            CapabilityReport::new(
                FEATURE_ID,
                CapabilityStatus::Unsupported {
                    reason: "The ScreenCast portal offers no monitor or window sources.".to_owned(),
                },
                "Screen recording is unavailable in this session.",
            )
        } else {
            let unavailable: Vec<&str> = AudioTrack::ALL
                .into_iter()
                .filter(|track| !self.track_available(*track))
                .map(AudioTrack::label)
                .collect();
            if unavailable.is_empty() {
                CapabilityReport::new(
                    FEATURE_ID,
                    CapabilityStatus::Supported,
                    "Recordings run through the desktop's ScreenCast portal, with optional system \
                     audio and microphone tracks.",
                )
                .with_selected_backend(BACKEND_LABEL)
            } else {
                let missing_audio: Vec<&'static str> = AUDIO_ELEMENTS
                    .iter()
                    .copied()
                    .filter(|element| self.missing_elements.contains(element))
                    .collect();
                let remediation = if missing_audio.is_empty() {
                    "Start a PulseAudio-compatible server (pipewire-pulse) with an output and an \
                     input device."
                        .to_owned()
                } else {
                    element_remediation(&missing_audio)
                };
                CapabilityReport::new(
                    FEATURE_ID,
                    CapabilityStatus::Limited {
                        reason: format!("{} unavailable.", unavailable.join(" and ")),
                    },
                    "Recordings run through the desktop's ScreenCast portal.",
                )
                .with_remediation(remediation)
                .with_selected_backend(BACKEND_LABEL)
            }
        };
        evidence
            .into_iter()
            .fold(report, |report, item| report.with_evidence(item))
    }
}

/// Bytes an unprivileged user can still write on the filesystem holding `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` fills caller-owned storage from a NUL-terminated path.
    let stats = unsafe {
        let mut stats: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(path.as_ptr(), &mut stats) == 0).then_some(stats)
    }?;
    Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

/// Names the plugin set each missing element ships in.
fn element_remediation(missing: &[&str]) -> String {
    let mut packages: Vec<&str> = missing
        .iter()
        .map(|element| match *element {
            "pipewiresrc" => "the GStreamer PipeWire plugin (gstreamer1.0-pipewire)",
            "videoconvert" | "videoscale" | "videorate" | "audioconvert" | "audioresample"
            | "opusenc" => "GStreamer Base plugins (gstreamer1.0-plugins-base)",
            _ => "GStreamer Good plugins (gstreamer1.0-plugins-good)",
        })
        .collect();
    packages.sort_unstable();
    packages.dedup();
    format!("Install {}.", packages.join(" and "))
}

fn screencast_property(bus: &Connection, name: &str) -> Option<u32> {
    let reply = bus
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.portal.ScreenCast", name),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}

/// The PipeWire socket libpipewire would connect to.
fn pipewire_socket() -> Option<PathBuf> {
    let name = env::var_os("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
    let name = Path::new(&name);
    if name.is_absolute() {
        return Some(name.to_path_buf());
    }
    let directory =
        env::var_os("PIPEWIRE_RUNTIME_DIR").or_else(|| env::var_os("XDG_RUNTIME_DIR"))?;
    Some(Path::new(&directory).join(name))
}

/// Read-only probe for `capture.recording`; opens no dialog and no stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecordingProbe;

impl CapabilityProbe for RecordingProbe {
    fn probe(&self) -> CapabilityReport {
        RecordingDetection::detect().capability()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(change: impl FnOnce(&mut RecordingDetection)) -> RecordingDetection {
        let mut detection = RecordingDetection {
            portal_version: Some(5),
            source_types: SOURCE_MONITOR | SOURCE_WINDOW,
            cursor_modes: 7,
            pipewire_socket: true,
            gstreamer_error: None,
            missing_elements: Vec::new(),
            system_audio: AudioAvailability::Available,
            microphone: AudioAvailability::Available,
        };
        change(&mut detection);
        detection
    }

    #[test]
    fn sources_follow_portal_types_and_the_crop_element() {
        use RecordingSource::{Area, Screen, Window};
        assert_eq!(detection(|_| {}).sources(), [Screen, Window, Area]);
        assert_eq!(
            detection(|d| d.source_types = SOURCE_WINDOW).sources(),
            [Window]
        );
        assert_eq!(
            detection(|d| d.missing_elements = vec![AREA_ELEMENT]).sources(),
            [Screen, Window]
        );
        assert!(detection(|d| d.portal_version = None).sources().is_empty());
    }

    #[test]
    fn capability_names_the_first_missing_requirement() {
        let status = |d: RecordingDetection| d.capability().status;
        assert!(matches!(
            status(detection(|d| d.portal_version = None)),
            CapabilityStatus::Unsupported { reason } if reason.contains("ScreenCast portal")
        ));
        let report = detection(|d| d.missing_elements = vec!["pipewiresrc", "vp8enc"]).capability();
        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        let remediation = report.remediation.expect("remediation");
        assert!(
            remediation.contains("gstreamer1.0-pipewire"),
            "{remediation}"
        );
        assert!(
            remediation.contains("gstreamer1.0-plugins-good"),
            "{remediation}"
        );
        assert!(matches!(
            status(detection(|d| d.pipewire_socket = false)),
            CapabilityStatus::Unsupported { reason } if reason.contains("PipeWire")
        ));
        assert!(matches!(
            status(detection(|d| d.gstreamer_error = Some("no registry".into()))),
            CapabilityStatus::Unsupported { .. }
        ));
        assert_eq!(status(detection(|_| {})), CapabilityStatus::Supported);
    }

    #[test]
    fn missing_audio_limits_recording_per_track() {
        let no_microphone = detection(|d| d.microphone = AudioAvailability::Absent);
        assert!(no_microphone.track_available(AudioTrack::System));
        assert!(!no_microphone.track_available(AudioTrack::Microphone));
        assert!(matches!(
            no_microphone.capability().status,
            CapabilityStatus::Limited { reason } if reason == "Microphone unavailable."
        ));
        let no_opus = detection(|d| d.missing_elements = vec!["opusenc"]);
        assert!(!no_opus.track_available(AudioTrack::System));
        let report = no_opus.capability();
        assert!(matches!(report.status, CapabilityStatus::Limited { .. }));
        assert!(
            report
                .remediation
                .expect("remediation")
                .contains("plugins-base")
        );
    }
}
