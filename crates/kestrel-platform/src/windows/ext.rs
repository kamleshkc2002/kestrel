use super::{
    WindowBackend, WindowError, WindowErrorKind, WindowHandle, WindowInfo, WindowProvider,
    bounded_text,
};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child, protocol::wl_seat};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

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
    finished: bool,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            manager: None,
            entries: Vec::new(),
            next: 1,
            finished: false,
        }
    }
}

enum Request {
    Snapshot(mpsc::Sender<Result<Vec<WindowInfo>, WindowError>>),
    Stop,
}

pub struct ExtProvider {
    tx: mpsc::Sender<Request>,
    thread: Option<JoinHandle<()>>,
}
impl ExtProvider {
    pub fn new() -> Result<Self, WindowError> {
        let conn = Connection::connect_to_env().map_err(|e| {
            WindowError::new(
                WindowErrorKind::Disconnected,
                format!("Wayland connection failed: {e}"),
            )
        })?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(Data::default()));
        let thread_state = state.clone();
        let thread = thread::Builder::new()
            .name("kestrel-windows".into())
            .spawn(move || {
                let mut queue = conn.new_event_queue();
                let qh = queue.handle();
                let mut data = Data::default();
                let display = conn.display();
                display.get_registry(&qh, ());
                let _ = queue.roundtrip(&mut data);
                let ok = data.manager.is_some();
                let _ = ready_tx.send(ok);
                if !ok {
                    return;
                }
                loop {
                    let _ = queue.roundtrip(&mut data);
                    if let Ok(mut shared) = thread_state.lock() {
                        *shared = data.clone_for_snapshot();
                    }
                    match rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(Request::Snapshot(reply)) => {
                            let _ = reply.send(Ok(snapshot(&data)));
                        }
                        Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(|e| WindowError::new(WindowErrorKind::Disconnected, e.to_string()))?;
        if !ready_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or(false)
        {
            let _ = tx.send(Request::Stop);
            let _ = thread.join();
            return Err(WindowError::new(
                WindowErrorKind::Unsupported,
                "ext_foreign_toplevel_list_v1 was not advertised",
            ));
        }
        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }
}
impl Drop for ExtProvider {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl WindowProvider for ExtProvider {
    fn backend(&self) -> WindowBackend {
        WindowBackend::ExtForeignToplevelList
    }
    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Request::Snapshot(tx))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        rx.recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }
    fn activate(&mut self, _: WindowHandle) -> Result<(), WindowError> {
        Err(WindowError::unsupported(
            "ext_foreign_toplevel_list_v1 has no activation request",
        ))
    }
    fn minimize(&mut self, _: WindowHandle) -> Result<(), WindowError> {
        Err(WindowError::unsupported(
            "ext_foreign_toplevel_list_v1 has no minimize request",
        ))
    }
}
fn snapshot(data: &Data) -> Vec<WindowInfo> {
    data.entries
        .iter()
        .filter(|e| !e.closed)
        .take(256)
        .map(|e| WindowInfo {
            handle: e.handle,
            title: e.title.clone(),
            app_id: e.app_id.clone(),
            active: false,
            minimized: false,
        })
        .collect()
}
impl Data {
    fn clone_for_snapshot(&self) -> Self {
        Self {
            manager: None,
            entries: self
                .entries
                .iter()
                .map(|e| Entry {
                    proxy: None,
                    handle: e.handle,
                    title: e.title.clone(),
                    app_id: e.app_id.clone(),
                    closed: e.closed,
                })
                .collect(),
            next: self.next,
            finished: self.finished,
        }
    }
}
impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()> for Data {
    fn event(
        data: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            if interface == "ext_foreign_toplevel_list_v1" {
                data.manager = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}
impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for Data {
    fn event(
        data: &mut Self,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Finished => data.finished = true,
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel: _ } => {}
            _ => {}
        }
    }
    event_created_child!(Data,ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,[ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE=>(ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,())]);
}
impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for Data {
    fn event(
        data: &mut Self,
        proxy: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let index = data
            .entries
            .iter()
            .position(|entry| entry.proxy.as_ref() == Some(proxy));
        let index = index.unwrap_or_else(|| {
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
        let entry = &mut data.entries[index];
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
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
