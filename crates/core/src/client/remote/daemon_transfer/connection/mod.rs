//! Daemon connection establishment and authentication.
//!
//! Handles the rsync daemon handshake protocol: greeting exchange, module
//! selection, MOTD output, authentication (AUTHREQD), and early-input
//! forwarding. Mirrors upstream `clientserver.c:start_inband_exchange()`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;

use protocol::ProtocolVersion;

use crate::auth::{DaemonAuthDigest, parse_daemon_digest_list, select_daemon_digest};

use super::super::super::CLIENT_SERVER_PROTOCOL_EXIT_CODE;
use super::super::super::error::{ClientError, daemon_error, socket_error};
use super::super::super::module_list::{
    DaemonAddress, DaemonAuthContext, load_daemon_password, send_daemon_auth_credentials,
};
use crate::client::error::invalid_argument_error;

/// Parsed daemon transfer request containing connection and path details.
#[derive(Clone, Debug)]
pub(crate) struct DaemonTransferRequest {
    pub(crate) address: DaemonAddress,
    pub(crate) module: String,
    pub(crate) path: String,
    pub(crate) username: Option<String>,
}

impl DaemonTransferRequest {
    /// Parses an rsync:// URL into a transfer request.
    ///
    /// Format: `rsync://[user@]host[:port]/module/path`
    pub(crate) fn parse_rsync_url(url: &str) -> Result<Self, ClientError> {
        use super::super::super::module_list::parse_host_port;

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

/// Parses the protocol version from an `@RSYNCD` greeting line.
///
/// Format: `@RSYNCD: XX.Y [digest_list]`
///
/// upstream: exchange_protocols line 178 - `sscanf(buf, "@RSYNCD: %d.%d", ...)`
fn parse_protocol_from_greeting(greeting: &str) -> Result<ProtocolVersion, ClientError> {
    let rest = greeting.get(9..).ok_or_else(|| {
        daemon_error(
            format!("malformed greeting: {greeting}"),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        )
    })?;

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
/// Format: `@RSYNCD: XX.Y [digest_list]`
///
/// Returns the list of advertised digests for authentication.
fn parse_digest_list_from_greeting(greeting: &str) -> Vec<DaemonAuthDigest> {
    let rest = greeting.get(9..).unwrap_or("");

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
/// Follows upstream `clientserver.c:start_inband_exchange()`:
/// 1. Read daemon greeting (`@RSYNCD: XX.Y`)
/// 2. Send client greeting (`@RSYNCD: XX.Y`)
/// 3. Send module name
/// 4. Read response lines (MOTD, `@RSYNCD: OK` / `@RSYNCD: AUTHREQD` / `@ERROR`)
///
/// Returns the negotiated protocol version.
///
/// When `output_motd` is true, MOTD lines are printed to stdout, mirroring
/// upstream rsync's `output_motd` global variable.
pub(crate) fn perform_daemon_handshake(
    stream: &mut TcpStream,
    request: &DaemonTransferRequest,
    output_motd: bool,
    daemon_params: &[String],
    early_input: Option<&Path>,
    protocol_override: Option<ProtocolVersion>,
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

    // upstream: exchange_protocols line 178 - sscanf(buf, "@RSYNCD: %d.%d", ...)
    let remote_protocol = parse_protocol_from_greeting(&greeting)?;
    let advertised_digests = parse_digest_list_from_greeting(&greeting);

    // upstream: compat.c:832-845 - for protocol 30+, client must include
    // supported auth digests.
    let our_version = protocol_override.unwrap_or(ProtocolVersion::NEWEST);
    let client_version = format!(
        "@RSYNCD: {}.0 sha512 sha256 sha1 md5 md4\n",
        our_version.as_u8()
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

    // upstream: clientserver.c:send_daemon_args() - each --dparam key=value is
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

    // upstream: clientserver.c:266-294 - sends `#early_input=<len>\n` followed by
    // the raw file contents before the module name.
    if let Some(path) = early_input {
        send_early_input(stream, path, request)?;
    }

    // upstream: clientserver.c:351 - module name is sent BEFORE waiting for @RSYNCD: OK
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

    // upstream: clientserver.c:357-390 - loop until @RSYNCD: OK, @ERROR, or
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

        if let Some(challenge) = trimmed.strip_prefix("@RSYNCD: AUTHREQD ") {
            let secret = load_daemon_password().ok_or_else(|| {
                daemon_error(
                    "daemon requires authentication but RSYNC_PASSWORD not set",
                    CLIENT_SERVER_PROTOCOL_EXIT_CODE,
                )
            })?;

            let username = request.username.clone().unwrap_or_else(|| {
                std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "rsync".to_owned())
            });

            // upstream: compat.c:858 - fallback depends on protocol version
            let digest = select_daemon_digest(&advertised_digests, remote_protocol.as_u8());

            let auth_context = DaemonAuthContext::new(username, secret, digest);
            send_daemon_auth_credentials(&mut reader, &auth_context, challenge, &request.address)?;

            continue;
        }

        if trimmed == "@RSYNCD: OK" {
            break;
        }

        if trimmed == "@RSYNCD: EXIT" {
            return Err(daemon_error(
                "daemon closed connection",
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            ));
        }

        if trimmed.starts_with("@ERROR") {
            return Err(daemon_error(
                trimmed.strip_prefix("@ERROR: ").unwrap_or(trimmed),
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            ));
        }

        // upstream: rprintf(FINFO, "%s\n", line) - MOTD output.
        if output_motd {
            println!("{trimmed}");
        }
    }

    // upstream: exchange_protocols:211-227 - negotiate to minimum of our and daemon version.
    let negotiated = if our_version.as_u8() < remote_protocol.as_u8() {
        our_version
    } else {
        remote_protocol
    };

    Ok(negotiated)
}

/// Maximum early-input file size in bytes.
///
/// Upstream rsync limits the file to `BIGPATHBUFLEN` (typically 5120 bytes on
/// systems where `MAXPATHLEN >= 4096`). The manpage documents this as "up to
/// 5K of data".
///
/// upstream: rsync.h - `BIGPATHBUFLEN` is `MAXPATHLEN + 1024` or `4096 + 1024`.
pub(crate) const EARLY_INPUT_MAX_SIZE: usize = 5120;

/// Command prefix for the early-input protocol message.
///
/// upstream: clientserver.c - `#define EARLY_INPUT_CMD "#early_input="`
const EARLY_INPUT_CMD: &str = "#early_input=";

/// Reads the early-input file content, capping at [`EARLY_INPUT_MAX_SIZE`] bytes.
///
/// Returns the file content truncated to 5120 bytes if the file is larger.
/// Returns an empty `Vec` for empty files.
///
/// upstream: clientserver.c:266-294 - `start_inband_exchange()` reads the file
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

/// Sends the early-input file to the daemon before the module name.
///
/// The data is sent as `#early_input=<len>\n` followed by the raw file bytes.
/// The daemon receives this in `rsync_module()` and passes it to the pre-xfer
/// exec script on stdin.
///
/// upstream: clientserver.c:266-294 - `start_inband_exchange()` sends the early
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

#[cfg(test)]
mod tests;
