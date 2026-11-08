//! Clipboard history runtime features.

use super::*;

/// What a manual clean of the clipboard's single link did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanLinkOutcome {
    /// Tracking parameters were removed and the cleaned link replaced it.
    Removed(usize),
    /// The link had nothing to remove.
    Nothing,
    /// The clipboard holds no single http(s) link, or it changed during the read.
    NotSingleLink,
}

impl ApplicationRuntime {
    pub fn clipboard_is_running(&self) -> bool {
        self.clipboard_history_is_running()
    }

    /// Builds clipboard presentation state for targeted updates.
    pub fn clipboard_view_model(
        &self,
        configuration: &ApplicationConfiguration,
        query: &str,
        matches: &[ClipboardMatch],
        preview: Option<&ClipboardPreview>,
    ) -> ClipboardViewModel {
        ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(self.clipboard_history.latest()),
                running: self.clipboard_history_is_running(),
                search_query: query,
                matches,
                preview,
                ocr_unavailable: clipboard_ocr_unavailable(),
                paste_unavailable: self.clipboard_paste_unavailable(),
            },
        )
    }

    /// Reads the text in an image entry and puts it on the clipboard without
    /// retaining it; returns the number of characters copied.
    pub fn copy_clipboard_image_text(&self, item_id: u64) -> Result<usize, String> {
        if !self.clipboard_history_is_running() {
            return Err("Clipboard history is not running".to_owned());
        }
        if let Some(reason) = clipboard_ocr_unavailable() {
            return Err(reason);
        }
        let png = self
            .clipboard_history
            .image_png(item_id)
            .map_err(|error| format!("The image could not be read: {error}"))?;
        let text = self
            .clipboard_ocr
            .recognize(
                png.expose(),
                &kestrel_platform::capture::CaptureCancel::new(),
            )
            .map_err(|error| format!("Text recognition failed: {}", error.message))?;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err("No text was found in the image".to_owned());
        }
        let characters = text.chars().count();
        self.clipboard_history
            .execute(ClipboardCommand::CopyUnstoredText(
                kestrel_services::clipboard::Unlogged::new(text),
            ))
            .map_err(|error| format!("The text could not be copied: {error}"))?;
        Ok(characters)
    }

    /// The recognizer the screenshot editor uses to find text, or why it
    /// cannot: the same Tesseract settings as the screen tools.
    pub fn editor_text_recognizer(&self) -> Result<Arc<dyn TextRecognizer>, String> {
        match TesseractAdapter::discover().executable() {
            Some(_) => Ok(Arc::clone(&self.clipboard_ocr)),
            None => Err("Install Tesseract (the tesseract-ocr package) to find text.".to_owned()),
        }
    }

    /// Returns retained-item metadata while keeping contents private.
    pub fn clipboard_snapshot(&self) -> ClipboardSnapshot {
        self.clipboard_history.latest()
    }

    /// Saves one image entry as a bounded PNG at the chosen path.
    pub fn export_clipboard_image(
        &self,
        entry_id: u64,
        dest: &Path,
        replace: bool,
        max_bytes: u64,
    ) -> Result<String, String> {
        if !self.clipboard_history_is_running() {
            return Err("Clipboard history is not running".to_owned());
        }
        let png = self
            .clipboard_history
            .image_png(entry_id)
            .map_err(|error| format!("The image could not be read: {error}"))?;
        kestrel_platform::file_actions::export_png(png.expose(), dest, max_bytes, replace)
            .map_err(|error| error.to_string())?;
        Ok(format!("Saved {}", dest.display()))
    }

    /// Returns bounded, explicitly requested previews outside snapshots and diagnostics.
    pub fn clipboard_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Option<Result<Vec<ClipboardMatch>, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.search(query, limit))
    }

    /// Returns one bounded entry preview through the running worker.
    pub fn clipboard_preview(
        &self,
        item_id: u64,
        max_bytes: usize,
    ) -> Option<Result<ClipboardPreview, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.preview(item_id, max_bytes))
    }

    /// Applies a clipboard command only while the opt-in service is running.
    pub fn execute_clipboard_command(
        &self,
        command: ClipboardCommand,
    ) -> Option<Result<ClipboardSnapshot, ClipboardServiceError>> {
        self.clipboard_history_is_running()
            .then(|| self.clipboard_history.execute(command))
    }

    /// Cleans the single link on the clipboard and replaces it.
    ///
    /// Anything else, including a link that changed during the read, is left
    /// exactly as it is.
    pub fn clean_clipboard_link(
        &mut self,
        rules: &kestrel_core::UrlRules,
    ) -> Result<CleanLinkOutcome, String> {
        let backend = self
            .session_clipboard
            .as_mut()
            .ok_or_else(|| "no clipboard connection is available for this session".to_owned())?;
        let Some(text) = backend.read_text().map_err(|error| error.to_string())? else {
            return Ok(CleanLinkOutcome::NotSingleLink);
        };
        let Some(link) = kestrel_core::single_http_link(&text) else {
            return Ok(CleanLinkOutcome::NotSingleLink);
        };
        let Ok(cleaned) = kestrel_core::clean_url(link, rules) else {
            return Ok(CleanLinkOutcome::NotSingleLink);
        };
        if cleaned.removed.is_empty() {
            return Ok(CleanLinkOutcome::Nothing);
        }
        match backend.read_text() {
            Ok(Some(current)) if current == text => {}
            _ => return Ok(CleanLinkOutcome::NotSingleLink),
        }
        backend
            .write_text(&cleaned.cleaned)
            .map_err(|error| error.to_string())?;
        Ok(CleanLinkOutcome::Removed(cleaned.removed.len()))
    }

    pub(super) fn start_clipboard_history(&mut self) -> Result<(), ClipboardServiceError> {
        let provider = discover_provider().map_err(ClipboardServiceError::Backend)?;
        let backend =
            ArboardClipboardBackend::new(provider).map_err(ClipboardServiceError::Backend)?;
        let privacy = LogindPrivacyMonitor::new().map_err(ClipboardServiceError::Backend)?;
        self.clipboard_history.start(backend, privacy)?;
        self.configure_clipboard_paste();
        Ok(())
    }

    /// Installs the keystroke provider a paste sends through, or why there is
    /// none.
    pub(super) fn configure_clipboard_paste(&mut self) {
        let discovery =
            discover_insertion_provider(self.snippet_provider_preference).and_then(|path| {
                ExecutableInsertionBackend::new(
                    path,
                    self.snippet_policy.insert_timeout.as_millis() as u64,
                )
            });
        let (provider, unavailable) = match discovery {
            Ok(provider) => (
                Some(Box::new(provider) as Box<dyn kestrel_platform::snippets::InsertionBackend>),
                None,
            ),
            Err(error) => (
                None,
                Some(format!(
                    "{} Install wtype for Wayland or xdotool for X11.",
                    error.message
                )),
            ),
        };
        self.clipboard_paste_unavailable = unavailable.clone();
        self.clipboard_history
            .set_paste_provider(provider, unavailable);
    }

    /// Why pasting an entry cannot send a keystroke, with a remedy.
    pub fn clipboard_paste_unavailable(&self) -> Option<String> {
        self.clipboard_paste_unavailable.clone()
    }

    /// Reports whether Kestrel still holds the focus and which modifiers are held.
    pub fn observe_clipboard_paste(&self, observation: PasteObservation) {
        self.clipboard_history.observe_paste(observation);
    }

    pub(super) fn clipboard_history_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == CLIPBOARD_HISTORY_ID && registration.running
        })
    }

    pub(super) fn clipboard_worker_is_running(&self) -> bool {
        matches!(
            self.clipboard_history.latest().lifecycle,
            kestrel_services::clipboard::ClipboardLifecycle::Running
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_services::clipboard::{ClipboardCommand, ClipboardLifecycle};
    #[test]
    fn clipboard_history_is_inert_until_explicitly_enabled() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();

        assert_eq!(
            runtime.clipboard_snapshot().lifecycle,
            ClipboardLifecycle::Stopped
        );
        assert!(
            runtime
                .execute_clipboard_command(ClipboardCommand::Wipe)
                .is_none()
        );
    }
}
