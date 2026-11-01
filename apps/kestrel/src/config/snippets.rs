//! Snippet bounds and insertion preferences.

use kestrel_core::{
    MAX_SNIPPET_CLIPBOARD_BYTES, MAX_SNIPPET_CONTENT_BYTES, MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
    MIN_SNIPPET_CLIPBOARD_BYTES, MIN_SNIPPET_CONTENT_BYTES, MIN_SNIPPET_INSERT_TIMEOUT_MILLIS,
};

use super::{LoadedConfiguration, fields::Section};

const PROVIDER_MESSAGE: &str = "The insertion provider preference is invalid and was ignored";
const TIMING_MESSAGE: &str = "The expansion timing value is invalid and was ignored";
const BOUND_PREFIX: &str = "The value must be";

pub(super) fn parse_snippets(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "snippets", value) else {
        return;
    };
    let snippets = &mut loaded.configuration.snippets;
    if let Some(bytes) = section.number(
        "max_content_bytes",
        u64::from(MIN_SNIPPET_CONTENT_BYTES)..=u64::from(MAX_SNIPPET_CONTENT_BYTES),
        (BOUND_PREFIX, ""),
    ) {
        snippets.max_content_bytes = bytes as u32;
    }
    if let Some(bytes) = section.number(
        "clipboard_variable_bytes",
        u64::from(MIN_SNIPPET_CLIPBOARD_BYTES)..=u64::from(MAX_SNIPPET_CLIPBOARD_BYTES),
        (BOUND_PREFIX, ""),
    ) {
        snippets.clipboard_variable_bytes = bytes as u32;
    }
    if let Some(millis) = section.number(
        "insert_timeout_millis",
        MIN_SNIPPET_INSERT_TIMEOUT_MILLIS..=MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
        (BOUND_PREFIX, ""),
    ) {
        snippets.insert_timeout_millis = millis;
    }
    section.read_detailed(
        "preferred_provider",
        PROVIDER_MESSAGE,
        &mut snippets.preferred_provider,
    );
    section.read_detailed(
        "expansion_timing",
        TIMING_MESSAGE,
        &mut snippets.expansion_timing,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::ApplicationConfiguration;

    use crate::config::{export_string, parse};

    #[test]
    fn snippets_table_parses_bounds_provider_and_timing() {
        let loaded = parse(
            r#"
schema_version = 3
[snippets]
max_content_bytes = 4096
clipboard_variable_bytes = 128
insert_timeout_millis = 750
preferred_provider = "xdotool"
expansion_timing = "delimiter"
"#,
        )
        .expect("document itself is valid TOML");

        let snippets = loaded.configuration.snippets;
        assert_eq!(snippets.max_content_bytes, 4096);
        assert_eq!(snippets.clipboard_variable_bytes, 128);
        assert_eq!(snippets.insert_timeout_millis, 750);
        assert_eq!(
            snippets.preferred_provider,
            kestrel_core::SnippetProviderPreference::Xdotool
        );
        assert_eq!(
            snippets.expansion_timing,
            kestrel_core::SnippetExpansionTiming::Delimiter
        );
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn malformed_snippet_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[snippets]
max_content_bytes = 0
clipboard_variable_bytes = 0
insert_timeout_millis = 1
preferred_provider = "telepathy"
expansion_timing = "vibes"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.snippets,
            kestrel_core::SnippetConfiguration::default()
        );
        for location in [
            "snippets.max_content_bytes",
            "snippets.clipboard_variable_bytes",
            "snippets.insert_timeout_millis",
            "snippets.preferred_provider",
            "snippets.expansion_timing",
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
    fn export_carries_snippet_bounds_but_never_snippet_content() {
        // Snippet content stays private and out of portable exports.
        let mut configuration = ApplicationConfiguration::default();
        configuration.snippets.max_content_bytes = 4096;

        let exported = export_string(&configuration).expect("configuration exports");

        assert!(exported.contains("[snippets]"));
        assert!(exported.contains("max_content_bytes = 4096"));
        assert!(!exported.contains("{{date}}"));
        assert!(!exported.contains("{{clipboard}}"));
        assert!(!exported.contains("Address"));
    }
}
