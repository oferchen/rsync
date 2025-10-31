use assert_cmd::prelude::*;
use std::process::{Command, Output};

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
fn oc_rsync_help_lists_usage() {
    let output = binary_output("oc-rsync", &["--help"]);
    assert!(output.status.success(), "--help should succeed");
    assert!(
        output.stderr.is_empty(),
        "help output should not write to stderr"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("oc-rsync"));
}

#[test]
fn oc_rsync_without_operands_shows_usage() {
    let output = binary_output("oc-rsync", &[]);
    assert!(
        !output.status.success(),
        "running without operands should fail so the caller sees the usage"
    );
    let combined = combined_utf8(&output);
    assert!(combined.contains("Usage:"));
}

#[test]
fn oc_rsyncd_help_lists_usage() {
    let output = binary_output("oc-rsyncd", &["--help"]);
    assert!(output.status.success(), "--help should succeed");
    assert!(
        output.stderr.is_empty(),
        "help output should not write to stderr"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("oc-rsyncd"));
}

#[test]
fn oc_rsyncd_rejects_unknown_flag() {
    let output = binary_output("oc-rsyncd", &["--definitely-not-a-flag"]);
    assert!(
        !output.status.success(),
        "unexpected flags should return a failure exit status"
    );
    let combined = combined_utf8(&output);
    assert!(combined.contains("unknown option"));
}
