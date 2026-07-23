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
//! - `request` - Sends file transfer requests (NDX + iflags + signature) to the sender.
//! - `response` - Synchronous response processing with direct disk I/O.
//! - `streaming` - Streaming response processing via SPSC channel to a disk commit thread.
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
    /// - `receiver.c:968-984`: opens destination directly when inplace
    pub inplace: bool,
    /// Per-file inplace for partial-dir basis files (CF_INPLACE_PARTIAL_DIR).
    ///
    /// When true, files whose basis type is `PartialDir` are written in-place.
    /// Combined with `fnamecmp_type` from sender attrs to make per-file decisions.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:910`: `one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR`
    pub inplace_partial: bool,
    /// Policy controlling io_uring usage for file I/O (`--io-uring` / `--no-io-uring`).
    pub io_uring_policy: fast_io::IoUringPolicy,
    /// Optional override for the io_uring submission queue depth (`--io-uring-depth=N`).
    ///
    /// `None` keeps the default
    /// ([`fast_io::IoUringConfig::sq_entries`]). `Some(n)` overrides the
    /// default with a power-of-two value previously validated via
    /// [`fast_io::validate_io_uring_depth`].
    pub io_uring_depth: Option<u32>,
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
    /// - `generator.c:787-788` - generator skips writing signature blocks in append mode
    /// - `sender.c:87-92` - `receive_sums()` returns early without reading blocks
    pub append: bool,
    /// Whether append-verify mode is active (`--append-verify`, append_mode == 2).
    ///
    /// When true the receiver folds the existing on-disk prefix into the
    /// whole-file checksum so a corrupted prefix fails verification and triggers
    /// a re-transmit. Plain `--append` trusts the prefix and never sums it.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:357-373` - `if (append_mode == 2)` prefix `sum_update`
    pub append_verify: bool,
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
    /// - `token.c:863-866` - zstd `r_init` only resets `rx_token`, not the DCtx
    ///
    /// # Errors
    ///
    /// Propagates the `io::Error` from [`TokenReader::new`] when the
    /// underlying compressed-token decoder fails to initialize.
    pub fn create_token_reader(&self) -> std::io::Result<TokenReader> {
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
    /// SEC-1.e parent-dirfd carrier rooted at the destination tree.
    ///
    /// Threaded through to the per-entry response processing so the
    /// SEC-1.f-j cutover sites can resolve relative names against a
    /// sandboxed dirfd via `*at` syscalls instead of re-walking paths
    /// through the kernel. Held as `&Arc` rather than `&DirSandbox` so
    /// SEC-1.r anchor sites (the temp-file Drop guard) can clone the
    /// carrier into a stable handle that outlives this borrow. `None`
    /// when the receiver could not open the destination root (e.g. it
    /// does not exist yet).
    #[cfg(unix)]
    pub sandbox: Option<&'a std::sync::Arc<fast_io::DirSandbox>>,
    /// Destination tree root anchor for the SEC-1.j leaf-rename detector.
    ///
    /// `process_file_response` uses this together with `sandbox` to route
    /// the temp -> final rename through `renameat(dirfd, leaf, dirfd,
    /// leaf)` when both the temp and final names are single-component
    /// leaves beneath this root, so a TOCTOU symlink swap on either leaf
    /// cannot redirect the commit. Multi-component / cross-tree cases
    /// keep the path-based fallback. `None` when no anchor is available.
    #[cfg(unix)]
    pub dest_dir: Option<&'a std::path::Path>,
}

/// Decides whether the receiver writes this file straight to its final
/// destination (inplace) or reconstructs it into a temp file that is renamed on
/// commit.
///
/// `--inplace` and `--append` write to the destination in place, preserving
/// existing content (append implies inplace, and the sender only transmits the
/// data beyond the existing length).
///
/// A `--partial-dir` resume is deliberately excluded. When the `I` capability is
/// negotiated (`inplace_partial`) and the basis is the partial-dir file
/// (`FNAMECMP_PARTIAL_DIR`), upstream's `one_inplace` (receiver.c:910) writes the
/// reconstruction in place to the partial file itself (`partialptr`,
/// receiver.c:969) and renames it onto the destination only once the transfer
/// completes, so an interrupt leaves the grown partial inside the partial dir and
/// never a truncated file at the live destination name. oc reaches the same
/// observable end state by reconstructing from the partial-dir basis into a temp
/// file and renaming on commit: keeping the transfer temp+rename
/// (`needs_rename == true`) lets the disk thread relocate the in-flight temp back
/// into the partial dir on interrupt (`retain_partial_file`'s `PartialDir` branch,
/// cleanup.c:105-115). Taking the inplace path for the resume would instead write
/// the reconstruction directly to the live destination and leave a full-size but
/// incomplete file there on interrupt - a silent data-integrity hazard.
///
/// `--inplace`/`--append` never combine with `--partial-dir` (rejected during
/// config validation), so the exclusion only ever suppresses the `one_inplace`
/// case; it is defensive against the two being wired together in the future.
///
/// # Upstream Reference
///
/// - `receiver.c:968` - append mode implies inplace.
/// - `receiver.c:910` - `one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR`.
/// - `receiver.c:969` - `fnametmp = one_inplace ? partialptr : fname`.
/// - `cleanup.c:105-115` - `handle_partial_dir()` moves the temp into the partial dir.
fn resolve_use_inplace(
    inplace: bool,
    append: bool,
    inplace_partial: bool,
    fnamecmp_type: Option<protocol::FnameCmpType>,
) -> bool {
    let write_to_destination = inplace || append;
    let one_inplace_partial_dir =
        inplace_partial && fnamecmp_type == Some(protocol::FnameCmpType::PartialDir);
    write_to_destination && !one_inplace_partial_dir
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

    // upstream: sender.c emits responses in NDX order; out-of-order is a protocol violation.
    if echoed_ndx != expected_ndx {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "sender echoed NDX {echoed_ndx} but expected {expected_ndx} - protocol violation"
            ),
        ));
    }

    // The echoed sum_head carries the existing file length used for the append mode offset.
    let echoed_sum_head = SumHead::read(reader)?;

    let (file_path, basis_path, signature, target_size) = pending.into_parts();

    let use_inplace = resolve_use_inplace(
        ctx.config.inplace,
        ctx.config.append,
        ctx.config.inplace_partial,
        sender_attrs.fnamecmp_type,
    );

    // upstream: receiver.c:352-372 - in append mode, seek output fd to existing file length
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
    /// - `receiver.c:372-373` - `offset = sum.flength; do_lseek(fd, offset, SEEK_SET)`
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
    fn resolve_use_inplace_writes_to_destination_for_inplace_and_append() {
        // upstream: receiver.c:968 - append implies inplace; both write to the
        // destination directly.
        assert!(resolve_use_inplace(true, false, false, None));
        assert!(resolve_use_inplace(false, true, false, None));
    }

    #[test]
    fn resolve_use_inplace_temp_rename_without_inplace_flags() {
        assert!(!resolve_use_inplace(false, false, false, None));
        assert!(!resolve_use_inplace(
            false,
            false,
            false,
            Some(protocol::FnameCmpType::Fname)
        ));
    }

    #[test]
    fn resolve_use_inplace_partial_dir_resume_stays_temp_rename() {
        // The one_inplace case (inplace_partial negotiated + partial-dir basis)
        // must NOT take the inplace path: staying temp+rename keeps
        // needs_rename == true so an interrupt relocates the in-flight temp into
        // the partial dir (retain_partial_file's PartialDir branch) instead of
        // leaving a full-size but incomplete file at the live destination name.
        // upstream: receiver.c:910,969 write one_inplace to partialptr, never fname.
        assert!(!resolve_use_inplace(
            false,
            false,
            true,
            Some(protocol::FnameCmpType::PartialDir)
        ));
        // A non-partial-dir basis with the capability negotiated (e.g. a fresh
        // or exact-name transfer) is likewise temp+rename.
        assert!(!resolve_use_inplace(
            false,
            false,
            true,
            Some(protocol::FnameCmpType::Fname)
        ));
        assert!(!resolve_use_inplace(false, false, true, None));
    }

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
            io_uring_depth: None,
            preserve_xattrs: false,
            want_xattr_optim: false,
            append: false,
            append_verify: false,
        };
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("RequestConfig"));
    }

    /// Builds a minimal protocol-31 `RequestConfig` with iflags enabled.
    fn iflags_request_config() -> RequestConfig<'static> {
        RequestConfig {
            protocol: ProtocolVersion::from_supported(31).expect("31 is supported"),
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
            io_uring_depth: None,
            preserve_xattrs: false,
            want_xattr_optim: false,
            append: false,
            append_verify: false,
        }
    }

    fn request_bytes(base_iflags: u32) -> Vec<u8> {
        use protocol::codec::MonotonicNdxWriter;
        let config = iflags_request_config();
        let mut ndx_codec = MonotonicNdxWriter::new(config.protocol.as_u8());
        let mut buf: Vec<u8> = Vec::new();
        send_file_request(
            &mut buf,
            &mut ndx_codec,
            0,
            std::path::PathBuf::from("data.txt"),
            None,
            None,
            protocol::FnameCmpType::Fname,
            None,
            0,
            base_iflags,
            &config,
        )
        .expect("request encodes");
        buf
    }

    /// upstream: generator.c:1937-1947 - the generator writes the full itemize
    /// iflags to the sender, so a new-file request carries ITEM_IS_NEW (0x2000)
    /// alongside ITEM_TRANSFER (0x8000) → wire shortint 0xA000 (LE 00 A0). The
    /// sender echoes these back and prints `<f+++++++++` (#301). A bare transfer
    /// request (no diff bits) writes only 0x8000 (LE 00 80).
    #[test]
    fn new_file_request_forwards_item_is_new() {
        let new_bytes = request_bytes(
            u32::from(SenderAttrs::ITEM_TRANSFER) | crate::generator::ItemFlags::ITEM_IS_NEW,
        );
        assert!(
            new_bytes.windows(2).any(|w| w == [0x00, 0xA0]),
            "new-file request must carry ITEM_TRANSFER|ITEM_IS_NEW (0xA000): {new_bytes:02x?}"
        );

        let bare_bytes = request_bytes(u32::from(SenderAttrs::ITEM_TRANSFER));
        assert!(
            bare_bytes.windows(2).any(|w| w == [0x00, 0x80]),
            "bare request carries ITEM_TRANSFER (0x8000): {bare_bytes:02x?}"
        );
        assert!(
            !bare_bytes.windows(2).any(|w| w == [0x00, 0xA0]),
            "bare request must NOT set ITEM_IS_NEW: {bare_bytes:02x?}"
        );
    }

    /// The managed trailing-field bits (XATTR/BASIS/XNAME) must never leak from
    /// `base_iflags` into the wire shortint, since they demand trailing bytes
    /// this call site does not write for a plain FNAME request.
    #[test]
    fn base_iflags_managed_bits_are_masked_off() {
        // Deliberately pollute base_iflags with the managed bits.
        let polluted = u32::from(
            SenderAttrs::ITEM_TRANSFER
                | SenderAttrs::ITEM_REPORT_XATTR
                | SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS
                | SenderAttrs::ITEM_XNAME_FOLLOWS,
        );
        let bytes = request_bytes(polluted);
        // Only ITEM_TRANSFER (0x8000, LE 00 80) survives; none of the managed
        // bits (which would each demand a trailing wire field) appear.
        assert!(
            bytes.windows(2).any(|w| w == [0x00, 0x80]),
            "masked request keeps ITEM_TRANSFER only: {bytes:02x?}"
        );
    }

    /// Encodes a bare (no-signature) file request with the given basis type and
    /// optional xname, returning the wire bytes.
    fn request_bytes_basis(fnamecmp_type: protocol::FnameCmpType, xname: Option<&[u8]>) -> Vec<u8> {
        use protocol::codec::MonotonicNdxWriter;
        let config = iflags_request_config();
        let mut ndx_codec = MonotonicNdxWriter::new(config.protocol.as_u8());
        let mut buf: Vec<u8> = Vec::new();
        send_file_request(
            &mut buf,
            &mut ndx_codec,
            0,
            std::path::PathBuf::from("data.txt"),
            None,
            None,
            fnamecmp_type,
            xname,
            0,
            u32::from(SenderAttrs::ITEM_TRANSFER),
            &config,
        )
        .expect("request encodes");
        buf
    }

    /// A fuzzy-matched basis must be advertised to the sender byte-for-byte like
    /// upstream: iflags with ITEM_TRANSFER|ITEM_BASIS_TYPE_FOLLOWS|
    /// ITEM_XNAME_FOLLOWS (0x9800), then the FNAMECMP_FUZZY byte (0x83), then the
    /// basename as a vstring. Without this the sender/receiver never learn the
    /// fuzzy basis over the wire, diverging from a real rsync generator.
    /// upstream: generator.c:1944-1948 (iflags + fnamecmp_type + write_vstring),
    /// io.c:2297 (vstring length prefix).
    #[test]
    fn fuzzy_request_emits_fnamecmp_fuzzy_and_xname_vstring() {
        let bytes = request_bytes_basis(protocol::FnameCmpType::Fuzzy(0), Some(b"old.txt"));
        // iflags 0x9800 (LE 00 98), then 0x83, then vstring(7, "old.txt").
        let expected: &[u8] = &[
            0x00, 0x98, 0x83, 0x07, b'o', b'l', b'd', b'.', b't', b'x', b't',
        ];
        assert!(
            bytes.windows(expected.len()).any(|w| w == expected),
            "fuzzy request must carry iflags|0x83|vstring(basename): {bytes:02x?}"
        );
    }

    /// #204: an alt-dest content-differ basis found in reference dir `j` (the
    /// destination is absent) must be advertised like a real upstream generator:
    /// iflags ITEM_TRANSFER|ITEM_BASIS_TYPE_FOLLOWS (0x8800, LE 00 88), then the
    /// FNAMECMP_BASIS_DIR_LOW + j byte (0x00 for j=0), and NO xname vstring - a
    /// basis-dir tag is below FNAMECMP_FUZZY so ITEM_XNAME_FOLLOWS stays clear.
    /// Without the basis-type byte a real upstream peer sees a bare FNAME request
    /// and the wire diverges. upstream: generator.c:1054 returns
    /// FNAMECMP_BASIS_DIR_LOW + j; generator.c:1943 sets ITEM_BASIS_TYPE_FOLLOWS;
    /// generator.c:1945 gates ITEM_XNAME_FOLLOWS on fnamecmp_type >= FNAMECMP_FUZZY.
    #[test]
    fn basis_dir_request_emits_basis_type_byte_no_xname() {
        let bytes = request_bytes_basis(protocol::FnameCmpType::BasisDir(0), None);
        // iflags 0x8800 (LE 00 88) then the basis-dir index byte 0x00.
        let expected: &[u8] = &[0x00, 0x88, 0x00];
        assert!(
            bytes.windows(expected.len()).any(|w| w == expected),
            "basis-dir request must carry iflags 0x8800 then the index byte: {bytes:02x?}"
        );
        // ITEM_XNAME_FOLLOWS (0x9800 / 0x9000) must stay clear for a basis dir.
        assert!(
            !bytes
                .windows(2)
                .any(|w| w == [0x00, 0x98] || w == [0x00, 0x90]),
            "basis-dir request must NOT set ITEM_XNAME_FOLLOWS: {bytes:02x?}"
        );
    }

    /// #205: a `-yy` fuzzy hit from reference dir `k` is advertised as
    /// FNAMECMP_FUZZY + (k + 1) (the dest dir is fuzzy-index 0), so reference dir
    /// 0 emits byte 0x84. iflags carry ITEM_TRANSFER|ITEM_BASIS_TYPE_FOLLOWS|
    /// ITEM_XNAME_FOLLOWS (0x9800, LE 00 98), then the 0x84 tag, then the basis
    /// basename as a vstring. upstream: generator.c:861,903 (FNAMECMP_FUZZY + i),
    /// generator.c:1944-1948 (iflags + fnamecmp_type + write_vstring).
    #[test]
    fn ref_dir_fuzzy_request_emits_fnamecmp_fuzzy_plus_index() {
        let bytes = request_bytes_basis(protocol::FnameCmpType::Fuzzy(1), Some(b"old.txt"));
        // iflags 0x9800 (LE 00 98), then 0x84, then vstring(7, "old.txt").
        let expected: &[u8] = &[
            0x00, 0x98, 0x84, 0x07, b'o', b'l', b'd', b'.', b't', b'x', b't',
        ];
        assert!(
            bytes.windows(expected.len()).any(|w| w == expected),
            "ref-dir fuzzy request must carry iflags|0x84|vstring(basename): {bytes:02x?}"
        );
    }

    /// With `--fuzzy` off (an ordinary FNAME basis) the request must be
    /// unchanged: bare ITEM_TRANSFER (0x8000, LE 00 80), no basis-type byte and
    /// no xname vstring. Guards against the fuzzy path leaking into the common
    /// case.
    #[test]
    fn fname_request_has_no_basis_byte_or_xname() {
        let bytes = request_bytes_basis(protocol::FnameCmpType::Fname, None);
        assert!(
            bytes.windows(2).any(|w| w == [0x00, 0x80]),
            "FNAME request carries bare ITEM_TRANSFER: {bytes:02x?}"
        );
        assert!(
            !bytes.contains(&0x83),
            "FNAME request must not emit the FNAMECMP_FUZZY byte: {bytes:02x?}"
        );
        // No ITEM_XNAME_FOLLOWS in the shortint (0x9000 / 0x9800 would set it).
        assert!(
            !bytes
                .windows(2)
                .any(|w| w == [0x00, 0x98] || w == [0x00, 0x90]),
            "FNAME request must not set ITEM_XNAME_FOLLOWS: {bytes:02x?}"
        );
    }
}
