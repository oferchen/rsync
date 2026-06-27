#![deny(unsafe_code)]
//! Server-side Generator role implementation.
//!
//! When the native server operates as a Generator (sender), it:
//! 1. Walks the local filesystem to build a file list
//! 2. Sends the file list to the client (receiver)
//! 3. Receives signatures from the client for existing files
//! 4. Generates and sends deltas for each file
//!
//! # Upstream Reference
//!
//! - `generator.c` - Upstream generator role implementation
//! - `flist.c` - File list building and transmission
//! - `match.c` - Block matching and delta generation
//!
//! This module mirrors upstream rsync's generator behavior while leveraging
//! Rust's type system for safety.
//!
//! # Implementation Guide
//!
//! For comprehensive documentation on how the generator works within the delta transfer
//! algorithm, see the [`crate::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::receiver`] - The receiver role that applies deltas from the generator
//! - [`engine::delta::DeltaGenerator`] - Core delta generation engine
//! - [`engine::delta::DeltaSignatureIndex`] - Fast block lookup for delta generation
//! - [`engine::signature`] - Signature reconstruction from wire format
//! - [`protocol::wire`] - Wire format for signatures and deltas
//!
//! # Sender-side INC_RECURSE state machine
//!
//! When the sender advertises and negotiates `INC_RECURSE` (`'i'` capability),
//! the file list is streamed as per-directory sub-segments rather than as one
//! monolithic list. The send loop drives the following state machine:
//!
//! ```text
//!   Idle -> ScanDir -> SendChunk -> WaitAck -> NextDir -> Done
//!     ^                                          |
//!     +------------------------------------------+
//! ```
//!
//! - **Idle**: handshake complete, top-level entries enqueued.
//! - **ScanDir**: walk one directory; produce a `PendingSegment`.
//! - **SendChunk**: emit the segment via `send_file_list` /
//!   `encode_and_send_segment`, throttled by `MIN_FILECNT_LOOKAHEAD`
//!   (see `SegmentScheduler`).
//! - **WaitAck**: process incoming NDX requests / signatures from the
//!   receiver while the next segment is staged.
//! - **NextDir**: advance the cursor; loop back to ScanDir.
//! - **Done**: emit `NDX_FLIST_EOF` (`flist_eof_sent = true`) and proceed
//!   to the goodbye phase.
//!
//! ## Upstream Reference
//!
//! - `flist.c:2192 send_file_list()` - top-level + initial segment dispatch.
//! - `flist.c:send_extra_file_list()` - per-directory sub-segments,
//!   `MIN_FILECNT_LOOKAHEAD` throttling, `NDX_FLIST_EOF` finalization.
//! - `sender.c:199 send_files()` - main send loop, calls into segment
//!   scheduling at the top and bottom of each iteration (lines ~227, ~261).
//! - `generator.c:2226 generate_files()` - peer side that consumes the
//!   segmented stream and drives signature/data exchange.
//! - `receiver.c:522 recv_files()` - receiver-side counterpart to the
//!   sender's segmented dispatch.
//! - `compat.c:161 set_allow_inc_recurse()` - capability negotiation gate
//!   (`allow_inc_recurse` is cleared when `!recurse || use_qsort` or when
//!   the sender side cannot satisfy the segmentation contract).
//!
//! ## Current status
//!
//! The sender-side state machine and segment scheduler are implemented
//! (see `IncrementalState`, `SegmentScheduler`, `PendingSegment`).
//! oc-rsync advertises the `'i'` capability in both transfer directions
//! by default, mirroring upstream's `allow_inc_recurse = 1`
//! initialization. `--no-inc-recursive` (or
//! `ClientConfigBuilder::inc_recursive_send(false)`) clears the flag and
//! suppresses the bit. Tracker #1862.

mod context;
mod delta;
mod diagnostics;
mod file_list;
mod filters;
pub mod io_error_flags;
mod item_flags;
pub mod itemize;
mod open_source;
mod protocol_io;
mod segments;
mod stats;
#[cfg(test)]
mod tests;
mod timing;
mod transfer;

pub use self::context::GeneratorContext;
pub use self::delta::generate_delta_from_signature;
pub use self::diagnostics::{
    flush_rate_totals, ndx_convert_totals, prepare_acl_totals, segment_dispatch_totals,
};
pub use self::item_flags::ItemFlags;
pub use self::protocol_io::{calculate_duration_ms, read_signature_blocks};
pub use self::stats::GeneratorStats;

// Re-exports for sibling submodules accessing diagnostics, segments, and stats
// through `super::*` (matches the pre-decomposition import surface).
pub(crate) use self::diagnostics::{flush_with_count, record_prepare_acl, record_segment_dispatch};
pub(crate) use self::segments::{DirSegment, PendingSegment, SegmentScheduler, TaggedIndex};
pub(crate) use self::stats::{TransferLoopResult, is_early_close_error};

#[cfg(test)]
pub(crate) use self::diagnostics::partition_point_depth;
