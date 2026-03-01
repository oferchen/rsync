//! crates/core/src/client/remote/daemon_transfer.rs
//!
//! Daemon transfer orchestration.
//!
//! This module coordinates daemon-based remote transfers (rsync:// URLs) by
//! connecting to rsync daemons, performing handshakes, and executing transfers
//! using the server infrastructure.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::instrument;

use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use crate::auth::{DaemonAuthDigest, parse_daemon_digest_list, select_daemon_digest};

use super::super::config::ClientConfig;
use super::super::error::{ClientError, daemon_error, invalid_argument_error, socket_error};
use super::super::module_list::{
    DaemonAddress, DaemonAuthContext, apply_socket_options, connect_direct, load_daemon_password,
    parse_host_port, resolve_connect_timeout, send_daemon_auth_credentials,
};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::super::{CLIENT_SERVER_PROTOCOL_EXIT_CODE, DAEMON_SOCKET_TIMEOUT};
use super::flags;
use super::invocation::{RemoteRole, TransferSpec, determine_transfer_role};
use transfer::setup::build_capability_string;

use crate::server::handshake::HandshakeResult;
use crate::server::{ServerConfig, ServerRole};

/// Parsed daemon transfer request containing connection and path details.
#[derive(Clone, Debug)]
struct DaemonTransferRequest {
    address: DaemonAddress,
    module: String,
    path: String,
    username: Option<String>,
}

impl DaemonTransferRequest {
    /// Parse an rsync:// URL into a transfer request.
    ///
    /// Format: rsync://[user@]host[:port]/module/path
    fn parse_rsync_url(url: &str) -> Result<Self, ClientError> {
        let rest = url
            .strip_prefix("rsync://")
            .or_else(|| url.strip_prefix("RSYNC://"))
            .ok_or_else(|| invalid_argument_error(&format!("not an rsync:// URL: {url}"), 1))?;

        let mut parts = rest.splitn(2, '/');
        let host_port = parts.next().unwrap_or("");
        let path_part = parts.next().unwrap_or("");

        let target = parse_host_port(host_port, 873)?;

        let mut path_parts = path_part.splitn(2, '/');
        let module = path_parts.next().unwrap_or("").to_owned();
        let file_path = path_parts.next().unwrap_or("").to_owned();

        if module.is_empty() {
            return Err(invalid_argument_error(
                &format!("rsync:// URL must specify a module: {url}"),
                1,
            ));
        }

        Ok(Self {
            address: target.address,
            module,
            path: file_path,
            username: target.username,
        })
    }
}

/// Executes a transfer over daemon protocol (rsync://).
///
/// This is the entry point for daemon-based remote transfers. It:
/// 1. Parses the rsync:// URL
/// 2. Connects to the daemon
/// 3. Performs the daemon handshake
/// 4. Requests the module
/// 5. Sends arguments to daemon
/// 6. Determines role from operand positions
/// 7. Executes the transfer using server infrastructure
///
/// # Arguments
///
/// * `config` - Client configuration with transfer options
/// * `observer` - Optional progress observer
///
/// # Returns
///
/// A summary of the transfer on success, or an error if any step fails.
///
/// # Errors
///
/// Returns error if:
/// - URL parsing fails
/// - Daemon connection fails
/// - Handshake fails
/// - Module access is denied
/// - Transfer execution fails
#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, _observer), name = "daemon_transfer")
)]
pub fn run_daemon_transfer(
    config: &ClientConfig,
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    let args = config.transfer_args();
    if args.len() < 2 {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    let (sources, destination) = args.split_at(args.len() - 1);
    let destination = &destination[0];

    let transfer_spec = determine_transfer_role(sources, destination)?;
    let role = transfer_spec.role();
    let local_paths = match &transfer_spec {
        TransferSpec::Push { local_sources, .. } => local_sources.clone(),
        TransferSpec::Pull { local_dest, .. } => vec![local_dest.clone()],
        TransferSpec::Proxy { .. } => {
            return Err(invalid_argument_error(
                "remote-to-remote transfers via rsync daemon are not supported",
                1,
            ));
        }
    };

    // Find the rsync:// URL operand
    let daemon_url = config
        .transfer_args()
        .iter()
        .find(|arg| {
            let s = arg.to_string_lossy();
            s.starts_with("rsync://") || s.starts_with("RSYNC://")
        })
        .ok_or_else(|| invalid_argument_error("no rsync:// URL found", 1))?;

    let request = DaemonTransferRequest::parse_rsync_url(&daemon_url.to_string_lossy())?;

    // Use config timeouts with DAEMON_SOCKET_TIMEOUT as the default fallback.
    // upstream: clientserver.c — start_daemon_client() applies io_timeout to connect.
    let connect_duration = resolve_connect_timeout(
        config.connect_timeout(),
        config.timeout(),
        DAEMON_SOCKET_TIMEOUT,
    );
    let handshake_io_timeout = config.timeout().effective(DAEMON_SOCKET_TIMEOUT);
    let mut stream = connect_direct(
        &request.address,
        connect_duration,
        handshake_io_timeout,
        config.address_mode(),
        config.bind_address().map(|b| b.socket()),
    )?;

    // Apply user-specified socket options (--sockopts) to the data transfer socket.
    // upstream: clientserver.c — start_daemon_client() calls set_socket_options()
    // on the daemon socket before the handshake.
    if let Some(sockopts) = config.sockopts() {
        apply_socket_options(&stream, sockopts)?;
    }

    // Output MOTD unless --no-motd was specified (upstream defaults to true)
    let output_motd = !config.no_motd();
    let protocol = perform_daemon_handshake(
        &mut stream,
        &request,
        output_motd,
        config.daemon_params(),
        config.early_input(),
    )?;

    // For pull (we receive), the daemon is the sender, so is_sender=true.
    // For push (we send), the daemon is the receiver, so is_sender=false.
    let daemon_is_sender = matches!(role, RemoteRole::Receiver);
    send_daemon_arguments(&mut stream, config, &request, protocol, daemon_is_sender)?;

    // Protocol is already negotiated via @RSYNCD text exchange (not binary 4-byte exchange)
    // This mirrors upstream where remote_protocol != 0 after exchange_protocols,
    // so setup_protocol skips the binary exchange (compat.c:599)
    match role {
        RemoteRole::Receiver => {
            // Pull: remote → local
            run_pull_transfer(config, stream, &local_paths, protocol)
        }
        RemoteRole::Sender => {
            // Push: local → remote
            run_push_transfer(config, stream, &local_paths, protocol)
        }
        RemoteRole::Proxy => {
            // Already handled above with an error return
            unreachable!("Proxy transfers via daemon are rejected earlier")
        }
    }
}

/// Parses the protocol version from an @RSYNCD greeting line.
///
/// Format: "@RSYNCD: XX.Y [digest_list]"
/// Mirrors upstream exchange_protocols line 178: sscanf(buf, "@RSYNCD: %d.%d", ...)
fn parse_protocol_from_greeting(greeting: &str) -> Result<ProtocolVersion, ClientError> {
    // Skip "@RSYNCD: " prefix (9 characters)
    let rest = greeting.get(9..).ok_or_else(|| {
        daemon_error(
            format!("malformed greeting: {greeting}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        )
    })?;

    // Parse "XX.Y" - version is before the dot
    let version_str = rest
        .split(|c: char| c == '.' || c.is_whitespace())
        .next()
        .ok_or_else(|| {
            daemon_error(
                format!("no version in greeting: {greeting}"),
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            )
        })?;

    let version_num: u8 = version_str.parse().map_err(|_| {
        daemon_error(
            format!("invalid version number in greeting: {greeting}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        )
    })?;

    ProtocolVersion::try_from(version_num).map_err(|e| {
        daemon_error(
            format!("unsupported protocol version {version_num}: {e}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        )
    })
}

/// Parses the digest list from a daemon greeting.
///
/// Format: "@RSYNCD: XX.Y [digest_list]"
/// Returns the list of advertised digests for authentication.
fn parse_digest_list_from_greeting(greeting: &str) -> Vec<DaemonAuthDigest> {
    // Skip "@RSYNCD: XX.Y " (version part) to get digest list
    // The greeting looks like: "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4"
    let rest = greeting.get(9..).unwrap_or("");

    // Skip version number (e.g., "31.0")
    let after_version = rest
        .split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest);

    parse_daemon_digest_list(if after_version.is_empty() {
        None
    } else {
        Some(after_version)
    })
}

/// Performs the rsync daemon handshake protocol.
///
/// This follows the upstream clientserver.c:start_inband_exchange() flow:
/// 1. Read daemon greeting (@RSYNCD: XX.Y)
/// 2. Send client greeting (@RSYNCD: XX.Y)
/// 3. Send module name
/// 4. Read response lines (MOTD, @RSYNCD: OK/@RSYNCD: AUTHREQD/@ERROR)
///
/// Returns the negotiated protocol version.
///
/// When `output_motd` is true, MOTD lines are printed to stdout, mirroring
/// upstream rsync's behavior controlled by the `output_motd` global variable.
fn perform_daemon_handshake(
    stream: &mut TcpStream,
    request: &DaemonTransferRequest,
    output_motd: bool,
    daemon_params: &[String],
    early_input: Option<&Path>,
) -> Result<ProtocolVersion, ClientError> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| socket_error("clone", request.address.socket_addr_display(), e))?,
    );

    let mut greeting = String::new();
    reader.read_line(&mut greeting).map_err(|e| {
        socket_error(
            "read daemon greeting from",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    if !greeting.starts_with("@RSYNCD:") {
        return Err(daemon_error(
            format!("unexpected daemon greeting: {greeting}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        ));
    }

    // upstream: exchange_protocols line 178: sscanf(buf, "@RSYNCD: %d.%d", ...)
    let remote_protocol = parse_protocol_from_greeting(&greeting)?;
    let advertised_digests = parse_digest_list_from_greeting(&greeting);

    // upstream: compat.c:832-845 — for protocol 30+, client must include
    // supported auth digests. Order follows checksum.c:71-84.
    let client_version = format!(
        "@RSYNCD: {}.0 sha512 sha256 sha1 md5 md4\n",
        ProtocolVersion::NEWEST.as_u8()
    );
    stream.write_all(client_version.as_bytes()).map_err(|e| {
        socket_error(
            "send client version to",
            request.address.socket_addr_display(),
            e,
        )
    })?;
    stream
        .flush()
        .map_err(|e| socket_error("flush to", request.address.socket_addr_display(), e))?;

    // upstream: clientserver.c:send_daemon_args() — each --dparam key=value is
    // sent as "OPTION key=value\n" before the module name.
    for param in daemon_params {
        let option_line = format!("OPTION {param}\n");
        stream.write_all(option_line.as_bytes()).map_err(|e| {
            socket_error(
                "send daemon option to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    // upstream: clientserver.c:266-294 — sends `#early_input=<len>\n` followed by
    // the raw file contents before the module name. The daemon reads this in
    // rsync_module() and passes it to pre-xfer exec on stdin.
    if let Some(path) = early_input {
        send_early_input(stream, path, request)?;
    }

    // upstream: clientserver.c:351 — module name is sent BEFORE waiting for @RSYNCD: OK
    let module_request = format!("{}\n", request.module);
    stream.write_all(module_request.as_bytes()).map_err(|e| {
        socket_error(
            "send module request to",
            request.address.socket_addr_display(),
            e,
        )
    })?;
    stream
        .flush()
        .map_err(|e| socket_error("flush to", request.address.socket_addr_display(), e))?;

    // upstream: clientserver.c:357-390 — loop until @RSYNCD: OK, @ERROR, or
    // @RSYNCD: EXIT. Other lines are MOTD output.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| {
            socket_error(
                "read response from",
                request.address.socket_addr_display(),
                e,
            )
        })?;

        let trimmed = line.trim();

        // Handle @RSYNCD: AUTHREQD (authentication required)
        // Format: "@RSYNCD: AUTHREQD <challenge>"
        if let Some(challenge) = trimmed.strip_prefix("@RSYNCD: AUTHREQD ") {
            // Load password from RSYNC_PASSWORD environment variable
            let secret = load_daemon_password().ok_or_else(|| {
                daemon_error(
                    "daemon requires authentication but RSYNC_PASSWORD not set",
                    CLIENT_SERVER_PROTOCOL_EXIT_CODE,
                )
            })?;

            // Get username from URL or default to current user
            let username = request.username.clone().unwrap_or_else(|| {
                std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "rsync".to_owned())
            });

            // Select strongest mutually supported digest
            // upstream: compat.c:858 — fallback depends on protocol version
            let digest = select_daemon_digest(&advertised_digests, remote_protocol.as_u8());

            let auth_context = DaemonAuthContext::new(username, secret, digest);
            send_daemon_auth_credentials(&mut reader, &auth_context, challenge, &request.address)?;

            // Continue reading for OK or another error after auth
            continue;
        }

        if trimmed == "@RSYNCD: OK" {
            break;
        }

        // Server closing connection (used for module listing)
        if trimmed == "@RSYNCD: EXIT" {
            return Err(daemon_error(
                "daemon closed connection",
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            ));
        }

        // Error from server
        if trimmed.starts_with("@ERROR") {
            return Err(daemon_error(
                trimmed.strip_prefix("@ERROR: ").unwrap_or(trimmed),
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            ));
        }

        // Any other line is MOTD - output if enabled (mirrors upstream rprintf(FINFO, "%s\n", line))
        if output_motd {
            println!("{trimmed}");
        }
    }

    // Negotiate to minimum of our version and daemon's version
    // (mirrors upstream exchange_protocols lines 211-227)
    let our_protocol = ProtocolVersion::NEWEST.as_u8();
    let negotiated = if our_protocol < remote_protocol.as_u8() {
        // SAFETY: NEWEST is a valid protocol version by construction
        ProtocolVersion::try_from(our_protocol).expect("NEWEST protocol version is always valid")
    } else {
        remote_protocol
    };

    // Success - return negotiated protocol version
    Ok(negotiated)
}

/// Maximum early-input file size in bytes.
///
/// Upstream rsync limits the file to `BIGPATHBUFLEN` (typically 5120 bytes on
/// systems where `MAXPATHLEN >= 4096`). The manpage documents this as "up to 5K
/// of data".
///
/// upstream: rsync.h — `BIGPATHBUFLEN` is `MAXPATHLEN + 1024` or `4096 + 1024`.
pub(crate) const EARLY_INPUT_MAX_SIZE: usize = 5120;

/// Command prefix for the early-input protocol message.
///
/// upstream: clientserver.c — `#define EARLY_INPUT_CMD "#early_input="`
const EARLY_INPUT_CMD: &str = "#early_input=";

/// Reads the early-input file content, capping at [`EARLY_INPUT_MAX_SIZE`] bytes.
///
/// Returns the file content truncated to 5120 bytes if the file is larger.
/// Returns an empty `Vec` for empty files. Returns an error if the file cannot
/// be opened or read.
///
/// upstream: clientserver.c:266-294 — `start_inband_exchange()` reads the file
/// specified by `--early-input` before sending it to the daemon.
pub(crate) fn read_early_input_file(path: &Path) -> Result<Vec<u8>, ClientError> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(path).map_err(|e| {
        daemon_error(
            format!("failed to open {}: {e}", path.display()),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        )
    })?;

    let mut buf = vec![0u8; EARLY_INPUT_MAX_SIZE];
    let mut total = 0;

    while total < EARLY_INPUT_MAX_SIZE {
        let n = file.read(&mut buf[total..]).map_err(|e| {
            daemon_error(
                format!("failed to read {}: {e}", path.display()),
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            )
        })?;
        if n == 0 {
            break;
        }
        total += n;
    }

    buf.truncate(total);
    Ok(buf)
}

/// Reads and sends the early-input file to the daemon before the module name.
///
/// The data is sent as `#early_input=<len>\n` followed by the raw file bytes.
/// The daemon receives this in `rsync_module()` and passes it to the pre-xfer
/// exec script on stdin.
///
/// upstream: clientserver.c:266-294 — `start_inband_exchange()` sends the early
/// input after `exchange_protocols()` and before the module name.
fn send_early_input(
    stream: &mut TcpStream,
    path: &Path,
    request: &DaemonTransferRequest,
) -> Result<(), ClientError> {
    let data = read_early_input_file(path)?;

    if data.is_empty() {
        return Ok(());
    }

    let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
    stream.write_all(header.as_bytes()).map_err(|e| {
        socket_error(
            "send early-input header to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    stream.write_all(&data).map_err(|e| {
        socket_error(
            "send early-input data to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    Ok(())
}

/// Sends daemon-mode arguments to the server.
///
/// When `--protect-args` / `-s` is active, uses a two-phase protocol
/// matching upstream `clientserver.c:393-408`:
/// - Phase 1: minimal args (`--server [-s] .`) so the daemon knows to
///   expect protected args
/// - Phase 2: full argument list via `send_secluded_args()` wire format
///
/// Without protect-args, sends all arguments in a single phase as before.
/// For protocol >= 30, strings are null-terminated; for < 30, newline-terminated.
fn send_daemon_arguments(
    stream: &mut TcpStream,
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Result<(), ClientError> {
    let protect = config.protect_args().unwrap_or(false);

    let full_args = build_full_daemon_args(config, request, protocol, is_sender);

    // Phase 1: send args over the daemon text protocol.
    // With protect-args, only send the minimal set so the daemon detects `-s`
    // and expects a phase-2 secluded-args payload.
    // upstream: clientserver.c:393-405 — stops at the NULL marker in sargs
    let phase1_args = if protect {
        build_minimal_daemon_args(is_sender)
    } else {
        full_args.clone()
    };

    let terminator = if protocol.as_u8() >= 30 { b'\0' } else { b'\n' };

    for arg in &phase1_args {
        stream.write_all(arg.as_bytes()).map_err(|e| {
            socket_error("send argument to", request.address.socket_addr_display(), e)
        })?;
        stream.write_all(&[terminator]).map_err(|e| {
            socket_error(
                "send terminator to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    // Empty string signals end of phase-1 argument list.
    stream.write_all(&[terminator]).map_err(|e| {
        socket_error(
            "send final terminator to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    // Phase 2: when protect-args is active, send the real arguments via
    // the secluded-args wire format (null-separated with empty terminator).
    // upstream: clientserver.c:407-408 — send_protected_args(f_out, sargs)
    if protect {
        let mut secluded = vec!["rsync"];
        secluded.extend(full_args.iter().map(String::as_str));
        protocol::secluded_args::send_secluded_args(stream, &secluded).map_err(|e| {
            socket_error(
                "send secluded args to",
                request.address.socket_addr_display(),
                e,
            )
        })?;
    }

    stream.flush().map_err(|e| {
        socket_error(
            "flush arguments to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    Ok(())
}

/// Builds the minimal phase-1 argument list for protect-args daemon mode.
///
/// The daemon only needs `--server [--sender] -s .` to know that
/// secluded args follow in phase 2.
/// upstream: clientserver.c:393-405 — sargs has a NULL marker after `-s .`
fn build_minimal_daemon_args(is_sender: bool) -> Vec<String> {
    let mut args = vec!["--server".to_owned()];
    if is_sender {
        args.push("--sender".to_owned());
    }
    args.push("-s".to_owned());
    args.push(".".to_owned());
    args
}

/// Builds the full argument list for daemon-mode transfer.
///
/// This is the complete set of flags, capability string, and module path
/// that the server needs for the transfer.
fn build_full_daemon_args(
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    args.push("--server".to_owned());
    if is_sender {
        args.push("--sender".to_owned());
    }

    // Forward --checksum-choice to the daemon so both sides agree on the
    // checksum algorithm (upstream options.c server_options()).
    let checksum_choice = config.checksum_choice();
    if let Some(override_algo) = checksum_choice.transfer_protocol_override() {
        args.push(format!("--checksum-choice={}", override_algo.as_str()));
    }

    let flag_string = flags::build_server_flag_string(config);
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    // Add capability flags for protocol 30+.
    // Uses CAPABILITY_MAPPINGS as single source of truth (mirrors upstream
    // options.c:3003-3050 maybe_add_e_option()).
    //
    // INC_RECURSE is only advertised for pull transfers (receiver role).
    // The sender-side incremental file list sending needs further interop
    // validation before enabling with upstream daemons.
    if protocol.as_u8() >= 30 {
        args.push(build_capability_string(!is_sender));
    }

    // Dummy argument (upstream requirement - represents CWD)
    args.push(".".to_owned());

    // Module path
    let module_path = format!("{}/{}", request.module, request.path);
    args.push(module_path);

    args
}

/// Executes a pull transfer (remote → local).
///
/// In a pull transfer, the local side acts as the receiver and the remote side
/// acts as the sender/generator. We reuse the server receiver infrastructure.
///
/// Protocol sequence (mirrors upstream client_run for !am_sender):
/// 1. Protocol already negotiated via @RSYNCD text exchange (not binary 4-byte)
/// 2. setup_protocol() does compat flags + checksum seed (NO version exchange)
/// 3. io_start_multiplex_out() - activates output multiplex
/// 4. send_filter_list() - we send, daemon receives (after multiplex activation)
/// 5. File list exchange and transfer
fn run_pull_transfer(
    config: &ClientConfig,
    mut stream: TcpStream,
    local_paths: &[String],
    protocol: ProtocolVersion,
) -> Result<ClientSummary, ClientError> {
    // Disable Nagle's algorithm to ensure data is sent immediately.
    // Without this, small writes may be buffered and not reach the daemon
    // before we start reading, causing a deadlock.
    stream
        .set_nodelay(true)
        .map_err(|e| socket_error("set nodelay on", "daemon socket", e))?;

    // Replace the handshake-phase socket timeout with the user-configured --timeout
    // for the data transfer phase. When --timeout is not set, clear the handshake
    // timeout (upstream default io_timeout is 0, meaning no timeout).
    // upstream: io.c — select_timeout() uses io_timeout for all transfer I/O.
    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| Duration::from_secs(s.get()));
    stream
        .set_read_timeout(transfer_timeout)
        .map_err(|e| socket_error("set read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(transfer_timeout)
        .map_err(|e| socket_error("set write timeout on", "daemon socket", e))?;

    let filter_rules = flags::build_wire_format_rules(config.filter_rules())?;

    // Protocol was negotiated via @RSYNCD text exchange, not binary 4-byte exchange.
    // setup_protocol() will skip the binary exchange because remote_protocol != 0
    // (mirrors upstream compat.c:599: if (remote_protocol == 0) { ... })
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: config.timeout().as_seconds().map(|s| s.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };

    let server_config = build_server_config_for_receiver(config, local_paths, filter_rules)?;
    let start = Instant::now();
    let server_stats =
        run_server_with_handshake_over_stream(server_config, handshake, &mut stream, None)?;
    let elapsed = start.elapsed();

    Ok(convert_server_stats_to_summary(server_stats, elapsed))
}

/// Executes a push transfer (local → remote).
///
/// In a push transfer, the local side acts as the sender/generator and the
/// remote side acts as the receiver. We reuse the server generator infrastructure.
///
/// Protocol sequence (mirrors upstream client_run for am_sender):
/// 1. Protocol already negotiated via @RSYNCD text exchange (not binary 4-byte)
/// 2. setup_protocol() does compat flags + checksum seed (NO version exchange)
/// 3. io_start_multiplex_out() - activates output multiplex
/// 4. send_filter_list() - we send, daemon receives (after multiplex activation)
/// 5. File list exchange and transfer
fn run_push_transfer(
    config: &ClientConfig,
    mut stream: TcpStream,
    local_paths: &[String],
    protocol: ProtocolVersion,
) -> Result<ClientSummary, ClientError> {
    // Disable Nagle's algorithm to ensure data is sent immediately.
    // Without this, small writes may be buffered and not reach the daemon
    // before we start reading, causing a deadlock.
    stream
        .set_nodelay(true)
        .map_err(|e| socket_error("set nodelay on", "daemon socket", e))?;

    // Replace handshake-phase timeout with user-configured --timeout (same as pull).
    let transfer_timeout = config
        .timeout()
        .as_seconds()
        .map(|s| Duration::from_secs(s.get()));
    stream
        .set_read_timeout(transfer_timeout)
        .map_err(|e| socket_error("set read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(transfer_timeout)
        .map_err(|e| socket_error("set write timeout on", "daemon socket", e))?;

    let filter_rules = flags::build_wire_format_rules(config.filter_rules())?;

    // Protocol was negotiated via @RSYNCD text exchange, not binary 4-byte exchange.
    // setup_protocol() will skip the binary exchange because remote_protocol != 0
    // (mirrors upstream compat.c:599: if (remote_protocol == 0) { ... })
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: config.timeout().as_seconds().map(|s| s.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };

    let server_config = build_server_config_for_generator(config, local_paths, filter_rules)?;
    let start = Instant::now();
    let server_stats =
        run_server_with_handshake_over_stream(server_config, handshake, &mut stream, None)?;
    let elapsed = start.elapsed();

    Ok(convert_server_stats_to_summary(server_stats, elapsed))
}

/// Converts server-side statistics to a client summary.
///
/// Maps the statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. The elapsed time is used to calculate
/// the transfer rate (bytes/sec) shown in the summary output.
fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;
    use transfer::io_error_flags;

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
                elapsed,
            );
            (s, 0i32, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // Map accumulated I/O error flags to an exit code.
    // upstream: log.c — log_exit() converts io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR — treat as partial transfer.
        summary.set_io_error_exit_code(23); // RERR_PARTIAL
    }

    summary
}

/// Helper function to run server over a TCP stream with pre-negotiated handshake.
///
/// This is used for daemon client mode where we've already done the binary protocol
/// version exchange in `perform_client_protocol_exchange`. Calls `run_server_with_handshake`
/// directly, skipping the duplicate version exchange.
fn run_server_with_handshake_over_stream(
    config: ServerConfig,
    handshake: HandshakeResult,
    stream: &mut TcpStream,
    progress: Option<&mut dyn crate::server::TransferProgressCallback>,
) -> Result<crate::server::ServerStats, ClientError> {
    let mut reader = stream
        .try_clone()
        .map_err(|e| invalid_argument_error(&format!("failed to clone stream: {e}"), 23))?;

    crate::server::run_server_with_handshake(config, handshake, &mut reader, stream, progress)
        .map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))
}

/// Builds server configuration for receiver role (pull transfer).
fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Set client_mode since we're a daemon client, not a server.
    // This prevents the context from trying to read filter list
    // (since we'll send it to the daemon after multiplex activation).
    server_config.client_mode = true;
    server_config.is_daemon_connection = true;
    server_config.filter_rules = filter_rules;

    // Set verbose flag for local output (not sent to daemon in server protocol string)
    server_config.flags.verbose = config.verbosity() > 0;

    // Propagate long-form-only flags that aren't part of the server flag string
    server_config.fsync = config.fsync();
    server_config.io_uring_policy = config.io_uring_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
    server_config.stop_at = config.stop_at();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Set client_mode since we're a daemon client, not a server.
    // This prevents the context from trying to read filter list
    // (since we'll send it to the daemon after multiplex activation).
    server_config.client_mode = true;
    server_config.is_daemon_connection = true;
    server_config.filter_rules = filter_rules;

    // Set verbose flag for local output (not sent to daemon in server protocol string)
    server_config.flags.verbose = config.verbosity() > 0;

    // Propagate long-form-only flags that aren't part of the server flag string
    server_config.fsync = config.fsync();
    server_config.io_uring_policy = config.io_uring_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
    server_config.stop_at = config.stop_at();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::DaemonAuthDigest;

    #[test]
    fn parse_digest_list_from_greeting_with_full_list() {
        let greeting = "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n";
        let digests = parse_digest_list_from_greeting(greeting);
        assert_eq!(digests.len(), 5);
        assert_eq!(digests[0], DaemonAuthDigest::Sha512);
        assert_eq!(digests[1], DaemonAuthDigest::Sha256);
        assert_eq!(digests[2], DaemonAuthDigest::Sha1);
        assert_eq!(digests[3], DaemonAuthDigest::Md5);
        assert_eq!(digests[4], DaemonAuthDigest::Md4);
    }

    #[test]
    fn parse_digest_list_from_greeting_with_partial_list() {
        let greeting = "@RSYNCD: 30.0 sha256 md5\n";
        let digests = parse_digest_list_from_greeting(greeting);
        assert_eq!(digests.len(), 2);
        assert_eq!(digests[0], DaemonAuthDigest::Sha256);
        assert_eq!(digests[1], DaemonAuthDigest::Md5);
    }

    #[test]
    fn parse_digest_list_from_greeting_without_digests() {
        // Old protocol versions may not include digest list
        let greeting = "@RSYNCD: 29.0\n";
        let digests = parse_digest_list_from_greeting(greeting);
        assert!(digests.is_empty());
    }

    #[test]
    fn parse_digest_list_from_greeting_ignores_unknown() {
        let greeting = "@RSYNCD: 31.0 sha512 unknown sha1 bogus md4\n";
        let digests = parse_digest_list_from_greeting(greeting);
        assert_eq!(digests.len(), 3);
        assert_eq!(digests[0], DaemonAuthDigest::Sha512);
        assert_eq!(digests[1], DaemonAuthDigest::Sha1);
        assert_eq!(digests[2], DaemonAuthDigest::Md4);
    }

    #[test]
    fn parse_protocol_from_greeting_extracts_version() {
        let greeting = "@RSYNCD: 31.0 sha512 sha256\n";
        let protocol = parse_protocol_from_greeting(greeting).unwrap();
        assert_eq!(protocol.as_u8(), 31);
    }

    #[test]
    fn parse_protocol_from_greeting_handles_version_only() {
        let greeting = "@RSYNCD: 28.0\n";
        let protocol = parse_protocol_from_greeting(greeting).unwrap();
        assert_eq!(protocol.as_u8(), 28);
    }

    mod early_input_tests {
        use super::*;

        #[test]
        fn read_normal_file() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("early.txt");
            std::fs::write(&file_path, b"hello early input").unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert_eq!(data, b"hello early input");
        }

        #[test]
        fn read_empty_file() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("empty.txt");
            std::fs::write(&file_path, b"").unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert!(data.is_empty());
        }

        #[test]
        fn read_file_exactly_at_limit() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("exact.bin");
            let content = vec![0xABu8; EARLY_INPUT_MAX_SIZE];
            std::fs::write(&file_path, &content).unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
            assert_eq!(data, content);
        }

        #[test]
        fn read_file_exceeding_limit_is_truncated() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("large.bin");
            let content = vec![0xCDu8; EARLY_INPUT_MAX_SIZE + 1024];
            std::fs::write(&file_path, &content).unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
            assert_eq!(data, &content[..EARLY_INPUT_MAX_SIZE]);
        }

        #[test]
        fn read_missing_file_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("nonexistent.txt");

            let err = read_early_input_file(&file_path).unwrap_err();
            assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
            assert!(err.to_string().contains("failed to open"));
        }

        #[test]
        fn max_size_constant_is_5k() {
            assert_eq!(EARLY_INPUT_MAX_SIZE, 5120);
        }

        #[test]
        fn read_file_with_binary_content() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("binary.bin");
            // All byte values 0x00..=0xFF repeated
            let content: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
            std::fs::write(&file_path, &content).unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert_eq!(data, content);
        }

        #[test]
        fn read_file_well_over_limit_truncated_to_max() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("huge.bin");
            // 10x the limit
            let content = vec![0xFFu8; EARLY_INPUT_MAX_SIZE * 10];
            std::fs::write(&file_path, &content).unwrap();

            let data = read_early_input_file(&file_path).unwrap();
            assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
        }

        #[test]
        fn wire_format_header_matches_upstream_protocol() {
            let data = b"test-payload";
            let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
            assert_eq!(header, "#early_input=12\n");
        }

        #[test]
        fn wire_format_uses_decimal_length() {
            let data = vec![0u8; 256];
            let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
            assert_eq!(header, "#early_input=256\n");
        }

        #[test]
        fn wire_format_at_max_size() {
            let header = format!("{EARLY_INPUT_CMD}{EARLY_INPUT_MAX_SIZE}\n");
            assert_eq!(header, "#early_input=5120\n");
        }

        #[test]
        fn wire_format_complete_message_structure() {
            let payload = b"auth-token";
            let header = format!("{EARLY_INPUT_CMD}{}\n", payload.len());
            let mut wire = header.into_bytes();
            wire.extend_from_slice(payload);

            // Verify structure: header line ends with \n, followed by raw data
            let newline_pos = wire.iter().position(|&b| b == b'\n').unwrap();
            let header_part = std::str::from_utf8(&wire[..newline_pos]).unwrap();
            assert_eq!(header_part, "#early_input=10");
            assert_eq!(&wire[newline_pos + 1..], b"auth-token");
        }

        #[test]
        fn early_input_cmd_constant_matches_upstream() {
            assert_eq!(EARLY_INPUT_CMD, "#early_input=");
        }
    }

    /// Integration tests verifying the complete early-input round-trip:
    /// client reads a file, sends it over a TCP socket, and the daemon-side
    /// wire format is validated against protocol expectations.
    mod early_input_roundtrip_tests {
        use super::*;
        use std::io::{BufRead, BufReader, Read};
        use std::net::TcpListener;

        /// Helper: creates a `DaemonTransferRequest` for test use.
        fn test_request() -> DaemonTransferRequest {
            DaemonTransferRequest {
                address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
                module: "test".to_owned(),
                path: String::new(),
                username: None,
            }
        }

        /// Helper: reads the early-input wire message from a stream, parsing
        /// the `#early_input=<len>\n` header and the raw payload bytes.
        ///
        /// Returns `None` if no data was sent (e.g. empty file case).
        fn receive_early_input(reader: &mut BufReader<impl Read>) -> Option<Vec<u8>> {
            let mut line = String::new();
            let n = reader.read_line(&mut line).unwrap();
            if n == 0 {
                return None;
            }

            let trimmed = line.trim_end_matches('\n');
            let len_str = trimmed.strip_prefix(EARLY_INPUT_CMD)?;
            let data_len: usize = len_str.parse().unwrap();

            let mut buf = vec![0u8; data_len];
            reader.read_exact(&mut buf).unwrap();
            Some(buf)
        }

        #[test]
        fn roundtrip_normal_content() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("early.txt");
            let content = b"hello early-input roundtrip";
            std::fs::write(&file_path, content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received, content);
        }

        #[test]
        fn roundtrip_empty_file_sends_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("empty.txt");
            std::fs::write(&file_path, b"").unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader);
            assert!(
                received.is_none(),
                "empty file should not produce any wire data"
            );
        }

        #[test]
        fn roundtrip_file_exactly_at_5k_limit() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("exact.bin");
            let content = vec![0xABu8; EARLY_INPUT_MAX_SIZE];
            std::fs::write(&file_path, &content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received.len(), EARLY_INPUT_MAX_SIZE);
            assert_eq!(received, content);
        }

        #[test]
        fn roundtrip_file_over_limit_is_truncated() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("large.bin");
            let content = vec![0xCDu8; EARLY_INPUT_MAX_SIZE + 2048];
            std::fs::write(&file_path, &content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received.len(), EARLY_INPUT_MAX_SIZE);
            assert_eq!(received, &content[..EARLY_INPUT_MAX_SIZE]);
        }

        #[test]
        fn roundtrip_binary_content_preserves_all_byte_values() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("binary.bin");
            let content: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
            std::fs::write(&file_path, &content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received, content);
        }

        #[test]
        fn roundtrip_wire_header_matches_daemon_protocol() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("proto.txt");
            let content = b"auth-token-data";
            std::fs::write(&file_path, content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            // Read the raw wire bytes to validate the exact header format
            let mut raw = Vec::new();
            let mut server = server;
            server.read_to_end(&mut raw).unwrap();

            let expected_header = format!("#early_input={}\n", content.len());
            let header_len = expected_header.len();

            assert_eq!(
                std::str::from_utf8(&raw[..header_len]).unwrap(),
                expected_header
            );
            assert_eq!(&raw[header_len..], content);
        }

        #[test]
        fn roundtrip_single_byte_file() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("one.bin");
            std::fs::write(&file_path, [0x42]).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received, vec![0x42]);
        }

        #[test]
        fn roundtrip_content_with_newlines_and_nulls() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("special.bin");
            let content = b"line1\nline2\n\0\0\nline3\n";
            std::fs::write(&file_path, content).unwrap();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = TcpStream::connect(addr).unwrap();
            let (server, _) = listener.accept().unwrap();

            let request = test_request();
            send_early_input(&mut client, &file_path, &request).unwrap();
            drop(client);

            let mut reader = BufReader::new(server);
            let received = receive_early_input(&mut reader).unwrap();
            assert_eq!(received, content);
        }
    }

    mod protect_args_daemon_tests {
        use super::*;

        fn test_daemon_request() -> DaemonTransferRequest {
            DaemonTransferRequest {
                address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
                module: "test".to_owned(),
                path: String::new(),
                username: None,
            }
        }

        #[test]
        fn build_minimal_args_receiver() {
            let args = build_minimal_daemon_args(false);
            assert_eq!(args, vec!["--server", "-s", "."]);
        }

        #[test]
        fn build_minimal_args_sender() {
            let args = build_minimal_daemon_args(true);
            assert_eq!(args, vec!["--server", "--sender", "-s", "."]);
        }

        #[test]
        fn build_full_args_includes_module_path() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert_eq!(args[0], "--server");
            assert!(args.contains(&".".to_owned()));
            let last = args.last().unwrap();
            assert!(last.starts_with(&request.module));
        }

        #[test]
        fn build_full_args_sender_flag() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, true);

            assert_eq!(args[0], "--server");
            assert_eq!(args[1], "--sender");
        }

        #[test]
        fn build_full_args_capability_flags_protocol30() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(32u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(args.iter().any(|a| a.starts_with("-e.")));
        }

        #[test]
        fn build_full_args_no_capability_flags_protocol29() {
            let config = ClientConfig::default();
            let request = test_daemon_request();
            let protocol = ProtocolVersion::try_from(29u8).unwrap();
            let args = build_full_daemon_args(&config, &request, protocol, false);

            assert!(!args.iter().any(|a| a.starts_with("-e.")));
        }
    }
}
