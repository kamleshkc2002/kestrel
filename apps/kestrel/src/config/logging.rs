//! Logging preferences.

use kestrel_core::{
    MAX_LOG_RETENTION_DAYS, MAX_LOG_TOTAL_MEGABYTES, MIN_LOG_RETENTION_DAYS,
    MIN_LOG_TOTAL_MEGABYTES,
};

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_logging(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "logging", value) else {
        return;
    };
    let logging = &mut loaded.configuration.logging;
    section.read(
        "debug",
        "Must be true or false; debug logging stays off.",
        &mut logging.debug,
    );
    section.in_range(
        "retention_days",
        MIN_LOG_RETENTION_DAYS..=MAX_LOG_RETENTION_DAYS,
        ("Must be", " days"),
        &mut logging.retention_days,
    );
    section.in_range(
        "max_total_megabytes",
        MIN_LOG_TOTAL_MEGABYTES..=MAX_LOG_TOTAL_MEGABYTES,
        ("Must be", " MB"),
        &mut logging.max_total_megabytes,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::parse;

    #[test]
    fn malformed_logging_fields_keep_their_defaults_and_warn() {
        let loaded = parse(
            "schema_version = 3\n[logging]\ndebug = true\nretention_days = 0\nmax_total_megabytes = 50\n",
        )
        .expect("document itself is valid TOML");
        let logging = loaded.configuration.logging;
        assert!(logging.debug);
        assert_eq!(
            logging.retention_days,
            kestrel_core::DEFAULT_LOG_RETENTION_DAYS,
            "an out-of-range retention keeps the default"
        );
        assert_eq!(logging.max_total_megabytes, 50);
        assert_eq!(
            loaded
                .warnings
                .iter()
                .map(|warning| warning.feature_id.as_str())
                .collect::<Vec<_>>(),
            vec!["logging.retention_days"]
        );
    }
}
