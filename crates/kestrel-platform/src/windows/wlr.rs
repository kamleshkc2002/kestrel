use super::{
    WindowBackend, WindowError, WindowErrorKind, WindowHandle, WindowInfo, WindowProvider,
    bounded_text,
};
use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, event_created_child,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};

const WLR_BACKEND_LABEL: &str = "wlroots foreign toplevel";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROVIDER_READY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROTOCOL_STATE_BYTES: usize = 4;
const ACTIVE_STATE: u32 = 2;
const MINIMIZED_STATE: u32 = 1;

struct Entry {
    proxy: Option<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1>,
    handle: WindowHandle,
    title: String,
    app_id: Option<String>,
    active: bool,
    minimized: bool,
    closed: bool,
}

struct Data {
    manager: Option<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    entries: Vec<Entry>,
    next: u64,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            manager: None,
            seat: None,
            entries: Vec::new(),
            next: 1,
        }
    }
}

enum Request {
    Snapshot(mpsc::Sender<Result<Vec<WindowInfo>, WindowError>>),
    Activate(WindowHandle, mpsc::Sender<Result<(), WindowError>>),
    Minimize(WindowHandle, mpsc::Sender<Result<(), WindowError>>),
    Stop,
}

/// Wayland foreign-toplevel provider for wlroots compositors.
pub struct WlrProvider {
    requests: mpsc::Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl WlrProvider {
    /// Connect to the compositor and start the Wayland worker.
    pub fn new() -> Result<Self, WindowError> {
        let connection = Connection::connect_to_env().map_err(|error| {
            WindowError::new(
                WindowErrorKind::Disconnected,
                format!("Wayland connection failed: {error}"),
            )
        })?;
        let (requests, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let worker_thread = thread::Builder::new()
            .name("kestrel-windows".into())
            .spawn(move || {
                let mut queue = connection.new_event_queue();
                let queue_handle = queue.handle();
                let mut data = Data::default();
                connection.display().get_registry(&queue_handle, ());
                let _ = queue.roundtrip(&mut data);
                let ready = data.manager.is_some() && data.seat.is_some();
                let _ = ready_sender.send(ready);
                if !ready {
                    return;
                }
                loop {
                    let _ = queue.roundtrip(&mut data);
                    match receiver.recv_timeout(WORKER_POLL_INTERVAL) {
                        Ok(Request::Snapshot(reply)) => {
                            let _ = reply.send(Ok(snapshot(&data)));
                        }
                        Ok(Request::Activate(handle, reply)) => {
                            let result = request_action(&data, handle, true);
                            let _ = reply.send(result);
                        }
                        Ok(Request::Minimize(handle, reply)) => {
                            let mut result = request_action(&data, handle, false);
                            if result.is_ok() {
                                result = queue
                                    .roundtrip(&mut data)
                                    .map_err(|error| {
                                        WindowError::new(
                                            WindowErrorKind::Disconnected,
                                            format!("Wayland roundtrip failed: {error}"),
                                        )
                                    })
                                    .and_then(|_| {
                                        minimize_outcome(
                                            data.entries
                                                .iter()
                                                .find(|entry| entry.handle == handle),
                                        )
                                    });
                            }
                            let _ = reply.send(result);
                        }
                        Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(|error| WindowError::new(WindowErrorKind::Disconnected, error.to_string()))?;
        if !ready_receiver
            .recv_timeout(PROVIDER_READY_TIMEOUT)
            .unwrap_or(false)
        {
            let _ = requests.send(Request::Stop);
            let _ = worker_thread.join();
            return Err(WindowError::new(
                WindowErrorKind::Unsupported,
                "wlr foreign toplevel manager or wl_seat was not advertised",
            ));
        }
        Ok(Self {
            requests,
            thread: Some(worker_thread),
        })
    }
}

fn snapshot(data: &Data) -> Vec<WindowInfo> {
    data.entries
        .iter()
        .filter(|entry| !entry.closed)
        .take(super::MAX_WINDOWS)
        .map(|entry| WindowInfo {
            handle: entry.handle,
            title: entry.title.clone(),
            app_id: entry.app_id.clone(),
            active: entry.active,
            minimized: entry.minimized,
        })
        .collect()
}

fn request_action(data: &Data, handle: WindowHandle, activate: bool) -> Result<(), WindowError> {
    let entry = data
        .entries
        .iter()
        .find(|entry| entry.handle == handle && !entry.closed)
        .ok_or_else(|| {
            WindowError::new(WindowErrorKind::NotFound, "window handle was not found")
        })?;
    let seat = data
        .seat
        .as_ref()
        .ok_or_else(|| WindowError::unsupported("wlr activation requires wl_seat"))?;
    let proxy = entry
        .proxy
        .as_ref()
        .expect("live foreign toplevel entries have proxies");
    if activate {
        proxy.activate(seat);
    } else {
        proxy.set_minimized();
    }
    proxy.version();
    Ok(())
}

/// Confirms the compositor applied a minimize request.
fn minimize_outcome(entry: Option<&Entry>) -> Result<(), WindowError> {
    let entry = entry.filter(|entry| !entry.closed).ok_or_else(|| {
        WindowError::new(WindowErrorKind::NotFound, "window handle was not found")
    })?;
    if entry.minimized {
        Ok(())
    } else {
        Err(WindowError::unsupported(format!(
            "{WLR_BACKEND_LABEL} did not minimize the window; the compositor ignores minimize requests"
        )))
    }
}

impl Drop for WlrProvider {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker_thread) = self.thread.take() {
            let _ = worker_thread.join();
        }
    }
}

impl WindowProvider for WlrProvider {
    fn backend(&self) -> WindowBackend {
        WindowBackend::WlrForeignToplevel
    }

    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Snapshot(sender))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }

    fn activate(&mut self, handle: WindowHandle) -> Result<(), WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Activate(handle, sender))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }

    fn minimize(&mut self, handle: WindowHandle) -> Result<(), WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Minimize(handle, sender))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Data {
    fn event(
        data: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _user_data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_foreign_toplevel_manager_v1" => {
                    data.manager = Some(registry.bind(name, version.min(3), queue, ()))
                }
                "wl_seat" if data.seat.is_none() => {
                    data.seat = Some(registry.bind(name, version.min(9), queue, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Data {
    fn event(
        _data: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for Data {
    fn event(
        _data: &mut Self,
        _manager: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        _event: zwlr_foreign_toplevel_manager_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(Data,zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,[zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE=>(zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,())]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for Data {
    fn event(
        data: &mut Self,
        proxy: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let entry_index = data
            .entries
            .iter()
            .position(|entry| entry.proxy.as_ref() == Some(proxy));
        let entry_index = entry_index.unwrap_or_else(|| {
            let handle = WindowHandle(data.next);
            data.next += 1;
            data.entries.push(Entry {
                proxy: Some(proxy.clone()),
                handle,
                title: String::new(),
                app_id: None,
                active: false,
                minimized: false,
                closed: false,
            });
            data.entries.len() - 1
        });
        let entry = &mut data.entries[entry_index];
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                entry.title = bounded_text(&title)
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                entry.app_id = Some(bounded_text(&app_id))
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state } => {
                entry.active = false;
                entry.minimized = false;
                for state_value in state
                    .as_chunks::<MAX_PROTOCOL_STATE_BYTES>()
                    .0
                    .iter()
                    .map(|bytes| u32::from_ne_bytes(*bytes))
                {
                    entry.active |= state_value == ACTIVE_STATE;
                    entry.minimized |= state_value == MINIMIZED_STATE;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => entry.closed = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(minimized: bool, closed: bool) -> Entry {
        Entry {
            proxy: None,
            handle: WindowHandle(1),
            title: String::new(),
            app_id: None,
            active: false,
            minimized,
            closed,
        }
    }

    #[test]
    fn minimize_outcome_accepts_honoured_request() {
        assert!(minimize_outcome(Some(&test_entry(true, false))).is_ok());
    }

    #[test]
    fn minimize_outcome_rejects_ignored_request() {
        let error = minimize_outcome(Some(&test_entry(false, false))).unwrap_err();
        assert_eq!(error.kind, WindowErrorKind::Unsupported);
    }

    #[test]
    fn minimize_outcome_reports_closed_window() {
        let error = minimize_outcome(Some(&test_entry(false, true))).unwrap_err();
        assert_eq!(error.kind, WindowErrorKind::NotFound);
        assert_eq!(
            minimize_outcome(None).unwrap_err().kind,
            WindowErrorKind::NotFound
        );
    }
}
