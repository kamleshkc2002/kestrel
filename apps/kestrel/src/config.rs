use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::{
    AlertKind, AppearancePreference, ApplicationConfiguration, AudioDisconnectPolicy,
    AudioOutputSwitch, CURRENT_CONFIGURATION_SCHEMA_VERSION, ConfigurationError,
    FeatureConfiguration, MAX_ALERT_COOLDOWN_SECONDS, MAX_ALERT_SUSTAIN_SAMPLES,
    MAX_ALERT_THRESHOLD_PERCENT, MAX_AUDIO_BOOST_PERCENT, MAX_CLIPBOARD_AGE_HOURS,
    MAX_CLIPBOARD_CLEAR_SECONDS, MAX_CLIPBOARD_FILE_ENTRIES, MAX_CLIPBOARD_IMAGE_BYTES,
    MAX_CLIPBOARD_MAX_ITEMS, MAX_CLIPBOARD_TOTAL_BYTES, MAX_COMMAND_FILE_ROOTS,
    MAX_COMMAND_RESULTS, MAX_MONITOR_HISTORY_SAMPLES, MAX_MONITOR_REFRESH_INTERVAL_MILLIS,
    MAX_SNIPPET_CLIPBOARD_BYTES, MAX_SNIPPET_CONTENT_BYTES, MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
    MAX_TEMPERATURE_ALERT_THRESHOLD_CELSIUS, MIN_ALERT_SUSTAIN_SAMPLES, MIN_CLIPBOARD_ITEM_BYTES,
    MIN_COMMAND_RESULTS, MIN_MONITOR_REFRESH_INTERVAL_MILLIS, MIN_SNIPPET_CLIPBOARD_BYTES,
    MIN_SNIPPET_CONTENT_BYTES, MIN_SNIPPET_INSERT_TIMEOUT_MILLIS, MonitorReadout, PanelSection,
    PanelSectionConfiguration, SnippetExpansionTiming, SnippetProviderPreference,
    StartupConfiguration, UNAMPLIFIED_AUDIO_VOLUME_PERCENT, validate_feature_id,
};
use serde::Deserialize;

/// Configuration loaded from disk, including isolated feature-level diagnostics.
#[derive(Debug, Default)]
pub struct LoadedConfiguration {
    pub configuration: ApplicationConfiguration,
    pub warnings: Vec<ConfigurationWarning>,
}

/// A malformed setting that disabled only its own preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationWarning {
    /// A dotted portable-document location, such as `features.audio.mixer`.
    pub feature_id: String,
    pub reason: String,
}

/// An unrecoverable configuration I/O or document-level error.
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

/// Returns the XDG path used for Kestrel's non-sensitive configuration.
pub fn configuration_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("kestrel").join("config.toml"))
}

/// Loads and migrates a configuration file, retaining valid settings.
pub fn load(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    match fs::read_to_string(path) {
        Ok(contents) => import_string(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedConfiguration::default())
        }
        Err(error) => Err(ConfigurationLoadError::Io(error)),
    }
}

/// Saves only the typed, versioned non-sensitive settings owned by this application.
pub fn save(path: &Path, configuration: &ApplicationConfiguration) -> Result<(), std::io::Error> {
    let content = export_string(configuration).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{error:?}"))
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

/// Imports a portable configuration string. Isolated setting errors become warnings.
pub fn import_string(contents: &str) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    parse(contents)
}

/// Exports only the typed portable configuration, in deterministic TOML form.
pub fn export_string(
    configuration: &ApplicationConfiguration,
) -> Result<String, ConfigurationError> {
    configuration.validate()?;
    Ok(toml::to_string_pretty(configuration).expect("portable configuration is serializable"))
}

/// Imports a portable configuration file. Unlike startup loading, a missing
/// explicitly selected import is reported to the caller.
pub fn import_file(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let contents = fs::read_to_string(path).map_err(ConfigurationLoadError::Io)?;
    import_string(&contents)
}

/// Exports a portable configuration file.
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
        1 => migrate_v1(document),
        2 => migrate_v2(document),
        CURRENT_CONFIGURATION_SCHEMA_VERSION => parse_current(document),
        version => Err(ConfigurationLoadError::UnsupportedSchemaVersion(version)),
    }
}

fn migrate_v2(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let source_has_monitoring = document
        .get("ui")
        .and_then(|value| value.as_table())
        .and_then(|ui| ui.get("panel_sections"))
        .and_then(|value| value.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                PanelSectionConfiguration::deserialize(entry.clone())
                    .map(|entry| entry.section == PanelSection::Monitoring)
                    .unwrap_or(false)
            })
        });
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
    if !source_has_monitoring
        && !loaded.warnings.iter().any(|warning| {
            warning.feature_id == "ui.panel_sections"
                && warning.reason == "Missing Monitoring panel section; its default was restored."
        })
    {
        loaded.warnings.push(warning(
            "ui.panel_sections",
            "Missing Monitoring panel section; its default was restored.",
        ));
    }
    Ok(loaded)
}

fn parse_current(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
    parse_monitoring(&mut loaded, document.get("monitoring"));
    parse_audio(&mut loaded, document.get("audio"));
    parse_clipboard(&mut loaded, document.get("clipboard"));
    parse_snippets(&mut loaded, document.get("snippets"));
    parse_command_bar(&mut loaded, document.get("command_bar"));
    Ok(loaded)
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

fn migrate_v1(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    parse_v2(document)
}

fn parse_v2(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    parse_features(&mut loaded, document.get("features"));
    parse_ui(&mut loaded, document.get("ui"));
    parse_startup(&mut loaded, document.get("startup"));
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

    if let Some(panel_sections) = ui.get("panel_sections") {
        parse_panel_sections(loaded, panel_sections);
    }
}

fn parse_panel_sections(loaded: &mut LoadedConfiguration, value: &toml::Value) {
    let Some(entries) = value.as_array() else {
        loaded.warnings.push(warning(
            "ui.panel_sections",
            "Panel sections must be an array.",
        ));
        return;
    };

    let mut parsed = Vec::new();
    for (index, value) in entries.iter().enumerate() {
        let location = format!("ui.panel_sections[{index}]");
        let entry = match PanelSectionConfiguration::deserialize(value.clone()) {
            Ok(entry) => entry,
            Err(error) => {
                loaded.warnings.push(warning(
                    &location,
                    format!("Panel section is invalid and was ignored: {error}"),
                ));
                continue;
            }
        };
        if parsed
            .iter()
            .any(|item: &PanelSectionConfiguration| item.section == entry.section)
        {
            loaded.warnings.push(warning(
                &location,
                "Duplicate panel section was ignored; the first entry was retained.",
            ));
            continue;
        }
        parsed.push(entry);
    }

    for section in PanelSection::ALL {
        if !parsed.iter().any(|entry| entry.section == section) {
            loaded.warnings.push(warning(
                "ui.panel_sections",
                format!("Missing {section:?} panel section; its default was restored."),
            ));
            parsed.push(PanelSectionConfiguration {
                section,
                visible: true,
            });
        }
    }
    loaded.configuration.ui.panel_sections = parsed;
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

    // A per-item bound above the total byte bound is not a valid shape. The
    // explicit total cap wins, because exceeding it is the one outcome a user
    // cannot have intended, and the entry bounds are lowered to fit it.
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
                // A malformed script is dropped on its own so one typo cannot
                // invalidate the whole table.
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

    // The assembled table must be self-consistent: an inconsistent one keeps the
    // user's provider switches but drops the roots that cannot be trusted.
    if let Err(error) = loaded.configuration.command_bar.validate() {
        let existing = loaded.configuration.command_bar.clone();
        loaded.configuration.command_bar = kestrel_core::CommandBarConfiguration {
            max_results: existing.max_results,
            enable_applications: existing.enable_applications,
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

fn warning(location: &str, reason: impl Into<String>) -> ConfigurationWarning {
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
    };
    use kestrel_core::{
        AppearancePreference, ApplicationConfiguration, AudioDisconnectPolicy, AudioOutputSwitch,
        CURRENT_CONFIGURATION_SCHEMA_VERSION, MonitorReadout, PanelSection,
        PanelSectionConfiguration,
    };

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
[[ui.panel_sections]]
section = "feature_hub"
visible = false
[[ui.panel_sections]]
section = "quick_controls"
visible = true
[startup]
autostart = true
"#,
        )
        .expect("v2 config parses");
        assert_eq!(
            loaded.configuration.ui.appearance,
            AppearancePreference::Dark
        );
        assert!(!loaded.configuration.ui.panel_sections[0].visible);
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
[[ui.panel_sections]]
section = "quick_controls"
visible = false
[[ui.panel_sections]]
section = "quick_controls"
visible = true
[startup]
autostart = "yes"
"#,
        )
        .expect("document itself is valid TOML");
        assert!(loaded.configuration.feature_enabled("audio.mixer"));
        assert_eq!(
            loaded.configuration.ui.panel_sections[0].section,
            PanelSection::QuickControls
        );
        assert!(!loaded.configuration.ui.panel_sections[0].visible);
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
                .any(|warning| warning.feature_id == "ui.panel_sections[1]")
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "startup.autostart")
        );
    }

    #[test]
    fn migrates_v2_with_monitoring_defaults_and_restored_panel() {
        let loaded = parse(
            r#"
schema_version = 2
[ui]
[[ui.panel_sections]]
section = "quick_controls"
visible = false
[[ui.panel_sections]]
section = "feature_hub"
visible = true
"#,
        )
        .expect("v2 config migrates");
        assert_eq!(
            loaded.configuration.monitoring,
            kestrel_core::MonitorConfiguration::default()
        );
        assert_eq!(
            loaded
                .configuration
                .ui
                .panel_sections
                .last()
                .map(|entry| entry.section),
            Some(PanelSection::Monitoring)
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "ui.panel_sections"
                    && warning.reason
                        == "Missing Monitoring panel section; its default was restored.")
        );
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
            3
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

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[audio]"));
        assert!(exported.contains("boost_percent = 150"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.audio, configuration.audio);
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
        // Snippet text lives in its own private file, and the portable export
        // must carry neither that text nor a resolved variable value.
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
    fn readout_order_and_panel_visibility_survive_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.monitoring.readouts = vec![
            MonitorReadout::Gpu,
            MonitorReadout::Cpu,
            MonitorReadout::Network,
        ];
        configuration.ui.panel_sections = vec![
            PanelSectionConfiguration {
                section: PanelSection::FeatureHub,
                visible: false,
            },
            PanelSectionConfiguration {
                section: PanelSection::Monitoring,
                visible: true,
            },
            PanelSectionConfiguration {
                section: PanelSection::QuickControls,
                visible: false,
            },
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
}
