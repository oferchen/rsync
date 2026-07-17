//! Daemon module list retrieval and parsing.
//!
//! Connects to an rsync daemon, negotiates the `@RSYNCD:` greeting, and
//! retrieves the advertised module table. This mirrors the protocol exchange
//! in upstream `clientserver.c:start_inband_exchange()` when no module is
//! selected (empty module name triggers listing mode).
//!
//! # Upstream Reference
//!
//! - `clientserver.c:start_inband_exchange()` - Module listing handshake
//! - `authenticate.c` - Challenge/response authentication

use std::io::{BufReader, Write};
use std::time::Duration;

use protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonGreetingOwned, LegacyDaemonMessage, missing_greeting_token,
    parse_legacy_daemon_message, parse_legacy_warning_message,
};
use rsync_io::negotiate_legacy_daemon_session;

use super::super::{
    CLIENT_SERVER_PROTOCOL_EXIT_CODE, ClientError, DAEMON_SOCKET_TIMEOUT,
    PARTIAL_TRANSFER_EXIT_CODE, TransferTimeout, daemon_access_denied_error,
    daemon_authentication_failed_error, daemon_authentication_required_error, daemon_error,
    daemon_listing_unavailable_error, daemon_protocol_error, socket_error,
};
use super::auth::{
    DaemonAuthContext, SensitiveBytes, is_motd_payload, load_daemon_password,
    normalize_motd_payload, send_daemon_auth_credentials,
};
use super::connect::{
    RshDaemonSpawn, open_daemon_stream, resolve_connect_timeout, spawn_rsh_daemon_stream,
};
use super::errors::{legacy_daemon_error_payload, map_daemon_handshake_error, read_trimmed_line};
use super::request::ModuleListOptions;
use super::request::ModuleListRequest;
use super::socket_options::apply_socket_options;
use super::types::DaemonAddress;
use crate::auth::{parse_daemon_digest_list, select_daemon_digest};

/// Collection of daemon modules together with MOTD, warnings, and capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleList {
    motd: Vec<String>,
    warnings: Vec<String>,
    capabilities: Vec<String>,
    entries: Vec<ModuleListEntry>,
}

impl ModuleList {
    const fn new(
        motd: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<String>,
        entries: Vec<ModuleListEntry>,
    ) -> Self {
        Self {
            motd,
            warnings,
            capabilities,
            entries,
        }
    }

    /// Returns the advertised module entries.
    #[must_use]
    pub fn entries(&self) -> &[ModuleListEntry] {
        &self.entries
    }

    /// Returns the optional message-of-the-day lines emitted by the daemon.
    #[must_use]
    pub fn motd_lines(&self) -> &[String] {
        &self.motd
    }

    /// Returns the warning messages emitted by the daemon while processing the request.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Returns the capability strings advertised by the daemon via `@RSYNCD: CAP` lines.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

/// Entry describing a single daemon module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListEntry {
    name: String,
    comment: Option<String>,
}

impl ModuleListEntry {
    fn from_line(line: &str) -> Self {
        match line.split_once('\t') {
            Some((name, comment)) => Self {
                name: name.to_owned(),
                comment: if comment.is_empty() {
                    None
                } else {
                    Some(comment.to_owned())
                },
            },
            None => Self {
                name: line.to_owned(),
                comment: None,
            },
        }
    }

    /// Returns the module name advertised by the daemon.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional comment associated with the module.
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// Performs a daemon module listing by connecting to the supplied address.
///
/// The helper honours the `RSYNC_PROXY` environment variable, establishing an
/// HTTP `CONNECT` tunnel through the specified proxy before negotiating with
/// the daemon when the variable is set. This mirrors the behaviour of
/// upstream rsync.
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

/// Performs a daemon module listing using caller-provided options.
///
/// This variant mirrors [`run_module_list`] while allowing callers to configure
/// behaviours such as suppressing daemon MOTD lines when `--no-motd` is supplied.
pub fn run_module_list_with_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        options,
        None,
        TransferTimeout::Default,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing using an optional password override.
///
/// When `password_override` is `Some`, the bytes are used for authentication
/// instead of loading `RSYNC_PASSWORD`. This mirrors `--password-file` in the
/// CLI and simplifies testing by avoiding environment manipulation.
pub fn run_module_list_with_password(
    request: ModuleListRequest,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        ModuleListOptions::default(),
        password_override,
        timeout,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing with the supplied options and password override.
///
/// The helper is primarily used by the CLI to honour flags such as `--no-motd`
/// while still exercising the optional password override path used for
/// `--password-file`. The [`ModuleListOptions`] parameter defaults to the same
/// behaviour as [`run_module_list`].
pub fn run_module_list_with_password_and_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes = password_override.map(SensitiveBytes::new);
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;
    let suppress_motd = options.suppresses_motd();
    let address_mode = options.address_mode();

    let effective_timeout = effective_timeout(timeout, DAEMON_SOCKET_TIMEOUT);
    let connect_duration = resolve_connect_timeout(connect_timeout);

    // Precedence mirrors upstream `main.c`: an explicit `-e`/`--rsh` for a
    // `host::` listing reaches the daemon through the remote shell
    // (daemon-over-rsh); otherwise RSYNC_CONNECT_PROG, otherwise plain TCP.
    let mut stream = if let Some(shell_args) = options.remote_shell() {
        // upstream: main.c:594-604 + main.c:1571-1586 - spawn the remote shell
        // with `rsync --server --daemon .` and speak the `@RSYNCD:` listing
        // handshake over its pipes instead of opening a TCP socket. Matches
        // `clientserver.c:start_inband_exchange` carried over the shell.
        spawn_rsh_daemon_stream(RshDaemonSpawn {
            shell_args,
            host: addr.host(),
            username: username.as_deref(),
            port: addr.port(),
            rsync_path: options.rsync_path(),
            bind_address: options.bind_address().map(|addr| addr.ip()),
            jump_hosts: None,
            connect_timeout: connect_duration,
            address_mode,
        })?
    } else {
        open_daemon_stream(
            addr,
            connect_duration,
            effective_timeout,
            address_mode,
            options.connect_program(),
            options.bind_address(),
            options.tcp_fastopen(),
        )?
    };

    configure_daemon_stream(&mut stream, &options, addr)?;

    let handshake = negotiate_legacy_daemon_session(stream, request.protocol())
        .map_err(|error| map_daemon_handshake_error(error, addr))?;
    let negotiated_protocol = handshake.negotiated_protocol();
    let server_greeting = handshake.server_greeting().clone();
    reject_incomplete_daemon_greeting(&server_greeting)?;
    let advertised_digests = parse_daemon_digest_list(server_greeting.digest_list());
    let selected_digest = select_daemon_digest(&advertised_digests, negotiated_protocol.as_u8());
    let mut reader = BufReader::new(handshake.into_stream());

    reader
        .get_mut()
        .write_all(b"#list\n")
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    let mut entries = Vec::new();
    let mut motd = Vec::new();
    let mut warnings = Vec::new();
    let mut capabilities = Vec::new();
    let mut acknowledged = false;
    let mut pre_ack_messages = Vec::new();

    // TCP_QUICKACK is one-shot; re-arm before each read so the client's ACKs
    // to the daemon's module-listing lines stay immediate across the exchange.
    fast_io::rearm_tcp_quickack(reader.get_ref().inner().as_tcp_stream());
    while let Some(line) = read_trimmed_line(&mut reader)
        .map_err(|error| socket_error("read from", addr.socket_addr_display(), error))?
    {
        fast_io::rearm_tcp_quickack(reader.get_ref().inner().as_tcp_stream());
        if let Some(payload) = legacy_daemon_error_payload(&line) {
            return Err(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        if let Some(payload) = parse_legacy_warning_message(&line) {
            warnings.push(payload.to_owned());
            continue;
        }

        if !acknowledged && !line.starts_with(LEGACY_DAEMON_PREFIX) {
            pre_ack_messages.push(line.clone());
            if !suppress_motd {
                motd.push(line);
            }
            continue;
        }

        if line.starts_with(LEGACY_DAEMON_PREFIX) {
            match parse_legacy_daemon_message(&line) {
                Ok(LegacyDaemonMessage::Ok) => {
                    acknowledged = true;
                    pre_ack_messages.clear();
                    continue;
                }
                Ok(LegacyDaemonMessage::Exit) => {
                    // upstream: clientserver.c - the daemon sends @RSYNCD: EXIT
                    // without a preceding @RSYNCD: OK for module listings.
                    // Lines collected in pre_ack_messages are the module entries.
                    if !acknowledged {
                        for msg in pre_ack_messages.drain(..) {
                            entries.push(ModuleListEntry::from_line(&msg));
                        }
                        acknowledged = true;
                        // Clear motd of module-entry lines (those containing tabs).
                        motd.retain(|line| !line.contains('\t'));
                    }
                    break;
                }
                Ok(LegacyDaemonMessage::Capabilities { flags }) => {
                    capabilities.push(flags.to_owned());
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthRequired { module }) => {
                    if auth_attempted {
                        return Err(daemon_protocol_error(
                            "daemon repeated authentication challenge",
                        ));
                    }

                    let username = username.as_deref().ok_or_else(|| {
                        daemon_authentication_required_error(
                            "supply a username in the daemon URL (e.g. rsync://user@host/)",
                        )
                    })?;

                    let secret = if let Some(secret) = password_bytes.as_ref() {
                        secret.to_vec()
                    } else {
                        password_bytes = load_daemon_password().map(SensitiveBytes::new);
                        password_bytes
                            .as_ref()
                            .map(SensitiveBytes::to_vec)
                            .ok_or_else(|| {
                                daemon_authentication_required_error(
                                    "set RSYNC_PASSWORD before contacting authenticated daemons",
                                )
                            })?
                    };

                    let context =
                        DaemonAuthContext::new(username.to_owned(), secret, selected_digest);
                    if let Some(challenge) = module {
                        send_daemon_auth_credentials(&mut reader, &context, challenge, addr)?;
                    }

                    auth_context = Some(context);
                    auth_attempted = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthChallenge { challenge }) => {
                    let context = auth_context.as_ref().ok_or_else(|| {
                        daemon_protocol_error(
                            "daemon issued authentication challenge before requesting credentials",
                        )
                    })?;

                    send_daemon_auth_credentials(&mut reader, context, challenge, addr)?;
                    continue;
                }
                Ok(LegacyDaemonMessage::Other(payload)) => {
                    if let Some(reason) = payload.strip_prefix("DENIED") {
                        return Err(daemon_access_denied_error(reason.trim()));
                    }

                    if let Some(reason) = payload.strip_prefix("AUTHFAILED") {
                        let reason = reason.trim();
                        return Err(daemon_authentication_failed_error(if reason.is_empty() {
                            None
                        } else {
                            Some(reason)
                        }));
                    }

                    if is_motd_payload(payload) {
                        if !suppress_motd {
                            motd.push(normalize_motd_payload(payload));
                        }
                        continue;
                    }

                    if !acknowledged {
                        pre_ack_messages.push(payload.to_owned());
                        continue;
                    }

                    return Err(daemon_protocol_error(&line));
                }
                Ok(LegacyDaemonMessage::Version(_)) => {
                    return Err(daemon_protocol_error(&line));
                }
                Err(_) => {
                    return Err(daemon_protocol_error(&line));
                }
            }
        }

        if !acknowledged {
            return Err(daemon_protocol_error(&line));
        }

        entries.push(ModuleListEntry::from_line(&line));
    }

    if !acknowledged {
        if !pre_ack_messages.is_empty() {
            let mut detail = pre_ack_messages.join("\n");
            if detail.is_empty() {
                detail = String::from("daemon closed connection before acknowledging module list");
            }
            return Err(daemon_listing_unavailable_error(&detail));
        }

        return Err(daemon_protocol_error(
            "daemon did not acknowledge module listing",
        ));
    }

    Ok(ModuleList::new(motd, warnings, capabilities, entries))
}

/// Enforces upstream's greeting-completeness gate on the daemon's banner.
///
/// upstream: clientserver.c:188-210 `exchange_protocols()` (am_client == 1) -
/// after parsing the `@RSYNCD:` greeting the client rejects a banner that omits
/// the subprotocol value (`remote_protocol >= 30`) or the digest name list
/// (`remote_protocol > 31`), printing `rsync: the server omitted the <token>:
/// <buf>` and aborting with `RERR_STARTCLIENT`. Module listing reaches this gate
/// through the same `start_inband_exchange()` path as a transfer (an empty
/// module name only changes what follows the handshake), so the listing client
/// applies the identical shared [`missing_greeting_token`] check that #6609
/// wired into the transfer handshake. The gate lives here at the application
/// layer, not in the lenient negotiation parser, so protocol-clamping paths that
/// deliberately accept digest-less banners to exercise version capping stay
/// intact.
fn reject_incomplete_daemon_greeting(
    greeting: &LegacyDaemonGreetingOwned,
) -> Result<(), ClientError> {
    let banner = reconstruct_daemon_greeting_line(greeting);
    if let Some(missing) = missing_greeting_token(&banner) {
        return Err(daemon_error(
            format!(
                "the server omitted the {}: {}",
                missing.description(),
                banner
            ),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        ));
    }
    Ok(())
}

/// Rebuilds the `@RSYNCD:` banner line from the parsed greeting metadata.
///
/// The negotiation helper consumes the raw greeting bytes while replying, so the
/// listing client reconstructs the canonical banner from the retained advertised
/// protocol, optional subprotocol suffix, and optional digest list. The gate in
/// [`missing_greeting_token`] depends only on those three fields, so the
/// reconstruction preserves the decision exactly and reproduces the banner real
/// daemons emit for the `the server omitted the <token>: <buf>` diagnostic.
fn reconstruct_daemon_greeting_line(greeting: &LegacyDaemonGreetingOwned) -> String {
    let mut banner = format!("{LEGACY_DAEMON_PREFIX} {}", greeting.advertised_protocol());
    if let Some(subprotocol) = greeting.subprotocol_raw() {
        banner.push('.');
        banner.push_str(&subprotocol.to_string());
    }
    if let Some(digests) = greeting.digest_list() {
        banner.push(' ');
        banner.push_str(digests);
    }
    banner
}

fn configure_daemon_stream(
    stream: &mut super::connect::DaemonStream,
    options: &ModuleListOptions,
    addr: &DaemonAddress,
) -> Result<(), ClientError> {
    if let super::connect::DaemonStream::Tcp(socket) = stream {
        if let Some(values) = options.sockopts() {
            apply_socket_options(socket, values);
        }

        // Module listing has no transfer, so no bwlimit pacing applies.
        super::tcp_perf::apply_client_tcp_perf_options(socket, options.tcp_fastopen(), None);

        if let Some(blocking) = options.blocking_io() {
            socket.set_nonblocking(!blocking).map_err(|error| {
                let action = if blocking {
                    "set blocking mode for"
                } else {
                    "set nonblocking mode for"
                };
                socket_error(action, addr.socket_addr_display(), error)
            })?;
        }
    }

    Ok(())
}

const fn effective_timeout(timeout: TransferTimeout, default: Duration) -> Option<Duration> {
    match timeout {
        TransferTimeout::Default => Some(default),
        TransferTimeout::Disabled => None,
        TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[test]
    fn module_list_entry_from_line_name_only() {
        let entry = ModuleListEntry::from_line("testmodule");
        assert_eq!(entry.name(), "testmodule");
        assert!(entry.comment().is_none());
    }

    #[test]
    fn module_list_entry_from_line_with_comment() {
        let entry = ModuleListEntry::from_line("backup\tBackup storage area");
        assert_eq!(entry.name(), "backup");
        assert_eq!(entry.comment(), Some("Backup storage area"));
    }

    #[test]
    fn module_list_entry_from_line_empty_comment() {
        let entry = ModuleListEntry::from_line("data\t");
        assert_eq!(entry.name(), "data");
        assert!(entry.comment().is_none());
    }

    #[test]
    fn module_list_entry_from_line_multiple_tabs() {
        let entry = ModuleListEntry::from_line("module\tfirst\tsecond");
        assert_eq!(entry.name(), "module");
        assert_eq!(entry.comment(), Some("first\tsecond"));
    }

    #[test]
    fn module_list_new_and_accessors() {
        let motd = vec!["Welcome".to_owned()];
        let warnings = vec!["Warning1".to_owned()];
        let capabilities = vec!["cap1".to_owned()];
        let entries = vec![ModuleListEntry::from_line("test")];
        let list = ModuleList::new(motd, warnings, capabilities, entries);
        assert_eq!(list.motd_lines(), &["Welcome"]);
        assert_eq!(list.warnings(), &["Warning1"]);
        assert_eq!(list.capabilities(), &["cap1"]);
        assert_eq!(list.entries().len(), 1);
    }

    #[test]
    fn effective_timeout_default() {
        let default = Duration::from_secs(30);
        let result = effective_timeout(TransferTimeout::Default, default);
        assert_eq!(result, Some(default));
    }

    #[test]
    fn effective_timeout_disabled() {
        let default = Duration::from_secs(30);
        let result = effective_timeout(TransferTimeout::Disabled, default);
        assert_eq!(result, None);
    }

    #[test]
    fn effective_timeout_custom() {
        let default = Duration::from_secs(30);
        let custom = NonZeroU64::new(60).unwrap();
        let result = effective_timeout(TransferTimeout::Seconds(custom), default);
        assert_eq!(result, Some(Duration::from_secs(60)));
    }

    // upstream: socket.c:274-277, options.c:125 - connect(2) is alarm-guarded
    // only when --contimeout > 0; the default connect_timeout is 0. --timeout
    // governs per-read I/O on an established stream and must never bound the
    // connect phase. Without --contimeout, a slow connect must NOT be capped.
    #[test]
    fn connect_timeout_unset_leaves_connect_unbounded_even_with_timeout() {
        // --timeout=8 set, --contimeout unset -> connect stays unbounded (None).
        assert_eq!(resolve_connect_timeout(TransferTimeout::Default), None);
    }

    // --contimeout=N (N>0) is the sole trigger for bounding connect(2), matching
    // the alarm(connect_timeout) upstream installs for connect_timeout > 0.
    #[test]
    fn connect_timeout_set_bounds_connect() {
        let contimeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        assert_eq!(
            resolve_connect_timeout(contimeout),
            Some(Duration::from_secs(5))
        );
    }

    // --contimeout=0 parses to Disabled and, like upstream's connect_timeout=0
    // default, means "no connect bound", never "expire immediately".
    #[test]
    fn connect_timeout_zero_disables_bound() {
        assert_eq!(resolve_connect_timeout(TransferTimeout::Disabled), None);
    }

    // Default (neither --timeout nor --contimeout) leaves connect unbounded.
    #[test]
    fn connect_timeout_default_is_unbounded() {
        assert_eq!(resolve_connect_timeout(TransferTimeout::Default), None);
    }

    fn greeting(
        advertised: u32,
        subprotocol: Option<u32>,
        digests: Option<&str>,
    ) -> LegacyDaemonGreetingOwned {
        LegacyDaemonGreetingOwned::from_parts(advertised, subprotocol, digests.map(str::to_owned))
            .expect("greeting parts within supported range")
    }

    #[test]
    fn reconstruct_banner_matches_canonical_daemon_form() {
        assert_eq!(
            reconstruct_daemon_greeting_line(&greeting(30, None, None)),
            "@RSYNCD: 30"
        );
        assert_eq!(
            reconstruct_daemon_greeting_line(&greeting(32, Some(0), None)),
            "@RSYNCD: 32.0"
        );
        assert_eq!(
            reconstruct_daemon_greeting_line(&greeting(31, Some(0), Some("md5 md4"))),
            "@RSYNCD: 31.0 md5 md4"
        );
    }

    /// upstream: clientserver.c:191 - a `remote_protocol >= 30` banner that omits
    /// the subprotocol value is fatal for the client with `RERR_STARTCLIENT`.
    /// The message and exit code must match so a listing sees the same failure a
    /// transfer would.
    #[test]
    fn listing_rejects_missing_subprotocol() {
        let err = reject_incomplete_daemon_greeting(&greeting(30, None, None))
            .expect_err("proto>=30 without subprotocol must be rejected");
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
        assert_eq!(err.exit_code(), 5);
        assert!(
            err.to_string()
                .contains("the server omitted the subprotocol value: @RSYNCD: 30"),
            "got: {err}"
        );
    }

    /// upstream: clientserver.c:207 - a `remote_protocol > 31` banner that omits
    /// the digest name list is fatal for the client with `RERR_STARTCLIENT`.
    #[test]
    fn listing_rejects_missing_digest_list() {
        let err = reject_incomplete_daemon_greeting(&greeting(32, Some(0), None))
            .expect_err("proto>31 without digest list must be rejected");
        assert_eq!(err.exit_code(), 5);
        assert!(
            err.to_string()
                .contains("the server omitted the digest name list: @RSYNCD: 32.0"),
            "got: {err}"
        );
    }

    /// A clamped future advertisement is gated on the raw advertised number, as
    /// upstream applies the digest gate before clamping `protocol_version`.
    #[test]
    fn listing_rejects_missing_digest_on_future_protocol() {
        let err = reject_incomplete_daemon_greeting(&greeting(40, Some(0), None))
            .expect_err("proto>31 without digest list must be rejected");
        assert_eq!(err.exit_code(), 5);
        assert!(
            err.to_string()
                .contains("the server omitted the digest name list: @RSYNCD: 40.0"),
            "got: {err}"
        );
    }

    /// Well-formed banners for every gated threshold must list normally: a
    /// modern banner carrying both tokens, a protocol-30 banner needing no digest
    /// list, and a legacy sub-30 banner needing neither token.
    #[test]
    fn listing_accepts_well_formed_greetings() {
        reject_incomplete_daemon_greeting(&greeting(31, Some(0), Some("md5 md4")))
            .expect("complete modern greeting is accepted");
        reject_incomplete_daemon_greeting(&greeting(30, Some(0), None))
            .expect("protocol 30 needs no digest list");
        reject_incomplete_daemon_greeting(&greeting(29, None, None))
            .expect("legacy protocol needs neither token");
    }

    /// When `-e`/remote_shell is configured (and no connect program), a
    /// `host::` listing must reach the daemon by spawning the remote shell -
    /// not by opening TCP to the daemon port.
    ///
    /// WHY: upstream `rsync -e PROG host::` lists modules over the spawned
    /// shell (`main.c` daemon-over-rsh). Regressing to TCP yields
    /// `connect()` -> ECONNREFUSED (exit 10) against port 873, the exact
    /// daemon.test failure this path fixes. Pointing remote_shell at a
    /// nonexistent program makes the rsh branch fail at spawn time with an
    /// IPC error mentioning "daemon-over-rsh", proving the listing took the
    /// remote-shell branch rather than the TCP branch.
    #[test]
    fn listing_with_remote_shell_uses_rsh_not_tcp() {
        use std::ffi::OsString;

        let operands = vec![OsString::from("localhost::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("valid operands")
            .expect("daemon listing request");

        let options = ModuleListOptions::default().with_remote_shell(Some(vec![OsString::from(
            "/nonexistent/oc-rsync-rsh-daemon-probe-bin",
        )]));

        let err = run_module_list_with_password_and_options(
            request,
            options,
            None,
            TransferTimeout::Default,
            TransferTimeout::Default,
        )
        .expect_err("spawning a nonexistent remote shell must fail");

        assert_eq!(err.exit_code(), crate::client::IPC_EXIT_CODE);
        assert!(
            err.to_string().contains("daemon-over-rsh"),
            "listing should fail in the daemon-over-rsh spawn, got: {err}"
        );
    }
}
