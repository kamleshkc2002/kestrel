//! Camera presentation.

use super::*;

pub(crate) struct CameraPresentation {
    pub running: bool,
    /// Why the feature is not running, when it is not.
    pub reason: Option<String>,
    pub snapshot: CameraSnapshot,
    pub cameras: Vec<CameraInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraChoiceViewModel {
    pub path: std::path::PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CameraViewModel {
    pub running: bool,
    pub status: String,
    pub cameras: Vec<CameraChoiceViewModel>,
    /// A preview is opening or showing frames; Stop is offered.
    pub active: bool,
    pub turn_on: Option<TurnOnViewModel>,
}

impl CameraViewModel {
    pub(crate) fn from_presentation(presentation: CameraPresentation) -> Self {
        let cameras = presentation
            .cameras
            .into_iter()
            .map(|camera| CameraChoiceViewModel {
                path: camera.path,
                name: camera.name,
            })
            .collect::<Vec<_>>();
        let phase = presentation.snapshot.phase;
        let status = match (&phase, presentation.running) {
            (_, false) => presentation
                .reason
                .unwrap_or_else(|| "Enable camera.preview in the Feature Hub.".to_owned()),
            (CameraPhase::Idle, true) if cameras.is_empty() => "No camera is connected.".to_owned(),
            (CameraPhase::Idle, true) => {
                "The camera stays off until you start a preview. Frames are never saved.".to_owned()
            }
            (CameraPhase::Starting { name }, true) => format!("Opening {name}…"),
            (
                CameraPhase::Previewing {
                    name,
                    width,
                    height,
                },
                true,
            ) => format!("{name} · {width} × {height}. Frames are never saved."),
            (CameraPhase::Failed { name, error }, true) => format!("{name}: {}", error.message),
        };
        Self {
            running: presentation.running,
            status,
            cameras,
            active: presentation.running
                && matches!(
                    phase,
                    CameraPhase::Starting { .. } | CameraPhase::Previewing { .. }
                ),
            turn_on: None,
        }
    }
}

#[cfg(test)]
mod tests {

    mod camera {
        use crate::view_model::{CameraPresentation, CameraViewModel};
        use kestrel_platform::camera::{CameraError, CameraErrorKind, CameraInfo};
        use kestrel_services::camera::{CameraPhase, CameraSnapshot};

        fn view(running: bool, phase: CameraPhase) -> CameraViewModel {
            CameraViewModel::from_presentation(CameraPresentation {
                running,
                reason: (!running).then(|| "Log in at the computer's own seat.".to_owned()),
                snapshot: CameraSnapshot {
                    phase,
                    generation: 1,
                },
                cameras: vec![CameraInfo {
                    path: "/dev/video0".into(),
                    name: "Camera".into(),
                }],
            })
        }

        #[test]
        fn preview_state_drives_stop_and_frame_display() {
            let idle = view(true, CameraPhase::Idle);
            assert!(!idle.active);
            let starting = view(
                true,
                CameraPhase::Starting {
                    name: "Camera".into(),
                },
            );
            assert!(starting.active);
            let previewing = view(
                true,
                CameraPhase::Previewing {
                    name: "Camera".into(),
                    width: 640,
                    height: 480,
                },
            );
            assert!(previewing.active);
            assert!(
                previewing.status.contains("640 × 480"),
                "{}",
                previewing.status
            );
        }

        #[test]
        fn failures_and_unavailable_features_explain_themselves() {
            let failed = view(
                true,
                CameraPhase::Failed {
                    name: "Camera".into(),
                    error: CameraError::new(CameraErrorKind::Busy, "Another app has it."),
                },
            );
            assert!(!failed.active);
            assert!(failed.status.contains("Another app has it."));
            let off = view(false, CameraPhase::Idle);
            assert!(!off.active);
            assert_eq!(off.status, "Log in at the computer's own seat.");
        }
    }
}
