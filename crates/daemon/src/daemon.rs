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
    Arc, Mutex,
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
use core::client::SkipCompressList;
use core::{
    auth::{digests_for_protocol, verify_daemon_auth_response},
    bandwidth::{
        BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange,
        parse_bandwidth_limit,
    },
    branding::{self, Brand, manifest},
    message::{Message, Role},
    rsync_error, rsync_info, rsync_warning,
    server::{
        HandshakeResult, ReferenceDirectory, ReferenceDirectoryKind, ServerConfig, ServerRole,
        run_server_with_handshake,
    },
};
use logging_sink::MessageSink;
use protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, MessageCode, MessageFrame, ProtocolVersion,
    filters::FilterRuleWireFormat, format_legacy_daemon_message, iconv::FilenameConverter,
    parse_legacy_daemon_message,
};

use crate::{config::DaemonConfig, error::DaemonError, systemd};

mod help;
pub(crate) mod tracing_stream;

#[cfg(feature = "concurrent-sessions")]
pub mod session_registry;

#[cfg(feature = "concurrent-sessions")]
pub mod connection_pool;

#[cfg(all(test, feature = "concurrent-sessions"))]
mod concurrent_tests;

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub mod async_session;

use self::help::help_text;

/// Exit code used when daemon functionality is unavailable.
pub(crate) const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
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
const HANDSHAKE_ERROR_PAYLOAD: &str = "@ERROR: daemon functionality is unavailable in this build";
/// Error payload sent when a host is denied access to a module.
///
/// upstream: clientserver.c:733 — `@ERROR: access denied to %s from %s (%s)\n`
/// where args are (name, host, addr).
const ACCESS_DENIED_PAYLOAD: &str = "@ERROR: access denied to {module} from {host} ({addr})";
/// Error payload sent when authentication fails on a protected module.
///
/// upstream: clientserver.c:762 — `@ERROR: auth failed on module %s\n`
const AUTH_FAILED_PAYLOAD: &str = "@ERROR: auth failed on module {module}";
/// Error payload returned when a requested module does not exist.
const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";
/// Error payload returned when a module reaches its connection cap.
const MODULE_MAX_CONNECTIONS_PAYLOAD: &str =
    "@ERROR: max connections ({limit}) reached -- try again later";
/// Error payload returned when updating the connection lock file fails.
const MODULE_LOCK_ERROR_PAYLOAD: &str =
    "@ERROR: failed to update module connection lock; please try again later";
mod module_state;
#[cfg(test)]
use self::module_state::TEST_CONFIG_CANDIDATES;
use self::module_state::resolve_peer_hostname;
pub(crate) use self::module_state::{
    AuthUser, ConnectionLimiter, ModuleConnectionError, ModuleDefinition, ModuleRuntime,
    UserAccessLevel, module_peer_hostname,
};
#[cfg(test)]
pub(crate) use self::module_state::{
    TEST_SECRETS_CANDIDATES, TEST_SECRETS_ENV, TestSecretsEnvOverride,
    clear_test_hostname_overrides, set_test_hostname_override,
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
/// `rsyncd.conf`, binds a TCP listener (defaulting to `0.0.0.0:873`), and
/// enters the connection accept loop. Each accepted connection is handled in
/// a dedicated thread with `catch_unwind` crash isolation.
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
    serve_connections(options, external_signal_flags, pre_bound_listener)
}

include!("daemon/sections/cli_args.rs");

/// Writes a [`Message`] to the given [`MessageSink`].
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

include!("daemon/sections/log_format.rs");

include!("daemon/sections/variable_expansion.rs");

include!("daemon/sections/name_converter.rs");

include!("daemon/sections/module_access.rs");

include!("daemon/sections/xfer_exec.rs");

include!("daemon/sections/symlink_munge.rs");

include!("daemon/sections/proxy_protocol.rs");

include!("daemon/sections/auth_helpers.rs");

include!("daemon/sections/module_parsing.rs");
