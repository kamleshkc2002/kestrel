//! This release's notes, shipped with Kestrel, and the version the user last ran.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use kestrel_core::ApplicationConfiguration;
use serde::Deserialize;

const NOTES: &str = include_str!("../../../data/release-notes.toml");
/// In the state directory: the version the user last ran.
const STATE_FILE: &str = "last-version";
/// The feature name of notes that concern every user.
pub const APP_WIDE: &str = "kestrel";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One user-visible change in this release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Change {
    /// A registered feature ID, or [`APP_WIDE`].
    pub feature: String,
    pub text: String,
}

#[derive(Deserialize)]
struct NotesFile {
    change: Vec<Change>,
}

/// The notes compiled into this build, in file order.
pub fn shipped_changes() -> Vec<Change> {
    toml::from_str::<NotesFile>(NOTES)
        .expect("the shipped release notes are valid")
        .change
}

pub fn state_path() -> Option<PathBuf> {
    Some(crate::config::state_directory()?.join(STATE_FILE))
}

/// A missing or unreadable file reads as a first run.
pub fn read_last_version(path: &Path) -> Option<String> {
    let version = fs::read_to_string(path).ok()?.trim().to_owned();
    (!version.is_empty()).then_some(version)
}

pub fn write_last_version(path: &Path, version: &str) -> io::Result<()> {
    crate::private_file::write_private_atomic(path, format!("{version}\n").as_bytes())
}

/// Records this build's version; a failure only means the notes may be
/// offered again after the next start.
pub fn record_current_version(path: Option<&Path>) {
    let Some(path) = path else { return };
    if let Err(error) = write_last_version(path, CURRENT_VERSION) {
        log::warn!(target: "startup.whats_new", "Could not record the version: {error}");
    }
}

/// App-wide notes concern everyone; a feature's notes, those who have it on.
pub fn in_use(configuration: &ApplicationConfiguration, feature: &str) -> bool {
    feature == APP_WIDE || configuration.feature_enabled(feature)
}

/// Whether Overview offers this release's notes after an update. A first run,
/// an update that changes nothing in use, or a switched-off view records the
/// version at once (`true` in the second value); otherwise it is recorded
/// when the notes are opened.
pub fn offer_after_update(
    last_version: Option<&str>,
    current: &str,
    changes: &[Change],
    offer: bool,
    in_use: impl Fn(&str) -> bool,
) -> (bool, bool) {
    match last_version {
        None => (false, true),
        Some(last) if last == current => (false, false),
        Some(_) => {
            let pending = offer && changes.iter().any(|change| in_use(&change.feature));
            (pending, !pending)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_offered_only_after_an_update_that_changes_a_feature_in_use() {
        let changes = [Change {
            feature: "capture.screenshot".to_owned(),
            text: String::new(),
        }];
        let previous = Some("0.4.0-preview.1");
        // (case, last version, offer, feature in use, offered, recorded now)
        let cases = [
            ("first run", None, true, "capture.screenshot", false, true),
            (
                "update changes a feature in use",
                previous,
                true,
                "capture.screenshot",
                true,
                false,
            ),
            (
                "update changes nothing in use",
                previous,
                true,
                "audio.mixer",
                false,
                true,
            ),
            (
                "view switched off",
                previous,
                false,
                "capture.screenshot",
                false,
                true,
            ),
            (
                "same version",
                Some(CURRENT_VERSION),
                true,
                "capture.screenshot",
                false,
                false,
            ),
        ];
        for (case, last, offer, used, offered, recorded) in cases {
            assert_eq!(
                offer_after_update(last, CURRENT_VERSION, &changes, offer, |feature| {
                    feature == used
                }),
                (offered, recorded),
                "{case}"
            );
        }
    }
}
