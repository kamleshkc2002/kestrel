//! The bug report form link prefilled with the running system.

use std::io::Read;

const REPORT_URL: &str =
    "https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml";
const MAX_VALUE_CHARS: usize = 100;
const MAX_OS_RELEASE_BYTES: u64 = 64 * 1024;

/// Session values that a report shares publicly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportEnvironment {
    pub desktop_session: Option<String>,
    pub distribution: Option<String>,
}

impl ReportEnvironment {
    /// Reads the values from the session that is running Kestrel.
    pub fn from_system() -> Self {
        let desktop_session = ["XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        Self {
            desktop_session: (!desktop_session.is_empty()).then(|| desktop_session.join(", ")),
            distribution: read_distribution(),
        }
    }
}

/// Builds the form link, leaving out values the environment does not provide.
pub fn bug_report_url(environment: &ReportEnvironment) -> String {
    let mut url = String::from(REPORT_URL);
    for (field, value) in [
        ("kestrel-version", Some(env!("CARGO_PKG_VERSION"))),
        ("desktop-session", environment.desktop_session.as_deref()),
        ("distribution", environment.distribution.as_deref()),
    ] {
        let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        url.push('&');
        url.push_str(field);
        url.push('=');
        push_encoded(&mut url, &bounded(value));
    }
    url
}

fn bounded(value: &str) -> String {
    value.chars().take(MAX_VALUE_CHARS).collect()
}

fn push_encoded(url: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                url.push(char::from(byte));
            }
            _ => {
                url.push('%');
                url.push(char::from(HEX[usize::from(byte >> 4)]));
                url.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
}

fn read_distribution() -> Option<String> {
    let mut contents = String::new();
    std::fs::File::open("/etc/os-release")
        .ok()?
        .take(MAX_OS_RELEASE_BYTES)
        .read_to_string(&mut contents)
        .ok()?;
    pretty_name(&contents)
}

fn pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "PRETTY_NAME" {
            return unquote(value.trim());
        }
    }
    None
}

fn unquote(value: &str) -> Option<String> {
    let Some(quote) = value
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))
    else {
        return Some(value.to_owned());
    };
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    if quote == '\'' {
        return Some(inner.to_owned());
    }
    let mut unescaped = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(escaped) = characters.next() {
                unescaped.push(escaped);
            }
        } else {
            unescaped.push(character);
        }
    }
    Some(unescaped)
}

#[cfg(test)]
mod tests {
    use super::{ReportEnvironment, bug_report_url};

    const BASE: &str =
        "https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml";

    fn environment(desktop_session: Option<&str>, distribution: Option<&str>) -> ReportEnvironment {
        ReportEnvironment {
            desktop_session: desktop_session.map(str::to_owned),
            distribution: distribution.map(str::to_owned),
        }
    }

    #[test]
    fn bug_report_url_prefills_reported_values() {
        let version = env!("CARGO_PKG_VERSION");
        let cases = [
            (
                Some("COSMIC, wayland"),
                Some("Pop!_OS 24.04 LTS"),
                "&desktop-session=COSMIC%2C%20wayland&distribution=Pop%21_OS%2024.04%20LTS",
            ),
            (
                Some("GNOME 46, Wayland & X11"),
                Some("Café Linux"),
                "&desktop-session=GNOME%2046%2C%20Wayland%20%26%20X11&distribution=Caf%C3%A9%20Linux",
            ),
            (Some("KDE, x11"), None, "&desktop-session=KDE%2C%20x11"),
            (
                None,
                Some("Fedora Linux 40"),
                "&distribution=Fedora%20Linux%2040",
            ),
            (None, None, ""),
            (Some("  "), Some(""), ""),
        ];
        for (desktop_session, distribution, suffix) in cases {
            let url = bug_report_url(&environment(desktop_session, distribution));
            assert_eq!(
                url,
                format!("{BASE}&kestrel-version={version}{suffix}"),
                "desktop {desktop_session:?}, distribution {distribution:?}"
            );
        }

        let long = "é".repeat(150);
        let url = bug_report_url(&environment(None, Some(&long)));
        assert_eq!(
            url,
            format!(
                "{BASE}&kestrel-version={version}&distribution={}",
                "%C3%A9".repeat(100)
            )
        );
    }
}
