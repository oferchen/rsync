//! Unit tests for the registered-buffer registry, slot lifecycle, batch
//! submission helpers, telemetry, and Drop contract. Split by concern so
//! each submodule stays well under the LoC cap.
//!
//! - [`registry`] - slot allocator / [`RegisteredBufferGroup`] checkout.
//! - [`submit`] - `ReadFixed`/`WriteFixed` batch submission helpers.
//! - [`stats`] - acquire / miss telemetry counters and snapshots.
//! - [`status`] - [`RegisteredBufferStatus`] reporting for `try_new_with_status`.
//! - [`drop_contract`] - Drop semantics and constrained-environment coverage
//!   for the fixed-buffer invariants audit (PR #4022, task #2118).

use io_uring::IoUring as RawIoUring;

use super::registry::RegisteredBufferGroup;

mod drop_contract;
mod registry;
mod stats;
mod status;
mod submit;

/// Constructs a [`RawIoUring`] with the given queue depth, returning
/// `None` when io_uring is not available in the current environment
/// (CI sandboxes, seccomp filters, kernel < 5.6). Tests treat `None`
/// as a skip rather than a failure.
pub(super) fn try_ring(entries: u32) -> Option<RawIoUring> {
    RawIoUring::new(entries).ok()
}

/// Registers a [`RegisteredBufferGroup`] of `count` buffers of
/// `buffer_size` bytes against `ring`, returning `None` when the
/// kernel refuses the registration (seccomp, kernel limit, etc.).
pub(super) fn try_group(
    ring: &RawIoUring,
    buffer_size: usize,
    count: usize,
) -> Option<RegisteredBufferGroup> {
    RegisteredBufferGroup::new(ring, buffer_size, count).ok()
}
