//! crates/core/src/client/remote/daemon_transfer.rs
//!
//! Daemon transfer orchestration.
//!
//! This module coordinates daemon-based remote transfers (rsync:// URLs) by
//! connecting to rsync daemons, performing handshakes, and executing transfers
//! using the server infrastructure.

// Note: This module uses the same TcpStream for both read and write.
// We use unsafe code to split the borrow for stdin/stdout, matching the
// pattern in ssh_transfer.rs
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::instrument;

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType};

use crate::auth::{DaemonAuthDigest, parse_daemon_digest_list, select_daemon_digest};

use super::super::config::{ClientConfig, FilterRuleKind, FilterRuleSpec};
use super::super::error::{ClientError, daemon_error, invalid_argument_error, socket_error};
use super::super::module_list::{
    DaemonAddress, DaemonAuthContext, connect_direct, load_daemon_password, parse_host_port,
    send_daemon_auth_credentials,
};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::super::{CLIENT_SERVER_PROTOCOL_EXIT_CODE, DAEMON_SOCKET_TIMEOUT};
use super::invocation::{RemoteRole, TransferSpec, determine_transfer_role};
use crate::server::handshake::HandshakeResult;
use crate::server::{ServerConfig, ServerRole};

/// Parsed daemon transfer request containing connection and path details.
#[derive(Clone, Debug)]
struct DaemonTransferRequest {
    address: DaemonAddress,
    module: String,
    #[allow(dead_code)] // Used in future server execution
    path: String,
    #[allow(dead_code)] // Used in future authentication
    username: Option<String>,
}

impl DaemonTransferRequest {
    /// Parse an rsync:// URL into a transfer request.
    ///
    /// Format: rsync://[user@]host[:port]/module/path
    fn parse_rsync_url(url: &str) -> Result<Self, ClientError> {
        // Strip rsync:// prefix
        let rest = url
            .strip_prefix("rsync://")
            .or_else(|| url.strip_prefix("RSYNC://"))
            .ok_or_else(|| invalid_argument_error(&format!("not an rsync:// URL: {url}"), 1))?;

        // Split into host and path components
        let mut parts = rest.splitn(2, '/');
        let host_port = parts.next().unwrap_or("");
        let path_part = parts.next().unwrap_or("");

        // Parse host and port
        let target = parse_host_port(host_port, 873)?;

        // Split path into module and file path
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
    // Step 1: Parse transfer args to determine role and paths
    let args = config.transfer_args();
    if args.len() < 2 {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    let (sources, destination) = args.split_at(args.len() - 1);
    let destination = &destination[0];

    // Determine push vs pull and extract local/remote paths
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

    // Step 2: Parse the URL
    let request = DaemonTransferRequest::parse_rsync_url(&daemon_url.to_string_lossy())?;

    // Step 3: Connect to daemon
    let mut stream = connect_direct(
        &request.address,
        None, // No custom connect timeout
        Some(DAEMON_SOCKET_TIMEOUT),
        config.address_mode(),
        None, // No bind address
    )?;

    // Step 4: Perform daemon handshake
    // Output MOTD unless --no-motd was specified (upstream defaults to true)
    let output_motd = !config.no_motd();
    let protocol = perform_daemon_handshake(&mut stream, &request, output_motd)?;

    // Step 5: Send arguments to daemon
    // For pull (we receive), the daemon is the sender, so is_sender=true
    // For push (we send), the daemon is the receiver, so is_sender=false
    let daemon_is_sender = matches!(role, RemoteRole::Receiver);
    send_daemon_arguments(&mut stream, config, &request, protocol, daemon_is_sender)?;

    // Step 6: Execute transfer based on role
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
) -> Result<ProtocolVersion, ClientError> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| socket_error("clone", request.address.socket_addr_display(), e))?,
    );

    // Step 1: Read daemon greeting: @RSYNCD: 31.0
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

    // Parse daemon's protocol version from greeting: @RSYNCD: XX.Y [digests]
    // Mirrors upstream exchange_protocols line 178: sscanf(buf, "@RSYNCD: %d.%d", ...)
    let remote_protocol = parse_protocol_from_greeting(&greeting)?;

    // Parse digest list from greeting for authentication
    let advertised_digests = parse_digest_list_from_greeting(&greeting);

    // Step 2: Send client version with auth digest list (upstream compat.c:832-845)
    // For protocol 30+, client must include supported auth digests.
    // Order follows upstream checksum.c:71-84 valid_auth_checksums_items[]
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

    // Step 3: Send module name (upstream clientserver.c:351)
    // This happens BEFORE waiting for @RSYNCD: OK
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

    // Step 4: Read response lines (upstream clientserver.c:357-390)
    // Loop until we get @RSYNCD: OK, @ERROR, or @RSYNCD: EXIT
    // Other lines are MOTD (message of the day) which we skip
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
            let digest = select_daemon_digest(&advertised_digests);

            // Build auth context and send credentials
            let auth_context = DaemonAuthContext::new(username, secret, digest);
            send_daemon_auth_credentials(&mut reader, &auth_context, challenge, &request.address)?;

            // Continue reading for OK or another error after auth
            continue;
        }

        // Success - module accepted
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

/// Sends daemon-mode arguments to the server.
///
/// Mirrors upstream clientserver.c:393-405 (send_to_server).
/// The format is: --server [--sender] <flags> . <module/path>
/// For protocol ≥30, sends null-terminated strings.
/// For protocol <30, sends newline-terminated strings.
fn send_daemon_arguments(
    stream: &mut TcpStream,
    config: &ClientConfig,
    request: &DaemonTransferRequest,
    protocol: ProtocolVersion,
    is_sender: bool,
) -> Result<(), ClientError> {
    // Build argument list (mirrors ssh_transfer.rs server_options)
    let mut args = Vec::new();

    // First arg is always --server (tells daemon we're using server protocol)
    args.push("--server".to_owned());

    // For pull (we receive), daemon is sender, so we send --sender
    // For push (we send), daemon is receiver, so we don't send --sender
    if is_sender {
        args.push("--sender".to_owned());
    }

    // Build flag string with capabilities
    let flag_string = build_server_flag_string(config);
    if !flag_string.is_empty() {
        args.push(flag_string);
    }

    // Add capability flags for protocol 30+ (upstream options.c:3010-3037 add_e_flags())
    // This tells the daemon what features the client supports.
    //
    // Capability flags:
    // - e. = capability prefix (. is placeholder for subprotocol version)
    // - L = symlink time-setting support (SYMLINK_TIMES)
    // - s = symlink iconv translation support (SYMLINK_ICONV)
    // - f = flist I/O-error safety support (SAFE_FILE_LIST)
    // - x = avoid xattr hardlink optimization (AVOID_XATTR_OPTIMIZATION)
    // - C = checksum seed order fix (CHECKSUM_SEED_FIX)
    // - I = inplace_partial behavior (INPLACE_PARTIAL_DIR)
    // - v = varint for flist flags (VARINT_FLIST_FLAGS)
    // - u = include uid 0 & gid 0 names (ID0_NAMES)
    //
    // NOTE: 'i' (INC_RECURSE) is NOT included because we send a complete
    // file list in one batch. With INC_RECURSE, the daemon expects separate
    // file lists for each directory level, which we don't implement yet.
    if protocol.as_u8() >= 30 {
        args.push("-e.LsfxCIvu".to_owned());
    }

    // Add dummy argument (upstream requirement - represents CWD)
    args.push(".".to_owned());

    // Add module path
    let module_path = format!("{}/{}", request.module, request.path);
    args.push(module_path);

    // Send arguments with appropriate terminator
    let terminator = if protocol.as_u8() >= 30 { b'\0' } else { b'\n' };

    for arg in &args {
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

    // Send final empty string to signal end
    stream.write_all(&[terminator]).map_err(|e| {
        socket_error(
            "send final terminator to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    stream.flush().map_err(|e| {
        socket_error(
            "flush arguments to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    Ok(())
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

    // Clear the handshake-phase socket timeouts before the data transfer begins.
    // DAEMON_SOCKET_TIMEOUT (10s) was set during connect_direct() to detect
    // unresponsive servers during the handshake, but during the actual transfer
    // the remote server may legitimately take longer to prepare the file list.
    // On Linux, an expired read timeout manifests as EAGAIN (errno 11), not
    // ETIMEDOUT, which causes spurious "Resource temporarily unavailable" errors.
    stream
        .set_read_timeout(None)
        .map_err(|e| socket_error("clear read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(None)
        .map_err(|e| socket_error("clear write timeout on", "daemon socket", e))?;

    // Build filter rules to pass to server config
    // (will be sent after multiplex activation in run_server_with_handshake)
    let filter_rules = build_wire_format_rules(config.filter_rules())?;

    // Build handshake result with negotiated protocol
    // Protocol was negotiated via @RSYNCD text exchange, not binary 4-byte exchange.
    // setup_protocol() will skip the binary exchange because remote_protocol != 0
    // (mirrors upstream compat.c:599: if (remote_protocol == 0) { ... })
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false, // setup_protocol() will do compat exchange
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    };

    // Build server config for receiver role with filter rules
    let server_config = build_server_config_for_receiver(config, local_paths, filter_rules)?;

    // Run server with pre-negotiated handshake, tracking elapsed time for rate calculation
    let start = Instant::now();
    let server_stats =
        run_server_with_handshake_over_stream(server_config, handshake, &mut stream)?;
    let elapsed = start.elapsed();

    // Convert server stats to client summary
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

    // Clear the handshake-phase socket timeouts (same rationale as run_pull_transfer).
    stream
        .set_read_timeout(None)
        .map_err(|e| socket_error("clear read timeout on", "daemon socket", e))?;
    stream
        .set_write_timeout(None)
        .map_err(|e| socket_error("clear write timeout on", "daemon socket", e))?;

    // Build filter rules to pass to server config
    // (will be sent after multiplex activation in run_server_with_handshake)
    let filter_rules = build_wire_format_rules(config.filter_rules())?;

    // Build handshake result with negotiated protocol
    // Protocol was negotiated via @RSYNCD text exchange, not binary 4-byte exchange.
    // setup_protocol() will skip the binary exchange because remote_protocol != 0
    // (mirrors upstream compat.c:599: if (remote_protocol == 0) { ... })
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false, // setup_protocol() will do compat exchange
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    };

    // Build server config for generator (sender) role with filter rules
    let server_config = build_server_config_for_generator(config, local_paths, filter_rules)?;

    // Run server with pre-negotiated handshake, tracking elapsed time for rate calculation
    let start = Instant::now();
    let server_stats =
        run_server_with_handshake_over_stream(server_config, handshake, &mut stream)?;
    let elapsed = start.elapsed();

    // Convert server stats to client summary
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

    let summary = match stats {
        ServerStats::Receiver(transfer_stats) => {
            // For pull transfers: we received files from remote
            LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
            )
        }
        ServerStats::Generator(generator_stats) => {
            // For push transfers: we sent files to remote
            LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
                elapsed,
            )
        }
    };

    ClientSummary::from_summary(summary)
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
) -> Result<crate::server::ServerStats, ClientError> {
    use std::io::Read;

    // SAFETY: We create two mutable references to the same stream, which is safe
    // because TcpStream internally manages separate read/write buffers.
    let stream_ptr = stream as *mut TcpStream;
    let result = unsafe {
        let stdin: &mut dyn Read = &mut *stream_ptr;
        let stdout = &mut *stream_ptr;
        crate::server::run_server_with_handshake(config, handshake, stdin, stdout)
    };

    result.map_err(|e| invalid_argument_error(&format!("transfer failed: {e}"), 23))
}

/// Builds server configuration for receiver role (pull transfer).
fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    // Build flag string from client config
    let flag_string = build_server_flag_string(config);

    // Receiver uses destination path as args
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Set client_mode since we're a daemon client, not a server.
    // This prevents the context from trying to read filter list
    // (since we'll send it to the daemon after multiplex activation).
    server_config.client_mode = true;
    server_config.filter_rules = filter_rules;

    // Set verbose flag for local output (not sent to daemon in server protocol string)
    server_config.flags.verbose = config.verbosity() > 0;

    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    // Build flag string from client config
    let flag_string = build_server_flag_string(config);

    // Generator uses source paths as args
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Set client_mode since we're a daemon client, not a server.
    // This prevents the context from trying to read filter list
    // (since we'll send it to the daemon after multiplex activation).
    server_config.client_mode = true;
    server_config.filter_rules = filter_rules;

    // Set verbose flag for local output (not sent to daemon in server protocol string)
    server_config.flags.verbose = config.verbosity() > 0;

    Ok(server_config)
}

/// Builds the compact server flag string from client configuration.
///
/// This mirrors ssh_transfer.rs:build_server_flag_string().
fn build_server_flag_string(config: &ClientConfig) -> String {
    let mut flags = String::from("-");

    // Transfer flags (order matches upstream server_options())
    if config.links() {
        flags.push('l');
    }
    if config.preserve_owner() {
        flags.push('o');
    }
    if config.preserve_group() {
        flags.push('g');
    }
    if config.preserve_devices() || config.preserve_specials() {
        flags.push('D');
    }
    if config.preserve_times() {
        flags.push('t');
    }
    if config.preserve_permissions() {
        flags.push('p');
    }
    if config.recursive() {
        flags.push('r');
    }
    if config.compress() {
        flags.push('z');
    }
    if config.checksum() {
        flags.push('c');
    }
    if config.preserve_hard_links() {
        flags.push('H');
    }
    #[cfg(all(unix, feature = "acl"))]
    if config.preserve_acls() {
        flags.push('A');
    }
    #[cfg(all(unix, feature = "xattr"))]
    if config.preserve_xattrs() {
        flags.push('X');
    }
    if config.numeric_ids() {
        flags.push('n');
    }
    if config.delete_mode().is_enabled() || config.delete_excluded() {
        flags.push('d');
    }
    if config.whole_file() {
        flags.push('W');
    }
    if config.sparse() {
        flags.push('S');
    }
    for _ in 0..config.one_file_system_level() {
        flags.push('x');
    }
    if config.relative_paths() {
        flags.push('R');
    }
    if config.partial() {
        flags.push('P');
    }
    if config.update() {
        flags.push('u');
    }
    // Note: verbose flag ('v') is not passed to daemon in server flag string.
    // It's a local output option that doesn't affect protocol behavior.
    // The ServerConfig.flags.verbose is set separately from server_flag_string parsing.

    flags
}

/// Converts client filter rules to wire format.
///
/// Maps FilterRuleSpec (client-side representation) to FilterRuleWireFormat
/// (protocol wire representation) for transmission to the remote server.
fn build_wire_format_rules(
    client_rules: &[FilterRuleSpec],
) -> Result<Vec<FilterRuleWireFormat>, ClientError> {
    let mut wire_rules = Vec::new();

    for spec in client_rules {
        // Convert FilterRuleKind to RuleType
        let rule_type = match spec.kind() {
            FilterRuleKind::Include => RuleType::Include,
            FilterRuleKind::Exclude => RuleType::Exclude,
            FilterRuleKind::Clear => RuleType::Clear,
            FilterRuleKind::Protect => RuleType::Protect,
            FilterRuleKind::Risk => RuleType::Risk,
            FilterRuleKind::DirMerge => RuleType::DirMerge,
            FilterRuleKind::ExcludeIfPresent => {
                // ExcludeIfPresent is transmitted as Exclude with 'e' flag
                // (FILTRULE_EXCLUDE_SELF in upstream rsync)
                wire_rules.push(FilterRuleWireFormat {
                    rule_type: RuleType::Exclude,
                    pattern: spec.pattern().to_owned(),
                    anchored: spec.pattern().starts_with('/'),
                    directory_only: spec.pattern().ends_with('/'),
                    no_inherit: false,
                    cvs_exclude: false,
                    word_split: false,
                    exclude_from_merge: true, // 'e' flag = EXCLUDE_SELF
                    xattr_only: spec.is_xattr_only(),
                    sender_side: spec.applies_to_sender(),
                    receiver_side: spec.applies_to_receiver(),
                    perishable: spec.is_perishable(),
                    negate: false,
                });
                continue;
            }
        };

        // Build wire format rule
        let mut wire_rule = FilterRuleWireFormat {
            rule_type,
            pattern: spec.pattern().to_owned(),
            anchored: spec.pattern().starts_with('/'),
            directory_only: spec.pattern().ends_with('/'),
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: spec.is_xattr_only(),
            sender_side: spec.applies_to_sender(),
            receiver_side: spec.applies_to_receiver(),
            perishable: spec.is_perishable(),
            negate: false,
        };

        // Handle dir_merge options if present
        if let Some(options) = spec.dir_merge_options() {
            wire_rule.no_inherit = !options.inherit_rules();
            wire_rule.word_split = options.uses_whitespace();
            wire_rule.exclude_from_merge = options.excludes_self();
        }

        wire_rules.push(wire_rule);
    }

    Ok(wire_rules)
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
}
