//! Iterator adapter over the bounded work queue receiver.

use std::sync::Arc;

use crossbeam_channel::Receiver;

use super::adaptive_semaphore::AdaptiveSemaphore;
use super::bounded::WorkQueueReceiver;
use crate::concurrent_delta::DeltaWork;

/// Iterator adapter over the work queue receiver.
///
/// Yields [`DeltaWork`] items until the sender drops and the queue drains.
/// Designed for use with `rayon::scope` based consumption.
pub struct WorkQueueIter {
    rx: Receiver<DeltaWork>,
    /// Dynamic-queue admission semaphore, or `None` for a fixed-bound queue.
    ///
    /// One permit is returned per yielded item so this bare-iterator
    /// consumption path also honours the acquire/release balance. Unlike the
    /// [`drain_parallel`](WorkQueueReceiver::drain_parallel) path, which
    /// releases when a work item finishes processing, this path releases on
    /// dequeue - the iterator has no notion of downstream completion, so a
    /// per-pull release is the balanced choice and can never leak a permit.
    release: Option<Arc<AdaptiveSemaphore>>,
}

impl IntoIterator for WorkQueueReceiver {
    type Item = DeltaWork;
    type IntoIter = WorkQueueIter;

    /// Converts the receiver into an iterator for `rayon::scope` consumption.
    ///
    /// The returned iterator yields items until the sender is dropped and the
    /// queue is drained.
    fn into_iter(self) -> WorkQueueIter {
        WorkQueueIter {
            rx: self.rx,
            release: self.release,
        }
    }
}

impl Iterator for WorkQueueIter {
    type Item = DeltaWork;

    fn next(&mut self) -> Option<DeltaWork> {
        let item = self.rx.recv().ok();
        if item.is_some() {
            // Return the admission permit acquired by the matching `send`.
            // No-op for a fixed-bound queue (`release` is `None`).
            if let Some(semaphore) = &self.release {
                semaphore.release();
            }
        }
        item
    }
}
