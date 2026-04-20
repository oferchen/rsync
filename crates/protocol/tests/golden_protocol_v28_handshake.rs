//! Golden byte tests for protocol 28 version exchange handshake.
//!
//! Protocol 28 (rsync 3.0.9) uses two distinct handshake modes:
//!
//! 1. **SSH/remote mode**: Both client and server send their protocol version
//!    as a 4-byte little-endian i32 via `write_int()`. No compatibility flags
//!    are exchanged (compat_flags were introduced in protocol 30).
//!
//! 2. **Daemon mode**: ASCII-based `@RSYNCD:` greeting exchange. The server
//!    sends `@RSYNCD: 28.0\n` and the client responds with the same format.
//!
//! In both cases, the negotiated protocol version is `min(client, server)`.
//! Protocol 28 has no binary compat_flags byte - the connection proceeds
//! directly to argument exchange after version numbers are exchanged.
//!
//! # Upstream Reference
//!
//! - SSH mode: `io.c:write_int()` / `read_int()` for version exchange
//! - Daemon mode: `clientserver.c:start_daemon()` sends greeting,
//!   `start_inband_exchange()` parses it
//! - compat.c: compat_flags only exchanged when protocol >= 30

use std::io::Cursor;

use protocol::{
    CompatibilityFlags, ProtocolVersion, format_legacy_daemon_greeting,
    parse_legacy_daemon_greeting, read_int, write_int,
};

// ---------------------------------------------------------------------------
// SSH/remote mode: protocol version exchange as 4-byte LE i32
// upstream: io.c - write_int(f, protocol_version) on both sides
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_ssh_client_version_advertisement() {
    // Client sends its protocol version as 4-byte LE i32.
    // Protocol 28 = 0x0000001C in LE: [0x1C, 0x00, 0x00, 0x00]
    let mut buf = Vec::new();
    write_int(&mut buf, 28).unwrap();

    assert_eq!(
        buf.len(),
        4,
        "version advertisement must be exactly 4 bytes"
    );
    assert_eq!(
        buf,
        [0x1C, 0x00, 0x00, 0x00],
        "protocol 28 must encode as LE i32"
    );
}

#[test]
fn golden_v28_ssh_server_version_advertisement() {
    // Server sends its protocol version identically to client.
    // When server speaks protocol 28, it sends the same 4 bytes.
    let mut buf = Vec::new();
    write_int(&mut buf, 28).unwrap();

    assert_eq!(
        buf,
        [0x1C, 0x00, 0x00, 0x00],
        "server protocol 28 advertisement must match client format"
    );

    // Parse it back as the client would
    let mut cursor = Cursor::new(&buf);
    let version = read_int(&mut cursor).unwrap();
    assert_eq!(version, 28);
}

#[test]
fn golden_v28_ssh_version_exchange_full_sequence() {
    // Full SSH handshake wire sequence for protocol 28:
    // 1. Server writes: write_int(28) = [0x1C, 0x00, 0x00, 0x00]
    // 2. Client writes: write_int(28) = [0x1C, 0x00, 0x00, 0x00]
    // 3. No compat_flags exchange (protocol < 30)
    // 4. Negotiated version = min(28, 28) = 28
    //
    // Total handshake bytes per direction: exactly 4 bytes.
    // No additional bytes follow the version number for protocol 28.

    // Server side
    let mut server_out = Vec::new();
    write_int(&mut server_out, 28).unwrap();
    assert_eq!(server_out, [0x1C, 0x00, 0x00, 0x00]);

    // Client side
    let mut client_out = Vec::new();
    write_int(&mut client_out, 28).unwrap();
    assert_eq!(client_out, [0x1C, 0x00, 0x00, 0x00]);

    // Client reads server version
    let mut cursor = Cursor::new(&server_out);
    let server_version = read_int(&mut cursor).unwrap();
    assert_eq!(server_version, 28);

    // Server reads client version
    let mut cursor = Cursor::new(&client_out);
    let client_version = read_int(&mut cursor).unwrap();
    assert_eq!(client_version, 28);

    // Negotiated version is min(client, server)
    let negotiated = server_version.min(client_version);
    assert_eq!(negotiated, 28);

    // Verify ProtocolVersion can be constructed from the negotiated value
    let protocol = ProtocolVersion::from_peer_advertisement(negotiated as u32).unwrap();
    assert_eq!(protocol, ProtocolVersion::V28);
}

#[test]
fn golden_v28_ssh_version_downgrade_from_v32_server() {
    // When a protocol 32 server talks to a protocol 28 client:
    // Server sends: write_int(32) = [0x20, 0x00, 0x00, 0x00]
    // Client sends: write_int(28) = [0x1C, 0x00, 0x00, 0x00]
    // Negotiated: min(32, 28) = 28

    let mut server_out = Vec::new();
    write_int(&mut server_out, 32).unwrap();
    assert_eq!(server_out, [0x20, 0x00, 0x00, 0x00]);

    let mut client_out = Vec::new();
    write_int(&mut client_out, 28).unwrap();
    assert_eq!(client_out, [0x1C, 0x00, 0x00, 0x00]);

    // Parse versions
    let mut cursor = Cursor::new(&server_out);
    let server_version = read_int(&mut cursor).unwrap();

    let mut cursor = Cursor::new(&client_out);
    let client_version = read_int(&mut cursor).unwrap();

    let negotiated = server_version.min(client_version);
    assert_eq!(negotiated, 28, "must downgrade to protocol 28");

    let protocol = ProtocolVersion::from_peer_advertisement(negotiated as u32).unwrap();
    assert_eq!(protocol, ProtocolVersion::V28);
    assert!(protocol.uses_legacy_ascii_negotiation());
    assert!(!protocol.uses_binary_negotiation());
}

// ---------------------------------------------------------------------------
// Protocol 28 has NO compat_flags exchange
// upstream: compat.c - compat_flags only exchanged when protocol >= 30
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_no_compat_flags_on_wire() {
    // Protocol 28 does NOT send any compatibility flags byte.
    // The handshake is complete after the 4-byte version exchange.
    // This is a critical difference from protocol 30+ which sends a varint
    // compat_flags byte after the version number.
    let v28 = ProtocolVersion::V28;

    // Protocol 28 uses legacy negotiation - no binary compat flags
    assert!(
        v28.uses_legacy_ascii_negotiation(),
        "protocol 28 must use legacy negotiation (no compat_flags)"
    );
    assert!(
        !v28.uses_binary_negotiation(),
        "protocol 28 must not use binary negotiation"
    );

    // The effective compat_flags for protocol 28 are always EMPTY (0x00)
    let flags = CompatibilityFlags::EMPTY;
    assert_eq!(flags.bits(), 0, "protocol 28 compat_flags must be zero");
    assert!(
        !flags.contains(CompatibilityFlags::INC_RECURSE),
        "protocol 28 must not have INC_RECURSE"
    );
    assert!(
        !flags.contains(CompatibilityFlags::SYMLINK_TIMES),
        "protocol 28 must not have SYMLINK_TIMES"
    );
    assert!(
        !flags.contains(CompatibilityFlags::SAFE_FILE_LIST),
        "protocol 28 must not have SAFE_FILE_LIST"
    );
}

// ---------------------------------------------------------------------------
// Daemon mode: ASCII greeting exchange
// upstream: clientserver.c - "@RSYNCD: 28.0\n"
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_daemon_server_greeting_bytes() {
    // Server sends ASCII greeting: "@RSYNCD: 28.0\n"
    // This is exactly 14 bytes of ASCII text.
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);

    #[rustfmt::skip]
    let expected: &[u8] = &[
        b'@', b'R', b'S', b'Y', b'N', b'C', b'D', b':', b' ',  // prefix (9 bytes)
        b'2', b'8',                                               // version (2 bytes)
        b'.', b'0',                                               // sub-version (2 bytes)
        b'\n',                                                    // terminator (1 byte)
    ];

    assert_eq!(greeting.as_bytes(), expected);
    assert_eq!(greeting.len(), 14, "greeting must be exactly 14 bytes");
}

#[test]
fn golden_v28_daemon_client_greeting_bytes() {
    // Client also sends the same greeting format back to the server.
    // upstream: clientserver.c - client responds with its version in same format.
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);

    assert_eq!(greeting, "@RSYNCD: 28.0\n");
    assert_eq!(
        greeting.as_bytes(),
        b"@RSYNCD: 28.0\n",
        "client greeting must be identical to server greeting format"
    );
}

#[test]
fn golden_v28_daemon_greeting_roundtrip() {
    // Server sends greeting, client parses it to extract protocol version.
    let server_greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
    let parsed_version = parse_legacy_daemon_greeting(&server_greeting).unwrap();

    assert_eq!(parsed_version, ProtocolVersion::V28);
    assert_eq!(parsed_version.as_u8(), 28);
}

#[test]
fn golden_v28_daemon_full_handshake_sequence() {
    // Full daemon handshake wire sequence for protocol 28:
    //
    // Server -> Client: "@RSYNCD: 28.0\n" (14 bytes)
    // Client -> Server: "@RSYNCD: 28.0\n" (14 bytes)
    //
    // After greeting exchange, the negotiated version is min(server, client) = 28.
    // No compat_flags follow. The next phase is module selection.

    // Server greeting
    let server_greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
    assert_eq!(server_greeting.as_bytes(), b"@RSYNCD: 28.0\n");

    // Client greeting (same format)
    let client_greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
    assert_eq!(client_greeting.as_bytes(), b"@RSYNCD: 28.0\n");

    // Both sides parse the other's greeting
    let server_ver = parse_legacy_daemon_greeting(&server_greeting).unwrap();
    let client_ver = parse_legacy_daemon_greeting(&client_greeting).unwrap();

    assert_eq!(server_ver, ProtocolVersion::V28);
    assert_eq!(client_ver, ProtocolVersion::V28);
}

#[test]
fn golden_v28_daemon_downgrade_from_v32_server() {
    // When a protocol 32 daemon talks to a protocol 28 client:
    // Server sends: "@RSYNCD: 32.0\n"
    // Client sends: "@RSYNCD: 28.0\n"
    // Negotiated: min(32, 28) = 28

    let server_greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
    let client_greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);

    assert_eq!(server_greeting.as_bytes(), b"@RSYNCD: 32.0\n");
    assert_eq!(client_greeting.as_bytes(), b"@RSYNCD: 28.0\n");

    let server_ver = parse_legacy_daemon_greeting(&server_greeting).unwrap();
    let client_ver = parse_legacy_daemon_greeting(&client_greeting).unwrap();

    // Negotiated version is min of both
    let negotiated = std::cmp::min(server_ver.as_u8(), client_ver.as_u8());
    assert_eq!(negotiated, 28, "must negotiate down to protocol 28");
}

// ---------------------------------------------------------------------------
// Contrast with protocol 30+ handshake (compat_flags present)
// upstream: compat.c - protocol 30+ adds compat_flags after version exchange
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_vs_v30_handshake_difference() {
    // Protocol 28 SSH handshake: 4 bytes total (version only)
    // Protocol 30 SSH handshake: 4 bytes (version) + N bytes (compat_flags varint)
    //
    // This test documents the structural difference.

    // Protocol 28: just the version, no flags
    let mut v28_handshake = Vec::new();
    write_int(&mut v28_handshake, 28).unwrap();
    assert_eq!(
        v28_handshake.len(),
        4,
        "v28 handshake is 4 bytes (no flags)"
    );
    assert_eq!(v28_handshake, [0x1C, 0x00, 0x00, 0x00]);

    // Protocol 30: version + compat_flags
    let mut v30_handshake = Vec::new();
    write_int(&mut v30_handshake, 30).unwrap();
    // Typical server flags: INC_RECURSE | SYMLINK_TIMES | SAFE_FILE_LIST | CHECKSUM_SEED_FIX
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::CHECKSUM_SEED_FIX;
    flags.encode_to_vec(&mut v30_handshake).unwrap();
    assert_eq!(
        v30_handshake.len(),
        5,
        "v30 handshake is 5 bytes (version + 1-byte flags)"
    );
    assert_eq!(v30_handshake, [0x1E, 0x00, 0x00, 0x00, 0x2B]);

    // The key difference: protocol 28 has NO trailing flags byte
    assert!(
        v28_handshake.len() < v30_handshake.len(),
        "protocol 28 handshake must be shorter than protocol 30"
    );
}

#[test]
fn golden_v28_version_byte_layout() {
    // Document the exact byte-level layout of protocol 28 version exchange.
    //
    // Wire format (SSH mode, each direction):
    //   Offset 0: version LSB = 0x1C (28 decimal)
    //   Offset 1: 0x00
    //   Offset 2: 0x00
    //   Offset 3: version MSB = 0x00
    //
    // Total: 4 bytes per direction, 8 bytes for complete handshake.
    // No additional data follows in protocol 28.

    let version_bytes = 28_i32.to_le_bytes();
    assert_eq!(version_bytes[0], 0x1C, "byte 0: version LSB = 28");
    assert_eq!(version_bytes[1], 0x00, "byte 1: zero");
    assert_eq!(version_bytes[2], 0x00, "byte 2: zero");
    assert_eq!(version_bytes[3], 0x00, "byte 3: version MSB = 0");

    // Verify via write_int produces identical result
    let mut buf = Vec::new();
    write_int(&mut buf, 28).unwrap();
    assert_eq!(buf, version_bytes.to_vec());
}
