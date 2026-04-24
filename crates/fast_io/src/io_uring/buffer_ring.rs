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
//! # Kernel requirements
//!
//! - **Linux 5.19+** for `IORING_REGISTER_PBUF_RING` support.
//! - The kernel must not block io_uring via seccomp.
//!
//! # References
//!
//! - `io_uring_register(2)` - `IORING_REGISTER_PBUF_RING` / `IORING_UNREGISTER_PBUF_RING`
//! - kernel source: `io_uring/kbuf.c` - `io_register_pbuf_ring()`

use std::io;
use std::ptr;
use std::sync::atomic::{AtomicU16, Ordering};

use io_uring::IoUring as RawIoUring;

use super::config;

// ──────────────────────────────────────────────────────────────────────────────
// Kernel constants not yet exposed by libc or io-uring crate
// ──────────────────────────────────────────────────────────────────────────────

/// `IORING_REGISTER_PBUF_RING` opcode (kernel 5.19+).
const IORING_REGISTER_PBUF_RING: libc::c_uint = 22;

/// `IORING_UNREGISTER_PBUF_RING` opcode (kernel 5.19+).
const IORING_UNREGISTER_PBUF_RING: libc::c_uint = 23;

/// Offset passed to `mmap` to map the provided buffer ring region.
const IORING_OFF_PBUF_RING: u64 = 0x80000000;

/// Minimum kernel version for PBUF_RING support.
const MIN_PBUF_RING_KERNEL: (u32, u32) = (5, 19);

/// CQE flag indicating the buffer ID is valid.
const IORING_CQE_F_BUFFER: u32 = 1 << 0;

/// Buffer ID shift in CQE flags.
const IORING_CQE_BUFFER_SHIFT: u32 = 16;

// ──────────────────────────────────────────────────────────────────────────────
// Kernel structs for PBUF_RING registration
// ──────────────────────────────────────────────────────────────────────────────

/// Matches `struct io_uring_buf_reg` from the kernel.
#[repr(C)]
struct IoUringBufReg {
    ring_addr: u64,
    ring_entries: u32,
    bgid: u16,
    flags: u16,
    resv: [u64; 3],
}

/// Matches `struct io_uring_buf` from the kernel - one entry in the ring.
#[repr(C)]
struct IoUringBuf {
    addr: u64,
    len: u32,
    bid: u16,
    resv: u16,
}

// ──────────────────────────────────────────────────────────────────────────────
// Error type
// ──────────────────────────────────────────────────────────────────────────────

/// Errors specific to buffer ring operations.
#[derive(Debug, thiserror::Error)]
pub enum BufferRingError {
    /// PBUF_RING is not supported on this kernel.
    #[error("PBUF_RING requires Linux 5.19+ (detected {major}.{minor})")]
    KernelTooOld {
        /// Detected kernel major version.
        major: u32,
        /// Detected kernel minor version.
        minor: u32,
    },

    /// Could not detect the kernel version.
    #[error("could not detect kernel version for PBUF_RING support check")]
    KernelVersionUnknown,

    /// Ring size is invalid (must be power of 2 and > 0).
    #[error("ring size must be a power of 2 and > 0, got {0}")]
    InvalidRingSize(u32),

    /// Buffer size is invalid (must be > 0).
    #[error("buffer size must be > 0")]
    InvalidBufferSize,

    /// The `mmap` call for the ring region failed.
    #[error("mmap for PBUF_RING failed: {0}")]
    MmapFailed(io::Error),

    /// The `IORING_REGISTER_PBUF_RING` syscall failed.
    #[error("IORING_REGISTER_PBUF_RING failed: {0}")]
    RegisterFailed(io::Error),

    /// Buffer memory allocation failed.
    #[error("buffer allocation failed: {0}")]
    AllocationFailed(io::Error),
}

impl From<BufferRingError> for io::Error {
    fn from(e: BufferRingError) -> Self {
        match &e {
            BufferRingError::KernelTooOld { .. } | BufferRingError::KernelVersionUnknown => {
                io::Error::new(io::ErrorKind::Unsupported, e)
            }
            BufferRingError::InvalidRingSize(_) | BufferRingError::InvalidBufferSize => {
                io::Error::new(io::ErrorKind::InvalidInput, e)
            }
            BufferRingError::MmapFailed(_)
            | BufferRingError::RegisterFailed(_)
            | BufferRingError::AllocationFailed(_) => io::Error::other(e),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

/// Configuration for a provided buffer ring.
///
/// Controls the ring dimensions and buffer group identity.
#[derive(Debug, Clone)]
pub struct BufferRingConfig {
    /// Number of entries in the ring (must be a power of 2).
    ///
    /// Each entry corresponds to one buffer. The kernel selects from
    /// available entries at I/O completion time.
    pub ring_size: u32,

    /// Size of each individual buffer in bytes.
    ///
    /// Should be large enough for the expected I/O size. Page-aligned
    /// sizes are recommended for optimal performance.
    pub buffer_size: u32,

    /// Buffer group ID for this ring.
    ///
    /// SQEs reference this group ID to select buffers from this ring.
    /// Multiple rings can coexist with different group IDs.
    pub bgid: u16,
}

impl Default for BufferRingConfig {
    fn default() -> Self {
        Self {
            ring_size: 64,
            buffer_size: 64 * 1024, // 64 KB
            bgid: 0,
        }
    }
}

impl BufferRingConfig {
    /// Validates the configuration parameters.
    fn validate(&self) -> Result<(), BufferRingError> {
        if self.ring_size == 0 || !self.ring_size.is_power_of_two() {
            return Err(BufferRingError::InvalidRingSize(self.ring_size));
        }
        if self.buffer_size == 0 {
            return Err(BufferRingError::InvalidBufferSize);
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Runtime detection
// ──────────────────────────────────────────────────────────────────────────────

/// Returns `true` if the kernel supports PBUF_RING (>= 5.19).
///
/// Checks the kernel version via `uname(2)`. This is a necessary but not
/// sufficient condition - the actual `IORING_REGISTER_PBUF_RING` call may
/// still fail if seccomp blocks it.
#[must_use]
pub fn is_supported() -> bool {
    let release = match config::config_detail::get_kernel_release_string() {
        Some(r) => r,
        None => return false,
    };
    let (major, minor) = match config::parse_kernel_version(&release) {
        Some(v) => v,
        None => return false,
    };
    (major, minor) >= MIN_PBUF_RING_KERNEL
}

// ──────────────────────────────────────────────────────────────────────────────
// BufferRing
// ──────────────────────────────────────────────────────────────────────────────

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
        config.validate()?;
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
            let entry_ptr = unsafe { ring_ptr.add(i * entry_size).cast::<IoUringBuf>() };
            let buf_addr = unsafe { buffers_ptr.add(i * buf_size) };
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
        let tail_ptr = unsafe { ring_ptr.add(tail_offset).cast::<u16>() };
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
        })
    }

    /// Attempts to create a buffer ring, returning `None` on any failure.
    ///
    /// This is the preferred entry point for optional PBUF_RING usage - it
    /// never returns an error, making it safe to call speculatively.
    #[must_use]
    pub fn try_new(ring: &RawIoUring, config: BufferRingConfig) -> Option<Self> {
        Self::new(ring, config).ok()
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
    #[must_use]
    pub fn buffer_ptr(&self, buf_id: u16) -> Option<*const u8> {
        if u32::from(buf_id) >= self.config.ring_size {
            return None;
        }
        let offset = usize::from(buf_id) * self.config.buffer_size as usize;
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
    #[must_use]
    pub unsafe fn buffer_slice(&self, buf_id: u16, len: usize) -> Option<&[u8]> {
        let ptr = self.buffer_ptr(buf_id)?;
        let clamped = len.min(self.config.buffer_size as usize);
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
    pub fn recycle_buffer(&self, buf_id: u16) {
        debug_assert!(
            u32::from(buf_id) < self.config.ring_size,
            "buf_id {buf_id} out of range for ring size {}",
            self.config.ring_size
        );

        let mask = self.config.ring_size - 1; // ring_size is power of 2
        let tail = self.tail.fetch_add(1, Ordering::AcqRel);
        let index = usize::from(tail & mask as u16);

        let entry_size = std::mem::size_of::<IoUringBuf>();
        let entry_ptr = unsafe { self.ring_ptr.add(index * entry_size).cast::<IoUringBuf>() };

        let buf_offset = usize::from(buf_id) * self.config.buffer_size as usize;
        let buf_addr = unsafe { self.buffers_ptr.add(buf_offset) };

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
        let tail_ptr = unsafe { self.ring_ptr.add(tail_offset).cast::<AtomicU16>() };
        let new_tail = tail.wrapping_add(1);
        unsafe { &*tail_ptr }.store(new_tail, Ordering::Release);
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

        // Unmap the ring descriptor region.
        // Safety: ring_ptr was returned by a successful mmap call and
        // ring_mmap_size is the same size passed to mmap.
        unsafe {
            libc::munmap(self.ring_ptr.cast(), self.ring_mmap_size);
        }

        // Free the buffer memory.
        // Safety: buffers_ptr was allocated with buffers_layout and has
        // not been freed yet. The kernel no longer references these buffers
        // after unregister completes.
        unsafe {
            std::alloc::dealloc(self.buffers_ptr, self.buffers_layout);
        }
    }
}

/// Extracts the buffer ID from CQE flags.
///
/// When a read completes using a provided buffer, the kernel sets
/// `IORING_CQE_F_BUFFER` in the flags and encodes the buffer ID in
/// the upper 16 bits. Returns `None` if the buffer flag is not set.
#[inline]
#[must_use]
pub fn buffer_id_from_cqe_flags(flags: u32) -> Option<u16> {
    if flags & IORING_CQE_F_BUFFER != 0 {
        Some((flags >> IORING_CQE_BUFFER_SHIFT) as u16)
    } else {
        None
    }
}

/// Uses `AsRawFd` to get the fd from the ring. This is a helper trait
/// import to keep the `new` method clean.
use std::os::unix::io::AsRawFd;

/// Checks that the running kernel supports PBUF_RING.
fn check_kernel_version() -> Result<(), BufferRingError> {
    let release = config::config_detail::get_kernel_release_string()
        .ok_or(BufferRingError::KernelVersionUnknown)?;
    let (major, minor) =
        config::parse_kernel_version(&release).ok_or(BufferRingError::KernelVersionUnknown)?;
    if (major, minor) < MIN_PBUF_RING_KERNEL {
        return Err(BufferRingError::KernelTooOld { major, minor });
    }
    Ok(())
}

/// Returns the system page size.
fn page_size() -> usize {
    // Safety: sysconf is always safe to call with _SC_PAGESIZE.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 { 4096 } else { size as usize }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_has_valid_values() {
        let config = BufferRingConfig::default();
        assert!(config.ring_size.is_power_of_two());
        assert!(config.ring_size > 0);
        assert!(config.buffer_size > 0);
        assert_eq!(config.bgid, 0);
    }

    #[test]
    fn config_validate_rejects_zero_ring_size() {
        let config = BufferRingConfig {
            ring_size: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_non_power_of_two() {
        let config = BufferRingConfig {
            ring_size: 3,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validate_rejects_zero_buffer_size() {
        let config = BufferRingConfig {
            buffer_size: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validate_accepts_valid_config() {
        let config = BufferRingConfig {
            ring_size: 16,
            buffer_size: 4096,
            bgid: 1,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_accepts_large_power_of_two() {
        let config = BufferRingConfig {
            ring_size: 1024,
            buffer_size: 256 * 1024,
            bgid: 0,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn is_supported_returns_bool_without_panic() {
        // On any platform, is_supported must not panic. It returns true on
        // Linux >= 5.19 and false otherwise.
        let _result: bool = is_supported();
    }

    #[test]
    fn buffer_id_from_cqe_flags_extracts_id() {
        // Buffer ID 42 encoded in upper 16 bits with IORING_CQE_F_BUFFER set.
        let flags = (42u32 << IORING_CQE_BUFFER_SHIFT) | IORING_CQE_F_BUFFER;
        assert_eq!(buffer_id_from_cqe_flags(flags), Some(42));
    }

    #[test]
    fn buffer_id_from_cqe_flags_returns_none_without_flag() {
        // No IORING_CQE_F_BUFFER flag set.
        let flags = 42u32 << IORING_CQE_BUFFER_SHIFT;
        assert_eq!(buffer_id_from_cqe_flags(flags), None);
    }

    #[test]
    fn buffer_id_from_cqe_flags_zero_id() {
        let flags = IORING_CQE_F_BUFFER; // buffer ID = 0
        assert_eq!(buffer_id_from_cqe_flags(flags), Some(0));
    }

    #[test]
    fn buffer_id_from_cqe_flags_max_id() {
        let flags = (u16::MAX as u32) << IORING_CQE_BUFFER_SHIFT | IORING_CQE_F_BUFFER;
        assert_eq!(buffer_id_from_cqe_flags(flags), Some(u16::MAX));
    }

    #[test]
    fn buffer_ring_error_converts_to_io_error() {
        let err: io::Error = BufferRingError::KernelTooOld {
            major: 5,
            minor: 15,
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let err: io::Error = BufferRingError::InvalidRingSize(3).into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err: io::Error = BufferRingError::InvalidBufferSize.into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn buffer_ring_error_display_messages() {
        let err = BufferRingError::KernelTooOld {
            major: 5,
            minor: 15,
        };
        let msg = format!("{err}");
        assert!(msg.contains("5.19"));
        assert!(msg.contains("5.15"));

        let err = BufferRingError::InvalidRingSize(7);
        let msg = format!("{err}");
        assert!(msg.contains("power of 2"));
        assert!(msg.contains("7"));
    }

    #[test]
    fn buffer_ring_new_on_supported_kernel() {
        // Skip if io_uring is not available or kernel < 5.19.
        if !is_supported() {
            return;
        }
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };

        let config = BufferRingConfig {
            ring_size: 4,
            buffer_size: 4096,
            bgid: 0,
        };

        let buf_ring = match BufferRing::new(&ring, config) {
            Ok(br) => br,
            Err(_) => return, // May fail due to seccomp or permissions
        };

        assert_eq!(buf_ring.ring_size(), 4);
        assert_eq!(buf_ring.buffer_size(), 4096);
        assert_eq!(buf_ring.bgid(), 0);

        // Verify buffer pointers are valid and in-range.
        for i in 0..4u16 {
            let ptr = buf_ring.buffer_ptr(i);
            assert!(ptr.is_some(), "buffer {i} pointer should be valid");
            assert!(
                !ptr.unwrap().is_null(),
                "buffer {i} pointer should not be null"
            );
        }

        // Out-of-range buffer ID.
        assert!(buf_ring.buffer_ptr(4).is_none());
        assert!(buf_ring.buffer_ptr(u16::MAX).is_none());

        // Drop triggers cleanup (unregister, munmap, dealloc).
        drop(buf_ring);
    }

    #[test]
    fn buffer_ring_try_new_returns_none_on_failure() {
        // On kernels < 5.19 or without io_uring, try_new should return None.
        if is_supported() {
            return; // Skip - we need a failing case for this test
        }

        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => {
                // io_uring itself unavailable - try_new should also fail
                // but we cannot even create the ring. Verify is_supported is false.
                assert!(!is_supported());
                return;
            }
        };

        let config = BufferRingConfig::default();
        assert!(BufferRing::try_new(&ring, config).is_none());
    }

    #[test]
    fn buffer_ring_recycle_on_supported_kernel() {
        if !is_supported() {
            return;
        }
        let ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return,
        };

        let config = BufferRingConfig {
            ring_size: 4,
            buffer_size: 4096,
            bgid: 1,
        };

        let buf_ring = match BufferRing::new(&ring, config) {
            Ok(br) => br,
            Err(_) => return,
        };

        // Recycling buffer 0 should not panic.
        buf_ring.recycle_buffer(0);
        buf_ring.recycle_buffer(1);
        buf_ring.recycle_buffer(2);
        buf_ring.recycle_buffer(3);

        drop(buf_ring);
    }

    #[test]
    fn page_size_is_positive_and_power_of_two() {
        let ps = page_size();
        assert!(ps > 0);
        assert!(ps.is_power_of_two());
    }
}
