//! Quick panel favorites.

use serde::Deserialize;

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_quick_panel(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "quick_panel", value) else {
        return;
    };
    let Some(favorites) = section.get("favorites") else {
        return;
    };
    let Ok(parsed) = Vec::<String>::deserialize(favorites.clone()) else {
        section.warn(
            "favorites",
            "Favorites must be an array of command IDs; the default was retained.",
        );
        section.finish(loaded);
        return;
    };
    let mut accepted = Vec::new();
    for (index, id) in parsed.into_iter().enumerate() {
        let location = format!("favorites[{index}]");
        if accepted.len() >= kestrel_core::MAX_QUICK_PANEL_FAVORITES {
            section.warn(
                &location,
                format!(
                    "The favorite limit is {}; entry ignored.",
                    kestrel_core::MAX_QUICK_PANEL_FAVORITES
                ),
            );
            continue;
        }
        if crate::command_line::CommandLineAction::parse(&id).is_err() {
            section.warn(
                &location,
                format!("Unknown command ID \"{id}\"; entry ignored."),
            );
            continue;
        }
        if accepted.contains(&id) {
            section.warn(
                &location,
                format!("Duplicate command ID \"{id}\"; entry ignored."),
            );
            continue;
        }
        accepted.push(id);
    }
    loaded.configuration.quick_panel.favorites = accepted;
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::parse;

    #[test]
    fn quick_panel_favorites_drop_unknown_duplicates_and_excess() {
        let loaded = parse(
            r#"
schema_version = 3
[quick_panel]
favorites = ["command-bar.open", "missing.action", "command-bar.open", "quick-panel.open"]
"#,
        )
        .expect("quick panel parses");
        assert_eq!(
            loaded.configuration.quick_panel.favorites,
            ["command-bar.open", "quick-panel.open"]
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "quick_panel.favorites[1]")
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "quick_panel.favorites[2]")
        );
    }
}
