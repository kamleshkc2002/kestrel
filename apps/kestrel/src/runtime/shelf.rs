//! Shelf runtime: drop, pin, clear, copy, and opt-in storage.

use super::*;
use kestrel_platform::clipboard::ClipboardEntryKind;
use kestrel_services::shelf::{ShelfInput, ShelfService};

impl ApplicationRuntime {
    pub fn shelf_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SHELF_ID && registration.running)
    }

    pub fn shelf_view_model(&self) -> ShelfViewModel {
        let running = self.shelf_is_running();
        ShelfViewModel::from_presentation(ShelfPresentation {
            running,
            keep: self.shelf_keep,
            snapshot: running.then(|| self.shelf.snapshot()),
            limits: self.shelf_policy,
            warning: None,
        })
    }

    /// Applies the age limit to unpinned entries before presenting them.
    pub fn shelf_expire(&mut self) -> usize {
        self.shelf.expire(SystemTime::now())
    }

    fn shelf_service(&mut self) -> Result<&mut ShelfService, String> {
        if !self.shelf_is_running() {
            return Err("Shelf is off; enable files.shelf in the Feature Hub.".to_owned());
        }
        Ok(&mut self.shelf)
    }

    pub fn shelf_add(&mut self, input: ShelfInput) -> Result<usize, String> {
        let now = SystemTime::now();
        self.shelf_service()?.expire(now);
        let added = self
            .shelf_service()?
            .add(input, now)
            .map_err(|error| error.to_string())?;
        self.shelf_save()?;
        Ok(added)
    }

    /// Reads the current selection once: files, then text or a link.
    pub fn shelf_add_clipboard(&mut self) -> Result<usize, String> {
        if !self.shelf_is_running() {
            return Err("Shelf is off; enable files.shelf in the Feature Hub.".to_owned());
        }
        let max_files = self.shelf_policy.max_items as usize;
        let backend = self
            .session_clipboard
            .as_mut()
            .ok_or_else(|| "No clipboard connection is available in this session.".to_owned())?;
        let kind = backend.offered_kind().unwrap_or(None);
        if kind == Some(ClipboardEntryKind::Files) {
            if let Ok(Some(paths)) = backend.read_file_list(max_files) {
                let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
                return self.shelf_add(ShelfInput::Files(paths));
            }
        }
        let text = backend
            .read_text()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "The clipboard holds no text, link, or files to add.".to_owned())?;
        self.shelf_add(ShelfInput::Text(text))
    }

    pub fn shelf_remove(&mut self, id: u64) -> Result<(), String> {
        self.shelf_service()?
            .remove(id)
            .map_err(|error| error.to_string())?;
        self.shelf_save()
    }

    pub fn shelf_set_pinned(&mut self, id: u64, pinned: bool) -> Result<(), String> {
        self.shelf_service()?
            .set_pinned(id, pinned)
            .map_err(|error| error.to_string())?;
        self.shelf_save()
    }

    pub fn shelf_clear(&mut self, include_pinned: bool) -> Result<usize, String> {
        let removed = self.shelf_service()?.clear(include_pinned);
        self.shelf_save()?;
        Ok(removed)
    }

    /// The entry as copied or dragged; the window builds the clipboard data.
    pub fn shelf_item(&self, id: u64) -> Option<ShelfItem> {
        self.shelf
            .items()
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    /// Turns keeping entries across restarts on or off, syncing the file.
    pub fn shelf_set_keep(&mut self, keep: bool) -> Result<(), String> {
        self.shelf_keep = keep;
        if keep {
            self.shelf_save()
        } else {
            let Some(path) = self.shelf_path.clone() else {
                return Ok(());
            };
            crate::shelf_store::remove_file(&path)
        }
    }

    pub(super) fn shelf_save(&self) -> Result<(), String> {
        if !self.shelf_keep {
            return Ok(());
        }
        let Some(path) = self.shelf_path.as_deref() else {
            return Err(
                "No writable data directory was resolved, so the shelf cannot be saved. \
                 Set XDG_DATA_HOME or HOME."
                    .to_owned(),
            );
        };
        crate::shelf_store::save(path, &self.shelf.persistable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ApplicationConfiguration {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SHELF_ID, true)
            .expect("a valid feature ID");
        configuration
    }

    #[test]
    fn disabled_shelf_refuses_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let error = runtime
            .shelf_add(ShelfInput::Text("test".to_owned()))
            .expect_err("disabled feature");
        assert!(error.contains(SHELF_ID));
    }

    #[test]
    fn enabled_shelf_adds_pins_and_clears_without_touching_disk() {
        let mut runtime = ApplicationRuntime::new(&enabled()).expect("runtime builds");
        runtime.start();
        assert_eq!(
            runtime
                .shelf_add(ShelfInput::Text("hello".to_owned()))
                .expect("the shelf is on"),
            1
        );
        let id = runtime.shelf.items()[0].id;
        runtime.shelf_set_pinned(id, true).expect("pinning works");
        assert_eq!(runtime.shelf_clear(false).expect("clear works"), 0);
        assert_eq!(runtime.shelf.items().len(), 1);
        assert_eq!(runtime.shelf_clear(true).expect("clear works"), 1);
        assert!(runtime.shelf.items().is_empty());
    }
}
