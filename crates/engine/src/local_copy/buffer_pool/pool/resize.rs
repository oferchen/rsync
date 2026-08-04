//! Adaptive soft-capacity resizing for [`BufferPool`].
//!
//! Drives the pressure-tracker feedback loop: periodically evaluates
//! hit/miss pressure and grows or shrinks the pool's soft capacity,
//! deallocating excess buffers on shrink. Capacity updates are atomic
//! stores and queue mutations are lock-free pops.
//!
//! When a throughput-governor actuator is subscribed
//! ([`with_governor_actuator`](BufferPool::with_governor_actuator)), the local
//! grow target is composed with the governor's published ceiling via
//! **conservative-min** before it is applied: the pool grows to
//! `min(local_target, governor_ceiling)` and so never grows more aggressively
//! than either the local pressure tracker or the governor wants. The two
//! sources are folded into a single [`ResizeAction`] before any atomic store,
//! so each evaluation still performs at most one resize.

use std::sync::atomic::Ordering;

use super::super::allocator::BufferAllocator;
use super::super::pressure::{PressureTracker, ResizeAction};
use super::BufferPool;

impl<A: BufferAllocator> BufferPool<A> {
    /// Evaluates pressure statistics and applies resize if warranted.
    ///
    /// The local [`PressureTracker`] recommendation is composed with the
    /// governor's buffer-pool ceiling by
    /// [`compose_with_governor`](Self::compose_with_governor) into a single
    /// action, so a subscribed governor and the local tracker never each apply
    /// their own resize in one pass.
    ///
    /// Capacity updates are atomic stores; the queue mutations on shrink
    /// are lock-free [`ArrayQueue::pop`](crossbeam_queue::ArrayQueue::pop) calls.
    /// Concurrent acquires may observe an intermediate state during shrink (a
    /// brief window where the queue still holds buffers above the new soft
    /// cap), but the extras are reclaimed on the next return.
    pub(super) fn maybe_resize(&self, pressure: &PressureTracker) {
        if !pressure.should_check() {
            return;
        }

        let current_capacity = self.soft_capacity.load(Ordering::Relaxed);
        let available = self.buffers.len();

        let local = pressure.evaluate(current_capacity, available);
        match self.compose_with_governor(local, current_capacity) {
            ResizeAction::Hold => {}
            ResizeAction::Grow(new_capacity) => {
                self.soft_capacity.store(new_capacity, Ordering::Relaxed);
                self.total_growths.fetch_add(1, Ordering::Relaxed);
            }
            ResizeAction::Shrink(new_capacity) => {
                self.soft_capacity.store(new_capacity, Ordering::Relaxed);
                // Deallocate excess buffers beyond the new capacity.
                while self.buffers.len() > new_capacity {
                    match self.buffers.pop() {
                        Some(buf) => {
                            self.central_count.fetch_sub(1, Ordering::Relaxed);
                            if let Some(budget) = &self.byte_budget {
                                budget.release(buf.capacity());
                            }
                            self.allocator.deallocate(buf);
                        }
                        None => break,
                    }
                }
            }
        }
    }

    /// Composes the local resize recommendation with the governor's published
    /// buffer-pool ceiling via conservative-min.
    ///
    /// With no subscribed actuator, or when the governor publishes no ceiling,
    /// the local action passes through unchanged - the byte-identical
    /// pre-governor behaviour. Otherwise the ceiling only restrains growth: a
    /// local [`Grow`](ResizeAction::Grow) target is capped at the ceiling
    /// (`min(target, ceiling)`) and demoted to [`Hold`](ResizeAction::Hold) if
    /// that leaves nothing above the current capacity, so the governor can never
    /// push the pool larger than local demand nor force an extra shrink beyond
    /// what the local tracker already wants.
    fn compose_with_governor(&self, local: ResizeAction, current_capacity: usize) -> ResizeAction {
        let Some(ceiling) = self.governor_ceiling() else {
            return local;
        };
        match local {
            ResizeAction::Grow(target) => {
                let capped = target.min(ceiling).max(current_capacity);
                if capped > current_capacity {
                    ResizeAction::Grow(capped)
                } else {
                    ResizeAction::Hold
                }
            }
            other => other,
        }
    }

    /// The governor's current buffer-pool ceiling, or `None` when no actuator is
    /// subscribed or none is published.
    fn governor_ceiling(&self) -> Option<usize> {
        self.governor
            .as_ref()
            .and_then(|actuator| actuator.buffer_pool_ceiling())
    }
}
