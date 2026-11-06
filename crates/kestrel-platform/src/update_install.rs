//! Downloading, verifying, and installing a signed AppImage update.
//!
//! Nothing here starts on its own: the caller downloads only after an explicit
//! user command and installs only after another one.

use std::{
    env, fmt, fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use kestrel_core::InstallAssets;
use minisign_verify::{PublicKey, Signature};

use crate::{applications::resolve_executable, cancellation::CancellationToken};

/// The release signing key, built into the binary so a changed worktree file
/// cannot swap the trust anchor.
const RELEASE_PUBLIC_KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/release-signing.pub"
));

const CONNECT_TIMEOUT_SECONDS: u64 = 10;
const TRANSFER_TIMEOUT_SECONDS: u64 = 600;
/// The absolute cap on one update file.
const MAX_UPDATE_BYTES: u64 = 1024 * 1024 * 1024;
const SIZE_SLACK_BYTES: u64 = 4 * 1024;
const SIGNATURE_LIMIT_BYTES: u64 = 16 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_millis(100);
const APPIMAGE_MODE: u32 = 0o755;
const TEMP_MODE: u32 = 0o600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateInstallErrorKind {
    NotAppImage,
    NoInstallAssets,
    CurlMissing,
    Network,
    Http,
    TooLarge,
    Cancelled,
    BadSignature,
    WrongFile,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInstallError {
    pub kind: UpdateInstallErrorKind,
    pub message: String,
}

impl UpdateInstallError {
    fn new(kind: UpdateInstallErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// A disk failure, described without the path so logs stay quiet.
    fn io(message: impl Into<String>) -> Self {
        Self::new(UpdateInstallErrorKind::Io, message)
    }

    fn cancelled() -> Self {
        Self::new(
            UpdateInstallErrorKind::Cancelled,
            "The update download was cancelled.",
        )
    }
}

impl fmt::Display for UpdateInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpdateInstallError {}

/// The AppImage this process runs from, when it can update itself.
pub fn running_appimage() -> Option<PathBuf> {
    let value = env::var_os("APPIMAGE")?;
    validated_appimage(Path::new(&value))
}

/// An absolute, executable, user-owned file in a writable directory.
fn validated_appimage(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return None;
    }
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    if !directory_is_writable(path.parent()?) {
        return None;
    }
    Some(path.to_path_buf())
}

fn directory_is_writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the C string outlives the call, which only inspects it.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// A downloaded AppImage and its signature, still under temporary names.
/// Dropping an unconsumed update removes both files, so quitting or a failed
/// install cannot leave a staged download beside the running AppImage.
#[derive(Debug, PartialEq, Eq)]
pub struct DownloadedUpdate {
    pub appimage: PathBuf,
    pub signature: PathBuf,
    /// Set once the AppImage has been renamed into place.
    consumed: bool,
}

impl DownloadedUpdate {
    fn staged(appimage: PathBuf, signature: PathBuf) -> Self {
        Self {
            appimage,
            signature,
            consumed: false,
        }
    }
}

impl Drop for DownloadedUpdate {
    fn drop(&mut self) {
        if self.consumed {
            return;
        }
        let _ = fs::remove_file(&self.appimage);
        let _ = fs::remove_file(&self.signature);
    }
}

/// Downloads the release's AppImage and signature into `dest_dir`, so the
/// install can rename them atomically beside the running file.
pub fn download_update(
    assets: &InstallAssets,
    dest_dir: &Path,
    cancel: &CancellationToken,
    progress: &dyn Fn(u64, u64),
) -> Result<DownloadedUpdate, UpdateInstallError> {
    let executable =
        resolve_executable("curl", env::var_os("PATH").as_deref()).ok_or_else(|| {
            UpdateInstallError::new(
                UpdateInstallErrorKind::CurlMissing,
                "curl is not installed.",
            )
        })?;
    download_update_with(&executable, assets, dest_dir, cancel, progress)
}

/// Runs the download through `executable`; the caller resolves the curl
/// binary, which tests replace with a script.
pub(crate) fn download_update_with(
    executable: &Path,
    assets: &InstallAssets,
    dest_dir: &Path,
    cancel: &CancellationToken,
    progress: &dyn Fn(u64, u64),
) -> Result<DownloadedUpdate, UpdateInstallError> {
    if cancel.is_cancelled() {
        return Err(UpdateInstallError::cancelled());
    }
    let limit = transfer_limit(assets.appimage_size)?;
    let appimage = create_temporary(dest_dir, "appimage")?;
    let signature = match create_temporary(dest_dir, "signature") {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&appimage);
            return Err(error);
        }
    };
    let downloaded = DownloadedUpdate::staged(appimage, signature);
    let attempt = (|| {
        transfer(
            executable,
            &assets.appimage_url,
            limit,
            &downloaded.appimage,
            assets.appimage_size,
            cancel,
            progress,
        )?;
        transfer(
            executable,
            &assets.signature_url,
            SIGNATURE_LIMIT_BYTES,
            &downloaded.signature,
            0,
            cancel,
            &|_, _| {},
        )?;
        Ok(())
    })();
    match attempt {
        Ok(()) => Ok(downloaded),
        // The guard removes both staged files as it drops.
        Err(error) => Err(error),
    }
}

/// The declared size plus a little slack, never past the absolute cap.
fn transfer_limit(declared: u64) -> Result<u64, UpdateInstallError> {
    if declared > MAX_UPDATE_BYTES {
        return Err(too_large());
    }
    if declared == 0 {
        return Ok(MAX_UPDATE_BYTES);
    }
    Ok(declared
        .saturating_add(SIZE_SLACK_BYTES)
        .min(MAX_UPDATE_BYTES))
}

fn too_large() -> UpdateInstallError {
    UpdateInstallError::new(
        UpdateInstallErrorKind::TooLarge,
        "The update is larger than Kestrel will download.",
    )
}

/// One transfer through curl, polling the growing file for progress and the
/// cancellation token.
fn transfer(
    executable: &Path,
    url: &str,
    limit: u64,
    destination: &Path,
    total: u64,
    cancel: &CancellationToken,
    progress: &dyn Fn(u64, u64),
) -> Result<(), UpdateInstallError> {
    let mut command = transfer_command(executable, url, limit, destination);
    let mut child = crate::spawn_with_busy_retry(&mut command).map_err(|_| {
        UpdateInstallError::new(
            UpdateInstallErrorKind::Network,
            "The update download could not be started.",
        )
    })?;
    let group = child.id() as libc::pid_t;
    let deadline = Instant::now() + Duration::from_secs(TRANSFER_TIMEOUT_SECONDS) + KILL_GRACE;
    loop {
        if cancel.is_cancelled() {
            kill_and_reap(&mut child, group);
            return Err(UpdateInstallError::cancelled());
        }
        let received = file_length(destination);
        if total > 0 {
            progress(received, total);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(error_for_exit(status.code()));
                }
                // Curl has closed the file, so this length is final.
                let received = file_length(destination);
                if received > limit {
                    return Err(too_large());
                }
                if total > 0 {
                    progress(received, total);
                }
                return Ok(());
            }
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(&mut child, group);
                return Err(UpdateInstallError::new(
                    UpdateInstallErrorKind::Network,
                    "The update download could not be waited for.",
                ));
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap(&mut child, group);
            return Err(UpdateInstallError::new(
                UpdateInstallErrorKind::Network,
                "The update download did not finish in time.",
            ));
        }
        std::thread::sleep(PROGRESS_INTERVAL);
    }
}

fn transfer_command(executable: &Path, url: &str, limit: u64, destination: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("--disable")
        .arg("--silent")
        .arg("--fail")
        .arg("--proto")
        .arg("=https")
        .arg("--location")
        .arg("--proto-redir")
        .arg("=https")
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECONDS.to_string())
        .arg("--max-time")
        .arg(TRANSFER_TIMEOUT_SECONDS.to_string())
        .arg("--max-filesize")
        .arg(limit.to_string())
        .arg("--output")
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

fn file_length(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Curl stderr stays private because it may contain resolved addresses.
fn error_for_exit(code: Option<i32>) -> UpdateInstallError {
    match code {
        Some(22) => UpdateInstallError::new(
            UpdateInstallErrorKind::Http,
            "GitHub returned an error instead of the update.",
        ),
        Some(63) => too_large(),
        Some(6 | 7 | 28) => UpdateInstallError::new(
            UpdateInstallErrorKind::Network,
            "GitHub could not be reached.",
        ),
        Some(35 | 51 | 58 | 60) => UpdateInstallError::new(
            UpdateInstallErrorKind::Network,
            "A secure connection to GitHub could not be verified.",
        ),
        _ => UpdateInstallError::new(
            UpdateInstallErrorKind::Network,
            "The update download failed.",
        ),
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

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique 0600 file beside the running AppImage, so the final rename stays
/// on one filesystem.
fn create_temporary(dest_dir: &Path, label: &str) -> Result<PathBuf, UpdateInstallError> {
    for _ in 0..8 {
        let count = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            ".kestrel-update-{}-{count}-{label}.part",
            std::process::id()
        );
        let path = dest_dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(TEMP_MODE)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(UpdateInstallError::io(
                    "The update could not be written beside Kestrel.",
                ));
            }
        }
    }
    Err(UpdateInstallError::io(
        "The update could not be given a temporary file.",
    ))
}

/// Checks the downloaded AppImage against the built-in release key.
pub fn verify_update(
    downloaded: &DownloadedUpdate,
    expected_file_name: &str,
) -> Result<(), UpdateInstallError> {
    verify_update_with_key(downloaded, expected_file_name, RELEASE_PUBLIC_KEY)
}

/// Checks the download against `public_key`, which tests replace.
pub fn verify_update_with_key(
    downloaded: &DownloadedUpdate,
    expected_file_name: &str,
    public_key: &str,
) -> Result<(), UpdateInstallError> {
    let public_key = PublicKey::decode(public_key).map_err(|_| {
        UpdateInstallError::new(
            UpdateInstallErrorKind::BadSignature,
            "The update signing key could not be read.",
        )
    })?;
    let signature_text = fs::read_to_string(&downloaded.signature).map_err(|_| {
        UpdateInstallError::new(
            UpdateInstallErrorKind::BadSignature,
            "The update signature could not be read.",
        )
    })?;
    let signature = Signature::decode(&signature_text).map_err(|_| {
        UpdateInstallError::new(
            UpdateInstallErrorKind::BadSignature,
            "The update signature could not be read.",
        )
    })?;
    if signature.trusted_comment() != expected_file_name {
        return Err(UpdateInstallError::new(
            UpdateInstallErrorKind::WrongFile,
            "The update signature names a different file.",
        ));
    }
    let file = fs::File::open(&downloaded.appimage)
        .map_err(|_| UpdateInstallError::io("The downloaded update could not be read back."))?;
    verify_file(&public_key, &signature, file)
}

fn verify_file(
    public_key: &PublicKey,
    signature: &Signature,
    mut file: fs::File,
) -> Result<(), UpdateInstallError> {
    let mut verifier = match public_key.verify_stream(signature) {
        Ok(verifier) => verifier,
        Err(minisign_verify::Error::UnsupportedLegacyMode) => {
            // Legacy signatures cover the file itself, so it is read whole.
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).map_err(|_| {
                UpdateInstallError::io("The downloaded update could not be read back.")
            })?;
            return public_key
                .verify(&contents, signature, true)
                .map_err(|error| signature_error(&error));
        }
        Err(error) => return Err(signature_error(&error)),
    };
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| UpdateInstallError::io("The downloaded update could not be read back."))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize().map_err(|error| signature_error(&error))
}

fn signature_error(error: &minisign_verify::Error) -> UpdateInstallError {
    let message = match error {
        minisign_verify::Error::UnexpectedKeyId => {
            "The update was signed with a different key than Kestrel's."
        }
        minisign_verify::Error::InvalidSignature => {
            "The update's signature does not match the downloaded file."
        }
        _ => "The update's signature could not be verified.",
    };
    UpdateInstallError::new(UpdateInstallErrorKind::BadSignature, message)
}

/// A finished install; the previous AppImage is kept beside the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledUpdate {
    pub previous: PathBuf,
}

/// Replaces `current` with the verified download, keeping the old file as
/// `<name>.previous`. The staged files belong to `downloaded`, which removes
/// whatever the install did not consume.
pub fn install_update(
    current: &Path,
    mut downloaded: DownloadedUpdate,
) -> Result<InstalledUpdate, UpdateInstallError> {
    let previous = previous_path(current);
    set_mode(&downloaded.appimage, APPIMAGE_MODE)?;
    sync_file(&downloaded.appimage)?;
    if previous.exists() {
        fs::remove_file(&previous).map_err(|_| {
            UpdateInstallError::io("The earlier update copy could not be replaced.")
        })?;
    }
    if fs::hard_link(current, &previous).is_err() {
        fs::copy(current, &previous)
            .map_err(|_| UpdateInstallError::io("The running AppImage could not be kept."))?;
        set_mode(&previous, APPIMAGE_MODE)?;
        sync_file(&previous)?;
    }
    fs::rename(&downloaded.appimage, current)
        .map_err(|_| UpdateInstallError::io("The update could not replace Kestrel."))?;
    // The file at the running path is now the one the guard held.
    downloaded.consumed = true;
    let synced = match current.parent() {
        Some(directory) => sync_directory(directory),
        None => Ok(()),
    };
    let _ = fs::remove_file(&downloaded.signature);
    synced.map(|()| InstalledUpdate { previous })
}

/// Restores `<name>.previous` over `current` after an install.
pub fn roll_back(current: &Path) -> Result<(), UpdateInstallError> {
    let previous = previous_path(current);
    if !previous.is_file() {
        return Err(UpdateInstallError::io(
            "There is no earlier version to restore.",
        ));
    }
    fs::rename(&previous, current)
        .map_err(|_| UpdateInstallError::io("The earlier version could not be restored."))?;
    if let Some(directory) = current.parent() {
        sync_directory(directory)?;
    }
    Ok(())
}

/// Whether an earlier AppImage waits beside `current`.
pub fn previous_version_available(current: &Path) -> bool {
    previous_path(current).is_file()
}

fn previous_path(current: &Path) -> PathBuf {
    let name = current
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kestrel".to_owned());
    current.with_file_name(format!("{name}.previous"))
}

fn set_mode(path: &Path, mode: u32) -> Result<(), UpdateInstallError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| UpdateInstallError::io("The update's permissions could not be set."))
}

fn sync_file(path: &Path) -> Result<(), UpdateInstallError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| UpdateInstallError::io("The update could not be written out."))?;
    file.sync_all()
        .map_err(|_| UpdateInstallError::io("The update could not be written out."))
}

fn sync_directory(directory: &Path) -> Result<(), UpdateInstallError> {
    let handle = fs::File::open(directory)
        .map_err(|_| UpdateInstallError::io("The update could not be written out."))?;
    handle
        .sync_all()
        .map_err(|_| UpdateInstallError::io("The update could not be written out."))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use kestrel_core::InstallAssets;
    use tempfile::TempDir;

    use super::{
        DownloadedUpdate, UpdateInstallErrorKind, install_update, previous_version_available,
        roll_back, validated_appimage, verify_update_with_key,
    };
    use crate::cancellation::CancellationToken;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/update");
    const FILE_NAME: &str = "Kestrel-9.9.9-x86_64.AppImage";

    fn fixture(name: &str) -> PathBuf {
        Path::new(FIXTURES).join(name)
    }

    fn test_key() -> String {
        fs::read_to_string(fixture("test-key.pub")).expect("the test key is committed")
    }

    /// A downloaded update built from fixtures in a temporary directory.
    fn downloaded(directory: &TempDir, signature: &str) -> DownloadedUpdate {
        let appimage = directory.path().join(FILE_NAME);
        let signature_path = directory.path().join(format!("{FILE_NAME}.minisig"));
        fs::copy(fixture(FILE_NAME), &appimage).expect("the fixture copies");
        fs::copy(fixture(signature), &signature_path).expect("the signature copies");
        DownloadedUpdate::staged(appimage, signature_path)
    }

    fn verify(directory: &TempDir, signature: &str) -> Result<(), super::UpdateInstallError> {
        verify_update_with_key(&downloaded(directory, signature), FILE_NAME, &test_key())
    }

    #[test]
    fn a_valid_signature_passes() {
        let directory = TempDir::new().expect("temporary directory");
        verify(&directory, "Kestrel-9.9.9-x86_64.AppImage.minisig")
            .expect("the committed signature verifies");
    }

    #[test]
    fn a_legacy_signature_also_passes() {
        let directory = TempDir::new().expect("temporary directory");
        verify(&directory, "Kestrel-9.9.9-x86_64.AppImage.legacy.minisig")
            .expect("the legacy signature verifies");
    }

    #[test]
    fn a_modified_file_fails_the_signature_check() {
        let directory = TempDir::new().expect("temporary directory");
        let download = downloaded(&directory, "Kestrel-9.9.9-x86_64.AppImage.minisig");
        fs::write(&download.appimage, b"tampered payload").expect("the fixture is replaced");
        let error = verify_update_with_key(&download, FILE_NAME, &test_key())
            .expect_err("a modified file is rejected");
        assert_eq!(error.kind, UpdateInstallErrorKind::BadSignature);
    }

    #[test]
    fn a_signature_naming_another_file_is_rejected() {
        let directory = TempDir::new().expect("temporary directory");
        let error = verify(
            &directory,
            "Kestrel-9.9.9-x86_64.AppImage.wrong-comment.minisig",
        )
        .expect_err("a mismatched trusted comment is rejected");
        assert_eq!(error.kind, UpdateInstallErrorKind::WrongFile);
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let directory = TempDir::new().expect("temporary directory");
        let error = verify(
            &directory,
            "Kestrel-9.9.9-x86_64.AppImage.other-key.minisig",
        )
        .expect_err("another key is rejected");
        assert_eq!(error.kind, UpdateInstallErrorKind::BadSignature);
    }

    #[test]
    fn only_an_executable_user_owned_appimage_qualifies() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("Kestrel.AppImage");
        fs::write(&path, b"payload").expect("the file writes");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode is set");
        assert!(
            validated_appimage(&path).is_none(),
            "a file without execute permission is not an AppImage"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("mode is set");
        assert_eq!(validated_appimage(&path), Some(path.clone()));
        assert!(
            validated_appimage(Path::new("relative.AppImage")).is_none(),
            "a relative path is refused"
        );
    }

    /// A fake curl that writes `payload` to the file named after `-o`.
    fn stub_curl(dir: &TempDir, payload: &str, linger: bool) -> PathBuf {
        let path = dir.path().join("curl");
        let extra = if linger { "sleep 30\n" } else { "" };
        let body = format!(
            "#!/bin/sh\nout=\"\"\nprev=\"\"\nfor arg in \"$@\"; do\n  \
             if [ \"$prev\" = \"-o\" ] || [ \"$prev\" = \"--output\" ]; then out=\"$arg\"; fi\n  \
             prev=\"$arg\"\ndone\nprintf '%s' '{payload}' > \"$out\"\n{extra}"
        );
        fs::write(&path, body).expect("stub is written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("stub is executable");
        path
    }

    fn assets(size: u64) -> InstallAssets {
        InstallAssets {
            appimage_url: "https://example.invalid/appimage".to_owned(),
            appimage_size: size,
            signature_url: "https://example.invalid/signature".to_owned(),
            file_name: FILE_NAME.to_owned(),
        }
    }

    fn temporary_files(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .expect("the directory lists")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".kestrel-update-"))
            })
            .collect()
    }

    #[test]
    fn a_download_writes_both_temporary_files() {
        let directory = TempDir::new().expect("temporary directory");
        let payload = "0123456789";
        let executable = stub_curl(&directory, payload, false);
        let seen = std::cell::RefCell::new(Vec::new());
        let download = super::download_update_with(
            &executable,
            &assets(payload.len() as u64),
            directory.path(),
            &CancellationToken::new(),
            &|received, total| seen.borrow_mut().push((received, total)),
        )
        .expect("the download succeeds");
        assert_eq!(
            fs::read(&download.appimage).expect("the file reads"),
            payload.as_bytes()
        );
        assert!(download.signature.is_file());
        let mode = fs::metadata(&download.appimage)
            .expect("the file is readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "temporary files are private");
        assert!(
            seen.borrow()
                .iter()
                .any(|(_, total)| *total == payload.len() as u64),
            "progress reports the expected size"
        );
        drop(download);
        assert!(temporary_files(directory.path()).is_empty());
    }

    #[test]
    fn dropping_a_staged_download_removes_both_files() {
        let directory = TempDir::new().expect("temporary directory");
        let download = downloaded(&directory, "Kestrel-9.9.9-x86_64.AppImage.minisig");
        let appimage = download.appimage.clone();
        let signature = download.signature.clone();
        assert!(appimage.exists() && signature.exists(), "the files staged");
        drop(download);
        assert!(!appimage.exists(), "the staged AppImage is removed");
        assert!(!signature.exists(), "the staged signature is removed");
    }

    #[test]
    fn a_download_over_the_size_cap_is_removed() {
        let directory = TempDir::new().expect("temporary directory");
        // The stub writes more than the release declares plus the slack.
        let executable = stub_curl(&directory, &"x".repeat(8192), false);
        let error = super::download_update_with(
            &executable,
            &assets(1),
            directory.path(),
            &CancellationToken::new(),
            &|_, _| {},
        )
        .expect_err("an oversized download fails");
        assert_eq!(error.kind, UpdateInstallErrorKind::TooLarge);
        assert!(
            temporary_files(directory.path()).is_empty(),
            "a failed download leaves no temporary files"
        );
    }

    #[test]
    fn cancelling_removes_the_temporary_files() {
        let directory = TempDir::new().expect("temporary directory");
        let executable = stub_curl(&directory, "0123456789", true);
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        let watched = directory.path().to_path_buf();
        let watcher = std::thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(5) {
                if !temporary_files(&watched).is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            canceller.cancel();
        });
        let started = Instant::now();
        let error = super::download_update_with(
            &executable,
            &assets(11),
            directory.path(),
            &cancel,
            &|_, _| {},
        )
        .expect_err("a cancelled download fails");
        watcher.join().expect("the watcher finishes");
        assert_eq!(error.kind, UpdateInstallErrorKind::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the download returns promptly"
        );
        assert!(
            temporary_files(directory.path()).is_empty(),
            "a cancelled download leaves no temporary files"
        );
    }

    fn installed_fixture(directory: &TempDir) -> (PathBuf, DownloadedUpdate) {
        let current = directory.path().join("Kestrel.AppImage");
        fs::write(&current, b"old version").expect("the current file writes");
        fs::set_permissions(&current, fs::Permissions::from_mode(0o755)).expect("mode is set");
        let download = DownloadedUpdate::staged(
            directory.path().join("download.AppImage"),
            directory.path().join("download.AppImage.minisig"),
        );
        fs::write(&download.appimage, b"new version").expect("the download writes");
        fs::write(&download.signature, b"signature").expect("the signature writes");
        (current, download)
    }

    #[test]
    fn installing_replaces_the_file_and_keeps_the_previous_one() {
        let directory = TempDir::new().expect("temporary directory");
        let (current, download) = installed_fixture(&directory);
        let staged_appimage = download.appimage.clone();
        let staged_signature = download.signature.clone();
        let installed = install_update(&current, download).expect("the install succeeds");
        assert_eq!(fs::read(&current).expect("the file reads"), b"new version");
        let mode = fs::metadata(&current)
            .expect("the file is readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(
            installed.previous,
            directory.path().join("Kestrel.AppImage.previous")
        );
        assert_eq!(
            fs::read(&installed.previous).expect("the previous file reads"),
            b"old version"
        );
        assert!(previous_version_available(&current));
        assert!(
            !staged_appimage.exists(),
            "the consumed staged AppImage stays installed"
        );
        assert!(!staged_signature.exists(), "the signature was removed");

        roll_back(&current).expect("the rollback succeeds");
        assert_eq!(fs::read(&current).expect("the file reads"), b"old version");
        assert!(!previous_version_available(&current));
    }

    #[test]
    fn a_failed_install_removes_the_staged_files() {
        let directory = TempDir::new().expect("temporary directory");
        let (_, download) = installed_fixture(&directory);
        let staged_appimage = download.appimage.clone();
        let staged_signature = download.signature.clone();
        let missing = directory.path().join("missing").join("Kestrel.AppImage");
        let error = install_update(&missing, download).expect_err("the install fails");
        assert_eq!(error.kind, UpdateInstallErrorKind::Io);
        assert!(
            !staged_appimage.exists(),
            "a failed install removes the staged AppImage"
        );
        assert!(
            !staged_signature.exists(),
            "a failed install removes the staged signature"
        );
    }

    #[test]
    fn a_second_install_replaces_the_previous_copy() {
        let directory = TempDir::new().expect("temporary directory");
        let (current, download) = installed_fixture(&directory);
        install_update(&current, download).expect("the install succeeds");

        let next = DownloadedUpdate::staged(
            directory.path().join("next.AppImage"),
            directory.path().join("next.AppImage.minisig"),
        );
        fs::write(&next.appimage, b"newest version").expect("the download writes");
        fs::write(&next.signature, b"signature").expect("the signature writes");
        let installed = install_update(&current, next).expect("the second install succeeds");
        assert_eq!(
            fs::read(&installed.previous).expect("the previous file reads"),
            b"new version",
            "the older .previous is replaced"
        );
        assert_eq!(
            fs::read(&current).expect("the file reads"),
            b"newest version"
        );
    }

    #[test]
    fn rolling_back_without_a_previous_copy_fails_cleanly() {
        let directory = TempDir::new().expect("temporary directory");
        let current = directory.path().join("Kestrel.AppImage");
        fs::write(&current, b"only version").expect("the file writes");
        assert!(!previous_version_available(&current));
        let error = roll_back(&current).expect_err("there is nothing to restore");
        assert_eq!(error.kind, UpdateInstallErrorKind::Io);
    }
}
