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

use crate::{ProtocolVersion, read_varint, write_varint};

/// Supported checksum algorithms in preference order.
///
/// This list matches upstream rsync 3.4.1's default order.
/// The client will select the first algorithm in this list that it also supports.
const SUPPORTED_CHECKSUMS: &[&str] = &["md5", "md4", "sha1", "xxh128"];

/// Supported compression algorithms in preference order.
///
/// This list matches upstream rsync 3.4.1's default order.
/// The client will select the first algorithm in this list that it also supports.
const SUPPORTED_COMPRESSIONS: &[&str] = &["zstd", "lz4", "zlibx", "zlib", "none"];

/// Checksum algorithm choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    /// MD4 checksum (legacy, protocol < 30 default)
    MD4,
    /// MD5 checksum (protocol 30+ default)
    MD5,
    /// SHA1 checksum
    SHA1,
    /// XXHash 64-bit
    XXH64,
    /// XXHash 128-bit
    XXH128,
}

impl ChecksumAlgorithm {
    /// Returns the wire protocol name for this algorithm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MD4 => "md4",
            Self::MD5 => "md5",
            Self::SHA1 => "sha1",
            Self::XXH64 => "xxh64",
            Self::XXH128 => "xxh128",
        }
    }

    /// Parses an algorithm from its wire protocol name.
    pub fn parse(name: &str) -> io::Result<Self> {
        match name {
            "md4" => Ok(Self::MD4),
            "md5" => Ok(Self::MD5),
            "sha1" => Ok(Self::SHA1),
            "xxh" | "xxh64" => Ok(Self::XXH64),
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
}

/// Result of capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// let result = negotiate_capabilities(protocol, &mut stdin(), &mut stdout(), true)?;
/// println!("Using checksum: {:?}, compression: {:?}",
///          result.checksum, result.compression);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn negotiate_capabilities(
    protocol: ProtocolVersion,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    do_negotiation: bool,
) -> io::Result<NegotiationResult> {
    // Protocol < 30 doesn't support negotiation, use defaults
    if protocol.as_u8() < 30 {
        return Ok(NegotiationResult {
            checksum: ChecksumAlgorithm::MD4,
            compression: CompressionAlgorithm::Zlib,
        });
    }

    // If client doesn't have VARINT_FLIST_FLAGS ('v' capability), skip negotiation
    // This matches upstream's do_negotiated_strings check (compat.c:561-585)
    if !do_negotiation {
        // Use protocol 30+ defaults without negotiation
        return Ok(NegotiationResult {
            checksum: ChecksumAlgorithm::MD5,  // Protocol 30+ default
            compression: CompressionAlgorithm::Zlib,
        });
    }

    // Step 1 & 2: Send our supported algorithms (upstream compat.c:541-544)
    let checksum_list = SUPPORTED_CHECKSUMS.join(" ");
    send_string(stdout, &checksum_list)?;

    let compression_list = SUPPORTED_COMPRESSIONS.join(" ");
    send_string(stdout, &compression_list)?;

    stdout.flush()?;

    // Step 3 & 4: Read client's choices (upstream compat.c:549-585)
    let client_checksum = recv_string(stdin)?;
    let checksum = ChecksumAlgorithm::parse(client_checksum.trim())?;

    let client_compression = recv_string(stdin)?;
    let compression = CompressionAlgorithm::parse(client_compression.trim())?;

    Ok(NegotiationResult {
        checksum,
        compression,
    })
}

/// Sends a negotiation string to the remote side.
///
/// Format: varint(length) + string_bytes
///
/// This matches upstream's `write_buf()` behavior in negotiation context
/// (compat.c:505-530).
fn send_string(writer: &mut dyn Write, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    write_varint(writer, bytes.len() as i32)?;
    writer.write_all(bytes)
}

/// Receives a negotiation string from the remote side.
///
/// Format: varint(length) + string_bytes
///
/// This matches upstream's `read_buf()` behavior in negotiation context
/// (compat.c:368-386).
///
/// # Encoding Notes
///
/// Negotiation strings are algorithm names (e.g., "md5", "zlib") which are
/// pure ASCII and thus valid UTF-8. However, rsync also supports charset
/// negotiation (iconv) for filename encoding, which is a separate mechanism.
///
/// The current implementation assumes UTF-8, which is safe for algorithm names
/// but may need extension for charset negotiation in the future.
fn recv_string(reader: &mut dyn Read) -> io::Result<String> {
    let len = read_varint(reader)? as usize;

    // Sanity check: negotiation strings should be small
    // Upstream uses a 1024-byte buffer (compat.c:537)
    if len > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("negotiation string too long: {len} bytes"),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;

    // Algorithm names are ASCII (subset of UTF-8), but validate anyway
    // to catch protocol errors early (e.g., reading from wrong stream layer)
    String::from_utf8(buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UTF-8 in negotiation string: {e}"),
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

        let result = negotiate_capabilities(protocol, &mut stdin, &mut stdout, true).unwrap();

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

        // Simulate client choosing md5 and zlib
        // Format: varint(len) + string, so 3 + "md5" + 4 + "zlib"
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(protocol, &mut stdin, &mut stdout, true).unwrap();

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

        // Simulate client choosing md5 and zstd
        // Format: varint(len) + string, so 3 + "md5" + 4 + "zstd"
        let client_response = b"\x03md5\x04zstd";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(protocol, &mut stdin, &mut stdout, true).unwrap();

        assert_eq!(result.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result.compression, CompressionAlgorithm::Zstd);
    }

    #[test]
    fn test_send_recv_string_roundtrip() {
        let test_str = "md5 md4 sha1 xxh128";
        let mut buffer = Vec::new();

        send_string(&mut buffer, test_str).unwrap();

        let mut reader = &buffer[..];
        let received = recv_string(&mut reader).unwrap();

        assert_eq!(received, test_str);
    }

    #[test]
    fn test_recv_string_length_limit() {
        // Create a varint that claims 10000 bytes
        let mut buffer = Vec::new();
        write_varint(&mut buffer, 10000).unwrap();
        buffer.extend_from_slice(&[b'x'; 100]); // But only provide 100

        let mut reader = &buffer[..];
        let result = recv_string(&mut reader);

        // Should fail because length exceeds limit or can't read enough bytes
        assert!(result.is_err());
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
}
