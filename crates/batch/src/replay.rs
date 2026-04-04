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

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::reader::BatchReader;

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
pub fn apply_delta_ops(
    basis_path: &Path,
    dest_path: &Path,
    delta_ops: Vec<protocol::wire::DeltaOp>,
    block_length: usize,
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
                // block_length when the explicit length is zero.
                let effective_length = if length == 0 {
                    block_length
                } else {
                    length as usize
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

    // Decode the file list using the protocol flist decoder.
    // This replaces the previous custom FileEntry::read_from() approach
    // and produces protocol::flist::FileEntry values that are compatible
    // with upstream rsync's batch file wire format.
    let entries = reader.read_protocol_flist()?;

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

        // upstream: io.c - NDX_FLIST_OFFSET-based values are incremental file
        // list directory indices (INC_RECURSE sub-lists). Skip during replay.
        if ndx <= NDX_FLIST_OFFSET {
            continue;
        }

        if ndx < 0 || ndx as usize >= entries.len() {
            return Err(BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid NDX {ndx} (flist has {} entries)", entries.len()),
            )));
        }

        // upstream: rsync.c:383 - read iflags (u16) for protocol >= 29
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

        const ITEM_TRANSFER: u16 = 0x8000;
        if iflags & ITEM_TRANSFER == 0 {
            // Metadata-only change, no delta data follows
            continue;
        }

        let entry = &entries[ndx as usize];
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
        let _count = i32::from_le_bytes([sum_buf[0], sum_buf[1], sum_buf[2], sum_buf[3]]);
        let block_length_wire =
            i32::from_le_bytes([sum_buf[4], sum_buf[5], sum_buf[6], sum_buf[7]]);
        let s2length = i32::from_le_bytes([sum_buf[8], sum_buf[9], sum_buf[10], sum_buf[11]]);
        let _remainder = i32::from_le_bytes([sum_buf[12], sum_buf[13], sum_buf[14], sum_buf[15]]);

        // Read delta tokens for this file
        let delta_ops = reader.read_file_delta_tokens().map_err(|e| {
            BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read delta tokens for '{}': {e}", entry.name()),
            ))
        })?;

        // upstream: receiver.c:408 - read file checksum (s2length bytes)
        if s2length > 0 {
            let stream = reader
                .inner_reader()
                .ok_or_else(|| BatchError::Io(std::io::Error::other("batch file not open")))?;
            let mut checksum_buf = vec![0u8; s2length as usize];
            stream.read_exact(&mut checksum_buf).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read file checksum: {e}"),
                ))
            })?;
        }

        if verbosity > 0 {
            println!("  {} delta operations", delta_ops.len());
        }

        let basis_exists = dest_path.exists();
        let block_length = if block_length_wire > 0 {
            block_length_wire as usize
        } else {
            choose_block_length(entry.size())
        };

        if !basis_exists {
            write_literals_to_file(&dest_path, &delta_ops)?;
        } else {
            let temp_path = dest_path.with_extension("~batch-tmp");
            apply_delta_ops(&dest_path, &temp_path, delta_ops, block_length)?;
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
        apply_delta_ops(&basis_path, &dest_path, ops, 700).unwrap();

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
        apply_delta_ops(&basis_path, &dest_path, ops, 10).unwrap();

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
        apply_delta_ops(&basis_path, &dest_path, ops, 5).unwrap();

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
        let result = apply_delta_ops(&basis_path, &dest_path, ops, 10);
        assert!(result.is_err());
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
}
