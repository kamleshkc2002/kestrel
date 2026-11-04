//! Consent, scheduling, and results for the optional update check.

use std::time::SystemTime;

use kestrel_core::{ReleaseInfo, UpdateCheck, newest_offer};
use kestrel_platform::updates::UpdateFetchError;
use kestrel_services::updates::UpdateSnapshot;
use semver::Version;

use super::ApplicationRuntime;
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
    ) -> Result<Option<ReleaseInfo>, String> {
        if !self.update_service.is_enabled() {
            return Ok(None);
        }
        let outcome = match result {
            Ok(releases) => {
                let offer = newest_offer(&current_version(), &releases);
                match &offer {
                    Some(release) => log::debug!(
                        target: "kestrel.updates",
                        "an update is available: {}",
                        release.version
                    ),
                    None => log::debug!(target: "kestrel.updates", "no newer release is published"),
                }
                self.update_service.record_success(now, offer.clone());
                Ok(offer)
            }
            Err(error) => {
                log::warn!(target: "kestrel.updates", "update check failed: {error}");
                self.update_service.record_failure(now, error.to_string());
                Err(error.to_string())
            }
        };
        self.persist_update_state();
        outcome
    }

    /// Records a check that could not start, so a worker failure does not
    /// leave the state looking in progress.
    pub fn abandon_update_check(&mut self, now: SystemTime) {
        if !self.update_service.is_enabled() {
            return;
        }
        self.update_service
            .record_failure(now, "The update check could not start.");
        self.persist_update_state();
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
        if let Err(error) = crate::update_state::write(
            self.update_state_path.as_deref(),
            &self.update_service.stored(),
        ) {
            log::warn!(target: "kestrel.updates", "could not record the update check: {error}");
        }
    }
}

fn current_version() -> Version {
    Version::parse(CURRENT_VERSION).expect("the build version is valid semver")
}
