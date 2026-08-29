//! Disposable Phase 0 probe for Kestrel Issue #5.
//!
//! The default mode never retrieves clipboard contents. The explicit lifecycle
//! mode only operates when the target selection is proven empty and reports
//! booleans rather than clipboard data.

use serde_json::{json, Value};
use std::{
    env,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use wl_clipboard_rs::paste::{
    get_mime_types, ClipboardType as WaylandClipboardType, Error as WaylandPasteError,
    Seat as WaylandSeat,
};
use x11rb::{protocol::xproto::ConnectionExt, rust_connection::RustConnection};

const CHILD_TIMEOUT: Duration = Duration::from_secs(3);
const WRITER_LIFETIME: Duration = Duration::from_millis(1_200);
const WRITER_STARTUP_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug)]
enum Backend {
    Wayland,
    X11,
}

impl Backend {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "wayland" => Some(Self::Wayland),
            "x11" => Some(Self::X11),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }

    fn restrict_environment(self) {
        match self {
            Self::Wayland => {
                env::remove_var("DISPLAY");
            }
            Self::X11 => {
                env::remove_var("WAYLAND_DISPLAY");
                env::remove_var("WAYLAND_SOCKET");
            }
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn command_exists(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(name))
            .any(|candidate| candidate.is_file())
    })
}

fn capability(
    feature_id: &str,
    status: &str,
    summary: &str,
    selected_backend: &str,
    remediation: Option<&str>,
    evidence: Value,
) -> Value {
    json!({
        "feature_id": feature_id,
        "status": status,
        "summary": summary,
        "selected_backend": selected_backend,
        "alternatives_considered": [
            "Wayland ext-data-control or wlr-data-control",
            "X11 CLIPBOARD selection through XWayland or Xorg",
            "desktop clipboard manager",
        ],
        "remediation": remediation,
        "evidence": evidence,
    })
}

fn wayland_selection_state() -> Result<Option<usize>, &'static str> {
    match get_mime_types(WaylandClipboardType::Regular, WaylandSeat::Unspecified) {
        Ok(types) => Ok(Some(types.len())),
        Err(WaylandPasteError::ClipboardEmpty) => Ok(Some(0)),
        Err(WaylandPasteError::NoSeats) => Err("no_seats"),
        Err(WaylandPasteError::MissingProtocol { .. }) => Err("missing_data_control_protocol"),
        Err(WaylandPasteError::SocketOpenError(_)) => Err("socket_open_failed"),
        Err(WaylandPasteError::WaylandConnection(_)) => Err("connection_failed"),
        Err(WaylandPasteError::WaylandCommunication(_)) => Err("communication_failed"),
        Err(WaylandPasteError::PrimarySelectionUnsupported) => Err("primary_unsupported"),
        Err(WaylandPasteError::NoMimeType) => Err("no_mime_type"),
        Err(WaylandPasteError::SeatNotFound) => Err("seat_not_found"),
        Err(WaylandPasteError::PipeCreation(_)) => Err("pipe_creation_failed"),
    }
}

fn wayland_capability() -> Value {
    match wayland_selection_state() {
        Ok(Some(mime_type_count)) => capability(
            "clipboard.wayland",
            "Supported",
            "Wayland data-control can inspect the regular clipboard selection without reading its contents.",
            "Wayland data-control",
            None,
            json!({
                "session_socket_present": env::var_os("WAYLAND_DISPLAY").is_some(),
                "selection_empty": mime_type_count == 0,
                "offered_mime_type_count": mime_type_count,
                "clipboard_content_read": false,
            }),
        ),
        Ok(None) => unreachable!("Wayland selection state always includes a MIME count"),
        Err("no_seats") => capability(
            "clipboard.wayland",
            "Limited",
            "Wayland data-control is reachable but no input seat is available for clipboard selection access.",
            "Wayland data-control",
            Some("Start the probe from an active graphical user session with an input seat."),
            json!({ "error_category": "no_seats" }),
        ),
        Err("missing_data_control_protocol") => capability(
            "clipboard.wayland",
            "Unsupported",
            "The compositor does not expose ext-data-control or wlr-data-control.",
            "Wayland data-control",
            Some("Use a compositor with a supported data-control protocol, or use the X11 compatibility backend when available."),
            json!({ "error_category": "missing_data_control_protocol" }),
        ),
        Err(error_category) => capability(
            "clipboard.wayland",
            "Unsupported",
            "Wayland clipboard inspection could not establish a usable session connection.",
            "Wayland data-control",
            Some("Run Kestrel in the active graphical session and re-probe after the compositor is available."),
            json!({ "error_category": error_category }),
        ),
    }
}

fn x11_selection_owner() -> Result<u32, &'static str> {
    let (connection, _) = RustConnection::connect(None).map_err(|_| "connection_failed")?;
    let clipboard = connection
        .intern_atom(false, b"CLIPBOARD")
        .map_err(|_| "atom_request_failed")?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|_| "atom_reply_failed")?;
    let owner = connection
        .get_selection_owner(clipboard)
        .map_err(|_| "request_failed")?
        .reply()
        .map_err(|_| "reply_failed")?
        .owner;
    Ok(owner)
}

fn x11_capability() -> Value {
    match x11_selection_owner() {
        Ok(owner) => capability(
            "clipboard.x11",
            "Supported",
            "The X11 CLIPBOARD selection is reachable.",
            "X11 selection",
            None,
            json!({
                "display_present": env::var_os("DISPLAY").is_some(),
                "selection_empty": owner == 0,
                "selection_owner_present": owner != 0,
                "clipboard_content_read": false,
            }),
        ),
        Err(error_category) => capability(
            "clipboard.x11",
            "Unsupported",
            "The X11 clipboard compatibility path is unavailable.",
            "X11 selection",
            Some("Run Kestrel with access to an Xorg or XWayland DISPLAY, or rely on a supported Wayland data-control path."),
            json!({ "error_category": error_category }),
        ),
    }
}

fn privacy_capabilities() -> Vec<Value> {
    vec![
        capability(
            "clipboard.history",
            "Limited",
            "Clipboard history must remain disabled until the user explicitly enables it.",
            "Kestrel policy",
            Some("Enable history only after configuring item-size, retention, immediate-wipe, and lock/sleep-clear policies."),
            json!({
                "history_default": "disabled",
                "persistent_storage_default": false,
                "required_before_persistence": [
                    "maximum_item_bytes",
                    "maximum_item_count_or_age",
                    "immediate_wipe",
                    "clear_on_lock_or_sleep",
                ],
            }),
        ),
        capability(
            "clipboard.application-exclusion",
            "Limited",
            "Standard Wayland and X11 clipboard protocols do not reliably disclose the source application or enforce history exclusion.",
            "Protocol privacy boundary",
            Some("Expose per-application exclusions only where a desktop-specific, verifiable source identity is available; otherwise label them unavailable."),
            json!({
                "standard_source_application_identity": false,
                "history_manager_exclusion_enforcement": false,
                "clipboard_content_read": false,
            }),
        ),
        capability(
            "clipboard.portal",
            "Unsupported",
            "No standard clipboard portal is selected as a Kestrel backend in this session.",
            "None",
            Some("Use Wayland data-control or the X11 compatibility backend; portal availability must be probed separately if a desktop adds a clipboard-specific interface."),
            json!({ "portal_permission_requested": false }),
        ),
    ]
}

fn build_report() -> Value {
    let mut capabilities = vec![wayland_capability(), x11_capability()];
    capabilities.extend(privacy_capabilities());
    json!({
        "probe": "clipboard",
        "phase": "0",
        "read_only": true,
        "observed_at_unix_ms": now_ms(),
        "transport": {
            "phase_0_only": true,
            "wayland_socket_present": env::var_os("WAYLAND_DISPLAY").is_some(),
            "x11_display_present": env::var_os("DISPLAY").is_some(),
            "external_tools": {
                "wl-copy": command_exists("wl-copy"),
                "wl-paste": command_exists("wl-paste"),
                "xclip": command_exists("xclip"),
                "xsel": command_exists("xsel"),
            },
            "clipboard_content_read": false,
        },
        "capabilities": capabilities,
    })
}

fn sentinel() -> String {
    format!("kestrel-clipboard-phase-0-{}", now_ms())
}

fn clipboard_write(backend: Backend, marker: &str) -> Result<(), ()> {
    backend.restrict_environment();
    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
    clipboard.set_text(marker).map_err(|_| ())?;
    thread::sleep(WRITER_LIFETIME);
    Ok(())
}

fn marker_matches(backend: Backend, marker: &str) -> Result<bool, ()> {
    backend.restrict_environment();
    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
    clipboard
        .get_text()
        .map(|text| text == marker)
        .map_err(|_| ())
}

fn clear_if_marker_matches(backend: Backend, marker: &str) -> Result<bool, ()> {
    backend.restrict_environment();
    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
    if clipboard.get_text().map_err(|_| ())? != marker {
        return Ok(false);
    }
    clipboard.clear().map_err(|_| ())?;
    Ok(true)
}

fn selection_is_empty(backend: Backend) -> Result<bool, &'static str> {
    match backend {
        Backend::Wayland => wayland_selection_state().map(|state| state == Some(0)),
        Backend::X11 => x11_selection_owner().map(|owner| owner == 0),
    }
}

fn wait_for_child(child: &mut Child) -> Result<ExitStatus, ()> {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait().map_err(|_| ())? {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                return child.wait().map_err(|_| ());
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn child_status(arguments: &[&str]) -> Result<ExitStatus, ()> {
    let executable = env::current_exe().map_err(|_| ())?;
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    wait_for_child(&mut child)
}

fn lifecycle_report(backend: Backend) -> Value {
    let initial_empty = selection_is_empty(backend);
    let Ok(true) = initial_empty else {
        let reason = match initial_empty {
            Ok(false) => "selection_not_empty",
            Err(error_category) => error_category,
            Ok(true) => unreachable!("handled above"),
        };
        return json!({
            "backend": backend.name(),
            "mode": "skipped",
            "reason": reason,
            "preexisting_clipboard_content_read": false,
            "generated_marker_compared_in_memory": false,
            "restored": true,
        });
    };

    let marker = sentinel();
    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(_) => {
            return json!({
                "backend": backend.name(),
                "mode": "failed",
                "error_category": "current_executable_unavailable",
                "restored": false,
            });
        }
    };
    let mut writer = match Command::new(executable)
        .args(["--internal-write", backend.name(), &marker])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(writer) => writer,
        Err(_) => {
            return json!({
                "backend": backend.name(),
                "mode": "failed",
                "error_category": "writer_spawn_failed",
                "restored": false,
            });
        }
    };

    thread::sleep(WRITER_STARTUP_DELAY);
    let available_while_owner_alive = child_status(&["--internal-match", backend.name(), &marker])
        .map(|status| status.success())
        .unwrap_or(false);
    let writer_exited_cleanly = wait_for_child(&mut writer)
        .map(|status| status.success())
        .unwrap_or(false);
    let available_after_owner_exit = child_status(&["--internal-match", backend.name(), &marker])
        .map(|status| status.success())
        .unwrap_or(false);
    let cleanup = child_status(&["--internal-clear-if-match", backend.name(), &marker]);
    let cleanup_result = match cleanup {
        Ok(status) if status.success() => "cleared_generated_marker",
        Ok(_) => "preserved_newer_or_unreadable_selection",
        Err(_) => "cleanup_failed",
    };
    let empty_after_cleanup = selection_is_empty(backend).unwrap_or(false);

    json!({
        "backend": backend.name(),
        "mode": "attempted",
        "initial_selection_empty": true,
        "available_while_owner_alive": available_while_owner_alive,
        "writer_exited_cleanly": writer_exited_cleanly,
        "available_after_owner_exit": available_after_owner_exit,
        "cleanup": cleanup_result,
        "empty_after_cleanup": empty_after_cleanup,
        "preexisting_clipboard_content_read": false,
        "generated_marker_compared_in_memory": true,
        "restored": empty_after_cleanup,
    })
}

fn internal_command(arguments: &[String]) -> bool {
    if arguments.len() != 3 {
        return false;
    }
    let Some(backend) = Backend::parse(&arguments[1]) else {
        return false;
    };
    let marker = &arguments[2];
    match arguments[0].as_str() {
        "--internal-write" => clipboard_write(backend, marker).is_ok(),
        "--internal-match" => marker_matches(backend, marker).unwrap_or(false),
        "--internal-clear-if-match" => clear_if_marker_matches(backend, marker).unwrap_or(false),
        _ => false,
    }
}

fn requested_backends(arguments: &[String]) -> Option<Vec<Backend>> {
    match arguments {
        [] => Some(Vec::new()),
        [flag, backend] if flag == "--exercise-lifecycle" => match backend.as_str() {
            "all" => Some(vec![Backend::Wayland, Backend::X11]),
            value => Backend::parse(value).map(|backend| vec![backend]),
        },
        _ => None,
    }
}

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|argument| argument.starts_with("--internal-"))
    {
        std::process::exit(if internal_command(&arguments) { 0 } else { 1 });
    }

    let Some(backends) = requested_backends(&arguments) else {
        eprintln!("usage: kestrel-clipboard-spike [--exercise-lifecycle wayland|x11|all]");
        std::process::exit(2);
    };

    let mut report = build_report();
    if !backends.is_empty() {
        report["read_only"] = Value::Bool(false);
        report["lifecycle"] = Value::Array(backends.into_iter().map(lifecycle_report).collect());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report must serialize")
    );
}

#[cfg(test)]
mod tests {
    use super::{requested_backends, Backend};

    #[test]
    fn parses_requested_backends() {
        assert!(requested_backends(&[]).is_some());
        assert!(matches!(
            requested_backends(&[
                "--exercise-lifecycle".to_string(),
                "wayland".to_string(),
            ]),
            Some(backends) if matches!(backends.as_slice(), [Backend::Wayland])
        ));
        assert!(
            requested_backends(&["--exercise-lifecycle".to_string(), "invalid".to_string(),])
                .is_none()
        );
    }
}
