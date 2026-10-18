use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, AudioDisconnectPolicy,
    AudioOutputSwitch, CURRENT_CONFIGURATION_SCHEMA_VERSION, ConfigurationError,
    FeatureConfiguration, MAX_ALERT_COOLDOWN_SECONDS, MAX_ALERT_SUSTAIN_SAMPLES,
    MAX_ALERT_THRESHOLD_PERCENT, MAX_AUDIO_BOOST_PERCENT, MAX_CAPTURE_MAX_AGE_HOURS,
    MAX_CAPTURE_MAX_ENTRIES, MAX_CAPTURE_MAX_TOTAL_MEGABYTES, MAX_CLIPBOARD_AGE_HOURS,
    MAX_CLIPBOARD_CLEAR_SECONDS, MAX_CLIPBOARD_FILE_ENTRIES, MAX_CLIPBOARD_IMAGE_BYTES,
    MAX_CLIPBOARD_MAX_ITEMS, MAX_CLIPBOARD_TOTAL_BYTES, MAX_COMMAND_FILE_ROOTS,
    MAX_COMMAND_RESULTS, MAX_MONITOR_HISTORY_SAMPLES, MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
    MAX_QUICK_PANEL_FAVORITES, MAX_SNIPPET_CLIPBOARD_BYTES, MAX_SNIPPET_CONTENT_BYTES,
    MAX_SNIPPET_INSERT_TIMEOUT_MILLIS, MAX_SPEED_TEST_DOWNLOAD_MEGABYTES,
    MAX_SPEED_TEST_TIMEOUT_SECONDS, MAX_SPEED_TEST_UPLOAD_MEGABYTES,
    MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS, MIN_ALERT_SUSTAIN_SAMPLES, MIN_CLIPBOARD_ITEM_BYTES,
    MIN_COMMAND_RESULTS, MIN_MONITOR_REFRESH_INTERVAL_MILLIS, MIN_SNIPPET_CLIPBOARD_BYTES,
    MIN_SNIPPET_CONTENT_BYTES, MIN_SNIPPET_INSERT_TIMEOUT_MILLIS,
    MIN_SPEED_TEST_DOWNLOAD_MEGABYTES, MIN_SPEED_TEST_TIMEOUT_SECONDS, MonitorReadout,
    SnippetExpansionTiming, SnippetProviderPreference, StartupConfiguration,
    UNAMPLIFIED_AUDIO_VOLUME_PERCENT, validate_feature_id,
};
use serde::Deserialize;

/// Feature ID of the retired Battery alerts quick toggle.
const RETIRED_BATTERY_ALERTS_ID: &str = "power.battery-alerts";

/// Loaded configuration and isolated diagnostics.
#[derive(Debug, Default)]
pub struct LoadedConfiguration {
    pub configuration: ApplicationConfiguration,
    pub warnings: Vec<ConfigurationWarning>,
}

/// A malformed setting disabling only its preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationWarning {
    /// Dotted portable-document location.
    pub feature_id: String,
    pub reason: String,
}

/// Unrecoverable configuration I/O or document error.
#[derive(Debug)]
pub enum ConfigurationLoadError {
    Io(std::io::Error),
    InvalidToml(toml::de::Error),
    InvalidSchemaVersion,
    UnsupportedSchemaVersion(u32),
    InvalidConfiguration(ConfigurationError),
}

impl std::fmt::Display for ConfigurationLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "configuration I/O failed: {error}"),
            Self::InvalidToml(error) => {
                write!(formatter, "configuration is not valid TOML: {error}")
            }
            Self::InvalidSchemaVersion => {
                write!(formatter, "schema_version must be a non-negative integer")
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "configuration schema version {version} is unsupported"
                )
            }
            Self::InvalidConfiguration(error) => {
                write!(formatter, "configuration validation failed: {error:?}")
            }
        }
    }
}

impl std::error::Error for ConfigurationLoadError {}

/// XDG path for non-sensitive configuration.
pub fn configuration_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(
        config_home
            .join(kestrel_core::DATA_DIRECTORY_NAME)
            .join("config.toml"),
    )
}

/// `$XDG_VIDEOS_DIR/Kestrel` from `user-dirs.dirs`, else `~/Videos/Kestrel`.
pub fn recordings_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let videos = fs::read_to_string(config_home.join("user-dirs.dirs"))
        .ok()
        .and_then(|contents| videos_directory(&contents, &home))
        .unwrap_or_else(|| home.join("Videos"));
    Some(videos.join(if kestrel_core::DEVELOPMENT_BUILD {
        "Kestrel (Development)"
    } else {
        "Kestrel"
    }))
}

/// `XDG_VIDEOS_DIR` from `user-dirs.dirs` contents; only `$HOME/…` and
/// absolute values are valid there.
fn videos_directory(contents: &str, home: &Path) -> Option<PathBuf> {
    let value = contents
        .lines()
        .filter_map(|line| line.trim().strip_prefix("XDG_VIDEOS_DIR="))
        .next_back()?
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?;
    if let Some(relative) = value.strip_prefix("$HOME") {
        let relative = relative.trim_start_matches('/');
        // `$HOME` alone means the directory is disabled.
        return (!relative.is_empty()).then(|| home.join(relative));
    }
    value.starts_with('/').then(|| PathBuf::from(value))
}

/// Loads and migrates configuration, retaining valid settings.
pub fn load(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    match fs::read_to_string(path) {
        Ok(contents) => import_string(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedConfiguration::default())
        }
        Err(error) => Err(ConfigurationLoadError::Io(error)),
    }
}

/// Saves typed, versioned non-sensitive settings.
pub fn save(path: &Path, configuration: &ApplicationConfiguration) -> Result<(), std::io::Error> {
    let content = export_string(configuration).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{error:?}"))
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

/// Imports portable configuration; isolated errors become warnings.
pub fn import_string(contents: &str) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    parse(contents)
}

/// Exports typed portable configuration as deterministic TOML.
pub fn export_string(
    configuration: &ApplicationConfiguration,
) -> Result<String, ConfigurationError> {
    configuration.validate()?;
    Ok(toml::to_string_pretty(configuration).expect("portable configuration is serializable"))
}

/// Imports a selected configuration file; missing files are errors.
pub fn import_file(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let contents = fs::read_to_string(path).map_err(ConfigurationLoadError::Io)?;
    import_string(&contents)
}

pub fn export_file(
    path: &Path,
    configuration: &ApplicationConfiguration,
) -> Result<(), ConfigurationLoadError> {
    let content =
        export_string(configuration).map_err(ConfigurationLoadError::InvalidConfiguration)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigurationLoadError::Io)?;
    }
    fs::write(path, content).map_err(ConfigurationLoadError::Io)
}

fn parse(contents: &str) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let document: toml::Table = contents
        .parse()
        .map_err(ConfigurationLoadError::InvalidToml)?;
    let schema_version = match document.get("schema_version") {
        None => 0,
        Some(value) => value
            .as_integer()
            .ok_or(ConfigurationLoadError::InvalidSchemaVersion)?
            .try_into()
            .map_err(|_| ConfigurationLoadError::InvalidSchemaVersion)?,
    };

    match schema_version {
        0 => migrate_v0(document),
        1 | 2 => parse_v2(document),
        CURRENT_CONFIGURATION_SCHEMA_VERSION => parse_current(document),
        version => Err(ConfigurationLoadError::UnsupportedSchemaVersion(version)),
    }
}

fn parse_current(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
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

fn migrate_v0(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
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
            loaded
                .warnings
                .push(feature_warning(&format!("features.{feature_id}"), error));
        }
    }
    Ok(loaded)
}

fn parse_v2(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
    retire_battery_alerts_toggle(&mut loaded);
    Ok(loaded)
}

fn parse_features(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(features) = value else { return };
    let Some(features) = features.as_table() else {
        loaded.warnings.push(warning(
            "features",
            "The features value must be a TOML table.",
        ));
        return;
    };
    for (feature_id, value) in features {
        let location = format!("features.{feature_id}");
        if let Err(error) = validate_feature_id(feature_id) {
            loaded.warnings.push(feature_warning(&location, error));
            continue;
        }
        match FeatureConfiguration::deserialize(value.clone()) {
            Ok(feature) => {
                loaded
                    .configuration
                    .features
                    .insert(feature_id.clone(), feature);
            }
            Err(error) => loaded.warnings.push(warning(
                &location,
                format!("Feature settings are invalid and were ignored: {error}"),
            )),
        }
    }
}

fn parse_ui(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(ui) = value.as_table() else {
        loaded
            .warnings
            .push(warning("ui", "The ui value must be a TOML table."));
        return;
    };

    if let Some(appearance) = ui.get("appearance") {
        match AppearancePreference::deserialize(appearance.clone()) {
            Ok(appearance) => loaded.configuration.ui.appearance = appearance,
            Err(error) => loaded.warnings.push(warning(
                "ui.appearance",
                format!("Appearance preference is invalid and defaults were retained: {error}"),
            )),
        }
    }

    if let Some(close_action) = ui.get("close_action") {
        match kestrel_core::CloseAction::deserialize(close_action.clone()) {
            Ok(close_action) => loaded.configuration.ui.close_action = close_action,
            Err(_) => loaded.warnings.push(warning(
                "ui.close_action",
                "Must be \"minimize\" or \"quit\"; the default was retained.",
            )),
        }
    }
}
fn parse_startup(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(startup) = value.as_table() else {
        loaded.warnings.push(warning(
            "startup",
            "The startup value must be a TOML table.",
        ));
        return;
    };
    if let Some(autostart) = startup.get("autostart") {
        match bool::deserialize(autostart.clone()) {
            Ok(autostart) => loaded.configuration.startup = StartupConfiguration { autostart },
            Err(error) => loaded.warnings.push(warning(
                "startup.autostart",
                format!("Autostart preference is invalid and defaults were retained: {error}"),
            )),
        }
    }
}

fn parse_monitoring(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(monitoring) = value.as_table() else {
        loaded.warnings.push(warning(
            "monitoring",
            "The monitoring value must be a TOML table.",
        ));
        return;
    };

    if let Some(refresh) = monitoring.get("refresh_interval_millis") {
        match u64::deserialize(refresh.clone()) {
            Ok(millis)
                if (MIN_MONITOR_REFRESH_INTERVAL_MILLIS..=MAX_MONITOR_REFRESH_INTERVAL_MILLIS)
                    .contains(&millis) =>
            {
                loaded.configuration.monitoring.refresh_interval_millis = millis;
            }
            _ => loaded.warnings.push(warning(
                "monitoring.refresh_interval_millis",
                format!(
                    "Refresh interval must be between {MIN_MONITOR_REFRESH_INTERVAL_MILLIS} and \
                     {MAX_MONITOR_REFRESH_INTERVAL_MILLIS} milliseconds; the default was retained."
                ),
            )),
        }
    }

    if let Some(history) = monitoring.get("history_samples") {
        match u32::deserialize(history.clone()) {
            Ok(samples) if samples > 0 && samples <= MAX_MONITOR_HISTORY_SAMPLES => {
                loaded.configuration.monitoring.history_samples = samples;
            }
            _ => loaded.warnings.push(warning(
                "monitoring.history_samples",
                format!(
                    "History samples must be between 1 and {MAX_MONITOR_HISTORY_SAMPLES}; \
                     the default was retained."
                ),
            )),
        }
    }

    if let Some(unit) = monitoring.get("network_rate_unit") {
        match kestrel_core::NetworkRateUnit::deserialize(unit.clone()) {
            Ok(unit) => loaded.configuration.monitoring.network_rate_unit = unit,
            Err(_) => loaded.warnings.push(warning(
                "monitoring.network_rate_unit",
                "Network rate unit must be \"bytes\" or \"bits\"; the default was retained.",
            )),
        }
    }

    if let Some(per_core) = monitoring.get("per_core_cpu") {
        match per_core.as_bool() {
            Some(per_core) => loaded.configuration.monitoring.per_core_cpu = per_core,
            None => loaded.warnings.push(warning(
                "monitoring.per_core_cpu",
                "Must be true or false; the default was retained.",
            )),
        }
    }

    if let Some(tray) = monitoring.get("tray_readouts") {
        match Vec::<MonitorReadout>::deserialize(tray.clone()) {
            Ok(readouts)
                if readouts.len() <= kestrel_core::MAX_TRAY_READOUTS
                    && readouts
                        .iter()
                        .enumerate()
                        .all(|(index, readout)| !readouts[..index].contains(readout)) =>
            {
                loaded.configuration.monitoring.tray_readouts = readouts;
            }
            _ => loaded.warnings.push(warning(
                "monitoring.tray_readouts",
                format!(
                    "Tray readouts must be at most {} different readouts; none are shown.",
                    kestrel_core::MAX_TRAY_READOUTS
                ),
            )),
        }
    }

    if let Some(readouts) = monitoring.get("readouts") {
        let Some(entries) = readouts.as_array() else {
            loaded.warnings.push(warning(
                "monitoring.readouts",
                "Monitor readouts must be an array; the default list was retained.",
            ));
            parse_monitoring_alerts(loaded, monitoring.get("alerts"));
            return;
        };
        let mut parsed = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let location = format!("monitoring.readouts[{index}]");
            let readout = match MonitorReadout::deserialize(entry.clone()) {
                Ok(readout) => readout,
                Err(error) => {
                    loaded.warnings.push(warning(
                        &location,
                        format!("Monitor readout is unknown or invalid and was ignored: {error}"),
                    ));
                    continue;
                }
            };
            if parsed.contains(&readout) {
                loaded.warnings.push(warning(
                    &location,
                    "Duplicate monitor readout was ignored; the first entry was retained.",
                ));
                continue;
            }
            parsed.push(readout);
        }
        if parsed.is_empty() {
            loaded.warnings.push(warning(
                "monitoring.readouts",
                "Monitor readouts were empty or invalid; the default list was retained.",
            ));
        } else {
            loaded.configuration.monitoring.readouts = parsed;
        }
    }

    parse_monitoring_alerts(loaded, monitoring.get("alerts"));
}

fn parse_audio(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(audio) = value.as_table() else {
        loaded
            .warnings
            .push(warning("audio", "The audio value must be a TOML table."));
        return;
    };

    if let Some(boost) = audio.get("boost_percent") {
        match u8::deserialize(boost.clone()) {
            Ok(percent)
                if (UNAMPLIFIED_AUDIO_VOLUME_PERCENT..=MAX_AUDIO_BOOST_PERCENT)
                    .contains(&percent) =>
            {
                loaded.configuration.audio.boost_percent = percent;
            }
            _ => loaded.warnings.push(warning(
                "audio.boost_percent",
                format!(
                    "Boost ceiling must be between {UNAMPLIFIED_AUDIO_VOLUME_PERCENT} and \
                     {MAX_AUDIO_BOOST_PERCENT} percent; the default was retained."
                ),
            )),
        }
    }

    if let Some(switch) = audio.get("output_switch") {
        match AudioOutputSwitch::deserialize(switch.clone()) {
            Ok(parsed) => loaded.configuration.audio.output_switch = parsed,
            Err(error) => loaded.warnings.push(warning(
                "audio.output_switch",
                format!("Output switch mode is invalid and was ignored: {error}"),
            )),
        }
    }

    if let Some(policy) = audio.get("disconnect_policy") {
        match AudioDisconnectPolicy::deserialize(policy.clone()) {
            Ok(parsed) => loaded.configuration.audio.disconnect_policy = parsed,
            Err(error) => loaded.warnings.push(warning(
                "audio.disconnect_policy",
                format!("Disconnect policy is invalid and was ignored: {error}"),
            )),
        }
    }

    if let Some(volume) = audio.get("disconnect_volume_percent") {
        match u8::deserialize(volume.clone()) {
            Ok(percent) if percent <= UNAMPLIFIED_AUDIO_VOLUME_PERCENT => {
                loaded.configuration.audio.disconnect_volume_percent = percent;
            }
            _ => loaded.warnings.push(warning(
                "audio.disconnect_volume_percent",
                format!(
                    "Disconnect volume must be between 0 and {UNAMPLIFIED_AUDIO_VOLUME_PERCENT} \
                     percent; the default was retained."
                ),
            )),
        }
    }

    if let Some(include) = audio.get("include_inactive_streams") {
        match bool::deserialize(include.clone()) {
            Ok(include) => loaded.configuration.audio.include_inactive_streams = include,
            Err(error) => loaded.warnings.push(warning(
                "audio.include_inactive_streams",
                format!("The inactive-stream preference is invalid and was ignored: {error}"),
            )),
        }
    }

    for (key, limit) in [
        ("output_priority", kestrel_core::MAX_AUDIO_PRIORITY_DEVICES),
        ("input_priority", kestrel_core::MAX_AUDIO_PRIORITY_DEVICES),
        ("mixer_favorites", kestrel_core::MAX_MIXER_FAVORITES),
    ] {
        let Some(value) = audio.get(key) else {
            continue;
        };
        match Vec::<String>::deserialize(value.clone()) {
            Ok(list) if kestrel_core::valid_audio_identity_list(&list, limit) => {
                let audio = &mut loaded.configuration.audio;
                match key {
                    "output_priority" => audio.output_priority = list,
                    "input_priority" => audio.input_priority = list,
                    _ => audio.mixer_favorites = list,
                }
            }
            _ => loaded.warnings.push(warning(
                &format!("audio.{key}"),
                format!(
                    "Must be a list of at most {limit} different names; the saved list was ignored."
                ),
            )),
        }
    }

    for key in ["follow_output_priority", "follow_input_priority"] {
        let Some(value) = audio.get(key) else {
            continue;
        };
        match value.as_bool() {
            Some(follow) if key == "follow_output_priority" => {
                loaded.configuration.audio.follow_output_priority = follow;
            }
            Some(follow) => loaded.configuration.audio.follow_input_priority = follow,
            None => loaded.warnings.push(warning(
                &format!("audio.{key}"),
                "Must be true or false; the default was retained.",
            )),
        }
    }
}

fn parse_shortcuts(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(section) = value.as_table() else {
        loaded.warnings.push(warning(
            "shortcuts",
            "The shortcuts value must be a TOML table.",
        ));
        return;
    };
    let Some(bindings) = section.get("bindings") else {
        return;
    };
    let Some(entries) = bindings.as_array() else {
        loaded.warnings.push(warning(
            "shortcuts.bindings",
            "Shortcut bindings must be an array of tables; the defaults were retained.",
        ));
        return;
    };
    // Each binding is checked on its own, so one bad entry drops only itself.
    let mut accepted: Vec<kestrel_core::ShortcutBinding> = Vec::new();
    let mut triggers: Vec<String> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let location = format!("shortcuts.bindings[{index}]");
        let binding = match kestrel_core::ShortcutBinding::deserialize(entry.clone()) {
            Ok(binding) => binding,
            Err(error) => {
                loaded.warnings.push(warning(
                    &location,
                    format!(
                        "Binding needs `command` and `trigger` strings and was ignored: {error}"
                    ),
                ));
                continue;
            }
        };
        let trigger = match binding.validate() {
            Ok(trigger) => trigger.to_string().to_ascii_lowercase(),
            Err(error) => {
                loaded.warnings.push(warning(
                    &location,
                    format!("Binding was ignored: {error:?}"),
                ));
                continue;
            }
        };
        if accepted
            .iter()
            .any(|existing| existing.command == binding.command)
            || triggers.contains(&trigger)
        {
            loaded.warnings.push(warning(
                &location,
                "Binding repeats an earlier command or trigger and was ignored.",
            ));
            continue;
        }
        if accepted.len() == kestrel_core::MAX_SHORTCUT_BINDINGS {
            loaded.warnings.push(warning(
                &location,
                format!(
                    "At most {} bindings are kept; the rest were ignored.",
                    kestrel_core::MAX_SHORTCUT_BINDINGS
                ),
            ));
            break;
        }
        triggers.push(trigger);
        accepted.push(binding);
    }
    loaded.configuration.shortcuts.bindings = accepted;
}

fn parse_speed_test(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(speed_test) = value.as_table() else {
        loaded.warnings.push(warning(
            "speed_test",
            "The speed_test value must be a TOML table.",
        ));
        return;
    };

    if let Some(download) = speed_test.get("download_megabytes") {
        match u32::deserialize(download.clone()) {
            Ok(megabytes)
                if (MIN_SPEED_TEST_DOWNLOAD_MEGABYTES..=MAX_SPEED_TEST_DOWNLOAD_MEGABYTES)
                    .contains(&megabytes) =>
            {
                loaded.configuration.speed_test.download_megabytes = megabytes;
            }
            _ => loaded.warnings.push(warning(
                "speed_test.download_megabytes",
                format!(
                    "Download size must be between {MIN_SPEED_TEST_DOWNLOAD_MEGABYTES} and \
                     {MAX_SPEED_TEST_DOWNLOAD_MEGABYTES} megabytes; the default was retained."
                ),
            )),
        }
    }

    if let Some(upload) = speed_test.get("upload_megabytes") {
        match u32::deserialize(upload.clone()) {
            Ok(megabytes) if megabytes <= MAX_SPEED_TEST_UPLOAD_MEGABYTES => {
                loaded.configuration.speed_test.upload_megabytes = megabytes;
            }
            _ => loaded.warnings.push(warning(
                "speed_test.upload_megabytes",
                format!(
                    "Upload size must be between 0 and {MAX_SPEED_TEST_UPLOAD_MEGABYTES} \
                     megabytes; the default was retained."
                ),
            )),
        }
    }

    if let Some(timeout) = speed_test.get("timeout_seconds") {
        match u32::deserialize(timeout.clone()) {
            Ok(seconds)
                if (MIN_SPEED_TEST_TIMEOUT_SECONDS..=MAX_SPEED_TEST_TIMEOUT_SECONDS)
                    .contains(&seconds) =>
            {
                loaded.configuration.speed_test.timeout_seconds = seconds;
            }
            _ => loaded.warnings.push(warning(
                "speed_test.timeout_seconds",
                format!(
                    "Timeout must be between {MIN_SPEED_TEST_TIMEOUT_SECONDS} and \
                     {MAX_SPEED_TEST_TIMEOUT_SECONDS} seconds; the default was retained."
                ),
            )),
        }
    }
}

fn parse_capture(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(capture) = value.as_table() else {
        loaded.warnings.push(warning(
            "capture",
            "The capture value must be a TOML table.",
        ));
        return;
    };
    // Each limit is validated alone; a bad one keeps its default.
    let mut bounded = |key: &str, max: u32, unit: &str, slot: &mut u32| {
        let Some(value) = capture.get(key) else {
            return;
        };
        match u32::deserialize(value.clone()) {
            Ok(number) if (1..=max).contains(&number) => *slot = number,
            _ => loaded.warnings.push(warning(
                &format!("capture.{key}"),
                format!("Must be between 1 and {max} {unit}; the default was retained."),
            )),
        }
    };
    let mut limits = loaded.configuration.capture;
    bounded(
        "max_entries",
        MAX_CAPTURE_MAX_ENTRIES,
        "captures",
        &mut limits.max_entries,
    );
    bounded(
        "max_total_megabytes",
        MAX_CAPTURE_MAX_TOTAL_MEGABYTES,
        "MiB",
        &mut limits.max_total_megabytes,
    );
    bounded(
        "max_age_hours",
        MAX_CAPTURE_MAX_AGE_HOURS,
        "hours",
        &mut limits.max_age_hours,
    );
    loaded.configuration.capture = limits;
}

fn parse_scratchpad(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(table) = value.as_table() else {
        loaded.warnings.push(warning(
            "scratchpad",
            "The scratchpad value must be a TOML table.",
        ));
        return;
    };
    let mut configuration = loaded.configuration.scratchpad;
    if let Some(value) = table.get("max_notes") {
        match u32::deserialize(value.clone()) {
            Ok(value)
                if (kestrel_core::MIN_SCRATCHPAD_MAX_NOTES
                    ..=kestrel_core::MAX_SCRATCHPAD_MAX_NOTES)
                    .contains(&value) =>
            {
                configuration.max_notes = value
            }
            _ => loaded.warnings.push(warning(
                "scratchpad.max_notes",
                "Must be between 1 and 50 notes; the default was retained.",
            )),
        }
    }
    if let Some(value) = table.get("max_note_bytes") {
        match u32::deserialize(value.clone()) {
            Ok(value)
                if (kestrel_core::MIN_SCRATCHPAD_NOTE_BYTES
                    ..=kestrel_core::MAX_SCRATCHPAD_NOTE_BYTES)
                    .contains(&value) =>
            {
                configuration.max_note_bytes = value
            }
            _ => loaded.warnings.push(warning(
                "scratchpad.max_note_bytes",
                "Must be between 1024 and 1048576 bytes; the default was retained.",
            )),
        }
    }
    if let Some(value) = table.get("max_age_days") {
        match u32::deserialize(value.clone()) {
            Ok(value) if value <= kestrel_core::MAX_SCRATCHPAD_AGE_DAYS => {
                configuration.max_age_days = value
            }
            _ => loaded.warnings.push(warning(
                "scratchpad.max_age_days",
                "Must be between 0 and 365 days; the default was retained.",
            )),
        }
    }
    loaded.configuration.scratchpad = configuration;
}
fn parse_processes(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(processes) = value.as_table() else {
        loaded.warnings.push(warning(
            "processes",
            "The processes value must be a TOML table.",
        ));
        return;
    };
    let defaults = loaded.configuration.processes;
    let mut configuration = defaults;
    if let Some(value) = processes.get("refresh_millis") {
        match u32::deserialize(value.clone()) {
            Ok(number)
                if (kestrel_core::MIN_PROCESS_REFRESH_MILLIS
                    ..=kestrel_core::MAX_PROCESS_REFRESH_MILLIS)
                    .contains(&number) =>
            {
                configuration.refresh_millis = number;
            }
            _ => loaded.warnings.push(warning(
                "processes.refresh_millis",
                "Must be between 1000 and 10000 milliseconds; the default was retained.",
            )),
        }
    }
    if let Some(value) = processes.get("max_rows") {
        match u32::deserialize(value.clone()) {
            Ok(number)
                if (kestrel_core::MIN_PROCESS_ROWS..=kestrel_core::MAX_PROCESS_ROWS)
                    .contains(&number) =>
            {
                configuration.max_rows = number;
            }
            _ => loaded.warnings.push(warning(
                "processes.max_rows",
                "Must be between 10 and 500 rows; the default was retained.",
            )),
        }
    }
    loaded.configuration.processes = configuration;
}

fn parse_screen_tools(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(table) = value.as_table() else {
        loaded.warnings.push(warning(
            "screen_tools",
            "The screen_tools value must be a TOML table.",
        ));
        return;
    };
    let configuration = &mut loaded.configuration.screen_tools;
    if let Some(value) = table.get("ocr_language") {
        match value.as_str() {
            Some(language) if kestrel_core::valid_ocr_language(language) => {
                configuration.ocr_language = language.to_owned();
            }
            _ => loaded.warnings.push(warning(
                "screen_tools.ocr_language",
                "Must be Tesseract language codes such as \"eng\" or \"eng+deu\"; the default \
                 was retained.",
            )),
        }
    }
    let configuration = &mut loaded.configuration.screen_tools;
    if let Some(value) = table.get("ocr_timeout_seconds") {
        match u32::deserialize(value.clone()) {
            Ok(seconds)
                if (kestrel_core::MIN_OCR_TIMEOUT_SECONDS
                    ..=kestrel_core::MAX_OCR_TIMEOUT_SECONDS)
                    .contains(&seconds) =>
            {
                configuration.ocr_timeout_seconds = seconds;
            }
            _ => loaded.warnings.push(warning(
                "screen_tools.ocr_timeout_seconds",
                "Must be between 5 and 120 seconds; the default was retained.",
            )),
        }
    }
}

fn parse_recording(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(table) = value.as_table() else {
        loaded.warnings.push(warning(
            "recording",
            "The recording value must be a TOML table.",
        ));
        return;
    };
    if let Some(value) = table.get("framerate") {
        match u32::deserialize(value.clone()) {
            Ok(framerate)
                if (kestrel_core::MIN_RECORDING_FRAMERATE
                    ..=kestrel_core::MAX_RECORDING_FRAMERATE)
                    .contains(&framerate) =>
            {
                loaded.configuration.recording.framerate = framerate;
            }
            _ => loaded.warnings.push(warning(
                "recording.framerate",
                "Must be between 10 and 60 frames per second; the default was retained.",
            )),
        }
    }
    for (key, field) in [
        ("cursor", &mut loaded.configuration.recording.cursor),
        (
            "system_audio",
            &mut loaded.configuration.recording.system_audio,
        ),
        ("microphone", &mut loaded.configuration.recording.microphone),
    ] {
        if let Some(value) = table.get(key) {
            match value.as_bool() {
                Some(enabled) => *field = enabled,
                None => loaded.warnings.push(warning(
                    &format!("recording.{key}"),
                    "Must be true or false; the default was retained.",
                )),
            }
        }
    }
}

fn parse_clipboard(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(clipboard) = value.as_table() else {
        loaded.warnings.push(warning(
            "clipboard",
            "The clipboard value must be a TOML table.",
        ));
        return;
    };

    let bounded_u32 = |loaded: &mut LoadedConfiguration, key: &str, minimum: u32, maximum: u32| {
        if let Some(value) = clipboard.get(key) {
            match u32::deserialize(value.clone()) {
                Ok(parsed) if (minimum..=maximum).contains(&parsed) => Some(parsed),
                _ => {
                    loaded.warnings.push(warning(
                        &format!("clipboard.{key}"),
                        format!(
                            "The value must be between {minimum} and {maximum}; the default was \
                             retained."
                        ),
                    ));
                    None
                }
            }
        } else {
            None
        }
    };

    if let Some(items) = bounded_u32(loaded, "max_items", 1, MAX_CLIPBOARD_MAX_ITEMS) {
        loaded.configuration.clipboard.max_items = items;
    }
    if let Some(bytes) = bounded_u32(
        loaded,
        "max_item_bytes",
        MIN_CLIPBOARD_ITEM_BYTES,
        MAX_CLIPBOARD_TOTAL_BYTES,
    ) {
        loaded.configuration.clipboard.max_item_bytes = bytes;
    }
    if let Some(bytes) = bounded_u32(
        loaded,
        "max_image_bytes",
        MIN_CLIPBOARD_ITEM_BYTES,
        MAX_CLIPBOARD_IMAGE_BYTES,
    ) {
        loaded.configuration.clipboard.max_image_bytes = bytes;
    }
    if let Some(entries) = bounded_u32(loaded, "max_file_entries", 1, MAX_CLIPBOARD_FILE_ENTRIES) {
        loaded.configuration.clipboard.max_file_entries = entries;
    }
    if let Some(bytes) = bounded_u32(
        loaded,
        "max_total_bytes",
        MIN_CLIPBOARD_ITEM_BYTES,
        MAX_CLIPBOARD_TOTAL_BYTES,
    ) {
        loaded.configuration.clipboard.max_total_bytes = bytes;
    }
    if let Some(hours) = bounded_u32(loaded, "max_age_hours", 1, MAX_CLIPBOARD_AGE_HOURS) {
        loaded.configuration.clipboard.max_age_hours = hours;
    }
    if let Some(value) = clipboard.get("clear_seconds") {
        match u64::deserialize(value.clone()) {
            Ok(seconds) if seconds <= MAX_CLIPBOARD_CLEAR_SECONDS => {
                loaded.configuration.clipboard.clear_seconds = seconds;
            }
            _ => loaded.warnings.push(warning(
                "clipboard.clear_seconds",
                format!(
                    "The automatic clear interval must be between 0 and \
                     {MAX_CLIPBOARD_CLEAR_SECONDS} seconds; the default was retained."
                ),
            )),
        }
    }
    for (key, target) in [
        (
            "filter_sensitive",
            &mut loaded.configuration.clipboard.filter_sensitive,
        ),
        (
            "paste_plain_text",
            &mut loaded.configuration.clipboard.paste_plain_text,
        ),
    ] {
        if let Some(value) = clipboard.get(key) {
            match bool::deserialize(value.clone()) {
                Ok(parsed) => *target = parsed,
                Err(error) => loaded.warnings.push(warning(
                    &format!("clipboard.{key}"),
                    format!("The value is invalid and was ignored: {error}"),
                )),
            }
        }
    }
    if let Some(value) = clipboard.get("history_layout") {
        match kestrel_core::ClipboardLayout::deserialize(value.clone()) {
            Ok(layout) => loaded.configuration.clipboard.history_layout = layout,
            Err(_) => loaded.warnings.push(warning(
                "clipboard.history_layout",
                "History layout must be \"list\" or \"cards\"; the list was retained.",
            )),
        }
    }

    // Total-byte bounds cap per-item limits when inconsistent.
    let total = loaded.configuration.clipboard.max_total_bytes;
    let mut repaired = false;
    if loaded.configuration.clipboard.max_item_bytes > total {
        loaded.configuration.clipboard.max_item_bytes = total;
        repaired = true;
    }
    if loaded.configuration.clipboard.max_image_bytes > total {
        loaded.configuration.clipboard.max_image_bytes = total;
        repaired = true;
    }
    if repaired {
        loaded.warnings.push(warning(
            "clipboard.max_total_bytes",
            format!(
                "The total byte bound ({total} bytes) is smaller than a single entry bound; the \
                 per-entry bounds were lowered to it."
            ),
        ));
    }
}

fn parse_snippets(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(snippets) = value.as_table() else {
        loaded.warnings.push(warning(
            "snippets",
            "The snippets value must be a TOML table.",
        ));
        return;
    };

    let bounded = |loaded: &mut LoadedConfiguration, key: &str, minimum: u64, maximum: u64| {
        if let Some(value) = snippets.get(key) {
            match u64::deserialize(value.clone()) {
                Ok(parsed) if (minimum..=maximum).contains(&parsed) => Some(parsed),
                _ => {
                    loaded.warnings.push(warning(
                        &format!("snippets.{key}"),
                        format!(
                            "The value must be between {minimum} and {maximum}; the default was \
                             retained."
                        ),
                    ));
                    None
                }
            }
        } else {
            None
        }
    };

    if let Some(bytes) = bounded(
        loaded,
        "max_content_bytes",
        u64::from(MIN_SNIPPET_CONTENT_BYTES),
        u64::from(MAX_SNIPPET_CONTENT_BYTES),
    ) {
        loaded.configuration.snippets.max_content_bytes = bytes as u32;
    }
    if let Some(bytes) = bounded(
        loaded,
        "clipboard_variable_bytes",
        u64::from(MIN_SNIPPET_CLIPBOARD_BYTES),
        u64::from(MAX_SNIPPET_CLIPBOARD_BYTES),
    ) {
        loaded.configuration.snippets.clipboard_variable_bytes = bytes as u32;
    }
    if let Some(millis) = bounded(
        loaded,
        "insert_timeout_millis",
        MIN_SNIPPET_INSERT_TIMEOUT_MILLIS,
        MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
    ) {
        loaded.configuration.snippets.insert_timeout_millis = millis;
    }
    if let Some(value) = snippets.get("preferred_provider") {
        match SnippetProviderPreference::deserialize(value.clone()) {
            Ok(parsed) => loaded.configuration.snippets.preferred_provider = parsed,
            Err(error) => loaded.warnings.push(warning(
                "snippets.preferred_provider",
                format!("The insertion provider preference is invalid and was ignored: {error}"),
            )),
        }
    }
    if let Some(value) = snippets.get("expansion_timing") {
        match SnippetExpansionTiming::deserialize(value.clone()) {
            Ok(parsed) => loaded.configuration.snippets.expansion_timing = parsed,
            Err(error) => loaded.warnings.push(warning(
                "snippets.expansion_timing",
                format!("The expansion timing value is invalid and was ignored: {error}"),
            )),
        }
    }
}

fn parse_quick_panel(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(section) = value.as_table() else {
        loaded.warnings.push(warning(
            "quick_panel",
            "The quick_panel value must be a TOML table.",
        ));
        return;
    };
    let Some(favorites) = section.get("favorites") else {
        return;
    };
    let Ok(parsed) = Vec::<String>::deserialize(favorites.clone()) else {
        loaded.warnings.push(warning(
            "quick_panel.favorites",
            "Favorites must be an array of command IDs; the default was retained.",
        ));
        return;
    };
    let mut accepted = Vec::new();
    for (index, id) in parsed.into_iter().enumerate() {
        let location = format!("quick_panel.favorites[{index}]");
        if accepted.len() >= MAX_QUICK_PANEL_FAVORITES {
            loaded.warnings.push(warning(
                &location,
                format!("The favorite limit is {MAX_QUICK_PANEL_FAVORITES}; entry ignored."),
            ));
            continue;
        }
        if crate::command_line::CommandLineAction::parse(&id).is_err() {
            loaded.warnings.push(warning(
                &location,
                format!("Unknown command ID \"{id}\"; entry ignored."),
            ));
            continue;
        }
        if accepted.contains(&id) {
            loaded.warnings.push(warning(
                &location,
                format!("Duplicate command ID \"{id}\"; entry ignored."),
            ));
            continue;
        }
        accepted.push(id);
    }
    loaded.configuration.quick_panel.favorites = accepted;
}

fn parse_command_bar(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(section) = value.as_table() else {
        loaded.warnings.push(warning(
            "command_bar",
            "The command_bar value must be a TOML table.",
        ));
        return;
    };

    if let Some(value) = section.get("max_results") {
        match u32::deserialize(value.clone()) {
            Ok(results) if (MIN_COMMAND_RESULTS..=MAX_COMMAND_RESULTS).contains(&results) => {
                loaded.configuration.command_bar.max_results = results;
            }
            _ => loaded.warnings.push(warning(
                "command_bar.max_results",
                format!(
                    "The result count must be between {MIN_COMMAND_RESULTS} and \
                     {MAX_COMMAND_RESULTS}; the default was retained."
                ),
            )),
        }
    }

    for (key, target) in [
        (
            "enable_applications",
            &mut loaded.configuration.command_bar.enable_applications,
        ),
        (
            "enable_files",
            &mut loaded.configuration.command_bar.enable_files,
        ),
        (
            "enable_scripts",
            &mut loaded.configuration.command_bar.enable_scripts,
        ),
        (
            "enable_emoji",
            &mut loaded.configuration.command_bar.enable_emoji,
        ),
        (
            "enable_windows",
            &mut loaded.configuration.command_bar.enable_windows,
        ),
        (
            "enable_clipboard",
            &mut loaded.configuration.command_bar.enable_clipboard,
        ),
    ] {
        if let Some(value) = section.get(key) {
            match bool::deserialize(value.clone()) {
                Ok(parsed) => *target = parsed,
                Err(error) => loaded.warnings.push(warning(
                    &format!("command_bar.{key}"),
                    format!("The value is invalid and was ignored: {error}"),
                )),
            }
        }
    }

    if let Some(roots) = section.get("file_roots") {
        match Vec::<String>::deserialize(roots.clone()) {
            Ok(parsed)
                if parsed.len() <= MAX_COMMAND_FILE_ROOTS
                    && parsed.iter().all(|root| root.trim().starts_with('/')) =>
            {
                loaded.configuration.command_bar.file_roots = parsed;
            }
            _ => loaded.warnings.push(warning(
                "command_bar.file_roots",
                format!(
                    "File roots must be at most {MAX_COMMAND_FILE_ROOTS} absolute paths; the \
                     default was retained."
                ),
            )),
        }
    }

    if let Some(scripts) = section.get("scripts") {
        match Vec::<kestrel_core::CommandScriptConfiguration>::deserialize(scripts.clone()) {
            Ok(parsed) => {
                // Malformed scripts are dropped while the table remains valid.
                let mut accepted = Vec::new();
                let mut names: Vec<String> = Vec::new();
                for (index, script) in parsed.into_iter().enumerate() {
                    let location = format!("command_bar.scripts[{index}]");
                    if let Err(error) = script.validate() {
                        loaded
                            .warnings
                            .push(warning(&location, format!("Script was ignored: {error:?}")));
                        continue;
                    }
                    let name = script.name.trim().to_string();
                    if names.contains(&name) {
                        loaded.warnings.push(warning(
                            &location,
                            format!("Script \"{name}\" is a duplicate and was ignored"),
                        ));
                        continue;
                    }
                    names.push(name);
                    accepted.push(script);
                }
                loaded.configuration.command_bar.scripts = accepted;
            }
            Err(error) => loaded.warnings.push(warning(
                "command_bar.scripts",
                format!("Script definitions are invalid and were ignored: {error}"),
            )),
        }
    }

    // Invalid command-bar roots are dropped while switches remain.
    if let Err(error) = loaded.configuration.command_bar.validate() {
        let existing = loaded.configuration.command_bar.clone();
        loaded.configuration.command_bar = kestrel_core::CommandBarConfiguration {
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
        loaded.warnings.push(warning(
            "command_bar",
            format!("The command bar configuration was inconsistent and was repaired: {error:?}"),
        ));
    }
}

fn parse_monitoring_alerts(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(value) = value else { return };
    let Some(alerts) = value.as_table() else {
        loaded.warnings.push(warning(
            "monitoring.alerts",
            "The monitoring alerts value must be a TOML table; defaults were retained.",
        ));
        return;
    };

    for kind in AlertKind::ALL {
        let name = alert_kind_name(kind);
        if let Some(rule) = alerts.get(name) {
            parse_alert_rule(loaded, kind, rule);
        }
    }
    for name in alerts.keys() {
        if !AlertKind::ALL
            .iter()
            .any(|kind| alert_kind_name(*kind) == name.as_str())
        {
            loaded.warnings.push(warning(
                &format!("monitoring.alerts.{name}"),
                "Unknown alert kind was ignored.",
            ));
        }
    }
}

fn parse_alert_rule(loaded: &mut LoadedConfiguration, kind: AlertKind, value: &toml::Value) {
    let location = format!("monitoring.alerts.{}", alert_kind_name(kind));
    let Some(rule) = value.as_table() else {
        loaded.warnings.push(warning(
            &location,
            "Alert rule must be a TOML table; defaults were retained.",
        ));
        return;
    };

    if let Some(enabled) = rule.get("enabled") {
        match bool::deserialize(enabled.clone()) {
            Ok(enabled) => {
                loaded
                    .configuration
                    .monitoring
                    .alerts
                    .rule_mut(kind)
                    .enabled = enabled
            }
            Err(error) => loaded.warnings.push(warning(
                &format!("{location}.enabled"),
                format!("Alert enabled value is invalid and the default was retained: {error}"),
            )),
        }
    }

    if let Some(threshold) = rule.get("threshold") {
        let maximum = if kind == AlertKind::Temperature {
            MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS
        } else {
            MAX_ALERT_THRESHOLD_PERCENT
        };
        match f64::deserialize(threshold.clone()) {
            Ok(threshold) if threshold.is_finite() && threshold > 0.0 && threshold <= maximum => {
                loaded
                    .configuration
                    .monitoring
                    .alerts
                    .rule_mut(kind)
                    .threshold = threshold;
            }
            _ => loaded.warnings.push(warning(
                &format!("{location}.threshold"),
                format!(
                    "Alert threshold must be finite, greater than zero, and no greater than \
                     {maximum}; the default was retained."
                ),
            )),
        }
    }

    if let Some(sustain) = rule.get("sustain_samples") {
        match u32::deserialize(sustain.clone()) {
            Ok(samples)
                if (MIN_ALERT_SUSTAIN_SAMPLES..=MAX_ALERT_SUSTAIN_SAMPLES).contains(&samples) =>
            {
                loaded
                    .configuration
                    .monitoring
                    .alerts
                    .rule_mut(kind)
                    .sustain_samples = samples;
            }
            _ => loaded.warnings.push(warning(
                &format!("{location}.sustain_samples"),
                format!(
                    "Alert sustain samples must be between {MIN_ALERT_SUSTAIN_SAMPLES} and \
                     {MAX_ALERT_SUSTAIN_SAMPLES}; the default was retained."
                ),
            )),
        }
    }

    if let Some(cooldown) = rule.get("cooldown_seconds") {
        match u64::deserialize(cooldown.clone()) {
            Ok(seconds) if seconds <= MAX_ALERT_COOLDOWN_SECONDS => {
                loaded
                    .configuration
                    .monitoring
                    .alerts
                    .rule_mut(kind)
                    .cooldown_seconds = seconds;
            }
            _ => loaded.warnings.push(warning(
                &format!("{location}.cooldown_seconds"),
                format!(
                    "Alert cooldown must be no greater than {MAX_ALERT_COOLDOWN_SECONDS} seconds; \
                     the default was retained."
                ),
            )),
        }
    }
}

fn alert_kind_name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Cpu => "cpu",
        AlertKind::Temperature => "temperature",
        AlertKind::Memory => "memory",
        AlertKind::Disk => "disk",
        AlertKind::Battery => "battery",
    }
}

pub(crate) fn warning(location: &str, reason: impl Into<String>) -> ConfigurationWarning {
    ConfigurationWarning {
        feature_id: location.to_owned(),
        reason: reason.into(),
    }
}

fn feature_warning(location: &str, error: ConfigurationError) -> ConfigurationWarning {
    warning(
        location,
        format!("Feature settings were ignored: {error:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigurationLoadError, export_string, import_file, import_string, load, parse, save,
        videos_directory,
    };
    use kestrel_core::{
        AppearancePreference, ApplicationConfiguration, AudioDisconnectPolicy, AudioOutputSwitch,
        CURRENT_CONFIGURATION_SCHEMA_VERSION, MonitorReadout,
    };
    use std::path::{Path, PathBuf};

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
"#,
        )
        .expect("legacy panel sections are ignored");
        assert_eq!(
            loaded.configuration.ui.appearance,
            AppearancePreference::Dark
        );
        assert!(loaded.configuration.startup.autostart);
        assert!(loaded.warnings.is_empty());
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

    #[test]
    fn malformed_monitoring_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
refresh_interval_millis = 500
history_samples = 0
readouts = ["network", "unknown", "network", "cpu"]
[monitoring.alerts.cpu]
enabled = false
threshold = 101.0
sustain_samples = 7
cooldown_seconds = 42
[monitoring.alerts.temperature]
threshold = 125.0
sustain_samples = "bad"
"#,
        )
        .expect("document itself is valid TOML");
        assert_eq!(loaded.configuration.monitoring.refresh_interval_millis, 500);
        assert_eq!(
            loaded.configuration.monitoring.history_samples,
            kestrel_core::DEFAULT_MONITOR_HISTORY_SAMPLES
        );
        assert_eq!(
            loaded.configuration.monitoring.readouts,
            vec![MonitorReadout::Network, MonitorReadout::Cpu]
        );
        assert!(!loaded.configuration.monitoring.alerts.cpu.enabled);
        assert_eq!(loaded.configuration.monitoring.alerts.cpu.threshold, 90.0);
        assert_eq!(
            loaded.configuration.monitoring.alerts.cpu.sustain_samples,
            7
        );
        assert_eq!(
            loaded.configuration.monitoring.alerts.cpu.cooldown_seconds,
            42
        );
        assert_eq!(
            loaded.configuration.monitoring.alerts.temperature.threshold,
            125.0
        );
        assert_eq!(
            loaded
                .configuration
                .monitoring
                .alerts
                .temperature
                .sustain_samples,
            60
        );
        for location in [
            "monitoring.history_samples",
            "monitoring.readouts[1]",
            "monitoring.readouts[2]",
            "monitoring.alerts.cpu.threshold",
            "monitoring.alerts.temperature.sustain_samples",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location)
            );
        }
    }

    #[test]
    fn clipboard_history_layout_parses_and_a_bad_value_keeps_the_list() {
        let loaded = parse("schema_version = 3\n[clipboard]\nhistory_layout = \"cards\"\n")
            .expect("document itself is valid TOML");
        assert_eq!(
            loaded.configuration.clipboard.history_layout,
            kestrel_core::ClipboardLayout::Cards
        );
        assert!(loaded.warnings.is_empty());

        let loaded =
            parse("schema_version = 3\n[clipboard]\nhistory_layout = \"grid\"\nmax_items = 50\n")
                .expect("document itself is valid TOML");
        let clipboard = &loaded.configuration.clipboard;
        assert_eq!(
            clipboard.history_layout,
            kestrel_core::ClipboardLayout::List
        );
        assert_eq!(clipboard.max_items, 50, "other keys still apply");
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "clipboard.history_layout")
        );
    }

    #[test]
    fn monitoring_depth_fields_parse_and_fail_independently() {
        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
network_rate_unit = "bits"
per_core_cpu = true
tray_readouts = ["cpu", "battery"]
"#,
        )
        .expect("document itself is valid TOML");
        let monitoring = &loaded.configuration.monitoring;
        assert_eq!(
            monitoring.network_rate_unit,
            kestrel_core::NetworkRateUnit::Bits
        );
        assert!(monitoring.per_core_cpu);
        assert_eq!(
            monitoring.tray_readouts,
            [MonitorReadout::Cpu, MonitorReadout::Battery]
        );
        assert!(loaded.warnings.is_empty());

        let loaded = parse(
            r#"
schema_version = 3
[monitoring]
refresh_interval_millis = 2000
network_rate_unit = "nibbles"
per_core_cpu = "yes"
tray_readouts = ["cpu", "memory", "swap", "disk"]
"#,
        )
        .expect("document itself is valid TOML");
        let monitoring = &loaded.configuration.monitoring;
        assert_eq!(
            monitoring.refresh_interval_millis, 2000,
            "valid keys still apply"
        );
        assert_eq!(
            monitoring.network_rate_unit,
            kestrel_core::NetworkRateUnit::Bytes
        );
        assert!(!monitoring.per_core_cpu);
        assert!(monitoring.tray_readouts.is_empty());
        for location in [
            "monitoring.network_rate_unit",
            "monitoring.per_core_cpu",
            "monitoring.tray_readouts",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
    }

    #[test]
    fn audio_table_parses_every_mixer_policy_field() {
        let loaded = parse(
            r#"
schema_version = 3
[audio]
boost_percent = 140
output_switch = "all_streams"
disconnect_policy = "reset_volume"
disconnect_volume_percent = 65
include_inactive_streams = true
"#,
        )
        .expect("document itself is valid TOML");

        let audio = loaded.configuration.audio;
        assert_eq!(audio.boost_percent, 140);
        assert_eq!(audio.output_switch, AudioOutputSwitch::AllStreams);
        assert_eq!(audio.disconnect_policy, AudioDisconnectPolicy::ResetVolume);
        assert_eq!(audio.disconnect_volume_percent, 65);
        assert!(audio.include_inactive_streams);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn malformed_audio_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[audio]
boost_percent = 400
output_switch = "surround"
disconnect_policy = "forget"
disconnect_volume_percent = 160
include_inactive_streams = "sometimes"
output_priority = ["headset", "headset"]
input_priority = "usb-mic"
mixer_favorites = [""]
follow_output_priority = "yes"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.audio,
            kestrel_core::AudioConfiguration::default()
        );
        for location in [
            "audio.boost_percent",
            "audio.output_switch",
            "audio.disconnect_policy",
            "audio.disconnect_volume_percent",
            "audio.include_inactive_streams",
            "audio.output_priority",
            "audio.input_priority",
            "audio.mixer_favorites",
            "audio.follow_output_priority",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
    }

    #[test]
    fn audio_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.audio.boost_percent = 150;
        configuration.audio.output_switch = AudioOutputSwitch::AllStreams;
        configuration.audio.disconnect_policy = AudioDisconnectPolicy::ResetVolume;
        configuration.audio.disconnect_volume_percent = 40;
        configuration.audio.include_inactive_streams = true;
        configuration.audio.output_priority = vec!["bluez_output.headset".into(), "hdmi".into()];
        configuration.audio.follow_output_priority = true;
        configuration.audio.input_priority = vec!["usb-mic".into()];
        configuration.audio.follow_input_priority = true;
        configuration.audio.mixer_favorites = vec!["firefox".into(), "spotify".into()];

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[audio]"));
        assert!(exported.contains("boost_percent = 150"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.audio, configuration.audio);
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn speed_test_table_parses_every_field() {
        let loaded = parse(
            r#"
schema_version = 3
[speed_test]
download_megabytes = 40
upload_megabytes = 20
timeout_seconds = 60
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.speed_test,
            kestrel_core::SpeedTestConfiguration {
                download_megabytes: 40,
                upload_megabytes: 20,
                timeout_seconds: 60,
            }
        );
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn malformed_speed_test_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[speed_test]
download_megabytes = 0
upload_megabytes = 51
timeout_seconds = "slow"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.speed_test,
            kestrel_core::SpeedTestConfiguration::default()
        );
        for location in [
            "speed_test.download_megabytes",
            "speed_test.upload_megabytes",
            "speed_test.timeout_seconds",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
    }

    #[test]
    fn speed_test_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.speed_test.download_megabytes = 75;
        configuration.speed_test.upload_megabytes = 0;
        configuration.speed_test.timeout_seconds = 90;

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[speed_test]"));
        assert!(exported.contains("download_megabytes = 75"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.speed_test, configuration.speed_test);
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn capture_limits_parse_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[capture]
max_entries = 0
max_total_megabytes = 512
max_age_hours = 721
"#,
        )
        .expect("document itself is valid TOML");
        let defaults = kestrel_core::CaptureConfiguration::default();
        assert_eq!(
            loaded.configuration.capture.max_entries,
            defaults.max_entries
        );
        assert_eq!(loaded.configuration.capture.max_total_megabytes, 512);
        assert_eq!(
            loaded.configuration.capture.max_age_hours,
            defaults.max_age_hours
        );
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["capture.max_entries", "capture.max_age_hours"]);

        let exported = export_string(&loaded.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.capture, loaded.configuration.capture);
    }

    #[test]
    fn process_limits_reject_out_of_range_keys_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[processes]
refresh_millis = 500
max_rows = 250
"#,
        )
        .expect("document itself is valid TOML");
        let defaults = kestrel_core::ProcessConfiguration::default();
        assert_eq!(
            loaded.configuration.processes.refresh_millis,
            defaults.refresh_millis
        );
        assert_eq!(loaded.configuration.processes.max_rows, 250);
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["processes.refresh_millis"]);

        let exported = export_string(&loaded.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.processes,
            loaded.configuration.processes
        );
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn screen_tool_settings_reject_unsafe_languages_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[screen_tools]
ocr_language = "-l eng"
ocr_timeout_seconds = 60
"#,
        )
        .expect("document itself is valid TOML");
        assert_eq!(loaded.configuration.screen_tools.ocr_language, "eng");
        assert_eq!(loaded.configuration.screen_tools.ocr_timeout_seconds, 60);
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["screen_tools.ocr_language"]);

        let mut configuration = loaded.configuration.clone();
        configuration.screen_tools.ocr_language = "eng+deu".to_owned();
        let exported = export_string(&configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.screen_tools,
            configuration.screen_tools
        );
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn recording_settings_reject_bad_values_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[recording]
framerate = 240
cursor = false
microphone = "yes"
system_audio = true
"#,
        )
        .expect("document itself is valid TOML");
        let recording = loaded.configuration.recording;
        assert_eq!(
            recording.framerate,
            kestrel_core::DEFAULT_RECORDING_FRAMERATE
        );
        assert!(!recording.cursor);
        assert!(recording.system_audio);
        assert!(!recording.microphone);
        let mut rejected: Vec<&str> = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect();
        rejected.sort_unstable();
        assert_eq!(rejected, ["recording.framerate", "recording.microphone"]);

        let mut configuration = loaded.configuration.clone();
        configuration.recording.framerate = 60;
        let exported = export_string(&configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.recording, configuration.recording);
        assert!(reloaded.warnings.is_empty());
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

    #[test]
    fn videos_folder_follows_user_dirs() {
        let home = Path::new("/home/user");
        let folder = |contents: &str| videos_directory(contents, home);
        assert_eq!(
            folder("# comment\nXDG_VIDEOS_DIR=\"$HOME/Filme\"\n"),
            Some(PathBuf::from("/home/user/Filme"))
        );
        assert_eq!(
            folder("XDG_VIDEOS_DIR=\"/data/videos\""),
            Some(PathBuf::from("/data/videos"))
        );
        assert_eq!(folder("XDG_VIDEOS_DIR=\"$HOME/\""), None, "disabled entry");
        assert_eq!(folder("XDG_VIDEOS_DIR=\"relative\""), None);
        assert_eq!(folder("XDG_MUSIC_DIR=\"$HOME/Music\""), None);
    }

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

    #[test]
    fn clipboard_table_parses_every_retention_bound() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_items = 250
max_item_bytes = 524288
max_image_bytes = 2097152
max_file_entries = 12
max_total_bytes = 8388608
max_age_hours = 6
clear_seconds = 45
filter_sensitive = true
paste_plain_text = false
"#,
        )
        .expect("document itself is valid TOML");

        let clipboard = loaded.configuration.clipboard;
        assert_eq!(clipboard.max_items, 250);
        assert_eq!(clipboard.max_item_bytes, 524_288);
        assert_eq!(clipboard.max_image_bytes, 2_097_152);
        assert_eq!(clipboard.max_file_entries, 12);
        assert_eq!(clipboard.max_total_bytes, 8_388_608);
        assert_eq!(clipboard.max_age_hours, 6);
        assert_eq!(clipboard.clear_seconds, 45);
        assert!(clipboard.filter_sensitive);
        assert!(!clipboard.paste_plain_text);
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn malformed_clipboard_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_items = 100000
max_item_bytes = 0
max_image_bytes = "big"
max_file_entries = 0
max_age_hours = 0
clear_seconds = 999999
filter_sensitive = "yes"
paste_plain_text = 3
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.clipboard,
            kestrel_core::ClipboardConfiguration::default()
        );
        for location in [
            "clipboard.max_items",
            "clipboard.max_item_bytes",
            "clipboard.max_image_bytes",
            "clipboard.max_file_entries",
            "clipboard.max_age_hours",
            "clipboard.clear_seconds",
            "clipboard.filter_sensitive",
            "clipboard.paste_plain_text",
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
    fn an_inconsistent_clipboard_bound_pair_is_reported_and_repaired() {
        let loaded = parse(
            r#"
schema_version = 3
[clipboard]
max_total_bytes = 4096
max_item_bytes = 1048576
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.clipboard.max_item_bytes, 4096,
            "the per-entry bound is lowered to the explicit total bound"
        );
        assert_eq!(loaded.configuration.clipboard.max_image_bytes, 4096);
        assert_eq!(loaded.configuration.clipboard.max_total_bytes, 4096);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "clipboard.max_total_bytes")
        );
        assert_eq!(loaded.configuration.validate(), Ok(()));
    }

    #[test]
    fn clipboard_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.clipboard.max_items = 12;
        configuration.clipboard.clear_seconds = 90;
        configuration.clipboard.filter_sensitive = true;

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[clipboard]"));
        assert!(exported.contains("max_items = 12"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.clipboard, configuration.clipboard);
        assert!(reloaded.warnings.is_empty());
    }

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

    #[test]
    fn readout_order_survives_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.monitoring.readouts = vec![
            MonitorReadout::Gpu,
            MonitorReadout::Cpu,
            MonitorReadout::Network,
        ];
        let exported = export_string(&configuration).expect("configuration exports");
        let imported = import_string(&exported).expect("configuration imports");
        assert_eq!(imported.configuration, configuration);
    }

    #[test]
    fn alert_threshold_sustain_and_cooldown_survive_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.monitoring.alerts.cpu.threshold = 77.5;
        configuration.monitoring.alerts.cpu.sustain_samples = 12;
        configuration.monitoring.alerts.cpu.cooldown_seconds = 1_234;
        configuration.monitoring.alerts.temperature.threshold = 120.0;
        configuration.monitoring.alerts.temperature.sustain_samples = 4;
        configuration.monitoring.alerts.temperature.cooldown_seconds = 2_345;
        let exported = export_string(&configuration).expect("configuration exports");
        let imported = import_string(&exported).expect("configuration imports");
        assert_eq!(
            imported.configuration.monitoring.alerts,
            configuration.monitoring.alerts
        );
    }

    #[test]
    fn missing_file_uses_defaults() {
        let path =
            std::env::temp_dir().join(format!("kestrel-missing-config-{}", std::process::id()));

        let loaded = load(&path).expect("missing configuration is not an error");
        assert_eq!(loaded.configuration, ApplicationConfiguration::default());
    }
    #[test]
    fn explicit_import_reports_missing_file() {
        let path =
            std::env::temp_dir().join(format!("kestrel-missing-import-{}", std::process::id()));
        let error = import_file(&path).expect_err("explicit import must not default on absence");
        assert!(matches!(
            error,
            ConfigurationLoadError::Io(error)
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn portable_export_round_trips_only_owned_fields() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled("audio.mixer", true)
            .expect("valid feature ID");
        configuration.ui.appearance = AppearancePreference::Light;
        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("schema_version = 3"));
        assert!(exported.contains("[features.\"audio.mixer\"]"));
        assert!(exported.contains("[ui]"));
        assert!(exported.contains("[startup]"));
        assert!(exported.contains("[monitoring]"));
        assert_eq!(
            import_string(&exported)
                .expect("export imports")
                .configuration,
            configuration
        );
    }

    #[test]
    fn saves_current_schema_without_sensitive_fields() {
        let directory =
            std::env::temp_dir().join(format!("kestrel-config-test-{}", std::process::id()));
        let path = directory.join("config.toml");
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled("audio.mixer", true)
            .expect("valid feature ID");
        save(&path, &configuration).expect("configuration saves");
        let saved = std::fs::read_to_string(&path).expect("configuration is readable");
        std::fs::remove_dir_all(directory).expect("temporary directory is removable");
        assert!(saved.contains("schema_version = 3"));
        assert!(saved.contains("[features.\"audio.mixer\"]"));
        assert!(!saved.contains("token"));
    }
    #[test]
    fn export_excludes_machine_specific_paths() {
        let configuration = ApplicationConfiguration::default();
        let exported = export_string(&configuration).expect("configuration exports");
        for path in ["/proc", "/sys", "/dev/", "/tmp/"] {
            assert!(
                !exported.contains(path),
                "export unexpectedly contains {path}"
            );
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            assert!(!exported.contains(home.as_ref()));
        }
        if let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") {
            for mount_point in mounts
                .lines()
                .filter_map(|line| line.split_whitespace().nth(1))
            {
                assert!(
                    !exported.contains(mount_point),
                    "export unexpectedly contains mount point {mount_point}"
                );
            }
        }
    }
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
