//! Page-aligned buffer pool for unbuffered I/O backends.
//!
//! Windows `FILE_FLAG_NO_BUFFERING` and Linux `O_DIRECT` both require the
//! caller's buffer to start on a sector boundary. The system page size is
//! always a multiple of the sector size on every filesystem we support, so
//! allocating page-aligned memory satisfies the constraint without having to
//! query the underlying volume.
//!
//! The standard heap allocator that backs the regular [`BufferPool`] makes
//! no alignment guarantees beyond `align_of::<u8>() == 1`. Submitting such a
//! buffer to a no-buffering write forces the kernel to bounce-copy the data
//! through an aligned scratch buffer it allocates on the caller's behalf,
//! defeating the purpose of bypassing the system cache. This module exposes
//! a separate pool whose buffers are guaranteed page-aligned (allocated via
//! [`fast_io::PageAlignedBuffer`]) so the IOCP writer can hand them directly
//! to overlapped `WriteFile` calls without any bounce copy.
//!
//! The pool mirrors the lock-free hot path used by [`BufferPool`] (a fixed
//! [`crossbeam_queue::ArrayQueue`] guarded by an atomic admission counter)
//! to keep the acquire/return cost wait-free under rayon concurrency. Hard
//! capacity is enforced on return: the queue never grows beyond
//! `soft_capacity`, so the worst-case memory budget is
//! `soft_capacity * round_up_to_page(buffer_size)`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crossbeam_queue::ArrayQueue;

use fast_io::{PageAlignedBuffer, round_up_to_page};

/// Default fixed capacity for the lock-free central queue.
///
/// Mirrors [`super::pool`]'s convention so the two pools share the same
/// reservoir sizing intuition.
const DEFAULT_QUEUE_CAPACITY: usize = 64;

fn queue_capacity(max_buffers: usize) -> usize {
    max_buffers.max(DEFAULT_QUEUE_CAPACITY).max(1)
}

/// Lock-free pool of page-aligned [`PageAlignedBuffer`] instances.
///
/// Use this in place of [`BufferPool`](super::BufferPool) whenever the
/// consumer submits the buffer to an I/O backend that requires sector
/// alignment - notably Windows IOCP writers opened with
/// `FILE_FLAG_NO_BUFFERING`. The two pools are deliberately separate
/// because their buffer types are not interchangeable: regular `Vec<u8>`
/// memory cannot be freed through `VirtualFree`, and `PageAlignedBuffer`
/// memory cannot be safely re-typed as `Vec<u8>` without an allocator-layout
/// mismatch.
///
/// Each buffer's capacity is rounded up to the next page boundary at
/// construction time; the configured `buffer_size` is treated as a minimum.
#[derive(Debug)]
pub struct PageAlignedBufferPool {
    buffers: ArrayQueue<PageAlignedBuffer>,
    central_count: AtomicUsize,
    soft_capacity: usize,
    buffer_size: usize,
    /// Cumulative count of acquire operations satisfied from the pool
    /// (no fresh allocation needed).
    hits: AtomicU64,
    /// Cumulative count of acquire operations that allocated a fresh buffer.
    misses: AtomicU64,
    /// Cumulative count of bounce-buffer copies avoided.
    ///
    /// The IOCP writer increments this counter every time it submits a
    /// page-aligned buffer to a no-buffering handle instead of letting the
    /// kernel allocate an aligned scratch buffer and memcpy the caller's
    /// data into it. Exposed via [`bounce_copies_avoided`](Self::bounce_copies_avoided)
    /// for benchmarks and the verbose status output.
    bounce_copies_avoided: AtomicU64,
}

impl PageAlignedBufferPool {
    /// Creates a new pool with the given soft capacity and buffer size.
    ///
    /// `buffer_size` is rounded up to the next page boundary internally;
    /// pass at least one full page or the value will be promoted to one.
    #[must_use]
    pub fn new(max_buffers: usize, buffer_size: usize) -> Self {
        let rounded = round_up_to_page(buffer_size);
        Self {
            buffers: ArrayQueue::new(queue_capacity(max_buffers)),
            central_count: AtomicUsize::new(0),
            soft_capacity: max_buffers.max(1),
            buffer_size: rounded,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            bounce_copies_avoided: AtomicU64::new(0),
        }
    }

    /// Returns the per-buffer byte capacity (always a page multiple).
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Returns the configured soft capacity.
    #[must_use]
    pub fn soft_capacity(&self) -> usize {
        self.soft_capacity
    }

    /// Returns the number of buffers currently retained in the pool.
    #[must_use]
    pub fn available(&self) -> usize {
        self.central_count.load(Ordering::Relaxed)
    }

    /// Returns the cumulative pool hits since construction.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Returns the cumulative pool misses (fresh allocations) since
    /// construction.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Returns the cumulative count of bounce-buffer copies avoided by
    /// handing aligned buffers to the unbuffered I/O backend.
    #[must_use]
    pub fn bounce_copies_avoided(&self) -> u64 {
        self.bounce_copies_avoided.load(Ordering::Relaxed)
    }

    /// Records that one page-aligned buffer was submitted to an unbuffered
    /// I/O operation - one bounce-buffer copy avoided.
    ///
    /// Called by the IOCP writer (or any other consumer that hands a buffer
    /// to a `FILE_FLAG_NO_BUFFERING` / `O_DIRECT` handle) each time a write
    /// or read is issued. Pure counter bump, safe to call from any thread.
    pub fn record_bounce_copy_avoided(&self) {
        self.bounce_copies_avoided.fetch_add(1, Ordering::Relaxed);
    }

    /// Acquires a buffer, allocating a fresh one on pool miss.
    #[must_use]
    pub fn acquire(self: &Arc<Self>) -> PageAlignedBufferGuard {
        let buffer = match self.buffers.pop() {
            Some(buf) => {
                self.central_count.fetch_sub(1, Ordering::Relaxed);
                self.hits.fetch_add(1, Ordering::Relaxed);
                buf
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                PageAlignedBuffer::new(self.buffer_size)
            }
        };
        PageAlignedBufferGuard {
            buffer: Some(buffer),
            pool: Arc::clone(self),
        }
    }

    fn return_buffer(&self, buffer: PageAlignedBuffer) {
        // Reject buffers that no longer match the configured size - the
        // pool would otherwise hand back the wrong capacity on the next
        // acquire. Mismatched buffers are simply dropped.
        if buffer.capacity() != self.buffer_size {
            return;
        }

        // Atomic admission counter: only push when the queue slot count is
        // below the soft cap. This avoids races where multiple concurrent
        // returns each observe `len() < capacity` and all push, overshooting
        // the cap.
        let mut current = self.central_count.load(Ordering::Relaxed);
        loop {
            if current >= self.soft_capacity {
                return;
            }
            match self.central_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }

        if self.buffers.push(buffer).is_err() {
            // The push only fails when the fixed-capacity queue is full -
            // unwind the admission counter so subsequent returns can try
            // again, and let the buffer drop.
            self.central_count.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// RAII guard that returns its buffer to the pool on drop.
///
/// Provides `&[u8]` / `&mut [u8]` access through dedicated accessors rather
/// than `Deref` because [`PageAlignedBuffer`] is not a slice newtype; the
/// extra explicitness mirrors the FFI use-case where callers need the raw
/// pointer.
#[derive(Debug)]
pub struct PageAlignedBufferGuard {
    buffer: Option<PageAlignedBuffer>,
    pool: Arc<PageAlignedBufferPool>,
}

impl PageAlignedBufferGuard {
    /// Returns the buffer as an immutable byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.buffer
            .as_ref()
            .expect("guard buffer present until drop")
            .as_slice()
    }

    /// Returns the buffer as a mutable byte slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer
            .as_mut()
            .expect("guard buffer present until drop")
            .as_mut_slice()
    }

    /// Returns the buffer capacity in bytes (page-aligned multiple).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffer
            .as_ref()
            .expect("guard buffer present until drop")
            .capacity()
    }

    /// Returns a raw pointer to the buffer for FFI use.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.buffer
            .as_ref()
            .expect("guard buffer present until drop")
            .as_ptr()
    }

    /// Returns a raw mutable pointer to the buffer for FFI use.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.buffer
            .as_mut()
            .expect("guard buffer present until drop")
            .as_mut_ptr()
    }
}

impl Drop for PageAlignedBufferGuard {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use fast_io::page_size;

    use super::*;

    #[test]
    fn buffer_is_page_aligned() {
        let pool = Arc::new(PageAlignedBufferPool::new(2, 64 * 1024));
        let buf = pool.acquire();
        let addr = buf.as_ptr() as usize;
        assert_eq!(
            addr % page_size(),
            0,
            "expected page-aligned pointer, got {addr:#x}"
        );
    }

    #[test]
    fn buffer_size_rounds_up_to_page() {
        let pool = Arc::new(PageAlignedBufferPool::new(1, 1));
        assert_eq!(pool.buffer_size(), page_size());
        let buf = pool.acquire();
        assert_eq!(buf.capacity(), page_size());
    }

    #[test]
    fn drop_returns_buffer_to_pool_for_reuse() {
        let pool = Arc::new(PageAlignedBufferPool::new(2, 8 * 1024));
        let first_ptr = {
            let buf = pool.acquire();
            buf.as_ptr() as usize
        };
        assert_eq!(pool.misses(), 1);
        assert_eq!(pool.available(), 1);

        // Second acquire should reuse the returned buffer instead of
        // allocating fresh: same pointer, miss counter unchanged.
        let second = pool.acquire();
        assert_eq!(second.as_ptr() as usize, first_ptr);
        assert_eq!(pool.misses(), 1);
        assert_eq!(pool.hits(), 1);
    }

    #[test]
    fn pool_respects_soft_capacity() {
        let pool = Arc::new(PageAlignedBufferPool::new(2, 4 * 1024));
        let b1 = pool.acquire();
        let b2 = pool.acquire();
        let b3 = pool.acquire();
        drop(b1);
        drop(b2);
        drop(b3);
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn bounce_copy_counter_is_zero_initially() {
        let pool = PageAlignedBufferPool::new(1, 4096);
        assert_eq!(pool.bounce_copies_avoided(), 0);
    }

    #[test]
    fn bounce_copy_counter_increments() {
        let pool = PageAlignedBufferPool::new(1, 4096);
        pool.record_bounce_copy_avoided();
        pool.record_bounce_copy_avoided();
        pool.record_bounce_copy_avoided();
        assert_eq!(pool.bounce_copies_avoided(), 3);
    }

    #[test]
    fn buffer_writes_are_visible_after_drop_and_reacquire() {
        let pool = Arc::new(PageAlignedBufferPool::new(1, 4 * 1024));
        {
            let mut buf = pool.acquire();
            buf.as_mut_slice()[0..4].copy_from_slice(b"\xDE\xAD\xBE\xEF");
        }
        let buf = pool.acquire();
        assert_eq!(&buf.as_slice()[0..4], b"\xDE\xAD\xBE\xEF");
    }

    #[test]
    fn pool_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PageAlignedBufferPool>();
        assert_send_sync::<PageAlignedBufferGuard>();
    }
}
