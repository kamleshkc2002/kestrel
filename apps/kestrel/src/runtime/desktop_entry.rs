//! The applications-menu entry an AppImage can add for itself.

use crate::{
    desktop_entry::OfferError, runtime::ApplicationRuntime, view_model::DesktopEntryViewModel,
};

impl ApplicationRuntime {
    pub fn desktop_entry_view_model(&self) -> DesktopEntryViewModel {
        DesktopEntryViewModel::new(&self.desktop_entry)
    }

    pub fn add_desktop_entry(&mut self) -> Result<(), OfferError> {
        self.desktop_entry.add()
    }

    pub fn dismiss_desktop_entry(&mut self) -> std::io::Result<()> {
        self.desktop_entry.dismiss()
    }

    pub fn remove_desktop_entry(&mut self) -> Result<(), OfferError> {
        self.desktop_entry.remove()
    }

    /// Rewrites a managed entry whose AppImage path changed.
    pub(crate) fn refresh_desktop_entry(&mut self) {
        if let Err(error) = self.desktop_entry.refresh_entry() {
            log::warn!(
                target: "kestrel",
                "The applications-menu entry could not be updated: {error}"
            );
        }
    }
}
