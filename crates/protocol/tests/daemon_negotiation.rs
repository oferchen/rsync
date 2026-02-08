//! Integration tests for capability negotiation.
//!
//! # Bidirectional Exchange (all modes)
//! Upstream rsync's negotiate_the_strings() is always bidirectional:
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

/// Generates the algorithm list data that a peer would send during negotiation.
/// This is what the remote side's write_vstring calls produce.
fn generate_peer_data(send_compression: bool) -> Vec<u8> {
    let mut data = Vec::new();
    write_vstring(&mut data, "xxh128 xxh3 xxh64 md5 md4 sha1 none").unwrap();
    if send_compression {
        // Match the supported_compressions() list from capabilities.rs
        #[cfg(all(feature = "zstd", feature = "lz4"))]
        write_vstring(&mut data, "zstd lz4 zlibx zlib none").unwrap();
        #[cfg(all(feature = "zstd", not(feature = "lz4")))]
        write_vstring(&mut data, "zstd zlibx zlib none").unwrap();
        #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
        write_vstring(&mut data, "lz4 zlibx zlib none").unwrap();
        #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
        write_vstring(&mut data, "zlibx zlib none").unwrap();
    }
    data
}

// ============================================================================
// Bidirectional Negotiation Flow Tests
// ============================================================================

#[test]
fn test_bidirectional_negotiation_exchange() {
    // Both sides send and receive in all modes (including daemon mode).
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Generate what a peer would send
    let peer_data = generate_peer_data(true);

    // Server side - sends its lists, reads peer's lists
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Verify server sent data (bidirectional)
    assert!(
        !server_output.is_empty(),
        "server must send algorithm lists"
    );

    // Server selects from peer's list
    assert_eq!(server_result.checksum.as_str(), "xxh128");

    // Client side - sends its lists, reads server's lists
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

    // Client also sends (bidirectional exchange)
    assert!(
        !client_output.is_empty(),
        "client must also send algorithm lists (bidirectional)"
    );

    // Both should have valid results
    assert_eq!(server_result.checksum.as_str(), "xxh128");
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
    assert_eq!(result.checksum.as_str(), "xxh128");
    // Client should also have sent its lists (bidirectional)
    assert!(!stdout.is_empty());
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

    // Peer sends only checksum list (no -z flag)
    let peer_data = generate_peer_data(false);

    let mut stdin = &peer_data[..];
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

    assert_eq!(result.checksum.as_str(), "xxh128");
    assert_eq!(result.compression.as_str(), "none");

    // Client sends its lists even in daemon mode (bidirectional)
    assert!(!stdout.is_empty());
}

// ============================================================================
// SSH Mode Bidirectional Flow Tests
// ============================================================================

#[test]
fn test_ssh_mode_bidirectional_exchange() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

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
// End-to-End Negotiation Tests
// ============================================================================

/// Tests full round-trip negotiation with compression enabled.
#[test]
fn test_e2e_with_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(true);

    // Server side
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    assert!(!server_output.is_empty());

    // Client side
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

    // Both should agree on algorithms
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
}

/// Tests full round-trip negotiation without compression.
#[test]
fn test_e2e_without_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(false);

    // Server side
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        false,
        true,
        true,
    )
    .expect("server negotiation should succeed");

    // Client side
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        false,
        true,
        false,
    )
    .expect("client negotiation should succeed");

    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
    assert_eq!(server_result.compression.as_str(), "none");
    assert_eq!(client_result.compression.as_str(), "none");
}

/// Tests negotiation with protocol version 30.
#[test]
fn test_e2e_protocol_30() {
    let protocol = ProtocolVersion::try_from(30).unwrap();
    let peer_data = generate_peer_data(true);

    // Server side
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Client side
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

    assert!(!server_output.is_empty());
    assert!(!client_output.is_empty());
    assert_eq!(server_result.checksum, client_result.checksum);
    assert_eq!(server_result.compression, client_result.compression);
}

/// Tests that both server and client agree on the same checksum algorithm.
#[test]
fn test_e2e_checksum_agreement() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(true);

    // Server side
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Client side
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

    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
    assert_eq!(
        server_result.checksum, client_result.checksum,
        "checksum algorithms must match"
    );
}

/// Tests error handling when client tries to read from an empty buffer.
#[test]
fn test_e2e_empty_buffer_error() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Client tries to read from empty buffer (but first sends its own data)
    let mut client_output = Vec::new();
    let mut client_input = &b""[..]; // Empty buffer — will fail after sending

    let result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        true,
        true,
        false,
    );

    // Should fail because after sending, it tries to read from empty input
    assert!(result.is_err(), "reading from empty buffer should fail");
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

/// Tests error handling when client reads from a truncated buffer.
#[test]
fn test_e2e_truncated_buffer_error() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Generate valid peer data and truncate it
    let peer_data = generate_peer_data(true);
    let truncated = &peer_data[..5.min(peer_data.len())];

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
#[test]
fn test_e2e_multiple_rounds() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    for round in 0..3 {
        let peer_data = generate_peer_data(true);

        // Server side
        let mut server_output = Vec::new();
        let mut server_input = &peer_data[..];

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

        // Client side
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

        assert_eq!(server_result.checksum.as_str(), "xxh128");
        assert_eq!(client_result.checksum.as_str(), "xxh128");
        assert_eq!(server_result.checksum, client_result.checksum);
        assert_eq!(server_result.compression, client_result.compression);
    }
}

/// Tests negotiation with compression disabled on both sides.
#[test]
fn test_e2e_compression_disabled_both_sides() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(false);

    // Server side without compression
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

    let server_result = negotiate_capabilities(
        protocol,
        &mut server_input,
        &mut server_output,
        true,
        false,
        true,
        true,
    )
    .unwrap();

    // Client side without compression
    let mut client_output = Vec::new();
    let mut client_input = &server_output[..];

    let client_result = negotiate_capabilities(
        protocol,
        &mut client_input,
        &mut client_output,
        true,
        false,
        true,
        false,
    )
    .unwrap();

    assert_eq!(server_result.compression.as_str(), "none");
    assert_eq!(client_result.compression.as_str(), "none");
    assert_eq!(server_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.checksum.as_str(), "xxh128");
}

/// Tests that server and client algorithm selections match exactly.
#[test]
fn test_e2e_server_client_algorithm_match() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(true);

    // Server side
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Client side
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

    assert_eq!(
        server_result.checksum, client_result.checksum,
        "checksum mismatch: server={:?}, client={:?}",
        server_result.checksum, client_result.checksum
    );
    assert_eq!(
        server_result.compression, client_result.compression,
        "compression mismatch: server={:?}, client={:?}",
        server_result.compression, client_result.compression
    );
}

/// Tests that client correctly parses server's algorithm lists.
#[test]
fn test_e2e_parse_server_lists() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(true);

    // Server sends its algorithm lists
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Parse what the server sent
    let mut reader = &server_output[..];
    let checksum_list = read_vstring(&mut reader).expect("should read checksum list");
    let compression_list = read_vstring(&mut reader).expect("should read compression list");

    assert!(checksum_list.contains("xxh128"));
    assert!(checksum_list.contains("md5"));
    assert!(
        compression_list.contains("zlib") || compression_list.contains("zlibx"),
        "compression list should contain zlib variants"
    );

    // Verify client can parse these lists
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

    assert!(checksum_list.contains(client_result.checksum.as_str()));
    assert!(compression_list.contains(client_result.compression.as_str()));
}

/// Tests negotiation with only a subset of compression algorithms.
#[test]
fn test_e2e_limited_compression_support() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Server offers specific algorithms
    let mut server_data = Vec::new();
    write_vstring(&mut server_data, "xxh128 md5 md4").unwrap();
    write_vstring(&mut server_data, "zlibx zlib none").unwrap();

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

    assert_eq!(client_result.checksum.as_str(), "xxh128");
    assert_eq!(client_result.compression.as_str(), "zlibx");
}

/// Tests that protocol 29 does not perform negotiation.
#[test]
fn test_e2e_protocol_29_no_negotiation() {
    let protocol = ProtocolVersion::try_from(29).unwrap();

    // Both sides: no I/O for protocol 29
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

    let mut client_output = Vec::new();
    let mut client_input = &b""[..];

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

    assert!(server_output.is_empty());
    assert!(client_output.is_empty());
    assert_eq!(server_result.checksum.as_str(), "md4");
    assert_eq!(server_result.compression.as_str(), "zlib");
    assert_eq!(client_result.checksum.as_str(), "md4");
    assert_eq!(client_result.compression.as_str(), "zlib");
}

/// Tests buffer reuse across multiple negotiations.
#[test]
fn test_e2e_buffer_reuse() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut shared_buffer = Vec::with_capacity(256);

    // First negotiation
    shared_buffer.clear();
    let peer_data = generate_peer_data(true);
    let mut server_input = &peer_data[..];
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

    // Second negotiation — should produce identical output
    shared_buffer.clear();
    let mut server_input = &peer_data[..];
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

    assert_eq!(shared_buffer.len(), first_size);
    assert_eq!(shared_buffer, first_data);
}

/// Tests that compression algorithms match between server and client.
#[test]
fn test_e2e_compression_algorithm_match() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let peer_data = generate_peer_data(true);

    // Server side with compression
    let mut server_output = Vec::new();
    let mut server_input = &peer_data[..];

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

    // Client side with compression
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

    assert_ne!(server_result.compression.as_str(), "none");
    assert_ne!(client_result.compression.as_str(), "none");
    assert_eq!(server_result.compression, client_result.compression);
}
