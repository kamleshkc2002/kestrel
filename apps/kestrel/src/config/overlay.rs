//! Status overlay module visibility.

use serde::Deserialize;

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section},
};

pub(super) fn parse_overlay(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "overlay", value) else {
        return;
    };
    let modules = &mut loaded.configuration.overlay;
    for (key, target) in [
        ("microphone", &mut modules.microphone),
        ("recording", &mut modules.recording),
        ("alerts", &mut modules.alerts),
        ("volume", &mut modules.volume),
        ("microphone_level", &mut modules.microphone_level),
        ("keyboard_light", &mut modules.keyboard_light),
        ("accessories", &mut modules.accessories),
        ("keep_awake", &mut modules.keep_awake),
    ] {
        section.read(key, BOOL_MESSAGE, target);
    }
    if section.get("accessory_warning").is_some() || section.get("accessory_critical").is_some() {
        let level = |key: &str, default: u8| {
            section
                .get(key)
                .map_or(Some(default), |value| u8::deserialize(value.clone()).ok())
        };
        let warning_level = level("accessory_warning", modules.accessory_warning);
        let critical_level = level("accessory_critical", modules.accessory_critical);
        match (warning_level, critical_level) {
            (Some(warning_level), Some(critical_level))
                if kestrel_core::accessory_levels_valid(critical_level, warning_level) =>
            {
                modules.accessory_warning = warning_level;
                modules.accessory_critical = critical_level;
            }
            _ => section.warn(
                "accessory_levels",
                concat!(
                    "Must be whole numbers from 1 to 50 with the critical level below ",
                    "the warning level; the defaults were retained."
                ),
            ),
        }
    }
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::parse;

    #[test]
    fn on_off_settings_parse_and_a_malformed_one_keeps_its_default() {
        let loaded = parse(
            "schema_version = 3\n[overlay]\nmicrophone = false\nrecording = \"no\"\nalerts = false\nvolume = false\nmicrophone_level = \"no\"\nkeyboard_light = true\naccessories = false\nkeep_awake = \"no\"\naccessory_warning = 35\naccessory_critical = 15\n[startup]\nwhats_new = \"no\"\n",
        )
        .expect("document itself is valid TOML");
        let overlay = loaded.configuration.overlay;
        assert!(!overlay.microphone && !overlay.alerts && !overlay.volume && !overlay.accessories);
        assert!(overlay.keyboard_light);
        assert!(
            overlay.recording && overlay.microphone_level && overlay.keep_awake,
            "a malformed value keeps its default"
        );
        assert_eq!(
            (overlay.accessory_warning, overlay.accessory_critical),
            (35, 15)
        );
        assert!(loaded.configuration.startup.whats_new);
        let mut warned = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        warned.sort_unstable();
        assert_eq!(
            warned,
            [
                "overlay.keep_awake",
                "overlay.microphone_level",
                "overlay.recording",
                "startup.whats_new"
            ]
        );

        let without = parse("schema_version = 3\n[overlay]\nkeep_awake = false\n")
            .expect("document itself is valid TOML");
        assert!(
            !without.configuration.overlay.keep_awake,
            "an explicit false is kept"
        );
        assert!(
            parse("schema_version = 3\n[overlay]\nalerts = false\n")
                .expect("document itself is valid TOML")
                .configuration
                .overlay
                .keep_awake,
            "the default is on"
        );

        let inverted = parse(
            "schema_version = 3\n[overlay]\naccessory_warning = 5\naccessory_critical = 12\n",
        )
        .expect("document itself is valid TOML");
        let overlay = inverted.configuration.overlay;
        assert_eq!(
            (overlay.accessory_warning, overlay.accessory_critical),
            (
                kestrel_core::DEFAULT_ACCESSORY_WARNING_LEVEL,
                kestrel_core::DEFAULT_ACCESSORY_CRITICAL_LEVEL
            ),
            "an inverted pair keeps both defaults"
        );
        assert_eq!(
            inverted
                .warnings
                .iter()
                .map(|warning| warning.feature_id.as_str())
                .collect::<Vec<_>>(),
            ["overlay.accessory_levels"]
        );
    }
}
