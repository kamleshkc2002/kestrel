//! Scratchpad and process inspector bounds.

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_scratchpad(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "scratchpad", value) else {
        return;
    };
    let scratchpad = &mut loaded.configuration.scratchpad;
    section.in_range(
        "max_notes",
        kestrel_core::MIN_SCRATCHPAD_MAX_NOTES..=kestrel_core::MAX_SCRATCHPAD_MAX_NOTES,
        ("Must be", " notes"),
        &mut scratchpad.max_notes,
    );
    section.in_range(
        "max_note_bytes",
        kestrel_core::MIN_SCRATCHPAD_NOTE_BYTES..=kestrel_core::MAX_SCRATCHPAD_NOTE_BYTES,
        ("Must be", " bytes"),
        &mut scratchpad.max_note_bytes,
    );
    section.in_range(
        "max_age_days",
        0..=kestrel_core::MAX_SCRATCHPAD_AGE_DAYS,
        ("Must be", " days"),
        &mut scratchpad.max_age_days,
    );
    section.finish(loaded);
}

pub(super) fn parse_processes(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "processes", value) else {
        return;
    };
    let processes = &mut loaded.configuration.processes;
    section.in_range(
        "refresh_millis",
        kestrel_core::MIN_PROCESS_REFRESH_MILLIS..=kestrel_core::MAX_PROCESS_REFRESH_MILLIS,
        ("Must be", " milliseconds"),
        &mut processes.refresh_millis,
    );
    section.in_range(
        "max_rows",
        kestrel_core::MIN_PROCESS_ROWS..=kestrel_core::MAX_PROCESS_ROWS,
        ("Must be", " rows"),
        &mut processes.max_rows,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::{export_string, parse};

    #[test]
    fn process_limits_reject_out_of_range_keys_alone_and_survive_export() {
        let loaded = parse(
            r#"
schema_version = 3
[processes]
refresh_millis = 500
max_rows = 250
"#,
        )
        .expect("document itself is valid TOML");
        let defaults = kestrel_core::ProcessConfiguration::default();
        assert_eq!(
            loaded.configuration.processes.refresh_millis,
            defaults.refresh_millis
        );
        assert_eq!(loaded.configuration.processes.max_rows, 250);
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["processes.refresh_millis"]);

        let exported = export_string(&loaded.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.processes,
            loaded.configuration.processes
        );
        assert!(reloaded.warnings.is_empty());
    }
}
