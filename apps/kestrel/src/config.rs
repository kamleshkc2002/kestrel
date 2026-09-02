use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::{
    validate_feature_id, ApplicationConfiguration, ConfigurationError, FeatureConfiguration,
    CURRENT_CONFIGURATION_SCHEMA_VERSION,
};
use serde::Deserialize;

/// Configuration loaded from disk, including isolated feature-level diagnostics.
#[derive(Debug, Default)]
pub struct LoadedConfiguration {
    pub configuration: ApplicationConfiguration,
    pub warnings: Vec<ConfigurationWarning>,
}

/// A malformed setting that disabled only its own feature preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationWarning {
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

/// Loads and migrates a configuration file, retaining valid feature settings.
pub fn load(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedConfiguration::default())
        }
        Err(error) => Err(ConfigurationLoadError::Io(error)),
    }
}

/// Saves only the versioned non-sensitive settings owned by this application.
pub fn save(path: &Path, configuration: &ApplicationConfiguration) -> Result<(), std::io::Error> {
    let content = toml::to_string_pretty(configuration)
        .expect("ApplicationConfiguration contains only serializable settings");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
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
        CURRENT_CONFIGURATION_SCHEMA_VERSION => parse_v1(document),
        version => Err(ConfigurationLoadError::UnsupportedSchemaVersion(version)),
    }
}

fn migrate_v0(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration::default();
    let Some(enabled_features) = document.get("enabled_features") else {
        return Ok(loaded);
    };
    let Some(feature_ids) = enabled_features.as_array() else {
        loaded.warnings.push(ConfigurationWarning {
            feature_id: "enabled_features".to_string(),
            reason: "Legacy enabled_features must be an array of feature IDs.".to_string(),
        });
        return Ok(loaded);
    };

    for value in feature_ids {
        let Some(feature_id) = value.as_str() else {
            loaded.warnings.push(ConfigurationWarning {
                feature_id: "<legacy entry>".to_string(),
                reason: "Legacy enabled_features entries must be strings.".to_string(),
            });
            continue;
        };
        if let Err(error) = loaded.configuration.set_feature_enabled(feature_id, true) {
            loaded.warnings.push(feature_warning(feature_id, error));
        }
    }
    Ok(loaded)
}

fn parse_v1(document: toml::Table) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    let mut loaded = LoadedConfiguration {
        configuration: ApplicationConfiguration::default(),
        warnings: Vec::new(),
    };
    let Some(features) = document.get("features") else {
        return Ok(loaded);
    };
    let Some(features) = features.as_table() else {
        loaded.warnings.push(ConfigurationWarning {
            feature_id: "features".to_string(),
            reason: "The features value must be a TOML table.".to_string(),
        });
        return Ok(loaded);
    };

    for (feature_id, value) in features {
        if let Err(error) = validate_feature_id(feature_id) {
            loaded.warnings.push(feature_warning(feature_id, error));
            continue;
        }

        match FeatureConfiguration::deserialize(value.clone()) {
            Ok(feature) => {
                loaded
                    .configuration
                    .features
                    .insert(feature_id.clone(), feature);
            }
            Err(error) => loaded.warnings.push(ConfigurationWarning {
                feature_id: feature_id.clone(),
                reason: format!("Feature settings are invalid and were ignored: {error}"),
            }),
        }
    }
    Ok(loaded)
}

fn feature_warning(feature_id: &str, error: ConfigurationError) -> ConfigurationWarning {
    ConfigurationWarning {
        feature_id: feature_id.to_string(),
        reason: format!("Feature settings were ignored: {error:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{load, parse, save};
    use kestrel_core::{ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION};

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
    fn ignores_only_malformed_feature_settings() {
        let loaded = parse(
            r#"
schema_version = 1
[features."audio.mixer"]
enabled = true
[features.clipboard]
enabled = "yes"
"#,
        )
        .expect("document itself is valid TOML");

        assert!(loaded.configuration.feature_enabled("audio.mixer"));
        assert!(!loaded.configuration.feature_enabled("clipboard"));
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].feature_id, "clipboard");
    }

    #[test]
    fn missing_file_uses_defaults() {
        let path =
            std::env::temp_dir().join(format!("kestrel-missing-config-{}", std::process::id()));
        let loaded = load(&path).expect("missing configuration is not an error");

        assert_eq!(loaded.configuration, ApplicationConfiguration::default());
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

        assert!(saved.contains("schema_version = 1"));
        assert!(saved.contains("[features.\"audio.mixer\"]"));
        assert!(!saved.contains("token"));
    }
}
