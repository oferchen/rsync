//! Implementation details backing [`crate::run_daemon`].
//!
//! The module hosts the listener loop, authentication helpers, and
//! connection-management utilities that were previously embedded in
//! `lib.rs`, keeping the crate root lightweight while preserving existing
//! functionality.

use dns_lookup::{lookup_addr, lookup_host};
use std::borrow::Cow;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::convert::TryFrom;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::process::{ChildStdin, Command as ProcessCommand, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use fs2::FileExt;
use tempfile::NamedTempFile;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use checksums::strong::Md5;
use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use core::{
    auth::{SUPPORTED_DAEMON_DIGESTS, verify_daemon_auth_response},
    bandwidth::{
        BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange,
        parse_bandwidth_limit,
    },
    branding::{self, Brand, manifest},
    fallback::{
        CLIENT_FALLBACK_ENV, DAEMON_FALLBACK_ENV, describe_missing_fallback_binary,
        fallback_binary_available, fallback_disabled_reason,
    },
    message::{Message, Role},
    rsync_error, rsync_info, rsync_warning,
    server::{HandshakeResult, ServerConfig, ServerRole, run_server_with_handshake},
};
use logging_sink::MessageSink;
use protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, MessageCode, MessageFrame, ProtocolVersion,
    format_legacy_daemon_message, parse_legacy_daemon_message,
};

use crate::{config::DaemonConfig, error::DaemonError, systemd};

mod help;
pub(crate) mod tracing_stream;

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

pub(crate) const BRANDED_CONFIG_ENV: &str = "OC_RSYNC_CONFIG";
pub(crate) const LEGACY_CONFIG_ENV: &str = "RSYNCD_CONFIG";
pub(crate) const BRANDED_SECRETS_ENV: &str = "OC_RSYNC_SECRETS";
pub(crate) const LEGACY_SECRETS_ENV: &str = "RSYNCD_SECRETS";
/// Timeout applied to accepted sockets to avoid hanging handshakes.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Error payload returned to clients while daemon functionality is incomplete.
const HANDSHAKE_ERROR_PAYLOAD: &str = "@ERROR: daemon functionality is unavailable in this build";
/// Error payload returned when a configured module is requested but file serving is unavailable.
#[allow(dead_code)]
const MODULE_UNAVAILABLE_PAYLOAD: &str =
    "@ERROR: module '{module}' transfers are not yet implemented in this build";
const ACCESS_DENIED_PAYLOAD: &str = "@ERROR: access denied to module '{module}' from {addr}";
/// Error payload returned when a requested module does not exist.
const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";
/// Error payload returned when a module reaches its connection cap.
const MODULE_MAX_CONNECTIONS_PAYLOAD: &str =
    "@ERROR: max connections ({limit}) reached -- try again later";
/// Error payload returned when updating the connection lock file fails.
const MODULE_LOCK_ERROR_PAYLOAD: &str =
    "@ERROR: failed to update module connection lock; please try again later";
// Deterministic help text describing the currently supported daemon surface.
//
// The snapshot adjusts the banner, usage line, and default configuration path
// to reflect the supplied [`Brand`], ensuring invocations via compatibility
// symlinks and the canonical single `oc-rsync` binary emit brand-appropriate help
// output.

include!("daemon/module_state.rs");

type SharedLogSink = Arc<Mutex<MessageSink<std::fs::File>>>;

include!("daemon/runtime_options.rs");

include!("daemon/sections/config_paths.rs");

include!("daemon/sections/config_parsing.rs");

include!("daemon/sections/module_definition.rs");

include!("daemon/sections/config_helpers.rs");

/// Runs the daemon orchestration using the provided configuration.
///
/// The helper binds a TCP listener (defaulting to `0.0.0.0:873`), accepts a
/// single connection, performs the legacy ASCII handshake, and replies with a
/// deterministic `@ERROR` message explaining that module serving is not yet
/// available. This behaviour gives higher layers a concrete negotiation target
/// while keeping the observable output stable.
pub fn run_daemon(config: DaemonConfig) -> Result<(), DaemonError> {
    let options = RuntimeOptions::parse_with_brand(
        config.arguments(),
        config.brand(),
        config.load_default_paths(),
    )?;
    serve_connections(options)
}

include!("daemon/sections/cli_args.rs");

pub(crate) fn write_message<W: Write>(
    message: &Message,
    sink: &mut MessageSink<W>,
) -> io::Result<()> {
    sink.write(message)
}

include!("daemon/sections/legacy_messages.rs");

include!("daemon/sections/server_runtime.rs");

include!("daemon/sections/session_runtime.rs");

include!("daemon/sections/delegation.rs");

include!("daemon/sections/module_access.rs");

include!("daemon/sections/auth_helpers.rs");

include!("daemon/sections/module_parsing.rs");
