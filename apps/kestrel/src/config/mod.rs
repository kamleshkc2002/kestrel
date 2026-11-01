//! Portable application configuration.

use std::{fs, path::Path};

use kestrel_core::{
    ApplicationConfiguration, CURRENT_CONFIGURATION_SCHEMA_VERSION, ConfigurationError,
};

mod audio;
mod bounds;
mod capture;
mod clipboard;
mod command_bar;
mod features;
mod fields;
mod logging;
mod migrate;
mod monitoring;
mod overlay;
mod paths;
mod quick_panel;
mod shortcuts;
mod snippets;
mod speed_test;

pub(crate) use paths::state_directory;
pub use paths::{configuration_path, recordings_path};

/// Loaded configuration and isolated diagnostics.
#[derive(Debug, Default)]
pub struct LoadedConfiguration {
    pub configuration: ApplicationConfiguration,
    pub warnings: Vec<ConfigurationWarning>,
    /// A configuration file existed on disk when this was loaded.
    pub existed: bool,
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

/// Loads and migrates configuration, retaining valid settings.
pub fn load(path: &Path) -> Result<LoadedConfiguration, ConfigurationLoadError> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let mut loaded = import_string(&contents)?;
            loaded.existed = true;
            Ok(loaded)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedConfiguration::default())
        }
        Err(error) => Err(ConfigurationLoadError::Io(error)),
    }
}

/// Saves typed, versioned settings privately and atomically.
pub fn save(path: &Path, configuration: &ApplicationConfiguration) -> Result<(), std::io::Error> {
    let content = export_string(configuration).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{error:?}"))
    })?;
    crate::private_file::write_private_atomic(path, content.as_bytes())
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
        0 => migrate::migrate_v0(document),
        1 | 2 => migrate::parse_v2(document),
        CURRENT_CONFIGURATION_SCHEMA_VERSION => migrate::parse_current(document),
        version => Err(ConfigurationLoadError::UnsupportedSchemaVersion(version)),
    }
}

pub(crate) fn warning(location: &str, reason: impl Into<String>) -> ConfigurationWarning {
    ConfigurationWarning {
        feature_id: location.to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::{AppearancePreference, ApplicationConfiguration, MonitorReadout};

    use super::{ConfigurationLoadError, export_string, import_file, import_string, load, save};

    #[test]
    fn missing_file_uses_defaults() {
        let path =
            std::env::temp_dir().join(format!("kestrel-missing-config-{}", std::process::id()));

        let loaded = load(&path).expect("missing configuration is not an error");
        assert_eq!(loaded.configuration, ApplicationConfiguration::default());
        assert!(
            !loaded.existed,
            "a missing file reports that no configuration existed"
        );

        let directory =
            std::env::temp_dir().join(format!("kestrel-existing-config-{}", std::process::id()));
        let existing = directory.join("config.toml");
        std::fs::create_dir_all(&directory).expect("temporary directory is creatable");
        std::fs::write(&existing, "").expect("empty configuration is writable");
        let loaded = load(&existing).expect("an empty configuration loads");
        std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
        assert!(
            loaded.existed,
            "an existing file reports that a configuration existed"
        );
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
    fn saved_configuration_is_private_and_leaves_no_temporary() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::TempDir::new().expect("temp dir");
        let path = root.path().join("kestrel/config.toml");
        let configuration = ApplicationConfiguration::default();

        save(&path, &configuration).expect("the save succeeds");
        save(&path, &configuration).expect("a replacement succeeds");

        let file = std::fs::metadata(&path).expect("metadata");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let directory = std::fs::metadata(path.parent().expect("parent")).expect("metadata");
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let entries = std::fs::read_dir(path.parent().expect("parent"))
            .expect("readable")
            .count();
        assert_eq!(entries, 1, "no temporary file is left behind");
        let reloaded = load(&path).expect("the saved file loads");
        assert_eq!(reloaded.configuration, configuration);
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
}
