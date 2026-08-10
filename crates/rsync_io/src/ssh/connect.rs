//! Structured connection primitive for the SSH transport.
//!
//! This module exposes a tagged-config entry point,
//! [`SshConnection::connect_with_config`], that wraps the underlying
//! [`SshCommand`](super::SshCommand) builder with explicit, named
//! [`SshConnectConfig`] and [`KeepAliveConfig`] structs. The intent is to
//! decouple call sites in higher layers (`core::client::remote`) from the
//! builder's setter sequence, so the SSH transport chain (#1795 -> #1796 ->
//! #1797 -> #1805 -> #1806) can compose connect, auth, and session-setup
//! steps from a single config value.
//!
//! # Scope
//!
//! This entry point is the synchronous primitive. It spawns the system
//! `ssh` binary via [`SshCommand::spawn`] and returns the existing
//! [`SshConnection`] type for back-compat. The async variant
//! ([`super::AsyncSshTransport::execute_remote_rsync`], task #1796) is
//! gated behind the `--features async-ssh` cargo feature so default
//! builds remain free of any tokio dependency in the SSH transport path.
//!
//! # Example
//!
//! ```no_run
//! use rsync_io::ssh::{KeepAliveConfig, SshConnectConfig, SshConnection};
//! use std::time::Duration;
//!
//! let config = SshConnectConfig::new()
//!     .with_connect_timeout(Some(Duration::from_secs(10)))
//!     .with_keepalive(Some(KeepAliveConfig {
//!         interval: Duration::from_secs(30),
//!         max_failures: 5,
//!     }));
//!
//! let connection = SshConnection::connect_with_config("backup@host.example", &config)
//!     .expect("spawn ssh");
//! drop(connection);
//! ```
//!
//! # Defaults
//!
//! [`SshConnectConfig::new`] mirrors the historical defaults baked into
//! [`SshCommand::new`]: a 30 second connect timeout and a 20 second
//! keepalive interval with three allowed failures. Setting either field to
//! `None` disables that injection.

use std::ffi::OsString;
use std::io;
use std::time::Duration;

use super::builder::SshCommand;
use super::connection::SshConnection;

/// Default SSH keepalive interval, matching upstream OpenSSH conventions
/// and the [`SshCommand`] builder's historical default.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Default keepalive failure budget before SSH terminates the connection.
pub const DEFAULT_KEEPALIVE_MAX_FAILURES: u32 = 3;

/// Default establishment timeout for the SSH TCP handshake.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Keepalive injection parameters for an SSH connection.
///
/// When supplied via [`SshConnectConfig::keepalive`], the values are
/// rendered as `-o ServerAliveInterval=<interval>` and
/// `-o ServerAliveCountMax=<max_failures>` on the spawned `ssh` command
/// line. `interval` is rounded up to whole seconds; sub-second values are
/// not representable in the OpenSSH option grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeepAliveConfig {
    /// Number of seconds the SSH client waits between keepalive probes
    /// sent through the encrypted channel.
    pub interval: Duration,
    /// Number of consecutive missed keepalive responses tolerated before
    /// SSH terminates the connection.
    pub max_failures: u32,
}

impl Default for KeepAliveConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_KEEPALIVE_INTERVAL,
            max_failures: DEFAULT_KEEPALIVE_MAX_FAILURES,
        }
    }
}

/// Structured configuration consumed by
/// [`SshConnection::connect_with_config`].
///
/// Field semantics:
///
/// - `connect_timeout` - when `Some`, injected as `-o ConnectTimeout=N` and
///   arms the in-process connect watchdog that kills the child on timeout
///   (see [`SshConnection::cancel_connect_watchdog`]). When `None`, neither
///   the SSH option nor the watchdog is installed and SSH falls back to the
///   OS TCP timeout.
/// - `keepalive` - when `Some`, injects `-o ServerAliveInterval` and
///   `-o ServerAliveCountMax`. When `None`, no keepalive options are
///   injected.
/// - `remote_command` - argv tokens appended after the destination operand
///   and forwarded verbatim to the spawned SSH child. Typically the remote
///   rsync invocation (`rsync --server ...`).
#[derive(Clone, Debug)]
pub struct SshConnectConfig {
    /// Connection establishment timeout. `None` disables the
    /// `-o ConnectTimeout` injection and the in-process watchdog.
    pub connect_timeout: Option<Duration>,
    /// Keepalive parameters. `None` disables `-o ServerAliveInterval` and
    /// `-o ServerAliveCountMax` injection.
    pub keepalive: Option<KeepAliveConfig>,
    /// Remote command argv appended after the destination operand.
    pub remote_command: Vec<OsString>,
}

impl Default for SshConnectConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            keepalive: Some(KeepAliveConfig::default()),
            remote_command: Vec::new(),
        }
    }
}

impl SshConnectConfig {
    /// Returns the default configuration: 30 second connect timeout, 20
    /// second keepalive interval with three allowed failures, and no
    /// remote command.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style setter for [`SshConnectConfig::connect_timeout`].
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Builder-style setter for [`SshConnectConfig::keepalive`].
    #[must_use]
    pub const fn with_keepalive(mut self, keepalive: Option<KeepAliveConfig>) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Replaces the remote command argv.
    #[must_use]
    pub fn with_remote_command<I, S>(mut self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.remote_command = argv.into_iter().map(Into::into).collect();
        self
    }
}

impl SshConnection {
    /// Spawns an SSH subprocess using a structured [`SshConnectConfig`].
    ///
    /// The `remote` operand follows the same conventions as the system
    /// `ssh` client: an optional `user@` prefix followed by a host name,
    /// IPv4 dotted-quad, or bracketed IPv6 literal. The host portion is
    /// extracted and forwarded to the underlying [`SshCommand`] builder
    /// along with the user, if present.
    ///
    /// This is the synchronous primitive for task #1795. It returns the
    /// existing [`SshConnection`] type so callers can transition without
    /// touching their downstream read/write paths. The async variant
    /// (`super::AsyncSshTransport::execute_remote_rsync`, task #1796)
    /// is gated behind the `--features async-ssh` cargo feature.
    ///
    /// # Errors
    ///
    /// Returns any [`io::Error`] surfaced by [`SshCommand::spawn`],
    /// typically `NotFound` when the `ssh` binary is missing or
    /// `PermissionDenied` when the process is sandboxed away from
    /// `execve`. Connection establishment failures past `spawn` surface
    /// later via the connect watchdog (see
    /// [`SshConnection::cancel_connect_watchdog`]).
    pub fn connect_with_config(remote: &str, config: &SshConnectConfig) -> io::Result<Self> {
        build_ssh_command(remote, config).spawn()
    }
}

/// Translates a [`SshConnectConfig`] plus `[user@]host` operand into a
/// fully configured [`SshCommand`].
///
/// Visible to the `tests` submodule via `pub(super)` so unit tests can
/// inspect the rendered argv without actually spawning a subprocess.
pub(super) fn build_ssh_command(remote: &str, config: &SshConnectConfig) -> SshCommand {
    let (user, host) = split_user_host(remote);
    let mut command = SshCommand::new(host);
    if let Some(user) = user {
        command.set_user(user);
    }

    command.set_connect_timeout(config.connect_timeout);

    if let Some(keepalive) = config.keepalive {
        let interval_secs = duration_to_secs_ceil(keepalive.interval);
        command.set_keepalive(false);
        command.push_option(OsString::from(format!(
            "-oServerAliveInterval={interval_secs}"
        )));
        command.push_option(OsString::from(format!(
            "-oServerAliveCountMax={}",
            keepalive.max_failures
        )));
    } else {
        command.set_keepalive(false);
    }

    if !config.remote_command.is_empty() {
        command.set_remote_command(config.remote_command.iter().cloned());
    }

    command
}

/// Splits a `user@host` operand into its components.
///
/// IPv6 literals can carry `@` characters inside zone identifiers
/// (`fe80::1%eth0`) but the standard SSH convention prohibits `@` in the
/// host portion, so `rsplit_once('@')` correctly identifies the user/host
/// boundary. When no `@` is present the entire operand is treated as the
/// host name.
fn split_user_host(remote: &str) -> (Option<&str>, &str) {
    match remote.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => (Some(user), host),
        _ => (None, remote),
    }
}

/// Rounds a [`Duration`] up to the nearest whole second.
///
/// OpenSSH's `ServerAliveInterval` and `ConnectTimeout` options accept
/// integer seconds only. Rounding up matches the existing
/// `SshCommand::connect_timeout_seconds` rounding policy so the two code
/// paths render identical argv for equivalent inputs.
fn duration_to_secs_ceil(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_to_strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn default_config_matches_builder_defaults() {
        let config = SshConnectConfig::new();
        assert_eq!(config.connect_timeout, Some(DEFAULT_CONNECT_TIMEOUT));
        let keepalive = config.keepalive.expect("default keepalive present");
        assert_eq!(keepalive.interval, DEFAULT_KEEPALIVE_INTERVAL);
        assert_eq!(keepalive.max_failures, DEFAULT_KEEPALIVE_MAX_FAILURES);
        assert!(config.remote_command.is_empty());
    }

    #[test]
    fn default_keepalive_matches_constants() {
        let keepalive = KeepAliveConfig::default();
        assert_eq!(keepalive.interval, DEFAULT_KEEPALIVE_INTERVAL);
        assert_eq!(keepalive.max_failures, DEFAULT_KEEPALIVE_MAX_FAILURES);
    }

    #[test]
    fn builder_setters_round_trip() {
        let keepalive = KeepAliveConfig {
            interval: Duration::from_secs(45),
            max_failures: 7,
        };
        let config = SshConnectConfig::new()
            .with_connect_timeout(Some(Duration::from_secs(12)))
            .with_keepalive(Some(keepalive))
            .with_remote_command(["rsync", "--server", "."]);

        assert_eq!(config.connect_timeout, Some(Duration::from_secs(12)));
        assert_eq!(config.keepalive, Some(keepalive));
        assert_eq!(config.remote_command.len(), 3);
        assert_eq!(config.remote_command[0], OsString::from("rsync"));
    }

    #[test]
    fn build_command_emits_connect_timeout_and_keepalive() {
        let config = SshConnectConfig::new();
        let (_, args) = build_ssh_command("user@example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            rendered.contains(&"-oConnectTimeout=30".to_owned()),
            "expected ConnectTimeout=30 in {rendered:?}"
        );
        assert!(
            rendered.contains(&"-oServerAliveInterval=20".to_owned()),
            "expected ServerAliveInterval=20 in {rendered:?}"
        );
        assert!(
            rendered.contains(&"-oServerAliveCountMax=3".to_owned()),
            "expected ServerAliveCountMax=3 in {rendered:?}"
        );
        assert!(
            rendered.contains(&"user@example.com".to_owned()),
            "user@host operand should be rendered: {rendered:?}"
        );
    }

    #[test]
    fn build_command_honours_custom_keepalive_values() {
        let config = SshConnectConfig::new().with_keepalive(Some(KeepAliveConfig {
            interval: Duration::from_secs(90),
            max_failures: 6,
        }));
        let (_, args) = build_ssh_command("example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            rendered.contains(&"-oServerAliveInterval=90".to_owned()),
            "expected ServerAliveInterval=90 in {rendered:?}"
        );
        assert!(
            rendered.contains(&"-oServerAliveCountMax=6".to_owned()),
            "expected ServerAliveCountMax=6 in {rendered:?}"
        );
        let interval_hits = rendered
            .iter()
            .filter(|a| a.contains("ServerAliveInterval"))
            .count();
        assert_eq!(
            interval_hits, 1,
            "default keepalive must not be injected alongside the custom one: {rendered:?}"
        );
    }

    #[test]
    fn build_command_skips_keepalive_when_disabled() {
        let config = SshConnectConfig::new().with_keepalive(None);
        let (_, args) = build_ssh_command("example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            !rendered.iter().any(|a| a.contains("ServerAliveInterval")),
            "keepalive interval must not be injected: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|a| a.contains("ServerAliveCountMax")),
            "keepalive count must not be injected: {rendered:?}"
        );
    }

    #[test]
    fn build_command_skips_connect_timeout_when_disabled() {
        let config = SshConnectConfig::new().with_connect_timeout(None);
        let (_, args) = build_ssh_command("example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            !rendered.iter().any(|a| a.contains("ConnectTimeout")),
            "ConnectTimeout must not be injected: {rendered:?}"
        );
    }

    #[test]
    fn build_command_rounds_subsecond_keepalive_interval_up() {
        let config = SshConnectConfig::new().with_keepalive(Some(KeepAliveConfig {
            interval: Duration::from_millis(15_500),
            max_failures: 3,
        }));
        let (_, args) = build_ssh_command("example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            rendered.contains(&"-oServerAliveInterval=16".to_owned()),
            "15.5s should round up to 16: {rendered:?}"
        );
    }

    #[test]
    fn build_command_renders_remote_command_after_host() {
        let config = SshConnectConfig::new().with_remote_command(["rsync", "--server", "."]);
        let (_, args) = build_ssh_command("example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        let host_idx = rendered
            .iter()
            .position(|a| a == "example.com")
            .expect("host operand present");
        let rsync_idx = rendered
            .iter()
            .position(|a| a == "rsync")
            .expect("remote command present");
        assert!(
            host_idx < rsync_idx,
            "remote argv must follow the destination operand: {rendered:?}"
        );
    }

    #[test]
    fn host_without_user_is_passed_through() {
        let config = SshConnectConfig::new();
        let (_, args) = build_ssh_command("plain.example", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            rendered.contains(&"plain.example".to_owned()),
            "bare host should render unchanged: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|a| a.contains("@plain.example")),
            "no user prefix when none is supplied: {rendered:?}"
        );
    }

    #[test]
    fn empty_user_segment_is_treated_as_host_only() {
        let config = SshConnectConfig::new();
        let (_, args) = build_ssh_command("@example.com", &config).command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert!(
            rendered.contains(&"@example.com".to_owned()),
            "an empty user prefix should be left intact: {rendered:?}"
        );
    }

    /// Network-touching smoke test: connect to a deliberately unreachable
    /// address with a tight watchdog and confirm we surface an
    /// [`io::ErrorKind::TimedOut`] within ~2x the configured timeout.
    ///
    /// Gated behind `OC_RSYNC_SSH_NET=1` because it requires (a) a working
    /// `ssh` binary on `PATH` and (b) the freedom to issue an outbound
    /// connection attempt. CI runners with locked-down networking would
    /// otherwise spuriously fail.
    #[cfg(unix)]
    #[test]
    fn connect_with_short_timeout_returns_io_error() {
        if std::env::var_os("OC_RSYNC_SSH_NET").is_none() {
            return;
        }
        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }

        let timeout = Duration::from_secs(1);
        let config = SshConnectConfig::new()
            .with_connect_timeout(Some(timeout))
            .with_remote_command(["true"]);

        // RFC 5737 TEST-NET-1 - guaranteed unroutable for documentation use.
        let start = std::time::Instant::now();
        let mut connection = SshConnection::connect_with_config("nobody@192.0.2.1", &config)
            .expect("spawn ssh subprocess");

        // Drive the watchdog: read on the connection to surface TimedOut.
        let mut buf = [0u8; 1];
        let read_result = std::io::Read::read(&mut connection, &mut buf);
        let elapsed = start.elapsed();

        assert!(
            elapsed < timeout * 4,
            "watchdog should fire within ~2x timeout, took {elapsed:?}"
        );
        match read_result {
            Err(err) => {
                assert!(
                    matches!(
                        err.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::UnexpectedEof
                    ),
                    "expected timeout-class error, got {err:?}"
                );
            }
            Ok(0) => {
                // EOF is acceptable: the watchdog killed the child before
                // any bytes were produced.
            }
            Ok(n) => panic!("unexpected {n} bytes read from unreachable host"),
        }
    }
}
