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
            now: SystemTime::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FeaturePreset;
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
        assert!(ApplicationRuntime::preset_enabled(
            FeaturePreset::Balanced,
            CAPTURE_ID
        ));
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
            assert!(ApplicationRuntime::preset_enabled(
                FeaturePreset::Balanced,
                tool.feature_id()
            ));
        }
    }
}
