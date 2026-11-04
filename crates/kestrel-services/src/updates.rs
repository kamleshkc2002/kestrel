//! Policy and bounded state for the opt-in daily update check.

use std::time::{Duration, SystemTime};

use kestrel_core::{ReleaseInfo, UpdateCheck, newest_offer};
use semver::Version;

/// How often an enabled check may contact GitHub, counted from the last
/// attempt whichever way it ended.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Consent has not been given, or it was withdrawn.
    Off,
    NotChecked,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Failed(String),
}

/// What Overview, the tray, and Settings show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSnapshot {
    pub status: UpdateStatus,
    pub last_checked: Option<SystemTime>,
    /// The banner hides while the offered version was dismissed.
    pub banner: Option<ReleaseInfo>,
    /// The offer stays visible in the tray and Settings after a dismissal.
    pub available: Option<ReleaseInfo>,
}

/// What survives a restart: the last attempt, the last result, and the
/// dismissed version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredUpdateState {
    pub last_attempt: Option<SystemTime>,
    pub last_checked: Option<SystemTime>,
    pub latest: Option<ReleaseInfo>,
    pub dismissed: Option<Version>,
}

/// Whether `now` is at least a day after the last attempt. A time in the
/// future is not due, so a clock change cannot start a second check.
pub fn is_due(now: SystemTime, last_attempt: Option<SystemTime>) -> bool {
    match last_attempt {
        None => true,
        Some(last) => now
            .duration_since(last)
            .is_ok_and(|elapsed| elapsed >= CHECK_INTERVAL),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateService {
    check: UpdateCheck,
    last_attempt: Option<SystemTime>,
    last_checked: Option<SystemTime>,
    latest: Option<ReleaseInfo>,
    dismissed: Option<Version>,
    status: UpdateStatus,
}

impl UpdateService {
    /// A saved offer is kept only while `current` still needs it, so an
    /// installed release stops being offered at the next launch.
    pub fn new(check: UpdateCheck, stored: StoredUpdateState, current: &Version) -> Self {
        let mut service = Self {
            check,
            last_attempt: stored.last_attempt,
            last_checked: stored.last_checked,
            latest: stored
                .latest
                .and_then(|release| newest_offer(current, &[release])),
            dismissed: stored.dismissed,
            status: UpdateStatus::Off,
        };
        service.restore_status();
        service
    }

    pub fn check(&self) -> UpdateCheck {
        self.check
    }

    pub fn is_enabled(&self) -> bool {
        self.check == UpdateCheck::Enabled
    }

    /// Applies a consent change; an in-flight result is ignored later.
    pub fn set_check(&mut self, check: UpdateCheck) {
        self.check = check;
        self.restore_status();
    }

    pub fn is_due(&self, now: SystemTime) -> bool {
        self.is_enabled() && is_due(now, self.last_attempt)
    }

    /// The due check starts; `false` means consent is absent, so nothing
    /// must be contacted.
    pub fn begin_check(&mut self, now: SystemTime) -> bool {
        if !self.is_enabled() {
            return false;
        }
        self.last_attempt = Some(now);
        self.status = UpdateStatus::Checking;
        true
    }

    /// A cancelled check keeps the last result and only drops the
    /// in-progress status.
    pub fn cancel_check(&mut self) {
        self.restore_status();
    }

    pub fn record_success(&mut self, now: SystemTime, offer: Option<ReleaseInfo>) {
        self.last_attempt = Some(now);
        self.last_checked = Some(now);
        self.latest = offer;
        self.status = self.result_status();
    }

    /// A failure keeps the last known offer and its check time.
    pub fn record_failure(&mut self, now: SystemTime, error: impl Into<String>) {
        self.last_attempt = Some(now);
        self.status = UpdateStatus::Failed(error.into());
    }

    pub fn dismiss(&mut self, version: Version) {
        self.dismissed = Some(version);
    }

    /// Dismisses the offer on screen, if there is one.
    pub fn dismiss_available(&mut self) -> bool {
        match &self.latest {
            Some(release) => {
                self.dismiss(release.version.clone());
                true
            }
            None => false,
        }
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        let banner = self
            .latest
            .clone()
            .filter(|release| self.dismissed.as_ref() != Some(&release.version));
        UpdateSnapshot {
            status: self.status.clone(),
            last_checked: self.last_checked,
            banner,
            available: self.latest.clone(),
        }
    }

    pub fn stored(&self) -> StoredUpdateState {
        StoredUpdateState {
            last_attempt: self.last_attempt,
            last_checked: self.last_checked,
            latest: self.latest.clone(),
            dismissed: self.dismissed.clone(),
        }
    }

    fn restore_status(&mut self) {
        self.status = if self.is_enabled() {
            self.result_status()
        } else {
            UpdateStatus::Off
        };
    }

    fn result_status(&self) -> UpdateStatus {
        match (&self.latest, self.last_checked) {
            (Some(release), _) => UpdateStatus::Available(release.clone()),
            (None, Some(_)) => UpdateStatus::UpToDate,
            (None, None) => UpdateStatus::NotChecked,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use kestrel_core::{ReleaseInfo, UpdateCheck};
    use semver::Version;

    use super::{CHECK_INTERVAL, StoredUpdateState, UpdateService, UpdateStatus, is_due};

    fn at(seconds: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).expect("test version parses"),
            url: format!("https://example.invalid/{version}"),
            prerelease: version.contains('-'),
        }
    }

    fn current(version: &str) -> Version {
        Version::parse(version).expect("test version parses")
    }

    #[test]
    fn due_checks_run_once_a_day_from_the_last_attempt() {
        let start = at(1_000_000);
        assert!(is_due(start, None), "a missing attempt is due");
        for (elapsed, due) in [
            (Duration::ZERO, false),
            (CHECK_INTERVAL - Duration::from_secs(1), false),
            (CHECK_INTERVAL, true),
            (CHECK_INTERVAL + Duration::from_secs(1), true),
        ] {
            assert_eq!(is_due(start + elapsed, Some(start)), due, "{elapsed:?}");
        }
        assert!(
            !is_due(start - Duration::from_secs(1), Some(start)),
            "a clock moved back does not repeat the check"
        );
    }

    #[test]
    fn dismissing_hides_only_that_version() {
        let now = at(2_000_000);
        let mut service = UpdateService::new(
            UpdateCheck::Enabled,
            StoredUpdateState {
                last_attempt: Some(now),
                last_checked: Some(now),
                latest: Some(release("0.6.0-preview.5")),
                dismissed: None,
            },
            &current("0.6.0-preview.4"),
        );
        assert_eq!(
            service
                .snapshot()
                .banner
                .map(|offer| offer.version.to_string()),
            Some("0.6.0-preview.5".to_owned())
        );

        service.dismiss(Version::parse("0.6.0-preview.5").expect("version parses"));
        let snapshot = service.snapshot();
        assert!(snapshot.banner.is_none(), "the dismissed banner is hidden");
        assert_eq!(
            snapshot.available.map(|offer| offer.version.to_string()),
            Some("0.6.0-preview.5".to_owned()),
            "the tray and Settings still see the offer"
        );
        assert_eq!(
            snapshot.status,
            UpdateStatus::Available(release("0.6.0-preview.5"))
        );

        service.record_success(now, Some(release("0.6.0-preview.6")));
        assert_eq!(
            service
                .snapshot()
                .banner
                .map(|offer| offer.version.to_string()),
            Some("0.6.0-preview.6".to_owned()),
            "a newer version shows the banner again"
        );
    }

    #[test]
    fn a_failure_keeps_the_last_known_offer() {
        let now = at(3_000_000);
        let mut service = UpdateService::new(
            UpdateCheck::Enabled,
            StoredUpdateState::default(),
            &current("0.6.0-preview.4"),
        );
        service.record_success(now, Some(release("0.6.0")));
        let later = now + Duration::from_secs(60);
        service.record_failure(later, "GitHub could not be reached.");

        let snapshot = service.snapshot();
        assert_eq!(
            snapshot.status,
            UpdateStatus::Failed("GitHub could not be reached.".to_owned())
        );
        assert_eq!(
            snapshot.available.map(|offer| offer.version.to_string()),
            Some("0.6.0".to_owned())
        );
        assert_eq!(
            snapshot.banner.map(|offer| offer.version.to_string()),
            Some("0.6.0".to_owned())
        );
        assert_eq!(snapshot.last_checked, Some(now), "the success time stays");
    }

    #[test]
    fn consent_gates_every_check() {
        let now = at(4_000_000);
        for check in [UpdateCheck::Unasked, UpdateCheck::Disabled] {
            let mut service =
                UpdateService::new(check, StoredUpdateState::default(), &current("0.6.0"));
            assert!(!service.is_due(now), "{check:?} is never due");
            assert!(!service.begin_check(now), "{check:?} cannot start a check");
            assert_eq!(service.snapshot().status, UpdateStatus::Off);
        }
    }

    #[test]
    fn a_restart_keeps_only_offers_the_running_version_still_needs() {
        let now = at(5_000_000);
        let mut service = UpdateService::new(
            UpdateCheck::Enabled,
            StoredUpdateState::default(),
            &current("0.6.0-preview.4"),
        );
        service.record_success(now, Some(release("0.6.0")));
        service.dismiss_available();
        let stored = service.stored();
        let restored =
            UpdateService::new(UpdateCheck::Enabled, stored, &current("0.6.0-preview.4"));
        assert!(restored.snapshot().banner.is_none());
        assert!(restored.snapshot().available.is_some());

        for (running, offered, kept) in [
            ("0.6.0-preview.4", "0.6.0-preview.5", true),
            ("0.6.0-preview.5", "0.6.0-preview.5", false),
            ("0.6.0-preview.6", "0.6.0-preview.5", false),
            ("0.6.0", "0.6.0-preview.5", false),
            ("0.6.0-preview.4", "0.6.0", true),
        ] {
            let restored = UpdateService::new(
                UpdateCheck::Enabled,
                StoredUpdateState {
                    latest: Some(release(offered)),
                    ..StoredUpdateState::default()
                },
                &current(running),
            );
            let case = format!("{running} with a saved {offered}");
            assert_eq!(restored.stored().latest.is_some(), kept, "{case}");
            assert_eq!(
                restored.snapshot().status == UpdateStatus::Available(release(offered)),
                kept,
                "{case}"
            );
        }
    }
}
