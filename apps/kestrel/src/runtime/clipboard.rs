//! Clipboard history runtime features.

use super::*;

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

    pub(super) fn start_clipboard_history(&mut self) -> Result<(), ClipboardServiceError> {
        let provider = discover_provider().map_err(ClipboardServiceError::Backend)?;
        let backend =
            ArboardClipboardBackend::new(provider).map_err(ClipboardServiceError::Backend)?;
        let privacy = LogindPrivacyMonitor::new().map_err(ClipboardServiceError::Backend)?;
        self.clipboard_history.start(backend, privacy)
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
