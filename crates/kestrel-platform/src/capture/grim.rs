//! `grim` (with `slurp` for Area) on compositors exposing screencopy.
//! Children run from absolute paths, without a shell, in their own process group.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::{
    CaptureBackend, CaptureCancel, CaptureError, CaptureErrorKind, CaptureMode, CaptureProvider,
    CapturedImage, MAX_CAPTURE_DIMENSION, MAX_CAPTURE_PNG_BYTES, cancelled, too_large,
    validate_png,
};

const SLURP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const GRIM_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_GEOMETRY_BYTES: usize = 256;
/// Largest absolute layout coordinate accepted from slurp.
const MAX_COORDINATE: i64 = 1 << 20;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct GrimCaptureBackend {
    grim: Option<PathBuf>,
    slurp: Option<PathBuf>,
    grim_timeout: Duration,
    slurp_timeout: Duration,
}

impl GrimCaptureBackend {
    /// Relative paths are ignored, so capture never searches `PATH`.
    pub fn new(grim: PathBuf, slurp: Option<PathBuf>) -> Self {
        Self::from_detection(Some(grim), slurp)
    }

    pub(super) fn from_detection(grim: Option<PathBuf>, slurp: Option<PathBuf>) -> Self {
        Self {
            grim: grim.filter(|path| path.is_absolute()),
            slurp: slurp.filter(|path| path.is_absolute()),
            grim_timeout: GRIM_TIMEOUT,
            slurp_timeout: SLURP_TIMEOUT,
        }
    }

    fn grim(&self) -> Result<&Path, CaptureError> {
        self.grim.as_deref().ok_or_else(|| {
            CaptureError::new(CaptureErrorKind::Unavailable, "grim is not installed.")
        })
    }

    fn select_area(&self, cancel: &CaptureCancel) -> Result<String, CaptureError> {
        let slurp = self.slurp.as_deref().ok_or_else(|| {
            CaptureError::new(CaptureErrorKind::Unavailable, "Area capture needs slurp.")
        })?;
        let output = run_bounded(slurp, &[], self.slurp_timeout, MAX_GEOMETRY_BYTES, cancel)?;
        if !output.status.success() {
            return Err(if output.stdout.is_empty() {
                cancelled()
            } else {
                CaptureError::new(CaptureErrorKind::Failed, "slurp failed.")
            });
        }
        parse_geometry(&output.stdout)
    }
}

impl CaptureBackend for GrimCaptureBackend {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Grim
    }

    fn modes(&self) -> Vec<CaptureMode> {
        match (&self.grim, &self.slurp) {
            (None, _) => Vec::new(),
            (Some(_), Some(_)) => vec![CaptureMode::Area, CaptureMode::Screen],
            (Some(_), None) => vec![CaptureMode::Screen],
        }
    }

    fn capture(
        &self,
        mode: CaptureMode,
        cancel: &CaptureCancel,
    ) -> Result<CapturedImage, CaptureError> {
        let geometry = match mode {
            CaptureMode::Screen => None,
            CaptureMode::Area => {
                self.grim()?;
                Some(self.select_area(cancel)?)
            }
            CaptureMode::Window | CaptureMode::Interactive => {
                return Err(CaptureError::new(
                    CaptureErrorKind::Unavailable,
                    "grim captures the screen or a selected area only.",
                ));
            }
        };
        let grim = self.grim()?;
        let mut args = vec!["-t", "png"];
        if let Some(geometry) = &geometry {
            args.extend(["-g", geometry.as_str()]);
        }
        args.push("-");
        let output = run_bounded(
            grim,
            &args,
            self.grim_timeout,
            MAX_CAPTURE_PNG_BYTES,
            cancel,
        )?;
        if !output.status.success() {
            return Err(CaptureError::new(CaptureErrorKind::Failed, "grim failed."));
        }
        validate_png(output.stdout)
    }
}

/// Parses `X,Y WxH` and re-renders it, so only validated numbers reach grim.
fn parse_geometry(bytes: &[u8]) -> Result<String, CaptureError> {
    let invalid = || CaptureError::new(CaptureErrorKind::Failed, "slurp returned no valid area.");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?.trim();
    let (position, size) = text.split_once(' ').ok_or_else(invalid)?;
    let (x, y) = position.split_once(',').ok_or_else(invalid)?;
    let (width, height) = size.split_once('x').ok_or_else(invalid)?;
    let coordinate = |value: &str| {
        value
            .parse::<i64>()
            .ok()
            .filter(|value| value.abs() <= MAX_COORDINATE)
            .ok_or_else(invalid)
    };
    let extent = |value: &str| {
        value
            .parse::<u32>()
            .ok()
            .filter(|value| (1..=MAX_CAPTURE_DIMENSION).contains(value))
            .ok_or_else(invalid)
    };
    let (x, y, width, height) = (
        coordinate(x)?,
        coordinate(y)?,
        extent(width)?,
        extent(height)?,
    );
    Ok(format!("{x},{y} {width}x{height}"))
}

struct ChildOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_bounded(
    executable: &Path,
    args: &[&str],
    timeout: Duration,
    limit: usize,
    cancel: &CaptureCancel,
) -> Result<ChildOutput, CaptureError> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let name = executable
        .file_name()
        .map_or_else(|| "capture tool".into(), |name| name.to_string_lossy());
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = crate::spawn_with_busy_retry(&mut command).map_err(|_| {
        CaptureError::new(
            CaptureErrorKind::Unavailable,
            format!("{name} could not be started."),
        )
    })?;
    let group = child.id() as libc::pid_t;
    let stdout = child.stdout.take().expect("child stdout is piped");
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("kestrel-capture-output".to_owned())
        .spawn(move || {
            let mut output = Vec::new();
            let result = stdout.take(limit as u64 + 1).read_to_end(&mut output);
            let _ = sender.send(result.map(|_| output));
        });
    if reader.is_err() {
        kill_and_reap(&mut child, group);
        return Err(CaptureError::new(
            CaptureErrorKind::Failed,
            "Starting the output reader failed.",
        ));
    }
    cancel.set_abort(Box::new(move || kill_group(group)));
    let result = wait_bounded(&mut child, group, &receiver, timeout, limit, cancel, &name);
    cancel.clear_abort();
    result
}

fn wait_bounded(
    child: &mut Child,
    group: libc::pid_t,
    receiver: &mpsc::Receiver<std::io::Result<Vec<u8>>>,
    timeout: Duration,
    limit: usize,
    cancel: &CaptureCancel,
    name: &str,
) -> Result<ChildOutput, CaptureError> {
    let deadline = Instant::now() + timeout;
    let mut stdout = None;
    let mut status = None;
    loop {
        if cancel.is_cancelled() {
            kill_and_reap(child, group);
            return Err(cancelled());
        }
        if stdout.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() > limit => {
                    kill_and_reap(child, group);
                    return Err(too_large());
                }
                Ok(Ok(bytes)) => stdout = Some(bytes),
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    kill_and_reap(child, group);
                    return Err(CaptureError::new(
                        CaptureErrorKind::Failed,
                        format!("Reading {name} output failed."),
                    ));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(exited) => status = exited,
                Err(_) => {
                    kill_and_reap(child, group);
                    return Err(CaptureError::new(
                        CaptureErrorKind::Failed,
                        format!("Waiting for {name} failed."),
                    ));
                }
            }
        }
        if let (Some(status), Some(_)) = (status, &stdout) {
            let stdout = stdout.take().unwrap_or_default();
            return Ok(ChildOutput { status, stdout });
        }
        if Instant::now() >= deadline {
            // A reaped child whose pipe is still open has live group members, so the group ID is not reused.
            kill_and_reap(child, group);
            return Err(CaptureError::new(
                CaptureErrorKind::Timeout,
                format!("{name} did not finish in time."),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn kill_group(group: libc::pid_t) {
    // SAFETY: the negative ID targets this capture's own process group; `ESRCH` is harmless.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

fn kill_and_reap(child: &mut Child, group: libc::pid_t) {
    kill_group(group);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::solid_png;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake executable");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make executable");
        path
    }

    /// A fake grim that logs its arguments and prints a 5x4 PNG.
    fn fake_grim(dir: &TempDir) -> PathBuf {
        let image = dir.path().join("image.png");
        fs::write(&image, solid_png(5, 4, [1, 2, 3, 255])).expect("write image");
        let log = dir.path().join("grim-args");
        script(
            dir,
            "grim",
            &format!(
                "printf '%s|' \"$@\" > '{}'\ncat '{}'",
                log.display(),
                image.display()
            ),
        )
    }

    fn grim_args(dir: &TempDir) -> String {
        fs::read_to_string(dir.path().join("grim-args")).unwrap_or_default()
    }

    #[test]
    fn screen_capture_runs_grim_to_stdout() {
        let dir = TempDir::new().expect("temp dir");
        let backend = GrimCaptureBackend::new(fake_grim(&dir), None);
        assert_eq!(backend.modes(), vec![CaptureMode::Screen]);
        let image = backend
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .expect("grim capture");
        assert_eq!((image.width, image.height), (5, 4));
        assert_eq!(grim_args(&dir), "-t|png|-|");
    }

    #[test]
    fn area_capture_passes_the_validated_slurp_geometry() {
        let dir = TempDir::new().expect("temp dir");
        let slurp = script(&dir, "slurp", "printf '10,-20 30x40\\n'");
        let backend = GrimCaptureBackend::new(fake_grim(&dir), Some(slurp));
        assert_eq!(
            backend.modes(),
            vec![CaptureMode::Area, CaptureMode::Screen]
        );
        let image = backend
            .capture(CaptureMode::Area, &CaptureCancel::new())
            .expect("area capture");
        assert_eq!((image.width, image.height), (5, 4));
        assert_eq!(grim_args(&dir), "-t|png|-g|10,-20 30x40|-|");
    }

    #[test]
    fn dismissed_selection_is_cancelled_and_grim_never_runs() {
        let dir = TempDir::new().expect("temp dir");
        let slurp = script(&dir, "slurp", "exit 1");
        let backend = GrimCaptureBackend::new(fake_grim(&dir), Some(slurp));
        let error = backend
            .capture(CaptureMode::Area, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Cancelled);
        assert_eq!(grim_args(&dir), "");
    }

    #[test]
    fn malformed_geometry_is_rejected() {
        for geometry in [
            "".as_bytes(),
            b"10,20",
            b"10,20 0x40",
            b"10,20 30x99999",
            b"a,b 1x1",
            b"10,20 30x40; rm -rf /",
            b"99999999,0 1x1",
        ] {
            assert!(parse_geometry(geometry).is_err(), "{geometry:?}");
        }
        assert_eq!(parse_geometry(b" 0,0 1x1\n").unwrap(), "0,0 1x1");
    }

    #[test]
    fn cancelling_kills_the_selector_process_group() {
        let dir = TempDir::new().expect("temp dir");
        let slurp = script(&dir, "slurp", "sleep 30 &\nsleep 30");
        let backend = GrimCaptureBackend::new(fake_grim(&dir), Some(slurp));
        let cancel = CaptureCancel::new();
        let remote = cancel.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            remote.cancel();
        });
        let started = Instant::now();
        let error = backend.capture(CaptureMode::Area, &cancel).unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
        canceller.join().expect("canceller joins");
    }

    #[test]
    fn a_stalled_grim_times_out() {
        let dir = TempDir::new().expect("temp dir");
        let grim = script(&dir, "grim", "sleep 30");
        let mut backend = GrimCaptureBackend::new(grim, None);
        backend.grim_timeout = Duration::from_millis(200);
        let started = Instant::now();
        let error = backend
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn oversized_or_invalid_output_is_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let huge = script(
            &dir,
            "grim",
            &format!("head -c {} /dev/zero", MAX_CAPTURE_PNG_BYTES + 1024),
        );
        let error = GrimCaptureBackend::new(huge, None)
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::TooLarge);

        let other = TempDir::new().expect("temp dir");
        let text = script(&other, "grim", "printf 'not a png'");
        let error = GrimCaptureBackend::new(text, None)
            .capture(CaptureMode::Screen, &CaptureCancel::new())
            .unwrap_err();
        assert_eq!(error.kind, CaptureErrorKind::Failed);
    }

    #[test]
    fn unsupported_modes_and_relative_paths_are_unavailable() {
        let backend = GrimCaptureBackend::new(PathBuf::from("grim"), None);
        assert!(backend.modes().is_empty());
        for mode in [
            CaptureMode::Screen,
            CaptureMode::Window,
            CaptureMode::Interactive,
        ] {
            let error = backend.capture(mode, &CaptureCancel::new()).unwrap_err();
            assert_eq!(error.kind, CaptureErrorKind::Unavailable);
        }
    }
}
