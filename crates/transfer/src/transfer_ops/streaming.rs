//! Streaming file transfer response processing via SPSC channel.
//!
//! Reads delta tokens from the sender and streams them as `FileMessage` chunks
//! to a background disk commit thread, decoupling network I/O from disk writes.
//! Supports single-chunk coalescing to reduce channel overhead for small files.
//!
//! # Upstream Reference
//!
//! - `receiver.c:recv_files()` reads deltas
//! - `receiver.c:receive_data()` applies delta tokens

use std::io::{self, Read};

use protocol::codec::NdxCodec;

use crate::delta_apply::ChecksumVerifier;
use crate::map_file::MapFile;
use crate::pipeline::PendingTransfer;
use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::pipeline::spsc;
use crate::reader::ServerReader;
use crate::token_reader::{DeltaToken, TokenReader};

use super::token_loop::{literal_to_buf, process_remaining_tokens};
use super::{ResponseContext, read_response_header};

/// Result of streaming a file response to the disk thread.
pub struct StreamingResult {
    /// Total bytes of file data read from the wire.
    pub total_bytes: u64,
    /// Literal (new) data bytes for this file.
    ///
    /// Data from `DeltaToken::Literal` tokens that did not match any block
    /// in the basis file. Accumulated during token processing.
    pub literal_bytes: u64,
    /// Matched (reused) data bytes for this file.
    ///
    /// Data from `DeltaToken::BlockRef` tokens that reference basis file blocks.
    /// Accumulated during token processing.
    pub matched_bytes: u64,
    /// Expected whole-file checksum read from the sender (for deferred verification).
    pub expected_checksum: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    /// Number of valid bytes in `expected_checksum`.
    pub checksum_len: usize,
}

/// Processes a file transfer response, streaming chunks to the disk thread.
///
/// Like [`super::process_file_response`], reads echoed attributes and delta tokens -
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
/// * `is_device_target` - Whether the target is a device file
/// * `xattr_list` - Optional extended attribute list for metadata application
/// * `token_reader` - Reusable token reader, shared across files in a session.
///   For zstd, the decompression context must be preserved across files because
///   upstream rsync uses a single continuous zstd stream for the entire session.
///   The caller must call `token_reader.reset()` between files.
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
    is_device_target: bool,
    xattr_list: Option<protocol::xattr::XattrList>,
    token_reader: &mut TokenReader,
) -> io::Result<StreamingResult> {
    let header = read_response_header(reader, ndx_codec, pending, ctx)?;

    // upstream: xattrs.c:744-755 - apply abbreviated values from sender to xattr list
    let xattr_list = if !header.xattr_values.is_empty() {
        let mut list = xattr_list.unwrap_or_default();
        crate::receiver::apply_xattr_abbreviation_values(&mut list, &header.xattr_values);
        Some(list)
    } else {
        xattr_list
    };

    // Move the checksum verifier to the disk thread so hashing overlaps with
    // I/O and the network thread can focus solely on reading the wire.
    let algo = checksum_verifier.algorithm();
    // upstream: checksum.c:sum_init() prepends seed for legacy MD4 (proto < 30).
    // The replacement verifier must also be seeded for the next file.
    let disk_verifier = std::mem::replace(
        checksum_verifier,
        ChecksumVerifier::for_algorithm_seeded(algo, ctx.config.checksum_seed),
    );

    // Defer sending Begin - allows coalescing single-chunk files into a
    // single WholeFile message (3 channel sends -> 1, reducing futex overhead).
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
    let first_delta = token_reader.read_token(reader)?;

    // Try single-chunk coalescing: if the first token is a literal and the
    // next token is end-of-file, send one WholeFile message instead of
    // Begin + Chunk + Commit (3 sends -> 1).
    match first_delta {
        DeltaToken::Literal(literal_data) if basis_map.is_none() => {
            let buf = literal_to_buf(literal_data, reader, buf_return_rx)?;
            let len = buf.len();

            let next_delta = token_reader.read_token(reader)?;

            if matches!(next_delta, DeltaToken::End) {
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
                    literal_bytes: total_bytes,
                    matched_bytes: 0,
                    expected_checksum,
                    checksum_len,
                });
            }

            // Not a single-chunk file - send Begin + first Chunk,
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

            process_remaining_tokens(
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
        }
        first_delta => {
            // First token was not a simple literal - send Begin and process normally.
            file_tx.send(FileMessage::Begin(begin_msg)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
            })?;

            process_remaining_tokens(
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
        }
    }
}
