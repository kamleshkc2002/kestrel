use std::{collections::HashMap, fmt};

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::OwnedValue,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationErrorKind {
    Unavailable,
    Protocol,
    PermissionDenied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationError {
    pub kind: NotificationErrorKind,
    pub message: String,
}

impl NotificationError {
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: NotificationErrorKind::Unavailable,
            message: message.into(),
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: NotificationErrorKind::Protocol,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> NotificationErrorKind {
        self.kind
    }
}

impl fmt::Display for NotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NotificationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertNotification {
    pub summary: String,
    pub body: String,
}

pub trait AlertNotifier: Send + Sync + 'static {
    fn notify(&self, notification: &AlertNotification) -> Result<(), NotificationError>;
}

/// `org.freedesktop.Notifications` over the session bus, using the blocking zbus API.
#[derive(Debug, Default)]
pub struct DesktopNotifier;

impl DesktopNotifier {
    pub fn new() -> Self {
        Self
    }
}

fn notification_proxy(connection: &Connection) -> Result<Proxy<'_>, NotificationError> {
    Proxy::new(
        connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .map_err(|error| {
        NotificationError::protocol(format!("creating notification proxy failed: {error}"))
    })
}

pub(crate) fn capabilities() -> Result<Vec<String>, NotificationError> {
    let connection = Connection::session().map_err(|error| {
        NotificationError::unavailable(format!("connecting to the session bus failed: {error}"))
    })?;
    let proxy = notification_proxy(&connection)?;
    proxy.call("GetCapabilities", &()).map_err(|error| {
        let message = error.to_string();
        let kind = if message.contains("AccessDenied") {
            NotificationErrorKind::PermissionDenied
        } else {
            NotificationErrorKind::Unavailable
        };
        NotificationError {
            kind,
            message: format!("querying notification capabilities failed: {error}"),
        }
    })
}

impl AlertNotifier for DesktopNotifier {
    fn notify(&self, notification: &AlertNotification) -> Result<(), NotificationError> {
        let connection = Connection::session().map_err(|error| {
            NotificationError::unavailable(format!("connecting to the session bus failed: {error}"))
        })?;
        let proxy = notification_proxy(&connection)?;
        let actions: Vec<String> = Vec::new();
        let hints: HashMap<String, OwnedValue> = HashMap::new();
        let _: u32 = proxy
            .call(
                "Notify",
                &(
                    "Kestrel",
                    0u32,
                    "battery-caution-symbolic",
                    notification.summary.as_str(),
                    notification.body.as_str(),
                    actions,
                    hints,
                    10_000i32,
                ),
            )
            .map_err(|error| {
                NotificationError::protocol(format!("sending notification failed: {error}"))
            })?;
        Ok(())
    }
}
