//! Optional gtk4-layer-shell integration and placement reporting.

#[cfg(kestrel_layer_shell)]
use gtk::glib::object::ObjectType;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfacePlacement {
    LayerShell { protocol_version: u32 },
    Transient { reason: String },
}

pub fn choose_placement(
    compiled_in: bool,
    supported: bool,
    protocol_version: u32,
) -> SurfacePlacement {
    if compiled_in && supported {
        SurfacePlacement::LayerShell { protocol_version }
    } else if !compiled_in {
        SurfacePlacement::Transient {
            reason: "Built without gtk4-layer-shell; floating surfaces use a normal window".into(),
        }
    } else {
        SurfacePlacement::Transient {
            reason: "The compositor offers no wlr-layer-shell; using a normal window".into(),
        }
    }
}

#[cfg(kestrel_layer_shell)]
mod ffi {
    use std::os::raw::{c_char, c_int, c_uint, c_void};

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

#[cfg(kestrel_layer_shell)]
pub fn observed_placement() -> SurfacePlacement {
    let supported = unsafe { ffi::gtk_layer_is_supported() != 0 };
    let protocol_version = unsafe { ffi::gtk_layer_get_protocol_version() };
    choose_placement(true, supported, protocol_version)
}

#[cfg(not(kestrel_layer_shell))]
pub fn observed_placement() -> SurfacePlacement {
    choose_placement(false, false, 0)
}

#[cfg(kestrel_layer_shell)]
pub fn configure_window(window: &gtk::Window, namespace: &str) {
    use std::ffi::CString;
    let namespace = CString::new(namespace).expect("static layer-shell namespace");
    let ptr = window.as_ptr() as *mut std::os::raw::c_void;
    unsafe {
        ffi::gtk_layer_init_for_window(ptr);
        ffi::gtk_layer_set_layer(ptr, 3); // OVERLAY
        ffi::gtk_layer_set_keyboard_mode(ptr, 1); // EXCLUSIVE
        ffi::gtk_layer_set_anchor(ptr, 0, 1); // TOP
        ffi::gtk_layer_set_margin(ptr, 0, 24);
        ffi::gtk_layer_set_namespace(ptr, namespace.as_ptr());
    }
}

#[cfg(not(kestrel_layer_shell))]
pub fn configure_window(_window: &gtk::Window, _namespace: &str) {}

#[cfg(test)]
mod tests {
    use super::{SurfacePlacement, choose_placement};

    #[test]
    fn placement_uses_layer_shell_only_when_observed() {
        assert_eq!(
            choose_placement(true, true, 4),
            SurfacePlacement::LayerShell {
                protocol_version: 4
            }
        );
        assert!(matches!(
            choose_placement(true, false, 0),
            SurfacePlacement::Transient { .. }
        ));
        assert!(matches!(
            choose_placement(false, true, 4),
            SurfacePlacement::Transient { .. }
        ));
    }
}
