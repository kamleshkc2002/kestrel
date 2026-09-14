use gio::{Settings, SettingsSchemaSource, prelude::SettingsExt};
use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};

use super::{
    QuickToggleControl, QuickToggleError, QuickToggleErrorKind, QuickToggleId, QuickToggleMutation,
    QuickToggleObservation, permission_denied, supported,
};

const APPEARANCE_SCHEMA: &str = "org.gnome.desktop.interface";
const APPEARANCE_KEY: &str = "color-scheme";
const HIDDEN_FILE_SETTINGS: [(&str, &str); 2] = [
    ("org.gnome.nautilus.preferences", "show-hidden-files"),
    ("org.gtk.Settings.FileChooser", "show-hidden"),
];
const DESKTOP_ICON_SETTINGS: [(&str, &str); 3] = [
    ("org.gnome.desktop.background", "show-desktop-icons"),
    ("org.nemo.desktop", "show-desktop-icons"),
    ("org.mate.background", "show-desktop-icons"),
];

pub(crate) fn appearance_capability() -> CapabilityReport {
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

pub(crate) fn observe_appearance() -> Result<QuickToggleObservation, QuickToggleError> {
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

pub(crate) fn set_appearance(mutation: &QuickToggleMutation) -> Result<(), QuickToggleError> {
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
