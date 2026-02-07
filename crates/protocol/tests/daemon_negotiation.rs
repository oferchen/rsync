//! Integration tests for capability negotiation in various modes.
//!
//! # Daemon Mode (Unidirectional)
//! - Server: SENDS algorithm lists, uses defaults locally
//! - Client: READS algorithm lists, selects from them (no send)
//!
//! # SSH Mode (Bidirectional)
//! - Both sides SEND their algorithm lists first
//! - Then both sides READ each other's lists
//! - Both independently select the first match from the remote's list
//!
//! # Legacy Protocol (< 30) or do_negotiation=false
//! - Neither side sends or reads anything
//! - Protocol defaults are used

use protocol::{ProtocolVersion, negotiate_capabilities};
use std::io::{Read, Write};

// ============================================================================
// Helper Functions
// ============================================================================

/// Writes a vstring (variable-length string) using upstream rsync's format.
fn write_vstring(writer: &mut impl Write, s: &str) -> std::io::Result<()> {
    let bytes = s.as_bytes();
    let len = bytes.len();

    let len_bytes = if len > 0x7F {
        let high = ((len >> 8) as u8) | 0x80;
        let low = (len & 0xFF) as u8;
        vec![high, low]
    } else {
        vec![len as u8]
    };

    writer.write_all(&len_bytes)?;
    writer.write_all(bytes)?;
    Ok(())
}

/// Reads a vstring from a buffer.
fn read_vstring(reader: &mut impl Read) -> std::io::Result<String> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;

    let len = if first[0] & 0x80 != 0 {
        let high = (first[0] & 0x7F) as usize;
        let mut second = [0u8; 1];
        reader.read_exact(&mut second)?;
        high * 256 + second[0] as usize
    } else {
        first[0] as usize
    };

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ============================================================================
// Daemon Mode Unidirectional Flow Tests
// ============================================================================

#[test]
fn test_bidirectional_negotiation_exchange() {
    // In DAEMON mode, the exchange is UNIDIRECTIONAL:
    // 1. Server sends its algorithm lists and uses defaults
    // 2. Client reads server's lists and selects from them
    // 3. Client does NOT send anything back
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server side - sends its lists, uses defaults (no read)
    let mut server_output = Vec::new();
    let mut server_input = &b""[..]; // Empty - server doesn't read in daemon mode

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true, // do_negotiation
        true, // send_compression
        true, // is_daemon_mode
        true, // is_server
    )
    .expect("server negotiation should succeed");

    // Verify server sent data
    assert!(
        !server_output.is_empty(),
        "daemon server must send algorithm lists"
    );

    // Server uses defaults (first in preference list)
    assert_eq!(server_result.checksum.as_str(), "xxh128");

    // Client side - reads server's lists, does NOT send
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server (client)
    )
    .expect("client negotiation should succeed");

    // Daemon client does NOT send in unidirectional exchange
    assert!(
        client_output.is_empty(),
        "daemon client should NOT send algorithm lists (unidirectional)"
    );

    // Both should have valid results
    assert!(!server_result.checksum.as_str().is_empty());
    assert!(!client_result.checksum.as_str().is_empty());

    // Client selected from server's list (xxh128)
    assert_eq!(client_result.checksum.as_str(), "xxh128");
}

#[test]
fn test_daemon_client_selects_first_supported_algorithm() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends specific algorithm lists
    let mut server_data = Vec::new();
    write_vstring(&mut server_data, "xxh128 xxh3 md5 md4 sha1 none").unwrap();
    write_vstring(&mut server_data, "zstd lz4 zlibx zlib none").unwrap();

    let mut stdin = &server_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        true,  // daemon mode
        false, // client
    )
    .unwrap();

    // Client should select first mutually supported from server's list
    // xxh128 is first and we support it
    assert_eq!(result.checksum.as_str(), "xxh128");
}

#[test]
fn test_daemon_client_falls_back_when_first_unsupported() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends list with unknown algorithm first
    let mut server_data = Vec::new();
    write_vstring(&mut server_data, "blake3 sha256 md5 md4").unwrap(); // blake3 unsupported
    write_vstring(&mut server_data, "zlibx zlib none").unwrap();

    let mut stdin = &server_data[..];
    let mut stdout = Vec::new();

    let result =
        negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, true, false).unwrap();

    // Should fall back to md5 (first we support)
    assert_eq!(result.checksum.as_str(), "md5");
    assert_eq!(result.compression.as_str(), "zlibx");
}

#[test]
fn test_negotiation_without_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends only checksum list (no -z flag) in daemon mode
    let mut server_data = Vec::new();
    write_vstring(&mut server_data, "md5 md4").unwrap();
    // No compression list since send_compression=false

    let mut stdin = &server_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        false, // send_compression = false
        true,  // daemon mode
        false, // client
    )
    .unwrap();

    assert_eq!(result.checksum.as_str(), "md5");
    assert_eq!(result.compression.as_str(), "none");

    // Daemon client does NOT send anything (unidirectional)
    assert!(stdout.is_empty(), "daemon client should not send anything");
}

// ============================================================================
// SSH Mode Bidirectional Flow Tests (for comparison)
// ============================================================================

#[test]
fn test_ssh_mode_bidirectional_exchange() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // In SSH mode, both sides send and receive
    // Simulate remote sending its lists
    let mut remote_data = Vec::new();
    write_vstring(&mut remote_data, "xxh128 md5 md4").unwrap();
    write_vstring(&mut remote_data, "zlib none").unwrap();

    let mut stdin = &remote_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        false, // SSH mode (not daemon)
        true,
    )
    .unwrap();

    // SSH mode SHOULD send data
    assert!(!stdout.is_empty(), "SSH mode must send algorithm lists");

    // Verify we can parse what we sent
    let mut our_output = &stdout[..];
    let checksum_list = read_vstring(&mut our_output).unwrap();
    let compression_list = read_vstring(&mut our_output).unwrap();

    assert!(checksum_list.contains("xxh128"), "should include xxh128");
    assert!(checksum_list.contains("md5"), "should include md5");
    assert!(
        compression_list.contains("zlib") || compression_list.contains("zlibx"),
        "should include zlib variant"
    );

    // Result should select from remote's list
    assert_eq!(result.checksum.as_str(), "xxh128");
}

// ============================================================================
// Protocol Version Edge Cases
// ============================================================================

#[test]
fn test_protocol_29_no_negotiation() {
    // Protocol < 30 doesn't support algorithm negotiation
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        true, // daemon mode
        false,
    )
    .unwrap();

    // Should use legacy defaults without any I/O
    assert_eq!(result.checksum.as_str(), "md4");
    assert_eq!(result.compression.as_str(), "zlib");
    assert!(stdout.is_empty(), "protocol 29 should not exchange lists");
}

#[test]
fn test_do_negotiation_false_uses_defaults() {
    // When do_negotiation=false (client lacks VARINT_FLIST_FLAGS)
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        false, // do_negotiation = false
        true,
        true,
        false,
    )
    .unwrap();

    // Should use protocol 30+ defaults
    assert_eq!(result.checksum.as_str(), "md5");
    assert_eq!(result.compression.as_str(), "none");
    assert!(stdout.is_empty());
}

// ============================================================================
// Vstring Format Tests
// ============================================================================

#[test]
fn test_vstring_roundtrip_short() {
    let original = "md5 md4 sha1";
    let mut buf = Vec::new();
    write_vstring(&mut buf, original).unwrap();

    // Short string uses 1-byte length
    assert_eq!(buf[0] as usize, original.len());

    let mut reader = &buf[..];
    let decoded = read_vstring(&mut reader).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn test_vstring_roundtrip_long() {
    // String > 127 bytes uses 2-byte length format
    let original = "x".repeat(200);
    let mut buf = Vec::new();
    write_vstring(&mut buf, &original).unwrap();

    // Long string uses 2-byte format with high bit set
    assert!(
        buf[0] & 0x80 != 0,
        "high bit should be set for long strings"
    );

    let mut reader = &buf[..];
    let decoded = read_vstring(&mut reader).unwrap();
    assert_eq!(decoded, original);
}

// ============================================================================
// End-to-End Compression Negotiation Tests
// ============================================================================

/// Tests full round-trip daemon negotiation with zstd compression enabled.
/// Server writes capabilities to buffer, client reads and selects zstd.
#[test]
fn test_daemon_e2e_with_zstd_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Step 1: Server-side negotiation (sends to buffer)
    let mut server_output = Vec::new();
    let mut server_input = &b""[..]; // Server doesn't read in daemon mode

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true, // do_negotiation
        true, // send_compression
        true, // is_daemon_mode
        true, // is_server
    )
    .expect("server negotiation should succeed");

    // Verify server sent data
    assert!(
        !server_output.is_empty(),
        "server must send algorithm lists"
    );

    // Step 2: Client-side negotiation (reads from buffer)
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,  // do_negotiation
        true,  // send_compression
        true,  // is_daemon_mode
        false, // is_server (client)
    )
    .expect("client negotiation should succeed");

    // Verify client didn't send (daemon mode is unidirectional)
    assert!(client_output.is_empty(), "daemon client should not send");

    // Verify both sides agree on algorithms
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");

    // Server uses first in preference list (zstd if available)
    #[cfg(feature = "zstd")]
    assert_eq!(server_result.compression.as_str(), "zstd");
    #[cfg(feature = "zstd")]
    assert_eq!(client_result.compression.as_str(), "zstd");

    // Without zstd feature, falls back to zlibx
    #[cfg(not(feature = "zstd"))]
    assert_eq!(server_result.compression.as_str(), "zlibx");
    #[cfg(not(feature = "zstd"))]
    assert_eq!(client_result.compression.as_str(), "zlibx");
}

/// Tests full round-trip daemon negotiation without compression.
/// Verifies that send_compression=false results in "none" compression.
#[test]
fn test_daemon_e2e_without_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Step 1: Server-side negotiation without compression
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,  // do_negotiation
        false, // send_compression = false (no -z flag)
        true,  // is_daemon_mode
        true,  // is_server
    )
    .expect("server negotiation should succeed");

    // Step 2: Client-side negotiation
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,  // do_negotiation
        false, // send_compression = false
        true,  // is_daemon_mode
        false, // is_server
    )
    .expect("client negotiation should succeed");

    // Verify both sides agree: checksum negotiated, compression is "none"
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
    assert_eq!(server_result.compression.as_str(), "none");
    assert_eq!(client_result.compression.as_str(), "none");
}

/// Tests daemon negotiation with protocol version 30.
/// Protocol 30 supports negotiation but may have different defaults.
#[test]
fn test_daemon_e2e_protocol_30() {
    let protocol = ProtocolVersion::try_from(30).unwrap();

    // Server-side
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .expect("server negotiation should succeed");

    // Client-side
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .expect("client negotiation should succeed");

    // Protocol 30 supports full negotiation
    assert!(!server_output.is_empty());
    assert!(client_output.is_empty(), "daemon client doesn't send");

    // Both should agree on algorithms
    assert_eq!(server_result.checksum, client_result.checksum);
    assert_eq!(server_result.compression, client_result.compression);
}

/// Tests daemon negotiation with protocol version 31.
/// Verifies latest protocol version works correctly.
#[test]
fn test_daemon_e2e_protocol_31() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server-side
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .expect("server negotiation should succeed");

    // Client-side
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .expect("client negotiation should succeed");

    // Verify successful negotiation
    assert!(!server_output.is_empty());
    assert!(client_output.is_empty());
    assert_eq!(server_result.checksum, client_result.checksum);
    assert_eq!(server_result.compression, client_result.compression);
}

/// Tests that both server and client agree on the same checksum algorithm.
/// Verifies xxh128 is selected as the first mutually supported algorithm.
#[test]
fn test_daemon_e2e_checksum_agreement() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server-side
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    // Client-side
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .unwrap();

    // Both should select xxh128 (first in preference list)
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");

    // Checksums must match exactly
    assert_eq!(
        server_result.checksum, client_result.checksum,
        "checksum algorithms must match"
    );
}

/// Tests error handling when client tries to read from an empty buffer.
/// Should fail with UnexpectedEof error.
#[test]
fn test_daemon_e2e_empty_buffer_error() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Client tries to read from empty buffer
    let mut client_output = Vec::new();
    let mut client_input = &b""[..]; // Empty buffer

    let result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,  // daemon mode
        false, // client
    );

    // Should fail with I/O error (UnexpectedEof)
    assert!(result.is_err(), "reading from empty buffer should fail");
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

/// Tests error handling when client reads from a truncated buffer.
/// Server sends partial data, client should fail gracefully.
#[test]
fn test_daemon_e2e_truncated_buffer_error() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Create a valid server output
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    // Truncate the buffer (only send first 5 bytes)
    let truncated = &server_output[..5.min(server_output.len())];

    // Client tries to read truncated data
    let mut client_output = Vec::new();
    let mut client_input = truncated;

    let result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    );

    // Should fail with I/O error
    assert!(result.is_err(), "reading truncated buffer should fail");
    let err = result.unwrap_err();
    assert!(
        err.kind() == std::io::ErrorKind::UnexpectedEof
            || err.kind() == std::io::ErrorKind::InvalidData,
        "expected UnexpectedEof or InvalidData, got {:?}",
        err.kind()
    );
}

/// Tests multiple sequential negotiations to verify state is independent.
/// Each negotiation should produce consistent results.
#[test]
fn test_daemon_e2e_multiple_rounds() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Run negotiation 3 times
    for round in 0..3 {
        // Server-side
        let mut server_output = Vec::new();
        let mut server_input = &b""[..];

        let server_result = negotiate_capabilities(
            protocol,
            &mut server_input,
            &mut server_output,
            true,
            true,
            true,
            true,
        )
        .unwrap_or_else(|_| panic!("server negotiation round {round} should succeed"));

        // Client-side
        let mut client_output = Vec::new();
        let mut client_input = &server_output[..];

        let client_result = negotiate_capabilities(
            protocol,
            &mut client_input,
            &mut client_output,
            true,
            true,
            true,
            false,
        )
        .unwrap_or_else(|_| panic!("client negotiation round {round} should succeed"));

        // All rounds should produce identical results
        assert_eq!(server_result.checksum.as_str(), "xxh128");
        assert_eq!(client_result.checksum.as_str(), "xxh128");
        assert_eq!(server_result.checksum, client_result.checksum);
        assert_eq!(server_result.compression, client_result.compression);
    }
}

/// Tests daemon negotiation with compression disabled on both sides.
/// Verifies that both server and client agree on "none" compression.
#[test]
fn test_daemon_e2e_compression_disabled_both_sides() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server-side without compression
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        false, // no compression
        true,
        true,
    )
    .unwrap();

    // Client-side without compression
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        false, // no compression
        true,
        false,
    )
    .unwrap();

    // Both should have compression = "none"
    assert_eq!(server_result.compression.as_str(), "none");
    assert_eq!(client_result.compression.as_str(), "none");

    // But checksum should still be negotiated
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
}

/// Tests that server and client algorithm selections match exactly.
/// Verifies both checksum and compression are identical after negotiation.
#[test]
fn test_daemon_e2e_server_client_algorithm_match() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server-side with compression
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    // Client-side with compression
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .unwrap();

    // Checksum algorithms must be identical
    assert_eq!(
        server_result.checksum, client_result.checksum,
        "checksum mismatch: server={:?}, client={:?}",
        server_result.checksum, client_result.checksum
    );

    // Compression algorithms must be identical
    assert_eq!(
        server_result.compression, client_result.compression,
        "compression mismatch: server={:?}, client={:?}",
        server_result.compression, client_result.compression
    );

    // Verify both are using valid algorithms (not defaults due to error)
    assert_ne!(server_result.checksum.as_str(), "");
    assert_ne!(client_result.checksum.as_str(), "");
}

/// Tests that client correctly parses server's algorithm lists.
/// Verifies the wire format is correctly interpreted.
#[test]
fn test_daemon_e2e_parse_server_lists() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server sends its algorithm lists
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    // Parse what the server sent using our vstring helpers
    let mut reader = &server_output[..];
    let checksum_list = read_vstring(&mut reader).expect("should read checksum list");
    let compression_list = read_vstring(&mut reader).expect("should read compression list");

    // Verify lists contain expected algorithms
    assert!(
        checksum_list.contains("xxh128"),
        "checksum list should contain xxh128"
    );
    assert!(
        checksum_list.contains("md5"),
        "checksum list should contain md5"
    );

    assert!(
        compression_list.contains("zlib") || compression_list.contains("zlibx"),
        "compression list should contain zlib variants"
    );

    // Now verify client can parse these lists
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .expect("client should successfully parse server lists");

    // Client should select from server's list
    assert!(checksum_list.contains(client_result.checksum.as_str()));
    assert!(compression_list.contains(client_result.compression.as_str()));
}

/// Tests negotiation with only a subset of compression algorithms available.
/// Simulates a server advertising algorithms the client doesn't support.
#[test]
fn test_daemon_e2e_limited_compression_support() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Create custom server data with only specific algorithms
    let mut server_data = Vec::new();
    write_vstring(&mut server_data, "xxh128 md5 md4").unwrap();
    // Server offers zlibx and zlib (both always available)
    write_vstring(&mut server_data, "zlibx zlib none").unwrap();

    // Client reads server's lists
    let mut client_output = Vec::new();
    let mut client_input = &server_data[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .unwrap();

    // Client should select from available algorithms
    assert_eq!(client_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.compression.as_str(), "zlibx"); // First available
}

/// Tests that protocol 29 does not perform negotiation even in daemon mode.
/// Should use legacy defaults without any wire exchange.
#[test]
fn test_daemon_e2e_protocol_29_no_negotiation() {
    let protocol = ProtocolVersion::try_from(29).unwrap();

    // Server-side
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    // Client-side
    let mut client_output = Vec::new();
    let mut client_input = &b""[..]; // No data from server

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    )
    .unwrap();

    // Protocol 29 uses legacy defaults, no I/O
    assert!(server_output.is_empty(), "protocol 29 should not send data");
    assert!(client_output.is_empty(), "protocol 29 should not send data");

    // Both use legacy defaults
    assert_eq!(server_result.checksum.as_str(), "md4");
    assert_eq!(server_result.compression.as_str(), "zlib");
    assert_eq!(client_result.checksum.as_str(), "md4");
    assert_eq!(client_result.compression.as_str(), "zlib");
}

/// Tests buffer reuse - same buffer can be used for multiple negotiations.
/// Verifies no state leaks between negotiations.
#[test]
fn test_daemon_e2e_buffer_reuse() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Shared buffer for all negotiations
    let mut shared_buffer = Vec::with_capacity(256);

    // First negotiation
    shared_buffer.clear();
    let mut server_input = &b""[..];
    negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut shared_buffer,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    let first_size = shared_buffer.len();
    let first_data = shared_buffer.clone();

    // Second negotiation - should produce identical output
    shared_buffer.clear();
    let mut server_input = &b""[..];
    negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut shared_buffer,
        true,
        true,
        true,
        true,
    )
    .unwrap();

    assert_eq!(
        shared_buffer.len(),
        first_size,
        "buffer size should be consistent"
    );
    assert_eq!(shared_buffer, first_data, "output should be identical");
}

/// Tests that compression algorithm matches between server and client
/// when using different compression settings.
#[test]
fn test_daemon_e2e_compression_algorithm_match() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Test with compression enabled
    let mut server_output = Vec::new();
    let mut server_input = &b""[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        true, // compression enabled
        true,
        true,
    )
    .unwrap();

    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true, // compression enabled
        true,
        false,
    )
    .unwrap();

    // Both should agree on a compression algorithm (not "none")
    assert_ne!(server_result.compression.as_str(), "none");
    assert_ne!(client_result.compression.as_str(), "none");
    assert_eq!(server_result.compression, client_result.compression);
}
