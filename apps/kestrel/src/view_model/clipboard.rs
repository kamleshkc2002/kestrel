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
            bounds,
        };
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
                }
            })
            .collect::<Vec<_>>();

        Self {
            running: true,
            status: clipboard_status(snapshot, presentation.matches.len()),
            search_query: presentation.search_query.to_owned(),
            items,
            total_items: snapshot.items.len(),
            pinned_items: snapshot.pinned_items,
            preview: presentation.preview.map(clipboard_preview),
            policy,
            newest_id: snapshot.items.iter().map(|item| item.id).max(),
            layout: configuration.history_layout,
            ocr_unavailable: presentation.ocr_unavailable,
        }
    }
}

fn clipboard_status(snapshot: &ClipboardSnapshot, shown: usize) -> String {
    let mut status = format!(
        "{} of {} retained entries shown · {} retained · captured kinds: {}",
        shown,
        snapshot.items.len(),
        format_clipboard_bytes(snapshot.total_bytes),
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
    if let Some(error) = &snapshot.last_error {
        status.push_str(&format!(" · last error: {error}"));
    }
    status
}

fn clipboard_item_detail(metadata: &kestrel_services::clipboard::ClipboardItemMetadata) -> String {
    let mut parts = vec![
        metadata.kind.label().to_owned(),
        format_clipboard_bytes(metadata.size_bytes),
        format_age(metadata.age),
    ];
    if metadata.pinned {
        parts.push("pinned".to_owned());
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

pub(super) fn format_clipboard_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
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
        assert_eq!(
            clipboard.policy.max_items,
            configuration.clipboard.max_items
        );
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
