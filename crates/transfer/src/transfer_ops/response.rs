//! Synchronous file transfer response processing.
//!
//! Reads echoed attributes and delta tokens from the sender, applies them
//! directly to disk, and verifies the whole-file checksum. Used for
//! non-pipelined transfers where the receiver processes one file at a time.
//!
//! # Upstream Reference
//!
//! - `receiver.c:recv_files()` reads deltas
//! - `receiver.c:receive_data()` applies delta tokens

use std::fs;
use std::io::{self, Read, Write};

use protocol::codec::NdxCodec;

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use crate::pipeline::PendingTransfer;
use crate::reader::ServerReader;
use crate::temp_guard::open_tmpfile;
use crate::token_buffer::TokenBuffer;
use crate::token_reader::{DeltaToken, LiteralData};

use super::{ResponseContext, read_response_header};

/// Processes a file transfer response from the sender.
///
/// Reads echoed attributes, delta tokens, and applies them to create the file.
/// Returns the number of bytes received for this file.
///
/// The caller provides reusable `checksum_verifier` and `token_buffer` to avoid
/// per-file allocation overhead. The verifier is reset internally via
/// `mem::replace` before checksum finalization.
///
/// # Arguments
///
/// * `reader` - Input stream from sender
/// * `ndx_codec` - NDX decoder (maintains delta decoding state)
/// * `pending` - The pending transfer to process
/// * `ctx` - Response processing context
/// * `checksum_verifier` - Reusable checksum verifier (reset per call)
/// * `token_buffer` - Reusable token buffer for cross-frame literal tokens
///
/// # Returns
///
/// Number of bytes written to the destination file.
///
/// # Upstream Reference
///
/// - `receiver.c:recv_files()` reads deltas
/// - `receiver.c:receive_data()` applies delta tokens
#[allow(clippy::too_many_arguments)]
pub fn process_file_response<R: Read>(
    reader: &mut ServerReader<R>,
    ndx_codec: &mut impl NdxCodec,
    pending: PendingTransfer,
    ctx: &ResponseContext<'_>,
    checksum_verifier: &mut ChecksumVerifier,
    token_buffer: &mut TokenBuffer,
) -> io::Result<u64> {
    let header = read_response_header(reader, ndx_codec, pending, ctx)?;
    let file_path = header.file_path;
    let basis_path = header.basis_path;
    let signature = header.signature;
    let target_size = header.target_size;
    let use_inplace = header.use_inplace;

    // Inplace: write directly to destination. Otherwise temp+rename for atomicity.
    let (file, mut cleanup_guard, needs_rename) = if use_inplace {
        // upstream: receiver.c:855 - do_open(fname, O_WRONLY|O_CREAT, 0600)
        let f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&file_path)?;
        (
            f,
            crate::temp_guard::TempFileGuard::new(file_path.clone()),
            false,
        )
    } else {
        let (f, guard) = open_tmpfile(&file_path, ctx.config.temp_dir)?;
        (f, guard, true)
    };

    // Use io_uring when available (Linux 5.6+), falling back to BufWriter.
    // Buffer capacity is adaptive based on file size:
    // - Small files (< 64KB): 4KB buffer to avoid wasted memory
    // - Medium files (64KB - 1MB): 64KB buffer for balanced performance
    // - Large files (> 1MB): 256KB buffer to maximize throughput
    let writer_capacity = adaptive_writer_capacity(target_size);
    let mut output =
        fast_io::writer_from_file(file, writer_capacity, ctx.config.io_uring_policy)?;
    let mut total_bytes: u64 = 0;

    // Sparse file support
    let mut sparse_state = if ctx.config.use_sparse {
        Some(SparseWriteState::default())
    } else {
        None
    };

    // Open basis file if delta transfer
    let mut basis_map = if let Some(ref path) = basis_path {
        Some(MapFile::open(path).map_err(|e| {
            io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
        })?)
    } else {
        None
    };

    // Read and apply delta tokens.
    // TokenReader handles both plain (4-byte LE) and compressed (flag-byte)
    // token formats transparently, matching upstream token.c:recv_token().
    let mut token_reader = ctx.config.token_reader();

    loop {
        match token_reader.read_token(reader)? {
            DeltaToken::End => {
                // End of file - verify checksum using stack buffers.
                // Use mem::replace to consume the verifier for finalization while
                // resetting it for the next file (avoids per-file construction).
                let checksum_len = checksum_verifier.digest_len();
                let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut expected[..checksum_len])?;

                let algo = checksum_verifier.algorithm();
                let old_verifier =
                    std::mem::replace(checksum_verifier, ChecksumVerifier::for_algorithm(algo));
                let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                let computed_len = old_verifier.finalize_into(&mut computed);
                if computed_len != checksum_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "checksum length mismatch for {file_path:?}: expected {checksum_len}, got {computed_len}",
                        ),
                    ));
                }
                if computed[..computed_len] != expected[..checksum_len] {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "checksum verification failed for {file_path:?}: expected {:02x?}, got {:02x?}",
                            &expected[..checksum_len],
                            &computed[..computed_len]
                        ),
                    ));
                }
                break;
            }
            DeltaToken::Literal(literal) => {
                // Write literal data to output and update checksum.
                // LiteralData::Ready = decompressed data from compressed stream.
                // LiteralData::Pending = plain mode, caller reads data from stream.
                match literal {
                    LiteralData::Ready(data) => {
                        let len = data.len();
                        if let Some(ref mut sparse) = sparse_state {
                            sparse.write(&mut output, &data)?;
                        } else {
                            output.write_all(&data)?;
                        }
                        checksum_verifier.update(&data);
                        total_bytes += len as u64;
                    }
                    LiteralData::Pending(len) => {
                        // Plain token: read data from stream.
                        if let Some(data) = reader.try_borrow_exact(len)? {
                            if let Some(ref mut sparse) = sparse_state {
                                sparse.write(&mut output, data)?;
                            } else {
                                output.write_all(data)?;
                            }
                            checksum_verifier.update(data);
                        } else {
                            token_buffer.resize_for(len);
                            reader.read_exact(token_buffer.as_mut_slice())?;
                            let data = token_buffer.as_slice();
                            if let Some(ref mut sparse) = sparse_state {
                                sparse.write(&mut output, data)?;
                            } else {
                                output.write_all(data)?;
                            }
                            checksum_verifier.update(data);
                        }
                        total_bytes += len as u64;
                    }
                }
            }
            DeltaToken::BlockRef(block_idx) => {
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

                    // upstream: token.c:631 - see_deflate_token() keeps the
                    // decompressor dictionary in sync after block matches.
                    token_reader.see_token(block_data)?;

                    total_bytes += bytes_to_copy as u64;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("block reference {block_idx} without basis file"),
                    ));
                }
            }
        }
    }

    // Finalize sparse writing if active
    if let Some(ref mut sparse) = sparse_state {
        let _final_pos = sparse.finish(&mut output)?;
    }

    // Flush and optionally sync (uses io_uring fsync op when available)
    if ctx.config.do_fsync {
        output.sync().map_err(|e| {
            io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}"))
        })?;
    } else {
        output.flush().map_err(|e| {
            io::Error::other(format!(
                "failed to flush output buffer for {file_path:?}: {e}"
            ))
        })?;
    }
    drop(output);

    if needs_rename {
        // Atomic rename: temp file to final destination.
        fs::rename(cleanup_guard.path(), &file_path)?;
    } else if ctx.config.inplace {
        // Inplace: truncate to final size.
        // upstream: receiver.c:340 - set_file_length(fd, F_LENGTH(file))
        let file = fs::OpenOptions::new().write(true).open(&file_path)?;
        file.set_len(total_bytes)?;
    }
    cleanup_guard.keep();

    Ok(total_bytes)
}
