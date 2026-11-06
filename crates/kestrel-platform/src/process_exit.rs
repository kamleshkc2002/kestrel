//! Waiting for another process to exit, used by the restart hand-off.

use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::{Duration, Instant},
};

/// How often the `/proc` fallback looks for the process.
const FALLBACK_POLL: Duration = Duration::from_millis(50);

/// Waits until `pid` exits or `timeout` passes; `true` means it exited.
/// A process the kernel cannot open a pidfd for is watched through `/proc`,
/// which only sees processes in this PID namespace.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    match open_pidfd(pid) {
        Some(fd) => poll_pidfd(&fd, timeout),
        None => {
            // No pidfd: either the kernel predates it or the process is gone.
            if process_has_exited(pid) {
                return true;
            }
            wait_through_proc(pid, timeout)
        }
    }
}

/// Opens a handle that becomes readable when `pid` exits; `None` when the
/// kernel lacks pidfds or the process is already gone.
fn open_pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: `pidfd_open` takes two plain integers and no pointers.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if descriptor < 0 {
        return None;
    }
    // SAFETY: a non-negative result is a descriptor this process owns.
    Some(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
}

fn poll_pidfd(fd: &OwnedFd, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let millis = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the descriptor array and its length describe one live entry.
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready > 0 {
            return descriptor.revents & libc::POLLIN != 0;
        }
        if ready == 0 {
            return false;
        }
        let interrupted = std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if !interrupted || Instant::now() >= deadline {
            return false;
        }
    }
}

fn wait_through_proc(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if process_has_exited(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(FALLBACK_POLL);
    }
}

/// A missing process, or one that exited and waits to be reaped, counts as
/// exited.
fn process_has_exited(pid: u32) -> bool {
    match std::fs::read_to_string(Path::new(&format!("/proc/{pid}/stat"))) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.trim_start().chars().next())
            .is_some_and(|state| state == 'Z'),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    use super::{wait_for_exit, wait_through_proc};

    #[test]
    fn the_wait_returns_after_the_process_exits() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 0.3")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the child starts");
        let started = Instant::now();
        assert!(
            wait_for_exit(child.id(), Duration::from_secs(10)),
            "the child is reported as exited"
        );
        let elapsed = started.elapsed();
        let _ = child.wait();
        assert!(
            elapsed >= Duration::from_millis(200),
            "the wait does not return before the child exits: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the wait returns promptly: {elapsed:?}"
        );
    }

    #[test]
    fn a_missing_process_is_already_exited() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the child starts");
        let pid = child.id();
        let _ = child.wait();
        assert!(wait_for_exit(pid, Duration::from_secs(5)));
    }

    #[test]
    fn the_proc_fallback_also_waits() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 0.3")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the child starts");
        let started = Instant::now();
        assert!(wait_through_proc(child.id(), Duration::from_secs(10)));
        let elapsed = started.elapsed();
        let _ = child.wait();
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(5),
            "the fallback waits for the child and returns promptly: {elapsed:?}"
        );
    }
}
