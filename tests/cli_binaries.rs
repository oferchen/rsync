use rsync_core::version::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};
use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

const CLIENT_BINARIES: &[&str] = &[PROGRAM_NAME, OC_PROGRAM_NAME];
const DAEMON_BINARIES: &[&str] = &[DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME];

fn binary_output(name: &str, args: &[&str]) -> Output {
    let mut command = binary_command(name);
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

fn binary_command(name: &str) -> Command {
    let binary = binary_path(name);
    if !binary.is_file() {
        panic!("failed to locate {name}: {}", binary.display());
    }

    if let Some(mut runner) = cargo_target_runner() {
        let runner_binary = runner
            .get(0)
            .cloned()
            .unwrap_or_else(|| panic!("{name} runner command is empty"));
        runner.remove(0);
        let mut command = Command::new(runner_binary);
        command.args(runner);
        command.arg(&binary);
        command
    } else {
        Command::new(binary)
    }
}

fn binary_path(name: &str) -> PathBuf {
    let env_var = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = env::var_os(&env_var) {
        return PathBuf::from(path);
    }

    let mut target_dir = env::current_exe().expect("current_exe should be available");
    target_dir.pop();
    if target_dir.ends_with("deps") {
        target_dir.pop();
    }
    target_dir.push(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    target_dir
}

fn cargo_target_runner() -> Option<Vec<String>> {
    let target = env::var("TARGET").ok()?;
    let runner_env = format!(
        "CARGO_TARGET_{}_RUNNER",
        target.replace('-', "_").to_uppercase()
    );
    let runner = env::var(&runner_env).ok()?;
    if runner.is_empty() {
        return None;
    }

    Some(runner.split(' ').map(str::to_string).collect())
}
