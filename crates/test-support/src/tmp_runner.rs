//! Per-process temporary directory for test binaries (Cargo target runner).
//!
//! Every test process normally shares one temp root (`%TEMP%` on Windows).
//! `tempfile` names are drawn from `fastrand`, whose per-thread seed hashes
//! only the clock and the thread id, so two test processes can draw the same
//! `.tmpXXXXXX` name. On Windows, creating a file over another process's
//! delete-pending file of that name fails with `ERROR_ACCESS_DENIED`, which
//! `tempfile` does not retry (it retries only `AlreadyExists`).
//!
//! The `tmp-runner` binary wraps each test process: it creates a fresh
//! directory, points `TMP`, `TEMP` and `TMPDIR` at it, runs the test binary,
//! forwards its exit status unchanged and removes the directory. Names drawn
//! by different processes then live in different directories and cannot
//! collide. CI selects it through `CARGO_TARGET_<TRIPLE>_RUNNER`, which
//! nextest honours like Cargo does.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

/// Environment variable the runner sets to the private directory it created.
///
/// Its presence tells a test that it runs under the runner.
pub const TMP_RUNNER_DIR_ENV: &str = "OC_TEST_TMP_RUNNER_DIR";

/// Exit code the runner uses when it fails before the test process runs.
pub const RUNNER_SETUP_FAILURE: i32 = 101;

/// Returns the name of the private directory for runner process `pid`.
///
/// The timestamp keeps a reused pid from landing on a directory a previous
/// process could not remove.
#[must_use]
pub fn private_dir_name(pid: u32, nanos: u128) -> String {
    format!("oct-{pid}-{nanos}")
}

/// Maps a child's exit status to the code the runner exits with.
///
/// On Windows every status carries a code, including the NTSTATUS of a crash
/// (`0xC0000005` and friends), and `std::process::exit` hands it back to
/// `ExitProcess` bit for bit, so the caller sees the crash as a crash. On
/// Unix a signal death becomes `128 + signal`, the shell convention.
#[must_use]
pub fn forwarded_exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    RUNNER_SETUP_FAILURE
}

/// Runs `program` with `args` inside a fresh private temp directory and
/// returns the code to exit with.
///
/// Cleanup is best effort: a directory that cannot be removed (an open
/// handle, a read-only entry) is reported on stderr and never changes the
/// returned code.
pub fn run(program: OsString, args: impl IntoIterator<Item = OsString>) -> i32 {
    let dir = match create_private_dir(&env::temp_dir()) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("tmp-runner: cannot create a private temp dir: {error}");
            return RUNNER_SETUP_FAILURE;
        }
    };
    let status = Command::new(&program)
        .args(args)
        .env("TMP", &dir)
        .env("TEMP", &dir)
        .env("TMPDIR", &dir)
        .env(TMP_RUNNER_DIR_ENV, &dir)
        .status();
    if let Err(error) = fs::remove_dir_all(&dir) {
        eprintln!("tmp-runner: leaving {}: {error}", dir.display());
    }
    match status {
        Ok(status) => forwarded_exit_code(status),
        Err(error) => {
            eprintln!(
                "tmp-runner: cannot run {}: {error}",
                Path::new(&program).display()
            );
            RUNNER_SETUP_FAILURE
        }
    }
}

fn create_private_dir(root: &Path) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = root.join(private_dir_name(std::process::id(), nanos));
    fs::create_dir(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_name_carries_pid_and_timestamp() {
        assert_eq!(private_dir_name(4242, 17), "oct-4242-17");
    }

    // WHY: the pid in the name is what keeps two live test processes in
    // different directories; a name without it would reopen the collision.
    #[test]
    fn create_private_dir_is_named_after_this_process() {
        let root = tempfile::tempdir().expect("root");
        let dir = create_private_dir(root.path()).expect("create");
        assert!(dir.is_dir());
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("oct-{}-", std::process::id()))
        );
    }
}
