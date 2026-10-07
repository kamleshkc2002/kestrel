//! `XGrabKey` backend for real X11 sessions.

use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
};

use kestrel_core::ShortcutModifiers;
use x11rb::{
    connection::Connection,
    errors::ReplyError,
    protocol::{
        ErrorKind, Event,
        xproto::{
            ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, CreateWindowAux,
            EventMask, GrabMode, KeyPressEvent, ModMask, WindowClass,
        },
    },
    rust_connection::RustConnection,
};

use super::{
    ActivationSink, BindingState, SharedShortcutStatus, ShortcutBackend, ShortcutError,
    ShortcutErrorKind, ShortcutPhase, ShortcutProvider, ShortcutRequest, update_status,
};

const CORE_MODIFIERS: u16 = 1 | 4 | 8 | 64;
const LOCK_MASK: u16 = 2;
const NUM_LOCK_MASK: u16 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundGrab {
    keycode: u8,
    modifiers: u16,
}

#[derive(Debug)]
struct StopTarget {
    display: Option<String>,
    window: u32,
    atom: u32,
}

#[derive(Debug)]
struct WorkerResult {
    target: Option<StopTarget>,
}

pub struct X11ShortcutBackend {
    worker: Option<JoinHandle<()>>,
    ready: Option<Receiver<WorkerResult>>,
    display: Option<String>,
}

impl Default for X11ShortcutBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl X11ShortcutBackend {
    pub fn new() -> Self {
        Self {
            worker: None,
            ready: None,
            display: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_display(display: impl Into<String>) -> Self {
        Self {
            worker: None,
            ready: None,
            display: Some(display.into()),
        }
    }
}

impl ShortcutBackend for X11ShortcutBackend {
    fn provider(&self) -> ShortcutProvider {
        ShortcutProvider::X11
    }

    fn start(
        &mut self,
        requests: Vec<ShortcutRequest>,
        sink: ActivationSink,
        status: SharedShortcutStatus,
    ) -> Result<(), ShortcutError> {
        self.stop();
        let bindings = requests
            .iter()
            .map(|request| super::BindingStatus {
                id: request.id.clone(),
                requested: request.trigger.to_string(),
                state: BindingState::Pending,
            })
            .collect();
        update_status(&status, |current| {
            current.provider = Some(ShortcutProvider::X11);
            current.phase = ShortcutPhase::Starting;
            current.bindings = bindings;
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_status = status.clone();
        let display = self.display.clone();
        let worker = thread::Builder::new()
            .name("kestrel-x11-shortcuts".to_owned())
            .spawn(move || run_worker(requests, sink, worker_status, ready_tx, display))
            .map_err(|error| {
                let message = format!("failed to start X11 shortcut worker: {error}");
                update_status(&status, |current| {
                    current.phase = ShortcutPhase::Failed(ShortcutError::new(
                        ShortcutErrorKind::WorkerUnavailable,
                        message.clone(),
                    ));
                });
                ShortcutError::new(ShortcutErrorKind::WorkerUnavailable, message)
            })?;
        self.worker = Some(worker);
        self.ready = Some(ready_rx);
        Ok(())
    }

    fn stop(&mut self) {
        let Some(worker) = self.worker.take() else {
            self.ready = None;
            return;
        };

        if let Some(ready) = self.ready.take() {
            if let Ok(result) = ready.recv() {
                if let Some(target) = result.target {
                    wake_worker(target);
                }
            }
        }
        let _ = worker.join();
    }
}

impl Drop for X11ShortcutBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(
    requests: Vec<ShortcutRequest>,
    sink: ActivationSink,
    status: SharedShortcutStatus,
    ready: Sender<WorkerResult>,
    display_override: Option<String>,
) {
    let display = display_override.or_else(|| std::env::var("DISPLAY").ok());
    let connection = RustConnection::connect(display.as_deref());
    let (connection, screen_num) = match connection {
        Ok(value) => value,
        Err(error) => {
            update_status(&status, |current| {
                current.phase = ShortcutPhase::Failed(ShortcutError::new(
                    ShortcutErrorKind::Unavailable,
                    format!("X11 display is unavailable: {error}"),
                ));
            });
            let _ = ready.send(WorkerResult { target: None });
            return;
        }
    };

    let setup = connection.setup();
    let Some(screen) = setup.roots.get(screen_num) else {
        update_status(&status, |current| {
            current.phase = ShortcutPhase::Failed(ShortcutError::new(
                ShortcutErrorKind::Unavailable,
                "X11 display has no screen",
            ));
        });
        let _ = ready.send(WorkerResult { target: None });
        return;
    };
    let root = screen.root;

    let stop_atom = match connection.intern_atom(false, b"_KESTREL_X11_STOP") {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply.atom,
            Err(error) => {
                fail_worker(
                    &status,
                    &ready,
                    format!("failed to intern X11 stop atom: {error}"),
                );
                return;
            }
        },
        Err(error) => {
            fail_worker(
                &status,
                &ready,
                format!("failed to intern X11 stop atom: {error}"),
            );
            return;
        }
    };
    let stop_window = match connection.generate_id() {
        Ok(id) => id,
        Err(error) => {
            fail_worker(
                &status,
                &ready,
                format!("failed to allocate X11 stop window: {error}"),
            );
            return;
        }
    };
    let create = connection.create_window(
        0,
        stop_window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    );
    let create_result = match create {
        Ok(cookie) => cookie.check().map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = create_result {
        fail_worker(
            &status,
            &ready,
            format!("failed to create X11 stop window: {error}"),
        );
        return;
    }
    let select_stop_events = connection.change_window_attributes(
        stop_window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    );
    let select_result = match select_stop_events {
        Ok(cookie) => cookie.check().map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = select_result {
        fail_worker(
            &status,
            &ready,
            format!("failed to select X11 stop window events: {error}"),
        );
        return;
    }
    if let Err(error) = connection.flush() {
        fail_worker(
            &status,
            &ready,
            format!("failed to flush X11 setup: {error}"),
        );
        return;
    }

    let target = StopTarget {
        display,
        window: stop_window,
        atom: stop_atom,
    };
    if ready
        .send(WorkerResult {
            target: Some(StopTarget {
                display: target.display.clone(),
                window: target.window,
                atom: target.atom,
            }),
        })
        .is_err()
    {
        cleanup_connection(&connection, root, stop_window, &[]);
        return;
    }

    let keysyms = match keyboard_keysyms(&connection, setup.min_keycode, setup.max_keycode) {
        Ok(value) => value,
        Err(error) => {
            update_status(&status, |current| {
                current.phase = ShortcutPhase::Failed(ShortcutError::new(
                    ShortcutErrorKind::Protocol,
                    format!("failed to read X11 keyboard mapping: {error}"),
                ));
            });
            cleanup_connection(&connection, root, stop_window, &[]);
            return;
        }
    };

    let mut grabs = Vec::new();
    let mut bound = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        let trigger_text = request.trigger.to_string();
        let Some(keysym) = keysym_from_name(&request.trigger.key) else {
            set_binding(
                &status,
                index,
                BindingState::Rejected {
                    reason: format!("unknown XKB keysym name: {}", request.trigger.key),
                },
            );
            continue;
        };
        let Some(&keycode) = keysyms.get(&keysym) else {
            set_binding(
                &status,
                index,
                BindingState::Rejected {
                    reason: format!(
                        "keysym is not present in the X11 keyboard mapping: {}",
                        request.trigger.key
                    ),
                },
            );
            continue;
        };
        let modifiers = modifier_mask(request.trigger.modifiers);
        let variants = modifier_variants(modifiers);
        let mut successful = Vec::with_capacity(variants.len());
        let mut failure = None;
        for variant in variants.iter().copied() {
            let checked = match connection.grab_key(
                false,
                root,
                ModMask::from(variant),
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            ) {
                Ok(cookie) => cookie.check().map_err(|error| {
                    let access = matches!(
                        &error,
                        ReplyError::X11Error(x11) if x11.error_kind == ErrorKind::Access
                    );
                    (error.to_string(), access)
                }),
                Err(error) => Err((error.to_string(), false)),
            };
            match checked {
                Ok(()) => successful.push(variant),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if let Some((error, access)) = failure {
            for variant in successful {
                if let Ok(cookie) = connection.ungrab_key(keycode, root, ModMask::from(variant)) {
                    let _ = cookie.check();
                }
            }
            let state = if access {
                BindingState::Conflict {
                    reason: format!("another X11 client holds {trigger_text}"),
                }
            } else {
                BindingState::Rejected {
                    reason: format!("failed to grab {trigger_text}: {error}"),
                }
            };
            set_binding(&status, index, state);
            continue;
        }
        for variant in variants {
            grabs.push((keycode, variant));
        }
        bound.push((request.id.as_str(), BoundGrab { keycode, modifiers }));
        set_binding(
            &status,
            index,
            BindingState::Bound {
                trigger: Some(trigger_text),
            },
        );
    }
    let _ = connection.flush();
    if bound.is_empty() {
        update_status(&status, |current| {
            current.phase = ShortcutPhase::Starting;
        });
    } else {
        update_status(&status, |current| {
            current.phase = ShortcutPhase::Active;
        });
    }

    while let Ok(event) = connection.wait_for_event() {
        match event {
            Event::KeyPress(event) => dispatch_key_press(&event, &bound, &sink),
            Event::ClientMessage(event)
                if event.window == stop_window && event.type_ == stop_atom =>
            {
                break;
            }
            _ => {}
        }
    }
    cleanup_connection(&connection, root, stop_window, &grabs);
    update_status(&status, |current| {
        current.phase = ShortcutPhase::Stopped;
    });
}

fn fail_worker(status: &SharedShortcutStatus, ready: &Sender<WorkerResult>, message: String) {
    update_status(status, |current| {
        current.phase =
            ShortcutPhase::Failed(ShortcutError::new(ShortcutErrorKind::Protocol, &message));
    });
    let _ = ready.send(WorkerResult { target: None });
}

fn cleanup_connection(
    connection: &RustConnection,
    root: u32,
    stop_window: u32,
    grabs: &[(u8, u16)],
) {
    for &(keycode, modifiers) in grabs {
        if let Ok(cookie) = connection.ungrab_key(keycode, root, ModMask::from(modifiers)) {
            let _ = cookie.check();
        }
    }
    if let Ok(cookie) = connection.destroy_window(stop_window) {
        let _ = cookie.check();
    }
    let _ = connection.flush();
}

fn wake_worker(target: StopTarget) {
    let connection = RustConnection::connect(target.display.as_deref());
    let Ok((connection, _)) = connection else {
        return;
    };
    let event = ClientMessageEvent::new(32, target.window, target.atom, [0; 5]);
    if let Ok(cookie) =
        connection.send_event(false, target.window, EventMask::PROPERTY_CHANGE, event)
    {
        let _ = cookie.check();
    }
    let _ = connection.flush();
}

fn set_binding(status: &SharedShortcutStatus, index: usize, state: BindingState) {
    update_status(status, |current| {
        if let Some(binding) = current.bindings.get_mut(index) {
            binding.state = state;
        }
    });
}

fn keyboard_keysyms(
    connection: &RustConnection,
    min_keycode: u8,
    max_keycode: u8,
) -> Result<std::collections::HashMap<u32, u8>, String> {
    // The request takes a first keycode and a count.
    let count = max_keycode
        .checked_sub(min_keycode)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| "X11 keycode range is invalid".to_owned())?;
    let cookie = connection
        .get_keyboard_mapping(min_keycode, count)
        .map_err(|error| error.to_string())?;
    let reply = cookie.reply().map_err(|error| error.to_string())?;
    let width = usize::from(reply.keysyms_per_keycode);
    if width == 0 {
        return Err("X11 keyboard mapping has zero symbols per keycode".to_owned());
    }
    let mut map = std::collections::HashMap::new();
    for (offset, symbols) in reply.keysyms.chunks(width).enumerate() {
        let keycode = min_keycode.saturating_add(offset as u8);
        for &keysym in symbols.iter().filter(|&&keysym| keysym != 0) {
            map.entry(keysym).or_insert(keycode);
        }
    }
    Ok(map)
}

fn dispatch_key_press(event: &KeyPressEvent, bound: &[(&str, BoundGrab)], sink: &ActivationSink) {
    let state = u16::from(event.state);
    for &(id, grab) in bound {
        if event.detail == grab.keycode && state_matches(state, grab.modifiers) {
            sink(id);
            return;
        }
    }
}

fn modifier_mask(modifiers: ShortcutModifiers) -> u16 {
    let mut mask = 0;
    if modifiers.ctrl {
        mask |= u16::from(ModMask::CONTROL);
    }
    if modifiers.alt {
        mask |= u16::from(ModMask::M1);
    }
    if modifiers.shift {
        mask |= u16::from(ModMask::SHIFT);
    }
    if modifiers.logo {
        mask |= u16::from(ModMask::M4);
    }
    mask
}

fn modifier_variants(mask: u16) -> Vec<u16> {
    [
        mask,
        mask | LOCK_MASK,
        mask | NUM_LOCK_MASK,
        mask | LOCK_MASK | NUM_LOCK_MASK,
    ]
    .into_iter()
    .fold(Vec::with_capacity(4), |mut values, value| {
        if !values.contains(&value) {
            values.push(value);
        }
        values
    })
}

fn state_matches(state: u16, modifiers: u16) -> bool {
    state & CORE_MODIFIERS == modifiers & CORE_MODIFIERS
}

fn keysym_from_name(name: &str) -> Option<u32> {
    match name {
        "space" => Some(0x20),
        "Return" => Some(0xff0d),
        "Escape" => Some(0xff1b),
        "Tab" => Some(0xff09),
        "BackSpace" => Some(0xff08),
        "Delete" => Some(0xffff),
        "Home" => Some(0xff50),
        "End" => Some(0xff57),
        "Page_Up" | "Prior" => Some(0xff55),
        "Page_Down" | "Next" => Some(0xff56),
        "Up" => Some(0xff52),
        "Down" => Some(0xff54),
        "Left" => Some(0xff51),
        "Right" => Some(0xff53),
        "Insert" => Some(0xff63),
        "Print" => Some(0xff61),
        "minus" => Some(0x2d),
        "equal" => Some(0x3d),
        "comma" => Some(0x2c),
        "period" => Some(0x2e),
        "slash" => Some(0x2f),
        "semicolon" => Some(0x3b),
        "apostrophe" => Some(0x27),
        "bracketleft" => Some(0x5b),
        "bracketright" => Some(0x5d),
        "backslash" => Some(0x5c),
        "grave" => Some(0x60),
        _ => {
            if let Some(number) = name
                .strip_prefix('F')
                .and_then(|value| value.parse::<u32>().ok())
            {
                if (1..=24).contains(&number) {
                    return Some(0xffbd + number);
                }
            }
            let bytes = name.as_bytes();
            if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
                return Some(u32::from(bytes[0]));
            }
            if bytes.len() == 1 && bytes[0].is_ascii_digit() {
                return Some(u32::from(bytes[0]));
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Child, Command, Stdio},
        sync::{LazyLock, Mutex, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::global_shortcuts::{ShortcutStatus, lock_status};
    use kestrel_core::ShortcutTrigger;
    use x11rb::{
        connection::Connection,
        protocol::{
            xproto::{self, ConnectionExt, GrabMode, ModMask},
            xtest,
        },
        rust_connection::RustConnection,
    };
    struct TestServer(Child);

    impl Drop for TestServer {
        fn drop(&mut self) {
            // SIGTERM lets Xvfb remove its socket and lock file.
            if let Ok(pid) = libc::pid_t::try_from(self.0.id()) {
                // SAFETY: signals only the child this guard spawned.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            let _ = self.0.wait();
        }
    }

    /// Starts a private Xvfb; rootless Xwayland drops XTEST input, so it is not used.
    fn start_private_server() -> Option<(TestServer, String)> {
        Command::new("Xvfb").arg("-help").output().ok()?;
        // Disjoint from the capture live test, which runs in parallel.
        for number in 90..200 {
            let socket = format!("/tmp/.X11-unix/X{number}");
            // Leaves displays owned by other servers alone.
            if fs::metadata(&socket).is_ok()
                || fs::metadata(format!("/tmp/.X{number}-lock")).is_ok()
            {
                continue;
            }
            let display = format!(":{number}");
            let mut command = Command::new("Xvfb");
            command
                .arg(&display)
                .args(["-nolisten", "tcp", "-noreset", "-screen", "0", "800x600x24"])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let Ok(mut child) = command.spawn() else {
                continue;
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                // Another server on this number answers with a different size.
                let ours = fs::metadata(&socket).is_ok()
                    && RustConnection::connect(Some(&display)).is_ok_and(|(connection, screen)| {
                        connection.setup().roots.get(screen).is_some_and(|root| {
                            (root.width_in_pixels, root.height_in_pixels) == (800, 600)
                        })
                    });
                if ours && matches!(child.try_wait(), Ok(None)) {
                    return Some((TestServer(child), display));
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        None
    }

    fn wait_for_bindings(status: &SharedShortcutStatus) -> ShortcutStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = lock_status(status).clone();
            if !snapshot.bindings.is_empty()
                && snapshot
                    .bindings
                    .iter()
                    .all(|binding| !matches!(binding.state, BindingState::Pending))
            {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "bindings settle within 2 s: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn live_x11_binding_conflict_activation_and_release() {
        static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Skips only when no private server can run here.
        let Some((_server, display)) = start_private_server() else {
            eprintln!("skipping: Xvfb is not available");
            return;
        };
        let (conflict_connection, screen_num) =
            RustConnection::connect(Some(&display)).expect("connect to the private server");
        let setup = conflict_connection.setup();
        let root = setup.roots[screen_num].root;
        let keysyms = keyboard_keysyms(&conflict_connection, setup.min_keycode, setup.max_keycode)
            .expect("read the keyboard mapping");
        let a_keycode = keysyms[&0x61];
        let b_keycode = keysyms[&0x62];
        let control_keycode = *keysyms
            .get(&0xffe3)
            .or_else(|| keysyms.get(&0xffe4))
            .expect("a Control key exists");
        conflict_connection
            .grab_key(
                false,
                root,
                ModMask::CONTROL,
                a_keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .expect("send the competing grab")
            .check()
            .expect("the competing grab succeeds");

        let status = std::sync::Arc::new(std::sync::Mutex::new(ShortcutStatus::default()));
        let (activation_tx, activation_rx) = mpsc::channel();
        let sink: ActivationSink = std::sync::Arc::new(move |id| {
            let _ = activation_tx.send(id.to_owned());
        });
        let requests = vec![
            ShortcutRequest {
                id: "bound".to_owned(),
                description: "bound".to_owned(),
                trigger: ShortcutTrigger::parse("CTRL+b").expect("valid test trigger"),
            },
            ShortcutRequest {
                id: "conflict".to_owned(),
                description: "conflict".to_owned(),
                trigger: ShortcutTrigger::parse("CTRL+a").expect("valid test trigger"),
            },
        ];
        let mut backend = X11ShortcutBackend::new_for_display(display.clone());
        backend
            .start(requests, sink, status.clone())
            .expect("the backend starts");
        let snapshot = wait_for_bindings(&status);
        let state = |id: &str| {
            snapshot
                .bindings
                .iter()
                .find(|binding| binding.id == id)
                .map(|binding| binding.state.clone())
        };
        assert!(matches!(state("bound"), Some(BindingState::Bound { .. })));
        assert!(matches!(
            state("conflict"),
            Some(BindingState::Conflict { .. })
        ));
        assert_eq!(snapshot.phase, ShortcutPhase::Active);

        let (xtest_connection, _) =
            RustConnection::connect(Some(&display)).expect("connect the XTEST client");
        xtest::get_version(&xtest_connection, 2, 2)
            .expect("send the XTEST query")
            .reply()
            .expect("the private server has XTEST");
        for (event_type, detail) in [
            (xproto::KEY_PRESS_EVENT, control_keycode),
            (xproto::KEY_PRESS_EVENT, b_keycode),
            (xproto::KEY_RELEASE_EVENT, b_keycode),
            (xproto::KEY_RELEASE_EVENT, control_keycode),
        ] {
            xtest::fake_input(&xtest_connection, event_type, detail, 0, root, 0, 0, 0)
                .expect("send a synthetic key")
                .check()
                .expect("the server accepts the synthetic key");
        }
        assert_eq!(
            activation_rx
                .recv_timeout(Duration::from_secs(2))
                .ok()
                .as_deref(),
            Some("bound")
        );

        let started = Instant::now();
        backend.stop();
        assert!(started.elapsed() < Duration::from_secs(2));

        let (second_connection, _) =
            RustConnection::connect(Some(&display)).expect("connect a second client");
        second_connection
            .grab_key(
                false,
                root,
                ModMask::CONTROL,
                b_keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .expect("send the re-grab")
            .check()
            .expect("stop released the grab");
    }

    #[test]
    fn keysym_name_lookup_covers_requested_names() {
        assert_eq!(keysym_from_name("a"), Some(0x61));
        assert_eq!(keysym_from_name("9"), Some(0x39));
        assert_eq!(keysym_from_name("F24"), Some(0xffd5));
        assert_eq!(keysym_from_name("Page_Up"), Some(0xff55));
        assert_eq!(keysym_from_name("unknown"), None);
    }

    #[test]
    fn modifier_mask_builds_lock_and_numlock_variants() {
        let mask = modifier_mask(ShortcutModifiers {
            ctrl: true,
            alt: true,
            shift: false,
            logo: true,
        });
        assert_eq!(modifier_variants(mask).len(), 4);
        assert!(modifier_variants(mask).contains(&(mask | LOCK_MASK | NUM_LOCK_MASK)));
    }

    #[test]
    fn state_matching_ignores_lock_and_numlock() {
        let mask = modifier_mask(ShortcutModifiers {
            ctrl: true,
            alt: false,
            shift: true,
            logo: false,
        });
        assert!(state_matches(mask | LOCK_MASK | NUM_LOCK_MASK, mask));
        assert!(!state_matches(mask | ModMask::M1.bits(), mask));
    }
}
