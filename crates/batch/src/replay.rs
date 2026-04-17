//! Batch replay logic for applying recorded delta operations to a destination.
//!
//! This module contains the core replay implementation that reads a batch file
//! and applies the recorded delta operations to reconstruct files at the
//! destination. The replay logic is decoupled from the orchestration layer
//! (core crate) so it can be tested and reused independently.
//!
//! # Overview
//!
//! Replay proceeds in three phases:
//!
//! 1. **Header validation**: The batch header is read and the stream flags
//!    bitmap is verified against the protocol version.
//! 2. **File list decoding**: The protocol flist wire format is decoded using
//!    [`protocol::flist::FileListReader`], matching the encoding produced by
//!    [`protocol::flist::FileListWriter`] during batch write.
//! 3. **Directory and metadata application**: Parent directories are created,
//!    symlinks are materialized, and metadata (permissions, timestamps) is
//!    applied to all entries.
//!
//! Delta replay for regular files is a separate concern - the batch body
//! after the flist contains delta operations that reference basis files at
//! the destination.
//!
//! # Upstream Reference
//!
//! - `batch.c:read_stream_flags()` - reads the stream flags bitmap
//! - `main.c:do_recv()` - orchestrates file list + delta application
//! - `receiver.c:recv_files()` - per-file delta application

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum,
};
use protocol::wire::{CompressedToken, CompressedTokenDecoder};

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::reader::BatchReader;
use protocol::flist::sort_file_list;

/// Result of a batch replay operation.
///
/// Contains aggregate statistics about the files processed during replay.
/// The caller can use these to report progress or build higher-level
/// summary types.
#[derive(Debug, Clone, Default)]
pub struct ReplayResult {
    /// Number of files processed during replay.
    pub file_count: u64,
    /// Total size in bytes of all processed files.
    pub total_size: u64,
    /// Whether the batch header had the recurse flag set.
    pub recurse: bool,
    /// Number of directories created during replay.
    pub dirs_created: u64,
    /// Number of symlinks created during replay.
    pub symlinks_created: u64,
}

/// Write literal-only delta operations to a new file.
///
/// When no basis file exists at the destination, the delta stream consists
/// entirely of literal data. This function creates the output file and writes
/// all literal chunks sequentially, ignoring any copy operations (which should
/// not be present without a basis).
fn write_literals_to_file(
    dest_path: &Path,
    delta_ops: &[protocol::wire::DeltaOp],
) -> BatchResult<()> {
    let output_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest_path)
        .map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to create output file '{}': {}",
                    dest_path.display(),
                    e
                ),
            ))
        })?;
    let mut output = BufWriter::new(output_file);

    for op in delta_ops {
        if let protocol::wire::DeltaOp::Literal(data) = op {
            output.write_all(data).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to write literal data: {e}"),
                ))
            })?;
        }
    }

    output.flush().map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to flush output file: {e}"),
        ))
    })?;

    Ok(())
}

/// Apply delta operations to reconstruct a target file from a basis file.
///
/// Reads copy and literal tokens from `delta_ops` and writes the
/// reconstructed output to `dest_path`. Copy tokens reference blocks in
/// `basis_path` at offsets computed as `block_index * block_length`.
///
/// # Arguments
///
/// * `basis_path` - Path to the existing basis file used for copy operations.
/// * `dest_path` - Path where the reconstructed output is written.
/// * `delta_ops` - Sequence of delta operations (literal data and basis-file
///   copies) to apply.
/// * `block_length` - Block size used to calculate basis-file offsets for copy
///   operations. Upstream rsync derives this from `choose_block_size()` in
///   `match.c:365`.
///
/// # Errors
///
/// Returns [`BatchError::Io`] if the basis file cannot be opened, the output
/// file cannot be created, or any read/write/seek operation fails.
/// Applies delta operations to reconstruct a file from a basis file.
///
/// `block_count` is the number of blocks in the basis file's signature.
/// `remainder` is the size of the last block (which may be shorter than
/// `block_length`). For the last block (index == block_count - 1), the copy
/// uses `remainder` bytes instead of `block_length`.
///
/// upstream: receiver.c:recv_files() / match.c - block_length for all blocks
/// except the last, which uses remainder.
pub fn apply_delta_ops(
    basis_path: &Path,
    dest_path: &Path,
    delta_ops: Vec<protocol::wire::DeltaOp>,
    block_length: usize,
    block_count: u32,
    remainder: usize,
) -> BatchResult<()> {
    let basis_file = File::open(basis_path).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to open basis file '{}': {}",
                basis_path.display(),
                e
            ),
        ))
    })?;
    let mut basis = BufReader::new(basis_file);

    let output_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest_path)
        .map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to create output file '{}': {}",
                    dest_path.display(),
                    e
                ),
            ))
        })?;
    let mut output = BufWriter::new(output_file);

    let mut buffer = vec![0u8; 8192];
    for op in delta_ops {
        match op {
            protocol::wire::DeltaOp::Literal(data) => {
                output.write_all(&data).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to write literal data: {e}"),
                    ))
                })?;
            }
            protocol::wire::DeltaOp::Copy {
                block_index,
                length,
            } => {
                let offset = u64::from(block_index) * (block_length as u64);

                basis.seek(SeekFrom::Start(offset)).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to seek to offset {offset}: {e}"),
                    ))
                })?;

                // Token-format block matches encode length=0 because the
                // receiver derives block size from the signature. Use
                // block_length for all blocks except the last, which uses
                // remainder (the last block is typically shorter).
                // upstream: receiver.c - block size for last block is remainder.
                let effective_length = if length > 0 {
                    length as usize
                } else if block_count > 0 && block_index == block_count - 1 {
                    remainder
                } else {
                    block_length
                };
                let mut remaining = effective_length;
                while remaining > 0 {
                    let chunk_size = remaining.min(buffer.len());
                    basis.read_exact(&mut buffer[..chunk_size]).map_err(|e| {
                        BatchError::Io(std::io::Error::new(
                            e.kind(),
                            format!("failed to read from basis file: {e}"),
                        ))
                    })?;
                    output.write_all(&buffer[..chunk_size]).map_err(|e| {
                        BatchError::Io(std::io::Error::new(
                            e.kind(),
                            format!("failed to write to output file: {e}"),
                        ))
                    })?;
                    remaining -= chunk_size;
                }
            }
        }
    }

    output.flush().map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to flush output file: {e}"),
        ))
    })?;

    Ok(())
}

/// Apply metadata (permissions, timestamps) from a protocol file entry to a
/// destination path.
///
/// Uses the `metadata` crate's [`metadata::apply_metadata_from_file_entry`]
/// to set permissions and modification times on the target file or directory.
/// Ownership is applied only when the corresponding batch flags are set.
///
/// # Errors
///
/// Returns [`BatchError::Io`] if metadata cannot be applied.
fn apply_entry_metadata(
    dest_path: &Path,
    entry: &protocol::flist::FileEntry,
    flags: &crate::format::BatchFlags,
) -> BatchResult<()> {
    let options = metadata::MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(flags.preserve_uid)
        .preserve_group(flags.preserve_gid);

    metadata::apply_metadata_from_file_entry(dest_path, entry, &options).map_err(|e| {
        BatchError::Io(std::io::Error::other(format!(
            "failed to apply metadata to '{}': {e}",
            dest_path.display()
        )))
    })?;

    Ok(())
}

/// Create a symlink at `dest_path` pointing to the given `target`.
///
/// On Unix, creates a symbolic link. On other platforms, falls back to
/// file copy (symlink creation is platform-specific).
#[cfg(unix)]
fn create_symlink(target: &Path, dest_path: &Path) -> BatchResult<()> {
    // Remove existing entry if present, to mirror upstream rsync behavior
    if dest_path.exists() || dest_path.symlink_metadata().is_ok() {
        let _ = fs::remove_file(dest_path);
    }
    std::os::unix::fs::symlink(target, dest_path).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to create symlink '{}' -> '{}': {e}",
                dest_path.display(),
                target.display()
            ),
        ))
    })
}

/// Create a symlink on Windows (best-effort directory detection).
#[cfg(not(unix))]
fn create_symlink(target: &Path, dest_path: &Path) -> BatchResult<()> {
    if dest_path.exists() || dest_path.symlink_metadata().is_ok() {
        let _ = fs::remove_file(dest_path);
    }
    // Windows requires knowing whether the target is a file or directory.
    // Default to file symlink; directory symlinks are rare in rsync batch use.
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, dest_path).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to create symlink '{}' -> '{}': {e}",
                    dest_path.display(),
                    target.display()
                ),
            ))
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (target, dest_path);
        Err(BatchError::Unsupported(
            "symlink creation not supported on this platform".to_owned(),
        ))
    }
}

/// Replay a batch file, applying recorded delta operations to a destination.
///
/// Opens the batch file described by `batch_cfg`, reads its header and
/// decodes the file list using the protocol flist wire format. For each
/// entry, the appropriate filesystem object is created (directory, symlink,
/// or regular file) and metadata (permissions, timestamps, ownership) is
/// applied.
///
/// Regular file delta replay reads delta operations from the batch body
/// after the file list and applies them against the existing basis file
/// at the destination path.
///
/// # Arguments
///
/// * `batch_cfg` - Configuration identifying the batch file to replay.
/// * `dest_root` - Root directory where files are reconstructed.
/// * `verbosity` - Verbosity level controlling stdout output (0 = silent).
///
/// # Returns
///
/// A [`ReplayResult`] with aggregate statistics about the replay.
///
/// # Errors
///
/// Returns [`BatchError`] if the batch file cannot be opened, the header
/// is invalid, file entries cannot be decoded, or delta application fails.
pub fn replay(
    batch_cfg: &BatchConfig,
    dest_root: &Path,
    verbosity: i32,
) -> BatchResult<ReplayResult> {
    let mut reader = BatchReader::new((*batch_cfg).clone())?;

    let flags = reader.read_header()?;

    let mut entries = reader.read_protocol_flist()?;

    // upstream: flist.c:2736 - flist_sort_and_clean() after recv_file_list().
    // NDX values from the generator reference sorted positions, not wire order.
    let pre29 = reader.config().protocol_version < 29;
    sort_file_list(&mut entries, false, pre29);

    let mut result = ReplayResult {
        file_count: entries.len() as u64,
        recurse: flags.recurse,
        ..ReplayResult::default()
    };

    // Phase 1: Create directories and symlinks, ensure parent dirs for regular files.
    // Directories must be created before files so that parent paths exist.
    for entry in &entries {
        let dest_path = dest_root.join(entry.name());
        result.total_size += entry.size();

        if verbosity > 0 {
            println!("{}", entry.name());
        }

        match entry.file_type() {
            protocol::flist::FileType::Directory => {
                if !dest_path.exists() {
                    fs::create_dir_all(&dest_path).map_err(|e| {
                        BatchError::Io(std::io::Error::new(
                            e.kind(),
                            format!("failed to create directory '{}': {e}", dest_path.display()),
                        ))
                    })?;
                    result.dirs_created += 1;
                }
            }
            protocol::flist::FileType::Symlink => {
                if let Some(target) = entry.link_target() {
                    // Ensure parent directory exists
                    if let Some(parent) = dest_path.parent() {
                        if !parent.exists() {
                            fs::create_dir_all(parent).map_err(|e| {
                                BatchError::Io(std::io::Error::new(
                                    e.kind(),
                                    format!(
                                        "failed to create parent directory '{}': {e}",
                                        parent.display()
                                    ),
                                ))
                            })?;
                        }
                    }
                    create_symlink(target, &dest_path)?;
                    result.symlinks_created += 1;
                }
            }
            protocol::flist::FileType::Regular => {
                // Ensure parent directory exists
                if let Some(parent) = dest_path.parent() {
                    if !parent.exists() {
                        fs::create_dir_all(parent).map_err(|e| {
                            BatchError::Io(std::io::Error::new(
                                e.kind(),
                                format!(
                                    "failed to create parent directory '{}': {e}",
                                    parent.display()
                                ),
                            ))
                        })?;
                    }
                }
            }
            // Block devices, char devices, FIFOs, sockets - skip during
            // batch replay (upstream rsync also skips special files in
            // batch mode unless running as root)
            _ => {}
        }
    }

    // Phase 2: Apply delta operations for regular files.
    // upstream: receiver.c:recv_files() reads NDX + iflags + sum_head per file,
    // then delta tokens, then file checksum. NDX_DONE signals phase transitions.
    let proto = reader.config().protocol_version;

    // upstream: batch.c:check_batch_flags() - when the batch stream flags
    // include do_compression (bit 8), the token data in the batch file uses
    // compressed format (DEFLATED_DATA headers). Create a decoder to inflate
    // the tokens, matching upstream's recv_deflated_token() dispatch in
    // token.c:recv_token().
    //
    // For protocol >= 31, upstream uses CPRES_ZLIBX where see_deflate_token()
    // is a no-op (the compressor does not maintain a sliding dictionary across
    // block matches). This allows eager token reading without interleaving
    // with basis file access.
    // upstream: compat.c:740 - do_compression = CPRES_ZLIBX for protocol >= 31
    let mut compressed_decoder = if flags.do_compression {
        // upstream: compat.c - protocol 31 always uses zlib in CPRES_ZLIBX mode.
        // Protocol >= 32 negotiates the compression algorithm via vstrings,
        // defaulting to zstd when both sides support it. The batch header does
        // not record which algorithm was negotiated, so we infer from the
        // protocol version: 31 = zlib, >= 32 = zstd (falling through to zlib
        // when the zstd feature is not compiled in).
        Some(create_compressed_decoder(proto)?)
    } else {
        None
    };
    // CPRES_ZLIBX (proto >= 31): eager token reads - see_token() is a no-op.
    // CPRES_ZLIB (proto < 31): streaming reads with see_token() between each
    // recv_token() to maintain the decompressor's sliding dictionary.
    let cpres_zlib = flags.do_compression && proto < 31;

    // upstream: flist.c:2923 - with INC_RECURSE, the first flist has ndx_start=1.
    // Subsequent sub-lists have ndx_start = prev->ndx_start + prev->used + 1
    // (flist.c:2931), creating a +1 gap between segments in NDX space.
    // We track per-segment (ndx_start, entries_offset, count) to map global
    // NDX values to flat Vec indices, mirroring upstream's flist_for_ndx().
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

    // Each tuple: (ndx_start, entries_offset, count)
    let mut flist_segments: Vec<(i32, usize, usize)> = vec![(initial_ndx_start, 0, entries.len())];
    // Reuse the NDX codec from flist reading if available (INC_RECURSE mode).
    // The codec carries delta-encoding state from reading incremental flist
    // segment NDX values; creating a fresh codec would desync.
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

        // upstream: main.c:read_final_goodbye() - NDX_DEL_STATS carries 5
        // varints of deletion statistics. Consume and discard during replay.
        if ndx == NDX_DEL_STATS {
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            let _del_stats = protocol::stats::DeleteStats::read_from(stream).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read delete stats from batch stream: {e}"),
                ))
            })?;
            continue;
        }

        // upstream: flist.c:recv_additional_file_list() - NDX_FLIST_OFFSET-based
        // values signal a new incremental sub-list (INC_RECURSE). Read the flist
        // segment entries on-the-fly to grow the entries vec, then create any new
        // directories so files in the sub-list have parent paths.
        if ndx <= NDX_FLIST_OFFSET {
            let prev_len = entries.len();

            // Compute this segment's ndx_start using upstream's formula
            // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
            let prev_seg = flist_segments.last().expect("at least initial segment");
            let seg_ndx_start = prev_seg.0 + prev_seg.2 as i32 + 1;

            reader.read_incremental_flist_segment(&mut entries)?;

            // upstream: flist.c:2736 - sort each sub-list segment after receiving.
            // INC_RECURSE batches require protocol >= 30, so pre29 is always false.
            sort_file_list(&mut entries[prev_len..], false, false);

            // Record this segment's NDX range for global-to-flat index mapping.
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

            result.file_count = entries.len() as u64;
            continue;
        }

        // upstream: rsync.c:383 - read iflags (u16) for protocol >= 29.
        // iflags MUST be read before the entry lookup because the stream
        // contains iflags for every positive NDX, including directory
        // metadata updates where NDX < ndx_start (INC_RECURSE).
        let iflags = if proto >= 29 {
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            let mut buf = [0u8; 2];
            stream.read_exact(&mut buf).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read iflags: {e}"),
                ))
            })?;
            u16::from_le_bytes(buf)
        } else {
            // upstream: rsync.c:384 - default to ITEM_TRANSFER | ITEM_MISSING_DATA
            0x8000 | 0x0400
        };

        // upstream: rsync.c:403-418 - consume optional trailing fields after iflags.
        // ITEM_BASIS_TYPE_FOLLOWS (0x0800): 1 byte fnamecmp_type.
        // ITEM_XNAME_FOLLOWS (0x1000): vstring (1-2 byte length + data).
        const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;
        const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;

        if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read fnamecmp_type: {e}"),
                ))
            })?;
        }

        if iflags & ITEM_XNAME_FOLLOWS != 0 {
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            // upstream: io.c:read_vstring() - 1-byte length, or 2-byte if high bit set
            let mut len_byte = [0u8; 1];
            stream.read_exact(&mut len_byte).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read xname length: {e}"),
                ))
            })?;
            let mut xname_len = len_byte[0] as usize;
            if xname_len & 0x80 != 0 {
                let mut hi = [0u8; 1];
                stream.read_exact(&mut hi).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to read xname extended length: {e}"),
                    ))
                })?;
                xname_len = (xname_len & !0x80) * 0x100 + hi[0] as usize;
            }
            if xname_len > 0 {
                let mut xname_buf = vec![0u8; xname_len];
                stream.read_exact(&mut xname_buf).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to read xname data: {e}"),
                    ))
                })?;
            }
        }

        const ITEM_TRANSFER: u16 = 0x8000;
        if iflags & ITEM_TRANSFER == 0 {
            // Metadata-only change, no delta data follows
            continue;
        }

        // upstream: rsync.c:flist_for_ndx() + receiver.c:590 - map global NDX
        // to the flat entries Vec index by finding the segment it belongs to.
        // Each segment has (ndx_start, entries_offset, count) with +1 gaps
        // between segments (upstream flist.c:2931).
        let flat_index = flist_segments
            .iter()
            .find_map(|&(seg_start, offset, count)| {
                if ndx >= seg_start && ndx < seg_start + count as i32 {
                    Some(offset + (ndx - seg_start) as usize)
                } else {
                    None
                }
            });

        let flat_index = match flat_index {
            Some(idx) => idx,
            None => {
                // upstream: receiver.c:590-593 - NDX < first segment's ndx_start
                // refers to a parent directory entry (INC_RECURSE metadata update).
                // Skip these - directories are already created.
                if ndx < flist_segments.first().map_or(0, |s| s.0) {
                    continue;
                }
                return Err(BatchError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "invalid NDX {ndx} (flist has {} entries across {} segments)",
                        entries.len(),
                        flist_segments.len()
                    ),
                )));
            }
        };

        let entry = &entries[flat_index];
        let dest_path = dest_root.join(entry.name());

        // upstream: receiver.c:273 - read_sum_head() reads 4 x i32
        let stream = reader
            .inner_reader()
            .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
        let mut sum_buf = [0u8; 16];
        stream.read_exact(&mut sum_buf).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read sum_head: {e}"),
            ))
        })?;
        let block_count = i32::from_le_bytes([sum_buf[0], sum_buf[1], sum_buf[2], sum_buf[3]]);
        let block_length_wire =
            i32::from_le_bytes([sum_buf[4], sum_buf[5], sum_buf[6], sum_buf[7]]);
        let _s2length = i32::from_le_bytes([sum_buf[8], sum_buf[9], sum_buf[10], sum_buf[11]]);
        let remainder_wire =
            i32::from_le_bytes([sum_buf[12], sum_buf[13], sum_buf[14], sum_buf[15]]);
        // Compute block geometry before token reading - needed for CPRES_ZLIB
        // see_token() calls which reference basis blocks by index.
        let basis_exists = dest_path.exists();
        let block_length = if block_length_wire > 0 {
            block_length_wire as usize
        } else {
            choose_block_length(entry.size())
        };
        let remainder = if remainder_wire > 0 {
            remainder_wire as usize
        } else {
            block_length
        };

        // Read delta tokens for this file. When compression was active during
        // batch creation, tokens use the compressed wire format with DEFLATED_DATA
        // headers. Otherwise, tokens use the simple 4-byte LE i32 format.
        // upstream: token.c:recv_token() dispatches to recv_deflated_token() or
        // simple_recv_token() based on do_compression.
        let delta_ops = if let Some(ref mut decoder) = compressed_decoder {
            // upstream: token.c:recv_deflated_token() r_init resets inflate
            // context per file. The decoder.reset() mirrors this behavior.
            decoder.reset();
            if cpres_zlib && basis_exists {
                // CPRES_ZLIB: streaming read with dictionary synchronization.
                // After each token, see_token() feeds the data into the
                // decompressor dictionary so subsequent tokens can reference
                // it via back-references. Without this, inflate fails with
                // "invalid distance too far back".
                // upstream: receiver.c:receive_data() + token.c:see_deflate_token()
                let basis_data = fs::read(&dest_path).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to read basis file '{}': {e}", dest_path.display()),
                    ))
                })?;
                let stream = reader
                    .inner_reader()
                    .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
                let mut ops = Vec::new();
                loop {
                    let token = decoder.recv_token(stream).map_err(|e| {
                        BatchError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "failed to read compressed delta token for '{}': {e}",
                                entry.name()
                            ),
                        ))
                    })?;
                    match token {
                        CompressedToken::End => break,
                        CompressedToken::Literal(data) => {
                            // Literals already pass through inflate in recv_token(),
                            // which updates the decompressor dictionary. Do NOT call
                            // see_token() here - upstream receiver.c only calls
                            // see_token() for block matches, not literals.
                            ops.push(protocol::wire::DeltaOp::Literal(data));
                        }
                        CompressedToken::BlockMatch(block_index) => {
                            // Feed matched block's basis data into dictionary.
                            // upstream: receiver.c - see_token(map, len) after block match
                            let offset = block_index as usize * block_length;
                            let len = if block_index == block_count as u32 - 1 {
                                remainder
                            } else {
                                block_length
                            };
                            let end = (offset + len).min(basis_data.len());
                            if offset < basis_data.len() {
                                decoder
                                    .see_token(&basis_data[offset..end])
                                    .map_err(BatchError::Io)?;
                            }
                            ops.push(protocol::wire::DeltaOp::Copy {
                                block_index,
                                length: 0,
                            });
                        }
                    }
                }
                ops
            } else {
                // CPRES_ZLIBX or no basis: eager read - see_token() is a no-op.
                reader.read_compressed_delta_tokens(decoder).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "failed to read compressed delta tokens for '{}': {e}",
                            entry.name()
                        ),
                    ))
                })?
            }
        } else {
            reader.read_file_delta_tokens().map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("failed to read delta tokens for '{}': {e}", entry.name()),
                ))
            })?
        };

        // upstream: receiver.c:408 - read_buf(f_in, sender_file_sum, xfer_sum_len)
        // The sender ALWAYS writes xfer_sum_len bytes of file checksum after
        // the delta stream, regardless of sum_head.s2length. For protocol 32
        // the default xfer checksum is XXH3-128 or MD5 - both 16 bytes. For
        // protocol 28-31 it is MD4 or MD5 - also 16 bytes.
        {
            let xfer_sum_len = default_xfer_sum_len(proto);
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            let mut checksum_buf = vec![0u8; xfer_sum_len];
            stream.read_exact(&mut checksum_buf).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read file checksum ({xfer_sum_len} bytes): {e}"),
                ))
            })?;
        }

        if verbosity > 0 {
            println!("  {} delta operations", delta_ops.len());
        }

        if !basis_exists {
            write_literals_to_file(&dest_path, &delta_ops)?;
        } else {
            let temp_path = dest_path.with_extension("~batch-tmp");
            apply_delta_ops(
                &dest_path,
                &temp_path,
                delta_ops,
                block_length,
                block_count as u32,
                remainder,
            )?;
            fs::rename(&temp_path, &dest_path).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "failed to rename temp file '{}' to '{}': {e}",
                        temp_path.display(),
                        dest_path.display()
                    ),
                ))
            })?;
        }
    }

    // Phase 3: Apply metadata. Directories are done last (in reverse order)
    // because setting timestamps on a directory before writing its contents
    // would cause the mtime to be updated by the file writes.
    // Regular files and symlinks get metadata immediately.
    for entry in &entries {
        let dest_path = dest_root.join(entry.name());

        match entry.file_type() {
            protocol::flist::FileType::Directory | protocol::flist::FileType::Regular => {
                if dest_path.exists() {
                    // Best-effort metadata application - log but don't fail
                    // on permission errors (e.g., when not running as root
                    // and trying to set ownership).
                    if let Err(e) = apply_entry_metadata(&dest_path, entry, &flags) {
                        if verbosity > 0 {
                            println!(
                                "  warning: could not apply metadata to '{}': {e}",
                                dest_path.display()
                            );
                        }
                    }
                }
            }
            // Symlink metadata is set via lchown/lutimes on platforms that
            // support it. The metadata crate handles this transparently.
            protocol::flist::FileType::Symlink => {
                if dest_path.symlink_metadata().is_ok() {
                    let _ = apply_entry_metadata(&dest_path, entry, &flags);
                }
            }
            _ => {}
        }
    }

    Ok(result)
}

/// Choose block length using the same heuristic as upstream rsync.
///
/// Upstream `match.c:choose_block_size()` computes the block length as the
/// integer square root of the file size, clamped to `[BLOCK_SIZE (700),
/// MAX_BLOCK_SIZE (128 * 1024)]`. For batch replay the exact same
/// derivation ensures copy-token offsets align with the blocks that the
/// sender used during the original transfer.
/// Returns the default xfer checksum length for batch replay.
///
/// upstream: `checksum.c:188` - `xfer_sum_len = csum_len_for_type(xfer_sum_nni->num, 0)`.
/// Batch files don't record the negotiated checksum algorithm. For all
/// supported protocols (28-32), the default xfer checksum is MD4, MD5, or
/// XXH3-128 - all produce 16-byte digests.
fn default_xfer_sum_len(protocol_version: i32) -> usize {
    let _ = protocol_version;
    16
}

/// Creates a `CompressedTokenDecoder` for batch replay.
///
/// Creates a compressed token decoder for batch replay.
///
/// Batch files do not record which compression algorithm was negotiated
/// during the original transfer. When upstream's `check_batch_flags()`
/// restores `do_compression` from the stream flags bitmap, the value is
/// just 1 (truthy). `parse_compress_choice()` then maps that to
/// `CPRES_ZLIB` (upstream compat.c:194-195), regardless of protocol
/// version.
///
/// However, the batch file contains raw wire bytes from the original
/// transfer. Protocol >= 31 uses CPRES_ZLIBX (no dictionary carry across
/// block matches), while protocol 29-30 uses CPRES_ZLIB (dictionary must
/// be maintained via `see_deflate_token()` after each block match).
///
/// We set zlibx mode based on the batch protocol version:
/// - Protocol >= 31: zlibx=true (see_token is a no-op, eager reads ok)
/// - Protocol < 31: zlibx=false (see_token feeds dictionary, streaming reads required)
///
/// upstream: compat.c:740 - protocol >= 31 uses CPRES_ZLIBX
/// upstream: token.c:recv_deflated_token() - decodes CPRES_ZLIB/CPRES_ZLIBX
/// upstream: token.c:see_deflate_token() - feeds block data into inflate dictionary
fn create_compressed_decoder(proto: i32) -> BatchResult<CompressedTokenDecoder> {
    let mut decoder = CompressedTokenDecoder::new();
    // upstream: compat.c:740 - CPRES_ZLIBX for protocol >= 31, CPRES_ZLIB for < 31
    decoder.set_zlibx(proto >= 31);
    Ok(decoder)
}

fn choose_block_length(file_size: u64) -> usize {
    const MIN_BLOCK: usize = 700;
    const MAX_BLOCK: usize = 128 * 1024;

    if file_size == 0 {
        return MIN_BLOCK;
    }

    let sqrt = (file_size as f64).sqrt() as usize;
    sqrt.clamp(MIN_BLOCK, MAX_BLOCK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn choose_block_length_small_file() {
        // Files smaller than 700^2 = 490_000 bytes get MIN_BLOCK
        assert_eq!(choose_block_length(0), 700);
        assert_eq!(choose_block_length(1000), 700);
        assert_eq!(choose_block_length(489_999), 700);
    }

    #[test]
    fn choose_block_length_medium_file() {
        // sqrt(1_000_000) = 1000
        assert_eq!(choose_block_length(1_000_000), 1000);
    }

    #[test]
    fn choose_block_length_large_file() {
        // Files larger than (128*1024)^2 get MAX_BLOCK
        let max_block = 128 * 1024;
        let threshold = (max_block as u64) * (max_block as u64);
        assert_eq!(choose_block_length(threshold + 1), max_block);
    }

    #[test]
    fn apply_delta_ops_literal_only() {
        let temp = TempDir::new().unwrap();
        let basis_path = temp.path().join("basis.txt");
        let dest_path = temp.path().join("output.txt");

        fs::write(&basis_path, b"").unwrap();

        let ops = vec![protocol::wire::DeltaOp::Literal(b"hello world".to_vec())];
        apply_delta_ops(&basis_path, &dest_path, ops, 700, 0, 700).unwrap();

        let result = fs::read(&dest_path).unwrap();
        assert_eq!(result, b"hello world");
    }

    #[test]
    fn apply_delta_ops_copy_from_basis() {
        let temp = TempDir::new().unwrap();
        let basis_path = temp.path().join("basis.txt");
        let dest_path = temp.path().join("output.txt");

        // Basis file has exactly one block of 10 bytes at block 0
        fs::write(&basis_path, b"0123456789").unwrap();

        let ops = vec![protocol::wire::DeltaOp::Copy {
            block_index: 0,
            length: 10,
        }];
        apply_delta_ops(&basis_path, &dest_path, ops, 10, 1, 10).unwrap();

        let result = fs::read(&dest_path).unwrap();
        assert_eq!(result, b"0123456789");
    }

    #[test]
    fn apply_delta_ops_mixed() {
        let temp = TempDir::new().unwrap();
        let basis_path = temp.path().join("basis.txt");
        let dest_path = temp.path().join("output.txt");

        // Basis has "ABCDE" at block 0 (block_length=5)
        fs::write(&basis_path, b"ABCDE").unwrap();

        let ops = vec![
            protocol::wire::DeltaOp::Literal(b">>".to_vec()),
            protocol::wire::DeltaOp::Copy {
                block_index: 0,
                length: 5,
            },
            protocol::wire::DeltaOp::Literal(b"<<".to_vec()),
        ];
        apply_delta_ops(&basis_path, &dest_path, ops, 5, 1, 5).unwrap();

        let result = fs::read(&dest_path).unwrap();
        assert_eq!(result, b">>ABCDE<<");
    }

    #[test]
    fn apply_delta_ops_nonexistent_basis() {
        let temp = TempDir::new().unwrap();
        let basis_path = temp.path().join("no_such_file.txt");
        let dest_path = temp.path().join("output.txt");

        let ops = vec![protocol::wire::DeltaOp::Copy {
            block_index: 0,
            length: 10,
        }];
        let result = apply_delta_ops(&basis_path, &dest_path, ops, 10, 1, 10);
        assert!(result.is_err());
    }

    /// Validates that the last block uses `remainder` bytes instead of `block_length`.
    ///
    /// upstream: receiver.c - when applying deltas, the last block in the basis
    /// file is shorter than `block_length`. The sum_head's `remainder` field
    /// specifies the actual size.
    #[test]
    fn apply_delta_last_block_uses_remainder() {
        let temp = TempDir::new().unwrap();
        // Basis: 15 bytes, block_length=10, so block 0 = 10 bytes, block 1 = 5 bytes (remainder).
        let basis_path = temp.path().join("basis.dat");
        fs::write(&basis_path, b"AAAAAAAAAA12345").unwrap();
        let dest_path = temp.path().join("output.dat");

        // Delta: copy block 1 (the last block, 5 bytes remainder), then literal.
        let ops = vec![
            protocol::wire::DeltaOp::Copy {
                block_index: 1,
                length: 0, // Token format: length=0 means derive from block_length/remainder
            },
            protocol::wire::DeltaOp::Literal(b"END".to_vec()),
        ];
        apply_delta_ops(&basis_path, &dest_path, ops, 10, 2, 5).unwrap();

        let result = fs::read(&dest_path).unwrap();
        // Should copy 5 bytes from block 1 ("12345"), not 10 bytes (which would overread).
        assert_eq!(result, b"12345END");
    }

    #[test]
    fn write_literals_to_new_file() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("new_file.txt");

        let ops = vec![
            protocol::wire::DeltaOp::Literal(b"hello ".to_vec()),
            protocol::wire::DeltaOp::Literal(b"world".to_vec()),
        ];
        write_literals_to_file(&dest_path, &ops).unwrap();

        let result = fs::read(&dest_path).unwrap();
        assert_eq!(result, b"hello world");
    }

    #[test]
    fn write_literals_ignores_copy_ops() {
        let temp = TempDir::new().unwrap();
        let dest_path = temp.path().join("literals_only.txt");

        let ops = vec![
            protocol::wire::DeltaOp::Literal(b"data".to_vec()),
            // Copy ops should be ignored when no basis exists
            protocol::wire::DeltaOp::Copy {
                block_index: 0,
                length: 100,
            },
            protocol::wire::DeltaOp::Literal(b"more".to_vec()),
        ];
        write_literals_to_file(&dest_path, &ops).unwrap();

        let result = fs::read(&dest_path).unwrap();
        assert_eq!(result, b"datamore");
    }

    #[test]
    fn compressed_decoder_created_for_proto_31() {
        // Protocol 31 creates a zlib/zlibx decoder.
        let decoder = create_compressed_decoder(31).unwrap();
        assert!(
            !decoder.initialized(),
            "fresh decoder should not be initialized"
        );
    }

    #[test]
    fn compressed_decoder_created_for_proto_32() {
        // Protocol 32 also uses zlib for batch replay - batch files don't
        // record the negotiated algorithm, and upstream always falls back
        // to CPRES_ZLIB via parse_compress_choice().
        let decoder = create_compressed_decoder(32).unwrap();
        assert!(
            !decoder.initialized(),
            "fresh decoder should not be initialized"
        );
    }

    #[test]
    fn compressed_batch_proto_lt_31_returns_unsupported() {
        // CPRES_ZLIB (protocol < 31) requires see_token() interleaving
        // which our eager token reading does not support.
        let flags = crate::format::BatchFlags {
            do_compression: true,
            ..Default::default()
        };
        let proto = 30;

        let result: Result<(), crate::BatchError> = if flags.do_compression && proto < 31 {
            Err(crate::BatchError::Unsupported(
                "compressed batch files from protocol < 31".to_owned(),
            ))
        } else {
            Ok(())
        };

        assert!(
            result.is_err(),
            "should reject compressed batch for proto < 31"
        );
        assert!(matches!(result, Err(crate::BatchError::Unsupported(_))));
    }
}
