//! URL cleanup preferences and per-site parameter rules.

use kestrel_core::UrlSiteConfiguration;

use super::{
    LoadedConfiguration,
    fields::{BOOL_MESSAGE, Section},
};

const LIST_MESSAGE: &str = "Must be an array of parameter names; the default was retained.";
const SITES_MESSAGE: &str = "Must be an array of site tables; no sites were read.";

pub(super) fn parse_url_cleanup(loaded: &mut LoadedConfiguration, value: Option<&toml::Value>) {
    let Some(mut section) = Section::open(loaded, "url_cleanup", value) else {
        return;
    };
    section.read(
        "automatic",
        BOOL_MESSAGE,
        &mut loaded.configuration.url_cleanup.automatic,
    );
    loaded.configuration.url_cleanup.extra_global = parameter_list(&mut section, "extra_global");
    loaded.configuration.url_cleanup.sites = site_rules(&mut section);
    section.finish(loaded);
}

fn parameter_list(section: &mut Section<'_>, key: &str) -> Vec<String> {
    let Some(value) = section.get(key) else {
        return Vec::new();
    };
    let Some(values) = value.as_array() else {
        section.warn(key, LIST_MESSAGE);
        return Vec::new();
    };
    let mut params = Vec::with_capacity(values.len());
    for value in values {
        let Some(param) = value.as_str() else {
            section.warn(
                key,
                "Each parameter name must be a string; the entry was ignored.",
            );
            continue;
        };
        let param = param.trim();
        if param.is_empty() {
            section.warn(key, "An empty parameter name was ignored.");
            continue;
        }
        params.push(param.to_owned());
    }
    params
}

fn site_rules(section: &mut Section<'_>) -> Vec<UrlSiteConfiguration> {
    let Some(value) = section.get("sites") else {
        return Vec::new();
    };
    let Some(values) = value.as_array() else {
        section.warn("sites", SITES_MESSAGE);
        return Vec::new();
    };
    let mut sites: Vec<UrlSiteConfiguration> = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let Some(table) = value.as_table() else {
            section.warn(
                "sites",
                format!("Site rule {index} must be a table; it was ignored."),
            );
            continue;
        };
        let Some(host) = table.get("host").and_then(toml::Value::as_str) else {
            section.warn(
                "sites",
                format!("Site rule {index} needs a host; it was ignored."),
            );
            continue;
        };
        let host = host.trim();
        if host.is_empty() {
            section.warn(
                "sites",
                format!("Site rule {index} has an empty host; it was ignored."),
            );
            continue;
        }
        if sites
            .iter()
            .any(|site| site.host.eq_ignore_ascii_case(host))
        {
            section.warn(
                "sites",
                format!("Site rule {index} repeats {host}; it was ignored."),
            );
            continue;
        }
        let mut site = UrlSiteConfiguration {
            host: host.to_owned(),
            enabled: true,
            params: Vec::new(),
        };
        if let Some(enabled) = table.get("enabled") {
            match enabled.as_bool() {
                Some(enabled) => site.enabled = enabled,
                None => section.warn(
                    "sites",
                    format!("Site rule {index} has a non-boolean enabled value; it stays on."),
                ),
            }
        }
        if let Some(params) = table.get("params") {
            match params.as_array() {
                Some(values) => {
                    for value in values {
                        let Some(param) = value.as_str() else {
                            section.warn(
                                "sites",
                                format!(
                                    "Site rule {index} has a non-string parameter; it was ignored."
                                ),
                            );
                            continue;
                        };
                        let param = param.trim();
                        if param.is_empty() {
                            section.warn(
                                "sites",
                                format!(
                                    "Site rule {index} has an empty parameter; it was ignored."
                                ),
                            );
                            continue;
                        }
                        site.params.push(param.to_owned());
                    }
                }
                None => section.warn(
                    "sites",
                    format!("Site rule {index} params must be an array; it was ignored."),
                ),
            }
        }
        sites.push(site);
    }
    sites
}

#[cfg(test)]
mod tests {
    use kestrel_core::ApplicationConfiguration;

    use crate::config::{export_string, parse};

    #[test]
    fn url_cleanup_parses_the_preference_extra_params_and_site_rules() {
        let loaded = parse(
            r#"
schema_version = 4
[url_cleanup]
automatic = true
extra_global = ["custom_param", "extra_*"]

[[url_cleanup.sites]]
host = "Example.com"
params = ["ref", "utm_*"]

[[url_cleanup.sites]]
host = "youtube.com"
enabled = false
params = ["si", "feature"]
"#,
        )
        .expect("document itself is valid TOML");

        let cleanup = &loaded.configuration.url_cleanup;
        assert!(cleanup.automatic);
        assert_eq!(cleanup.extra_global, ["custom_param", "extra_*"]);
        assert_eq!(cleanup.sites.len(), 2);
        assert_eq!(cleanup.sites[0].host, "Example.com");
        assert!(cleanup.sites[0].enabled);
        assert_eq!(cleanup.sites[0].params, ["ref", "utm_*"]);
        assert!(!cleanup.sites[1].enabled);
        assert_eq!(cleanup.sites[1].params, ["si", "feature"]);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn invalid_cleanup_entries_warn_and_are_skipped() {
        let loaded = parse(
            r#"
schema_version = 4
[url_cleanup]
automatic = "yes"
extra_global = ["keep", 3, ""]

[[url_cleanup.sites]]
host = ""

[[url_cleanup.sites]]
host = "example.com"
enabled = "off"
params = ["ok", 2, " "]

[[url_cleanup.sites]]
host = "example.com"
params = ["dropped"]

[[url_cleanup.sites]]
params = ["no-host"]
"#,
        )
        .expect("document itself is valid TOML");

        let cleanup = &loaded.configuration.url_cleanup;
        assert!(!cleanup.automatic, "a bad boolean keeps the default");
        assert_eq!(cleanup.extra_global, ["keep"]);
        assert_eq!(cleanup.sites.len(), 1, "only the valid rule is kept");
        assert_eq!(cleanup.sites[0].host, "example.com");
        assert!(cleanup.sites[0].enabled, "a bad value leaves the site on");
        assert_eq!(cleanup.sites[0].params, ["ok"]);
        for location in [
            "url_cleanup.automatic",
            "url_cleanup.extra_global",
            "url_cleanup.sites",
        ] {
            assert!(
                loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.feature_id == location),
                "{location} must warn"
            );
        }
    }

    #[test]
    fn a_non_table_cleanup_section_is_reported_and_defaults_are_used() {
        let loaded = parse("schema_version = 4\nurl_cleanup = \"off\"\n")
            .expect("document itself is valid TOML");
        assert_eq!(
            loaded.configuration.url_cleanup,
            kestrel_core::UrlCleanupConfiguration::default()
        );
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "url_cleanup")
        );
    }

    #[test]
    fn cleanup_preferences_survive_an_export_round_trip() {
        let mut configuration = ApplicationConfiguration::default();
        configuration.url_cleanup.automatic = true;
        configuration.url_cleanup.extra_global = vec!["from_export".to_owned()];
        configuration.url_cleanup.sites = vec![kestrel_core::UrlSiteConfiguration {
            host: "example.com".to_owned(),
            enabled: false,
            params: vec!["ref".to_owned()],
        }];

        let exported = export_string(&configuration).expect("configuration exports");
        assert!(exported.contains("[url_cleanup]"));
        assert!(exported.contains("automatic = true"));

        let reloaded = parse(&exported).expect("exported configuration parses");
        assert_eq!(
            reloaded.configuration.url_cleanup,
            configuration.url_cleanup
        );
        assert!(reloaded.warnings.is_empty());
    }
}
