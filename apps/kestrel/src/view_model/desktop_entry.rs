//! The applications-menu offer for Overview and Settings.

use crate::desktop_entry::DesktopEntry;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopEntryViewModel {
    /// Overview asks before the entry is written.
    pub banner: bool,
    pub can_add: bool,
    pub can_remove: bool,
}

impl DesktopEntryViewModel {
    pub(crate) fn new(entry: &DesktopEntry) -> Self {
        Self {
            banner: entry.offers_banner(),
            can_add: entry.can_add(),
            can_remove: entry.can_remove(),
        }
    }
}
