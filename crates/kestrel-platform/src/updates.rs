//! The release list behind the opt-in update check, fetched through `curl`.
//! Curl stderr stays private because it may contain resolved addresses.

use std::{
    env, fmt,
    io::Read,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use kestrel_core::ReleaseInfo;
use serde::Deserialize;

use crate::applications::resolve_executable;

pub const RELEASES_URL: &str =
    "https://api.github.com/repos/kamleshkc2002/kestrel/releases?per_page=20";
const CONNECT_TIMEOUT_SECONDS: u64 = 10;
const MAX_TIME_SECONDS: u64 = 20;
const RESPONSE_LIMIT_BYTES: usize = 512 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const KILL_GRACE: Duration = Duration::from_millis(100);

/// One entry as GitHub's release list reports it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawRelease {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    pub html_url: String,
}

impl RawRelease {
    /// Turns a published release into an offer; drafts and tags that are not
    /// `v<semver>` are skipped.
    pub fn into_release(self) -> Option<ReleaseInfo> {
        if self.draft {
            return None;
        }
        let version: semver::Version = self.tag_name.strip_prefix('v')?.parse().ok()?;
        Some(ReleaseInfo {
            prerelease: self.prerelease || !version.pre.is_empty(),
            version,
            url: self.html_url,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFetchErrorKind {
    CurlMissing,
    SpawnFailed,
    Network,
    Http,
    TooLarge,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFetchError {
    pub kind: UpdateFetchErrorKind,
    pub message: String,
}

impl UpdateFetchError {
    fn new(kind: UpdateFetchErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for UpdateFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpdateFetchError {}

/// Fetches the recent releases of Kestrel's repository, bounded in time and size.
pub fn fetch_releases() -> Result<Vec<RawRelease>, UpdateFetchError> {
    let executable =
        resolve_executable("curl", env::var_os("PATH").as_deref()).ok_or_else(|| {
            UpdateFetchError::new(UpdateFetchErrorKind::CurlMissing, "curl is not installed.")
        })?;
    let mut command = command(&executable);
    let mut child = crate::spawn_with_busy_retry(&mut command).map_err(|_| {
        UpdateFetchError::new(
            UpdateFetchErrorKind::SpawnFailed,
            "curl could not be started.",
        )
    })?;
    let group = child.id() as libc::pid_t;
    let stdout = child.stdout.take().ok_or_else(|| {
        UpdateFetchError::new(
            UpdateFetchErrorKind::SpawnFailed,
            "curl did not provide its output.",
        )
    })?;
    let receiver = spawn_reader(stdout);
    let contents = match receiver.recv_timeout(Duration::from_secs(MAX_TIME_SECONDS) + KILL_GRACE) {
        Ok(Ok(contents)) => contents,
        Ok(Err(_)) | Err(_) => {
            kill_and_reap(&mut child, group);
            return Err(UpdateFetchError::new(
                UpdateFetchErrorKind::Network,
                "The release list could not be read.",
            ));
        }
    };
    if contents.len() > RESPONSE_LIMIT_BYTES {
        kill_and_reap(&mut child, group);
        return Err(UpdateFetchError::new(
            UpdateFetchErrorKind::TooLarge,
            "The release list was too large.",
        ));
    }
    let status = wait_process(&mut child, group)?;
    if !status.success() {
        return Err(error_for_exit(status.code()));
    }
    serde_json::from_slice(&contents).map_err(|_| {
        UpdateFetchError::new(
            UpdateFetchErrorKind::Malformed,
            "The release list could not be understood.",
        )
    })
}

fn command(executable: &PathBuf) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("--disable")
        .arg("--silent")
        .arg("--fail")
        .arg("--proto")
        .arg("=https")
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECONDS.to_string())
        .arg("--max-time")
        .arg(MAX_TIME_SECONDS.to_string())
        .arg("--header")
        .arg("Accept: application/vnd.github+json")
        .arg(RELEASES_URL)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

/// Reads at most one byte past the limit, so an oversized response is known
/// without being kept.
fn spawn_reader(
    mut stdout: impl Read + Send + 'static,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("kestrel-update-read".to_owned())
        .spawn(move || {
            let mut buffer = Vec::new();
            let result = stdout
                .by_ref()
                .take((RESPONSE_LIMIT_BYTES + 1) as u64)
                .read_to_end(&mut buffer)
                .map(|_| buffer);
            let _ = sender.send(result);
        });
    receiver
}

fn wait_process(
    child: &mut Child,
    group: libc::pid_t,
) -> Result<std::process::ExitStatus, UpdateFetchError> {
    let deadline = Instant::now() + Duration::from_secs(MAX_TIME_SECONDS) + KILL_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(child, group);
                return Err(UpdateFetchError::new(
                    UpdateFetchErrorKind::Network,
                    "The release request could not be waited for.",
                ));
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap(child, group);
            return Err(UpdateFetchError::new(
                UpdateFetchErrorKind::Network,
                "The release request did not finish in time.",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn kill_group(group: libc::pid_t) {
    // SAFETY: The negative ID targets this run's group; `ESRCH` is harmless.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

fn kill_and_reap(child: &mut Child, group: libc::pid_t) {
    kill_group(group);
    let _ = child.kill();
    let _ = child.wait();
}

/// Curl stderr stays private because it may contain resolved addresses.
fn error_for_exit(code: Option<i32>) -> UpdateFetchError {
    match code {
        Some(22) => UpdateFetchError::new(
            UpdateFetchErrorKind::Http,
            "GitHub returned an error instead of the release list.",
        ),
        Some(6 | 7 | 28) => UpdateFetchError::new(
            UpdateFetchErrorKind::Network,
            "GitHub could not be reached.",
        ),
        Some(35 | 51 | 58 | 60) => UpdateFetchError::new(
            UpdateFetchErrorKind::Network,
            "A secure connection to GitHub could not be verified.",
        ),
        _ => UpdateFetchError::new(UpdateFetchErrorKind::Network, "The release request failed."),
    }
}

#[cfg(test)]
mod tests {
    use super::RawRelease;

    const RELEASES: &str = r#"[
        {
            "tag_name": "v0.6.0-preview.5",
            "prerelease": true,
            "draft": false,
            "html_url": "https://example.invalid/preview-5"
        },
        {
            "tag_name": "v0.6.0-preview.4",
            "prerelease": true,
            "draft": false,
            "html_url": "https://example.invalid/preview-4"
        },
        {
            "tag_name": "v0.7.0",
            "prerelease": false,
            "draft": true,
            "html_url": "https://example.invalid/draft"
        },
        {
            "tag_name": "nightly",
            "prerelease": false,
            "draft": false,
            "html_url": "https://example.invalid/nightly"
        },
        {
            "tag_name": "0.6.1",
            "prerelease": false,
            "draft": false,
            "html_url": "https://example.invalid/no-v"
        }
    ]"#;

    #[test]
    fn drafts_and_unparseable_tags_are_skipped() {
        let releases = serde_json::from_str::<Vec<RawRelease>>(RELEASES)
            .expect("the fixture is a release list");
        let versions = releases
            .into_iter()
            .filter_map(RawRelease::into_release)
            .map(|release| (release.version.to_string(), release.prerelease, release.url))
            .collect::<Vec<_>>();
        assert_eq!(
            versions,
            [
                (
                    "0.6.0-preview.5".to_owned(),
                    true,
                    "https://example.invalid/preview-5".to_owned()
                ),
                (
                    "0.6.0-preview.4".to_owned(),
                    true,
                    "https://example.invalid/preview-4".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn the_reader_stops_one_byte_past_the_limit() {
        let source = std::io::Cursor::new(vec![b'0'; super::RESPONSE_LIMIT_BYTES + 10]);
        let receiver = super::spawn_reader(source);
        let buffer = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the reader returns")
            .expect("the source is readable");
        assert_eq!(buffer.len(), super::RESPONSE_LIMIT_BYTES + 1);
    }

    #[test]
    fn a_release_flag_marks_a_pre_release_tag_too() {
        let release = RawRelease {
            tag_name: "v0.6.0".to_owned(),
            prerelease: true,
            draft: false,
            html_url: "https://example.invalid/release".to_owned(),
        }
        .into_release()
        .expect("the tag parses");
        assert!(release.prerelease);
    }
}
