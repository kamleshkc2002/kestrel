//! Screen tools: reading text, QR codes, and colors from the screen.
//!
//! Frames come from the screenshot providers in `capture`; text recognition
//! runs the `tesseract` executable, and colors may come from the desktop's
//! own picker. Probes never capture, pick, or run OCR.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus};

use crate::applications::resolve_executable;
use crate::capture::{
    CaptureCancel, CaptureDetection, CaptureError, CaptureErrorKind, CaptureProvider, run_bounded,
};

pub const TEXT_FEATURE_ID: &str = "capture.text";
pub const QR_FEATURE_ID: &str = "capture.qr";
pub const COLOR_FEATURE_ID: &str = "capture.color";

/// Largest recognized text kept from one OCR run.
pub const MAX_OCR_OUTPUT_BYTES: usize = 256 * 1024;
/// The first portal version with `PickColor`.
const PICK_COLOR_PORTAL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenTool {
    Text,
    Qr,
    Color,
}

pub const ALL_SCREEN_TOOLS: [ScreenTool; 3] = [ScreenTool::Text, ScreenTool::Qr, ScreenTool::Color];

impl ScreenTool {
    pub const fn feature_id(self) -> &'static str {
        match self {
            Self::Text => TEXT_FEATURE_ID,
            Self::Qr => QR_FEATURE_ID,
            Self::Color => COLOR_FEATURE_ID,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Read text",
            Self::Qr => "Read QR code",
            Self::Color => "Pick color",
        }
    }
}

/// Where a color sample comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSource {
    /// The desktop's `PickColor` picker, with its own magnifier.
    Picker,
    /// A captured frame the user clicks in Kestrel's selection view.
    Frame(CaptureProvider),
}

/// The color source a detection offers, if any.
pub fn color_source(detection: &CaptureDetection) -> Option<ColorSource> {
    let provider = detection.provider()?;
    if provider == CaptureProvider::Portal
        && detection
            .portal_version
            .is_some_and(|version| version >= PICK_COLOR_PORTAL_VERSION)
    {
        Some(ColorSource::Picker)
    } else {
        Some(ColorSource::Frame(provider))
    }
}

/// Capability of one screen tool from an existing detection; nothing is run.
pub fn screen_tool_capability(
    tool: ScreenTool,
    detection: &CaptureDetection,
    tesseract: Option<&Path>,
) -> CapabilityReport {
    let feature = tool.feature_id();
    let provider = detection.provider();
    let provider_evidence = CapabilityEvidence::new(
        "capture_provider",
        provider.map_or("absent", CaptureProvider::label),
    );
    let Some(provider) = provider else {
        return CapabilityReport::new(
            feature,
            CapabilityStatus::Unsupported {
                reason: "no screenshot provider is available".into(),
            },
            format!("{} needs a screenshot provider.", tool.label()),
        )
        .with_evidence(provider_evidence)
        .with_remediation(
            "Follow the setup steps shown for capture.screenshot in the Feature Hub.",
        );
    };
    let report = match tool {
        ScreenTool::Text => match tesseract {
            Some(_) => CapabilityReport::new(
                feature,
                CapabilityStatus::Supported,
                "Text is recognized locally by Tesseract.",
            )
            .with_selected_backend(format!("tesseract with {}", provider.label())),
            None => CapabilityReport::new(
                feature,
                CapabilityStatus::MissingDependency {
                    name: "tesseract".into(),
                },
                "Reading text needs Tesseract.",
            )
            .with_remediation(
                "Install Tesseract and a language pack, for example \
                 `sudo apt install tesseract-ocr tesseract-ocr-eng`, then refresh.",
            ),
        },
        ScreenTool::Qr => CapabilityReport::new(
            feature,
            CapabilityStatus::Supported,
            "QR codes are decoded locally.",
        )
        .with_selected_backend(format!("rqrr with {}", provider.label())),
        ScreenTool::Color => match color_source(detection) {
            Some(ColorSource::Picker) => CapabilityReport::new(
                feature,
                CapabilityStatus::Supported,
                "Colors come from the desktop's color picker.",
            )
            .with_selected_backend("Screenshot portal color picker"),
            _ => CapabilityReport::new(
                feature,
                CapabilityStatus::Supported,
                "Colors are sampled from a capture you click in.",
            )
            .with_selected_backend(provider.label()),
        },
    };
    report
        .with_evidence(provider_evidence)
        .with_evidence(CapabilityEvidence::new(
            "tesseract",
            if tesseract.is_some() {
                "present"
            } else {
                "absent"
            },
        ))
}

/// Runs `tesseract stdin stdout -l <language>` from an absolute path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TesseractAdapter {
    executable: Option<PathBuf>,
}

impl TesseractAdapter {
    /// Resolves `tesseract` on `PATH` without running it.
    pub fn discover() -> Self {
        Self {
            executable: resolve_executable("tesseract", env::var_os("PATH").as_deref()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_executable(executable: PathBuf) -> Self {
        Self {
            executable: Some(executable),
        }
    }

    pub fn executable(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    /// Recognizes text in one PNG; output beyond `MAX_OCR_OUTPUT_BYTES` is refused.
    pub fn recognize(
        &self,
        png: &[u8],
        language: &str,
        timeout: Duration,
        cancel: &CaptureCancel,
    ) -> Result<String, CaptureError> {
        let executable = self.executable.as_deref().ok_or_else(|| {
            CaptureError::new(CaptureErrorKind::Unavailable, "Tesseract is not installed.")
        })?;
        if !kestrel_core::valid_ocr_language(language) {
            return Err(CaptureError::new(
                CaptureErrorKind::Failed,
                "The OCR language setting is invalid.",
            ));
        }
        let output = run_bounded(
            executable,
            &["stdin", "stdout", "-l", language],
            Some(png.to_vec()),
            timeout,
            MAX_OCR_OUTPUT_BYTES,
            cancel,
        )
        .map_err(|error| match error.kind {
            CaptureErrorKind::TooLarge => CaptureError::new(
                CaptureErrorKind::TooLarge,
                "The recognized text exceeds the supported size.",
            ),
            CaptureErrorKind::Timeout => CaptureError::new(
                CaptureErrorKind::Timeout,
                "Tesseract did not finish in time; select a smaller area or raise the timeout.",
            ),
            _ => error,
        })?;
        if !output.status.success() {
            return Err(CaptureError::new(
                CaptureErrorKind::Failed,
                format!(
                    "Tesseract could not read the image; check that the `{language}` \
                     language pack is installed."
                ),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\n', '\r', ' ', '\u{c}'])
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_shortcuts::SessionKind;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;
    use tempfile::TempDir;

    fn script(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("tesseract");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn recognize(adapter: &TesseractAdapter, timeout: Duration) -> Result<String, CaptureError> {
        adapter.recognize(b"png bytes", "eng", timeout, &CaptureCancel::new())
    }

    #[test]
    fn tesseract_reads_stdin_and_returns_trimmed_stdout() {
        let dir = TempDir::new().expect("temp dir");
        let log = dir.path().join("args");
        let tesseract = script(
            &dir,
            &format!(
                "echo \"$@\" > '{}'; input=$(cat); printf 'read %s\\n\\f' \"$input\"",
                log.display()
            ),
        );
        let text = recognize(
            &TesseractAdapter::with_executable(tesseract),
            Duration::from_secs(5),
        )
        .expect("text recognized");
        assert_eq!(text, "read png bytes");
        assert_eq!(
            fs::read_to_string(log).expect("args").trim(),
            "stdin stdout -l eng"
        );
    }

    #[test]
    fn tesseract_failures_are_typed() {
        let dir = TempDir::new().expect("temp dir");
        let failing = TesseractAdapter::with_executable(script(&dir, "cat >/dev/null; exit 1"));
        assert_eq!(
            recognize(&failing, Duration::from_secs(5))
                .unwrap_err()
                .kind,
            CaptureErrorKind::Failed
        );

        let dir = TempDir::new().expect("temp dir");
        let flood = TesseractAdapter::with_executable(script(
            &dir,
            "cat >/dev/null; head -c 400000 /dev/zero",
        ));
        assert_eq!(
            recognize(&flood, Duration::from_secs(5)).unwrap_err().kind,
            CaptureErrorKind::TooLarge
        );

        let dir = TempDir::new().expect("temp dir");
        let stalled = TesseractAdapter::with_executable(script(&dir, "sleep 30"));
        let started = Instant::now();
        assert_eq!(
            recognize(&stalled, Duration::from_millis(200))
                .unwrap_err()
                .kind,
            CaptureErrorKind::Timeout
        );
        assert!(started.elapsed() < Duration::from_secs(5));

        assert_eq!(
            recognize(&TesseractAdapter::default(), Duration::from_secs(1))
                .unwrap_err()
                .kind,
            CaptureErrorKind::Unavailable
        );
    }

    #[test]
    fn invalid_languages_never_reach_tesseract() {
        let dir = TempDir::new().expect("temp dir");
        let marker = dir.path().join("ran");
        let adapter = TesseractAdapter::with_executable(script(
            &dir,
            &format!("touch '{}'", marker.display()),
        ));
        for language in ["", "-psm", "eng deu", "../eng"] {
            let error = adapter
                .recognize(
                    b"x",
                    language,
                    Duration::from_secs(1),
                    &CaptureCancel::new(),
                )
                .unwrap_err();
            assert_eq!(error.kind, CaptureErrorKind::Failed, "{language:?}");
        }
        assert!(!marker.exists());
        assert!(kestrel_core::valid_ocr_language("eng+deu"));
        assert!(kestrel_core::valid_ocr_language("chi_sim"));
    }

    fn detection(
        provider: Option<CaptureProvider>,
        portal_version: Option<u32>,
    ) -> CaptureDetection {
        let mut detection = CaptureDetection::new(SessionKind::X11);
        match provider {
            Some(CaptureProvider::Portal) => detection.portal_version = portal_version,
            Some(CaptureProvider::X11) => detection.x11_display = true,
            _ => {}
        }
        detection
    }

    #[test]
    fn colors_use_the_portal_picker_only_when_it_exists() {
        for (name, detection, expected) in [
            (
                "portal v2",
                detection(Some(CaptureProvider::Portal), Some(2)),
                Some(ColorSource::Picker),
            ),
            (
                "portal v1",
                detection(Some(CaptureProvider::Portal), Some(1)),
                Some(ColorSource::Frame(CaptureProvider::Portal)),
            ),
            (
                "x11",
                detection(Some(CaptureProvider::X11), None),
                Some(ColorSource::Frame(CaptureProvider::X11)),
            ),
            ("no provider", detection(None, None), None),
        ] {
            assert_eq!(color_source(&detection), expected, "{name}");
        }
    }

    #[test]
    fn each_tool_reports_its_own_gate() {
        let x11 = detection(Some(CaptureProvider::X11), None);
        let tesseract = Path::new("/usr/bin/tesseract");
        let status = |tool, detection: &CaptureDetection, tesseract| {
            screen_tool_capability(tool, detection, tesseract).status
        };
        for (tool, tesseract, expected) in [
            (
                ScreenTool::Text,
                None,
                CapabilityStatus::MissingDependency {
                    name: "tesseract".into(),
                },
            ),
            (ScreenTool::Qr, None, CapabilityStatus::Supported),
            (ScreenTool::Color, None, CapabilityStatus::Supported),
            (
                ScreenTool::Text,
                Some(tesseract),
                CapabilityStatus::Supported,
            ),
        ] {
            assert_eq!(status(tool, &x11, tesseract), expected, "{tool:?}");
        }

        let none = detection(None, None);
        for tool in ALL_SCREEN_TOOLS {
            assert!(matches!(
                status(tool, &none, Some(tesseract)),
                CapabilityStatus::Unsupported { .. }
            ));
        }
    }
}
