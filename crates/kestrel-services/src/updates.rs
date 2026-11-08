//! Policy and bounded state for the opt-in daily update check.

use std::time::{Duration, SystemTime};

use kestrel_core::{ReleaseInfo, UpdateCheck, newest_offer};
use semver::Version;

use crate::update_install::{self, InstallPhase};

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
    /// The explicit download and install step, when one was asked for.
    pub install: InstallPhase,
    /// Kestrel runs from a writable AppImage and the offer publishes one.
    pub can_self_update: bool,
}

/// What survives a restart: the last attempt, the last result, and the
/// dismissed version. A release's install assets are not stored; the next
/// check re-fetches them, so a restored offer shows its version without
/// offering a download until then.
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
    install: InstallPhase,
    /// The running file can be replaced by an AppImage update.
    self_update_available: bool,
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
            install: InstallPhase::Idle,
            self_update_available: false,
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

    /// Records whether this launch can replace the AppImage it runs from.
    pub fn set_self_update_available(&mut self, available: bool) {
        self.self_update_available = available;
    }

    /// Whether the surfaces may offer a download: the running file is a
    /// writable AppImage and the offer publishes signed assets.
    pub fn can_self_update(&self) -> bool {
        self.self_update_available
            && self
                .latest
                .as_ref()
                .is_some_and(|release| release.install.is_some())
    }

    /// The explicit download and install step.
    pub fn install_phase(&self) -> &InstallPhase {
        &self.install
    }

    /// The offered release, as the last check reported it.
    pub fn offer(&self) -> Option<&ReleaseInfo> {
        self.latest.as_ref()
    }

    /// Starts a download after an explicit command; `false` means the running
    /// build cannot replace itself, has no offer with signed assets, or a step
    /// is already running.
    pub fn begin_install_download(&mut self) -> bool {
        if !self.can_self_update() {
            return false;
        }
        match update_install::download_started(&self.install, self.latest.as_ref()) {
            Some(phase) => {
                self.install = phase;
                true
            }
            None => false,
        }
    }

    pub fn record_download_progress(&mut self, received: u64, total: u64) {
        if let Some(phase) = update_install::download_progress(&self.install, received, total) {
            self.install = phase;
        }
    }

    /// The files are on disk and their signature is being checked.
    pub fn begin_install_verify(&mut self) -> bool {
        match update_install::verify_started(&self.install) {
            Some(phase) => {
                self.install = phase;
                true
            }
            None => false,
        }
    }

    /// The signature matched, so the install command may run.
    pub fn record_install_verified(&mut self, version: Version) -> bool {
        match update_install::verify_finished(&self.install, &version) {
            Some(phase) => {
                self.install = phase;
                true
            }
            None => false,
        }
    }

    /// The new AppImage replaced the running one; a restart finishes it.
    pub fn record_installed(&mut self) -> bool {
        match update_install::install_finished(&self.install) {
            Some(phase) => {
                self.install = phase;
                true
            }
            None => false,
        }
    }

    pub fn record_install_failure(&mut self, reason: impl Into<String>) {
        if let Some(phase) = update_install::failed(&self.install, reason) {
            self.install = phase;
        }
    }

    /// A cancelled download or verification returns to Idle.
    pub fn cancel_install(&mut self) {
        if let Some(phase) = update_install::cancelled(&self.install) {
            self.install = phase;
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
            install: self.install.clone(),
            can_self_update: self.can_self_update(),
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
    use crate::update_install::InstallPhase;

    fn at(seconds: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).expect("test version parses"),
            url: format!("https://example.invalid/{version}"),
            prerelease: version.contains('-'),
            install: None,
        }
    }

    fn installable(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            install: Some(kestrel_core::InstallAssets {
                appimage_url: "https://example.invalid/download".to_owned(),
                appimage_size: 42,
                signature_url: "https://example.invalid/download.minisig".to_owned(),
                file_name: format!("Kestrel-{version}-x86_64.AppImage"),
            }),
            ..release(version)
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

    #[test]
    fn the_install_step_follows_only_explicit_commands() {
        let now = at(6_000_000);
        let mut service = UpdateService::new(
            UpdateCheck::Enabled,
            StoredUpdateState {
                last_attempt: Some(now),
                last_checked: Some(now),
                latest: Some(installable("0.6.0")),
                dismissed: None,
            },
            &current("0.6.0-preview.4"),
        );
        assert!(
            !service.can_self_update(),
            "only an AppImage may self-update"
        );
        service.set_self_update_available(true);
        assert!(service.can_self_update());

        assert!(service.begin_install_download(), "the command starts it");
        assert_eq!(
            service.install_phase(),
            &InstallPhase::Downloading {
                received: 0,
                total: 42
            }
        );
        assert!(
            !service.begin_install_download(),
            "a second download is rejected"
        );

        // A scheduled check finishing mid-download changes nothing.
        service.record_success(now, Some(installable("0.6.0")));
        assert!(matches!(
            service.install_phase(),
            InstallPhase::Downloading { .. }
        ));

        service.record_download_progress(21, 42);
        assert_eq!(
            service.install_phase(),
            &InstallPhase::Downloading {
                received: 21,
                total: 42
            }
        );
        assert!(
            !service.record_installed(),
            "install before verifying fails"
        );
        assert!(service.begin_install_verify());
        service.record_success(now, Some(installable("0.6.0")));
        assert_eq!(service.install_phase(), &InstallPhase::Verifying);
        assert!(service.record_install_verified(current("0.6.0")));
        assert_eq!(
            service.install_phase(),
            &InstallPhase::Ready {
                version: current("0.6.0")
            }
        );
        service.record_success(now, Some(installable("0.6.0")));
        assert!(
            matches!(service.install_phase(), InstallPhase::Ready { .. }),
            "a check result does not reset a ready download"
        );
        assert!(service.record_installed());
        assert_eq!(
            service.install_phase(),
            &InstallPhase::Installed {
                version: current("0.6.0")
            }
        );
        assert!(service.snapshot().can_self_update);
        assert_eq!(service.snapshot().install, *service.install_phase());
    }

    #[test]
    fn a_download_without_assets_is_rejected_and_cancel_returns_to_idle() {
        let now = at(7_000_000);
        let mut service = UpdateService::new(
            UpdateCheck::Enabled,
            StoredUpdateState {
                latest: Some(release("0.6.0")),
                ..StoredUpdateState::default()
            },
            &current("0.6.0-preview.4"),
        );
        service.set_self_update_available(true);
        assert!(
            !service.begin_install_download(),
            "an offer without signed assets cannot download"
        );
        assert!(!service.can_self_update());
        assert_eq!(service.install_phase(), &InstallPhase::Idle);

        service.record_success(now, Some(installable("0.6.0")));
        assert!(service.begin_install_download());
        service.record_install_failure("The download failed.");
        assert_eq!(
            service.install_phase(),
            &InstallPhase::Failed("The download failed.".to_owned())
        );
        assert!(
            service.begin_install_download(),
            "a failed download can be retried"
        );
        service.cancel_install();
        assert_eq!(service.install_phase(), &InstallPhase::Idle);
        service.cancel_install();
        assert_eq!(service.install_phase(), &InstallPhase::Idle);
    }
}
