//! Bounded local scripts and root-scoped file search.
//! Scripts use resolved executables, no shell, timeouts, and bounded output.

use std::{
    env,
    error::Error,
    ffi::OsStr,
    fmt,
    io::Read,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use kestrel_core::CommandScriptConfiguration;

use crate::applications::resolve_executable;

const POLL_INTERVAL: Duration = Duration::from_millis(2);
/// Grace period for output readers after script exit.
///
/// Descendants may retain pipes; this bound prevents indefinite stalls.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptErrorKind {
    /// Configured executable resolution failed.
    MissingExecutable,
    SpawnFailed,
    TimedOut,
    Failed,
    OutputTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    pub kind: ScriptErrorKind,
    pub message: String,
}

impl ScriptError {
    fn new(kind: ScriptErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ScriptError {}

/// Bounded result of one script run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptOutcome {
    pub name: String,
    pub executable: PathBuf,
    pub exit_code: Option<i32>,
    pub stdout: String,
    /// Whether output reached its byte bound.
    pub stdout_truncated: bool,
    pub stderr: String,
    pub stderr_truncated: bool,
    pub duration: Duration,
}

/// Runs one configured script action.
pub fn run_script(
    script: &CommandScriptConfiguration,
    path_env: Option<&OsStr>,
) -> Result<ScriptOutcome, ScriptError> {
    let executable = resolve_executable(&script.executable, path_env).ok_or_else(|| {
        ScriptError::new(
            ScriptErrorKind::MissingExecutable,
            format!("{} was not found on PATH", script.executable),
        )
    })?;
    let started = Instant::now();
    let mut command = Command::new(&executable);
    command
        .args(&script.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A process group lets timeout terminate all descendants.
        .process_group(0);
    let mut child = crate::spawn_with_busy_retry(&mut command).map_err(|error| {
        ScriptError::new(
            ScriptErrorKind::SpawnFailed,
            format!("{} could not be started: {error}", script.name),
        )
    })?;
    let group = child.id() as libc::pid_t;

    // Dedicated readers cap output and avoid blocking the exit-status wait.
    let limit = script.output_bytes as usize;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = stdout.map(|pipe| spawn_bounded_reader(pipe, limit));
    let stderr_reader = stderr.map(|pipe| spawn_bounded_reader(pipe, limit));

    let timeout = Duration::from_millis(script.timeout_millis);
    let deadline = started + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child, group);
                return Err(ScriptError::new(
                    ScriptErrorKind::Failed,
                    format!("{} could not be waited for: {error}", script.name),
                ));
            }
        }
        if Instant::now() >= deadline {
            terminate(&mut child, group);
            return Err(ScriptError::new(
                ScriptErrorKind::TimedOut,
                format!(
                    "{} did not finish within {} ms",
                    script.name, script.timeout_millis
                ),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };

    // Descendants may retain pipes; drain briefly, then stop the group.
    let drain_deadline = Instant::now() + OUTPUT_DRAIN_GRACE;
    let stdout = collect_output(stdout_reader, drain_deadline);
    let stderr = collect_output(stderr_reader, drain_deadline);
    let (Some((stdout, stdout_truncated)), Some((stderr, stderr_truncated))) = (stdout, stderr)
    else {
        kill_group(group);
        return Err(ScriptError::new(
            ScriptErrorKind::TimedOut,
            format!(
                "{} left a background process holding its output open and was stopped",
                script.name
            ),
        ));
    };

    Ok(ScriptOutcome {
        name: script.name.clone(),
        executable,
        exit_code: status.code(),
        stdout,
        stdout_truncated,
        stderr,
        stderr_truncated,
        duration: started.elapsed(),
    })
}

type BoundedReader = Receiver<(String, bool)>;

/// Ends the whole process group and reaps its leader.
fn terminate(child: &mut Child, group: libc::pid_t) {
    kill_group(group);
    let _ = child.kill();
    let _ = child.wait();
}

/// Sends `SIGKILL` to every member of a process group.
fn kill_group(group: libc::pid_t) {
    // SAFETY: The negative ID targets this run's process group; `ESRCH` is harmless.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

fn spawn_bounded_reader<R: Read + Send + 'static>(pipe: R, limit: usize) -> BoundedReader {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("kestrel-script-output".to_string())
        .spawn(move || {
            let _ = sender.send(read_bounded(pipe, limit));
        })
        .expect("spawning an output reader cannot fail at this size");
    receiver
}

fn read_bounded<R: Read>(mut pipe: R, limit: usize) -> (String, bool) {
    let mut collected = Vec::with_capacity(limit.min(4096));
    let mut chunk = [0u8; 4096];
    let mut truncated = false;
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                if collected.len() + count > limit {
                    let remaining = limit.saturating_sub(collected.len());
                    collected.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    // Drain remaining bytes so the child can finish despite a full pipe.
                    let mut discard = [0u8; 4096];
                    while pipe
                        .read(&mut discard)
                        .map(|read| read > 0)
                        .unwrap_or(false)
                    {}
                    break;
                }
                collected.extend_from_slice(&chunk[..count]);
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&collected).into_owned(), truncated)
}

/// Waits for one reader until `deadline`; `None` means the pipe stayed open.
fn collect_output(reader: Option<BoundedReader>, deadline: Instant) -> Option<(String, bool)> {
    let receiver = reader?;
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(output) => Some(output),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => Some((String::new(), false)),
    }
}

/// Finds bounded, shallow files under configured roots.
///
/// Breadth-first traversal skips hidden and symlinked directories and is deterministic.
pub fn search_roots(
    roots: &[PathBuf],
    query: &str,
    max_depth: u32,
    max_entries: u32,
    max_matches: usize,
) -> Vec<PathBuf> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || max_matches == 0 {
        return Vec::new();
    }
    // Preserve depth so nearer matches sort first.
    let mut matches: Vec<(u32, PathBuf)> = Vec::new();
    let mut visited = 0u32;
    let mut queue: Vec<(PathBuf, u32)> = roots
        .iter()
        .filter(|root| root.is_absolute())
        .map(|root| (root.clone(), 0))
        .collect();

    while let Some((directory, depth)) = queue.pop() {
        if depth > max_depth || visited >= max_entries || matches.len() >= max_matches {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut children: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            visited += 1;
            if visited >= max_entries {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if is_dir && name.starts_with('.') {
                continue;
            }
            if is_dir {
                children.push(path);
            } else if name.contains(&needle) {
                matches.push((depth, path));
                if matches.len() >= max_matches {
                    break;
                }
            }
        }
        children.sort();
        // Sorted children keep traversal deterministic with iterative traversal.
        for child in children.into_iter().rev() {
            queue.push((child, depth + 1));
        }
    }

    // Nearer files first, then lexicographic order.
    matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    matches.truncate(max_matches);
    matches.into_iter().map(|(_, path)| path).collect()
}

/// Returns the process `PATH` used for script runs.
pub fn process_path() -> Option<std::ffi::OsString> {
    env::var_os("PATH")
}

/// Whether a configured root is an existing directory.
pub fn root_is_usable(root: &Path) -> bool {
    root.is_dir() && root.metadata().map(|m| m.is_dir()).unwrap_or(false)
}

/// Whether this user can execute a path.
pub fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    use super::{
        ScriptErrorKind, is_executable, read_bounded, root_is_usable, run_script, search_roots,
    };

    fn script(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, body).expect("script is written");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("mode is set");
        path
    }

    fn configuration(
        name: &str,
        executable: &str,
        args: Vec<String>,
        timeout_millis: u64,
        output_bytes: u32,
    ) -> kestrel_core::CommandScriptConfiguration {
        kestrel_core::CommandScriptConfiguration {
            name: name.to_string(),
            executable: executable.to_string(),
            args,
            timeout_millis,
            output_bytes,
        }
    }

    #[test]
    fn a_script_runs_with_bounded_output_and_reports_its_status() {
        let dir = TempDir::new().expect("temp dir");
        let executable = script(&dir, "echoer", "#!/bin/sh\nprintf 'hello\\n'; exit 0\n");
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());
        let outcome = run_script(
            &configuration(
                "Echo",
                executable.to_str().unwrap(),
                Vec::new(),
                5_000,
                4_096,
            ),
            Some(&path_env),
        )
        .expect("the script runs");

        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.stdout, "hello\n");
        assert!(!outcome.stdout_truncated);
        assert_eq!(outcome.name, "Echo");
    }

    #[test]
    fn script_output_is_capped_and_marked_truncated() {
        let dir = TempDir::new().expect("temp dir");
        let executable = script(
            &dir,
            "firehose",
            "#!/bin/sh\ni=0\nwhile [ $i -lt 200 ]; do printf 'xxxxxxxxxxxxxxxxxxxx'; i=$((i+1)); done\n",
        );
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());
        let outcome = run_script(
            &configuration(
                "Firehose",
                executable.to_str().unwrap(),
                Vec::new(),
                5_000,
                1_024,
            ),
            Some(&path_env),
        )
        .expect("the script runs");

        assert_eq!(outcome.stdout.len(), 1_024);
        assert!(outcome.stdout_truncated, "the cap is reported, not hidden");
    }

    #[test]
    fn scripts_report_missing_executables_failures_and_timeouts() {
        let dir = TempDir::new().expect("temp dir");
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());

        let error = run_script(
            &configuration("Missing", "no-such-tool", Vec::new(), 1_000, 1_024),
            Some(&path_env),
        )
        .expect_err("a missing executable is reported");
        assert_eq!(error.kind, ScriptErrorKind::MissingExecutable);

        let failing = script(&dir, "failer", "#!/bin/sh\nexit 7\n");
        let outcome = run_script(
            &configuration(
                "Failer",
                failing.to_str().unwrap(),
                Vec::new(),
                5_000,
                1_024,
            ),
            Some(&path_env),
        )
        .expect("a failing script still reports");
        assert_eq!(
            outcome.exit_code,
            Some(7),
            "a non-zero exit is data, not a transport failure"
        );

        let slow = script(&dir, "slow", "#!/bin/sh\nsleep 5\n");
        let error = run_script(
            &configuration("Slow", slow.to_str().unwrap(), Vec::new(), 150, 1_024),
            Some(&path_env),
        )
        .expect_err("a slow script times out");
        assert_eq!(error.kind, ScriptErrorKind::TimedOut);
    }

    /// Whether a process has exited; zombies awaiting reaping count as gone.
    fn process_is_gone(pid: i32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
                .is_none_or(|state| state == 'Z'),
        }
    }

    #[test]
    fn a_timeout_ends_the_script_and_every_descendant() {
        let dir = TempDir::new().expect("temp dir");
        let pid_file = dir.path().join("descendant.pid");
        let executable = script(
            &dir,
            "spawner",
            &format!(
                "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
                pid_file.display()
            ),
        );
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());

        let started = Instant::now();
        let error = run_script(
            &configuration(
                "Spawner",
                executable.to_str().unwrap(),
                Vec::new(),
                400,
                1_024,
            ),
            Some(&path_env),
        )
        .expect_err("the script times out");

        assert_eq!(error.kind, ScriptErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the timeout does not wait for the descendant"
        );
        let pid: i32 = fs::read_to_string(&pid_file)
            .expect("the script recorded its descendant")
            .trim()
            .parse()
            .expect("the recorded id is a number");
        let mut gone = false;
        for _ in 0..100 {
            if process_is_gone(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone, "the descendant must not outlive the timed-out script");
    }

    #[test]
    fn a_descendant_holding_the_output_open_cannot_stall_the_caller() {
        let dir = TempDir::new().expect("temp dir");
        let executable = script(&dir, "leaver", "#!/bin/sh\nsleep 30 &\nexit 0\n");
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());

        let started = Instant::now();
        let error = run_script(
            &configuration(
                "Leaver",
                executable.to_str().unwrap(),
                Vec::new(),
                20_000,
                1_024,
            ),
            Some(&path_env),
        )
        .expect_err("a script that leaves its output open is stopped");

        assert_eq!(error.kind, ScriptErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "output collection is bounded, not tied to the descendant: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn arguments_are_passed_verbatim_without_a_shell() {
        let dir = TempDir::new().expect("temp dir");
        let record = dir.path().join("args.txt");
        let executable = script(
            &dir,
            "recorder",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" \"$2\" > {}\n",
                record.display()
            ),
        );
        let path_env = std::ffi::OsString::from(dir.path().as_os_str());
        run_script(
            &configuration(
                "Recorder",
                executable.to_str().unwrap(),
                vec!["a; rm -rf /".to_string(), "$(whoami)".to_string()],
                5_000,
                4_096,
            ),
            Some(&path_env),
        )
        .expect("the script runs");

        assert_eq!(
            fs::read_to_string(&record).expect("arguments are recorded"),
            "2\na; rm -rf /\n$(whoami)\n",
            "shell metacharacters stay literal arguments"
        );
    }

    #[test]
    fn read_bounded_reports_the_cap() {
        let (text, truncated) = read_bounded(&b"abcdefghij"[..], 4);
        assert_eq!(text, "abcd");
        assert!(truncated);

        let (text, truncated) = read_bounded(&b"abc"[..], 8);
        assert_eq!(text, "abc");
        assert!(!truncated);
    }

    #[test]
    fn file_search_is_bounded_to_roots_and_depth() {
        let dir = TempDir::new().expect("temp dir");
        let nested = dir.path().join("nested/deeper");
        fs::create_dir_all(&nested).expect("directories are created");
        fs::write(dir.path().join("report-final.txt"), "a").expect("file");
        fs::write(dir.path().join("other.txt"), "b").expect("file");
        fs::write(nested.join("report-nested.txt"), "c").expect("file");
        fs::create_dir_all(dir.path().join(".hidden")).expect("hidden directory");
        fs::write(dir.path().join(".hidden/report-hidden.txt"), "d").expect("file");

        let roots = vec![dir.path().to_path_buf()];
        let found = search_roots(&roots, "report", 4, 2_000, 10);
        assert_eq!(
            found,
            vec![
                dir.path().join("report-final.txt"),
                nested.join("report-nested.txt")
            ],
            "hidden directories are skipped and nearer files come first"
        );

        let shallow = search_roots(&roots, "report", 0, 2_000, 10);
        assert_eq!(
            shallow,
            vec![dir.path().join("report-final.txt")],
            "depth bounds the walk"
        );

        let limited = search_roots(&roots, "report", 4, 2_000, 1);
        assert_eq!(limited.len(), 1, "the match bound is honoured");

        assert!(search_roots(&roots, "", 4, 2_000, 10).is_empty());
        assert!(
            search_roots(&[std::path::PathBuf::from("relative/path")], "x", 4, 10, 10).is_empty(),
            "relative roots are ignored"
        );
        assert!(search_roots(&roots, "nothing-matches", 4, 2_000, 10).is_empty());
    }

    #[test]
    fn entry_budget_stops_the_walk() {
        let dir = TempDir::new().expect("temp dir");
        for index in 0..40 {
            fs::write(dir.path().join(format!("file-{index:02}.txt")), "x").expect("file");
        }
        let found = search_roots(&[dir.path().to_path_buf()], "file-", 1, 5, 40);
        assert!(
            found.len() <= 5,
            "the entry budget bounds how many files are visited: {}",
            found.len()
        );
    }

    #[test]
    fn roots_and_executables_are_validated() {
        let dir = TempDir::new().expect("temp dir");
        assert!(root_is_usable(dir.path()));
        assert!(!root_is_usable(&dir.path().join("missing")));

        let executable = script(&dir, "tool", "#!/bin/sh\nexit 0\n");
        assert!(is_executable(&executable));
        assert!(!is_executable(&dir.path().join("missing")));
    }
}
