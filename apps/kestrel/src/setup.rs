//! First-launch setup: when it is offered, what Finish applies, and the
//! features whose use the last step explains.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::FeaturePreset;

/// In the state directory: setup was offered and then finished or skipped.
const STATE_FILE: &str = "setup";
const OFFERED_CONTENTS: &str = "offered\n";
const DONE_CONTENTS: &str = "done\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    Unknown,
    Offered,
    Done,
}

/// Setup is offered until it is finished or skipped.
pub fn should_offer_setup(config_existed: bool, state: SetupState) -> bool {
    match state {
        SetupState::Done => false,
        SetupState::Offered => true,
        SetupState::Unknown => !config_existed,
    }
}

pub fn state_path() -> Option<PathBuf> {
    Some(crate::config::state_directory()?.join(STATE_FILE))
}

/// A missing or unrecognised record reads as unknown.
pub fn state(path: Option<&Path>) -> SetupState {
    let Some(path) = path else {
        return SetupState::Unknown;
    };
    match fs::read_to_string(path) {
        Ok(contents) => match contents.trim() {
            "offered" => SetupState::Offered,
            "done" => SetupState::Done,
            _ => SetupState::Unknown,
        },
        Err(_) => SetupState::Unknown,
    }
}

/// Records setup as offered so it resumes if a configuration appears.
pub fn record_offered(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = crate::private_file::write_private_atomic(path, OFFERED_CONTENTS.as_bytes())
    {
        log::debug!(target: "startup.setup", "Could not record setup: {error}");
    }
}

/// Records setup as finished or skipped; a failure means it is offered again.
pub fn record_done(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = crate::private_file::write_private_atomic(path, DONE_CONTENTS.as_bytes()) {
        log::warn!(target: "startup.setup", "Could not record setup: {error}");
    }
}

/// A feature whose enablement changes what the desktop, other apps, or the
/// network see, with one line on its use and one on what Kestrel keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensitiveFeature {
    pub feature_id: &'static str,
    pub uses: &'static str,
    pub keeps: &'static str,
}

pub const SENSITIVE_FEATURES: [SensitiveFeature; 12] = [
    SensitiveFeature {
        feature_id: "capture.screenshot",
        uses: "Uses the desktop's screenshot permission when you take one.",
        keeps: "Keeps recent captures as files in Kestrel's data folder, within your limits, until deleted.",
    },
    SensitiveFeature {
        feature_id: "capture.text",
        uses: "Takes one frame through the screenshot provider when you use it.",
        keeps: "The frame stays in memory only, and text is read on this computer.",
    },
    SensitiveFeature {
        feature_id: "capture.qr",
        uses: "Takes one frame through the screenshot provider when you use it.",
        keeps: "The frame stays in memory only, and the code is read on this computer.",
    },
    SensitiveFeature {
        feature_id: "capture.color",
        uses: "Takes one frame through the screenshot provider when you use it.",
        keeps: "The frame stays in memory only, and the colour is read from it.",
    },
    SensitiveFeature {
        feature_id: "capture.recording",
        uses: "Opens the desktop's screen-sharing dialog every time, and records audio only if you turn it on.",
        keeps: "Keeps recordings in your Videos folder.",
    },
    SensitiveFeature {
        feature_id: "camera.preview",
        uses: "Opens the camera only while the preview runs.",
        keeps: "Nothing is recorded or saved.",
    },
    SensitiveFeature {
        feature_id: "clipboard.history",
        uses: "Reads what you copy.",
        keeps: "Kept in memory only, bounded, and wiped on lock, sleep, and quit.",
    },
    SensitiveFeature {
        feature_id: "snippets.text",
        uses: "Types into the focused app when you insert a snippet.",
        keeps: "Snippets are saved in Kestrel's data folder.",
    },
    SensitiveFeature {
        feature_id: "global.shortcuts",
        uses: "Registers keys with the desktop, which may ask you to confirm them.",
        keeps: "Keeps your bindings in the config.",
    },
    SensitiveFeature {
        feature_id: "network.speed_test",
        uses: "Contacts a speed-test server, which sees your IP address, only when you start a test.",
        keeps: "Keeps the last result in memory only.",
    },
    SensitiveFeature {
        feature_id: "audio.microphone",
        uses: "Controls microphone mute and levels.",
        keeps: "Never records audio.",
    },
    SensitiveFeature {
        feature_id: "system.processes",
        uses: "Can end your own processes after you confirm.",
        keeps: "Keeps the process list in memory only; command lines and environments are never read.",
    },
];

pub fn sensitive_feature(feature_id: &str) -> Option<&'static SensitiveFeature> {
    SENSITIVE_FEATURES
        .iter()
        .find(|row| row.feature_id == feature_id)
}

/// The explanation rows for a choice: the preset's features, in registration
/// order, that appear in the sensitive table.
pub fn sensitive_rows<'a>(
    preset: FeaturePreset,
    registered: impl IntoIterator<Item = &'a str>,
) -> Vec<&'static SensitiveFeature> {
    registered
        .into_iter()
        .filter(|feature_id| preset.enables(feature_id))
        .filter_map(sensitive_feature)
        .collect()
}

/// A preset choice's summary line, with the features it turns on by label.
/// Everything names what it adds to Balanced.
pub fn preset_detail(preset: FeaturePreset, features: &[(&str, &str)]) -> String {
    if preset == FeaturePreset::Everything {
        let added = features
            .iter()
            .filter(|(feature_id, _)| {
                FeaturePreset::Everything.enables(feature_id)
                    && !FeaturePreset::Balanced.enables(feature_id)
            })
            .map(|(_, label)| *label)
            .collect::<Vec<_>>();
        return if added.is_empty() {
            "Everything in Balanced.".to_owned()
        } else {
            format!("Everything in Balanced, plus {}.", added.join(", "))
        };
    }
    let labels = features
        .iter()
        .filter(|(feature_id, _)| preset.enables(feature_id))
        .map(|(_, label)| *label)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "Turns nothing on here.".to_owned()
    } else {
        format!("Turns on {}.", labels.join(", "))
    }
}

/// The enablement Finish applies: the preset's features, minus those left
/// off. Choosing yourself returns nothing to change.
pub fn setup_features(
    preset: Option<FeaturePreset>,
    left_off: &[String],
    registered: &[&str],
) -> BTreeMap<String, bool> {
    let Some(preset) = preset else {
        return BTreeMap::new();
    };
    registered
        .iter()
        .map(|feature_id| {
            let enabled =
                preset.enables(feature_id) && !left_off.iter().any(|left| left == feature_id);
            ((*feature_id).to_owned(), enabled)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn setup_is_offered_until_it_is_finished_or_skipped() {
        // (case, configuration existed, state, offered)
        let cases = [
            ("first launch", false, SetupState::Unknown, true),
            (
                "an existing configuration",
                true,
                SetupState::Unknown,
                false,
            ),
            (
                "offered with no configuration",
                false,
                SetupState::Offered,
                true,
            ),
            (
                "closed early and a setting changed",
                true,
                SetupState::Offered,
                true,
            ),
            ("done with no configuration", false, SetupState::Done, false),
            ("done with a configuration", true, SetupState::Done, false),
        ];
        for (case, config_existed, state, offered) in cases {
            assert_eq!(should_offer_setup(config_existed, state), offered, "{case}");
        }
    }

    #[test]
    fn recording_setup_state_uses_the_private_atomic_writer() {
        let directory = TempDir::new().expect("temp dir");
        let path = directory.path().join("setup");

        assert_eq!(
            state(Some(&path)),
            SetupState::Unknown,
            "a missing record is unknown"
        );
        assert_eq!(state(None), SetupState::Unknown, "no path is unknown");

        record_offered(Some(&path));
        assert_eq!(state(Some(&path)), SetupState::Offered);
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            crate::private_file::FILE_MODE
        );

        record_done(Some(&path));
        assert_eq!(state(Some(&path)), SetupState::Done);
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            crate::private_file::FILE_MODE
        );

        fs::write(&path, "garbage\n").expect("write garbage");
        assert_eq!(
            state(Some(&path)),
            SetupState::Unknown,
            "an unrecognised record is unknown"
        );
    }

    #[test]
    fn setup_applies_the_preset_minus_features_left_off() {
        let registered = [
            "capture.screenshot",
            "capture.recording",
            "clipboard.history",
        ];
        let left_off = ["capture.recording".to_owned()];

        let balanced = setup_features(Some(FeaturePreset::Balanced), &left_off, &registered);
        assert_eq!(balanced.get("capture.screenshot"), Some(&true));
        assert_eq!(
            balanced.get("capture.recording"),
            Some(&false),
            "a feature left off stays off"
        );
        assert_eq!(
            balanced.get("clipboard.history"),
            Some(&false),
            "the rest follow Balanced"
        );

        let essentials = setup_features(Some(FeaturePreset::Essentials), &[], &registered);
        assert_eq!(essentials.get("capture.screenshot"), Some(&false));
        assert_eq!(
            setup_features(None, &left_off, &registered),
            BTreeMap::new(),
            "choosing yourself changes nothing"
        );
    }

    #[test]
    fn setup_explains_the_chosen_presets_and_their_sensitive_features() {
        let registered = [
            "capture.screenshot",
            "capture.recording",
            "clipboard.history",
            "global.shortcuts",
            "audio.microphone",
            "system.monitor",
            "snippets.text",
        ];
        let ids = |preset| {
            sensitive_rows(preset, registered)
                .into_iter()
                .map(|row| row.feature_id)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(FeaturePreset::Balanced),
            [
                "capture.screenshot",
                "capture.recording",
                "audio.microphone",
                "snippets.text"
            ]
        );
        assert_eq!(ids(FeaturePreset::Essentials), ["audio.microphone"]);
        assert_eq!(
            ids(FeaturePreset::Everything),
            [
                "capture.screenshot",
                "capture.recording",
                "clipboard.history",
                "global.shortcuts",
                "audio.microphone",
                "snippets.text"
            ],
            "a feature without a table row gets no row"
        );

        let labelled = [
            ("capture.screenshot", "Screenshots"),
            ("capture.recording", "Screen recording"),
            ("clipboard.history", "Clipboard history"),
            ("global.shortcuts", "Keyboard shortcuts"),
            ("audio.microphone", "Microphone controls"),
            ("system.monitor", "Monitoring"),
            ("snippets.text", "Snippets"),
        ];
        assert_eq!(
            preset_detail(FeaturePreset::Balanced, &labelled),
            "Turns on Screenshots, Screen recording, Microphone controls, Monitoring, Snippets."
        );
        assert_eq!(
            preset_detail(FeaturePreset::Everything, &labelled),
            "Everything in Balanced, plus Clipboard history, Keyboard shortcuts."
        );
    }
}
