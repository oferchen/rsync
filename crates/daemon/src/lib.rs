#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_daemon` provides the thin command-line front-end for the Rust `oc-rsyncd`
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
//! stream.write_all(b"@RSYNCD: 32.0\n")?;
//! stream.flush()?;
//! line.clear();
//! reader.read_line(&mut line)?;
//! assert_eq!(line, "@RSYNCD: OK\n");
//! stream.write_all(b"module\n")?;
//! stream.flush()?;
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

use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_core::{
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_protocol::{
    LegacyDaemonMessage, ProtocolVersion, format_legacy_daemon_message, parse_legacy_daemon_message,
};

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
const AUTHENTICATION_UNAVAILABLE_PAYLOAD: &str =
    "@ERROR: authentication for module '{module}' is not implemented in this build";
const ACCESS_DENIED_PAYLOAD: &str = "@ERROR: access denied to module '{module}' from {addr}";
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
    "  --config FILE      Load module definitions from FILE (rsyncd.conf subset).\n",
    "  --module SPEC      Register an in-memory module (NAME=PATH[,COMMENT]).\n",
    "  --motd-file FILE   Append MOTD lines from FILE before module listings.\n",
    "  --motd-line TEXT   Append TEXT as an additional MOTD line.\n",
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
    hosts_allow: Vec<HostPattern>,
    hosts_deny: Vec<HostPattern>,
    auth_users: Vec<String>,
    secrets_file: Option<PathBuf>,
}

impl ModuleDefinition {
    fn permits(&self, addr: IpAddr) -> bool {
        if !self.hosts_allow.is_empty()
            && !self.hosts_allow.iter().any(|pattern| pattern.matches(addr))
        {
            return false;
        }

        if self.hosts_deny.iter().any(|pattern| pattern.matches(addr)) {
            return false;
        }

        true
    }

    fn requires_authentication(&self) -> bool {
        !self.auth_users.is_empty()
    }

    #[cfg(test)]
    fn auth_users(&self) -> &[String] {
        &self.auth_users
    }

    #[cfg(test)]
    fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }
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
    motd_lines: Vec<String>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            modules: Vec::new(),
            motd_lines: Vec::new(),
        }
    }
}

impl RuntimeOptions {
    fn parse(arguments: &[OsString]) -> Result<Self, DaemonError> {
        let mut options = Self::default();
        let mut seen_modules = HashSet::new();
        let mut iter = arguments.iter();

        while let Some(argument) = iter.next() {
            if let Some(value) = take_option_value(argument, &mut iter, "--port")? {
                options.port = parse_port(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--bind")? {
                options.bind_address = parse_bind_address(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--address")? {
                options.bind_address = parse_bind_address(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--config")? {
                options.load_config_modules(&value, &mut seen_modules)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-file")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-line")? {
                options.push_motd_line(value);
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).unwrap())?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--max-sessions")? {
                let max = parse_max_sessions(&value)?;
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

    fn load_config_modules(
        &mut self,
        value: &OsString,
        seen_modules: &mut HashSet<String>,
    ) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let modules = parse_config_modules(&path)?;

        for module in modules {
            if !seen_modules.insert(module.name.clone()) {
                return Err(duplicate_module(&module.name));
            }
            self.modules.push(module);
        }

        Ok(())
    }

    #[cfg(test)]
    fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
    }

    #[cfg(test)]
    fn motd_lines(&self) -> &[String] {
        &self.motd_lines
    }

    fn load_motd_file(&mut self, value: &OsString) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let contents =
            fs::read_to_string(&path).map_err(|error| config_io_error("read", &path, error))?;

        for raw_line in contents.lines() {
            let line = raw_line.trim_end_matches('\r').to_string();
            self.motd_lines.push(line);
        }

        Ok(())
    }

    fn push_motd_line(&mut self, value: OsString) {
        let line = value
            .to_string_lossy()
            .trim_matches(['\r', '\n'])
            .to_string();
        self.motd_lines.push(line);
    }
}

fn take_option_value<'a, I>(
    argument: &'a OsString,
    iter: &mut I,
    option: &str,
) -> Result<Option<OsString>, DaemonError>
where
    I: Iterator<Item = &'a OsString>,
{
    if argument == option {
        let value = iter
            .next()
            .cloned()
            .ok_or_else(|| missing_argument_value(option))?;
        return Ok(Some(value));
    }

    let text = argument.to_string_lossy();
    if let Some(rest) = text.strip_prefix(option) {
        if let Some(value) = rest.strip_prefix('=') {
            return Ok(Some(OsString::from(value)));
        }
    }

    Ok(None)
}

fn parse_config_modules(path: &Path) -> Result<Vec<ModuleDefinition>, DaemonError> {
    let contents =
        fs::read_to_string(path).map_err(|error| config_io_error("read", path, error))?;
    let mut modules = Vec::new();
    let mut current: Option<ModuleDefinitionBuilder> = None;

    for (index, raw_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') {
            let end = line.find(']').ok_or_else(|| {
                config_parse_error(path, line_number, "unterminated module header")
            })?;
            let name = line[1..end].trim();

            if name.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "module name must be non-empty",
                ));
            }

            ensure_valid_module_name(name)
                .map_err(|msg| config_parse_error(path, line_number, msg))?;

            let trailing = line[end + 1..].trim();
            if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';') {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "unexpected characters after module header",
                ));
            }

            if let Some(builder) = current.take() {
                modules.push(builder.finish(path)?);
            }

            current = Some(ModuleDefinitionBuilder::new(name.to_string(), line_number));
            continue;
        }

        let (key, value) = line.split_once('=').ok_or_else(|| {
            config_parse_error(path, line_number, "expected 'key = value' directive")
        })?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        let builder = current.as_mut().ok_or_else(|| {
            config_parse_error(path, line_number, "directive outside module section")
        })?;

        match key.as_str() {
            "path" => {
                if value.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "module path directive must not be empty",
                    ));
                }
                builder.set_path(PathBuf::from(value), path, line_number)?;
            }
            "comment" => {
                let comment = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
                builder.set_comment(comment, path, line_number)?;
            }
            "hosts allow" => {
                let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
                builder.set_hosts_allow(patterns, path, line_number)?;
            }
            "hosts deny" => {
                let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
                builder.set_hosts_deny(patterns, path, line_number)?;
            }
            "auth users" => {
                let users = parse_auth_user_list(value).map_err(|error| {
                    config_parse_error(
                        path,
                        line_number,
                        format!("invalid 'auth users' directive: {error}"),
                    )
                })?;
                builder.set_auth_users(users, path, line_number)?;
            }
            "secrets file" => {
                if value.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "'secrets file' directive must not be empty",
                    ));
                }
                builder.set_secrets_file(PathBuf::from(value), path, line_number)?;
            }
            _ => {
                // Unsupported directives are ignored for now.
            }
        }
    }

    if let Some(builder) = current {
        modules.push(builder.finish(path)?);
    }

    Ok(modules)
}

struct ModuleDefinitionBuilder {
    name: String,
    path: Option<PathBuf>,
    comment: Option<String>,
    hosts_allow: Option<Vec<HostPattern>>,
    hosts_deny: Option<Vec<HostPattern>>,
    auth_users: Option<Vec<String>>,
    secrets_file: Option<PathBuf>,
    declaration_line: usize,
}

impl ModuleDefinitionBuilder {
    fn new(name: String, line: usize) -> Self {
        Self {
            name,
            path: None,
            comment: None,
            hosts_allow: None,
            hosts_deny: None,
            auth_users: None,
            secrets_file: None,
            declaration_line: line,
        }
    }

    fn set_path(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.path.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'path' directive in module '{}'", self.name),
            ));
        }

        self.path = Some(path);
        Ok(())
    }

    fn set_comment(
        &mut self,
        comment: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.comment.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'comment' directive in module '{}'", self.name),
            ));
        }

        self.comment = comment;
        Ok(())
    }

    fn set_hosts_allow(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_allow.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'hosts allow' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.hosts_allow = Some(patterns);
        Ok(())
    }

    fn set_hosts_deny(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_deny.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'hosts deny' directive in module '{}'", self.name),
            ));
        }

        self.hosts_deny = Some(patterns);
        Ok(())
    }

    fn set_auth_users(
        &mut self,
        users: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.auth_users.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'auth users' directive in module '{}'", self.name),
            ));
        }

        if users.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        self.auth_users = Some(users);
        Ok(())
    }

    fn set_secrets_file(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.secrets_file.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'secrets file' directive in module '{}'",
                    self.name
                ),
            ));
        }

        let validated = validate_secrets_file(&path, config_path, line)?;
        self.secrets_file = Some(validated);
        Ok(())
    }

    fn finish(self, config_path: &Path) -> Result<ModuleDefinition, DaemonError> {
        let path = self.path.ok_or_else(|| {
            config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' is missing required 'path' directive",
                    self.name
                ),
            )
        })?;

        if self.auth_users.as_ref().map_or(false, Vec::is_empty) {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        if self.auth_users.is_some() && self.secrets_file.is_none() {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' specifies 'auth users' but is missing the required 'secrets file' directive",
                    self.name
                ),
            ));
        }

        Ok(ModuleDefinition {
            name: self.name,
            path,
            comment: self.comment,
            hosts_allow: self.hosts_allow.unwrap_or_default(),
            hosts_deny: self.hosts_deny.unwrap_or_default(),
            auth_users: self.auth_users.unwrap_or_default(),
            secrets_file: self.secrets_file,
        })
    }
}

fn parse_auth_user_list(value: &str) -> Result<Vec<String>, String> {
    let mut users = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            if seen.insert(trimmed.to_ascii_lowercase()) {
                users.push(trimmed.to_string());
            }
        }
    }

    if users.is_empty() {
        return Err("must specify at least one username".to_string());
    }

    Ok(users)
}

fn validate_secrets_file(
    path: &Path,
    config_path: &Path,
    line: usize,
) -> Result<PathBuf, DaemonError> {
    let metadata = fs::metadata(path).map_err(|error| {
        config_parse_error(
            config_path,
            line,
            format!(
                "failed to access secrets file '{}': {}",
                path.display(),
                error
            ),
        )
    })?;

    if !metadata.is_file() {
        return Err(config_parse_error(
            config_path,
            line,
            format!("secrets file '{}' must be a regular file", path.display()),
        ));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "secrets file '{}' must not be accessible to group or others (expected permissions 0600)",
                    path.display()
                ),
            ));
        }
    }

    Ok(path.to_path_buf())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostPattern {
    Any,
    Ipv4 { network: Ipv4Addr, prefix: u8 },
    Ipv6 { network: Ipv6Addr, prefix: u8 },
}

impl HostPattern {
    fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        if token == "*" || token.eq_ignore_ascii_case("all") {
            return Ok(Self::Any);
        }

        let (address_str, prefix_text) = if let Some((addr, mask)) = token.split_once('/') {
            (addr, Some(mask))
        } else {
            (token, None)
        };

        if let Ok(ipv4) = address_str.parse::<Ipv4Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv4 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv4(ipv4, prefix.unwrap_or(32));
        }

        if let Ok(ipv6) = address_str.parse::<Ipv6Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv6 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv6(ipv6, prefix.unwrap_or(128));
        }

        Err("invalid host pattern; expected IPv4/IPv6 address".to_string())
    }

    fn from_ipv4(addr: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err("IPv4 prefix length must be between 0 and 32".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv4 {
                network: Ipv4Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 32 - u32::from(prefix);
        let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
        let network = u32::from(addr) & mask;
        Ok(Self::Ipv4 {
            network: Ipv4Addr::from(network),
            prefix,
        })
    }

    fn from_ipv6(addr: Ipv6Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 128 {
            return Err("IPv6 prefix length must be between 0 and 128".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv6 {
                network: Ipv6Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 128 - u32::from(prefix);
        let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
        let network = u128::from(addr) & mask;
        Ok(Self::Ipv6 {
            network: Ipv6Addr::from(network),
            prefix,
        })
    }

    fn matches(&self, addr: IpAddr) -> bool {
        match (self, addr) {
            (Self::Any, _) => true,
            (Self::Ipv4 { network, prefix }, IpAddr::V4(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 32 - u32::from(*prefix);
                    let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
                    (u32::from(candidate) & mask) == u32::from(*network)
                }
            }
            (Self::Ipv6 { network, prefix }, IpAddr::V6(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 128 - u32::from(*prefix);
                    let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
                    (u128::from(candidate) & mask) == u128::from(*network)
                }
            }
            _ => false,
        }
    }
}

fn parse_host_list(
    value: &str,
    config_path: &Path,
    line: usize,
    directive: &str,
) -> Result<Vec<HostPattern>, DaemonError> {
    let mut patterns = Vec::new();

    for token in value.split(|ch: char| ch.is_ascii_whitespace() || ch == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let pattern = HostPattern::parse(token).map_err(|message| {
            config_parse_error(
                config_path,
                line,
                format!("{directive} directive contains invalid pattern '{token}': {message}"),
            )
        })?;
        patterns.push(pattern);
    }

    if patterns.is_empty() {
        return Err(config_parse_error(
            config_path,
            line,
            format!("{directive} directive must specify at least one pattern"),
        ));
    }

    Ok(patterns)
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

fn write_message<W: Write>(message: &Message, sink: &mut MessageSink<W>) -> io::Result<()> {
    sink.write(message)
}

fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    let RuntimeOptions {
        bind_address,
        port,
        max_sessions,
        modules,
        motd_lines,
    } = options;

    let modules = Arc::new(modules);
    let motd_lines = Arc::new(motd_lines);
    let requested_addr = SocketAddr::new(bind_address, port);
    let listener =
        TcpListener::bind(requested_addr).map_err(|error| bind_error(requested_addr, error))?;
    let local_addr = listener.local_addr().unwrap_or(requested_addr);

    let mut served = 0usize;
    let mut workers: Vec<thread::JoinHandle<WorkerResult>> = Vec::new();
    let max_sessions = max_sessions.map(NonZeroUsize::get);

    loop {
        reap_finished_workers(&mut workers)?;

        match listener.accept() {
            Ok((stream, peer_addr)) => {
                configure_stream(&stream)
                    .map_err(|error| stream_error(Some(peer_addr), "configure socket", error))?;
                let modules = Arc::clone(&modules);
                let motd_lines = Arc::clone(&motd_lines);
                let handle = thread::spawn(move || {
                    let modules_vec = modules.as_ref();
                    let motd_vec = motd_lines.as_ref();
                    handle_legacy_session(
                        stream,
                        peer_addr,
                        modules_vec.as_slice(),
                        motd_vec.as_slice(),
                    )
                    .map_err(|error| (Some(peer_addr), error))
                });
                workers.push(handle);
                served = served.saturating_add(1);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(error) => {
                return Err(accept_error(local_addr, error));
            }
        }

        if let Some(limit) = max_sessions {
            if served >= limit {
                break;
            }
        }
    }

    drain_workers(&mut workers)
}

type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;

fn reap_finished_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
) -> Result<(), DaemonError> {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let handle = workers.remove(index);
            join_worker(handle)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn drain_workers(workers: &mut Vec<thread::JoinHandle<WorkerResult>>) -> Result<(), DaemonError> {
    while let Some(handle) = workers.pop() {
        join_worker(handle)?;
    }
    Ok(())
}

fn join_worker(handle: thread::JoinHandle<WorkerResult>) -> Result<(), DaemonError> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err((peer, error))) => Err(stream_error(peer, "serve legacy handshake", error)),
        Err(panic) => {
            let description = match panic.downcast::<String>() {
                Ok(message) => *message,
                Err(payload) => match payload.downcast::<&str>() {
                    Ok(message) => (*message).to_string(),
                    Err(_) => "worker thread panicked".to_string(),
                },
            };
            let error = io::Error::new(io::ErrorKind::Other, description);
            Err(stream_error(None, "serve legacy handshake", error))
        }
    }
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

fn handle_legacy_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    modules: &[ModuleDefinition],
    motd_lines: &[String],
) -> io::Result<()> {
    let mut reader = BufReader::new(stream);

    let greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(ProtocolVersion::NEWEST));
    reader.get_mut().write_all(greeting.as_bytes())?;
    reader.get_mut().flush()?;

    let mut request = None;

    if let Some(line) = read_trimmed_line(&mut reader)? {
        if let Ok(LegacyDaemonMessage::Version(_)) = parse_legacy_daemon_message(&line) {
            let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
            reader.get_mut().write_all(ok.as_bytes())?;
            reader.get_mut().flush()?;
            request = read_trimmed_line(&mut reader)?;
        } else {
            request = Some(line);
        }
    }

    let request = request.unwrap_or_default();

    if request == "#list" {
        respond_with_module_list(reader.get_mut(), modules, motd_lines)?;
    } else if request.is_empty() {
        reader
            .get_mut()
            .write_all(HANDSHAKE_ERROR_PAYLOAD.as_bytes())?;
        reader.get_mut().write_all(b"\n")?;
        let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
        reader.get_mut().write_all(exit.as_bytes())?;
        reader.get_mut().flush()?;
    } else {
        respond_with_module_request(reader.get_mut(), modules, &request, peer_addr.ip())?;
    }

    Ok(())
}

fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn respond_with_module_list(
    stream: &mut TcpStream,
    modules: &[ModuleDefinition],
    motd_lines: &[String],
) -> io::Result<()> {
    for line in motd_lines {
        let payload = if line.is_empty() {
            "MOTD".to_string()
        } else {
            format!("MOTD {line}")
        };
        let message = format_legacy_daemon_message(LegacyDaemonMessage::Other(&payload));
        stream.write_all(message.as_bytes())?;
    }

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
    peer_ip: IpAddr,
) -> io::Result<()> {
    if let Some(module) = modules.iter().find(|module| module.name == request) {
        if module.permits(peer_ip) {
            if module.requires_authentication() {
                let message = format_legacy_daemon_message(LegacyDaemonMessage::AuthRequired {
                    module: Some(&module.name),
                });
                stream.write_all(message.as_bytes())?;
                let payload =
                    AUTHENTICATION_UNAVAILABLE_PAYLOAD.replace("{module}", &module.name);
                stream.write_all(payload.as_bytes())?;
                stream.write_all(b"\n")?;
                let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                stream.write_all(exit.as_bytes())?;
                return stream.flush();
            }

            let payload = MODULE_UNAVAILABLE_PAYLOAD.replace("{module}", request);
            stream.write_all(payload.as_bytes())?;
            stream.write_all(b"\n")?;
        } else {
            let payload = ACCESS_DENIED_PAYLOAD
                .replace("{module}", request)
                .replace("{addr}", &peer_ip.to_string());
            stream.write_all(payload.as_bytes())?;
            stream.write_all(b"\n")?;
        }
    } else {
        let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", request);
        stream.write_all(payload.as_bytes())?;
        stream.write_all(b"\n")?;
    }

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
    ensure_valid_module_name(name).map_err(|msg| config_error(msg.to_string()))?;

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
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        secrets_file: None,
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

fn config_parse_error(path: &Path, line: usize, message: impl Into<String>) -> DaemonError {
    let text = format!(
        "failed to parse config '{}': {} (line {})",
        path.display(),
        message.into(),
        line
    );
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn config_io_error(action: &str, path: &Path, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} config '{}': {error}", path.display());
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn ensure_valid_module_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("module name must be non-empty and cannot contain whitespace");
    }

    if name
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '/' || ch == '\\')
    {
        return Err("module name cannot contain whitespace or path separators");
    }

    Ok(())
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
    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(arguments) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Daemon);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{}", error);
            }
            1
        }
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
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
        let report = VersionInfoReport::default()
            .with_program_name(rsync_core::version::DAEMON_PROGRAM_NAME);
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
                let _ = writeln!(stderr.writer_mut(), "{}", error.message());
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
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;
    use tempfile::{NamedTempFile, tempdir};

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
    fn runtime_options_load_modules_from_config_file() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\ncomment = Documentation\n\n[logs]\npath=/var/log\n"
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

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
    fn runtime_options_parse_motd_sources() {
        let dir = tempdir().expect("motd dir");
        let motd_path = dir.path().join("motd.txt");
        fs::write(&motd_path, "Welcome to oc-rsyncd\nSecond line\n").expect("write motd");

        let options = RuntimeOptions::parse(&[
            OsString::from("--motd-file"),
            motd_path.as_os_str().to_os_string(),
            OsString::from("--motd-line"),
            OsString::from("Trailing notice"),
        ])
        .expect("parse motd options");

        let expected = vec![
            String::from("Welcome to oc-rsyncd"),
            String::from("Second line"),
            String::from("Trailing notice"),
        ];

        assert_eq!(options.motd_lines(), expected.as_slice());
    }

    #[test]
    fn runtime_options_parse_hosts_allow_and_deny() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nhosts allow = 127.0.0.1,192.168.0.0/24\nhosts deny = 192.168.0.5\n",
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse hosts directives");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);

        let module = &modules[0];
        assert_eq!(module.hosts_allow.len(), 2);
        assert!(matches!(
            module.hosts_allow[0],
            HostPattern::Ipv4 { prefix: 32, .. }
        ));
        assert!(matches!(
            module.hosts_allow[1],
            HostPattern::Ipv4 { prefix: 24, .. }
        ));
        assert_eq!(module.hosts_deny.len(), 1);
        assert!(matches!(
            module.hosts_deny[0],
            HostPattern::Ipv4 { prefix: 32, .. }
        ));
    }

    #[test]
    fn runtime_options_parse_auth_users_and_secrets_file() {
        let dir = tempdir().expect("config dir");
        let module_dir = dir.path().join("module");
        fs::create_dir_all(&module_dir).expect("module dir");
        let secrets_path = dir.path().join("secrets.txt");
        fs::write(&secrets_path, "alice:password\n").expect("write secrets");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
                .expect("chmod secrets");
        }

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[secure]\npath = {}\nauth users = alice, bob\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse auth users");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert_eq!(
            module.auth_users(),
            &[String::from("alice"), String::from("bob")]
        );
        assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
    }

    #[test]
    fn runtime_options_require_secrets_file_with_auth_users() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[secure]\npath = /srv/secure\nauth users = alice\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("missing secrets file should error");

        assert!(
            error
                .message()
                .to_string()
                .contains("missing the required 'secrets file' directive")
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_options_rejects_world_readable_secrets_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("config dir");
        let module_dir = dir.path().join("module");
        fs::create_dir_all(&module_dir).expect("module dir");
        let secrets_path = dir.path().join("secrets.txt");
        fs::write(&secrets_path, "alice:password\n").expect("write secrets");
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o644))
            .expect("chmod secrets");

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        )
        .expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("world-readable secrets file should error");

        assert!(
            error
                .message()
                .to_string()
                .contains("must not be accessible to group or others")
        );
    }

    #[test]
    fn runtime_options_rejects_config_missing_path() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\ncomment = sample\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("missing path should error");

        assert!(
            error
                .message()
                .to_string()
                .contains("missing required 'path' directive")
        );
    }

    #[test]
    fn runtime_options_rejects_duplicate_module_across_config_and_cli() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from("docs=/other/path"),
        ])
        .expect_err("duplicate module should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate module definition 'docs'")
        );
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

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake response");
        stream.flush().expect("flush handshake response");

        line.clear();
        reader.read_line(&mut line).expect("handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

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
    fn run_daemon_requests_authentication_for_protected_module() {
        let dir = tempdir().expect("config dir");
        let module_dir = dir.path().join("module");
        fs::create_dir_all(&module_dir).expect("module dir");
        let secrets_path = dir.path().join("secrets.txt");
        fs::write(&secrets_path, "alice:password\n").expect("write secrets");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
                .expect("chmod secrets");
        }

        let config_path = dir.path().join("rsyncd.conf");
        fs::write(
            &config_path,
            format!(
                "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
                module_dir.display(),
                secrets_path.display()
            ),
        )
        .expect("write config");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--once"),
                OsString::from("--config"),
                config_path.as_os_str().to_os_string(),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake response");
        stream.flush().expect("flush handshake response");

        line.clear();
        reader.read_line(&mut line).expect("handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

        stream.write_all(b"secure\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("auth request");
        assert_eq!(line, "@RSYNCD: AUTHREQD secure\n");

        line.clear();
        reader
            .read_line(&mut line)
            .expect("authentication error message");
        assert_eq!(
            line,
            "@ERROR: authentication for module 'secure' is not implemented in this build\n"
        );

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

            stream
                .write_all(b"@RSYNCD: 32.0\n")
                .expect("send handshake response");
            stream.flush().expect("flush handshake response");

            line.clear();
            reader.read_line(&mut line).expect("handshake ack");
            assert_eq!(line, "@RSYNCD: OK\n");

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
    fn run_daemon_handles_parallel_sessions() {
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

        let barrier = Arc::new(Barrier::new(2));
        let mut clients = Vec::new();

        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            clients.push(thread::spawn(move || {
                barrier.wait();
                let mut stream = connect_with_retries(port);
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

                let mut line = String::new();
                reader.read_line(&mut line).expect("greeting");
                assert_eq!(line, "@RSYNCD: 32.0\n");

                stream
                    .write_all(b"@RSYNCD: 32.0\n")
                    .expect("send handshake response");
                stream.flush().expect("flush handshake response");

                line.clear();
                reader.read_line(&mut line).expect("handshake ack");
                assert_eq!(line, "@RSYNCD: OK\n");

                stream.write_all(b"module\n").expect("send module request");
                stream.flush().expect("flush module request");

                line.clear();
                reader.read_line(&mut line).expect("error message");
                assert!(line.starts_with("@ERROR:"));

                line.clear();
                reader.read_line(&mut line).expect("exit message");
                assert_eq!(line, "@RSYNCD: EXIT\n");
            }));
        }

        for client in clients {
            client.join().expect("client thread");
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
    fn run_daemon_denies_module_when_host_not_allowed() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nhosts allow = 10.0.0.0/8\n",)
            .expect("write config");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--config"),
                file.path().as_os_str().to_os_string(),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake response");
        stream.flush().expect("flush handshake response");

        line.clear();
        reader.read_line(&mut line).expect("handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

        stream.write_all(b"docs\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("error message");
        assert_eq!(
            line.trim_end(),
            "@ERROR: access denied to module 'docs' from 127.0.0.1"
        );

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_lists_all_modules_during_list_request() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[public]\npath = /srv/public\n\n[private]\npath = /srv/private\nhosts allow = 10.0.0.0/8\n",
        )
        .expect("write config");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--config"),
                file.path().as_os_str().to_os_string(),
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
        reader.read_line(&mut line).expect("public module");
        assert_eq!(line.trim_end(), "public");

        line.clear();
        reader.read_line(&mut line).expect("private module");
        assert_eq!(line.trim_end(), "private");

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_lists_modules_with_motd_lines() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let dir = tempdir().expect("motd dir");
        let motd_path = dir.path().join("motd.txt");
        fs::write(
            &motd_path,
            "Welcome to oc-rsyncd\nRemember to sync responsibly\n",
        )
        .expect("write motd");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--motd-file"),
                motd_path.as_os_str().to_os_string(),
                OsString::from("--motd-line"),
                OsString::from("Additional notice"),
                OsString::from("--module"),
                OsString::from("docs=/srv/docs"),
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
        reader.read_line(&mut line).expect("motd line 1");
        assert_eq!(line.trim_end(), "@RSYNCD: MOTD Welcome to oc-rsyncd");

        line.clear();
        reader.read_line(&mut line).expect("motd line 2");
        assert_eq!(
            line.trim_end(),
            "@RSYNCD: MOTD Remember to sync responsibly"
        );

        line.clear();
        reader.read_line(&mut line).expect("motd line 3");
        assert_eq!(line.trim_end(), "@RSYNCD: MOTD Additional notice");

        line.clear();
        reader.read_line(&mut line).expect("ok line");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("module line");
        assert_eq!(line.trim_end(), "docs");

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn read_trimmed_line_strips_crlf_terminators() {
        let input: &[u8] = b"payload data\r\n";
        let mut reader = BufReader::new(input);

        let line = read_trimmed_line(&mut reader)
            .expect("read line")
            .expect("line available");

        assert_eq!(line, "payload data");

        let eof = read_trimmed_line(&mut reader).expect("eof read");
        assert!(eof.is_none());
    }

    #[test]
    fn version_flag_renders_report() {
        let (code, stdout, stderr) =
            run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--version")]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());

        let expected = VersionInfoReport::default()
            .with_program_name(rsync_core::version::DAEMON_PROGRAM_NAME)
            .human_readable();
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
