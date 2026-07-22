//! Phase-2 driver: walks the batch NDX stream and applies per-file deltas.
//!
//! This module owns the main replay loop that consumes the protocol byte
//! stream after the file list, dispatching by NDX value to the appropriate
//! handler (delete stats, incremental flist segments, per-file delta data,
//! phase transitions). It coordinates compression codec detection,
//! sum-head decoding, and the per-file commit through the helpers in
//! [`super::dispatch`] and [`super::codec`].

use std::fs;
use std::path::Path;

use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum,
};
use protocol::flist::sort_file_list;

use crate::error::{BatchError, BatchResult};
use crate::format::BatchFlags;
use crate::reader::BatchReader;

use super::ReplayResult;
#[cfg(feature = "zstd")]
use super::codec::detect_compression_codec;
use super::codec::{CompressionCodec, create_compressed_decoder};
use super::delta::{choose_block_length, default_xfer_sum_len};
use super::dispatch::{
    ITEM_TRANSFER, apply_file_delta, read_and_discard_file_checksum,
    read_compressed_deltas_streaming, read_iflags_and_skip_meta, read_sum_head,
};

/// Phase 2: drive the NDX loop and apply per-file deltas.
///
/// upstream: receiver.c:recv_files() reads NDX + iflags + sum_head per file,
/// then delta tokens, then file checksum. NDX_DONE signals phase transitions.
pub(super) fn apply_delta_phase(
    reader: &mut BatchReader,
    entries: &mut Vec<protocol::flist::FileEntry>,
    dest_root: &Path,
    flags: &BatchFlags,
    result: &mut ReplayResult,
    verbosity: i32,
) -> BatchResult<()> {
    let proto = reader.config().protocol_version;
    let mut codec_state = CodecState::new(flags)?;
    let mut flist_segments = init_flist_segments(reader, entries.len())?;
    let mut ndx_codec = reader
        .take_ndx_codec()
        .unwrap_or_else(|| NdxCodecEnum::new(proto as u8));
    let max_phase = if proto >= 29 { 2 } else { 1 };
    let mut phase = 1;

    loop {
        let stream = reader
            .inner_reader()
            .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;

        let ndx = ndx_codec.read_ndx(stream).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read NDX from batch stream: {e}"),
            ))
        })?;

        if ndx == NDX_DONE {
            phase += 1;
            if phase > max_phase {
                break;
            }
            continue;
        }

        // upstream: receiver.c - NDX_FLIST_EOF signals end of incremental file
        // lists (INC_RECURSE). Skip it during replay since all entries are
        // already decoded from the batch file list.
        if ndx == NDX_FLIST_EOF {
            continue;
        }

        if ndx == NDX_DEL_STATS {
            consume_del_stats(reader)?;
            continue;
        }

        if ndx <= NDX_FLIST_OFFSET {
            handle_inc_recurse_segment(reader, entries, dest_root, &mut flist_segments)?;
            result.file_count = entries.len() as u64;
            continue;
        }

        process_file_ndx(
            reader,
            entries,
            dest_root,
            &flist_segments,
            &mut codec_state,
            ndx,
            proto,
            verbosity,
        )?;
    }

    Ok(())
}

/// Tracks compression-codec state across the NDX loop iterations.
struct CodecState {
    decoder: Option<protocol::wire::CompressedTokenDecoder>,
    /// Currently detected codec. `None` means the batch is uncompressed.
    /// Only read when the `zstd` feature is enabled; without `zstd` the
    /// codec is always [`CompressionCodec::Zlib`] and detection is a no-op.
    #[cfg(feature = "zstd")]
    detected: Option<CompressionCodec>,
    /// True when the active codec is CPRES_ZLIB, requiring `see_token()`
    /// dictionary sync between block-match tokens.
    cpres_zlib: bool,
}

impl CodecState {
    fn new(flags: &BatchFlags) -> BatchResult<Self> {
        // upstream: batch.c:check_batch_flags() - when the batch stream flags
        // include do_compression (bit 8), the token data in the batch file
        // uses compressed format (DEFLATED_DATA headers).
        //
        // Upstream always forces CPRES_ZLIB for batch reads (compat.c:194-195),
        // so we start with a zlib decoder. However, if zlib decompression
        // fails on the first token, we fall back to zstd. This auto-detection
        // handles both standard upstream batch files (always zlib) and
        // hypothetical zstd-compressed batch files from patched or future
        // upstream versions.
        let decoder = if flags.do_compression {
            Some(create_compressed_decoder(CompressionCodec::Zlib)?)
        } else {
            None
        };
        let cpres_zlib = flags.do_compression;
        Ok(Self {
            decoder,
            #[cfg(feature = "zstd")]
            detected: if flags.do_compression {
                Some(CompressionCodec::Zlib)
            } else {
                None
            },
            cpres_zlib,
        })
    }

    /// Run codec auto-detection at most once. If detection finds zstd, build
    /// a fresh zstd decoder so the next read uses the right inflate context.
    #[cfg(feature = "zstd")]
    fn detect_once(&mut self, reader: &mut BatchReader) -> BatchResult<()> {
        let was_zlib = self.detected == Some(CompressionCodec::Zlib);
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(());
        };
        if !was_zlib || decoder.initialized() {
            return Ok(());
        }
        let stream = reader
            .inner_reader()
            .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
        let actual_codec = detect_compression_codec(stream);
        if actual_codec != CompressionCodec::Zlib {
            self.detected = Some(actual_codec);
            self.cpres_zlib = false;
            // Replace the zlib decoder with a fresh zstd one. Subsequent
            // files reuse this decoder across iterations.
            self.decoder = Some(create_compressed_decoder(CompressionCodec::Zstd)?);
        }
        Ok(())
    }

    /// Without `zstd`, codec detection is a no-op: the active codec is always
    /// CPRES_ZLIB and there is nothing to swap.
    #[cfg(not(feature = "zstd"))]
    fn detect_once(&mut self, _reader: &mut BatchReader) -> BatchResult<()> {
        Ok(())
    }
}

/// Build the initial flist-segments table from the batch header.
///
/// upstream: flist.c:2958 - with INC_RECURSE, the first flist has
/// `ndx_start=1`. Subsequent sub-lists have `ndx_start = prev->ndx_start +
/// prev->used + 1` (flist.c:2966), creating a +1 gap between segments in NDX
/// space. We track per-segment (ndx_start, entries_offset, count) to map
/// global NDX values to flat Vec indices, mirroring upstream's
/// `flist_for_ndx()`.
fn init_flist_segments(
    reader: &BatchReader,
    initial_count: usize,
) -> BatchResult<Vec<(i32, usize, usize)>> {
    let header = reader
        .header()
        .ok_or_else(|| BatchError::Io(std::io::Error::other("batch header not read")))?;
    let inc_recurse = header
        .compat_flags
        .map(|cf| {
            protocol::CompatibilityFlags::from_bits(cf as u32)
                .contains(protocol::CompatibilityFlags::INC_RECURSE)
        })
        .unwrap_or(false);
    let initial_ndx_start: i32 = if inc_recurse { 1 } else { 0 };
    Ok(vec![(initial_ndx_start, 0, initial_count)])
}

/// Consume the `NDX_DEL_STATS` payload and discard.
///
/// upstream: main.c:read_final_goodbye() - NDX_DEL_STATS carries 5 varints
/// of deletion statistics. Consume and discard during replay.
fn consume_del_stats(reader: &mut BatchReader) -> BatchResult<()> {
    let stream = reader
        .inner_reader()
        .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
    let _del_stats = protocol::stats::DeleteStats::read_from(stream).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read delete stats from batch stream: {e}"),
        ))
    })?;
    Ok(())
}

/// Process a single per-file NDX entry: read iflags, sum_head, delta tokens,
/// transfer checksum, and commit the file via [`apply_file_delta`].
#[allow(clippy::too_many_arguments)]
fn process_file_ndx(
    reader: &mut BatchReader,
    entries: &[protocol::flist::FileEntry],
    dest_root: &Path,
    flist_segments: &[(i32, usize, usize)],
    codec_state: &mut CodecState,
    ndx: i32,
    proto: i32,
    verbosity: i32,
) -> BatchResult<()> {
    let stream = reader
        .inner_reader()
        .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
    let iflags = read_iflags_and_skip_meta(stream, proto)?;

    if iflags & ITEM_TRANSFER == 0 {
        // Metadata-only change, no delta data follows.
        return Ok(());
    }

    // upstream: rsync.c:flist_for_ndx() + receiver.c:700 - map global NDX
    // to the flat entries Vec index by finding the segment it belongs to.
    let flat_index = match lookup_flat_index(ndx, flist_segments, entries.len())? {
        Some(idx) => idx,
        None => return Ok(()), // INC_RECURSE parent-dir metadata update; skip.
    };

    let entry_name = entries[flat_index].name().to_owned();
    let entry_size = entries[flat_index].size();
    // Only regular files are valid delta targets. A directory or symlink must
    // never be opened as a basis file or overwritten with literal data - on
    // Unix `File::open` on a directory succeeds (so a stray transfer record is
    // harmless), but on Windows it returns ERROR_ACCESS_DENIED. We still drain
    // the per-file sum_head + token + checksum stream below to stay in sync
    // before skipping the materialisation step.
    // upstream: rsync only sends ITEM_TRANSFER for regular files.
    let is_regular = entries[flat_index].file_type() == protocol::flist::FileType::Regular;
    let dest_path = dest_root.join(&entry_name);

    let stream = reader
        .inner_reader()
        .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
    let (block_count, block_length_wire, remainder_wire) = read_sum_head(stream)?;

    // Compute block geometry before token reading - needed for CPRES_ZLIB
    // see_token() calls which reference basis blocks by index. A non-regular
    // dest never has a usable basis (and must not be opened as one).
    let basis_exists = is_regular && dest_path.exists();
    let block_length = if block_length_wire > 0 {
        block_length_wire as usize
    } else {
        choose_block_length(entry_size)
    };
    let remainder = if remainder_wire > 0 {
        remainder_wire as usize
    } else {
        block_length
    };

    // Auto-detect compression codec on the first compressed file before
    // building delta operations. Detection runs once per batch.
    codec_state.detect_once(reader)?;

    let delta_ops = read_delta_tokens(
        reader,
        codec_state,
        &dest_path,
        basis_exists,
        &entry_name,
        block_length,
        block_count,
        remainder,
    )?;

    {
        let xfer_sum_len = default_xfer_sum_len(proto);
        let stream = reader
            .inner_reader()
            .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
        read_and_discard_file_checksum(stream, xfer_sum_len)?;
    }

    if verbosity > 0 {
        println!("  {} delta operations", delta_ops.len());
    }

    // The per-file stream is now fully drained. Only materialise regular
    // files; directories and symlinks were created in the flist phase and must
    // not be overwritten with a delta-reconstructed file.
    if is_regular {
        apply_file_delta(
            &dest_path,
            basis_exists,
            delta_ops,
            block_length,
            block_count as u32,
            remainder,
        )
    } else {
        Ok(())
    }
}

/// Reads delta tokens for one file, dispatching by compression codec.
///
/// When compression was active during batch creation, tokens use the
/// compressed wire format with DEFLATED_DATA headers. Otherwise, tokens use
/// the simple 4-byte LE i32 format.
///
/// upstream: token.c:recv_token() dispatches to recv_deflated_token() or
/// simple_recv_token() based on do_compression.
#[allow(clippy::too_many_arguments)]
fn read_delta_tokens(
    reader: &mut BatchReader,
    codec_state: &mut CodecState,
    dest_path: &Path,
    basis_exists: bool,
    entry_name: &str,
    block_length: usize,
    block_count: i32,
    remainder: usize,
) -> BatchResult<Vec<protocol::wire::DeltaOp>> {
    let Some(decoder) = codec_state.decoder.as_mut() else {
        return reader.read_file_delta_tokens().map_err(|e| {
            BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read delta tokens for '{entry_name}': {e}"),
            ))
        });
    };

    // upstream: token.c:recv_deflated_token() r_init resets inflate
    // context per file. The decoder.reset() mirrors this behavior.
    decoder.reset();
    if codec_state.cpres_zlib && basis_exists {
        let basis_data = fs::read(dest_path).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read basis file '{}': {e}", dest_path.display()),
            ))
        })?;
        let stream = reader
            .inner_reader()
            .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
        read_compressed_deltas_streaming(
            decoder,
            stream,
            &basis_data,
            entry_name,
            block_length,
            block_count,
            remainder,
        )
    } else {
        // CPRES_ZLIBX or no basis: eager read - see_token() is a no-op.
        reader.read_compressed_delta_tokens(decoder).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read compressed delta tokens for '{entry_name}': {e}"),
            ))
        })
    }
}

/// Handle an incremental flist sub-list segment (INC_RECURSE).
///
/// upstream: flist.c:recv_additional_file_list() - reads the next segment of
/// entries on-the-fly, sorts them in place, records the NDX range for
/// global-to-flat index mapping, and creates any newly discovered
/// directories so files in the sub-list have parent paths.
fn handle_inc_recurse_segment(
    reader: &mut BatchReader,
    entries: &mut Vec<protocol::flist::FileEntry>,
    dest_root: &Path,
    flist_segments: &mut Vec<(i32, usize, usize)>,
) -> BatchResult<()> {
    let prev_len = entries.len();

    // upstream: flist.c:2966 - ndx_start = prev->ndx_start + prev->used + 1
    let prev_seg = flist_segments.last().expect("at least initial segment");
    let seg_ndx_start = prev_seg.0 + prev_seg.2 as i32 + 1;

    reader.read_incremental_flist_segment(entries)?;

    // upstream: flist.c:2771 - sort each sub-list segment after receiving.
    // INC_RECURSE batches require protocol >= 30, so pre29 is always false.
    sort_file_list(&mut entries[prev_len..], false, false);

    let seg_count = entries.len() - prev_len;
    flist_segments.push((seg_ndx_start, prev_len, seg_count));

    // Create directories and symlinks for newly discovered entries.
    for entry in &entries[prev_len..] {
        let dest_path = dest_root.join(entry.name());
        if entry.is_dir() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::create_dir_all(&dest_path).ok();
        } else if entry.is_symlink() {
            if let Some(_target) = entry.link_target() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).ok();
                }
                #[cfg(unix)]
                {
                    let _ = std::os::unix::fs::symlink(_target, &dest_path);
                }
            }
        } else if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).ok();
        }
    }
    Ok(())
}

/// Map a global NDX value to a flat entries-Vec index, scanning segments.
///
/// upstream: rsync.c:flist_for_ndx() + receiver.c:700 - each segment has
/// `(ndx_start, entries_offset, count)` with +1 gaps between segments
/// (upstream flist.c:2966).
///
/// Returns `Ok(Some(index))` for a valid match, `Ok(None)` when the NDX
/// refers to an INC_RECURSE parent-directory metadata update (which is
/// skipped during replay), and `Err` when the NDX is invalid.
fn lookup_flat_index(
    ndx: i32,
    flist_segments: &[(i32, usize, usize)],
    total_entries: usize,
) -> BatchResult<Option<usize>> {
    if let Some(idx) = flist_segments
        .iter()
        .find_map(|&(seg_start, offset, count)| {
            if ndx >= seg_start && ndx < seg_start + count as i32 {
                Some(offset + (ndx - seg_start) as usize)
            } else {
                None
            }
        })
    {
        return Ok(Some(idx));
    }
    // upstream: receiver.c:700-705 - NDX < first segment's ndx_start refers
    // to a parent directory entry (INC_RECURSE metadata update). Skip these
    // - directories are already created.
    if ndx < flist_segments.first().map_or(0, |s| s.0) {
        return Ok(None);
    }
    Err(BatchError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "invalid NDX {ndx} (flist has {total_entries} entries across {} segments)",
            flist_segments.len()
        ),
    )))
}
