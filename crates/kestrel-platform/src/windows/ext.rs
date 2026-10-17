use super::{
    WindowBackend, WindowError, WindowErrorKind, WindowHandle, WindowInfo, WindowProvider,
    bounded_text,
};
use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child, protocol::wl_seat};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROVIDER_READY_TIMEOUT: Duration = Duration::from_secs(2);

struct Entry {
    proxy: Option<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1>,
    handle: WindowHandle,
    title: String,
    app_id: Option<String>,
    closed: bool,
}

struct Data {
    manager: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    entries: Vec<Entry>,
    next: u64,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            manager: None,
            entries: Vec::new(),
            next: 1,
        }
    }
}

enum Request {
    Snapshot(mpsc::Sender<Result<Vec<WindowInfo>, WindowError>>),
    Stop,
}

/// Wayland read-only foreign-toplevel-list provider.
pub struct ExtProvider {
    requests: mpsc::Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl ExtProvider {
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
                let ready = data.manager.is_some();
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
                "ext_foreign_toplevel_list_v1 was not advertised",
            ));
        }
        Ok(Self {
            requests,
            thread: Some(worker_thread),
        })
    }
}

impl Drop for ExtProvider {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker_thread) = self.thread.take() {
            let _ = worker_thread.join();
        }
    }
}

impl WindowProvider for ExtProvider {
    fn backend(&self) -> WindowBackend {
        WindowBackend::ExtForeignToplevelList
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

    fn activate(&mut self, _handle: WindowHandle) -> Result<(), WindowError> {
        Err(WindowError::unsupported(
            "ext_foreign_toplevel_list_v1 has no activation request",
        ))
    }

    fn minimize(&mut self, _handle: WindowHandle) -> Result<(), WindowError> {
        Err(WindowError::unsupported(
            "ext_foreign_toplevel_list_v1 has no minimize request",
        ))
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
            active: false,
            minimized: false,
        })
        .collect()
}

impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()> for Data {
    fn event(
        data: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _user_data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            if interface == "ext_foreign_toplevel_list_v1" {
                data.manager = Some(registry.bind(name, 1, queue, ()))
            }
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for Data {
    fn event(
        _data: &mut Self,
        _list: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(Data,ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,[ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE=>(ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,())]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for Data {
    fn event(
        data: &mut Self,
        proxy: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
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
                closed: false,
            });
            data.entries.len() - 1
        });
        let entry = &mut data.entries[entry_index];
        match event {
            ext_foreign_toplevel_handle_v1::Event::Closed => entry.closed = true,
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                entry.title = bounded_text(&title)
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                entry.app_id = Some(bounded_text(&app_id))
            }
            _ => {}
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
