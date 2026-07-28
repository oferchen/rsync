//! Delta-application primitives for batch replay.
//!
//! This module contains the routines that translate decoded delta operations
//! into concrete file output, plus the block-geometry helpers used by both
//! the orchestrator in [`super::replay`] and the unit tests.
//!
//! - [`apply_delta_ops`] writes the reconstructed file from a basis + ops.
//! - [`write_literals_to_file`] handles the no-basis (literal-only) path.
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

/// Widens a wire block index for [`protocol::wire::SumHead::block_span`],
/// which mirrors upstream's signed `i < 0 || i >= sum.count` test.
///
/// A `u32` past [`i32::MAX`] is an index upstream would have read back as
/// negative, so it maps to -1 and is rejected rather than truncated.
pub(super) const fn block_index_as_i32(block_index: u32) -> i32 {
    if block_index > i32::MAX as u32 {
        -1
    } else {
        block_index as i32
    }
}

/// Applies delta operations to reconstruct a file from a basis file.
///
/// Reads copy and literal tokens from `delta_ops` and writes the
/// reconstructed output to `dest_path`. Copy tokens reference blocks in
/// `basis_path` at offsets computed as `block_index * block_length`.
///
/// `sum_head` is the block geometry the batch advertised for this file. Every
/// copy token is resolved through it, so a token that references a block the
/// header does not describe fails here rather than reconstructing the wrong
/// bytes - the same check upstream performs at `receiver.c:414`.
///
/// upstream: receiver.c:recv_files() / match.c - block_length for all blocks
/// except the last, which uses remainder.
///
/// # Errors
///
/// Returns [`BatchError::Io`] if the basis file cannot be opened, the output
/// file cannot be created, any read/write/seek operation fails, or a copy
/// token references a block outside `sum_head`.
pub fn apply_delta_ops(
    basis_path: &Path,
    dest_path: &Path,
    delta_ops: Vec<protocol::wire::DeltaOp>,
    sum_head: protocol::wire::SumHead,
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
                // Token-format block matches encode length=0 because the
                // receiver derives the block span from the advertised
                // geometry. Resolving through the sum_head is what rejects a
                // header that does not describe this body.
                // upstream: receiver.c:414-422
                let (offset, span) = sum_head
                    .block_span(block_index_as_i32(block_index))
                    .map_err(|error| {
                        BatchError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("{error} while replaying '{}'", dest_path.display()),
                        ))
                    })?;

                basis.seek(SeekFrom::Start(offset)).map_err(|e| {
                    BatchError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to seek to offset {offset}: {e}"),
                    ))
                })?;

                let effective_length = if length > 0 {
                    length as usize
                } else {
                    span as usize
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
