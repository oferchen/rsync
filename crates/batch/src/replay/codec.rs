//! Compression codec detection and decoder construction for batch replay.
//!
//! Upstream rsync write-batch forces `compress_choice = "zlib"` (compat.c:413-414),
//! so batch files from upstream always contain zlib-compressed data. However,
//! upstream rsync 3.4.1+ with SUPPORT_ZSTD can auto-negotiate zstd for live
//! transfers, and a hypothetical or patched upstream could produce batch files
//! with zstd-compressed tokens. This module detects the actual codec from the
//! compressed payload to handle both cases correctly.

#[cfg(feature = "zstd")]
use std::fs::File;
#[cfg(feature = "zstd")]
use std::io::{BufReader, Read, Seek, SeekFrom};

use protocol::wire::CompressedTokenDecoder;

#[cfg(feature = "zstd")]
use crate::error::BatchError;
use crate::error::BatchResult;

/// Compression codec used in a batch file's compressed token stream.
///
/// Upstream rsync write-batch forces `compress_choice = "zlib"` (compat.c:413-414),
/// so batch files from upstream always contain zlib-compressed data. However,
/// upstream rsync 3.4.1+ with SUPPORT_ZSTD can auto-negotiate zstd for live
/// transfers, and a hypothetical or patched upstream could produce batch files
/// with zstd-compressed tokens. oc-rsync detects the actual codec from the
/// compressed payload to handle both cases correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompressionCodec {
    /// Zlib/DEFLATE - the upstream default for batch files.
    /// upstream: compat.c:194-195 - batch read defaults to CPRES_ZLIB
    Zlib,
    /// Zstd - possible if the batch was written by a patched upstream or
    /// future rsync version that allows zstd in batch mode.
    #[cfg(feature = "zstd")]
    Zstd,
}

/// Creates a `CompressedTokenDecoder` for batch replay.
///
/// Selects the appropriate decoder based on the detected compression codec.
/// Upstream rsync write-batch forces zlib (compat.c:413-414), and read-batch
/// defaults to CPRES_ZLIB (compat.c:194-195). oc-rsync extends this by
/// auto-detecting the codec from the compressed payload, allowing it to
/// read batch files regardless of which algorithm was used during recording.
///
/// For zlib: sets zlibx=false so `see_token()` feeds matched block data
/// into the inflate dictionary. Without this, inflate fails with "invalid
/// distance too far back".
///
/// For zstd: `see_token()` is a noop - no dictionary synchronization needed.
///
/// upstream: compat.c:194-195 - batch read defaults to CPRES_ZLIB
/// upstream: token.c:see_deflate_token() - feeds block data into inflate dictionary
pub(super) fn create_compressed_decoder(
    codec: CompressionCodec,
) -> BatchResult<CompressedTokenDecoder> {
    match codec {
        CompressionCodec::Zlib => {
            let mut decoder = CompressedTokenDecoder::new();
            decoder.set_zlibx(false);
            Ok(decoder)
        }
        #[cfg(feature = "zstd")]
        CompressionCodec::Zstd => CompressedTokenDecoder::new_zstd().map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to create zstd decoder for batch replay: {e}"),
            ))
        }),
    }
}

/// Detects the compression codec from the batch stream by peeking at compressed data.
///
/// Upstream rsync write-batch always uses zlib (compat.c:413-414), but a
/// patched or future upstream could produce zstd-compressed batch files.
/// This function peeks at the first DEFLATED_DATA block in the stream,
/// checks for the zstd magic number (`0xFD2FB528` LE), and returns the
/// detected codec. The stream position is restored after peeking.
///
/// The function scans forward from the current position looking for a byte
/// with the DEFLATED_DATA flag (upper 2 bits = 0x40). It reads the 2-byte
/// header to get the payload length, then checks the first 4 bytes of the
/// payload for the zstd frame magic. The stream is then seeked back to
/// the original position.
///
/// If the stream contains no DEFLATED_DATA blocks before EOF, or if an
/// I/O error occurs during peeking, falls back to zlib (the upstream default).
///
/// upstream: token.c:recv_deflated_token() - DEFLATED_DATA flag = 0x40
/// zstd spec: frames start with magic 0xFD2FB528 (LE bytes: 28 B5 2F FD)
#[cfg(feature = "zstd")]
pub(super) fn detect_compression_codec(reader: &mut BufReader<File>) -> CompressionCodec {
    let start_pos = match reader.stream_position() {
        Ok(pos) => pos,
        Err(_) => return CompressionCodec::Zlib,
    };

    let result = peek_for_codec(reader);

    // Always restore stream position regardless of detection result.
    let _ = reader.seek(SeekFrom::Start(start_pos));

    result.unwrap_or(CompressionCodec::Zlib)
}

/// Inner peek logic for codec detection, separated for clean error handling.
///
/// Scans the stream byte-by-byte looking for a DEFLATED_DATA header (flag
/// byte with upper 2 bits = 0x40). Once found, reads the payload length
/// from the 2-byte header and checks the first 4 bytes for the zstd magic.
///
/// Returns `None` if no DEFLATED_DATA block is found before EOF or on error.
#[cfg(feature = "zstd")]
fn peek_for_codec(reader: &mut BufReader<File>) -> Option<CompressionCodec> {
    // Scan for the first DEFLATED_DATA flag byte. The compressed token stream
    // starts with flag bytes that can be END_FLAG (0x00), TOKEN_LONG (0x20),
    // TOKENRUN_LONG (0x21), DEFLATED_DATA (0x40-0x7F), TOKEN_REL (0x80-0xBF),
    // or TOKENRUN_REL (0xC0-0xFF). We need to find a DEFLATED_DATA byte.
    //
    // Limit scan to 64KB to avoid reading the entire batch file.
    const SCAN_LIMIT: usize = 65536;
    let mut scanned = 0;

    while scanned < SCAN_LIMIT {
        let mut byte = [0u8; 1];
        if reader.read_exact(&mut byte).is_err() {
            return None;
        }
        scanned += 1;

        let flag = byte[0];

        // Check if this byte has the DEFLATED_DATA pattern (upper 2 bits = 01)
        if (flag & 0xC0) == 0x40 {
            // Read the second byte of the DEFLATED_DATA header
            let high = (flag & 0x3F) as usize;
            let mut low_buf = [0u8; 1];
            if reader.read_exact(&mut low_buf).is_err() {
                return None;
            }
            let len = (high << 8) | (low_buf[0] as usize);

            if len < 4 {
                // Payload too short to contain zstd magic - assume zlib.
                return Some(CompressionCodec::Zlib);
            }

            // Read the first 4 bytes of the compressed payload
            let mut magic_buf = [0u8; 4];
            if reader.read_exact(&mut magic_buf).is_err() {
                return None;
            }

            // Zstd frame magic: 0xFD2FB528 stored as LE bytes [0x28, 0xB5, 0x2F, 0xFD]
            #[cfg(feature = "zstd")]
            if magic_buf == [0x28, 0xB5, 0x2F, 0xFD] {
                return Some(CompressionCodec::Zstd);
            }

            return Some(CompressionCodec::Zlib);
        }

        // Skip over known flag types to avoid false DEFLATED_DATA matches.
        // TOKEN_LONG: 4-byte token follows
        if flag == 0x20 {
            let mut skip = [0u8; 4];
            if reader.read_exact(&mut skip).is_err() {
                return None;
            }
            scanned += 4;
            continue;
        }
        // TOKENRUN_LONG: 4-byte token + 2-byte run count
        if flag == 0x21 {
            let mut skip = [0u8; 6];
            if reader.read_exact(&mut skip).is_err() {
                return None;
            }
            scanned += 6;
            continue;
        }
        // TOKEN_REL (0x80-0xBF): no additional data
        if flag & 0xC0 == 0x80 {
            continue;
        }
        // TOKENRUN_REL (0xC0-0xFF): 2-byte run count follows
        if flag & 0xC0 == 0xC0 {
            let mut skip = [0u8; 2];
            if reader.read_exact(&mut skip).is_err() {
                return None;
            }
            scanned += 2;
            continue;
        }
        // END_FLAG (0x00) or other: continue scanning
    }

    None
}
