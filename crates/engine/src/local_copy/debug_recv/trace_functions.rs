//! Free trace functions for receiver debug output.
//!
//! Each function has a tracing-enabled variant that emits structured events
//! and a no-op variant compiled when the `tracing` feature is disabled.
//! Matches upstream rsync's `receiver.c` / `generator.c` debug output format.

use std::time::Duration;

use super::RECV_TARGET;

/// Traces the start of a file receive operation.
///
/// Corresponds to the entry point of upstream `recv_files()`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_start(name: &str, file_size: u64, index: usize) {
    tracing::info!(
        target: RECV_TARGET,
        name = %name,
        file_size = file_size,
        index = index,
        "recv_file: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_start(_name: &str, _file_size: u64, _index: usize) {}

/// Traces the completion of a file receive operation.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_end(name: &str, bytes_received: u64, elapsed: Duration) {
    tracing::info!(
        target: RECV_TARGET,
        name = %name,
        bytes_received = bytes_received,
        elapsed_ms = elapsed.as_millis(),
        "recv_file: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_end(_name: &str, _bytes_received: u64, _elapsed: Duration) {}

/// Traces basis file selection during receive.
///
/// Corresponds to basis file selection in upstream `generator.c`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_basis_file_selected(name: &str, basis_path: &str, basis_size: u64) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        basis_path = %basis_path,
        basis_size = basis_size,
        "basis: selected"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_basis_file_selected(_name: &str, _basis_path: &str, _basis_size: u64) {}

/// Traces the start of delta application for a file.
///
/// Corresponds to delta application in upstream `receiver.c`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_start(name: &str, basis_size: u64, delta_size: u64) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        basis_size = basis_size,
        delta_size = delta_size,
        "delta_apply: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_start(_name: &str, _basis_size: u64, _delta_size: u64) {}

/// Traces a block copy event during delta application.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_match(block_index: usize, offset: u64, length: u32) {
    tracing::trace!(
        target: RECV_TARGET,
        block_index = block_index,
        offset = offset,
        length = length,
        "delta_apply: match"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_match(_block_index: usize, _offset: u64, _length: u32) {}

/// Traces a literal data event during delta application.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_literal(offset: u64, length: u32) {
    tracing::trace!(
        target: RECV_TARGET,
        offset = offset,
        length = length,
        "delta_apply: literal"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_literal(_offset: u64, _length: u32) {}

/// Traces the completion of delta application for a file.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_end(name: &str, output_size: u64, elapsed: Duration) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        output_size = output_size,
        elapsed_ms = elapsed.as_millis(),
        "delta_apply: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_end(_name: &str, _output_size: u64, _elapsed: Duration) {}

/// Traces checksum verification for a received file.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_verify(name: &str, expected: &[u8], computed: &[u8], matched: bool) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        expected = format!("{:02x?}", expected),
        computed = format!("{:02x?}", computed),
        matched = matched,
        "checksum: verify"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_verify(_name: &str, _expected: &[u8], _computed: &[u8], _matched: bool) {}

/// Traces a summary of all receive operations.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_summary(total_files: usize, total_bytes: u64, total_elapsed: Duration) {
    tracing::info!(
        target: RECV_TARGET,
        total_files = total_files,
        total_bytes = total_bytes,
        elapsed_ms = total_elapsed.as_millis(),
        "recv: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_summary(_total_files: usize, _total_bytes: u64, _total_elapsed: Duration) {}
