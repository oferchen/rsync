//! Delta token processing loop for streaming transfers.
//!
//! Handles buffer recycling, literal-to-buffer conversion, and the main
//! token loop that reads delta tokens from the wire and sends them as
//! `FileMessage` chunks to the disk commit thread.
//!
//! # Buffer Recycling
//!
//! Mirrors upstream rsync's `simple_recv_token` (token.c:284) single-buffer
//! pattern. Buffers flow from the network thread to the disk thread and back
//! through a return channel, avoiding per-chunk allocation.

use std::io::{self, Read};

use engine::signature::FileSignature;

use crate::delta_apply::ChecksumVerifier;
use crate::map_file::MapFile;
use crate::pipeline::messages::FileMessage;
use crate::pipeline::spsc;
use crate::reader::ServerReader;
use crate::token_reader::{DeltaToken, LiteralData, TokenReader};

use super::streaming::StreamingResult;

/// Try to reuse a buffer returned by the disk thread, or allocate a new one.
///
/// Mirrors upstream rsync's `simple_recv_token` (token.c:284) which uses a
/// single static buffer. Here we recycle buffers through a return channel.
#[inline]
pub(super) fn recycle_or_alloc(
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    capacity: usize,
) -> Vec<u8> {
    if let Ok(mut buf) = buf_return_rx.try_recv() {
        buf.clear();
        if buf.capacity() < capacity {
            buf.reserve(capacity - buf.capacity());
        }
        buf
    } else {
        Vec::with_capacity(capacity)
    }
}

/// Converts a [`LiteralData`] to a buffer, reading pending bytes from the stream.
///
/// For `Ready` data (compressed mode), wraps the decompressed data directly.
/// For `Pending` data (plain mode), reads the specified number of bytes from `reader`,
/// using a recycled buffer when available.
pub(super) fn literal_to_buf<R: Read>(
    literal: LiteralData,
    reader: &mut ServerReader<R>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
) -> io::Result<Vec<u8>> {
    match literal {
        LiteralData::Ready(data) => Ok(data),
        LiteralData::Pending(len) => {
            let mut buf = recycle_or_alloc(buf_return_rx, len);
            if let Some(borrowed) = reader.try_borrow_exact(len)? {
                buf.extend_from_slice(borrowed);
            } else {
                let start = buf.len();
                buf.resize(start + len, 0);
                reader.read_exact(&mut buf[start..])?;
            }
            Ok(buf)
        }
    }
}

/// Processes remaining delta tokens after the initial coalescing check.
///
/// If `pending_delta` is `Some`, it is processed first without reading from
/// the wire. Then the regular token loop continues until end-of-file.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_remaining_tokens<R: Read>(
    reader: &mut ServerReader<R>,
    file_tx: &spsc::Sender<FileMessage>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    checksum_verifier: &mut ChecksumVerifier,
    signature: &Option<FileSignature>,
    basis_map: &mut Option<MapFile>,
    mut total_bytes: u64,
    pending_delta: Option<DeltaToken>,
    token_reader: &mut TokenReader,
) -> io::Result<StreamingResult> {
    let send_abort = |tx: &spsc::Sender<FileMessage>, reason: String| {
        let _ = tx.send(FileMessage::Abort { reason });
    };

    let mut next_delta = pending_delta;

    loop {
        let delta = match next_delta.take() {
            Some(d) => d,
            None => match token_reader.read_token(reader) {
                Ok(d) => d,
                Err(e) => {
                    send_abort(file_tx, format!("network read error: {e}"));
                    return Err(e);
                }
            },
        };

        match delta {
            DeltaToken::End => {
                let checksum_len = checksum_verifier.digest_len();
                let mut expected_checksum = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                if let Err(e) = reader.read_exact(&mut expected_checksum[..checksum_len]) {
                    send_abort(file_tx, format!("failed to read checksum: {e}"));
                    return Err(e);
                }

                file_tx.send(FileMessage::Commit).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "disk commit thread disconnected during commit",
                    )
                })?;

                return Ok(StreamingResult {
                    total_bytes,
                    expected_checksum,
                    checksum_len,
                });
            }
            DeltaToken::Literal(literal_data) => {
                let buf = match literal_to_buf(literal_data, reader, buf_return_rx) {
                    Ok(b) => b,
                    Err(e) => {
                        send_abort(file_tx, format!("network read error: {e}"));
                        return Err(e);
                    }
                };
                let len = buf.len();

                file_tx.send(FileMessage::Chunk(buf)).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "disk commit thread disconnected during chunk send",
                    )
                })?;
                total_bytes += len as u64;
            }
            DeltaToken::BlockRef(block_idx) => {
                if let (Some(sig), Some(basis_map)) = (signature, basis_map.as_mut()) {
                    let layout = sig.layout();
                    let block_count = layout.block_count() as usize;

                    if block_idx >= block_count {
                        let msg = format!(
                            "block index {block_idx} out of bounds (file has {block_count} blocks)"
                        );
                        send_abort(file_tx, msg.clone());
                        return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
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

                    // upstream: token.c:631 - see_deflate_token() keeps the
                    // decompressor dictionary in sync after block matches.
                    token_reader.see_token(block_data)?;

                    let mut buf = recycle_or_alloc(buf_return_rx, bytes_to_copy);
                    buf.extend_from_slice(block_data);
                    file_tx.send(FileMessage::Chunk(buf)).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "disk commit thread disconnected during block send",
                        )
                    })?;
                    total_bytes += bytes_to_copy as u64;
                } else {
                    let msg = format!("block reference {block_idx} without basis file");
                    send_abort(file_tx, msg.clone());
                    return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                }
            }
        }
    }
}
