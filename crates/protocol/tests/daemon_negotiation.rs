//! Integration tests for capability negotiation in various modes.
//!
//! When `do_negotiation=true` (client has CF_VARINT_FLIST_FLAGS capability),
//! the exchange is BIDIRECTIONAL regardless of daemon/SSH mode:
//! - Both sides SEND their algorithm lists first
//! - Then both sides READ each other's lists
//! - Both independently select the first match from the remote's list
//!
//! Upstream rsync comment: "We send all the negotiation strings before we
//! start to read them to help avoid a slow startup."
//!
//! When `do_negotiation=false`, neither side sends or reads anything and
//! protocol defaults are used.

use protocol::{negotiate_capabilities, ProtocolVersion};
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
    // When do_negotiation=true, the exchange is bidirectional:
    // 1. Both sides send their algorithm lists
    // 2. Both sides read the other's lists
    // 3. Both select the first match from the remote's list
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Prepare "client" data that server will read
    let mut client_lists = Vec::new();
    write_vstring(&mut client_lists, "xxh128 md5 md4").unwrap();
    write_vstring(&mut client_lists, "zstd zlibx zlib none").unwrap();

    // Server side - sends its lists, reads client lists
    let mut server_output = Vec::new();
    let mut server_input = &client_lists[..];

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

    // Client also sends in bidirectional exchange
    assert!(
        !client_output.is_empty(),
        "client must also send algorithm lists in bidirectional exchange"
    );

    // Both should have valid results
    assert!(!server_result.checksum.as_str().is_empty());
    assert!(!client_result.checksum.as_str().is_empty());

    // Server selected from client's list (xxh128)
    assert_eq!(server_result.checksum.as_str(), "xxh128");
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

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        true,
        true,
        false,
    )
    .unwrap();

    // Should fall back to md5 (first we support)
    assert_eq!(result.checksum.as_str(), "md5");
    assert_eq!(result.compression.as_str(), "zlibx");
}

#[test]
fn test_negotiation_without_compression() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Remote sends only checksum list (no -z flag)
    let mut remote_data = Vec::new();
    write_vstring(&mut remote_data, "md5 md4").unwrap();

    let mut stdin = &remote_data[..];
    let mut stdout = Vec::new();

    let result = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        true,
        false, // send_compression = false
        true,
        false,
    )
    .unwrap();

    assert_eq!(result.checksum.as_str(), "md5");
    assert_eq!(result.compression.as_str(), "none");

    // Even without compression, we still send checksum list in bidirectional exchange
    assert!(!stdout.is_empty(), "checksum list should still be sent");

    // Verify only checksum list was sent (no compression list)
    let mut output_reader = &stdout[..];
    let checksum_list = read_vstring(&mut output_reader).unwrap();
    assert!(
        checksum_list.contains("md5"),
        "should include md5 in checksum list"
    );
    // Should have consumed all output (no compression list)
    assert!(
        output_reader.is_empty(),
        "no compression list should be sent"
    );
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
    assert!(
        !stdout.is_empty(),
        "SSH mode must send algorithm lists"
    );

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
    assert!(buf[0] & 0x80 != 0, "high bit should be set for long strings");

    let mut reader = &buf[..];
    let decoded = read_vstring(&mut reader).unwrap();
    assert_eq!(decoded, original);
}
