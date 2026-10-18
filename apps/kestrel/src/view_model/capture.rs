//! Screenshot capture presentation.

use super::*;

pub(crate) struct CapturePresentation {
    pub snapshot: CaptureSnapshot,
    pub running: bool,
    pub provider: Option<CaptureProvider>,
    /// Modes the provider offers, in preferred order.
    pub modes: Vec<CaptureMode>,
    /// Thumbnail files aligned with `snapshot.entries`.
    pub thumbnails: Vec<Option<std::path::PathBuf>>,
    pub limits: kestrel_core::CaptureConfiguration,
    pub now: std::time::SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureModeViewModel {
    pub mode: CaptureMode,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEntryViewModel {
    pub id: CaptureId,
    pub title: String,
    pub subtitle: String,
    pub thumbnail: Option<std::path::PathBuf>,
}

/// Screenshot controls and recent captures, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureViewModel {
    pub running: bool,
    pub status: String,
    pub modes: Vec<CaptureModeViewModel>,
    pub capturing: bool,
    pub entries: Vec<CaptureEntryViewModel>,
    pub usage: String,
    pub storage_error: Option<String>,
    pub limits: kestrel_core::CaptureConfiguration,
}

impl CaptureViewModel {
    pub(crate) fn from_presentation(presentation: CapturePresentation) -> Self {
        let snapshot = presentation.snapshot;
        let capturing = matches!(snapshot.phase, CapturePhase::Capturing { .. });
        let status = if !presentation.running {
            "Screenshots are off. Enable capture.screenshot in the Feature Hub.".to_owned()
        } else if let Some(provider) = presentation.provider {
            match &snapshot.phase {
                CapturePhase::Idle => format!("Captures go through {}.", provider.label()),
                CapturePhase::Capturing { mode, .. } => match mode {
                    CaptureMode::Interactive => {
                        "Waiting for the desktop's capture dialog…".to_owned()
                    }
                    CaptureMode::Area => "Drag to select an area; Esc cancels.".to_owned(),
                    CaptureMode::Window | CaptureMode::Screen => "Capturing…".to_owned(),
                },
                CapturePhase::Captured { .. } => "Added to recent captures.".to_owned(),
                CapturePhase::Cancelled => "Capture cancelled; nothing was kept.".to_owned(),
                CapturePhase::Failed { kind, message } => {
                    let remedy = match kind {
                        CaptureErrorKind::Denied => {
                            " Allow screenshots for Kestrel in the desktop's privacy settings, \
                             or use Choose in dialog."
                        }
                        CaptureErrorKind::NoImage => {
                            " Save to a file in the desktop's dialog to add it here."
                        }
                        CaptureErrorKind::Unavailable => {
                            " Refresh capabilities after installing the missing provider."
                        }
                        CaptureErrorKind::Cancelled
                        | CaptureErrorKind::TooLarge
                        | CaptureErrorKind::Timeout
                        | CaptureErrorKind::Failed => "",
                    };
                    format!("Capture failed: {message}.{remedy}")
                }
            }
        } else {
            "No screenshot provider was found; the Feature Hub lists setup steps.".to_owned()
        };
        let modes = if presentation.running {
            presentation
                .modes
                .iter()
                .map(|&mode| CaptureModeViewModel {
                    mode,
                    label: mode.label(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let entries = snapshot
            .entries
            .iter()
            .zip(
                presentation
                    .thumbnails
                    .into_iter()
                    .chain(std::iter::repeat(None)),
            )
            .map(|(entry, thumbnail)| {
                let age = presentation
                    .now
                    .duration_since(entry.created_at)
                    .unwrap_or_default();
                CaptureEntryViewModel {
                    id: entry.id,
                    title: format!(
                        "{} · {}×{}{}",
                        entry.mode.label(),
                        entry.width,
                        entry.height,
                        if entry.edited { " · edited" } else { "" }
                    ),
                    subtitle: format!("{} · {}", format_age(age), format_bytes(entry.bytes as f64)),
                    thumbnail,
                }
            })
            .collect::<Vec<_>>();
        let limits = presentation.limits;
        let usage = format!(
            "{} of {} captures · {} of {} MiB · kept up to {} h",
            entries.len(),
            limits.max_entries,
            format_bytes(snapshot.total_bytes as f64),
            limits.max_total_megabytes,
            limits.max_age_hours
        );
        Self {
            running: presentation.running,
            status,
            modes,
            capturing,
            entries,
            usage,
            storage_error: snapshot.storage_error,
            limits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::test_support::*;

    fn capture_snapshot_with_entries(
        directory: &std::path::Path,
        phase: kestrel_services::capture::CapturePhase,
    ) -> kestrel_services::capture::CaptureSnapshot {
        use kestrel_services::capture::{
            CaptureHistory, CapturePolicy, NewCapture,
            image::{RgbaImage, encode_png},
        };
        let mut history = CaptureHistory::open(
            directory.to_path_buf(),
            CapturePolicy::from_configuration(&kestrel_core::CaptureConfiguration::default()),
        )
        .expect("history opens");
        let png = encode_png(&RgbaImage::new(4, 3, vec![255; 48]).expect("valid image"))
            .expect("png encodes");
        let base = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        for (offset, edited) in [(0, false), (60, true)] {
            history
                .insert(
                    &png,
                    NewCapture {
                        mode: CaptureMode::Area,
                        edited,
                        width: 4,
                        height: 3,
                    },
                    base + std::time::Duration::from_secs(offset),
                )
                .expect("capture stores");
        }
        kestrel_services::capture::CaptureSnapshot {
            phase,
            entries: history.entries().to_vec(),
            total_bytes: history.total_bytes(),
            storage_error: None,
            generation: 3,
        }
    }

    #[test]
    fn capture_panel_lists_newest_first_and_explains_a_denied_capture() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let snapshot = capture_snapshot_with_entries(
            directory.path(),
            kestrel_services::capture::CapturePhase::Failed {
                kind: CaptureErrorKind::Denied,
                message: "The desktop denied screenshot permission".to_owned(),
            },
        );
        let view = super::CaptureViewModel::from_presentation(super::CapturePresentation {
            snapshot,
            running: true,
            provider: Some(CaptureProvider::Portal),
            modes: vec![CaptureMode::Interactive, CaptureMode::Screen],
            thumbnails: Vec::new(),
            limits: kestrel_core::CaptureConfiguration::default(),
            now: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_120),
        });

        assert!(view.status.contains("privacy settings"), "{}", view.status);
        assert_eq!(
            view.modes.iter().map(|mode| mode.mode).collect::<Vec<_>>(),
            [CaptureMode::Interactive, CaptureMode::Screen]
        );
        assert!(!view.capturing);
        assert_eq!(view.entries.len(), 2);
        assert!(view.entries[0].title.ends_with("edited"), "newest first");
        assert!(view.entries[0].subtitle.starts_with("1 min old"));
        assert!(view.entries[1].subtitle.starts_with("2 min old"));
        assert!(view.usage.starts_with("2 of 30 captures"), "{}", view.usage);
    }

    #[test]
    fn a_stopped_capture_feature_offers_no_modes() {
        let view = super::CaptureViewModel::from_presentation(super::CapturePresentation {
            running: false,
            provider: Some(CaptureProvider::X11),
            modes: vec![CaptureMode::Window, CaptureMode::Screen],
            ..test_capture_presentation()
        });
        assert!(view.modes.is_empty());
        assert!(view.status.contains("capture.screenshot"));
    }
}
