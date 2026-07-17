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

use compress::zlib::CompressionLevel;
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
    level: CompressionLevel,
    workers: Option<std::num::NonZeroU8>,
) -> io::Result<Option<CompressedTokenEncoder>> {
    match algo {
        CompressionAlgorithm::Zlib | CompressionAlgorithm::ZlibX => {
            // upstream: token.c:378 - deflateInit2() uses per_file_default_level
            // (= the negotiated do_compression_level). Protocol version 31 mirrors
            // the historical default; only the level varies per --compress-level.
            let mut enc = CompressedTokenEncoder::new(level, ZLIB_TOKEN_PROTOCOL_VERSION);
            if algo == CompressionAlgorithm::ZlibX {
                enc.set_zlibx(true);
            }
            Ok(Some(enc))
        }
        #[cfg(feature = "zstd")]
        // upstream: token.c:748 - ZSTD_CCtx_setParameter(.., ZSTD_c_compressionLevel,
        // do_compression_level). Negative "fast" levels pass through unchanged.
        CompressionAlgorithm::Zstd => Ok(Some(CompressedTokenEncoder::new_zstd(
            compress::zstd::level_to_i32(level),
            workers,
        )?)),
        #[cfg(feature = "lz4")]
        CompressionAlgorithm::LZ4 => {
            let _ = (workers, level);
            Ok(Some(CompressedTokenEncoder::new_lz4()))
        }
        _ => {
            let _ = (workers, level);
            Ok(None)
        }
    }
}

/// Selects the whole-stream compression level, applying upstream's
/// skip-compress "match all" special case.
///
/// When `match_all` is set (a daemon module's `dont compress = *`) and the
/// negotiated codec is zlib/zlibx, the entire deflate stream is initialised at
/// level 0 (store) instead of compressing per block. All other cases keep the
/// `configured` level.
///
/// upstream: token.c:206-211 `init_set_compression()` - a bare `*` in the
/// sender's dont-compress match list sets `per_file_default_level =
/// skip_compression_level` (`Z_NO_COMPRESSION` for zlib), which token.c:378
/// feeds into `deflateInit2()` to store the whole stream. zstd/lz4 keep
/// `do_compression_level` (token.c:748), so the match-all case never lowers
/// their level.
pub(super) fn whole_stream_compression_level(
    match_all: bool,
    codec: CompressionAlgorithm,
    configured: CompressionLevel,
) -> CompressionLevel {
    if match_all
        && matches!(
            codec,
            CompressionAlgorithm::Zlib | CompressionAlgorithm::ZlibX
        )
    {
        CompressionLevel::None
    } else {
        configured
    }
}

/// Protocol version historically used to initialise the zlib token encoder.
///
/// Matches the previous `CompressedTokenEncoder::default()` behaviour so this
/// change alters only the compression level, never the zlib token framing.
const ZLIB_TOKEN_PROTOCOL_VERSION: u32 = 31;

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
/// (SSH pipe or stdio) or callers that never touch a socket (tests). When the
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
    /// concrete `TcpStream`. `None` for pipe/stdio writers or any writer
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
    let needed = consecutive_match_needed(&config);
    let updating_basis_file = config.updating_basis_file;
    let index = build_signature_index(config)?;

    let generator = DeltaGenerator::new()
        .with_consecutive_match_needed(needed)
        .with_updating_basis_file(updating_basis_file);
    generator.generate(source, &index).map_err(|e| {
        io::Error::other(format!(
            "delta generation failed: {e} {}{}",
            error_location!(),
            crate::role_trailer::sender()
        ))
    })
}

/// Generates a delta script from a received signature using the opt-in
/// parallel scan when the basis is duplicate-free.
///
/// The caller (`generator::transfer::transfer_loop`) opens the source as a
/// memory-mapped slice and passes it here only after the size/core gate in
/// `should_parallel_delta` has fired. Because the mapping spans the whole
/// source file (pages fault in lazily during the scan), peak RSS for the
/// chunked scan is proportional to the file size, not bounded to a fixed
/// window; the win is CPU parallelism, not a memory reduction. This
/// function then reconstructs the signature index (sharing
/// `build_signature_index` with the sequential
/// [`generate_delta_from_signature`], so there is a single reconstruction
/// path) and decides:
///
/// - **Duplicate-free basis** ([`DeltaSignatureIndex::has_duplicate_blocks`]
///   is `false`): run [`DeltaGenerator::generate_chunked`], which scans the
///   ranges across rayon workers. Reconstruction and the total/literal byte
///   counts match the sequential scan on every input. For the eligible inputs
///   (matches never straddle a range boundary) the emitted token stream also
///   matches, and the wire bytes match in the common case; a rare literal-run
///   segmentation seam at a range boundary can still shift the literal-token
///   length framing by a few bytes (never the counts or the reconstructed
///   data). See [`DeltaGenerator::generate_chunked`] for the exact contract.
/// - **Duplicate-content basis**: fall back to the pruned sequential
///   [`DeltaGenerator::generate`] over the same slice, because the prune-off
///   parallel scan would resolve duplicate siblings differently and diverge
///   from the wire bytes the receiver expects.
///
/// `max_chunks` bounds the number of parallel ranges (the caller passes
/// `rayon::current_num_threads().min(8)`).
///
/// # Upstream Reference
///
/// The sender-side matching contract is unchanged from
/// [`generate_delta_from_signature`]; this only parallelizes the scan of a
/// single large file. See `match.c:hash_search()`.
pub fn generate_delta_from_signature_chunked(
    source: &[u8],
    config: DeltaGeneratorConfig<'_>,
    max_chunks: usize,
) -> io::Result<DeltaScript> {
    // Carry the negotiated consecutive-match threshold onto the parallel path:
    // when the mutual CAP_CONSECUTIVE_MATCH bit is set the receiver has halved
    // the strong-sum length, so the sender must apply seq_matches=2 gating here
    // too. `generate_chunked` routes needed >= 2 through the sequential gated
    // scan, so the parallel opt-in stays wire-transparent under the bit.
    let needed = consecutive_match_needed(&config);
    let updating_basis_file = config.updating_basis_file;
    let index = build_signature_index(config)?;

    let generator = DeltaGenerator::new()
        .with_consecutive_match_needed(needed)
        .with_updating_basis_file(updating_basis_file);
    let result = if index.has_duplicate_blocks() {
        // Duplicate-content basis: the prune-off parallel scan would diverge
        // from the pruned sequential wire bytes, so keep the sequential path.
        generator.generate(io::Cursor::new(source), &index)
    } else {
        generator.generate_chunked(source, &index, max_chunks)
    };

    result.map_err(|e| {
        io::Error::other(format!(
            "delta generation failed: {e} {}{}",
            error_location!(),
            crate::role_trailer::sender()
        ))
    })
}

/// Reconstructs the [`DeltaSignatureIndex`] from wire-format signature blocks.
///
/// Shared by [`generate_delta_from_signature`] and
/// [`generate_delta_from_signature_chunked`] so the wire-block -> engine
/// signature -> index reconstruction lives in exactly one place. Consumes
/// `config` because `sig_blocks` is moved into the engine signature to avoid
/// cloning strong-checksum data.
///
/// # Upstream Reference
///
/// - `sender.c:389-430` - delta generation path after `receive_sums()`
fn build_signature_index(config: DeltaGeneratorConfig<'_>) -> io::Result<DeltaSignatureIndex> {
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

    DeltaSignatureIndex::from_signature(&signature, checksum_algorithm).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "failed to create signature index {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )
    })
}

/// Derives the zsync consecutive-match gating threshold (`seq_matches`) from the
/// mutually negotiated compat flags.
///
/// Returns `2` only when the private `CAP_CONSECUTIVE_MATCH` bit is present -
/// the exact same condition under which the receiver halved the per-block
/// strong-sum length carried in `config.strong_sum_length`. The gating
/// compensates for the shorter, weaker checksum; the two are bound to one
/// negotiated bit so they can never diverge. Absent the bit (any upstream peer,
/// or no opt-in) the threshold stays at `1` and the scan is upstream-identical.
fn consecutive_match_needed(config: &DeltaGeneratorConfig<'_>) -> u8 {
    if config
        .compat_flags
        .is_some_and(|f| f.contains(protocol::CompatibilityFlags::CONSECUTIVE_MATCH))
    {
        2
    } else {
        1
    }
}

/// Computes upstream's per-file `updating_basis_file` flag (`sender.c:337`).
///
/// This gates the delta generator's backward-`Copy` suppression
/// ([`DeltaGenerator::with_updating_basis_file`], `match.c:211`): when the
/// receiver rewrites the basis in place, the basis being matched is the
/// destination itself, so a `Copy` whose basis offset precedes the source
/// cursor would read a region the receiver has already overwritten. The bytes
/// are still transferred (demoted to a literal), so reconstruction is
/// byte-identical.
///
/// Mirrors upstream exactly:
///
/// ```text
/// updating_basis_file = (inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR)
///     || (inplace && (protocol_version >= 29 ? fnamecmp_type == FNAMECMP_FNAME
///                                            : make_backups <= 0));
/// ```
///
/// A missing basis-type byte defaults to `FNAMECMP_FNAME` (`rsync.c:326`). Note
/// that at protocol >= 29 `--inplace --backup` still qualifies: the generator
/// makes a side backup copy but keeps `fnamecmp_type == FNAMECMP_FNAME` and
/// rewrites the destination in place (`generator.c:1862,1898`); the
/// `make_backups <= 0` clause only applies to the legacy protocol < 29 path.
///
/// upstream: sender.c:337 `updating_basis_file`.
pub(crate) fn updating_basis_file(
    inplace: bool,
    inplace_partial: bool,
    make_backups: bool,
    proto: protocol::ProtocolVersion,
    fnamecmp_type: Option<protocol::FnameCmpType>,
) -> bool {
    let is_fname = matches!(fnamecmp_type, None | Some(protocol::FnameCmpType::Fname));
    let is_partial_dir = matches!(fnamecmp_type, Some(protocol::FnameCmpType::PartialDir));
    (inplace_partial && is_partial_dir)
        || (inplace
            && if proto.as_u8() >= 29 {
                is_fname
            } else {
                !make_backups
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
#[allow(clippy::too_many_arguments)]
pub(super) fn stream_whole_file_transfer<R: Read, W: Write>(
    writer: &mut W,
    mut source: R,
    file_size: u64,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_seed: i32,
    protocol: protocol::ProtocolVersion,
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

    // upstream: checksum.c:558 sum_init() - the legacy MD4 whole-file sum
    // (CSUM_MD4_OLD, proto 27-29) prepends the 4-byte LE seed before file
    // data; MD5 and the modern algorithms do not. Mirror the receiver's
    // ChecksumVerifier::new so both sides agree at protocol < 30.
    let mut verifier =
        ChecksumVerifier::for_algorithm_seeded(checksum_algorithm, checksum_seed, protocol);

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

/// Streams the appended tail of a file to the wire in append mode.
///
/// In append mode the receiver already holds the first `flength` bytes, so the
/// sender transmits only `[flength, file_size)` as literal tokens - never any
/// block-match tokens (the sum_head carried no block checksums). The whole-file
/// checksum folds in the existing prefix only when `append_verify` is set
/// (upstream `append_mode == 2`); plain append trusts the prefix and sums just
/// the new tail so both sides agree.
///
/// # Upstream Reference
///
/// - `match.c:371-390 match_sums()` - prefix `sum_update` gated on `append_mode == 2`,
///   `s->count = 0`, `last_match = s->flength`.
/// - `sender.c:89-95 receive_sums()` - append mode derives `flength` and reads no blocks.
#[allow(clippy::too_many_arguments)]
pub(super) fn stream_append_transfer<R: Read, W: Write>(
    writer: &mut W,
    mut source: R,
    file_size: u64,
    flength: u64,
    append_verify: bool,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_seed: i32,
    protocol: protocol::ProtocolVersion,
    encoder: Option<&mut CompressedTokenEncoder>,
    buf: &mut Vec<u8>,
) -> io::Result<StreamResult> {
    let mut verifier =
        ChecksumVerifier::for_algorithm_seeded(checksum_algorithm, checksum_seed, protocol);

    const MAX_READ_SIZE: usize = 256 * 1024;

    // upstream: match.c:372-390 - consume the existing prefix. append_mode == 2
    // folds it into the whole-file checksum (verify); append_mode == 1 skips the
    // sum (trust). Either way the prefix bytes are never sent as tokens.
    let mut prefix_remaining = flength.min(file_size);
    if prefix_remaining > 0 {
        buf.resize((prefix_remaining as usize).clamp(1, MAX_READ_SIZE), 0);
        while prefix_remaining > 0 {
            let to_read = buf.len().min(prefix_remaining as usize);
            source.read_exact(&mut buf[..to_read])?;
            if append_verify {
                verifier.update(&buf[..to_read]);
            }
            prefix_remaining -= to_read as u64;
        }
    }

    // Stream [flength, file_size) as literal tokens, folding into the checksum.
    let mut remaining = file_size.saturating_sub(flength);
    let read_size = (remaining as usize).clamp(1, MAX_READ_SIZE);

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
        buf.resize(4 + read_size, 0);
        while remaining > 0 {
            let to_read = (buf.len() - 4).min(remaining as usize);
            source.read_exact(&mut buf[4..4 + to_read])?;
            verifier.update(&buf[4..4 + to_read]);
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
#[allow(clippy::too_many_arguments)]
pub(super) fn write_delta_with_inline_checksum<W: Write>(
    writer: &mut W,
    ops: &[DeltaOp],
    encoder: Option<&mut CompressedTokenEncoder>,
    is_zlib: bool,
    source_path: &Path,
    use_noatime: bool,
    checksum_algorithm: ChecksumAlgorithm,
    checksum_seed: i32,
    protocol: protocol::ProtocolVersion,
) -> io::Result<InlineChecksumResult> {
    // upstream: checksum.c:558 sum_init() - prepend the 4-byte LE seed for the
    // legacy MD4 whole-file sum (CSUM_MD4_OLD, proto 27-29); MD5 and modern
    // algorithms are unseeded. Keeps the sender symmetric with the receiver's
    // ChecksumVerifier so protocol < 30 peers accept the reconstructed file.
    let mut verifier =
        ChecksumVerifier::for_algorithm_seeded(checksum_algorithm, checksum_seed, protocol);

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

    // WHY: a daemon module's `dont compress = *` must store the whole zlib
    // stream (level 0), because upstream token.c:206-211 sets
    // per_file_default_level = skip_compression_level for a `*` match list and
    // token.c:378 feeds it into deflateInit2(). Compressing per block instead
    // would diverge from upstream's observable "no compression savings" output.
    #[test]
    fn whole_stream_store_forces_level_none_for_zlib() {
        assert_eq!(
            whole_stream_compression_level(
                true,
                CompressionAlgorithm::Zlib,
                CompressionLevel::Best
            ),
            CompressionLevel::None,
        );
        assert_eq!(
            whole_stream_compression_level(
                true,
                CompressionAlgorithm::ZlibX,
                CompressionLevel::Best,
            ),
            CompressionLevel::None,
        );
    }

    // WHY: upstream feeds per_file_default_level only into deflateInit2() (zlib).
    // zstd/lz4 use do_compression_level unconditionally (token.c:748), so the
    // match-all case must NOT lower their level.
    #[test]
    fn whole_stream_store_leaves_non_zlib_codecs_unchanged() {
        for codec in [CompressionAlgorithm::Zstd, CompressionAlgorithm::LZ4] {
            assert_eq!(
                whole_stream_compression_level(true, codec, CompressionLevel::Best),
                CompressionLevel::Best,
            );
        }
    }

    // WHY: without the match-all trigger (normal skip list or none) the
    // configured level must pass through untouched so ordinary transfers stay
    // wire-identical to upstream.
    #[test]
    fn without_match_all_configured_level_passes_through() {
        assert_eq!(
            whole_stream_compression_level(
                false,
                CompressionAlgorithm::Zlib,
                CompressionLevel::Best,
            ),
            CompressionLevel::Best,
        );
    }

    // WHY: token framing is a session-level concern, not a per-file one. Once a
    // codec is negotiated (`-z`), EVERY file is framed with that codec on the
    // wire - upstream token.c:1065 send_token() dispatches purely on the global
    // `do_compression`, and token.c:225 set_compression()'s per-file suffix
    // lookup is compiled out under `#if 0`. A `--skip-compress` suffix must NOT
    // switch a file to plain 4-byte-LE tokens: the receiver builds one
    // session-level token reader from the negotiated codec and always expects
    // deflated framing, so plain tokens desync the stream. This pins that a
    // present encoder emits deflated framing while an absent encoder (no `-z`)
    // emits plain tokens, for byte-identical input.
    #[test]
    fn present_encoder_emits_deflated_framing_not_plain_tokens() {
        let data: Vec<u8> = (0..64u8).collect();
        // A single literal chunk in plain framing is a 4-byte little-endian
        // length prefix followed by the verbatim data (upstream
        // simple_send_token / match.c send_token()).
        let plain_prefix = (data.len() as i32).to_le_bytes();

        // No codec negotiated: the correct non-`-z` path emits plain tokens.
        let mut plain = Vec::new();
        let mut buf = Vec::new();
        let plain_res = stream_whole_file_transfer(
            &mut plain,
            &data[..],
            data.len() as u64,
            ChecksumAlgorithm::MD5,
            0,
            protocol::ProtocolVersion::NEWEST,
            None,
            &mut buf,
            None,
        )
        .expect("plain stream");
        assert!(
            plain.starts_with(&plain_prefix) && plain[4..4 + data.len()] == data[..],
            "no-codec path must emit plain 4-byte-LE tokens carrying verbatim data"
        );

        // Codec negotiated (as under `-z`, regardless of any skip-compress
        // suffix): the wire must be deflated framing, never plain tokens with
        // the literal on the wire verbatim.
        let mut enc =
            create_token_encoder(CompressionAlgorithm::Zlib, CompressionLevel::Best, None)
                .expect("zlib encoder creation should succeed")
                .expect("zlib produces an encoder");
        let mut deflated = Vec::new();
        let mut buf2 = Vec::new();
        let deflated_res = stream_whole_file_transfer(
            &mut deflated,
            &data[..],
            data.len() as u64,
            ChecksumAlgorithm::MD5,
            0,
            protocol::ProtocolVersion::NEWEST,
            Some(&mut enc),
            &mut buf2,
            None,
        )
        .expect("deflated stream");

        assert_ne!(
            deflated, plain,
            "codec framing must differ from plain token framing"
        );
        let emits_plain_verbatim = deflated.len() >= 4 + data.len()
            && deflated.starts_with(&plain_prefix)
            && deflated[4..4 + data.len()] == data[..];
        assert!(
            !emits_plain_verbatim,
            "codec path must not fall back to plain tokens (the skip-compress desync bug)"
        );
        // Framing changed; the reconstructed data (and its whole-file checksum)
        // did not.
        assert_eq!(
            &plain_res.checksum_buf[..plain_res.checksum_len],
            &deflated_res.checksum_buf[..deflated_res.checksum_len],
            "whole-file checksum is codec-independent"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn create_token_encoder_zstd_no_workers() {
        let encoder =
            create_token_encoder(CompressionAlgorithm::Zstd, CompressionLevel::Default, None)
                .expect("zstd encoder creation should succeed");
        assert!(encoder.is_some(), "zstd should produce an encoder");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn create_token_encoder_zstd_with_workers() {
        let workers = std::num::NonZeroU8::new(1);
        let encoder = create_token_encoder(
            CompressionAlgorithm::Zstd,
            CompressionLevel::Default,
            workers,
        )
        .expect("zstd encoder with workers=1 should succeed");
        assert!(encoder.is_some(), "zstd should produce an encoder");
    }

    #[test]
    fn create_token_encoder_zlib_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
            workers,
        )
        .expect("zlib encoder should succeed even with workers");
        assert!(encoder.is_some(), "zlib should produce an encoder");
    }

    #[test]
    fn create_token_encoder_zlibx_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(
            CompressionAlgorithm::ZlibX,
            CompressionLevel::Default,
            workers,
        )
        .expect("zlibx encoder should succeed even with workers");
        assert!(encoder.is_some(), "zlibx should produce an encoder");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn create_token_encoder_lz4_ignores_workers() {
        let workers = std::num::NonZeroU8::new(4);
        let encoder = create_token_encoder(
            CompressionAlgorithm::LZ4,
            CompressionLevel::Default,
            workers,
        )
        .expect("lz4 encoder should succeed even with workers");
        assert!(encoder.is_some(), "lz4 should produce an encoder");
    }

    /// The negotiated `--compress-level` must reach the wire token encoder:
    /// upstream `token.c` inits both the zlib (`deflateInit2`) and zstd
    /// (`ZSTD_c_compressionLevel`) contexts with `do_compression_level`, so a
    /// higher level must produce a materially smaller compressed token stream
    /// for the same compressible literal. A regression that hardcodes the
    /// default level would make these two streams identical.
    #[cfg(feature = "zstd")]
    #[test]
    fn create_token_encoder_zstd_honors_negotiated_level() {
        use std::num::NonZeroU8;

        fn emit(level: CompressionLevel) -> Vec<u8> {
            let mut enc = create_token_encoder(CompressionAlgorithm::Zstd, level, None)
                .expect("zstd encoder")
                .expect("zstd produces an encoder");
            // A repetitive-but-structured payload so a higher zstd level wins.
            let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
            let mut out = Vec::new();
            enc.send_literal(&mut out, &payload).expect("send literal");
            enc.finish(&mut out).expect("finish");
            out
        }

        let fast = emit(CompressionLevel::Precise(NonZeroU8::new(1).unwrap()));
        let best = emit(CompressionLevel::Precise(NonZeroU8::new(19).unwrap()));
        assert_ne!(
            fast, best,
            "distinct zstd levels must yield distinct compressed token streams"
        );
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
            0,
            proto(31),
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

    /// The append streamer must transmit only the tail past `flength` and fold
    /// the existing prefix into the whole-file checksum only under append-verify
    /// (append_mode == 2). Mirrors upstream match.c:371-390.
    #[test]
    fn stream_append_transfer_tail_and_prefix_checksum() {
        let file: Vec<u8> = (0..100u32).map(|i| (i % 256) as u8).collect();
        let flength: u64 = 40;
        let tail_len = file.len() as u64 - flength;

        // Trust mode (append_mode == 1): checksum covers only [flength, len).
        let mut wire_trust = Vec::new();
        let mut buf = Vec::new();
        let res_trust = stream_append_transfer(
            &mut wire_trust,
            std::io::Cursor::new(file.clone()),
            file.len() as u64,
            flength,
            false,
            ChecksumAlgorithm::MD5,
            0,
            proto(31),
            None,
            &mut buf,
        )
        .expect("append stream (trust)");

        let mut exp_tail = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        exp_tail.update(&file[flength as usize..]);
        let mut exp_tail_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let exp_tail_len = exp_tail.finalize_into(&mut exp_tail_buf);
        assert_eq!(
            &res_trust.checksum_buf[..res_trust.checksum_len],
            &exp_tail_buf[..exp_tail_len],
            "append (trust) checksum must cover only the appended tail",
        );

        // Verify mode (append_mode == 2): checksum covers the whole file.
        let mut wire_verify = Vec::new();
        buf.clear();
        let res_verify = stream_append_transfer(
            &mut wire_verify,
            std::io::Cursor::new(file.clone()),
            file.len() as u64,
            flength,
            true,
            ChecksumAlgorithm::MD5,
            0,
            proto(31),
            None,
            &mut buf,
        )
        .expect("append stream (verify)");

        let mut exp_whole = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        exp_whole.update(&file);
        let mut exp_whole_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let exp_whole_len = exp_whole.finalize_into(&mut exp_whole_buf);
        assert_eq!(
            &res_verify.checksum_buf[..res_verify.checksum_len],
            &exp_whole_buf[..exp_whole_len],
            "append-verify checksum must cover the existing prefix plus the tail",
        );

        // The prefix is never sent as tokens, so both modes emit the same wire:
        // one literal chunk carrying exactly the tail, then the end token.
        assert_eq!(wire_trust, wire_verify, "verify must not change the wire");
        assert_eq!(
            &wire_trust[0..4],
            &(tail_len as i32).to_le_bytes(),
            "first wire chunk length must equal the appended tail length",
        );
        assert_eq!(
            wire_trust.len() as u64,
            4 + tail_len + 4,
            "wire is [len][tail][end], no prefix bytes",
        );
    }

    use protocol::{FnameCmpType, ProtocolVersion};

    fn proto(v: u8) -> ProtocolVersion {
        ProtocolVersion::try_from(v).expect("protocol version")
    }

    // WHY: the sender must activate the in-place guard exactly when the receiver
    // rewrites the basis (the destination) in place, i.e. it is matching against
    // FNAMECMP_FNAME under --inplace. This is the core of upstream sender.c:337
    // at protocol >= 29 (proto 32 here): a missing basis-type byte and an
    // explicit FNAMECMP_FNAME both mean "the destination itself", so the guard
    // must engage. Without it a backward Copy would tell the receiver to read a
    // region it has already overwritten.
    #[test]
    fn updating_basis_file_active_for_inplace_against_destination() {
        for fnamecmp in [None, Some(FnameCmpType::Fname)] {
            assert!(
                updating_basis_file(true, false, false, proto(32), fnamecmp),
                "inplace against FNAMECMP_FNAME must activate the guard",
            );
        }
    }

    // WHY: without --inplace the receiver writes to a temp file and renames, so
    // the basis is never overwritten during the transfer. A backward Copy is
    // safe; the guard must stay off so ordinary transfers remain byte-for-byte
    // identical to upstream (updating_basis_file = 0 in that case).
    #[test]
    fn updating_basis_file_inactive_without_inplace() {
        for fnamecmp in [None, Some(FnameCmpType::Fname)] {
            assert!(
                !updating_basis_file(false, false, false, proto(32), fnamecmp),
                "no --inplace means no in-place guard",
            );
        }
    }

    // WHY: upstream keeps fnamecmp_type == FNAMECMP_FNAME under --inplace
    // --backup at protocol >= 29 - the generator copies the destination aside as
    // a backup but still reads the basis from, and rewrites, the destination in
    // place (generator.c:1862,1898). So the guard MUST stay active with
    // --backup. The legacy `make_backups <= 0` clause only governs protocol < 29,
    // where a backup instead turns the guard off. Both directions are pinned so a
    // future refactor cannot collapse the protocol split.
    #[test]
    fn updating_basis_file_backup_split_on_protocol_version() {
        // protocol >= 29: --inplace --backup still guards (fnamecmp is FNAME).
        assert!(
            updating_basis_file(true, false, true, proto(32), Some(FnameCmpType::Fname)),
            "proto >= 29 --inplace --backup keeps FNAMECMP_FNAME and must guard",
        );
        // protocol < 29: a backup turns the guard off (make_backups <= 0 is false).
        assert!(
            !updating_basis_file(true, false, true, proto(28), Some(FnameCmpType::Fname)),
            "proto < 29 --inplace --backup must not guard (make_backups > 0)",
        );
        // protocol < 29 without a backup keeps the guard on (make_backups <= 0).
        assert!(
            updating_basis_file(true, false, false, proto(28), Some(FnameCmpType::Fname)),
            "proto < 29 --inplace without --backup must guard",
        );
    }

    // WHY: when the receiver reads from an alternate basis (a --compare/copy/
    // link-dest directory, tagged FNAMECMP_BASIS_DIR) the in-place write to the
    // destination cannot clobber that separate basis, so upstream does not set
    // updating_basis_file. The guard must stay off to avoid needlessly demoting
    // safe backward Copies to literals.
    #[test]
    fn updating_basis_file_inactive_for_alternate_basis() {
        assert!(
            !updating_basis_file(
                true,
                false,
                false,
                proto(32),
                Some(FnameCmpType::BasisDir(0))
            ),
            "an alternate basis dir is not overwritten in place; no guard",
        );
    }

    // WHY: the partial-dir branch of sender.c:337 is independent of --inplace: it
    // fires only when the CF_INPLACE_PARTIAL_DIR capability was negotiated
    // (inplace_partial) AND the basis is the partial file (FNAMECMP_PARTIAL_DIR),
    // which the receiver resumes into in place. Without the negotiated capability
    // the partial file is a plain basis and the guard stays off.
    #[test]
    fn updating_basis_file_partial_dir_requires_capability() {
        assert!(
            updating_basis_file(
                false,
                true,
                false,
                proto(32),
                Some(FnameCmpType::PartialDir)
            ),
            "inplace_partial + FNAMECMP_PARTIAL_DIR must guard",
        );
        assert!(
            !updating_basis_file(
                false,
                false,
                false,
                proto(32),
                Some(FnameCmpType::PartialDir)
            ),
            "FNAMECMP_PARTIAL_DIR without the capability must not guard",
        );
    }

    /// True when every `Copy` references a basis offset at or ahead of the
    /// running source cursor - the invariant an in-place receiver depends on, as
    /// a backward `Copy` would read a destination region already overwritten.
    fn copies_are_monotonic(script: &DeltaScript, block_len: usize) -> bool {
        let mut cursor = 0u64;
        for token in script.tokens() {
            match token {
                DeltaToken::Copy { index, len } => {
                    if *index * (block_len as u64) < cursor {
                        return false;
                    }
                    cursor += *len as u64;
                }
                DeltaToken::Literal(bytes) => cursor += bytes.len() as u64,
            }
        }
        true
    }

    /// Builds the wire signature blocks for `basis` using the exact algorithm
    /// [`generate_delta_from_signature`] reconstructs internally, so the sender's
    /// source blocks match them byte-for-byte.
    fn wire_signature(
        basis: &[u8],
        block_len: u32,
        strong_len: u8,
    ) -> Vec<protocol::wire::signature::SignatureBlock> {
        use signature::{
            SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
        };
        use std::num::{NonZeroU8, NonZeroU32};

        let algorithm = ChecksumFactory::from_negotiation(None, ProtocolVersion::NEWEST, 0, None)
            .signature_algorithm();
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            basis.len() as u64,
            NonZeroU32::new(block_len),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(strong_len).expect("strong length"),
        ))
        .expect("layout");
        let sig = generate_file_signature(io::Cursor::new(basis.to_vec()), layout, algorithm)
            .expect("signature");
        sig.blocks()
            .iter()
            .map(|b| protocol::wire::signature::SignatureBlock {
                index: b.index() as u32,
                rolling_sum: b.rolling().value(),
                strong_sum: b.strong().to_vec(),
            })
            .collect()
    }

    fn delta_config(
        sig_blocks: Vec<protocol::wire::signature::SignatureBlock>,
        block_len: u32,
        strong_len: u8,
        guard: bool,
    ) -> DeltaGeneratorConfig<'static> {
        DeltaGeneratorConfig {
            block_length: block_len,
            sig_blocks,
            strong_sum_length: strong_len,
            protocol: ProtocolVersion::NEWEST,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
            updating_basis_file: guard,
        }
    }

    // WHY: this pins the activation end-to-end - the config field must reach the
    // DeltaGenerator and change its output. The source swaps two basis blocks so
    // the second half references basis offset 0, behind the write cursor. With
    // the guard active (as --inplace against the destination sets it) that
    // backward Copy must be demoted to a literal so an in-place receiver never
    // reads clobbered data; with the guard inactive the backward Copy survives.
    // If the wiring dropped the flag, both runs would be identical and this fails.
    #[test]
    fn generate_delta_honors_updating_basis_file_flag() {
        let block_len = 64u32;
        let bl = block_len as usize;
        let strong_len = 16u8;

        let block0: Vec<u8> = (0..bl).map(|i| (i % 256) as u8).collect();
        let block1: Vec<u8> = (0..bl).map(|i| ((i + 100) % 256) as u8).collect();
        let mut basis = block0.clone();
        basis.extend_from_slice(&block1);
        // Swap the halves: the trailing source block matches basis block 0, whose
        // offset (0) precedes the write cursor once block 1 has been written.
        let mut source = block1.clone();
        source.extend_from_slice(&block0);

        // Guard inactive: the backward match survives as a Copy (non-monotonic).
        let off = generate_delta_from_signature(
            io::Cursor::new(source.clone()),
            delta_config(
                wire_signature(&basis, block_len, strong_len),
                block_len,
                strong_len,
                false,
            ),
        )
        .expect("delta (guard off)");
        assert!(
            !copies_are_monotonic(&off, bl),
            "without the guard the backward Copy must survive",
        );

        // Guard active: the backward Copy is demoted to a literal (monotonic).
        let on = generate_delta_from_signature(
            io::Cursor::new(source.clone()),
            delta_config(
                wire_signature(&basis, block_len, strong_len),
                block_len,
                strong_len,
                true,
            ),
        )
        .expect("delta (guard on)");
        assert!(
            copies_are_monotonic(&on, bl),
            "with the guard active no Copy may point behind the write cursor",
        );
        assert!(
            on.tokens()
                .iter()
                .any(|t| matches!(t, DeltaToken::Literal(_))),
            "the suppressed backward block must reappear as a literal",
        );
    }
}
