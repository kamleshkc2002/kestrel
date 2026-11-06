//! The self-update install phase, advanced only by explicit user commands.
//!
//! The transitions are pure functions so the table is the whole policy: a
//! download needs an offer that publishes signed assets, a verify needs a
//! finished download, and an install needs a verified file.

use kestrel_core::ReleaseInfo;
use semver::Version;

/// Where an explicit download or install currently is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InstallPhase {
    /// Nothing downloaded; the next command may start one.
    #[default]
    Idle,
    Downloading {
        received: u64,
        total: u64,
    },
    Verifying,
    /// The download verified and waits for the install command.
    Ready {
        version: Version,
    },
    /// Installed beside the running file; the restart is pending.
    Installed {
        version: Version,
    },
    Failed(String),
}

impl InstallPhase {
    /// Whether a download or verification is in progress.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Downloading { .. } | Self::Verifying)
    }

    /// A one-line description for the update surfaces.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "No update downloaded",
            Self::Downloading { .. } => "Downloading…",
            Self::Verifying => "Verifying the download…",
            Self::Ready { .. } => "Ready to install",
            Self::Installed { .. } => "Installed; restart to finish",
            Self::Failed(_) => "The update could not be completed",
        }
    }
}

/// Starts a download for an offer that publishes an AppImage and signature.
pub fn download_started(phase: &InstallPhase, offer: Option<&ReleaseInfo>) -> Option<InstallPhase> {
    if !matches!(phase, InstallPhase::Idle | InstallPhase::Failed(_)) {
        return None;
    }
    let assets = offer?.install.as_ref()?;
    Some(InstallPhase::Downloading {
        received: 0,
        total: assets.appimage_size,
    })
}

/// Records transfer progress; only a download in progress accepts it.
pub fn download_progress(phase: &InstallPhase, received: u64, total: u64) -> Option<InstallPhase> {
    match phase {
        InstallPhase::Downloading { .. } => Some(InstallPhase::Downloading { received, total }),
        _ => None,
    }
}

/// Starts verification once the files are on disk.
pub fn verify_started(phase: &InstallPhase) -> Option<InstallPhase> {
    matches!(phase, InstallPhase::Downloading { .. }).then_some(InstallPhase::Verifying)
}

/// A finished verification makes the download ready to install.
pub fn verify_finished(phase: &InstallPhase, version: &Version) -> Option<InstallPhase> {
    match phase {
        InstallPhase::Verifying => Some(InstallPhase::Ready {
            version: version.clone(),
        }),
        _ => None,
    }
}

/// Only a verified download may be installed.
pub fn install_allowed(phase: &InstallPhase) -> bool {
    matches!(phase, InstallPhase::Ready { .. })
}

/// A finished install waits for the restart.
pub fn install_finished(phase: &InstallPhase) -> Option<InstallPhase> {
    match phase {
        InstallPhase::Ready { version } => Some(InstallPhase::Installed {
            version: version.clone(),
        }),
        _ => None,
    }
}

/// Cancelling applies to a download or verification, not to a ready file.
pub fn cancelled(phase: &InstallPhase) -> Option<InstallPhase> {
    phase.is_busy().then_some(InstallPhase::Idle)
}

/// A failed step keeps its reason, so the next explicit command can retry.
pub fn failed(phase: &InstallPhase, reason: impl Into<String>) -> Option<InstallPhase> {
    let retryable = matches!(
        phase,
        InstallPhase::Downloading { .. } | InstallPhase::Verifying | InstallPhase::Ready { .. }
    );
    retryable.then(|| InstallPhase::Failed(reason.into()))
}

#[cfg(test)]
mod tests {
    use kestrel_core::{InstallAssets, ReleaseInfo};
    use semver::Version;

    use super::{
        InstallPhase, cancelled, download_progress, download_started, failed, install_allowed,
        install_finished, verify_finished, verify_started,
    };

    fn version() -> Version {
        Version::parse("0.7.0").expect("version parses")
    }

    fn offer(with_assets: bool) -> ReleaseInfo {
        ReleaseInfo {
            version: version(),
            url: "https://example.invalid/v0.7.0".to_owned(),
            prerelease: false,
            install: with_assets.then(|| InstallAssets {
                appimage_url: "https://example.invalid/download".to_owned(),
                appimage_size: 42,
                signature_url: "https://example.invalid/download.minisig".to_owned(),
                file_name: "Kestrel-0.7.0-x86_64.AppImage".to_owned(),
            }),
        }
    }

    fn phases() -> Vec<InstallPhase> {
        vec![
            InstallPhase::Idle,
            InstallPhase::Downloading {
                received: 1,
                total: 42,
            },
            InstallPhase::Verifying,
            InstallPhase::Ready { version: version() },
            InstallPhase::Installed { version: version() },
            InstallPhase::Failed("download failed".to_owned()),
        ]
    }

    #[test]
    fn download_needs_an_offer_with_signed_assets_and_a_quiet_phase() {
        let expected = InstallPhase::Downloading {
            received: 0,
            total: 42,
        };
        for phase in phases() {
            let allowed = matches!(phase, InstallPhase::Idle | InstallPhase::Failed(_));
            assert_eq!(
                download_started(&phase, Some(&offer(true))).as_ref(),
                allowed.then_some(&expected),
                "download from {phase:?}"
            );
            assert_eq!(
                download_started(&phase, Some(&offer(false))),
                None,
                "an offer without assets cannot download from {phase:?}"
            );
            assert_eq!(
                download_started(&phase, None),
                None,
                "no offer cannot download from {phase:?}"
            );
        }
    }

    #[test]
    fn progress_and_verification_only_advance_a_download() {
        for phase in phases() {
            let downloading = matches!(phase, InstallPhase::Downloading { .. });
            assert_eq!(
                download_progress(&phase, 7, 42).is_some(),
                downloading,
                "progress in {phase:?}"
            );
            assert_eq!(
                verify_started(&phase).is_some(),
                downloading,
                "verify start in {phase:?}"
            );
        }
    }

    #[test]
    fn readiness_needs_a_finished_verification() {
        for phase in phases() {
            assert_eq!(
                verify_finished(&phase, &version()).is_some(),
                phase == InstallPhase::Verifying,
                "verify finish in {phase:?}"
            );
        }
    }

    #[test]
    fn installing_needs_a_ready_download() {
        for phase in phases() {
            let ready = phase == InstallPhase::Ready { version: version() };
            assert_eq!(install_allowed(&phase), ready, "install gate for {phase:?}");
            assert_eq!(
                install_finished(&phase).is_some(),
                ready,
                "install finish in {phase:?}"
            );
        }
    }

    #[test]
    fn cancelling_only_ends_an_active_step() {
        for phase in phases() {
            let active = matches!(
                phase,
                InstallPhase::Downloading { .. } | InstallPhase::Verifying
            );
            assert_eq!(
                cancelled(&phase),
                active.then_some(InstallPhase::Idle),
                "cancel in {phase:?}"
            );
        }
    }

    #[test]
    fn a_failure_covers_the_active_and_ready_phases() {
        for phase in phases() {
            let retryable = matches!(
                phase,
                InstallPhase::Downloading { .. }
                    | InstallPhase::Verifying
                    | InstallPhase::Ready { .. }
            );
            assert_eq!(
                failed(&phase, "boom"),
                retryable.then(|| InstallPhase::Failed("boom".to_owned())),
                "failure in {phase:?}"
            );
        }
    }
}
