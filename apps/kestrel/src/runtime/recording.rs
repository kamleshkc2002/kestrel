//! Camera and recording runtime features.

use super::*;
use kestrel_platform::{
    audio::AudioBackend,
    recording::{RecordingDetection, SystemAudioSource},
};

/// The tracks and System audio source a recording starts with.
///
/// When the settings save one application and this session cannot isolate it,
/// the System track is left out so the recording keeps the chosen scope.
fn recording_audio(
    settings: &kestrel_core::RecordingConfiguration,
    detection: &RecordingDetection,
) -> (Vec<AudioTrack>, SystemAudioSource) {
    let application = settings
        .system_audio_app
        .as_ref()
        .filter(|_| detection.application_audio_available());
    let isolated = settings.system_audio_app.is_none() || application.is_some();
    let mut audio = Vec::new();
    if settings.system_audio && isolated && detection.track_available(AudioTrack::System) {
        audio.push(AudioTrack::System);
    }
    if settings.microphone && detection.track_available(AudioTrack::Microphone) {
        audio.push(AudioTrack::Microphone);
    }
    let system_audio = match application {
        Some(identity) => SystemAudioSource::Application {
            identity: identity.clone(),
        },
        None => SystemAudioSource::Output,
    };
    (audio, system_audio)
}

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
            settings: self.recording_settings.clone(),
            applications: self.recording_applications.clone(),
            application_audio: detection.as_ref().map_or_else(
                || Err("Recording support has not been checked yet.".to_owned()),
                |detection| detection.application_audio.clone(),
            ),
            folder: self.recording.folder().to_path_buf(),
        })
    }

    /// Refreshes the applications offered for the System track; `true` when
    /// the list changed and the panel should be rebuilt.
    pub fn refresh_recording_applications(&mut self) -> bool {
        if !self.recording_is_running()
            || matches!(
                self.recording.snapshot().phase,
                RecordingPhase::Recording { .. } | RecordingPhase::Stopping
            )
        {
            return false;
        }
        // The mixer hides idle streams unless asked to keep them; a paused
        // application must still be pickable, so then read the server directly.
        let applications = if self.audio_mixer_is_running()
            && self.audio_mixer.policy().include_inactive_streams
        {
            Self::applications_from_streams(self.audio_mixer.latest().streams.iter())
        } else {
            match PulseAudioBackend::new().discover() {
                Ok(discovery) => Self::applications_from_streams(discovery.streams.iter()),
                Err(_) => Vec::new(),
            }
        };
        if applications == self.recording_applications {
            return false;
        }
        self.recording_applications = applications;
        true
    }

    /// The applications with playback streams, including paused ones,
    /// deduplicated by identity and sorted by label.
    fn applications_from_streams<'a>(
        streams: impl Iterator<Item = &'a kestrel_platform::audio::PlaybackStream>,
    ) -> Vec<RecordingApplicationViewModel> {
        let mut applications: Vec<RecordingApplicationViewModel> = Vec::new();
        for stream in streams {
            let Some(identity) = stream.application_id.clone() else {
                continue;
            };
            if applications
                .iter()
                .any(|application| application.identity == identity)
            {
                continue;
            }
            applications.push(RecordingApplicationViewModel {
                label: stream
                    .application_name
                    .clone()
                    .unwrap_or_else(|| identity.clone()),
                identity,
            });
        }
        applications.sort_by(|left, right| left.label.cmp(&right.label));
        applications
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
                let settings = self.recording_settings.clone();
                let (audio, system_audio) = recording_audio(&settings, &detection);
                self.recording.start(RecordingOptions {
                    source,
                    cursor: settings.cursor && detection.cursor_available(),
                    framerate: settings.framerate,
                    audio,
                    system_audio,
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

    #[test]
    fn recording_audio_keeps_a_saved_application_scope() {
        use kestrel_platform::recording::AudioAvailability;
        let detection = |application_audio: Result<(), String>| RecordingDetection {
            portal_version: Some(5),
            source_types: 0,
            cursor_modes: 0,
            pipewire_socket: true,
            gstreamer_error: None,
            missing_elements: Vec::new(),
            system_audio: AudioAvailability::Available,
            microphone: AudioAvailability::Available,
            application_audio,
        };
        let settings =
            |system_audio: bool, application: Option<&str>| kestrel_core::RecordingConfiguration {
                system_audio,
                microphone: true,
                system_audio_app: application.map(str::to_owned),
                ..kestrel_core::RecordingConfiguration::default()
            };
        let application = || SystemAudioSource::Application {
            identity: "firefox".to_owned(),
        };
        let cases = [
            (
                "System audio off",
                recording_audio(&settings(false, None), &detection(Ok(()))),
                vec![AudioTrack::Microphone],
                SystemAudioSource::Output,
            ),
            (
                "no saved application",
                recording_audio(&settings(true, None), &detection(Ok(()))),
                vec![AudioTrack::System, AudioTrack::Microphone],
                SystemAudioSource::Output,
            ),
            (
                "saved application isolated",
                recording_audio(&settings(true, Some("firefox")), &detection(Ok(()))),
                vec![AudioTrack::System, AudioTrack::Microphone],
                application(),
            ),
            (
                "saved application cannot be isolated",
                recording_audio(
                    &settings(true, Some("firefox")),
                    &detection(Err("Needs a PipeWire audio server.".to_owned())),
                ),
                vec![AudioTrack::Microphone],
                SystemAudioSource::Output,
            ),
        ];
        for (case, (audio, source), expected_audio, expected_source) in cases {
            assert_eq!(audio, expected_audio, "{case}");
            assert_eq!(source, expected_source, "{case}");
        }
    }

    #[test]
    fn offered_applications_include_paused_streams_and_deduplicate() {
        use kestrel_platform::audio::PlaybackStream;
        let stream = |id: u32, identity: &str, name: Option<&str>, corked: bool| PlaybackStream {
            id,
            name: "stream".to_owned(),
            application_name: name.map(str::to_owned),
            application_id: Some(identity.to_owned()),
            media_name: None,
            output_id: 1,
            volume_percent: None,
            volume_writable: false,
            muted: false,
            corked,
        };
        let streams = [
            stream(1, "zeta", Some("Aurora"), false),
            stream(2, "alpha", None, false),
            stream(3, "zeta", Some("Aurora elsewhere"), false),
            stream(4, "idle", Some("Idle"), true),
            stream(5, "nameless", None, false),
        ];
        let applications = ApplicationRuntime::applications_from_streams(streams.iter());
        let offered: Vec<(&str, &str)> = applications
            .iter()
            .map(|application| (application.identity.as_str(), application.label.as_str()))
            .collect();
        assert_eq!(
            offered,
            [
                ("zeta", "Aurora"),
                ("idle", "Idle"),
                ("alpha", "alpha"),
                ("nameless", "nameless")
            ]
        );
    }
}
