//! Speed test limits.

use kestrel_core::{
    MAX_SPEED_TEST_DOWNLOAD_MEGABYTES, MAX_SPEED_TEST_TIMEOUT_SECONDS,
    MAX_SPEED_TEST_UPLOAD_MEGABYTES, MIN_SPEED_TEST_DOWNLOAD_MEGABYTES,
    MIN_SPEED_TEST_TIMEOUT_SECONDS,
};

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_speed_test(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "speed_test", value) else {
        return;
    };
    let speed_test = &mut loaded.configuration.speed_test;
    section.in_range(
        "download_megabytes",
        MIN_SPEED_TEST_DOWNLOAD_MEGABYTES..=MAX_SPEED_TEST_DOWNLOAD_MEGABYTES,
        ("Download size must be", " megabytes"),
        &mut speed_test.download_megabytes,
    );
    section.in_range(
        "upload_megabytes",
        0..=MAX_SPEED_TEST_UPLOAD_MEGABYTES,
        ("Upload size must be", " megabytes"),
        &mut speed_test.upload_megabytes,
    );
    section.in_range(
        "timeout_seconds",
        MIN_SPEED_TEST_TIMEOUT_SECONDS..=MAX_SPEED_TEST_TIMEOUT_SECONDS,
        ("Timeout must be", " seconds"),
        &mut speed_test.timeout_seconds,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use kestrel_core::ApplicationConfiguration;

    use crate::config::{export_string, parse};

    #[test]
    fn speed_test_table_parses_every_field() {
        let loaded = parse(
            r#"
schema_version = 3
[speed_test]
download_megabytes = 40
upload_megabytes = 20
timeout_seconds = 60
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.speed_test,
            kestrel_core::SpeedTestConfiguration {
                download_megabytes: 40,
                upload_megabytes: 20,
                timeout_seconds: 60,
            }
        );
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn malformed_speed_test_fields_are_isolated() {
        let loaded = parse(
            r#"
schema_version = 3
[speed_test]
download_megabytes = 0
upload_megabytes = 51
timeout_seconds = "slow"
"#,
        )
        .expect("document itself is valid TOML");

        assert_eq!(
            loaded.configuration.speed_test,
            kestrel_core::SpeedTestConfiguration::default()
        );
        for location in [
            "speed_test.download_megabytes",
            "speed_test.upload_megabytes",
            "speed_test.timeout_seconds",
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
    fn speed_test_policy_survives_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.speed_test.download_megabytes = 75;
        configuration.speed_test.upload_megabytes = 0;
        configuration.speed_test.timeout_seconds = 90;

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[speed_test]"));
        assert!(exported.contains("download_megabytes = 75"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.speed_test, configuration.speed_test);
        assert!(reloaded.warnings.is_empty());
    }
}
