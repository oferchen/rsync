//! Delta-application primitives for batch replay.
//!
//! This module contains the routines that translate decoded delta operations
//! into concrete file output, plus the block-geometry helpers used by both
//! the orchestrator in [`super::replay`] and the unit tests.
//!
//! - [`apply_delta_ops`] writes the reconstructed file from a basis + ops.
//! - [`write_literals_to_file`] handles the no-basis (literal-only) path.
//! - [`choose_block_length`] mirrors upstream `match.c:choose_block_size()`.
//! - [`default_xfer_sum_len`] returns the per-file transfer checksum length.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{BatchError, BatchResult};

/// Write literal-only delta operations to a new file.
///
/// When no basis file exists at the destination, the delta stream consists
/// entirely of literal data. This function creates the output file and writes
/// all literal chunks sequentially, ignoring any copy operations (which should
/// not be present without a basis).
pub(super) fn write_literals_to_file(
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

/// Applies delta operations to reconstruct a file from a basis file.
///
/// Reads copy and literal tokens from `delta_ops` and writes the
/// reconstructed output to `dest_path`. Copy tokens reference blocks in
/// `basis_path` at offsets computed as `block_index * block_length`.
///
/// `block_count` is the number of blocks in the basis file's signature.
/// `remainder` is the size of the last block (which may be shorter than
/// `block_length`). For the last block (index == block_count - 1), the copy
/// uses `remainder` bytes instead of `block_length`.
///
/// upstream: receiver.c:recv_files() / match.c - block_length for all blocks
/// except the last, which uses remainder.
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

/// Returns the default xfer checksum length for batch replay.
///
/// upstream: `checksum.c:188` - `xfer_sum_len = csum_len_for_type(xfer_sum_nni->num, 0)`.
/// Batch files don't record the negotiated checksum algorithm. For all
/// supported protocols (28-32), the default xfer checksum is MD4, MD5, or
/// XXH3-128 - all produce 16-byte digests.
pub(super) fn default_xfer_sum_len(protocol_version: i32) -> usize {
    let _ = protocol_version;
    16
}

/// Chooses block length using the same heuristic as upstream rsync.
///
/// Upstream `match.c:choose_block_size()` computes the block length as the
/// integer square root of the file size, clamped to `[BLOCK_SIZE (700),
/// MAX_BLOCK_SIZE (128 * 1024)]`. For batch replay the exact same
/// derivation ensures copy-token offsets align with the blocks that the
/// sender used during the original transfer.
pub(super) fn choose_block_length(file_size: u64) -> usize {
    const MIN_BLOCK: usize = 700;
    const MAX_BLOCK: usize = 128 * 1024;

    if file_size == 0 {
        return MIN_BLOCK;
    }

    let sqrt = (file_size as f64).sqrt() as usize;
    sqrt.clamp(MIN_BLOCK, MAX_BLOCK)
}
