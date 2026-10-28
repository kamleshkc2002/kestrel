//! Screen recording presentation.

use super::*;

pub(crate) struct RecordingPresentation {
    pub running: bool,
    /// Why the feature is not running, when it is not.
    pub reason: Option<String>,
    pub snapshot: RecordingSnapshot,
    pub sources: Vec<RecordingSource>,
    /// Tracks this session can record.
    pub available_tracks: Vec<AudioTrack>,
    pub cursor_available: bool,
    pub settings: kestrel_core::RecordingConfiguration,
    pub folder: std::path::PathBuf,
}

/// An optional audio track in the recording setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTrackOptionViewModel {
    pub track: AudioTrack,
    pub available: bool,
    pub enabled: bool,
}

/// One track of the running recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTrackViewModel {
    pub label: &'static str,
    pub status: String,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTakeViewModel {
    pub path: std::path::PathBuf,
    pub name: String,
    pub detail: String,
}

/// Which controls the recording panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingControls {
    /// Source buttons and options.
    #[default]
    Setup,
    /// Waiting on the desktop or the area view; Cancel only.
    Waiting,
    Recording {
        paused: bool,
    },
    /// Completing the file; nothing to press.
    Stopping,
}

fn export_view(export: ExportPhase) -> Option<RecordingExportViewModel> {
    let file_name = |path: &std::path::Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match export {
        ExportPhase::Idle => None,
        ExportPhase::Exporting { name, percent } => Some(RecordingExportViewModel {
            status: format!("Exporting {name}… {percent}%"),
            running: true,
            failed: false,
            path: None,
        }),
        ExportPhase::Exported { path } => Some(RecordingExportViewModel {
            status: format!("Exported {}.", file_name(&path)),
            running: false,
            failed: false,
            path: Some(path),
        }),
        ExportPhase::Failed { error } => Some(RecordingExportViewModel {
            status: match error.remediation() {
                Some(remediation) => format!("Export failed: {} {remediation}", error.message),
                None => format!("Export failed: {}", error.message),
            },
            running: false,
            failed: true,
            path: None,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingViewModel {
    pub running: bool,
    pub status: String,
    /// Set while the status reports a problem.
    pub warning: bool,
    pub controls: RecordingControls,
    pub sources: Vec<RecordingSource>,
    pub tracks_offered: Vec<RecordingTrackOptionViewModel>,
    pub cursor_available: bool,
    pub cursor: bool,
    pub framerate: u32,
    /// Running time while recording; stands still while paused.
    pub clock: Option<RecordingClock>,
    pub tracks: Vec<RecordingTrackViewModel>,
    pub takes: Vec<RecordingTakeViewModel>,
    pub folder: std::path::PathBuf,
    /// A finished or failed outcome can be cleared.
    pub dismissable: bool,
    /// The area view should open for the frame the runtime holds.
    pub choosing_area: bool,
    /// The running or last trimmed export.
    pub export: Option<RecordingExportViewModel>,
    pub turn_on: Option<TurnOnViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingExportViewModel {
    pub status: String,
    /// Cancel is offered while it runs; Dismiss once it ended.
    pub running: bool,
    pub failed: bool,
    /// The finished file, for Play.
    pub path: Option<std::path::PathBuf>,
}

impl RecordingViewModel {
    pub(crate) fn from_presentation(presentation: RecordingPresentation) -> Self {
        let RecordingPresentation {
            running,
            reason,
            snapshot,
            sources,
            available_tracks,
            cursor_available,
            settings,
            folder,
        } = presentation;
        let tracks_offered = AudioTrack::ALL
            .into_iter()
            .map(|track| RecordingTrackOptionViewModel {
                track,
                available: available_tracks.contains(&track),
                enabled: match track {
                    AudioTrack::System => settings.system_audio,
                    AudioTrack::Microphone => settings.microphone,
                },
            })
            .collect();
        let takes = snapshot
            .takes
            .iter()
            .map(|take| RecordingTakeViewModel {
                path: take.path.clone(),
                name: take.name.clone(),
                detail: match take.duration {
                    Some(duration) => {
                        format!(
                            "{} · {}",
                            format_bytes(take.bytes as f64),
                            format_clock(duration)
                        )
                    }
                    None => format_bytes(take.bytes as f64),
                },
            })
            .collect();
        let mut model = Self {
            running,
            sources,
            tracks_offered,
            cursor_available,
            cursor: settings.cursor,
            framerate: settings.framerate,
            takes,
            folder,
            export: export_view(snapshot.export),
            ..Self::default()
        };
        if !running {
            model.status =
                reason.unwrap_or_else(|| "Enable capture.recording in the Feature Hub.".to_owned());
            model.export = None;
            return model;
        }
        match snapshot.phase {
            RecordingPhase::Idle => {
                model.status = "Choose what to record. The desktop asks before sharing, and \
                                recordings stay on this computer."
                    .to_owned();
            }
            RecordingPhase::Requesting { source } => {
                model.controls = RecordingControls::Waiting;
                model.status = format!(
                    "Waiting for the desktop to share a {}…",
                    match source {
                        RecordingSource::Window => "window",
                        RecordingSource::Screen | RecordingSource::Area => "screen",
                    }
                );
            }
            RecordingPhase::ChoosingArea { .. } => {
                model.controls = RecordingControls::Waiting;
                model.choosing_area = true;
                model.status = "Drag the area to record.".to_owned();
            }
            RecordingPhase::Recording {
                source,
                tracks,
                paused,
                clock,
            } => {
                model.controls = RecordingControls::Recording { paused };
                model.clock = Some(clock);
                model.status = format!(
                    "{} recording {}.",
                    source.label(),
                    if paused { "paused" } else { "in progress" }
                );
                model.warning = tracks.iter().any(|track| track.error.is_some());
                model.tracks = tracks
                    .into_iter()
                    .map(|state| RecordingTrackViewModel {
                        label: state.track.label(),
                        failed: state.error.is_some(),
                        status: state
                            .error
                            .map_or_else(|| "Recording".to_owned(), |error| error.message),
                    })
                    .collect();
            }
            RecordingPhase::Stopping => {
                model.controls = RecordingControls::Stopping;
                model.status = "Saving the recording…".to_owned();
            }
            RecordingPhase::Completed { take, note } => {
                model.dismissable = true;
                model.warning = note.is_some();
                model.status = match note {
                    Some(note) => format!("Saved {}. {}", take.name, note.message),
                    None => format!("Saved {}.", take.name),
                };
            }
            RecordingPhase::Failed { error } => {
                model.dismissable = true;
                model.warning = true;
                model.status = match error.remediation() {
                    Some(remediation) => format!("{} {remediation}", error.message),
                    None => error.message,
                };
            }
        }
        model
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_clock(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {

    mod recording {
        use crate::view_model::{
            RecordingControls, RecordingPresentation, RecordingViewModel, format_clock,
        };
        use kestrel_platform::recording::{
            AudioTrack, RecordingError, RecordingErrorKind, RecordingSource,
        };
        use kestrel_services::recording::{
            RecordingClock, RecordingPhase, RecordingSnapshot, Take, TrackState,
        };
        use std::time::Duration;

        fn view(phase: RecordingPhase, available: Vec<AudioTrack>) -> RecordingViewModel {
            RecordingViewModel::from_presentation(RecordingPresentation {
                running: true,
                reason: None,
                snapshot: RecordingSnapshot {
                    phase,
                    generation: 1,
                    takes: vec![Take {
                        path: "/videos/Kestrel-recording-1.webm".into(),
                        name: "Kestrel-recording-1.webm".into(),
                        bytes: 3 * 1024 * 1024,
                        duration: Some(Duration::from_secs(75)),
                    }],
                    export: kestrel_services::recording::ExportPhase::Idle,
                },
                sources: vec![RecordingSource::Screen, RecordingSource::Window],
                available_tracks: available,
                cursor_available: true,
                settings: kestrel_core::RecordingConfiguration {
                    microphone: true,
                    ..kestrel_core::RecordingConfiguration::default()
                },
                folder: "/videos".into(),
            })
        }

        #[test]
        fn setup_offers_only_available_tracks() {
            let setup = view(RecordingPhase::Idle, vec![AudioTrack::System]);
            assert_eq!(setup.controls, RecordingControls::Setup);
            let microphone = &setup.tracks_offered[1];
            assert_eq!(microphone.track, AudioTrack::Microphone);
            assert!(microphone.enabled && !microphone.available);
            assert!(setup.tracks_offered[0].available);
            assert_eq!(setup.takes[0].detail, "3.0 MiB · 1:15");
        }

        #[test]
        fn running_recordings_show_controls_and_failed_tracks() {
            let recording = view(
                RecordingPhase::Recording {
                    source: RecordingSource::Window,
                    tracks: vec![
                        TrackState {
                            track: AudioTrack::System,
                            error: None,
                            application: None,
                        },
                        TrackState {
                            track: AudioTrack::Microphone,
                            error: Some(RecordingError::new(
                                RecordingErrorKind::Unavailable,
                                "Microphone stopped: unplugged",
                            )),
                            application: None,
                        },
                    ],
                    paused: true,
                    clock: RecordingClock::default(),
                },
                AudioTrack::ALL.to_vec(),
            );
            assert_eq!(
                recording.controls,
                RecordingControls::Recording { paused: true }
            );
            assert!(recording.warning);
            assert!(!recording.tracks[0].failed);
            assert!(recording.tracks[1].failed);
            assert_eq!(recording.tracks[1].status, "Microphone stopped: unplugged");
        }

        #[test]
        fn outcomes_can_be_dismissed_and_explain_remediation() {
            let denied = view(
                RecordingPhase::Failed {
                    error: RecordingError::new(RecordingErrorKind::Denied, "Refused."),
                },
                Vec::new(),
            );
            assert!(denied.dismissable && denied.warning);
            assert!(
                denied.status.contains("privacy settings"),
                "{}",
                denied.status
            );
            let choosing = view(
                RecordingPhase::ChoosingArea {
                    frame: std::sync::Arc::new(
                        kestrel_services::capture::image::RgbaImage::new(1, 1, vec![0; 4])
                            .expect("image"),
                    ),
                },
                Vec::new(),
            );
            assert!(choosing.choosing_area);
            assert_eq!(choosing.controls, RecordingControls::Waiting);
        }

        #[test]
        fn clock_text_grows_hours_past_an_hour() {
            assert_eq!(format_clock(Duration::from_secs(59)), "0:59");
            assert_eq!(format_clock(Duration::from_secs(3_725)), "1:02:05");
        }

        #[test]
        fn exports_show_progress_then_their_outcome() {
            use kestrel_services::recording::ExportPhase;
            let with_export = |export: ExportPhase| {
                let mut presentation_view = view(RecordingPhase::Idle, Vec::new());
                presentation_view.export = super::super::export_view(export);
                presentation_view
            };
            let running = with_export(ExportPhase::Exporting {
                name: "clip.webm".into(),
                percent: 40,
            })
            .export
            .expect("export row");
            assert!(running.running && !running.failed);
            assert_eq!(running.status, "Exporting clip.webm… 40%");
            let failed = with_export(ExportPhase::Failed {
                error: RecordingError::new(RecordingErrorKind::Unavailable, "No VP8 encoder."),
            })
            .export
            .expect("export row");
            assert!(failed.failed && !failed.running);
            assert!(failed.status.contains("Install"), "{}", failed.status);
            let done = with_export(ExportPhase::Exported {
                path: "/clips/clip.webm".into(),
            })
            .export
            .expect("export row");
            assert_eq!(
                done.path.as_deref(),
                Some(std::path::Path::new("/clips/clip.webm"))
            );
            assert!(with_export(ExportPhase::Idle).export.is_none());
        }
    }
}
