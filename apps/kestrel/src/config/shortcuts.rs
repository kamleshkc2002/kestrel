//! Global shortcut bindings.

use serde::Deserialize;

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_shortcuts(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "shortcuts", value) else {
        return;
    };
    let Some(bindings) = section.get("bindings") else {
        return;
    };
    let Some(entries) = bindings.as_array() else {
        section.warn(
            "bindings",
            "Shortcut bindings must be an array of tables; the defaults were retained.",
        );
        section.finish(loaded);
        return;
    };
    // Each binding is checked on its own, so one bad entry drops only itself.
    let mut accepted: Vec<kestrel_core::ShortcutBinding> = Vec::new();
    let mut triggers: Vec<String> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let location = format!("bindings[{index}]");
        let binding = match kestrel_core::ShortcutBinding::deserialize(entry.clone()) {
            Ok(binding) => binding,
            Err(error) => {
                section.warn(
                    &location,
                    format!(
                        "Binding needs `command` and `trigger` strings and was ignored: {error}"
                    ),
                );
                continue;
            }
        };
        let trigger = match binding.validate() {
            Ok(trigger) => trigger.to_string().to_ascii_lowercase(),
            Err(error) => {
                section.warn(&location, format!("Binding was ignored: {error:?}"));
                continue;
            }
        };
        if accepted
            .iter()
            .any(|existing| existing.command == binding.command)
            || triggers.contains(&trigger)
        {
            section.warn(
                &location,
                "Binding repeats an earlier command or trigger and was ignored.",
            );
            continue;
        }
        if accepted.len() == kestrel_core::MAX_SHORTCUT_BINDINGS {
            section.warn(
                &location,
                format!(
                    "At most {} bindings are kept; the rest were ignored.",
                    kestrel_core::MAX_SHORTCUT_BINDINGS
                ),
            );
            break;
        }
        triggers.push(trigger);
        accepted.push(binding);
    }
    loaded.configuration.shortcuts.bindings = accepted;
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::{export_string, parse};

    #[test]
    fn shortcut_bindings_are_validated_one_by_one() {
        let loaded = parse(
            r#"
schema_version = 3
[[shortcuts.bindings]]
command = "microphone.toggle-mute"
trigger = "CTRL+ALT+m"

[[shortcuts.bindings]]
command = "window.show"
trigger = "k"

[[shortcuts.bindings]]
command = "audio.output-next"
trigger = "ctrl+alt+M"

[[shortcuts.bindings]]
command = "Bad Command"
trigger = "LOGO+x"

[[shortcuts.bindings]]
trigger = "LOGO+y"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.shortcuts.bindings,
            vec![kestrel_core::ShortcutBinding {
                command: "microphone.toggle-mute".to_owned(),
                trigger: "CTRL+ALT+m".to_owned(),
            }]
        );
        for (index, reason) in [
            (1, "a bare key cannot be a global trigger"),
            (2, "the same keys as binding 0"),
            (3, "an invalid command ID"),
            (4, "a missing command"),
        ] {
            let location = format!("shortcuts.bindings[{index}]");
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must warn for {reason}"
            );
        }
    }

    #[test]
    fn shortcut_bindings_default_when_absent_and_stay_empty_when_cleared() {
        let absent = parse("schema_version = 3\n").expect("valid");
        assert_eq!(
            absent.configuration.shortcuts,
            kestrel_core::ShortcutConfiguration::default()
        );
        assert!(!absent.configuration.shortcuts.bindings.is_empty());

        let cleared = parse("schema_version = 3\n[shortcuts]\nbindings = []\n").expect("valid");
        assert!(cleared.configuration.shortcuts.bindings.is_empty());
        assert!(cleared.warnings.is_empty());

        let exported = export_string(&absent.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.shortcuts,
            absent.configuration.shortcuts
        );
        assert!(reloaded.warnings.is_empty());
    }
}
