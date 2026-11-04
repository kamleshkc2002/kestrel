//! Consent, scheduling, and results for the optional update check.

use std::time::SystemTime;

use kestrel_core::{ReleaseInfo, UpdateCheck, newest_offer};
use kestrel_platform::updates::UpdateFetchError;
use kestrel_services::updates::UpdateSnapshot;
use semver::Version;

use super::ApplicationRuntime;
use crate::UpdateCheckOutcome;
use crate::whats_new::CURRENT_VERSION;

impl ApplicationRuntime {
    pub fn update_check(&self) -> UpdateCheck {
        self.update_service.check()
    }

    pub fn update_snapshot(&self) -> UpdateSnapshot {
        self.update_service.snapshot()
    }

    /// Setup finished or was skipped, so the Overview card may ask.
    pub fn mark_setup_done(&mut self) {
        self.setup_done = true;
    }

    pub fn set_update_check(&mut self, check: UpdateCheck) {
        self.update_service.set_check(check);
    }

    pub fn update_check_enabled(&self) -> bool {
        self.update_service.is_enabled()
    }

    pub fn update_check_due(&self, now: SystemTime) -> bool {
        self.update_service.is_due(now)
    }

    /// Marks a check as running; `false` means consent is absent and nothing
    /// may be contacted.
    pub fn begin_update_check(&mut self, now: SystemTime) -> bool {
        self.update_service.begin_check(now)
    }

    /// Records a finished check. A result that arrives after consent was
    /// withdrawn is ignored, and a failure keeps the last offer.
    pub fn finish_update_check(
        &mut self,
        now: SystemTime,
        result: Result<Vec<ReleaseInfo>, UpdateFetchError>,
    ) -> UpdateCheckOutcome {
        if !self.update_service.is_enabled() {
            return UpdateCheckOutcome::Ignored;
        }
        let outcome = match result {
            Ok(releases) => {
                let offer = newest_offer(&current_version(), &releases);
                match &offer {
                    Some(release) => {
                        log::debug!(
                            target: "kestrel.updates",
                            "an update is available: {}",
                            release.version
                        );
                        self.update_service
                            .record_success(now, Some(release.clone()));
                        UpdateCheckOutcome::Available(release.clone())
                    }
                    None => {
                        log::debug!(target: "kestrel.updates", "no newer release is published");
                        self.update_service.record_success(now, None);
                        UpdateCheckOutcome::UpToDate
                    }
                }
            }
            Err(error) => {
                log::warn!(target: "kestrel.updates", "update check failed: {error}");
                let message = error.to_string();
                self.update_service.record_failure(now, message.clone());
                UpdateCheckOutcome::Failed(message)
            }
        };
        self.persist_update_state();
        outcome
    }

    /// Writes the attempt that `begin_update_check` set, so the daily
    /// interval survives a crash and an unwritable state file is noticed
    /// before the network is contacted.
    pub fn record_update_attempt(&mut self) -> Result<(), String> {
        self.write_update_state()
    }

    /// Records a check that could not start, so a worker failure does not
    /// leave the state looking in progress.
    pub fn abandon_update_check(
        &mut self,
        now: SystemTime,
        message: impl Into<String>,
    ) -> UpdateCheckOutcome {
        if !self.update_service.is_enabled() {
            return UpdateCheckOutcome::Ignored;
        }
        let message = message.into();
        self.update_service.record_failure(now, message.clone());
        self.persist_update_state();
        UpdateCheckOutcome::Failed(message)
    }

    /// Hides the banner for the offered version; the tray and Settings keep it.
    pub fn dismiss_update(&mut self) -> bool {
        if !self.update_service.dismiss_available() {
            return false;
        }
        self.persist_update_state();
        true
    }

    fn persist_update_state(&self) {
        if let Err(error) = self.write_update_state() {
            log::warn!(target: "kestrel.updates", "could not record the update check: {error}");
        }
    }

    fn write_update_state(&self) -> Result<(), String> {
        crate::update_state::write(
            self.update_state_path.as_deref(),
            &self.update_service.stored(),
        )
        .map_err(|error| error.to_string())
    }
}

fn current_version() -> Version {
    Version::parse(CURRENT_VERSION).expect("the build version is valid semver")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, UNIX_EPOCH},
    };

    use kestrel_core::{ApplicationConfiguration, UpdateCheck};
    use kestrel_services::updates::StoredUpdateState;

    use super::ApplicationRuntime;

    fn enabled_runtime(state_path: &Path) -> ApplicationRuntime {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.set_update_check(UpdateCheck::Enabled);
        runtime.update_state_path = Some(state_path.to_path_buf());
        runtime
    }

    #[test]
    fn the_attempt_reaches_the_state_file_before_any_result() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("update-check");
        let mut runtime = enabled_runtime(&path);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(runtime.begin_update_check(now));
        runtime
            .record_update_attempt()
            .expect("the attempt is recorded");
        assert_eq!(
            crate::update_state::read(Some(&path)),
            StoredUpdateState {
                last_attempt: Some(now),
                ..StoredUpdateState::default()
            }
        );
    }

    #[test]
    fn an_unwritable_state_path_reports_the_failure() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let blocker = directory.path().join("blocker");
        fs::write(&blocker, b"not a directory").expect("the blocker file writes");
        let mut runtime = enabled_runtime(&blocker.join("update-check"));
        assert!(runtime.begin_update_check(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
        assert!(runtime.record_update_attempt().is_err());
    }
}
