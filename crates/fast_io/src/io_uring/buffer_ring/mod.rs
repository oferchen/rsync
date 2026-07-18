//! io_uring provided buffer ring (PBUF_RING) for zero-copy reads.
//!
//! PBUF_RING (introduced in Linux 5.19) allows the kernel to select buffers
//! from a shared ring at completion time, eliminating the need to pre-assign
//! buffers to individual SQEs. This enables true zero-copy reads where the
//! kernel writes directly into userspace-owned buffers without intermediate
//! copies.
//!
//! # How it works
//!
//! 1. Userspace allocates a contiguous buffer region and a ring of buffer
//!    descriptors via `mmap` of the io_uring fd with offset
//!    `IORING_OFF_PBUF_RING`.
//! 2. The ring is registered with the kernel via `IORING_REGISTER_PBUF_RING`.
//! 3. Read SQEs specify `IOSQE_BUFFER_SELECT` and a buffer group ID. The
//!    kernel picks an available buffer from the ring at completion time.
//! 4. The CQE flags contain the buffer ID (`IORING_CQE_F_BUFFER`) so
//!    userspace knows which buffer was used.
//! 5. After processing, userspace recycles the buffer by advancing the ring
//!    tail pointer.
//!
//! # Module layout
//!
//! - `allocator` - process-wide bgid allocator and counters.
//! - `registration` - kernel registration plumbing and version probe.
//! - this file - the [`BufferRing`] type plus its construction, recycling
//!   and `Drop` logic.
//!
//! # Kernel requirements
//!
//! - **Linux 5.19+** for `IORING_REGISTER_PBUF_RING` support.
//! - The kernel must not block io_uring via seccomp.
//!
//! # Runtime probe
//!
//! [`pbuf_ring_supported`] returns whether the running kernel can register a
//! provided buffer ring. The first call performs the `uname(2)` parse; the
//! result is cached in a process-wide `OnceLock` so subsequent calls are a
//! single relaxed atomic load. The check is necessary but not sufficient -
//! seccomp profiles, `IOSQE_REGISTER_PBUF_RING` rejection, or `-ENOMEM` can
//! still cause [`BufferRing::new`] to fail; callers should prefer
//! [`BufferRing::try_new`] for speculative use.
//!
//! # Fallback chain
//!
//! Each layer degrades independently so that PBUF_RING usage is best-effort:
//!
//! 1. **PBUF_RING** (Linux 5.19+, opcode 22) - completion-time buffer
//!    selection, the path documented in this module.
//! 2. **Classic provide-buffers** (Linux 5.6+, `IORING_OP_PROVIDE_BUFFERS`
//!    opcode 31, or `IORING_REGISTER_BUFFERS` opcode 0) - pre-registered
//!    buffer pool with per-SQE selection. See
//!    [`crate::io_uring::registered_buffers::RegisteredBufferGroup`].
//! 3. **Standard `read(2)` / `write(2)`** (any kernel) - the
//!    `traits::StdFileReader` / `traits::StdFileWriter` fallback used when
//!    io_uring is unavailable entirely.
//! 4. **Non-Linux io_uring stub** (`io_uring_stub.rs`) - returns `false` from
//!    [`pbuf_ring_supported`], `Err(Unsupported)` from [`BufferRing::new`],
//!    and `None` from [`BufferRing::try_new`].
//!
//! # References
//!
//! - `io_uring_register(2)` - `IORING_REGISTER_PBUF_RING` / `IORING_UNREGISTER_PBUF_RING`
//! - kernel source: `io_uring/kbuf.c` - `io_register_pbuf_ring()`
//! - audit: `docs/audits/iouring-pbuf-ring.md` (task #2043)

use std::io;
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::sync::atomic::{AtomicU16, Ordering};

use io_uring::IoUring as RawIoUring;

mod allocator;
mod registration;

pub use crate::io_uring_common::{
    BgidAllocError, BufferRingConfig, BufferRingError, buffer_id_from_cqe_flags,
};
pub use allocator::{
    BgidAllocator, BgidSessionStats, BgidSnapshot, bgid_exhausted_count, bgid_inflight,
    bgid_peak_used, bgid_snapshot,
};
pub use registration::{is_supported, pbuf_ring_supported};

use allocator::warn_bgid_fallback;
use registration::{
    IORING_OFF_PBUF_RING, IORING_REGISTER_PBUF_RING, IORING_UNREGISTER_PBUF_RING, IoUringBufReg,
    check_kernel_version,
};

/// Matches `struct io_uring_buf` from the kernel - one entry in the ring.
#[repr(C)]
struct IoUringBuf {
    addr: u64,
    len: u32,
    bid: u16,
    resv: u16,
}

/// Validates a [`BufferRingConfig`] for use with the Linux backend.
///
/// The plain-data [`BufferRingConfig`] lives in `io_uring_common`
/// so the non-Linux stub can expose the identical field layout; this
/// validator is the only Linux-only behaviour and stays here next to the
/// rest of the ring construction code.
fn validate_buffer_ring_config(c: &BufferRingConfig) -> Result<(), BufferRingError> {
    if c.ring_size == 0 || !c.ring_size.is_power_of_two() {
        return Err(BufferRingError::InvalidRingSize(c.ring_size));
    }
    if c.buffer_size == 0 {
        return Err(BufferRingError::InvalidBufferSize);
    }
    Ok(())
}

/// A provided buffer ring (PBUF_RING) registered with an io_uring instance.
///
/// Manages a ring of buffers that the kernel can select from at I/O
/// completion time, enabling zero-copy reads. The kernel writes directly
/// into these buffers and reports which buffer was used via CQE flags.
///
/// # Usage pattern
///
/// 1. Create a `BufferRing` with [`new`](Self::new).
/// 2. Submit read SQEs with `IOSQE_BUFFER_SELECT` and the matching
///    buffer group ID.
/// 3. On CQE completion, extract the buffer ID from CQE flags using
///    [`buffer_id_from_cqe_flags`].
/// 4. Process the data in the buffer.
/// 5. Call [`recycle_buffer`](Self::recycle_buffer) to return the buffer
///    to the ring for reuse.
///
/// # Cleanup
///
/// The `Drop` implementation unregisters the buffer ring from the kernel
/// and unmaps the shared memory region. Buffers must not be in use by
/// pending I/O operations when the ring is dropped.
pub struct BufferRing {
    /// File descriptor of the io_uring instance (for mmap/unregister).
    ring_fd: i32,

    /// Pointer to the mmap'd ring region (contains `IoUringBuf` entries
    /// followed by tail pointer).
    ring_ptr: *mut u8,

    /// Total size of the mmap'd ring region in bytes.
    ring_mmap_size: usize,

    /// Pointer to the contiguous buffer memory backing all ring entries.
    buffers_ptr: *mut u8,

    /// Layout used for the buffer memory allocation.
    buffers_layout: std::alloc::Layout,

    /// Configuration used to create this ring.
    config: BufferRingConfig,

    /// Atomic tail pointer for recycling buffers.
    ///
    /// Mirrors the kernel's ring tail. Userspace advances this to make
    /// previously consumed buffers available to the kernel again.
    tail: AtomicU16,

    /// `true` if [`BgidAllocator`] issued [`config.bgid`](BufferRingConfig::bgid)
    /// and `Drop` should return it to the free-list. `false` when the bgid
    /// was supplied directly by the caller via [`new`](Self::new), in
    /// which case the caller owns the namespace slot and `Drop` leaves it
    /// alone.
    allocator_owned: bool,
}

// SAFETY: The raw pointers point to memory exclusively owned by this struct.
// The ring is only accessed through the owning BufferRing instance.
unsafe impl Send for BufferRing {}

// SAFETY: The atomic tail provides thread-safe recycling. Buffer memory
// is accessed by the kernel (reads) and userspace (consumption/recycling)
// with proper ordering via the ring protocol.
unsafe impl Sync for BufferRing {}

impl BufferRing {
    /// Creates and registers a new provided buffer ring with the given io_uring instance.
    ///
    /// Allocates buffer memory, maps the ring descriptor region via `mmap`,
    /// populates all ring entries, and registers the ring with the kernel
    /// via `IORING_REGISTER_PBUF_RING`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The kernel version is below 5.19
    /// - Configuration validation fails (ring size not power of 2, zero buffer size)
    /// - Memory allocation or `mmap` fails
    /// - The `IORING_REGISTER_PBUF_RING` syscall fails
    pub fn new(ring: &RawIoUring, config: BufferRingConfig) -> Result<Self, BufferRingError> {
        validate_buffer_ring_config(&config)?;
        check_kernel_version()?;

        let ring_fd = ring.as_raw_fd();
        let ring_entries = config.ring_size as usize;
        let buf_size = config.buffer_size as usize;

        // Calculate the mmap region size: ring entries + tail u16 (at the end,
        // aligned to the struct size).
        // The kernel expects: sizeof(io_uring_buf) * ring_entries, plus space
        // for the 16-bit tail at the end of the ring page.
        let entry_size = std::mem::size_of::<IoUringBuf>();
        let ring_region_size = entry_size * ring_entries;
        // Round up to page size for mmap.
        let page_size = page_size();
        let ring_mmap_size = ring_region_size.next_multiple_of(page_size);

        // Allocate the contiguous buffer memory (page-aligned).
        let total_buf_size =
            buf_size
                .checked_mul(ring_entries)
                .ok_or(BufferRingError::AllocationFailed(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "buffer size * ring_entries overflows",
                )))?;
        let buf_layout =
            std::alloc::Layout::from_size_align(total_buf_size, page_size).map_err(|e| {
                BufferRingError::AllocationFailed(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid buffer layout: {e}"),
                ))
            })?;

        // Safety: layout has non-zero size and valid alignment.
        let buffers_ptr = unsafe { std::alloc::alloc_zeroed(buf_layout) };
        if buffers_ptr.is_null() {
            return Err(BufferRingError::AllocationFailed(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "failed to allocate buffer memory",
            )));
        }

        // mmap the ring descriptor region from the io_uring fd.
        // Safety: ring_fd is a valid io_uring fd. The offset IORING_OFF_PBUF_RING
        // with bgid encoded tells the kernel which buffer group to map.
        let mmap_offset = IORING_OFF_PBUF_RING | (u64::from(config.bgid) << 16);
        let ring_ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                ring_mmap_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                ring_fd,
                mmap_offset as libc::off_t,
            )
        };

        if ring_ptr == libc::MAP_FAILED {
            // Clean up buffer allocation.
            unsafe { std::alloc::dealloc(buffers_ptr, buf_layout) };
            return Err(BufferRingError::MmapFailed(io::Error::last_os_error()));
        }

        let ring_ptr = ring_ptr.cast::<u8>();

        // Populate ring entries with buffer addresses.
        for i in 0..ring_entries {
            // SAFETY: `i < ring_entries` and the mmap covers
            // `ring_entries * entry_size` bytes plus the tail word, so the
            // computed entry pointer stays within the mapping.
            let entry_ptr = unsafe { ring_ptr.add(i * entry_size).cast::<IoUringBuf>() };
            // SAFETY: `i < ring_entries` and the buffer arena is
            // `ring_entries * buf_size` bytes, so the offset is in bounds.
            let buf_addr = unsafe { buffers_ptr.add(i * buf_size) };
            // SAFETY: `entry_ptr` points into the freshly mmap'd ring with
            // proper alignment for `IoUringBuf` (16-byte struct in a
            // page-aligned mapping); no concurrent reader exists yet.
            unsafe {
                ptr::write(
                    entry_ptr,
                    IoUringBuf {
                        addr: buf_addr as u64,
                        len: config.buffer_size,
                        bid: i as u16,
                        resv: 0,
                    },
                );
            }
        }

        // Set the initial tail to ring_entries (all buffers available).
        // The tail pointer is at offset `ring_entries * entry_size` from the base
        // in the mmap'd region (the kernel reads it from there).
        let tail_offset = ring_entries * entry_size;
        // SAFETY: the mmap reserves space for the tail word at this offset
        // (see `ring_mmap_size = ring_entries * entry_size + size_of::<u16>()`)
        // and is `u16`-aligned because `entry_size` is a multiple of 2.
        let tail_ptr = unsafe { ring_ptr.add(tail_offset).cast::<u16>() };
        // SAFETY: `tail_ptr` is valid and aligned per the comment above; no
        // other thread has access to the mapping yet.
        unsafe {
            ptr::write(tail_ptr, ring_entries as u16);
        }

        // Register the buffer ring with the kernel.
        let reg = IoUringBufReg {
            ring_addr: ring_ptr as u64,
            ring_entries: config.ring_size,
            bgid: config.bgid,
            flags: 0,
            resv: [0; 3],
        };

        // Safety: reg is a valid IoUringBufReg struct, ring_fd is a valid io_uring fd.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_io_uring_register,
                ring_fd,
                IORING_REGISTER_PBUF_RING,
                &reg as *const IoUringBufReg,
                1u32,
            )
        };

        if ret < 0 {
            // Clean up on registration failure.
            unsafe {
                libc::munmap(ring_ptr.cast(), ring_mmap_size);
                std::alloc::dealloc(buffers_ptr, buf_layout);
            }
            return Err(BufferRingError::RegisterFailed(io::Error::last_os_error()));
        }

        Ok(Self {
            ring_fd,
            ring_ptr,
            ring_mmap_size,
            buffers_ptr,
            buffers_layout: buf_layout,
            config,
            tail: AtomicU16::new(ring_entries as u16),
            allocator_owned: false,
        })
    }

    /// Attempts to create a buffer ring, returning `None` on any failure.
    ///
    /// This is the preferred entry point for optional PBUF_RING usage - it
    /// never returns an error, making it safe to call speculatively.
    pub fn try_new(ring: &RawIoUring, config: BufferRingConfig) -> Option<Self> {
        Self::new(ring, config).ok()
    }

    /// Creates a buffer ring with a bgid issued by [`BgidAllocator`].
    ///
    /// The supplied `config.bgid` is overridden with the next id returned
    /// by [`BgidAllocator::allocate`], and the returned ring takes
    /// ownership of that id - dropping the ring calls
    /// [`BgidAllocator::deallocate`] so the id is returned to the
    /// free-list for reuse. Long-running daemons that cycle through many
    /// buffer rings should prefer this entry point over [`new`](Self::new)
    /// to avoid exhausting the 16-bit bgid namespace.
    ///
    /// # Errors
    ///
    /// Returns [`BufferRingError::BgidExhausted`] (converted from
    /// [`BgidAllocError::Exhausted`]) when the allocator has no ids
    /// available; on that path a single throttled `tracing::warn!` is
    /// emitted per process so callers can fall back to plain `recv` /
    /// `read` without the buffer-ring optimization. Otherwise returns any
    /// error [`new`](Self::new) would return; in that case the allocated
    /// bgid is returned to the free-list before propagating the failure
    /// so it is not leaked.
    pub fn new_with_allocator(
        ring: &RawIoUring,
        mut config: BufferRingConfig,
    ) -> Result<Self, BufferRingError> {
        let bgid = match BgidAllocator::allocate() {
            Ok(id) => id,
            Err(e) => {
                warn_bgid_fallback(e);
                return Err(e.into());
            }
        };
        config.bgid = bgid;
        match Self::new(ring, config) {
            Ok(mut br) => {
                br.allocator_owned = true;
                Ok(br)
            }
            Err(e) => {
                BgidAllocator::deallocate(bgid);
                Err(e)
            }
        }
    }

    /// Returns the buffer group ID for this ring.
    ///
    /// SQEs must specify this group ID with `IOSQE_BUFFER_SELECT` to
    /// use buffers from this ring.
    #[inline]
    #[must_use]
    pub fn bgid(&self) -> u16 {
        self.config.bgid
    }

    /// Returns the number of entries (buffers) in the ring.
    #[inline]
    #[must_use]
    pub fn ring_size(&self) -> u32 {
        self.config.ring_size
    }

    /// Returns the size of each buffer in bytes.
    #[inline]
    #[must_use]
    pub fn buffer_size(&self) -> u32 {
        self.config.buffer_size
    }

    /// Returns a pointer to the buffer identified by `buf_id`.
    ///
    /// The `buf_id` is extracted from CQE flags after a successful read
    /// completion. The returned pointer is valid until the buffer is
    /// recycled via [`recycle_buffer`](Self::recycle_buffer) or the ring
    /// is dropped.
    ///
    /// Returns `None` if `buf_id` is out of range.
    pub fn buffer_ptr(&self, buf_id: u16) -> Option<*const u8> {
        if u32::from(buf_id) >= self.config.ring_size {
            return None;
        }
        let offset = usize::from(buf_id) * self.config.buffer_size as usize;
        // SAFETY: `buf_id < ring_size` was just bounds-checked, so `offset`
        // is within the buffer arena allocated for `ring_size * buffer_size`
        // bytes.
        Some(unsafe { self.buffers_ptr.add(offset) })
    }

    /// Returns a slice view of the buffer identified by `buf_id`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `buf_id` was obtained from a completed CQE (not recycled yet)
    /// - `len` does not exceed the CQE result (bytes actually written by kernel)
    /// - No concurrent recycling of this buffer occurs during the slice lifetime
    pub unsafe fn buffer_slice(&self, buf_id: u16, len: usize) -> Option<&[u8]> {
        let ptr = self.buffer_ptr(buf_id)?;
        let clamped = len.min(self.config.buffer_size as usize);
        // SAFETY: `ptr` is a valid base into the arena (verified by
        // `buffer_ptr`), `clamped <= buffer_size` keeps us inside the
        // buffer, and the caller's `unsafe` contract guarantees the kernel
        // initialised those bytes and is not concurrently recycling them.
        Some(unsafe { std::slice::from_raw_parts(ptr, clamped) })
    }

    /// Recycles a buffer back into the ring for reuse by the kernel.
    ///
    /// After processing data from a completed read, call this to make the
    /// buffer available for future I/O operations. The buffer is identified
    /// by its `buf_id` from the CQE flags.
    ///
    /// The ring tail is advanced atomically, making this safe to call from
    /// any thread.
    ///
    /// # Errors
    ///
    /// Returns [`BufferRingError::BufferIdOutOfRange`] if `buf_id` is outside
    /// the configured ring range. The check runs in both debug and release
    /// builds, and the recycle is refused before any state is mutated so a
    /// bogus `buf_id` cannot advance the ring tail or write into
    /// kernel-shared memory. Callers may safely log and ignore the error or
    /// surface it via the `From<BufferRingError> for io::Error` conversion.
    pub fn recycle_buffer(&self, buf_id: u16) -> Result<(), BufferRingError> {
        if u32::from(buf_id) >= self.config.ring_size {
            return Err(BufferRingError::BufferIdOutOfRange {
                buf_id,
                ring_size: self.config.ring_size,
            });
        }

        let mask = self.config.ring_size - 1; // ring_size is power of 2
        let tail = self.tail.fetch_add(1, Ordering::AcqRel);
        let index = usize::from(tail & mask as u16);

        let entry_size = std::mem::size_of::<IoUringBuf>();
        // SAFETY: `index = tail & (ring_size - 1) < ring_size`, so the offset
        // stays within the `ring_size * entry_size` portion of the mmap.
        let entry_ptr = unsafe { self.ring_ptr.add(index * entry_size).cast::<IoUringBuf>() };

        let buf_offset = usize::from(buf_id) * self.config.buffer_size as usize;
        // SAFETY: `buf_id < ring_size` (checked above), so `buf_offset` lies
        // inside the buffer arena allocated at construction.
        let buf_addr = unsafe { self.buffers_ptr.add(buf_offset) };

        // SAFETY: `entry_ptr` is in-bounds and properly aligned for
        // `IoUringBuf`; the atomic tail increment guarantees no other thread
        // writes to the same slot concurrently.
        unsafe {
            ptr::write(
                entry_ptr,
                IoUringBuf {
                    addr: buf_addr as u64,
                    len: self.config.buffer_size,
                    bid: buf_id,
                    resv: 0,
                },
            );
        }

        // Write the updated tail to the shared memory location that the kernel reads.
        let tail_offset = self.config.ring_size as usize * entry_size;
        // SAFETY: `tail_offset` points at the kernel-shared tail word that
        // sits immediately after the entry array (sized into the mmap at
        // construction). The pointer is `u16`-aligned because `entry_size`
        // is a multiple of 2 and the mapping is page-aligned.
        let tail_ptr = unsafe { self.ring_ptr.add(tail_offset).cast::<AtomicU16>() };
        let new_tail = tail.wrapping_add(1);
        // SAFETY: `tail_ptr` references the shared tail word; reborrowing as
        // an `AtomicU16` is sound because the kernel uses single-word loads
        // with matching alignment per the io_uring buffer-ring ABI.
        unsafe { &*tail_ptr }.store(new_tail, Ordering::Release);
        Ok(())
    }

    /// Returns the configuration used to create this ring.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &BufferRingConfig {
        &self.config
    }
}

impl Drop for BufferRing {
    fn drop(&mut self) {
        // Unregister the buffer ring from the kernel.
        let reg = IoUringBufReg {
            ring_addr: 0,
            ring_entries: 0,
            bgid: self.config.bgid,
            flags: 0,
            resv: [0; 3],
        };

        // Safety: ring_fd is still valid at drop time. The unregister call
        // tells the kernel to stop using buffers from this group.
        unsafe {
            libc::syscall(
                libc::SYS_io_uring_register,
                self.ring_fd,
                IORING_UNREGISTER_PBUF_RING,
                &reg as *const IoUringBufReg,
                1u32,
            );
        }

        // Safety: ring_ptr was returned by a successful mmap call and
        // ring_mmap_size is the same size passed to mmap.
        unsafe {
            libc::munmap(self.ring_ptr.cast(), self.ring_mmap_size);
        }

        // Safety: buffers_ptr was allocated with buffers_layout and has
        // not been freed yet. The kernel no longer references these buffers
        // after unregister completes.
        unsafe {
            std::alloc::dealloc(self.buffers_ptr, self.buffers_layout);
        }

        // Return the bgid to the allocator's free-list if this ring owned
        // an allocator-issued id. Caller-supplied bgids are left alone -
        // the caller continues to own that namespace slot.
        if self.allocator_owned {
            BgidAllocator::deallocate(self.config.bgid);
        }
    }
}

/// Returns the system page size.
fn page_size() -> usize {
    // Safety: sysconf is always safe to call with _SC_PAGESIZE.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 { 4096 } else { size as usize }
}

#[cfg(test)]
mod tests;
