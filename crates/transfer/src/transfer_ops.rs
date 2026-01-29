//! Transfer operation helpers for separating request and response phases.
//!
//! This module provides helpers to break down the synchronous file transfer loop
//! into distinct request and response phases, enabling pipelined transfers.
//!
//! # Architecture
//!
//! A file transfer consists of two phases:
//!
//! 1. **Request phase**: Send NDX + iflags + sum_head + signature to sender
//! 2. **Response phase**: Read echoed attributes + apply delta + verify checksum
//!
//! By separating these phases, we enable pipelining: sending multiple requests
//! before waiting for responses.
//!
//! # Protocol Flow
//!
//! ```text
//! Receiver (us)                         Sender
//! ─────────────                         ──────
//! NDX + iflags + sum_head ───────────▶
//!                                       Echo NDX + iflags + sum_head
//!                          ◀─────────── Delta tokens
//!                          ◀─────────── File checksum
//! ```
//!
//! # Upstream Reference
//!
//! - `generator.c:recv_generator()` - Sends file indices and signatures
//! - `sender.c:send_files()` - Receives requests, sends deltas
//! - `receiver.c:recv_files()` - Receives deltas, applies them

use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;

use engine::signature::FileSignature;
use protocol::codec::NdxCodec;
use protocol::ProtocolVersion;

use crate::adaptive_buffer::{adaptive_token_capacity, adaptive_writer_capacity};
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use crate::pipeline::PendingTransfer;
use crate::receiver::{SenderAttrs, SumHead, write_signature_blocks};
use crate::temp_guard::TempFileGuard;
use crate::token_buffer::TokenBuffer;

/// Configuration for sending file transfer requests.
#[derive(Debug)]
pub struct RequestConfig<'a> {
    /// Protocol version for encoding.
    pub protocol: ProtocolVersion,
    /// Whether to write iflags (protocol >= 29).
    pub write_iflags: bool,
    /// Checksum truncation length.
    pub checksum_length: NonZeroU8,
    /// Checksum algorithm for verification.
    pub checksum_algorithm: engine::signature::SignatureAlgorithm,
    /// Reference to negotiated algorithms for checksum verification.
    pub negotiated_algorithms: Option<&'a protocol::NegotiationResult>,
    /// Compatibility flags.
    pub compat_flags: Option<&'a protocol::CompatibilityFlags>,
    /// Checksum seed from protocol setup.
    pub checksum_seed: i32,
    /// Whether to use sparse file writing.
    pub use_sparse: bool,
    /// Whether to fsync after write.
    pub do_fsync: bool,
}

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
        const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
        writer.write_all(&ITEM_TRANSFER.to_le_bytes())?;
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
    writer.flush()?;

    // Create pending transfer for response processing
    let pending = match (signature, basis_path) {
        (Some(sig), Some(basis)) => {
            PendingTransfer::new_delta_transfer(ndx, file_path, basis, sig, target_size)
        }
        _ => PendingTransfer::new_full_transfer(ndx, file_path, target_size),
    };

    Ok(pending)
}

/// Context for processing a file transfer response.
pub struct ResponseContext<'a> {
    /// Configuration for the response.
    pub config: &'a RequestConfig<'a>,
}

/// Processes a file transfer response from the sender.
///
/// Reads echoed attributes, delta tokens, and applies them to create the file.
/// Returns the number of bytes received for this file.
///
/// # Arguments
///
/// * `reader` - Input stream from sender
/// * `ndx_codec` - NDX decoder (maintains delta decoding state)
/// * `pending` - The pending transfer to process
/// * `ctx` - Response processing context
///
/// # Returns
///
/// Number of bytes written to the destination file.
///
/// # Upstream Reference
///
/// - `receiver.c:recv_files()` reads deltas
/// - `receiver.c:receive_data()` applies delta tokens
pub fn process_file_response<R: Read>(
    reader: &mut R,
    ndx_codec: &mut impl NdxCodec,
    pending: PendingTransfer,
    ctx: &ResponseContext<'_>,
) -> io::Result<u64> {
    let expected_ndx = pending.ndx();

    // Read sender attributes (echoed NDX + iflags)
    let (echoed_ndx, _sender_attrs) = SenderAttrs::read_with_codec(reader, ndx_codec)?;

    // Verify NDX matches - protocol requires in-order responses
    if echoed_ndx != expected_ndx {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "sender echoed NDX {echoed_ndx} but expected {expected_ndx} - protocol violation"
            ),
        ));
    }

    // Read echoed sum_head (we don't use it, but must consume it)
    let _echoed_sum_head = SumHead::read(reader)?;

    // Decompose pending transfer
    let (file_path, basis_path, signature, target_size) = pending.into_parts();

    // Apply delta to reconstruct file
    let temp_path = file_path.with_extension("oc-rsync.tmp");
    let mut temp_guard = TempFileGuard::new(temp_path.clone());

    // Use BufWriter with adaptive capacity based on file size:
    // - Small files (< 64KB): 4KB buffer to avoid wasted memory
    // - Medium files (64KB - 1MB): 64KB buffer for balanced performance
    // - Large files (> 1MB): 256KB buffer to maximize throughput
    let file = fs::File::create(&temp_path)?;
    let writer_capacity = adaptive_writer_capacity(target_size);
    let mut output = std::io::BufWriter::with_capacity(writer_capacity, file);
    let mut total_bytes: u64 = 0;

    // Sparse file support
    let mut sparse_state = if ctx.config.use_sparse {
        Some(SparseWriteState::default())
    } else {
        None
    };

    // Create checksum verifier
    let mut checksum_verifier = ChecksumVerifier::new(
        ctx.config.negotiated_algorithms,
        ctx.config.protocol,
        ctx.config.checksum_seed,
        ctx.config.compat_flags,
    );

    // Open basis file if delta transfer
    let mut basis_map = if let Some(ref path) = basis_path {
        Some(MapFile::open(path).map_err(|e| {
            io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
        })?)
    } else {
        None
    };

    // Use adaptive token buffer capacity based on file size
    let token_capacity = adaptive_token_capacity(target_size);
    let mut token_buffer = TokenBuffer::with_capacity(token_capacity);

    // Read and apply delta tokens
    loop {
        let mut token_buf = [0u8; 4];
        reader.read_exact(&mut token_buf)?;
        let token = i32::from_le_bytes(token_buf);

        if token == 0 {
            // End of file - verify checksum
            let checksum_len = checksum_verifier.digest_len();
            let mut file_checksum = vec![0u8; checksum_len];
            reader.read_exact(&mut file_checksum)?;

            let computed = checksum_verifier.finalize();
            if computed.len() != file_checksum.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "checksum length mismatch for {file_path:?}: expected {}, got {}",
                        checksum_len,
                        computed.len()
                    ),
                ));
            }
            if computed != file_checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "checksum verification failed for {file_path:?}: expected {file_checksum:02x?}, got {computed:02x?}"
                    ),
                ));
            }
            break;
        } else if token > 0 {
            // Literal data
            let len = token as usize;
            token_buffer.resize_for(len);
            reader.read_exact(token_buffer.as_mut_slice())?;
            let data = token_buffer.as_slice();

            if let Some(ref mut sparse) = sparse_state {
                sparse.write(&mut output, data)?;
            } else {
                output.write_all(data)?;
            }
            checksum_verifier.update(data);
            total_bytes += len as u64;
        } else {
            // Block reference
            let block_idx = -(token + 1) as usize;
            if let (Some(sig), Some(basis_map)) = (&signature, basis_map.as_mut()) {
                let layout = sig.layout();
                let block_count = layout.block_count() as usize;

                if block_idx >= block_count {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "block index {block_idx} out of bounds (file has {block_count} blocks)"
                        ),
                    ));
                }

                let block_len = layout.block_length().get() as u64;
                let offset = block_idx as u64 * block_len;

                let bytes_to_copy = if block_idx == block_count.saturating_sub(1) {
                    let remainder = layout.remainder();
                    if remainder > 0 {
                        remainder as usize
                    } else {
                        block_len as usize
                    }
                } else {
                    block_len as usize
                };

                let block_data = basis_map.map_ptr(offset, bytes_to_copy)?;

                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(&mut output, block_data)?;
                } else {
                    output.write_all(block_data)?;
                }
                checksum_verifier.update(block_data);
                total_bytes += bytes_to_copy as u64;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("block reference {block_idx} without basis file"),
                ));
            }
        }
    }

    // Finalize sparse writing if active
    if let Some(ref mut sparse) = sparse_state {
        let _final_pos = sparse.finish(&mut output)?;
    }

    // Flush and optionally sync
    let file = output.into_inner().map_err(|e| {
        io::Error::other(format!(
            "failed to flush output buffer for {file_path:?}: {e}"
        ))
    })?;
    if ctx.config.do_fsync {
        file.sync_all().map_err(|e| {
            io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}"))
        })?;
    }
    drop(file);

    // Atomic rename
    fs::rename(&temp_path, &file_path)?;
    temp_guard.keep();

    Ok(total_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_config_debug() {
        // Verify RequestConfig is debuggable
        let protocol = ProtocolVersion::from_supported(31).expect("31 is supported");
        let config = RequestConfig {
            protocol,
            write_iflags: true,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: engine::signature::SignatureAlgorithm::Md4,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
            use_sparse: false,
            do_fsync: false,
        };
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("RequestConfig"));
    }
}
