//! Shelf runtime: drop, pin, clear, copy, move, rename, reveal, and opt-in storage.

use super::*;
use kestrel_core::ShelfContent;
use kestrel_platform::clipboard::ClipboardEntryKind;
use kestrel_platform::file_actions::{self, FileIdentity, MoveMethod, MoveOutcome, RevealOutcome};
use kestrel_platform::install_handoff::{self, HandoffOutcome, InstallTarget, Provider};
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
            reveal: file_actions::reveal_support(),
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

    /// Adds a stored screenshot as a file reference, or its pixels when the
    /// capture has no path.
    pub fn shelf_add_capture(&mut self, id: CaptureId) -> Result<usize, String> {
        if let Some(path) = self.capture_path(id) {
            return self.shelf_add(ShelfInput::Files(vec![path]));
        }
        let png = self.capture_png(id)?;
        let image = kestrel_services::capture::image::decode_png(&png)
            .map_err(|error| error.to_string())?;
        self.shelf_add(ShelfInput::Image {
            png,
            width: image.width,
            height: image.height,
        })
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

    /// The path and display name of a file entry an action can change.
    fn shelf_file_entry(&self, id: u64) -> Result<(PathBuf, String), String> {
        if !self.shelf_is_running() {
            return Err("Shelf is off; enable files.shelf in the Feature Hub.".to_owned());
        }
        match self.shelf.items().iter().find(|item| item.id == id) {
            Some(ShelfItem {
                content: ShelfContent::File { path, name },
                ..
            }) => Ok((path.clone(), name.clone())),
            Some(_) => Err("Only a file entry can be moved or renamed.".to_owned()),
            None => Err("That shelf entry is no longer there.".to_owned()),
        }
    }

    /// Moves one file entry into `dest_dir` after confirming its identity.
    pub fn shelf_move_item(
        &mut self,
        id: u64,
        dest_dir: &Path,
        expected: &FileIdentity,
    ) -> Result<String, String> {
        let (path, name) = self.shelf_file_entry(id)?;
        let outcome =
            file_actions::move_to(&path, dest_dir, expected).map_err(|error| error.to_string())?;
        match outcome {
            MoveOutcome::Moved { new_path, method } => {
                self.shelf_point_at(id, &new_path)?;
                Ok(match method {
                    MoveMethod::Renamed => {
                        format!("Moved {name} to {}.", dest_dir.display())
                    }
                    MoveMethod::CopiedAndRemoved => format!(
                        "Copied {name} to {}, checked it, and removed the original.",
                        dest_dir.display()
                    ),
                })
            }
            MoveOutcome::CopiedOriginalKept { new_path, original } => {
                self.shelf_point_at(id, &new_path)?;
                Ok(format!(
                    "Copied {name} to {} but the original at {} could not be removed; both \
                     copies exist.",
                    new_path.display(),
                    original.display()
                ))
            }
        }
    }

    /// Renames one file entry in place after confirming its identity.
    pub fn shelf_rename_item(
        &mut self,
        id: u64,
        new_name: &str,
        expected: &FileIdentity,
    ) -> Result<String, String> {
        let (path, _) = self.shelf_file_entry(id)?;
        let new_path = file_actions::rename_in_place(&path, new_name, expected)
            .map_err(|error| error.to_string())?;
        self.shelf_point_at(id, &new_path)?;
        Ok(format!("Renamed the file to {new_name}."))
    }

    /// Shows one entry in the file manager, or opens its folder.
    pub fn shelf_reveal_item(&self, id: u64) -> Result<String, String> {
        let (path, _) = self.shelf_file_entry(id)?;
        match file_actions::show_items(std::slice::from_ref(&path)) {
            Ok(RevealOutcome::Selected) => Ok("Shown in your file manager.".to_owned()),
            Ok(RevealOutcome::OpenedFolder { provider }) => Ok(match provider {
                Some(provider) => {
                    format!("Opened the folder in {provider}; selecting the file is not available.")
                }
                None => "Opened the folder; selecting the file is not available.".to_owned(),
            }),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Points a moved or renamed entry at its new path and saves when keeping.
    fn shelf_point_at(&mut self, id: u64, new_path: &Path) -> Result<(), String> {
        let name = new_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| new_path.display().to_string());
        self.shelf
            .update_file(id, new_path.to_path_buf(), name)
            .map_err(|error| error.to_string())?;
        self.shelf_save()
    }

    /// The entry as copied or dragged; the window builds the clipboard data.
    pub fn shelf_item(&self, id: u64) -> Option<ShelfItem> {
        self.shelf
            .items()
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    /// Checks a file and resolves the app for its type; work for a worker thread.
    pub fn shelf_install_prepare(
        &self,
        path: &Path,
    ) -> Result<(InstallTarget, Option<Provider>), String> {
        let target = install_handoff::classify(path).map_err(|error| error.to_string())?;
        if !target.kind.is_supported() {
            return Err(
                "Only packages and disk images can be handed to the app that handles them."
                    .to_owned(),
            );
        }
        let provider = install_handoff::provider_for(&target.content_type);
        Ok((target, provider))
    }

    /// Launches the confirmed app; the file is re-checked and left unchanged.
    pub fn shelf_install(
        &self,
        target: &InstallTarget,
        provider: &Provider,
    ) -> Result<String, String> {
        let outcome = install_handoff::hand_off(target, provider, &install_handoff::GioDesktopApps)
            .map_err(|error| error.to_string())?;
        match outcome {
            HandoffOutcome::Launched { provider_name } => Ok(format!(
                "Opened {} with {provider_name}",
                target.display_name()
            )),
        }
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
    fn handoff_refuses_a_file_that_is_not_a_package_or_image() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("notes.txt");
        std::fs::write(&path, b"plain text\n").expect("fixture writes");
        let runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        let error = runtime
            .shelf_install_prepare(&path)
            .expect_err("text is not a package");
        assert!(error.contains("packages and disk images"));
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
