//! Protocol 30+ capability negotiation.
//!
//! This module implements the `negotiate_the_strings()` function from upstream
//! rsync (compat.c:534-585), which negotiates checksum and compression algorithms
//! between client and server for Protocol 30+.
//!
//! # Protocol Flow
//!
//! For protocol versions >= 30, after the compatibility flags exchange, both sides
//! must negotiate which checksum and compression algorithms to use:
//!
//! 1. Server sends list of supported checksums (space-separated string)
//! 2. Server sends list of supported compressions (space-separated string)
//! 3. Server reads client's checksum choice (single algorithm name)
//! 4. Server reads client's compression choice (single algorithm name)
//! 5. Both sides select the first mutually supported algorithm
//!
//! # Character Set Encoding
//!
//! The negotiation strings (algorithm names) are ASCII and thus valid UTF-8.
//! Upstream rsync also supports charset negotiation via iconv for filename
//! encoding conversion, but that is a separate mechanism handled elsewhere.
//!
//! **Future extension**: Charset negotiation for filename encoding may be added
//! to this module to support cross-platform filename compatibility (e.g.,
//! macOS UTF-8 normalization, Windows codepages).
//!
//! # References
//!
//! - Upstream: `compat.c:534-585` (negotiate_the_strings)
//! - Upstream: `compat.c:332-391` (parse_negotiate_str, recv_negotiate_str)
//! - Upstream: `options.c` (iconv support for charset conversion)

use std::io::{self, Read, Write};

use logging::debug_log;

use crate::ProtocolVersion;

/// Supported checksum algorithms in preference order.
///
/// This list matches upstream rsync 3.4.1's default order.
/// The client will select the first algorithm in this list that it also supports.
/// Upstream order: xxh128 xxh3 xxh64 md5 md4 sha1 none
const SUPPORTED_CHECKSUMS: &[&str] = &["xxh128", "xxh3", "xxh64", "md5", "md4", "sha1", "none"];

/// Supported compression algorithms in preference order.
///
/// This list matches upstream rsync 3.4.1's default order.
/// The client will select the first algorithm in this list that it also supports.
const SUPPORTED_COMPRESSIONS: &[&str] = &["zstd", "lz4", "zlibx", "zlib", "none"];

/// Checksum algorithm choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChecksumAlgorithm {
    /// No checksum (for listing directories, etc.)
    None,
    /// MD4 checksum (legacy, protocol < 30 default)
    MD4,
    /// MD5 checksum (protocol 30+ default)
    MD5,
    /// SHA1 checksum
    SHA1,
    /// XXHash 64-bit
    XXH64,
    /// XXHash 3 (fast)
    XXH3,
    /// XXHash 128-bit
    XXH128,
}

impl ChecksumAlgorithm {
    /// Returns the wire protocol name for this algorithm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MD4 => "md4",
            Self::MD5 => "md5",
            Self::SHA1 => "sha1",
            Self::XXH64 => "xxh64",
            Self::XXH3 => "xxh3",
            Self::XXH128 => "xxh128",
        }
    }

    /// Parses an algorithm from its wire protocol name.
    pub fn parse(name: &str) -> io::Result<Self> {
        match name {
            "none" => Ok(Self::None),
            "md4" => Ok(Self::MD4),
            "md5" => Ok(Self::MD5),
            "sha1" => Ok(Self::SHA1),
            "xxh" | "xxh64" => Ok(Self::XXH64),
            "xxh3" => Ok(Self::XXH3),
            "xxh128" => Ok(Self::XXH128),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported checksum algorithm: {name}"),
            )),
        }
    }
}

/// Compression algorithm choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CompressionAlgorithm {
    /// No compression
    None,
    /// Zlib compression (legacy)
    Zlib,
    /// Zlib with matched data excluded (more compatible)
    ZlibX,
    /// LZ4 compression (fast)
    LZ4,
    /// Zstandard compression (modern, efficient)
    Zstd,
}

impl CompressionAlgorithm {
    /// Returns the wire protocol name for this algorithm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
            Self::ZlibX => "zlibx",
            Self::LZ4 => "lz4",
            Self::Zstd => "zstd",
        }
    }

    /// Parses an algorithm from its wire protocol name.
    pub fn parse(name: &str) -> io::Result<Self> {
        match name {
            "none" => Ok(Self::None),
            "zlib" => Ok(Self::Zlib),
            "zlibx" => Ok(Self::ZlibX),
            "lz4" => Ok(Self::LZ4),
            "zstd" => Ok(Self::Zstd),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported compression algorithm: {name}"),
            )),
        }
    }

    /// Converts to the compression crate's algorithm enum.
    ///
    /// Returns `None` if this is the `None` variant (no compression).
    /// Returns an error if the algorithm is not supported in this build.
    ///
    /// # Errors
    ///
    /// Returns an error if the algorithm requires a feature that is not enabled
    /// (e.g., LZ4 or Zstd without the corresponding feature flag).
    pub fn to_compress_algorithm(
        &self,
    ) -> io::Result<Option<compress::algorithm::CompressionAlgorithm>> {
        match self {
            Self::None => Ok(None),
            Self::Zlib | Self::ZlibX => Ok(Some(compress::algorithm::CompressionAlgorithm::Zlib)),
            #[cfg(feature = "lz4")]
            Self::LZ4 => Ok(Some(compress::algorithm::CompressionAlgorithm::Lz4)),
            #[cfg(not(feature = "lz4"))]
            Self::LZ4 => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LZ4 compression not available (feature not enabled)",
            )),
            #[cfg(feature = "zstd")]
            Self::Zstd => Ok(Some(compress::algorithm::CompressionAlgorithm::Zstd)),
            #[cfg(not(feature = "zstd"))]
            Self::Zstd => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Zstd compression not available (feature not enabled)",
            )),
        }
    }
}

/// Result of capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NegotiationResult {
    /// Selected checksum algorithm.
    pub checksum: ChecksumAlgorithm,
    /// Selected compression algorithm.
    pub compression: CompressionAlgorithm,
}

/// Negotiates checksum and compression algorithms with the client.
///
/// This function implements the server side of upstream rsync's
/// `negotiate_the_strings()` function (compat.c:534-585).
///
/// # Protocol Flow
///
/// 1. Server sends list of supported checksums (space-separated)
/// 2. Server sends list of supported compressions (space-separated)
/// 3. Server reads client's checksum choice (single algorithm name)
/// 4. Server reads client's compression choice (single algorithm name)
///
/// # Arguments
///
/// * `protocol` - The negotiated protocol version
/// * `stdin` - Input stream for reading client's choices
/// * `stdout` - Output stream for sending server's lists
///
/// # Returns
///
/// Returns the negotiated algorithms, or an I/O error if negotiation fails.
///
/// # Errors
///
/// - Protocol < 30: Not an error, returns default algorithms (MD4, Zlib)
/// - Client chooses unsupported algorithm: InvalidData error
/// - I/O errors during send/receive
///
/// # Examples
///
/// ```no_run
/// use protocol::{ProtocolVersion, negotiate_capabilities};
/// use std::io::{stdin, stdout};
///
/// let protocol = ProtocolVersion::try_from(32)?;
/// // Arguments: protocol, stdin, stdout, do_negotiation, send_compression,
/// //            is_daemon_mode, is_server
/// let result = negotiate_capabilities(
///     protocol,
///     &mut stdin(),
///     &mut stdout(),
///     true,   // do_negotiation
///     false,  // send_compression
///     false,  // is_daemon_mode (SSH mode)
///     false,  // is_server (client side)
/// )?;
/// println!("Using checksum: {:?}, compression: {:?}",
///          result.checksum, result.compression);
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Daemon Mode vs SSH Mode
///
/// - **SSH mode** (bidirectional): Both sides send lists, then both sides read lists
/// - **Daemon mode** (unidirectional): Server sends lists, client does NOT respond
///
/// The `is_daemon_mode` parameter controls whether we expect responses from the client.
/// The `is_server` parameter controls whether we are the server or client:
/// - When `is_server=true` in daemon mode: SEND lists only (client won't respond)
/// - When `is_server=false` in daemon mode: READ lists only (don't send)
/// - In SSH mode: both sides send, then both sides read
pub fn negotiate_capabilities(
    protocol: ProtocolVersion,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    do_negotiation: bool,
    send_compression: bool,
    is_daemon_mode: bool,
    is_server: bool,
) -> io::Result<NegotiationResult> {
    // Protocol < 30 doesn't support negotiation, use defaults
    if protocol.as_u8() < 30 {
        debug_log!(
            Proto,
            1,
            "protocol {} < 30, using legacy defaults (MD4, Zlib)",
            protocol.as_u8()
        );
        return Ok(NegotiationResult {
            checksum: ChecksumAlgorithm::MD4,
            compression: CompressionAlgorithm::Zlib,
        });
    }

    // CRITICAL: If client doesn't have VARINT_FLIST_FLAGS ('v' capability), it doesn't
    // support negotiate_the_strings() at all. We must NOT send negotiation strings to
    // such clients - they will interpret the varint length as a multiplex tag and fail
    // with "unexpected tag N" errors.
    //
    // This matches upstream's do_negotiated_strings check (compat.c:561-585) where
    // negotiate_the_strings is ONLY CALLED when do_negotiated_strings is TRUE.
    // When FALSE, upstream uses pre-filled defaults without any wire protocol exchange.
    if !do_negotiation {
        // Use protocol 30+ defaults without sending or reading anything
        // Upstream default when compression is not negotiated is CPRES_NONE (compat.c:234)
        debug_log!(
            Proto,
            1,
            "client lacks VARINT_FLIST_FLAGS, using defaults (MD5, None)"
        );
        return Ok(NegotiationResult {
            checksum: ChecksumAlgorithm::MD5,
            compression: CompressionAlgorithm::None,
        });
    }

    // Negotiation flow (upstream compat.c:534-570 negotiate_the_strings):
    //
    // BIDIRECTIONAL exchange in BOTH daemon and SSH modes when CF_VARINT_FLIST_FLAGS is set:
    //   - Both sides SEND their algorithm lists first
    //   - Then both sides READ each other's lists
    //   - Both independently choose the first match from the remote's list
    //
    // Upstream comment: "We send all the negotiation strings before we start
    // to read them to help avoid a slow startup."
    //
    // Note: Even though daemon mode advertises algorithms in the @RSYNCD greeting,
    // the vstring exchange STILL happens if CF_VARINT_FLIST_FLAGS is negotiated.
    // The greeting is just informational; the actual selection uses vstrings.
    let _ = is_daemon_mode; // Exchange happens in all modes when do_negotiation=true
    let _ = is_server; // Both sides behave symmetrically

    // Step 1: SEND our supported algorithm lists (upstream compat.c:541-544)
    // Uses vstring format (NOT varint) - see write_vstring documentation
    let checksum_list = SUPPORTED_CHECKSUMS.join(" ");
    debug_log!(Proto, 2, "sending checksum list: {}", checksum_list);
    write_vstring(stdout, &checksum_list)?;

    // Send compression list only if compression is enabled
    if send_compression {
        let compression_list = SUPPORTED_COMPRESSIONS.join(" ");
        debug_log!(Proto, 2, "sending compression list: {}", compression_list);
        write_vstring(stdout, &compression_list)?;
    }

    stdout.flush()?;

    // Step 2: READ the remote side's algorithm lists (upstream compat.c:546-564)
    // Uses vstring format (NOT varint) - see read_vstring documentation
    let remote_checksum_list = read_vstring(stdin)?;
    debug_log!(Proto, 2, "received checksum list: {}", remote_checksum_list);

    let remote_compression_list = if send_compression {
        let list = read_vstring(stdin)?;
        debug_log!(Proto, 2, "received compression list: {}", list);
        Some(list)
    } else {
        None
    };

    // Step 3: Choose algorithms - pick first from REMOTE's list that WE also support
    // This matches upstream where "the client picks the first name in the server's list
    // that is also in the client's list"
    let checksum = choose_checksum_algorithm(&remote_checksum_list)?;

    let compression = if let Some(ref list) = remote_compression_list {
        choose_compression_algorithm(list)?
    } else {
        CompressionAlgorithm::None
    };

    debug_log!(
        Proto,
        1,
        "negotiated checksum={}, compression={}",
        checksum.as_str(),
        compression.as_str()
    );
    Ok(NegotiationResult {
        checksum,
        compression,
    })
}

/// Chooses a checksum algorithm from the client's list.
///
/// Selects the first algorithm in the client's list that we also support.
/// This matches upstream's algorithm selection logic where "the client picks
/// the first name in the server's list that is also in the client's list"
/// (from server perspective: pick first in client's list we support).
fn choose_checksum_algorithm(client_list: &str) -> io::Result<ChecksumAlgorithm> {
    for algo in client_list.split_whitespace() {
        // Try to parse each algorithm the client supports
        if let Ok(checksum) = ChecksumAlgorithm::parse(algo) {
            // Check if we support it
            if SUPPORTED_CHECKSUMS.contains(&algo) {
                return Ok(checksum);
            }
        }
    }

    // No common algorithm found - use protocol 30+ default
    Ok(ChecksumAlgorithm::MD5)
}

/// Chooses a compression algorithm from the client's list.
///
/// Selects the first algorithm in the client's list that we also support.
fn choose_compression_algorithm(client_list: &str) -> io::Result<CompressionAlgorithm> {
    for algo in client_list.split_whitespace() {
        // Try to parse each algorithm the client supports
        if let Ok(compression) = CompressionAlgorithm::parse(algo) {
            // Check if we support it
            if SUPPORTED_COMPRESSIONS.contains(&algo) {
                return Ok(compression);
            }
        }
    }

    // No common algorithm found - use "none"
    Ok(CompressionAlgorithm::None)
}

/// Writes a vstring (variable-length string) using upstream rsync's format.
///
/// Format (upstream io.c:2222-2240 write_vstring):
/// - For len <= 127: 1 byte = len
/// - For len > 127: 2 bytes = [(len >> 8) | 0x80, len & 0xFF]
///
/// This is DIFFERENT from varint encoding! Varint uses 7 bits per byte with
/// continuation bits, while vstring uses a simpler 1-or-2 byte format.
fn write_vstring(writer: &mut dyn Write, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    let len = bytes.len();

    if len > 0x7FFF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vstring too long: {len} > {}", 0x7FFF),
        ));
    }

    let len_bytes = if len > 0x7F {
        // 2-byte format: high byte with 0x80 marker, then low byte
        let high = ((len >> 8) as u8) | 0x80;
        let low = (len & 0xFF) as u8;
        vec![high, low]
    } else {
        // 1-byte format: just the length
        vec![len as u8]
    };

    writer.write_all(&len_bytes)?;
    writer.write_all(bytes)?;
    Ok(())
}

/// Reads a vstring (variable-length string) using upstream rsync's format.
///
/// Format (upstream io.c:1944-1961 read_vstring):
/// - Read first byte
/// - If high bit set: len = (first & 0x7F) * 256 + read_another_byte
/// - Otherwise: len = first byte
/// - Then read `len` bytes of string data
///
/// This is DIFFERENT from varint encoding!
fn read_vstring(reader: &mut dyn Read) -> io::Result<String> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;

    let len = if first[0] & 0x80 != 0 {
        // 2-byte format
        let high = (first[0] & 0x7F) as usize;
        let mut second = [0u8; 1];
        reader.read_exact(&mut second)?;
        high * 256 + second[0] as usize
    } else {
        // 1-byte format
        first[0] as usize
    };

    // Sanity check: negotiation strings should be small
    // Upstream uses MAX_NSTR_STRLEN = 1024 (compat.c:537)
    if len > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("vstring too long: {len} bytes"),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;

    // Algorithm names are ASCII (subset of UTF-8), but validate anyway
    // to catch protocol errors early (e.g., reading from wrong stream layer)
    String::from_utf8(buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 in vstring: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_algorithm_roundtrip() {
        for &name in &["md4", "md5", "sha1", "xxh64", "xxh128"] {
            let algo = ChecksumAlgorithm::parse(name).unwrap();
            // Note: xxh64 wire name is "xxh64" but as_str returns "xxh64"
            // This is correct as the parsing accepts both "xxh" and "xxh64"
            let roundtrip = algo.as_str();
            let reparsed = ChecksumAlgorithm::parse(roundtrip).unwrap();
            assert_eq!(algo, reparsed, "roundtrip failed for {name}");
        }
    }

    #[test]
    fn test_compression_algorithm_roundtrip() {
        for &name in &["none", "zlib", "zlibx", "lz4", "zstd"] {
            let algo = CompressionAlgorithm::parse(name).unwrap();
            let roundtrip = algo.as_str();
            let reparsed = CompressionAlgorithm::parse(roundtrip).unwrap();
            assert_eq!(algo, reparsed, "roundtrip failed for {name}");
        }
    }

    #[test]
    fn test_xxh_alias() {
        // "xxh" should parse to XXH64
        let algo = ChecksumAlgorithm::parse("xxh").unwrap();
        assert_eq!(algo, ChecksumAlgorithm::XXH64);
    }

    #[test]
    fn test_negotiate_proto29_uses_defaults() {
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        // Protocol < 30 should use defaults without any I/O
        assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        assert!(
            stdout.is_empty(),
            "no data should be sent for protocol < 30"
        );
    }

    #[test]
    fn test_negotiate_proto30_md5_zlib() {
        let protocol = ProtocolVersion::try_from(30).unwrap();

        // Simulate remote choosing md5 and zlib
        // Format: vstring(len) + string, so len byte + "md5" + len byte + "zlib"
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);

        // Verify we sent our lists
        let output = String::from_utf8_lossy(&stdout);
        assert!(
            output.contains("md5"),
            "should send checksum list containing md5"
        );
        assert!(
            output.contains("zlib"),
            "should send compression list containing zlib"
        );
    }

    #[test]
    fn test_negotiate_proto32_zstd() {
        let protocol = ProtocolVersion::try_from(32).unwrap();

        // Simulate remote choosing md5 and zstd
        // Format: vstring(len) + string, so len byte + "md5" + len byte + "zstd"
        let client_response = b"\x03md5\x04zstd";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zstd);
    }

    #[test]
    fn test_vstring_roundtrip() {
        let test_str = "md5 md4 sha1 xxh128";
        let mut buffer = Vec::new();

        write_vstring(&mut buffer, test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();

        assert_eq!(received, test_str);
    }

    #[test]
    fn test_vstring_length_limit() {
        // Create a vstring that claims 10000 bytes (uses 2-byte format)
        // 10000 = 0x2710, so high byte = 0x27 | 0x80 = 0xA7, low byte = 0x10
        let mut buffer = vec![0xA7, 0x10];
        buffer.extend_from_slice(&[b'x'; 100]); // But only provide 100 bytes

        let mut reader = &buffer[..];
        let result = read_vstring(&mut reader);

        // Should fail because we can't read enough bytes
        assert!(result.is_err());
    }

    #[test]
    fn test_vstring_two_byte_format() {
        // Test vstring encoding for length > 127
        let test_str = "x".repeat(200); // 200 bytes > 127, needs 2-byte format
        let mut buffer = Vec::new();

        write_vstring(&mut buffer, &test_str).unwrap();

        // First byte should have high bit set (0xC8 = 0x80 | 0x00, second byte = 0xC8)
        // 200 = 0x00C8, so [0x80, 0xC8]
        assert_eq!(buffer[0], 0x80); // (200 >> 8) | 0x80 = 0 | 0x80 = 0x80
        assert_eq!(buffer[1], 0xC8); // 200 & 0xFF = 0xC8

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    #[test]
    fn test_unsupported_checksum() {
        let result = ChecksumAlgorithm::parse("blake2");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported checksum algorithm")
        );
    }

    #[test]
    fn test_unsupported_compression() {
        let result = CompressionAlgorithm::parse("bzip2");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported compression algorithm")
        );
    }

    // ========================================================================
    // Tests for do_negotiation parameter and daemon/SSH mode variations
    // ========================================================================

    #[test]
    fn test_negotiate_do_negotiation_false_uses_defaults_no_io() {
        // When do_negotiation=false, should return defaults without any I/O
        // This happens when client lacks VARINT_FLIST_FLAGS capability
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b""[..]; // Empty input - should not be read
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            false, // do_negotiation = false
            true,  // send_compression
            false, // is_daemon_mode
            true,  // is_server
        )
        .unwrap();

        // Should use MD5 (protocol 30+ default) and None (no compression)
        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::None);
        // No I/O should have occurred
        assert!(
            stdout.is_empty(),
            "no data should be sent when do_negotiation=false"
        );
    }

    #[test]
    fn test_negotiate_compression_disabled() {
        // When send_compression=false, should only exchange checksum list
        let protocol = ProtocolVersion::try_from(31).unwrap();

        // Only provide checksum list, no compression list
        let client_response = b"\x03md5"; // Just "md5", no compression
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            false, // send_compression = false
            false, // is_daemon_mode
            true,  // is_server
        )
        .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        // Compression should be None when not negotiated
        assert_eq!(result.compression, CompressionAlgorithm::None);

        // Should have sent checksum list but not compression list
        let output = String::from_utf8_lossy(&stdout);
        assert!(output.contains("md5"), "should send checksum list");
        // We can't easily verify compression wasn't sent without parsing,
        // but the test passing means stdin wasn't over-read
    }

    #[test]
    fn test_negotiate_daemon_mode_server() {
        // Daemon mode server (is_daemon_mode=true, is_server=true)
        // Currently still bidirectional, but parameters should be accepted
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true, // do_negotiation
            true, // send_compression
            true, // is_daemon_mode = true
            true, // is_server = true
        )
        .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    }

    #[test]
    fn test_negotiate_daemon_mode_client() {
        // Daemon mode client (is_daemon_mode=true, is_server=false)
        // Currently still bidirectional, but parameters should be accepted
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            true,  // send_compression
            true,  // is_daemon_mode = true
            false, // is_server = false
        )
        .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
    }

    #[test]
    fn test_negotiate_ssh_mode() {
        // SSH mode (is_daemon_mode=false) - bidirectional exchange
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let client_response = b"\x06xxh128\x04zstd";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            true,  // send_compression
            false, // is_daemon_mode = false (SSH mode)
            true,  // is_server
        )
        .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::XXH128);
        assert_eq!(result.compression, CompressionAlgorithm::Zstd);
    }

    #[test]
    fn test_choose_checksum_first_match_wins() {
        // When client sends multiple checksums, we pick the first one we support
        let client_list = "xxh128 xxh64 md5 md4";
        let result = choose_checksum_algorithm(client_list).unwrap();
        // xxh128 is first and we support it
        assert_eq!(result, ChecksumAlgorithm::XXH128);
    }

    #[test]
    fn test_choose_checksum_fallback_to_later_match() {
        // If first item is unsupported, pick next supported one
        let client_list = "blake3 sha256 md5 md4";
        let result = choose_checksum_algorithm(client_list).unwrap();
        // blake3 and sha256 are not supported, md5 is
        assert_eq!(result, ChecksumAlgorithm::MD5);
    }

    #[test]
    fn test_choose_checksum_empty_list() {
        // Empty list should fall back to MD5
        let result = choose_checksum_algorithm("").unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5);
    }

    #[test]
    fn test_choose_compression_first_match_wins() {
        let client_list = "zstd lz4 zlib none";
        let result = choose_compression_algorithm(client_list).unwrap();
        assert_eq!(result, CompressionAlgorithm::Zstd);
    }

    #[test]
    fn test_choose_compression_empty_list() {
        // Empty list should fall back to None
        let result = choose_compression_algorithm("").unwrap();
        assert_eq!(result, CompressionAlgorithm::None);
    }
}
