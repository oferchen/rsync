//! Capacity policy for the bounded work queue.
//!
//! Defines the default capacity multiplier used by [`bounded`](super::bounded)
//! plus the [`adaptive_queue_depth`] heuristic that tunes queue depth based on
//! the average file size of the transfer set.

/// Default capacity multiplier applied to the rayon thread count.
pub(super) const CAPACITY_MULTIPLIER: usize = 2;

/// Size threshold below which files are considered "small" for queue sizing.
///
/// Files under 64 KiB benefit from deeper queues because per-file overhead
/// (syscalls, metadata) dominates over I/O time, making worker starvation
/// the primary bottleneck.
const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;

/// Size threshold above which files are considered "large" for queue sizing.
///
/// Files over 1 MiB are I/O-bound - deeper queues just waste memory without
/// improving throughput since each worker spends most of its time in I/O.
const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024;

/// Queue depth multiplier for small file workloads.
const SMALL_FILE_MULTIPLIER: usize = 8;

/// Queue depth multiplier for large file workloads.
const LARGE_FILE_MULTIPLIER: usize = 2;

/// Queue depth multiplier for mixed/medium file workloads.
const MEDIUM_FILE_MULTIPLIER: usize = 4;

/// Returns the default work queue capacity for the current rayon thread pool.
///
/// Equal to `2 * rayon::current_num_threads()`.
#[must_use]
pub fn default_capacity() -> usize {
    rayon::current_num_threads() * CAPACITY_MULTIPLIER
}

/// Returns an adaptive work queue capacity based on the average file size.
///
/// Small files (< 64 KiB) are CPU/syscall-bound, so a deeper queue (8x
/// thread count) keeps workers saturated despite per-file overhead. Large
/// files (> 1 MiB) are I/O-bound, so a shallow queue (2x) avoids wasting
/// memory. Medium files interpolate to 4x.
///
/// # Arguments
///
/// * `avg_file_size` - Average file size in bytes across the transfer set.
///   Use 0 or `None`-equivalent when unknown to get the default 2x multiplier.
///
/// # Examples
///
/// ```
/// use engine::concurrent_delta::work_queue;
///
/// // Many small config files - deep queue to avoid worker starvation.
/// let depth = work_queue::adaptive_queue_depth(4096);
/// assert!(depth >= rayon::current_num_threads() * 4);
///
/// // Large media files - shallow queue, I/O-bound.
/// let depth = work_queue::adaptive_queue_depth(10_000_000);
/// assert!(depth <= rayon::current_num_threads() * 4);
/// ```
#[must_use]
pub fn adaptive_queue_depth(avg_file_size: u64) -> usize {
    let threads = rayon::current_num_threads();
    let multiplier = if avg_file_size < SMALL_FILE_THRESHOLD {
        SMALL_FILE_MULTIPLIER
    } else if avg_file_size > LARGE_FILE_THRESHOLD {
        LARGE_FILE_MULTIPLIER
    } else {
        MEDIUM_FILE_MULTIPLIER
    };
    threads * multiplier
}
