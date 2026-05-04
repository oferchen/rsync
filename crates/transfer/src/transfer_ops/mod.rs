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
//! # Submodules
//!
//! - [`request`] - Sends file transfer requests (NDX + iflags + signature) to the sender.
//! - [`response`] - Synchronous response processing with direct disk I/O.
//! - [`streaming`] - Streaming response processing via SPSC channel to a disk commit thread.
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

mod request;
mod response;
mod streaming;
mod token_loop;

use std::io::{self, Read};
use std::num::NonZeroU8;
use std::path::Path;

use protocol::ProtocolVersion;

use crate::reader::ServerReader;
use crate::receiver::{SenderAttrs, SumHead};

pub use self::request::{send_file_request, send_file_request_xattr};
pub use self::response::process_file_response;
pub use self::streaming::{StreamingResult, process_file_response_streaming};
pub use crate::token_reader::TokenReader;

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
    /// Temporary directory for staging received files before final placement.
    pub temp_dir: Option<&'a Path>,
    /// Whether to write data directly to device files (`--write-devices`).
    ///
    /// When true, device file targets are opened with `O_WRONLY` and receive
    /// delta data like regular files. Implies inplace for device targets
    /// (no temp file + rename).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c`: `write_devices && IS_DEVICE(st.st_mode)` - open device for writing
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
    /// Per-file inplace for partial-dir basis files (CF_INPLACE_PARTIAL_DIR).
    ///
    /// When true, files whose basis type is `PartialDir` are written in-place.
    /// Combined with `fnamecmp_type` from sender attrs to make per-file decisions.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:797`: `one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR`
    pub inplace_partial: bool,
    /// Policy controlling io_uring usage for file I/O (`--io-uring` / `--no-io-uring`).
    pub io_uring_policy: fast_io::IoUringPolicy,
    /// Whether xattr preservation is active (`-X` / `--xattrs`).
    ///
    /// When true, the sender may include `ITEM_REPORT_XATTR` in iflags with
    /// abbreviated xattr value data following the standard attributes.
    pub preserve_xattrs: bool,
    /// Whether xattr hardlink optimization is active (`CF_AVOID_XATTR_OPTIM`).
    ///
    /// When true and the item has both `ITEM_XNAME_FOLLOWS` and
    /// `ITEM_LOCAL_CHANGE`, the xattr exchange is skipped for that item.
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c` - `want_xattr_optim` set from capability negotiation
    pub want_xattr_optim: bool,
    /// Whether append mode is active (`--append`).
    ///
    /// When true, signature blocks are NOT written after sum_head. The sender
    /// uses sum_head fields to calculate existing file length and only sends
    /// data appended beyond that point.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:775-776` - generator skips writing signature blocks in append mode
    /// - `sender.c:87-92` - `receive_sums()` returns early without reading blocks
    pub append: bool,
}

impl RequestConfig<'_> {
    /// Creates a [`TokenReader`] matching the negotiated compression algorithm.
    ///
    /// Returns a compressed token reader when the negotiated algorithms include
    /// a supported compression algorithm, otherwise a plain 4-byte LE token reader.
    ///
    /// The returned reader must be reused across all files in a transfer session.
    /// For zstd, upstream rsync uses a single continuous stream - the decompression
    /// context must persist across file boundaries. Call [`TokenReader::reset()`]
    /// between files to reset per-file state while preserving the stream context.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:271` - `recv_token()` selects plain or compressed based on `-z`
    /// - `token.c:807-810` - zstd `r_init` only resets `rx_token`, not the DCtx
    pub fn create_token_reader(&self) -> TokenReader {
        let compression = self.negotiated_algorithms.map(|n| n.compression);
        TokenReader::new(compression)
    }
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

/// Reads and validates the echoed NDX and sum_head from the sender response.
///
/// Returns the file path, basis path, signature, target size, sender attributes,
/// and whether inplace mode applies for this file.
///
/// # Errors
///
/// Returns an error if the echoed NDX does not match the expected value.
fn read_response_header<R: Read>(
    reader: &mut ServerReader<R>,
    ndx_codec: &mut impl protocol::codec::NdxCodec,
    pending: crate::pipeline::PendingTransfer,
    ctx: &ResponseContext<'_>,
) -> io::Result<ResponseHeader> {
    let expected_ndx = pending.ndx();

    let (echoed_ndx, sender_attrs) = SenderAttrs::read_with_codec_xattr(
        reader,
        ndx_codec,
        ctx.config.preserve_xattrs,
        ctx.config.want_xattr_optim,
    )?;

    // Protocol requires in-order responses.
    if echoed_ndx != expected_ndx {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "sender echoed NDX {echoed_ndx} but expected {expected_ndx} - protocol violation"
            ),
        ));
    }

    // Echoed sum_head provides the existing file length for append mode offset.
    let echoed_sum_head = SumHead::read(reader)?;

    let (file_path, basis_path, signature, target_size) = pending.into_parts();

    // upstream: receiver.c:797 - one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR
    // upstream: receiver.c:855 - append mode implies inplace (write directly to destination,
    // preserving existing content; sender only sends data beyond the existing length)
    let use_inplace = ctx.config.inplace
        || ctx.config.append
        || (ctx.config.inplace_partial
            && sender_attrs.fnamecmp_type == Some(protocol::FnameCmpType::PartialDir));

    // upstream: receiver.c:287-307 - in append mode, seek output fd to existing file length
    // (derived from echoed sum_head) before writing new data
    let append_offset = if ctx.config.append {
        echoed_sum_head.flength()
    } else {
        0
    };

    Ok(ResponseHeader {
        file_path,
        basis_path,
        signature,
        target_size,
        use_inplace,
        append_offset,
        xattr_values: sender_attrs.xattr_values,
    })
}

/// Parsed response header from the sender after NDX and sum_head validation.
struct ResponseHeader {
    /// Destination file path.
    file_path: std::path::PathBuf,
    /// Optional basis file path for delta transfers.
    basis_path: Option<std::path::PathBuf>,
    /// Optional file signature for delta matching.
    signature: Option<engine::signature::FileSignature>,
    /// Expected final file size.
    target_size: u64,
    /// Whether to write directly to the destination (inplace mode).
    use_inplace: bool,
    /// Byte offset at which to start writing in append mode.
    ///
    /// Derived from the echoed sum_head: existing file length that the sender
    /// skipped. Zero when not in append mode.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:307-308` - `offset = sum.flength; do_lseek(fd, offset, SEEK_SET)`
    append_offset: u64,
    /// Abbreviated xattr values from the sender (1-based num, value pairs).
    ///
    /// Non-empty only when the sender included `ITEM_REPORT_XATTR` in iflags.
    xattr_values: Vec<(i32, Vec<u8>)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_config_debug() {
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
            temp_dir: None,
            write_devices: false,
            inplace: false,
            inplace_partial: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            preserve_xattrs: false,
            want_xattr_optim: false,
            append: false,
        };
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("RequestConfig"));
    }
}
