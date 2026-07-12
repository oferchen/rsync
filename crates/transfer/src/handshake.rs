#![deny(unsafe_code)]
//! Server-side protocol handshake utilities.
//!
//! The rsync protocol begins with a version exchange where both sides advertise
//! their supported protocol versions and negotiate a common version to use.
//!
//! For server mode (invoked via `--server`), the sequence is:
//! 1. Write our maximum protocol version to stdout (upstream: compat.c:603)
//! 2. Read the remote's protocol version from stdin (upstream: compat.c:604)
//! 3. Negotiate to the highest mutually supported version
//! 4. Proceed with the negotiated protocol version
//!
//! Both sides write before reading to avoid deadlock when both ends are the
//! same implementation.

use std::io::{self, BufRead, BufReader, Read, Write};

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion, select_highest_mutual};

/// Result of a successful server-side handshake.
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    /// The negotiated protocol version.
    pub protocol: ProtocolVersion,
    /// Any bytes that were buffered during version detection.
    pub buffered: Vec<u8>,
    /// Whether compatibility flags have already been exchanged on the raw stream.
    /// When true, setup_protocol() should skip the compat flags exchange.
    pub compat_exchanged: bool,
    /// Client arguments for daemon mode (includes -e option with capabilities).
    /// None for SSH mode.
    pub client_args: Option<Vec<String>>,
    /// I/O timeout value in seconds for daemon mode.
    /// When Some(_), daemon mode should send MSG_IO_TIMEOUT for protocol >= 31.
    /// None for SSH mode or when no timeout is configured.
    pub io_timeout: Option<u64>,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    /// None for protocols < 30 or when compat exchange was skipped.
    pub compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    /// Generated during setup_protocol() and used when creating XXHash instances.
    pub checksum_seed: i32,
}

/// Performs the server-side protocol version handshake.
///
/// This writes our maximum protocol version, reads the remote's version, and
/// negotiates the highest common version. Returns the negotiated version.
///
/// # Protocol
///
/// Each side sends a 4-byte binary version advertisement:
/// - Byte 0: Protocol version (e.g., 32)
/// - Byte 1: Protocol sub-version (usually 0)
/// - Bytes 2-3: Reserved/compatibility flags
pub fn perform_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<HandshakeResult> {
    perform_handshake_with_max(stdin, stdout, ProtocolVersion::NEWEST)
}

/// Performs the version handshake while advertising at most `max_version`.
///
/// `max_version` caps both the version we advertise and the version we accept
/// as negotiated, mirroring the upstream `--protocol=N` flag. Upstream stores
/// the requested value in `protocol_version` (options.c:846) - already lowered
/// from the `PROTOCOL_VERSION` default - and `setup_protocol()` writes it, reads
/// the peer's version, then clamps with `protocol_version = MIN(protocol_version,
/// remote_protocol)` (compat.c:604-607). Passing `ProtocolVersion::NEWEST` (the
/// default) reproduces the uncapped behaviour.
pub fn perform_handshake_with_max(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    max_version: ProtocolVersion,
) -> io::Result<HandshakeResult> {
    // upstream: compat.c:602-604 - write our max version first, then read.
    // Both sides do this simultaneously; reversing the order deadlocks when
    // both ends are oc-rsync (each waits for the other to write first).
    write_server_version(stdout, max_version)?;
    stdout.flush()?;

    let remote_version = read_client_version(stdin)?;

    let negotiated = select_highest_mutual([remote_version]).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "remote protocol version {} is not supported: {e}",
                remote_version.as_u8()
            ),
        )
    })?;
    // upstream: compat.c:606-607 - protocol_version = MIN(protocol_version,
    // remote_protocol). Clamp the mutually-supported version to our advertised
    // ceiling so `--protocol=N` caps the negotiated version.
    let negotiated = negotiated.min(max_version);

    Ok(HandshakeResult {
        protocol: negotiated,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,           // SSH mode doesn't have daemon client args
        io_timeout: None,            // SSH mode doesn't configure I/O timeouts
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    })
}

/// Reads the client's protocol version from a 4-byte binary advertisement.
fn read_client_version(stdin: &mut dyn Read) -> io::Result<ProtocolVersion> {
    let mut buf = [0u8; 4];
    stdin.read_exact(&mut buf)?;

    let version_byte = buf[0];
    if version_byte == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "received invalid protocol version 0",
        ));
    }

    ProtocolVersion::try_from(version_byte).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid protocol version: {e}"),
        )
    })
}

/// Writes the server's protocol version advertisement.
fn write_server_version(stdout: &mut dyn Write, version: ProtocolVersion) -> io::Result<()> {
    let mut buf = [0u8; 4];
    buf[0] = version.as_u8();
    // buf[1] = sub-version (0 for now)
    // buf[2..4] = reserved/compatibility flags
    stdout.write_all(&buf)
}

/// Performs legacy ASCII-based handshake for older protocol versions.
///
/// Some older rsync clients (protocol < 30) use an ASCII-based greeting format
/// instead of the binary handshake.
pub fn perform_legacy_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<HandshakeResult> {
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let trimmed = line.trim();
    if !trimmed.starts_with("@RSYNCD:") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected @RSYNCD: greeting, got: {trimmed}"),
        ));
    }

    let version_str = trimmed
        .strip_prefix("@RSYNCD:")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "expected @RSYNCD: prefix in greeting",
            )
        })?
        .split_whitespace()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing version in legacy greeting",
            )
        })?;

    let version_number: u8 = version_str
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid legacy protocol version: {version_str}"),
            )
        })?;

    let client_version = ProtocolVersion::try_from(version_number).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported protocol version: {e}"),
        )
    })?;

    let negotiated = select_highest_mutual([client_version]).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protocol version {version_number} is not supported: {e}"),
        )
    })?;

    let response = format!("@RSYNCD: {}.0\n", negotiated.as_u8());
    stdout.write_all(response.as_bytes())?;
    stdout.flush()?;

    Ok(HandshakeResult {
        protocol: negotiated,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,           // SSH mode doesn't have daemon client args
        io_timeout: None,            // SSH mode doesn't configure I/O timeouts
        negotiated_algorithms: None, // Will be populated by setup_protocol()
        compat_flags: None,          // Will be populated by setup_protocol()
        checksum_seed: 0,            // Will be populated by setup_protocol()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn binary_handshake_negotiates_version() {
        // Client sends version 32
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol.as_u8(), 32);

        // Server should respond with 4-byte version
        assert_eq!(stdout.len(), 4);
        assert_eq!(stdout[0], 32);
    }

    #[test]
    fn max_version_caps_negotiated_below_remote() {
        // Remote advertises 32; we cap to 29 via --protocol=29. The negotiated
        // version must clamp to 29 and we must advertise 29 on the wire so the
        // peer's own MIN(remote, ours) converges to 29 too.
        // upstream: compat.c:604-607
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake_with_max(&mut stdin, &mut stdout, ProtocolVersion::V29)
            .expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::V29);
        assert_eq!(stdout[0], 29, "must advertise the capped version");
    }

    #[test]
    fn max_version_does_not_raise_below_remote() {
        // We cap to 32 (default) but the remote only offers 30: negotiate 30.
        let mut stdin = Cursor::new(vec![30, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake_with_max(&mut stdin, &mut stdout, ProtocolVersion::V32)
            .expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::V30);
        assert_eq!(stdout[0], 32);
    }

    #[test]
    fn max_version_30_and_31_are_honored() {
        for (cap, expected) in [(ProtocolVersion::V30, 30u8), (ProtocolVersion::V31, 31u8)] {
            let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
            let mut stdout = Vec::new();
            let result = perform_handshake_with_max(&mut stdin, &mut stdout, cap)
                .expect("handshake succeeds");
            assert_eq!(result.protocol.as_u8(), expected);
            assert_eq!(stdout[0], expected);
        }
    }

    #[test]
    fn default_handshake_advertises_newest() {
        // The no-cap path is byte-identical to advertising NEWEST.
        let mut stdin = Cursor::new(vec![32, 0, 0, 0]);
        let mut stdout = Vec::new();
        let result = perform_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol, ProtocolVersion::NEWEST);
        assert_eq!(stdout[0], ProtocolVersion::NEWEST.as_u8());
    }

    #[test]
    fn binary_handshake_caps_to_supported_version() {
        // Client sends a higher version than we support
        let mut stdin = Cursor::new(vec![99, 0, 0, 0]);
        let mut stdout = Vec::new();

        // Should fail because version 99 is not supported
        let result = perform_handshake(&mut stdin, &mut stdout);
        assert!(result.is_err());
    }

    #[test]
    fn binary_handshake_rejects_version_zero() {
        let mut stdin = Cursor::new(vec![0, 0, 0, 0]);
        let mut stdout = Vec::new();

        let result = perform_handshake(&mut stdin, &mut stdout);
        assert!(result.is_err());
    }

    #[test]
    fn legacy_handshake_parses_greeting() {
        let mut stdin = Cursor::new(b"@RSYNCD: 32.0\n".to_vec());
        let mut stdout = Vec::new();

        let result = perform_legacy_handshake(&mut stdin, &mut stdout).expect("handshake succeeds");
        assert_eq!(result.protocol.as_u8(), 32);

        let response = String::from_utf8_lossy(&stdout);
        assert!(response.starts_with("@RSYNCD: 32"));
    }
}
