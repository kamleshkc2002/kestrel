//! Schema versions, migrations, and the current document dispatch.

use kestrel_core::CloseAction;

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
    shelf::parse_shelf,
    shortcuts::parse_shortcuts,
    snippets::parse_snippets,
    speed_test::parse_speed_test,
    updates::parse_updates,
    url_cleanup::parse_url_cleanup,
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
    parse_updates(&mut loaded, document.get("updates"));
    parse_monitoring(&mut loaded, document.get("monitoring"));
    parse_audio(&mut loaded, document.get("audio"));
    parse_speed_test(&mut loaded, document.get("speed_test"));
    parse_capture(&mut loaded, document.get("capture"));
    parse_processes(&mut loaded, document.get("processes"));
    parse_screen_tools(&mut loaded, document.get("screen_tools"));
    parse_shortcuts(&mut loaded, document.get("shortcuts"));
    parse_clipboard(&mut loaded, document.get("clipboard"));
    parse_url_cleanup(&mut loaded, document.get("url_cleanup"));
    parse_snippets(&mut loaded, document.get("snippets"));
    parse_scratchpad(&mut loaded, document.get("scratchpad"));
    parse_recording(&mut loaded, document.get("recording"));
    parse_command_bar(&mut loaded, document.get("command_bar"));
    parse_quick_panel(&mut loaded, document.get("quick_panel"));
    parse_shelf(&mut loaded, document.get("shelf"));
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
    ask_for_legacy_minimize(&mut loaded);
    Ok(loaded)
}

pub(super) fn parse_v3(
    document: toml::Table,
) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = parse_current(document)?;
    ask_for_legacy_minimize(&mut loaded);
    Ok(loaded)
}

/// Older builds wrote the default `minimize` to every file, so it cannot be
/// told apart from a deliberate choice; those files ask once instead.
fn ask_for_legacy_minimize(loaded: &mut LoadedConfiguration) {
    if loaded.configuration.ui.close_action == CloseAction::Minimize {
        loaded.configuration.ui.close_action = CloseAction::Ask;
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::{
        AppearancePreference, AudioDisconnectPolicy, AudioOutputSwitch,
        CURRENT_CONFIGURATION_SCHEMA_VERSION, CaptureAfterScreenshot, ClipboardLayout, CloseAction,
        MonitorReadout, NetworkRateUnit, SnippetExpansionTiming, SnippetProviderPreference,
        UpdateCheck,
    };

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
    fn legacy_minimize_asks_once_and_current_choices_are_kept() {
        for (version, close_action, expected) in [
            (2, "minimize", CloseAction::Ask),
            (3, "minimize", CloseAction::Ask),
            (3, "quit", CloseAction::Quit),
            (4, "minimize", CloseAction::Minimize),
            (4, "quit", CloseAction::Quit),
            (4, "ask", CloseAction::Ask),
        ] {
            let loaded = parse(&format!(
                "schema_version = {version}\n[ui]\nclose_action = \"{close_action}\"\n"
            ))
            .expect("valid configuration");
            assert_eq!(
                loaded.configuration.ui.close_action, expected,
                "version {version} with {close_action}"
            );
            assert_eq!(
                loaded.configuration.schema_version,
                CURRENT_CONFIGURATION_SCHEMA_VERSION
            );
        }
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

    const RELEASED_V1: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stability/config-v1.toml"
    ));
    const RELEASED_V2: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stability/config-v2.toml"
    ));
    const RELEASED_V3: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stability/config-v3.toml"
    ));
    const RELEASED_V4: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stability/config-v4.toml"
    ));

    #[test]
    fn released_configuration_fixtures_keep_their_settings() {
        let load = |contents: &str| {
            let loaded = parse(contents).expect("a released configuration loads");
            assert!(
                loaded.warnings.is_empty(),
                "a released configuration loads without warnings: {:?}",
                loaded.warnings
            );
            assert_eq!(
                loaded.configuration.schema_version, CURRENT_CONFIGURATION_SCHEMA_VERSION,
                "an old file is migrated to the current schema"
            );
            loaded.configuration
        };

        let v1 = load(RELEASED_V1);
        assert!(v1.feature_enabled("audio.mixer"));
        assert!(v1.feature_enabled("system.monitor"));
        assert!(!v1.feature_enabled("clipboard.history"));
        assert!(
            v1.features.contains_key("clipboard.history"),
            "a disabled entry survives as a choice"
        );

        let v2 = load(RELEASED_V2);
        assert!(v2.feature_enabled("audio.mixer"));
        assert!(!v2.feature_enabled("clipboard.history"));
        assert_eq!(v2.ui.appearance, AppearancePreference::Dark);
        assert!(v2.startup.autostart);

        let v3 = load(RELEASED_V3);
        assert!(v3.feature_enabled("audio.mixer"));
        assert!(v3.feature_enabled("system.monitor"));
        assert!(!v3.feature_enabled("clipboard.history"));
        assert_eq!(v3.ui.appearance, AppearancePreference::Dark);
        assert_eq!(
            v3.ui.close_action,
            CloseAction::Ask,
            "a schema 3 minimize asks once, as the migration documents"
        );
        assert!(v3.startup.autostart);
        assert!(!v3.startup.whats_new);
        assert_eq!(v3.monitoring.refresh_interval_millis, 2_000);
        assert_eq!(v3.monitoring.history_samples, 300);
        assert_eq!(
            v3.monitoring.readouts,
            [
                MonitorReadout::Cpu,
                MonitorReadout::Memory,
                MonitorReadout::Network,
                MonitorReadout::Battery,
            ]
        );
        assert_eq!(v3.monitoring.network_rate_unit, NetworkRateUnit::Bits);
        assert!(v3.monitoring.per_core_cpu);
        assert_eq!(
            v3.monitoring.tray_readouts,
            [MonitorReadout::Cpu, MonitorReadout::Battery]
        );
        assert!(!v3.monitoring.alerts.cpu.enabled);
        assert_eq!(v3.monitoring.alerts.cpu.threshold, 75.5);
        assert_eq!(v3.monitoring.alerts.cpu.sustain_samples, 10);
        assert_eq!(v3.monitoring.alerts.cpu.cooldown_seconds, 1_200);
        assert_eq!(v3.audio.boost_percent, 140);
        assert_eq!(v3.audio.output_switch, AudioOutputSwitch::AllStreams);
        assert_eq!(
            v3.audio.disconnect_policy,
            AudioDisconnectPolicy::ResetVolume
        );
        assert_eq!(v3.audio.disconnect_volume_percent, 80);
        assert!(v3.audio.include_inactive_streams);
        assert_eq!(v3.audio.output_priority, ["speakers"]);
        assert!(v3.audio.follow_output_priority);
        assert_eq!(v3.audio.input_priority, ["microphone"]);
        assert!(v3.audio.follow_input_priority);
        assert_eq!(v3.audio.mixer_favorites, ["firefox"]);
        assert_eq!(v3.speed_test.download_megabytes, 50);
        assert_eq!(v3.speed_test.upload_megabytes, 20);
        assert_eq!(v3.speed_test.timeout_seconds, 60);
        assert_eq!(v3.capture.max_entries, 40);
        assert_eq!(v3.capture.max_total_megabytes, 512);
        assert_eq!(v3.capture.max_age_hours, 72);
        assert_eq!(v3.capture.after_screenshot, CaptureAfterScreenshot::Edit);
        assert_eq!(v3.processes.refresh_millis, 3_000);
        assert_eq!(v3.processes.max_rows, 200);
        assert_eq!(v3.screen_tools.ocr_language, "deu");
        assert_eq!(v3.screen_tools.ocr_timeout_seconds, 45);
        assert_eq!(v3.shortcuts.bindings.len(), 2);
        assert_eq!(v3.shortcuts.bindings[0].command, "window.show");
        assert_eq!(v3.shortcuts.bindings[0].trigger, "LOGO+ALT+k");
        assert_eq!(v3.shortcuts.bindings[1].command, "clipboard.quick-paste");
        assert_eq!(v3.clipboard.max_items, 200);
        assert_eq!(v3.clipboard.max_item_bytes, 2_097_152);
        assert_eq!(v3.clipboard.max_image_bytes, 8_388_608);
        assert_eq!(v3.clipboard.max_file_entries, 128);
        assert_eq!(v3.clipboard.max_total_bytes, 33_554_432);
        assert_eq!(v3.clipboard.max_age_hours, 48);
        assert_eq!(v3.clipboard.clear_seconds, 30);
        assert!(v3.clipboard.filter_sensitive);
        assert!(!v3.clipboard.paste_plain_text);
        assert_eq!(v3.clipboard.history_layout, ClipboardLayout::Cards);
        assert_eq!(v3.snippets.max_content_bytes, 131_072);
        assert_eq!(v3.snippets.clipboard_variable_bytes, 8_192);
        assert_eq!(v3.snippets.insert_timeout_millis, 5_000);
        assert_eq!(
            v3.snippets.preferred_provider,
            SnippetProviderPreference::Wtype
        );
        assert_eq!(
            v3.snippets.expansion_timing,
            SnippetExpansionTiming::Delimiter
        );
        assert_eq!(
            v3.quick_panel.favorites,
            ["window.show", "clipboard.quick-paste"]
        );
        assert_eq!(v3.command_bar.max_results, 20);
        assert!(v3.command_bar.enable_applications);
        assert!(v3.command_bar.enable_windows);
        assert!(!v3.command_bar.enable_clipboard);
        assert!(v3.command_bar.enable_files);
        assert!(v3.command_bar.enable_scripts);
        assert!(v3.command_bar.enable_emoji);
        assert_eq!(v3.command_bar.file_roots, ["/home/user/Documents"]);
        assert_eq!(v3.command_bar.scripts.len(), 1);
        assert_eq!(v3.command_bar.scripts[0].name, "Sample");
        assert_eq!(v3.command_bar.scripts[0].executable, "sample");
        assert_eq!(v3.command_bar.scripts[0].args, ["--flag"]);
        assert_eq!(v3.command_bar.scripts[0].timeout_millis, 4_000);
        assert_eq!(v3.command_bar.scripts[0].output_bytes, 8_192);
        assert_eq!(v3.scratchpad.max_notes, 30);
        assert_eq!(v3.scratchpad.max_note_bytes, 524_288);
        assert_eq!(v3.scratchpad.max_age_days, 30);
        assert_eq!(v3.recording.framerate, 60);
        assert!(!v3.recording.cursor);
        assert!(v3.recording.system_audio);
        assert_eq!(v3.recording.system_audio_app.as_deref(), Some("firefox"));
        assert!(v3.recording.microphone);
        assert!(v3.logging.debug);
        assert_eq!(v3.logging.retention_days, 14);
        assert_eq!(v3.logging.max_total_megabytes, 50);
        assert!(!v3.overlay.microphone);
        assert!(v3.overlay.recording);
        assert!(!v3.overlay.alerts);
        assert!(v3.overlay.volume);
        assert!(!v3.overlay.microphone_level);
        assert!(v3.overlay.keyboard_light);
        assert!(!v3.overlay.accessories);
        assert!(v3.overlay.keep_awake);
        assert_eq!(v3.overlay.accessory_warning, 25);
        assert_eq!(v3.overlay.accessory_critical, 10);

        let v4 = load(RELEASED_V4);
        assert!(v4.feature_enabled("audio.mixer"));
        assert!(v4.feature_enabled("files.shelf"));
        assert!(!v4.feature_enabled("clipboard.history"));
        assert_eq!(v4.ui.appearance, AppearancePreference::Light);
        assert_eq!(
            v4.ui.close_action,
            CloseAction::Quit,
            "a schema 4 choice is kept"
        );
        assert!(v4.startup.autostart);
        assert!(!v4.startup.whats_new);
        assert_eq!(v4.updates.check, UpdateCheck::Enabled);
        assert_eq!(v4.monitoring.refresh_interval_millis, 2_000);
        assert_eq!(v4.audio.boost_percent, 140);
        assert_eq!(v4.capture.after_screenshot, CaptureAfterScreenshot::Edit);
        assert_eq!(v4.clipboard.history_layout, ClipboardLayout::Cards);
        assert!(v4.url_cleanup.automatic);
        assert_eq!(v4.url_cleanup.extra_global, ["ref"]);
        assert_eq!(v4.url_cleanup.sites.len(), 1);
        assert_eq!(v4.url_cleanup.sites[0].host, "example.com");
        assert!(!v4.url_cleanup.sites[0].enabled);
        assert_eq!(v4.url_cleanup.sites[0].params, ["utm_source"]);
        assert_eq!(
            v4.quick_panel.favorites,
            ["window.show", "clipboard.quick-paste"]
        );
        assert_eq!(v4.command_bar.scripts[0].args, ["--flag"]);
        assert_eq!(v4.recording.system_audio_app.as_deref(), Some("firefox"));
        assert_eq!(v4.logging.retention_days, 14);
        assert_eq!(v4.overlay.accessory_warning, 25);
        assert!(v4.shelf.keep_across_restarts);
        assert!(v4.shelf.add_screenshots);
        assert_eq!(v4.shelf.max_items, 100);
        assert_eq!(v4.shelf.max_text_megabytes, 2);
        assert_eq!(v4.shelf.max_image_megabytes, 32);
        assert_eq!(v4.shelf.max_age_hours, 48);
    }
}
