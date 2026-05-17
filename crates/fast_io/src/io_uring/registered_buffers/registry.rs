//! [`RegisteredBufferGroup`] and the per-slot [`RegisteredBufferSlot`] handle.
//!
//! Owns the page-aligned buffer pool, the atomic free-bitset slot allocator,
//! and the lifetime contract between the user-side memory and the kernel-side
//! pinning. See the parent module docs for the full lifecycle and drop-order
//! invariants.

use std::alloc::{self, Layout};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use io_uring::IoUring as RawIoUring;

use super::stats::{RegisteredBufferStats, RegisteredBufferStatus};
use super::{MAX_REGISTERED_BUFFERS, page_size};

/// A group of page-aligned buffers registered with an io_uring instance.
///
/// Each buffer is allocated with page alignment for optimal DMA and zero-copy
/// performance. The group tracks which buffer indices are available via an
/// atomic bitset, enabling lock-free checkout/return from multiple threads
/// (though io_uring submission itself is single-threaded).
///
/// # Safety invariants
///
/// - All buffer pointers remain valid and pinned until `drop()`.
/// - The `iovec` array passed to `register_buffers` references memory owned by
///   this struct. The kernel holds references to these pages until
///   `unregister_buffers` is called.
/// - Buffer indices returned by `checkout()` are guaranteed to be in-bounds.
///
/// # Telemetry
///
/// The group records `total_acquires` and `total_misses` counters, both
/// bumped on every [`checkout`](Self::checkout). A miss is a checkout that
/// returns `None` because every slot is in use, which forces the caller
/// to fall back to non-registered I/O. The counters are exposed via
/// [`stats`](Self::stats) and feed the adaptive sizing design described
/// in `docs/audits/io-uring-adaptive-buffer-sizing.md`.
pub struct RegisteredBufferGroup {
    /// Raw pointers to page-aligned buffer memory.
    pub(super) buffers: Vec<*mut u8>,
    /// Layout used for each buffer allocation (for deallocation).
    layout: Layout,
    /// Size of each individual buffer in bytes.
    pub(super) buffer_size: usize,
    /// Number of buffers in the group.
    count: usize,
    /// Atomic bitset tracking which buffer indices are free (1 = free, 0 = in use).
    /// Supports up to 64 buffers per word. Multiple words for larger counts.
    free_bitset: Vec<AtomicU64>,
    /// Total number of `checkout` calls (whether they succeeded or not).
    total_acquires: AtomicU64,
    /// Number of `checkout` calls that returned `None` because every slot
    /// was in use - a forced fallback to non-registered I/O.
    total_misses: AtomicU64,
}

// SAFETY: The raw pointers point to memory exclusively owned by this struct.
// No aliasing occurs because checkout/return ensures exclusive access per slot.
unsafe impl Send for RegisteredBufferGroup {}

// SAFETY: The atomic bitset provides thread-safe checkout/return. Buffer memory
// is only accessed by the holder of a checked-out slot index.
unsafe impl Sync for RegisteredBufferGroup {}

/// A checked-out buffer slot from a [`RegisteredBufferGroup`].
///
/// Provides access to the underlying buffer memory and the buffer index needed
/// for `ReadFixed`/`WriteFixed` SQEs. The slot is returned to the group on drop.
pub struct RegisteredBufferSlot<'a> {
    group: &'a RegisteredBufferGroup,
    /// The buffer index within the registered group (used as `buf_index` in SQEs).
    index: u16,
}

impl<'a> RegisteredBufferSlot<'a> {
    /// Returns the buffer index for use in `ReadFixed`/`WriteFixed` SQEs.
    #[inline]
    #[must_use]
    pub fn buf_index(&self) -> u16 {
        self.index
    }

    /// Returns a mutable pointer to the buffer memory.
    ///
    /// Callers must ensure writes stay within `buffer_size()` bounds.
    #[inline]
    #[must_use]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.group.buffers[self.index as usize]
    }

    /// Returns a const pointer to the buffer memory.
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.group.buffers[self.index as usize]
    }

    /// Returns the size of the buffer in bytes.
    #[inline]
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.group.buffer_size
    }

    /// Returns a slice view of the buffer up to `len` bytes.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len` bytes have been initialized (e.g.,
    /// after a successful `ReadFixed` completion with result >= `len`).
    #[inline]
    pub unsafe fn as_slice(&self, len: usize) -> &[u8] {
        debug_assert!(len <= self.group.buffer_size);
        let clamped = len.min(self.group.buffer_size);
        unsafe { std::slice::from_raw_parts(self.as_ptr(), clamped) }
    }

    /// Returns a mutable slice view of the buffer up to `len` bytes.
    ///
    /// # Safety
    ///
    /// The caller must ensure no other references to this buffer memory exist.
    #[inline]
    pub unsafe fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        debug_assert!(len <= self.group.buffer_size);
        let clamped = len.min(self.group.buffer_size);
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), clamped) }
    }
}

impl Drop for RegisteredBufferSlot<'_> {
    fn drop(&mut self) {
        self.group.return_slot(self.index);
    }
}

impl RegisteredBufferGroup {
    /// Creates a new group of page-aligned buffers and registers them with io_uring.
    ///
    /// Each buffer is `buffer_size` bytes, page-aligned for optimal kernel DMA.
    /// The `count` parameter specifies how many buffers to allocate (capped at 1024).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `count` is zero
    /// - `buffer_size` is zero
    /// - Memory allocation fails
    /// - `IORING_REGISTER_BUFFERS` fails (kernel limit exceeded, seccomp, etc.)
    pub fn new(ring: &RawIoUring, buffer_size: usize, count: usize) -> io::Result<Self> {
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer count must be > 0",
            ));
        }
        if buffer_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer size must be > 0",
            ));
        }
        if count > MAX_REGISTERED_BUFFERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("buffer count {count} exceeds kernel limit {MAX_REGISTERED_BUFFERS}"),
            ));
        }

        let page_size = page_size();
        let aligned_size = buffer_size.next_multiple_of(page_size);
        let layout = Layout::from_size_align(aligned_size, page_size).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid buffer layout: {e}"),
            )
        })?;

        let mut buffers = Vec::with_capacity(count);
        let mut iovecs = Vec::with_capacity(count);

        for _ in 0..count {
            // Safety: layout has non-zero size and valid alignment.
            let ptr = unsafe { alloc::alloc_zeroed(layout) };
            if ptr.is_null() {
                // Clean up already-allocated buffers.
                for prev in &buffers {
                    unsafe { alloc::dealloc(*prev, layout) };
                }
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "failed to allocate page-aligned buffer",
                ));
            }
            iovecs.push(libc::iovec {
                iov_base: ptr.cast::<libc::c_void>(),
                iov_len: aligned_size,
            });
            buffers.push(ptr);
        }

        // Register all buffers with the kernel.
        // Safety: iovec pointers are valid and will remain valid until
        // unregister_buffers is called in Drop. The buffers are owned by this
        // struct and not moved or freed until then.
        let register_result = unsafe { ring.submitter().register_buffers(&iovecs) };
        if let Err(e) = register_result {
            for ptr in &buffers {
                unsafe { alloc::dealloc(*ptr, layout) };
            }
            return Err(io::Error::other(format!(
                "IORING_REGISTER_BUFFERS failed: {e}"
            )));
        }

        // Initialize free bitset - all slots start as free (bit = 1).
        let words = count.div_ceil(64);
        let mut free_bitset = Vec::with_capacity(words);
        for i in 0..words {
            let bits_in_word = if i < words - 1 {
                64
            } else {
                let remainder = count % 64;
                if remainder == 0 { 64 } else { remainder }
            };
            // Set `bits_in_word` lower bits to 1.
            let mask = if bits_in_word == 64 {
                u64::MAX
            } else {
                (1u64 << bits_in_word) - 1
            };
            free_bitset.push(AtomicU64::new(mask));
        }

        Ok(Self {
            buffers,
            layout,
            buffer_size: aligned_size,
            count,
            free_bitset,
            total_acquires: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
        })
    }

    /// Attempts to register buffers with the given ring, returning `None` on failure.
    ///
    /// This is the preferred entry point for optional buffer registration - it
    /// never returns an error, making it safe to call speculatively.
    pub fn try_new(ring: &RawIoUring, buffer_size: usize, count: usize) -> Option<Self> {
        Self::new(ring, buffer_size, count).ok()
    }

    /// Attempts registration honoring an `enabled` flag, returning the group
    /// (if any) plus a [`RegisteredBufferStatus`] that distinguishes the
    /// "disabled by config" and "registration failed" paths.
    ///
    /// - When `enabled` is `false`: returns `(None, Disabled)` without calling
    ///   the kernel.
    /// - When `enabled` is `true` and registration succeeds: returns
    ///   `(Some(group), Enabled)`.
    /// - When `enabled` is `true` and the kernel rejects registration:
    ///   returns `(None, RegistrationFailed { reason })` carrying the
    ///   formatted `errno` for telemetry.
    pub fn try_new_with_status(
        ring: &RawIoUring,
        buffer_size: usize,
        count: usize,
        enabled: bool,
    ) -> (Option<Self>, RegisteredBufferStatus) {
        if !enabled {
            return (None, RegisteredBufferStatus::Disabled);
        }
        match Self::new(ring, buffer_size, count) {
            Ok(g) => (Some(g), RegisteredBufferStatus::Enabled),
            Err(e) => (
                None,
                RegisteredBufferStatus::RegistrationFailed {
                    reason: e.to_string(),
                },
            ),
        }
    }

    /// Returns the number of buffers in this group.
    #[inline]
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Returns the size of each buffer in bytes (page-aligned).
    #[inline]
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Returns the number of currently available (free) buffer slots.
    #[must_use]
    pub fn available(&self) -> usize {
        self.free_bitset
            .iter()
            .map(|word| word.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }

    /// Checks out a free buffer slot for use with `ReadFixed`/`WriteFixed`.
    ///
    /// Returns `None` if all slots are currently in use. The returned
    /// [`RegisteredBufferSlot`] automatically returns the slot on drop.
    ///
    /// Bumps `total_acquires` on entry and `total_misses` on the `None`
    /// return path. Both counters are `Relaxed` and feed [`stats`](Self::stats).
    pub fn checkout(&self) -> Option<RegisteredBufferSlot<'_>> {
        self.total_acquires.fetch_add(1, Ordering::Relaxed);
        for (word_idx, word) in self.free_bitset.iter().enumerate() {
            loop {
                let current = word.load(Ordering::Acquire);
                if current == 0 {
                    break; // No free bits in this word.
                }
                let bit = current.trailing_zeros();
                let mask = 1u64 << bit;
                // CAS to claim the bit (set it to 0).
                match word.compare_exchange_weak(
                    current,
                    current & !mask,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let index = (word_idx * 64 + bit as usize) as u16;
                        return Some(RegisteredBufferSlot { group: self, index });
                    }
                    Err(_) => continue, // Retry on contention.
                }
            }
        }
        self.total_misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Returns a snapshot of acquire / miss counters.
    ///
    /// The returned [`RegisteredBufferStats`] reads each atomic counter
    /// independently with `Relaxed` ordering. Individual values are
    /// accurate but the snapshot is not strictly consistent across the
    /// two fields under concurrent updates - identical to the
    /// `BufferPoolStats` pattern in the engine's local-copy buffer pool.
    ///
    /// Used by the adaptive buffer sizer to drive an EMA-smoothed miss-rate
    /// signal. See `docs/audits/io-uring-adaptive-buffer-sizing.md` for the
    /// design.
    #[must_use]
    pub fn stats(&self) -> RegisteredBufferStats {
        RegisteredBufferStats {
            total_acquires: self.total_acquires.load(Ordering::Relaxed),
            total_misses: self.total_misses.load(Ordering::Relaxed),
        }
    }

    /// Returns a buffer slot to the free pool.
    fn return_slot(&self, index: u16) {
        let word_idx = index as usize / 64;
        let bit = index as usize % 64;
        let mask = 1u64 << bit;
        self.free_bitset[word_idx].fetch_or(mask, Ordering::Release);
    }

    /// Unregisters all buffers from the io_uring instance.
    ///
    /// Called by the ring owner before dropping the ring. If the ring is dropped
    /// first, the kernel automatically unregisters buffers - but explicit
    /// unregistration is preferred for deterministic cleanup.
    pub fn unregister(&self, ring: &RawIoUring) -> io::Result<()> {
        ring.submitter().unregister_buffers()
    }
}

impl Drop for RegisteredBufferGroup {
    fn drop(&mut self) {
        // Drop ordering invariant (see module docs): the ring fd is closed
        // before this Drop runs, which causes the kernel to release the
        // pinning on these buffer pages (io_uring_register(2) /
        // fs/io_uring.c:io_sqe_buffers_unregister). All we do here is free
        // the user-side memory.
        //
        // Panic safety: alloc::dealloc does not panic when called with a
        // matching layout, so this Drop is safe during stack unwinding.
        // No double-panic / process abort can be triggered from here.
        //
        // SIGKILL: this code does not run on forced termination, but the
        // kernel reclaims both the ring fd and the registered pages as part
        // of process teardown.
        for ptr in &self.buffers {
            // Safety: each pointer was allocated with self.layout via
            // alloc::alloc_zeroed and has not been freed yet. We own all
            // buffers exclusively at this point - no slot can outlive the
            // group because RegisteredBufferSlot borrows &self.
            unsafe { alloc::dealloc(*ptr, self.layout) };
        }
    }
}
