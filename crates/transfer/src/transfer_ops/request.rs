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
use protocol::xattr::XattrList;

use crate::pipeline::PendingTransfer;
use crate::receiver::{SenderAttrs, SumHead, write_signature_blocks, write_xattr_request};

use super::RequestConfig;

/// Sends a file transfer request to the sender.
///
/// Writes NDX + iflags + sum_head + signature blocks to the wire.
/// When `xattr_list` is provided and contains entries needing full values,
/// `ITEM_REPORT_XATTR` is included in iflags and the xattr request is
/// written after the standard fields.
///
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
/// - `xattrs.c:623` - `send_xattr_request()` writes abbreviated xattr request
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
    send_file_request_xattr(
        writer,
        ndx_codec,
        ndx,
        file_path,
        signature,
        basis_path,
        target_size,
        config,
        None,
    )
}

/// Sends a file transfer request with optional xattr abbreviation request.
///
/// See [`send_file_request`] for details. When `xattr_list` is provided and
/// has entries in `XSTATE_TODO` state, `ITEM_REPORT_XATTR` is set in iflags
/// and the xattr request is written after the standard fields.
///
/// # Upstream Reference
///
/// - `sender.c:193-196` - sender reads xattr request when ITEM_REPORT_XATTR set
/// - `xattrs.c:623-675` - `send_xattr_request()` writes request from generator
#[allow(clippy::too_many_arguments)]
pub fn send_file_request_xattr<W: Write + ?Sized>(
    writer: &mut W,
    ndx_codec: &mut impl NdxCodec,
    ndx: i32,
    file_path: PathBuf,
    signature: Option<FileSignature>,
    basis_path: Option<PathBuf>,
    target_size: u64,
    config: &RequestConfig<'_>,
    xattr_list: Option<&XattrList>,
) -> io::Result<PendingTransfer> {
    ndx_codec.write_ndx(writer, ndx)?;

    // For protocol >= 29, sender expects iflags after NDX.
    // ITEM_TRANSFER (0x8000) tells sender to read sum_head and send delta.
    // upstream: generator.c - ITEM_REPORT_XATTR set when xattr_diff() detects changes
    if config.write_iflags {
        let has_xattr_request = xattr_list.is_some_and(|list| {
            list.iter()
                .any(|e| e.state().needs_send() || e.state().needs_request())
        });
        let mut iflags = SenderAttrs::ITEM_TRANSFER;
        if has_xattr_request && config.preserve_xattrs {
            iflags |= SenderAttrs::ITEM_REPORT_XATTR;
        }
        writer.write_all(&iflags.to_le_bytes())?;

        // upstream: sender.c:193-196 - write xattr request data after iflags
        if has_xattr_request && config.preserve_xattrs {
            if let Some(list) = xattr_list {
                write_xattr_request(writer, list)?;
            }
        }
    }

    let sum_head = match signature {
        Some(ref sig) => SumHead::from_signature(sig),
        None => SumHead::empty(),
    };
    sum_head.write(writer)?;

    // upstream: generator.c:775-776 - in append mode, generator skips writing
    // signature blocks after sum_head. The sender's receive_sums() (sender.c:87-92)
    // returns early without reading blocks, using sum_head to calculate existing length.
    if !config.append {
        if let Some(ref sig) = signature {
            write_signature_blocks(writer, sig, sum_head.s2length)?;
        }
    }
    // No flush here - the pipeline loop flushes before blocking on response
    // reads. Per-file flushes defeat buffer batching, causing 1 sendto per
    // ~20-byte request instead of upstream's batched iobuf_out pattern.

    let pending = match (signature, basis_path) {
        (Some(sig), Some(basis)) => {
            PendingTransfer::new_delta_transfer(ndx, file_path, basis, sig, target_size)
        }
        _ => PendingTransfer::new_full_transfer(ndx, file_path, target_size),
    };

    Ok(pending)
}
