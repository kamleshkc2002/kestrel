//! Locating the logind session that owns this desktop.
//!
//! Desktops that launch applications in their own systemd scope, such as
//! COSMIC, leave `GetSessionByPID` without a session, so the lookup tries the
//! environment, then logind's automatic session, then the user's display.

use std::{env, fmt};

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::OwnedObjectPath,
};

pub const LOGIN_DESTINATION: &str = "org.freedesktop.login1";
pub const LOGIN_PATH: &str = "/org/freedesktop/login1";
pub const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
const USER_INTERFACE: &str = "org.freedesktop.login1.User";

/// The D-Bus calls a session lookup needs.
pub trait LogindSessionLookup {
    fn get_session(&self, id: &str) -> Result<OwnedObjectPath, zbus::Error>;
    fn get_user_display(&self, uid: u32) -> Result<OwnedObjectPath, zbus::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLookupErrorKind {
    PermissionDenied,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLookupError {
    pub kind: SessionLookupErrorKind,
    pub message: String,
}

impl SessionLookupError {
    fn from_dbus(error: zbus::Error) -> Self {
        let detail = error.to_string();
        let kind = if detail.contains("AccessDenied")
            || detail.contains("NotAuthorized")
            || detail.contains("InteractiveAuthorizationRequired")
        {
            SessionLookupErrorKind::PermissionDenied
        } else if detail.contains("ServiceUnknown") || detail.contains("NameHasNoOwner") {
            SessionLookupErrorKind::Unavailable
        } else {
            SessionLookupErrorKind::Failed
        };
        Self {
            kind,
            message: format!("the logind session could not be found: {detail}"),
        }
    }
}

impl fmt::Display for SessionLookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionLookupError {}

/// The logind manager proxy used by the production lookup.
pub struct LogindManager<'a> {
    manager: Proxy<'a>,
}

impl<'a> LogindManager<'a> {
    pub fn new(connection: &'a Connection) -> Result<Self, zbus::Error> {
        Ok(Self {
            manager: Proxy::new(connection, LOGIN_DESTINATION, LOGIN_PATH, MANAGER_INTERFACE)?,
        })
    }
}

impl LogindSessionLookup for LogindManager<'_> {
    fn get_session(&self, id: &str) -> Result<OwnedObjectPath, zbus::Error> {
        self.manager.call("GetSession", &(id))
    }

    fn get_user_display(&self, uid: u32) -> Result<OwnedObjectPath, zbus::Error> {
        let user_path: OwnedObjectPath = self.manager.call("GetUser", &(uid))?;
        let user = Proxy::new(
            self.manager.connection(),
            LOGIN_DESTINATION,
            user_path.as_str(),
            USER_INTERFACE,
        )?;
        let (_id, display): (String, OwnedObjectPath) = user.get_property("Display")?;
        if display.as_str().is_empty() {
            return Err(zbus::Error::Failure(
                "the logind user reports no graphical session".to_owned(),
            ));
        }
        Ok(display)
    }
}

/// Resolves the session object path, trying each source in turn.
pub fn resolve_session(
    lookup: &impl LogindSessionLookup,
    session_id: Option<&str>,
    uid: u32,
) -> Result<OwnedObjectPath, SessionLookupError> {
    if let Some(id) = session_id.filter(|id| !id.trim().is_empty()) {
        if let Ok(path) = lookup.get_session(id) {
            return Ok(path);
        }
    }
    if let Ok(path) = lookup.get_session("auto") {
        return Ok(path);
    }
    lookup
        .get_user_display(uid)
        .map_err(SessionLookupError::from_dbus)
}

/// Resolves the current desktop's session path over an existing connection.
pub fn current_session_path(
    connection: &Connection,
) -> Result<OwnedObjectPath, SessionLookupError> {
    let manager = LogindManager::new(connection).map_err(SessionLookupError::from_dbus)?;
    let session_id = env::var("XDG_SESSION_ID").ok();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    resolve_session(&manager, session_id.as_deref(), uid)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct FakeLookup {
        sessions: Vec<(&'static str, Result<&'static str, &'static str>)>,
        display: Result<&'static str, &'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeLookup {
        fn new(
            sessions: Vec<(&'static str, Result<&'static str, &'static str>)>,
            display: Result<&'static str, &'static str>,
        ) -> Self {
            Self {
                sessions,
                display,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl LogindSessionLookup for FakeLookup {
        fn get_session(&self, id: &str) -> Result<OwnedObjectPath, zbus::Error> {
            self.calls.borrow_mut().push(format!("session:{id}"));
            match self.sessions.iter().find(|(name, _)| *name == id) {
                Some((_, Ok(path))) => Ok(OwnedObjectPath::try_from(*path).unwrap()),
                Some((_, Err(detail))) => Err(zbus::Error::Failure((*detail).to_owned())),
                None => Err(zbus::Error::Failure(format!("no session {id}"))),
            }
        }

        fn get_user_display(&self, uid: u32) -> Result<OwnedObjectPath, zbus::Error> {
            self.calls.borrow_mut().push(format!("user:{uid}"));
            match self.display {
                Ok(path) => Ok(OwnedObjectPath::try_from(path).unwrap()),
                Err(detail) => Err(zbus::Error::Failure(detail.to_owned())),
            }
        }
    }

    struct Case {
        session_id: Option<&'static str>,
        sessions: Vec<(&'static str, Result<&'static str, &'static str>)>,
        display: Result<&'static str, &'static str>,
        expected: Result<&'static str, &'static str>,
        calls: Vec<&'static str>,
    }

    fn case(
        session_id: Option<&'static str>,
        sessions: Vec<(&'static str, Result<&'static str, &'static str>)>,
        display: Result<&'static str, &'static str>,
        expected: Result<&'static str, &'static str>,
        calls: Vec<&'static str>,
    ) -> Case {
        Case {
            session_id,
            sessions,
            display,
            expected,
            calls,
        }
    }

    #[test]
    fn session_lookup_tries_env_then_auto_then_user_display() {
        let cases = [
            case(
                Some("3"),
                vec![("3", Ok("/org/freedesktop/login1/session/_33"))],
                Ok("/org/freedesktop/login1/session/_1000"),
                Ok("/org/freedesktop/login1/session/_33"),
                vec!["session:3"],
            ),
            case(
                Some("3"),
                vec![
                    ("3", Err("GetSession failed")),
                    ("auto", Ok("/org/freedesktop/login1/session/auto")),
                ],
                Ok("/org/freedesktop/login1/session/_1000"),
                Ok("/org/freedesktop/login1/session/auto"),
                vec!["session:3", "session:auto"],
            ),
            case(
                None,
                vec![("auto", Ok("/org/freedesktop/login1/session/auto"))],
                Ok("/org/freedesktop/login1/session/_1000"),
                Ok("/org/freedesktop/login1/session/auto"),
                vec!["session:auto"],
            ),
            case(
                None,
                vec![("auto", Err("no automatic session"))],
                Ok("/org/freedesktop/login1/session/_1000"),
                Ok("/org/freedesktop/login1/session/_1000"),
                vec!["session:auto", "user:1000"],
            ),
            case(
                Some("3"),
                vec![
                    ("3", Err("the session is gone")),
                    ("auto", Err("no automatic session")),
                ],
                Err("the logind user reports no graphical session"),
                Err("the logind user reports no graphical session"),
                vec!["session:3", "session:auto", "user:1000"],
            ),
        ];
        for (index, case) in cases.iter().enumerate() {
            let lookup = FakeLookup::new(case.sessions.clone(), case.display);
            let result = resolve_session(&lookup, case.session_id, 1000);
            match (&case.expected, &result) {
                (Ok(path), Ok(found)) => assert_eq!(
                    found.as_str(),
                    *path,
                    "case {index}: expected session path {path}"
                ),
                (Err(detail), Err(error)) => {
                    assert!(
                        error
                            .message
                            .contains("the logind session could not be found"),
                        "case {index}: {error}"
                    );
                    assert!(
                        error.message.contains(detail),
                        "case {index}: {error} does not name {detail}"
                    );
                }
                _ => panic!("case {index}: unexpected result {result:?}"),
            }
            assert_eq!(
                *lookup.calls.borrow(),
                case.calls,
                "case {index}: unexpected lookup calls"
            );
        }
    }

    #[test]
    fn empty_session_id_falls_through_to_auto() {
        let lookup = FakeLookup::new(
            vec![("auto", Ok("/org/freedesktop/login1/session/auto"))],
            Err("unused"),
        );
        let path = resolve_session(&lookup, Some("  "), 1000).unwrap();
        assert_eq!(path.as_str(), "/org/freedesktop/login1/session/auto");
        assert_eq!(*lookup.calls.borrow(), vec!["session:auto"]);
    }
}
