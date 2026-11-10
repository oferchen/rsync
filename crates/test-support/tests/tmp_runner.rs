//! Contract tests for the `tmp-runner` Cargo target runner.
//!
//! Two properties matter. The runner must hand back the wrapped process's
//! exit status unchanged, crash codes included, or nextest would misreport a
//! crashing test. And every process it wraps must see a temp directory no
//! other process shares, which is what removes the Windows delete-pending
//! name collision behind task 509.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use test_support::tmp_runner::TMP_RUNNER_DIR_ENV;

const RUNNER: &str = env!("CARGO_BIN_EXE_tmp-runner");

/// Runs `script` through the platform shell under the runner.
fn run_under_runner(script: &str) -> Output {
    let mut command = Command::new(RUNNER);
    if cfg!(windows) {
        command.args(["cmd", "/C", script]);
    } else {
        command.args(["sh", "-c", script]);
    }
    command.output().expect("spawn tmp-runner")
}

/// The private directory the wrapped shell printed as its first line.
fn printed_dir(output: &Output) -> PathBuf {
    let stdout = String::from_utf8_lossy(&output.stdout);
    PathBuf::from(stdout.lines().next().expect("dir line").trim())
}

#[cfg(unix)]
const PRINT_TMP: &str = r#"printf '%s\n' "$TMPDIR""#;
#[cfg(windows)]
const PRINT_TMP: &str = "echo %TEMP%";

#[test]
fn forwards_a_plain_exit_code() {
    let output = run_under_runner(&format!("{PRINT_TMP} && exit 3"));
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn forwards_success() {
    let output = run_under_runner(PRINT_TMP);
    assert!(output.status.success(), "{output:?}");
}

// WHY: nextest classifies a Windows test whose exit code is an NTSTATUS such
// as STATUS_ACCESS_VIOLATION as a crash; a runner that truncated or remapped
// it would turn crashes into ordinary failures.
#[cfg(windows)]
#[test]
fn forwards_a_crash_ntstatus_bit_for_bit() {
    const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
    let output = run_under_runner(&format!("exit {}", STATUS_ACCESS_VIOLATION as i32));
    assert_eq!(output.status.code(), Some(STATUS_ACCESS_VIOLATION as i32));
}

#[cfg(unix)]
#[test]
fn reports_a_signal_death_as_128_plus_signal() {
    let output = run_under_runner("kill -SEGV $$");
    assert_eq!(output.status.code(), Some(128 + 11));
}

#[test]
fn wrapped_process_gets_a_fresh_private_dir_that_is_removed_after() {
    let output = run_under_runner(PRINT_TMP);
    let dir = printed_dir(&output);
    let name = dir.file_name().expect("dir name").to_string_lossy();
    assert!(name.starts_with("oct-"), "unexpected private dir {dir:?}");
    assert!(!dir.exists(), "runner left {dir:?} behind");
}

// WHY: cleanup is best effort; a directory that cannot be removed must never
// change the exit status the test process produced.
#[cfg(unix)]
#[test]
fn an_unremovable_dir_does_not_change_the_exit_code() {
    if platform::privilege::is_effective_root() {
        // Root removes the read-only entry anyway, so there is no failure to observe.
        return;
    }
    let output = run_under_runner(&format!(
        r#"{PRINT_TMP}; mkdir "$TMPDIR/locked" && touch "$TMPDIR/locked/f" && chmod 500 "$TMPDIR/locked"; exit 7"#
    ));
    let dir = printed_dir(&output);
    assert_eq!(output.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tmp-runner: leaving"), "{stderr}");
    restore_and_remove(&dir);
}

#[cfg(unix)]
fn restore_and_remove(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let locked = dir.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("unlock");
    fs::remove_dir_all(dir).expect("remove leftover dir");
}

/// One isolation probe, run as its own nextest process.
///
/// Each probe creates a fixed-name file in its temp dir and leaves it there.
/// Two processes sharing a temp dir therefore collide on the second probe
/// regardless of timing; under the runner every probe gets its own dir. CI
/// sets `OC_REQUIRE_TMP_RUNNER=1` on the Windows cell so a missing runner
/// fails here instead of skipping.
fn isolation_probe() {
    let required = env::var_os("OC_REQUIRE_TMP_RUNNER").is_some_and(|value| value == "1");
    let Some(runner_dir) = env::var_os(TMP_RUNNER_DIR_ENV) else {
        assert!(
            !required,
            "OC_REQUIRE_TMP_RUNNER=1 but tmp-runner is not wrapping this test"
        );
        return;
    };
    let temp = env::temp_dir();
    assert_eq!(
        temp,
        PathBuf::from(runner_dir),
        "std::env::temp_dir ignores the runner"
    );
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp.join("oc-tmp-runner-probe"))
        .expect("probe already present: another test process shares this temp dir");
}

#[test]
fn isolation_probe_a() {
    isolation_probe();
}

#[test]
fn isolation_probe_b() {
    isolation_probe();
}

#[test]
fn isolation_probe_c() {
    isolation_probe();
}

#[test]
fn isolation_probe_d() {
    isolation_probe();
}
