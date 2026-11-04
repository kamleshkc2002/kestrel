//! Optional update-check consent.

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_updates(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "updates", value) else {
        return;
    };
    let updates = &mut loaded.configuration.updates;
    section.read(
        "check",
        "Must be unasked, enabled, or disabled; the default was retained.",
        &mut updates.check,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::{
        ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION, UpdateCheck,
    };

    use crate::config::{export_string, parse};

    #[test]
    fn update_check_table_parses_every_value() {
        for (text, expected) in [
            ("unasked", UpdateCheck::Unasked),
            ("enabled", UpdateCheck::Enabled),
            ("disabled", UpdateCheck::Disabled),
        ] {
            let loaded = parse(&format!(
                "schema_version = {CURRENT_CONFIGURATION_SCHEMA_VERSION}\n[updates]\ncheck = \"{text}\"\n"
            ))
            .expect("valid configuration parses");
            assert_eq!(loaded.configuration.updates.check, expected, "{text}");
        }
    }

    #[test]
    fn malformed_update_check_is_isolated() {
        let loaded = parse(&format!(
            "schema_version = {CURRENT_CONFIGURATION_SCHEMA_VERSION}\n[updates]\ncheck = \"always\"\n"
        ))
        .expect("configuration still parses");
        assert_eq!(loaded.configuration.updates.check, UpdateCheck::Unasked);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "updates.check"),
            "{:?}",
            loaded.warnings
        );
    }

    #[test]
    fn update_check_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.updates.check = UpdateCheck::Disabled;
        let text = export_string(&configuration).expect("configuration exports");
        let loaded = parse(&text).expect("exported configuration parses");
        assert_eq!(loaded.configuration.updates.check, UpdateCheck::Disabled);
    }
}
