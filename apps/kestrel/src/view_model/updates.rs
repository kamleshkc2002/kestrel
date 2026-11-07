//! Update-check presentation for Overview, the tray, and Settings.

use std::time::SystemTime;

use kestrel_core::{ReleaseInfo, UpdateCheck};
use kestrel_services::{
    update_install::InstallPhase,
    updates::{UpdateSnapshot, UpdateStatus},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdatesViewModel {
    pub check: UpdateCheck,
    /// The one-time Overview question, before any answer is saved.
    pub show_consent_card: bool,
    /// The Overview banner; hidden while its version was dismissed and no
    /// download or install is in progress.
    pub banner: Option<UpdatePhaseViewModel>,
    /// The tray and Settings offer, which stays after a dismissal.
    pub available: Option<UpdateOfferViewModel>,
    /// The Settings status line.
    pub status: String,
    /// Kestrel runs from a writable AppImage and the offer publishes one.
    pub can_self_update: bool,
    /// The explicit download and install step.
    pub install: UpdateInstallViewModel,
    /// The action row Settings shows, with or without the Overview banner.
    pub install_row: Option<UpdatePhaseViewModel>,
    /// An earlier AppImage waits beside the running one.
    pub previous_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOfferViewModel {
    pub version: String,
    pub url: String,
}

impl UpdateOfferViewModel {
    fn new(release: &ReleaseInfo) -> Self {
        Self {
            version: release.version.to_string(),
            url: release.url.clone(),
        }
    }
}

/// Where an explicit download or install is, for the update surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateInstallViewModel {
    #[default]
    Idle,
    Downloading {
        received: u64,
        total: u64,
        /// "Downloading 12 of 80 MB".
        text: String,
        /// Whole percent, for a progress bar.
        percent: u8,
    },
    Verifying,
    Ready {
        version: String,
    },
    Installed {
        version: String,
    },
    Failed {
        reason: String,
    },
}

impl UpdateInstallViewModel {
    fn from_phase(phase: &InstallPhase) -> Self {
        match phase {
            InstallPhase::Idle => Self::Idle,
            InstallPhase::Downloading { received, total } => Self::Downloading {
                received: *received,
                total: *total,
                text: format!(
                    "Downloading {} of {}",
                    kestrel_core::format_bytes(*received as f64),
                    kestrel_core::format_bytes(*total as f64)
                ),
                percent: percent(*received, *total),
            },
            InstallPhase::Verifying => Self::Verifying,
            InstallPhase::Ready { version } => Self::Ready {
                version: version.to_string(),
            },
            InstallPhase::Installed { version } => Self::Installed {
                version: version.to_string(),
            },
            InstallPhase::Failed(reason) => Self::Failed {
                reason: reason.clone(),
            },
        }
    }

    fn version(&self) -> Option<&str> {
        match self {
            Self::Ready { version } | Self::Installed { version } => Some(version),
            _ => None,
        }
    }
}

/// A button on an update surface; its label and command are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    GetUpdate,
    ViewRelease,
    Dismiss,
    CancelDownload,
    InstallAndRestart,
    RestartToFinish,
    TryAgain,
}

impl UpdateAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::GetUpdate => "Get update",
            Self::ViewRelease => "View release",
            Self::Dismiss => "Dismiss",
            Self::CancelDownload => "Cancel",
            Self::InstallAndRestart => "Install and restart",
            Self::RestartToFinish => "Restart to finish",
            Self::TryAgain => "Try again",
        }
    }
}

/// One phase-dependent row: the Overview banner and the Settings install
/// row both come from this shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdatePhaseViewModel {
    pub title: String,
    pub subtitle: Option<String>,
    /// The release page for View release.
    pub url: Option<String>,
    /// Whole-percent progress while a download runs.
    pub percent: Option<u8>,
    pub actions: Vec<UpdateAction>,
    /// The action drawn as suggested.
    pub suggested: Option<UpdateAction>,
}

impl UpdatesViewModel {
    pub(crate) fn new(
        check: UpdateCheck,
        snapshot: &UpdateSnapshot,
        setup_done: bool,
        previous_available: bool,
        now: SystemTime,
    ) -> Self {
        let offer = snapshot.banner.as_ref().map(UpdateOfferViewModel::new);
        let available = snapshot.available.as_ref().map(UpdateOfferViewModel::new);
        let install = UpdateInstallViewModel::from_phase(&snapshot.install);
        let version = install
            .version()
            .map(str::to_owned)
            .or_else(|| available.as_ref().map(|offer| offer.version.clone()))
            .or_else(|| offer.as_ref().map(|offer| offer.version.clone()));
        let url = available
            .as_ref()
            .or(offer.as_ref())
            .map(|offer| offer.url.clone());
        let install_row = install_row(
            &install,
            version.as_deref(),
            url.clone(),
            snapshot.can_self_update,
        );
        let banner = match &install {
            UpdateInstallViewModel::Idle => offer
                .as_ref()
                .map(|offer| idle_banner(offer, snapshot.can_self_update, url.clone())),
            UpdateInstallViewModel::Failed { .. } => install_row.clone().map(|mut row| {
                row.actions.push(UpdateAction::ViewRelease);
                row
            }),
            _ => install_row.clone(),
        };
        Self {
            check,
            show_consent_card: check == UpdateCheck::Unasked && setup_done,
            banner,
            available,
            status: status_text(snapshot, now),
            can_self_update: snapshot.can_self_update,
            install,
            install_row,
            previous_available,
        }
    }
}

fn idle_banner(
    offer: &UpdateOfferViewModel,
    can_self_update: bool,
    url: Option<String>,
) -> UpdatePhaseViewModel {
    let (actions, suggested) = if can_self_update {
        (
            vec![
                UpdateAction::GetUpdate,
                UpdateAction::ViewRelease,
                UpdateAction::Dismiss,
            ],
            Some(UpdateAction::GetUpdate),
        )
    } else {
        (
            vec![UpdateAction::ViewRelease, UpdateAction::Dismiss],
            Some(UpdateAction::ViewRelease),
        )
    };
    UpdatePhaseViewModel {
        title: format!("Kestrel {} is available", offer.version),
        subtitle: None,
        url,
        percent: None,
        actions,
        suggested,
    }
}

/// The install row Settings keeps after the Overview banner is dismissed.
fn install_row(
    install: &UpdateInstallViewModel,
    version: Option<&str>,
    url: Option<String>,
    can_self_update: bool,
) -> Option<UpdatePhaseViewModel> {
    let version = version.unwrap_or_default();
    match install {
        UpdateInstallViewModel::Idle => can_self_update.then(|| UpdatePhaseViewModel {
            title: format!("Kestrel {version} is available"),
            subtitle: Some("Downloads the AppImage and checks its signature.".to_owned()),
            url,
            percent: None,
            actions: vec![UpdateAction::GetUpdate],
            suggested: Some(UpdateAction::GetUpdate),
        }),
        UpdateInstallViewModel::Downloading { text, percent, .. } => Some(UpdatePhaseViewModel {
            title: format!("Downloading Kestrel {version}"),
            subtitle: Some(text.clone()),
            url,
            percent: Some(*percent),
            actions: vec![UpdateAction::CancelDownload],
            suggested: None,
        }),
        UpdateInstallViewModel::Verifying => Some(UpdatePhaseViewModel {
            title: "Checking the download…".to_owned(),
            url,
            percent: None,
            actions: Vec::new(),
            ..UpdatePhaseViewModel::default()
        }),
        UpdateInstallViewModel::Ready { .. } => Some(UpdatePhaseViewModel {
            title: format!("Kestrel {version} is ready to install"),
            subtitle: None,
            url,
            percent: None,
            actions: vec![UpdateAction::InstallAndRestart],
            suggested: Some(UpdateAction::InstallAndRestart),
        }),
        UpdateInstallViewModel::Installed { .. } => Some(UpdatePhaseViewModel {
            title: format!("Kestrel {version} is installed"),
            subtitle: None,
            url,
            percent: None,
            actions: vec![UpdateAction::RestartToFinish],
            suggested: Some(UpdateAction::RestartToFinish),
        }),
        UpdateInstallViewModel::Failed { reason } => Some(UpdatePhaseViewModel {
            title: "The update did not finish".to_owned(),
            subtitle: Some(reason.clone()),
            url,
            percent: None,
            actions: vec![UpdateAction::TryAgain],
            suggested: Some(UpdateAction::TryAgain),
        }),
    }
}

fn percent(received: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    received
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(100)
        .min(100) as u8
}

fn status_text(snapshot: &UpdateSnapshot, now: SystemTime) -> String {
    match &snapshot.status {
        UpdateStatus::Off => "Check is off.".to_owned(),
        UpdateStatus::NotChecked => "Not checked yet".to_owned(),
        UpdateStatus::Checking => "Checking…".to_owned(),
        UpdateStatus::UpToDate => match snapshot.last_checked {
            Some(checked) => format!("Up to date · checked {}", ago(now, checked)),
            None => "Up to date".to_owned(),
        },
        UpdateStatus::Available(release) => format!("{} is available", release.version),
        UpdateStatus::Failed(reason) => format!("Last check failed: {reason}"),
    }
}

/// A compact elapsed time for the status line.
fn ago(now: SystemTime, then: SystemTime) -> String {
    let elapsed = now.duration_since(then).unwrap_or_default().as_secs();
    match elapsed {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", elapsed / 60),
        3_600..=86_399 => format!("{} h ago", elapsed / 3_600),
        _ => format!("{} d ago", elapsed / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use kestrel_core::{ReleaseInfo, UpdateCheck};
    use kestrel_services::updates::{UpdateSnapshot, UpdateStatus};
    use semver::Version;

    use super::{UpdateAction, UpdateInstallViewModel, UpdatePhaseViewModel, UpdatesViewModel};

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).expect("test version parses"),
            url: "https://example.invalid/release".to_owned(),
            prerelease: version.contains('-'),
            install: None,
        }
    }

    fn snapshot(
        install: kestrel_services::update_install::InstallPhase,
        can_self_update: bool,
        offer: bool,
    ) -> UpdateSnapshot {
        let available = offer.then(|| release("0.6.0"));
        UpdateSnapshot {
            status: available
                .clone()
                .map_or(UpdateStatus::UpToDate, UpdateStatus::Available),
            last_checked: None,
            banner: available.clone(),
            available,
            install,
            can_self_update,
        }
    }

    fn model(
        phase: kestrel_services::update_install::InstallPhase,
        can_self_update: bool,
        offer: bool,
    ) -> UpdatesViewModel {
        UpdatesViewModel::new(
            UpdateCheck::Enabled,
            &snapshot(phase, can_self_update, offer),
            true,
            false,
            at(1_000_000),
        )
    }

    fn version(value: &str) -> Version {
        Version::parse(value).expect("test version parses")
    }

    fn actions(row: &Option<UpdatePhaseViewModel>) -> Vec<UpdateAction> {
        row.as_ref()
            .map(|row| row.actions.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_status_line_names_the_result_and_check_time() {
        let now = at(1_000_000);
        let cases = [
            (UpdateStatus::NotChecked, None, None, "Not checked yet"),
            (
                UpdateStatus::UpToDate,
                None,
                Some(now - Duration::from_secs(2 * 60 * 60)),
                "Up to date · checked 2 h ago",
            ),
            (
                UpdateStatus::Available(release("0.6.0-preview.5")),
                Some(release("0.6.0-preview.5")),
                Some(now),
                "0.6.0-preview.5 is available",
            ),
            (
                UpdateStatus::Failed("GitHub could not be reached.".to_owned()),
                None,
                None,
                "Last check failed: GitHub could not be reached.",
            ),
        ];
        for (status, available, last_checked, expected) in cases {
            let snapshot = UpdateSnapshot {
                status,
                last_checked,
                banner: available.clone(),
                available,
                install: kestrel_services::update_install::InstallPhase::Idle,
                can_self_update: false,
            };
            let model = UpdatesViewModel::new(UpdateCheck::Enabled, &snapshot, true, false, now);
            assert_eq!(model.status, expected);
        }
    }

    #[test]
    fn the_consent_card_appears_once_setup_is_out_of_the_way() {
        let snapshot = UpdateSnapshot {
            status: UpdateStatus::Off,
            last_checked: None,
            banner: None,
            available: None,
            install: kestrel_services::update_install::InstallPhase::Idle,
            can_self_update: false,
        };
        let now = at(1_000_000);
        assert!(
            !UpdatesViewModel::new(UpdateCheck::Unasked, &snapshot, false, false, now)
                .show_consent_card
        );
        assert!(
            UpdatesViewModel::new(UpdateCheck::Unasked, &snapshot, true, false, now)
                .show_consent_card
        );
        assert!(
            !UpdatesViewModel::new(UpdateCheck::Disabled, &snapshot, true, false, now)
                .show_consent_card
        );
    }

    #[test]
    fn an_idle_offer_offers_get_update_only_when_the_appimage_can_update_itself() {
        let phase = kestrel_services::update_install::InstallPhase::Idle;
        let self_update = model(phase.clone(), true, true);
        assert_eq!(
            actions(&self_update.banner),
            vec![
                UpdateAction::GetUpdate,
                UpdateAction::ViewRelease,
                UpdateAction::Dismiss
            ]
        );
        assert_eq!(
            self_update.banner.as_ref().and_then(|row| row.suggested),
            Some(UpdateAction::GetUpdate)
        );
        assert_eq!(
            actions(&self_update.install_row),
            vec![UpdateAction::GetUpdate]
        );

        let download_only = model(phase, false, true);
        assert_eq!(
            actions(&download_only.banner),
            vec![UpdateAction::ViewRelease, UpdateAction::Dismiss]
        );
        assert!(download_only.install_row.is_none());
    }

    #[test]
    fn the_banner_returns_for_a_phase_even_when_the_offer_was_dismissed() {
        let mut dismissed = snapshot(
            kestrel_services::update_install::InstallPhase::Downloading {
                received: 1,
                total: 2,
            },
            true,
            true,
        );
        dismissed.banner = None;
        let model = UpdatesViewModel::new(UpdateCheck::Enabled, &dismissed, true, false, at(5));
        assert_eq!(actions(&model.banner), vec![UpdateAction::CancelDownload]);
    }

    #[test]
    fn each_phase_offers_its_own_action() {
        use kestrel_services::update_install::InstallPhase;

        let cases = [
            (
                InstallPhase::Downloading {
                    received: 12,
                    total: 80,
                },
                vec![UpdateAction::CancelDownload],
                vec![UpdateAction::CancelDownload],
                Some("Downloading Kestrel 0.6.0"),
            ),
            (
                InstallPhase::Verifying,
                vec![],
                vec![],
                Some("Checking the download…"),
            ),
            (
                InstallPhase::Ready {
                    version: version("0.6.0"),
                },
                vec![UpdateAction::InstallAndRestart],
                vec![UpdateAction::InstallAndRestart],
                Some("Kestrel 0.6.0 is ready to install"),
            ),
            (
                InstallPhase::Installed {
                    version: version("0.6.0"),
                },
                vec![UpdateAction::RestartToFinish],
                vec![UpdateAction::RestartToFinish],
                Some("Kestrel 0.6.0 is installed"),
            ),
            (
                InstallPhase::Failed("The signature did not match.".to_owned()),
                vec![UpdateAction::TryAgain],
                vec![UpdateAction::TryAgain, UpdateAction::ViewRelease],
                Some("The update did not finish"),
            ),
        ];
        for (phase, install_actions, banner_actions, title) in cases {
            let model = model(phase, true, true);
            assert_eq!(actions(&model.install_row), install_actions);
            assert_eq!(actions(&model.banner), banner_actions);
            if let Some(title) = title {
                assert_eq!(
                    model.banner.as_ref().map(|row| row.title.as_str()),
                    Some(title)
                );
            }
        }
    }

    #[test]
    fn the_downloading_row_names_the_progress() {
        let model = model(
            kestrel_services::update_install::InstallPhase::Downloading {
                received: 12 * 1024 * 1024,
                total: 80 * 1024 * 1024,
            },
            true,
            true,
        );
        let row = model.banner.expect("a download shows its banner");
        assert_eq!(row.percent, Some(15));
        assert!(
            row.subtitle
                .as_deref()
                .is_some_and(|text| text.starts_with("Downloading 12") && text.contains("of 80")),
            "{row:?}"
        );
    }

    #[test]
    fn the_installed_version_wins_over_the_previous_offer() {
        let model = model(
            kestrel_services::update_install::InstallPhase::Ready {
                version: version("0.6.0"),
            },
            true,
            true,
        );
        assert!(matches!(
            model.install,
            UpdateInstallViewModel::Ready { ref version } if version == "0.6.0"
        ));
        assert!(
            model
                .banner
                .as_ref()
                .is_some_and(|row| row.title.contains("0.6.0"))
        );
    }

    #[test]
    fn previous_available_comes_from_the_runtime() {
        let snapshot = snapshot(
            kestrel_services::update_install::InstallPhase::Idle,
            true,
            true,
        );
        let with_previous =
            UpdatesViewModel::new(UpdateCheck::Enabled, &snapshot, true, true, at(5));
        assert!(with_previous.previous_available);
        let without_previous =
            UpdatesViewModel::new(UpdateCheck::Enabled, &snapshot, true, false, at(5));
        assert!(!without_previous.previous_available);
    }
}
