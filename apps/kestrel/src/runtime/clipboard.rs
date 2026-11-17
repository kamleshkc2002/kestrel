//! Clipboard history runtime features.

use super::*;
use kestrel_platform::secret_service::{PinKeyStore, SecretServiceKeyStore};

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
    /// The runtime policy, with the store path only while saving is enabled.
    pub(super) fn clipboard_policy(configuration: &ApplicationConfiguration) -> ClipboardPolicy {
        let mut policy = ClipboardPolicy::from_configuration(
            &configuration.clipboard,
            &configuration.url_cleanup,
        );
        policy.type_text_max_bytes = configuration.snippets.max_content_bytes as usize;
        policy.pin_file = configuration
            .clipboard
            .keep_pinned
            .then(crate::config::clipboard_pins_path)
            .flatten();
        policy.pin_keyring_withheld = kestrel_platform::sandbox::flatpak();
        policy
    }

    /// Connects the Secret Service the first time saved pins are wanted.
    pub(super) fn ensure_clipboard_pin_key_store(&mut self, wanted: bool) {
        if !wanted || self.clipboard_pin_store_ready {
            return;
        }
        if kestrel_platform::sandbox::flatpak() {
            // The sandbox does not reach the host keyring; the store reports
            // that instead of asking for one.
            return;
        }
        match SecretServiceKeyStore::connect() {
            Ok(store) => {
                let store: Arc<dyn PinKeyStore> = Arc::new(store);
                self.clipboard_history.set_pin_key_store(store);
                self.clipboard_pin_store_ready = true;
            }
            Err(error) => {
                log::debug!(
                    target: CLIPBOARD_HISTORY_ID,
                    "clipboard pin key store is unavailable: {error}"
                );
            }
        }
    }

    /// Deletes the encrypted store; `reset` also removes its keyring item.
    pub fn wipe_saved_pins(
        &mut self,
        reset: bool,
    ) -> Result<kestrel_services::clipboard::PinWipeReport, String> {
        self.ensure_clipboard_pin_key_store(true);
        self.clipboard_history
            .wipe_saved_pins(reset)
            .map_err(|error| format!("Saved pins could not be deleted: {error}"))
    }

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
        let text = kestrel_services::screen_tools::image_text(
            self.clipboard_ocr.as_ref(),
            png.expose(),
            &kestrel_platform::capture::CaptureCancel::new(),
        )
        .map_err(|error| format!("Text recognition failed: {}", error.message))?;
        let Some(text) = text else {
            return Err("No text was found in the image".to_owned());
        };
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
            None => Err(kestrel_platform::screen_tools::TESSERACT_REMEDIATION.to_owned()),
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
        let keep_pinned = self.clipboard_history.policy().keep_pinned;
        self.ensure_clipboard_pin_key_store(keep_pinned);
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

    /// Whether a paste is still waiting or sending.
    pub fn clipboard_paste_in_progress(&self) -> bool {
        self.clipboard_history
            .latest()
            .paste
            .is_some_and(|stage| !stage.is_terminal())
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
    use std::{
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use kestrel_platform::clipboard::{
        ClipboardBackend, ClipboardEntryKind, ClipboardError, ClipboardKindSupport,
        ClipboardProvider, OwnedSelection, PrivacyEventSource, SessionPrivacyEvent,
    };
    use kestrel_platform::snippets::{InsertionBackend, InsertionError, InsertionProvider};
    use kestrel_services::clipboard::{ClipboardCommand, ClipboardLifecycle};
    use kestrel_services::paste::{PasteObservation, PasteStage};

    use super::*;

    #[derive(Default)]
    struct FakeClipboardState {
        text: Option<String>,
        writes: usize,
    }

    struct FakeClipboard {
        state: Arc<Mutex<FakeClipboardState>>,
    }

    impl ClipboardBackend for FakeClipboard {
        fn provider(&self) -> ClipboardProvider {
            ClipboardProvider::WaylandDataControl
        }

        fn kind_support(&self) -> ClipboardKindSupport {
            ClipboardKindSupport::TEXT_ONLY
        }

        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            Ok(self.state.lock().expect("fake state").text.clone())
        }

        fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            state.text = Some(text.to_owned());
            state.writes += 1;
            Ok(())
        }

        fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
            Ok(self
                .state
                .lock()
                .expect("fake state")
                .text
                .as_ref()
                .map(|_| ClipboardEntryKind::Text))
        }

        fn owns_selection(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
            let state = self.state.lock().expect("fake state");
            Ok(matches!(
                (expected, state.text.as_deref()),
                (OwnedSelection::Text(text), Some(current)) if text == current
            ))
        }

        fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
            let mut state = self.state.lock().expect("fake state");
            let matches = matches!(
                (expected, state.text.as_deref()),
                (OwnedSelection::Text(text), Some(current)) if text == current
            );
            if matches {
                state.text = None;
            }
            Ok(matches)
        }
    }

    struct InertPrivacy;

    impl PrivacyEventSource for InertPrivacy {
        fn poll_events(&mut self) -> Result<Vec<SessionPrivacyEvent>, ClipboardError> {
            Ok(Vec::new())
        }
    }

    #[derive(Default)]
    struct FakeProvider {
        texts: Arc<Mutex<Vec<String>>>,
        chords: Arc<Mutex<Vec<kestrel_core::ClipboardPasteKeys>>>,
    }

    impl InsertionBackend for FakeProvider {
        fn provider(&self) -> InsertionProvider {
            InsertionProvider::Wtype
        }

        fn insert_text(&mut self, text: &str) -> Result<(), InsertionError> {
            self.texts.lock().expect("fake texts").push(text.to_owned());
            Ok(())
        }

        fn press_keys(
            &mut self,
            keys: kestrel_core::ClipboardPasteKeys,
        ) -> Result<(), InsertionError> {
            self.chords.lock().expect("fake chords").push(keys);
            Ok(())
        }
    }

    #[test]
    fn a_runtime_typed_paste_reaches_the_provider_once_after_the_handoff() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.paste_keys = kestrel_core::ClipboardPasteKeys::TypeText;
        configuration.clipboard.paste_plain_text = false;
        configuration.snippets.max_content_bytes = 64;
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");

        let state = Arc::new(Mutex::new(FakeClipboardState {
            text: Some("42".to_owned()),
            writes: 0,
        }));
        runtime
            .clipboard_history
            .start(
                FakeClipboard {
                    state: Arc::clone(&state),
                },
                InertPrivacy,
            )
            .expect("clipboard worker starts");
        let provider = FakeProvider::default();
        let texts = Arc::clone(&provider.texts);
        let chords = Arc::clone(&provider.chords);
        runtime
            .clipboard_history
            .set_paste_provider(Some(Box::new(provider)), None);

        for _ in 0..400 {
            if runtime.clipboard_snapshot().items.len() == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let snapshot = runtime.clipboard_snapshot();
        assert_eq!(snapshot.items.len(), 1, "the entry is captured");
        let item_id = snapshot.items[0].id;
        let writes_before = state.lock().expect("fake state").writes;

        runtime
            .clipboard_history
            .execute(ClipboardCommand::PasteItem {
                item_id,
                plain_text: false,
            })
            .expect("a text entry can be typed");

        // A visible Kestrel surface keeps the focus: clearing the modifiers must
        // not type anything while the window is still there.
        let mut waiting = false;
        for _ in 0..400 {
            runtime.observe_clipboard_paste(PasteObservation {
                focus_released: false,
                modifiers_released: true,
            });
            let snapshot = runtime.clipboard_snapshot();
            if snapshot.paste == Some(PasteStage::WaitingForFocus) {
                waiting = true;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(waiting, "the paste waits for the window to hide");
        assert!(
            texts.lock().expect("fake texts").is_empty(),
            "nothing is typed while Kestrel can still hold the focus"
        );

        // Once every surface is hidden, the observer reports the focus released
        // and the text is typed exactly once.
        let mut terminal = None;
        for _ in 0..400 {
            runtime.observe_clipboard_paste(PasteObservation {
                focus_released: true,
                modifiers_released: true,
            });
            let snapshot = runtime.clipboard_snapshot();
            if snapshot.paste.as_ref().is_some_and(PasteStage::is_terminal) {
                terminal = snapshot.paste;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            terminal,
            Some(PasteStage::Typed),
            "the observer sees a successful terminal stage"
        );
        assert_eq!(
            texts.lock().expect("fake texts").as_slice(),
            ["42"],
            "the provider receives the entry's text exactly once"
        );
        assert!(
            chords.lock().expect("fake chords").is_empty(),
            "a typed paste sends no chord"
        );
        assert_eq!(
            state.lock().expect("fake state").writes,
            writes_before,
            "a typed paste never writes the clipboard"
        );
        runtime.clipboard_history.stop();
    }

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
