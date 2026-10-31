use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gio::{Settings, SettingsSchemaSource, prelude::SettingsExt};
use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};
use zbus::{
    blocking::{connection::Builder, fdo::DBusProxy},
    names::BusName,
};

use super::{
    QuickToggleControl, QuickToggleError, QuickToggleErrorKind, QuickToggleId, QuickToggleMutation,
    QuickToggleObservation, permission_denied, supported,
};

const APPEARANCE_SCHEMA: &str = "org.gnome.desktop.interface";
const APPEARANCE_KEY: &str = "color-scheme";
const COSMIC_MODE_DIRECTORY: &str = "com.system76.CosmicTheme.Mode/v1";
const COSMIC_MODE_KEY: &str = "is_dark";
const COSMIC_AUTO_SWITCH_KEY: &str = "auto_switch";
const COSMIC_SETTINGS_DAEMON: &str = "com.system76.CosmicSettingsDaemon";
const COSMIC_BUS_TIMEOUT: Duration = Duration::from_secs(2);
const COSMIC_SOURCE_LABEL: &str = "COSMIC theme mode";
const HIDDEN_FILE_SETTINGS: [(&str, &str); 2] = [
    ("org.gnome.nautilus.preferences", "show-hidden-files"),
    ("org.gtk.Settings.FileChooser", "show-hidden"),
];
const DESKTOP_ICON_SETTINGS: [(&str, &str); 3] = [
    ("org.gnome.desktop.background", "show-desktop-icons"),
    ("org.nemo.desktop", "show-desktop-icons"),
    ("org.mate.background", "show-desktop-icons"),
];

/// Base paths that hold COSMIC's theme mode files.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CosmicPaths<'a> {
    pub(crate) config_home: &'a Path,
    pub(crate) system_root: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppearanceBackend {
    Cosmic,
    Gnome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeSource {
    User,
    System,
}

impl ModeSource {
    const fn label(self) -> &'static str {
        match self {
            Self::User => "user setting",
            Self::System => "system default",
        }
    }
}

pub(crate) fn choose_appearance_backend(
    cosmic_mode_known: bool,
    cosmic_daemon_present: bool,
) -> AppearanceBackend {
    if cosmic_mode_known && cosmic_daemon_present {
        AppearanceBackend::Cosmic
    } else {
        AppearanceBackend::Gnome
    }
}

pub(crate) fn appearance_capability(paths: CosmicPaths<'_>) -> CapabilityReport {
    let (backend, source) = selected_appearance_backend(paths);
    match backend {
        AppearanceBackend::Cosmic => {
            cosmic_appearance_capability(source.unwrap_or(ModeSource::System))
        }
        AppearanceBackend::Gnome => gnome_appearance_capability(),
    }
}

pub(crate) fn observe_appearance(
    paths: CosmicPaths<'_>,
) -> Result<QuickToggleObservation, QuickToggleError> {
    match selected_appearance_backend(paths).0 {
        AppearanceBackend::Cosmic => observe_cosmic_appearance(paths),
        AppearanceBackend::Gnome => gnome_observe_appearance(),
    }
}

pub(crate) fn set_appearance(
    paths: CosmicPaths<'_>,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    match selected_appearance_backend(paths).0 {
        AppearanceBackend::Cosmic => set_cosmic_appearance(paths, mutation),
        AppearanceBackend::Gnome => gnome_set_appearance(mutation),
    }
}

fn selected_appearance_backend(paths: CosmicPaths<'_>) -> (AppearanceBackend, Option<ModeSource>) {
    let mode = read_cosmic_bool(paths, COSMIC_MODE_KEY);
    let known = mode.is_ok();
    let daemon = known && cosmic_settings_daemon_present();
    let source = mode.ok().map(|(_, source)| source);
    (choose_appearance_backend(known, daemon), source)
}

fn gnome_appearance_capability() -> CapabilityReport {
    let id = QuickToggleId::Appearance;
    let settings = match lookup(APPEARANCE_SCHEMA, APPEARANCE_KEY) {
        Ok(settings) => settings,
        Err(error) => return unsupported(id, error.message),
    };
    if !settings.is_writable(APPEARANCE_KEY) {
        return permission_denied(
            id,
            Permission::DesktopSettings,
            "The desktop appearance setting is present but read-only.",
            "Allow the current user to change the desktop color-scheme setting.",
        );
    }
    supported(
        id,
        "The desktop exposes a writable dark-appearance preference.",
        "GSettings org.gnome.desktop.interface",
    )
}

fn gnome_observe_appearance() -> Result<QuickToggleObservation, QuickToggleError> {
    let settings = lookup(APPEARANCE_SCHEMA, APPEARANCE_KEY)?;
    let scheme = settings.string(APPEARANCE_KEY);
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled: scheme.as_str() == "prefer-dark",
            confirmation: None,
        },
        detail: format!("Desktop color scheme: {scheme}."),
    })
}

fn gnome_set_appearance(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch());
    };
    let settings = lookup(APPEARANCE_SCHEMA, APPEARANCE_KEY)?;
    settings
        .set_string(
            APPEARANCE_KEY,
            if *enabled { "prefer-dark" } else { "default" },
        )
        .map_err(|error| {
            QuickToggleError::new(
                QuickToggleErrorKind::PermissionDenied,
                format!("the desktop rejected the appearance change: {error}"),
            )
        })
}

fn cosmic_appearance_capability(source: ModeSource) -> CapabilityReport {
    supported(
        QuickToggleId::Appearance,
        "COSMIC exposes a writable light and dark theme mode.",
        format!("{COSMIC_SOURCE_LABEL} (cosmic-config)"),
    )
    .with_evidence(CapabilityEvidence::new(
        "settings-daemon",
        COSMIC_SETTINGS_DAEMON,
    ))
    .with_evidence(CapabilityEvidence::new("mode-source", source.label()))
}

fn observe_cosmic_appearance(
    paths: CosmicPaths<'_>,
) -> Result<QuickToggleObservation, QuickToggleError> {
    let (enabled, _) = read_cosmic_bool(paths, COSMIC_MODE_KEY)?;
    let mut detail = if enabled {
        "COSMIC dark mode is on.".to_owned()
    } else {
        "COSMIC dark mode is off.".to_owned()
    };
    if cosmic_auto_switch(paths) {
        detail.push_str(
            " COSMIC switches between light and dark automatically, so it may change this back.",
        );
    }
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled,
            confirmation: None,
        },
        detail,
    })
}

fn set_cosmic_appearance(
    paths: CosmicPaths<'_>,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch());
    };
    write_cosmic_mode(
        &user_cosmic_path(paths.config_home, COSMIC_MODE_KEY),
        *enabled,
    )?;
    let (written, _) = read_cosmic_bool(paths, COSMIC_MODE_KEY)?;
    if written != *enabled {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::Io,
            "the COSMIC mode value did not read back as written",
        ));
    }
    Ok(())
}

fn user_cosmic_path(config_home: &Path, key: &str) -> PathBuf {
    config_home
        .join("cosmic")
        .join(COSMIC_MODE_DIRECTORY)
        .join(key)
}

fn system_cosmic_path(system_root: &Path, key: &str) -> PathBuf {
    system_root.join(COSMIC_MODE_DIRECTORY).join(key)
}

fn read_cosmic_bool(
    paths: CosmicPaths<'_>,
    key: &str,
) -> Result<(bool, ModeSource), QuickToggleError> {
    match fs::read_to_string(user_cosmic_path(paths.config_home, key)) {
        Ok(value) => parse_cosmic_bool(&value, ModeSource::User),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let value = fs::read_to_string(system_cosmic_path(paths.system_root, key))
                .map_err(|error| QuickToggleError::io("reading the COSMIC theme mode", &error))?;
            parse_cosmic_bool(&value, ModeSource::System)
        }
        Err(error) => Err(QuickToggleError::io(
            "reading the COSMIC theme mode",
            &error,
        )),
    }
}

fn parse_cosmic_bool(
    value: &str,
    source: ModeSource,
) -> Result<(bool, ModeSource), QuickToggleError> {
    match value.trim() {
        "true" => Ok((true, source)),
        "false" => Ok((false, source)),
        _ => Err(QuickToggleError::protocol(
            "the COSMIC mode value is not a boolean",
        )),
    }
}

fn cosmic_auto_switch(paths: CosmicPaths<'_>) -> bool {
    read_cosmic_bool(paths, COSMIC_AUTO_SWITCH_KEY)
        .map(|(enabled, _)| enabled)
        .unwrap_or(false)
}

fn write_cosmic_mode(path: &Path, enabled: bool) -> Result<(), QuickToggleError> {
    let parent = path.parent().ok_or_else(|| {
        QuickToggleError::unavailable("the COSMIC mode path has no parent directory")
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        QuickToggleError::io("creating the COSMIC theme mode directory", &error)
    })?;
    let temporary = parent.join(format!(
        ".is_dark.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    let write = || -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        let value = if enabled { "true" } else { "false" };
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
        fs::rename(&temporary, path)?;
        Ok(())
    };
    write().map_err(|error| {
        let _ = fs::remove_file(&temporary);
        QuickToggleError::io("writing the COSMIC theme mode", &error)
    })
}

fn cosmic_settings_daemon_present() -> bool {
    let Ok(connection) =
        Builder::session().and_then(|builder| builder.method_timeout(COSMIC_BUS_TIMEOUT).build())
    else {
        return false;
    };
    let Ok(proxy) = DBusProxy::new(&connection) else {
        return false;
    };
    let Ok(name) = BusName::try_from(COSMIC_SETTINGS_DAEMON) else {
        return false;
    };
    proxy.name_has_owner(name).unwrap_or(false)
}

pub(crate) fn hidden_files_capability() -> CapabilityReport {
    boolean_capability(
        QuickToggleId::HiddenFiles,
        &HIDDEN_FILE_SETTINGS,
        "Hidden-file visibility can be changed for the selected file-manager backend.",
    )
}

pub(crate) fn observe_hidden_files() -> Result<QuickToggleObservation, QuickToggleError> {
    observe_boolean(&HIDDEN_FILE_SETTINGS, "Hidden files")
}

pub(crate) fn set_hidden_files(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    set_boolean(&HIDDEN_FILE_SETTINGS, mutation)
}

pub(crate) fn desktop_icons_capability() -> CapabilityReport {
    boolean_capability(
        QuickToggleId::DesktopIcons,
        &DESKTOP_ICON_SETTINGS,
        "Desktop-icon visibility can be changed for the selected desktop backend.",
    )
}

pub(crate) fn observe_desktop_icons() -> Result<QuickToggleObservation, QuickToggleError> {
    observe_boolean(&DESKTOP_ICON_SETTINGS, "Desktop icons")
}

pub(crate) fn set_desktop_icons(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
    set_boolean(&DESKTOP_ICON_SETTINGS, mutation)
}

fn boolean_capability(
    id: QuickToggleId,
    candidates: &[(&'static str, &'static str)],
    summary: &'static str,
) -> CapabilityReport {
    let (settings, schema, key) = match lookup_first(candidates) {
        Ok(target) => target,
        Err(error) => return unsupported(id, error.message),
    };
    if !settings.is_writable(key) {
        return permission_denied(
            id,
            Permission::DesktopSettings,
            format!("The {schema} setting is present but read-only."),
            "Allow the current user to change this desktop setting; Kestrel will not elevate itself.",
        );
    }
    supported(id, summary, format!("GSettings {schema}"))
        .with_evidence(CapabilityEvidence::new("settings-key", key))
}

fn observe_boolean(
    candidates: &[(&'static str, &'static str)],
    label: &str,
) -> Result<QuickToggleObservation, QuickToggleError> {
    let (settings, schema, key) = lookup_first(candidates)?;
    let enabled = settings.boolean(key);
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Switch {
            enabled,
            confirmation: None,
        },
        detail: format!(
            "{label} are {} through {schema}.",
            if enabled { "visible" } else { "hidden" }
        ),
    })
}

fn set_boolean(
    candidates: &[(&'static str, &'static str)],
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetEnabled(enabled) = mutation else {
        return Err(invalid_switch());
    };
    let (settings, schema, key) = lookup_first(candidates)?;
    settings.set_boolean(key, *enabled).map_err(|error| {
        QuickToggleError::new(
            QuickToggleErrorKind::PermissionDenied,
            format!("{schema} rejected the setting change: {error}"),
        )
    })
}

fn lookup_first(
    candidates: &[(&'static str, &'static str)],
) -> Result<(Settings, &'static str, &'static str), QuickToggleError> {
    for (schema, key) in candidates {
        if let Ok(settings) = lookup(schema, key) {
            return Ok((settings, *schema, *key));
        }
    }
    Err(QuickToggleError::unavailable(format!(
        "none of the supported settings keys are installed: {}",
        candidates
            .iter()
            .map(|(schema, key)| format!("{schema}:{key}"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn lookup(schema_id: &str, key: &str) -> Result<Settings, QuickToggleError> {
    let source = SettingsSchemaSource::default()
        .ok_or_else(|| QuickToggleError::unavailable("no GSettings schema source is available"))?;
    let schema = source.lookup(schema_id, true).ok_or_else(|| {
        QuickToggleError::unavailable(format!("the {schema_id} settings schema is not installed"))
    })?;
    if !schema.has_key(key) {
        return Err(QuickToggleError::unavailable(format!(
            "the {schema_id} schema does not expose {key}"
        )));
    }
    Ok(Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

fn unsupported(id: QuickToggleId, reason: String) -> CapabilityReport {
    CapabilityReport::new(
        id.feature_id(),
        CapabilityStatus::Unsupported {
            reason: reason.clone(),
        },
        format!("{} is unavailable for this desktop.", id.label()),
    )
    .with_remediation(reason)
}

fn invalid_switch() -> QuickToggleError {
    QuickToggleError::new(
        QuickToggleErrorKind::InvalidRequest,
        "this adapter accepts only an enabled/disabled value",
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use kestrel_core::CapabilityEvidence;
    use tempfile::TempDir;

    use super::{
        AppearanceBackend, COSMIC_MODE_DIRECTORY, COSMIC_MODE_KEY, CosmicPaths, ModeSource,
        choose_appearance_backend, cosmic_appearance_capability, observe_cosmic_appearance,
        read_cosmic_bool, set_cosmic_appearance,
    };
    use crate::quick_toggles::{
        QuickToggleControl, QuickToggleErrorKind, QuickToggleId, QuickToggleMutation,
    };

    fn paths<'a>(config_home: &'a Path, system_root: &'a Path) -> CosmicPaths<'a> {
        CosmicPaths {
            config_home,
            system_root,
        }
    }

    fn write_system(system_root: &Path, key: &str, value: &str) {
        let directory = system_root.join(COSMIC_MODE_DIRECTORY);
        fs::create_dir_all(&directory).expect("system COSMIC theme mode directory");
        fs::write(directory.join(key), value).expect("system COSMIC value");
    }

    fn write_user(config_home: &Path, key: &str, value: &str) {
        let directory = config_home.join("cosmic").join(COSMIC_MODE_DIRECTORY);
        fs::create_dir_all(&directory).expect("user COSMIC theme mode directory");
        fs::write(directory.join(key), value).expect("user COSMIC value");
    }

    #[test]
    fn appearance_backend_needs_cosmic_mode_and_daemon() {
        let cases = [
            (true, true, AppearanceBackend::Cosmic),
            (true, false, AppearanceBackend::Gnome),
            (false, true, AppearanceBackend::Gnome),
            (false, false, AppearanceBackend::Gnome),
        ];
        for (mode_known, daemon_present, expected) in cases {
            assert_eq!(
                choose_appearance_backend(mode_known, daemon_present),
                expected,
                "mode_known={mode_known} daemon_present={daemon_present}"
            );
        }
    }

    #[test]
    fn cosmic_observe_uses_the_system_default() {
        let directory = TempDir::new().expect("temporary roots");
        let config_home = directory.path().join("config");
        let system_root = directory.path().join("system");
        write_system(&system_root, COSMIC_MODE_KEY, "true");

        let observation = observe_cosmic_appearance(paths(&config_home, &system_root))
            .expect("COSMIC observation");

        assert_eq!(
            observation.control,
            QuickToggleControl::Switch {
                enabled: true,
                confirmation: None,
            }
        );
        assert_eq!(observation.detail, "COSMIC dark mode is on.");
    }

    #[test]
    fn cosmic_user_override_wins_over_the_system_default() {
        let directory = TempDir::new().expect("temporary roots");
        let config_home = directory.path().join("config");
        let system_root = directory.path().join("system");
        write_system(&system_root, COSMIC_MODE_KEY, "false");
        write_user(&config_home, COSMIC_MODE_KEY, "true");

        let (enabled, source) =
            read_cosmic_bool(paths(&config_home, &system_root), COSMIC_MODE_KEY)
                .expect("COSMIC mode");
        assert!(enabled);
        assert_eq!(source, ModeSource::User);

        let observation = observe_cosmic_appearance(paths(&config_home, &system_root))
            .expect("COSMIC observation");
        assert_eq!(
            observation.control,
            QuickToggleControl::Switch {
                enabled: true,
                confirmation: None,
            }
        );
    }

    #[test]
    fn cosmic_auto_switch_adds_a_note() {
        let directory = TempDir::new().expect("temporary roots");
        let config_home = directory.path().join("config");
        let system_root = directory.path().join("system");
        write_system(&system_root, COSMIC_MODE_KEY, "false");
        write_system(&system_root, "auto_switch", "true");

        let observation = observe_cosmic_appearance(paths(&config_home, &system_root))
            .expect("COSMIC observation");

        assert_eq!(
            observation.detail,
            "COSMIC dark mode is off. COSMIC switches between light and dark automatically, so it may change this back."
        );
    }

    #[test]
    fn cosmic_non_boolean_mode_is_an_error() {
        let directory = TempDir::new().expect("temporary roots");
        let config_home = directory.path().join("config");
        let system_root = directory.path().join("system");
        write_system(&system_root, COSMIC_MODE_KEY, "maybe");

        let error = observe_cosmic_appearance(paths(&config_home, &system_root))
            .expect_err("malformed COSMIC mode");

        assert_eq!(error.kind, QuickToggleErrorKind::Protocol);
        assert!(error.message.contains("not a boolean"), "{}", error.message);
    }

    #[test]
    fn cosmic_set_writes_the_user_mode_and_leaves_auto_switch_alone() {
        let directory = TempDir::new().expect("temporary roots");
        let config_home = directory.path().join("config");
        let system_root = directory.path().join("system");
        write_system(&system_root, COSMIC_MODE_KEY, "true");
        write_system(&system_root, "auto_switch", "true");
        let system_auto_switch = system_root.join(COSMIC_MODE_DIRECTORY).join("auto_switch");
        let system_auto_switch_before =
            fs::read_to_string(&system_auto_switch).expect("system auto_switch");

        set_cosmic_appearance(
            paths(&config_home, &system_root),
            &QuickToggleMutation::SetEnabled(false),
        )
        .expect("COSMIC mode writes");

        let user_mode = config_home
            .join("cosmic")
            .join(COSMIC_MODE_DIRECTORY)
            .join(COSMIC_MODE_KEY);
        assert_eq!(
            fs::read_to_string(&user_mode).expect("user COSMIC mode"),
            "false"
        );
        assert_eq!(
            fs::metadata(&user_mode)
                .expect("user COSMIC mode metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );

        let observation = observe_cosmic_appearance(paths(&config_home, &system_root))
            .expect("COSMIC observation");
        assert_eq!(
            observation.control,
            QuickToggleControl::Switch {
                enabled: false,
                confirmation: None,
            }
        );

        assert_eq!(
            fs::read_to_string(&system_auto_switch).expect("system auto_switch"),
            system_auto_switch_before
        );
        assert!(
            !config_home
                .join("cosmic")
                .join(COSMIC_MODE_DIRECTORY)
                .join("auto_switch")
                .exists(),
            "auto_switch is never written"
        );
    }

    #[test]
    fn cosmic_capability_names_the_daemon_and_mode_source() {
        let report = cosmic_appearance_capability(ModeSource::System);

        assert_eq!(report.feature_id, QuickToggleId::Appearance.feature_id());
        assert_eq!(
            report.selected_backend.as_deref(),
            Some("COSMIC theme mode (cosmic-config)")
        );
        assert_eq!(
            report.evidence,
            vec![
                CapabilityEvidence::new("settings-daemon", "com.system76.CosmicSettingsDaemon"),
                CapabilityEvidence::new("mode-source", "system default"),
            ]
        );
    }
}
