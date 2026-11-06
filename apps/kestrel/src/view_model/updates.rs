//! Update-check presentation for Overview, the tray, and Settings.

use std::time::SystemTime;

use kestrel_core::{ReleaseInfo, UpdateCheck};
use kestrel_services::updates::{UpdateSnapshot, UpdateStatus};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdatesViewModel {
    pub check: UpdateCheck,
    /// The one-time Overview question, before any answer is saved.
    pub show_consent_card: bool,
    /// The Overview banner; hidden while its version was dismissed.
    pub banner: Option<UpdateOfferViewModel>,
    /// The tray and Settings offer, which stays after a dismissal.
    pub available: Option<UpdateOfferViewModel>,
    /// The Settings status line.
    pub status: String,
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

impl UpdatesViewModel {
    pub(crate) fn new(
        check: UpdateCheck,
        snapshot: &UpdateSnapshot,
        setup_done: bool,
        now: SystemTime,
    ) -> Self {
        Self {
            check,
            show_consent_card: check == UpdateCheck::Unasked && setup_done,
            banner: snapshot.banner.as_ref().map(UpdateOfferViewModel::new),
            available: snapshot.available.as_ref().map(UpdateOfferViewModel::new),
            status: status_text(snapshot, now),
        }
    }
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

    use super::UpdatesViewModel;

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
            let model = UpdatesViewModel::new(UpdateCheck::Enabled, &snapshot, true, now);
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
            !UpdatesViewModel::new(UpdateCheck::Unasked, &snapshot, false, now).show_consent_card
        );
        assert!(
            UpdatesViewModel::new(UpdateCheck::Unasked, &snapshot, true, now).show_consent_card
        );
        assert!(
            !UpdatesViewModel::new(UpdateCheck::Disabled, &snapshot, true, now).show_consent_card
        );
    }
}
