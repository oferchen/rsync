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
use protocol::ProtocolVersion;
use protocol::codec::NdxCodec;

use crate::pipeline::spsc;

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use protocol::flist::FileEntry;

use crate::pipeline::PendingTransfer;
use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::reader::ServerReader;
use crate::receiver::{SenderAttrs, SumHead, write_signature_blocks};
use crate::temp_guard::open_tmpfile;
use crate::token_buffer::TokenBuffer;
use fast_io::FileWriter;

/// Configuration for sending file transfer requests and processing responses.
///
/// Groups protocol version, checksum parameters, and write options into a
/// single struct shared between [`send_file_request`] and [`process_file_response`].
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
    /// Whether to write data directly to device files (`--write-devices`).
    ///
    /// When true, device file targets are opened with `O_WRONLY` and receive
    /// delta data like regular files. Implies inplace for device targets
    /// (no temp file + rename).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c`: `write_devices && IS_DEVICE(st.st_mode)` — open device for writing
    pub write_devices: bool,
    /// Update destination files in place without temp-file + rename (`--inplace`).
    ///
    /// When true, delta data is written directly to the destination file.
    /// The destination file is opened for writing (create if needed) and
    /// truncated to the target size after delta application completes.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:855-860`: opens destination directly when inplace
    pub inplace: bool,
    /// Policy controlling io_uring usage for file I/O (`--io-uring` / `--no-io-uring`).
    pub io_uring_policy: fast_io::IoUringPolicy,
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

/// Context for processing a file transfer response from the sender.
///
/// Wraps a [`RequestConfig`] reference so that [`process_file_response`] can
/// access protocol parameters, checksum settings, and sparse-write options
/// without requiring them as individual function arguments.
pub struct ResponseContext<'a> {
    /// Protocol and checksum configuration shared with the request phase.
    pub config: &'a RequestConfig<'a>,
}

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

    // Inplace: write directly to destination. Otherwise temp+rename for atomicity.
    let (file, mut cleanup_guard, needs_rename) = if ctx.config.inplace {
        // upstream: receiver.c:855 — do_open(fname, O_WRONLY|O_CREAT, 0600)
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
        let (f, guard) = open_tmpfile(&file_path, None)?;
        (f, guard, true)
    };

    // Use io_uring when available (Linux 5.6+), falling back to BufWriter.
    // Buffer capacity is adaptive based on file size:
    // - Small files (< 64KB): 4KB buffer to avoid wasted memory
    // - Medium files (64KB - 1MB): 64KB buffer for balanced performance
    // - Large files (> 1MB): 256KB buffer to maximize throughput
    let writer_capacity = adaptive_writer_capacity(target_size);
    let mut output = fast_io::writer_from_file(file, writer_capacity, ctx.config.io_uring_policy)?;
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

    // Read and apply delta tokens
    loop {
        let mut token_buf = [0u8; 4];
        reader.read_exact(&mut token_buf)?;
        let token = i32::from_le_bytes(token_buf);

        if token == 0 {
            // End of file — verify checksum using stack buffers.
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
        } else if token > 0 {
            // Literal data — try zero-copy from the multiplex frame buffer,
            // falling back to TokenBuffer when the token spans frame boundaries.
            let len = token as usize;

            if let Some(data) = reader.try_borrow_exact(len)? {
                // Zero-copy path: data borrowed directly from MultiplexReader buffer
                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(&mut output, data)?;
                } else {
                    output.write_all(data)?;
                }
                checksum_verifier.update(data);
            } else {
                // Fallback: token spans frame boundary, copy into TokenBuffer
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
        // upstream: receiver.c:340 — set_file_length(fd, F_LENGTH(file))
        let file = fs::OpenOptions::new().write(true).open(&file_path)?;
        file.set_len(total_bytes)?;
    }
    cleanup_guard.keep();

    Ok(total_bytes)
}

/// Try to reuse a buffer returned by the disk thread, or allocate a new one.
///
/// Mirrors upstream rsync's `simple_recv_token` (token.c:284) which uses a
/// single static buffer. Here we recycle buffers through a return channel.
#[inline]
fn recycle_or_alloc(buf_return_rx: &spsc::Receiver<Vec<u8>>, capacity: usize) -> Vec<u8> {
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

/// Result of streaming a file response to the disk thread.
pub struct StreamingResult {
    /// Total bytes of file data read from the wire.
    pub total_bytes: u64,
    /// Expected whole-file checksum read from the sender (for deferred verification).
    pub expected_checksum: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    /// Number of valid bytes in `expected_checksum`.
    pub checksum_len: usize,
}

/// Processes a file transfer response, streaming chunks to the disk thread.
///
/// Like [`process_file_response`], reads echoed attributes and delta tokens —
/// but instead of writing to disk directly, sends `FileMessage::Chunk` items
/// through `file_tx` for the disk commit thread.
///
/// Checksum computation is deferred to the disk thread. The expected checksum
/// read from the wire is returned in [`StreamingResult`] for the caller to
/// pass to [`crate::pipeline::receiver::PipelinedReceiver::note_commit_sent`].
///
/// # Arguments
///
/// * `reader` - Input stream from sender
/// * `ndx_codec` - NDX decoder (maintains delta decoding state)
/// * `pending` - The pending transfer to process
/// * `ctx` - Response processing context
/// * `checksum_verifier` - Reusable checksum verifier (reset per call)
/// * `file_tx` - Channel sender to the disk commit thread
/// * `buf_return_rx` - Return channel for recycled buffers from the disk thread
/// * `file_entry_index` - Index into the file list for metadata application
/// * `file_entry` - File entry for metadata application on the disk thread
#[allow(clippy::too_many_arguments)]
pub fn process_file_response_streaming<R: Read>(
    reader: &mut ServerReader<R>,
    ndx_codec: &mut impl NdxCodec,
    pending: PendingTransfer,
    ctx: &ResponseContext<'_>,
    checksum_verifier: &mut ChecksumVerifier,
    file_tx: &spsc::Sender<FileMessage>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    file_entry_index: usize,
    file_entry: Option<FileEntry>,
) -> io::Result<StreamingResult> {
    let expected_ndx = pending.ndx();

    // Read sender attributes (echoed NDX + iflags)
    let (echoed_ndx, _sender_attrs) = SenderAttrs::read_with_codec(reader, ndx_codec)?;

    if echoed_ndx != expected_ndx {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "sender echoed NDX {echoed_ndx} but expected {expected_ndx} - protocol violation"
            ),
        ));
    }

    // Consume echoed sum_head
    let _echoed_sum_head = SumHead::read(reader)?;

    let (file_path, basis_path, signature, target_size) = pending.into_parts();

    // Move the checksum verifier to the disk thread so hashing overlaps with
    // I/O and the network thread can focus solely on reading the wire.
    let algo = checksum_verifier.algorithm();
    let disk_verifier = std::mem::replace(checksum_verifier, ChecksumVerifier::for_algorithm(algo));

    // Defer sending Begin — allows coalescing single-chunk files into a
    // single WholeFile message (3 channel sends → 1, reducing futex overhead).
    let is_device_target =
        ctx.config.write_devices && file_entry.as_ref().is_some_and(|e| e.is_device());
    let begin_msg = Box::new(BeginMessage {
        file_path: file_path.clone(),
        target_size,
        file_entry_index,
        use_sparse: ctx.config.use_sparse,
        checksum_verifier: Some(disk_verifier),
        file_entry,
        is_device_target,
        is_inplace: ctx.config.inplace,
    });

    // Open basis file for block references
    let mut basis_map = if let Some(ref path) = basis_path {
        Some(MapFile::open(path).map_err(|e| {
            io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
        })?)
    } else {
        None
    };

    let mut total_bytes: u64 = 0;

    // Read the first token to determine if this is a single-chunk file.
    let mut token_buf = [0u8; 4];
    reader.read_exact(&mut token_buf)?;
    let first_token = i32::from_le_bytes(token_buf);

    // Try single-chunk coalescing: if the first token is a literal and the
    // next token is end-of-file (0), send one WholeFile message instead of
    // Begin + Chunk + Commit (3 sends → 1).
    if first_token > 0 && basis_map.is_none() {
        let len = first_token as usize;
        let mut buf = recycle_or_alloc(buf_return_rx, len);

        if let Some(borrowed) = reader.try_borrow_exact(len)? {
            buf.extend_from_slice(borrowed);
        } else {
            let start = buf.len();
            buf.resize(start + len, 0);
            reader.read_exact(&mut buf[start..])?;
        }

        // Peek at the next token.
        reader.read_exact(&mut token_buf)?;
        let next_token = i32::from_le_bytes(token_buf);

        if next_token == 0 {
            // Single-chunk file — coalesce into WholeFile.
            total_bytes = len as u64;
            let checksum_len = checksum_verifier.digest_len();
            let mut expected_checksum = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
            reader.read_exact(&mut expected_checksum[..checksum_len])?;

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
                expected_checksum,
                checksum_len,
            });
        }

        // Not a single-chunk file — fall through: send Begin + first Chunk,
        // then continue the regular loop starting with the peeked token.
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

        // Process the peeked token as the current token in the loop below.
        // We set `pending_token` so the loop body processes it first.
        return process_remaining_tokens(
            reader,
            file_tx,
            buf_return_rx,
            checksum_verifier,
            &signature,
            &mut basis_map,
            total_bytes,
            Some(next_token),
        );
    }

    // First token was not a simple literal — send Begin and process normally.
    file_tx.send(FileMessage::Begin(begin_msg)).map_err(|_| {
        io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
    })?;

    process_remaining_tokens(
        reader,
        file_tx,
        buf_return_rx,
        checksum_verifier,
        &signature,
        &mut basis_map,
        total_bytes,
        Some(first_token),
    )
}

/// Processes remaining delta tokens after the initial coalescing check.
///
/// If `pending_token` is `Some`, it is processed first without reading from
/// the wire. Then the regular token loop continues until end-of-file (token 0).
#[allow(clippy::too_many_arguments)]
fn process_remaining_tokens<R: Read>(
    reader: &mut ServerReader<R>,
    file_tx: &spsc::Sender<FileMessage>,
    buf_return_rx: &spsc::Receiver<Vec<u8>>,
    checksum_verifier: &mut ChecksumVerifier,
    signature: &Option<FileSignature>,
    basis_map: &mut Option<MapFile>,
    mut total_bytes: u64,
    pending_token: Option<i32>,
) -> io::Result<StreamingResult> {
    let send_abort = |tx: &spsc::Sender<FileMessage>, reason: String| {
        let _ = tx.send(FileMessage::Abort { reason });
    };

    let mut next_token = pending_token;

    loop {
        let token = match next_token.take() {
            Some(t) => t,
            None => {
                let mut token_buf = [0u8; 4];
                if let Err(e) = reader.read_exact(&mut token_buf) {
                    send_abort(file_tx, format!("network read error: {e}"));
                    return Err(e);
                }
                i32::from_le_bytes(token_buf)
            }
        };

        if token == 0 {
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
        } else if token > 0 {
            let len = token as usize;
            let mut buf = recycle_or_alloc(buf_return_rx, len);

            if let Some(borrowed) = reader.try_borrow_exact(len)? {
                buf.extend_from_slice(borrowed);
            } else {
                let start = buf.len();
                buf.resize(start + len, 0);
                reader.read_exact(&mut buf[start..])?;
            };

            file_tx.send(FileMessage::Chunk(buf)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "disk commit thread disconnected during chunk send",
                )
            })?;
            total_bytes += len as u64;
        } else {
            let block_idx = -(token + 1) as usize;
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
            write_devices: false,
            inplace: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
        };
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("RequestConfig"));
    }
}
