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
}
