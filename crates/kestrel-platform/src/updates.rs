//! The release list behind the opt-in update check, fetched through `curl`.
//! Curl stderr stays private because it may contain resolved addresses.

use std::{
    env, fmt,
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use kestrel_core::{InstallAssets, ReleaseInfo};
use serde::Deserialize;

use crate::{applications::resolve_executable, cancellation::CancellationToken};

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
    #[serde(default)]
    pub assets: Vec<RawAsset>,
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

impl RawRelease {
    /// Turns a published release into an offer; drafts and tags that are not
    /// `v<semver>` are skipped.
    pub fn into_release(self) -> Option<ReleaseInfo> {
        if self.draft {
            return None;
        }
        let version: semver::Version = self.tag_name.strip_prefix('v')?.parse().ok()?;
        let install = self.install_assets(&version, std::env::consts::ARCH);
        Some(ReleaseInfo {
            prerelease: self.prerelease || !version.pre.is_empty(),
            version,
            url: self.html_url,
            install,
        })
    }

    /// The signed AppImage pair, published only for x86_64.
    fn install_assets(&self, version: &semver::Version, arch: &str) -> Option<InstallAssets> {
        if arch != "x86_64" {
            return None;
        }
        let file_name = format!("Kestrel-{version}-x86_64.AppImage");
        let signature_name = format!("{file_name}.minisig");
        let appimage = self.assets.iter().find(|asset| asset.name == file_name)?;
        let signature = self
            .assets
            .iter()
            .find(|asset| asset.name == signature_name)?;
        Some(InstallAssets {
            appimage_url: appimage.browser_download_url.clone(),
            appimage_size: appimage.size,
            signature_url: signature.browser_download_url.clone(),
            file_name,
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
    Cancelled,
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
pub fn fetch_releases(cancel: &CancellationToken) -> Result<Vec<RawRelease>, UpdateFetchError> {
    let executable =
        resolve_executable("curl", env::var_os("PATH").as_deref()).ok_or_else(|| {
            UpdateFetchError::new(UpdateFetchErrorKind::CurlMissing, "curl is not installed.")
        })?;
    fetch_releases_with(&executable, cancel)
}

/// Runs the fetch through `executable`; the caller resolves the curl binary.
pub(crate) fn fetch_releases_with(
    executable: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<RawRelease>, UpdateFetchError> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let mut command = command(executable);
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
    let deadline = Instant::now() + Duration::from_secs(MAX_TIME_SECONDS) + KILL_GRACE;
    let contents = loop {
        if cancel.is_cancelled() {
            kill_and_reap(&mut child, group);
            return Err(cancelled());
        }
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(Ok(contents)) => break contents,
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                kill_and_reap(&mut child, group);
                return Err(UpdateFetchError::new(
                    UpdateFetchErrorKind::Network,
                    "The release list could not be read.",
                ));
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                kill_and_reap(&mut child, group);
                return Err(UpdateFetchError::new(
                    UpdateFetchErrorKind::Network,
                    "The release request did not finish in time.",
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    if contents.len() > RESPONSE_LIMIT_BYTES {
        kill_and_reap(&mut child, group);
        return Err(UpdateFetchError::new(
            UpdateFetchErrorKind::TooLarge,
            "The release list was too large.",
        ));
    }
    let status = wait_process(&mut child, group, cancel)?;
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

fn cancelled() -> UpdateFetchError {
    UpdateFetchError::new(
        UpdateFetchErrorKind::Cancelled,
        "The update check was cancelled.",
    )
}

fn command(executable: &Path) -> Command {
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
    cancel: &CancellationToken,
) -> Result<std::process::ExitStatus, UpdateFetchError> {
    let deadline = Instant::now() + Duration::from_secs(MAX_TIME_SECONDS) + KILL_GRACE;
    loop {
        if cancel.is_cancelled() {
            kill_and_reap(child, group);
            return Err(cancelled());
        }
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
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    use super::{RawRelease, UpdateFetchErrorKind};
    use crate::cancellation::CancellationToken;

    /// A fake curl that ignores its arguments and runs `body`.
    fn stub(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("curl");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stub is written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("stub is executable");
        path
    }

    fn process_is_gone(pid: i32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
                .is_none_or(|state| state == 'Z'),
        }
    }

    fn wait_for_pid(path: &Path) -> i32 {
        for _ in 0..200 {
            if let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the stub never recorded its descendant");
    }

    #[test]
    fn cancelling_ends_the_fetch_and_its_whole_process_group() {
        let dir = TempDir::new().expect("temp dir");
        let pid_file = dir.path().join("descendant.pid");
        let executable = stub(
            &dir,
            &format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
        );
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        let watched = pid_file.clone();
        let watcher = std::thread::spawn(move || {
            let pid = wait_for_pid(&watched);
            canceller.cancel();
            pid
        });

        let started = Instant::now();
        let error =
            super::fetch_releases_with(&executable, &cancel).expect_err("a cancelled fetch fails");
        let pid = watcher.join().expect("watcher finishes");

        assert_eq!(error.kind, UpdateFetchErrorKind::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the fetch returns promptly"
        );
        let mut gone = false;
        for _ in 0..100 {
            if process_is_gone(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone, "no descendant outlives a cancelled fetch");
    }

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
            assets: Vec::new(),
        }
        .into_release()
        .expect("the tag parses");
        assert!(release.prerelease);
    }

    const ASSETS: &str = r#"[
        {
            "tag_name": "v0.7.0",
            "prerelease": false,
            "draft": false,
            "html_url": "https://example.invalid/v0.7.0",
            "assets": [
                {
                    "name": "Kestrel-0.7.0-x86_64.AppImage",
                    "browser_download_url": "https://example.invalid/download/appimage",
                    "size": 73400320
                },
                {
                    "name": "Kestrel-0.7.0-x86_64.AppImage.sha256",
                    "browser_download_url": "https://example.invalid/download/checksum",
                    "size": 96
                },
                {
                    "name": "Kestrel-0.7.0-x86_64.AppImage.minisig",
                    "browser_download_url": "https://example.invalid/download/signature",
                    "size": 291
                }
            ]
        },
        {
            "tag_name": "v0.8.0",
            "prerelease": false,
            "draft": false,
            "html_url": "https://example.invalid/v0.8.0",
            "assets": [
                {
                    "name": "Kestrel-0.8.0-x86_64.AppImage",
                    "browser_download_url": "https://example.invalid/download/unsigned",
                    "size": 73400320
                }
            ]
        }
    ]"#;

    #[test]
    fn a_release_with_a_signed_appimage_offers_its_assets() {
        let releases =
            serde_json::from_str::<Vec<RawRelease>>(ASSETS).expect("the fixture is a release list");
        let install = releases
            .into_iter()
            .find(|release| release.tag_name == "v0.7.0")
            .and_then(RawRelease::into_release)
            .and_then(|release| release.install)
            .expect("the signed release offers an install");
        assert_eq!(install.file_name, "Kestrel-0.7.0-x86_64.AppImage");
        assert_eq!(
            install.appimage_url,
            "https://example.invalid/download/appimage"
        );
        assert_eq!(install.appimage_size, 73_400_320);
        assert_eq!(
            install.signature_url,
            "https://example.invalid/download/signature"
        );
    }

    #[test]
    fn an_appimage_without_its_signature_offers_no_install() {
        let releases =
            serde_json::from_str::<Vec<RawRelease>>(ASSETS).expect("the fixture is a release list");
        let unsigned = releases
            .into_iter()
            .find(|release| release.tag_name == "v0.8.0")
            .expect("the unsigned release is listed")
            .into_release()
            .expect("the tag parses");
        assert!(unsigned.install.is_none());
    }

    #[test]
    fn another_architecture_offers_no_install() {
        let release = serde_json::from_str::<Vec<RawRelease>>(ASSETS)
            .expect("the fixture is a release list")
            .into_iter()
            .find(|release| release.tag_name == "v0.7.0")
            .expect("the signed release is listed");
        let version = semver::Version::parse("0.7.0").expect("version parses");
        assert!(release.install_assets(&version, "x86_64").is_some());
        assert!(release.install_assets(&version, "aarch64").is_none());
    }
}
