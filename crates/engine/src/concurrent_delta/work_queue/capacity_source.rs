//! Admission-capacity source for the bounded work queue.
//!
//! [`WorkQueueSender`](super::WorkQueueSender) admits work items subject to a
//! capacity limit. Historically that limit was a single fixed bound baked into
//! the underlying [`crossbeam_channel::bounded`] channel. [`CapacitySource`]
//! abstracts where that limit comes from so the queue can, in a later change,
//! draw its admission capacity from a dynamic source instead:
//!
//! - [`CapacitySource::Fixed`] - the original behaviour. The crossbeam channel
//!   bound is the sole admission gate; the sender adds no extra accounting, so
//!   this path is byte-for-byte identical to the pre-abstraction sender.
//! - [`CapacitySource::Dynamic`] - admission is gated by an
//!   [`AdaptiveSemaphore`] whose ceiling may grow or shrink at runtime between a
//!   configured `min` and `max`. The channel is opened at `max` so it never
//!   becomes the binding constraint; the semaphore alone bounds in-flight work.
//!
//! # Scope
//!
//! This module holds only the abstraction and the sender-side acquire. The
//! matching release lives on the receiver: `drain_parallel` and
//! `drain_parallel_into` return one permit when each item's task completes or
//! unwinds (`PermitGuard`), and the bare iterator returns one per dequeued
//! item. The ceiling is moved by
//! [`AdaptiveQueueController`](super::AdaptiveQueueController) or by the
//! throughput governor's `Rope`. No production transfer builds a dynamic queue
//! yet: `bounded_dynamic` is reached only through
//! `throughput::governed_work_queue` in `GovernorMode::Observe`, which no
//! transfer path calls. The dynamic path is therefore opt-in, and every live
//! queue uses [`CapacitySource::Fixed`].

use std::sync::Arc;

use super::adaptive_semaphore::AdaptiveSemaphore;

/// Where a [`WorkQueueSender`](super::WorkQueueSender) draws its admission
/// capacity from.
///
/// See the [module documentation](self) for the two variants and their
/// semantics.
#[derive(Clone)]
pub(super) enum CapacitySource {
    /// A fixed admission bound enforced solely by the crossbeam channel.
    ///
    /// This is the original, default behaviour: the sender performs no extra
    /// capacity accounting and backpressure comes entirely from the bounded
    /// channel filling up.
    Fixed,
    /// A dynamic admission bound enforced by an [`AdaptiveSemaphore`].
    ///
    /// The semaphore ceiling may move between `min` and `max` at runtime. The
    /// channel is opened at `max` so admission is governed by the semaphore
    /// rather than the channel bound.
    Dynamic {
        /// The resizable semaphore that gates admission.
        semaphore: Arc<AdaptiveSemaphore>,
        /// The lowest ceiling the semaphore may be resized to.
        min: usize,
        /// The highest ceiling the semaphore may be resized to; also the fixed
        /// capacity of the backing channel.
        max: usize,
    },
}

impl CapacitySource {
    /// Acquires admission for one work item, blocking until capacity is free.
    ///
    /// For [`Fixed`](Self::Fixed) this is a no-op: the caller relies on the
    /// bounded channel's own blocking send for backpressure. For
    /// [`Dynamic`](Self::Dynamic) this blocks on the semaphore until a permit is
    /// available, applying backpressure ahead of the (over-provisioned) channel.
    pub(super) fn acquire(&self) {
        if let CapacitySource::Dynamic { semaphore, .. } = self {
            semaphore.acquire();
        }
    }
}
