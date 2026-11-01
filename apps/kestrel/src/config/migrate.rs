//! Schema versions, migrations, and the current document dispatch.

use super::{
    ConfigurationLoadError, LoadedConfiguration,
    audio::parse_audio,
    bounds::{parse_processes, parse_scratchpad},
    capture::{parse_capture, parse_recording, parse_screen_tools},
    clipboard::parse_clipboard,
    command_bar::parse_command_bar,
    features::{parse_features, parse_startup, parse_ui},
    fields::feature_reason,
    logging::parse_logging,
    monitoring::parse_monitoring,
    overlay::parse_overlay,
    quick_panel::parse_quick_panel,
    shortcuts::parse_shortcuts,
    snippets::parse_snippets,
    speed_test::parse_speed_test,
    warning,
};

/// Feature ID of the retired Battery alerts quick toggle.
const RETIRED_BATTERY_ALERTS_ID: &str = "power.battery-alerts";

pub(super) fn parse_current(
    document: toml::Table,
) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
    parse_monitoring(&mut loaded, document.get("monitoring"));
    parse_audio(&mut loaded, document.get("audio"));
    parse_speed_test(&mut loaded, document.get("speed_test"));
    parse_capture(&mut loaded, document.get("capture"));
    parse_processes(&mut loaded, document.get("processes"));
    parse_screen_tools(&mut loaded, document.get("screen_tools"));
    parse_shortcuts(&mut loaded, document.get("shortcuts"));
    parse_clipboard(&mut loaded, document.get("clipboard"));
    parse_snippets(&mut loaded, document.get("snippets"));
    parse_scratchpad(&mut loaded, document.get("scratchpad"));
    parse_recording(&mut loaded, document.get("recording"));
    parse_command_bar(&mut loaded, document.get("command_bar"));
    parse_quick_panel(&mut loaded, document.get("quick_panel"));
    parse_logging(&mut loaded, document.get("logging"));
    parse_overlay(&mut loaded, document.get("overlay"));
    retire_battery_alerts_toggle(&mut loaded);
    Ok(loaded)
}

/// The Battery alerts quick toggle was retired; the Monitoring alert rule is
/// the only switch. A toggle left off had kept the rule silent, so the rule
/// takes that state once and the stale feature entry is dropped.
fn retire_battery_alerts_toggle(loaded: &mut LoadedConfiguration) {
    let configuration = &mut loaded.configuration;
    if let Some(toggle) = configuration.features.remove(RETIRED_BATTERY_ALERTS_ID) {
        if !toggle.enabled {
            configuration.monitoring.alerts.battery.enabled = false;
        }
    }
}

pub(super) fn migrate_v0(
    document: toml::Table,
) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    let Some(enabled_features) = document.get("enabled_features") else {
        return Ok(loaded);
    };
    let Some(feature_ids) = enabled_features.as_array() else {
        loaded.warnings.push(warning(
            "enabled_features",
            "Legacy enabled_features must be an array of feature IDs.",
        ));
        return Ok(loaded);
    };

    for (index, value) in feature_ids.iter().enumerate() {
        let Some(feature_id) = value.as_str() else {
            loaded.warnings.push(warning(
                &format!("enabled_features[{index}]"),
                "Legacy enabled_features entries must be strings.",
            ));
            continue;
        };
        if loaded.configuration.features.contains_key(feature_id) {
            loaded.warnings.push(warning(
                &format!("features.{feature_id}"),
                "Duplicate legacy feature entry was ignored; the first entry was retained.",
            ));
            continue;
        }
        if let Err(error) = loaded.configuration.set_feature_enabled(feature_id, true) {
            loaded.warnings.push(warning(
                &format!("features.{feature_id}"),
                feature_reason(error),
            ));
        }
    }
    Ok(loaded)
}

pub(super) fn parse_v2(
    document: toml::Table,
) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
    retire_battery_alerts_toggle(&mut loaded);
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use kestrel_core::{AppearancePreference, CURRENT_CONFIGURATION_SCHEMA_VERSION};

    use crate::config::parse;

    #[test]
    fn migrates_legacy_enabled_features() {
        let loaded = parse("enabled_features = [\"audio.mixer\"]").expect("legacy config migrates");
        assert!(loaded.configuration.feature_enabled("audio.mixer"));
        assert_eq!(
            loaded.configuration.schema_version,
            CURRENT_CONFIGURATION_SCHEMA_VERSION
        );
    }

    #[test]
    fn migrates_v1_and_parses_v2_globals() {
        let loaded = parse(
            r#"
schema_version = 1
[features."audio.mixer"]
enabled = true
"#,
        )
        .expect("v1 config migrates");
        assert!(loaded.configuration.feature_enabled("audio.mixer"));
        assert_eq!(
            loaded.configuration.ui.appearance,
            AppearancePreference::System
        );
        assert!(loaded.configuration.startup.whats_new);

        let loaded = parse(
            r#"
schema_version = 2
[ui]
appearance = "dark"
[startup]
autostart = true
"#,
        )
        .expect("v2 config parses");
        assert_eq!(
            loaded.configuration.ui.appearance,
            AppearancePreference::Dark
        );
        assert!(loaded.configuration.startup.autostart);
    }

    #[test]
    fn malformed_v2_siblings_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 2
[features."audio.mixer"]
enabled = true
[features.clipboard]
enabled = "yes"
[ui]
appearance = "invalid"
[startup]
autostart = "yes"
"#,
        )
        .expect("document itself is valid TOML");
        assert!(loaded.configuration.feature_enabled("audio.mixer"));
        assert!(!loaded.configuration.startup.autostart);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "features.clipboard")
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "ui.appearance")
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "startup.autostart")
        );
    }

    #[test]
    fn retired_battery_toggle_left_off_keeps_battery_alerts_off() {
        let parse_battery = |toggle: &str| {
            let loaded = parse(&format!(
                "schema_version = 3\n{toggle}\n[monitoring.alerts.battery]\nenabled = true\n"
            ))
            .expect("document itself is valid TOML");
            assert!(
                !loaded
                    .configuration
                    .features
                    .contains_key("power.battery-alerts"),
                "the retired feature entry is dropped"
            );
            loaded.configuration.monitoring.alerts.battery.enabled
        };
        assert!(!parse_battery(
            "[features.\"power.battery-alerts\"]\nenabled = false"
        ));
        assert!(parse_battery(
            "[features.\"power.battery-alerts\"]\nenabled = true"
        ));
        assert!(parse_battery(""));
    }
}
