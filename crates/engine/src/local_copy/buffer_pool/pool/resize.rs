//! Adaptive soft-capacity resizing for [`BufferPool`].
//!
//! Drives the pressure-tracker feedback loop: periodically evaluates
//! hit/miss pressure and grows or shrinks the pool's soft capacity,
//! deallocating excess buffers on shrink. Capacity updates are atomic
//! stores and queue mutations are lock-free pops.

use std::sync::atomic::Ordering;

use super::super::allocator::BufferAllocator;
use super::super::pressure::{PressureTracker, ResizeAction};
use super::BufferPool;

impl<A: BufferAllocator> BufferPool<A> {
    /// Evaluates pressure statistics and applies resize if warranted.
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

        match pressure.evaluate(current_capacity, available) {
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
}
