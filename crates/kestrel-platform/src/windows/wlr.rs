use super::{
    WindowBackend, WindowError, WindowErrorKind, WindowHandle, WindowInfo, WindowProvider,
    bounded_text,
};
use std::{
    sync::{Arc, Mutex, mpsc},
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
#[allow(dead_code)]
pub struct WlrProvider {
    tx: mpsc::Sender<Request>,
    state: Arc<Mutex<Vec<WindowInfo>>>,
    thread: Option<JoinHandle<()>>,
}
impl WlrProvider {
    pub fn new() -> Result<Self, WindowError> {
        let conn = Connection::connect_to_env().map_err(|e| {
            WindowError::new(
                WindowErrorKind::Disconnected,
                format!("Wayland connection failed: {e}"),
            )
        })?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(Vec::new()));
        let shared = state.clone();
        let thread = thread::Builder::new()
            .name("kestrel-windows".into())
            .spawn(move || {
                let mut queue = conn.new_event_queue();
                let qh = queue.handle();
                let mut data = Data::default();
                conn.display().get_registry(&qh, ());
                let _ = queue.roundtrip(&mut data);
                let ok = data.manager.is_some() && data.seat.is_some();
                let _ = ready_tx.send(ok);
                if !ok {
                    return;
                }
                loop {
                    let _ = queue.roundtrip(&mut data);
                    if let Ok(mut s) = shared.lock() {
                        *s = snapshot(&data);
                    }
                    match rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(Request::Snapshot(r)) => {
                            let _ = r.send(Ok(snapshot(&data)));
                        }
                        Ok(Request::Activate(h, r)) => {
                            let result = command(&data, h, true);
                            let _ = r.send(result);
                        }
                        Ok(Request::Minimize(h, r)) => {
                            let result = command(&data, h, false);
                            let _ = r.send(result);
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
                "wlr foreign toplevel manager or wl_seat was not advertised",
            ));
        }
        Ok(Self {
            tx,
            state,
            thread: Some(thread),
        })
    }
}
fn snapshot(d: &Data) -> Vec<WindowInfo> {
    d.entries
        .iter()
        .filter(|e| !e.closed)
        .take(256)
        .map(|e| WindowInfo {
            handle: e.handle,
            title: e.title.clone(),
            app_id: e.app_id.clone(),
            active: e.active,
            minimized: e.minimized,
        })
        .collect()
}
fn command(d: &Data, h: WindowHandle, activate: bool) -> Result<(), WindowError> {
    let e = d
        .entries
        .iter()
        .find(|e| e.handle == h && !e.closed)
        .ok_or_else(|| {
            WindowError::new(WindowErrorKind::NotFound, "window handle was not found")
        })?;
    let seat = d
        .seat
        .as_ref()
        .ok_or_else(|| WindowError::unsupported("wlr activation requires wl_seat"))?;
    if activate {
        e.proxy.as_ref().unwrap().activate(seat)
    } else {
        e.proxy.as_ref().unwrap().set_minimized();
    }
    e.proxy.as_ref().unwrap().version();
    Ok(())
}
impl Drop for WlrProvider {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl WindowProvider for WlrProvider {
    fn backend(&self) -> WindowBackend {
        WindowBackend::WlrForeignToplevel
    }
    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
        let (a, b) = mpsc::channel();
        self.tx
            .send(Request::Snapshot(a))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        b.recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }
    fn activate(&mut self, h: WindowHandle) -> Result<(), WindowError> {
        let (a, b) = mpsc::channel();
        self.tx
            .send(Request::Activate(h, a))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        b.recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }
    fn minimize(&mut self, h: WindowHandle) -> Result<(), WindowError> {
        let (a, b) = mpsc::channel();
        self.tx
            .send(Request::Minimize(h, a))
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?;
        b.recv()
            .map_err(|_| WindowError::disconnected("Wayland worker stopped"))?
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for Data {
    fn event(
        d: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "zwlr_foreign_toplevel_manager_v1" => {
                    d.manager = Some(r.bind(name, version.min(3), q, ()))
                }
                "wl_seat" if d.seat.is_none() => d.seat = Some(r.bind(name, version.min(9), q, ())),
                _ => {}
            }
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
impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for Data {
    fn event(
        _: &mut Self,
        _: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        _: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    event_created_child!(Data,zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,[zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE=>(zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,())]);
}
impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for Data {
    fn event(
        d: &mut Self,
        p: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        e: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut i = d.entries.iter().position(|x| x.proxy.as_ref() == Some(p));
        if i.is_none() {
            let h = WindowHandle(d.next);
            d.next += 1;
            d.entries.push(Entry {
                proxy: Some(p.clone()),
                handle: h,
                title: String::new(),
                app_id: None,
                active: false,
                minimized: false,
                closed: false,
            });
            i = Some(d.entries.len() - 1);
        }
        let e0 = &mut d.entries[i.unwrap()];
        match e {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                e0.title = bounded_text(&title)
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                e0.app_id = Some(bounded_text(&app_id))
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state } => {
                e0.active = false;
                e0.minimized = false;
                for s in state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|x| u32::from_ne_bytes(*x))
                {
                    e0.active |= s == 2;
                    e0.minimized |= s == 1;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => e0.closed = true,
            _ => {}
        }
    }
}
