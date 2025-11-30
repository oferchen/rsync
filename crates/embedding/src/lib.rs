#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::Write;

/// Captured output produced by an embedded entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutput {
    fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self { stdout, stderr }
    }

    /// Returns the captured standard output.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns the captured standard error.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Consumes the output and returns the captured standard output buffer.
    #[must_use]
    pub fn into_stdout(self) -> Vec<u8> {
        self.stdout
    }

    /// Consumes the output and returns the captured standard error buffer.
    #[must_use]
    pub fn into_stderr(self) -> Vec<u8> {
        self.stderr
    }
}

/// Identifies the entry point executed by the embedding helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// The rsync client entry point.
    Client,
    /// The hidden `--server` entry point.
    Server,
    /// The daemon front-end (`oc-rsync --daemon`).
    Daemon,
}

impl CommandKind {
    fn description(self) -> &'static str {
        match self {
            CommandKind::Client => "client",
            CommandKind::Server => "server",
            CommandKind::Daemon => "daemon",
        }
    }
}

impl fmt::Display for CommandKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// Error returned when an embedded entry point exits with a non-zero status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatusError {
    kind: CommandKind,
    status: i32,
}

impl ExitStatusError {
    const fn new(kind: CommandKind, status: i32) -> Self {
        let lower_bounded = if status < 0 { 0 } else { status };
        let upper_bounded = if lower_bounded > u8::MAX as i32 {
            u8::MAX as i32
        } else {
            lower_bounded
        };
        Self {
            kind,
            status: upper_bounded,
        }
    }

    /// Returns the exit status reported by the entry point.
    #[must_use]
    pub const fn exit_status(self) -> i32 {
        self.status
    }

    /// Returns the entry point that produced this status code.
    #[must_use]
    pub const fn command_kind(self) -> CommandKind {
        self.kind
    }
}

impl fmt::Display for ExitStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{kind} entry point exited with status {status}",
            kind = self.kind,
            status = self.status,
        )
    }
}

impl Error for ExitStatusError {}

/// Error returned by the capturing helpers when execution fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    status: ExitStatusError,
    output: CommandOutput,
}

impl CommandError {
    fn new(kind: CommandKind, status: i32, output: CommandOutput) -> Self {
        Self {
            status: ExitStatusError::new(kind, status),
            output,
        }
    }

    /// Returns the exit status reported by the entry point.
    #[must_use]
    pub const fn exit_status(&self) -> i32 {
        self.status.exit_status()
    }

    /// Returns the entry point that produced this status code.
    #[must_use]
    pub const fn command_kind(&self) -> CommandKind {
        self.status.command_kind()
    }

    /// Returns the captured output buffers associated with the failure.
    #[must_use]
    pub fn output(&self) -> &CommandOutput {
        &self.output
    }

    /// Consumes the error and returns the captured output buffers.
    #[must_use]
    pub fn into_output(self) -> CommandOutput {
        self.output
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{kind} entry point exited with status {status}",
            kind = self.command_kind(),
            status = self.exit_status(),
        )
    }
}

impl Error for CommandError {}

/// Executes the client entry point and captures its output.
pub fn run_client<I, S>(args: I) -> Result<CommandOutput, CommandError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_with_capture(CommandKind::Client, args, cli::run)
}

/// Executes the client entry point using caller-provided writers.
pub fn run_client_with<I, S, Out, Err>(
    args: I,
    stdout: &mut Out,
    stderr: &mut Err,
) -> Result<(), ExitStatusError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    run_with_streams(CommandKind::Client, args, stdout, stderr, cli::run)
}

/// Executes the hidden server entry point (invoked via `--server`).
pub fn run_server<I, S>(args: I) -> Result<CommandOutput, CommandError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_with_capture(CommandKind::Server, args, cli::run)
}

/// Executes the hidden server entry point using caller-provided writers.
pub fn run_server_with<I, S, Out, Err>(
    args: I,
    stdout: &mut Out,
    stderr: &mut Err,
) -> Result<(), ExitStatusError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    run_with_streams(CommandKind::Server, args, stdout, stderr, cli::run)
}

/// Executes the daemon CLI front-end and captures its output.
pub fn run_daemon<I, S>(args: I) -> Result<CommandOutput, CommandError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_with_capture(CommandKind::Daemon, args, daemon::run)
}

/// Executes the daemon CLI front-end using caller-provided writers.
pub fn run_daemon_with<I, S, Out, Err>(
    args: I,
    stdout: &mut Out,
    stderr: &mut Err,
) -> Result<(), ExitStatusError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    run_with_streams(CommandKind::Daemon, args, stdout, stderr, daemon::run)
}

/// Re-export the daemon configuration builder so embedders can construct
/// long-running daemons without assembling a command-line argument list first.
pub use daemon::{DaemonConfig, DaemonConfigBuilder, DaemonError};

/// Re-export the native daemon loop for direct embedding.
pub use daemon::run_daemon as run_daemon_config;

fn run_with_capture<I, S, Runner>(
    kind: CommandKind,
    args: I,
    runner: Runner,
) -> Result<CommandOutput, CommandError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Runner: FnOnce(Vec<OsString>, &mut Vec<u8>, &mut Vec<u8>) -> i32,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = runner(argv, &mut stdout, &mut stderr);
    let output = CommandOutput::new(stdout, stderr);

    if status == 0 {
        Ok(output)
    } else {
        Err(CommandError::new(kind, status, output))
    }
}

fn run_with_streams<I, S, Out, Err, Runner>(
    kind: CommandKind,
    args: I,
    stdout: &mut Out,
    stderr: &mut Err,
    runner: Runner,
) -> Result<(), ExitStatusError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
    Runner: FnOnce(Vec<OsString>, &mut Out, &mut Err) -> i32,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let status = runner(argv, stdout, stderr);

    if status == 0 {
        Ok(())
    } else {
        Err(ExitStatusError::new(kind, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::branding::{client_program_name, daemon_program_name, oc_client_program_name};
    use std::ffi::OsString;

    fn cli_invocation<I, S>(args: I) -> CommandOutput
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = cli::run(argv, &mut stdout, &mut stderr);
        assert_eq!(status, 0, "cli invocation should succeed");
        CommandOutput::new(stdout, stderr)
    }

    #[test]
    fn run_client_matches_cli_output() {
        let args = [client_program_name(), "--version"];
        let direct = cli_invocation(args);
        let embedded = run_client(args).expect("--version succeeds");

        assert_eq!(embedded.stdout(), direct.stdout());
        assert_eq!(embedded.stderr(), direct.stderr());
    }

    #[test]
    fn run_client_with_forwards_streams() {
        let args = [oc_client_program_name(), "--help"];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_client_with(args, &mut stdout, &mut stderr).expect("--help succeeds");

        let direct = cli_invocation(args);
        assert_eq!(stdout, direct.stdout());
        assert_eq!(stderr, direct.stderr());
    }

    #[test]
    fn run_client_reports_failure_status() {
        let args = [client_program_name(), "--definitely-invalid-flag"];
        let error = run_client(args).expect_err("invalid flag should fail");
        assert_eq!(error.exit_status(), 1);
        assert!(
            !error.output().stderr().is_empty(),
            "stderr should contain diagnostics"
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run_client_with(args, &mut stdout, &mut stderr).unwrap_err();
        assert_eq!(status.exit_status(), 1);
        assert!(!stderr.is_empty());
    }

    #[test]
    fn run_daemon_matches_cli_output() {
        let args = [daemon_program_name(), "--help"];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let direct_status = daemon::run(args, &mut stdout, &mut stderr);
        assert_eq!(direct_status, 0);
        let direct = CommandOutput::new(stdout, stderr);

        let embedded = run_daemon(args).expect("--help succeeds");
        assert_eq!(embedded.stdout(), direct.stdout());
        assert_eq!(embedded.stderr(), direct.stderr());
    }

    #[test]
    fn run_server_reports_exit_status() {
        let args = [
            client_program_name(),
            "--server",
            "-logDtpre.iLsfxC",
            ".",
            ".",
        ];
        let error = run_server(args).expect_err("server mode should fail deterministically");
        assert_eq!(error.exit_status(), 4);
        assert!(
            !error.output().stderr().is_empty(),
            "stderr should contain diagnostics"
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run_server_with(args, &mut stdout, &mut stderr).unwrap_err();
        assert_eq!(status.exit_status(), 4);
        assert!(!stderr.is_empty(), "stderr should report diagnostics");
    }
}
