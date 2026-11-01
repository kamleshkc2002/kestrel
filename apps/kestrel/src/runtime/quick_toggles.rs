//! Quick-toggle runtime features.

use super::*;

impl ApplicationRuntime {
    pub fn quick_toggle_snapshots(&self) -> impl Iterator<Item = &QuickToggleSnapshot> {
        self.quick_toggles.snapshots()
    }

    /// Applies a quick-toggle command only while its independent adapter is running.
    pub fn execute_quick_toggle_command(
        &mut self,
        command: QuickToggleCommand,
    ) -> Option<Result<QuickToggleSnapshot, QuickToggleError>> {
        if !self.quick_toggle_is_running(command.id) {
            return None;
        }
        Some(self.quick_toggles.execute(command).cloned())
    }

    pub(super) fn refresh_quick_toggles(&mut self) {
        for id in ALL_QUICK_TOGGLES {
            if self.quick_toggle_is_running(id) {
                self.quick_toggles.refresh(id);
            }
        }
    }

    pub(super) fn quick_toggle_is_running(&self, id: QuickToggleId) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == id.feature_id() && registration.running)
    }

    /// Re-reads the keyboard light; returns its view model when the reading changed.
    pub fn refresh_keyboard_light(&mut self) -> Option<QuickToggleViewModel> {
        let id = QuickToggleId::KeyboardLight;
        if !self.quick_toggle_is_running(id) {
            return None;
        }
        let before = self
            .quick_toggles
            .snapshots()
            .find(|snapshot| snapshot.id == id)
            .cloned();
        let after = self.quick_toggles.refresh(id).clone();
        if before.as_ref() == Some(&after) {
            return None;
        }
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == id.feature_id());
        Some(QuickToggleViewModel::from_snapshot(&after, registration))
    }

    /// The length the next keep-awake session uses.
    pub fn keep_awake_choice(&self) -> KeepAwakeDuration {
        self.keep_awake_choice
    }

    /// Starts or restarts a keep-awake session with `duration`.
    pub fn start_keep_awake(
        &mut self,
        duration: KeepAwakeDuration,
    ) -> Option<Result<QuickToggleSnapshot, QuickToggleError>> {
        if !self.quick_toggle_is_running(QuickToggleId::KeepAwake) {
            return None;
        }
        self.keep_awake_choice = duration;
        Some(
            self.quick_toggles
                .start_keep_awake(duration, kestrel_platform::clock::boot_time())
                .cloned(),
        )
    }

    /// Releases the inhibitor; false when keep awake is not running.
    pub fn stop_keep_awake(&mut self) -> bool {
        if !self.quick_toggle_is_running(QuickToggleId::KeepAwake) {
            return false;
        }
        self.quick_toggles.stop_keep_awake();
        true
    }

    /// Stores the choice, restarting a running session from now.
    pub fn set_keep_awake_choice(
        &mut self,
        duration: KeepAwakeDuration,
    ) -> Option<Result<QuickToggleSnapshot, QuickToggleError>> {
        self.keep_awake_choice = duration;
        if !self.keep_awake_is_active() {
            return None;
        }
        Some(
            self.quick_toggles
                .start_keep_awake(duration, kestrel_platform::clock::boot_time())
                .cloned(),
        )
    }

    /// Ends a timed session at its deadline; true once.
    pub fn expire_keep_awake(&mut self, now: Duration) -> bool {
        self.quick_toggles.expire_keep_awake(now)
    }

    /// Whether an inhibitor is held.
    pub fn keep_awake_is_active(&self) -> bool {
        self.quick_toggles
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::KeepAwake)
            .is_some_and(|snapshot| snapshot.keep_awake.is_some())
    }

    /// Whether the running session has a deadline.
    pub fn keep_awake_is_timed(&self) -> bool {
        self.quick_toggles
            .snapshots()
            .find(|snapshot| snapshot.id == QuickToggleId::KeepAwake)
            .and_then(|snapshot| snapshot.keep_awake)
            .is_some_and(|session| session.deadline.is_some())
    }

    /// The keep-awake row as it reads now.
    pub fn keep_awake_view_model(&self) -> Option<QuickToggleViewModel> {
        let id = QuickToggleId::KeepAwake;
        let snapshot = self
            .quick_toggles
            .snapshots()
            .find(|snapshot| snapshot.id == id)?;
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == id.feature_id());
        let mut model = QuickToggleViewModel::from_snapshot(snapshot, registration);
        model.attach_keep_awake(
            snapshot,
            self.keep_awake_choice,
            kestrel_platform::clock::boot_time(),
        );
        Some(model)
    }
}
