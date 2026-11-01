//! Feature toggles, appearance, and startup preferences.

use kestrel_core::{FeatureConfiguration, validate_feature_id};
use serde::Deserialize;

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section, feature_reason},
};

const APPEARANCE_MESSAGE: &str = "Appearance preference is invalid and defaults were retained";
const CLOSE_ACTION_MESSAGE: &str = "Must be \"minimize\" or \"quit\"; the default was retained.";
const AUTOSTART_MESSAGE: &str = "Autostart preference is invalid and defaults were retained";

pub(super) fn parse_features(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "features", value) else {
        return;
    };
    for (feature_id, value) in section.table() {
        if let Err(error) = validate_feature_id(feature_id) {
            section.warn(feature_id, feature_reason(error));
            continue;
        }
        match FeatureConfiguration::deserialize(value.clone()) {
            Ok(feature) => {
                loaded
                    .configuration
                    .features
                    .insert(feature_id.clone(), feature);
            }
            Err(error) => section.warn(
                feature_id,
                format!("Feature settings are invalid and were ignored: {error}"),
            ),
        }
    }
    section.finish(loaded);
}

pub(super) fn parse_ui(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "ui", value) else {
        return;
    };
    let ui = &mut loaded.configuration.ui;
    section.read_detailed("appearance", APPEARANCE_MESSAGE, &mut ui.appearance);
    section.read("close_action", CLOSE_ACTION_MESSAGE, &mut ui.close_action);
    section.finish(loaded);
}

pub(super) fn parse_startup(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "startup", value) else {
        return;
    };
    let startup = &mut loaded.configuration.startup;
    section.read_detailed("autostart", AUTOSTART_MESSAGE, &mut startup.autostart);
    section.read("whats_new", BOOL_MESSAGE, &mut startup.whats_new);
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::AppearancePreference;

    use crate::config::{export_string, parse};

    #[test]
    fn old_panel_sections_are_ignored_while_ui_settings_load() {
        let loaded = parse(
            r#"
schema_version = 3
[ui]
appearance = "dark"
[[ui.panel_sections]]
section = "feature_hub"
visible = false
[startup]
autostart = true
whats_new = false
"#,
        )
        .expect("legacy panel sections are ignored");
        assert_eq!(
            loaded.configuration.ui.appearance,
            AppearancePreference::Dark
        );
        assert!(loaded.configuration.startup.autostart);
        assert!(!loaded.configuration.startup.whats_new);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn close_action_defaults_to_minimize_and_rejects_unknown_values() {
        let default = parse("schema_version = 3\n").expect("valid");
        assert_eq!(
            default.configuration.ui.close_action,
            kestrel_core::CloseAction::Minimize
        );
        let quit = parse("schema_version = 3\n[ui]\nclose_action = \"quit\"\n").expect("valid");
        assert_eq!(
            quit.configuration.ui.close_action,
            kestrel_core::CloseAction::Quit
        );
        let reloaded =
            parse(&export_string(&quit.configuration).expect("exports")).expect("parses");
        assert_eq!(
            reloaded.configuration.ui.close_action,
            kestrel_core::CloseAction::Quit
        );
        let bad = parse("schema_version = 3\n[ui]\nclose_action = \"hide\"\n").expect("valid TOML");
        assert_eq!(
            bad.configuration.ui.close_action,
            kestrel_core::CloseAction::Minimize
        );
        assert!(
            bad.warnings
                .iter()
                .any(|warning| warning.feature_id == "ui.close_action")
        );
    }
}
