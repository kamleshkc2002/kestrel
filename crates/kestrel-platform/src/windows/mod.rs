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

const MAX_WINDOWS: usize = 256;

/// Window-management protocol selected for the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowBackend {
    /// X11 Extended Window Manager Hints.
    X11Ewmh,
    /// The wlroots foreign-toplevel protocol.
    WlrForeignToplevel,
    /// COSMIC toplevel management.
    CosmicToplevel,
    /// The read-only ext foreign-toplevel list protocol.
    ExtForeignToplevelList,
}

impl WindowBackend {
    /// Returns the human-readable backend label.
    pub fn label(self) -> &'static str {
        match self {
            Self::X11Ewmh => "X11 EWMH",
            Self::WlrForeignToplevel => "wlroots foreign toplevel",
            Self::CosmicToplevel => "COSMIC toplevel",
            Self::ExtForeignToplevelList => "ext foreign toplevel list",
        }
    }

    /// Returns whether the backend can activate windows.
    pub fn can_activate(self) -> bool {
        !matches!(self, Self::ExtForeignToplevelList)
    }

    /// Returns whether the backend can minimize windows.
    pub fn can_minimize(self) -> bool {
        !matches!(self, Self::ExtForeignToplevelList)
    }
}

/// Stable identifier for a window owned by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub u64);

/// Snapshot of a discovered window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Provider-owned window identifier.
    pub handle: WindowHandle,
    /// Current window title.
    pub title: String,
    /// Application identifier when available.
    pub app_id: Option<String>,
    /// Whether the window is active.
    pub active: bool,
    /// Whether the window is minimized.
    pub minimized: bool,
}

/// Category of a window operation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowErrorKind {
    /// The compositor or protocol cannot perform the operation.
    Unsupported,
    /// The requested window no longer exists.
    NotFound,
    /// The provider's session connection stopped working.
    Disconnected,
    /// The backend returned an operational failure.
    Backend,
}

/// Typed window-provider operation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowError {
    /// Failure category.
    pub kind: WindowErrorKind,
    /// Human-readable failure detail.
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for WindowError {}

/// Interface for discovering and controlling session windows.
pub trait WindowProvider: Send {
    /// Returns the protocol backing this provider.
    fn backend(&self) -> WindowBackend;
    /// Lists currently tracked windows.
    fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError>;
    /// Requests activation of a window.
    fn activate(&mut self, window: WindowHandle) -> Result<(), WindowError>;
    /// Requests minimization of a window.
    fn minimize(&mut self, window: WindowHandle) -> Result<(), WindowError>;
}

/// Explains why no window provider is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowProviderUnavailable {
    /// Summary of the unavailable capability.
    pub summary: String,
    /// User-facing remediation guidance.
    pub remediation: String,
}

impl fmt::Display for WindowProviderUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.summary, self.remediation)
    }
}

impl Error for WindowProviderUnavailable {}

/// Bounds compositor-provided strings before they cross the platform boundary.
fn bounded_text(value: &str) -> String {
    value.chars().take(MAX_WINDOWS).collect()
}

struct Globals {
    names: Vec<String>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        globals: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _user_data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event {
            globals.names.push(interface);
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

/// Discovers the best window provider for the active graphical session.
pub fn discover_window_provider() -> Result<Box<dyn WindowProvider>, WindowProviderUnavailable> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let globals = wayland_globals()?;
        let has_global = |name: &str| globals.iter().any(|global| global == name);
        if has_global("zcosmic_toplevel_info_v1") && has_global("zcosmic_toplevel_manager_v1") {
            return cosmic::CosmicProvider::new()
                .map(|provider| Box::new(provider) as Box<dyn WindowProvider>)
                .map_err(|error| WindowProviderUnavailable {
                    summary: format!(
                        "COSMIC globals observed but provider failed: {}",
                        error.message
                    ),
                    remediation: "Check COSMIC toplevel protocol permissions.".into(),
                });
        }
        if has_global("zwlr_foreign_toplevel_manager_v1") {
            return wlr::WlrProvider::new()
                .map(|provider| Box::new(provider) as Box<dyn WindowProvider>)
                .map_err(|error| WindowProviderUnavailable {
                    summary: format!(
                        "wlroots global observed but provider failed: {}",
                        error.message
                    ),
                    remediation: "Check wl_seat and compositor protocol support.".into(),
                });
        }
        if has_global("ext_foreign_toplevel_list_v1") {
            return ext::ExtProvider::new()
                .map(|provider| Box::new(provider) as Box<dyn WindowProvider>)
                .map_err(|error| WindowProviderUnavailable {
                    summary: format!(
                        "ext foreign toplevel global observed but provider failed: {}",
                        error.message
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
            .map(|provider| Box::new(provider) as Box<dyn WindowProvider>)
            .map_err(|error| WindowProviderUnavailable {
                summary: error.message,
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

    const X11_PROPERTY_WORD_LIMIT: u32 = 4096;
    const X11_SUPPORTED_WORD_LIMIT: u32 = 1024;
    const X11_WORD_BYTES: usize = 4;
    const X11_CLIENT_MESSAGE_RESPONSE_TYPE: u8 = 33;
    const X11_CLIENT_MESSAGE_FORMAT: u8 = 32;
    const X11_ACTIVE_SOURCE: u32 = 2;
    const X11_MINIMIZE_SOURCE: u32 = 3;
    const X11_ACTIVE_DATA: [u32; 5] = [X11_ACTIVE_SOURCE, 0, 0, 0, 0];
    const X11_MINIMIZE_DATA: [u32; 5] = [X11_MINIMIZE_SOURCE, 0, 0, 0, 0];

    pub struct X11Provider {
        connection: Arc<RustConnection>,
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

    fn atom<C: x11rb::connection::Connection>(
        connection: &C,
        name: &[u8],
    ) -> Result<Atom, WindowError> {
        connection
            .intern_atom(false, name)
            .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))
    }

    impl X11Provider {
        pub fn connect() -> Result<Self, WindowError> {
            let (connection, _screen) = RustConnection::connect(None).map_err(|error| {
                WindowError::new(WindowErrorKind::Disconnected, error.to_string())
            })?;
            let root = connection
                .setup()
                .roots
                .first()
                .ok_or_else(|| WindowError::new(WindowErrorKind::Backend, "X11 has no screen"))?
                .root;
            let atoms = Atoms {
                supported: atom(&connection, b"_NET_SUPPORTED")?,
                client_list: atom(&connection, b"_NET_CLIENT_LIST")?,
                wm_name: atom(&connection, b"_NET_WM_NAME")?,
                wm_class: atom(&connection, b"WM_CLASS")?,
                active: atom(&connection, b"_NET_ACTIVE_WINDOW")?,
                state: atom(&connection, b"_NET_WM_STATE")?,
                hidden: atom(&connection, b"_NET_WM_STATE_HIDDEN")?,
                utf8: atom(&connection, b"UTF8_STRING")?,
                net_active: atom(&connection, b"_NET_ACTIVE_WINDOW")?,
                wm_change_state: atom(&connection, b"WM_CHANGE_STATE")?,
            };
            let supported = connection
                .get_property(
                    false,
                    root,
                    AtomEnum::ATOM,
                    atoms.supported,
                    0,
                    X11_SUPPORTED_WORD_LIMIT,
                )
                .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))?
                .reply()
                .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))?;
            let advertises_client_list = supported
                .value
                .as_chunks::<X11_WORD_BYTES>()
                .0
                .iter()
                .any(|bytes| u32::from_ne_bytes(*bytes) == atoms.client_list);
            if !advertises_client_list {
                return Err(WindowError::unsupported(
                    "_NET_CLIENT_LIST is absent from _NET_SUPPORTED",
                ));
            }
            Ok(Self {
                connection: Arc::new(connection),
                root,
                atoms,
            })
        }

        fn property(&self, window: Window, property: Atom, atom_type: Atom) -> Option<Vec<u8>> {
            self.connection
                .get_property(
                    false,
                    window,
                    property,
                    atom_type,
                    0,
                    X11_PROPERTY_WORD_LIMIT,
                )
                .ok()?
                .reply()
                .ok()
                .map(|reply| reply.value)
        }

        fn list(&self) -> Vec<Window> {
            self.property(self.root, self.atoms.client_list, AtomEnum::WINDOW.into())
                .unwrap_or_default()
                .as_chunks::<X11_WORD_BYTES>()
                .0
                .iter()
                .map(|bytes| u32::from_ne_bytes(*bytes))
                .collect()
        }

        fn title(&self, window: Window) -> String {
            self.property(window, self.atoms.wm_name, self.atoms.utf8)
                .or_else(|| {
                    self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into())
                })
                .map(|value| {
                    String::from_utf8_lossy(&value)
                        .trim_end_matches('\0')
                        .to_string()
                })
                .map(|title| bounded_text(&title))
                .unwrap_or_default()
        }

        fn info(&self, window: Window, active_window: Window) -> WindowInfo {
            let app_id = self
                .property(window, self.atoms.wm_class, AtomEnum::STRING.into())
                .and_then(|value| {
                    value
                        .split(|byte| *byte == 0)
                        .next()
                        .map(|class_name| String::from_utf8_lossy(class_name).into_owned())
                })
                .filter(|class_name| !class_name.is_empty());
            let minimized = self
                .property(window, self.atoms.state, AtomEnum::ATOM.into())
                .map(|value| {
                    value
                        .as_chunks::<X11_WORD_BYTES>()
                        .0
                        .iter()
                        .any(|bytes| u32::from_ne_bytes(*bytes) == self.atoms.hidden)
                })
                .unwrap_or(false);
            WindowInfo {
                handle: WindowHandle(window as u64),
                title: self.title(window),
                app_id,
                active: window == active_window,
                minimized,
            }
        }
    }

    impl WindowProvider for X11Provider {
        fn backend(&self) -> WindowBackend {
            WindowBackend::X11Ewmh
        }

        fn windows(&mut self) -> Result<Vec<WindowInfo>, WindowError> {
            let active_window = self
                .property(self.root, self.atoms.active, AtomEnum::WINDOW.into())
                .and_then(|value| {
                    value
                        .get(..X11_WORD_BYTES)
                        .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                })
                .unwrap_or(0);
            Ok(self
                .list()
                .into_iter()
                .take(MAX_WINDOWS)
                .map(|window| self.info(window, active_window))
                .collect())
        }

        fn activate(&mut self, window: WindowHandle) -> Result<(), WindowError> {
            let message = ClientMessageEvent {
                response_type: X11_CLIENT_MESSAGE_RESPONSE_TYPE,
                format: X11_CLIENT_MESSAGE_FORMAT,
                sequence: 0,
                window: window.0 as u32,
                type_: self.atoms.net_active,
                data: ClientMessageData::from(X11_ACTIVE_DATA),
            };
            self.connection
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    message,
                )
                .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))?;
            self.connection
                .flush()
                .map_err(|error| WindowError::new(WindowErrorKind::Disconnected, error.to_string()))
        }

        fn minimize(&mut self, window: WindowHandle) -> Result<(), WindowError> {
            let message = ClientMessageEvent {
                response_type: X11_CLIENT_MESSAGE_RESPONSE_TYPE,
                format: X11_CLIENT_MESSAGE_FORMAT,
                sequence: 0,
                window: window.0 as u32,
                type_: self.atoms.wm_change_state,
                data: ClientMessageData::from(X11_MINIMIZE_DATA),
            };
            self.connection
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    message,
                )
                .map_err(|error| WindowError::new(WindowErrorKind::Backend, error.to_string()))?;
            self.connection
                .flush()
                .map_err(|error| WindowError::new(WindowErrorKind::Disconnected, error.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_bounded_on_char_boundaries() {
        assert_eq!(bounded_text(&"é".repeat(300)).chars().count(), MAX_WINDOWS);
    }
}
