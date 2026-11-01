//! Command bar providers, file roots, and scripts.

use kestrel_core::{MAX_COMMAND_RESULTS, MIN_COMMAND_RESULTS};
use serde::Deserialize;

use super::{LoadedConfiguration, fields::Section};

const IGNORED_MESSAGE: &str = "The value is invalid and was ignored";

pub(super) fn parse_command_bar(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "command_bar", value) else {
        return;
    };
    let command_bar = &mut loaded.configuration.command_bar;
    section.in_range(
        "max_results",
        MIN_COMMAND_RESULTS..=MAX_COMMAND_RESULTS,
        ("The result count must be", ""),
        &mut command_bar.max_results,
    );
    for (key, target) in [
        ("enable_applications", &mut command_bar.enable_applications),
        ("enable_files", &mut command_bar.enable_files),
        ("enable_scripts", &mut command_bar.enable_scripts),
        ("enable_emoji", &mut command_bar.enable_emoji),
        ("enable_windows", &mut command_bar.enable_windows),
        ("enable_clipboard", &mut command_bar.enable_clipboard),
    ] {
        section.read_detailed(key, IGNORED_MESSAGE, target);
    }
    if let Some(roots) = section.get("file_roots") {
        match Vec::<String>::deserialize(roots.clone()) {
            Ok(parsed)
                if parsed.len() <= kestrel_core::MAX_COMMAND_FILE_ROOTS
                    && parsed.iter().all(|root| root.trim().starts_with('/')) =>
            {
                command_bar.file_roots = parsed;
            }
            _ => section.warn(
                "file_roots",
                format!(
                    "File roots must be at most {} absolute paths; the \
                     default was retained.",
                    kestrel_core::MAX_COMMAND_FILE_ROOTS
                ),
            ),
        }
    }
    if let Some(scripts) = section.get("scripts") {
        match Vec::<kestrel_core::CommandScriptConfiguration>::deserialize(scripts.clone()) {
            Ok(parsed) => {
                // Malformed scripts are dropped while the table remains valid.
                let mut accepted = Vec::new();
                let mut names: Vec<String> = Vec::new();
                for (index, script) in parsed.into_iter().enumerate() {
                    let location = format!("scripts[{index}]");
                    if let Err(error) = script.validate() {
                        section.warn(&location, format!("Script was ignored: {error:?}"));
                        continue;
                    }
                    let name = script.name.trim().to_string();
                    if names.contains(&name) {
                        section.warn(
                            &location,
                            format!("Script \"{name}\" is a duplicate and was ignored"),
                        );
                        continue;
                    }
                    names.push(name);
                    accepted.push(script);
                }
                command_bar.scripts = accepted;
            }
            Err(error) => section.warn(
                "scripts",
                format!("Script definitions are invalid and were ignored: {error}"),
            ),
        }
    }
    // Invalid command-bar roots are dropped while switches remain.
    if let Err(error) = command_bar.validate() {
        let existing = command_bar.clone();
        *command_bar = kestrel_core::CommandBarConfiguration {
            max_results: existing.max_results,
            enable_applications: existing.enable_applications,
            enable_windows: existing.enable_windows,
            enable_clipboard: existing.enable_clipboard,
            enable_files: false,
            enable_scripts: existing.enable_scripts,
            enable_emoji: existing.enable_emoji,
            file_roots: Vec::new(),
            scripts: existing.scripts,
        };
        section.warn_at(
            "command_bar",
            format!("The command bar configuration was inconsistent and was repaired: {error:?}"),
        );
    }
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::parse;

    #[test]
    fn command_bar_table_parses_providers_roots_and_scripts() {
        let loaded = parse(
            r#"
schema_version = 3
[command_bar]
max_results = 20
enable_applications = true
enable_files = true
enable_scripts = true
enable_emoji = false
file_roots = ["/home/user/Documents", "/srv/notes"]

[[command_bar.scripts]]
name = "Restart audio"
executable = "systemctl"
args = ["--user", "restart", "pipewire"]
timeout_millis = 4000
output_bytes = 2048
"#,
        )
        .expect("document itself is valid TOML");

        let command_bar = loaded.configuration.command_bar.clone();
        assert_eq!(command_bar.max_results, 20);
        assert!(command_bar.enable_files);
        assert!(!command_bar.enable_emoji);
        assert_eq!(command_bar.file_roots.len(), 2);
        assert_eq!(command_bar.scripts.len(), 1);
        assert_eq!(command_bar.scripts[0].name, "Restart audio");
        assert_eq!(command_bar.scripts[0].args.len(), 3);
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn malformed_command_bar_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[command_bar]
max_results = 5000
enable_files = "yes"
file_roots = ["relative/path"]
[[command_bar.scripts]]
name = ""
executable = "//bad//path"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.command_bar.max_results,
            kestrel_core::DEFAULT_COMMAND_RESULTS
        );
        assert!(!loaded.configuration.command_bar.enable_files);
        assert!(loaded.configuration.command_bar.file_roots.is_empty());
        for location in [
            "command_bar.max_results",
            "command_bar.enable_files",
            "command_bar.file_roots",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "command_bar.scripts[0]"),
            "the malformed script definition must be reported and dropped"
        );
        assert!(
            loaded.configuration.command_bar.scripts.is_empty(),
            "malformed script definitions never survive loading"
        );
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn file_search_needs_a_root_and_relative_roots_are_refused() {
        let loaded = parse(
            r#"
schema_version = 3
[command_bar]
enable_files = true
"#,
        )
        .expect("document itself is valid TOML");

        assert!(
            !loaded.configuration.command_bar.enable_files,
            "file search without a root is turned off rather than indexing anything"
        );
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }
}
