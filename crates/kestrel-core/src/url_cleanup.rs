//! Lossless removal of tracking parameters from http and https links.
//!
//! The query is split by hand so every kept byte stays exactly as written;
//! nothing is decoded, re-encoded, or case-folded in the output.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longer inputs are not treated as links.
pub const MAX_URL_BYTES: usize = 8 * 1024;

/// One query parameter that a clean removed, with its original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedParam {
    pub key: String,
    pub value: String,
}

/// A cleaned link and the parameters taken out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedUrl {
    pub cleaned: String,
    pub removed: Vec<RemovedParam>,
}

/// Why a link cannot be cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlCleanupErrorKind {
    /// The scheme is not http or https.
    Unsupported,
    /// The text is not a well-formed link.
    Malformed,
}

/// A typed cleanup failure; the original text stays with the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlCleanupError {
    pub kind: UrlCleanupErrorKind,
}

impl UrlCleanupError {
    fn unsupported() -> Self {
        Self {
            kind: UrlCleanupErrorKind::Unsupported,
        }
    }

    fn malformed() -> Self {
        Self {
            kind: UrlCleanupErrorKind::Malformed,
        }
    }
}

impl fmt::Display for UrlCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            UrlCleanupErrorKind::Unsupported => {
                formatter.write_str("the link does not use http or https")
            }
            UrlCleanupErrorKind::Malformed => formatter.write_str("the link is malformed"),
        }
    }
}

impl std::error::Error for UrlCleanupError {}

/// Parameters removed for one host and whether that host's rules apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRule {
    /// Matches the host itself or a subdomain ending in a dot and this suffix.
    pub host_suffix: String,
    pub enabled: bool,
    pub params: Vec<String>,
}

/// The local parameter rules used by manual and automatic cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRules {
    pub global: Vec<String>,
    pub sites: Vec<SiteRule>,
}

const DEFAULT_GLOBAL_PARAMS: [&str; 19] = [
    "utm_*",
    "fbclid",
    "gclid",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "mc_cid",
    "mc_eid",
    "igshid",
    "yclid",
    "_hsenc",
    "_hsmi",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "rb_clickid",
    "s_cid",
];

const AMAZON_PARAMS: [&str; 6] = ["ref", "ref_", "pf_rd_*", "pd_rd_*", "_encoding", "psc"];

const AMAZON_HOSTS: [&str; 5] = [
    "amazon.com",
    "amazon.co.uk",
    "amazon.de",
    "amazon.ca",
    "amazon.in",
];

impl Default for UrlRules {
    fn default() -> Self {
        let mut sites = Vec::new();
        for host in AMAZON_HOSTS {
            sites.push(site_rule(host, &AMAZON_PARAMS));
        }
        for (host, params) in [
            ("youtube.com", &["si", "feature", "pp"][..]),
            ("youtu.be", &["si"][..]),
            ("twitter.com", &["s", "t"][..]),
            ("x.com", &["s", "t"][..]),
            ("instagram.com", &["igsh"][..]),
            ("spotify.com", &["si"][..]),
            ("reddit.com", &["share_id", "utm_*"][..]),
            ("linkedin.com", &["trk", "trackingId", "lipi"][..]),
        ] {
            sites.push(site_rule(host, params));
        }
        Self {
            global: DEFAULT_GLOBAL_PARAMS
                .iter()
                .map(|param| (*param).to_owned())
                .collect(),
            sites,
        }
    }
}

fn site_rule(host: &str, params: &[&str]) -> SiteRule {
    SiteRule {
        host_suffix: host.to_owned(),
        enabled: true,
        params: params.iter().map(|param| (*param).to_owned()).collect(),
    }
}

impl UrlRules {
    /// Built-in rules with the configured extras and site overrides applied.
    ///
    /// A configured site replaces the built-in rule with the same host, so a
    /// disabled entry keeps the parameter names stored with it.
    pub fn merged(extra_global: &[String], sites: &[SiteRule]) -> Self {
        let mut merged = Self::default();
        for pattern in extra_global {
            let pattern = pattern.trim();
            if pattern.is_empty()
                || merged
                    .global
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(pattern))
            {
                continue;
            }
            merged.global.push(pattern.to_owned());
        }
        for site in sites {
            let host = site.host_suffix.trim();
            if host.is_empty() {
                continue;
            }
            let mut site = site.clone();
            site.host_suffix = host.to_owned();
            site.params = normalized_params(&site.params);
            match merged
                .sites
                .iter_mut()
                .find(|existing| existing.host_suffix.eq_ignore_ascii_case(host))
            {
                Some(existing) => *existing = site,
                None => merged.sites.push(site),
            }
        }
        merged
    }

    /// The effective rules for a configuration section.
    pub fn from_configuration(configuration: &UrlCleanupConfiguration) -> Self {
        let sites = configuration
            .sites
            .iter()
            .filter_map(|site| {
                let host = site.host.trim();
                (!host.is_empty()).then(|| SiteRule {
                    host_suffix: host.to_owned(),
                    enabled: site.enabled,
                    params: normalized_params(&site.params),
                })
            })
            .collect::<Vec<_>>();
        Self::merged(&configuration.extra_global, &sites)
    }
}

fn normalized_params(params: &[String]) -> Vec<String> {
    params
        .iter()
        .filter_map(|param| {
            let param = param.trim();
            (!param.is_empty()).then(|| param.to_owned())
        })
        .collect()
}

/// Removes every matching query parameter, preserving all other bytes.
pub fn clean_url(input: &str, rules: &UrlRules) -> Result<CleanedUrl, UrlCleanupError> {
    let parts = parse_parts(input)?;
    let Some(query_start) = parts.query_start else {
        return Ok(CleanedUrl {
            cleaned: input.to_owned(),
            removed: Vec::new(),
        });
    };
    let fragment_start = parts.fragment_start.unwrap_or(input.len());
    let query = &input[query_start + 1..fragment_start];
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for segment in query.split('&') {
        if segment.is_empty() {
            kept.push(segment);
            continue;
        }
        let (key, value) = match segment.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (segment, None),
        };
        if param_matches(key, parts.host, rules) {
            removed.push(RemovedParam {
                key: key.to_owned(),
                value: value.unwrap_or_default().to_owned(),
            });
        } else {
            kept.push(segment);
        }
    }
    if removed.is_empty() {
        return Ok(CleanedUrl {
            cleaned: input.to_owned(),
            removed,
        });
    }
    let tail = &input[fragment_start..];
    let cleaned = if kept.iter().all(|segment| segment.is_empty()) {
        format!("{}{}", &input[..query_start], tail)
    } else {
        format!("{}{}{}{}", &input[..query_start], '?', kept.join("&"), tail)
    };
    Ok(CleanedUrl { cleaned, removed })
}

/// The one http(s) link in `text`, after trimming one trailing newline.
///
/// Lists, surrounding text, images, and other schemes yield `None`.
pub fn single_http_link(text: &str) -> Option<&str> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return None;
    }
    parse_parts(trimmed).ok()?;
    Some(trimmed)
}

struct UrlParts<'a> {
    host: &'a str,
    query_start: Option<usize>,
    fragment_start: Option<usize>,
}

fn parse_parts(input: &str) -> Result<UrlParts<'_>, UrlCleanupError> {
    if input.len() > MAX_URL_BYTES {
        return Err(UrlCleanupError::malformed());
    }
    if input
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(UrlCleanupError::malformed());
    }
    let Some(scheme_end) = input.find(':') else {
        return Err(UrlCleanupError::unsupported());
    };
    let scheme = &input[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(UrlCleanupError::unsupported());
    }
    let rest = &input[scheme_end + 1..];
    let Some(authority_and_path) = rest.strip_prefix("//") else {
        return Err(UrlCleanupError::malformed());
    };
    let authority_start = input.len() - authority_and_path.len();
    let authority_end = authority_and_path
        .find(['/', '?', '#'])
        .map_or(input.len(), |offset| authority_start + offset);
    let authority = &input[authority_start..authority_end];
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed
            .find(']')
            .map(|end| &bracketed[..end])
            .unwrap_or_default(),
        None => match authority.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|character| character.is_ascii_digit()) => host,
            _ => authority,
        },
    };
    if host.is_empty() {
        return Err(UrlCleanupError::malformed());
    }
    let fragment_start = input.find('#');
    let query_start = match (input.find('?'), fragment_start) {
        (Some(query), Some(fragment)) if query < fragment => Some(query),
        (Some(query), None) => Some(query),
        _ => None,
    };
    Ok(UrlParts {
        host,
        query_start,
        fragment_start,
    })
}

fn param_matches(key: &str, host: &str, rules: &UrlRules) -> bool {
    let decoded = decoded_key(key);
    if rules
        .global
        .iter()
        .any(|pattern| pattern_matches(pattern, &decoded))
    {
        return true;
    }
    rules.sites.iter().any(|site| {
        site.enabled
            && host_matches(host, &site.host_suffix)
            && site
                .params
                .iter()
                .any(|pattern| pattern_matches(pattern, &decoded))
    })
}

fn pattern_matches(pattern: &str, key: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => key.starts_with(&prefix.to_ascii_lowercase()),
        None => pattern.eq_ignore_ascii_case(key),
    }
}

fn host_matches(host: &str, suffix: &str) -> bool {
    let suffix = suffix.trim();
    if suffix.is_empty() || host.len() < suffix.len() {
        return false;
    }
    if host.len() == suffix.len() {
        return host.eq_ignore_ascii_case(suffix);
    }
    let start = host.len() - suffix.len();
    host.is_char_boundary(start)
        && host.as_bytes()[start - 1] == b'.'
        && host[start..].eq_ignore_ascii_case(suffix)
}

/// Percent-decodes and ASCII-lowercases a key for comparison only.
fn decoded_key(key: &str) -> String {
    let bytes = key.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    for byte in &mut decoded {
        byte.make_ascii_lowercase();
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// User rules for URL cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UrlCleanupConfiguration {
    /// Clean a single copied link as soon as it is copied.
    #[serde(default)]
    pub automatic: bool,
    /// Extra parameter names for every site.
    #[serde(default)]
    pub extra_global: Vec<String>,
    /// Site rules that replace built-ins with the same host.
    #[serde(default)]
    pub sites: Vec<UrlSiteConfiguration>,
}

/// One configured site rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UrlSiteConfiguration {
    pub host: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub params: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(host: &str, enabled: bool, params: &[&str]) -> SiteRule {
        SiteRule {
            host_suffix: host.to_owned(),
            enabled,
            params: params.iter().map(|param| (*param).to_owned()).collect(),
        }
    }

    fn removed(cleaned: &CleanedUrl) -> Vec<(&str, &str)> {
        cleaned
            .removed
            .iter()
            .map(|param| (param.key.as_str(), param.value.as_str()))
            .collect()
    }

    #[test]
    fn tracking_parameters_are_removed_and_other_parts_are_preserved() {
        let rules = UrlRules::default();
        let cleaned = clean_url(
            "https://Example.COM/Path?utm_source=news&keep=One%2FTwo&UTM_Medium=mail#Frag?x=1",
            &rules,
        )
        .expect("valid link");
        assert_eq!(
            cleaned.cleaned,
            "https://Example.COM/Path?keep=One%2FTwo#Frag?x=1"
        );
        assert_eq!(
            removed(&cleaned),
            [("utm_source", "news"), ("UTM_Medium", "mail")]
        );

        let ordered = clean_url("https://example.com/?b=2&utm_source=x&a=1&fbclid=y", &rules)
            .expect("valid link");
        assert_eq!(ordered.cleaned, "https://example.com/?b=2&a=1");
        assert_eq!(removed(&ordered), [("utm_source", "x"), ("fbclid", "y")]);
    }

    #[test]
    fn percent_encoded_and_mixed_case_keys_match_without_changing_kept_text() {
        let rules = UrlRules::default();
        let cleaned = clean_url(
            "https://example.com/?%75tm_source=x&q=A%2Fb&K=%41&MsClKiD=abc",
            &rules,
        )
        .expect("valid link");
        assert_eq!(cleaned.cleaned, "https://example.com/?q=A%2Fb&K=%41");
        assert_eq!(
            removed(&cleaned),
            [("%75tm_source", "x"), ("MsClKiD", "abc")]
        );
    }

    #[test]
    fn site_rules_match_subdomains_by_dot_boundary_and_can_be_disabled() {
        let rules = UrlRules::default();
        let youtube = clean_url(
            "https://www.youtube.com/watch?v=abc&t=30s&si=share&feature=shared&pp=xyz",
            &rules,
        )
        .expect("valid link");
        assert_eq!(youtube.cleaned, "https://www.youtube.com/watch?v=abc&t=30s");
        assert_eq!(youtube.removed.len(), 3);

        let look_alike =
            clean_url("https://notyoutube.com/watch?si=1&v=abc", &rules).expect("valid link");
        assert_eq!(
            look_alike.cleaned,
            "https://notyoutube.com/watch?si=1&v=abc"
        );
        assert!(look_alike.removed.is_empty());

        let disabled =
            UrlRules::merged(&[], &[site("youtube.com", false, &["si", "feature", "pp"])]);
        let kept =
            clean_url("https://www.youtube.com/watch?v=abc&si=1", &disabled).expect("valid link");
        assert_eq!(kept.cleaned, "https://www.youtube.com/watch?v=abc&si=1");
        assert!(kept.removed.is_empty());
    }

    #[test]
    fn prefix_patterns_and_an_empty_query_drop_the_question_mark() {
        let rules = UrlRules::merged(&[String::from("extra_*")], &[]);
        let cleaned = clean_url(
            "https://example.com/page?utm_campaign=x&extra_one=1&keep=2&utm_=z",
            &rules,
        )
        .expect("valid link");
        assert_eq!(cleaned.cleaned, "https://example.com/page?keep=2");
        assert_eq!(cleaned.removed.len(), 3);

        let empty = clean_url("https://example.com/page?utm_source=x", &rules).expect("valid link");
        assert_eq!(empty.cleaned, "https://example.com/page");
        assert_eq!(empty.removed.len(), 1);
    }

    #[test]
    fn a_question_mark_inside_a_value_stays_and_nothing_matched_returns_the_input() {
        let rules = UrlRules::default();
        let value =
            clean_url("https://example.com/?a=b?c&utm_source=x", &rules).expect("valid link");
        assert_eq!(value.cleaned, "https://example.com/?a=b?c");

        let input = "https://example.com/path?a=b#frag";
        let untouched = clean_url(input, &rules).expect("valid link");
        assert_eq!(untouched.cleaned, input);
        assert!(untouched.removed.is_empty());
    }

    #[test]
    fn amazon_domains_use_the_site_specific_parameters() {
        let rules = UrlRules::default();
        let cleaned = clean_url(
            "https://www.amazon.co.uk/dp/B0?ref_=abc&pf_rd_p=x&th=1&_encoding=UTF8",
            &rules,
        )
        .expect("valid link");
        assert_eq!(cleaned.cleaned, "https://www.amazon.co.uk/dp/B0?th=1");
        assert_eq!(cleaned.removed.len(), 3);
    }

    #[test]
    fn unsupported_and_malformed_links_are_reported_without_touching_the_input() {
        let rules = UrlRules::default();
        for input in [
            "mailto:someone@example.com",
            "ftp://example.com/?utm_source=x",
            "example.com",
        ] {
            assert_eq!(
                clean_url(input, &rules).map(|cleaned| cleaned.cleaned),
                Err(UrlCleanupError {
                    kind: UrlCleanupErrorKind::Unsupported
                }),
                "{input}"
            );
        }
        for input in [
            " https://example.com/?utm_source=x",
            "https://example.com/\n?utm_source=x",
            "https://example.com/\t?utm_source=x",
            "https:///path?utm_source=x",
            "https://",
            "https://?utm_source=x",
        ] {
            assert_eq!(
                clean_url(input, &rules).map(|cleaned| cleaned.cleaned),
                Err(UrlCleanupError {
                    kind: UrlCleanupErrorKind::Malformed
                }),
                "{input:?}"
            );
        }
        let too_long = format!(
            "https://example.com/?utm_source={}",
            "x".repeat(MAX_URL_BYTES)
        );
        assert_eq!(
            clean_url(&too_long, &rules).map(|cleaned| cleaned.cleaned),
            Err(UrlCleanupError {
                kind: UrlCleanupErrorKind::Malformed
            })
        );
        let at_limit = format!(
            "https://example.com/?utm_source={}",
            "x".repeat(MAX_URL_BYTES - "https://example.com/?utm_source=".len())
        );
        assert_eq!(at_limit.len(), MAX_URL_BYTES);
        assert!(clean_url(&at_limit, &rules).is_ok());
    }

    #[test]
    fn a_single_link_rule_accepts_one_line_and_rejects_lists_and_extra_text() {
        assert_eq!(
            single_http_link("https://example.com/?utm_source=x\n"),
            Some("https://example.com/?utm_source=x")
        );
        assert_eq!(
            single_http_link("https://example.com/?utm_source=x\r\n"),
            Some("https://example.com/?utm_source=x")
        );
        assert_eq!(
            single_http_link("https://a.example/?x=1\nhttps://b.example/?y=2"),
            None
        );
        assert_eq!(single_http_link("Copy: https://example.com/?x=1"), None);
        assert_eq!(single_http_link(" https://example.com/?x=1"), None);
        assert_eq!(single_http_link("mailto:someone@example.com"), None);
        assert_eq!(single_http_link("not a link"), None);
        assert_eq!(single_http_link(""), None);
    }

    #[test]
    fn configured_sites_replace_built_ins_and_keep_params_when_disabled() {
        let configuration = UrlCleanupConfiguration {
            automatic: true,
            extra_global: vec!["custom".to_owned(), "utm_*".to_owned()],
            sites: vec![
                UrlSiteConfiguration {
                    host: "YouTube.com".to_owned(),
                    enabled: false,
                    params: vec!["si".to_owned(), "my_tracking".to_owned()],
                },
                UrlSiteConfiguration {
                    host: "example.com".to_owned(),
                    enabled: true,
                    params: vec!["ref".to_owned()],
                },
            ],
        };
        let rules = UrlRules::from_configuration(&configuration);
        let youtube = rules
            .sites
            .iter()
            .find(|site| site.host_suffix.eq_ignore_ascii_case("youtube.com"))
            .expect("youtube rule");
        assert!(
            !youtube.enabled,
            "the configured rule replaces the built-in"
        );
        assert_eq!(youtube.params, ["si", "my_tracking"]);
        assert_eq!(
            rules
                .global
                .iter()
                .filter(|param| param.eq_ignore_ascii_case("utm_*"))
                .count(),
            1,
            "an extra already in the defaults is not duplicated"
        );
        assert!(rules.global.iter().any(|param| param == "custom"));
        assert!(
            rules
                .sites
                .iter()
                .any(|site| site.host_suffix == "example.com")
        );

        let cleaned = clean_url("https://www.youtube.com/watch?si=1&my_tracking=2", &rules)
            .expect("valid link");
        assert_eq!(
            cleaned.cleaned, "https://www.youtube.com/watch?si=1&my_tracking=2",
            "a disabled site keeps its parameters"
        );
    }
}
