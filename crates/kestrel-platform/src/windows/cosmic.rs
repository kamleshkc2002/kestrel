use super::{
    MAX_WINDOWS, WindowBackend, WindowError, WindowErrorKind, WindowHandle, WindowInfo,
    WindowProvider, bounded_text,
};
use cosmic_protocols::{
    toplevel_info::v1::client::{zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1},
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
};
use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, event_created_child,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

const COSMIC_BACKEND_LABEL: &str = "COSMIC toplevel";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROVIDER_READY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROTOCOL_STATE_BYTES: usize = 4;
const ACTIVE_STATE: u32 = 2;
const MINIMIZED_STATE: u32 = 1;

struct Entry {
    foreign: Option<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1>,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    handle: WindowHandle,
    title: String,
    app_id: Option<String>,
    active: bool,
    minimized: bool,
    closed: bool,
}

struct State {
    list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    manager: Option<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    entries: Vec<Entry>,
    next: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            list: None,
            info: None,
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

/// COSMIC toplevel-management provider.
pub struct CosmicProvider {
    requests: mpsc::Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl CosmicProvider {
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
                let mut state = State::default();
                connection.display().get_registry(&queue_handle, ());
                let _ = queue.roundtrip(&mut state);
                let ready = state.info.is_some() && state.manager.is_some() && state.seat.is_some();
                let _ = ready_sender.send(ready);
                if !ready {
                    return;
                }
                loop {
                    if queue.roundtrip(&mut state).is_err() {
                        break;
                    }
                    match receiver.recv_timeout(WORKER_POLL_INTERVAL) {
                        Ok(Request::Snapshot(reply)) => {
                            let _ = reply.send(Ok(snapshot(&state)));
                        }
                        Ok(Request::Activate(handle, reply)) => {
                            let result = request_action(&state, handle, true);
                            let _ = reply.send(result);
                        }
                        Ok(Request::Minimize(handle, reply)) => {
                            let mut result = request_action(&state, handle, false);
                            if result.is_ok() {
                                result = queue
                                    .roundtrip(&mut state)
                                    .map_err(|error| {
                                        WindowError::new(
                                            WindowErrorKind::Disconnected,
                                            format!("Wayland roundtrip failed: {error}"),
                                        )
                                    })
                                    .and_then(|_| {
                                        minimize_outcome(
                                            state
                                                .entries
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
            return Err(WindowError::unsupported(
                "COSMIC toplevel info, management, and wl_seat globals were not usable",
            ));
        }
        Ok(Self {
            requests,
            thread: Some(worker_thread),
        })
    }
}

impl Drop for CosmicProvider {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker_thread) = self.thread.take() {
            let _ = worker_thread.join();
        }
    }
}

impl WindowProvider for CosmicProvider {
    fn backend(&self) -> WindowBackend {
        WindowBackend::CosmicToplevel
    }

    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Snapshot(sender))
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?
    }

    fn activate(&mut self, handle: WindowHandle) -> Result<(), WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Activate(handle, sender))
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?
    }

    fn minimize(&mut self, handle: WindowHandle) -> Result<(), WindowError> {
        let (sender, receiver) = mpsc::channel();
        self.requests
            .send(Request::Minimize(handle, sender))
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?;
        receiver
            .recv()
            .map_err(|_| WindowError::disconnected("COSMIC window worker stopped"))?
    }
}

fn snapshot(state: &State) -> Vec<WindowInfo> {
    state
        .entries
        .iter()
        .filter(|entry| !entry.closed)
        .take(MAX_WINDOWS)
        .map(|entry| WindowInfo {
            handle: entry.handle,
            title: entry.title.clone(),
            app_id: entry.app_id.clone(),
            active: entry.active,
            minimized: entry.minimized,
        })
        .collect()
}

fn request_action(state: &State, handle: WindowHandle, activate: bool) -> Result<(), WindowError> {
    let entry = state
        .entries
        .iter()
        .find(|entry| entry.handle == handle && !entry.closed)
        .ok_or_else(|| {
            WindowError::new(WindowErrorKind::NotFound, "window handle was not found")
        })?;
    let manager = state
        .manager
        .as_ref()
        .ok_or_else(|| WindowError::unsupported("COSMIC toplevel management is unavailable"))?;
    let seat = state
        .seat
        .as_ref()
        .ok_or_else(|| WindowError::unsupported("COSMIC activation requires wl_seat"))?;
    let cosmic = entry
        .cosmic
        .as_ref()
        .ok_or_else(|| WindowError::new(WindowErrorKind::Backend, "COSMIC handle is not ready"))?;
    if activate {
        manager.activate(cosmic, seat);
    } else {
        manager.set_minimized(cosmic);
    }
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
            "{COSMIC_BACKEND_LABEL} did not minimize the window; the compositor ignores minimize requests"
        )))
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
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
                "ext_foreign_toplevel_list_v1" => {
                    state.list = Some(registry.bind(name, version.min(1), queue, ()))
                }
                "zcosmic_toplevel_info_v1" => {
                    state.info = Some(registry.bind(name, version.min(3), queue, ()))
                }
                "zcosmic_toplevel_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(4), queue, ()))
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(9), queue, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for State {
    fn event(
        _state: &mut Self,
        _list: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ())]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        foreign: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let entry_index = state
            .entries
            .iter()
            .position(|entry| entry.foreign.as_ref() == Some(foreign));
        let entry_index = entry_index.unwrap_or_else(|| {
            let handle = WindowHandle(state.next);
            state.next += 1;
            state.entries.push(Entry {
                foreign: Some(foreign.clone()),
                cosmic: None,
                handle,
                title: String::new(),
                app_id: None,
                active: false,
                minimized: false,
                closed: false,
            });
            state.entries.len() - 1
        });
        let entry = &mut state.entries[entry_index];
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                entry.title = bounded_text(&title)
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                entry.app_id = Some(bounded_text(&app_id))
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => entry.closed = true,
            _ => {}
        }
        if entry.cosmic.is_none() {
            if let Some(info) = state.info.as_ref() {
                info.get_cosmic_toplevel(foreign, queue, ());
            }
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        _state: &mut Self,
        _info: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        _event: zcosmic_toplevel_info_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, [zcosmic_toplevel_info_v1::REQ_GET_COSMIC_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ())]);
}

impl Dispatch<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _manager: &zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1,
        _event: zcosmic_toplevel_manager_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        cosmic: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.cosmic.is_none())
        {
            entry.cosmic = Some(cosmic.clone());
        }
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.cosmic.as_ref() == Some(cosmic))
        {
            if let zcosmic_toplevel_handle_v1::Event::State { state: states } = event {
                entry.active = false;
                entry.minimized = false;
                for state_value in states
                    .as_chunks::<MAX_PROTOCOL_STATE_BYTES>()
                    .0
                    .iter()
                    .map(|bytes| u32::from_ne_bytes(*bytes))
                {
                    entry.active |= state_value == ACTIVE_STATE;
                    entry.minimized |= state_value == MINIMIZED_STATE;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(minimized: bool, closed: bool) -> Entry {
        Entry {
            foreign: None,
            cosmic: None,
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
