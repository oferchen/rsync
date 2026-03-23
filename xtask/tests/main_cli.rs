use std::io;
use std::process::{Command, Output, Stdio};
use std::str;
use std::time::{Duration, Instant};

/// Spawn a process and wait for completion with a timeout.
///
/// Kills the process if it exceeds the timeout, preventing CI hangs.
fn spawn_with_timeout(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command.spawn()?;
    let start = Instant::now();

    loop {
        match child.try_wait()? {
            Some(_status) => return child.wait_with_output(),
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("process exceeded timeout of {timeout:?} and was killed"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
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
    // clap shows help text when no subcommand is provided
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
    // clap reports unrecognized subcommands
    assert!(stderr.contains("unrecognized subcommand"));
}
