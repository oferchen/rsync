//! Async (tokio-transfer) twin of the streaming file-response processor.
//!
//! [`process_file_response_streaming_async`] is the `.await` twin of the
//! network-read side of [`process_file_response_streaming`](super::process_file_response_streaming).
//! It reads the sender's echoed header, delta tokens, literal bytes, and trailing
//! whole-file checksum off an [`AsyncRead`](tokio::io::AsyncRead) transport, and
//! streams the reconstructed chunks to the **same** synchronous SPSC channel the
//! sync path uses (`file_tx` / `buf_return_rx`). The background disk-commit thread
//! that drains those channels stays a dedicated `std::thread`: it is a legitimate
//! permanent boundary (it never cooperates with the async runtime, so there is no
//! runtime-cooperation deadlock), so only the wire reads become `.await`.
//!
//! Every non-IO step - the single-chunk coalescing decision, the `see_token`
//! dictionary feed, the basis-block copy via [`MapFile`], the buffer recycling,
//! and the `FileMessage` hand-off - runs through the identical synchronous logic
//! the sync path uses. For the same wire bytes it therefore produces the same
//! sequence of `FileMessage`s and the same [`StreamingResult`] as the sync leaf,
//! independent of how the bytes are chunked across `.await` points.
//!
//! # Upstream Reference
//!
//! - `receiver.c:recv_files()` reads deltas (the sync twin mirrors this)
//! - `receiver.c:receive_data()` applies delta tokens
//! - `token.c:284` - `simple_recv_token()` single-buffer literal pattern

use std::io;

use protocol::codec::NdxCodecEnum;
use tokio::io::AsyncReadExt;

use crate::delta_apply::ChecksumVerifier;
use crate::map_file::MapFile;
use crate::pipeline::PendingTransfer;
use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::pipeline::spsc;
use crate::token_reader::{DeltaToken, LiteralData, TokenReader};

use super::streaming::StreamingResult;
use super::token_loop::recycle_or_alloc;
use super::{ResponseContext, read_response_header_async};

/// Async twin of [`literal_to_buf`](super::token_loop::literal_to_buf).
///
/// For `Ready` data (compressed mode) wraps the decompressed bytes directly.
/// For `Pending` data (plain mode) reads exactly `len` bytes off the async
/// transport into a recycled buffer. Unlike the sync leaf there is no
/// zero-copy `try_borrow_exact` fast path (the async transport is a plain
/// [`AsyncRead`], not a buffered `ServerReader`), but the produced bytes are
/// identical.
async fn literal_to_buf_async<R>(
    literal: LiteralData,
    reader: &mut R,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
) -> io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    match literal {
        LiteralData::Ready(data) => Ok(data),
        LiteralData::Pending(len) => {
            let mut buf = recycle_or_alloc(buf_return_rx, len);
            let start = buf.len();
            buf.resize(start + len, 0);
            reader.read_exact(&mut buf[start..]).await?;
            Ok(buf)
        }
    }
}

/// Async twin of [`process_file_response_streaming`](super::process_file_response_streaming).
///
/// Reads the echoed header, delta tokens, and trailing checksum off `reader`
/// via `.await` and streams the reconstructed chunks to the synchronous disk
/// thread through `file_tx`. Deferred-checksum verification, single-chunk
/// coalescing, buffer recycling, and the basis-block copy all match the sync
/// leaf exactly.
///
/// # Arguments
///
/// See [`process_file_response_streaming`](super::process_file_response_streaming);
/// the only difference is `reader` is an [`AsyncRead`](tokio::io::AsyncRead)
/// transport and `ndx_codec` is a concrete [`NdxCodecEnum`] (the type the async
/// NDX reader needs).
///
/// # Upstream Reference
///
/// - `receiver.c:recv_files()` reads deltas
#[allow(clippy::too_many_arguments)]
pub async fn process_file_response_streaming_async<R>(
    reader: &mut R,
    ndx_codec: &mut NdxCodecEnum,
    pending: PendingTransfer,
    ctx: &ResponseContext<'_>,
    checksum_verifier: &mut ChecksumVerifier,
    file_tx: &spsc::Sender<FileMessage>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    file_entry_index: usize,
    is_device_target: bool,
    xattr_list: Option<protocol::xattr::XattrList>,
    token_reader: &mut TokenReader,
) -> io::Result<StreamingResult>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    let header = read_response_header_async(reader, ndx_codec, pending, ctx).await?;

    // upstream: xattrs.c:744-755 - apply abbreviated values from sender to xattr list
    let xattr_list = if !header.xattr_values.is_empty() {
        let mut list = xattr_list.unwrap_or_default();
        crate::receiver::apply_xattr_abbreviation_values(&mut list, &header.xattr_values);
        Some(list)
    } else {
        xattr_list
    };

    // Move the checksum verifier to the disk thread so hashing overlaps with
    // I/O and the network task can focus solely on reading the wire.
    let algo = checksum_verifier.algorithm();
    // upstream: checksum.c:sum_init() prepends seed for legacy MD4 (proto < 30).
    // The replacement verifier must also be seeded for the next file.
    let disk_verifier = std::mem::replace(
        checksum_verifier,
        ChecksumVerifier::for_algorithm_seeded(algo, ctx.config.checksum_seed),
    );

    // Defer sending Begin - allows coalescing single-chunk files into a single
    // WholeFile message (3 channel sends -> 1, reducing futex overhead).
    let begin_msg = Box::new(BeginMessage {
        file_path: header.file_path,
        target_size: header.target_size,
        file_entry_index,
        checksum_verifier: Some(disk_verifier),
        is_device_target,
        // upstream: receiver.c:797 - one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR
        is_inplace: header.use_inplace,
        append_offset: header.append_offset,
        xattr_list,
    });

    let mut basis_map = if let Some(ref path) = header.basis_path {
        Some(MapFile::open(path).map_err(|e| {
            io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
        })?)
    } else {
        None
    };

    let mut total_bytes: u64 = 0;

    // upstream: token.c:807-810 - reset per-file token state. For zstd the
    // decompression context is preserved (single continuous stream across all
    // files); for zlib it reinitializes the inflate context (per-file streams).
    token_reader.reset();

    // Read the first token to determine if this is a single-chunk file.
    let first_delta = token_reader.read_token_async(reader).await?;

    // Try single-chunk coalescing: if the first token is a literal and the next
    // token is end-of-file, send one WholeFile message instead of
    // Begin + Chunk + Commit (3 sends -> 1).
    match first_delta {
        DeltaToken::Literal(literal_data) if basis_map.is_none() => {
            let buf = literal_to_buf_async(literal_data, reader, buf_return_rx).await?;
            let len = buf.len();

            let next_delta = token_reader.read_token_async(reader).await?;

            if matches!(next_delta, DeltaToken::End) {
                total_bytes = len as u64;
                let checksum_len = checksum_verifier.digest_len();
                let mut expected_checksum = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader
                    .read_exact(&mut expected_checksum[..checksum_len])
                    .await?;

                file_tx
                    .send(FileMessage::WholeFile {
                        begin: begin_msg,
                        data: buf,
                    })
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
                    })?;

                return Ok(StreamingResult {
                    total_bytes,
                    literal_bytes: total_bytes,
                    matched_bytes: 0,
                    expected_checksum,
                    checksum_len,
                });
            }

            // Not a single-chunk file - send Begin + first Chunk, then continue
            // the regular loop starting with the peeked token.
            file_tx.send(FileMessage::Begin(begin_msg)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
            })?;
            file_tx.send(FileMessage::Chunk(buf)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "disk commit thread disconnected during chunk send",
                )
            })?;
            total_bytes = len as u64;

            process_remaining_tokens_async(
                reader,
                file_tx,
                buf_return_rx,
                checksum_verifier,
                &header.signature,
                &mut basis_map,
                total_bytes,
                Some(next_delta),
                token_reader,
                total_bytes, // initial literal bytes from first chunk
            )
            .await
        }
        first_delta => {
            // First token was not a simple literal - send Begin and process normally.
            file_tx.send(FileMessage::Begin(begin_msg)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
            })?;

            process_remaining_tokens_async(
                reader,
                file_tx,
                buf_return_rx,
                checksum_verifier,
                &header.signature,
                &mut basis_map,
                total_bytes,
                Some(first_delta),
                token_reader,
                0,
            )
            .await
        }
    }
}

/// Async twin of [`process_remaining_tokens`](super::token_loop::process_remaining_tokens).
///
/// Reads the remaining delta tokens off the async transport (`.await`) and
/// forwards them as `FileMessage`s to the synchronous disk thread. The
/// basis-block copy (via [`MapFile::map_ptr`]), the `see_token` dictionary feed,
/// buffer recycling, and the abort-on-error hand-off are the identical
/// synchronous logic the sync leaf runs.
#[allow(clippy::too_many_arguments)]
async fn process_remaining_tokens_async<R>(
    reader: &mut R,
    file_tx: &spsc::Sender<FileMessage>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    checksum_verifier: &mut ChecksumVerifier,
    signature: &Option<engine::signature::FileSignature>,
    basis_map: &mut Option<MapFile>,
    mut total_bytes: u64,
    pending_delta: Option<DeltaToken>,
    token_reader: &mut TokenReader,
    initial_literal_bytes: u64,
) -> io::Result<StreamingResult>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    let send_abort = |tx: &spsc::Sender<FileMessage>, reason: String| {
        let _ = tx.send(FileMessage::Abort { reason });
    };

    let mut literal_bytes: u64 = initial_literal_bytes;
    let mut matched_bytes: u64 = 0;
    let mut next_delta = pending_delta;

    loop {
        let delta = match next_delta.take() {
            Some(d) => d,
            None => match token_reader.read_token_async(reader).await {
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
                if let Err(e) = reader
                    .read_exact(&mut expected_checksum[..checksum_len])
                    .await
                {
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
                    literal_bytes,
                    matched_bytes,
                    expected_checksum,
                    checksum_len,
                });
            }
            DeltaToken::Literal(literal_data) => {
                let buf = match literal_to_buf_async(literal_data, reader, buf_return_rx).await {
                    Ok(b) => b,
                    Err(e) => {
                        send_abort(file_tx, format!("network read error: {e}"));
                        return Err(e);
                    }
                };
                let len = buf.len() as u64;

                file_tx.send(FileMessage::Chunk(buf)).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "disk commit thread disconnected during chunk send",
                    )
                })?;
                total_bytes += len;
                literal_bytes += len;
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
                    let copy_len = bytes_to_copy as u64;
                    total_bytes += copy_len;
                    matched_bytes += copy_len;
                } else {
                    let msg = format!("block reference {block_idx} without basis file");
                    send_abort(file_tx, msg.clone());
                    return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                }
            }
        }
    }
}
