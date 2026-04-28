//! Optional multi-producer support for [`WorkQueueSender`].
//!
//! Gated behind the `multi-producer` cargo feature. When enabled, cloning a
//! sender allows multiple producer threads to feed the work queue concurrently,
//! turning the pipeline from SPMC into MPMC. Each clone shares the same
//! underlying bounded channel, so backpressure and capacity limits still apply.
//! Sequence numbering must be coordinated externally when multiple producers
//! are active.

use super::bounded::WorkQueueSender;

/// Cloning the sender enables multiple producer threads to feed the work queue
/// concurrently, turning the pipeline from SPMC into MPMC. Each clone shares
/// the same underlying bounded channel, so backpressure and capacity limits
/// still apply. Sequence numbering must be coordinated externally when multiple
/// producers are active.
impl Clone for WorkQueueSender {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}
