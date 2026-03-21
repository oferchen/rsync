//! Delta matching trace functions.
//!
//! Traces block matching operations that correspond to upstream rsync's
//! `match.c`. All tracing is conditionally compiled behind the `tracing`
//! feature flag.

use std::time::Duration;

use super::DELTASUM_TARGET;

/// Traces the start of delta matching for a file.
///
/// Emits a tracing event when beginning to match target file data against
/// basis file checksums. Corresponds to matching logic in upstream `match.c`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_start(file_name: &str, basis_size: u64, target_size: u64) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        basis_size = basis_size,
        target_size = target_size,
        "match: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_start(_file_name: &str, _basis_size: u64, _target_size: u64) {}

/// Traces a successful block match during delta generation.
///
/// Logs when a block from the target matches a block in the basis file via
/// rolling checksum, allowing compression via reference.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_hit(block_index: usize, offset: u64, length: u32, weak: u32) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        block_index = block_index,
        offset = offset,
        length = length,
        weak = format!("{:08x}", weak),
        "match: hit"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_hit(_block_index: usize, _offset: u64, _length: u32, _weak: u32) {}

/// Traces a miss during delta matching.
///
/// Logs when no matching block is found, requiring literal data transmission.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_miss(offset: u64, length: u32) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        offset = offset,
        length = length,
        "match: miss"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_miss(_offset: u64, _length: u32) {}

/// Traces a false alarm during delta matching.
///
/// Logs when a weak checksum matches but strong checksum verification fails,
/// indicating a collision in the rolling checksum algorithm.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_false_alarm(weak: u32, offset: u64) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        weak = format!("{:08x}", weak),
        offset = offset,
        "match: false_alarm"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_false_alarm(_weak: u32, _offset: u64) {}

/// Traces the completion of delta matching for a file.
///
/// Emits summary statistics showing match efficiency and data transfer
/// characteristics.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_end(
    file_name: &str,
    hits: usize,
    misses: usize,
    false_alarms: usize,
    data_bytes: u64,
    matched_bytes: u64,
    elapsed: Duration,
) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        hits = hits,
        misses = misses,
        false_alarms = false_alarms,
        data_bytes = data_bytes,
        matched_bytes = matched_bytes,
        elapsed_ms = elapsed.as_millis(),
        "match: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_end(
    _file_name: &str,
    _hits: usize,
    _misses: usize,
    _false_alarms: usize,
    _data_bytes: u64,
    _matched_bytes: u64,
    _elapsed: Duration,
) {
}

/// Traces aggregate statistics for the entire delta/checksum session.
///
/// Emits totals across all files processed, including match efficiency and
/// data transfer ratios.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_deltasum_summary(
    total_files: usize,
    total_hits: usize,
    total_misses: usize,
    total_false_alarms: usize,
    total_matched: u64,
    total_literal: u64,
) {
    tracing::info!(
        target: DELTASUM_TARGET,
        total_files = total_files,
        total_hits = total_hits,
        total_misses = total_misses,
        total_false_alarms = total_false_alarms,
        total_matched = total_matched,
        total_literal = total_literal,
        "deltasum: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_deltasum_summary(
    _total_files: usize,
    _total_hits: usize,
    _total_misses: usize,
    _total_false_alarms: usize,
    _total_matched: u64,
    _total_literal: u64,
) {
}
