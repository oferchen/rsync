#![deny(unsafe_code)]
//! Server-side protocol handshake utilities.
//!
//! The rsync protocol begins with a version exchange where both sides advertise
//! their supported protocol versions and negotiate a common version to use.
//!
//! For server mode (invoked via `--server`), the sequence is:
//! 1. Read the client's protocol version advertisement from stdin
//! 2. Select the highest mutually supported version
//! 3. Write our version advertisement to stdout
//! 4. Proceed with the negotiated protocol version

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
/// This reads the client's version advertisement, selects a common version,
/// and writes our response. Returns the negotiated version.
///
/// # Protocol
///
/// The client sends a 4-byte binary version advertisement:
/// - Byte 0: Protocol version (e.g., 32)
/// - Byte 1: Protocol sub-version (usually 0)
/// - Bytes 2-3: Reserved/compatibility flags
///
/// The server responds with the same format.
pub fn perform_handshake(
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<HandshakeResult> {
    // Read the client's version advertisement
    let client_version = read_client_version(stdin)?;

    // Select highest mutually supported version
    let negotiated = select_highest_mutual([client_version]).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "client protocol version {} is not supported by this server: {e}",
                client_version.as_u8()
            ),
        )
    })?;

    // Write our version advertisement
    write_server_version(stdout, negotiated)?;
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

/// Reads the client's protocol version from a 4-byte binary advertisement.
fn read_client_version(stdin: &mut dyn Read) -> io::Result<ProtocolVersion> {
    let mut buf = [0u8; 4];
    stdin.read_exact(&mut buf)?;

    // The first byte is the protocol version
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

    // Parse the legacy greeting line
    // Format: @RSYNCD: <version>\n
    let trimmed = line.trim();
    if !trimmed.starts_with("@RSYNCD:") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected @RSYNCD: greeting, got: {trimmed}"),
        ));
    }

    let version_str = trimmed
        .strip_prefix("@RSYNCD:")
        .unwrap()
        .split_whitespace()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing version in legacy greeting",
            )
        })?;

    // Parse version (may include sub-version like "32.0")
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

    // Respond with our version in legacy format
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
