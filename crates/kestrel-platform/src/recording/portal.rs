//! `org.freedesktop.portal.ScreenCast`: one dedicated bus connection per
//! session, kept open while recording and closed with the session.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Str};

use super::{
    ActiveRecording, CURSOR_EMBEDDED, CaptureSession, RecordingError, RecordingErrorKind,
    RecordingPlan, RecordingSource, SOURCE_MONITOR, SOURCE_WINDOW, VideoFrame, pipeline,
};
use crate::capture::CaptureCancel;
use crate::capture::portal::{
    Failure, INTERACTIVE_TIMEOUT, PORTAL_BUS, PORTAL_PATH, Response, close_request, connect,
    register_application, request_path, send_request, token_suffix,
};

const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `AvailableCursorModes` bit for a hidden cursor.
const CURSOR_HIDDEN: u32 = 1;
/// How long a still frame may take to arrive.
const STILL_TIMEOUT: Duration = Duration::from_secs(5);

pub struct PortalScreenCast {
    address: Option<String>,
}

impl PortalScreenCast {
    pub fn new() -> Self {
        Self { address: None }
    }

    #[cfg(test)]
    fn with_bus_address(address: &str) -> Self {
        Self {
            address: Some(address.to_owned()),
        }
    }

    /// Creates a session, lets the user pick a source in the desktop's dialog,
    /// and opens its PipeWire remote. Blocks until answered or cancelled.
    pub fn open(
        &self,
        source: RecordingSource,
        cursor: bool,
        cancel: &CaptureCancel,
    ) -> Result<ScreenCastSession, RecordingError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let connection =
            connect(self.address.as_deref(), INTERACTIVE_TIMEOUT).map_err(|error| {
                RecordingError::new(
                    RecordingErrorKind::Unavailable,
                    format!("Connecting to the session bus failed: {error}"),
                )
            })?;
        let abort = connection.clone();
        cancel.set_abort(Box::new(move || {
            let _ = abort.close();
        }));
        let mut session = None;
        let opened = negotiate(&connection, source, cursor, &mut session);
        cancel.clear_abort();
        match opened {
            Ok((node, remote, closed, watcher)) => Ok(ScreenCastSession {
                connection,
                session: session.unwrap_or_default(),
                node,
                remote: Some(remote),
                closed,
                watcher: Some(watcher),
            }),
            Err(error) => {
                if let Some(session) = session {
                    close_session(&connection, &session);
                }
                let _ = connection.close();
                Err(if cancel.is_cancelled() {
                    cancelled()
                } else {
                    error
                })
            }
        }
    }
}

impl Default for PortalScreenCast {
    fn default() -> Self {
        Self::new()
    }
}

type Negotiated = (u32, OwnedFd, Arc<AtomicBool>, JoinHandle<()>);

/// CreateSession → SelectSources → Start → OpenPipeWireRemote. `session` is
/// set as soon as one exists, so the caller can close it on failure.
fn negotiate(
    connection: &Connection,
    source: RecordingSource,
    cursor: bool,
    session: &mut Option<String>,
) -> Result<Negotiated, RecordingError> {
    let sender = connection
        .unique_name()
        .map(ToString::to_string)
        .ok_or_else(|| failed("The session bus assigned no unique name."))?;
    register_application(connection);
    let portal = Proxy::new(connection, PORTAL_BUS, PORTAL_PATH, SCREENCAST_INTERFACE)
        .map_err(|error| failed(format!("Creating the ScreenCast proxy failed: {error}")))?;
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let version: u32 = portal.get_property("version").unwrap_or(1);
    let cursor_modes: u32 = portal.get_property("AvailableCursorModes").unwrap_or(0);

    let session_token = format!("kestrel_recording_{}", token_suffix());
    let created = request(
        connection,
        &sender,
        deadline,
        vec![(
            "session_handle_token",
            OwnedValue::from(Str::from(session_token)),
        )],
        |options| portal.call("CreateSession", &(options,)),
    )?;
    let handle = created
        .get("session_handle")
        .and_then(|value| {
            String::try_from(value.clone()).ok().or_else(|| {
                OwnedObjectPath::try_from(value.clone())
                    .ok()
                    .map(|path| path.to_string())
            })
        })
        .ok_or_else(|| failed("The portal created no session."))?;
    *session = Some(handle.clone());
    let path = ObjectPath::try_from(handle.as_str())
        .map_err(|_| failed("The portal returned an invalid session handle."))?;
    // Watched before Start, so a desktop that ends sharing at once is seen.
    let closed = Arc::new(AtomicBool::new(false));
    let watcher = watch_closed(connection, &handle, Arc::clone(&closed))?;

    let types = match source {
        RecordingSource::Window => SOURCE_WINDOW,
        RecordingSource::Screen | RecordingSource::Area => SOURCE_MONITOR,
    };
    let cursor_mode = if cursor && cursor_modes & CURSOR_EMBEDDED != 0 {
        CURSOR_EMBEDDED
    } else {
        CURSOR_HIDDEN
    };
    let mut select = vec![
        ("types", OwnedValue::from(types)),
        ("multiple", OwnedValue::from(false)),
    ];
    if cursor_modes != 0 {
        select.push(("cursor_mode", OwnedValue::from(cursor_mode)));
    }
    if version >= 4 {
        // Every recording asks again; nothing is remembered.
        select.push(("persist_mode", OwnedValue::from(0_u32)));
    }
    request(connection, &sender, deadline, select, |options| {
        portal.call("SelectSources", &(&path, options))
    })?;
    let started = request(connection, &sender, deadline, Vec::new(), |options| {
        portal.call("Start", &(&path, "", options))
    })?;
    let node = started
        .get("streams")
        .and_then(|value| Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(value.clone()).ok())
        .and_then(|streams| streams.first().map(|(node, _)| *node))
        .ok_or_else(|| failed("The portal shared no stream."))?;
    let remote = open_remote(connection, &handle)?;
    Ok((node, remote, closed, watcher))
}

/// A new PipeWire connection for the session. Each pipeline gets its own:
/// two clients cannot share one connection, even one after the other.
fn open_remote(connection: &Connection, session: &str) -> Result<OwnedFd, RecordingError> {
    let unavailable = |error: zbus::Error| {
        RecordingError::new(
            RecordingErrorKind::Unavailable,
            format!("Opening the PipeWire stream failed: {error}"),
        )
    };
    let path = ObjectPath::try_from(session)
        .map_err(|_| failed("The portal returned an invalid session handle."))?;
    let remote: zbus::zvariant::OwnedFd =
        Proxy::new(connection, PORTAL_BUS, PORTAL_PATH, SCREENCAST_INTERFACE)
            .map_err(unavailable)?
            .call(
                "OpenPipeWireRemote",
                &(&path, HashMap::<String, OwnedValue>::new()),
            )
            .map_err(unavailable)?;
    Ok(remote.into())
}

/// Sends one request with a fresh handle token and maps its response code.
fn request(
    connection: &Connection,
    sender: &str,
    deadline: Instant,
    extra: Vec<(&str, OwnedValue)>,
    call: impl FnOnce(HashMap<String, OwnedValue>) -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, RecordingError> {
    let token = format!("kestrel_recording_{}", token_suffix());
    let request = request_path(sender, &token);
    let mut options: HashMap<String, OwnedValue> = extra
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    options.insert(
        "handle_token".to_owned(),
        OwnedValue::from(Str::from(token)),
    );
    let response: Result<Response, Failure> =
        send_request(connection, &request, deadline, |_| {}, || call(options));
    match response {
        Ok((0, results)) => Ok(results),
        Ok((1, _)) => Err(cancelled()),
        Ok(_) => Err(RecordingError::new(
            RecordingErrorKind::Denied,
            "The desktop declined to share the screen.",
        )),
        Err(Failure::Timeout) => {
            close_request(connection, &request);
            Err(RecordingError::new(
                RecordingErrorKind::Timeout,
                "The desktop did not answer the sharing request in time.",
            ))
        }
        Err(Failure::Error(error)) => Err(failed(error.message)),
    }
}

/// Sets `closed` once the desktop emits `Closed` for the session; ends when
/// the connection closes.
fn watch_closed(
    connection: &Connection,
    session: &str,
    closed: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, RecordingError> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(SESSION_INTERFACE)
        .and_then(|rule| rule.member("Closed"))
        .and_then(|rule| rule.path(session.to_owned()))
        .map(|rule| rule.build())
        .map_err(|error| failed(format!("Watching the session failed: {error}")))?;
    let signals = MessageIterator::for_match_rule(rule, connection, Some(4))
        .map_err(|error| failed(format!("Watching the session failed: {error}")))?;
    thread::Builder::new()
        .name("kestrel-recording-session".to_owned())
        .spawn(move || {
            for message in signals {
                if message.is_ok() {
                    closed.store(true, Ordering::Release);
                    break;
                }
            }
        })
        .map_err(|_| failed("Starting the session watcher failed."))
}

fn close_session(connection: &Connection, session: &str) {
    if let Ok(proxy) = Proxy::new(connection, PORTAL_BUS, session, SESSION_INTERFACE) {
        let _: Result<(), _> = proxy.call("Close", &());
    }
}

fn cancelled() -> RecordingError {
    RecordingError::new(
        RecordingErrorKind::Cancelled,
        "Screen sharing was cancelled.",
    )
}

fn failed(message: impl Into<String>) -> RecordingError {
    RecordingError::new(RecordingErrorKind::Failed, message)
}

/// A consented ScreenCast session. Dropping it closes the session and the
/// connection.
pub struct ScreenCastSession {
    connection: Connection,
    session: String,
    node: u32,
    /// The remote opened with the session, until a pipeline takes it.
    remote: Option<OwnedFd>,
    closed: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

impl ScreenCastSession {
    /// The shared stream's PipeWire node.
    pub fn node(&self) -> u32 {
        self.node
    }

    /// An unused PipeWire remote for the next pipeline.
    fn fresh_remote(&mut self) -> Result<OwnedFd, RecordingError> {
        match self.remote.take() {
            Some(remote) => Ok(remote),
            None => open_remote(&self.connection, &self.session),
        }
    }
}

impl CaptureSession for ScreenCastSession {
    fn still(&mut self) -> Result<VideoFrame, RecordingError> {
        let remote = self.fresh_remote()?;
        pipeline::grab_still(
            pipeline::VideoInput::PipeWire {
                fd: remote.as_raw_fd(),
                node: self.node,
            },
            STILL_TIMEOUT,
        )
    }

    fn record(&mut self, plan: &RecordingPlan) -> Result<Box<dyn ActiveRecording>, RecordingError> {
        let remote = self.fresh_remote()?;
        let recording = pipeline::GstRecording::start(
            pipeline::VideoInput::PipeWire {
                fd: remote.as_raw_fd(),
                node: self.node,
            },
            plan,
        )?;
        Ok(Box::new(recording.owning(remote)))
    }

    fn ended_by_desktop(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl Drop for ScreenCastSession {
    fn drop(&mut self) {
        if !self.ended_by_desktop() {
            close_session(&self.connection, &self.session);
        }
        let _ = self.connection.clone().close();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::object_server::SignalEmitter;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum StartReply {
        Share,
        Code(u32),
        /// Shares, then ends the session from the desktop side.
        ShareThenClose,
    }

    #[derive(Default)]
    struct Observed {
        types: Option<u32>,
        cursor_mode: Option<u32>,
        persist_mode: Option<u32>,
        closed_sessions: Vec<String>,
        remotes_opened: usize,
    }

    struct FakeScreenCast {
        reply: StartReply,
        observed: Arc<Mutex<Observed>>,
    }

    struct FakeSession {
        observed: Arc<Mutex<Observed>>,
    }

    fn fdo(error: impl std::fmt::Display) -> zbus::fdo::Error {
        zbus::fdo::Error::Failed(error.to_string())
    }

    fn uint(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
        options
            .get(key)
            .and_then(|value| u32::try_from(value.clone()).ok())
    }

    async fn respond(
        connection: &zbus::Connection,
        header: &zbus::message::Header<'_>,
        options: &HashMap<String, OwnedValue>,
        code: u32,
        results: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let token = options
            .get("handle_token")
            .and_then(|value| String::try_from(value.clone()).ok())
            .ok_or_else(|| fdo("missing handle token"))?;
        let sender = header.sender().ok_or_else(|| fdo("missing sender"))?;
        let path = request_path(sender.as_str(), &token);
        SignalEmitter::new(connection, path.clone())
            .map_err(fdo)?
            .emit(
                "org.freedesktop.portal.Request",
                "Response",
                &(code, results),
            )
            .await
            .map_err(fdo)?;
        OwnedObjectPath::try_from(path).map_err(fdo)
    }

    const SESSION: &str = "/org/freedesktop/portal/desktop/session/fake/one";

    #[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
    impl FakeScreenCast {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            5
        }

        #[zbus(property)]
        fn available_cursor_modes(&self) -> u32 {
            7
        }

        async fn create_session(
            &self,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            connection
                .object_server()
                .at(
                    SESSION,
                    FakeSession {
                        observed: Arc::clone(&self.observed),
                    },
                )
                .await
                .map_err(fdo)?;
            let results = HashMap::from([(
                "session_handle".to_owned(),
                OwnedValue::from(Str::from(SESSION)),
            )]);
            respond(connection, &header, &options, 0, results).await
        }

        async fn select_sources(
            &self,
            _session: OwnedObjectPath,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            {
                let mut observed = self.observed.lock().map_err(fdo)?;
                observed.types = uint(&options, "types");
                observed.cursor_mode = uint(&options, "cursor_mode");
                observed.persist_mode = uint(&options, "persist_mode");
            }
            respond(connection, &header, &options, 0, HashMap::new()).await
        }

        async fn start(
            &self,
            session: OwnedObjectPath,
            _parent_window: String,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let code = match self.reply {
                StartReply::Code(code) => code,
                StartReply::Share | StartReply::ShareThenClose => 0,
            };
            let streams = vec![(
                42_u32,
                HashMap::from([("source_type".to_owned(), OwnedValue::from(1_u32))]),
            )];
            let value = zbus::zvariant::Value::from(streams);
            let results = HashMap::from([(
                "streams".to_owned(),
                OwnedValue::try_from(value).map_err(fdo)?,
            )]);
            let handle = respond(connection, &header, &options, code, results).await?;
            if self.reply == StartReply::ShareThenClose {
                SignalEmitter::new(connection, session)
                    .map_err(fdo)?
                    .emit(
                        SESSION_INTERFACE,
                        "Closed",
                        &(HashMap::<String, OwnedValue>::new(),),
                    )
                    .await
                    .map_err(fdo)?;
            }
            Ok(handle)
        }

        fn open_pipe_wire_remote(
            &self,
            _session: OwnedObjectPath,
            _options: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
            self.observed.lock().map_err(fdo)?.remotes_opened += 1;
            let file = std::fs::File::open("/dev/null").map_err(fdo)?;
            Ok(OwnedFd::from(file).into())
        }
    }

    #[zbus::interface(name = "org.freedesktop.portal.Session")]
    impl FakeSession {
        fn close(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<()> {
            let path = header.path().map(ToString::to_string).unwrap_or_default();
            self.observed
                .lock()
                .map_err(fdo)?
                .closed_sessions
                .push(path);
            Ok(())
        }
    }

    fn fake(reply: StartReply) -> Option<(crate::test_bus::PrivateBus, Arc<Mutex<Observed>>)> {
        let observed = Arc::new(Mutex::new(Observed::default()));
        let portal = FakeScreenCast {
            reply,
            observed: Arc::clone(&observed),
        };
        let bus = crate::test_bus::PrivateBus::start(PORTAL_BUS, move |builder| {
            builder.serve_at(PORTAL_PATH, portal)
        })?;
        Some((bus, observed))
    }

    fn wait_for(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_shared_monitor_yields_its_node_and_closing_ends_the_session() {
        let Some((bus, observed)) = fake(StartReply::Share) else {
            return;
        };
        let mut session = PortalScreenCast::with_bus_address(&bus.address)
            .open(RecordingSource::Area, true, &CaptureCancel::new())
            .expect("sharing succeeds");
        assert_eq!(session.node(), 42);
        assert!(!session.ended_by_desktop());
        {
            let observed = observed.lock().expect("observed");
            assert_eq!(
                observed.types,
                Some(SOURCE_MONITOR),
                "Area shares a monitor"
            );
            assert_eq!(observed.cursor_mode, Some(CURSOR_EMBEDDED));
            assert_eq!(observed.persist_mode, Some(0), "consent is never persisted");
        }
        // The still and the recorder each get their own PipeWire connection.
        let first = session.fresh_remote().expect("first remote");
        let second = session.fresh_remote().expect("second remote");
        assert_ne!(first.as_raw_fd(), second.as_raw_fd());
        assert_eq!(observed.lock().expect("observed").remotes_opened, 2);
        drop(session);
        assert!(wait_for(|| observed
            .lock()
            .expect("observed")
            .closed_sessions
            .contains(&SESSION.to_owned())));
    }

    #[test]
    fn dismissal_and_refusal_are_distinct_outcomes() {
        for (code, kind) in [
            (1, RecordingErrorKind::Cancelled),
            (2, RecordingErrorKind::Denied),
        ] {
            let Some((bus, observed)) = fake(StartReply::Code(code)) else {
                return;
            };
            let error = PortalScreenCast::with_bus_address(&bus.address)
                .open(RecordingSource::Window, false, &CaptureCancel::new())
                .err()
                .expect("sharing fails");
            assert_eq!(error.kind, kind);
            let observed = observed.lock().expect("observed");
            assert_eq!(observed.types, Some(SOURCE_WINDOW));
            assert_eq!(observed.cursor_mode, Some(CURSOR_HIDDEN));
            assert_eq!(
                observed.closed_sessions,
                [SESSION],
                "failed sessions are closed"
            );
        }
    }

    #[test]
    fn the_desktop_ending_the_session_is_observed() {
        let Some((bus, _observed)) = fake(StartReply::ShareThenClose) else {
            return;
        };
        let session = PortalScreenCast::with_bus_address(&bus.address)
            .open(RecordingSource::Screen, false, &CaptureCancel::new())
            .expect("sharing succeeds");
        assert!(wait_for(|| session.ended_by_desktop()));
    }

    #[test]
    fn cancelling_before_the_request_opens_nothing() {
        let cancel = CaptureCancel::new();
        cancel.cancel();
        let error = PortalScreenCast::with_bus_address("unix:path=/nonexistent")
            .open(RecordingSource::Screen, false, &cancel)
            .err()
            .expect("cancelled");
        assert_eq!(error.kind, RecordingErrorKind::Cancelled);
    }
}
