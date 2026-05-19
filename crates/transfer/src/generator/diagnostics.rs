//! Process-wide diagnostic counters for sender-side INC_RECURSE.
//!
//! Each counter tracks one hot-path activity in the generator: NDX flat/wire
//! conversion (#2199, I4), `writer.flush()` invocations on the transfer hot
//! path (#2198, I3), `prepare_pending_acl` invocations (#2200, I5), and
//! `encode_and_send_segment` invocations (#2197, I2). Counters are sampled at
//! end-of-transfer in `GeneratorContext::run` and emitted via `tracing::debug!`.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Total invocations of `wire_to_flat_ndx` + `flat_to_wire_ndx` across all
/// generator transfers in this process. Diagnostic counter for sender-side
/// INC_RECURSE (#2199, I4) - quantifies how often the NDX conversion hot path
/// fires per transfer relative to the file count.
///
/// Sampled at end-of-transfer in `GeneratorContext::run` via
/// [`ndx_convert_totals`] and emitted via `tracing::debug!`.
pub(crate) static NDX_CONVERT_CALLS: AtomicU64 = AtomicU64::new(0);

/// Cumulative `partition_point` comparison depth (approximated as
/// `floor(log2(len)) + 1`) summed across every NDX conversion call.
/// Diagnostic counter for sender-side INC_RECURSE (#2199, I4) - lets
/// operators see when the segment table grows large enough for the
/// binary-search cost to matter.
pub(crate) static NDX_CONVERT_CMPS: AtomicU64 = AtomicU64::new(0);

/// Approximate number of comparisons a binary search performs on a sorted
/// slice of length `len`. Returns `floor(log2(len)) + 1`, matching the worst
/// case of `[T]::partition_point` on the segment table.
pub(crate) fn partition_point_depth(len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    u64::from((len as u64).ilog2()) + 1
}

/// Snapshot of the global NDX conversion counters.
///
/// Returns `(call_count, cumulative_partition_point_depth)`. Used by the
/// generator finalize path to emit an end-of-transfer diagnostic line and by
/// unit tests that assert the counters monotonically grow.
#[must_use]
pub fn ndx_convert_totals() -> (u64, u64) {
    (
        NDX_CONVERT_CALLS.load(Ordering::Relaxed),
        NDX_CONVERT_CMPS.load(Ordering::Relaxed),
    )
}

/// Total `writer.flush()` invocations on the generator transfer hot path across
/// all generator transfers in this process. Diagnostic counter for sender-side
/// INC_RECURSE (#2198, I3) - quantifies how often the sender forces a flush
/// per file and per NDX-control echo, relative to the file count.
///
/// Sampled at end-of-transfer in `GeneratorContext::run` via
/// [`flush_rate_totals`] and emitted via `tracing::debug!`.
static FLUSH_CALLS: AtomicU64 = AtomicU64::new(0);

/// Records a flush invocation on the generator transfer hot path. Used by the
/// transfer loop (per-iteration, NDX_DONE echo, dry-run, final NDX_DONE) to
/// bump [`FLUSH_CALLS`] for INC_RECURSE diagnostic I3 (#2198).
pub(crate) fn flush_with_count<W: Write>(writer: &mut W) -> io::Result<()> {
    FLUSH_CALLS.fetch_add(1, Ordering::Relaxed);
    writer.flush()
}

/// Snapshot of the global generator flush counter.
///
/// Returns the cumulative number of `writer.flush()` calls recorded by
/// [`flush_with_count`]. Used by the generator finalize path to emit an
/// end-of-transfer diagnostic line and by unit tests that assert the counter
/// monotonically grows.
#[must_use]
pub fn flush_rate_totals() -> u64 {
    FLUSH_CALLS.load(Ordering::Relaxed)
}

/// Total invocations of `prepare_pending_acl` across all generator transfers
/// in this process. Diagnostic counter for sender-side INC_RECURSE (#2200, I5);
/// quantifies how often the per-entry ACL prep fires per segment relative to
/// the file count.
///
/// Sampled at end-of-transfer in `GeneratorContext::run` via
/// [`prepare_acl_totals`] and emitted via `tracing::debug!`.
static PREPARE_ACL_CALLS: AtomicU64 = AtomicU64::new(0);

/// Cumulative elapsed time spent inside `prepare_pending_acl`, in nanoseconds,
/// summed across every invocation. Diagnostic counter for sender-side
/// INC_RECURSE (#2200, I5); lets operators see when filesystem ACL reads
/// become a measurable share of segment-encoding time.
static PREPARE_ACL_ELAPSED_NS: AtomicU64 = AtomicU64::new(0);

/// Records one `prepare_pending_acl` invocation and adds its elapsed wall
/// time (in nanoseconds, saturating to `u64::MAX`) to the cumulative counter.
/// Used by `GeneratorContext::prepare_pending_acl` to bump
/// [`PREPARE_ACL_CALLS`] / [`PREPARE_ACL_ELAPSED_NS`] for INC_RECURSE
/// diagnostic I5 (#2200).
pub(crate) fn record_prepare_acl(elapsed: Duration) {
    PREPARE_ACL_CALLS.fetch_add(1, Ordering::Relaxed);
    let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
    PREPARE_ACL_ELAPSED_NS.fetch_add(ns, Ordering::Relaxed);
}

/// Snapshot of the global `prepare_pending_acl` counters.
///
/// Returns `(call_count, cumulative_elapsed_ns)`. Used by the generator
/// finalize path to emit an end-of-transfer diagnostic line and by unit tests
/// that assert the counters monotonically grow.
#[must_use]
pub fn prepare_acl_totals() -> (u64, u64) {
    (
        PREPARE_ACL_CALLS.load(Ordering::Relaxed),
        PREPARE_ACL_ELAPSED_NS.load(Ordering::Relaxed),
    )
}

/// Total invocations of `encode_and_send_segment` across all generator
/// transfers in this process. Diagnostic counter for sender-side INC_RECURSE
/// (#2197, I2); quantifies how often per-directory segments are dispatched
/// from the transfer loop and the segment scheduler, relative to the file
/// count and the `MIN_FILECNT_LOOKAHEAD` throttling threshold.
///
/// Sampled at end-of-transfer in `GeneratorContext::run` via
/// [`segment_dispatch_totals`] and emitted via `tracing::debug!`.
static SEGMENT_DISPATCH_CALLS: AtomicU64 = AtomicU64::new(0);

/// Cumulative elapsed time spent inside `encode_and_send_segment`, in
/// nanoseconds, summed across every invocation. Diagnostic counter for
/// sender-side INC_RECURSE (#2197, I2); lets operators see what share of the
/// transfer wall time is spent encoding and pushing sub-list bytes onto the
/// wire.
static SEGMENT_DISPATCH_ELAPSED_NS: AtomicU64 = AtomicU64::new(0);

/// Records one `encode_and_send_segment` invocation and adds its elapsed wall
/// time (in nanoseconds, saturating to `u64::MAX`) to the cumulative counter.
/// Used by `GeneratorContext::encode_and_send_segment` to bump
/// [`SEGMENT_DISPATCH_CALLS`] / [`SEGMENT_DISPATCH_ELAPSED_NS`] for
/// INC_RECURSE diagnostic I2 (#2197).
pub(crate) fn record_segment_dispatch(elapsed: Duration) {
    SEGMENT_DISPATCH_CALLS.fetch_add(1, Ordering::Relaxed);
    let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
    SEGMENT_DISPATCH_ELAPSED_NS.fetch_add(ns, Ordering::Relaxed);
}

/// Snapshot of the global `encode_and_send_segment` counters.
///
/// Returns `(call_count, cumulative_elapsed_ns)`. Used by the generator
/// finalize path to emit an end-of-transfer diagnostic line and by unit tests
/// that assert the counters monotonically grow.
#[must_use]
pub fn segment_dispatch_totals() -> (u64, u64) {
    (
        SEGMENT_DISPATCH_CALLS.load(Ordering::Relaxed),
        SEGMENT_DISPATCH_ELAPSED_NS.load(Ordering::Relaxed),
    )
}
