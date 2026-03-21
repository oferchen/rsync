//! Checksum generation trace functions.
//!
//! Traces block checksum computation that corresponds to upstream rsync's
//! `checksum.c`. All tracing is conditionally compiled behind the `tracing`
//! feature flag.

use std::time::Duration;

use super::DELTASUM_TARGET;

/// Traces the start of checksum generation for a file.
///
/// Emits a tracing event when beginning to compute checksums for basis file
/// blocks. Corresponds to checksum generation in upstream `checksum.c`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_start(file_name: &str, block_count: usize, block_size: u32) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        block_count = block_count,
        block_size = block_size,
        "checksum: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_start(_file_name: &str, _block_count: usize, _block_size: u32) {}

/// Traces a single checksum block computation.
///
/// Logs the weak (rolling) and strong checksums for a single block in the
/// basis file.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_block(block_index: usize, weak: u32, strong: &[u8]) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        block_index = block_index,
        weak = format!("{:08x}", weak),
        strong = strong.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        }),
        "checksum: block"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_block(_block_index: usize, _weak: u32, _strong: &[u8]) {}

/// Traces the completion of checksum generation.
///
/// Emits summary statistics for the checksum generation phase.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_end(file_name: &str, block_count: usize, elapsed: Duration) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        block_count = block_count,
        elapsed_ms = elapsed.as_millis(),
        "checksum: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_end(_file_name: &str, _block_count: usize, _elapsed: Duration) {}
