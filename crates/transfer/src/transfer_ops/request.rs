//! File transfer request phase.
//!
//! Sends NDX + iflags + sum_head + signature blocks to the sender,
//! creating a `PendingTransfer` for subsequent response processing.
//!
//! # Upstream Reference
//!
//! - `generator.c:recv_generator()` sends NDX, iflags, sum_head
//! - `match.c:write_sum_head()` sends signature header
//! - `match.c:395` sends signature blocks

use std::io::{self, Write};
use std::path::PathBuf;

use engine::signature::FileSignature;
use protocol::codec::NdxCodec;

use crate::pipeline::PendingTransfer;
use crate::receiver::{SenderAttrs, SumHead, write_signature_blocks};

use super::RequestConfig;

/// Sends a file transfer request to the sender.
///
/// Writes NDX + iflags + sum_head + signature blocks to the wire.
/// Returns a `PendingTransfer` to track this request for response processing.
///
/// # Arguments
///
/// * `writer` - Output stream to sender
/// * `ndx_codec` - NDX encoder (maintains delta encoding state)
/// * `ndx` - File index in the file list
/// * `file_path` - Destination path for the file
/// * `signature` - Optional signature from basis file
/// * `basis_path` - Optional path to basis file
/// * `target_size` - Expected file size
/// * `config` - Protocol configuration
///
/// # Returns
///
/// A `PendingTransfer` that should be stored for response processing.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` sends NDX, iflags, sum_head
/// - `match.c:write_sum_head()` sends signature header
/// - `match.c:395` sends signature blocks
#[allow(clippy::too_many_arguments)]
pub fn send_file_request<W: Write + ?Sized>(
    writer: &mut W,
    ndx_codec: &mut impl NdxCodec,
    ndx: i32,
    file_path: PathBuf,
    signature: Option<FileSignature>,
    basis_path: Option<PathBuf>,
    target_size: u64,
    config: &RequestConfig<'_>,
) -> io::Result<PendingTransfer> {
    // Send file index using NDX encoding
    ndx_codec.write_ndx(writer, ndx)?;

    // For protocol >= 29, sender expects iflags after NDX
    // ITEM_TRANSFER (0x8000) tells sender to read sum_head and send delta
    if config.write_iflags {
        writer.write_all(&SenderAttrs::ITEM_TRANSFER.to_le_bytes())?;
    }

    // Send sum_head (signature header)
    let sum_head = match signature {
        Some(ref sig) => SumHead::from_signature(sig),
        None => SumHead::empty(),
    };
    sum_head.write(writer)?;

    // Write signature blocks if we have a signature
    if let Some(ref sig) = signature {
        write_signature_blocks(writer, sig, sum_head.s2length)?;
    }
    // No flush here - the pipeline loop flushes before blocking on response
    // reads. Per-file flushes defeat buffer batching, causing 1 sendto per
    // ~20-byte request instead of upstream's batched iobuf_out pattern.

    // Create pending transfer for response processing
    let pending = match (signature, basis_path) {
        (Some(sig), Some(basis)) => {
            PendingTransfer::new_delta_transfer(ndx, file_path, basis, sig, target_size)
        }
        _ => PendingTransfer::new_full_transfer(ndx, file_path, target_size),
    };

    Ok(pending)
}
