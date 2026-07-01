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

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use logging::debug_log;
#[cfg(test)]
use protocol::wire::write_token_stream;
use protocol::wire::{
    CHUNK_SIZE, CompressedTokenEncoder, DeltaOp, write_token_block_match, write_token_end,
    write_token_literal,
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
/// `workers` plumbs `--compress-threads=N` through to zstd's
/// `ZSTD_c_nbWorkers`. Ignored for non-zstd algorithms.
///
/// upstream: token.c dispatches on `do_compression` to select the codec.
/// upstream: token.c:701 - `ZSTD_CCtx_setParameter(.., ZSTD_c_nbWorkers, ..)`
pub(super) fn create_token_encoder(
    algo: CompressionAlgorithm,
    workers: Option<std::num::NonZeroU8>,
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
        CompressionAlgorithm::Zstd => Ok(Some(CompressedTokenEncoder::new_zstd(3, workers)?)),
        #[cfg(feature = "lz4")]
        CompressionAlgorithm::LZ4 => {
            let _ = workers;
            Ok(Some(CompressedTokenEncoder::new_lz4()))
        }
        _ => {
            let _ = workers;
            Ok(None)
        }
    }
}

/// Soft warning threshold for whole-file transfers (8 GB).
///
/// Files of any size can be transferred, but very large whole-file transfers
/// generate a debug log warning. For files over this size, delta transfers
/// with a basis file are strongly preferred to reduce bandwidth.
pub(super) const LARGE_FILE_WARNING_THRESHOLD: u64 = 8 * 1024 * 1024 * 1024; // 8 GB

/// Concrete source-file and destination-socket descriptors for the SERVE path.
///
/// NSV-1 plumbing: threads the raw source `File` descriptor and, when available,
/// the raw destination socket descriptor down to [`stream_whole_file_transfer`]
/// so a later applicability gate (NSV-3) and platform sender (NSV-6..10) can
/// engage a zero-copy file->socket path (sendfile / TransmitFile / splice).
///
/// Passed as `Option<ServeFds>`: `None` for transports without a usable fd pair
/// (SSH pipe, stdio, TLS) or callers that never touch a socket (tests). When the
/// source is a plain `File` but the socket fd is not reachable through the writer
/// abstraction, `dst_fd` is `None` and the source fd is still surfaced so the
/// future gate can decide.
///
/// This struct is purely additive: the current transfer never reads these fds,
/// so wire bytes, stats, and output are byte-for-byte unchanged. The fields are
/// intentionally unread until the NSV-3 zero-copy gate consumes them.
#[allow(dead_code)]
pub(super) struct ServeFds {
    /// Raw descriptor of the concrete source `File`, when the source is a plain
    /// file (not an io_uring reader or a `BufReader`).
    #[cfg(unix)]
    pub src_fd: std::os::fd::RawFd,
    /// Raw descriptor of the destination socket, when the writer wraps a
    /// concrete `TcpStream`. `None` for pipe/stdio/TLS writers or any writer
    /// that erases its fd behind `dyn Write`.
    #[cfg(unix)]
    pub dst_fd: Option<std::os::fd::RawFd>,
    /// Windows placeholder so the struct stays constructible on all platforms
    /// while zero-copy remains Unix-only for now (NSV-6..10 add TransmitFile).
    #[cfg(not(unix))]
    pub _unsupported: (),
}

/// Result of streaming a whole file to the wire.
///
/// Returned by [`stream_whole_file_transfer`] with the whole-file checksum that
/// the sender appends after the token stream. The wire byte count is tracked by
/// the caller's `CountingWriter` (post-compression), so it is not duplicated here.
pub(super) struct StreamResult {
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
    serve_fds: Option<ServeFds>,
) -> io::Result<StreamResult> {
    // NSV-1: `serve_fds` carries the concrete source-file and destination-socket
    // descriptors for the daemon SERVE path. It is plumbed but unused here. A
    // future zero-copy gate (NSV-3) will branch at this point: when `serve_fds`
    // yields both a source fd and a socket fd, no compression/checksum-only
    // constraints apply, and the platform supports it, dispatch to a
    // sendfile/TransmitFile/splice sender (NSV-6..10) instead of the read->hash->
    // write loop below. Until then the bytes flow through the existing path
    // unchanged, so wire output and stats are byte-for-byte identical.
    let _ = &serve_fds;

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

    let mut remaining = file_size;

    if let Some(encoder) = encoder {
        buf.resize(read_size, 0);
        while remaining > 0 {
            let to_read = buf.len().min(remaining as usize);
            source.read_exact(&mut buf[..to_read])?;
            verifier.update(&buf[..to_read]);
            encoder.send_literal(writer, &buf[..to_read])?;
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
            remaining -= to_read as u64;
        }
        write_token_end(writer)?;
    }

    let mut checksum_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let checksum_len = verifier.finalize_into(&mut checksum_buf);

    Ok(StreamResult {
        checksum_buf,
        checksum_len,
    })
}

/// Converts engine delta script to wire protocol delta operations.
///
/// Takes ownership of the script to avoid cloning literal data. Multi-block
/// Copy tokens emitted by the seq-match optimization (where a single
/// `DeltaToken::Copy` covers a run of `N` adjacent basis blocks) are expanded
/// here into `N` consecutive `DeltaOp::Copy` entries with `length =
/// block_length`. This keeps the wire byte stream byte-identical to the
/// no-coalesce baseline: each basis block is still emitted as one
/// `write_int(-(block_index + 1))` token by the wire layer.
///
/// `block_length` is the canonical length of a full basis block. Tokens whose
/// stored `len` is not a multiple of `block_length` are emitted unchanged so
/// last-block tail copies (when the basis ends with a short block) round-trip
/// cleanly.
///
/// # Upstream Reference
///
/// - `match.c:matched()` - emits tokens as they are generated
/// - `token.c:send_token()` - writes tokens to the wire
pub(super) fn script_to_wire_delta(script: DeltaScript, block_length: u32) -> Vec<DeltaOp> {
    let block_len = block_length as usize;
    let mut ops = Vec::with_capacity(script.tokens().len());
    for token in script.into_tokens() {
        match token {
            DeltaToken::Literal(data) => ops.push(DeltaOp::Literal(data)),
            DeltaToken::Copy { index, len } => {
                if block_len > 0 && len > block_len && len % block_len == 0 {
                    let run = len / block_len;
                    for k in 0..run {
                        ops.push(DeltaOp::Copy {
                            block_index: (index + k as u64) as u32,
                            length: block_length,
                        });
                    }
                } else {
                    ops.push(DeltaOp::Copy {
                        block_index: index as u32,
                        length: len as u32,
                    });
                }
            }
        }
    }
    ops
}

/// Writes delta tokens to the wire, using compression if enabled.
///
/// Superseded by [`write_delta_with_inline_checksum`] which merges checksum
/// computation into the same pass. Retained for tests that verify compression
/// independently of checksumming.
///
/// # Upstream Reference
///
/// - `token.c:send_token()` - switches between simple and deflated token sending
/// - `token.c:send_deflated_token()` lines 460-484 - dictionary sync after block match
/// - `token.c:see_deflate_token()` lines 631-670 - receiver-side dictionary sync
#[cfg(test)]
pub(super) fn write_delta_with_compression<W: Write>(
    writer: &mut W,
    ops: &[DeltaOp],
    encoder: Option<&mut CompressedTokenEncoder>,
    is_zlib: bool,
    source_path: &Path,
    use_noatime: bool,
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
                Some(io::BufReader::new(
                    super::open_source::open_source_with_noatime(source_path, use_noatime)?,
                ))
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

/// Result of writing a delta to the wire with inline checksum computation.
pub(super) struct InlineChecksumResult {
    /// Whole-file checksum computed during the wire-write pass.
    pub checksum_buf: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    /// Number of valid bytes in `checksum_buf`.
    pub checksum_len: usize,
    /// Bytes covered by Copy tokens (block matches) on this file.
    /// upstream: `match.c:118` `stats.matched_data += s2length`.
    pub matched_data: u64,
    /// Bytes sent as literal data on this file.
    /// upstream: `match.c:330` `stats.literal_data += s->sums[j].len`.
    pub literal_data: u64,
}

/// Writes delta tokens to the wire and computes the file checksum in a single pass.
///
/// Merges the work of `write_delta_with_compression` and `compute_file_checksum`
/// into one iteration over the delta ops. For Copy tokens, source data is read
/// once and fed to both the checksum verifier and (for CPRES_ZLIB) the compressor
/// dictionary, eliminating the extra file open+read pass that
/// `compute_file_checksum` previously performed.
///
/// # Upstream Reference
///
/// Mirrors upstream `match.c:matched()` where `sum_update()` and `send_token()`
/// operate on the same `map_ptr()` data in a single pass, and
/// `token.c:send_deflated_token()` feeds the same data to the compressor
/// dictionary.
pub(super) fn write_delta_with_inline_checksum<W: Write>(
    writer: &mut W,
    ops: &[DeltaOp],
    encoder: Option<&mut CompressedTokenEncoder>,
    is_zlib: bool,
    source_path: &Path,
    use_noatime: bool,
    checksum_algorithm: ChecksumAlgorithm,
) -> io::Result<InlineChecksumResult> {
    let mut verifier = ChecksumVerifier::for_algorithm(checksum_algorithm);

    // Lazily open source file only when Copy tokens are present.
    // A single file handle serves both checksum and dictionary sync.
    let has_copies = ops.iter().any(|op| matches!(op, DeltaOp::Copy { .. }));
    let mut source_file = if has_copies {
        Some(io::BufReader::new(
            super::open_source::open_source_with_noatime(source_path, use_noatime)?,
        ))
    } else {
        None
    };
    let mut source_offset: u64 = 0;
    let mut read_buf = Vec::new();
    // upstream: match.c matched()/send_token() accumulate stats.matched_data
    // and stats.literal_data as each token is emitted on the sender.
    let mut matched_data: u64 = 0;
    let mut literal_data: u64 = 0;

    match encoder {
        Some(encoder) => {
            let needs_dict_sync = is_zlib && has_copies;

            for op in ops {
                match op {
                    DeltaOp::Literal(data) => {
                        verifier.update(data);
                        encoder.send_literal(writer, data)?;
                        source_offset += data.len() as u64;
                        literal_data += data.len() as u64;
                    }
                    DeltaOp::Copy {
                        block_index,
                        length,
                    } => {
                        encoder.send_block_match(writer, *block_index)?;

                        // Read block data once, feed to both checksum and dict sync.
                        let len = *length as usize;
                        read_buf.clear();
                        read_buf.resize(len, 0);
                        if let Some(ref mut file) = source_file {
                            file.seek(SeekFrom::Start(source_offset))?;
                            file.read_exact(&mut read_buf)?;
                        }
                        verifier.update(&read_buf);
                        if needs_dict_sync {
                            encoder.see_token(&read_buf)?;
                        }
                        source_offset += u64::from(*length);
                        matched_data += u64::from(*length);
                    }
                }
            }

            encoder.finish(writer)?;
        }
        None => {
            // Uncompressed path: write tokens and compute checksum in one pass.
            for op in ops {
                match op {
                    DeltaOp::Literal(data) => {
                        verifier.update(data);
                        write_token_literal(writer, data)?;
                        // Advance source cursor past the literal so subsequent
                        // Copy tokens read source bytes from the correct offset
                        // for inline checksum computation. upstream:
                        // match.c:matched() interleaves literal and block-match
                        // tokens against a single advancing file cursor.
                        source_offset += data.len() as u64;
                        literal_data += data.len() as u64;
                    }
                    DeltaOp::Copy {
                        block_index,
                        length,
                    } => {
                        write_token_block_match(writer, *block_index)?;

                        // Read block data for checksum computation.
                        let len = *length as usize;
                        read_buf.clear();
                        read_buf.resize(len, 0);
                        if let Some(ref mut file) = source_file {
                            file.seek(SeekFrom::Start(source_offset))?;
                            file.read_exact(&mut read_buf)?;
                        }
                        verifier.update(&read_buf);
                        source_offset += u64::from(*length);
                        matched_data += u64::from(*length);
                    }
                }
            }
            write_token_end(writer)?;
        }
    }

    let mut checksum_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let checksum_len = verifier.finalize_into(&mut checksum_buf);

    Ok(InlineChecksumResult {
        checksum_buf,
        checksum_len,
        matched_data,
        literal_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "zstd")]
    #[test]
    fn create_token_encoder_zstd_no_workers() {
        let encoder = create_token_encoder(CompressionAlgorithm::Zstd, None)
            .expect("zstd encoder creation should succeed");
        assert!(encoder.is_some(), "zstd should produce an encoder");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn create_token_encoder_zstd_with_workers() {
        let workers = std::num::NonZeroU8::new(1);
        let encoder = create_token_encoder(CompressionAlgorithm::Zstd, workers)
            .expect("zstd encoder with workers=1 should succeed");
        assert!(encoder.is_some(), "zstd should produce an encoder");
    }

    #[test]
    fn create_token_encoder_zlib_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(CompressionAlgorithm::Zlib, workers)
            .expect("zlib encoder should succeed even with workers");
        assert!(encoder.is_some(), "zlib should produce an encoder");
    }

    #[test]
    fn create_token_encoder_zlibx_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(CompressionAlgorithm::ZlibX, workers)
            .expect("zlibx encoder should succeed even with workers");
        assert!(encoder.is_some(), "zlibx should produce an encoder");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn create_token_encoder_lz4_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(CompressionAlgorithm::LZ4, workers)
            .expect("lz4 encoder should succeed even with workers");
        assert!(encoder.is_some(), "lz4 should produce an encoder");
    }

    /// Reverse-daemon-delta regression: with a delta script that interleaves
    /// Copy and Literal tokens, the uncompressed inline-checksum path must
    /// read source bytes for each Copy from the correct file offset. The bug
    /// fixed alongside this test left `source_offset` un-incremented after a
    /// Literal, so any Copy that followed a Literal hashed source data from a
    /// stale (earlier) offset and the final whole-file checksum disagreed with
    /// the receiver's reconstructed-file checksum, causing
    /// "failed verification -- update discarded" on a push that produced a
    /// byte-correct reconstructed file.
    ///
    /// upstream: match.c:matched() advances a single file cursor through both
    /// literal and block-match tokens; the inline-checksum sender must mirror
    /// that invariant.
    #[test]
    fn write_delta_inline_checksum_advances_source_offset_past_literals() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Construct a source file with three distinct regions of known
        // content. A delta script that emits Copy(0), Literal(B), Copy(C)
        // forces the bug: after writing Literal B, the next Copy must read
        // source[B+block_a..] not source[block_a..].
        let block_a: Vec<u8> = (0..16u8).cycle().take(64).collect();
        let lit_b: Vec<u8> = (32..96u8).collect(); // 64 bytes, distinct
        let block_c: Vec<u8> = (128..192u8).collect(); // 64 bytes, distinct
        let mut source = Vec::new();
        source.extend_from_slice(&block_a);
        source.extend_from_slice(&lit_b);
        source.extend_from_slice(&block_c);

        let mut temp = NamedTempFile::new().expect("temp file");
        temp.write_all(&source).expect("write source");
        temp.flush().expect("flush");
        let source_path = temp.path().to_path_buf();

        // Build the wire-op sequence by hand. The block_index values are
        // wire token indices only; the Copy reads the LOCAL source file for
        // checksum purposes, which is what this regression exercises.
        let ops = vec![
            DeltaOp::Copy {
                block_index: 0,
                length: block_a.len() as u32,
            },
            DeltaOp::Literal(lit_b.clone()),
            DeltaOp::Copy {
                block_index: 1,
                length: block_c.len() as u32,
            },
        ];

        let mut wire = Vec::new();
        let result = write_delta_with_inline_checksum(
            &mut wire,
            &ops,
            None,
            false,
            &source_path,
            false,
            ChecksumAlgorithm::MD5,
        )
        .expect("write_delta_with_inline_checksum");

        // Compare against the checksum of the actual source bytes - which is
        // what the receiver computes over the reconstructed file. Pre-fix
        // these diverged because the second Copy hashed source[block_a.len()..]
        // (lit_b bytes) instead of source[block_a.len()+lit_b.len()..]
        // (block_c bytes).
        let mut expected = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        expected.update(&source);
        let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let expected_len = expected.finalize_into(&mut expected_buf);

        assert_eq!(
            result.checksum_len, expected_len,
            "checksum length mismatch"
        );
        assert_eq!(
            &result.checksum_buf[..result.checksum_len],
            &expected_buf[..expected_len],
            "inline checksum must equal checksum of source bytes covered by the script",
        );
    }
}
