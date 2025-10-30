use std::process::Command;
use std::str;

fn run_xtask(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run xtask: {}", error))
}

#[test]
fn xtask_without_arguments_reports_usage() {
    let output = run_xtask(&[]);
    assert!(
        !output.status.success(),
        "missing command should be reported as a usage failure"
    );

    let stderr = str::from_utf8(&output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("missing command"));
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
    assert!(stderr.contains("unrecognised command"));
    assert!(stderr.contains("Usage:"));
}
