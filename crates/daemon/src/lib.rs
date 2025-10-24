#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_daemon` provides the thin command-line front-end for the Rust `oc-rsyncd`
//! binary. The crate now exposes a deterministic daemon loop capable of
//! accepting sequential legacy (`@RSYNCD:`) TCP connections, greeting each peer
//! with protocol `32`, serving `#list` requests from an in-memory module table,
//! authenticating `auth users` entries via the upstream challenge/response
//! exchange backed by the configured secrets file, and replying with
//! explanatory `@ERROR` messages when module transfers are not yet available.
//! Clients that initiate the newer binary negotiation (protocols ≥ 30) are
//! recognised automatically: the daemon responds with the negotiated protocol
//! advertisement and emits multiplexed error frames describing the current
//! feature gap so modern clients observe a graceful failure rather than
//! stalling on the ASCII greeting.
//! The number of connections can be capped via
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
//!   serves one or more connections. It recognises both the legacy ASCII
//!   prologue and the binary negotiation used by modern clients, ensuring
//!   graceful diagnostics regardless of the handshake style. Requests for
//!   `#list` reuse the configured module table, while module transfers continue
//!   to emit availability diagnostics until the full engine lands.
//! - Authentication mirrors upstream rsync: the daemon issues a base64-encoded
//!   challenge, verifies the client's response against the configured secrets
//!   file using MD5, and only then reports that transfers are unavailable while
//!   the data path is under construction.
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
//! Launching the daemon binds a TCP listener (defaulting to `127.0.0.1:873`),
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
//! assert_eq!(line, "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n");
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

use dns_lookup::lookup_addr;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::TryFrom;
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
#[cfg(test)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::time::Instant;

use std::process::{Command as ProcessCommand, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_checksums::strong::Md5;
use rsync_core::{
    bandwidth::{BandwidthLimiter, BandwidthParseError, parse_bandwidth_argument},
    message::{Message, Role},
    rsync_error, rsync_info,
    version::{RUST_VERSION, VersionInfoReport},
};
use rsync_logging::MessageSink;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, MessageCode, MessageFrame, ProtocolVersion,
    format_legacy_daemon_message, parse_legacy_daemon_message,
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
const DEFAULT_PORT: u16 = 873;
/// Timeout applied to accepted sockets to avoid hanging handshakes.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Error payload returned to clients while daemon functionality is incomplete.
const HANDSHAKE_ERROR_PAYLOAD: &str = "@ERROR: daemon functionality is unavailable in this build";
/// Error payload returned when a configured module is requested but file serving is unavailable.
const MODULE_UNAVAILABLE_PAYLOAD: &str =
    "@ERROR: module '{module}' transfers are not yet implemented in this build";
const ACCESS_DENIED_PAYLOAD: &str = "@ERROR: access denied to module '{module}' from {addr}";
/// Error payload returned when a requested module does not exist.
const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";
/// Error payload returned when a module reaches its connection cap.
const MODULE_MAX_CONNECTIONS_PAYLOAD: &str =
    "@ERROR: max connections ({limit}) reached -- try again later";
/// Digest algorithms advertised during the legacy daemon greeting.
const LEGACY_HANDSHAKE_DIGESTS: &[&str] = &["sha512", "sha256", "sha1", "md5", "md4"];

/// Deterministic help text describing the currently supported daemon surface.
const HELP_TEXT: &str = concat!(
    "oc-rsyncd 3.4.1-rust\n",
    "https://github.com/oferchen/rsync\n",
    "\n",
    "Usage: oc-rsyncd [--help] [--version] [--delegate-system-rsync] [ARGS...]\n",
    "\n",
    "Daemon mode is under active development. This build recognises:\n",
    "  --help        Show this help message and exit.\n",
    "  --version     Output version information and exit.\n",
    "  --delegate-system-rsync  Launch the system rsync daemon with the supplied arguments.\n",
    "  --bind ADDR         Bind to the supplied IPv4/IPv6 address (default 127.0.0.1).\n",
    "  --ipv4             Restrict the listener to IPv4 sockets.\n",
    "  --ipv6             Restrict the listener to IPv6 sockets (defaults to ::1 when no bind address is provided).\n",
    "  --port PORT         Listen on the supplied TCP port (default 873).\n",
    "  --once              Accept a single connection and exit.\n",
    "  --max-sessions N    Accept N connections before exiting (N > 0).\n",
    "  --config FILE      Load module definitions from FILE (rsyncd.conf subset).\n",
    "  --module SPEC      Register an in-memory module (NAME=PATH[,COMMENT]).\n",
    "  --motd-file FILE   Append MOTD lines from FILE before module listings.\n",
    "  --motd-line TEXT   Append TEXT as an additional MOTD line.\n",
    "  --pid-file FILE    Write the daemon PID to FILE for process supervision.\n",
    "  --bwlimit=RATE     Limit per-connection bandwidth in KiB/s (0 = unlimited).\n",
    "  --no-bwlimit       Remove any per-connection bandwidth limit configured so far.\n",
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
    bandwidth_limit: Option<NonZeroU64>,
    refuse_options: Vec<String>,
    read_only: bool,
    numeric_ids: bool,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<NonZeroU64>,
    listable: bool,
    use_chroot: bool,
    max_connections: Option<NonZeroU32>,
}

impl ModuleDefinition {
    fn permits(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
        if !self.hosts_allow.is_empty()
            && !self
                .hosts_allow
                .iter()
                .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        if self
            .hosts_deny
            .iter()
            .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        true
    }

    fn requires_hostname_lookup(&self) -> bool {
        self.hosts_allow
            .iter()
            .chain(self.hosts_deny.iter())
            .any(HostPattern::requires_hostname)
    }

    fn requires_authentication(&self) -> bool {
        !self.auth_users.is_empty()
    }

    fn max_connections(&self) -> Option<NonZeroU32> {
        self.max_connections
    }

    #[cfg(test)]
    fn auth_users(&self) -> &[String] {
        &self.auth_users
    }

    #[cfg(test)]
    fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    #[cfg(test)]
    fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    fn refused_options(&self) -> &[String] {
        &self.refuse_options
    }

    #[cfg(test)]
    fn read_only(&self) -> bool {
        self.read_only
    }

    #[cfg(test)]
    fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    #[cfg(test)]
    fn uid(&self) -> Option<u32> {
        self.uid
    }

    #[cfg(test)]
    fn gid(&self) -> Option<u32> {
        self.gid
    }

    #[cfg(test)]
    fn timeout(&self) -> Option<NonZeroU64> {
        self.timeout
    }

    #[cfg(test)]
    fn listable(&self) -> bool {
        self.listable
    }

    #[cfg(test)]
    fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    fn inherit_refuse_options(&mut self, options: &[String]) {
        if self.refuse_options.is_empty() {
            self.refuse_options = options.to_vec();
        }
    }
}

struct ModuleRuntime {
    definition: ModuleDefinition,
    active_connections: AtomicU32,
}

impl ModuleRuntime {
    fn new(definition: ModuleDefinition) -> Self {
        Self {
            definition,
            active_connections: AtomicU32::new(0),
        }
    }

    fn try_acquire_connection(&self) -> Result<ModuleConnectionGuard<'_>, NonZeroU32> {
        if let Some(limit) = self.definition.max_connections() {
            let limit_value = limit.get();
            let mut current = self.active_connections.load(Ordering::Acquire);
            loop {
                if current >= limit_value {
                    return Err(limit);
                }

                match self.active_connections.compare_exchange(
                    current,
                    current + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Ok(ModuleConnectionGuard::limited(self)),
                    Err(updated) => current = updated,
                }
            }
        } else {
            Ok(ModuleConnectionGuard::unlimited())
        }
    }

    fn release(&self) {
        if self.definition.max_connections().is_some() {
            self.active_connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl From<ModuleDefinition> for ModuleRuntime {
    fn from(definition: ModuleDefinition) -> Self {
        Self::new(definition)
    }
}

impl std::ops::Deref for ModuleRuntime {
    type Target = ModuleDefinition;

    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

struct ModuleConnectionGuard<'a> {
    module: Option<&'a ModuleRuntime>,
}

impl<'a> ModuleConnectionGuard<'a> {
    fn limited(module: &'a ModuleRuntime) -> Self {
        Self {
            module: Some(module),
        }
    }

    const fn unlimited() -> Self {
        Self { module: None }
    }
}

impl<'a> Drop for ModuleConnectionGuard<'a> {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            module.release();
        }
    }
}

fn module_peer_hostname<'a>(
    module: &ModuleDefinition,
    cache: &'a mut Option<Option<String>>,
    peer_ip: IpAddr,
) -> Option<&'a str> {
    if !module.requires_hostname_lookup() {
        return None;
    }

    if cache.is_none() {
        *cache = Some(resolve_peer_hostname(peer_ip));
    }

    cache.as_ref().and_then(|value| value.as_deref())
}

fn resolve_peer_hostname(peer_ip: IpAddr) -> Option<String> {
    #[cfg(test)]
    if let Some(mapped) = TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow().get(&peer_ip).cloned()) {
        return mapped.map(normalize_hostname_owned);
    }

    lookup_addr(&peer_ip).ok().map(normalize_hostname_owned)
}

fn normalize_hostname_owned(mut name: String) -> String {
    if name.ends_with('.') {
        name.pop();
    }
    name.make_ascii_lowercase();
    name
}

#[cfg(test)]
thread_local! {
    static TEST_HOSTNAME_OVERRIDES: RefCell<HashMap<IpAddr, Option<String>>> =
        RefCell::new(HashMap::new());
}

#[cfg(test)]
fn set_test_hostname_override(addr: IpAddr, hostname: Option<&str>) {
    TEST_HOSTNAME_OVERRIDES.with(|map| {
        map.borrow_mut()
            .insert(addr, hostname.map(|value| value.to_string()));
    });
}

#[cfg(test)]
fn clear_test_hostname_overrides() {
    TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow_mut().clear());
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

type SharedLogSink = Arc<Mutex<MessageSink<std::fs::File>>>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeOptions {
    bind_address: IpAddr,
    port: u16,
    max_sessions: Option<NonZeroUsize>,
    modules: Vec<ModuleDefinition>,
    motd_lines: Vec<String>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_configured: bool,
    address_family: Option<AddressFamily>,
    bind_address_overridden: bool,
    log_file: Option<PathBuf>,
    log_file_configured: bool,
    global_refuse_options: Option<Vec<String>>,
    pid_file: Option<PathBuf>,
    pid_file_from_config: bool,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            modules: Vec::new(),
            motd_lines: Vec::new(),
            bandwidth_limit: None,
            bandwidth_limit_configured: false,
            address_family: None,
            bind_address_overridden: false,
            log_file: None,
            log_file_configured: false,
            global_refuse_options: None,
            pid_file: None,
            pid_file_from_config: false,
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
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--address")? {
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--config")? {
                options.load_config_modules(&value, &mut seen_modules)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-file")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-line")? {
                options.push_motd_line(value);
            } else if let Some(value) = take_option_value(argument, &mut iter, "--bwlimit")? {
                let limit = parse_runtime_bwlimit(&value)?;
                options.set_bandwidth_limit(limit)?;
            } else if argument == "--no-bwlimit" {
                options.set_bandwidth_limit(None)?;
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).unwrap())?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--max-sessions")? {
                let max = parse_max_sessions(&value)?;
                options.set_max_sessions(max)?;
            } else if argument == "--ipv4" {
                options.force_address_family(AddressFamily::Ipv4)?;
            } else if argument == "--ipv6" {
                options.force_address_family(AddressFamily::Ipv6)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--log-file")? {
                options.set_log_file(PathBuf::from(value))?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--pid-file")? {
                options.set_pid_file(PathBuf::from(value))?;
            } else if argument == "--module" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--module"))?;
                let mut module = parse_module_definition(value)?;
                if let Some(global) = &options.global_refuse_options {
                    module.inherit_refuse_options(global);
                }
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

    fn set_bandwidth_limit(&mut self, limit: Option<NonZeroU64>) -> Result<(), DaemonError> {
        if self.bandwidth_limit_configured {
            return Err(duplicate_argument("--bwlimit"));
        }

        self.bandwidth_limit = limit;
        self.bandwidth_limit_configured = true;
        Ok(())
    }

    fn set_log_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if self.log_file_configured {
            return Err(duplicate_argument("--log-file"));
        }

        self.log_file = Some(path);
        self.log_file_configured = true;
        Ok(())
    }

    fn set_pid_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if let Some(existing) = &self.pid_file {
            if !self.pid_file_from_config {
                return Err(duplicate_argument("--pid-file"));
            }

            if existing == &path {
                self.pid_file_from_config = false;
                return Ok(());
            }
        }

        self.pid_file = Some(path);
        self.pid_file_from_config = false;
        Ok(())
    }

    fn set_bind_address(&mut self, addr: IpAddr) -> Result<(), DaemonError> {
        if let Some(family) = self.address_family {
            if !family.matches(addr) {
                return Err(match family {
                    AddressFamily::Ipv4 => config_error(
                        "cannot bind an IPv6 address when --ipv4 is specified".to_string(),
                    ),
                    AddressFamily::Ipv6 => config_error(
                        "cannot bind an IPv4 address when --ipv6 is specified".to_string(),
                    ),
                });
            }
        } else {
            self.address_family = Some(AddressFamily::from_ip(addr));
        }

        self.bind_address = addr;
        self.bind_address_overridden = true;
        Ok(())
    }

    fn force_address_family(&mut self, family: AddressFamily) -> Result<(), DaemonError> {
        if let Some(existing) = self.address_family {
            if existing != family {
                let text = if self.bind_address_overridden {
                    match existing {
                        AddressFamily::Ipv4 => {
                            "cannot use --ipv6 with an IPv4 bind address".to_string()
                        }
                        AddressFamily::Ipv6 => {
                            "cannot use --ipv4 with an IPv6 bind address".to_string()
                        }
                    }
                } else {
                    "cannot combine --ipv4 with --ipv6".to_string()
                };
                return Err(config_error(text));
            }
        } else {
            self.address_family = Some(family);
        }

        match family {
            AddressFamily::Ipv4 => {
                if matches!(self.bind_address, IpAddr::V6(_)) {
                    if self.bind_address_overridden {
                        return Err(config_error(
                            "cannot use --ipv4 with an IPv6 bind address".to_string(),
                        ));
                    }
                    self.bind_address = IpAddr::V4(Ipv4Addr::LOCALHOST);
                } else if !self.bind_address_overridden {
                    self.bind_address = IpAddr::V4(Ipv4Addr::LOCALHOST);
                }
            }
            AddressFamily::Ipv6 => {
                if matches!(self.bind_address, IpAddr::V4(_)) {
                    if self.bind_address_overridden {
                        return Err(config_error(
                            "cannot use --ipv6 with an IPv4 bind address".to_string(),
                        ));
                    }
                    self.bind_address = IpAddr::V6(Ipv6Addr::LOCALHOST);
                } else if !self.bind_address_overridden {
                    self.bind_address = IpAddr::V6(Ipv6Addr::LOCALHOST);
                }
            }
        }

        Ok(())
    }

    fn load_config_modules(
        &mut self,
        value: &OsString,
        seen_modules: &mut HashSet<String>,
    ) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let parsed = parse_config_modules(&path)?;

        for (options, origin) in parsed.global_refuse_options {
            self.inherit_global_refuse_options(options, &origin)?;
        }

        if let Some((pid_file, origin)) = parsed.pid_file {
            self.set_config_pid_file(pid_file, &origin)?;
        }

        if !parsed.motd_lines.is_empty() {
            self.motd_lines.extend(parsed.motd_lines);
        }

        let mut modules = parsed.modules;
        if let Some(global) = &self.global_refuse_options {
            for module in &mut modules {
                module.inherit_refuse_options(global);
            }
        }

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
    fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    #[cfg(test)]
    fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    #[cfg(test)]
    fn bind_address(&self) -> IpAddr {
        self.bind_address
    }

    #[cfg(test)]
    fn address_family(&self) -> Option<AddressFamily> {
        self.address_family
    }

    #[cfg(test)]
    fn motd_lines(&self) -> &[String] {
        &self.motd_lines
    }

    #[cfg(test)]
    fn log_file(&self) -> Option<&PathBuf> {
        self.log_file.as_ref()
    }

    #[cfg(test)]
    fn pid_file(&self) -> Option<&Path> {
        self.pid_file.as_deref()
    }

    fn inherit_global_refuse_options(
        &mut self,
        options: Vec<String>,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.global_refuse_options {
            if existing != &options {
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'refuse options' directive in global section",
                ));
            }
            return Ok(());
        }

        for module in &mut self.modules {
            module.inherit_refuse_options(&options);
        }

        self.global_refuse_options = Some(options);
        Ok(())
    }

    fn set_config_pid_file(
        &mut self,
        path: PathBuf,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.pid_file {
            if self.pid_file_from_config {
                if existing == &path {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'pid file' directive in global section",
                ));
            }

            return Ok(());
        }

        self.pid_file = Some(path);
        self.pid_file_from_config = true;
        Ok(())
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfigDirectiveOrigin {
    path: PathBuf,
    line: usize,
}

#[derive(Debug)]
struct ParsedConfigModules {
    modules: Vec<ModuleDefinition>,
    global_refuse_options: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
}

fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack)
}

fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<ParsedConfigModules, DaemonError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| config_io_error("read", path, error))?;

    if stack.iter().any(|seen| seen == &canonical) {
        return Err(config_parse_error(
            path,
            0,
            format!("recursive include detected for '{}'", canonical.display()),
        ));
    }

    let contents = fs::read_to_string(&canonical)
        .map_err(|error| config_io_error("read", &canonical, error))?;
    stack.push(canonical.clone());

    let mut modules = Vec::new();
    let mut current: Option<ModuleDefinitionBuilder> = None;
    let mut global_refuse_directives = Vec::new();
    let mut global_refuse_line: Option<usize> = None;
    let mut motd_lines = Vec::new();
    let mut pid_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;

    let result = (|| -> Result<ParsedConfigModules, DaemonError> {
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
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';')
                {
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

            if let Some(builder) = current.as_mut() {
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
                    "bwlimit" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'bwlimit' directive must not be empty",
                            ));
                        }
                        let limit = parse_config_bwlimit(value, path, line_number)?;
                        builder.set_bandwidth_limit(limit, path, line_number)?;
                    }
                    "refuse options" => {
                        let options = parse_refuse_option_list(value).map_err(|error| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid 'refuse options' directive: {error}"),
                            )
                        })?;
                        builder.set_refuse_options(options, path, line_number)?;
                    }
                    "read only" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'read only'"),
                            )
                        })?;
                        builder.set_read_only(parsed, path, line_number)?;
                    }
                    "use chroot" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'use chroot'"),
                            )
                        })?;
                        builder.set_use_chroot(parsed, path, line_number)?;
                    }
                    "numeric ids" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'numeric ids'"),
                            )
                        })?;
                        builder.set_numeric_ids(parsed, path, line_number)?;
                    }
                    "list" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'list'"),
                            )
                        })?;
                        builder.set_listable(parsed, path, line_number)?;
                    }
                    "uid" => {
                        let uid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid uid '{value}'"))
                        })?;
                        builder.set_uid(uid, path, line_number)?;
                    }
                    "gid" => {
                        let gid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid gid '{value}'"))
                        })?;
                        builder.set_gid(gid, path, line_number)?;
                    }
                    "timeout" => {
                        let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid timeout '{value}'"),
                            )
                        })?;
                        builder.set_timeout(timeout, path, line_number)?;
                    }
                    "max connections" => {
                        let max = parse_max_connections_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid max connections value '{value}'"),
                            )
                        })?;
                        builder.set_max_connections(max, path, line_number)?;
                    }
                    _ => {
                        // Unsupported directives are ignored for now.
                    }
                }
                continue;
            }

            match key.as_str() {
                "refuse options" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'refuse options' directive must not be empty",
                        ));
                    }
                    let options = parse_refuse_option_list(value).map_err(|error| {
                        config_parse_error(
                            path,
                            line_number,
                            format!("invalid 'refuse options' directive: {error}"),
                        )
                    })?;

                    if let Some(existing_line) = global_refuse_line {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            format!(
                                "duplicate 'refuse options' directive in global section (previously defined on line {})",
                                existing_line
                            ),
                        ));
                    }

                    global_refuse_line = Some(line_number);
                    global_refuse_directives.push((
                        options,
                        ConfigDirectiveOrigin {
                            path: canonical.clone(),
                            line: line_number,
                        },
                    ));
                }
                "include" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'include' directive must not be empty",
                        ));
                    }

                    let include_path = resolve_config_relative_path(&canonical, trimmed);
                    let included = parse_config_modules_inner(&include_path, stack)?;

                    if !included.modules.is_empty() {
                        modules.extend(included.modules);
                    }

                    if !included.motd_lines.is_empty() {
                        motd_lines.extend(included.motd_lines);
                    }

                    if !included.global_refuse_options.is_empty() {
                        global_refuse_directives.extend(included.global_refuse_options);
                    }
                }
                "motd file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'motd file' directive must not be empty",
                        ));
                    }

                    let motd_path = resolve_config_relative_path(path, trimmed);
                    let contents = fs::read_to_string(&motd_path).map_err(|error| {
                        config_parse_error(
                            path,
                            line_number,
                            format!(
                                "failed to read motd file '{}': {}",
                                motd_path.display(),
                                error
                            ),
                        )
                    })?;

                    for raw_line in contents.lines() {
                        motd_lines.push(raw_line.trim_end_matches('\r').to_string());
                    }
                }
                "motd" => {
                    motd_lines.push(value.trim_end_matches(['\r', '\n']).to_string());
                }
                "pid file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'pid file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    if let Some((existing, origin)) = &pid_file {
                        if existing != &resolved {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'pid file' directive in global section (previously defined on line {})",
                                    origin.line
                                ),
                            ));
                        }
                    } else {
                        pid_file = Some((
                            resolved,
                            ConfigDirectiveOrigin {
                                path: canonical.clone(),
                                line: line_number,
                            },
                        ));
                    }
                }
                _ => {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "directive outside module section",
                    ));
                }
            }
        }

        if let Some(builder) = current {
            modules.push(builder.finish(path)?);
        }

        Ok(ParsedConfigModules {
            modules,
            global_refuse_options: global_refuse_directives,
            motd_lines,
            pid_file,
        })
    })();

    stack.pop();
    result
}

fn resolve_config_relative_path(config_path: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }

    if let Some(parent) = config_path.parent() {
        parent.join(candidate)
    } else {
        candidate.to_path_buf()
    }
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
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_set: bool,
    refuse_options: Option<Vec<String>>,
    read_only: Option<bool>,
    numeric_ids: Option<bool>,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<Option<NonZeroU64>>,
    listable: Option<bool>,
    use_chroot: Option<bool>,
    max_connections: Option<Option<NonZeroU32>>,
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
            bandwidth_limit: None,
            bandwidth_limit_set: false,
            refuse_options: None,
            read_only: None,
            numeric_ids: None,
            uid: None,
            gid: None,
            timeout: None,
            listable: None,
            use_chroot: None,
            max_connections: None,
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

    fn set_bandwidth_limit(
        &mut self,
        limit: Option<NonZeroU64>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.bandwidth_limit_set {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'bwlimit' directive in module '{}'", self.name),
            ));
        }

        self.bandwidth_limit = limit;
        self.bandwidth_limit_set = true;
        Ok(())
    }

    fn set_refuse_options(
        &mut self,
        options: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.refuse_options.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'refuse options' directive in module '{}'",
                    self.name
                ),
            ));
        }

        if options.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'refuse options' directive in module '{}' must list at least one option",
                    self.name
                ),
            ));
        }

        self.refuse_options = Some(options);
        Ok(())
    }

    fn set_read_only(
        &mut self,
        read_only: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.read_only.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'read only' directive in module '{}'", self.name),
            ));
        }

        self.read_only = Some(read_only);
        Ok(())
    }

    fn set_numeric_ids(
        &mut self,
        numeric_ids: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.numeric_ids.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'numeric ids' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.numeric_ids = Some(numeric_ids);
        Ok(())
    }

    fn set_listable(
        &mut self,
        listable: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.listable.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'list' directive in module '{}'", self.name),
            ));
        }

        self.listable = Some(listable);
        Ok(())
    }

    fn set_use_chroot(
        &mut self,
        use_chroot: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.use_chroot.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'use chroot' directive in module '{}'", self.name),
            ));
        }

        self.use_chroot = Some(use_chroot);
        Ok(())
    }

    fn set_uid(&mut self, uid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.uid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'uid' directive in module '{}'", self.name),
            ));
        }

        self.uid = Some(uid);
        Ok(())
    }

    fn set_gid(&mut self, gid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.gid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'gid' directive in module '{}'", self.name),
            ));
        }

        self.gid = Some(gid);
        Ok(())
    }

    fn set_timeout(
        &mut self,
        timeout: Option<NonZeroU64>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.timeout.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'timeout' directive in module '{}'", self.name),
            ));
        }

        self.timeout = Some(timeout);
        Ok(())
    }

    fn set_max_connections(
        &mut self,
        max: Option<NonZeroU32>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.max_connections.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'max connections' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.max_connections = Some(max);
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

        let use_chroot = self.use_chroot.unwrap_or(true);

        if use_chroot && !path.is_absolute() {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' requires an absolute path when 'use chroot' is enabled",
                    self.name
                ),
            ));
        }

        if self.auth_users.as_ref().is_some_and(Vec::is_empty) {
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
            bandwidth_limit: self.bandwidth_limit,
            refuse_options: self.refuse_options.unwrap_or_default(),
            read_only: self.read_only.unwrap_or(false),
            numeric_ids: self.numeric_ids.unwrap_or(false),
            uid: self.uid,
            gid: self.gid,
            timeout: self.timeout.unwrap_or(None),
            listable: self.listable.unwrap_or(true),
            use_chroot,
            max_connections: self.max_connections.unwrap_or(None),
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

fn parse_refuse_option_list(value: &str) -> Result<Vec<String>, String> {
    let mut options = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            let canonical = trimmed.to_ascii_lowercase();
            if seen.insert(canonical.clone()) {
                options.push(canonical);
            }
        }
    }

    if options.is_empty() {
        return Err("must specify at least one option".to_string());
    }

    Ok(options)
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

fn parse_boolean_directive(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_numeric_identifier(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse().ok()
}

fn parse_timeout_seconds(value: &str) -> Option<Option<NonZeroU64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let seconds: u64 = trimmed.parse().ok()?;
    if seconds == 0 {
        Some(None)
    } else {
        Some(NonZeroU64::new(seconds))
    }
}

fn parse_max_connections_directive(value: &str) -> Option<Option<NonZeroU32>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "0" {
        return Some(None);
    }

    trimmed.parse::<NonZeroU32>().ok().map(Some)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostPattern {
    Any,
    Ipv4 { network: Ipv4Addr, prefix: u8 },
    Ipv6 { network: Ipv6Addr, prefix: u8 },
    Hostname(HostnamePattern),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressFamily {
    Ipv4,
    Ipv6,
}

impl AddressFamily {
    fn from_ip(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }

    fn matches(self, addr: IpAddr) -> bool {
        matches!(
            (self, addr),
            (Self::Ipv4, IpAddr::V4(_)) | (Self::Ipv6, IpAddr::V6(_))
        )
    }
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

        if prefix_text.is_some() {
            return Err("invalid host pattern; expected IPv4/IPv6 address".to_string());
        }

        HostnamePattern::parse(address_str).map(Self::Hostname)
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

    fn matches(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
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
            (Self::Hostname(pattern), _) => {
                hostname.map(|name| pattern.matches(name)).unwrap_or(false)
            }
            _ => false,
        }
    }

    fn requires_hostname(&self) -> bool {
        matches!(self, Self::Hostname(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostnamePattern {
    kind: HostnamePatternKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostnamePatternKind {
    Exact(String),
    Suffix(String),
    Wildcard(String),
}

impl HostnamePattern {
    fn parse(pattern: &str) -> Result<Self, String> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        let normalized = trimmed.trim_end_matches('.');
        let lower = normalized.to_ascii_lowercase();

        if lower.contains('*') || lower.contains('?') {
            return Ok(Self {
                kind: HostnamePatternKind::Wildcard(lower),
            });
        }

        if lower.starts_with('.') {
            let suffix = lower.trim_start_matches('.').to_string();
            return Ok(Self {
                kind: HostnamePatternKind::Suffix(suffix),
            });
        }

        Ok(Self {
            kind: HostnamePatternKind::Exact(lower),
        })
    }

    fn matches(&self, hostname: &str) -> bool {
        match &self.kind {
            HostnamePatternKind::Exact(expected) => hostname == expected,
            HostnamePatternKind::Suffix(suffix) => {
                if suffix.is_empty() {
                    return true;
                }

                if hostname == suffix {
                    return true;
                }

                if hostname.len() <= suffix.len() {
                    return false;
                }

                hostname.ends_with(suffix)
                    && hostname
                        .as_bytes()
                        .get(hostname.len() - suffix.len() - 1)
                        .is_some_and(|byte| *byte == b'.')
            }
            HostnamePatternKind::Wildcard(pattern) => wildcard_match(pattern, hostname),
        }
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();

    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text_bytes.len() {
        if pat_index < pattern_bytes.len()
            && (pattern_bytes[pat_index] == b'?'
                || pattern_bytes[pat_index] == text_bytes[text_index])
        {
            pat_index += 1;
            text_index += 1;
        } else if pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
            // Record the position of the wildcard and optimistically advance past it.
            star_index = Some(pat_index);
            pat_index += 1;
            match_index = text_index;
        } else if let Some(star_pos) = star_index {
            // Retry the match by letting the last '*' consume one additional character.
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern_bytes.len()
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
/// The helper binds a TCP listener (defaulting to `127.0.0.1:873`), accepts a
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
    delegate_system_rsync: bool,
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
            Arg::new("delegate-system-rsync")
                .long("delegate-system-rsync")
                .help("Launch the system rsync daemon with the supplied arguments.")
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
    let delegate_system_rsync = matches.get_flag("delegate-system-rsync");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();

    Ok(ParsedArgs {
        show_help,
        show_version,
        delegate_system_rsync,
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
        bandwidth_limit,
        log_file,
        pid_file,
        ..
    } = options;

    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path)?)
    } else {
        None
    };

    let modules: Arc<Vec<ModuleRuntime>> =
        Arc::new(modules.into_iter().map(ModuleRuntime::from).collect());
    let motd_lines = Arc::new(motd_lines);
    let requested_addr = SocketAddr::new(bind_address, port);
    let listener =
        TcpListener::bind(requested_addr).map_err(|error| bind_error(requested_addr, error))?;
    let local_addr = listener.local_addr().unwrap_or(requested_addr);

    if let Some(log) = log_sink.as_ref() {
        let text = format!(
            "rsyncd version {} starting, listening on port {}",
            RUST_VERSION,
            local_addr.port()
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let mut served = 0usize;
    let mut workers: Vec<thread::JoinHandle<WorkerResult>> = Vec::new();
    let max_sessions = max_sessions.map(NonZeroUsize::get);

    loop {
        reap_finished_workers(&mut workers)?;

        match listener.accept() {
            Ok((stream, peer_addr)) => {
                let modules = Arc::clone(&modules);
                let motd_lines = Arc::clone(&motd_lines);
                let log_for_worker = log_sink.as_ref().map(Arc::clone);
                let handle = thread::spawn(move || {
                    let modules_vec = modules.as_ref();
                    let motd_vec = motd_lines.as_ref();
                    handle_session(
                        stream,
                        peer_addr,
                        modules_vec.as_slice(),
                        motd_vec.as_slice(),
                        bandwidth_limit,
                        log_for_worker,
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

    let result = drain_workers(&mut workers);

    if let Some(log) = log_sink.as_ref() {
        let text = format!("rsyncd version {} shutting down", RUST_VERSION);
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    drop(pid_guard);

    result
}

struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn create(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| pid_file_error(&path, error))?;
            }
        }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|error| pid_file_error(&path, error))?;
        writeln!(file, "{}", std::process::id()).map_err(|error| pid_file_error(&path, error))?;
        file.sync_all()
            .map_err(|error| pid_file_error(&path, error))?;

        Ok(Self { path })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
        Ok(Err((peer, error))) => {
            let kind = error.kind();
            if is_connection_closed_error(kind) {
                Ok(())
            } else {
                Err(stream_error(peer, "serve legacy handshake", error))
            }
        }
        Err(panic) => {
            let description = match panic.downcast::<String>() {
                Ok(message) => *message,
                Err(payload) => match payload.downcast::<&str>() {
                    Ok(message) => (*message).to_string(),
                    Err(_) => "worker thread panicked".to_string(),
                },
            };
            let error = io::Error::other(description);
            Err(stream_error(None, "serve legacy handshake", error))
        }
    }
}

fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

fn handle_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    daemon_limit: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    let style = detect_session_style(&stream)?;
    configure_stream(&stream)?;

    let peer_host = resolve_peer_hostname(peer_addr.ip());
    if let Some(log) = log_sink.as_ref() {
        log_connection(log, peer_host.as_deref(), peer_addr);
    }

    match style {
        SessionStyle::Binary => handle_binary_session(stream, daemon_limit, log_sink),
        SessionStyle::Legacy => handle_legacy_session(
            stream,
            peer_addr,
            modules,
            motd_lines,
            daemon_limit,
            log_sink,
            peer_host,
        ),
    }
}

fn detect_session_style(stream: &TcpStream) -> io::Result<SessionStyle> {
    stream.set_nonblocking(true)?;
    let mut peek_buf = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let decision = match stream.peek(&mut peek_buf) {
        Ok(0) => Ok(SessionStyle::Legacy),
        Ok(_) => {
            if peek_buf[0] == b'@' {
                Ok(SessionStyle::Legacy)
            } else {
                Ok(SessionStyle::Binary)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(SessionStyle::Legacy),
        Err(error) => Err(error),
    };
    let restore_result = stream.set_nonblocking(false);
    match (decision, restore_result) {
        (Ok(style), Ok(())) => Ok(style),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(primary), Err(restore)) => Err(io::Error::new(
            primary.kind(),
            format!("{primary}; also failed to restore blocking mode: {restore}",),
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStyle {
    Legacy,
    Binary,
}

fn write_limited(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    payload: &[u8],
) -> io::Result<()> {
    if let Some(limiter) = limiter {
        let mut remaining = payload;
        while !remaining.is_empty() {
            let chunk_len = limiter.recommended_read_size(remaining.len());
            stream.write_all(&remaining[..chunk_len])?;
            limiter.register(chunk_len);
            remaining = &remaining[chunk_len..];
        }
        Ok(())
    } else {
        stream.write_all(payload)
    }
}

fn handle_legacy_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    daemon_limit: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    peer_host: Option<String>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut limiter = daemon_limit.map(BandwidthLimiter::new);

    let greeting = legacy_daemon_greeting();
    write_limited(reader.get_mut(), &mut limiter, greeting.as_bytes())?;
    reader.get_mut().flush()?;

    let mut request = None;
    let mut refused_options = Vec::new();

    while let Some(line) = read_trimmed_line(&mut reader)? {
        match parse_legacy_daemon_message(&line) {
            Ok(LegacyDaemonMessage::Version(_)) => {
                let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
                write_limited(reader.get_mut(), &mut limiter, ok.as_bytes())?;
                reader.get_mut().flush()?;
                continue;
            }
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_string());
                    continue;
                }
            }
            Ok(LegacyDaemonMessage::Exit) => return Ok(()),
            Ok(
                LegacyDaemonMessage::Ok
                | LegacyDaemonMessage::Capabilities { .. }
                | LegacyDaemonMessage::AuthRequired { .. }
                | LegacyDaemonMessage::AuthChallenge { .. },
            ) => {
                request = Some(line);
                break;
            }
            Err(_) => {}
        }

        request = Some(line);
        break;
    }

    let request = request.unwrap_or_default();

    advertise_capabilities(reader.get_mut(), modules)?;

    if request == "#list" {
        if let Some(log) = log_sink.as_ref() {
            log_list_request(log, peer_host.as_deref(), peer_addr);
        }
        respond_with_module_list(
            reader.get_mut(),
            &mut limiter,
            modules,
            motd_lines,
            peer_addr.ip(),
        )?;
    } else if request.is_empty() {
        write_limited(
            reader.get_mut(),
            &mut limiter,
            HANDSHAKE_ERROR_PAYLOAD.as_bytes(),
        )?;
        write_limited(reader.get_mut(), &mut limiter, b"\n")?;
        let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
        write_limited(reader.get_mut(), &mut limiter, exit.as_bytes())?;
        reader.get_mut().flush()?;
    } else {
        respond_with_module_request(
            &mut reader,
            &mut limiter,
            modules,
            &request,
            peer_addr.ip(),
            peer_host.as_deref(),
            &refused_options,
            log_sink.as_ref(),
        )?;
    }

    Ok(())
}

fn handle_binary_session(
    mut stream: TcpStream,
    daemon_limit: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    let mut limiter = daemon_limit.map(BandwidthLimiter::new);

    let mut client_bytes = [0u8; 4];
    stream.read_exact(&mut client_bytes)?;
    let client_raw = u32::from_be_bytes(client_bytes);
    let client_byte = client_raw.min(u32::from(u8::MAX)) as u8;
    ProtocolVersion::from_peer_advertisement(client_byte).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "binary negotiation protocol identifier outside supported range",
        )
    })?;

    let server_bytes = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream.write_all(&server_bytes)?;
    stream.flush()?;

    let mut frames = Vec::new();
    MessageFrame::new(
        MessageCode::Error,
        HANDSHAKE_ERROR_PAYLOAD.as_bytes().to_vec(),
    )?
    .encode_into_writer(&mut frames)?;
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_be_bytes().to_vec())?
        .encode_into_writer(&mut frames)?;
    write_limited(&mut stream, &mut limiter, &frames)?;
    stream.flush()?;

    if let Some(log) = log_sink.as_ref() {
        let message =
            rsync_info!("binary negotiation forwarded error frames").with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(())
}

fn legacy_daemon_greeting() -> String {
    let mut greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(ProtocolVersion::NEWEST));
    debug_assert!(greeting.ends_with('\n'));
    greeting.pop();

    for digest in LEGACY_HANDSHAKE_DIGESTS {
        greeting.push(' ');
        greeting.push_str(digest);
    }

    greeting.push('\n');
    greeting
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

fn advertise_capabilities(stream: &mut TcpStream, modules: &[ModuleRuntime]) -> io::Result<()> {
    for payload in advertised_capability_lines(modules) {
        let message = format_legacy_daemon_message(LegacyDaemonMessage::Capabilities {
            flags: payload.as_str(),
        });
        stream.write_all(message.as_bytes())?;
    }

    if modules.is_empty() {
        Ok(())
    } else {
        stream.flush()
    }
}

fn advertised_capability_lines(modules: &[ModuleRuntime]) -> Vec<String> {
    if modules.is_empty() {
        return Vec::new();
    }

    let mut features = Vec::with_capacity(2);
    features.push(String::from("modules"));

    if modules
        .iter()
        .any(|module| module.requires_authentication())
    {
        features.push(String::from("authlist"));
    }

    vec![features.join(" ")]
}

fn respond_with_module_list(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    peer_ip: IpAddr,
) -> io::Result<()> {
    for line in motd_lines {
        let payload = if line.is_empty() {
            "MOTD".to_string()
        } else {
            format!("MOTD {line}")
        };
        let message = format_legacy_daemon_message(LegacyDaemonMessage::Other(&payload));
        write_limited(stream, limiter, message.as_bytes())?;
    }

    let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
    write_limited(stream, limiter, ok.as_bytes())?;

    let mut hostname_cache: Option<Option<String>> = None;
    for module in modules {
        if !module.listable {
            continue;
        }

        let peer_host = module_peer_hostname(module, &mut hostname_cache, peer_ip);
        if !module.permits(peer_ip, peer_host) {
            continue;
        }

        let mut line = module.name.clone();
        if let Some(comment) = &module.comment {
            if !comment.is_empty() {
                line.push('\t');
                line.push_str(comment);
            }
        }
        line.push('\n');
        write_limited(stream, limiter, line.as_bytes())?;
    }

    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    Granted,
    Denied,
}

fn perform_module_authentication(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
) -> io::Result<AuthenticationStatus> {
    let challenge = generate_auth_challenge(peer_ip);
    {
        let stream = reader.get_mut();
        let message = format_legacy_daemon_message(LegacyDaemonMessage::AuthRequired {
            module: Some(&challenge),
        });
        write_limited(stream, limiter, message.as_bytes())?;
        stream.flush()?;
    }

    let response = match read_trimmed_line(reader)? {
        Some(line) => line,
        None => {
            deny_module(reader.get_mut(), module, peer_ip, limiter)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };

    let mut segments = response.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let username = segments.next().unwrap_or_default();
    let digest = segments
        .next()
        .map(|segment| segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace()))
        .unwrap_or("");

    if username.is_empty() || digest.is_empty() {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !module.auth_users.iter().any(|user| user == username) {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !verify_secret_response(module, username, &challenge, digest)? {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    Ok(AuthenticationStatus::Granted)
}

fn generate_auth_challenge(peer_ip: IpAddr) -> String {
    let mut input = [0u8; 32];
    let address_text = peer_ip.to_string();
    let address_bytes = address_text.as_bytes();
    let copy_len = address_bytes.len().min(16);
    input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = (timestamp.as_secs() & u64::from(u32::MAX)) as u32;
    let micros = timestamp.subsec_micros();
    let pid = std::process::id();

    input[16..20].copy_from_slice(&seconds.to_le_bytes());
    input[20..24].copy_from_slice(&micros.to_le_bytes());
    input[24..28].copy_from_slice(&pid.to_le_bytes());

    let mut hasher = Md5::new();
    hasher.update(&input);
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    challenge: &str,
    response: &str,
) -> io::Result<bool> {
    let secrets_path = match &module.secrets_file {
        Some(path) => path,
        None => return Ok(false),
    };

    let contents = fs::read_to_string(secrets_path)?;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((user, secret)) = line.split_once(':') {
            if user == username {
                let expected = compute_auth_response(secret, challenge);
                return Ok(expected == response);
            }
        }
    }

    Ok(false)
}

fn compute_auth_response(secret: &str, challenge: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(secret.as_bytes());
    hasher.update(challenge.as_bytes());
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn deny_module(
    stream: &mut TcpStream,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let payload = ACCESS_DENIED_PAYLOAD
        .replace("{module}", &module.name)
        .replace("{addr}", &peer_ip.to_string());
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

fn send_daemon_ok(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
    write_limited(stream, limiter, ok.as_bytes())?;
    stream.flush()
}

fn apply_module_bandwidth_limit(
    limiter: &mut Option<BandwidthLimiter>,
    module_limit: Option<NonZeroU64>,
) {
    if let Some(module_limit) = module_limit {
        match limiter {
            Some(existing) => {
                let current = existing.limit_bytes();
                if module_limit < current {
                    *existing = BandwidthLimiter::new(module_limit);
                }
            }
            None => {
                *limiter = Some(BandwidthLimiter::new(module_limit));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn respond_with_module_request(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    request: &str,
    peer_ip: IpAddr,
    session_peer_host: Option<&str>,
    options: &[String],
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    if let Some(module) = modules.iter().find(|module| module.name == request) {
        apply_module_bandwidth_limit(limiter, module.bandwidth_limit());

        let mut hostname_cache: Option<Option<String>> = None;
        let module_peer_host = module_peer_hostname(module, &mut hostname_cache, peer_ip);
        if module.permits(peer_ip, module_peer_host) {
            let _connection_guard = match module.try_acquire_connection() {
                Ok(guard) => guard,
                Err(limit) => {
                    let payload =
                        MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.get().to_string());
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, payload.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                    write_limited(stream, limiter, exit.as_bytes())?;
                    stream.flush()?;
                    if let Some(log) = log_sink {
                        log_module_limit(
                            log,
                            module_peer_host.or(session_peer_host),
                            peer_ip,
                            request,
                            limit,
                        );
                    }
                    return Ok(());
                }
            };

            if let Some(log) = log_sink {
                log_module_request(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }

            if let Some(refused) = refused_option(module, options) {
                let payload = format!("@ERROR: The server is configured to refuse {}", refused);
                let stream = reader.get_mut();
                write_limited(stream, limiter, payload.as_bytes())?;
                write_limited(stream, limiter, b"\n")?;
                let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                write_limited(stream, limiter, exit.as_bytes())?;
                stream.flush()?;
                if let Some(log) = log_sink {
                    log_module_refused_option(
                        log,
                        module_peer_host.or(session_peer_host),
                        peer_ip,
                        request,
                        refused,
                    );
                }
                return Ok(());
            }

            apply_module_timeout(reader.get_mut(), module)?;
            let mut acknowledged = false;
            if module.requires_authentication() {
                match perform_module_authentication(reader, limiter, module, peer_ip)? {
                    AuthenticationStatus::Denied => {
                        if let Some(log) = log_sink {
                            log_module_auth_failure(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        return Ok(());
                    }
                    AuthenticationStatus::Granted => {
                        if let Some(log) = log_sink {
                            log_module_auth_success(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        send_daemon_ok(reader.get_mut(), limiter)?;
                        acknowledged = true;
                    }
                }
            }

            if !acknowledged {
                send_daemon_ok(reader.get_mut(), limiter)?;
            }

            let payload = MODULE_UNAVAILABLE_PAYLOAD.replace("{module}", request);
            let stream = reader.get_mut();
            write_limited(stream, limiter, payload.as_bytes())?;
            write_limited(stream, limiter, b"\n")?;
            if let Some(log) = log_sink {
                log_module_unavailable(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }
        } else {
            if let Some(log) = log_sink {
                log_module_denied(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }
            deny_module(reader.get_mut(), module, peer_ip, limiter)?;
            return Ok(());
        }
    } else {
        let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", request);
        let stream = reader.get_mut();
        write_limited(stream, limiter, payload.as_bytes())?;
        write_limited(stream, limiter, b"\n")?;
        if let Some(log) = log_sink {
            log_unknown_module(log, session_peer_host, peer_ip, request);
        }
    }

    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    let stream = reader.get_mut();
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

fn open_log_sink(path: &Path) -> Result<SharedLogSink, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| log_file_error(path, error))?;
    Ok(Arc::new(Mutex::new(MessageSink::new(file))))
}

fn log_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open log file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn pid_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to write pid file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn log_message(log: &SharedLogSink, message: &Message) {
    if let Ok(mut sink) = log.lock() {
        if sink.write(message).is_ok() {
            let _ = sink.flush();
        }
    }
}

fn format_host(host: Option<&str>, fallback: IpAddr) -> String {
    host.map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

fn log_connection(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let text = format!("connect from {} ({})", display, peer_addr.ip());
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_list_request(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let text = format!("list request from {} ({})", display, peer_addr.ip());
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_request(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "module '{}' requested from {} ({})",
        module, display, peer_ip
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_limit(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limit: NonZeroU32,
) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "refusing module '{}' from {} ({}): max connections {}",
        module, display, peer_ip, limit,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_refused_option(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    refused: &str,
) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "refusing option '{}' for module '{}' from {} ({})",
        refused, module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_auth_failure(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "authentication failed for module '{}' from {} ({})",
        module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_auth_success(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "authentication succeeded for module '{}' from {} ({})",
        module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_unavailable(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "module '{}' transfers unavailable for {} ({})",
        module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_denied(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "access denied to module '{}' from {} ({})",
        module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_unknown_module(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let text = format!(
        "unknown module '{}' requested from {} ({})",
        module, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn parse_daemon_option(payload: &str) -> Option<&str> {
    let (keyword, remainder) = payload.split_once(char::is_whitespace)?;
    if !keyword.eq_ignore_ascii_case("OPTION") {
        return None;
    }

    let option = remainder.trim();
    if option.is_empty() {
        None
    } else {
        Some(option)
    }
}

fn refused_option<'a>(module: &ModuleDefinition, options: &'a [String]) -> Option<&'a str> {
    options.iter().find_map(|candidate| {
        let canonical_candidate = canonical_option(candidate);
        module
            .refuse_options
            .iter()
            .map(String::as_str)
            .any(|refused| canonical_option(refused) == canonical_candidate)
            .then_some(candidate.as_str())
    })
}

fn canonical_option(text: &str) -> String {
    let token = text
        .trim()
        .trim_start_matches('-')
        .split([' ', '\t', '='])
        .next()
        .unwrap_or("");
    token.to_ascii_lowercase()
}

fn apply_module_timeout(stream: &TcpStream, module: &ModuleDefinition) -> io::Result<()> {
    if let Some(timeout) = module.timeout {
        let duration = Duration::from_secs(timeout.get());
        stream.set_read_timeout(Some(duration))?;
        stream.set_write_timeout(Some(duration))?;
    }

    Ok(())
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

    let (path_part, comment_part) = split_module_path_and_comment(remainder);

    let path_text = path_part.trim();
    if path_text.is_empty() {
        return Err(config_error("module path must be non-empty".to_string()));
    }

    let path_text = unescape_module_component(path_text);
    let comment = comment_part
        .map(|value| unescape_module_component(value.trim()))
        .filter(|value| !value.is_empty());

    let path = PathBuf::from(&path_text);
    if !path.is_absolute() {
        return Err(config_error(format!(
            "module path '{}' must be absolute when 'use chroot' is enabled",
            path_text
        )));
    }

    Ok(ModuleDefinition {
        name: name.to_string(),
        path,
        comment,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        refuse_options: Vec::new(),
        read_only: false,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
    })
}

fn split_module_path_and_comment(value: &str) -> (&str, Option<&str>) {
    let chars = value.char_indices();
    let mut escape = false;

    for (idx, ch) in chars {
        if escape {
            escape = false;
            continue;
        }

        if ch == '\\' {
            escape = true;
            continue;
        }

        if ch == ',' {
            let (path, rest) = value.split_at(idx);
            let comment = rest.get(1..);
            return (path, comment);
        }
    }

    (value, None)
}

fn unescape_module_component(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                result.push(next);
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn parse_runtime_bwlimit(value: &OsString) -> Result<Option<NonZeroU64>, DaemonError> {
    let text = value.to_string_lossy();
    match parse_bandwidth_argument(&text) {
        Ok(limit) => Ok(limit),
        Err(error) => Err(runtime_bwlimit_error(&text, error)),
    }
}

fn parse_config_bwlimit(
    value: &str,
    path: &Path,
    line: usize,
) -> Result<Option<NonZeroU64>, DaemonError> {
    match parse_bandwidth_argument(value) {
        Ok(limit) => Ok(limit),
        Err(error) => Err(config_bwlimit_error(path, line, value, error)),
    }
}

fn runtime_bwlimit_error(value: &str, error: BandwidthParseError) -> DaemonError {
    let text = match error {
        BandwidthParseError::Invalid => format!("--bwlimit={} is invalid", value),
        BandwidthParseError::TooSmall => format!(
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            value
        ),
        BandwidthParseError::TooLarge => format!("--bwlimit={} is too large", value),
    };
    config_error(text)
}

fn config_bwlimit_error(
    path: &Path,
    line: usize,
    value: &str,
    error: BandwidthParseError,
) -> DaemonError {
    let detail = match error {
        BandwidthParseError::Invalid => format!("invalid 'bwlimit' value '{value}'"),
        BandwidthParseError::TooSmall => {
            format!("'bwlimit' value '{value}' is too small (min: 512 or 0 for unlimited)")
        }
        BandwidthParseError::TooLarge => format!("'bwlimit' value '{value}' is too large"),
    };
    config_parse_error(path, line, detail)
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

    if parsed.delegate_system_rsync
        || auto_delegate_system_rsync_enabled()
        || fallback_binary_configured()
    {
        return run_delegate_mode(parsed.remainder.as_slice(), stderr);
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

fn auto_delegate_system_rsync_enabled() -> bool {
    matches!(env_flag("OC_RSYNC_DAEMON_AUTO_DELEGATE"), Some(true))
}

fn fallback_binary_configured() -> bool {
    env::var_os("OC_RSYNC_DAEMON_FALLBACK")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("OC_RSYNC_FALLBACK").filter(|value| !value.is_empty()))
        .is_some()
}

fn env_flag(name: &str) -> Option<bool> {
    let value = env::var_os(name)?;
    let value = value.to_string_lossy();
    if value.trim().is_empty() {
        return Some(true);
    }

    match value.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "no" | "off" => Some(false),
        _ => Some(true),
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

fn run_delegate_mode<Err>(args: &[OsString], stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let binary = env::var_os("OC_RSYNC_DAEMON_FALLBACK")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("OC_RSYNC_FALLBACK").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| OsString::from("rsync"));

    let mut command = ProcessCommand::new(&binary);
    command.arg("--daemon");
    command.arg("--no-detach");
    command.args(args);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = rsync_error!(
                1,
                format!(
                    "failed to launch system rsync daemon '{}': {}",
                    Path::new(&binary).display(),
                    error
                )
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to launch system rsync daemon '{}': {}",
                    Path::new(&binary).display(),
                    error
                );
            }
            return 1;
        }
    };

    match child.wait() {
        Ok(status) => {
            if status.success() {
                0
            } else {
                let code = status.code().unwrap_or(MAX_EXIT_CODE);
                let message = rsync_error!(
                    code,
                    format!("system rsync daemon exited with status {}", status)
                )
                .with_role(Role::Daemon);
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(
                        stderr.writer_mut(),
                        "system rsync daemon exited with status {}",
                        status
                    );
                }
                code
            }
        }
        Err(error) => {
            let message = rsync_error!(
                1,
                format!("failed to wait for system rsync daemon: {}", error)
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to wait for system rsync daemon: {}",
                    error
                );
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;
    use tempfile::{NamedTempFile, tempdir};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn base_module(name: &str) -> ModuleDefinition {
        ModuleDefinition {
            name: String::from(name),
            path: PathBuf::from("/srv/module"),
            comment: None,
            hosts_allow: Vec::new(),
            hosts_deny: Vec::new(),
            auth_users: Vec::new(),
            secrets_file: None,
            bandwidth_limit: None,
            refuse_options: Vec::new(),
            read_only: false,
            numeric_ids: false,
            uid: None,
            gid: None,
            timeout: None,
            listable: true,
            use_chroot: true,
            max_connections: None,
        }
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    #[allow(unsafe_code)]
    impl EnvGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                unsafe {
                    std::env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write script");
        let mut permissions = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("set script permissions");
    }

    #[test]
    fn advertised_capability_lines_empty_without_modules() {
        assert!(advertised_capability_lines(&[]).is_empty());
    }

    #[test]
    fn advertised_capability_lines_report_modules_without_auth() {
        let module = ModuleRuntime::from(base_module("docs"));

        assert_eq!(
            advertised_capability_lines(&[module]),
            vec![String::from("modules")]
        );
    }

    #[test]
    fn advertised_capability_lines_include_authlist_when_required() {
        let mut definition = base_module("secure");
        definition.auth_users.push(String::from("alice"));
        definition.secrets_file = Some(PathBuf::from("secrets.txt"));
        let module = ModuleRuntime::from(definition);

        assert_eq!(
            advertised_capability_lines(&[module]),
            vec![String::from("modules authlist")]
        );
    }

    fn module_with_host_patterns(allow: &[&str], deny: &[&str]) -> ModuleDefinition {
        ModuleDefinition {
            name: String::from("module"),
            path: PathBuf::from("/srv/module"),
            comment: None,
            hosts_allow: allow
                .iter()
                .map(|pattern| HostPattern::parse(pattern).expect("parse allow pattern"))
                .collect(),
            hosts_deny: deny
                .iter()
                .map(|pattern| HostPattern::parse(pattern).expect("parse deny pattern"))
                .collect(),
            auth_users: Vec::new(),
            secrets_file: None,
            bandwidth_limit: None,
            refuse_options: Vec::new(),
            read_only: false,
            numeric_ids: false,
            uid: None,
            gid: None,
            timeout: None,
            listable: true,
            use_chroot: true,
            max_connections: None,
        }
    }

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
    fn module_definition_hostname_allow_matches_exact() {
        let module = module_with_host_patterns(&["trusted.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("trusted.example.com")));
        assert!(!module.permits(peer, Some("other.example.com")));
        assert!(!module.permits(peer, None));
    }

    #[test]
    fn module_definition_hostname_suffix_matches() {
        let module = module_with_host_patterns(&[".example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("node.example.com")));
        assert!(module.permits(peer, Some("example.com")));
        assert!(!module.permits(peer, Some("example.net")));
        assert!(!module.permits(peer, Some("sampleexample.com")));
    }

    #[test]
    fn module_definition_hostname_wildcard_matches() {
        let module = module_with_host_patterns(&["build?.example.*"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("build1.example.net")));
        assert!(module.permits(peer, Some("builda.example.org")));
        assert!(!module.permits(peer, Some("build12.example.net")));
    }

    #[test]
    fn module_definition_hostname_deny_takes_precedence() {
        let module = module_with_host_patterns(&["*"], &["bad.example.com"]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(!module.permits(peer, Some("bad.example.com")));
        assert!(module.permits(peer, Some("good.example.com")));
    }

    #[test]
    fn module_definition_hostname_wildcard_handles_multiple_asterisks() {
        let module = module_with_host_patterns(&["*build*node*.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("fastbuild-node1.example.com")));
        assert!(module.permits(peer, Some("build-node.example.com")));
        assert!(!module.permits(peer, Some("build.example.org")));
    }

    #[test]
    fn module_definition_hostname_wildcard_treats_question_as_single_character() {
        let module = module_with_host_patterns(&["app??.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("app12.example.com")));
        assert!(!module.permits(peer, Some("app1.example.com")));
        assert!(!module.permits(peer, Some("app123.example.com")));
    }

    #[test]
    fn module_definition_hostname_wildcard_collapses_consecutive_asterisks() {
        let module = module_with_host_patterns(&["**.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(module.permits(peer, Some("node.example.com")));
        assert!(!module.permits(peer, Some("node.example.org")));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_invokes_fallback_binary() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _guard = EnvGuard::set("OC_RSYNC_DAEMON_FALLBACK", script_path.as_os_str());

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--delegate-system-rsync"),
            OsStr::new("--config"),
            OsStr::new("/etc/rsyncd.conf"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let recorded = fs::read_to_string(&log_path).expect("read invocation log");
        assert!(recorded.contains("--daemon"));
        assert!(recorded.contains("--no-detach"));
        assert!(recorded.contains("--config /etc/rsyncd.conf"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_propagates_exit_code() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        write_executable_script(&script_path, "#!/bin/sh\nexit 7\n");
        let _guard = EnvGuard::set("OC_RSYNC_DAEMON_FALLBACK", script_path.as_os_str());

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--delegate-system-rsync"),
        ]);

        assert_eq!(code, 7);
        let stderr_str = String::from_utf8_lossy(&stderr);
        assert!(stderr_str.contains("system rsync daemon exited"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_falls_back_to_client_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _guard = EnvGuard::set("OC_RSYNC_FALLBACK", script_path.as_os_str());

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--delegate-system-rsync"),
            OsStr::new("--port"),
            OsStr::new("1234"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let recorded = fs::read_to_string(&log_path).expect("read invocation log");
        assert!(recorded.contains("--port 1234"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_env_triggers_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _fallback = EnvGuard::set("OC_RSYNC_DAEMON_FALLBACK", script_path.as_os_str());
        let _auto = EnvGuard::set("OC_RSYNC_DAEMON_AUTO_DELEGATE", OsStr::new("1"));

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--config"),
            OsStr::new("/etc/rsyncd.conf"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let recorded = fs::read_to_string(&log_path).expect("read invocation log");
        assert!(recorded.contains("--daemon"));
        assert!(recorded.contains("--config /etc/rsyncd.conf"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_fallback_env_triggers_delegation() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _fallback = EnvGuard::set("OC_RSYNC_FALLBACK", script_path.as_os_str());

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--config"),
            OsStr::new("/etc/rsyncd.conf"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let recorded = fs::read_to_string(&log_path).expect("read invocation log");
        assert!(recorded.contains("--daemon"));
        assert!(recorded.contains("--config /etc/rsyncd.conf"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_daemon_fallback_env_triggers_delegation() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _fallback = EnvGuard::set("OC_RSYNC_DAEMON_FALLBACK", script_path.as_os_str());

        let (code, _stdout, stderr) = run_with_args([
            OsStr::new("oc-rsyncd"),
            OsStr::new("--config"),
            OsStr::new("/etc/rsyncd.conf"),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let recorded = fs::read_to_string(&log_path).expect("read invocation log");
        assert!(recorded.contains("--daemon"));
        assert!(recorded.contains("--config /etc/rsyncd.conf"));
    }

    #[cfg(unix)]
    #[test]
    fn delegate_system_rsync_env_false_skips_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("rsync-wrapper.sh");
        let log_path = temp.path().join("invocation.log");
        let script = format!("#!/bin/sh\necho invoked > {}\nexit 0\n", log_path.display());
        write_executable_script(&script_path, &script);
        let _fallback = EnvGuard::set("OC_RSYNC_DAEMON_FALLBACK", script_path.as_os_str());
        let _auto = EnvGuard::set("OC_RSYNC_DAEMON_AUTO_DELEGATE", OsStr::new("0"));

        let (code, stdout, _stderr) =
            run_with_args([OsStr::new("oc-rsyncd"), OsStr::new("--help")]);

        assert_eq!(code, 0);
        assert!(!stdout.is_empty());
        assert!(!log_path.exists());
    }

    #[test]
    fn module_peer_hostname_uses_override() {
        clear_test_hostname_overrides();
        let module = module_with_host_patterns(&["trusted.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        set_test_hostname_override(peer, Some("Trusted.Example.Com"));
        let mut cache = None;
        let resolved = module_peer_hostname(&module, &mut cache, peer);
        assert_eq!(resolved, Some("trusted.example.com"));
        assert!(module.permits(peer, resolved));
        clear_test_hostname_overrides();
    }

    #[test]
    fn module_peer_hostname_missing_resolution_denies_hostname_only_rules() {
        clear_test_hostname_overrides();
        let module = module_with_host_patterns(&["trusted.example.com"], &[]);
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let mut cache = None;
        let resolved = module_peer_hostname(&module, &mut cache, peer);
        if let Some(host) = resolved {
            assert_ne!(host, "trusted.example.com");
        }
        assert!(!module.permits(peer, resolved));
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
        assert!(modules[0].bandwidth_limit().is_none());
        assert!(modules[0].refused_options().is_empty());
        assert_eq!(modules[1].name, "logs");
        assert_eq!(modules[1].path, PathBuf::from("/var/log"));
        assert!(modules[1].comment.is_none());
        assert!(modules[1].bandwidth_limit().is_none());
        assert!(modules[1].refused_options().is_empty());
    }

    #[test]
    fn runtime_options_module_definition_supports_escaped_commas() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--module"),
            OsString::from("docs=/srv/docs\\,archive,Project\\, Docs"),
        ])
        .expect("parse modules with escapes");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "docs");
        assert_eq!(modules[0].path, PathBuf::from("/srv/docs,archive"));
        assert_eq!(modules[0].comment.as_deref(), Some("Project, Docs"));
    }

    #[test]
    fn runtime_options_module_definition_preserves_escaped_backslash() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--module"),
            OsString::from("logs=/var/log\\\\files,Log share"),
        ])
        .expect("parse modules with escaped backslash");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].path, PathBuf::from("/var/log\\files"));
        assert_eq!(modules[0].comment.as_deref(), Some("Log share"));
    }

    #[test]
    fn runtime_options_parse_pid_file_argument() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--pid-file"),
            OsString::from("/var/run/oc-rsyncd.pid"),
        ])
        .expect("parse pid file argument");

        assert_eq!(
            options.pid_file(),
            Some(Path::new("/var/run/oc-rsyncd.pid"))
        );
    }

    #[test]
    fn runtime_options_reject_duplicate_pid_file_argument() {
        let error = RuntimeOptions::parse(&[
            OsString::from("--pid-file"),
            OsString::from("/var/run/one.pid"),
            OsString::from("--pid-file"),
            OsString::from("/var/run/two.pid"),
        ])
        .expect_err("duplicate pid file should fail");

        assert!(error.message().to_string().contains("--pid-file"));
    }

    #[test]
    fn runtime_options_ipv6_sets_default_bind_address() {
        let options =
            RuntimeOptions::parse(&[OsString::from("--ipv6")]).expect("parse --ipv6 succeeds");

        assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
    }

    #[test]
    fn runtime_options_ipv6_accepts_ipv6_bind_address() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--bind"),
            OsString::from("::1"),
            OsString::from("--ipv6"),
        ])
        .expect("ipv6 bind succeeds");

        assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
    }

    #[test]
    fn runtime_options_ipv6_rejects_ipv4_bind_address() {
        let error = RuntimeOptions::parse(&[
            OsString::from("--bind"),
            OsString::from("127.0.0.1"),
            OsString::from("--ipv6"),
        ])
        .expect_err("ipv4 bind with --ipv6 should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("cannot use --ipv6 with an IPv4 bind address")
        );
    }

    #[test]
    fn runtime_options_ipv4_rejects_ipv6_bind_address() {
        let error = RuntimeOptions::parse(&[
            OsString::from("--bind"),
            OsString::from("::1"),
            OsString::from("--ipv4"),
        ])
        .expect_err("ipv6 bind with --ipv4 should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("cannot use --ipv4 with an IPv6 bind address")
        );
    }

    #[test]
    fn runtime_options_rejects_ipv4_ipv6_combo() {
        let error = RuntimeOptions::parse(&[OsString::from("--ipv4"), OsString::from("--ipv6")])
            .expect_err("conflicting address families should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("cannot combine --ipv4 with --ipv6")
        );
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
        assert!(modules[0].bandwidth_limit().is_none());
        assert!(modules[0].listable());
        assert_eq!(modules[1].name, "logs");
        assert_eq!(modules[1].path, PathBuf::from("/var/log"));
        assert!(modules[1].comment.is_none());
        assert!(modules[1].bandwidth_limit().is_none());
        assert!(modules[1].listable());
        assert!(modules.iter().all(ModuleDefinition::use_chroot));
    }

    #[test]
    fn runtime_options_loads_pid_file_from_config() {
        let dir = tempdir().expect("config dir");
        let config_path = dir.path().join("rsyncd.conf");
        writeln!(
            File::create(&config_path).expect("create config"),
            "pid file = daemon.pid\n[docs]\npath = /srv/docs\n"
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .expect("parse config with pid file");

        let expected = dir.path().join("daemon.pid");
        assert_eq!(options.pid_file(), Some(expected.as_path()));
    }

    #[test]
    fn runtime_options_config_pid_file_respects_cli_override() {
        let dir = tempdir().expect("config dir");
        let config_path = dir.path().join("rsyncd.conf");
        writeln!(
            File::create(&config_path).expect("create config"),
            "pid file = config.pid\n[docs]\npath = /srv/docs\n"
        )
        .expect("write config");

        let cli_pid = PathBuf::from("/var/run/override.pid");
        let options = RuntimeOptions::parse(&[
            OsString::from("--pid-file"),
            cli_pid.as_os_str().to_os_string(),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .expect("parse config with cli override");

        assert_eq!(options.pid_file(), Some(cli_pid.as_path()));
    }

    #[test]
    fn runtime_options_loads_bwlimit_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert_eq!(module.name, "docs");
        assert_eq!(
            module.bandwidth_limit(),
            Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
        );
    }

    #[test]
    fn runtime_options_loads_refuse_options_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nrefuse options = delete, compress progress\n"
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert_eq!(module.name, "docs");
        assert_eq!(
            module.refused_options(),
            &["delete", "compress", "progress"]
        );
    }

    #[test]
    fn runtime_options_loads_boolean_and_id_directives_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nread only = yes\nnumeric ids = on\nuid = 1234\ngid = 4321\nlist = no\n",
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert!(module.read_only());
        assert!(module.numeric_ids());
        assert_eq!(module.uid(), Some(1234));
        assert_eq!(module.gid(), Some(4321));
        assert!(!module.listable());
        assert!(module.use_chroot());
    }

    #[test]
    fn runtime_options_loads_use_chroot_directive_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nuse chroot = no\n",).expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        assert!(!modules[0].use_chroot());
    }

    #[test]
    fn runtime_options_allows_relative_path_when_use_chroot_disabled() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = data/docs\nuse chroot = no\n",).expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].path, PathBuf::from("data/docs"));
        assert!(!modules[0].use_chroot());
    }

    #[test]
    fn runtime_options_loads_timeout_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 120\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert_eq!(module.timeout().map(NonZeroU64::get), Some(120));
    }

    #[test]
    fn runtime_options_allows_timeout_zero_in_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 0\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config modules");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);
        let module = &modules[0];
        assert!(module.timeout().is_none());
    }

    #[test]
    fn runtime_options_rejects_invalid_boolean_directive() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nread only = maybe\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid boolean should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("invalid boolean value 'maybe'")
        );
    }

    #[test]
    fn runtime_options_rejects_duplicate_use_chroot_directive() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nuse chroot = yes\nuse chroot = no\n",
        )
        .expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("duplicate directive should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate 'use chroot' directive")
        );
    }

    #[test]
    fn runtime_options_rejects_relative_path_with_chroot_enabled() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = data/docs\nuse chroot = yes\n",).expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("relative path with chroot should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("requires an absolute path when 'use chroot' is enabled")
        );
    }

    #[test]
    fn runtime_options_rejects_invalid_list_directive() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nlist = maybe\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid list boolean should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("invalid boolean value 'maybe' for 'list'")
        );
    }

    #[test]
    fn runtime_options_apply_global_refuse_options() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "refuse options = compress, delete\n[docs]\npath = /srv/docs\n[logs]\npath = /srv/logs\nrefuse options = stats\n",
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse config with global refuse options");

        assert_eq!(
            options.modules()[0].refused_options(),
            ["compress", "delete"]
        );
        assert_eq!(options.modules()[1].refused_options(), ["stats"]);
    }

    #[test]
    fn runtime_options_cli_modules_inherit_global_refuse_options() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "refuse options = compress\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from("extra=/srv/extra"),
        ])
        .expect("parse config with cli module");

        assert_eq!(options.modules()[0].refused_options(), ["compress"]);
    }

    #[test]
    fn runtime_options_loads_modules_from_included_config() {
        let dir = tempdir().expect("tempdir");
        let include_path = dir.path().join("modules.conf");
        writeln!(
            File::create(&include_path).expect("create include"),
            "[docs]\npath = /srv/docs\n"
        )
        .expect("write include");

        let main_path = dir.path().join("rsyncd.conf");
        writeln!(
            File::create(&main_path).expect("create config"),
            "include = modules.conf\n"
        )
        .expect("write main config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            main_path.as_os_str().to_os_string(),
        ])
        .expect("parse config with include");

        assert_eq!(options.modules().len(), 1);
        assert_eq!(options.modules()[0].name(), "docs");
    }

    #[test]
    fn parse_config_modules_detects_recursive_include() {
        let dir = tempdir().expect("tempdir");
        let first = dir.path().join("first.conf");
        let second = dir.path().join("second.conf");

        writeln!(
            File::create(&first).expect("create first"),
            "include = second.conf\n"
        )
        .expect("write first");
        writeln!(
            File::create(&second).expect("create second"),
            "include = first.conf\n"
        )
        .expect("write second");

        let error = parse_config_modules(&first).expect_err("recursive include should fail");
        assert!(error.message().to_string().contains("recursive include"));
    }

    #[test]
    fn runtime_options_rejects_duplicate_global_refuse_options() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "refuse options = compress\nrefuse options = delete\n[docs]\npath = /srv/docs\n",
        )
        .expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("duplicate global refuse options should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate 'refuse options' directive")
        );
    }

    #[test]
    fn runtime_options_rejects_invalid_uid() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nuid = alpha\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid uid should fail");

        assert!(error.message().to_string().contains("invalid uid"));
    }

    #[test]
    fn runtime_options_rejects_invalid_timeout() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\ntimeout = never\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid timeout should fail");

        assert!(error.message().to_string().contains("invalid timeout"));
    }

    #[test]
    fn runtime_options_rejects_invalid_bwlimit_in_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = nope\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid bwlimit should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("invalid 'bwlimit' value 'nope'")
        );
    }

    #[test]
    fn runtime_options_rejects_duplicate_bwlimit_in_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nbwlimit = 1M\nbwlimit = 2M\n"
        )
        .expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("duplicate bwlimit should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate 'bwlimit' directive")
        );
    }

    #[test]
    fn runtime_options_loads_max_connections_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 7\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("config parses");

        assert_eq!(options.modules[0].max_connections(), NonZeroU32::new(7));
    }

    #[test]
    fn runtime_options_loads_unlimited_max_connections_from_config() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 0\n").expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("config parses");

        assert!(options.modules[0].max_connections().is_none());
    }

    #[test]
    fn runtime_options_rejects_invalid_max_connections() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nmax connections = nope\n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("invalid max connections should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("invalid max connections value")
        );
    }

    #[test]
    fn runtime_options_rejects_duplicate_refuse_options_directives() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nrefuse options = delete\nrefuse options = compress\n"
        )
        .expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("duplicate refuse options should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate 'refuse options' directive")
        );
    }

    #[test]
    fn runtime_options_rejects_empty_refuse_options_directive() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[docs]\npath = /srv/docs\nrefuse options =   \n").expect("write config");

        let error = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect_err("empty refuse options should fail");

        let rendered = error.message().to_string();
        assert!(rendered.contains("must specify at least one option"));
    }

    #[test]
    fn runtime_options_parse_bwlimit_argument() {
        let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M")])
            .expect("parse bwlimit");

        assert_eq!(
            options.bandwidth_limit(),
            Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
        );
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn runtime_options_trim_bwlimit_argument() {
        let options =
            RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from(" 8M \n")])
                .expect("parse bwlimit");

        assert_eq!(
            options.bandwidth_limit(),
            Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
        );
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn runtime_options_parse_bwlimit_unlimited() {
        let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0")])
            .expect("parse unlimited");

        assert!(options.bandwidth_limit().is_none());
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn runtime_options_parse_no_bwlimit_argument() {
        let options =
            RuntimeOptions::parse(&[OsString::from("--no-bwlimit")]).expect("parse no-bwlimit");

        assert!(options.bandwidth_limit().is_none());
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn runtime_options_reject_invalid_bwlimit() {
        let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("foo")])
            .expect_err("invalid bwlimit should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("--bwlimit=foo is invalid")
        );
    }

    #[test]
    fn runtime_options_reject_duplicate_bwlimit() {
        let error = RuntimeOptions::parse(&[
            OsString::from("--bwlimit"),
            OsString::from("8M"),
            OsString::from("--bwlimit"),
            OsString::from("16M"),
        ])
        .expect_err("duplicate bwlimit should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate daemon argument '--bwlimit'")
        );
    }

    #[test]
    fn runtime_options_parse_log_file_argument() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--log-file"),
            OsString::from("/var/log/oc-rsyncd.log"),
        ])
        .expect("parse log file argument");

        assert_eq!(
            options.log_file(),
            Some(&PathBuf::from("/var/log/oc-rsyncd.log"))
        );
    }

    #[test]
    fn runtime_options_reject_duplicate_log_file_argument() {
        let error = RuntimeOptions::parse(&[
            OsString::from("--log-file"),
            OsString::from("/tmp/one.log"),
            OsString::from("--log-file"),
            OsString::from("/tmp/two.log"),
        ])
        .expect_err("duplicate log file should fail");

        assert!(
            error
                .message()
                .to_string()
                .contains("duplicate daemon argument '--log-file'")
        );
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
    fn runtime_options_loads_motd_from_config_directives() {
        let dir = tempdir().expect("motd dir");
        let config_path = dir.path().join("rsyncd.conf");
        let motd_path = dir.path().join("motd.txt");
        fs::write(&motd_path, "First line\nSecond line\r\n").expect("write motd file");

        fs::write(
            &config_path,
            "motd file = motd.txt\nmotd = Inline note\n[docs]\npath = /srv/docs\n",
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .expect("parse config with motd directives");

        let expected = vec![
            String::from("First line"),
            String::from("Second line"),
            String::from("Inline note"),
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
    fn runtime_options_parse_hostname_patterns() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nhosts allow = trusted.example.com,.example.org\nhosts deny = bad?.example.net\n",
        )
        .expect("write config");

        let options = RuntimeOptions::parse(&[
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ])
        .expect("parse hostname hosts directives");

        let modules = options.modules();
        assert_eq!(modules.len(), 1);

        let module = &modules[0];
        assert_eq!(module.hosts_allow.len(), 2);
        assert!(matches!(module.hosts_allow[0], HostPattern::Hostname(_)));
        assert!(matches!(module.hosts_allow[1], HostPattern::Hostname(_)));
        assert_eq!(module.hosts_deny.len(), 1);
        assert!(matches!(module.hosts_deny[0], HostPattern::Hostname(_)));
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

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
    fn run_daemon_handles_binary_negotiation() {
        use rsync_protocol::{BorrowedMessageFrames, MessageCode};

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
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
        stream
            .write_all(&advertisement)
            .expect("send client advertisement");
        stream.flush().expect("flush advertisement");

        let mut response = [0u8; 4];
        stream
            .read_exact(&mut response)
            .expect("read server advertisement");
        assert_eq!(response, advertisement);

        let mut frames = Vec::new();
        stream.read_to_end(&mut frames).expect("read frames");

        let mut iter = BorrowedMessageFrames::new(&frames);
        let first = iter.next().expect("first frame").expect("decode frame");
        assert_eq!(first.code(), MessageCode::Error);
        assert_eq!(first.payload(), HANDSHAKE_ERROR_PAYLOAD.as_bytes());
        let second = iter.next().expect("second frame").expect("decode frame");
        assert_eq!(second.code(), MessageCode::ErrorExit);
        assert_eq!(
            second.payload(),
            u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE)
                .expect("feature unavailable exit code fits")
                .to_be_bytes()
        );
        assert!(iter.next().is_none());

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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

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
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

        line.clear();
        reader.read_line(&mut line).expect("auth request");
        assert!(line.starts_with("@RSYNCD: AUTHREQD "));
        let challenge = line
            .trim_end()
            .strip_prefix("@RSYNCD: AUTHREQD ")
            .expect("challenge prefix");
        assert!(!challenge.is_empty());

        stream.write_all(b"\n").expect("send empty credentials");
        stream.flush().expect("flush empty credentials");

        line.clear();
        reader.read_line(&mut line).expect("denied message");
        assert_eq!(
            line.trim_end(),
            "@ERROR: access denied to module 'secure' from 127.0.0.1"
        );

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_enforces_module_connection_limit() {
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
        writeln!(
            fs::File::create(&config_path).expect("create config"),
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\nmax connections = 1\n",
            module_dir.display(),
            secrets_path.display()
        )
        .expect("write config");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--max-sessions"),
                OsString::from("2"),
                OsString::from("--config"),
                config_path.as_os_str().to_os_string(),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut first_stream = connect_with_retries(port);
        let mut first_reader = BufReader::new(first_stream.try_clone().expect("clone stream"));

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        first_reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        first_stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake");
        first_stream.flush().expect("flush handshake");

        line.clear();
        first_reader.read_line(&mut line).expect("handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

        first_stream
            .write_all(b"secure\n")
            .expect("send module request");
        first_stream.flush().expect("flush module");

        line.clear();
        first_reader
            .read_line(&mut line)
            .expect("capabilities for first client");
        assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

        line.clear();
        first_reader
            .read_line(&mut line)
            .expect("auth request for first client");
        assert!(line.starts_with("@RSYNCD: AUTHREQD"));

        let mut second_stream = connect_with_retries(port);
        let mut second_reader = BufReader::new(second_stream.try_clone().expect("clone second"));

        line.clear();
        second_reader.read_line(&mut line).expect("second greeting");
        assert_eq!(line, expected_greeting);

        second_stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send second handshake");
        second_stream.flush().expect("flush second handshake");

        line.clear();
        second_reader
            .read_line(&mut line)
            .expect("second handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

        second_stream
            .write_all(b"secure\n")
            .expect("send second module");
        second_stream.flush().expect("flush second module");

        line.clear();
        second_reader
            .read_line(&mut line)
            .expect("second capabilities");
        assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

        line.clear();
        second_reader.read_line(&mut line).expect("limit error");
        assert_eq!(
            line.trim_end(),
            "@ERROR: max connections (1) reached -- try again later"
        );

        line.clear();
        second_reader
            .read_line(&mut line)
            .expect("second exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        first_stream
            .write_all(b"\n")
            .expect("send empty credentials to first client");
        first_stream.flush().expect("flush first credentials");

        line.clear();
        first_reader
            .read_line(&mut line)
            .expect("first denial message");
        assert!(line.starts_with("@ERROR: access denied"));

        line.clear();
        first_reader
            .read_line(&mut line)
            .expect("first exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(second_reader);
        drop(second_stream);
        drop(first_reader);
        drop(first_stream);

        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn run_daemon_accepts_valid_credentials() {
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

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
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

        line.clear();
        reader.read_line(&mut line).expect("auth request");
        let challenge = line
            .trim_end()
            .strip_prefix("@RSYNCD: AUTHREQD ")
            .expect("challenge prefix");

        let mut hasher = Md5::new();
        hasher.update(b"password");
        hasher.update(challenge.as_bytes());
        let digest = STANDARD_NO_PAD.encode(hasher.finalize());
        let response_line = format!("alice {digest}\n");
        stream
            .write_all(response_line.as_bytes())
            .expect("send credentials");
        stream.flush().expect("flush credentials");

        line.clear();
        reader
            .read_line(&mut line)
            .expect("post-auth acknowledgement");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("unavailable message");
        assert_eq!(
            line.trim_end(),
            "@ERROR: module 'secure' transfers are not yet implemented in this build"
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

        let expected_greeting = legacy_daemon_greeting();
        for _ in 0..2 {
            let mut stream = connect_with_retries(port);
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

            let mut line = String::new();
            reader.read_line(&mut line).expect("greeting");
            assert_eq!(line, expected_greeting);

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
                assert_eq!(line, legacy_daemon_greeting());

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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

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
    fn run_daemon_writes_and_removes_pid_file() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let temp = tempdir().expect("pid dir");
        let pid_path = temp.path().join("oc-rsyncd.pid");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--pid-file"),
                pid_path.as_os_str().to_os_string(),
                OsString::from("--once"),
            ])
            .build();

        let pid_clone = pid_path.clone();
        let handle = thread::spawn(move || run_daemon(config));

        let start = Instant::now();
        while !pid_clone.exists() {
            if start.elapsed() > Duration::from_secs(5) {
                panic!("pid file not created");
            }
            thread::sleep(Duration::from_millis(20));
        }

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        drop(reader);
        drop(stream);

        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
        assert!(!pid_path.exists());
    }

    #[test]
    fn run_daemon_enforces_bwlimit_during_module_list() {
        let mut recorder = rsync_bandwidth::recorded_sleep_session();
        recorder.clear();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let comment = "x".repeat(4096);
        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--bwlimit"),
                OsString::from("1K"),
                OsString::from("--module"),
                OsString::from(format!("docs=/srv/docs,{}", comment)),
                OsString::from("--module"),
                OsString::from("logs=/var/log"),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        let mut total_bytes = 0usize;

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");
        total_bytes += line.as_bytes().len();

        line.clear();
        reader.read_line(&mut line).expect("ok line");
        assert_eq!(line, "@RSYNCD: OK\n");
        total_bytes += line.as_bytes().len();

        line.clear();
        reader.read_line(&mut line).expect("first module");
        assert_eq!(line.trim_end(), format!("docs\t{}", comment));
        total_bytes += line.as_bytes().len();

        line.clear();
        reader.read_line(&mut line).expect("second module");
        assert_eq!(line.trim_end(), "logs");
        total_bytes += line.as_bytes().len();

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");
        total_bytes += line.as_bytes().len();

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());

        let recorded = recorder.take();
        assert!(
            !recorded.is_empty(),
            "expected bandwidth limiter to record sleep intervals"
        );
        let total_sleep = recorded
            .into_iter()
            .fold(Duration::ZERO, |acc, duration| acc + duration);
        let expected = Duration::from_secs_f64(total_bytes as f64 / 1024.0);
        let tolerance = Duration::from_millis(250);
        let diff = if total_sleep > expected {
            total_sleep - expected
        } else {
            expected - total_sleep
        };
        assert!(
            diff <= tolerance,
            "expected sleep around {:?}, got {:?}",
            expected,
            total_sleep
        );
    }

    #[test]
    fn run_daemon_omits_unlisted_modules_from_listing() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[visible]\npath = /srv/visible\n\n[hidden]\npath = /srv/hidden\nlist = no\n",
        )
        .expect("write config");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--bwlimit"),
                OsString::from("1K"),
                OsString::from("--config"),
                file.path().as_os_str().to_os_string(),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

        line.clear();
        reader.read_line(&mut line).expect("ok line");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("first module");
        assert_eq!(line.trim_end(), "visible");

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());
    }

    #[test]
    fn module_bwlimit_cannot_raise_daemon_cap() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(2 * 1024 * 1024).unwrap(),
        ));

        apply_module_bandwidth_limit(&mut limiter, NonZeroU64::new(8 * 1024 * 1024));

        let limiter = limiter.expect("limiter remains configured");
        assert_eq!(
            limiter.limit_bytes(),
            NonZeroU64::new(2 * 1024 * 1024).unwrap()
        );
    }

    #[test]
    fn module_bwlimit_can_lower_daemon_cap() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(8 * 1024 * 1024).unwrap(),
        ));

        apply_module_bandwidth_limit(&mut limiter, NonZeroU64::new(1024 * 1024));

        let limiter = limiter.expect("limiter remains configured");
        assert_eq!(limiter.limit_bytes(), NonZeroU64::new(1024 * 1024).unwrap());
    }

    #[test]
    fn module_bwlimit_configures_unlimited_daemon() {
        let mut limiter = None;

        apply_module_bandwidth_limit(&mut limiter, NonZeroU64::new(2 * 1024 * 1024));

        let limiter = limiter.expect("limiter configured by module");
        assert_eq!(
            limiter.limit_bytes(),
            NonZeroU64::new(2 * 1024 * 1024).unwrap()
        );

        let mut limiter = Some(limiter);
        apply_module_bandwidth_limit(&mut limiter, None);
        let limiter = limiter.expect("limiter preserved");
        assert_eq!(
            limiter.limit_bytes(),
            NonZeroU64::new(2 * 1024 * 1024).unwrap()
        );
    }

    #[test]
    fn run_daemon_refuses_disallowed_module_options() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "[docs]\npath = /srv/docs\nrefuse options = compress\n",
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake response");
        stream.flush().expect("flush handshake response");

        line.clear();
        reader.read_line(&mut line).expect("handshake ack");
        assert_eq!(line, "@RSYNCD: OK\n");

        stream
            .write_all(b"@RSYNCD: OPTION --compress\n")
            .expect("send refused option");
        stream.flush().expect("flush refused option");

        stream.write_all(b"docs\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

        line.clear();
        reader.read_line(&mut line).expect("refusal message");
        assert_eq!(
            line.trim_end(),
            "@ERROR: The server is configured to refuse --compress",
        );

        line.clear();
        reader.read_line(&mut line).expect("exit message");
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

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
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

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
    fn run_daemon_filters_modules_during_list_request() {
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

        line.clear();
        reader.read_line(&mut line).expect("ok line");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("public module");
        assert_eq!(line.trim_end(), "public");

        line.clear();
        reader
            .read_line(&mut line)
            .expect("exit line after accessible modules");
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

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream.write_all(b"#list\n").expect("send list request");
        stream.flush().expect("flush list request");

        line.clear();
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

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
    fn run_daemon_records_log_file_entries() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("ephemeral port");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        let temp = tempdir().expect("log dir");
        let log_path = temp.path().join("oc-rsyncd.log");

        let config = DaemonConfig::builder()
            .arguments([
                OsString::from("--port"),
                OsString::from(port.to_string()),
                OsString::from("--log-file"),
                log_path.as_os_str().to_os_string(),
                OsString::from("--module"),
                OsString::from("docs=/srv/docs"),
                OsString::from("--once"),
            ])
            .build();

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let expected_greeting = legacy_daemon_greeting();
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

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
        reader.read_line(&mut line).expect("capabilities");
        assert_eq!(line, "@RSYNCD: CAP modules\n");

        line.clear();
        reader.read_line(&mut line).expect("module acknowledgement");
        assert_eq!(line, "@RSYNCD: OK\n");

        line.clear();
        reader.read_line(&mut line).expect("module response");
        assert!(line.starts_with("@ERROR:"));

        line.clear();
        reader.read_line(&mut line).expect("exit line");
        assert_eq!(line, "@RSYNCD: EXIT\n");

        drop(reader);
        let result = handle.join().expect("daemon thread");
        assert!(result.is_ok());

        let log_contents = fs::read_to_string(&log_path).expect("read log file");
        assert!(log_contents.contains("connect from"));
        assert!(log_contents.contains("127.0.0.1"));
        assert!(log_contents.contains("module 'docs'"));
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
