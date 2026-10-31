//! The one-time notice that closing the window leaves Kestrel running.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::CloseAction;

/// In the state directory: the close button hid the window once.
const STATE_FILE: &str = "background-notice";
const SHOWN_CONTENTS: &str = "shown\n";

pub const SUMMARY: &str = "Kestrel is still running";
pub const BODY: &str = "Monitoring, clipboard history, and recordings keep working with the \
                        window closed. To quit, use Quit Kestrel in the app's menu, the tray's \
                        Quit, or Ctrl+Q.";

/// The notice follows the first close that hides the window.
pub fn should_send_background_notice(close_action: CloseAction, already_shown: bool) -> bool {
    close_action == CloseAction::Minimize && !already_shown
}

pub fn state_path() -> Option<PathBuf> {
    Some(crate::config::state_directory()?.join(STATE_FILE))
}

/// A missing or unrecognised record reads as not shown.
pub fn shown(path: Option<&Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    matches!(fs::read_to_string(path), Ok(contents) if contents.trim() == "shown")
}

/// Records the notice so a later close sends nothing.
pub fn record_shown(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = crate::private_file::write_private_atomic(path, SHOWN_CONTENTS.as_bytes()) {
        log::warn!(target: "startup.background-notice", "Could not record the background notice: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn the_notice_follows_only_the_first_close_that_hides_the_window() {
        // (case, close action, already shown, send)
        let cases = [
            ("first minimize", CloseAction::Minimize, false, true),
            ("later minimize", CloseAction::Minimize, true, false),
            ("close quits", CloseAction::Quit, false, false),
            ("close quits after a notice", CloseAction::Quit, true, false),
        ];
        for (case, close_action, already_shown, send) in cases {
            assert_eq!(
                should_send_background_notice(close_action, already_shown),
                send,
                "{case}"
            );
        }
    }

    #[test]
    fn recording_the_notice_uses_the_private_atomic_writer() {
        let directory = TempDir::new().expect("temp dir");
        let path = directory.path().join("background-notice");

        assert!(!shown(Some(&path)), "a missing record is not shown");
        assert!(!shown(None), "no path is not shown");

        record_shown(Some(&path));
        assert!(shown(Some(&path)));
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            crate::private_file::FILE_MODE
        );
        assert_eq!(fs::read_to_string(&path).expect("contents"), "shown\n");

        fs::write(&path, "garbage\n").expect("write garbage");
        assert!(!shown(Some(&path)), "an unrecognised record is not shown");
    }
}
