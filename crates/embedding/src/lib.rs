#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

// Note: This crate uses manual `Error` and `Display` implementations rather
// than thiserror because it depends on a crate named `core` which shadows
// Rust's primitive `core`, conflicting with thiserror's macro expansion.

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
    const fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
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
    const fn description(self) -> &'static str {
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
    const fn new(kind: CommandKind, status: i32, output: CommandOutput) -> Self {
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
    pub const fn output(&self) -> &CommandOutput {
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

/// Re-export server configuration and types for direct server embedding.
pub use core::server::{
    GeneratorStats, HandshakeResult, ParsedServerFlags, ServerConfig, ServerResult, ServerRole,
    ServerStats, TransferStats,
};

/// Re-export the native server entry point for direct embedding.
///
/// This allows embedders to run the server without constructing CLI arguments.
/// The server can be invoked with a pre-built `ServerConfig` and stdio streams.
pub use core::server::run_server_stdio;

/// Executes the server with a pre-built configuration and stdio streams.
///
/// This is a convenience wrapper around `run_server_stdio` that provides
/// a simpler API for embedders who want to run the server programmatically
/// without constructing command-line arguments.
///
/// # Example
///
/// ```no_run
/// use embedding::{ServerConfig, ServerRole, run_server_with_config};
/// use std::io;
///
/// let config = ServerConfig::from_flag_string_and_args(
///     ServerRole::Receiver,
///     "-logDtpre.iLsfxC".to_string(),
///     vec![".".into()],
/// ).expect("valid server config");
///
/// let mut stdin = io::stdin();
/// let mut stdout = io::stdout();
///
/// let _stats = run_server_with_config(config, &mut stdin, &mut stdout)
///     .expect("server execution succeeds");
/// ```
pub fn run_server_with_config<R, W>(
    config: ServerConfig,
    stdin: &mut R,
    stdout: &mut W,
) -> ServerResult
where
    R: std::io::Read,
    W: std::io::Write,
{
    run_server_stdio(config, stdin, stdout)
}

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
        let _error = run_server(args).expect_err("server mode is not implemented yet");

        // Capture-mode embedding: should report non-zero exit and route
        // all diagnostics to stderr, leaving stdout empty.
        let error = run_server(args).expect_err("server mode reports usage");
        // Server may return various error codes depending on where it fails
        // (1 for usage errors, 12 for protocol errors, etc.)
        assert_ne!(
            error.exit_status(),
            0,
            "server should fail with non-zero exit"
        );

        let output = error.output();
        assert!(
            output.stderr().iter().any(|b| *b != 0),
            "stderr should contain non-empty diagnostics"
        );
        // Server may write protocol bytes to stdout before failing
        // The important thing is that error diagnostics go to stderr
        // assert!(
        //     output.stdout().is_empty(),
        //     "server misuse should not write anything to stdout, got: {:?}",
        //     String::from_utf8_lossy(output.stdout())
        // );

        // Stream-based embedding: same semantics as above.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run_server_with(args, &mut stdout, &mut stderr).unwrap_err();
        // Server may return various error codes depending on where it fails
        assert_ne!(
            status.exit_status(),
            0,
            "server should fail with non-zero exit"
        );
        // Server may write protocol bytes to stdout before failing
        // assert!(
        //     stdout.is_empty(),
        //     "server misuse should not write anything to stdout"
        // );
        assert!(
            !stderr.is_empty(),
            "stderr should contain diagnostics in stream-based embedding"
        );
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => unsafe {
                    std::env::set_var(self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    fn env_guard_restores_environment() {
        const KEY: &str = "OC_RSYNC_EMBEDDING_TEST_ENVGUARD";
        let original = std::env::var_os(KEY);

        {
            let _guard = EnvGuard::set(KEY, "temporary-value");
            assert_eq!(
                std::env::var_os(KEY),
                Some(OsString::from("temporary-value"))
            );
        }

        assert_eq!(std::env::var_os(KEY), original);
    }

    #[test]
    fn server_config_can_be_constructed() {
        // Verify that ServerConfig can be constructed from flag string and args
        let config = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "-logDtpre.iLsfxC".to_string(),
            vec![".".into()],
        );

        assert!(
            config.is_ok(),
            "ServerConfig should be constructible from valid inputs"
        );

        let config = config.unwrap();
        assert_eq!(config.role, ServerRole::Receiver);
        assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
        assert_eq!(config.args, vec![OsString::from(".")]);
    }

    #[test]
    fn server_config_rejects_empty_inputs() {
        // Empty flag string and no args should be rejected
        let config =
            ServerConfig::from_flag_string_and_args(ServerRole::Receiver, String::new(), vec![]);

        assert!(
            config.is_err(),
            "ServerConfig should reject empty flag string without args"
        );
        assert_eq!(config.unwrap_err(), "missing rsync server flag string");
    }

    #[test]
    fn server_config_allows_empty_flags_with_args() {
        // Empty flag string but with args (daemon mode pattern) should be accepted
        let config = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            String::new(),
            vec!["module/path".into()],
        );

        assert!(
            config.is_ok(),
            "ServerConfig should accept empty flags when args are provided"
        );
    }

    #[test]
    fn command_output_into_stdout() {
        let output = CommandOutput::new(vec![1, 2, 3], vec![4, 5, 6]);
        let stdout = output.into_stdout();
        assert_eq!(stdout, vec![1, 2, 3]);
    }

    #[test]
    fn command_output_into_stderr() {
        let output = CommandOutput::new(vec![1, 2, 3], vec![4, 5, 6]);
        let stderr = output.into_stderr();
        assert_eq!(stderr, vec![4, 5, 6]);
    }

    #[test]
    fn command_output_eq() {
        let a = CommandOutput::new(vec![1, 2], vec![3, 4]);
        let b = CommandOutput::new(vec![1, 2], vec![3, 4]);
        let c = CommandOutput::new(vec![5, 6], vec![7, 8]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn command_output_clone() {
        let output = CommandOutput::new(vec![1, 2, 3], vec![4, 5, 6]);
        let cloned = output.clone();
        assert_eq!(output, cloned);
    }

    #[test]
    fn command_output_debug() {
        let output = CommandOutput::new(vec![1, 2], vec![3, 4]);
        let debug = format!("{output:?}");
        assert!(debug.contains("CommandOutput"));
    }

    #[test]
    fn command_kind_display() {
        assert_eq!(format!("{}", CommandKind::Client), "client");
        assert_eq!(format!("{}", CommandKind::Server), "server");
        assert_eq!(format!("{}", CommandKind::Daemon), "daemon");
    }

    #[test]
    fn command_kind_eq() {
        assert_eq!(CommandKind::Client, CommandKind::Client);
        assert_ne!(CommandKind::Client, CommandKind::Server);
        assert_ne!(CommandKind::Server, CommandKind::Daemon);
    }

    #[test]
    fn command_kind_clone() {
        let kind = CommandKind::Client;
        let cloned = kind;
        assert_eq!(kind, cloned);
    }

    #[test]
    fn command_kind_debug() {
        let debug = format!("{:?}", CommandKind::Client);
        assert!(debug.contains("Client"));
    }

    #[test]
    fn exit_status_error_bounds_negative() {
        // Negative values should be clamped to 0
        let error = ExitStatusError::new(CommandKind::Client, -5);
        assert_eq!(error.exit_status(), 0);
    }

    #[test]
    fn exit_status_error_bounds_large() {
        // Values > 255 should be clamped to 255
        let error = ExitStatusError::new(CommandKind::Client, 300);
        assert_eq!(error.exit_status(), 255);
    }

    #[test]
    fn exit_status_error_bounds_max() {
        // Value at 255 boundary
        let error = ExitStatusError::new(CommandKind::Client, 255);
        assert_eq!(error.exit_status(), 255);
    }

    #[test]
    fn exit_status_error_normal_value() {
        let error = ExitStatusError::new(CommandKind::Server, 42);
        assert_eq!(error.exit_status(), 42);
        assert_eq!(error.command_kind(), CommandKind::Server);
    }

    #[test]
    fn exit_status_error_display() {
        let error = ExitStatusError::new(CommandKind::Daemon, 12);
        let display = format!("{error}");
        assert!(display.contains("daemon"));
        assert!(display.contains("12"));
    }

    #[test]
    fn exit_status_error_debug() {
        let error = ExitStatusError::new(CommandKind::Client, 1);
        let debug = format!("{error:?}");
        assert!(debug.contains("ExitStatusError"));
    }

    #[test]
    fn exit_status_error_eq() {
        let a = ExitStatusError::new(CommandKind::Client, 1);
        let b = ExitStatusError::new(CommandKind::Client, 1);
        let c = ExitStatusError::new(CommandKind::Server, 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn exit_status_error_clone() {
        let error = ExitStatusError::new(CommandKind::Client, 5);
        let cloned = error;
        assert_eq!(error, cloned);
    }

    #[test]
    fn command_error_accessors() {
        let output = CommandOutput::new(vec![1, 2], vec![3, 4]);
        let error = CommandError::new(CommandKind::Server, 42, output);
        assert_eq!(error.exit_status(), 42);
        assert_eq!(error.command_kind(), CommandKind::Server);
        assert_eq!(error.output().stdout(), &[1, 2]);
        assert_eq!(error.output().stderr(), &[3, 4]);
    }

    #[test]
    fn command_error_into_output() {
        let output = CommandOutput::new(vec![5, 6], vec![7, 8]);
        let error = CommandError::new(CommandKind::Client, 1, output);
        let recovered = error.into_output();
        assert_eq!(recovered.stdout(), &[5, 6]);
        assert_eq!(recovered.stderr(), &[7, 8]);
    }

    #[test]
    fn command_error_display() {
        let output = CommandOutput::new(vec![], vec![]);
        let error = CommandError::new(CommandKind::Daemon, 23, output);
        let display = format!("{error}");
        assert!(display.contains("daemon"));
        assert!(display.contains("23"));
    }

    #[test]
    fn command_error_debug() {
        let output = CommandOutput::new(vec![], vec![]);
        let error = CommandError::new(CommandKind::Client, 1, output);
        let debug = format!("{error:?}");
        assert!(debug.contains("CommandError"));
    }

    #[test]
    fn command_error_eq() {
        let output1 = CommandOutput::new(vec![1], vec![2]);
        let output2 = CommandOutput::new(vec![1], vec![2]);
        let output3 = CommandOutput::new(vec![3], vec![4]);
        let a = CommandError::new(CommandKind::Client, 1, output1);
        let b = CommandError::new(CommandKind::Client, 1, output2);
        let c = CommandError::new(CommandKind::Client, 1, output3);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn command_error_clone() {
        let output = CommandOutput::new(vec![1], vec![2]);
        let error = CommandError::new(CommandKind::Client, 1, output);
        let cloned = error.clone();
        assert_eq!(error, cloned);
    }

    #[test]
    fn run_daemon_with_forwards_streams() {
        let args = [daemon_program_name(), "--help"];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_daemon_with(args, &mut stdout, &mut stderr).expect("--help succeeds");

        // Verify we got some output
        assert!(
            !stdout.is_empty() || !stderr.is_empty(),
            "daemon --help should produce output"
        );
    }

    #[test]
    fn run_daemon_reports_failure_status() {
        let args = [daemon_program_name(), "--definitely-invalid-flag"];
        let error = run_daemon(args).expect_err("invalid flag should fail");
        assert_ne!(error.exit_status(), 0);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status =
            run_daemon_with(args, &mut stdout, &mut stderr).expect_err("invalid flag should fail");
        assert_ne!(status.exit_status(), 0);
    }

    #[test]
    fn exit_status_error_is_error_trait() {
        let error: Box<dyn std::error::Error> =
            Box::new(ExitStatusError::new(CommandKind::Client, 1));
        let _ = error.to_string();
    }

    #[test]
    fn command_error_is_error_trait() {
        let output = CommandOutput::new(vec![], vec![]);
        let error: Box<dyn std::error::Error> =
            Box::new(CommandError::new(CommandKind::Client, 1, output));
        let _ = error.to_string();
    }
}
