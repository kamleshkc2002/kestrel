use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::{
    AppearancePreference, ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION,
    ConfigurationError, FeatureConfiguration, PanelSection, PanelSectionConfiguration,
    StartupConfiguration, validate_feature_id,
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
        CURRENT_CONFIGURATION_SCHEMA_VERSION => parse_v2(document),
        version => Err(ConfigurationLoadError::UnsupportedSchemaVersion(version)),
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

    for section in [PanelSection::QuickControls, PanelSection::FeatureHub] {
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
        AppearancePreference, ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION,
        PanelSection,
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
        assert!(exported.contains("schema_version = 2"));
        assert!(exported.contains("[features.\"audio.mixer\"]"));
        assert!(exported.contains("[ui]"));
        assert!(exported.contains("[startup]"));
        assert!(!exported.contains("token"));
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
        assert!(saved.contains("schema_version = 2"));
        assert!(saved.contains("[features.\"audio.mixer\"]"));
        assert!(!saved.contains("token"));
    }
}
