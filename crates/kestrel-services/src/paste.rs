//! One paste of a staged history entry into the previously focused application.
//!
//! The operation stages an entry on the clipboard, then waits for the window to
//! lose focus and for held modifiers to be released before sending a single
//! keystroke. The provider receives only the key combination.

use std::time::{Duration, Instant};

use kestrel_core::ClipboardPasteKeys;
use kestrel_platform::clipboard::{
    ClipboardBackend, ClipboardError, ClipboardErrorKind, OwnedSelection,
};
use kestrel_platform::snippets::{InsertionBackend, InsertionProvider};

pub const DEFAULT_FOCUS_TIMEOUT: Duration = Duration::from_secs(2);
pub const DEFAULT_MODIFIER_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the operation waits and what it sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteSettings {
    pub keys: ClipboardPasteKeys,
    pub focus_timeout: Duration,
    pub modifier_timeout: Duration,
    pub max_image_bytes: usize,
    pub max_file_entries: usize,
}

impl Default for PasteSettings {
    fn default() -> Self {
        Self {
            keys: ClipboardPasteKeys::CtrlV,
            focus_timeout: DEFAULT_FOCUS_TIMEOUT,
            modifier_timeout: DEFAULT_MODIFIER_TIMEOUT,
            max_image_bytes: super::clipboard::DEFAULT_MAX_IMAGE_BYTES,
            max_file_entries: super::clipboard::DEFAULT_MAX_FILE_ENTRIES,
        }
    }
}

/// What the application observes each tick while a paste waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PasteObservation {
    /// No Kestrel window holds the focus any more.
    pub focus_released: bool,
    /// No shift, control, alt, or super key is held.
    pub modifiers_released: bool,
}

/// Why the operation stopped without a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteAbandonReason {
    /// The window kept the focus for the whole bound.
    FocusTimeout,
    /// Modifier keys were still held when the bound ran out.
    ModifiersHeld,
    /// Another copy replaced the staged selection, so nothing was typed.
    ClipboardChanged,
    /// The clipboard could not be read to confirm the staged selection.
    ClipboardUnreadable,
    /// The backend cannot report selection contents.
    ClipboardReadUnsupported,
}

impl PasteAbandonReason {
    pub const fn message(self) -> &'static str {
        match self {
            Self::FocusTimeout => {
                "The window kept the focus, so nothing was pasted; the entry stays on the clipboard."
            }
            Self::ModifiersHeld => {
                "Modifier keys were still held, so nothing was pasted; the entry stays on the \
                 clipboard."
            }
            Self::ClipboardChanged => {
                "The clipboard changed before the paste, so nothing was typed; the newer copy is \
                 untouched."
            }
            Self::ClipboardUnreadable => {
                "The clipboard could not be read to confirm the paste, so nothing was typed."
            }
            Self::ClipboardReadUnsupported => {
                "This clipboard backend cannot report its contents, so the paste was not sent."
            }
        }
    }
}

/// Why the provider could not send the keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteFailure {
    pub provider: Option<InsertionProvider>,
    pub message: String,
}

impl PasteFailure {
    pub fn new(provider: Option<InsertionProvider>, message: String) -> Self {
        Self { provider, message }
    }

    pub fn message(&self) -> String {
        format!("The paste keystroke failed: {}", self.message)
    }
}

/// The operation's typed lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteStage {
    /// The entry is on the clipboard and the window may still hold focus.
    Staged,
    WaitingForFocusLoss,
    WaitingForModifiers,
    Sending,
    Done,
    Abandoned(PasteAbandonReason),
    Failed(PasteFailure),
}

impl PasteStage {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Abandoned(_) | Self::Failed(_))
    }

    /// Compact number used to detect snapshot changes.
    pub const fn code(&self) -> u8 {
        match self {
            Self::Staged => 1,
            Self::WaitingForFocusLoss => 2,
            Self::WaitingForModifiers => 3,
            Self::Sending => 4,
            Self::Done => 5,
            Self::Abandoned(_) => 6,
            Self::Failed(_) => 7,
        }
    }

    /// What the window says about the operation, when it says anything.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Staged | Self::Sending => None,
            Self::WaitingForFocusLoss => {
                Some("Paste: waiting for the window to lose focus".to_owned())
            }
            Self::WaitingForModifiers => {
                Some("Paste: waiting for modifier keys to be released".to_owned())
            }
            Self::Done => Some("Pasted into the previous application".to_owned()),
            Self::Abandoned(reason) => Some(reason.message().to_owned()),
            Self::Failed(failure) => Some(failure.message()),
        }
    }
}

/// A single paste; a terminal operation never sends a second keystroke.
#[derive(Debug, Clone)]
pub struct PasteOperation {
    item_id: u64,
    staged: OwnedSelection,
    stage: PasteStage,
    settings: PasteSettings,
    deadline: Instant,
}

impl PasteOperation {
    /// Starts the bounded wait; the caller hides its window once staging lands.
    pub fn staged(
        item_id: u64,
        staged: OwnedSelection,
        settings: PasteSettings,
        now: Instant,
    ) -> Self {
        Self {
            item_id,
            staged,
            stage: PasteStage::Staged,
            settings,
            deadline: now + settings.focus_timeout,
        }
    }

    pub fn item_id(&self) -> u64 {
        self.item_id
    }

    pub fn stage(&self) -> &PasteStage {
        &self.stage
    }

    pub fn is_terminal(&self) -> bool {
        self.stage.is_terminal()
    }

    pub fn staged_selection(&self) -> &OwnedSelection {
        &self.staged
    }

    /// Ends the operation without a keystroke.
    pub fn fail(&mut self, failure: PasteFailure) {
        if !self.is_terminal() {
            self.stage = PasteStage::Failed(failure);
        }
    }

    /// Advances the wait with the observations and, once both holds are gone,
    /// confirms the staged selection through `backend` before sending.
    ///
    /// `still_owned` is whether Kestrel still tracks the staged selection as the
    /// one it owns; a newer copy clears it before this call.
    pub fn advance<B: ClipboardBackend, I: InsertionBackend + ?Sized>(
        &mut self,
        now: Instant,
        observation: PasteObservation,
        still_owned: bool,
        backend: &mut B,
        provider: &mut I,
    ) -> &PasteStage {
        loop {
            match self.stage {
                PasteStage::Staged => self.stage = PasteStage::WaitingForFocusLoss,
                PasteStage::WaitingForFocusLoss => {
                    if now >= self.deadline {
                        self.stage = PasteStage::Abandoned(PasteAbandonReason::FocusTimeout);
                    } else if observation.focus_released {
                        self.stage = PasteStage::WaitingForModifiers;
                        self.deadline = now + self.settings.modifier_timeout;
                    } else {
                        break;
                    }
                }
                PasteStage::WaitingForModifiers => {
                    if now >= self.deadline {
                        self.stage = PasteStage::Abandoned(PasteAbandonReason::ModifiersHeld);
                    } else if observation.modifiers_released {
                        self.send(still_owned, backend, provider);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        &self.stage
    }

    fn send<B: ClipboardBackend, I: InsertionBackend + ?Sized>(
        &mut self,
        still_owned: bool,
        backend: &mut B,
        provider: &mut I,
    ) {
        if !still_owned {
            self.stage = PasteStage::Abandoned(PasteAbandonReason::ClipboardChanged);
            return;
        }
        match holds_staged(backend, &self.staged, &self.settings) {
            Ok(true) => {
                self.stage = PasteStage::Sending;
                self.stage = match provider.press_keys(self.settings.keys) {
                    Ok(()) => PasteStage::Done,
                    Err(error) => PasteStage::Failed(PasteFailure::new(
                        Some(provider.provider()),
                        error.message,
                    )),
                };
            }
            Ok(false) => {
                self.stage = PasteStage::Abandoned(PasteAbandonReason::ClipboardChanged);
            }
            Err(error) => {
                self.stage = PasteStage::Abandoned(read_abandon_reason(&error));
            }
        }
    }
}

fn read_abandon_reason(error: &ClipboardError) -> PasteAbandonReason {
    match error.kind {
        ClipboardErrorKind::Unavailable => PasteAbandonReason::ClipboardReadUnsupported,
        _ => PasteAbandonReason::ClipboardUnreadable,
    }
}

/// Reads the live selection back, bounded, and compares it with the staged one.
fn holds_staged<B: ClipboardBackend>(
    backend: &mut B,
    staged: &OwnedSelection,
    settings: &PasteSettings,
) -> Result<bool, ClipboardError> {
    match staged {
        OwnedSelection::Text(text) => Ok(backend.read_text()?.as_deref() == Some(text.as_str())),
        OwnedSelection::ImagePng(bytes) => {
            Ok(backend.read_image_png(settings.max_image_bytes)?.as_deref()
                == Some(bytes.as_slice()))
        }
        OwnedSelection::Files(paths) => Ok(backend
            .read_file_list(settings.max_file_entries)?
            .as_deref()
            == Some(paths.as_slice())),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use kestrel_core::ClipboardPasteKeys;
    use kestrel_platform::clipboard::{
        ClipboardBackend, ClipboardEntryKind, ClipboardError, ClipboardErrorKind,
        ClipboardKindSupport, ClipboardProvider, OwnedSelection,
    };
    use kestrel_platform::snippets::{
        InsertionBackend, InsertionError, InsertionErrorKind, InsertionProvider,
    };

    use super::{PasteAbandonReason, PasteObservation, PasteOperation, PasteSettings, PasteStage};

    #[derive(Default)]
    struct FakeBackend {
        text: Option<String>,
        read_error: Option<ClipboardErrorKind>,
    }

    impl ClipboardBackend for FakeBackend {
        fn provider(&self) -> ClipboardProvider {
            ClipboardProvider::WaylandDataControl
        }

        fn kind_support(&self) -> ClipboardKindSupport {
            ClipboardKindSupport::TEXT_ONLY
        }

        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            if let Some(kind) = self.read_error {
                return Err(ClipboardError {
                    kind,
                    message: "the fake backend cannot read".to_owned(),
                });
            }
            Ok(self.text.clone())
        }

        fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
            self.text = Some(text.to_owned());
            Ok(())
        }

        fn offered_kind(&mut self) -> Result<Option<ClipboardEntryKind>, ClipboardError> {
            Ok(self.text.as_ref().map(|_| ClipboardEntryKind::Text))
        }

        fn clear_if_matches(&mut self, expected: &OwnedSelection) -> Result<bool, ClipboardError> {
            let matches = matches!((expected, self.text.as_deref()), (OwnedSelection::Text(text), Some(current)) if text == current);
            if matches {
                self.text = None;
            }
            Ok(matches)
        }
    }

    #[derive(Default)]
    struct FakeProvider {
        keys: Vec<ClipboardPasteKeys>,
        error: Option<InsertionError>,
    }

    impl InsertionBackend for FakeProvider {
        fn provider(&self) -> InsertionProvider {
            InsertionProvider::Wtype
        }

        fn insert_text(&mut self, text: &str) -> Result<(), InsertionError> {
            let _ = text;
            Ok(())
        }

        fn press_keys(&mut self, keys: ClipboardPasteKeys) -> Result<(), InsertionError> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            self.keys.push(keys);
            Ok(())
        }
    }

    fn settings(keys: ClipboardPasteKeys) -> PasteSettings {
        PasteSettings {
            keys,
            focus_timeout: Duration::from_secs(1),
            modifier_timeout: Duration::from_millis(500),
            max_image_bytes: 1024,
            max_file_entries: 4,
        }
    }

    fn staged_text(text: &str, now: Instant) -> (PasteOperation, FakeBackend, FakeProvider) {
        let operation = PasteOperation::staged(
            7,
            OwnedSelection::Text(text.to_owned()),
            settings(ClipboardPasteKeys::CtrlShiftV),
            now,
        );
        (
            operation,
            FakeBackend {
                text: Some(text.to_owned()),
                read_error: None,
            },
            FakeProvider::default(),
        )
    }

    fn released() -> PasteObservation {
        PasteObservation {
            focus_released: true,
            modifiers_released: true,
        }
    }

    #[test]
    fn a_waiting_paste_sends_its_chord_once_after_both_holds_clear() {
        let now = Instant::now();
        let (mut operation, mut backend, mut provider) = staged_text("one", now);

        assert_eq!(
            operation.advance(
                now,
                PasteObservation::default(),
                true,
                &mut backend,
                &mut provider
            ),
            &PasteStage::WaitingForFocusLoss
        );
        assert_eq!(
            operation.advance(
                now + Duration::from_millis(10),
                PasteObservation {
                    focus_released: true,
                    modifiers_released: false,
                },
                true,
                &mut backend,
                &mut provider,
            ),
            &PasteStage::WaitingForModifiers
        );
        assert!(
            provider.keys.is_empty(),
            "nothing is sent while a hold remains"
        );

        let sent_at = now + Duration::from_millis(20);
        assert_eq!(
            operation.advance(sent_at, released(), true, &mut backend, &mut provider),
            &PasteStage::Done
        );
        assert_eq!(provider.keys, [ClipboardPasteKeys::CtrlShiftV]);

        // A finished paste never sends again.
        operation.advance(
            sent_at + Duration::from_secs(1),
            released(),
            true,
            &mut backend,
            &mut provider,
        );
        assert_eq!(operation.stage(), &PasteStage::Done);
        assert_eq!(provider.keys.len(), 1);
    }

    #[test]
    fn a_clipboard_change_aborts_without_a_keystroke() {
        let now = Instant::now();
        let (mut operation, mut backend, mut provider) = staged_text("one", now);
        backend.text = Some("two".to_owned());

        let stage = operation.advance(now, released(), true, &mut backend, &mut provider);
        assert_eq!(
            stage,
            &PasteStage::Abandoned(PasteAbandonReason::ClipboardChanged)
        );
        assert!(
            provider.keys.is_empty(),
            "a changed clipboard sends nothing"
        );
    }

    #[test]
    fn a_released_ownership_aborts_without_reading_the_clipboard() {
        let now = Instant::now();
        let (mut operation, mut backend, mut provider) = staged_text("one", now);
        backend.read_error = Some(ClipboardErrorKind::ReadFailed);

        let stage = operation.advance(now, released(), false, &mut backend, &mut provider);
        assert_eq!(
            stage,
            &PasteStage::Abandoned(PasteAbandonReason::ClipboardChanged)
        );
        assert!(provider.keys.is_empty());
    }

    #[test]
    fn modifier_timeout_and_focus_timeout_abandon_the_operation() {
        let now = Instant::now();
        let (mut operation, mut backend, mut provider) = staged_text("one", now);
        let focus_late = now + Duration::from_secs(2);

        assert_eq!(
            operation.advance(
                focus_late,
                PasteObservation::default(),
                true,
                &mut backend,
                &mut provider
            ),
            &PasteStage::Abandoned(PasteAbandonReason::FocusTimeout)
        );
        assert!(provider.keys.is_empty());

        let (mut operation, mut backend, mut provider) = staged_text("one", now);
        let focus_lost = now + Duration::from_millis(10);
        operation.advance(
            focus_lost,
            PasteObservation {
                focus_released: true,
                modifiers_released: false,
            },
            true,
            &mut backend,
            &mut provider,
        );
        assert_eq!(
            operation.advance(
                focus_lost + Duration::from_millis(600),
                PasteObservation {
                    focus_released: true,
                    modifiers_released: false,
                },
                true,
                &mut backend,
                &mut provider,
            ),
            &PasteStage::Abandoned(PasteAbandonReason::ModifiersHeld)
        );
        assert!(provider.keys.is_empty());
    }

    #[test]
    fn an_unreadable_clipboard_reports_the_capability_and_sends_nothing() {
        for (kind, expected) in [
            (
                ClipboardErrorKind::Unavailable,
                PasteAbandonReason::ClipboardReadUnsupported,
            ),
            (
                ClipboardErrorKind::ReadFailed,
                PasteAbandonReason::ClipboardUnreadable,
            ),
        ] {
            let now = Instant::now();
            let (mut operation, mut backend, mut provider) = staged_text("one", now);
            backend.read_error = Some(kind);
            assert_eq!(
                operation.advance(now, released(), true, &mut backend, &mut provider),
                &PasteStage::Abandoned(expected),
                "{kind:?}"
            );
            assert!(provider.keys.is_empty());
        }
    }

    #[test]
    fn a_provider_failure_is_reported_and_a_second_paste_starts_cleanly() {
        let now = Instant::now();
        let (mut operation, mut backend, mut provider) = staged_text("one", now);
        provider.error = Some(InsertionError {
            kind: InsertionErrorKind::Rejected,
            message: "wtype refused the paste".to_owned(),
        });

        let stage = operation.advance(now, released(), true, &mut backend, &mut provider);
        let PasteStage::Failed(failure) = stage else {
            panic!("expected a provider failure, got {stage:?}");
        };
        assert_eq!(failure.provider, Some(InsertionProvider::Wtype));
        assert!(failure.message().contains("wtype refused the paste"));
        assert!(provider.keys.is_empty());

        // The terminal operation no longer blocks a new one.
        let mut next = PasteOperation::staged(
            8,
            OwnedSelection::Text("one".to_owned()),
            settings(ClipboardPasteKeys::CtrlV),
            now,
        );
        provider.error = None;
        assert_eq!(
            next.advance(now, released(), true, &mut backend, &mut provider),
            &PasteStage::Done
        );
        assert_eq!(provider.keys, [ClipboardPasteKeys::CtrlV]);
    }
}
