//! Shelf bounds and whether entries survive a restart.

use super::{LoadedConfiguration, fields::Section};

pub(super) fn parse_shelf(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "shelf", value) else {
        return;
    };
    let shelf = &mut loaded.configuration.shelf;
    section.read(
        "keep_across_restarts",
        super::fields::BOOL_MESSAGE,
        &mut shelf.keep_across_restarts,
    );
    section.read(
        "add_screenshots",
        super::fields::BOOL_MESSAGE,
        &mut shelf.add_screenshots,
    );
    section.in_range(
        "max_items",
        kestrel_core::MIN_SHELF_MAX_ITEMS..=kestrel_core::MAX_SHELF_MAX_ITEMS,
        ("Must be", " entries"),
        &mut shelf.max_items,
    );
    section.in_range(
        "max_text_megabytes",
        kestrel_core::MIN_SHELF_TEXT_MEGABYTES..=kestrel_core::MAX_SHELF_TEXT_MEGABYTES,
        ("Must be", " MiB"),
        &mut shelf.max_text_megabytes,
    );
    section.in_range(
        "max_image_megabytes",
        kestrel_core::MIN_SHELF_IMAGE_MEGABYTES..=kestrel_core::MAX_SHELF_IMAGE_MEGABYTES,
        ("Must be", " MiB"),
        &mut shelf.max_image_megabytes,
    );
    section.in_range(
        "max_age_hours",
        0..=kestrel_core::MAX_SHELF_AGE_HOURS,
        ("Must be", " hours"),
        &mut shelf.max_age_hours,
    );
    section.finish(loaded);
}

#[cfg(test)]
mod tests {
    use crate::config::{export_string, parse};

    #[test]
    fn shelf_defaults_are_kept_and_limits_are_bounded() {
        let loaded = parse(
            r#"
schema_version = 4
[shelf]
keep_across_restarts = true
add_screenshots = true
max_items = 500
max_text_megabytes = 2
max_image_megabytes = 0
max_age_hours = 48
bool_for_text = true
"#,
        )
        .expect("document itself is valid TOML");
        let shelf = loaded.configuration.shelf;
        assert!(shelf.keep_across_restarts);
        assert!(shelf.add_screenshots);
        assert_eq!(shelf.max_items, kestrel_core::DEFAULT_SHELF_MAX_ITEMS);
        assert_eq!(shelf.max_text_megabytes, 2);
        assert_eq!(
            shelf.max_image_megabytes,
            kestrel_core::DEFAULT_SHELF_IMAGE_MEGABYTES
        );
        assert_eq!(shelf.max_age_hours, 48);
        let rejected = loaded
            .warnings
            .iter()
            .map(|warning| warning.feature_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rejected, ["shelf.max_items", "shelf.max_image_megabytes"]);

        let exported = export_string(&loaded.configuration).expect("configuration exports");
        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(reloaded.configuration.shelf, shelf);
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn a_non_boolean_keep_value_warns_and_stays_off() {
        let loaded = parse(
            r#"
schema_version = 4
[shelf]
keep_across_restarts = "yes"
"#,
        )
        .expect("document itself is valid TOML");
        assert!(!loaded.configuration.shelf.keep_across_restarts);
        assert!(!loaded.configuration.shelf.add_screenshots);
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].feature_id, "shelf.keep_across_restarts");
    }
}
