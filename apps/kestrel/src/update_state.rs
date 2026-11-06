//! The update check's last attempt, last result, and dismissed version,
//! kept in the state directory.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use kestrel_services::updates::StoredUpdateState;

/// In the state directory: the update check's small record.
const STATE_FILE: &str = "update-check";

pub fn state_path() -> Option<PathBuf> {
    Some(crate::config::state_directory()?.join(STATE_FILE))
}

/// A missing or unreadable record reads as a first run.
pub fn read(path: Option<&Path>) -> StoredUpdateState {
    let Some(path) = path else {
        return StoredUpdateState::default();
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return StoredUpdateState::default();
    };
    parse(&contents)
}

pub fn write(path: Option<&Path>, state: &StoredUpdateState) -> std::io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    crate::private_file::write_private_atomic(path, serialize(state).as_bytes())
}

fn serialize(state: &StoredUpdateState) -> String {
    // Install assets are deliberately not written: the next check re-fetches
    // them, so an offer restored from disk names its version without
    // offering a download until then.
    let mut contents = String::new();
    if let Some(seconds) = state.last_attempt.and_then(seconds) {
        contents.push_str(&format!("attempt={seconds}\n"));
    }
    if let Some(seconds) = state.last_checked.and_then(seconds) {
        contents.push_str(&format!("checked={seconds}\n"));
    }
    if let Some(release) = &state.latest {
        contents.push_str(&format!(
            "latest={} {} {}\n",
            release.version,
            u8::from(release.prerelease),
            release.url
        ));
    }
    if let Some(version) = &state.dismissed {
        contents.push_str(&format!("dismissed={version}\n"));
    }
    contents
}

fn parse(contents: &str) -> StoredUpdateState {
    let mut state = StoredUpdateState::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "attempt" => state.last_attempt = time(value.trim()),
            "checked" => state.last_checked = time(value.trim()),
            "latest" => state.latest = release(value.trim()),
            "dismissed" => state.dismissed = value.trim().parse().ok(),
            _ => {}
        }
    }
    state
}

fn seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

fn time(value: &str) -> Option<SystemTime> {
    let seconds = value.parse().ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn release(value: &str) -> Option<kestrel_core::ReleaseInfo> {
    let mut fields = value.split_whitespace();
    let version = fields.next()?.parse().ok()?;
    let prerelease = fields.next()? == "1";
    let url = fields.next()?;
    Some(kestrel_core::ReleaseInfo {
        version,
        prerelease,
        url: url.to_owned(),
        install: None,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use kestrel_core::ReleaseInfo;
    use kestrel_services::updates::StoredUpdateState;
    use semver::Version;

    use super::{parse, read, state_path, write};

    fn fixture() -> StoredUpdateState {
        StoredUpdateState {
            last_attempt: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            last_checked: Some(UNIX_EPOCH + Duration::from_secs(1_699_999_000)),
            latest: Some(ReleaseInfo {
                version: Version::parse("0.6.0-preview.5").expect("version parses"),
                prerelease: true,
                url: "https://example.invalid/preview-5".to_owned(),
                install: None,
            }),
            dismissed: Some(Version::parse("0.6.0-preview.5").expect("version parses")),
        }
    }

    #[test]
    fn the_record_round_trips_through_the_state_file() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("update-check");
        let state = fixture();
        write(Some(&path), &state).expect("state writes");
        assert_eq!(read(Some(&path)), state);
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let state = parse(
            "attempt=not-a-number\nchecked=1700000000\nlatest=broken\n\
             dismissed=0.6.0\nunknown=value\n",
        );
        assert_eq!(state.last_attempt, None);
        assert_eq!(
            state.last_checked,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
        assert_eq!(state.latest, None);
        assert_eq!(
            state.dismissed.map(|version| version.to_string()),
            Some("0.6.0".to_owned())
        );
    }

    #[test]
    fn the_path_lives_in_the_state_directory() {
        let path = state_path().expect("a home directory is set");
        assert!(path.ends_with("update-check"), "{}", path.display());
    }
}
