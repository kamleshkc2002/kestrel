//! Clipboard presentation and history.

use super::*;

#[derive(Default)]
pub(crate) struct ClipboardPresentation<'a> {
    /// Metadata only; no clipboard content.
    pub snapshot: Option<ClipboardSnapshot>,
    pub running: bool,
    pub search_query: &'a str,
    pub matches: &'a [ClipboardMatch],
    pub preview: Option<&'a ClipboardPreview>,
    /// Why text cannot be read from images, when it cannot.
    pub ocr_unavailable: Option<String>,
    /// Why pasting cannot send a keystroke, with a remedy.
    pub paste_unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardViewModel {
    pub running: bool,
    pub status: String,
    pub search_query: String,
    pub items: Vec<ClipboardItemViewModel>,
    pub total_items: usize,
    pub pinned_items: usize,
    pub preview: Option<ClipboardPreviewViewModel>,
    pub policy: ClipboardPolicyViewModel,
    /// Newest retained entry; clears confirm up to it so later copies survive.
    pub newest_id: Option<u64>,
    pub layout: kestrel_core::ClipboardLayout,
    pub ocr_unavailable: Option<String>,
    /// Why pasting cannot send a keystroke, with a remedy.
    pub paste_unavailable: Option<String>,
    /// State and bounds of the encrypted pin store.
    pub pin_store: ClipboardPinStoreViewModel,
}

/// The saved-pin store's presentation, metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPinStoreViewModel {
    pub keep_pinned: bool,
    pub state: kestrel_services::clipboard::PinStoreState,
    pub saved_count: usize,
    pub saved_bytes: usize,
    pub status: String,
    pub remedy: Option<String>,
    /// The store is unreadable and offers Reset or Keep.
    pub needs_reset: bool,
    pub pinned_max_age_days: u32,
    pub max_pinned_age_days: u32,
    /// There is something to delete from the store.
    pub can_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItemViewModel {
    pub id: u64,
    pub kind: &'static str,
    pub detail: String,
    pub preview: String,
    /// Byte ranges of `preview` that match the search.
    pub highlights: Vec<(usize, usize)>,
    pub pinned: bool,
    pub is_image: bool,
    /// How this entry relates to the saved-pin store.
    pub pin_save: kestrel_services::clipboard::PinSaveState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPreviewViewModel {
    pub id: u64,
    pub kind: &'static str,
    pub text: String,
    pub truncated: bool,
    /// The text is JSON laid out one value per line.
    pub json: bool,
    pub editable: bool,
    pub is_image: bool,
    pub image_dimensions: Option<(u32, u32)>,
    pub file_count: Option<usize>,
}

/// Byte ranges where `needle` occurs in `text`, ignoring case.
pub(crate) fn match_ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let folded: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut wanted = folded.iter();
        let mut end = None;
        let mut pending = Vec::new();
        for (offset, character) in text[start..].char_indices() {
            pending.extend(character.to_lowercase());
            let mut matched = true;
            for folded_char in pending.drain(..) {
                if wanted.next() != Some(&folded_char) {
                    matched = false;
                    break;
                }
            }
            if !matched {
                break;
            }
            if wanted.as_slice().is_empty() {
                end = Some(start + offset + character.len_utf8());
                break;
            }
        }
        match end {
            Some(end) => {
                ranges.push((start, end));
                start = end;
            }
            None => {
                start += text[start..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    ranges
}

/// Editable policy values plus the bounds the controls must enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicyViewModel {
    pub max_items: u32,
    pub max_item_bytes: u32,
    pub max_image_bytes: u32,
    pub max_file_entries: u32,
    pub max_age_hours: u32,
    pub clear_seconds: u64,
    pub filter_sensitive: bool,
    pub paste_plain_text: bool,
    /// The keystroke a paste sends.
    pub paste_keys: kestrel_core::ClipboardPasteKeys,
    /// The second opt-in for keeping pinned text across restarts.
    pub keep_pinned: bool,
    /// How long a saved pin is kept.
    pub pinned_max_age_days: u32,
    pub bounds: ClipboardBoundsViewModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardBoundsViewModel {
    pub min_item_bytes: u32,
    pub max_item_bytes: u32,
    pub max_image_bytes: u32,
    pub max_file_entries: u32,
    pub max_items: u32,
    pub max_age_hours: u32,
    pub max_clear_seconds: u64,
    pub max_pinned_age_days: u32,
}

impl ClipboardViewModel {
    pub(crate) fn from_presentation(
        configuration: &kestrel_core::ClipboardConfiguration,
        presentation: ClipboardPresentation<'_>,
    ) -> Self {
        let bounds = ClipboardBoundsViewModel {
            min_item_bytes: kestrel_core::MIN_CLIPBOARD_ITEM_BYTES,
            max_item_bytes: kestrel_core::MAX_CLIPBOARD_ITEM_BYTES,
            max_image_bytes: kestrel_core::MAX_CLIPBOARD_IMAGE_BYTES,
            max_file_entries: kestrel_core::MAX_CLIPBOARD_FILE_ENTRIES,
            max_items: kestrel_core::MAX_CLIPBOARD_MAX_ITEMS,
            max_age_hours: kestrel_core::MAX_CLIPBOARD_AGE_HOURS,
            max_clear_seconds: kestrel_core::MAX_CLIPBOARD_CLEAR_SECONDS,
            max_pinned_age_days: kestrel_core::MAX_CLIPBOARD_PINNED_MAX_AGE_DAYS,
        };
        let policy = ClipboardPolicyViewModel {
            max_items: configuration.max_items,
            max_item_bytes: configuration.max_item_bytes,
            max_image_bytes: configuration.max_image_bytes,
            max_file_entries: configuration.max_file_entries,
            max_age_hours: configuration.max_age_hours,
            clear_seconds: configuration.clear_seconds,
            filter_sensitive: configuration.filter_sensitive,
            paste_plain_text: configuration.paste_plain_text,
            paste_keys: configuration.paste_keys,
            keep_pinned: configuration.keep_pinned,
            pinned_max_age_days: configuration.pinned_max_age_days,
            bounds,
        };
        let pin_store = clipboard_pin_store(
            configuration,
            presentation.running,
            presentation
                .snapshot
                .as_ref()
                .map(|snapshot| &snapshot.pin_store),
        );
        let Some(snapshot) = presentation
            .snapshot
            .as_ref()
            .filter(|_| presentation.running)
        else {
            return Self {
                running: presentation.running,
                status: if presentation.running {
                    "The clipboard history has not produced a snapshot yet.".to_owned()
                } else {
                    "Clipboard history is not running. Enable clipboard.history in the Feature \
                     Hub to retain entries."
                        .to_owned()
                },
                search_query: presentation.search_query.to_owned(),
                items: Vec::new(),
                total_items: 0,
                pinned_items: 0,
                preview: presentation.preview.map(clipboard_preview),
                policy,
                newest_id: None,
                layout: configuration.history_layout,
                ocr_unavailable: presentation.ocr_unavailable,
                paste_unavailable: presentation.paste_unavailable,
                pin_store,
            };
        };

        // Render only explicit search matches.
        let items = presentation
            .matches
            .iter()
            .map(|matched| {
                let metadata = snapshot
                    .items
                    .iter()
                    .find(|item| item.id == matched.id)
                    .cloned();
                ClipboardItemViewModel {
                    id: matched.id,
                    kind: matched.kind.label(),
                    detail: metadata
                        .as_ref()
                        .map(clipboard_item_detail)
                        .unwrap_or_else(|| "Retained entry".to_owned()),
                    highlights: match_ranges(&matched.preview, presentation.search_query),
                    preview: matched.preview.clone(),
                    pinned: metadata.as_ref().is_some_and(|item| item.pinned),
                    is_image: matched.kind
                        == kestrel_platform::clipboard::ClipboardEntryKind::Image,
                    pin_save: metadata
                        .as_ref()
                        .map(|item| item.pin_save)
                        .unwrap_or(kestrel_services::clipboard::PinSaveState::NotPinned),
                }
            })
            .collect::<Vec<_>>();

        Self {
            running: true,
            status: clipboard_status(
                snapshot,
                presentation.matches.len(),
                presentation.paste_unavailable.as_deref(),
            ),
            search_query: presentation.search_query.to_owned(),
            items,
            total_items: snapshot.items.len(),
            pinned_items: snapshot.pinned_items,
            preview: presentation.preview.map(clipboard_preview),
            policy,
            newest_id: snapshot.items.iter().map(|item| item.id).max(),
            layout: configuration.history_layout,
            ocr_unavailable: presentation.ocr_unavailable,
            paste_unavailable: presentation.paste_unavailable,
            pin_store,
        }
    }
}

/// Builds the saved-pins status with counts and a remedy, never content.
fn clipboard_pin_store(
    configuration: &kestrel_core::ClipboardConfiguration,
    running: bool,
    status: Option<&kestrel_services::clipboard::PinStoreStatus>,
) -> ClipboardPinStoreViewModel {
    use kestrel_services::clipboard::{PIN_STORE_REMEDY, PinStoreState};

    let state = status
        .map(|status| status.state)
        .unwrap_or(PinStoreState::Disabled);
    let saved_count = status.map_or(0, |status| status.saved_count);
    let saved_bytes = status.map_or(0, |status| status.saved_bytes);
    let keep_pinned = configuration.keep_pinned;
    let (message, remedy, needs_reset) = if !keep_pinned {
        (
            "Pinned entries stay in memory only. Turn this on to keep text pins across \
             restarts."
                .to_owned(),
            None,
            false,
        )
    } else if !running {
        (
            "Saved pins load when clipboard history starts.".to_owned(),
            None,
            false,
        )
    } else {
        match state {
            PinStoreState::Disabled => (
                "Pinned entries stay in memory only.".to_owned(),
                None,
                false,
            ),
            PinStoreState::Idle => (
                format!(
                    "{saved_count} saved pins · {} · kept for up to {} days",
                    format_bytes(saved_bytes as f64),
                    configuration.pinned_max_age_days
                ),
                None,
                false,
            ),
            PinStoreState::Saving | PinStoreState::Saved => (
                format!(
                    "{saved_count} saved pins · {} · kept for up to {} days",
                    format_bytes(saved_bytes as f64),
                    configuration.pinned_max_age_days
                ),
                None,
                false,
            ),
            PinStoreState::Unavailable | PinStoreState::Locked => (
                format!("{saved_count} saved pins · pinning is not being saved right now"),
                Some(PIN_STORE_REMEDY.to_owned()),
                false,
            ),
            PinStoreState::Unreadable => (
                "Saved pins could not be read.".to_owned(),
                Some(
                    "Reset deletes the unreadable file and its key; Keep leaves it to try again \
                     later."
                        .to_owned(),
                ),
                true,
            ),
            PinStoreState::Failed => (
                "Saved pins could not be saved.".to_owned(),
                Some(
                    "Check that the data folder is writable and that a keyring is available."
                        .to_owned(),
                ),
                false,
            ),
        }
    };
    ClipboardPinStoreViewModel {
        keep_pinned,
        state,
        saved_count,
        saved_bytes,
        status: message,
        remedy,
        needs_reset,
        pinned_max_age_days: configuration.pinned_max_age_days,
        max_pinned_age_days: kestrel_core::MAX_CLIPBOARD_PINNED_MAX_AGE_DAYS,
        can_delete: keep_pinned && running && (saved_count > 0 || needs_reset),
    }
}

fn clipboard_status(
    snapshot: &ClipboardSnapshot,
    shown: usize,
    paste_unavailable: Option<&str>,
) -> String {
    let mut status = format!(
        "{} of {} retained entries shown · {} retained · captured kinds: {}",
        shown,
        snapshot.items.len(),
        format_bytes(snapshot.total_bytes as f64),
        snapshot.kind_support.evidence_value()
    );
    if snapshot.pinned_items > 0 {
        status.push_str(&format!(" · {} pinned", snapshot.pinned_items));
    }
    if snapshot.filtered_sensitive_items > 0 {
        status.push_str(&format!(
            " · {} filtered as sensitive",
            snapshot.filtered_sensitive_items
        ));
    }
    if snapshot.selection_clears > 0 {
        status.push_str(&format!(
            " · {} automatic selection clears",
            snapshot.selection_clears
        ));
    }
    if snapshot.rejected_oversize_items > 0 {
        status.push_str(&format!(
            " · {} over the size bound",
            snapshot.rejected_oversize_items
        ));
    }
    match snapshot.temporary_writes {
        kestrel_services::clipboard::TemporaryWriteState::Undetectable => {
            status.push_str(" · temporary clipboard writes cannot be detected on this backend")
        }
        kestrel_services::clipboard::TemporaryWriteState::Detected
            if snapshot.skipped_temporary_writes > 0 =>
        {
            status.push_str(&format!(
                " · {} temporary writes skipped",
                snapshot.skipped_temporary_writes
            ));
        }
        kestrel_services::clipboard::TemporaryWriteState::Detected => {}
    }
    if let Some(paste) = &snapshot.paste {
        if let Some(message) = paste.message() {
            status.push_str(&format!(" · {message}"));
        }
    }
    if let Some(reason) = paste_unavailable {
        status.push_str(&format!(" · paste unavailable: {reason}"));
    }
    if let Some(error) = &snapshot.last_error {
        status.push_str(&format!(" · last error: {error}"));
    }
    status
}

fn clipboard_item_detail(metadata: &kestrel_services::clipboard::ClipboardItemMetadata) -> String {
    let mut parts = vec![
        metadata.kind.label().to_owned(),
        format_bytes(metadata.size_bytes as f64),
        format_age(metadata.age),
    ];
    if metadata.pinned {
        parts.push("pinned".to_owned());
    }
    if metadata.pin_save != kestrel_services::clipboard::PinSaveState::NotPinned {
        parts.push(metadata.pin_save.label().to_owned());
    }
    if let Some((width, height)) = metadata.image_dimensions {
        parts.push(format!("{width}×{height}"));
    }
    if let Some(count) = metadata.file_count {
        parts.push(format!("{count} files"));
    }
    parts.join(" · ")
}

fn clipboard_preview(preview: &ClipboardPreview) -> ClipboardPreviewViewModel {
    ClipboardPreviewViewModel {
        id: preview.id,
        kind: preview.kind.label(),
        text: preview.text.clone(),
        truncated: preview.truncated,
        json: preview.format == kestrel_services::clipboard::PreviewFormat::Json,
        // Laid-out JSON is not the stored text, so it is shown read-only.
        editable: preview.kind == kestrel_platform::clipboard::ClipboardEntryKind::Text
            && preview.format == kestrel_services::clipboard::PreviewFormat::Plain,
        is_image: preview.kind == kestrel_platform::clipboard::ClipboardEntryKind::Image,
        image_dimensions: preview.image_dimensions,
        file_count: preview.file_count,
    }
}

pub(super) fn format_age(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0 => "just now".to_owned(),
        1..=59 => format!("{seconds} s old"),
        60..=3599 => format!("{} min old", seconds / 60),
        _ => format!("{} h old", seconds / 3600),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClipboardQuery<'a> {
    pub query: &'a str,
    pub matches: &'a [ClipboardMatch],
    pub preview: Option<&'a ClipboardPreview>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::test_support::*;

    use kestrel_platform::clipboard::ClipboardEntryKind;

    #[test]
    fn search_matches_are_found_case_insensitively_on_char_boundaries() {
        assert_eq!(
            super::match_ranges("Token and TOKEN", "token"),
            [(0, 5), (10, 15)]
        );
        let text = "Größe der Größe";
        let ranges = super::match_ranges(text, "GRÖSSE");
        assert!(ranges.is_empty(), "ß does not fold to ss");
        let ranges = super::match_ranges(text, "größe");
        assert_eq!(ranges.len(), 2);
        assert!(
            ranges
                .iter()
                .all(|&(start, end)| text.is_char_boundary(start)
                    && text.is_char_boundary(end)
                    && text[start..end].to_lowercase() == "größe")
        );
        assert!(super::match_ranges("anything", "  ").is_empty());
    }

    #[test]
    fn stopped_clipboard_presentation_points_at_the_feature_hub() {
        let configuration = ApplicationConfiguration::default();
        let view_model = ApplicationViewModel::new(
            std::iter::empty(),
            std::iter::empty(),
            AudioPresentation {
                snapshot: None,
                running: false,
                policy: AudioPolicy::default(),
            },
            MicrophonePresentation {
                snapshot: &MicrophoneSnapshot::unavailable(),
                running: false,
            },
            test_speed_presentation(),
            test_shortcuts_presentation(),
            test_capture_presentation(),
            ClipboardPresentation::default(),
            SnippetsPresentation {
                library: &SnippetLibrary::default(),
                policy: SnippetPolicy::default(),
                running: false,
                provider: None,
                unavailable_reason: None,
                stopped_reason: None,
                directory: None,
                expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
                provider_preference: kestrel_core::SnippetProviderPreference::Auto,
                search_query: "",
                matches: &[],
                draft: None,
                warnings: &[],
            },
            CommandBarPresentation::default(),
            MonitorPresentation {
                snapshot: None,
                history: None,
                alerts: Default::default(),
                running: false,
            },
            &[],
            &configuration,
            false,
        );

        let clipboard = &view_model.clipboard;
        assert!(!clipboard.running);
        assert!(clipboard.status.contains("Feature Hub"));
        assert!(clipboard.items.is_empty());
    }
    #[test]
    fn clipboard_rows_come_only_from_explicit_search_matches() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        let matches = clipboard_matches();
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "match",
                matches: &matches,
                preview: None,
                ocr_unavailable: None,
                paste_unavailable: None,
            },
        );

        assert_eq!(
            clipboard
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![7, 8],
            "only the explicitly matched entries become rows"
        );
        assert_eq!(clipboard.items[0].preview, "matched text");
        assert!(clipboard.items[0].pinned);
        assert!(clipboard.items[1].detail.contains("2 files"));
        assert_eq!(clipboard.search_query, "match");
        assert_eq!(clipboard.total_items, 2);
        assert!(clipboard.status.contains("2 of 2 retained entries shown"));
        assert!(clipboard.status.contains("1 filtered as sensitive"));
        assert!(clipboard.status.contains("3 automatic selection clears"));
        assert!(clipboard.status.contains("2 over the size bound"));
    }

    #[test]
    fn clipboard_rows_are_empty_without_an_explicit_search() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        // No query means no row content.
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "",
                matches: &[],
                preview: None,
                ocr_unavailable: None,
                paste_unavailable: None,
            },
        );

        assert!(clipboard.items.is_empty(), "no content without a request");
        assert_eq!(clipboard.total_items, 2, "metadata stays visible");
        assert_eq!(clipboard.pinned_items, 1);
        assert!(
            !format!("{clipboard:?}").contains("matched text"),
            "no content leaks into the presentation without a request"
        );
    }

    #[test]
    fn clipboard_preview_exposes_bounded_text_and_rich_metadata() {
        let configuration = ApplicationConfiguration::default();
        let snapshot = clipboard_snapshot();
        let preview = ClipboardPreview {
            id: 8,
            kind: ClipboardEntryKind::Files,
            text: "/tmp/a\n/tmp/b".to_string(),
            truncated: true,
            format: kestrel_services::clipboard::PreviewFormat::Plain,
            size_bytes: 24,
            image_dimensions: None,
            file_count: Some(2),
        };
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation {
                snapshot: Some(snapshot),
                running: true,
                search_query: "",
                matches: &[],
                preview: Some(&preview),
                ocr_unavailable: None,
                paste_unavailable: None,
            },
        );

        let preview = clipboard.preview.expect("the requested preview is carried");
        assert_eq!(preview.text, "/tmp/a\n/tmp/b");
        assert!(preview.truncated);
        assert!(!preview.editable, "file entries are not text-editable");
        assert_eq!(preview.file_count, Some(2));
    }

    #[test]
    fn clipboard_policy_exposes_the_control_bounds() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_items = 42;
        configuration.clipboard.clear_seconds = 30;
        configuration.clipboard.filter_sensitive = true;
        let clipboard = ClipboardViewModel::from_presentation(
            &configuration.clipboard,
            ClipboardPresentation::default(),
        );

        let policy = clipboard.policy;
        assert_eq!(policy.max_items, 42);
        assert_eq!(policy.clear_seconds, 30);
        assert!(policy.filter_sensitive);
        assert_eq!(
            policy.bounds.min_item_bytes,
            kestrel_core::MIN_CLIPBOARD_ITEM_BYTES
        );
        assert_eq!(
            policy.bounds.max_items,
            kestrel_core::MAX_CLIPBOARD_MAX_ITEMS
        );
        assert_eq!(
            policy.bounds.max_clear_seconds,
            kestrel_core::MAX_CLIPBOARD_CLEAR_SECONDS
        );
    }
}
