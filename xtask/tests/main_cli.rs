use std::io;
use std::process::{Command, Output, Stdio};
use std::str;
use std::time::Duration;

use test_support::Deadlined;

/// Spawn a process and wait for completion with a timeout.
///
/// Delegates to the workspace's bounded drain-and-wait primitive, which reads
/// both pipes throughout and bounds the output collection against the same
/// deadline as the process wait. An `xtask` subcommand is among the most
/// verbose children in the repo, so an undrained poll loop would report the
/// harness's own back-pressure as a timeout.
fn spawn_with_timeout(mut command: Command, timeout: Duration) -> io::Result<Output> {
    match test_support::run_deadlined(&mut command, timeout)? {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        Deadlined::Expired { budget, .. } => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("process exceeded timeout of {budget:?} and was killed"),
        )),
    }
}

fn run_xtask(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.args(args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    spawn_with_timeout(command, Duration::from_secs(60))
        .unwrap_or_else(|error| panic!("failed to run xtask: {error}"))
}

#[test]
fn xtask_without_arguments_reports_usage() {
    let output = run_xtask(&[]);
    assert!(
        !output.status.success(),
        "missing command should be reported as a usage failure"
    );

    let stderr = str::from_utf8(&output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("Usage:"));
}

#[test]
fn xtask_help_command_prints_usage_to_stdout() {
    let output = run_xtask(&["help"]);
    assert!(output.status.success(), "help command should succeed");
    assert!(
        output.stderr.is_empty(),
        "help output should not write to stderr"
    );

    let stdout = str::from_utf8(&output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("sbom"));
}

#[test]
fn xtask_unknown_command_reports_error() {
    let output = run_xtask(&["definitely-not-a-command"]);
    assert!(
        !output.status.success(),
        "unknown commands should fail so callers see the diagnostic"
    );

    let stderr = str::from_utf8(&output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("unrecognized subcommand"));
}

/// RED-FIRST probe: `spawn_with_timeout` must collect a child that outwrites
/// the pipe buffer. Undrained, the child blocks in `write` at ~64 KiB and the
/// poll loop reports the harness's own back-pressure as a timeout.
#[cfg(unix)]
#[test]
fn spawn_with_timeout_collects_a_child_that_outwrites_the_pipe_buffer() {
    const FLOOD: usize = 256 * 1024;
    let script = format!(
        "yes 0123456789abcdef | head -c {FLOOD}\n\
         yes 0123456789abcdef | head -c {FLOOD} >&2\n"
    );
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(&script);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = spawn_with_timeout(command, Duration::from_secs(10))
        .expect("flooding child must not be reported as a timeout");
    assert_eq!(output.stdout.len(), FLOOD, "stdout truncated");
    assert_eq!(output.stderr.len(), FLOOD, "stderr truncated");
}
