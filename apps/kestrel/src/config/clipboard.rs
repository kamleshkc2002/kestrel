//! Clipboard retention and paste preferences.

use kestrel_core::{
    MAX_CLIPBOARD_AGE_HOURS, MAX_CLIPBOARD_CLEAR_SECONDS, MAX_CLIPBOARD_FILE_ENTRIES,
    MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_MAX_ITEMS, MAX_CLIPBOARD_TOTAL_BYTES,
    MIN_CLIPBOARD_ITEM_BYTES,
};

use super::{LoadedConfiguration, fields::Section};

const IGNORED_MESSAGE: &str = "The value is invalid and was ignored";
const HISTORY_LAYOUT_MESSAGE: &str =
    "History layout must be \"list\" or \"cards\"; the list was retained.";
const BOUND_PREFIX: &str = "The value must be";

pub(super) fn parse_clipboard(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "clipboard", value) else {
        return;
    };
    let limits = &mut loaded.configuration.clipboard;
    for (key, range, target) in [
        (
            "max_items",
            1..=MAX_CLIPBOARD_MAX_ITEMS,
            &mut limits.max_items,
        ),
        (
            "max_item_bytes",
            MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_TOTAL_BYTES,
            &mut limits.max_item_bytes,
        ),
        (
            "max_image_bytes",
            MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_IMAGE_BYTES,
            &mut limits.max_image_bytes,
        ),
        (
            "max_file_entries",
            1..=MAX_CLIPBOARD_FILE_ENTRIES,
            &mut limits.max_file_entries,
        ),
        (
            "max_total_bytes",
            MIN_CLIPBOARD_ITEM_BYTES..=MAX_CLIPBOARD_TOTAL_BYTES,
            &mut limits.max_total_bytes,
        ),
        (
            "max_age_hours",
            1..=MAX_CLIPBOARD_AGE_HOURS,
            &mut limits.max_age_hours,
        ),
    ] {
        section.in_range(key, range, (BOUND_PREFIX, ""), target);
    }
    section.in_range(
        "clear_seconds",
        0..=MAX_CLIPBOARD_CLEAR_SECONDS,
        ("The automatic clear interval must be", " seconds"),
        &mut limits.clear_seconds,
    );
    section.read_detailed(
        "filter_sensitive",
        IGNORED_MESSAGE,
        &mut limits.filter_sensitive,
    );
    section.read_detailed(
        "paste_plain_text",
        IGNORED_MESSAGE,
        &mut limits.paste_plain_text,
    );
    section.read(
        "history_layout",
        HISTORY_LAYOUT_MESSAGE,
        &mut limits.history_layout,
    );

    // Total-byte bounds cap per-item limits when inconsistent.
    let total = limits.max_total_bytes;
    let mut repaired = false;
    if limits.max_item_bytes > total {
        limits.max_item_bytes = total;
        repaired = true;
    }
    if limits.max_image_bytes > total {
        limits.max_image_bytes = total;
        repaired = true;
    }
    if repaired {
        section.warn(
            "max_total_bytes",
            format!(
                "The total byte bound ({total} bytes) is smaller than a single entry bound; the \
                 per-entry bounds were lowered to it."
            ),
        );
    }
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::ApplicationConfiguration;

    use crate::config::{export_string, parse};

    #[test]
    fn clipboard_history_layout_parses_and_a_bad_value_keeps_the_list() {
        let loaded = parse("schema_version = 3\n[clipboard]\nhistory_layout = \"cards\"\n")
            .expect("document itself is valid TOML");
        assert_eq!(
            loaded.configuration.clipboard.history_layout,
            kestrel_core::ClipboardLayout::Cards
        );
        assert!(loaded.warnings.is_empty());

        let loaded =
            parse("schema_version = 3\n[clipboard]\nhistory_layout = \"grid\"\nmax_items = 50\n")
                .expect("document itself is valid TOML");
        let clipboard = &loaded.configuration.clipboard;
        assert_eq!(
            clipboard.history_layout,
            kestrel_core::ClipboardLayout::List
        );
        assert_eq!(clipboard.max_items, 50, "other keys still apply");
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "clipboard.history_layout")
        );
    }

    #[test]
    fn clipboard_table_parses_every_retention_bound() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_items = 250
max_item_bytes = 524288
max_image_bytes = 2097152
max_file_entries = 12
max_total_bytes = 8388608
max_age_hours = 6
clear_seconds = 45
filter_sensitive = true
paste_plain_text = false
"#,
        )
        .expect("document itself is valid TOML");

        let clipboard = loaded.configuration.clipboard;
        assert_eq!(clipboard.max_items, 250);
        assert_eq!(clipboard.max_item_bytes, 524_288);
        assert_eq!(clipboard.max_image_bytes, 2_097_152);
        assert_eq!(clipboard.max_file_entries, 12);
        assert_eq!(clipboard.max_total_bytes, 8_388_608);
        assert_eq!(clipboard.max_age_hours, 6);
        assert_eq!(clipboard.clear_seconds, 45);
        assert!(clipboard.filter_sensitive);
        assert!(!clipboard.paste_plain_text);
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn malformed_clipboard_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_items = 100000
max_item_bytes = 0
max_image_bytes = "big"
max_file_entries = 0
max_age_hours = 0
clear_seconds = 999999
filter_sensitive = "yes"
paste_plain_text = 3
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.clipboard,
            kestrel_core::ClipboardConfiguration::default()
        );
        for location in [
            "clipboard.max_items",
            "clipboard.max_item_bytes",
            "clipboard.max_image_bytes",
            "clipboard.max_file_entries",
            "clipboard.max_age_hours",
            "clipboard.clear_seconds",
            "clipboard.filter_sensitive",
            "clipboard.paste_plain_text",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn an_inconsistent_clipboard_bound_pair_is_reported_and_repaired() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_total_bytes = 4096
max_item_bytes = 1048576
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.clipboard.max_item_bytes, 4096,
            "the per-entry bound is lowered to the explicit total bound"
        );
        assert_eq!(loaded.configuration.clipboard.max_image_bytes, 4096);
        assert_eq!(loaded.configuration.clipboard.max_total_bytes, 4096);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "clipboard.max_total_bytes")
        );
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn clipboard_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_items = 12;
        configuration.clipboard.clear_seconds = 90;
        configuration.clipboard.filter_sensitive = true;

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[clipboard]"));
        assert!(exported.contains("max_items = 12"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.clipboard, configuration.clipboard);
        assert!(reloaded.warnings.is_empty());
    }
}
