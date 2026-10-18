//! Application window decision helpers.

use super::*;

/// What running an application result does, given its open windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationWindowDecision {
    Hide(WindowHandle),
    Restore(WindowHandle),
    Launch,
}

/// Kestrel's own windows never appear as results or count as the front window.
pub(super) fn is_kestrel_window(app_id: Option<&str>) -> bool {
    app_id.is_some_and(|app_id| {
        app_id.eq_ignore_ascii_case(kestrel_core::APPLICATION_ID)
            || app_id.eq_ignore_ascii_case("kestrel")
    })
}

fn normalized_window_id(value: &str) -> String {
    value
        .strip_suffix(".desktop")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

pub fn decide_application_window_action(
    windows: &[WindowInfo],
    last_external_active: Option<WindowHandle>,
    application: &ApplicationEntry,
) -> ApplicationWindowDecision {
    let desktop_id = normalized_window_id(&application.id);
    let startup_class = application
        .startup_wm_class
        .as_deref()
        .map(normalized_window_id);
    let matched = windows.iter().find(|window| {
        window.app_id.as_deref().is_some_and(|app_id| {
            let app_id = normalized_window_id(app_id);
            app_id == desktop_id || startup_class.as_deref() == Some(app_id.as_str())
        })
    });
    let Some(window) = matched else {
        return ApplicationWindowDecision::Launch;
    };
    if window.minimized {
        ApplicationWindowDecision::Restore(window.handle)
    } else if last_external_active == Some(window.handle) || window.active {
        ApplicationWindowDecision::Hide(window.handle)
    } else {
        ApplicationWindowDecision::Restore(window.handle)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::application;
    use super::*;
    use kestrel_platform::windows::{WindowHandle, WindowInfo};
    #[test]
    fn application_window_decision_hides_restores_and_launches() {
        let application = application("org.example.Editor.desktop", "Editor", false);
        let window = WindowInfo {
            handle: WindowHandle(9),
            title: "Editor".to_owned(),
            app_id: Some("org.example.editor".to_owned()),
            active: true,
            minimized: false,
        };
        let windows = [window.clone()];
        assert_eq!(
            decide_application_window_action(&windows, Some(WindowHandle(9)), &application),
            ApplicationWindowDecision::Hide(WindowHandle(9))
        );
        let mut restored = window;
        restored.active = false;
        restored.minimized = true;
        assert_eq!(
            decide_application_window_action(&[restored], Some(WindowHandle(1)), &application),
            ApplicationWindowDecision::Restore(WindowHandle(9))
        );
        assert_eq!(
            decide_application_window_action(&[], None, &application),
            ApplicationWindowDecision::Launch
        );
    }
}
