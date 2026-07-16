//! Implementation details backing [`crate::run_daemon`].
//!
//! Hosts the TCP listener loop, `@RSYNCD:` greeting negotiation, module
//! authentication, and per-connection session management. Configuration is
//! loaded from `oc-rsyncd.conf` and parsed into [`RuntimeOptions`] before the
//! accept loop starts.

use dns_lookup::lookup_host;

use std::borrow::Cow;
use std::collections::HashSet;
use std::convert::TryFrom;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(feature = "tracing")]
use tracing::instrument;

use std::process::{Command as ProcessCommand, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use checksums::strong::{Md4, Md5};
use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use core::client::{SkipCompressList, TcpFastOpenMode};
use core::{
    auth::{digests_for_protocol, verify_daemon_auth_response},
    bandwidth::{
        BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange,
        parse_bandwidth_limit,
    },
    branding::{self, Brand, manifest},
    exit_code::ExitCode,
    message::{Message, Role},
    rsync_error, rsync_info, rsync_warning,
    server::{
        HandshakeResult, ReferenceDirectory, ReferenceDirectoryKind, ServerConfig, ServerResult,
        ServerRole, run_server_with_handshake,
    },
};
// ASY sub-rung 2: tokio-driver entry for the socket-backed daemon receiver.
// Default-off; the threaded path never references it. See
// `crates/transfer/src/pipeline/tokio_driver.rs` and
// `docs/design/asy-2-tokio-runtime-feature.md` section 5.
#[cfg(feature = "tokio-transfer")]
use core::server::run_server_with_handshake_on;
// BENCHMARK-ONLY (default-off): receiver handoff for the gated async-bench path.
#[cfg(feature = "async-bench")]
use core::server::AsyncBenchReceiver;
use logging_sink::MessageSink;
use protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, MessageCode, MessageFrame, ProtocolVersion,
    filters::FilterRuleWireFormat, format_legacy_daemon_message, iconv::FilenameConverter,
    missing_greeting_token, parse_legacy_daemon_message,
};

use crate::{
    config::DaemonConfig,
    connection::{ConnectionState, InvalidTransition},
    daemon_stream::DaemonStream,
    error::DaemonError,
    systemd,
};

mod help;
pub(crate) mod tracing_stream;

/// Concurrent session tracking for the daemon accept loop.
#[cfg(feature = "concurrent-sessions")]
pub mod session_registry;

/// Thread-safe connection pool with per-IP rate limiting.
#[cfg(feature = "concurrent-sessions")]
pub mod connection_pool;

#[cfg(all(test, feature = "concurrent-sessions"))]
mod concurrent_tests;

/// Tokio-based async session handling for the rsync daemon.
#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub mod async_session;

use self::help::help_text;

/// Exit code used when daemon functionality is unavailable.
pub(crate) const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code for a usage/syntax error, mirroring upstream `RERR_SYNTAX`.
///
/// upstream: errcode.h:25 - `#define RERR_SYNTAX 1`. The daemon's read-only
/// push and write-only pull rejections call `exit_cleanup(RERR_SYNTAX)`
/// (main.c:934, main.c:1167).
pub(crate) const RERR_SYNTAX_EXIT_CODE: i32 = 1;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;

/// Maximum exit code representable by a Unix process.
pub(crate) const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Default bind address when no CLI overrides are provided.
const DEFAULT_BIND_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
/// Default port used for the development daemon listener.
const DEFAULT_PORT: u16 = 873;

/// Environment variable that overrides the default config file path (branded).
pub(crate) const BRANDED_CONFIG_ENV: &str = "OC_RSYNC_CONFIG";
/// Legacy environment variable for config file override; checked when `OC_RSYNC_CONFIG` is unset.
pub(crate) const LEGACY_CONFIG_ENV: &str = "RSYNCD_CONFIG";
/// Environment variable that overrides the default secrets file path (branded).
pub(crate) const BRANDED_SECRETS_ENV: &str = "OC_RSYNC_SECRETS";
/// Legacy environment variable for secrets file override; checked when `OC_RSYNC_SECRETS` is unset.
pub(crate) const LEGACY_SECRETS_ENV: &str = "RSYNCD_SECRETS";
/// Timeout applied to accepted sockets to avoid hanging handshakes.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Error payload returned to clients while daemon functionality is incomplete.
pub(crate) const HANDSHAKE_ERROR_PAYLOAD: &str =
    "@ERROR: daemon functionality is unavailable in this build";
/// Error payload sent when a host is denied access to a module.
///
/// upstream: clientserver.c:733 - `@ERROR: access denied to %s from %s (%s)\n`
/// where args are (name, host, addr).
pub(crate) const ACCESS_DENIED_PAYLOAD: &str =
    "@ERROR: access denied to {module} from {host} ({addr})";
/// Error payload sent when authentication fails on a protected module.
///
/// upstream: clientserver.c:762 - `@ERROR: auth failed on module %s\n`
pub(crate) const AUTH_FAILED_PAYLOAD: &str = "@ERROR: auth failed on module {module}";
/// Error payload returned when a requested module does not exist.
///
/// upstream: clientserver.c:730 - `@ERROR: Unknown module '%s'\n`
pub(crate) const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";
/// Error payload returned when a `#`-prefixed request is not a recognized command.
///
/// A `#`-prefixed line (e.g. `#bogus`) that is neither `#list` nor the
/// already-consumed `#early_input=` command is a command the daemon does not
/// understand. Upstream rejects it with a message distinct from the
/// unknown-module response, keeping the raw line (leading `#` included).
///
/// upstream: clientserver.c:1429 - `@ERROR: Unknown command '%s'\n`
pub(crate) const UNKNOWN_COMMAND_PAYLOAD: &str = "@ERROR: Unknown command '{command}'";
/// Error payload returned when a module reaches its connection cap.
///
/// upstream: clientserver.c:752 - `@ERROR: max connections (%d) reached -- try again later\n`
pub(crate) const MODULE_MAX_CONNECTIONS_PAYLOAD: &str =
    "@ERROR: max connections ({limit}) reached -- try again later";
/// Error payload returned when opening the module connection lock file fails.
///
/// upstream: clientserver.c:748 - `@ERROR: failed to open lock file\n`
pub(crate) const MODULE_LOCK_ERROR_PAYLOAD: &str = "@ERROR: failed to open lock file";
/// Error payload returned when chroot fails during module setup.
///
/// upstream: clientserver.c:981 - `@ERROR: chroot failed\n`
pub(crate) const CHROOT_FAILED_PAYLOAD: &str = "@ERROR: chroot failed";
/// Error payload returned when chdir fails during module setup.
///
/// upstream: clientserver.c:647 - `@ERROR: chdir failed\n`
pub(crate) const CHDIR_FAILED_PAYLOAD: &str = "@ERROR: chdir failed";
/// Error payload returned when setuid fails after chroot.
///
/// upstream: clientserver.c:1039 - `@ERROR: setuid failed\n`
pub(crate) const SETUID_FAILED_PAYLOAD: &str = "@ERROR: setuid failed";
/// Error payload returned when setgid fails after chroot.
///
/// upstream: clientserver.c:1010 - `@ERROR: setgid failed\n`
pub(crate) const SETGID_FAILED_PAYLOAD: &str = "@ERROR: setgid failed";
/// Error payload returned when setgroups fails after chroot.
///
/// upstream: clientserver.c:1017 - `@ERROR: setgroups failed\n`
pub(crate) const SETGROUPS_FAILED_PAYLOAD: &str = "@ERROR: setgroups failed";
/// Error payload returned when a module's uid directive is invalid.
///
/// upstream: clientserver.c:783 - `@ERROR: invalid uid %s\n`
#[cfg(test)]
pub(crate) const INVALID_UID_PAYLOAD: &str = "@ERROR: invalid uid {uid}";
/// Error payload returned when a module's gid directive is invalid.
///
/// upstream: clientserver.c:656 - `@ERROR: invalid gid %s\n`
#[cfg(test)]
pub(crate) const INVALID_GID_PAYLOAD: &str = "@ERROR: invalid gid {gid}";
/// Error payload returned when a module is read-only and the client pushes.
///
/// upstream: main.c:1167 `do_server_recv()` - `rprintf(FERROR, "ERROR:
/// module is read only\n")`. This fires after `setup_protocol()` and
/// `io_start_multiplex_out()`, so the text is a plain `FERROR` message (no
/// `@ERROR:` greeting prefix) delivered inside a `MSG_ERROR_XFER` frame.
pub(crate) const MODULE_READ_ONLY_PAYLOAD: &str = "ERROR: module is read only";
/// Error payload returned when a module is write-only and the client pulls.
///
/// upstream: main.c:935 `do_server_sender()` - `rprintf(FERROR, "ERROR:
/// module is write only\n")`, delivered post-multiplex like the read-only
/// rejection above.
pub(crate) const MODULE_WRITE_ONLY_PAYLOAD: &str = "ERROR: module is write only";
mod module_state;
#[cfg(test)]
use self::module_state::TEST_CONFIG_CANDIDATES;
use self::module_state::build_module_runtimes;
use self::module_state::resolve_peer_hostname;
pub(crate) use self::module_state::{
    AuthUser, ConnectionLimiter, GidSetting, ModuleConnectionError, ModuleDefinition,
    ModuleRuntime, UserAccessLevel, module_peer_hostname,
};
#[cfg(test)]
pub(crate) use self::module_state::{
    TEST_SECRETS_CANDIDATES, TEST_SECRETS_ENV, TestSecretsEnvOverride,
    clear_test_hostname_overrides, set_test_forward_override, set_test_hostname_override,
    set_test_netgroup_members,
};

type SharedLogSink = Arc<Mutex<MessageSink<std::fs::File>>>;

include!("daemon/runtime_options/types.rs");
include!("daemon/runtime_options/parsing.rs");
include!("daemon/runtime_options/setters.rs");
include!("daemon/runtime_options/config.rs");
include!("daemon/runtime_options/accessors.rs");
include!("daemon/runtime_options/resolve.rs");
include!("daemon/runtime_options/tests.rs");

include!("daemon/sections/config_paths.rs");

include!("daemon/sections/config_parsing.rs");

include!("daemon/sections/module_definition.rs");

include!("daemon/sections/config_helpers.rs");

include!("daemon/sections/group_expansion.rs");

/// Runs the daemon orchestration using the provided configuration.
///
/// Parses runtime options from the `DaemonConfig` arguments, loads
/// `rsyncd.conf`, and either serves a single session over stdin/stdout
/// (inetd/connect-program mode) or binds a TCP listener and enters the
/// connection accept loop.
///
/// upstream: clientserver.c:1496-1510 - `daemon_main()` checks
/// `is_a_socket(STDIN_FILENO)` before binding TCP. When stdin is a socket
/// (inetd, xinetd, systemd socket activation, or `RSYNC_CONNECT_PROG`),
/// the daemon serves one session over the inherited file descriptors and
/// exits. Otherwise it proceeds to the normal TCP accept loop.
///
/// # Errors
///
/// Returns a `DaemonError` if option parsing, config loading, or socket
/// binding fails.
#[cfg_attr(feature = "tracing", instrument(skip(config), name = "daemon_run"))]
pub fn run_daemon(mut config: DaemonConfig) -> Result<(), DaemonError> {
    let external_signal_flags = config.take_signal_flags();
    let pre_bound_listener = config.take_pre_bound_listener();
    let options = RuntimeOptions::parse_with_brand(
        config.arguments(),
        config.brand(),
        config.load_default_paths(),
    )?;

    apply_verbosity(options.verbosity());

    // upstream: clientserver.c:1498 - `if (is_a_socket(STDIN_FILENO))`
    // When stdin is a socket, serve a single session over stdio (inetd mode)
    // instead of binding a TCP listener.
    if is_stdin_socket() {
        return serve_inetd_session(options);
    }

    serve_connections(options, external_signal_flags, pre_bound_listener)
}

/// Seeds the thread-local [`logging::VerbosityConfig`] from the daemon's
/// `-v` / `--verbose` counter so subsequent `info_gte` / `debug_gte` checks
/// emitted from the protocol and transfer crates respect the operator's
/// requested verbosity.
///
/// upstream: options.c:2062 `set_output_verbosity(verbose, DEFAULT_PRIORITY)`
/// is invoked once after option parsing in `main.c`/`daemon-main` startup.
/// Without this seeding the daemon's `INFO_GTE`/`DEBUG_GTE` checks short-
/// circuit at level 0 regardless of how many `-v` flags were stacked on the
/// daemon command line, producing diagnostically silent daemons under
/// `-vv`/`-vvv` and breaking upstream verbosity parity (the bug surfaced
/// as a UTS testsuite divergence after PR #5887 made the daemon accept
/// `-v`/`-vv`/`-vvv` without wiring the count to log-message filtering).
pub(crate) fn apply_verbosity(level: u8) {
    logging::init(logging::VerbosityConfig::from_verbose_level(level));
}

/// Runs the daemon protocol over stdin/stdout for remote-shell daemon mode.
///
/// This implements the `--server --daemon` path where the daemon protocol is
/// served over inherited file descriptors instead of a TCP listener. Used when
/// a remote shell (e.g., SSH via `lsh.sh`) invokes the daemon, matching
/// upstream rsync's `start_daemon(STDIN_FILENO, STDOUT_FILENO)` in `main.c`.
///
/// The function loads configuration (from `--config` or default paths), builds
/// the module table, and serves a single connection on the provided
/// stdin/stdout streams. No TCP binding or signal handler registration occurs.
///
/// upstream: main.c:1843-1844 - `if (am_server && am_daemon) return
/// start_daemon(STDIN_FILENO, STDOUT_FILENO);`
///
/// # Errors
///
/// Returns a `DaemonError` if configuration loading fails or the session
/// encounters an I/O error.
pub fn run_daemon_stdio(config: DaemonConfig) -> Result<(), DaemonError> {
    let brand = config.brand();
    // upstream: clientserver.c:1275-1283 `load_config()` - when invoked as
    // `--server --daemon` (rsh-spawned, am_daemon < 0) without an explicit
    // `--config`, upstream picks `RSYNCD_USERCONF` (`./rsyncd.conf` - relative
    // to CWD) before falling back to `/etc/rsyncd.conf`. The test harness
    // (`testsuite/daemon_test.py`) drops a `rsyncd.conf` symlink in the
    // scratch dir and invokes `rsync -e lsh.sh --rsync-path=oc-rsync host::`
    // - lsh.sh stays in CWD via `--no-cd`, so the CWD lookup is what makes
    // the per-test config visible. Without this branch, the daemon parses
    // an empty config, serves an empty module listing, and the upstream
    // `daemon` test fails with "module list did not contain the expected
    // modules". Inject `--config=rsyncd.conf` ahead of parse when a usable
    // CWD config exists and the caller did not supply one; the absolute
    // brand paths take over otherwise via `parse_with_brand`.
    let arguments: Vec<OsString> = if config.load_default_paths()
        && !config_argument_present(config.arguments())
        && PathBuf::from("rsyncd.conf").is_file()
    {
        let mut augmented = Vec::with_capacity(config.arguments().len() + 1);
        let mut config_flag = OsString::from("--config=");
        config_flag.push("rsyncd.conf");
        augmented.push(config_flag);
        augmented.extend(config.arguments().iter().cloned());
        augmented
    } else {
        config.arguments().to_vec()
    };
    let options = RuntimeOptions::parse_with_brand(&arguments, brand, config.load_default_paths())?;

    apply_verbosity(options.verbosity());

    let RuntimeOptions {
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        reverse_lookup,
        ..
    } = options;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, brand)?)
    } else {
        None
    };

    // Apply Linux-only defense-in-depth startup hardenings before serving
    // the remote-shell daemon session. Mirrors the standalone and inetd
    // paths so PR_SET_NO_NEW_PRIVS and the LSM audit line apply uniformly
    // regardless of which entry point launched the daemon.
    apply_startup_hardening(log_sink.as_ref());

    let connection_limiter: Option<Arc<ConnectionLimiter>> = None;
    let modules: Vec<ModuleRuntime> = modules
        .into_iter()
        .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
        .collect();

    // LSM-CAP.5: verify required Linux capabilities are present before serving
    // the remote-shell daemon session. Same pre-flight as the standalone and
    // inetd paths so capability misconfiguration fails loud and uniformly.
    // No-op on non-Linux targets.
    if let Err(reason) = preflight_required_capabilities(&modules) {
        return Err(DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("oc-rsyncd: error: {reason}")
            )
            .with_role(Role::Daemon),
        ));
    }

    // Build a DaemonStream::Stdio from process stdin/stdout.
    let stdin = io::stdin();
    let stdout = io::stdout();
    let pair = crate::daemon_stream::StdioPair::new(Box::new(stdin), Box::new(stdout));
    let stream = DaemonStream::stdio(pair);

    // upstream: start_daemon() with stdin/stdout fds uses 127.0.0.1:0 as the
    // synthetic peer address. In remote-shell daemon mode the real peer address
    // is not available since there is no TCP socket to query.
    let peer_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    // Resolve hostname for the synthetic peer (will resolve localhost).
    // upstream: clientname.c `client_name` forward-confirms unconditionally;
    // per-module `forward lookup` governs the access-control match.
    let peer_host = if reverse_lookup {
        resolve_peer_hostname(peer_addr.ip(), true)
    } else {
        None
    };

    if let Some(log) = log_sink.as_ref() {
        log_connection(log, peer_host.as_deref(), peer_addr);
    }

    handle_legacy_session(
        stream,
        peer_addr,
        LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: bandwidth_limit,
            daemon_burst: bandwidth_burst,
            log_sink,
            peer_host,
            reverse_lookup,
        },
    )
    .map_err(|error| {
        DaemonError::new(
            SOCKET_IO_EXIT_CODE,
            rsync_error!(
                SOCKET_IO_EXIT_CODE,
                format!("stdio daemon session failed: {error}")
            )
            .with_role(Role::Daemon),
        )
    })
}

/// Runs the daemon orchestration using the hybrid tokio listener.
///
/// The configuration is parsed identically to [`run_daemon`], but the accept
/// loop is hosted on a tokio multi-thread runtime via
/// [`crate::async_listener::run_hybrid_listener`]. Each accepted connection is
/// served by the existing synchronous session handler on a dedicated OS thread
/// (see `ConnectionContext::serve_one_connection`), so the wire protocol,
/// auth, and transfer pipeline stay byte-identical with the sync daemon; only
/// the accept + task dispatch is asynchronous. The number of concurrent worker
/// threads is bounded so it never throttles below the configured
/// `max connections` (see `async_max_inflight`).
///
/// # Selection
///
/// This entrypoint is not the default. The TCP daemon dispatch selects it only
/// when the `async-daemon` cargo feature is compiled in **and** the
/// `OC_RSYNC_ASYNC_DAEMON` environment variable is set; otherwise the daemon
/// runs through [`run_daemon`]. It exists to enable the async-vs-sync daemon
/// concurrency benchmark and for downstream embedders opting in early.
///
/// # Limitation: privileged modules unsupported
///
/// Privilege drop, chroot, and setuid/setgid are **not** plumbed through the
/// async accept path. To avoid a silent security regression, this function
/// fails closed: if any module sets `uid`, `gid`, or `use chroot = true`
/// (the upstream default for `use chroot` is `true`), or a global daemon
/// `uid`/`gid`/`chroot` is configured, it returns a `DaemonError` instructing
/// the operator to use the synchronous daemon. Non-privileged modules
/// (`use chroot = false`, no `uid`/`gid`) - the benchmark case - run fine.
///
/// # Errors
///
/// Returns a `DaemonError` if option parsing, config loading, capability
/// preflight, runtime construction, or the initial socket bind fails, or when
/// a privileged module is configured (see the limitation above).
#[cfg(feature = "async-daemon")]
#[cfg_attr(docsrs, doc(cfg(feature = "async-daemon")))]
pub fn run_async_daemon(mut config: DaemonConfig) -> Result<(), DaemonError> {
    let external_signal_flags = config.take_signal_flags();
    let _ = config.take_pre_bound_listener();
    let brand = config.brand();
    let options =
        RuntimeOptions::parse_with_brand(config.arguments(), brand, config.load_default_paths())?;

    apply_verbosity(options.verbosity());

    let max_connections = options.max_connections.map(NonZeroUsize::get);
    let socket_options_str = options.socket_options().map(str::to_string);
    let RuntimeOptions {
        bind_address,
        port,
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        reverse_lookup,
        lock_file,
        proxy_protocol,
        daemon_uid,
        daemon_gid,
        daemon_chroot,
        ..
    } = options;

    // Fail closed on privileged configuration: the async accept path does not
    // perform chroot or setuid/setgid, so silently serving a privileged module
    // would be a security regression. Reject up front with a clear message.
    if daemon_uid.is_some() || daemon_gid.is_some() || daemon_chroot.is_some() {
        return Err(async_privileged_module_error());
    }

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, brand)?)
    } else {
        None
    };

    apply_startup_hardening(log_sink.as_ref());

    let connection_limiter = if let Some(path) = lock_file {
        Some(Arc::new(ConnectionLimiter::open(path)?))
    } else {
        None
    };

    let modules: Arc<Vec<ModuleRuntime>> =
        Arc::new(build_module_runtimes(modules, &connection_limiter)?);

    // Reject privileged per-module settings for the same reason as the global
    // checks above.
    for module in modules.iter() {
        if module.definition.uid.is_some()
            || module.definition.gid.is_some()
            || module.definition.use_chroot
        {
            return Err(async_privileged_module_error());
        }
    }

    // Same capability preflight the sync path runs before binding.
    if let Err(reason) = preflight_required_capabilities(&modules) {
        return Err(DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("oc-rsyncd: error: {reason}")
            )
            .with_role(Role::Daemon),
        ));
    }

    let client_socket_options: Arc<Vec<SocketOption>> =
        if let Some(ref opts_str) = socket_options_str {
            let parsed = parse_socket_options(opts_str).map_err(|msg| {
                DaemonError::new(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    rsync_error!(
                        FEATURE_UNAVAILABLE_EXIT_CODE,
                        format!("invalid socket options: {msg}")
                    )
                    .with_role(Role::Daemon),
                )
            })?;
            Arc::new(parsed)
        } else {
            Arc::new(Vec::new())
        };

    let context = ConnectionContext::new(
        modules,
        Arc::new(motd_lines),
        log_sink,
        client_socket_options,
        bandwidth_limit,
        bandwidth_burst,
        reverse_lookup,
        proxy_protocol,
    );

    let bind_addr = std::net::SocketAddr::new(bind_address, port);
    let worker_threads = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
        .min(8);

    // Reuse the existing platform signal handlers so SIGTERM/SIGINT (or the
    // Windows Service stop event injected via `signal_flags`) drains the
    // async loop the same way it drains the sync accept loop.
    let signal_flags = match external_signal_flags {
        Some(flags) => SignalFlags::from(flags),
        None => register_signal_handlers().map_err(|error| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("failed to register signal handlers: {error}")
                )
                .with_role(Role::Daemon),
            )
        })?,
    };
    let shutdown = Arc::clone(&signal_flags.shutdown);

    // Bound concurrent in-flight connections to `max_connections` in addition
    // to any per-module `ConnectionLimiter` already enforced inside the
    // session handler, mirroring the sync path's daemon-level cap. `None`
    // leaves dispatch unbounded (matching the sync default).
    let admission = max_connections.map(|limit| Arc::new(tokio::sync::Semaphore::new(limit)));

    let worker: crate::async_listener::SyncWorker = Arc::new(move |stream, peer| {
        // Enforce the daemon-level connection cap by refusing to serve past
        // the limit. Acquiring fails only when the semaphore is exhausted; on
        // exhaustion write the same `@ERROR: max connections` refusal the sync
        // path emits, then drop the connection.
        let _permit = match admission.as_ref() {
            Some(sem) => match Arc::clone(sem).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    refuse_async_connection_at_capacity(stream, peer, max_connections);
                    return Ok(());
                }
            },
            None => None,
        };
        context.serve_one_connection(stream, peer)
    });

    let max_inflight = async_max_inflight(max_connections);

    crate::async_listener::run_hybrid_listener(
        bind_addr,
        worker_threads,
        max_inflight,
        shutdown,
        worker,
    )
    .map_err(|error| {
        DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("async-daemon listener failed: {error}")
            )
            .with_role(Role::Daemon),
        )
    })
}

/// Builds the fail-closed error returned when the async daemon is asked to
/// serve a privileged (`uid`/`gid`/`use chroot`) module.
///
/// The async accept path does not perform chroot or setuid/setgid, so serving
/// such a module would silently skip the privilege drop. Refusing at startup
/// keeps the security posture explicit.
#[cfg(feature = "async-daemon")]
fn async_privileged_module_error() -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            "async-daemon does not support privileged (uid/gid/chroot) modules; \
             use the sync daemon"
                .to_owned()
        )
        .with_role(Role::Daemon),
    )
}

/// Derives the concurrent-worker-thread cap for the async accept loop.
///
/// The accept loop bounds in-flight per-connection worker threads with a
/// semaphore for flood protection. That backstop must never be the binding
/// constraint below the operator's configured `max connections`: a daemon set
/// to `max connections = 1000` should serve up to 1000 concurrent sessions,
/// not the 512-thread flood floor. So the cap is the larger of the configured
/// limit and [`DEFAULT_MAX_INFLIGHT_WORKERS`]. When `max connections` is unset
/// (unbounded, matching the sync default) the floor alone applies. Keeping the
/// cap at or above the configured limit also preserves the per-session refusal
/// semantics: the `max connections` admission semaphore inside the worker still
/// fires first, emitting the upstream `@ERROR: max connections` reply rather
/// than silently parking the excess connection.
#[cfg(feature = "async-daemon")]
pub(crate) fn async_max_inflight(max_connections: Option<usize>) -> usize {
    use crate::async_listener::DEFAULT_MAX_INFLIGHT_WORKERS;
    match max_connections {
        Some(limit) => limit.max(DEFAULT_MAX_INFLIGHT_WORKERS),
        None => DEFAULT_MAX_INFLIGHT_WORKERS,
    }
}

/// Writes the upstream-compatible max-connections refusal to an accepted
/// async socket, then lets it drop.
///
/// upstream: clientserver.c:752 - `@ERROR: max connections (%d) reached --
/// try again later\n`. Mirrors the sync accept loop's `refuse_if_at_capacity`
/// wording so clients see an identical reply regardless of accept engine.
#[cfg(feature = "async-daemon")]
fn refuse_async_connection_at_capacity(
    mut stream: TcpStream,
    _peer: SocketAddr,
    limit: Option<usize>,
) {
    let limit = limit.unwrap_or(0);
    let payload = format!(
        "{}\n",
        MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.to_string())
    );
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

include!("daemon/sections/cli_args.rs");

/// Writes a `Message` to the given [`MessageSink`].
///
/// Delegates to `sink.write(message)`, providing a uniform call site for
/// diagnostic output across daemon entry points.
pub(crate) fn write_message<W: Write>(
    message: &Message,
    sink: &mut MessageSink<W>,
) -> io::Result<()> {
    sink.write(message)
}

include!("daemon/sections/legacy_messages.rs");

include!("daemon/sections/signals.rs");

#[cfg(unix)]
include!("daemon/sections/daemonize.rs");

include!("daemon/sections/server_runtime.rs");

include!("daemon/sections/session_runtime.rs");

include!("daemon/sections/greeting.rs");

include!("daemon/sections/privilege.rs");

include!("daemon/sections/seccomp.rs");

include!("daemon/sections/capabilities.rs");

include!("daemon/sections/hardening.rs");

include!("daemon/sections/log_format.rs");

include!("daemon/sections/variable_expansion.rs");

include!("daemon/sections/name_converter.rs");

include!("daemon/sections/module_access.rs");

include!("daemon/sections/xfer_exec.rs");

include!("daemon/sections/symlink_munge.rs");

include!("daemon/sections/proxy_protocol.rs");

include!("daemon/sections/auth_helpers.rs");

include!("daemon/sections/module_parsing.rs");

include!("daemon/sections/stdio_session.rs");

include!("daemon/sections/inetd.rs");
