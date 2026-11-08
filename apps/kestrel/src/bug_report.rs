//! The bug report form link prefilled with the running system.

use std::io::Read;

const REPORT_URL: &str =
    "https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml";
const MAX_VALUE_CHARS: usize = 100;
const MAX_OS_RELEASE_BYTES: u64 = 64 * 1024;

/// Session values that a report shares publicly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportEnvironment {
    pub desktop: Option<String>,
    pub session_type: Option<String>,
    pub distribution: Option<String>,
}

impl ReportEnvironment {
    /// Reads the values from the session that is running Kestrel.
    pub fn from_system() -> Self {
        Self {
            desktop: std::env::var("XDG_CURRENT_DESKTOP")
                .ok()
                .and_then(|value| desktop_option(&value)),
            session_type: std::env::var("XDG_SESSION_TYPE")
                .ok()
                .and_then(|value| session_type_option(&value)),
            distribution: read_distribution(),
        }
    }
}

/// Maps a desktop name to an option in the bug report form.
fn desktop_option(value: &str) -> Option<String> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    let option = if normalized.contains("cosmic") {
        "COSMIC"
    } else if normalized.contains("sway") {
        "Sway"
    } else if normalized.contains("hyprland") {
        "Hyprland"
    } else if normalized.contains("kde") || normalized.contains("plasma") {
        "KDE Plasma"
    } else if normalized.contains("gnome") {
        "GNOME"
    } else {
        "Other"
    };
    Some(option.to_owned())
}

/// Maps a session type to an option in the bug report form.
fn session_type_option(value: &str) -> Option<String> {
    match value.trim().to_lowercase().as_str() {
        "wayland" => Some("Wayland".to_owned()),
        "x11" => Some("X11".to_owned()),
        _ => None,
    }
}

/// Builds the form link, leaving out values the environment does not provide.
pub fn bug_report_url(environment: &ReportEnvironment) -> String {
    let mut url = String::from(REPORT_URL);
    for (field, value) in [
        ("kestrel-version", Some(env!("CARGO_PKG_VERSION"))),
        ("desktop", environment.desktop.as_deref()),
        ("session-type", environment.session_type.as_deref()),
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
    use super::{ReportEnvironment, bug_report_url, desktop_option, session_type_option};

    const BASE: &str =
        "https://github.com/kamleshkc2002/kestrel/issues/new?template=bug_report.yml";

    fn environment(
        desktop: Option<&str>,
        session_type: Option<&str>,
        distribution: Option<&str>,
    ) -> ReportEnvironment {
        ReportEnvironment {
            desktop: desktop.map(str::to_owned),
            session_type: session_type.map(str::to_owned),
            distribution: distribution.map(str::to_owned),
        }
    }

    #[test]
    fn bug_report_url_prefills_reported_values() {
        let version = env!("CARGO_PKG_VERSION");
        let cases = [
            (
                Some("COSMIC"),
                Some("Wayland"),
                Some("Pop!_OS 24.04 LTS"),
                "&desktop=COSMIC&session-type=Wayland&distribution=Pop%21_OS%2024.04%20LTS",
            ),
            (
                Some("GNOME"),
                Some("X11"),
                Some("Café Linux"),
                "&desktop=GNOME&session-type=X11&distribution=Caf%C3%A9%20Linux",
            ),
            (Some("KDE Plasma"), None, None, "&desktop=KDE%20Plasma"),
            (None, Some("Wayland"), None, "&session-type=Wayland"),
            (
                None,
                None,
                Some("Fedora Linux 40"),
                "&distribution=Fedora%20Linux%2040",
            ),
            (None, None, None, ""),
            (Some("  "), Some(""), Some(""), ""),
        ];
        for (desktop, session_type, distribution, suffix) in cases {
            let url = bug_report_url(&environment(desktop, session_type, distribution));
            assert_eq!(
                url,
                format!("{BASE}&kestrel-version={version}{suffix}"),
                "desktop {desktop:?}, session {session_type:?}, distribution {distribution:?}"
            );
        }

        let long = "é".repeat(150);
        let url = bug_report_url(&environment(None, None, Some(&long)));
        assert_eq!(
            url,
            format!(
                "{BASE}&kestrel-version={version}&distribution={}",
                "%C3%A9".repeat(100)
            )
        );
    }

    #[test]
    fn report_options_match_the_form() {
        let desktops = [
            ("COSMIC", Some("COSMIC")),
            ("cosmic", Some("COSMIC")),
            ("sway", Some("Sway")),
            ("Hyprland", Some("Hyprland")),
            ("KDE", Some("KDE Plasma")),
            ("plasma", Some("KDE Plasma")),
            ("ubuntu:GNOME", Some("GNOME")),
            ("GNOME-Classic:GNOME", Some("GNOME")),
            ("X-Cinnamon", Some("Other")),
            ("", None),
            ("  ", None),
        ];
        for (value, expected) in desktops {
            assert_eq!(desktop_option(value).as_deref(), expected, "{value:?}");
        }

        let sessions = [
            ("wayland", Some("Wayland")),
            ("Wayland", Some("Wayland")),
            ("x11", Some("X11")),
            ("X11", Some("X11")),
            ("tty", None),
            ("", None),
        ];
        for (value, expected) in sessions {
            assert_eq!(session_type_option(value).as_deref(), expected, "{value:?}");
        }
    }
}
