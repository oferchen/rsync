//! Batch replay logic for applying recorded delta operations to a destination.
//!
//! This module contains the core replay implementation that reads a batch file
//! and applies the recorded delta operations to reconstruct files at the
//! destination. The replay logic is decoupled from the orchestration layer
//! (core crate) so it can be tested and reused independently.
//!
//! # Overview
//!
//! Replay proceeds in two phases:
//!
//! 1. **Header validation**: The batch header is read and the stream flags
//!    bitmap is verified against the protocol version.
//! 2. **File iteration**: Each file entry is read from the batch, its delta
//!    operations are decoded, and [`apply_delta_ops`] reconstructs the target
//!    file by combining basis data with literal insertions.
//!
//! # Upstream Reference
//!
//! - `batch.c:read_stream_flags()` — reads the stream flags bitmap
//! - `main.c:do_recv()` — orchestrates file list + delta application
//! - `receiver.c:recv_files()` — per-file delta application

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{BatchError, BatchResult};
use crate::reader::BatchReader;
use crate::BatchConfig;

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

                let mut remaining = length as usize;
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

/// Replay a batch file, applying recorded delta operations to a destination.
///
/// Opens the batch file described by `batch_cfg`, reads its header and file
/// entries, and applies delta operations for each entry under `dest_root`.
/// When `verbosity > 0`, file names and operation counts are printed to
/// stdout to mirror upstream rsync's `--verbose` output.
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
/// is invalid, file entries cannot be read, or delta application fails.
pub fn replay(
    batch_cfg: &BatchConfig,
    dest_root: &Path,
    verbosity: i32,
) -> BatchResult<ReplayResult> {
    let mut reader = BatchReader::new((*batch_cfg).clone())?;

    let flags = reader.read_header()?;

    let mut file_count = 0u64;
    let mut total_size = 0u64;

    while let Some(entry) = reader.read_file_entry()? {
        file_count += 1;
        total_size += entry.size;

        if verbosity > 0 {
            println!("{}", entry.path);
        }

        let delta_ops = reader.read_all_delta_ops().map_err(|e| {
            BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read delta operations for '{}': {e}", entry.path),
            ))
        })?;

        if verbosity > 0 {
            println!("  {} delta operations", delta_ops.len());
        }

        let dest_path = dest_root.join(&entry.path);

        // For batch replay, the basis file is the existing file at the
        // destination (upstream receiver.c uses the same path for both).
        let basis_path = dest_path.clone();

        // Upstream rsync calculates block_length dynamically based on file
        // size (match.c:365, choose_block_size()). The batch file entry
        // carries the original file size, so we derive the block length
        // using the same heuristic: sqrt(file_size) clamped to [700, 16384].
        let block_length = choose_block_length(entry.size);
        apply_delta_ops(&basis_path, &dest_path, delta_ops, block_length)?;
    }

    Ok(ReplayResult {
        file_count,
        total_size,
        recurse: flags.recurse,
    })
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
}
