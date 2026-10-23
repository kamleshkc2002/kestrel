//! Optional gtk4-layer-shell integration and placement reporting.
//!
//! `build.rs` links gtk4-layer-shell and sets `kestrel_layer_shell` only when
//! pkg-config finds it; the library must load before libwayland-client, so it
//! is linked at build time.

/// How floating surfaces are placed in this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfacePlacement {
    /// A wlr-layer-shell overlay, after the compositor advertised the protocol.
    LayerShell { protocol_version: u32 },
    /// A normal undecorated window.
    Window { reason: &'static str },
}

impl SurfacePlacement {
    /// One line for provider status: the backend used, or why it is a window.
    pub fn summary(&self) -> String {
        match self {
            Self::LayerShell { protocol_version } => {
                format!("Layer-shell overlay (wlr-layer-shell protocol {protocol_version})")
            }
            Self::Window { reason } => format!("Normal window: {reason}"),
        }
    }
}

/// Decides placement from what was compiled in and what the session offers.
pub fn choose_placement(
    compiled_in: bool,
    wayland: bool,
    protocol_version: Option<u32>,
) -> SurfacePlacement {
    match (compiled_in, wayland, protocol_version) {
        (true, true, Some(protocol_version)) => SurfacePlacement::LayerShell { protocol_version },
        (false, _, _) => SurfacePlacement::Window {
            reason: "this build has no gtk4-layer-shell; build with it installed for overlays",
        },
        (true, false, _) => SurfacePlacement::Window {
            reason: "layer-shell overlays need a Wayland session",
        },
        (true, true, None) => SurfacePlacement::Window {
            reason: "the compositor does not advertise wlr-layer-shell",
        },
    }
}

fn wayland_display() -> bool {
    use gtk::prelude::*;
    gtk::gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkWaylandDisplay")
}

#[cfg(kestrel_layer_shell)]
mod ffi {
    use std::os::raw::{c_char, c_int, c_uint, c_void};

    pub const LAYER_TOP: c_int = 2;
    pub const LAYER_OVERLAY: c_int = 3;
    pub const KEYBOARD_MODE_NONE: c_int = 0;
    pub const KEYBOARD_MODE_EXCLUSIVE: c_int = 1;
    pub const EDGE_TOP: c_int = 2;

    unsafe extern "C" {
        pub fn gtk_layer_is_supported() -> c_int;
        pub fn gtk_layer_get_protocol_version() -> c_uint;
        pub fn gtk_layer_init_for_window(window: *mut c_void);
        pub fn gtk_layer_set_layer(window: *mut c_void, layer: c_int);
        pub fn gtk_layer_set_keyboard_mode(window: *mut c_void, mode: c_int);
        pub fn gtk_layer_set_anchor(window: *mut c_void, edge: c_int, anchor: c_int);
        pub fn gtk_layer_set_margin(window: *mut c_void, edge: c_int, margin: c_int);
        pub fn gtk_layer_set_namespace(window: *mut c_void, namespace: *const c_char);
    }
}

/// Observes support at runtime; call after GTK is initialized.
#[cfg(kestrel_layer_shell)]
pub fn observed_placement() -> SurfacePlacement {
    let wayland = wayland_display();
    // SAFETY: both functions only inspect the default GDK display.
    let protocol_version = (wayland && unsafe { ffi::gtk_layer_is_supported() } != 0)
        .then(|| unsafe { ffi::gtk_layer_get_protocol_version() });
    choose_placement(true, wayland, protocol_version)
}

#[cfg(not(kestrel_layer_shell))]
pub fn observed_placement() -> SurfacePlacement {
    choose_placement(false, wayland_display(), None)
}

/// The status overlay's capability for `placement`: it needs a layer-shell
/// surface, and every other entry point keeps working without one.
pub fn overlay_capability(placement: &SurfacePlacement) -> kestrel_core::CapabilityReport {
    use kestrel_core::{CapabilityReport, CapabilityStatus};
    match placement {
        SurfacePlacement::LayerShell { protocol_version } => CapabilityReport::new(
            kestrel::OVERLAY_ID,
            CapabilityStatus::Supported,
            "The status overlay can sit at the top centre of the screen.",
        )
        .with_selected_backend(format!("wlr-layer-shell protocol {protocol_version}")),
        SurfacePlacement::Window { reason } => CapabilityReport::new(
            kestrel::OVERLAY_ID,
            CapabilityStatus::Unsupported {
                reason: (*reason).to_owned(),
            },
            "The status overlay needs a desktop that lets apps place a layer-shell surface.",
        )
        .with_remediation(
            "COSMIC, KDE Plasma, Sway, Hyprland, and other wlroots desktops support it; \
             Kestrel's window, tray, and shortcuts work without it.",
        ),
    }
}

/// Makes `window` a keyboard-exclusive overlay anchored to the top edge.
/// Call before the window is mapped, and only after `observed_placement`
/// reported layer-shell.
#[cfg(kestrel_layer_shell)]
pub fn configure_window(window: &gtk::Window, namespace: &str) {
    use gtk::glib::object::ObjectType;
    use std::ffi::CString;

    let Ok(namespace) = CString::new(namespace) else {
        return;
    };
    let window = window.as_ptr().cast::<std::os::raw::c_void>();
    // SAFETY: `window` is a live, unmapped GtkWindow and `namespace` outlives the call.
    unsafe {
        ffi::gtk_layer_init_for_window(window);
        ffi::gtk_layer_set_layer(window, ffi::LAYER_OVERLAY);
        ffi::gtk_layer_set_keyboard_mode(window, ffi::KEYBOARD_MODE_EXCLUSIVE);
        ffi::gtk_layer_set_anchor(window, ffi::EDGE_TOP, 1);
        ffi::gtk_layer_set_margin(window, ffi::EDGE_TOP, 96);
        ffi::gtk_layer_set_namespace(window, namespace.as_ptr());
    }
}

#[cfg(not(kestrel_layer_shell))]
pub fn configure_window(_window: &gtk::Window, _namespace: &str) {}

/// Makes `window` a status overlay at the top centre: above normal windows,
/// never taking the keyboard, with input confined to its own surface. Same
/// preconditions as [`configure_window`].
#[cfg(kestrel_layer_shell)]
pub fn configure_status_window(window: &gtk::Window, namespace: &str) {
    use gtk::glib::object::ObjectType;
    use std::ffi::CString;

    let Ok(namespace) = CString::new(namespace) else {
        return;
    };
    let window = window.as_ptr().cast::<std::os::raw::c_void>();
    // SAFETY: `window` is a live, unmapped GtkWindow and `namespace` outlives the call.
    unsafe {
        ffi::gtk_layer_init_for_window(window);
        ffi::gtk_layer_set_layer(window, ffi::LAYER_TOP);
        ffi::gtk_layer_set_keyboard_mode(window, ffi::KEYBOARD_MODE_NONE);
        ffi::gtk_layer_set_anchor(window, ffi::EDGE_TOP, 1);
        ffi::gtk_layer_set_margin(window, ffi::EDGE_TOP, 8);
        ffi::gtk_layer_set_namespace(window, namespace.as_ptr());
    }
}

#[cfg(not(kestrel_layer_shell))]
pub fn configure_status_window(_window: &gtk::Window, _namespace: &str) {}

#[cfg(test)]
mod tests {
    use super::{SurfacePlacement, choose_placement};

    #[test]
    fn layer_shell_is_used_only_when_built_in_and_observed_on_wayland() {
        assert_eq!(
            choose_placement(true, true, Some(4)),
            SurfacePlacement::LayerShell {
                protocol_version: 4
            }
        );
        for (compiled_in, wayland, version) in [
            (false, true, Some(4)),
            (true, false, Some(4)),
            (true, true, None),
            (false, false, None),
        ] {
            assert!(
                matches!(
                    choose_placement(compiled_in, wayland, version),
                    SurfacePlacement::Window { .. }
                ),
                "{compiled_in} {wayland} {version:?}"
            );
        }
    }
}
