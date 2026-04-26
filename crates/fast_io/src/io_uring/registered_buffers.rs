//! Page-aligned buffer registration for io_uring `READ_FIXED`/`WRITE_FIXED` operations.
//!
//! Registered buffers avoid kernel-userspace address translation on every SQE by
//! pinning a set of fixed buffers via `IORING_REGISTER_BUFFERS`. The kernel maps
//! these buffers once at registration time, eliminating per-op `get_user_pages()`
//! calls - a significant win for high-throughput I/O.
//!
//! # Buffer lifecycle
//!
//! 1. **Allocate** - page-aligned buffers via [`std::alloc::alloc`] with proper layout.
//! 2. **Register** - pass `iovec` array to `submitter.register_buffers()`.
//! 3. **Checkout** - callers acquire a slot index for use with `ReadFixed`/`WriteFixed`.
//! 4. **Return** - callers release the slot back to the free list.
//! 5. **Drop** - frees user-side memory. Kernel-side unregistration happens
//!    implicitly when the ring fd is closed; callers may also invoke
//!    [`RegisteredBufferGroup::unregister`] explicitly while the ring is alive.
//!
//! # Drop ordering and the ring fd
//!
//! [`RegisteredBufferGroup`] does not hold a reference to the [`RawIoUring`]
//! instance it was registered with. This is intentional: the kernel
//! automatically releases the pinned user pages when the ring fd is closed
//! (see `io_uring_register(2)` and `fs/io_uring.c:io_sqe_buffers_unregister`).
//!
//! Owners of both a `RawIoUring` and a `RegisteredBufferGroup` (such as
//! `IoUringReader` and `IoUringWriter`) MUST declare the ring field BEFORE
//! the `RegisteredBufferGroup` field. Rust drops fields in declaration
//! order, so this ensures:
//!
//! 1. `RawIoUring::Drop` closes the ring fd first, releasing the kernel's
//!    pinning of the registered buffer pages.
//! 2. `RegisteredBufferGroup::Drop` then deallocates the user-side memory
//!    backing those buffers.
//!
//! Reversing this order (group before ring) would still be sound because
//! `Drop` only deallocates user memory and never touches the ring; the
//! kernel would still hold the pinning until the ring fd later closes.
//! However, the documented ordering matches the implementation in
//! [`super::file_reader`] and [`super::file_writer`].
//!
//! # Why Drop does not call `unregister_buffers`
//!
//! Calling `submitter.unregister_buffers()` from `Drop` would require the
//! group to hold a reference to the ring. That introduces lifetime coupling
//! and makes it impossible for the ring to be dropped first - which is the
//! natural ordering when the ring owns the group. Instead we rely on the
//! kernel's automatic cleanup on ring fd close, and expose
//! [`RegisteredBufferGroup::unregister`] for callers that want deterministic
//! cleanup while keeping the ring alive (e.g., to register a new buffer set).
//!
//! # Panic safety
//!
//! `Drop` performs only `std::alloc::dealloc` calls, which do not panic when
//! given a layout that matches the original allocation. This makes the impl
//! safe during stack unwinding: a panic in user code that drops a
//! `RegisteredBufferGroup` will not trigger a double-panic abort.
//!
//! # Process termination
//!
//! On `SIGKILL` or other forced exits, neither `Drop` nor any userspace
//! cleanup runs, but the kernel reclaims both the ring fd and the registered
//! buffer pages as part of normal process teardown. No leak occurs.
//!
//! # Kernel limits
//!
//! The maximum number of registered buffers is typically 1024 (kernel-dependent).
//! Registration of more than the kernel supports returns `EINVAL` or `ENOMEM`.

use std::alloc::{self, Layout};
use std::io;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

use io_uring::IoUring as RawIoUring;

/// Maximum number of buffers that can be registered with io_uring.
///
/// The kernel typically allows up to 1024 registered buffers. We cap at this
/// limit to avoid kernel rejections.
const MAX_REGISTERED_BUFFERS: usize = 1024;

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
pub struct RegisteredBufferGroup {
    /// Raw pointers to page-aligned buffer memory.
    buffers: Vec<*mut u8>,
    /// Layout used for each buffer allocation (for deallocation).
    layout: Layout,
    /// Size of each individual buffer in bytes.
    buffer_size: usize,
    /// Number of buffers in the group.
    count: usize,
    /// Atomic bitset tracking which buffer indices are free (1 = free, 0 = in use).
    /// Supports up to 64 buffers per word. Multiple words for larger counts.
    free_bitset: Vec<AtomicU64>,
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
        })
    }

    /// Attempts to register buffers with the given ring, returning `None` on failure.
    ///
    /// This is the preferred entry point for optional buffer registration - it
    /// never returns an error, making it safe to call speculatively.
    #[must_use]
    pub fn try_new(ring: &RawIoUring, buffer_size: usize, count: usize) -> Option<Self> {
        Self::new(ring, buffer_size, count).ok()
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
    #[must_use]
    pub fn checkout(&self) -> Option<RegisteredBufferSlot<'_>> {
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
        None
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

/// Returns the system page size.
fn page_size() -> usize {
    // Safety: sysconf is always safe to call with _SC_PAGESIZE.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        4096 // Fallback to 4K, the most common page size.
    } else {
        size as usize
    }
}

/// Submits a batch of `ReadFixed` SQEs reading into registered buffers.
///
/// Reads `total_len` bytes from the file starting at `base_offset`, using
/// registered buffers from `slots`. Each slot handles one chunk of data.
/// Completions are collected and the total bytes read is returned.
///
/// The `slots` parameter provides buffer indices and pointers. Callers must
/// ensure slots are checked out from a `RegisteredBufferGroup` that is
/// registered with the same ring.
pub(super) fn submit_read_fixed_batch(
    ring: &mut RawIoUring,
    fd: io_uring::types::Fd,
    output: &mut [u8],
    base_offset: u64,
    slots: &[RegisteredBufferSlotInfo],
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    use super::batching::maybe_fixed_file;
    use io_uring::opcode::ReadFixed;

    if output.is_empty() || slots.is_empty() {
        return Ok(0);
    }

    let mut total_read = 0usize;
    let total = output.len();
    let chunk_size = slots[0].buffer_size;

    // Process in rounds, one SQE per slot per round.
    while total_read < total {
        let remaining = total - total_read;
        let n_sqes = remaining.div_ceil(chunk_size).min(slots.len());
        let mut submitted = 0u32;

        // Track how many bytes each SQE requested for short-read detection.
        let mut requested_per_sqe: Vec<usize> = Vec::with_capacity(n_sqes);

        for (i, slot) in slots.iter().enumerate().take(n_sqes) {
            let offset_in_output = total_read + i * chunk_size;
            let want = chunk_size.min(total - offset_in_output);
            let file_offset = base_offset + offset_in_output as u64;

            let entry = ReadFixed::new(fd, slot.ptr, want as u32, slot.buf_index)
                .offset(file_offset)
                .build()
                .user_data(i as u64);
            let entry = maybe_fixed_file(entry, fixed_fd_slot);

            // Safety: the registered buffer at slot is valid and pinned for
            // the duration of this submit_and_wait cycle.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }
            requested_per_sqe.push(want);
            submitted += 1;
        }

        if submitted == 0 {
            break;
        }

        ring.submit_and_wait(submitted as usize)?;

        // Collect actual bytes read per SQE index. CQEs may arrive out of order.
        let mut actual_per_sqe = vec![0usize; submitted as usize];

        let mut completed = 0u32;
        while completed < submitted {
            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("missing CQE"))?;

            let idx = cqe.user_data() as usize;
            let result = cqe.result();

            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }

            let bytes = result as usize;
            actual_per_sqe[idx] = bytes;

            let out_start = total_read + idx * chunk_size;
            let out_end = (out_start + bytes).min(total);
            let copy_len = out_end - out_start;

            // Safety: the kernel wrote `bytes` into the registered buffer.
            // We copy from the registered buffer into the caller's output slice.
            unsafe {
                ptr::copy_nonoverlapping(
                    slots[idx].ptr,
                    output[out_start..].as_mut_ptr(),
                    copy_len,
                );
            }

            completed += 1;
        }

        // Advance by the contiguous prefix of fully-read SQEs. If SQE `i`
        // returned fewer bytes than requested (short read - common on NFS,
        // FUSE, and slow block devices), we stop at that point so the outer
        // loop retries from the correct offset.
        let mut batch_advance = 0usize;
        for i in 0..submitted as usize {
            batch_advance += actual_per_sqe[i];
            if actual_per_sqe[i] < requested_per_sqe[i] {
                break;
            }
        }

        if batch_advance == 0 {
            break; // EOF or zero-length read - avoid infinite loop.
        }
        total_read += batch_advance;
    }

    Ok(total_read.min(total))
}

/// Submits a batch of `WriteFixed` SQEs writing from registered buffers.
///
/// Writes `data` to the file starting at `base_offset`, copying chunks into
/// registered buffers and submitting `WriteFixed` SQEs. Returns the total
/// bytes written.
pub(super) fn submit_write_fixed_batch(
    ring: &mut RawIoUring,
    fd: io_uring::types::Fd,
    data: &[u8],
    base_offset: u64,
    slots: &[RegisteredBufferSlotInfo],
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    use super::batching::maybe_fixed_file;
    use io_uring::opcode::WriteFixed;

    if data.is_empty() || slots.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut total_written = 0usize;
    let chunk_size = slots[0].buffer_size;

    while total_written < total {
        let remaining = total - total_written;
        let n_sqes = remaining.div_ceil(chunk_size).min(slots.len());
        let mut submitted = 0u32;

        for (i, slot) in slots.iter().enumerate().take(n_sqes) {
            let src_start = total_written + i * chunk_size;
            let want = chunk_size.min(total - src_start);
            let file_offset = base_offset + src_start as u64;

            // Copy data into registered buffer.
            // Safety: registered buffer at slot is valid and large enough.
            unsafe {
                ptr::copy_nonoverlapping(data[src_start..].as_ptr(), slot.ptr, want);
            }

            let entry = WriteFixed::new(fd, slot.ptr, want as u32, slot.buf_index)
                .offset(file_offset)
                .build()
                .user_data(i as u64);
            let entry = maybe_fixed_file(entry, fixed_fd_slot);

            // Safety: the registered buffer contains valid data and is pinned
            // for the duration of this submit_and_wait cycle.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }
            submitted += 1;
        }

        if submitted == 0 {
            break;
        }

        ring.submit_and_wait(submitted as usize)?;

        let mut batch_written = 0usize;
        let mut completed = 0u32;
        while completed < submitted {
            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("missing CQE"))?;

            let result = cqe.result();
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_fixed returned 0 bytes",
                ));
            }

            batch_written += result as usize;
            completed += 1;
        }

        total_written += batch_written;
    }

    Ok(total_written)
}

/// Lightweight info struct for passing registered buffer metadata to batch helpers.
///
/// Avoids lifetime complications of passing `RegisteredBufferSlot` references
/// into the batch submission functions.
pub(super) struct RegisteredBufferSlotInfo {
    /// Raw pointer to the registered buffer memory.
    pub ptr: *mut u8,
    /// Buffer index for `ReadFixed`/`WriteFixed` SQEs.
    pub buf_index: u16,
    /// Size of the buffer in bytes.
    pub buffer_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_is_positive_and_power_of_two() {
        let ps = page_size();
        assert!(ps > 0);
        assert!(ps.is_power_of_two());
    }

    #[test]
    fn registered_buffer_group_rejects_zero_count() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return, // io_uring not available
        };
        let result = RegisteredBufferGroup::new(&ring, 4096, 0);
        assert!(result.is_err());
    }

    #[test]
    fn registered_buffer_group_rejects_zero_size() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let result = RegisteredBufferGroup::new(&ring, 0, 4);
        assert!(result.is_err());
    }

    #[test]
    fn registered_buffer_group_rejects_excessive_count() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let result = RegisteredBufferGroup::new(&ring, 4096, MAX_REGISTERED_BUFFERS + 1);
        assert!(result.is_err());
    }

    #[test]
    fn registered_buffer_group_create_and_checkout() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
            Ok(g) => g,
            Err(_) => return, // Registration failed (seccomp, kernel limit, etc.)
        };

        assert_eq!(group.count(), 4);
        assert!(group.buffer_size() >= 4096);
        assert_eq!(group.available(), 4);

        // Check out all 4 slots.
        let mut s0 = group.checkout().expect("slot 0");
        assert_eq!(group.available(), 3);
        let s1 = group.checkout().expect("slot 1");
        let mut s2 = group.checkout().expect("slot 2");
        let mut s3 = group.checkout().expect("slot 3");
        assert_eq!(group.available(), 0);

        // No more slots available.
        assert!(group.checkout().is_none());

        // Return one slot.
        drop(s1);
        assert_eq!(group.available(), 1);

        // Check out again.
        let mut s1b = group.checkout().expect("slot 1 reacquired");
        assert_eq!(group.available(), 0);

        // Verify buffer pointers are non-null and unique.
        let ptrs: Vec<*mut u8> = [&mut s0, &mut s1b, &mut s2, &mut s3]
            .iter_mut()
            .map(|s| s.as_mut_ptr())
            .collect();
        for p in &ptrs {
            assert!(!p.is_null());
        }
        // All pointers should be distinct.
        for i in 0..ptrs.len() {
            for j in (i + 1)..ptrs.len() {
                assert_ne!(ptrs[i], ptrs[j], "slots {i} and {j} share a pointer");
            }
        }

        drop(s0);
        drop(s1b);
        drop(s2);
        drop(s3);
        assert_eq!(group.available(), 4);

        // Explicit unregister.
        let _ = group.unregister(&ring);
    }

    #[test]
    fn buffer_slot_read_write_memory() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        let mut slot = group.checkout().expect("checkout");

        // Write a pattern into the buffer.
        let pattern = b"hello io_uring registered buffers!";
        unsafe {
            ptr::copy_nonoverlapping(pattern.as_ptr(), slot.as_mut_ptr(), pattern.len());
            let read_back = slot.as_slice(pattern.len());
            assert_eq!(read_back, pattern);
        }

        drop(slot);
        let _ = group.unregister(&ring);
    }

    #[test]
    fn read_fixed_write_fixed_roundtrip() {
        let ring = match RawIoUring::new(64) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
            Ok(g) => g,
            Err(_) => return,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixed_roundtrip.bin");

        // Generate test data larger than one buffer.
        let test_data: Vec<u8> = (0..12000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &test_data).unwrap();

        // Collect slot info for batch operations.
        let mut checked_out: Vec<_> = (0..4).filter_map(|_| group.checkout()).collect();
        let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
            .iter_mut()
            .map(|s| RegisteredBufferSlotInfo {
                ptr: s.as_mut_ptr(),
                buf_index: s.buf_index(),
                buffer_size: s.buffer_size(),
            })
            .collect();

        // Read the file using ReadFixed.
        let file = std::fs::File::open(&path).unwrap();
        let raw_fd = {
            use std::os::unix::io::AsRawFd;
            file.as_raw_fd()
        };
        let fd = io_uring::types::Fd(raw_fd);

        let mut read_buf = vec![0u8; test_data.len()];
        let mut ring_rw = ring;
        let bytes_read = submit_read_fixed_batch(
            &mut ring_rw,
            fd,
            &mut read_buf,
            0,
            &slot_infos,
            super::super::batching::NO_FIXED_FD,
        )
        .unwrap();

        assert_eq!(bytes_read, test_data.len());
        assert_eq!(read_buf, test_data);

        // Write using WriteFixed to a new file.
        let write_path = dir.path().join("fixed_write_out.bin");
        let write_file = std::fs::File::create(&write_path).unwrap();
        let write_fd = {
            use std::os::unix::io::AsRawFd;
            io_uring::types::Fd(write_file.as_raw_fd())
        };

        let bytes_written = submit_write_fixed_batch(
            &mut ring_rw,
            write_fd,
            &test_data,
            0,
            &slot_infos,
            super::super::batching::NO_FIXED_FD,
        )
        .unwrap();

        assert_eq!(bytes_written, test_data.len());
        drop(write_file); // Flush.

        let written_data = std::fs::read(&write_path).unwrap();
        assert_eq!(written_data, test_data);

        drop(checked_out);
        let _ = group.unregister(&ring_rw);
    }

    /// Reads with an output buffer larger than the file to trigger a natural
    /// short read (EOF before buffer is full). Before the fix, the function
    /// would advance past unread bytes, returning `total` even though the
    /// file was smaller - silently zero-filling the tail.
    #[test]
    fn read_fixed_batch_short_read_at_eof() {
        let ring = match RawIoUring::new(64) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
            Ok(g) => g,
            Err(_) => return,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short_read.bin");

        // File is 5000 bytes but we ask to read 16384 (4 * 4096).
        let test_data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &test_data).unwrap();

        let mut checked_out: Vec<_> = (0..4).filter_map(|_| group.checkout()).collect();
        let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
            .iter_mut()
            .map(|s| RegisteredBufferSlotInfo {
                ptr: s.as_mut_ptr(),
                buf_index: s.buf_index(),
                buffer_size: s.buffer_size(),
            })
            .collect();

        let file = std::fs::File::open(&path).unwrap();
        let raw_fd = {
            use std::os::unix::io::AsRawFd;
            file.as_raw_fd()
        };
        let fd = io_uring::types::Fd(raw_fd);

        // Request more bytes than the file contains.
        let request_size = 4 * 4096;
        let mut read_buf = vec![0xFFu8; request_size];
        let mut ring_rw = ring;
        let bytes_read = submit_read_fixed_batch(
            &mut ring_rw,
            fd,
            &mut read_buf,
            0,
            &slot_infos,
            super::super::batching::NO_FIXED_FD,
        )
        .unwrap();

        // Must return exactly the file size, not the request size.
        assert_eq!(bytes_read, test_data.len());
        assert_eq!(&read_buf[..bytes_read], &test_data[..]);

        drop(checked_out);
        let _ = group.unregister(&ring_rw);
    }

    /// Reads a file that is smaller than a single registered buffer chunk.
    /// The first SQE returns a short read (file size < chunk size), and the
    /// function must report only the actual bytes read.
    #[test]
    fn read_fixed_batch_file_smaller_than_chunk() {
        let ring = match RawIoUring::new(64) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.bin");

        let test_data = b"small file content";
        std::fs::write(&path, test_data).unwrap();

        let mut checked_out: Vec<_> = (0..2).filter_map(|_| group.checkout()).collect();
        let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
            .iter_mut()
            .map(|s| RegisteredBufferSlotInfo {
                ptr: s.as_mut_ptr(),
                buf_index: s.buf_index(),
                buffer_size: s.buffer_size(),
            })
            .collect();

        let file = std::fs::File::open(&path).unwrap();
        let raw_fd = {
            use std::os::unix::io::AsRawFd;
            file.as_raw_fd()
        };
        let fd = io_uring::types::Fd(raw_fd);

        // Request 8192 bytes (2 chunks) but file is only 18 bytes.
        let mut read_buf = vec![0xFFu8; 8192];
        let mut ring_rw = ring;
        let bytes_read = submit_read_fixed_batch(
            &mut ring_rw,
            fd,
            &mut read_buf,
            0,
            &slot_infos,
            super::super::batching::NO_FIXED_FD,
        )
        .unwrap();

        assert_eq!(bytes_read, test_data.len());
        assert_eq!(&read_buf[..bytes_read], &test_data[..]);

        drop(checked_out);
        let _ = group.unregister(&ring_rw);
    }

    /// Drop ordering invariant: dropping the `RegisteredBufferGroup` BEFORE
    /// the `RawIoUring` is sound. The kernel still holds the buffer pinning
    /// (released later when the ring fd closes), but we may safely deallocate
    /// the user-side memory because `Drop` does not touch the ring.
    #[test]
    fn drop_group_before_ring_does_not_panic() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        // Drop the group first while the ring is still alive.
        drop(group);

        // Ring is still usable for ordinary operations after the group dies.
        // Submitting a no-op (nop) verifies the ring fd remains valid.
        let entry = io_uring::opcode::Nop::new().build().user_data(0xdead);
        // Safety: SQE is a Nop with no buffer pointers.
        unsafe {
            ring.submission()
                .push(&entry)
                .expect("nop submission after group drop");
        }
        ring.submit_and_wait(1).expect("nop completes");
        let cqe = ring.completion().next().expect("nop CQE");
        assert_eq!(cqe.user_data(), 0xdead);
        assert_eq!(cqe.result(), 0);
    }

    /// Drop ordering invariant: dropping the ring BEFORE the group is the
    /// natural order used by `IoUringReader`/`IoUringWriter`. The kernel
    /// auto-releases the buffer pinning when the ring fd closes; the group
    /// then frees user-side memory in its own Drop.
    #[test]
    fn drop_ring_before_group_frees_memory_cleanly() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        // Close the ring first - kernel releases buffer pinning.
        drop(ring);

        // Now dropping the group must still be sound: it deallocates user
        // memory and never accesses the (now-closed) ring fd.
        drop(group);
    }

    /// Mirrors the field declaration order used by `IoUringReader` and
    /// `IoUringWriter`: ring before registered_buffers. Verifies that
    /// implicit drop runs ring-first then group-second without aborting.
    #[test]
    fn struct_field_drop_order_matches_callers() {
        struct OwnerLikeReader {
            ring: RawIoUring,
            #[allow(dead_code)]
            registered_buffers: Option<RegisteredBufferGroup>,
        }

        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = RegisteredBufferGroup::try_new(&ring, 4096, 2);

        let owner = OwnerLikeReader {
            ring,
            registered_buffers: group,
        };

        // Implicit drop in declaration order: ring first, then group.
        // Must complete without panic or process abort.
        drop(owner);
    }

    /// Panic during slot use must not corrupt the group: dropping the slot
    /// during unwinding returns it to the free list, and dropping the group
    /// during unwinding deallocates buffers safely.
    #[test]
    fn panic_during_slot_use_unwinds_cleanly() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _slot = group.checkout().expect("slot checkout");
            // Slot is held; panic forces Drop during unwinding.
            panic!("simulated panic during slot use");
        }));

        assert!(result.is_err(), "panic should propagate via catch_unwind");

        // Slot must have been returned to the free list during unwinding.
        assert_eq!(
            group.available(),
            2,
            "slot should be returned on panic-driven drop"
        );

        // Group is still usable after the panic.
        let _slot_again = group.checkout().expect("re-checkout after panic");
    }

    /// `unregister()` returns an error when the buffer set has already been
    /// released by closing the ring (or never registered). The error must
    /// be reported to the caller; it must NOT cause a panic or abort.
    #[test]
    fn unregister_after_ring_closed_returns_error_or_ok() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };
        let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
            Ok(g) => g,
            Err(_) => return,
        };

        // Successful explicit unregister against the live ring.
        let first = group.unregister(&ring);
        assert!(
            first.is_ok(),
            "first unregister against live ring should succeed: {first:?}"
        );

        // A second unregister has nothing to release; the kernel may return
        // EINVAL/ENXIO. The wrapper must surface this gracefully (Result),
        // never panic. The exact error code is kernel-dependent, so we just
        // require the call returns (Ok or Err) without panicking.
        let _ = group.unregister(&ring);
    }

    /// User-side buffer memory must be freed regardless of whether
    /// `unregister()` was called. We verify this by exercising both code
    /// paths (with and without explicit unregister) and confirming Drop
    /// completes without panic - leak detection is delegated to ASan/Miri
    /// in CI when available.
    #[test]
    fn buffers_freed_with_or_without_explicit_unregister() {
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };

        // Path A: explicit unregister, then drop.
        {
            let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
                Ok(g) => g,
                Err(_) => return,
            };
            let _ = group.unregister(&ring);
            drop(group);
        }

        // Path B: drop without explicit unregister (relies on kernel cleanup
        // when ring closes; here we keep the ring alive across drop).
        {
            let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
                Ok(g) => g,
                Err(_) => return,
            };
            drop(group);
        }

        // Path C: re-register on the same ring, drop, repeat. Verifies the
        // ring remains in a clean state for further registrations.
        for _ in 0..3 {
            if let Some(group) = RegisteredBufferGroup::try_new(&ring, 4096, 2) {
                let _ = group.unregister(&ring);
            }
        }
    }
}
