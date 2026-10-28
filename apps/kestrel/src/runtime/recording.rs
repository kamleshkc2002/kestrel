//! Camera and recording runtime features.

use super::*;

impl ApplicationRuntime {
    pub fn camera_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == CAMERA_ID && registration.running)
    }

    pub fn camera_view_model(&self) -> CameraViewModel {
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == CAMERA_ID);
        let running = registration.is_some_and(|registration| registration.running);
        let reason = registration.filter(|_| !running).map(|registration| {
            if registration.enabled {
                registration
                    .capability
                    .remediation
                    .clone()
                    .unwrap_or_else(|| registration.capability.summary.clone())
            } else {
                "Camera preview is off. Enable camera.preview in the Feature Hub.".to_owned()
            }
        });
        CameraViewModel::from_presentation(CameraPresentation {
            running,
            reason,
            snapshot: self.camera.snapshot(),
            cameras: if running {
                system_cameras()
            } else {
                Vec::new()
            },
        })
    }

    pub fn camera_generation(&self) -> u64 {
        self.camera.snapshot().generation
    }

    /// The slot the window reads preview frames from.
    pub fn camera_frames(&self) -> CameraFrameSlot {
        self.camera.frames()
    }

    /// Opens the camera at `path`; only listed cameras are accepted.
    pub fn start_camera(&mut self, path: &std::path::Path) -> Result<(), String> {
        if !self.camera_is_running() {
            return Err(
                "Camera preview is not running; enable camera.preview in the Feature Hub."
                    .to_owned(),
            );
        }
        let camera = system_cameras()
            .into_iter()
            .find(|camera| camera.path == path)
            .ok_or_else(|| "That camera is no longer connected; refresh the list.".to_owned())?;
        self.camera.start(camera.path, camera.name)
    }

    pub fn stop_camera(&mut self) {
        self.camera.stop();
    }

    pub fn recording_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == RECORDING_ID && registration.running)
    }

    pub fn recording_generation(&self) -> u64 {
        self.recording.snapshot().generation
    }

    pub fn recording_view_model(&self) -> RecordingViewModel {
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == RECORDING_ID);
        let running = registration.is_some_and(|registration| registration.running);
        let reason = registration.filter(|_| !running).map(|registration| {
            if registration.enabled {
                registration
                    .capability
                    .remediation
                    .clone()
                    .unwrap_or_else(|| registration.capability.summary.clone())
            } else {
                "Screen recording is off. Enable capture.recording in the Feature Hub.".to_owned()
            }
        });
        let detection = self
            .recording_detection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        RecordingViewModel::from_presentation(RecordingPresentation {
            running,
            reason,
            snapshot: self.recording.snapshot(),
            sources: detection
                .as_ref()
                .map(RecordingDetection::sources)
                .unwrap_or_default(),
            available_tracks: AudioTrack::ALL
                .into_iter()
                .filter(|track| {
                    detection
                        .as_ref()
                        .is_some_and(|detection| detection.track_available(*track))
                })
                .collect(),
            cursor_available: detection
                .as_ref()
                .is_some_and(RecordingDetection::cursor_available),
            settings: self.recording_settings,
            folder: self.recording.folder().to_path_buf(),
        })
    }

    /// The frame to choose an area from, while one is being chosen.
    pub fn recording_area_frame(&self) -> Option<Arc<kestrel_services::capture::image::RgbaImage>> {
        match self.recording.snapshot().phase {
            RecordingPhase::ChoosingArea { frame } => Some(frame),
            _ => None,
        }
    }

    pub fn recording_request(&mut self, request: crate::RecordingRequest) -> Result<(), String> {
        use crate::RecordingRequest;
        if !self.recording_is_running() {
            return Err(
                "Screen recording is not running; enable capture.recording in the Feature Hub."
                    .to_owned(),
            );
        }
        match request {
            RecordingRequest::StartDefault => {
                let detection = self
                    .recording_detection
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| "Recording support has not been checked yet.".to_owned())?;
                let source = detection
                    .sources()
                    .into_iter()
                    .find(|source| *source == kestrel_platform::recording::RecordingSource::Screen)
                    .or_else(|| detection.sources().into_iter().next())
                    .ok_or_else(|| {
                        "No recording source is available in this desktop session.".to_owned()
                    })?;
                self.recording_request(RecordingRequest::Start(source))
            }
            RecordingRequest::Start(source) => {
                let detection = self
                    .recording_detection
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| "Recording support has not been checked yet.".to_owned())?;
                if !detection.sources().contains(&source) {
                    return Err(format!(
                        "This desktop cannot record a {}.",
                        source.label().to_lowercase()
                    ));
                }
                let settings = self.recording_settings;
                let audio = AudioTrack::ALL
                    .into_iter()
                    .filter(|track| match track {
                        AudioTrack::System => settings.system_audio,
                        AudioTrack::Microphone => settings.microphone,
                    })
                    .filter(|track| detection.track_available(*track))
                    .collect();
                self.recording.start(RecordingOptions {
                    source,
                    cursor: settings.cursor && detection.cursor_available(),
                    framerate: settings.framerate,
                    audio,
                    system_audio: kestrel_platform::recording::SystemAudioSource::Output,
                })
            }
            RecordingRequest::ChooseArea(Some(region)) => self.recording.choose_area(Some(region)),
            RecordingRequest::ChooseArea(None) => self.recording.record_whole_frame(),
            RecordingRequest::Pause => self.recording.pause(),
            RecordingRequest::Resume => self.recording.resume(),
            RecordingRequest::Stop => self.recording.stop(),
            RecordingRequest::Cancel => self.recording.cancel(),
            RecordingRequest::Dismiss => {
                self.recording.dismiss();
                Ok(())
            }
            RecordingRequest::Delete(path) => self.recording.delete_take(&path),
            RecordingRequest::Export {
                take,
                start,
                end,
                destination,
            } => self.recording.export(&take, start, end, destination),
            RecordingRequest::CancelExport => {
                self.recording.cancel_export();
                Ok(())
            }
            RecordingRequest::DismissExport => {
                self.recording.dismiss_export();
                Ok(())
            }
        }
    }

    /// The length of a listed take and a preview function over it, for the
    /// trim editor.
    pub fn recording_trim(
        &self,
        take: &Path,
    ) -> Result<(std::time::Duration, crate::TrimPreview), String> {
        if !self.recording_is_running() {
            return Err(
                "Screen recording is not running; enable capture.recording in the Feature Hub."
                    .to_owned(),
            );
        }
        let duration = self.recording.take_duration(take)?;
        let editor = Arc::clone(&self.recording_editor);
        let take = take.to_path_buf();
        let preview: crate::TrimPreview = Arc::new(move |position| {
            let frame = editor
                .frame_at(&take, position)
                .map_err(|error| error.message)?;
            kestrel_services::capture::image::RgbaImage::new(
                frame.width,
                frame.height,
                frame.pixels,
            )
            .map_err(|_| "The picture is too large to preview.".to_owned())
        });
        Ok((duration, preview))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_disabled_camera_never_opens() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let error = runtime
            .start_camera(std::path::Path::new("/dev/video0"))
            .expect_err("disabled feature");
        assert!(error.contains("camera.preview"), "{error}");
        assert_eq!(
            runtime.camera.snapshot().phase,
            kestrel_services::camera::CameraPhase::Idle
        );
        assert!(runtime.camera_view_model().cameras.is_empty());
    }
}
