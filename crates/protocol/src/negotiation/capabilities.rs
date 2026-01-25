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

/// Returns supported compression algorithms in preference order.
///
/// This list is based on upstream rsync 3.4.1's default order, but only includes
/// algorithms that are actually available in this build (feature-gated).
/// The client will select the first algorithm in this list that it also supports.
#[allow(clippy::vec_init_then_push)] // Feature-gated pushes require incremental building
fn supported_compressions() -> Vec<&'static str> {
    let mut list = Vec::new();
    #[cfg(feature = "zstd")]
    list.push("zstd");
    #[cfg(feature = "lz4")]
    list.push("lz4");
    // zlibx and zlib are always available (via flate2/miniz_oxide)
    list.push("zlibx");
    list.push("zlib");
    list.push("none");
    list
}

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
    pub const fn as_str(&self) -> &'static str {
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
    pub const fn as_str(&self) -> &'static str {
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
    if protocol.uses_fixed_encoding() {
        debug_log!(
            Proto,
            1,
            "protocol {} uses legacy encoding, using defaults (MD4, Zlib)",
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
        let compression_list = supported_compressions().join(" ");
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
    let supported = supported_compressions();
    for algo in client_list.split_whitespace() {
        // Try to parse each algorithm the client supports
        if let Ok(compression) = CompressionAlgorithm::parse(algo) {
            // Check if we support it (both parse AND have feature enabled)
            if supported.contains(&algo) {
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
#[allow(clippy::uninlined_format_args)]
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
    #[cfg(feature = "zstd")]
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
    #[cfg(not(feature = "zstd"))]
    fn test_negotiate_proto32_zlib() {
        let protocol = ProtocolVersion::try_from(32).unwrap();

        // Simulate remote choosing md5 and zlib (zstd not available in this build)
        // Format: vstring(len) + string, so len byte + "md5" + len byte + "zlib"
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
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
    #[cfg(feature = "zstd")]
    fn test_negotiate_ssh_mode_zstd() {
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
    #[cfg(not(feature = "zstd"))]
    fn test_negotiate_ssh_mode_zlib() {
        // SSH mode (is_daemon_mode=false) - bidirectional exchange
        // Without zstd feature, remote's zstd preference is ignored, falls back to zlib
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let client_response = b"\x06xxh128\x04zlib";
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
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
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
    #[cfg(feature = "zstd")]
    fn test_choose_compression_first_match_wins_zstd() {
        let client_list = "zstd lz4 zlib none";
        let result = choose_compression_algorithm(client_list).unwrap();
        assert_eq!(result, CompressionAlgorithm::Zstd);
    }

    #[test]
    #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
    fn test_choose_compression_first_match_wins_lz4() {
        // Without zstd, first match should be lz4
        let client_list = "zstd lz4 zlib none";
        let result = choose_compression_algorithm(client_list).unwrap();
        assert_eq!(result, CompressionAlgorithm::Lz4);
    }

    #[test]
    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    fn test_choose_compression_first_match_wins_zlib() {
        // Without zstd or lz4, first match should be zlibx (always available)
        let client_list = "zstd lz4 zlibx zlib none";
        let result = choose_compression_algorithm(client_list).unwrap();
        assert_eq!(result, CompressionAlgorithm::ZlibX);
    }

    #[test]
    fn test_choose_compression_empty_list() {
        // Empty list should fall back to None
        let result = choose_compression_algorithm("").unwrap();
        assert_eq!(result, CompressionAlgorithm::None);
    }

    // ========================================================================
    // Interop Tests - Upstream Protocol Compatibility
    // ========================================================================

    #[test]
    fn test_upstream_checksum_list_format() {
        // Upstream rsync 3.4.1 sends checksums in this format:
        // "xxh128 xxh3 xxh64 md5 md4 sha1 none"
        let upstream_format = "xxh128 xxh3 xxh64 md5 md4 sha1 none";
        let result = choose_checksum_algorithm(upstream_format).unwrap();
        // First match should be xxh128
        assert_eq!(result, ChecksumAlgorithm::XXH128);
    }

    #[test]
    fn test_legacy_rsync_checksum_list() {
        // Older rsync might only offer md4 and md5
        let legacy_format = "md5 md4";
        let result = choose_checksum_algorithm(legacy_format).unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5);
    }

    #[test]
    fn test_minimal_rsync_checksum_list() {
        // Minimal rsync might only offer none
        let minimal_format = "none";
        let result = choose_checksum_algorithm(minimal_format).unwrap();
        assert_eq!(result, ChecksumAlgorithm::None);
    }

    // ========================================================================
    // Algorithm Parsing Edge Cases
    // ========================================================================

    #[test]
    fn test_checksum_case_sensitive() {
        // Algorithm names are case-sensitive
        assert!(ChecksumAlgorithm::parse("MD5").is_err());
        assert!(ChecksumAlgorithm::parse("Md5").is_err());
        assert!(ChecksumAlgorithm::parse("md5").is_ok());
    }

    #[test]
    fn test_compression_case_sensitive() {
        assert!(CompressionAlgorithm::parse("ZLIB").is_err());
        assert!(CompressionAlgorithm::parse("Zlib").is_err());
        assert!(CompressionAlgorithm::parse("zlib").is_ok());
    }

    #[test]
    fn test_checksum_with_whitespace() {
        // Lists can have multiple spaces between items
        let list = "md5   md4     sha1";
        let result = choose_checksum_algorithm(list).unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5);
    }

    #[test]
    fn test_compression_with_leading_trailing_space() {
        // split_whitespace handles leading/trailing spaces
        let list = "  zlib   zlibx  none  ";
        let result = choose_compression_algorithm(list).unwrap();
        assert_eq!(result, CompressionAlgorithm::Zlib);
    }

    // ========================================================================
    // NegotiationResult Tests
    // ========================================================================

    #[test]
    fn test_negotiation_result_debug() {
        let result = NegotiationResult {
            checksum: ChecksumAlgorithm::MD5,
            compression: CompressionAlgorithm::Zlib,
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("MD5"));
        assert!(debug.contains("Zlib"));
    }

    #[test]
    fn test_negotiation_result_equality() {
        let r1 = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH128,
            compression: CompressionAlgorithm::None,
        };
        let r2 = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH128,
            compression: CompressionAlgorithm::None,
        };
        let r3 = NegotiationResult {
            checksum: ChecksumAlgorithm::MD5,
            compression: CompressionAlgorithm::None,
        };
        assert_eq!(r1, r2);
        assert_ne!(r1, r3);
    }

    #[test]
    fn test_negotiation_result_clone() {
        let r1 = NegotiationResult {
            checksum: ChecksumAlgorithm::SHA1,
            compression: CompressionAlgorithm::ZlibX,
        };
        let r2 = r1;
        assert_eq!(r1.checksum, r2.checksum);
        assert_eq!(r1.compression, r2.compression);
    }

    // ========================================================================
    // vstring Encoding Tests
    // ========================================================================

    #[test]
    fn test_vstring_empty_string() {
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, "").unwrap();
        assert_eq!(buffer, vec![0x00]); // Length 0

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, "");
    }

    #[test]
    fn test_vstring_single_byte_boundary() {
        // Length 127 should use single-byte format
        let test_str = "x".repeat(127);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();
        assert_eq!(buffer[0], 127); // Single byte length
        assert_eq!(buffer.len(), 1 + 127);
    }

    #[test]
    fn test_vstring_two_byte_boundary() {
        // Length 128 should use two-byte format
        let test_str = "x".repeat(128);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();
        assert!(buffer[0] & 0x80 != 0); // Two-byte format indicator
        assert_eq!(buffer.len(), 2 + 128);
    }

    #[test]
    fn test_vstring_max_single_byte() {
        // Maximum single-byte length is 127
        let test_str = "y".repeat(127);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    #[test]
    fn test_vstring_moderate_length() {
        // Test a moderate length that uses 2-byte format
        let test_str = "z".repeat(500);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    // ========================================================================
    // Protocol Version Specific Tests
    // ========================================================================

    #[test]
    fn test_all_supported_versions_negotiate() {
        for version_num in 28..=32 {
            let protocol = ProtocolVersion::try_from(version_num).unwrap();

            if protocol.uses_fixed_encoding() {
                // Protocol < 30 uses defaults
                let mut stdin = &b""[..];
                let mut stdout = Vec::new();
                let result = negotiate_capabilities(
                    protocol,
                    &mut stdin,
                    &mut stdout,
                    true,
                    true,
                    false,
                    true,
                )
                .unwrap();
                assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
                assert_eq!(result.compression, CompressionAlgorithm::Zlib);
            } else {
                // Protocol >= 30 exchanges lists
                let client_response = b"\x03md5\x04zlib";
                let mut stdin = &client_response[..];
                let mut stdout = Vec::new();
                let result = negotiate_capabilities(
                    protocol,
                    &mut stdin,
                    &mut stdout,
                    true,
                    true,
                    false,
                    true,
                )
                .unwrap();
                assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
                assert_eq!(result.compression, CompressionAlgorithm::Zlib);
            }
        }
    }

    #[test]
    fn test_v28_uses_legacy_defaults() {
        let protocol = ProtocolVersion::try_from(28).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        assert!(stdout.is_empty());
    }

    #[test]
    fn test_v29_uses_legacy_defaults() {
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
        assert_eq!(result.compression, CompressionAlgorithm::Zlib);
        assert!(stdout.is_empty());
    }

    #[test]
    fn test_v30_requires_exchange() {
        let protocol = ProtocolVersion::try_from(30).unwrap();
        let client_response = b"\x05xxh64\x05zlibx";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true)
                .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::XXH64);
        assert_eq!(result.compression, CompressionAlgorithm::ZlibX);
        assert!(!stdout.is_empty()); // Should have sent our lists
    }

    // ========================================================================
    // PHASE 2.10: VSTRING 1-BYTE LENGTH FORMAT TESTS
    // ========================================================================
    //
    // The vstring format uses a simple length-prefixed encoding:
    // - For lengths 0-127: single byte = length (high bit clear)
    // - For lengths 128-32767: two bytes = [(len >> 8) | 0x80, len & 0xFF]
    //
    // These tests verify the 1-byte length format for strings up to 127 bytes.

    /// Tests that empty string uses 1-byte length format.
    #[test]
    fn phase2_10_vstring_1byte_empty_string() {
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, "").unwrap();

        // Should be: 1 length byte (0x00) + 0 data bytes = 1 byte total
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0], 0x00);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, "");
    }

    /// Tests that single-character string uses 1-byte length format.
    #[test]
    fn phase2_10_vstring_1byte_single_char() {
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, "x").unwrap();

        // Should be: 1 length byte (0x01) + 1 data byte = 2 bytes total
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer[0], 0x01);
        assert_eq!(buffer[1], b'x');

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, "x");
    }

    /// Tests all 1-byte length values (0-127).
    #[test]
    fn phase2_10_vstring_1byte_all_lengths() {
        for len in 0..=127usize {
            let test_str = "a".repeat(len);
            let mut buffer = Vec::new();
            write_vstring(&mut buffer, &test_str).unwrap();

            // Should use 1-byte length format
            assert_eq!(
                buffer[0], len as u8,
                "length {} should encode as single byte",
                len
            );
            assert_eq!(buffer.len(), 1 + len, "total size should be 1 + {}", len);
            // High bit should be clear
            assert!(
                buffer[0] & 0x80 == 0,
                "high bit should be clear for length {}",
                len
            );

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, test_str, "round-trip failed for length {}", len);
        }
    }

    /// Tests boundary at 127 (maximum 1-byte length).
    #[test]
    fn phase2_10_vstring_1byte_boundary_127() {
        let test_str = "b".repeat(127);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // Should use single-byte format: 0x7F
        assert_eq!(buffer[0], 0x7F);
        assert!(buffer[0] & 0x80 == 0, "high bit should be clear");
        assert_eq!(buffer.len(), 128); // 1 + 127

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests that raw 1-byte length sequences decode correctly.
    #[test]
    fn phase2_10_vstring_1byte_decode_raw() {
        // Test decoding raw bytes: length byte + content
        for len in 0u8..=127 {
            let mut data = vec![len];
            data.extend(vec![b'x'; len as usize]);

            let mut reader = &data[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received.len(), len as usize);
            assert!(received.chars().all(|c| c == 'x'));
        }
    }

    /// Tests typical algorithm names (all use 1-byte format).
    #[test]
    fn phase2_10_vstring_1byte_algorithm_names() {
        let names = [
            "md4", "md5", "sha1", "xxh64", "xxh128", "zlib", "zlibx", "zstd", "lz4", "none",
        ];
        for name in names {
            assert!(
                name.len() <= 127,
                "algorithm name should fit in 1-byte format"
            );

            let mut buffer = Vec::new();
            write_vstring(&mut buffer, name).unwrap();

            // Verify 1-byte format
            assert_eq!(buffer[0], name.len() as u8);
            assert!(buffer[0] & 0x80 == 0);

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, name);
        }
    }

    /// Tests typical space-separated algorithm lists (1-byte format).
    #[test]
    fn phase2_10_vstring_1byte_algorithm_lists() {
        let lists = [
            "md5 md4 sha1",
            "xxh128 xxh3 xxh64 md5 md4 sha1 none",
            "zstd lz4 zlibx zlib none",
        ];
        for list in lists {
            assert!(list.len() <= 127, "list should fit in 1-byte format");

            let mut buffer = Vec::new();
            write_vstring(&mut buffer, list).unwrap();

            // Verify 1-byte format
            assert_eq!(buffer[0], list.len() as u8);
            assert!(buffer[0] & 0x80 == 0);

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, list);
        }
    }

    // ========================================================================
    // PHASE 2.11: VSTRING 2-BYTE LENGTH FORMAT TESTS
    // ========================================================================
    //
    // For lengths 128-32767, vstring uses a 2-byte length format:
    // - First byte: (len >> 8) | 0x80 (high bit indicates 2-byte format)
    // - Second byte: len & 0xFF
    //
    // This allows encoding strings up to 32767 bytes.

    /// Tests boundary at 128 (minimum 2-byte length).
    #[test]
    fn phase2_11_vstring_2byte_boundary_128() {
        let test_str = "c".repeat(128);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // Should use 2-byte format: [0x80, 0x80] for length 128
        // 128 = 0x0080, so high byte = 0x00 | 0x80 = 0x80, low byte = 0x80
        assert_eq!(buffer[0], 0x80);
        assert_eq!(buffer[1], 0x80);
        assert!(
            buffer[0] & 0x80 != 0,
            "high bit should be set for 2-byte format"
        );
        assert_eq!(buffer.len(), 2 + 128); // 2 length bytes + 128 data bytes

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests value 200 (clear case of 2-byte format).
    #[test]
    fn phase2_11_vstring_2byte_length_200() {
        let test_str = "d".repeat(200);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // 200 = 0x00C8, so high byte = 0x00 | 0x80 = 0x80, low byte = 0xC8
        assert_eq!(buffer[0], 0x80);
        assert_eq!(buffer[1], 0xC8);
        assert_eq!(buffer.len(), 2 + 200);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests value 255 (boundary within first 256-byte range).
    #[test]
    fn phase2_11_vstring_2byte_length_255() {
        let test_str = "e".repeat(255);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // 255 = 0x00FF, so high byte = 0x00 | 0x80 = 0x80, low byte = 0xFF
        assert_eq!(buffer[0], 0x80);
        assert_eq!(buffer[1], 0xFF);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests value 256 (crosses into second high byte).
    #[test]
    fn phase2_11_vstring_2byte_length_256() {
        let test_str = "f".repeat(256);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // 256 = 0x0100, so high byte = 0x01 | 0x80 = 0x81, low byte = 0x00
        assert_eq!(buffer[0], 0x81);
        assert_eq!(buffer[1], 0x00);

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests sample values across the 2-byte range.
    #[test]
    fn phase2_11_vstring_2byte_sample_values() {
        let lengths = [128, 200, 255, 256, 500, 1000, 2000, 4000, 8000];
        for len in lengths {
            let test_str = "g".repeat(len);
            let mut buffer = Vec::new();
            write_vstring(&mut buffer, &test_str).unwrap();

            // Verify 2-byte format
            assert!(
                buffer[0] & 0x80 != 0,
                "high bit should be set for length {}",
                len
            );

            // Verify encoding: len = ((buffer[0] & 0x7F) << 8) | buffer[1]
            let decoded_len = ((buffer[0] & 0x7F) as usize) * 256 + buffer[1] as usize;
            assert_eq!(decoded_len, len, "length encoding mismatch for {}", len);

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, test_str, "round-trip failed for length {}", len);
        }
    }

    /// Tests decoding raw 2-byte length sequences.
    #[test]
    fn phase2_11_vstring_2byte_decode_raw() {
        // Test specific 2-byte encoded lengths
        let cases = [
            (128, 0x80u8, 0x80u8), // 128 = 0x0080
            (200, 0x80, 0xC8),     // 200 = 0x00C8
            (256, 0x81, 0x00),     // 256 = 0x0100
            (1000, 0x83, 0xE8),    // 1000 = 0x03E8
            (8000, 0x9F, 0x40),    // 8000 = 0x1F40
        ];

        for (len, high, low) in cases {
            let mut data = vec![high, low];
            data.extend(vec![b'x'; len]);

            let mut reader = &data[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received.len(), len, "decode failed for length {}", len);
        }
    }

    /// Tests truncated 2-byte length (only high byte present).
    #[test]
    fn phase2_11_vstring_2byte_truncated_length() {
        // Only the high byte, missing the low byte
        let data = [0x80u8];
        let mut reader = &data[..];
        let result = read_vstring(&mut reader);
        assert!(result.is_err(), "should fail on truncated 2-byte length");
    }

    /// Tests truncated 2-byte vstring (length present but data truncated).
    #[test]
    fn phase2_11_vstring_2byte_truncated_data() {
        // Length says 200 bytes, but only 50 provided
        let mut data = vec![0x80, 0xC8]; // Length 200
        data.extend(vec![b'x'; 50]); // Only 50 bytes

        let mut reader = &data[..];
        let result = read_vstring(&mut reader);
        assert!(result.is_err(), "should fail on truncated data");
    }

    /// Tests multiple 2-byte vstrings in sequence.
    #[test]
    fn phase2_11_vstring_2byte_multiple_in_sequence() {
        let strings = ["h".repeat(128), "i".repeat(200), "j".repeat(500)];
        let mut buffer = Vec::new();

        for s in &strings {
            write_vstring(&mut buffer, s).unwrap();
        }

        let mut reader = &buffer[..];
        for expected in &strings {
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, *expected);
        }
    }

    // ========================================================================
    // PHASE 2.12: VSTRING MAXIMUM LENGTH HANDLING TESTS
    // ========================================================================
    //
    // The vstring format has limits:
    // - Maximum encodable length: 0x7FFF (32767 bytes)
    // - Sanity limit in read_vstring: 8192 bytes
    // - Upstream MAX_NSTR_STRLEN: 1024 bytes
    //
    // These tests verify boundary conditions and error handling.

    /// Tests maximum encodable length (0x7FFF = 32767).
    #[test]
    fn phase2_12_vstring_max_encodable_length() {
        let test_str = "k".repeat(0x7FFF);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // 0x7FFF encoded as [0xFF, 0xFF] (7F | 80 = FF, FF)
        assert_eq!(buffer[0], 0xFF);
        assert_eq!(buffer[1], 0xFF);

        // Verify round-trip (note: exceeds sanity limit, so read will fail)
        // This test specifically verifies the ENCODING works for max length
        assert_eq!(buffer.len(), 2 + 0x7FFF);
    }

    /// Tests that encoding length > 0x7FFF fails.
    #[test]
    fn phase2_12_vstring_exceeds_max_encodable() {
        let test_str = "l".repeat(0x8000); // 32768 bytes
        let mut buffer = Vec::new();
        let result = write_vstring(&mut buffer, &test_str);

        assert!(result.is_err(), "should reject strings > 0x7FFF bytes");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("vstring too long"));
    }

    /// Tests sanity limit in read_vstring (8192 bytes).
    #[test]
    fn phase2_12_vstring_sanity_limit_exceeded() {
        // Encode a length of 10000 (exceeds 8192 sanity limit)
        // 10000 = 0x2710, so high byte = 0x27 | 0x80 = 0xA7, low byte = 0x10
        let data = [0xA7u8, 0x10];

        let mut reader = &data[..];
        let result = read_vstring(&mut reader);

        assert!(result.is_err(), "should reject vstrings > 8192 bytes");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("vstring too long"));
    }

    /// Tests exactly at sanity limit (8192 bytes).
    #[test]
    fn phase2_12_vstring_at_sanity_limit() {
        let test_str = "m".repeat(8192);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests just below sanity limit (8191 bytes).
    #[test]
    fn phase2_12_vstring_below_sanity_limit() {
        let test_str = "n".repeat(8191);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests just above sanity limit (8193 bytes).
    #[test]
    fn phase2_12_vstring_above_sanity_limit() {
        // Write succeeds (max is 0x7FFF)
        let test_str = "o".repeat(8193);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        // Read fails (sanity limit is 8192)
        let mut reader = &buffer[..];
        let result = read_vstring(&mut reader);
        assert!(result.is_err(), "should reject vstrings > 8192 bytes");
    }

    /// Tests typical upstream limit (1024 bytes, MAX_NSTR_STRLEN).
    #[test]
    fn phase2_12_vstring_upstream_typical_limit() {
        // Upstream uses MAX_NSTR_STRLEN = 1024 for negotiation strings
        let test_str = "p".repeat(1024);
        let mut buffer = Vec::new();
        write_vstring(&mut buffer, &test_str).unwrap();

        let mut reader = &buffer[..];
        let received = read_vstring(&mut reader).unwrap();
        assert_eq!(received, test_str);
    }

    /// Tests empty input (EOF) handling.
    #[test]
    fn phase2_12_vstring_empty_input() {
        let data: [u8; 0] = [];
        let mut reader = &data[..];
        let result = read_vstring(&mut reader);

        assert!(result.is_err(), "should fail on empty input");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    /// Tests UTF-8 validation (algorithm names are ASCII but we validate).
    #[test]
    fn phase2_12_vstring_invalid_utf8() {
        // Create a vstring with invalid UTF-8 bytes
        let mut data = vec![0x03]; // Length 3
        data.extend([0xFF, 0xFE, 0x80]); // Invalid UTF-8 sequence

        let mut reader = &data[..];
        let result = read_vstring(&mut reader);

        assert!(result.is_err(), "should reject invalid UTF-8");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("UTF-8"));
    }

    /// Tests various boundary values around encoding transitions.
    #[test]
    fn phase2_12_vstring_encoding_transitions() {
        let boundaries = [
            0,    // Minimum
            1,    // Single char
            127,  // Max 1-byte
            128,  // Min 2-byte
            255,  // 0x00FF
            256,  // 0x0100
            1023, // Just under upstream limit
            1024, // Upstream limit
            4096, // 4KB
            8191, // Just under sanity limit
            8192, // At sanity limit
        ];

        for len in boundaries {
            let test_str = "q".repeat(len);
            let mut buffer = Vec::new();
            write_vstring(&mut buffer, &test_str).unwrap();

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received.len(), len, "round-trip failed for length {}", len);
        }
    }

    /// Tests that write_vstring properly handles boundary between 1 and 2 byte formats.
    #[test]
    fn phase2_12_vstring_format_boundary_exact() {
        // 127 bytes should use 1-byte format
        let s127 = "r".repeat(127);
        let mut buf127 = Vec::new();
        write_vstring(&mut buf127, &s127).unwrap();
        assert!(buf127[0] & 0x80 == 0, "127 should use 1-byte format");

        // 128 bytes should use 2-byte format
        let s128 = "s".repeat(128);
        let mut buf128 = Vec::new();
        write_vstring(&mut buf128, &s128).unwrap();
        assert!(buf128[0] & 0x80 != 0, "128 should use 2-byte format");
    }

    /// Tests maximum practical negotiation string from upstream.
    #[test]
    fn phase2_12_vstring_realistic_max_negotiation() {
        // Realistic maximum: all supported checksums + compressions
        // "xxh128 xxh3 xxh64 md5 md4 sha1 none" = 37 chars
        // "zstd lz4 zlibx zlib none" = 24 chars
        // Well under both limits
        let checksum_list = "xxh128 xxh3 xxh64 md5 md4 sha1 none";
        let compression_list = "zstd lz4 zlibx zlib none";

        for list in [checksum_list, compression_list] {
            let mut buffer = Vec::new();
            write_vstring(&mut buffer, list).unwrap();

            // All realistic lists should fit in 1-byte format
            assert!(
                buffer[0] & 0x80 == 0,
                "realistic list should use 1-byte format"
            );

            let mut reader = &buffer[..];
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, list);
        }
    }

    // ========================================================================
    // PHASE 3: I/O ERROR HANDLING TESTS
    // ========================================================================

    #[test]
    fn phase3_write_vstring_io_error() {
        struct FailWriter;
        impl std::io::Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let result = write_vstring(&mut FailWriter, "test");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn phase3_read_vstring_io_error() {
        struct FailReader;
        impl std::io::Read for FailReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "read failed",
                ))
            }
        }

        let result = read_vstring(&mut FailReader);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn phase3_negotiate_stdin_io_error() {
        struct FailReader;
        impl std::io::Read for FailReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::TimedOut, "read timeout"))
            }
        }

        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut FailReader,
            &mut stdout,
            true,
            true,
            false,
            true,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn phase3_negotiate_stdout_io_error() {
        struct FailWriter;
        impl std::io::Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "write blocked"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let protocol = ProtocolVersion::try_from(31).unwrap();
        let input = b"\x03md5\x04zlib";

        let result = negotiate_capabilities(
            protocol,
            &mut &input[..],
            &mut FailWriter,
            true,
            true,
            false,
            true,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    }

    // ========================================================================
    // PHASE 4: ALGORITHM CONVERSION TESTS
    // ========================================================================

    #[test]
    fn phase4_checksum_algorithm_as_str() {
        assert_eq!(ChecksumAlgorithm::None.as_str(), "none");
        assert_eq!(ChecksumAlgorithm::MD4.as_str(), "md4");
        assert_eq!(ChecksumAlgorithm::MD5.as_str(), "md5");
        assert_eq!(ChecksumAlgorithm::SHA1.as_str(), "sha1");
        assert_eq!(ChecksumAlgorithm::XXH64.as_str(), "xxh64");
        assert_eq!(ChecksumAlgorithm::XXH3.as_str(), "xxh3");
        assert_eq!(ChecksumAlgorithm::XXH128.as_str(), "xxh128");
    }

    #[test]
    fn phase4_compression_algorithm_as_str() {
        assert_eq!(CompressionAlgorithm::None.as_str(), "none");
        assert_eq!(CompressionAlgorithm::Zlib.as_str(), "zlib");
        assert_eq!(CompressionAlgorithm::ZlibX.as_str(), "zlibx");
        assert_eq!(CompressionAlgorithm::LZ4.as_str(), "lz4");
        assert_eq!(CompressionAlgorithm::Zstd.as_str(), "zstd");
    }

    #[test]
    fn phase4_checksum_algorithm_copy() {
        let algo1 = ChecksumAlgorithm::MD5;
        let algo2 = algo1; // Copy
        assert_eq!(algo1, algo2);
    }

    #[test]
    fn phase4_compression_algorithm_copy() {
        let algo1 = CompressionAlgorithm::Zlib;
        let algo2 = algo1; // Copy
        assert_eq!(algo1, algo2);
    }

    #[test]
    fn phase4_checksum_algorithm_debug() {
        let debug = format!("{:?}", ChecksumAlgorithm::XXH128);
        assert!(debug.contains("XXH128"));
    }

    #[test]
    fn phase4_compression_algorithm_debug() {
        let debug = format!("{:?}", CompressionAlgorithm::Zstd);
        assert!(debug.contains("Zstd"));
    }

    #[test]
    fn phase4_compression_to_compress_algorithm_none() {
        let result = CompressionAlgorithm::None.to_compress_algorithm();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn phase4_compression_to_compress_algorithm_zlib() {
        let result = CompressionAlgorithm::Zlib.to_compress_algorithm();
        assert!(result.is_ok());
        let algo = result.unwrap();
        assert!(algo.is_some());
    }

    #[test]
    fn phase4_compression_to_compress_algorithm_zlibx() {
        // ZlibX also maps to Zlib compression
        let result = CompressionAlgorithm::ZlibX.to_compress_algorithm();
        assert!(result.is_ok());
        let algo = result.unwrap();
        assert!(algo.is_some());
    }

    #[test]
    #[cfg(feature = "lz4")]
    fn phase4_compression_to_compress_algorithm_lz4() {
        let result = CompressionAlgorithm::LZ4.to_compress_algorithm();
        assert!(result.is_ok());
        let algo = result.unwrap();
        assert!(algo.is_some());
    }

    #[test]
    #[cfg(not(feature = "lz4"))]
    fn phase4_compression_lz4_not_available() {
        let result = CompressionAlgorithm::LZ4.to_compress_algorithm();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("LZ4"));
    }

    #[test]
    #[cfg(feature = "zstd")]
    fn phase4_compression_to_compress_algorithm_zstd() {
        let result = CompressionAlgorithm::Zstd.to_compress_algorithm();
        assert!(result.is_ok());
        let algo = result.unwrap();
        assert!(algo.is_some());
    }

    #[test]
    #[cfg(not(feature = "zstd"))]
    fn phase4_compression_zstd_not_available() {
        let result = CompressionAlgorithm::Zstd.to_compress_algorithm();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("Zstd"));
    }

    // ========================================================================
    // PHASE 5: NEGOTIATION EDGE CASES
    // ========================================================================

    #[test]
    fn phase5_negotiate_only_unsupported_checksums() {
        // If client sends only unsupported checksums, fallback to MD5
        let list = "blake3 sha256 sha512 xxh256";
        let result = choose_checksum_algorithm(list).unwrap();
        assert_eq!(result, ChecksumAlgorithm::MD5);
    }

    #[test]
    fn phase5_negotiate_only_unsupported_compressions() {
        // If client sends only unsupported compressions, fallback to None
        let list = "bzip2 lzma xz brotli";
        let result = choose_compression_algorithm(list).unwrap();
        assert_eq!(result, CompressionAlgorithm::None);
    }

    #[test]
    fn phase5_negotiate_whitespace_only_list() {
        let list = "   \t   \n   ";
        let checksum = choose_checksum_algorithm(list).unwrap();
        assert_eq!(checksum, ChecksumAlgorithm::MD5);

        let compression = choose_compression_algorithm(list).unwrap();
        assert_eq!(compression, CompressionAlgorithm::None);
    }

    #[test]
    fn phase5_negotiate_mixed_supported_unsupported() {
        // Mix of supported and unsupported, should pick first supported
        let list = "blake3 unsupported xxh128 md5";
        let result = choose_checksum_algorithm(list).unwrap();
        assert_eq!(result, ChecksumAlgorithm::XXH128);
    }

    #[test]
    fn phase5_negotiate_order_preference() {
        // First supported algorithm should win
        let list1 = "xxh128 md5 sha1";
        let list2 = "md5 xxh128 sha1";
        let list3 = "sha1 md5 xxh128";

        assert_eq!(
            choose_checksum_algorithm(list1).unwrap(),
            ChecksumAlgorithm::XXH128
        );
        assert_eq!(
            choose_checksum_algorithm(list2).unwrap(),
            ChecksumAlgorithm::MD5
        );
        assert_eq!(
            choose_checksum_algorithm(list3).unwrap(),
            ChecksumAlgorithm::SHA1
        );
    }

    #[test]
    fn phase5_negotiate_xxh3_support() {
        let list = "xxh3 xxh128";
        let result = choose_checksum_algorithm(list).unwrap();
        assert_eq!(result, ChecksumAlgorithm::XXH3);
    }

    #[test]
    fn phase5_negotiate_zlibx_vs_zlib() {
        let list = "zlibx zlib";
        let result = choose_compression_algorithm(list).unwrap();
        assert_eq!(result, CompressionAlgorithm::ZlibX);
    }

    // ========================================================================
    // PHASE 6: FULL NEGOTIATION FLOW TESTS
    // ========================================================================

    #[test]
    fn phase6_full_negotiation_all_supported_versions() {
        for version in 28..=32 {
            let protocol = ProtocolVersion::try_from(version).unwrap();

            if protocol.uses_fixed_encoding() {
                // Legacy: no exchange needed
                let mut stdin = &b""[..];
                let mut stdout = Vec::new();
                let result = negotiate_capabilities(
                    protocol,
                    &mut stdin,
                    &mut stdout,
                    true,
                    true,
                    false,
                    true,
                )
                .unwrap();
                assert_eq!(result.checksum, ChecksumAlgorithm::MD4);
                assert_eq!(result.compression, CompressionAlgorithm::Zlib);
                assert!(stdout.is_empty());
            } else {
                // Modern: exchange required
                let response = b"\x04sha1\x04none";
                let mut stdin = &response[..];
                let mut stdout = Vec::new();
                let result = negotiate_capabilities(
                    protocol,
                    &mut stdin,
                    &mut stdout,
                    true,
                    true,
                    false,
                    true,
                )
                .unwrap();
                assert_eq!(result.checksum, ChecksumAlgorithm::SHA1);
                assert_eq!(result.compression, CompressionAlgorithm::None);
                assert!(!stdout.is_empty());
            }
        }
    }

    #[test]
    fn phase6_full_negotiation_checksum_only() {
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let response = b"\x04sha1"; // Only checksum, no compression
        let mut stdin = &response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            false, // send_compression = false
            false,
            true,
        )
        .unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::SHA1);
        assert_eq!(result.compression, CompressionAlgorithm::None);
    }

    // ========================================================================
    // PHASE 7: VSTRING SEQUENTIAL OPERATIONS
    // ========================================================================

    #[test]
    fn phase7_vstring_multiple_sequential() {
        let strings = ["first", "second", "third", "fourth"];
        let mut buffer = Vec::new();

        for s in &strings {
            write_vstring(&mut buffer, s).unwrap();
        }

        let mut reader = &buffer[..];
        for expected in &strings {
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, *expected);
        }
    }

    #[test]
    fn phase7_vstring_mixed_sizes() {
        let strings = [
            "a",               // 1 byte
            "hello world",     // 11 bytes
            &"x".repeat(127),  // max 1-byte format
            &"y".repeat(128),  // min 2-byte format
            &"z".repeat(1000), // larger 2-byte format
        ];
        let mut buffer = Vec::new();

        for s in &strings {
            write_vstring(&mut buffer, s).unwrap();
        }

        let mut reader = &buffer[..];
        for expected in &strings {
            let received = read_vstring(&mut reader).unwrap();
            assert_eq!(received, *expected);
        }
    }

    // ========================================================================
    // PHASE 8: NEGOTIATION RESULT TESTS
    // ========================================================================

    #[test]
    fn phase8_negotiation_result_copy() {
        let r1 = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH128,
            compression: CompressionAlgorithm::ZlibX,
        };
        let r2 = r1; // Copy
        assert_eq!(r1.checksum, r2.checksum);
        assert_eq!(r1.compression, r2.compression);
    }

    #[test]
    fn phase8_negotiation_result_all_combinations() {
        let checksums = [
            ChecksumAlgorithm::None,
            ChecksumAlgorithm::MD4,
            ChecksumAlgorithm::MD5,
            ChecksumAlgorithm::SHA1,
            ChecksumAlgorithm::XXH64,
            ChecksumAlgorithm::XXH3,
            ChecksumAlgorithm::XXH128,
        ];
        let compressions = [
            CompressionAlgorithm::None,
            CompressionAlgorithm::Zlib,
            CompressionAlgorithm::ZlibX,
            CompressionAlgorithm::LZ4,
            CompressionAlgorithm::Zstd,
        ];

        for &checksum in &checksums {
            for &compression in &compressions {
                let result = NegotiationResult {
                    checksum,
                    compression,
                };
                assert_eq!(result.checksum, checksum);
                assert_eq!(result.compression, compression);
            }
        }
    }
}
