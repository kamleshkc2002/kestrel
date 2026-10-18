//! Shared view-model test fixtures.

use super::*;
use kestrel_platform::audio::{OutputDevice, PlaybackStream};
use kestrel_platform::clipboard::{ClipboardEntryKind, ClipboardKindSupport};
use kestrel_services::audio::{AudioAvailability, AudioReconcileOutcome, AudioSnapshot};
use kestrel_services::clipboard::{ClipboardItemMetadata, ClipboardMatch, ClipboardSnapshot};
use kestrel_services::snippets::{SnippetLibrary, SnippetPolicy};
use kestrel_services::speed_test::{SpeedTestSnapshot, SpeedTestStatus};
use std::time::Duration;

pub(crate) fn test_capture_presentation() -> super::CapturePresentation {
    super::CapturePresentation {
        snapshot: kestrel_services::capture::CaptureSnapshot::default(),
        running: false,
        provider: None,
        modes: Vec::new(),
        thumbnails: Vec::new(),
        limits: kestrel_core::CaptureConfiguration::default(),
        now: std::time::SystemTime::UNIX_EPOCH,
    }
}

pub(crate) fn test_shortcuts_presentation() -> ShortcutsPresentation<'static> {
    ShortcutsPresentation {
        status: kestrel_platform::global_shortcuts::ShortcutStatus::default(),
        running: false,
        configured: &[],
        skipped: &[],
    }
}

pub(crate) fn test_speed_presentation() -> SpeedTestPresentation {
    SpeedTestPresentation {
        snapshot: SpeedTestSnapshot {
            status: SpeedTestStatus::Idle,
            last_measurement: None,
            provider: kestrel_platform::speed_test::CLOUDFLARE_PROVIDER,
            plan: kestrel_platform::speed_test::SpeedTestPlan {
                download_bytes: 1,
                upload_bytes: 0,
                phase_timeout: Duration::from_secs(5),
            },
            available: false,
            generation: 0,
        },
        running: false,
    }
}
pub(crate) fn audio_output(
    id: u32,
    is_default: bool,
    volume_percent: u8,
    muted: bool,
) -> OutputDevice {
    OutputDevice {
        id,
        name: format!("output-{id}"),
        description: format!("Device {id}"),
        volume_percent,
        muted,
        is_default,
        card_name: Some("Built-in Audio".to_string()),
        port_description: Some("Headphones".to_string()),
    }
}

pub(crate) fn audio_stream(id: u32, output_id: u32, corked: bool) -> PlaybackStream {
    PlaybackStream {
        id,
        name: format!("Stream {id}"),
        application_name: Some(format!("Application {id}")),
        application_id: Some(format!("application-{id}")),
        media_name: Some("A track".to_string()),
        output_id,
        volume_percent: Some(60),
        volume_writable: true,
        muted: false,
        corked,
    }
}

pub(crate) fn audio_snapshot(streams: Vec<PlaybackStream>, inactive: usize) -> AudioSnapshot {
    AudioSnapshot {
        availability: AudioAvailability::Ready,
        outputs: vec![
            audio_output(3, true, 75, false),
            audio_output(4, false, 40, true),
        ],
        streams,
        inactive_streams: inactive,
        boost_ceiling_percent: 130,
        last_switch: None,
        last_reconcile: Some(AudioReconcileOutcome {
            lost_output_ids: vec![9],
            rehomed_streams: 1,
            volume_resets: 1,
            failures: 0,
        }),
    }
}

pub(crate) fn clipboard_snapshot() -> ClipboardSnapshot {
    ClipboardSnapshot {
        lifecycle: kestrel_services::clipboard::ClipboardLifecycle::Running,
        provider: Some(kestrel_platform::clipboard::ClipboardProvider::WaylandDataControl),
        kind_support: ClipboardKindSupport::ALL,
        items: vec![
            ClipboardItemMetadata {
                id: 7,
                kind: ClipboardEntryKind::Text,
                size_bytes: 12,
                age: Duration::from_secs(3),
                pinned: true,
                image_dimensions: None,
                file_count: None,
            },
            ClipboardItemMetadata {
                id: 8,
                kind: ClipboardEntryKind::Files,
                size_bytes: 24,
                age: Duration::from_secs(90),
                pinned: false,
                image_dimensions: None,
                file_count: Some(2),
            },
        ],
        total_bytes: 36,
        pinned_items: 1,
        rejected_oversize_items: 2,
        filtered_sensitive_items: 1,
        selection_clears: 3,
        wipe_count: 0,
        last_filtered_pattern: Some("password_assignment"),
        last_error: None,
    }
}

pub(crate) fn clipboard_matches() -> Vec<ClipboardMatch> {
    vec![
        ClipboardMatch {
            id: 7,
            kind: ClipboardEntryKind::Text,
            preview: "matched text".to_string(),
        },
        ClipboardMatch {
            id: 8,
            kind: ClipboardEntryKind::Files,
            preview: "/tmp/a, /tmp/b".to_string(),
        },
    ]
}

pub(crate) fn snippet_library() -> SnippetLibrary {
    let mut library = SnippetLibrary::default();
    library
        .upsert(
            kestrel_core::Snippet::new(
                "Address",
                Some("Contact".to_string()),
                Some(";addr".to_string()),
                "Street 1 {{date}} {{clipboard}}",
            ),
            SnippetPolicy::default(),
        )
        .expect("snippet is valid");
    library
}

pub(crate) fn snippet_presentation<'a>(
    library: &'a SnippetLibrary,
    matches: &'a [kestrel_services::snippets::SnippetMatch],
) -> SnippetsPresentation<'a> {
    SnippetsPresentation {
        library,
        policy: SnippetPolicy::default(),
        running: true,
        provider: Some(kestrel_platform::snippets::InsertionProvider::Wtype),
        unavailable_reason: None,
        stopped_reason: None,
        directory: Some("/tmp/example/kestrel".to_string()),
        expansion_timing: kestrel_core::SnippetExpansionTiming::Manual,
        provider_preference: kestrel_core::SnippetProviderPreference::Auto,
        search_query: "",
        matches,
        draft: None,
        warnings: &[],
    }
}
