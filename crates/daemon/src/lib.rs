#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_daemon` provides the thin command-line front-end for the Rust `rsyncd`
//! binary. The crate now exposes a deterministic daemon loop capable of
//! accepting sequential legacy (`@RSYNCD:`) TCP connections, greeting each peer
//! with protocol `32`, serving `#list` requests from an in-memory module table,
//! and replying with explanatory `@ERROR` messages when module transfers are not
//! yet available. The number of connections can be capped via
//! command-line flags, allowing integration tests to exercise the handshake
//! without leaving background threads running indefinitely while keeping the
//! default behaviour ready for long-lived daemons once module serving lands.
//!
//! # Design
//!
//! - [`run`] mirrors upstream `rsyncd` by accepting argument iterators together
//!   with writable handles for standard output and error streams.
//! - [`DaemonConfig`] stores the caller-provided daemon arguments. A
//!   [`DaemonConfigBuilder`] exposes an API that higher layers will expand once
//!   full daemon support lands.
//! - [`run_daemon`] parses command-line arguments, binds a TCP listener, and
//!   serves one or more legacy connections using deterministic handshake
//!   semantics. Requests for `#list` reuse the configured module table, while
//!   module transfers continue to emit availability diagnostics until the full
//!   engine lands.
//! - [`render_help`] returns a deterministic description of the limited daemon
//!   capabilities available today, keeping the help text aligned with actual
//!   behaviour until the parity help renderer is implemented.
//!
//! # Invariants
//!
//! - Diagnostics are routed through [`rsync_core::message`] so trailers and
//!   source locations follow workspace conventions.
//! - `run` never panics. I/O failures propagate as exit code `1` with the
//!   original error rendered verbatim.
//! - [`DaemonError::exit_code`] always matches the exit code embedded within the
//!   associated [`Message`].
//! - `run_daemon` configures read and write timeouts on accepted sockets so
//!   handshake deadlocks are avoided, mirroring upstream rsync's timeout
//!   handling expectations.
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
//! Launching the daemon binds a TCP listener (defaulting to `127.0.0.1:8730`),
//! accepts a legacy connection, and responds with an explanatory error.
//!
//! ```
//! use rsync_daemon::{run_daemon, DaemonConfig};
//! use std::io::{BufRead, BufReader, Write};
//! use std::net::{TcpListener, TcpStream};
//! use std::thread;
//! use std::time::Duration;
//!
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let listener = TcpListener::bind("127.0.0.1:0")?;
//! let port = listener.local_addr()?.port();
//! drop(listener);
//!
//! let config = DaemonConfig::builder()
//!     .arguments(["--port", &port.to_string(), "--once"])
//!     .build();
//!
//! let handle = thread::spawn(move || run_daemon(config));
//!
//! let mut stream = loop {
//!     match TcpStream::connect(("127.0.0.1", port)) {
//!         Ok(stream) => break stream,
//!         Err(error) => {
//!             if error.kind() != std::io::ErrorKind::ConnectionRefused {
//!                 return Err(Box::new(error));
//!             }
//!         }
//!     }
//!     thread::sleep(Duration::from_millis(20));
//! };
//! let mut reader = BufReader::new(stream.try_clone()?);
//! let mut line = String::new();
//! reader.read_line(&mut line)?;
//! assert_eq!(line, "@RSYNCD: 32.0\n");
//! stream.write_all(b"module\n")?;
//! line.clear();
//! reader.read_line(&mut line)?;
//! assert!(line.starts_with("@ERROR:"));
//! line.clear();
//! reader.read_line(&mut line)?;
//! assert_eq!(line, "@RSYNCD: EXIT\n");
//!
//! handle.join().expect("thread").expect("daemon run succeeds");
//! # Ok(())
//! # }
//! # demo().unwrap();
//! ```
//!
//! When one or more modules are supplied via `--module NAME=PATH[,COMMENT]`, a
//! client issuing `#list` receives the configured table before the daemon closes
//! the session with `@RSYNCD: EXIT`.
//!
//! # See also
//!
//! - [`rsync_core::version`] for the shared `--version` banner helpers.
//! - [`rsync_core::client`] for the analogous client-facing orchestration.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_protocol::{LegacyDaemonMessage, ProtocolVersion, format_legacy_daemon_message};

/// Exit code used when daemon functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Default bind address when no CLI overrides are provided.
const DEFAULT_BIND_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Default port used for the development daemon listener.
const DEFAULT_PORT: u16 = 8730;
/// Timeout applied to accepted sockets to avoid hanging handshakes.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Error payload returned to clients while daemon functionality is incomplete.
const HANDSHAKE_ERROR_PAYLOAD: &str = "@ERROR: daemon functionality is unavailable in this build";
/// Error payload returned when a configured module is requested but file serving is unavailable.
const MODULE_UNAVAILABLE_PAYLOAD: &str =
    "@ERROR: module '{module}' transfers are not yet implemented in this build";
/// Error payload returned when a requested module does not exist.
const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";

/// Deterministic help text describing the currently supported daemon surface.
const HELP_TEXT: &str = concat!(
    "oc-rsyncd 3.4.1-rust\n",
    "https://github.com/oferchen/rsync\n",
    "\n",
    "Usage: oc-rsyncd [--help] [--version] [ARGS...]\n",
    "\n",
    "Daemon mode is under active development. This build recognises:\n",
    "  --help        Show this help message and exit.\n",
    "  --version     Output version information and exit.\n",
    "  --bind ADDR         Bind to the supplied IPv4/IPv6 address (default 127.0.0.1).\n",
    "  --port PORT         Listen on the supplied TCP port (default 8730).\n",
    "  --once              Accept a single connection and exit.\n",
    "  --max-sessions N    Accept N connections before exiting (N > 0).\n",
    "  --module SPEC      Register an in-memory module (NAME=PATH[,COMMENT]).\n",
    "\n",
    "The listener accepts legacy @RSYNCD: connections sequentially, reports the\n",
    "negotiated protocol as 32, lists configured modules for #list requests, and\n",
    "replies with an @ERROR diagnostic while full module support is implemented.\n",
);

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleDefinition {
    name: String,
    path: PathBuf,
    comment: Option<String>,
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeOptions {
    bind_address: IpAddr,
    port: u16,
    max_sessions: Option<NonZeroUsize>,
    modules: Vec<ModuleDefinition>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            modules: Vec::new(),
        }
    }
}

impl RuntimeOptions {
    fn parse(arguments: &[OsString]) -> Result<Self, DaemonError> {
        let mut options = Self::default();
        let mut seen_modules = std::collections::HashSet::new();
        let mut iter = arguments.iter();

        while let Some(argument) = iter.next() {
            if argument == "--port" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--port"))?;
                options.port = parse_port(value)?;
            } else if argument == "--bind" || argument == "--address" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value(argument.to_string_lossy().as_ref()))?;
                options.bind_address = parse_bind_address(value)?;
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).unwrap())?;
            } else if argument == "--max-sessions" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--max-sessions"))?;
                let max = parse_max_sessions(value)?;
                options.set_max_sessions(max)?;
            } else if argument == "--module" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--module"))?;
                let module = parse_module_definition(value)?;
                if !seen_modules.insert(module.name.clone()) {
                    return Err(duplicate_module(&module.name));
                }
                options.modules.push(module);
            } else {
                return Err(unsupported_option(argument.clone()));
            }
        }

        Ok(options)
    }

    fn set_max_sessions(&mut self, value: NonZeroUsize) -> Result<(), DaemonError> {
        if self.max_sessions.is_some() {
            return Err(duplicate_argument("--max-sessions"));
        }

        self.max_sessions = Some(value);
        Ok(())
    }

    fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
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
/// The helper binds a TCP listener (defaulting to `127.0.0.1:8730`), accepts a
/// single connection, performs the legacy ASCII handshake, and replies with a
/// deterministic `@ERROR` message explaining that module serving is not yet
/// available. This behaviour gives higher layers a concrete negotiation target
/// while keeping the observable output stable.
pub fn run_daemon(config: DaemonConfig) -> Result<(), DaemonError> {
    let options = RuntimeOptions::parse(config.arguments())?;
    serve_connections(options)
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
    HELP_TEXT.to_string()
}

fn write_message<W: Write>(message: &Message, writer: &mut W) -> io::Result<()> {
    message.render_line_to_writer(writer)
}

fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    let requested_addr = SocketAddr::new(options.bind_address, options.port);
    let listener =
        TcpListener::bind(requested_addr).map_err(|error| bind_error(requested_addr, error))?;
    let local_addr = listener.local_addr().unwrap_or(requested_addr);

    let mut served = 0usize;

    loop {
        match listener.accept() {
            Ok((stream, peer_addr)) => {
                configure_stream(&stream)
                    .map_err(|error| stream_error(Some(peer_addr), "configure socket", error))?;
                handle_legacy_session(stream, options.modules()).map_err(|error| {
                    stream_error(Some(peer_addr), "serve legacy handshake", error)
                })?;
                served = served.saturating_add(1);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(error) => {
                return Err(accept_error(local_addr, error));
            }
        }

        if let Some(limit) = options.max_sessions {
            if served >= limit.get() {
                break;
            }
        }
    }

    Ok(())
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

fn handle_legacy_session(stream: TcpStream, modules: &[ModuleDefinition]) -> io::Result<()> {
    let mut reader = BufReader::new(stream);

    let greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(ProtocolVersion::NEWEST));
    reader.get_mut().write_all(greeting.as_bytes())?;
    reader.get_mut().flush()?;

    let mut request = String::new();
    match reader.read_line(&mut request) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        Err(error) => return Err(error),
    }

    let trimmed = request.trim_end_matches(|ch| matches!(ch, '\r' | '\n'));

    if trimmed == "#list" {
        respond_with_module_list(reader.get_mut(), modules)?;
    } else if trimmed.is_empty() {
        reader
            .get_mut()
            .write_all(HANDSHAKE_ERROR_PAYLOAD.as_bytes())?;
        reader.get_mut().write_all(b"\n")?;
        let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
        reader.get_mut().write_all(exit.as_bytes())?;
        reader.get_mut().flush()?;
    } else {
        respond_with_module_request(reader.get_mut(), modules, trimmed)?;
    }

    Ok(())
}

fn respond_with_module_list(
    stream: &mut TcpStream,
    modules: &[ModuleDefinition],
) -> io::Result<()> {
    let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
    stream.write_all(ok.as_bytes())?;

    for module in modules {
        let mut line = module.name.clone();
        if let Some(comment) = &module.comment {
            if !comment.is_empty() {
                line.push('\t');
                line.push_str(comment);
            }
        }
        line.push('\n');
        stream.write_all(line.as_bytes())?;
    }

    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    stream.write_all(exit.as_bytes())?;
    stream.flush()
}

fn respond_with_module_request(
    stream: &mut TcpStream,
    modules: &[ModuleDefinition],
    request: &str,
) -> io::Result<()> {
    let payload = if modules.iter().any(|module| module.name == request) {
        MODULE_UNAVAILABLE_PAYLOAD.replace("{module}", request)
    } else {
        UNKNOWN_MODULE_PAYLOAD.replace("{module}", request)
    };

    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    stream.write_all(exit.as_bytes())?;
    stream.flush()
}

fn missing_argument_value(option: &str) -> DaemonError {
    config_error(format!("missing value for {option}"))
}

fn parse_port(value: &OsString) -> Result<u16, DaemonError> {
    let text = value.to_string_lossy();
    text.parse::<u16>()
        .map_err(|_| config_error(format!("invalid value for --port: '{text}'")))
}

fn parse_bind_address(value: &OsString) -> Result<IpAddr, DaemonError> {
    let text = value.to_string_lossy();
    text.parse::<IpAddr>()
        .map_err(|_| config_error(format!("invalid bind address '{text}'")))
}

fn parse_max_sessions(value: &OsString) -> Result<NonZeroUsize, DaemonError> {
    let text = value.to_string_lossy();
    let parsed: usize = text
        .parse()
        .map_err(|_| config_error(format!("invalid value for --max-sessions: '{text}'")))?;
    NonZeroUsize::new(parsed)
        .ok_or_else(|| config_error("--max-sessions must be greater than zero".to_string()))
}

fn parse_module_definition(value: &OsString) -> Result<ModuleDefinition, DaemonError> {
    let text = value.to_string_lossy();
    let (name_part, remainder) = text.split_once('=').ok_or_else(|| {
        config_error(format!(
            "invalid module specification '{text}': expected NAME=PATH"
        ))
    })?;

    let name = name_part.trim();
    if name.is_empty() {
        return Err(config_error(
            "module name must be non-empty and cannot contain whitespace".to_string(),
        ));
    }
    if name
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '/' || ch == '\\')
    {
        return Err(config_error(
            "module name cannot contain whitespace or path separators".to_string(),
        ));
    }

    let (path_part, comment_part) = match remainder.split_once(',') {
        Some((path, comment)) => (path, Some(comment.trim().to_string())),
        None => (remainder, None),
    };

    let path_text = path_part.trim();
    if path_text.is_empty() {
        return Err(config_error("module path must be non-empty".to_string()));
    }

    let comment = comment_part.filter(|value| !value.is_empty());

    Ok(ModuleDefinition {
        name: name.to_string(),
        path: PathBuf::from(path_text),
        comment,
    })
}

fn unsupported_option(option: OsString) -> DaemonError {
    let text = format!("unsupported daemon argument '{}'", option.to_string_lossy());
    config_error(text)
}

fn config_error(text: String) -> DaemonError {
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn duplicate_argument(option: &str) -> DaemonError {
    config_error(format!("duplicate daemon argument '{option}'"))
}

fn duplicate_module(name: &str) -> DaemonError {
    config_error(format!("duplicate module definition '{name}'"))
}

fn bind_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("bind listener", address, error)
}

fn accept_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("accept connection on", address, error)
}

fn stream_error(peer: Option<SocketAddr>, action: &str, error: io::Error) -> DaemonError {
    match peer {
        Some(addr) => network_error(action, addr, error),
        None => network_error(action, "connection", error),
    }
}

fn network_error<T: fmt::Display>(action: &str, target: T, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} {target}: {error}");
    let message = Message::error(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(SOCKET_IO_EXIT_CODE, message)
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
            let _ = writeln!(stdout, "{help}");
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
                let _ = writeln!(stderr, "{}", error.message());
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

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
    fn runtime_options_parse_module_definitions() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--module"),
            OsString::from("docs=/srv/docs,Documentation"),
            OsString::from("--module"),
            OsString::from("logs=/var/log"),
        ])
        .expect("parse modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].name, "docs");
        assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
        assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
        assert_eq!(modules[1].name, "logs");
        assert_eq!(modules[1].path, PathBuf::from("/var/log"));
        assert!(modules[1].comment.is_none());
    }

    #[test]
    fn run_daemon_serves_single_legacy_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

        stream.write_all(b"module\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("error message");
        assert!(line.starts_with("@ERROR:"));

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_honours_max_sessions() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--max-sessions"),
                OsString::from("2"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        for _ in 0..2 {
            let mut stream = connect_with_retries(port);
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

            let mut line = String::new();
            reader.read_line(&mut line).expect("greeting");
            assert_eq!(line, "@RSYNCD: 32.0\n");

            stream.write_all(b"module\n").expect("send module request");
            stream.flush().expect("flush module request");

            line.clear();
            reader.read_line(&mut line).expect("error message");
            assert!(line.starts_with("@ERROR:"));

            line.clear();
            reader.read_line(&mut line).expect("exit message");
            assert_eq!(line, "@RSYNCD: EXIT\n");
        }

        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_lists_modules_on_request() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--module"),
                OsString::from("docs=/srv/docs,Documentation"),
                OsString::from("--module"),
                OsString::from("logs=/var/log"),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        line.clear();
        reader.read_line(&mut line).expect("ok line");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("first module");
        assert_eq!(line.trim_end(), "docs\tDocumentation");

        line.clear();
        reader.read_line(&mut line).expect("second module");
        assert_eq!(line.trim_end(), "logs");

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
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
    fn help_flag_renders_static_help_snapshot() {
        let (code, stdout, stderr) = run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = render_help();
        assert_eq!(stdout, expected.into_bytes());
    }

    #[test]
    fn run_daemon_rejects_unknown_argument() {
        let config = DaemonConfig::builder()
            .arguments([OsString::from("--unknown")])
            .build();

        let error = run_daemon(config).expect_err("unknown argument should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("unsupported daemon argument")
        );
    }

    #[test]
    fn run_daemon_rejects_invalid_port() {
        let config = DaemonConfig::builder()
            .arguments([OsString::from("--port"), OsString::from("not-a-number")])
            .build();

        let error = run_daemon(config).expect_err("invalid port should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("invalid value for --port")
        );
    }

    #[test]
    fn run_daemon_rejects_invalid_max_sessions() {
        let config = DaemonConfig::builder()
            .arguments([OsString::from("--max-sessions"), OsString::from("0")])
            .build();

        let error = run_daemon(config).expect_err("invalid max sessions should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("--max-sessions must be greater than zero")
        );
    }

    #[test]
    fn run_daemon_rejects_duplicate_session_limits() {
        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--once"),
                OsString::from("--max-sessions"),
                OsString::from("2"),
            ])
            .build();

        let error = run_daemon(config).expect_err("duplicate session limits should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate daemon argument '--max-sessions'")
        );
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

    fn connect_with_retries(port: u16) -> TcpStream {
        for attempt in 0..100 {
            match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                Ok(stream) => return stream,
                Err(error) => {
                    if attempt == 99 {
                        panic!("failed to connect to daemon: {error}");
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        unreachable!("loop exits via return or panic");
    }
}
