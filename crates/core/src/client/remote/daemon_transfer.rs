//! Daemon transfer orchestration.
//!
//! This module coordinates daemon-based remote transfers (rsync:// URLs) by
//! connecting to rsync daemons, performing handshakes, and executing transfers
//! using the server infrastructure.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use protocol::ProtocolVersion;

use super::super::config::ClientConfig;
use super::super::error::{ClientError, daemon_error, invalid_argument_error, socket_error};
use super::super::module_list::{DaemonAddress, connect_direct, parse_host_port};
use super::super::progress::ClientProgressObserver;
use super::super::summary::ClientSummary;
use super::super::{CLIENT_SERVER_PROTOCOL_EXIT_CODE, DAEMON_SOCKET_TIMEOUT};

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
        let module = path_parts.next().unwrap_or("").to_string();
        let file_path = path_parts.next().unwrap_or("").to_string();

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
/// 5. Executes the transfer using server infrastructure
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
pub fn run_daemon_transfer(
    config: &ClientConfig,
    _observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    // Find the rsync:// URL operand
    let daemon_url = config
        .transfer_args()
        .iter()
        .find(|arg| {
            let s = arg.to_string_lossy();
            s.starts_with("rsync://") || s.starts_with("RSYNC://")
        })
        .ok_or_else(|| invalid_argument_error("no rsync:// URL found", 1))?;

    // Parse the URL
    let request = DaemonTransferRequest::parse_rsync_url(&daemon_url.to_string_lossy())?;

    // Connect to daemon
    let mut stream = connect_direct(
        &request.address,
        None, // No custom connect timeout
        Some(DAEMON_SOCKET_TIMEOUT),
        config.address_mode(),
        None, // No bind address
    )?;

    // Perform daemon handshake
    perform_daemon_handshake(&mut stream, &request)?;

    // TODO: Build ServerConfig and execute transfer
    // For now, return error indicating not fully implemented
    Err(daemon_error(
        "daemon data transfer implementation incomplete",
        1,
    ))
}

/// Performs the rsync daemon handshake protocol.
///
/// Exchanges version information and requests the specified module.
fn perform_daemon_handshake(
    stream: &mut TcpStream,
    request: &DaemonTransferRequest,
) -> Result<(), ClientError> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| socket_error("clone", request.address.socket_addr_display(), e))?,
    );

    // Read daemon greeting: @RSYNCD: 31.0
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

    // Send client version: @RSYNCD: 31.0
    let client_version = format!("@RSYNCD: {}.0\n", ProtocolVersion::NEWEST.as_u8());
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

    // Read handshake acknowledgment: @RSYNCD: OK
    let mut ack = String::new();
    reader.read_line(&mut ack).map_err(|e| {
        socket_error(
            "read handshake ack from",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    if ack.trim() != "@RSYNCD: OK" {
        return Err(daemon_error(
            format!("unexpected handshake response: {ack}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        ));
    }

    // Request the module
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

    // Read module response
    let mut response = String::new();
    reader.read_line(&mut response).map_err(|e| {
        socket_error(
            "read module response from",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    // Check for errors
    if response.starts_with("@ERROR") {
        return Err(daemon_error(
            response
                .trim()
                .strip_prefix("@ERROR: ")
                .unwrap_or(&response),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        ));
    }

    // Success - module accepted
    Ok(())
}
