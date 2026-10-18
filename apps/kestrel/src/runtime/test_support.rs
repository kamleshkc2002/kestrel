//! Shared test fixtures for runtime feature modules.

use kestrel_platform::applications::ApplicationEntry;

pub(super) fn application(id: &str, name: &str, terminal: bool) -> ApplicationEntry {
    ApplicationEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        comment: None,
        startup_wm_class: None,
        command: vec![name.to_lowercase()],
        terminal,
        actions: Vec::new(),
        source: None,
    }
}
