//! Threshold-gated delta pipeline that auto-selects sequential or parallel mode.

use std::io;

use engine::concurrent_delta::{DeltaResult, DeltaWork};

use super::{
    DEFAULT_PARALLEL_THRESHOLD, ParallelDeltaPipeline, ReceiverDeltaPipeline,
    SequentialDeltaPipeline,
};

/// Mode tracking for the threshold pipeline.
pub(super) enum ThresholdMode {
    /// Buffering work items until the threshold is reached.
    Buffering(Vec<DeltaWork>),
    /// Delegating to a parallel pipeline (threshold reached).
    ///
    /// Boxed so the enum's size is dominated by the small buffering variant
    /// rather than the much larger pipeline (which now carries the adaptive
    /// queue controller), keeping `clippy::large_enum_variant` quiet.
    Parallel(Box<ParallelDeltaPipeline>),
}

/// Threshold-gated delta pipeline that auto-selects sequential or parallel mode.
///
/// Buffers submitted work items until either:
/// - The buffer reaches the threshold, at which point a [`ParallelDeltaPipeline`]
///   is created and all buffered items are flushed into it.
/// - [`flush`](ReceiverDeltaPipeline::flush) is called before the threshold,
///   in which case items are processed sequentially.
///
/// This follows the threshold-based dual-path pattern used throughout the
/// codebase (e.g., `ParallelThresholds` in the receiver). For small
/// transfers, the overhead of spawning threads and channels exceeds the
/// benefit of parallelism.
///
/// # Default Threshold
///
/// [`DEFAULT_PARALLEL_THRESHOLD`] = 64, matching the receiver's
/// default stat threshold from `ParallelThresholds`.
pub struct ThresholdDeltaPipeline {
    /// Number of items required to switch to parallel mode.
    pub(super) threshold: usize,
    /// Current operating mode.
    pub(super) mode: ThresholdMode,
    /// When `true`, the parallel pipeline bypasses reorder buffering.
    bypass_reorder: bool,
}

impl std::fmt::Debug for ThresholdDeltaPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode_label = match &self.mode {
            ThresholdMode::Buffering(buf) => format!("Buffering({})", buf.len()),
            ThresholdMode::Parallel(_) => "Parallel".to_string(),
        };
        f.debug_struct("ThresholdDeltaPipeline")
            .field("threshold", &self.threshold)
            .field("mode", &mode_label)
            .field("bypass_reorder", &self.bypass_reorder)
            .finish()
    }
}

impl ThresholdDeltaPipeline {
    /// Creates a threshold pipeline with the given threshold.
    #[must_use]
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            mode: ThresholdMode::Buffering(Vec::new()),
            bypass_reorder: false,
        }
    }

    /// Creates a threshold pipeline with [`DEFAULT_PARALLEL_THRESHOLD`].
    #[must_use]
    pub fn with_default_threshold() -> Self {
        Self::new(DEFAULT_PARALLEL_THRESHOLD)
    }

    /// Creates a threshold pipeline that bypasses reorder buffering.
    ///
    /// When the threshold is reached and parallel mode activates, the
    /// internal [`ParallelDeltaPipeline`] delivers results in completion
    /// order rather than submission order. This eliminates reorder overhead
    /// when strict file-list ordering is unnecessary.
    #[must_use]
    pub fn new_bypass(threshold: usize) -> Self {
        Self {
            threshold,
            mode: ThresholdMode::Buffering(Vec::new()),
            bypass_reorder: true,
        }
    }

    /// Promotes from buffering to parallel mode, flushing buffered items.
    ///
    /// Sizes the work queue using [`ParallelDeltaPipeline::new_adaptive`] with
    /// the average target file size computed from the buffered items, so
    /// small-file workloads get a deeper queue (8x cores) to keep workers
    /// saturated, while large-file workloads stay at 2x to bound memory.
    fn promote_to_parallel(&mut self, buffered: Vec<DeltaWork>) -> io::Result<()> {
        let worker_count = rayon::current_num_threads();
        let avg_target_size = average_target_size(&buffered);
        let mut parallel = if self.bypass_reorder {
            ParallelDeltaPipeline::new_bypass_adaptive(worker_count, avg_target_size)
        } else {
            ParallelDeltaPipeline::new_adaptive(worker_count, avg_target_size)
        };
        for item in buffered {
            parallel.submit_work(item)?;
        }
        self.mode = ThresholdMode::Parallel(Box::new(parallel));
        Ok(())
    }
}

/// Returns the arithmetic mean of `target_size()` across `items`, or 0 when
/// the slice is empty. Saturating-add so a long tail of very large files
/// cannot overflow the u128 accumulator before the division.
pub(super) fn average_target_size(items: &[DeltaWork]) -> u64 {
    if items.is_empty() {
        return 0;
    }
    let total: u128 = items
        .iter()
        .map(|w| u128::from(w.target_size()))
        .fold(0u128, |a, b| a.saturating_add(b));
    let avg = total / items.len() as u128;
    u64::try_from(avg).unwrap_or(u64::MAX)
}

impl ReceiverDeltaPipeline for ThresholdDeltaPipeline {
    fn submit_work(&mut self, work: DeltaWork) -> io::Result<()> {
        match &mut self.mode {
            ThresholdMode::Buffering(buf) => {
                buf.push(work);
                if buf.len() >= self.threshold {
                    let buffered = std::mem::take(buf);
                    self.promote_to_parallel(buffered)?;
                }
                Ok(())
            }
            ThresholdMode::Parallel(par) => par.submit_work(work),
        }
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        match &mut self.mode {
            ThresholdMode::Buffering(_) => None,
            ThresholdMode::Parallel(par) => par.poll_result(),
        }
    }

    fn flush(self: Box<Self>) -> Vec<DeltaResult> {
        match self.mode {
            ThresholdMode::Buffering(buffered) => {
                if buffered.is_empty() {
                    return Vec::new();
                }
                // Below threshold - process sequentially.
                let mut seq = SequentialDeltaPipeline::new();
                for item in buffered {
                    // Dispatch is infallible for sequential pipeline.
                    let _ = seq.submit_work(item);
                }
                Box::new(seq).flush()
            }
            ThresholdMode::Parallel(par) => par.flush(),
        }
    }
}
