//! Optional update-check consent and the release a build may offer.

use serde::{Deserialize, Serialize};

/// Whether the daily update check may contact GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheck {
    /// The user has not answered yet; nothing is contacted.
    #[default]
    Unasked,
    Enabled,
    Disabled,
}

/// App-wide update-check preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UpdatesConfiguration {
    #[serde(default)]
    pub check: UpdateCheck,
}

/// One published release Kestrel may offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: semver::Version,
    pub url: String,
    pub prerelease: bool,
}

/// The newest release worth offering: any newer release on a pre-release
/// build, only newer stable releases on a stable one.
pub fn newest_offer(current: &semver::Version, releases: &[ReleaseInfo]) -> Option<ReleaseInfo> {
    let allow_prerelease = !current.pre.is_empty();
    releases
        .iter()
        .filter(|release| release.version > *current)
        .filter(|release| allow_prerelease || !release.prerelease)
        .max_by(|left, right| left.version.cmp(&right.version))
        .cloned()
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{ReleaseInfo, newest_offer};

    fn release(version: &str, prerelease: bool) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).expect("test versions parse"),
            url: format!("https://example.invalid/{version}"),
            prerelease,
        }
    }

    #[test]
    fn newest_offer_follows_the_stable_and_preview_rule() {
        let current = Version::parse("0.6.0-preview.4").expect("version parses");
        let stable_build = Version::parse("0.6.0").expect("version parses");
        let preview = |version: &str| release(version, true);
        let stable = |version: &str| release(version, false);

        let cases: [(&str, &Version, Vec<ReleaseInfo>, Option<&str>); 7] = [
            (
                "a newer preview is offered to a preview build",
                &current,
                vec![preview("0.6.0-preview.5")],
                Some("0.6.0-preview.5"),
            ),
            (
                "the running version is not offered",
                &current,
                vec![preview("0.6.0-preview.4")],
                None,
            ),
            (
                "older releases are not offered",
                &current,
                vec![preview("0.6.0-preview.3"), preview("0.5.0")],
                None,
            ),
            (
                "a newer stable release is offered to a preview build",
                &current,
                vec![preview("0.6.0-preview.5"), stable("0.6.0")],
                Some("0.6.0"),
            ),
            (
                "a preview is not offered to a stable build",
                &stable_build,
                vec![preview("0.7.0-preview.1")],
                None,
            ),
            (
                "a newer stable release is offered to a stable build",
                &stable_build,
                vec![stable("0.6.1")],
                Some("0.6.1"),
            ),
            (
                "preview numbers compare numerically",
                &current,
                vec![preview("0.6.0-preview.9"), preview("0.6.0-preview.10")],
                Some("0.6.0-preview.10"),
            ),
        ];

        for (case, running, releases, expected) in cases {
            assert_eq!(
                newest_offer(running, &releases)
                    .map(|release| release.version.to_string())
                    .as_deref(),
                expected,
                "{case}"
            );
        }
    }
}
