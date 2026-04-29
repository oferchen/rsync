//! Iterator adapter over the bounded work queue receiver.

use crossbeam_channel::Receiver;

use super::bounded::WorkQueueReceiver;
use crate::concurrent_delta::DeltaWork;

/// Iterator adapter over the work queue receiver.
///
/// Yields [`DeltaWork`] items until the sender drops and the queue drains.
/// Designed for use with `rayon::scope` based consumption.
pub struct WorkQueueIter {
    rx: Receiver<DeltaWork>,
}

impl IntoIterator for WorkQueueReceiver {
    type Item = DeltaWork;
    type IntoIter = WorkQueueIter;

    /// Converts the receiver into an iterator for `rayon::scope` consumption.
    ///
    /// The returned iterator yields items until the sender is dropped and the
    /// queue is drained.
    fn into_iter(self) -> WorkQueueIter {
        WorkQueueIter { rx: self.rx }
    }
}

impl Iterator for WorkQueueIter {
    type Item = DeltaWork;

    fn next(&mut self) -> Option<DeltaWork> {
        self.rx.recv().ok()
    }
}
