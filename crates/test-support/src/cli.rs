//! Process runner for the built `oc-rsync` binary.
//!
//! [`OcRsyncCliRunner`] locates the workspace `oc-rsync` executable, spawns it
//! with a caller-supplied argv / env / cwd / stdin, enforces a wall-time
//! timeout, and returns a structured [`CliOutput`]. It backs the operational
//! ports described in `docs/design/uts-nextest-edge-b-test-harness.md`
//! section 4.
//!
//! Failure is loud, not silent: a missing binary or a spawn error surfaces as a
//! typed [`RunnerError`] rather than a skipped assertion. Tests that want to
//! self-skip when the binary is absent should gate on
//! [`crate::require_binary`] first.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::skip::locate_workspace_binary;

/// Default wall-time cap for a single invocation.
///
/// Section 4.4 of the harness design: 30 s is the hard kill-switch so a
/// regression that would otherwise hang (e.g. a goodbye-flush stall) fails
/// loud inside nextest's per-test budget instead of stalling the cell.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval used while waiting for the child to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Error raised when a runner cannot locate, spawn, or drive the binary.
///
/// Distinct from the child's own non-zero exit: a non-zero exit is a
/// successful *run* that produced a [`CliOutput`], whereas these variants mean
/// the run never completed cleanly.
#[derive(Debug)]
pub enum RunnerError {
    /// The `oc-rsync` binary could not be found in `target/{debug,release}/`
    /// and `CARGO_BIN_EXE_oc-rsync` was unset or pointed at a missing file.
    BinaryNotFound,
    /// The OS failed to spawn the child process.
    Spawn(std::io::Error),
    /// Collecting the child's output failed.
    Wait(std::io::Error),
    /// The child exceeded the configured wall-time timeout and was killed.
    Timeout {
        /// The timeout that was exceeded.
        after: Duration,
        /// stderr captured up to the kill point, lossily decoded.
        stderr: String,
    },
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::BinaryNotFound => write!(
                f,
                "oc-rsync binary not found (set CARGO_BIN_EXE_oc-rsync or build the workspace)"
            ),
            RunnerError::Spawn(e) => write!(f, "failed to spawn oc-rsync: {e}"),
            RunnerError::Wait(e) => write!(f, "failed to collect child output: {e}"),
            RunnerError::Timeout { after, stderr } => write!(
                f,
                "oc-rsync exceeded {after:?} timeout and was killed; stderr so far:\n{stderr}"
            ),
        }
    }
}

impl std::error::Error for RunnerError {}

/// Locate the `oc-rsync` binary.
///
/// Prefers Cargo's `CARGO_BIN_EXE_oc-rsync` (set for integration tests of a
/// crate that depends on the `oc-rsync` bin target) and falls back to
/// [`locate_workspace_binary`] which walks the enclosing profile directory.
#[must_use]
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    locate_workspace_binary("oc-rsync")
}

/// Builder + executor for a single `oc-rsync` invocation.
///
/// Construct with [`OcRsyncCliRunner::new`] (auto-locates the binary), chain
/// the builder setters, then call [`run`](OcRsyncCliRunner::run). Every setter
/// takes `self` by value and returns it, matching the repo's builder
/// convention.
pub struct OcRsyncCliRunner {
    binary: Option<PathBuf>,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    env_clear: bool,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    cwd: Option<PathBuf>,
}

impl Default for OcRsyncCliRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl OcRsyncCliRunner {
    /// Create a runner, auto-locating the `oc-rsync` binary.
    ///
    /// Binary resolution is deferred to [`run`](OcRsyncCliRunner::run): if the
    /// binary is missing, `run` returns [`RunnerError::BinaryNotFound`] rather
    /// than panicking in the constructor. This lets a test build the runner and
    /// still choose to self-skip.
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: locate_oc_rsync(),
            args: Vec::new(),
            env: BTreeMap::new(),
            env_clear: false,
            stdin: None,
            timeout: DEFAULT_TIMEOUT,
            cwd: None,
        }
    }

    /// Override the binary path (e.g. to test a specific build profile).
    #[must_use]
    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary = Some(path.into());
        self
    }

    /// Append a single argument.
    #[must_use]
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_os_string());
        self
    }

    /// Append several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for a in args {
            self.args.push(a.as_ref().to_os_string());
        }
        self
    }

    /// Set an environment variable for the child.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Self {
        self.env
            .insert(key.as_ref().to_os_string(), val.as_ref().to_os_string());
        self
    }

    /// Clear the inherited environment before applying [`env`](Self::env)
    /// overrides. Useful for tests that must not leak `RSYNC_*` from the host.
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    /// Provide bytes to feed on the child's stdin.
    #[must_use]
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    /// Override the wall-time timeout (default 30 s).
    #[must_use]
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Set the working directory for the child.
    #[must_use]
    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
        self.cwd = Some(p.into());
        self
    }

    /// Run synchronously, enforcing the timeout.
    ///
    /// Returns a [`CliOutput`] on any completed run (including non-zero exit).
    /// Returns a [`RunnerError`] if the binary is missing, the spawn fails, or
    /// the child overruns the timeout (in which case it is killed and reaped).
    pub fn run(self) -> Result<CliOutput, RunnerError> {
        let binary = self.binary.ok_or(RunnerError::BinaryNotFound)?;

        let mut cmd = Command::new(&binary);
        cmd.args(&self.args);
        if self.env_clear {
            cmd.env_clear();
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let start = Instant::now();
        let mut child = cmd.spawn().map_err(RunnerError::Spawn)?;

        if let Some(data) = &self.stdin {
            // Write on a scratch thread so a child that never drains stdin
            // cannot deadlock us; the timeout below still reaps such a child.
            let mut sink = child.stdin.take().expect("stdin was piped");
            let data = data.clone();
            // Detached: the thread finishes when the child consumes the input
            // or when the pipe closes on child exit. Dropping the JoinHandle
            // does not detach the OS thread, which is intended here.
            let _ = thread::spawn(move || {
                let _ = sink.write_all(&data);
                // Dropping `sink` closes the pipe, signalling EOF.
            });
        }

        // Poll for completion under the timeout. Draining the pipes happens
        // after exit via `wait_with_output`; the payloads in operational tests
        // are small (design section 8), so pipe buffers do not fill before the
        // child exits.
        loop {
            match child.try_wait().map_err(RunnerError::Wait)? {
                Some(_) => break,
                None => {
                    if start.elapsed() >= self.timeout {
                        let _ = child.kill();
                        let output = child.wait_with_output().map_err(RunnerError::Wait)?;
                        return Err(RunnerError::Timeout {
                            after: self.timeout,
                            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                        });
                    }
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }

        let output = child.wait_with_output().map_err(RunnerError::Wait)?;
        let duration = start.elapsed();

        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            output.status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;

        Ok(CliOutput {
            status: output.status.code(),
            signal,
            stdout: output.stdout,
            stderr: output.stderr,
            duration,
        })
    }
}

/// Captured result of a completed `oc-rsync` run.
pub struct CliOutput {
    /// The process exit code, or `None` when the child died from a signal.
    pub status: Option<i32>,
    /// The terminating signal number on Unix, or `None` for a normal exit.
    pub signal: Option<i32>,
    /// Raw stdout bytes.
    pub stdout: Vec<u8>,
    /// Raw stderr bytes.
    pub stderr: Vec<u8>,
    /// Wall-clock duration of the run.
    pub duration: Duration,
}

impl CliOutput {
    /// Assert the run exited 0. Panics with captured stderr on mismatch.
    pub fn assert_success(&self) -> &Self {
        assert_eq!(
            self.status,
            Some(0),
            "expected exit 0, got status={:?} signal={:?}\nstderr:\n{}",
            self.status,
            self.signal,
            self.stderr_str()
        );
        self
    }

    /// Assert the run exited with exactly `code`.
    pub fn assert_exit(&self, code: i32) -> &Self {
        assert_eq!(
            self.status,
            Some(code),
            "expected exit {code}, got status={:?} signal={:?}\nstderr:\n{}",
            self.status,
            self.signal,
            self.stderr_str()
        );
        self
    }

    /// Assert the exit code is one of `codes`.
    ///
    /// Used by tests that accept a partial-transfer code (23) alongside 0.
    pub fn assert_exit_in(&self, codes: &[i32]) -> &Self {
        assert!(
            self.status.is_some_and(|c| codes.contains(&c)),
            "expected exit in {codes:?}, got status={:?} signal={:?}\nstderr:\n{}",
            self.status,
            self.signal,
            self.stderr_str()
        );
        self
    }

    /// Assert the child was not killed by a signal.
    ///
    /// Backs the crafted-input ports (design section 4.3) that must reject
    /// malformed input cleanly, never crash with SIGSEGV/SIGABRT.
    pub fn assert_no_signal_death(&self) -> &Self {
        assert!(
            self.signal.is_none(),
            "child died from signal {:?}\nstderr:\n{}",
            self.signal,
            self.stderr_str()
        );
        self
    }

    /// stdout as a lossily-decoded UTF-8 string.
    #[must_use]
    pub fn stdout_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// stderr as a lossily-decoded UTF-8 string.
    #[must_use]
    pub fn stderr_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }

    /// Whether stderr contains `needle` (lossy match).
    #[must_use]
    pub fn stderr_contains(&self, needle: &str) -> bool {
        self.stderr_str().contains(needle)
    }

    /// Whether stdout contains `needle` (lossy match).
    #[must_use]
    pub fn stdout_contains(&self, needle: &str) -> bool {
        self.stdout_str().contains(needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_not_found_variant_when_unlocatable() {
        // Why: Rule 12 - when no binary can be located at all, run must yield
        // the dedicated BinaryNotFound variant so callers can distinguish
        // "not built" from "spawn failed", never a silent success.
        let mut runner = OcRsyncCliRunner::new();
        runner.binary = None;
        assert!(matches!(runner.run(), Err(RunnerError::BinaryNotFound)));
    }

    #[test]
    fn bad_binary_path_is_a_loud_spawn_error() {
        // Why: an override pointing at a real-but-absent path must fail loudly
        // as a Spawn error, never spawn garbage or return Ok.
        let err = OcRsyncCliRunner::new()
            .binary("/nonexistent/oc-rsync-xyzzy")
            .arg("--version")
            .run();
        assert!(matches!(err, Err(RunnerError::Spawn(_))));
    }

    #[test]
    fn runs_a_real_command_and_captures_output() {
        // Why: proves the spawn/capture path end-to-end against a binary that
        // is guaranteed present on every host. Uses the `binary` override so
        // the test does not depend on oc-rsync being built. Asserts exit 0 and
        // no signal death.
        let (prog, args): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/C", "exit", "0"])
        } else {
            ("true", &[])
        };
        let Some(path) = crate::skip::locate_command_on_path(prog) else {
            eprintln!("skipping: {prog} not on PATH");
            return;
        };
        let out = OcRsyncCliRunner::new()
            .binary(path)
            .args(args)
            .run()
            .expect("run should succeed");
        out.assert_success().assert_no_signal_death();
        assert_eq!(out.status, Some(0));
    }

    #[test]
    fn nonzero_exit_is_ok_not_error() {
        // Why: a non-zero exit is a completed run, not a RunnerError. Tests
        // assert on exit codes, so run must return Ok(CliOutput) carrying the
        // code, reserving Err for spawn/timeout faults.
        let (prog, args): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/C", "exit", "3"])
        } else {
            ("sh", &["-c", "exit 3"])
        };
        let Some(path) = crate::skip::locate_command_on_path(prog) else {
            eprintln!("skipping: {prog} not on PATH");
            return;
        };
        let out = OcRsyncCliRunner::new()
            .binary(path)
            .args(args)
            .run()
            .expect("run itself should succeed");
        out.assert_exit(3);
        out.assert_exit_in(&[0, 3]);
    }

    #[test]
    fn timeout_kills_a_hanging_child() {
        // Why: the timeout is the only hang kill-switch (design 4.4). A child
        // that sleeps longer than the timeout must be killed and reported as
        // RunnerError::Timeout, never allowed to stall the test.
        if cfg!(windows) {
            // `sleep` semantics differ; the Unix path proves the mechanism.
            return;
        }
        let Some(path) = crate::skip::locate_command_on_path("sleep") else {
            eprintln!("skipping: sleep not on PATH");
            return;
        };
        let start = Instant::now();
        let err = OcRsyncCliRunner::new()
            .binary(path)
            .arg("30")
            .timeout(Duration::from_millis(200))
            .run();
        assert!(matches!(err, Err(RunnerError::Timeout { .. })));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout must fire promptly, not wait for the child"
        );
    }

    #[test]
    fn stdin_is_delivered_to_the_child() {
        // Why: the stdin plumbing must actually reach the child; a broken pipe
        // path would silently drop input and mask protocol-input regressions.
        // `cat` echoes stdin to stdout, so we round-trip a payload.
        if cfg!(windows) {
            return;
        }
        let Some(path) = crate::skip::locate_command_on_path("cat") else {
            eprintln!("skipping: cat not on PATH");
            return;
        };
        let out = OcRsyncCliRunner::new()
            .binary(path)
            .stdin(b"hello harness\n".to_vec())
            .run()
            .expect("run should succeed");
        out.assert_success();
        assert_eq!(out.stdout, b"hello harness\n");
    }

    #[test]
    fn env_clear_then_set_isolates_the_child() {
        // Why: env_clear must genuinely wipe the inherited environment so a
        // test isolating RSYNC_* behaviour is not polluted by the host, while
        // an explicit env() set after clearing still reaches the child.
        if cfg!(windows) {
            return;
        }
        let Some(path) = crate::skip::locate_command_on_path("sh") else {
            eprintln!("skipping: sh not on PATH");
            return;
        };
        let out = OcRsyncCliRunner::new()
            .binary(path)
            .env_clear()
            .env("HARNESS_MARKER", "present")
            .args(["-c", "printf '%s' \"$HARNESS_MARKER\""])
            .run()
            .expect("run should succeed");
        out.assert_success();
        assert_eq!(out.stdout, b"present");
    }

    #[test]
    fn cwd_sets_the_child_working_directory() {
        // Why: cwd must actually change the child's directory; ports that pass
        // relative source/dest paths rely on it. `pwd` reports the effective
        // directory, which must match the temp dir we set.
        if cfg!(windows) {
            return;
        }
        let Some(path) = crate::skip::locate_command_on_path("pwd") else {
            eprintln!("skipping: pwd not on PATH");
            return;
        };
        let dir = crate::create_tempdir();
        // Canonicalize to absorb symlinked temp roots (e.g. /var -> /private
        // on macOS) so the comparison is against the real path pwd prints.
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
        let out = OcRsyncCliRunner::new()
            .binary(path)
            .cwd(&canon)
            .run()
            .expect("run should succeed");
        out.assert_success();
        assert_eq!(out.stdout_str().trim_end(), canon.to_string_lossy());
    }
}
