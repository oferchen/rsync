#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_daemon` provides the thin command-line front-end for the Rust `rsyncd`
//! binary. The current implementation focuses on deterministic diagnostics while
//! the actual daemon transport loop is under construction. It recognises a small
//! flag surface (`--help` and `--version`), forwards future command-line
//! arguments into a [`DaemonConfig`], and reports that daemon functionality has
//! not yet shipped.
//!
//! # Design
//!
//! - [`run`] mirrors upstream `rsyncd` by accepting argument iterators together
//!   with writable handles for standard output and error streams.
//! - [`DaemonConfig`] stores the caller-provided daemon arguments. A
//!   [`DaemonConfigBuilder`] exposes an API that higher layers will expand once
//!   full daemon support lands.
//! - [`run_daemon`] centralises the feature-gap diagnostic so both the CLI and
//!   future tests report the same wording and exit code.
//!
//! # Invariants
//!
//! - Diagnostics are routed through [`rsync_core::message`] so trailers and
//!   source locations follow workspace conventions.
//! - `run` never panics. I/O failures propagate as exit code `1` with the
//!   original error rendered verbatim.
//! - [`DaemonError::exit_code`] always matches the exit code embedded within the
//!   associated [`Message`].
//!
//! # Errors
//!
//! Parsing failures surface as exit code `1` and emit the `clap`-generated
//! diagnostic. Transfer attempts report that daemon functionality is currently
//! unavailable, also using exit code `1`.
//!
//! # Examples
//!
//! Render the `--version` banner into an in-memory buffer.
//!
//! ```
//! use rsync_daemon::run;
//!
//! let mut stdout = Vec::new();
//! let mut stderr = Vec::new();
//! let status = run(["oc-rsyncd", "--version"], &mut stdout, &mut stderr);
//!
//! assert_eq!(status, 0);
//! assert!(stderr.is_empty());
//! assert!(!stdout.is_empty());
//! ```
//!
//! Attempting to launch the daemon currently reports the missing feature.
//!
//! ```
//! use rsync_daemon::{run_daemon, DaemonConfig};
//!
//! let config = DaemonConfig::builder().build();
//! let error = run_daemon(config).expect_err("daemon functionality is unavailable");
//!
//! assert_eq!(error.exit_code(), 1);
//! assert!(error.message().to_string().contains("daemon functionality is unavailable"));
//! ```
//!
//! # See also
//!
//! - [`rsync_core::version`] for the shared `--version` banner helpers.
//! - [`rsync_core::client`] for the analogous client-facing orchestration.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};

/// Exit code used when daemon functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Configuration describing the requested daemon operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DaemonConfig {
    arguments: Vec<OsString>,
}

impl DaemonConfig {
    /// Creates a new [`DaemonConfigBuilder`].
    #[must_use]
    pub fn builder() -> DaemonConfigBuilder {
        DaemonConfigBuilder::default()
    }

    /// Returns the raw arguments supplied to the daemon.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Reports whether any daemon-specific arguments were provided.
    #[must_use]
    pub fn has_runtime_request(&self) -> bool {
        !self.arguments.is_empty()
    }
}

/// Builder used to assemble a [`DaemonConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DaemonConfigBuilder {
    arguments: Vec<OsString>,
}

impl DaemonConfigBuilder {
    /// Supplies the arguments that should be forwarded to the daemon loop once implemented.
    #[must_use]
    pub fn arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    /// Finalises the builder and constructs the [`DaemonConfig`].
    #[must_use]
    pub fn build(self) -> DaemonConfig {
        DaemonConfig {
            arguments: self.arguments,
        }
    }
}

/// Error returned when daemon orchestration fails.
#[derive(Clone, Debug)]
pub struct DaemonError {
    exit_code: i32,
    message: Message,
}

impl DaemonError {
    /// Creates a new [`DaemonError`] from the supplied message and exit code.
    fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    #[must_use]
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for DaemonError {}

/// Runs the daemon orchestration using the provided configuration.
///
/// At present the daemon transport loop has not been implemented, so the helper
/// reports a structured diagnostic that mirrors the CLI's existing behaviour.
pub fn run_daemon(config: DaemonConfig) -> Result<(), DaemonError> {
    let text = if config.has_runtime_request() {
        "daemon functionality is unavailable in this build: the server mode has not been implemented yet"
    } else {
        "daemon functionality is unavailable in this build: only --help and --version are currently supported"
    };
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);

    Err(DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message))
}

/// Parsed command produced by [`parse_args`].
#[derive(Debug, Default)]
struct ParsedArgs {
    show_help: bool,
    show_version: bool,
    remainder: Vec<OsString>,
}

fn clap_command() -> Command {
    Command::new("oc-rsyncd")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false)
        .arg(
            Arg::new("help")
                .long("help")
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}

fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();

    if args.is_empty() {
        args.push(OsString::from("oc-rsyncd"));
    }

    let mut matches = clap_command().try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();

    Ok(ParsedArgs {
        show_help,
        show_version,
        remainder,
    })
}

fn render_help() -> String {
    clap_command().render_help().to_string()
}

fn write_message<W: Write>(message: &Message, writer: &mut W) -> io::Result<()> {
    message.render_line_to_writer(writer)
}

/// Runs the daemon CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// Diagnostics are rendered using the central [`rsync_core::message`] utilities.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    match parse_args(arguments) {
        Ok(parsed) => execute(parsed, stdout, stderr),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(stderr, "{}", error);
            }
            1
        }
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut Err) -> i32
where
    Out: Write,
    Err: Write,
{
    if parsed.show_help {
        let help = render_help();
        if stdout.write_all(help.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    if parsed.show_version && parsed.remainder.is_empty() {
        let report = VersionInfoReport::default();
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let config = DaemonConfig::builder().arguments(parsed.remainder).build();

    match run_daemon(config) {
        Ok(()) => 0,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(
                    stderr,
                    "rsync error: daemon functionality is unavailable in this build (code 1)",
                );
            }
            error.exit_code()
        }
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(args, &mut stdout, &mut stderr);
        (code, stdout, stderr)
    }

    #[test]
    fn builder_collects_arguments() {
        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--config"),
                OsString::from("/tmp/rsyncd.conf"),
            ])
            .build();

        assert_eq!(
            config.arguments(),
            &[
                OsString::from("--config"),
                OsString::from("/tmp/rsyncd.conf")
            ]
        );
        assert!(config.has_runtime_request());
    }

    #[test]
    fn run_daemon_reports_feature_gap() {
        let config = DaemonConfig::builder().build();
        let error = run_daemon(config).expect_err("daemon support is missing");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("daemon functionality is unavailable"));
        assert!(rendered.contains("[daemon=3.4.1-rust]"));
    }

    #[test]
    fn version_flag_renders_report() {
        let (code, stdout, stderr) =
            run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--version")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default().human_readable();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn help_flag_renders_output() {
        let (code, stdout, stderr) = run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn transfer_request_reports_missing_daemon() {
        let (code, stdout, stderr) =
            run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--no-detach")]);

        assert_eq!(code, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains("rsync error: daemon functionality is unavailable"));
        assert!(rendered.contains("[daemon=3.4.1-rust]"));
    }

    #[test]
    fn clap_parse_error_is_reported_via_message() {
        let command = clap_command();
        let error = command
            .try_get_matches_from(vec!["oc-rsyncd", "--version=extra"])
            .unwrap_err();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = run(
            [
                OsString::from("oc-rsyncd"),
                OsString::from("--version=extra"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(status, 1);
        assert!(stdout.is_empty());

        let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
        assert!(rendered.contains(error.to_string().trim()));
    }
}
