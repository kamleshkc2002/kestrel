//! Window discovery and bounded activation/minimization adapters.
mod cosmic;
mod ext;
mod wlr;

use std::{error::Error, fmt, sync::Arc};
use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_registry};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{
        Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask,
        Window,
    },
    rust_connection::RustConnection,
};

pub use cosmic::CosmicProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowBackend {
    X11Ewmh,
    WlrForeignToplevel,
    CosmicToplevel,
    ExtForeignToplevelList,
}
impl WindowBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::X11Ewmh => "X11 EWMH",
            Self::WlrForeignToplevel => "wlroots foreign toplevel",
            Self::CosmicToplevel => "COSMIC toplevel",
            Self::ExtForeignToplevelList => "ext foreign toplevel list",
        }
    }
    pub fn can_activate(self) -> bool {
        !matches!(self, Self::ExtForeignToplevelList)
    }
    pub fn can_minimize(self) -> bool {
        !matches!(self, Self::ExtForeignToplevelList)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub u64);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub handle: WindowHandle,
    pub title: String,
    pub app_id: Option<String>,
    pub active: bool,
    pub minimized: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowErrorKind {
    Unsupported,
    NotFound,
    Disconnected,
    Backend,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowError {
    pub kind: WindowErrorKind,
    pub message: String,
}
impl WindowError {
    fn new(kind: WindowErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    fn unsupported(message: impl Into<String>) -> Self {
        Self::new(WindowErrorKind::Unsupported, message)
    }
    fn disconnected(message: impl Into<String>) -> Self {
        Self::new(WindowErrorKind::Disconnected, message)
    }
}
impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for WindowError {}
pub trait WindowProvider: Send {
    fn backend(&self) -> WindowBackend;
    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError>;
    fn activate(&mut self, window: WindowHandle) -> Result<(), WindowError>;
    fn minimize(&mut self, window: WindowHandle) -> Result<(), WindowError>;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowProviderUnavailable {
    pub summary: String,
    pub remediation: String,
}
impl fmt::Display for WindowProviderUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.summary, self.remediation)
    }
}
impl Error for WindowProviderUnavailable {}

const MAX_WINDOWS: usize = 256;
fn bounded_text(value: &str) -> String {
    value.chars().take(MAX_WINDOWS).collect()
}

struct Globals {
    names: Vec<String>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        g: &mut Self,
        _: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = e {
            g.names.push(interface);
        }
    }
}
fn wayland_globals() -> Result<Vec<String>, WindowProviderUnavailable> {
    let connection = Connection::connect_to_env().map_err(|error| WindowProviderUnavailable {
        summary: format!("Wayland compositor connection failed: {error}"),
        remediation: "The session compositor must expose a Wayland client connection.".into(),
    })?;
    let mut queue = connection.new_event_queue();
    let queue_handle = queue.handle();
    let mut globals = Globals { names: Vec::new() };
    connection.display().get_registry(&queue_handle, ());
    queue
        .roundtrip(&mut globals)
        .map_err(|error| WindowProviderUnavailable {
            summary: format!("Wayland registry failed: {error}"),
            remediation: "Restart the compositor session.".into(),
        })?;
    Ok(globals.names)
}
pub fn discover_window_provider() -> Result<Box<dyn WindowProvider>, WindowProviderUnavailable> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let globals = wayland_globals()?;
        let has = |name: &str| globals.iter().any(|g| g == name);
        if has("zcosmic_toplevel_info_v1") && has("zcosmic_toplevel_manager_v1") {
            return cosmic::CosmicProvider::new()
                .map(|p| Box::new(p) as Box<dyn WindowProvider>)
                .map_err(|e| WindowProviderUnavailable {
                    summary: format!("COSMIC globals observed but provider failed: {}", e.message),
                    remediation: "Check COSMIC toplevel protocol permissions.".into(),
                });
        }
        if has("zwlr_foreign_toplevel_manager_v1") {
            return wlr::WlrProvider::new()
                .map(|p| Box::new(p) as Box<dyn WindowProvider>)
                .map_err(|e| WindowProviderUnavailable {
                    summary: format!("wlroots global observed but provider failed: {}", e.message),
                    remediation: "Check wl_seat and compositor protocol support.".into(),
                });
        }
        if has("ext_foreign_toplevel_list_v1") {
            return ext::ExtProvider::new()
                .map(|p| Box::new(p) as Box<dyn WindowProvider>)
                .map_err(|e| WindowProviderUnavailable {
                    summary: format!(
                        "ext foreign toplevel global observed but provider failed: {}",
                        e.message
                    ),
                    remediation: "The compositor advertised the list but rejected the client."
                        .into(),
                });
        }
        return Err(WindowProviderUnavailable {
            summary: "No supported Wayland window-list global observed (COSMIC info + management, wlroots manager, ext foreign list).".into(),
            remediation: "GNOME Shell exposes no window list to ordinary applications; use a compositor extension or X11 session.".into(),
        });
    }
    if std::env::var_os("DISPLAY").is_some() {
        return x11::X11Provider::connect()
            .map(|p| Box::new(p) as Box<dyn WindowProvider>)
            .map_err(|e| WindowProviderUnavailable {
                summary: e.message,
                remediation:
                    "The window manager must advertise _NET_CLIENT_LIST in _NET_SUPPORTED.".into(),
            });
    }
    Err(WindowProviderUnavailable {
        summary: "No WAYLAND_DISPLAY or DISPLAY observed".into(),
        remediation: "Start a graphical session.".into(),
    })
}

mod x11 {
    use super::*;
    pub struct X11Provider {
        conn: Arc<RustConnection>,
        root: Window,
        atoms: Atoms,
    }
    struct Atoms {
        supported: Atom,
        client_list: Atom,
        wm_name: Atom,
        wm_class: Atom,
        active: Atom,
        state: Atom,
        hidden: Atom,
        utf8: Atom,
        net_active: Atom,
        wm_change_state: Atom,
    }
    fn atom<C: x11rb::connection::Connection>(c: &C, name: &[u8]) -> Result<Atom, WindowError> {
        c.intern_atom(false, name)
            .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))
    }
    impl X11Provider {
        pub fn connect() -> Result<Self, WindowError> {
            let (c, _) = RustConnection::connect(None)
                .map_err(|e| WindowError::new(WindowErrorKind::Disconnected, e.to_string()))?;
            let root = c
                .setup()
                .roots
                .first()
                .ok_or_else(|| WindowError::new(WindowErrorKind::Backend, "X11 has no screen"))?
                .root;
            let a = Atoms {
                supported: atom(&c, b"_NET_SUPPORTED")?,
                client_list: atom(&c, b"_NET_CLIENT_LIST")?,
                wm_name: atom(&c, b"_NET_WM_NAME")?,
                wm_class: atom(&c, b"WM_CLASS")?,
                active: atom(&c, b"_NET_ACTIVE_WINDOW")?,
                state: atom(&c, b"_NET_WM_STATE")?,
                hidden: atom(&c, b"_NET_WM_STATE_HIDDEN")?,
                utf8: atom(&c, b"UTF8_STRING")?,
                net_active: atom(&c, b"_NET_ACTIVE_WINDOW")?,
                wm_change_state: atom(&c, b"WM_CHANGE_STATE")?,
            };
            let supported = c
                .get_property(false, root, AtomEnum::ATOM, a.supported, 0, 1024)
                .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))?
                .reply()
                .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))?;
            let advertises_client_list = supported
                .value
                .as_chunks::<4>()
                .0
                .iter()
                .any(|atom| u32::from_ne_bytes(*atom) == a.client_list);
            if !advertises_client_list {
                return Err(WindowError::unsupported(
                    "_NET_CLIENT_LIST is absent from _NET_SUPPORTED",
                ));
            }
            Ok(Self {
                conn: Arc::new(c),
                root,
                atoms: a,
            })
        }
        fn prop(&self, w: Window, property: Atom, ty: Atom) -> Option<Vec<u8>> {
            self.conn
                .get_property(false, w, property, ty, 0, 4096)
                .ok()?
                .reply()
                .ok()
                .map(|r| r.value)
        }
        fn list(&self) -> Vec<Window> {
            self.prop(self.root, self.atoms.client_list, AtomEnum::WINDOW.into())
                .unwrap_or_default()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|x| u32::from_ne_bytes(*x))
                .collect()
        }
        fn title(&self, w: Window) -> String {
            self.prop(w, self.atoms.wm_name, self.atoms.utf8)
                .or_else(|| self.prop(w, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
                .map(|v| {
                    String::from_utf8_lossy(&v)
                        .trim_end_matches('\0')
                        .to_string()
                })
                .map(|s| bounded_text(&s))
                .unwrap_or_default()
        }
        fn info(&self, w: Window, active: Window) -> WindowInfo {
            let class = self
                .prop(w, self.atoms.wm_class, AtomEnum::STRING.into())
                .and_then(|v| {
                    v.split(|b| *b == 0)
                        .next()
                        .map(|v| String::from_utf8_lossy(v).into_owned())
                })
                .filter(|s| !s.is_empty());
            let hidden = self
                .prop(w, self.atoms.state, AtomEnum::ATOM.into())
                .map(|v| {
                    v.as_chunks::<4>()
                        .0
                        .iter()
                        .any(|x| u32::from_ne_bytes(*x) == self.atoms.hidden)
                })
                .unwrap_or(false);
            WindowInfo {
                handle: WindowHandle(w as u64),
                title: self.title(w),
                app_id: class,
                active: w == active,
                minimized: hidden,
            }
        }
    }
    impl WindowProvider for X11Provider {
        fn backend(&self) -> WindowBackend {
            WindowBackend::X11Ewmh
        }
        fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
            let active = self
                .prop(self.root, self.atoms.active, AtomEnum::WINDOW.into())
                .and_then(|v| {
                    v.get(..4)
                        .map(|x| u32::from_ne_bytes([x[0], x[1], x[2], x[3]]))
                })
                .unwrap_or(0);
            Ok(self
                .list()
                .into_iter()
                .take(256)
                .map(|w| self.info(w, active))
                .collect())
        }
        fn activate(&mut self, w: WindowHandle) -> Result<(), WindowError> {
            let e = ClientMessageEvent {
                response_type: 33,
                format: 32,
                sequence: 0,
                window: w.0 as u32,
                type_: self.atoms.net_active,
                data: ClientMessageData::from([2, 0, 0, 0, 0]),
            };
            self.conn
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    e,
                )
                .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))?;
            self.conn
                .flush()
                .map_err(|e| WindowError::new(WindowErrorKind::Disconnected, e.to_string()))
        }
        fn minimize(&mut self, w: WindowHandle) -> Result<(), WindowError> {
            let e = ClientMessageEvent {
                response_type: 33,
                format: 32,
                sequence: 0,
                window: w.0 as u32,
                type_: self.atoms.wm_change_state,
                data: ClientMessageData::from([3, 0, 0, 0, 0]),
            };
            self.conn
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    e,
                )
                .map_err(|e| WindowError::new(WindowErrorKind::Backend, e.to_string()))?;
            self.conn
                .flush()
                .map_err(|e| WindowError::new(WindowErrorKind::Disconnected, e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn titles_are_bounded_on_char_boundaries() {
        assert_eq!(bounded_text(&"é".repeat(300)).chars().count(), 256);
    }
}
