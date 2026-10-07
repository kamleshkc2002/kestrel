//! `org.freedesktop.portal.GlobalShortcuts` backend.

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use zbus::MatchRule;
use zbus::blocking::{
    Connection, MessageIterator, Proxy, connection::Builder as ConnectionBuilder,
};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

use super::{
    ActivationSink, BindingState, SharedShortcutStatus, ShortcutBackend, ShortcutError,
    ShortcutErrorKind, ShortcutPhase, ShortcutProvider, ShortcutRequest, update_status,
};

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const GLOBAL_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
const REGISTRY_BUS: &str = "org.freedesktop.host.portal.Registry";
const REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";
const RESPONSE_MEMBER: &str = "Response";
const ACTIVATED_MEMBER: &str = "Activated";
const CLOSED_MEMBER: &str = "Closed";

type ResponsePayload = (u32, HashMap<String, OwnedValue>);

struct WorkerControl {
    connection_slot: Arc<Mutex<Option<Connection>>>,
    session_slot: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseResult {
    Accepted,
    Cancelled,
    Denied,
    Protocol,
}

fn map_response_code(code: u32) -> ResponseResult {
    match code {
        0 => ResponseResult::Accepted,
        1 => ResponseResult::Cancelled,
        2 => ResponseResult::Denied,
        _ => ResponseResult::Protocol,
    }
}

/// The portal spec: drop the leading ':' of the unique name, then '.' → '_'.
fn request_path(sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

fn owned_string(value: &str) -> OwnedValue {
    OwnedValue::from(Str::from(value))
}

fn options(
    entries: impl IntoIterator<Item = (&'static str, String)>,
) -> HashMap<String, OwnedValue> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), owned_string(&value)))
        .collect()
}

fn shortcut_payload(requests: &[ShortcutRequest]) -> Vec<(String, HashMap<String, OwnedValue>)> {
    requests
        .iter()
        .map(|request| {
            (
                request.id.clone(),
                options([
                    ("description", request.description.clone()),
                    ("preferred_trigger", request.trigger.to_string()),
                ]),
            )
        })
        .collect()
}

fn signal_iterator(connection: &Connection) -> Result<MessageIterator, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(PORTAL_BUS)?
        .path_namespace(PORTAL_PATH)?
        .build();
    MessageIterator::for_match_rule(rule, connection, Some(32))
}

fn portal_proxy<'a>(connection: &'a Connection) -> Result<Proxy<'a>, zbus::Error> {
    Proxy::new(connection, PORTAL_BUS, PORTAL_PATH, GLOBAL_INTERFACE)
}

fn session_proxy<'a>(connection: &'a Connection, path: &'a str) -> Result<Proxy<'a>, zbus::Error> {
    Proxy::new(connection, PORTAL_BUS, path, SESSION_INTERFACE)
}

fn fail(status: &SharedShortcutStatus, kind: ShortcutErrorKind, message: impl Into<String>) {
    let error = ShortcutError::new(kind, message);
    let reason = error.message.clone();
    update_status(status, |current| {
        current.phase = ShortcutPhase::Failed(error);
        for binding in &mut current.bindings {
            binding.state = BindingState::Rejected {
                reason: reason.clone(),
            };
        }
    });
}

fn response_error(result: ResponseResult) -> (ShortcutErrorKind, &'static str) {
    match result {
        ResponseResult::Cancelled => (ShortcutErrorKind::Cancelled, "portal request cancelled"),
        ResponseResult::Denied => (ShortcutErrorKind::Denied, "portal request denied"),
        ResponseResult::Protocol => (
            ShortcutErrorKind::Protocol,
            "portal returned an unknown response",
        ),
        ResponseResult::Accepted => (
            ShortcutErrorKind::Protocol,
            "portal request unexpectedly succeeded",
        ),
    }
}

fn parse_response(message: &zbus::Message) -> Result<(u32, HashMap<String, OwnedValue>), String> {
    message
        .body()
        .deserialize()
        .map_err(|error| format!("decoding portal response failed: {error}"))
}

fn is_signal(message: &zbus::Message, interface: &str, member: &str) -> bool {
    message.message_type() == Type::Signal
        && message
            .header()
            .interface()
            .is_some_and(|value| value.as_str() == interface)
        && message
            .header()
            .member()
            .is_some_and(|value| value.as_str() == member)
}

fn is_path(message: &zbus::Message, path: &str) -> bool {
    message
        .header()
        .path()
        .is_some_and(|value| value.as_str() == path)
}

/// Waits for `Request.Response` on the predicted path or the handle the call
/// returned; older portals return a handle that differs from the prediction.
fn wait_for_response(
    iterator: &mut MessageIterator,
    requests: [&str; 2],
    session: Option<&str>,
    sink: &ActivationSink,
    status: &SharedShortcutStatus,
    stop: &AtomicBool,
) -> Result<Option<ResponsePayload>, String> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some(message) = iterator.next() else {
            return Ok(None);
        };
        let message =
            message.map_err(|error| format!("receiving portal signal failed: {error}"))?;
        if is_signal(&message, REQUEST_INTERFACE, RESPONSE_MEMBER)
            && requests.iter().any(|request| is_path(&message, request))
        {
            return parse_response(&message).map(Some);
        }
        if let Some(session) = session {
            if is_signal(&message, SESSION_INTERFACE, CLOSED_MEMBER) && is_path(&message, session) {
                update_status(status, |current| current.phase = ShortcutPhase::Stopped);
                return Ok(None);
            }
            if is_signal(&message, GLOBAL_INTERFACE, ACTIVATED_MEMBER)
                && is_path(&message, PORTAL_PATH)
            {
                if let Ok((active_session, shortcut_id, _timestamp, _options)) = message
                    .body()
                    .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
                {
                    if active_session.as_str() == session {
                        sink(&shortcut_id);
                    }
                }
            }
        }
    }
}

fn mark_bindings(
    status: &SharedShortcutStatus,
    requests: &[ShortcutRequest],
    result: &HashMap<String, OwnedValue>,
) -> Result<(), String> {
    let returned: Vec<(String, HashMap<String, OwnedValue>)> = result
        .get("shortcuts")
        .ok_or_else(|| "portal response omitted shortcuts".to_owned())
        .and_then(|value| {
            Vec::try_from(value.clone())
                .map_err(|error| format!("decoding bound shortcuts failed: {error}"))
        })?;
    let requested: HashSet<&str> = requests.iter().map(|request| request.id.as_str()).collect();
    let returned_ids: HashSet<&str> = returned.iter().map(|(id, _)| id.as_str()).collect();
    if returned_ids.iter().any(|id| !requested.contains(id)) {
        return Err("portal returned an unknown shortcut ID".to_owned());
    }
    update_status(status, |current| {
        for binding in &mut current.bindings {
            if let Some((_, values)) = returned.iter().find(|(id, _)| id == &binding.id) {
                let trigger = values
                    .get("trigger_description")
                    .and_then(|value| String::try_from(value.clone()).ok());
                binding.state = BindingState::Bound { trigger };
            } else {
                binding.state = BindingState::Rejected {
                    reason: "portal did not bind this shortcut".to_owned(),
                };
            }
        }
        current.phase = ShortcutPhase::Active;
    });
    Ok(())
}

fn connect(address: Option<&str>) -> Result<Connection, zbus::Error> {
    match address {
        Some(address) => ConnectionBuilder::address(address)?.build(),
        None => Connection::session(),
    }
}

fn run_worker(
    app_id: String,
    requests: Vec<ShortcutRequest>,
    sink: ActivationSink,
    status: SharedShortcutStatus,
    address: Option<String>,
    control: WorkerControl,
) {
    let WorkerControl {
        connection_slot,
        session_slot,
        stop,
    } = control;
    let connection = match connect(address.as_deref()) {
        Ok(connection) => connection,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Unavailable,
                format!("connecting to the session bus failed: {error}"),
            );
            return;
        }
    };
    if stop.load(Ordering::Acquire) {
        let _ = connection.close();
        return;
    }
    if let Ok(mut slot) = connection_slot.lock() {
        *slot = Some(connection.clone());
    }

    if let Ok(registry) = Proxy::new(&connection, REGISTRY_BUS, PORTAL_PATH, REGISTRY_INTERFACE) {
        let empty: HashMap<String, OwnedValue> = HashMap::new();
        let _: Result<(), _> = registry.call("Register", &(app_id.as_str(), empty));
    }

    let Some(sender) = connection.unique_name().map(ToString::to_string) else {
        fail(
            &status,
            ShortcutErrorKind::Protocol,
            "session bus did not provide a unique name",
        );
        let _ = connection.close();
        return;
    };
    let mut signals = match signal_iterator(&connection) {
        Ok(iterator) => iterator,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Protocol,
                format!("subscribing to portal signals failed: {error}"),
            );
            let _ = connection.close();
            return;
        }
    };

    let create_token = format!("kestrel_create_{}", token_suffix());
    let create_path = request_path(&sender, &create_token);
    let create_options = options([
        ("handle_token", create_token),
        (
            "session_handle_token",
            format!("kestrel_session_{}", token_suffix()),
        ),
    ]);
    let portal_connection = connection.clone();
    let portal = match portal_proxy(&portal_connection) {
        Ok(proxy) => proxy,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Protocol,
                format!("creating portal proxy failed: {error}"),
            );
            let _ = connection.close();
            return;
        }
    };
    let create_handle: OwnedObjectPath = match portal.call("CreateSession", &create_options) {
        Ok(handle) => handle,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Unavailable,
                format!("creating portal session failed: {error}"),
            );
            let _ = connection.close();
            return;
        }
    };
    let create_result = match wait_for_response(
        &mut signals,
        [&create_path, create_handle.as_str()],
        None,
        &sink,
        &status,
        &stop,
    ) {
        Ok(Some(response)) => response,
        Ok(None) => {
            let _ = connection.close();
            return;
        }
        Err(error) => {
            fail(&status, ShortcutErrorKind::Protocol, error);
            let _ = connection.close();
            return;
        }
    };
    let (code, create_values) = create_result;
    match map_response_code(code) {
        ResponseResult::Accepted => {}
        result => {
            let (kind, message) = response_error(result);
            fail(&status, kind, message);
            let _ = connection.close();
            return;
        }
    }
    let session_path = match create_values.get("session_handle") {
        Some(value) => match String::try_from(value.clone()) {
            Ok(path) => path,
            Err(error) => {
                fail(
                    &status,
                    ShortcutErrorKind::Protocol,
                    format!("decoding session handle failed: {error}"),
                );
                let _ = connection.close();
                return;
            }
        },
        None => {
            fail(
                &status,
                ShortcutErrorKind::Protocol,
                "portal response omitted session handle",
            );
            let _ = connection.close();
            return;
        }
    };
    if let Ok(mut slot) = session_slot.lock() {
        *slot = Some(session_path.clone());
    }
    if stop.load(Ordering::Acquire) {
        let _ = connection.close();
        return;
    }

    let bind_token = format!("kestrel_bind_{}", token_suffix());
    let bind_path = request_path(&sender, &bind_token);
    let bind_options = options([("handle_token", bind_token)]);
    let payload = shortcut_payload(&requests);
    let session_object_path = match OwnedObjectPath::try_from(session_path.as_str()) {
        Ok(path) => path,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Protocol,
                format!("portal returned an invalid session path: {error}"),
            );
            let _ = connection.close();
            return;
        }
    };
    let bind_handle: OwnedObjectPath = match portal.call(
        "BindShortcuts",
        &(&session_object_path, payload, "", bind_options),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            fail(
                &status,
                ShortcutErrorKind::Protocol,
                format!("binding portal shortcuts failed: {error}"),
            );
            let _ = connection.close();
            return;
        }
    };
    let bind_result = match wait_for_response(
        &mut signals,
        [&bind_path, bind_handle.as_str()],
        Some(&session_path),
        &sink,
        &status,
        &stop,
    ) {
        Ok(Some(response)) => response,
        Ok(None) => {
            let _ = connection.close();
            return;
        }
        Err(error) => {
            fail(&status, ShortcutErrorKind::Protocol, error);
            let _ = connection.close();
            return;
        }
    };
    match map_response_code(bind_result.0) {
        ResponseResult::Accepted => {
            if let Err(error) = mark_bindings(&status, &requests, &bind_result.1) {
                fail(&status, ShortcutErrorKind::Protocol, error);
                let _ = connection.close();
                return;
            }
        }
        result => {
            let (kind, message) = response_error(result);
            fail(&status, kind, message);
            let _ = connection.close();
            return;
        }
    }

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let Some(message) = signals.next() else {
            break;
        };
        let Ok(message) = message else {
            break;
        };
        if is_signal(&message, SESSION_INTERFACE, CLOSED_MEMBER) && is_path(&message, &session_path)
        {
            update_status(&status, |current| current.phase = ShortcutPhase::Stopped);
            break;
        }
        if is_signal(&message, GLOBAL_INTERFACE, ACTIVATED_MEMBER) && is_path(&message, PORTAL_PATH)
        {
            if let Ok((active_session, shortcut_id, _timestamp, _options)) = message
                .body()
                .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
            {
                if active_session.as_str() == session_path {
                    sink(&shortcut_id);
                }
            }
        }
    }
    let _ = session_proxy(&connection, &session_path)
        .and_then(|proxy| proxy.call_noreply("Close", &()));
    let _ = connection.close();
}

fn token_suffix() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

pub struct PortalShortcutBackend {
    app_id: String,
    address: Option<String>,
    worker: Option<JoinHandle<()>>,
    connection: Arc<Mutex<Option<Connection>>>,
    session: Arc<Mutex<Option<String>>>,
    stop: Option<Arc<AtomicBool>>,
    status: Option<SharedShortcutStatus>,
}

impl PortalShortcutBackend {
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            address: None,
            worker: None,
            connection: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(None)),
            stop: None,
            status: None,
        }
    }

    #[cfg(test)]
    fn with_bus_address(app_id: &str, address: &str) -> Self {
        let mut backend = Self::new(app_id);
        backend.address = Some(address.to_owned());
        backend
    }
}

impl Drop for PortalShortcutBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ShortcutBackend for PortalShortcutBackend {
    fn provider(&self) -> ShortcutProvider {
        ShortcutProvider::Portal
    }

    fn start(
        &mut self,
        requests: Vec<ShortcutRequest>,
        sink: ActivationSink,
        status: SharedShortcutStatus,
    ) -> Result<(), ShortcutError> {
        if self.worker.is_some() {
            return Err(ShortcutError::new(
                ShortcutErrorKind::WorkerUnavailable,
                "portal shortcut backend is already running",
            ));
        }
        update_status(&status, |current| {
            current.provider = Some(ShortcutProvider::Portal);
            current.phase = ShortcutPhase::Starting;
            current.bindings = requests
                .iter()
                .map(|request| super::BindingStatus {
                    id: request.id.clone(),
                    requested: request.trigger.to_string(),
                    state: BindingState::Pending,
                })
                .collect();
        });
        let stop = Arc::new(AtomicBool::new(false));
        let connection = Arc::new(Mutex::new(None));
        let session = Arc::new(Mutex::new(None));
        let worker_control = WorkerControl {
            connection_slot: Arc::clone(&connection),
            session_slot: Arc::clone(&session),
            stop: Arc::clone(&stop),
        };
        let app_id = self.app_id.clone();
        let address = self.address.clone();
        let worker_status = Arc::clone(&status);
        let worker = thread::Builder::new()
            .name("kestrel-portal-shortcuts".to_owned())
            .spawn(move || {
                run_worker(
                    app_id,
                    requests,
                    sink,
                    worker_status,
                    address,
                    worker_control,
                )
            })
            .map_err(|error| {
                ShortcutError::new(
                    ShortcutErrorKind::WorkerUnavailable,
                    format!("spawning portal worker failed: {error}"),
                )
            })?;
        self.connection = connection;
        self.session = session;
        self.stop = Some(stop);
        self.status = Some(status);
        self.worker = Some(worker);
        Ok(())
    }

    fn stop(&mut self) {
        let Some(stop) = self.stop.take() else {
            if let Some(status) = &self.status {
                update_status(status, |current| current.phase = ShortcutPhase::Stopped);
            }
            return;
        };
        stop.store(true, Ordering::Release);
        let connection = self.connection.lock().ok().and_then(|slot| slot.clone());
        if let Some(connection) = connection {
            if let Some(session) = self.session.lock().ok().and_then(|slot| slot.clone()) {
                let _ = session_proxy(&connection, &session)
                    .and_then(|proxy| proxy.call_noreply("Close", &()));
            }
            let _ = connection.close();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(status) = &self.status {
            update_status(status, |current| current.phase = ShortcutPhase::Stopped);
        }
        self.status = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::ShortcutTrigger;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use zbus::blocking::connection::Builder as ConnectionBuilder;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::Value;

    fn request() -> ShortcutRequest {
        ShortcutRequest {
            id: "toggle".to_owned(),
            description: "Toggle panel".to_owned(),
            trigger: ShortcutTrigger::parse("CTRL+ALT+m").expect("valid trigger"),
        }
    }

    #[derive(Clone)]
    struct FakePortal {
        cancel_bind: Arc<AtomicBool>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
    impl FakePortal {
        fn create_session(
            &self,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let token = String::try_from(
                options
                    .get("handle_token")
                    .ok_or_else(|| zbus::fdo::Error::Failed("missing handle token".into()))?
                    .clone(),
            )
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let sender = header
                .sender()
                .ok_or_else(|| zbus::fdo::Error::Failed("missing sender".into()))?;
            let path = request_path(sender.as_str(), &token);
            let session = "/org/freedesktop/portal/session/fake";
            let mut results = HashMap::new();
            results.insert("session_handle".to_owned(), owned_string(session));
            let request_path = OwnedObjectPath::try_from(path.as_str())
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let response_emitter = SignalEmitter::new(connection, path)
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            zbus::block_on(response_emitter.emit(
                REQUEST_INTERFACE,
                RESPONSE_MEMBER,
                &(0u32, results),
            ))
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let _ = emitter;
            Ok(request_path)
        }

        #[allow(clippy::too_many_arguments)]
        fn bind_shortcuts(
            &self,
            session: OwnedObjectPath,
            shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
            _parent_window: String,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let token = String::try_from(
                options
                    .get("handle_token")
                    .ok_or_else(|| zbus::fdo::Error::Failed("missing handle token".into()))?
                    .clone(),
            )
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let sender = header
                .sender()
                .ok_or_else(|| zbus::fdo::Error::Failed("missing sender".into()))?;
            let path = request_path(sender.as_str(), &token);
            let request_path = OwnedObjectPath::try_from(path.as_str())
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let response_emitter = SignalEmitter::new(connection, path)
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let code: u32 = if self.cancel_bind.load(Ordering::Acquire) {
                1
            } else {
                0
            };
            let returned = if code == 0 {
                shortcuts
                    .into_iter()
                    .map(|(id, values)| {
                        let description = values
                            .get("description")
                            .cloned()
                            .unwrap_or_else(|| owned_string(""));
                        let mut result = HashMap::new();
                        result.insert("description".to_owned(), description);
                        result.insert(
                            "trigger_description".to_owned(),
                            owned_string("Fake trigger"),
                        );
                        (id, result)
                    })
                    .collect()
            } else {
                Vec::<(String, HashMap<String, OwnedValue>)>::new()
            };
            let returned_value = Value::new(returned)
                .try_into_owned()
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            let mut results = HashMap::new();
            results.insert("shortcuts".to_owned(), returned_value);
            zbus::block_on(response_emitter.emit(
                REQUEST_INTERFACE,
                RESPONSE_MEMBER,
                &(code, results),
            ))
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            if code == 0 {
                let activated = (
                    session,
                    "toggle".to_owned(),
                    1u64,
                    HashMap::<String, OwnedValue>::new(),
                );
                zbus::block_on(emitter.emit(GLOBAL_INTERFACE, ACTIVATED_MEMBER, &activated))
                    .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            }
            Ok(request_path)
        }

        fn close(&self) {}
    }

    struct FakeBus {
        address: String,
        stop: Arc<AtomicBool>,
        daemon: std::process::Child,
        thread: Option<JoinHandle<()>>,
    }

    impl FakeBus {
        fn start(cancel_bind: bool) -> Option<Self> {
            let mut daemon = Command::new("dbus-daemon")
                .args(["--session", "--print-address", "--nofork"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdout = daemon.stdout.take()?;
            let mut output = BufReader::new(stdout);
            let mut address = String::new();
            output.read_line(&mut address).ok()?;
            let address = address.trim().to_owned();
            if address.is_empty() {
                return None;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let (ready_tx, ready_rx) = mpsc::channel();
            let worker_address = address.clone();
            let thread = thread::Builder::new()
                .name("kestrel-test-portal".to_owned())
                .spawn(move || {
                    let connection = match ConnectionBuilder::address(worker_address.as_str())
                        .and_then(|builder| builder.build())
                    {
                        Ok(connection) => connection,
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    if let Err(error) = connection.request_name(PORTAL_BUS) {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                    if let Err(error) = connection.object_server().at(
                        PORTAL_PATH,
                        FakePortal {
                            cancel_bind: Arc::new(AtomicBool::new(cancel_bind)),
                        },
                    ) {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                    let _ = ready_tx.send(Ok(()));
                    while !worker_stop.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    let _ = connection.close();
                })
                .ok()?;
            if ready_rx.recv_timeout(Duration::from_secs(2)).ok()?.is_err() {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                return None;
            }
            Some(Self {
                address,
                stop,
                daemon,
                thread: Some(thread),
            })
        }
    }

    impl Drop for FakeBus {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    #[test]
    fn predicts_portal_request_path_from_unique_sender() {
        assert_eq!(
            request_path(":1.42", "token"),
            "/org/freedesktop/portal/desktop/request/1_42/token"
        );
    }

    #[test]
    fn builds_shortcut_payload_with_description_and_trigger() {
        let payload = shortcut_payload(&[request()]);
        assert_eq!(payload.len(), 1);
        assert_eq!(payload[0].0, "toggle");
        assert_eq!(
            String::try_from(payload[0].1["description"].clone()).unwrap(),
            "Toggle panel"
        );
        assert_eq!(
            String::try_from(payload[0].1["preferred_trigger"].clone()).unwrap(),
            "CTRL+ALT+m"
        );
    }

    #[test]
    fn maps_portal_response_codes() {
        assert_eq!(map_response_code(0), ResponseResult::Accepted);
        assert_eq!(map_response_code(1), ResponseResult::Cancelled);
        assert_eq!(map_response_code(2), ResponseResult::Denied);
        assert_eq!(map_response_code(99), ResponseResult::Protocol);
    }

    fn wait_for_phase(status: &SharedShortcutStatus, timeout: Duration) -> ShortcutPhase {
        let deadline = Instant::now() + timeout;
        loop {
            let phase = crate::global_shortcuts::lock_status(status).phase.clone();
            if !matches!(phase, ShortcutPhase::Starting) || Instant::now() >= deadline {
                return phase;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn portal_binds_activates_and_stops_promptly() {
        let Some(bus) = FakeBus::start(false) else {
            return;
        };
        let mut backend = PortalShortcutBackend::with_bus_address("test.app", &bus.address);
        let status = Arc::new(Mutex::new(
            crate::global_shortcuts::ShortcutStatus::default(),
        ));
        let (activated_tx, activated_rx) = mpsc::channel();
        let sink: ActivationSink = Arc::new(move |id| {
            let _ = activated_tx.send(id.to_owned());
        });
        backend
            .start(vec![request()], sink, Arc::clone(&status))
            .unwrap();
        assert_eq!(
            wait_for_phase(&status, Duration::from_secs(2)),
            ShortcutPhase::Active
        );
        let binding = crate::global_shortcuts::lock_status(&status).bindings[0].clone();
        assert_eq!(
            binding.state,
            BindingState::Bound {
                trigger: Some("Fake trigger".to_owned())
            }
        );
        assert_eq!(
            activated_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "toggle"
        );
        let started = Instant::now();
        backend.stop();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            crate::global_shortcuts::lock_status(&status).phase,
            ShortcutPhase::Stopped
        );
    }

    #[test]
    fn portal_cancelled_bind_fails_all_bindings() {
        let Some(bus) = FakeBus::start(true) else {
            return;
        };
        let mut backend = PortalShortcutBackend::with_bus_address("test.app", &bus.address);
        let status = Arc::new(Mutex::new(
            crate::global_shortcuts::ShortcutStatus::default(),
        ));
        let sink: ActivationSink = Arc::new(|_| {});
        backend
            .start(vec![request()], sink, Arc::clone(&status))
            .unwrap();
        let phase = wait_for_phase(&status, Duration::from_secs(2));
        assert!(matches!(
            phase,
            ShortcutPhase::Failed(ShortcutError {
                kind: ShortcutErrorKind::Cancelled,
                ..
            })
        ));
        assert!(matches!(
            crate::global_shortcuts::lock_status(&status).bindings[0].state,
            BindingState::Rejected { .. }
        ));
        backend.stop();
    }

    #[test]
    fn accepts_test_bus_address() {
        let backend = PortalShortcutBackend::with_bus_address("test.app", "unix:path=/tmp/test");
        assert_eq!(backend.address.as_deref(), Some("unix:path=/tmp/test"));
    }
}
