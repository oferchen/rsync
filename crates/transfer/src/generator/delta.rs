//! Delta generation, file streaming, and checksum computation.
//!
//! Provides the core delta pipeline for the generator (sender) role:
//! signature reconstruction from wire format, delta script generation via
//! [`DeltaGenerator`], whole-file streaming with inline checksumming, and
//! compressed token encoding for wire transmission.
//!
//! # Upstream Reference
//!
//! - `match.c` - Block matching and delta generation
//! - `sender.c:354-430` - File transfer with delta or whole-file paths
//! - `token.c` - Token encoding with optional compression

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use logging::debug_log;
use protocol::wire::{
    CHUNK_SIZE, CompressedTokenEncoder, DeltaOp, write_token_end, write_token_stream,
};
use protocol::{ChecksumAlgorithm, CompressionAlgorithm};

use engine::delta::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken};

use super::super::delta_apply::ChecksumVerifier;
use super::super::delta_config::DeltaGeneratorConfig;
use super::super::shared::ChecksumFactory;
use crate::role_trailer::error_location;

/// Creates a `CompressedTokenEncoder` for the given compression algorithm.
///
/// Returns `None` for algorithms that don't use per-token compression.
/// The encoder should be created once per transfer session and reused across
/// files - upstream rsync uses a single compression context for the entire
/// session. Call `encoder.reset()` is handled internally by `finish()`.
///
/// upstream: token.c dispatches on `do_compression` to select the codec.
pub(super) fn create_token_encoder(
    algo: CompressionAlgorithm,
) -> io::Result<Option<CompressedTokenEncoder>> {
    match algo {
        CompressionAlgorithm::Zlib | CompressionAlgorithm::ZlibX => {
            let mut enc = CompressedTokenEncoder::default();
            if algo == CompressionAlgorithm::ZlibX {
                enc.set_zlibx(true);
            }
            Ok(Some(enc))
        }
        #[cfg(feature = "zstd")]
        CompressionAlgorithm::Zstd => Ok(Some(CompressedTokenEncoder::new_zstd(3)?)),
        #[cfg(feature = "lz4")]
        CompressionAlgorithm::LZ4 => Ok(Some(CompressedTokenEncoder::new_lz4())),
        _ => Ok(None),
    }
}

/// Soft warning threshold for whole-file transfers (8 GB).
///
/// Files of any size can be transferred, but very large whole-file transfers
/// generate a debug log warning. For files over this size, delta transfers
/// with a basis file are strongly preferred to reduce bandwidth.
pub(super) const LARGE_FILE_WARNING_THRESHOLD: u64 = 8 * 1024 * 1024 * 1024; // 8 GB

/// Result of streaming a whole file to the wire.
///
/// Returned by [`stream_whole_file_transfer`] with the byte count and
/// whole-file checksum that the sender appends after the token stream.
pub(super) struct StreamResult {
    /// Total bytes of file content written to the wire.
    pub total_bytes: u64,
    /// Whole-file checksum computed during streaming.
    pub checksum_buf: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    /// Number of valid bytes in `checksum_buf`.
    pub checksum_len: usize,
}

/// Generates a delta script from a received signature.
///
/// Reconstructs the signature from wire format blocks, builds a hash index
/// for O(1) block lookup, and runs the rolling-checksum delta algorithm
/// against the source file.
///
/// Takes ownership of `sig_blocks` via the config to avoid cloning strong_sum
/// data, which can be expensive for files with many signature blocks.
///
/// # Upstream Reference
///
/// - `sender.c:389-430` - delta generation path after `receive_sums()`
/// - `match.c:hash_search()` - rolling checksum block matching
pub fn generate_delta_from_signature<R: Read>(
    source: R,
    config: DeltaGeneratorConfig<'_>,
) -> io::Result<DeltaScript> {
    use checksums::RollingDigest;
    use engine::delta::SignatureLayout;
    use engine::signature::{FileSignature, SignatureBlock};
    use std::num::{NonZeroU8, NonZeroU32};

    // Reconstruct engine signature from wire format
    let block_length_nz = NonZeroU32::new(config.block_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "block length must be non-zero {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )
    })?;

    let strong_sum_length_nz = NonZeroU8::new(config.strong_sum_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "strong sum length must be non-zero {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )
    })?;

    let block_count = config.sig_blocks.len() as u64;

    // Reconstruct signature layout (remainder unknown, set to 0)
    let layout = SignatureLayout::from_raw_parts(
        block_length_nz,
        0, // remainder unknown from wire format
        block_count,
        strong_sum_length_nz,
    );

    // Convert wire blocks to engine signature blocks (consumes sig_blocks)
    let engine_blocks: Vec<SignatureBlock> = config
        .sig_blocks
        .into_iter()
        .map(|wire_block| {
            SignatureBlock::from_raw_parts(
                wire_block.index as u64,
                RollingDigest::from_value(wire_block.rolling_sum, config.block_length as usize),
                &wire_block.strong_sum,
            )
        })
        .collect();

    // Calculate total bytes (approximation since we don't know exact remainder)
    let total_bytes = (block_count.saturating_sub(1)) * u64::from(config.block_length);
    let signature = FileSignature::from_raw_parts(layout, engine_blocks, total_bytes);

    // Select checksum algorithm using ChecksumFactory (handles negotiated vs default)
    let checksum_factory = ChecksumFactory::from_negotiation(
        config.negotiated_algorithms,
        config.protocol,
        config.checksum_seed,
        config.compat_flags,
    );
    let checksum_algorithm = checksum_factory.signature_algorithm();

    let index =
        DeltaSignatureIndex::from_signature(&signature, checksum_algorithm).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "failed to create signature index {}{}",
                    error_location!(),
                    crate::role_trailer::sender()
                ),
            )
        })?;

    let generator = DeltaGenerator::new();
    generator.generate(source, &index).map_err(|e| {
        io::Error::other(format!(
            "delta generation failed: {e} {}{}",
            error_location!(),
            crate::role_trailer::sender()
        ))
    })
}

/// Streams a whole file to the wire in a single pass: read -> hash -> write.
///
/// Eliminates the `DeltaScript` intermediate representation. Each chunk is read
/// into a reusable buffer, fed to the checksum verifier, and written directly
/// to the wire. This reduces memory passes from 3 to 1 and eliminates
/// per-file allocation for the many-small-files case.
///
/// The buffer `buf` is caller-owned and reused across files to avoid allocation.
///
/// # Wire format
///
/// Produces the same byte sequence as the previous `DeltaScript`-based path:
/// `[write_int(len) + data]` per 32KB chunk, followed by `write_int(0)` end marker.
///
/// # Upstream Reference
///
/// Mirrors upstream `match.c` interleaved pattern where `sum_update()` and
/// `send_token()` happen on the same data pass.
pub(super) fn stream_whole_file_transfer<R: Read, W: Write>(
    writer: &mut W,
    mut source: R,
    file_size: u64,
    checksum_algorithm: ChecksumAlgorithm,
    encoder: Option<&mut CompressedTokenEncoder>,
    buf: &mut Vec<u8>,
) -> io::Result<StreamResult> {
    if file_size > LARGE_FILE_WARNING_THRESHOLD {
        debug_log!(
            Send,
            1,
            "Large whole-file transfer: {} bytes ({:.2} GB). Consider using delta mode.",
            file_size,
            file_size as f64 / (1024.0 * 1024.0 * 1024.0)
        );
    }

    let mut verifier = ChecksumVerifier::for_algorithm(checksum_algorithm);

    // Read buffer sized for fewer syscalls (up to 256KB per read).
    // Buffer is reused across files - no allocation after the first large file.
    const MAX_READ_SIZE: usize = 256 * 1024;
    let read_size = (file_size as usize).clamp(1, MAX_READ_SIZE);

    let mut total_bytes: u64 = 0;
    let mut remaining = file_size;

    if let Some(encoder) = encoder {
        buf.resize(read_size, 0);
        while remaining > 0 {
            let to_read = buf.len().min(remaining as usize);
            source.read_exact(&mut buf[..to_read])?;
            verifier.update(&buf[..to_read]);
            encoder.send_literal(writer, &buf[..to_read])?;
            total_bytes += to_read as u64;
            remaining -= to_read as u64;
        }
        encoder.finish(writer)?;
    } else {
        // Reserve 4 bytes at front for the length prefix of each wire chunk.
        // Data is read at buf[4..], then for each 32KB wire chunk the 4-byte
        // length prefix is written into the space before the chunk data.
        // The combined write (4 + 32768 = 32772 bytes) exceeds MultiplexWriter's
        // 32KB buffer threshold, triggering direct-send to the socket layer
        // and bypassing one memcpy. Upstream reference: match.c send_token().
        buf.resize(4 + read_size, 0);
        while remaining > 0 {
            let to_read = (buf.len() - 4).min(remaining as usize);
            source.read_exact(&mut buf[4..4 + to_read])?;
            verifier.update(&buf[4..4 + to_read]);
            // Write wire chunks with combined [length_prefix + data].
            // For offset 0, the prefix uses the reserved buf[0..4].
            // For subsequent offsets, the prefix overwrites already-sent bytes.
            let mut wire_off = 0;
            while wire_off < to_read {
                let chunk = (to_read - wire_off).min(CHUNK_SIZE);
                buf[wire_off..wire_off + 4].copy_from_slice(&(chunk as i32).to_le_bytes());
                writer.write_all(&buf[wire_off..wire_off + 4 + chunk])?;
                wire_off += chunk;
            }
            total_bytes += to_read as u64;
            remaining -= to_read as u64;
        }
        write_token_end(writer)?;
    }

    let mut checksum_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let checksum_len = verifier.finalize_into(&mut checksum_buf);

    Ok(StreamResult {
        total_bytes,
        checksum_buf,
        checksum_len,
    })
}

/// Computes the file transfer checksum from delta script data.
///
/// Hashes ALL bytes described by the delta script - both literal data and
/// block-reference data read from the source file. Upstream rsync feeds every
/// byte that will appear in the reconstructed file through `sum_update()`,
/// including matched-block data read back from the source via `map_ptr()`.
///
/// # Upstream Reference
///
/// - `match.c:370` - `sum_init(xfer_sum_nni, checksum_seed)`
/// - `match.c:383-385` - `sum_update()` on literal data
/// - `match.c:matched():131-135` - `sum_update()` on matched block data
/// - `checksum.c:686` - `sum_end(char *sum)` finalizes into caller buffer
pub(super) fn compute_file_checksum(
    script: &DeltaScript,
    algorithm: ChecksumAlgorithm,
    _seed: i32,
    _compat_flags: Option<&protocol::CompatibilityFlags>,
    source_path: &Path,
    block_length: u32,
) -> io::Result<([u8; ChecksumVerifier::MAX_DIGEST_LEN], usize)> {
    let mut verifier = ChecksumVerifier::for_algorithm(algorithm);

    // Lazily open source file only when Copy tokens are present.
    let mut source_file: Option<fs::File> = None;
    let mut read_buf = Vec::new();

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => {
                verifier.update(data);
            }
            DeltaToken::Copy { index, len } => {
                // upstream: match.c:matched() - re-reads block data from source
                // and feeds it through sum_update() for whole-file checksum.
                let file = match source_file.as_mut() {
                    Some(f) => f,
                    None => {
                        source_file = Some(fs::File::open(source_path)?);
                        source_file.as_mut().unwrap()
                    }
                };
                let offset = *index * u64::from(block_length);
                file.seek(SeekFrom::Start(offset))?;
                read_buf.resize(*len, 0);
                file.read_exact(&mut read_buf)?;
                verifier.update(&read_buf);
            }
        }
    }

    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = verifier.finalize_into(&mut buf);
    Ok((buf, len))
}

/// Converts engine delta script to wire protocol delta operations.
///
/// Takes ownership of the script to avoid cloning literal data.
///
/// # Upstream Reference
///
/// - `match.c:matched()` - emits tokens as they are generated
/// - `token.c:send_token()` - writes tokens to the wire
pub(super) fn script_to_wire_delta(script: DeltaScript) -> Vec<DeltaOp> {
    script
        .into_tokens()
        .into_iter()
        .map(|token| match token {
            DeltaToken::Literal(data) => DeltaOp::Literal(data),
            DeltaToken::Copy { index, len } => DeltaOp::Copy {
                block_index: index as u32,
                length: len as u32,
            },
        })
        .collect()
}

/// Writes delta tokens to the wire, using compression if enabled.
///
/// When compression is None or `CompressionAlgorithm::None`, uses the plain
/// token format (`write_token_stream`). Otherwise, uses the compressed token
/// format with DEFLATED_DATA headers as expected by upstream rsync.
///
/// For CPRES_ZLIB mode, after each block match token the matched block's data
/// is fed to the compressor dictionary via `see_token()`, keeping the deflate
/// stream synchronized between sender and receiver. The receiver performs the
/// corresponding `see_deflate_token()` call. CPRES_ZLIBX skips this step.
///
/// # Arguments
///
/// * `writer` - Output stream for compressed tokens
/// * `ops` - Delta operations to encode
/// * `compression` - Negotiated compression algorithm
/// * `source_path` - Path to the source file, needed to re-read matched block
///   data for CPRES_ZLIB dictionary synchronization
///
/// # Upstream Reference
///
/// - `token.c:send_token()` - switches between simple and deflated token sending
/// - `token.c:send_deflated_token()` lines 460-484 - dictionary sync after block match
/// - `token.c:see_deflate_token()` lines 631-670 - receiver-side dictionary sync
pub(super) fn write_delta_with_compression<W: Write>(
    writer: &mut W,
    ops: &[DeltaOp],
    encoder: Option<&mut CompressedTokenEncoder>,
    is_zlib: bool,
    source_path: &Path,
) -> io::Result<()> {
    match encoder {
        Some(encoder) => {
            // For CPRES_ZLIB dictionary sync we need to re-read matched block
            // data from the source file. Track the cumulative offset as we
            // process tokens sequentially (they describe the source file in
            // order). Upstream: token.c:send_deflated_token() lines 463-484.
            let needs_dict_sync =
                is_zlib && ops.iter().any(|op| matches!(op, DeltaOp::Copy { .. }));
            let mut source_file = if needs_dict_sync {
                Some(io::BufReader::new(fs::File::open(source_path)?))
            } else {
                None
            };
            let mut source_offset: u64 = 0;
            let mut see_buf = Vec::new();

            for op in ops {
                match op {
                    DeltaOp::Literal(data) => {
                        encoder.send_literal(writer, data)?;
                        source_offset += data.len() as u64;
                    }
                    DeltaOp::Copy {
                        block_index,
                        length,
                    } => {
                        encoder.send_block_match(writer, *block_index)?;

                        // upstream: token.c:463-484 - feed block data to the
                        // compressor dictionary so the deflate stream stays in
                        // sync with what the receiver sees.
                        if let Some(ref mut file) = source_file {
                            let len = *length as usize;
                            see_buf.clear();
                            see_buf.resize(len, 0);
                            file.seek(SeekFrom::Start(source_offset))?;
                            file.read_exact(&mut see_buf)?;
                            encoder.see_token(&see_buf)?;
                        }
                        source_offset += u64::from(*length);
                    }
                }
            }

            encoder.finish(writer)?;
            Ok(())
        }
        None => write_token_stream(writer, ops),
    }
}
