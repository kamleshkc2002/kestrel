//! Classifies failed D-Bus calls by their structured error name.

use zbus::fdo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbusErrorKind {
    PermissionDenied,
    Unavailable,
    Failed,
}

const PERMISSION_DENIED_ERRORS: [&str; 3] = [
    "org.freedesktop.DBus.Error.AccessDenied",
    "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired",
    "org.freedesktop.PolicyKit1.Error.NotAuthorized",
];
const UNAVAILABLE_ERRORS: [&str; 2] = [
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
];

pub fn dbus_error_kind(error: &zbus::Error) -> DbusErrorKind {
    match error {
        zbus::Error::MethodError(name, _, _) => {
            let name = name.as_str();
            if PERMISSION_DENIED_ERRORS.contains(&name) {
                DbusErrorKind::PermissionDenied
            } else if UNAVAILABLE_ERRORS.contains(&name) {
                DbusErrorKind::Unavailable
            } else {
                DbusErrorKind::Failed
            }
        }
        zbus::Error::FDO(error) => match error.as_ref() {
            fdo::Error::AccessDenied(_) | fdo::Error::InteractiveAuthorizationRequired(_) => {
                DbusErrorKind::PermissionDenied
            }
            fdo::Error::ServiceUnknown(_) | fdo::Error::NameHasNoOwner(_) => {
                DbusErrorKind::Unavailable
            }
            _ => DbusErrorKind::Failed,
        },
        _ => DbusErrorKind::Failed,
    }
}
