#![deny(unsafe_code)]
//! Server-side Receiver role implementation.
//!
//! When the native server operates as a Receiver, it:
//! 1. Receives the file list from the client (sender)
//! 2. Generates signatures for existing local files
//! 3. Receives delta data and applies it to create/update files
//! 4. Sets metadata (permissions, times, ownership) on received files
//!
//! # Upstream Reference
//!
//! - `receiver.c:340` - `receive_data()` - Delta application logic
//! - `receiver.c:720` - `recv_files()` - Main file reception loop
//! - `generator.c:1450` - `recv_generator()` - Signature generation
//!
//! Mirrors upstream rsync's receiver behavior with block-by-block delta
//! application and atomic file updates via temporary files.
//!
//! # Implementation Guide
//!
//! For comprehensive documentation on how the receiver works within the delta transfer
//! algorithm, see the [`crate::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::generator`] - The generator role that sends deltas to the receiver
//! - [`engine::delta`] - Delta generation and application engine
//! - [`engine::signature`] - Signature generation utilities
//! - [`metadata::apply_metadata_from_file_entry`] - Metadata preservation
//! - [`protocol::wire`] - Wire format for signatures and deltas

mod basis;
mod context;
mod dest_root;
mod directory;
mod file_list;
mod itemize;
mod pipeline_setup;
mod quick_check;
mod stats;
#[cfg(test)]
mod tests;
mod transfer;
mod wire;

use std::num::NonZeroU8;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) use crate::parallel_io::ParallelThresholds;

use signature;

pub use self::basis::{BasisFileConfig, BasisFileResult, find_basis_file_with_config};
pub use self::context::ReceiverContext;
pub(in crate::receiver) use self::dest_root::dest_arg_has_trailing_slash;
pub use self::dest_root::ensure_dest_root_exists;
pub use self::file_list::IncrementalFileListReceiver;
pub(in crate::receiver) use self::pipeline_setup::{
    PipelineSetup, apply_acls_from_receiver_cache, compile_daemon_filter_set,
};
pub use self::stats::{ListOnlyEntry, SenderStats, TransferStats};
pub use self::wire::{
    SenderAttrs, SumHead, apply_xattr_abbreviation_values, write_signature_blocks,
    write_xattr_request,
};

/// Phase 1 checksum length (2 bytes) - reduced signature overhead.
///
/// Upstream rsync uses `SHORT_SUM_LENGTH` (2) during phase 1 to reduce
/// signature wire size. The `derive_strong_sum_length()` heuristic computes
/// a dynamic length between 2-16 bytes based on file and block sizes.
///
/// (upstream: generator.c:2157 `csum_length = SHORT_SUM_LENGTH`)
const PHASE1_CHECKSUM_LENGTH: NonZeroU8 =
    NonZeroU8::new(signature::block_size::SHORT_SUM_LENGTH).unwrap();

/// Phase 2 redo checksum length (16 bytes) - full collision resistance.
///
/// Upstream rsync switches to `SUM_LENGTH` (16) for phase 2 redo to ensure
/// maximum integrity after a whole-file checksum failure.
///
/// (upstream: generator.c:2163 `csum_length = SUM_LENGTH`)
const REDO_CHECKSUM_LENGTH: NonZeroU8 =
    NonZeroU8::new(signature::block_size::MAX_SUM_LENGTH).unwrap();

/// Total invocations of [`ReceiverContext::flat_to_wire_ndx`] across all
/// receiver transfers in this process. Diagnostic counter for receiver-side
/// INC_RECURSE (#2199, I4) - quantifies how often the wire/flat conversion
/// hot path fires per transfer relative to the file count.
///
/// Sampled at end-of-transfer in the receiver finalize path via
/// [`ndx_convert_totals`] and emitted via `tracing::debug!`.
static NDX_CONVERT_CALLS: AtomicU64 = AtomicU64::new(0);

/// Cumulative `partition_point` comparison depth (approximated as
/// `floor(log2(len)) + 1`) summed across every NDX conversion call.
/// Diagnostic counter for receiver-side INC_RECURSE (#2199, I4) - lets
/// operators see when the segment table grows large enough for the
/// binary-search cost to matter.
static NDX_CONVERT_CMPS: AtomicU64 = AtomicU64::new(0);

/// Approximate number of comparisons a binary search performs on a sorted
/// slice of length `len`. Returns `floor(log2(len)) + 1`, matching the worst
/// case of `[T]::partition_point` on the segment table.
fn partition_point_depth(len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    u64::from((len as u64).ilog2()) + 1
}

/// Snapshot of the global NDX conversion counters.
///
/// Returns `(call_count, cumulative_partition_point_depth)`. Used by the
/// receiver finalize path to emit an end-of-transfer diagnostic line and by
/// unit tests that assert the counters monotonically grow.
#[must_use]
pub fn ndx_convert_totals() -> (u64, u64) {
    (
        NDX_CONVERT_CALLS.load(Ordering::Relaxed),
        NDX_CONVERT_CMPS.load(Ordering::Relaxed),
    )
}
