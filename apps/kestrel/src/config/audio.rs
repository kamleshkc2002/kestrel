//! Audio mixer preferences and device rankings.

use kestrel_core::{MAX_AUDIO_BOOST_PERCENT, UNAMPLIFIED_AUDIO_VOLUME_PERCENT};
use serde::Deserialize;

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section},
};

const OUTPUT_SWITCH_MESSAGE: &str = "Output switch mode is invalid and was ignored";
const DISCONNECT_POLICY_MESSAGE: &str = "Disconnect policy is invalid and was ignored";
const INACTIVE_STREAMS_MESSAGE: &str = "The inactive-stream preference is invalid and was ignored";

pub(super) fn parse_audio(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "audio", value) else {
        return;
    };
    let audio = &mut loaded.configuration.audio;
    section.in_range(
        "boost_percent",
        UNAMPLIFIED_AUDIO_VOLUME_PERCENT..=MAX_AUDIO_BOOST_PERCENT,
        ("Boost ceiling must be", " percent"),
        &mut audio.boost_percent,
    );
    section.read_detailed(
        "output_switch",
        OUTPUT_SWITCH_MESSAGE,
        &mut audio.output_switch,
    );
    section.read_detailed(
        "disconnect_policy",
        DISCONNECT_POLICY_MESSAGE,
        &mut audio.disconnect_policy,
    );
    section.in_range(
        "disconnect_volume_percent",
        0..=UNAMPLIFIED_AUDIO_VOLUME_PERCENT,
        ("Disconnect volume must be", " percent"),
        &mut audio.disconnect_volume_percent,
    );
    section.read_detailed(
        "include_inactive_streams",
        INACTIVE_STREAMS_MESSAGE,
        &mut audio.include_inactive_streams,
    );
    for (key, limit) in [
        ("output_priority", kestrel_core::MAX_AUDIO_PRIORITY_DEVICES),
        ("input_priority", kestrel_core::MAX_AUDIO_PRIORITY_DEVICES),
        ("mixer_favorites", kestrel_core::MAX_MIXER_FAVORITES),
    ] {
        let Some(value) = section.get(key) else {
            continue;
        };
        match Vec::<String>::deserialize(value.clone()) {
            Ok(list) if kestrel_core::valid_audio_identity_list(&list, limit) => match key {
                "output_priority" => audio.output_priority = list,
                "input_priority" => audio.input_priority = list,
                _ => audio.mixer_favorites = list,
            },
            _ => section.warn(
                key,
                format!(
                    "Must be a list of at most {limit} different names; the saved list was ignored."
                ),
            ),
        }
    }
    for (key, target) in [
        ("follow_output_priority", &mut audio.follow_output_priority),
        ("follow_input_priority", &mut audio.follow_input_priority),
    ] {
        section.read(key, BOOL_MESSAGE, target);
    }
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::{ApplicationConfiguration, AudioDisconnectPolicy, AudioOutputSwitch};

    use crate::config::{export_string, parse};

    #[test]
    fn audio_table_parses_every_mixer_policy_field() {
        let loaded = parse(
            r#"
schema_version = 3
[audio]
boost_percent = 140
output_switch = "all_streams"
disconnect_policy = "reset_volume"
disconnect_volume_percent = 65
include_inactive_streams = true
"#,
        )
        .expect("document itself is valid TOML");

        let audio = loaded.configuration.audio;
        assert_eq!(audio.boost_percent, 140);
        assert_eq!(audio.output_switch, AudioOutputSwitch::AllStreams);
        assert_eq!(audio.disconnect_policy, AudioDisconnectPolicy::ResetVolume);
        assert_eq!(audio.disconnect_volume_percent, 65);
        assert!(audio.include_inactive_streams);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn malformed_audio_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[audio]
boost_percent = 400
output_switch = "surround"
disconnect_policy = "forget"
disconnect_volume_percent = 160
include_inactive_streams = "sometimes"
output_priority = ["headset", "headset"]
input_priority = "usb-mic"
mixer_favorites = [""]
follow_output_priority = "yes"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.audio,
            kestrel_core::AudioConfiguration::default()
        );
        for location in [
            "audio.boost_percent",
            "audio.output_switch",
            "audio.disconnect_policy",
            "audio.disconnect_volume_percent",
            "audio.include_inactive_streams",
            "audio.output_priority",
            "audio.input_priority",
            "audio.mixer_favorites",
            "audio.follow_output_priority",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must report an isolated warning"
            );
        }
    }

    #[test]
    fn audio_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.audio.boost_percent = 150;
        configuration.audio.output_switch = AudioOutputSwitch::AllStreams;
        configuration.audio.disconnect_policy = AudioDisconnectPolicy::ResetVolume;
        configuration.audio.disconnect_volume_percent = 40;
        configuration.audio.include_inactive_streams = true;
        configuration.audio.output_priority = vec!["bluez_output.headset".into(), "hdmi".into()];
        configuration.audio.follow_output_priority = true;
        configuration.audio.input_priority = vec!["usb-mic".into()];
        configuration.audio.follow_input_priority = true;
        configuration.audio.mixer_favorites = vec!["firefox".into(), "spotify".into()];

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[audio]"));
        assert!(exported.contains("boost_percent = 150"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.audio, configuration.audio);
        assert!(reloaded.warnings.is_empty());
    }
}
