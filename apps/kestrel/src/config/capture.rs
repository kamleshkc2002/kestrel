//! Capture limits, screen tools, and recording defaults.

use kestrel_core::{
    MAX_CAPTURE_MAX_AGE_HOURS, MAX_CAPTURE_MAX_ENTRIES, MAX_CAPTURE_MAX_TOTAL_MEGABYTES,
    MAX_RECORDING_FRAMERATE, MIN_RECORDING_FRAMERATE,
};

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section},
};

const OCR_LANGUAGE_MESSAGE: &str = concat!(
    "Must be Tesseract language codes such as \"eng\" or \"eng+deu\"; the default ",
    "was retained."
);
const SYSTEM_AUDIO_APP_MESSAGE: &str =
    "Must be an application identity; the whole output was retained.";

pub(super) fn parse_capture(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "capture", value) else {
        return;
    };
    // Each limit is validated alone; a bad one keeps its default.
    let limits = &mut loaded.configuration.capture;
    section.in_range(
        "max_entries",
        1..=MAX_CAPTURE_MAX_ENTRIES,
        ("Must be", " captures"),
        &mut limits.max_entries,
    );
    section.in_range(
        "max_total_megabytes",
        1..=MAX_CAPTURE_MAX_TOTAL_MEGABYTES,
        ("Must be", " MiB"),
        &mut limits.max_total_megabytes,
    );
    section.in_range(
        "max_age_hours",
        1..=MAX_CAPTURE_MAX_AGE_HOURS,
        ("Must be", " hours"),
        &mut limits.max_age_hours,
    );
    section.read(
        "after_screenshot",
        "Must be \"notify\", \"edit\", or \"nothing\"; the default was retained.",
        &mut limits.after_screenshot,
    );
    section.finish(loaded);
}

pub(super) fn parse_screen_tools(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "screen_tools", value) else {
        return;
    };
    let screen_tools = &mut loaded.configuration.screen_tools;
    if let Some(value) = section.get("ocr_language") {
        match value.as_str() {
            Some(language) if kestrel_core::valid_ocr_language(language) => {
                screen_tools.ocr_language = language.to_owned();
            }
            _ => section.warn("ocr_language", OCR_LANGUAGE_MESSAGE),
        }
    }
    section.in_range(
        "ocr_timeout_seconds",
        kestrel_core::MIN_OCR_TIMEOUT_SECONDS..=kestrel_core::MAX_OCR_TIMEOUT_SECONDS,
        ("Must be", " seconds"),
        &mut screen_tools.ocr_timeout_seconds,
    );
    section.finish(loaded);
}

pub(super) fn parse_recording(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "recording", value) else {
        return;
    };
    let recording = &mut loaded.configuration.recording;
    section.in_range(
        "framerate",
        MIN_RECORDING_FRAMERATE..=MAX_RECORDING_FRAMERATE,
        ("Must be", " frames per second"),
        &mut recording.framerate,
    );
    for (key, target) in [
        ("cursor", &mut recording.cursor),
        ("system_audio", &mut recording.system_audio),
        ("microphone", &mut recording.microphone),
    ] {
        section.read(key, BOOL_MESSAGE, target);
    }
    if let Some(value) = section.get("system_audio_app") {
        match value.as_str() {
            Some("") => {}
            Some(identity)
                if kestrel_core::valid_audio_identity_list(&[identity.to_owned()], 1) =>
            {
                recording.system_audio_app = Some(identity.to_owned());
            }
            _ => section.warn("system_audio_app", SYSTEM_AUDIO_APP_MESSAGE),
        }
    }
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::{export_string, parse};

    #[test]
    fn capture_limits_parse_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[capture]
max_entries = 0
max_total_megabytes = 512
max_age_hours = 721
"#,
        )
        .expect("document itself is valid TOML");
        let defaults = kestrel_core::CaptureConfiguration::default();
        assert_eq!(
            loaded.configuration.capture.max_entries,
            defaults.max_entries
        );
        assert_eq!(loaded.configuration.capture.max_total_megabytes, 512);
        assert_eq!(
            loaded.configuration.capture.max_age_hours,
            defaults.max_age_hours
        );
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["capture.max_entries", "capture.max_age_hours"]);

        let exported = export_string(&loaded.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.capture, loaded.configuration.capture);
    }

    #[test]
    fn malformed_capture_follow_up_keeps_notify_default() {
        let loaded = parse(
            r#"
schema_version = 3
[capture]
after_screenshot = "later"
"#,
        )
        .expect("document itself is valid TOML");
        assert_eq!(
            loaded.configuration.capture.after_screenshot,
            kestrel_core::CaptureAfterScreenshot::Notify
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "capture.after_screenshot"
                    && warning.reason.contains("default was retained"))
        );
    }

    #[test]
    fn screen_tool_settings_reject_unsafe_languages_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[screen_tools]
ocr_language = "-l eng"
ocr_timeout_seconds = 60
"#,
        )
        .expect("document itself is valid TOML");
        assert_eq!(loaded.configuration.screen_tools.ocr_language, "eng");
        assert_eq!(loaded.configuration.screen_tools.ocr_timeout_seconds, 60);
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["screen_tools.ocr_language"]);

        let mut configuration = loaded.configuration.clone();
        configuration.screen_tools.ocr_language = "eng+deu".to_owned();
        let exported = export_string(&configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.screen_tools,
            configuration.screen_tools
        );
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn recording_settings_reject_bad_values_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[recording]
framerate = 240
cursor = false
microphone = "yes"
system_audio = true
system_audio_app = "firefox"
"#,
        )
        .expect("document itself is valid TOML");
        let recording = loaded.configuration.recording.clone();
        assert_eq!(
            recording.framerate,
            kestrel_core::DEFAULT_RECORDING_FRAMERATE
        );
        assert!(!recording.cursor);
        assert!(recording.system_audio);
        assert!(!recording.microphone);
        assert_eq!(recording.system_audio_app.as_deref(), Some("firefox"));
        let mut rejected: Vec<&str> = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect();
        rejected.sort_unstable();
        assert_eq!(rejected, ["recording.framerate", "recording.microphone"]);

        let mut configuration = loaded.configuration.clone();
        configuration.recording.framerate = 60;
        let exported = export_string(&configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.recording, configuration.recording);
        assert!(reloaded.warnings.is_empty());

        // An empty string means the whole output; an invalid identity keeps it.
        let whole = parse("schema_version = 3\n[recording]\nsystem_audio_app = \"\"\n")
            .expect("document itself is valid TOML");
        assert_eq!(whole.configuration.recording.system_audio_app, None);
        assert!(whole.warnings.is_empty());
        let invalid = parse("schema_version = 3\n[recording]\nsystem_audio_app = \"bad\\u0001\"\n")
            .expect("document itself is valid TOML");
        assert_eq!(invalid.configuration.recording.system_audio_app, None);
        assert_eq!(
            invalid
                .warnings
                .iter()
                .map(|warning| warning.feature_id.as_str())
                .collect::<Vec<_>>(),
            ["recording.system_audio_app"]
        );
    }
}
