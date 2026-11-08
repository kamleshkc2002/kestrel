//! Snapshot checks for the compatibility promise in `docs/stability.md`.

mod tests {
    use std::collections::BTreeSet;

    use kestrel_core::ApplicationConfiguration;

    use crate::command_line::CommandLineAction;
    use crate::config;
    use crate::runtime::ApplicationRuntime;

    /// Feature IDs released to users. Append new IDs; removal needs a migration.
    const RELEASED_FEATURE_IDS: &[&str] = &[
        "app.command-surface",
        "app.status-notifier",
        "audio.microphone",
        "audio.mixer",
        "camera.preview",
        "capture.color",
        "capture.qr",
        "capture.recording",
        "capture.screenshot",
        "capture.text",
        "clipboard.history",
        "commands.bar",
        "desktop.appearance",
        "desktop.icons",
        "display.brightness",
        "files.empty-trash",
        "files.shelf",
        "files.show-hidden",
        "global.shortcuts",
        "keyboard.backlight",
        "network.listeners",
        "network.speed_test",
        "notes.scratchpad",
        "overlay.status",
        "power.keep-awake",
        "radio.bluetooth",
        "radio.wifi",
        "session.lock",
        "snippets.text",
        "storage.eject",
        "system.monitor",
        "system.processes",
    ];

    /// `kestrel --command` IDs released to users. Append new IDs.
    const RELEASED_COMMAND_IDS: &[&str] = &[
        "app.quit",
        "audio.output-next",
        "audio.output-port-next",
        "audio.output-previous",
        "capabilities.refresh",
        "capture.color",
        "capture.edit-last",
        "capture.qr",
        "capture.screen",
        "capture.screenshot",
        "capture.text",
        "clipboard.clear-selection",
        "clipboard.quick-paste",
        "clipboard.wipe",
        "command-bar.open",
        "diagnostics.dump-state",
        "keep-awake.start-15m",
        "keep-awake.start-1h",
        "keep-awake.start-2h",
        "keep-awake.start-30m",
        "keep-awake.start-4h",
        "keep-awake.start-8h",
        "keep-awake.start-until-stopped",
        "keep-awake.stop",
        "microphone.mute",
        "microphone.toggle-mute",
        "microphone.unmute",
        "preset.balanced",
        "preset.essentials",
        "preset.everything",
        "preset.undo",
        "processes.open",
        "quick-panel.open",
        "recording.start",
        "recording.stop",
        "scratchpad.open",
        "shelf.add-clipboard",
        "shelf.clear",
        "shelf.open",
        "shelf.page",
        "speed-test.cancel",
        "speed-test.start",
        "toggle.desktop.appearance",
        "toggle.desktop.icons",
        "toggle.display.brightness",
        "toggle.files.empty-trash",
        "toggle.files.show-hidden",
        "toggle.keyboard.backlight",
        "toggle.power.keep-awake",
        "toggle.radio.bluetooth",
        "toggle.radio.wifi",
        "toggle.session.lock",
        "toggle.storage.eject",
        "window.show",
    ];

    /// Configuration key paths released to users. Append new keys.
    ///
    /// The live list comes from the default export, which serializes every key
    /// the parser reads. Arrays of tables use `[]` for one element's fields.
    const RELEASED_CONFIGURATION_KEYS: &[&str] = &[
        "audio.boost_percent",
        "audio.disconnect_policy",
        "audio.disconnect_volume_percent",
        "audio.follow_input_priority",
        "audio.follow_output_priority",
        "audio.include_inactive_streams",
        "audio.input_priority",
        "audio.mixer_favorites",
        "audio.output_priority",
        "audio.output_switch",
        "capture.after_screenshot",
        "capture.max_age_hours",
        "capture.max_entries",
        "capture.max_total_megabytes",
        "clipboard.clear_seconds",
        "clipboard.filter_sensitive",
        "clipboard.history_layout",
        "clipboard.max_age_hours",
        "clipboard.max_file_entries",
        "clipboard.max_image_bytes",
        "clipboard.max_item_bytes",
        "clipboard.max_items",
        "clipboard.max_total_bytes",
        "clipboard.paste_plain_text",
        "command_bar.enable_applications",
        "command_bar.enable_clipboard",
        "command_bar.enable_emoji",
        "command_bar.enable_files",
        "command_bar.enable_scripts",
        "command_bar.enable_windows",
        "command_bar.file_roots",
        "command_bar.max_results",
        "command_bar.scripts[].args",
        "command_bar.scripts[].executable",
        "command_bar.scripts[].name",
        "command_bar.scripts[].output_bytes",
        "command_bar.scripts[].timeout_millis",
        "features",
        "logging.debug",
        "logging.max_total_megabytes",
        "logging.retention_days",
        "monitoring.alerts.battery.cooldown_seconds",
        "monitoring.alerts.battery.enabled",
        "monitoring.alerts.battery.sustain_samples",
        "monitoring.alerts.battery.threshold",
        "monitoring.alerts.cpu.cooldown_seconds",
        "monitoring.alerts.cpu.enabled",
        "monitoring.alerts.cpu.sustain_samples",
        "monitoring.alerts.cpu.threshold",
        "monitoring.alerts.disk.cooldown_seconds",
        "monitoring.alerts.disk.enabled",
        "monitoring.alerts.disk.sustain_samples",
        "monitoring.alerts.disk.threshold",
        "monitoring.alerts.memory.cooldown_seconds",
        "monitoring.alerts.memory.enabled",
        "monitoring.alerts.memory.sustain_samples",
        "monitoring.alerts.memory.threshold",
        "monitoring.alerts.temperature.cooldown_seconds",
        "monitoring.alerts.temperature.enabled",
        "monitoring.alerts.temperature.sustain_samples",
        "monitoring.alerts.temperature.threshold",
        "monitoring.history_samples",
        "monitoring.network_rate_unit",
        "monitoring.per_core_cpu",
        "monitoring.readouts",
        "monitoring.refresh_interval_millis",
        "monitoring.tray_readouts",
        "overlay.accessories",
        "overlay.accessory_critical",
        "overlay.accessory_warning",
        "overlay.alerts",
        "overlay.keep_awake",
        "overlay.keyboard_light",
        "overlay.microphone",
        "overlay.microphone_level",
        "overlay.recording",
        "overlay.volume",
        "processes.max_rows",
        "processes.refresh_millis",
        "quick_panel.favorites",
        "recording.cursor",
        "recording.framerate",
        "recording.microphone",
        "recording.system_audio",
        "recording.system_audio_app",
        "schema_version",
        "scratchpad.max_age_days",
        "scratchpad.max_note_bytes",
        "scratchpad.max_notes",
        "screen_tools.ocr_language",
        "screen_tools.ocr_timeout_seconds",
        "shelf.add_screenshots",
        "shelf.keep_across_restarts",
        "shelf.max_age_hours",
        "shelf.max_image_megabytes",
        "shelf.max_items",
        "shelf.max_text_megabytes",
        "shortcuts.bindings[].command",
        "shortcuts.bindings[].trigger",
        "snippets.clipboard_variable_bytes",
        "snippets.expansion_timing",
        "snippets.insert_timeout_millis",
        "snippets.max_content_bytes",
        "snippets.preferred_provider",
        "speed_test.download_megabytes",
        "speed_test.timeout_seconds",
        "speed_test.upload_megabytes",
        "startup.autostart",
        "startup.whats_new",
        "ui.appearance",
        "ui.close_action",
        "updates.check",
        "url_cleanup.automatic",
        "url_cleanup.extra_global",
        "url_cleanup.sites[].enabled",
        "url_cleanup.sites[].host",
        "url_cleanup.sites[].params",
    ];

    #[test]
    fn released_feature_ids_stay_available() {
        let runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        let live = runtime
            .registrations()
            .map(|registration| registration.feature.id.to_owned())
            .collect();
        compare("feature ID", live, RELEASED_FEATURE_IDS);
    }

    #[test]
    fn released_command_ids_stay_available() {
        let mut live = CommandLineAction::all()
            .into_iter()
            .map(CommandLineAction::id)
            .collect::<BTreeSet<_>>();
        // The recovery checks query; hidden from `--list-commands`.
        live.insert(crate::command_line::DIAGNOSTICS_COMMAND_ID.to_owned());
        compare("command ID", live, RELEASED_COMMAND_IDS);
    }

    #[test]
    fn released_configuration_keys_stay_available() {
        compare(
            "configuration key",
            live_configuration_keys(),
            RELEASED_CONFIGURATION_KEYS,
        );
    }

    /// Every key the writer emits, which is every key the parser reads.
    ///
    /// A sample with one entry in each array of tables makes their nested keys
    /// visible; the default export leaves those arrays empty. The empty
    /// `features` table is named here because its entries are the released
    /// feature IDs checked above.
    fn live_configuration_keys() -> BTreeSet<String> {
        let sample = r#"
schema_version = 4

[recording]
system_audio_app = "sample"

[[command_bar.scripts]]
name = "sample"
executable = "sample"

[[url_cleanup.sites]]
host = "example.com"
"#;
        let configuration = config::import_string(sample)
            .expect("sample configuration imports")
            .configuration;
        let exported = config::export_string(&configuration).expect("configuration exports");
        let document: toml::Table = exported.parse().expect("exported TOML parses");
        let mut keys = BTreeSet::new();
        collect_keys("", &document, &mut keys);
        keys.insert("features".to_owned());
        keys
    }

    fn collect_keys(prefix: &str, table: &toml::Table, keys: &mut BTreeSet<String>) {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                toml::Value::Table(nested) => collect_keys(&path, nested, keys),
                toml::Value::Array(values) if values.iter().any(toml::Value::is_table) => {
                    for value in values {
                        if let Some(nested) = value.as_table() {
                            collect_keys(&format!("{path}[]"), nested, keys);
                        }
                    }
                }
                _ => {
                    keys.insert(path);
                }
            }
        }
    }

    fn compare(kind: &str, live: BTreeSet<String>, released: &[&str]) {
        let released = released
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect::<BTreeSet<_>>();
        let removed = released.difference(&live).collect::<Vec<_>>();
        assert!(
            removed.is_empty(),
            "released {kind}s disappeared: {removed:?}. A removal or rename needs a migration \
             and an update to docs/stability.md."
        );
        let added = live.difference(&released).collect::<Vec<_>>();
        if !added.is_empty() {
            eprintln!("{kind}s missing from the stability list: {added:?}");
        }
    }
}
