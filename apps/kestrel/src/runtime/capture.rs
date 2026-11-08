//! Capture and screen-tool runtime features.

use super::*;

pub(super) fn capture_directory() -> Option<PathBuf> {
    crate::snippets::kestrel_data_directory().map(|directory| directory.join("captures"))
}

impl ApplicationRuntime {
    pub fn capture_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == CAPTURE_ID && registration.running)
    }

    fn require_capture(&self) -> Result<(), String> {
        if !self.capture_is_running() {
            return Err(
                "Screenshots are not running; enable capture.screenshot in the Feature Hub."
                    .to_owned(),
            );
        }
        if !self.capture_storage {
            return Err(
                "No data directory is available for captures; set XDG_DATA_HOME or HOME."
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn capture_provider(&self) -> (Option<CaptureProvider>, Vec<CaptureMode>) {
        self.capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|detection| (detection.provider(), detection.modes()))
            .unwrap_or_default()
    }

    /// Starts a capture; `None` picks the provider's preferred mode.
    pub fn begin_capture(&mut self, mode: Option<CaptureMode>) -> Result<(), String> {
        self.require_capture()?;
        let detection = self
            .capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                "Screenshot providers have not been probed yet; refresh capabilities.".to_owned()
            })?;
        let provider = detection.provider().ok_or_else(|| {
            "No screenshot provider is available; the Feature Hub lists setup steps.".to_owned()
        })?;
        let mode = match mode {
            Some(mode) => mode,
            None => *detection
                .modes()
                .first()
                .ok_or_else(|| "The screenshot provider offers no capture mode.".to_owned())?,
        };
        self.capture
            .begin(capture_backend_for(provider, &detection), mode)
            .map_err(|error| error.to_string())
    }

    pub fn cancel_capture(&self) {
        self.capture.cancel();
    }

    pub fn delete_capture(&mut self, id: CaptureId) -> Result<(), String> {
        self.require_capture()?;
        self.capture.delete(id);
        Ok(())
    }

    pub fn clear_captures(&mut self) -> Result<(), String> {
        self.require_capture()?;
        self.capture.clear().map_err(|error| error.to_string())
    }

    pub fn capture_png(&self, id: CaptureId) -> Result<Vec<u8>, String> {
        self.require_capture()?;
        self.capture.read_png(id).map_err(|error| error.to_string())
    }

    /// Why the recent captures cannot offer text recognition, when Tesseract
    /// is missing; the same remedy the screen tools show.
    pub fn capture_ocr_unavailable(&self) -> Option<String> {
        TesseractAdapter::discover()
            .executable()
            .is_none()
            .then(|| kestrel_platform::screen_tools::TESSERACT_REMEDIATION.to_owned())
    }

    /// Text recognized in one stored capture, trimmed; `None` when it holds
    /// no text. Neither the pixels nor the text are retained.
    pub fn capture_text(&self, id: CaptureId) -> Result<Option<String>, String> {
        self.require_capture()?;
        if let Some(reason) = self.capture_ocr_unavailable() {
            return Err(reason);
        }
        stored_capture_text(&self.capture, self.clipboard_ocr.as_ref(), id)
    }

    /// The stored PNG's path, when the capture service still has it.
    pub fn capture_path(&self, id: CaptureId) -> Option<PathBuf> {
        self.capture.path(id)
    }

    pub fn export_capture(&self, id: CaptureId, destination: &Path) -> Result<(), String> {
        self.require_capture()?;
        self.capture
            .export(id, destination)
            .map_err(|error| error.to_string())
    }

    /// Stores the edited image as a new capture; the source stays.
    pub fn save_capture_edit(&mut self, id: CaptureId, plan: &EditPlan) -> Result<(), String> {
        self.require_capture()?;
        self.capture
            .save_edit(id, plan)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Applies the age bound; cheap when nothing expired.
    pub fn prune_captures(&mut self) {
        if self.capture_is_running() {
            self.capture.prune();
        }
    }

    pub fn capture_snapshot(&self) -> CaptureSnapshot {
        self.capture.snapshot()
    }

    pub fn screen_tool_is_running(&self, tool: ScreenTool) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == tool.feature_id() && registration.running
        })
    }

    pub fn screen_tools_snapshot(&self) -> ScreenToolsSnapshot {
        self.screen_tools.snapshot()
    }

    pub fn screen_tools_view_model(&self) -> ScreenToolsViewModel {
        let gates = ALL_SCREEN_TOOLS
            .into_iter()
            .map(|tool| {
                let registration = self
                    .registry
                    .registrations()
                    .find(|registration| registration.feature.id == tool.feature_id());
                let running = registration.is_some_and(|registration| registration.running);
                let reason = match registration {
                    _ if running => None,
                    Some(registration) if !registration.enabled => {
                        Some(format!("Enable {} in the Feature Hub.", tool.feature_id()))
                    }
                    Some(registration) => Some(
                        registration
                            .capability
                            .remediation
                            .clone()
                            .unwrap_or_else(|| registration.capability.summary.clone()),
                    ),
                    None => Some(format!("{} is not registered.", tool.feature_id())),
                };
                ScreenToolGate {
                    tool,
                    running,
                    reason,
                }
            })
            .collect();
        ScreenToolsViewModel::from_presentation(ScreenToolsPresentation {
            snapshot: self.screen_tools.snapshot(),
            gates,
            settings: self.screen_tool_settings.clone(),
        })
    }

    /// Captures a frame for `tool`, or opens the desktop's color picker.
    pub fn begin_screen_tool(&self, tool: ScreenTool) -> Result<(), String> {
        if !self.screen_tool_is_running(tool) {
            return Err(format!(
                "{} is not running; enable {} in the Feature Hub.",
                tool.label(),
                tool.feature_id()
            ));
        }
        let detection = self
            .capture_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                "Screenshot providers have not been probed yet; refresh capabilities.".to_owned()
            })?;
        let source = match (tool, color_source(&detection)) {
            (ScreenTool::Color, Some(ColorSource::Picker)) => {
                ScreenToolSource::Picker(Box::new(PortalColorPicker::new()))
            }
            _ => {
                let provider = detection.provider().ok_or_else(|| {
                    "No screenshot provider is available; the Feature Hub lists setup steps."
                        .to_owned()
                })?;
                let modes = detection.modes();
                // Prefer a provider-side selection; the selection view refines it.
                let mode = [CaptureMode::Area, CaptureMode::Interactive]
                    .into_iter()
                    .find(|mode| modes.contains(mode))
                    .unwrap_or(CaptureMode::Screen);
                ScreenToolSource::Frame {
                    backend: capture_backend_for(provider, &detection),
                    mode,
                }
            }
        };
        self.screen_tools.begin(tool, source)
    }

    /// The frame waiting in the selection view.
    pub fn screen_tool_frame(&self) -> Option<Arc<kestrel_services::capture::RgbaImage>> {
        self.screen_tools.frame()
    }

    pub fn select_screen_region(&self, selection: Selection) -> Result<(), String> {
        self.screen_tools.select(selection)
    }

    pub fn cancel_screen_tool(&self) {
        self.screen_tools.cancel();
    }

    pub fn dismiss_screen_tool(&self) {
        self.screen_tools.dismiss();
    }

    pub fn capture_view_model(&self) -> CaptureViewModel {
        CaptureViewModel::from_presentation(self.capture_presentation())
    }

    pub(super) fn capture_presentation(&self) -> CapturePresentation {
        let snapshot = self.capture.snapshot();
        let thumbnails = snapshot
            .entries
            .iter()
            .map(|entry| self.capture.thumbnail_path(entry.id))
            .collect();
        let (provider, modes) = self.capture_provider();
        CapturePresentation {
            snapshot,
            running: self.capture_is_running(),
            provider,
            modes,
            thumbnails,
            limits: self.capture_limits,
            ocr_unavailable: self.capture_ocr_unavailable(),
            now: SystemTime::now(),
        }
    }
}

/// Text from one capture's stored PNG; `None` when it holds no text.
fn stored_capture_text(
    capture: &CaptureService,
    recognizer: &dyn TextRecognizer,
    id: CaptureId,
) -> Result<Option<String>, String> {
    let png = capture.read_png(id).map_err(|error| error.to_string())?;
    kestrel_services::screen_tools::image_text(
        recognizer,
        &png,
        &kestrel_platform::capture::CaptureCancel::new(),
    )
    .map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FeaturePreset;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn tesseract_script(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("tesseract");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn capture_with_png(directory: &Path) -> (CaptureService, CaptureId) {
        use kestrel_services::capture::{
            CaptureHistory, CapturePolicy, NewCapture,
            image::{RgbaImage, encode_png},
        };
        let policy =
            CapturePolicy::from_configuration(&kestrel_core::CaptureConfiguration::default());
        let mut history =
            CaptureHistory::open(directory.to_path_buf(), policy).expect("history opens");
        let png = encode_png(&RgbaImage::new(4, 3, vec![255; 48]).expect("valid image"))
            .expect("png encodes");
        let entry = history
            .insert(
                &png,
                NewCapture {
                    mode: CaptureMode::Area,
                    edited: false,
                    width: 4,
                    height: 3,
                },
                SystemTime::now(),
            )
            .expect("capture stores");
        drop(history);
        let mut service = CaptureService::new(policy, directory.to_path_buf());
        service.start().expect("capture service starts");
        (service, entry.id)
    }

    fn recognizer(executable: PathBuf) -> TesseractRecognizer {
        TesseractRecognizer {
            adapter: TesseractAdapter::with_executable(executable),
            language: "eng".to_owned(),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn saved_capture_text_is_read_empty_or_typed() {
        let dir = TempDir::new().expect("temp dir");
        let (service, id) = capture_with_png(&dir.path().join("captures"));

        let text = recognizer(tesseract_script(
            &dir,
            "cat >/dev/null; printf 'saved words\\n\\f'",
        ));
        assert_eq!(
            stored_capture_text(&service, &text, id).expect("text read"),
            Some("saved words".to_owned())
        );

        let empty = recognizer(tesseract_script(&dir, "cat >/dev/null; printf ' \\n\\f'"));
        assert_eq!(
            stored_capture_text(&service, &empty, id).expect("empty read"),
            None
        );

        let stalled = TesseractRecognizer {
            timeout: Duration::from_millis(100),
            ..recognizer(tesseract_script(&dir, "sleep 30"))
        };
        assert!(
            stored_capture_text(&service, &stalled, id)
                .expect_err("a stalled read fails")
                .contains("did not finish in time")
        );

        let missing = TesseractRecognizer {
            adapter: TesseractAdapter::default(),
            ..recognizer(PathBuf::new())
        };
        assert!(
            stored_capture_text(&service, &missing, id)
                .expect_err("a missing tesseract fails")
                .contains("Tesseract is not installed")
        );
    }

    #[test]
    fn disabled_screenshots_refuse_every_capture_action() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert!(!runtime.capture_is_running());
        let error = runtime
            .begin_capture(None)
            .expect_err("a disabled feature never captures");
        assert!(error.contains(CAPTURE_ID), "{error}");
        assert!(runtime.clear_captures().is_err());
        assert!(runtime.capture_snapshot().entries.is_empty());
        assert!(
            FeaturePreset::Balanced.enables(CAPTURE_ID),
            "Balanced turns screenshots on"
        );
    }
    #[test]
    fn disabled_screen_tools_refuse_jobs_and_keep_no_worker() {
        use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        let error = runtime
            .begin_screen_tool(ScreenTool::Text)
            .expect_err("a disabled tool never captures");
        assert!(error.contains("capture.text"), "{error}");
        assert!(!runtime.screen_tools.is_running());
        let view = runtime.screen_tools_view_model();
        assert!(view.buttons.iter().all(|button| !button.enabled));
        assert!(view.buttons.iter().all(|button| button.reason.is_some()));
        for tool in ALL_SCREEN_TOOLS {
            assert!(FeaturePreset::Balanced.enables(tool.feature_id()));
        }
    }
}
