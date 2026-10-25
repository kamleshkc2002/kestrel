use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{Arc, Mutex, Once},
    thread,
};

use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator, Proxy},
    message::Type,
    zvariant::OwnedValue,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationError {
    pub message: String,
}

impl NotificationError {
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureNotificationAction {
    Open,
    Edit,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationEvent {
    ActionInvoked {
        notification_id: u32,
        action: CaptureNotificationAction,
    },
    NotificationClosed {
        notification_id: u32,
    },
}

pub fn parse_capture_action_key(key: &str) -> Option<CaptureNotificationAction> {
    match key {
        "default" => Some(CaptureNotificationAction::Open),
        "edit" => Some(CaptureNotificationAction::Edit),
        "copy" => Some(CaptureNotificationAction::Copy),
        _ => None,
    }
}

fn capture_actions(capabilities: &[String]) -> Vec<String> {
    if capabilities
        .iter()
        .any(|capability| capability == "actions")
    {
        vec![
            "default".to_owned(),
            "Open".to_owned(),
            "edit".to_owned(),
            "Edit".to_owned(),
            "copy".to_owned(),
            "Copy".to_owned(),
        ]
    } else {
        Vec::new()
    }
}

pub trait AlertNotifier: Send + Sync + 'static {
    fn notify(&self, notification: &AlertNotification) -> Result<(), NotificationError>;
}

/// `org.freedesktop.Notifications` over the session bus, using the blocking zbus API.
#[derive(Debug, Clone)]
pub struct DesktopNotifier {
    events: Arc<Mutex<VecDeque<NotificationEvent>>>,
    own_ids: Arc<Mutex<HashSet<u32>>>,
    listener_started: Arc<Once>,
    connection: Arc<Mutex<Option<Arc<Connection>>>>,
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopNotifier {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
            own_ids: Arc::new(Mutex::new(HashSet::new())),
            listener_started: Arc::new(Once::new()),
            connection: Arc::new(Mutex::new(None)),
        }
    }

    fn session_connection(&self) -> Result<Arc<Connection>, NotificationError> {
        let mut slot = self.connection.lock().map_err(|_| {
            NotificationError::unavailable("notification connection lock is unavailable")
        })?;
        if let Some(connection) = slot.as_ref() {
            return Ok(Arc::clone(connection));
        }
        let connection = Arc::new(Connection::session().map_err(|error| {
            NotificationError::unavailable(format!("connecting to the session bus failed: {error}"))
        })?);
        *slot = Some(Arc::clone(&connection));
        Ok(connection)
    }

    pub fn notify_capture(&self) -> Result<u32, NotificationError> {
        let connection = self.session_connection()?;
        self.start_listener(Arc::clone(&connection));
        let proxy = notification_proxy(&connection)?;
        let capabilities = proxy
            .call::<_, _, Vec<String>>("GetCapabilities", &())
            .unwrap_or_default();
        let actions = capture_actions(&capabilities);
        let hints: HashMap<String, OwnedValue> = HashMap::new();
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    kestrel_core::APPLICATION_NAME,
                    0u32,
                    kestrel_core::APPLICATION_ID,
                    "Screenshot saved",
                    "Added to recent captures.",
                    actions,
                    hints,
                    10_000i32,
                ),
            )
            .map_err(|error| {
                NotificationError::protocol(format!(
                    "sending screenshot notification failed: {error}"
                ))
            })?;
        if let Ok(mut ids) = self.own_ids.lock() {
            ids.insert(id);
        }
        Ok(id)
    }

    pub fn take_events(&self) -> Vec<NotificationEvent> {
        self.events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    fn start_listener(&self, connection: Arc<Connection>) {
        let events = Arc::clone(&self.events);
        let own_ids = Arc::clone(&self.own_ids);
        self.listener_started.call_once(move || {
            let _ = thread::Builder::new()
                .name("kestrel-notification-events".to_owned())
                .spawn(move || listen_for_events(connection, events, own_ids));
        });
    }
}

fn listen_for_events(
    connection: Arc<Connection>,
    events: Arc<Mutex<VecDeque<NotificationEvent>>>,
    own_ids: Arc<Mutex<HashSet<u32>>>,
) {
    let Ok(builder) = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.Notifications")
        .and_then(|builder| builder.interface("org.freedesktop.Notifications"))
        .and_then(|builder| builder.path("/org/freedesktop/Notifications"))
    else {
        return;
    };
    let rule = builder.build();
    let Ok(iterator) = MessageIterator::for_match_rule(rule, &connection, Some(64)) else {
        return;
    };
    for message in iterator {
        let Ok(message) = message else {
            continue;
        };
        let header = message.header();
        let Some(member) = header.member() else {
            continue;
        };
        let event =
            match member.as_str() {
                "ActionInvoked" => message.body().deserialize::<(u32, String)>().ok().and_then(
                    |(notification_id, key)| {
                        parse_capture_action_key(&key).map(|action| {
                            NotificationEvent::ActionInvoked {
                                notification_id,
                                action,
                            }
                        })
                    },
                ),
                "NotificationClosed" => message.body().deserialize::<(u32, u32)>().ok().map(
                    |(notification_id, _reason)| NotificationEvent::NotificationClosed {
                        notification_id,
                    },
                ),
                _ => None,
            };
        let Some(event) = event else {
            continue;
        };
        let notification_id = match event {
            NotificationEvent::ActionInvoked {
                notification_id, ..
            }
            | NotificationEvent::NotificationClosed { notification_id } => notification_id,
        };
        let owned = own_ids
            .lock()
            .map(|mut ids| {
                let owned = ids.contains(&notification_id);
                if matches!(event, NotificationEvent::NotificationClosed { .. }) {
                    ids.remove(&notification_id);
                }
                owned
            })
            .unwrap_or(false);
        if owned {
            if let Ok(mut queued) = events.lock() {
                queued.push_back(event);
            }
        }
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
                    kestrel_core::APPLICATION_NAME,
                    0u32,
                    kestrel_core::APPLICATION_ID,
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

#[cfg(test)]
mod tests {
    use super::{CaptureNotificationAction, capture_actions, parse_capture_action_key};

    #[test]
    fn notification_actions_parse_to_typed_follow_ups() {
        for (key, expected) in [
            ("default", Some(CaptureNotificationAction::Open)),
            ("edit", Some(CaptureNotificationAction::Edit)),
            ("copy", Some(CaptureNotificationAction::Copy)),
            ("launch", None),
        ] {
            assert_eq!(parse_capture_action_key(key), expected, "{key}");
        }
    }

    #[test]
    fn notification_buttons_require_actions_capability() {
        let actions = capture_actions(&["actions".to_owned()]);
        assert!(actions.contains(&"edit".to_owned()));
        assert!(actions.contains(&"copy".to_owned()));
        assert!(actions.contains(&"default".to_owned()));
        assert!(capture_actions(&["body-markup".to_owned()]).is_empty());
    }
}
