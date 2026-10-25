//! `org.freedesktop.portal.Screenshot` backend: one dedicated bus connection
//! per capture, closed before `capture` returns.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy, connection::Builder};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

use super::{
    CaptureBackend, CaptureCancel, CaptureError, CaptureErrorKind, CaptureMode, CaptureProvider,
    CapturedImage, MAX_CAPTURE_PNG_BYTES, cancelled, too_large, validate_png,
};
use crate::global_shortcuts::APPLICATION_ID;

pub(crate) const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

pub(crate) fn portal_property(bus: &Connection, interface: &str, name: &str) -> Option<u32> {
    let reply = bus
        .call_method(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(interface, name),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}
const SCREENSHOT_INTERFACE: &str = "org.freedesktop.portal.Screenshot";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const RESPONSE_MEMBER: &str = "Response";
const REGISTRY_BUS: &str = "org.freedesktop.host.portal.Registry";
const REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";
pub(crate) const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SCREEN_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a late response is awaited after a timeout or cancel, so its file is removed.
const LATE_RESPONSE_GRACE: Duration = Duration::from_millis(200);

pub(crate) type Response = (u32, HashMap<String, OwnedValue>);

pub struct PortalCaptureBackend {
    address: Option<String>,
}

impl PortalCaptureBackend {
    pub fn new() -> Self {
        Self { address: None }
    }

    #[cfg(test)]
    fn with_bus_address(address: &str) -> Self {
        Self {
            address: Some(address.to_owned()),
        }
    }
}

/// The desktop's own color picker (`PickColor`), with its magnifier.
pub struct PortalColorPicker {
    address: Option<String>,
}

impl PortalColorPicker {
    pub fn new() -> Self {
        Self { address: None }
    }

    #[cfg(test)]
    fn with_bus_address(address: &str) -> Self {
        Self {
            address: Some(address.to_owned()),
        }
    }

    /// Blocks until the user picks a pixel, dismisses the picker, or `cancel` fires.
    pub fn pick(&self, cancel: &CaptureCancel) -> Result<[u8; 3], CaptureError> {
        let response = call_portal(
            self.address.as_deref(),
            "PickColor",
            HashMap::new(),
            INTERACTIVE_TIMEOUT,
            cancel,
            |_| {},
        );
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        color_from_response(response?)
    }
}

impl Default for PortalColorPicker {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn connect(address: Option<&str>, timeout: Duration) -> Result<Connection, zbus::Error> {
    let builder = match address {
        Some(address) => Builder::address(address)?,
        None => Builder::session()?,
    };
    builder.method_timeout(timeout).build()
}

/// Tells the host portal registry which app is asking; absent registries are fine.
pub(crate) fn register_application(connection: &Connection) {
    if let Ok(registry) = Proxy::new(connection, REGISTRY_BUS, PORTAL_PATH, REGISTRY_INTERFACE) {
        let empty: HashMap<String, OwnedValue> = HashMap::new();
        let _: Result<(), _> = registry.call("Register", &(APPLICATION_ID, empty));
    }
}

impl Default for PortalCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for PortalCaptureBackend {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Portal
    }

    fn modes(&self) -> Vec<CaptureMode> {
        vec![CaptureMode::Interactive, CaptureMode::Screen]
    }

    fn capture(
        &self,
        mode: CaptureMode,
        cancel: &CaptureCancel,
    ) -> Result<CapturedImage, CaptureError> {
        let (interactive, timeout) = match mode {
            CaptureMode::Interactive => (true, INTERACTIVE_TIMEOUT),
            CaptureMode::Screen => (false, SCREEN_TIMEOUT),
            CaptureMode::Area | CaptureMode::Window => {
                return Err(CaptureError::new(
                    CaptureErrorKind::Unavailable,
                    "The Screenshot portal offers Screen and dialog captures only.",
                ));
            }
        };
        let options = HashMap::from([
            ("interactive".to_owned(), OwnedValue::from(interactive)),
            ("modal".to_owned(), OwnedValue::from(false)),
        ]);
        let response = call_portal(
            self.address.as_deref(),
            "Screenshot",
            options,
            timeout,
            cancel,
            // A late image is still read so its temporary file is removed.
            |response| {
                let _ = image_from_response(response);
            },
        );
        let outcome = response.and_then(image_from_response);
        if cancel.is_cancelled() {
            // Any image was already removed by `image_from_response`.
            return Err(cancelled());
        }
        outcome
    }
}

/// Sends one Screenshot-portal request on a dedicated connection and waits
/// for its `Response`. Cancel and timeout close the request and connection;
/// a response arriving shortly after a timeout goes to `discard`.
fn call_portal(
    address: Option<&str>,
    method: &str,
    mut options: HashMap<String, OwnedValue>,
    timeout: Duration,
    cancel: &CaptureCancel,
    discard: fn(Response),
) -> Result<Response, CaptureError> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let deadline = Instant::now() + timeout;
    let connection = connect(address, timeout).map_err(|error| {
        CaptureError::new(
            CaptureErrorKind::Unavailable,
            format!("Connecting to the session bus failed: {error}"),
        )
    })?;
    let Some(sender) = connection.unique_name().map(ToString::to_string) else {
        let _ = connection.close();
        return Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "The session bus assigned no unique name.",
        ));
    };
    let token = format!("kestrel_capture_{}", token_suffix());
    let request = request_path(&sender, &token);
    options.insert(
        "handle_token".to_owned(),
        OwnedValue::from(Str::from(token)),
    );
    let abort_connection = connection.clone();
    let abort_request = request.clone();
    cancel.set_abort(Box::new(move || {
        close_request(&abort_connection, &abort_request);
        let _ = abort_connection.close();
    }));
    register_application(&connection);
    let response = send_request(&connection, &request, deadline, discard, || {
        Proxy::new(&connection, PORTAL_BUS, PORTAL_PATH, SCREENSHOT_INTERFACE)?
            .call(method, &("", options))
    });
    cancel.clear_abort();
    let _ = connection.close();
    match response {
        Ok(response) => Ok(response),
        Err(Failure::Timeout) => Err(CaptureError::new(
            CaptureErrorKind::Timeout,
            "The desktop did not answer the request in time.",
        )),
        Err(Failure::Error(error)) => Err(error),
    }
}

pub(crate) enum Failure {
    Timeout,
    Error(CaptureError),
}

fn failed(message: impl Into<String>) -> Failure {
    Failure::Error(CaptureError::new(CaptureErrorKind::Failed, message))
}

/// Makes one portal request through `call` and waits for its `Response`
/// until `deadline`. On timeout the request and connection close; a response
/// arriving shortly after goes to `discard`.
pub(crate) fn send_request(
    connection: &Connection,
    request: &str,
    deadline: Instant,
    discard: fn(Response),
    call: impl FnOnce() -> zbus::Result<OwnedObjectPath>,
) -> Result<Response, Failure> {
    // Subscribed before the call, so a fast Response is not lost.
    let signals = response_signals(connection)
        .map_err(|error| failed(format!("Subscribing to portal responses failed: {error}")))?;
    let handle = call().map_err(|error| failed(format!("The portal request failed: {error}")))?;
    // Portals older than version 0.9 answer on the returned handle.
    let paths = [request.to_owned(), handle.as_str().to_owned()];
    let receiver = spawn_waiter(signals, paths)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(result) => result.map_err(failed),
        Err(RecvTimeoutError::Timeout) => {
            close_request(connection, request);
            let _ = connection.clone().close();
            if let Ok(Ok(response)) = receiver.recv_timeout(LATE_RESPONSE_GRACE) {
                discard(response);
            }
            Err(Failure::Timeout)
        }
        Err(RecvTimeoutError::Disconnected) => Err(failed("The portal response waiter stopped.")),
    }
}

fn response_signals(connection: &Connection) -> Result<MessageIterator, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(PORTAL_BUS)?
        .interface(REQUEST_INTERFACE)?
        .member(RESPONSE_MEMBER)?
        .build();
    MessageIterator::for_match_rule(rule, connection, Some(16))
}

/// The iterator blocks without a deadline, so it runs on its own thread and
/// ends once the connection closes.
fn spawn_waiter(
    mut signals: MessageIterator,
    paths: [String; 2],
) -> Result<Receiver<Result<Response, String>>, Failure> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("kestrel-capture-portal".to_owned())
        .spawn(move || {
            let result = loop {
                let Some(message) = signals.next() else {
                    break Err("The portal connection closed.".to_owned());
                };
                let Ok(message) = message else {
                    break Err("Receiving the portal response failed.".to_owned());
                };
                let header = message.header();
                let matches = header
                    .path()
                    .is_some_and(|path| paths.iter().any(|expected| expected == path.as_str()));
                if matches {
                    break message
                        .body()
                        .deserialize::<Response>()
                        .map_err(|_| "The portal response was malformed.".to_owned());
                }
            };
            let _ = sender.send(result);
        })
        .map_err(|_| failed("Starting the portal response waiter failed."))?;
    Ok(receiver)
}

fn color_from_response((code, results): Response) -> Result<[u8; 3], CaptureError> {
    match code {
        0 => {
            let (red, green, blue) = results
                .get("color")
                .and_then(|value| <(f64, f64, f64)>::try_from(value.clone()).ok())
                .ok_or_else(|| {
                    CaptureError::new(
                        CaptureErrorKind::Failed,
                        "The portal response named no color.",
                    )
                })?;
            let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            Ok([channel(red), channel(green), channel(blue)])
        }
        1 => Err(cancelled()),
        2 => Err(CaptureError::new(
            CaptureErrorKind::Denied,
            "The desktop denied the color picker.",
        )),
        _ => Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "The color picker failed.",
        )),
    }
}

fn image_from_response((code, results): Response) -> Result<CapturedImage, CaptureError> {
    match code {
        0 => {
            let uri = results
                .get("uri")
                .and_then(|value| String::try_from(value.clone()).ok())
                .ok_or_else(|| {
                    CaptureError::new(
                        CaptureErrorKind::Failed,
                        "The portal response named no image.",
                    )
                })?;
            take_image(&uri)
        }
        1 => Err(cancelled()),
        2 => Err(CaptureError::new(
            CaptureErrorKind::Denied,
            "The desktop denied screenshot permission.",
        )),
        _ => Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "The Screenshot portal failed.",
        )),
    }
}

/// Reads the image a `file://` URI names and removes it when it is a
/// temporary file; user-saved files stay.
fn take_image(uri: &str) -> Result<CapturedImage, CaptureError> {
    let Some(encoded) = uri.strip_prefix("file://") else {
        return Err(CaptureError::new(
            CaptureErrorKind::NoImage,
            "The desktop kept the screenshot on the clipboard.",
        ));
    };
    let path = file_uri_path(encoded).ok_or_else(|| {
        CaptureError::new(
            CaptureErrorKind::Failed,
            "The portal returned an unreadable image location.",
        )
    })?;
    let result = read_bounded(&path).and_then(validate_png);
    if is_temporary(&path) {
        let _ = fs::remove_file(&path);
    }
    result
}

/// Decodes the part after `file://`; only local absolute paths are accepted.
fn file_uri_path(encoded: &str) -> Option<PathBuf> {
    let encoded = encoded.strip_prefix("localhost").unwrap_or(encoded);
    if !encoded.starts_with('/') {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    if decoded.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

/// Inside the temporary directory or `$XDG_RUNTIME_DIR`, after resolving the parent.
fn is_temporary(path: &Path) -> bool {
    let Some(parent) = path.parent().and_then(|parent| parent.canonicalize().ok()) else {
        return false;
    };
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    [Some(std::env::temp_dir()), runtime]
        .into_iter()
        .flatten()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| parent.starts_with(root))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, CaptureError> {
    let unreadable = || {
        CaptureError::new(
            CaptureErrorKind::Failed,
            "The screenshot file is unreadable.",
        )
    };
    // Non-blocking open keeps a FIFO from stalling the capture.
    let file: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| unreadable())?;
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.is_file() {
        return Err(unreadable());
    }
    if metadata.len() > MAX_CAPTURE_PNG_BYTES as u64 {
        return Err(too_large());
    }
    let mut png = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CAPTURE_PNG_BYTES as u64 + 1)
        .read_to_end(&mut png)
        .map_err(|_| unreadable())?;
    if png.len() > MAX_CAPTURE_PNG_BYTES {
        return Err(too_large());
    }
    Ok(png)
}

pub(crate) fn close_request(connection: &Connection, path: &str) {
    if let Ok(proxy) = Proxy::new(connection, PORTAL_BUS, path, REQUEST_INTERFACE) {
        let _ = proxy.call_noreply("Close", &());
    }
}

pub(crate) fn request_path(sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

pub(crate) fn token_suffix() -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{decode_rgba, solid_png};
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;

    #[derive(Clone, Copy)]
    enum Reply {
        Image,
        Code(u32),
        Clipboard,
        Never,
    }

    #[derive(Default)]
    struct Observed {
        interactive: Option<bool>,
        written: Option<PathBuf>,
    }

    struct FakePortal {
        reply: Reply,
        observed: Arc<Mutex<Observed>>,
    }

    fn fdo(error: impl std::fmt::Display) -> zbus::fdo::Error {
        zbus::fdo::Error::Failed(error.to_string())
    }

    #[zbus::interface(name = "org.freedesktop.portal.Screenshot")]
    impl FakePortal {
        // Async so emitting never blocks inside zbus's executor (tokio when
        // another workspace crate enables it).
        async fn screenshot(
            &self,
            _parent_window: String,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let token = options
                .get("handle_token")
                .and_then(|value| String::try_from(value.clone()).ok())
                .ok_or_else(|| fdo("missing handle token"))?;
            let interactive = options
                .get("interactive")
                .and_then(|value| bool::try_from(value.clone()).ok());
            let sender = header.sender().ok_or_else(|| fdo("missing sender"))?;
            let path = request_path(sender.as_str(), &token);
            let handle = OwnedObjectPath::try_from(path.as_str()).map_err(fdo)?;
            let (code, uri) = {
                let mut observed = self.observed.lock().map_err(fdo)?;
                observed.interactive = interactive;
                match self.reply {
                    Reply::Never => return Ok(handle),
                    Reply::Code(code) => (code, None),
                    Reply::Clipboard => (0, Some("clipboard:///".to_owned())),
                    Reply::Image => {
                        let file = std::env::temp_dir().join(format!(
                            "kestrel fake portal {} {}.png",
                            std::process::id(),
                            token_suffix()
                        ));
                        fs::write(&file, solid_png(4, 3, [10, 200, 30, 255])).map_err(fdo)?;
                        let uri = format!("file://{}", file.display()).replace(' ', "%20");
                        observed.written = Some(file);
                        (0, Some(uri))
                    }
                }
            };
            let mut results = HashMap::new();
            if let Some(uri) = uri {
                results.insert("uri".to_owned(), OwnedValue::from(Str::from(uri)));
            }
            let emitter = SignalEmitter::new(connection, path).map_err(fdo)?;
            emitter
                .emit(REQUEST_INTERFACE, RESPONSE_MEMBER, &(code, results))
                .await
                .map_err(fdo)?;
            Ok(handle)
        }

        async fn pick_color(
            &self,
            _parent_window: String,
            options: HashMap<String, OwnedValue>,
            #[zbus(connection)] connection: &zbus::Connection,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let token = options
                .get("handle_token")
                .and_then(|value| String::try_from(value.clone()).ok())
                .ok_or_else(|| fdo("missing handle token"))?;
            let sender = header.sender().ok_or_else(|| fdo("missing sender"))?;
            let path = request_path(sender.as_str(), &token);
            let handle = OwnedObjectPath::try_from(path.as_str()).map_err(fdo)?;
            let mut results = HashMap::new();
            let code = match self.reply {
                Reply::Never => return Ok(handle),
                Reply::Code(code) => code,
                Reply::Image | Reply::Clipboard => {
                    let color = zbus::zvariant::Value::from((0.2_f64, 0.4_f64, 1.5_f64));
                    results.insert(
                        "color".to_owned(),
                        OwnedValue::try_from(color).map_err(fdo)?,
                    );
                    0
                }
            };
            let emitter = SignalEmitter::new(connection, path).map_err(fdo)?;
            emitter
                .emit(REQUEST_INTERFACE, RESPONSE_MEMBER, &(code, results))
                .await
                .map_err(fdo)?;
            Ok(handle)
        }
    }

    struct FakeBus {
        address: String,
        observed: Arc<Mutex<Observed>>,
        /// Kept alive for the test's duration.
        _bus: crate::test_bus::PrivateBus,
    }

    impl FakeBus {
        fn start(reply: Reply) -> Option<Self> {
            let observed = Arc::new(Mutex::new(Observed::default()));
            let portal = FakePortal {
                reply,
                observed: Arc::clone(&observed),
            };
            let bus = crate::test_bus::PrivateBus::start(PORTAL_BUS, move |builder| {
                builder.serve_at(PORTAL_PATH, portal)
            })?;
            Some(Self {
                address: bus.address.clone(),
                observed,
                _bus: bus,
            })
        }

        fn observed(&self) -> std::sync::MutexGuard<'_, Observed> {
            self.observed.lock().expect("observed state")
        }
    }

    #[test]
    fn screen_capture_reads_the_portal_image_and_removes_the_temp_file() {
        let Some(bus) = FakeBus::start(Reply::Image) else {
            return;
        };
        let backend = PortalCaptureBackend::with_bus_address(&bus.address);
        let image = backend
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .expect("portal capture succeeds");
        assert_eq!((image.width, image.height), (4, 3));
        let (_, _, pixels) = decode_rgba(&image.png);
        assert_eq!(&pixels[..4], &[10, 200, 30, 255]);
        let observed = bus.observed();
        assert_eq!(observed.interactive, Some(false));
        let written = observed.written.as_ref().expect("fake wrote an image");
        assert!(!written.exists(), "temporary portal file is removed");
    }

    #[test]
    fn interactive_capture_asks_for_the_dialog_and_maps_dismissal_to_cancelled() {
        let Some(bus) = FakeBus::start(Reply::Code(1)) else {
            return;
        };
        let backend = PortalCaptureBackend::with_bus_address(&bus.address);
        let error = backend
            .capture(CaptureMode::Interactive, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Cancelled);
        assert_eq!(bus.observed().interactive, Some(true));
    }

    #[test]
    fn denied_response_is_typed() {
        let Some(bus) = FakeBus::start(Reply::Code(2)) else {
            return;
        };
        let backend = PortalCaptureBackend::with_bus_address(&bus.address);
        let error = backend
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Denied);
        assert!(error.message.contains("permission"));
    }

    #[test]
    fn clipboard_result_reports_no_image() {
        let Some(bus) = FakeBus::start(Reply::Clipboard) else {
            return;
        };
        let backend = PortalCaptureBackend::with_bus_address(&bus.address);
        let error = backend
            .capture(CaptureMode::Interactive, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::NoImage);
    }

    #[test]
    fn cancelling_a_pending_request_returns_promptly() {
        let Some(bus) = FakeBus::start(Reply::Never) else {
            return;
        };
        let backend = PortalCaptureBackend::with_bus_address(&bus.address);
        let cancel = CaptureCancel::new();
        let remote = cancel.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            remote.cancel();
        });
        let started = Instant::now();
        let error = backend
            .capture(CaptureMode::Interactive, &cancel)
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
        canceller.join().expect("canceller joins");
    }

    #[test]
    fn non_portal_modes_are_unavailable_without_touching_the_bus() {
        let backend = PortalCaptureBackend::with_bus_address("unix:path=/nonexistent/bus");
        for mode in [CaptureMode::Area, CaptureMode::Window] {
            let error = backend.capture(mode, &CaptureCancel::new()).unwrap_err();
            assert_eq!(error.kind, CaptureErrorKind::Unavailable);
        }
    }

    #[test]
    fn file_uris_decode_to_local_paths() {
        for (uri, expected) in [
            ("/tmp/a%20b%25.png", Some(PathBuf::from("/tmp/a b%.png"))),
            ("localhost/tmp/x.png", Some(PathBuf::from("/tmp/x.png"))),
            ("host/tmp/x.png", None),
            ("/tmp/bad%2", None),
            ("/tmp/nul%00.png", None),
        ] {
            assert_eq!(file_uri_path(uri), expected, "{uri}");
        }
    }

    #[test]
    fn only_files_in_temporary_directories_are_removed() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        assert!(is_temporary(&dir.path().join("screenshot.png")));
        assert!(!is_temporary(Path::new("/usr/share/screenshot.png")));
        assert!(!is_temporary(Path::new("/nonexistent-dir/screenshot.png")));
    }

    #[test]
    fn predicts_request_paths_from_the_unique_name() {
        assert_eq!(
            request_path(":1.42", "kestrel_capture_7"),
            "/org/freedesktop/portal/desktop/request/1_42/kestrel_capture_7"
        );
    }

    #[test]
    fn picked_colors_are_clamped_to_bytes() {
        let Some(bus) = FakeBus::start(Reply::Image) else {
            return;
        };
        let picker = PortalColorPicker::with_bus_address(&bus.address);
        let color = picker.pick(&CaptureCancel::new()).expect("color picked");
        assert_eq!(color, [51, 102, 255]);
    }

    #[test]
    fn a_dismissed_or_denied_picker_is_typed() {
        for (code, kind) in [
            (1, CaptureErrorKind::Cancelled),
            (2, CaptureErrorKind::Denied),
        ] {
            let Some(bus) = FakeBus::start(Reply::Code(code)) else {
                return;
            };
            let picker = PortalColorPicker::with_bus_address(&bus.address);
            let error = picker.pick(&CaptureCancel::new()).unwrap_err();
            assert_eq!(error.kind, kind);
        }
    }
}
