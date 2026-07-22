//! Daemon connection establishment and authentication.
//!
//! Handles the rsync daemon handshake protocol: greeting exchange, module
//! selection, MOTD output, authentication (AUTHREQD), and early-input
//! forwarding. Mirrors upstream `clientserver.c:start_inband_exchange()`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use protocol::ProtocolVersion;
use protocol::missing_greeting_token;
use protocol::nstr::{trace_daemon_auth_negotiated, trace_daemon_greeting_auth_list};

use crate::auth::{
    DaemonAuthDigest, compute_daemon_auth_response, parse_daemon_digest_list, select_daemon_digest,
};

use super::super::super::CLIENT_SERVER_PROTOCOL_EXIT_CODE;
use super::super::super::error::{ClientError, daemon_error, socket_error};
use super::super::super::module_list::{DaemonAddress, load_daemon_password};
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

    /// Parses a double-colon daemon operand into a transfer request.
    ///
    /// Format: `[user@]host::module[/path]`
    ///
    /// upstream: `main.c` - `host::module` is equivalent to `rsync://host/module`.
    pub(crate) fn parse_double_colon(operand: &str) -> Result<Self, ClientError> {
        use super::super::super::module_list::parse_host_port;

        let (host_part, module_path) = operand.split_once("::").ok_or_else(|| {
            invalid_argument_error(&format!("not a daemon operand: {operand}"), 1)
        })?;

        let target = parse_host_port(host_part, 873)?;

        let mut path_parts = module_path.splitn(2, '/');
        let module = path_parts.next().unwrap_or("").to_owned();
        let file_path = path_parts.next().unwrap_or("").to_owned();

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
    parse_daemon_digest_list(extract_digest_list_from_greeting(greeting))
}

/// Extracts the raw digest-list slice from a daemon greeting.
///
/// Returns the substring after the protocol version (trimmed of trailing
/// newlines) so callers can emit it verbatim for `--debug=NSTR`
/// diagnostics. Returns `None` when no digest list is present.
fn extract_digest_list_from_greeting(greeting: &str) -> Option<&str> {
    let rest = greeting.get(9..).unwrap_or("");

    let after_version = rest
        .split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest)
        .trim_end_matches(['\r', '\n']);

    if after_version.is_empty() {
        None
    } else {
        Some(after_version)
    }
}

/// Echoes a daemon `@ERROR` line to stderr and constructs the matching
/// client error.
///
/// Mirrors upstream `clientserver.c:382` - `rprintf(FERROR, "%s\n", line)` -
/// which prints the raw `@ERROR` line verbatim to stderr before returning
/// the fatal error to the caller. External tools (and the upstream
/// testsuite/daemon-chroot-acl_test.py GHSA-rjfm-3w2m-jf4f regression
/// among them) match on the `@ERROR` prefix in the client's stderr to
/// confirm the daemon's deny path fired; suppressing the verbatim echo
/// makes those checks fail-OPEN even though the daemon correctly denied
/// the connection.
///
/// The payload is also threaded into a structured `ClientError` so the
/// existing diagnostic envelope (exit code, role, source location) is
/// preserved.
#[cold]
fn handle_daemon_at_error(line: &str) -> ClientError {
    eprintln!("{line}");
    daemon_error(
        line.strip_prefix("@ERROR: ").unwrap_or(line),
        CLIENT_SERVER_PROTOCOL_EXIT_CODE,
    )
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
pub(crate) fn perform_daemon_handshake<R: std::io::Read, W: Write>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    request: &DaemonTransferRequest,
    output_motd: bool,
    daemon_params: &[String],
    early_input: Option<&Path>,
    protocol_override: Option<ProtocolVersion>,
    password_override: Option<&[u8]>,
) -> Result<ProtocolVersion, ClientError> {
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

    // upstream: clientserver.c:188-210 (exchange_protocols, am_client == 1) - a
    // server greeting that omits the subprotocol value (protocol >= 30) or the
    // digest name list (protocol > 31) is fatal. Upstream prints
    // `rsync: the server omitted the <token>: <buf>` to stderr and aborts with
    // RERR_STARTCLIENT. The shared gate mirrors the daemon's `am_client == 0`
    // check so both roles enforce identical thresholds.
    if let Some(missing) = missing_greeting_token(&greeting) {
        return Err(daemon_error(
            format!(
                "the server omitted the {}: {}",
                missing.description(),
                greeting.trim_end_matches(['\r', '\n'])
            ),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
        ));
    }

    let advertised_digests = parse_digest_list_from_greeting(&greeting);

    // upstream: compat.c:843-844 - `am_client && DEBUG_GTE(NSTR, 2)` emits
    // "Client auth list (on client): <list>" using the raw token sequence
    // from `valid_auth_checksums`. The daemon greeting carries the same
    // list verbatim, so we echo whatever the server advertised.
    if let Some(list) = extract_digest_list_from_greeting(&greeting) {
        trace_daemon_greeting_auth_list(list);
    }

    // upstream: compat.c:832-845 - for protocol 30+, client must include
    // supported auth digests.
    let our_version = protocol_override.unwrap_or(ProtocolVersion::NEWEST);
    let client_version = format!(
        "@RSYNCD: {}.0 sha512 sha256 sha1 md5 md4\n",
        our_version.as_u8()
    );
    writer.write_all(client_version.as_bytes()).map_err(|e| {
        socket_error(
            "send client version to",
            request.address.socket_addr_display(),
            e,
        )
    })?;
    writer
        .flush()
        .map_err(|e| socket_error("flush to", request.address.socket_addr_display(), e))?;

    // upstream: clientserver.c:send_daemon_args() - each --dparam key=value is
    // sent as "OPTION key=value\n" before the module name.
    for param in daemon_params {
        let option_line = format!("OPTION {param}\n");
        writer.write_all(option_line.as_bytes()).map_err(|e| {
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
        send_early_input(writer, path, request)?;
    }

    // upstream: clientserver.c:353 - module name is sent BEFORE waiting for @RSYNCD: OK
    let module_request = format!("{}\n", request.module);
    writer.write_all(module_request.as_bytes()).map_err(|e| {
        socket_error(
            "send module request to",
            request.address.socket_addr_display(),
            e,
        )
    })?;
    writer
        .flush()
        .map_err(|e| socket_error("flush to", request.address.socket_addr_display(), e))?;

    // upstream: clientserver.c:357-390 - loop until @RSYNCD: OK, @ERROR, or
    // @RSYNCD: EXIT. Other lines are MOTD output.
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|e| {
            socket_error(
                "read response from",
                request.address.socket_addr_display(),
                e,
            )
        })?;

        // upstream: clientserver.c:359-361 - `read_line_old()` returns 0 (false)
        // when the daemon closes the control socket without a proper terminator,
        // and rsync fails with "didn't get server startup line". A 0-byte read is
        // EOF: without this guard `trim()` yields an empty string that matches no
        // branch below, so the loop would spin forever printing blank MOTD lines.
        if bytes == 0 {
            return Err(daemon_error(
                "connection unexpectedly closed by daemon: didn't get server startup line",
                CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            ));
        }

        let trimmed = line.trim();

        if let Some(challenge) = trimmed.strip_prefix("@RSYNCD: AUTHREQD ") {
            let secret = password_override
                .map(|s| s.to_vec())
                .or_else(load_daemon_password)
                .ok_or_else(|| {
                    daemon_error(
                        "daemon requires authentication but no password source available \
                         (use --password-command, --password-file, or RSYNC_PASSWORD)",
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

            // upstream: compat.c:865-868 - `DEBUG_GTE(NSTR, 1)` emits
            // "Client negotiated auth: <name>" after the strongest mutual
            // digest is selected.
            trace_daemon_auth_negotiated(digest.name());

            // Send auth credentials via the writer (not through BufReader).
            let digest_response = compute_daemon_auth_response(&secret, challenge, digest);
            let auth_line = format!("{username} {digest_response}\n");
            writer.write_all(auth_line.as_bytes()).map_err(|e| {
                socket_error(
                    "send auth credentials to",
                    request.address.socket_addr_display(),
                    e,
                )
            })?;
            writer
                .flush()
                .map_err(|e| socket_error("flush to", request.address.socket_addr_display(), e))?;

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
            return Err(handle_daemon_at_error(trimmed));
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
fn send_early_input<W: Write>(
    writer: &mut W,
    path: &Path,
    request: &DaemonTransferRequest,
) -> Result<(), ClientError> {
    let data = read_early_input_file(path)?;

    if data.is_empty() {
        return Ok(());
    }

    let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
    writer.write_all(header.as_bytes()).map_err(|e| {
        socket_error(
            "send early-input header to",
            request.address.socket_addr_display(),
            e,
        )
    })?;

    writer.write_all(&data).map_err(|e| {
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
