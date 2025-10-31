use assert_cmd::prelude::*;
use rsync_core::version::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};
use std::process::{Command, Output};

const CLIENT_BINARIES: &[&str] = &[PROGRAM_NAME, OC_PROGRAM_NAME];
const DAEMON_BINARIES: &[&str] = &[DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME];

fn binary_output(name: &str, args: &[&str]) -> Output {
    #[allow(deprecated)]
    let mut command =
        Command::cargo_bin(name).unwrap_or_else(|error| panic!("failed to locate {name}: {error}"));
    command.args(args);
    command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {name}: {error}"))
}

fn combined_utf8(output: &std::process::Output) -> String {
    let mut data = output.stdout.clone();
    data.extend_from_slice(&output.stderr);
    String::from_utf8(data).expect("binary output should be valid UTF-8")
}

#[test]
fn client_help_lists_usage() {
    for &binary in CLIENT_BINARIES {
        let output = binary_output(binary, &["--help"]);
        assert!(output.status.success(), "{binary} --help should succeed");
        assert!(
            output.stderr.is_empty(),
            "{binary} help output should not write to stderr"
        );
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(stdout.contains("Usage:"));
        assert!(stdout.contains(binary));
    }
}

#[test]
fn client_without_operands_shows_usage() {
    for &binary in CLIENT_BINARIES {
        let output = binary_output(binary, &[]);
        assert!(
            !output.status.success(),
            "running {binary} without operands should fail so the caller sees the usage"
        );
        let combined = combined_utf8(&output);
        assert!(combined.contains("Usage:"));
    }
}

#[test]
fn daemon_help_lists_usage() {
    for &binary in DAEMON_BINARIES {
        let output = binary_output(binary, &["--help"]);
        assert!(output.status.success(), "{binary} --help should succeed");
        assert!(
            output.stderr.is_empty(),
            "{binary} help output should not write to stderr"
        );
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(stdout.contains("Usage:"));
        assert!(stdout.contains(binary));
    }
}

#[test]
fn daemon_rejects_unknown_flag() {
    for &binary in DAEMON_BINARIES {
        let output = binary_output(binary, &["--definitely-not-a-flag"]);
        assert!(
            !output.status.success(),
            "unexpected flags should return a failure exit status for {binary}"
        );
        let combined = combined_utf8(&output);
        assert!(combined.contains("unknown option"));
    }
}
