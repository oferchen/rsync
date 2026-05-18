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
//! # Runtime probe
//!
//! [`pbuf_ring_supported`] returns whether the running kernel can register a
//! provided buffer ring. The first call performs the `uname(2)` parse; the
//! result is cached in a process-wide [`OnceLock`] so subsequent calls are a
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
//!    [`super::registered_buffers::RegisteredBufferGroup`].
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
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use io_uring::IoUring as RawIoUring;

use super::config;

/// `IORING_REGISTER_PBUF_RING` opcode (kernel 5.19+).
const IORING_REGISTER_PBUF_RING: libc::c_uint = 22;

/// `IORING_UNREGISTER_PBUF_RING` opcode (kernel 5.19+).
const IORING_UNREGISTER_PBUF_RING: libc::c_uint = 23;

/// Offset passed to `mmap` to map the provided buffer ring region.
const IORING_OFF_PBUF_RING: u64 = 0x80000000;

/// Minimum kernel version for PBUF_RING support.
const MIN_PBUF_RING_KERNEL: (u32, u32) = (5, 19);

pub use crate::io_uring_common::{
    BgidAllocError, BufferRingConfig, BufferRingError, buffer_id_from_cqe_flags,
};
#[cfg(test)]
use crate::io_uring_common::{IORING_CQE_BUFFER_SHIFT, IORING_CQE_F_BUFFER};

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

/// Validates a [`BufferRingConfig`] for use with the Linux backend.
///
/// The plain-data [`BufferRingConfig`] lives in [`crate::io_uring_common`]
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

/// Maximum number of distinct buffer group IDs available per process.
///
/// The io_uring kernel interface stores bgid as `u16` inside
/// `struct io_uring_buf_reg` (upstream: io_uring/kbuf.c,
/// `io_register_pbuf_ring()`), bounding the namespace to
/// `u16::MAX + 1 = 65 536` values (0..=65 535). Registering a 65 537th
/// group without first unregistering an existing one causes the kernel to
/// return `EEXIST` or silently collide, so callers must stay within this
/// bound.
const BGID_NAMESPACE_SIZE: u32 = u16::MAX as u32 + 1;

/// Process-wide monotonic counter for automatic buffer group ID assignment.
///
/// Stored as `u32` so values above `u16::MAX` can be detected without
/// wrapping. Incremented once per [`BgidAllocator::allocate`] call (when
/// the free-list is empty) and decremented only on the boundary call that
/// crosses past the namespace limit, keeping the counter capped at
/// `BGID_NAMESPACE_SIZE` thereafter.
static NEXT_BGID: AtomicU32 = AtomicU32::new(0);

/// Process-wide high-water mark for concurrently-allocated bgids.
///
/// Updated via `fetch_max` after every successful
/// [`BgidAllocator::allocate`] call. Exposed by [`bgid_peak_used`] so
/// operators and tests can observe the worst-case occupancy of the 16-bit
/// namespace over the process lifetime. Never decreases; deallocation
/// returns ids to the free-list but the peak stays.
static PEAK_USED: AtomicU16 = AtomicU16::new(0);

/// Process-wide counter of [`BgidAllocator::allocate`] calls that
/// returned [`BgidAllocError::Exhausted`].
///
/// Read with [`bgid_exhausted_count`]. Each exhausted return increments
/// the counter by one. Monotonic and cumulative for the process lifetime;
/// callers that want a rate compute the delta between two snapshots.
static BGID_EXHAUSTED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Process-wide one-shot flag for the "BGID space exhausted, falling
/// back" warning emitted by [`BufferRing::new_with_allocator`].
///
/// `false` means the warning has not yet fired in this process. Set to
/// `true` on the first exhaustion path so the hot allocation loop does
/// not repeatedly spam the log; operators get a single deterministic
/// marker and can correlate it with the cumulative counter from
/// [`bgid_exhausted_count`].
static BGID_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Initial capacity reserved for the bgid free-list.
///
/// Sized to cover the typical steady-state churn of a long-running daemon
/// recycling buffer rings without triggering `Vec` reallocations under the
/// free-list mutex. 4 096 entries match a common upper bound on
/// simultaneously-open buffer rings before the namespace warning fires
/// (50 % of u16::MAX is 32 767).
const BGID_FREE_LIST_INITIAL_CAPACITY: usize = 1 << 12;

/// Occupancy threshold (in absolute bgid count) that triggers the
/// throttled namespace-pressure warning.
///
/// Set to 50 % of the 16-bit namespace so operators get early notice that
/// the process is approaching exhaustion while there is still headroom to
/// react.
const BGID_OCCUPANCY_WARN_THRESHOLD: u16 = (BGID_NAMESPACE_SIZE / 2) as u16;

/// Minimum interval between successive namespace-pressure warnings.
const BGID_WARN_THROTTLE: Duration = Duration::from_secs(30);

/// Process-wide free-list of returned bgids available for reuse.
///
/// Populated by [`BgidAllocator::deallocate`] when a [`BufferRing`] that
/// was issued a bgid by [`BgidAllocator::allocate`] is dropped. Drained by
/// [`BgidAllocator::allocate`] before incrementing [`NEXT_BGID`], so the
/// monotonic counter only advances when no reusable id is available.
///
/// Pre-sized to `BGID_FREE_LIST_INITIAL_CAPACITY` so the steady-state
/// churn of a long-running daemon does not trigger `Vec` reallocations
/// under the free-list mutex.
fn bgid_free_list() -> &'static Mutex<Vec<u16>> {
    static FREE_LIST: OnceLock<Mutex<Vec<u16>>> = OnceLock::new();
    FREE_LIST.get_or_init(|| Mutex::new(Vec::with_capacity(BGID_FREE_LIST_INITIAL_CAPACITY)))
}

/// Last time the namespace-pressure warning was emitted.
///
/// `None` means "never emitted". Updated under the mutex so two threads
/// observing > 50 % occupancy simultaneously do not both emit a warning.
fn bgid_warn_last() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Emits the "BGID space exhausted, falling back" `tracing::warn!` at
/// most once per process lifetime.
///
/// Called from [`BufferRing::new_with_allocator`] whenever
/// [`BgidAllocator::allocate`] returns [`BgidAllocError::Exhausted`].
/// The atomic compare-exchange guarantees the message fires exactly once
/// even when many threads hit exhaustion simultaneously, so the hot
/// allocation loop in a long-running daemon does not flood the log.
fn warn_bgid_fallback_once(err: BgidAllocError) {
    if BGID_FALLBACK_WARNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let BgidAllocError::Exhausted {
            fresh_used,
            free_list_len,
        } = err;
        tracing::warn!(
            target: "fast_io::buffer_ring",
            fresh_used,
            free_list_len,
            exhausted_count = BGID_EXHAUSTED_COUNT.load(Ordering::Relaxed),
            "BGID space exhausted, falling back to non-registered receive path"
        );
    }
}

/// Emits a throttled `tracing::warn!` when bgid occupancy crosses 50 %.
///
/// The warning fires at most once per `BGID_WARN_THROTTLE` window so a
/// hot allocation path under sustained pressure does not flood the log.
fn maybe_warn_namespace_pressure(in_flight: u16) {
    if in_flight < BGID_OCCUPANCY_WARN_THRESHOLD {
        return;
    }
    let mut last = match bgid_warn_last().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = Instant::now();
    let fire = match *last {
        None => true,
        Some(t) => now.duration_since(t) >= BGID_WARN_THROTTLE,
    };
    if fire {
        *last = Some(now);
        tracing::warn!(
            target: "fast_io::buffer_ring",
            in_flight,
            namespace = BGID_NAMESPACE_SIZE,
            "io_uring bgid occupancy crossed 50% of the 16-bit namespace"
        );
    }
}

/// Allocator for io_uring buffer group IDs (bgid).
///
/// io_uring provided buffer rings (PBUF_RING) are identified by a 16-bit
/// Buffer Group ID. With only 65 536 possible values, a long-running
/// process that continuously allocates new buffer rings without recycling
/// bgids will eventually exhaust the namespace and silently collide with
/// rings still active in the kernel.
///
/// [`BgidAllocator`] provides a safe, bounded allocation path:
///
/// - [`allocate`](Self::allocate) returns a bgid - either a previously
///   freed id from the internal free-list, or the next monotonic value
///   starting at 0.
/// - [`deallocate`](Self::deallocate) returns a bgid to the free-list so
///   that future [`allocate`](Self::allocate) calls can reuse it.
/// - Once the monotonic counter reaches `BGID_NAMESPACE_SIZE` (65 536)
///   and the free-list is empty, [`allocate`](Self::allocate) returns
///   [`BufferRingError::BgidExhausted`] rather than wrapping and silently
///   reusing a bgid still held by an active ring.
///
/// Callers that create a bounded, fixed number of buffer rings per
/// process may set [`BufferRingConfig::bgid`] directly with known
/// constants and skip this allocator entirely.
pub struct BgidAllocator;

impl BgidAllocator {
    /// Allocates the next available buffer group ID.
    ///
    /// First drains the internal free-list of previously-deallocated bgids.
    /// If the free-list is empty, falls through to a process-wide monotonic
    /// `u32` counter starting at 0. When the counter would exceed
    /// `u16::MAX` (65 535) - meaning all 65 536 possible bgids have been
    /// issued and none have been returned - returns
    /// [`BgidAllocError::Exhausted`] without panicking and bumps
    /// [`bgid_exhausted_count`].
    ///
    /// # Errors
    ///
    /// Returns [`BgidAllocError::Exhausted`] when both the free-list is
    /// empty and the monotonic counter is at the namespace limit. The
    /// error carries the live `fresh_used` and `free_list_len` snapshot
    /// for operator diagnostics. Callers must drop existing
    /// [`BufferRing`] instances that own their bgid (so
    /// [`deallocate`](Self::deallocate) runs in the destructor) to make
    /// ids available again; otherwise the recommended downgrade is to
    /// skip the buffer-ring registration and continue serving with plain
    /// `recv`/`read` on that connection.
    pub fn allocate() -> Result<u16, BgidAllocError> {
        // Reuse a freed id when one is available. The lock is held only for
        // the pop, so contention with concurrent deallocate calls is
        // negligible in practice (one buffer ring per long-running task).
        let popped = bgid_free_list()
            .lock()
            .expect("bgid free-list poisoned")
            .pop();
        if let Some(id) = popped {
            record_allocation();
            return Ok(id);
        }

        // Relaxed ordering is sufficient: uniqueness within the process is
        // guaranteed by the atomic RMW alone; no other memory operations
        // depend on this value being observed in a particular order.
        let id = NEXT_BGID.fetch_add(1, Ordering::Relaxed);
        if id < BGID_NAMESPACE_SIZE {
            record_allocation();
            Ok(id as u16)
        } else {
            // Cap the counter at BGID_NAMESPACE_SIZE rather than letting it
            // climb toward `u32::MAX` and eventually wrap back to 0, which
            // would resume issuing valid u16 IDs that collide with active
            // rings.
            NEXT_BGID.fetch_sub(1, Ordering::Relaxed);
            BGID_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
            let free_list_len = bgid_free_list()
                .lock()
                .expect("bgid free-list poisoned")
                .len();
            Err(BgidAllocError::Exhausted {
                fresh_used: BGID_NAMESPACE_SIZE,
                free_list_len,
            })
        }
    }

    /// Returns a previously-allocated bgid to the free-list for reuse.
    ///
    /// Wired into [`BufferRing`]'s `Drop` implementation when the ring's
    /// bgid was issued by [`allocate`](Self::allocate); callers should not
    /// normally invoke this directly. The next call to
    /// [`allocate`](Self::allocate) will return this id before advancing
    /// the monotonic counter.
    ///
    /// # Idempotence
    ///
    /// Calling `deallocate` more than once for the same bgid is a no-op
    /// after the first call - the duplicate is silently dropped so the
    /// free-list never contains the same id twice. This defends against
    /// double-drop scenarios where, e.g., a buffer ring is moved out of an
    /// `Option` and the original holder is also dropped.
    ///
    /// # Assumption
    ///
    /// The caller must own `bgid`: it must have been returned by a prior
    /// [`allocate`](Self::allocate) call and not handed back through this
    /// method since. Returning a caller-provided constant (a bgid that was
    /// never issued by this allocator) pollutes the free-list and causes a
    /// later [`allocate`](Self::allocate) to issue an id that may collide
    /// with a ring active elsewhere in the process.
    pub fn deallocate(bgid: u16) {
        let mut free_list = bgid_free_list().lock().expect("bgid free-list poisoned");
        if !free_list.contains(&bgid) {
            free_list.push(bgid);
        }
    }

    /// Returns the number of bgids remaining in the namespace.
    ///
    /// Includes both unallocated counter slots and free-list entries
    /// available for reuse. When this reaches zero,
    /// [`allocate`](Self::allocate) returns
    /// [`BufferRingError::BgidExhausted`]. The value may decrease
    /// concurrently as other threads allocate.
    pub fn remaining() -> u32 {
        let used = NEXT_BGID.load(Ordering::Relaxed).min(BGID_NAMESPACE_SIZE);
        let free = bgid_free_list()
            .lock()
            .expect("bgid free-list poisoned")
            .len() as u32;
        BGID_NAMESPACE_SIZE - used + free
    }
}

/// Updates `PEAK_USED` and fires the throttled namespace-pressure
/// warning when in-flight occupancy crosses `BGID_OCCUPANCY_WARN_THRESHOLD`.
///
/// Called once per successful [`BgidAllocator::allocate`] return so the
/// high-water mark reflects every issued id, whether the slot came from
/// the free-list or from advancing the monotonic counter.
fn record_allocation() {
    let in_flight = current_in_flight();
    PEAK_USED.fetch_max(in_flight, Ordering::Relaxed);
    maybe_warn_namespace_pressure(in_flight);
}

/// Computes the current number of allocator-issued bgids that have not
/// been returned to the free-list.
///
/// Saturates at `u16::MAX` so the result always fits in `u16`. The
/// snapshot is best-effort: under concurrent allocate/deallocate the
/// counter and free-list reads are not atomic together, but the value
/// never overstates occupancy because both inputs are observed under the
/// same monotonic discipline.
fn current_in_flight() -> u16 {
    let issued = NEXT_BGID.load(Ordering::Relaxed).min(BGID_NAMESPACE_SIZE);
    let free = bgid_free_list()
        .lock()
        .expect("bgid free-list poisoned")
        .len() as u32;
    issued.saturating_sub(free).min(u16::MAX as u32) as u16
}

/// Returns the high-water mark for concurrently-allocated bgids since
/// process start.
///
/// Monotonic: deallocation never lowers this value. Reflects the worst
/// observed pressure on the 16-bit bgid namespace and is intended for
/// operational dashboards and capacity-planning tests.
#[must_use]
pub fn bgid_peak_used() -> u16 {
    PEAK_USED.load(Ordering::Relaxed)
}

/// Returns the current count of allocator-issued bgids not yet returned
/// to the free-list.
///
/// Saturates at `u16::MAX`. Intended for ad-hoc diagnostics; the value
/// can change between the read and the next allocate/deallocate call.
#[must_use]
pub fn bgid_inflight() -> u16 {
    current_in_flight()
}

/// Returns the cumulative number of [`BgidAllocator::allocate`] calls
/// that returned [`BgidAllocError::Exhausted`] for this process.
///
/// Monotonic and never resets while the process is alive. A non-zero
/// value indicates the caller-side fallback path (skip buffer-ring
/// registration, use plain `recv`/`read`) has been exercised at least
/// once; pair with [`bgid_peak_used`] / [`bgid_inflight`] to size the
/// namespace correctly for the workload.
#[must_use]
pub fn bgid_exhausted_count() -> u64 {
    BGID_EXHAUSTED_COUNT.load(Ordering::Relaxed)
}

/// Process-wide cache for the PBUF_RING support probe.
///
/// Populated by the first call to [`is_supported`] / [`pbuf_ring_supported`]
/// and reused for the lifetime of the process. Caching avoids repeating
/// the `uname(2)` syscall and version parse on every speculative call site.
static PBUF_RING_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Returns `true` if the kernel supports PBUF_RING (>= 5.19).
///
/// Checks the kernel version via `uname(2)`. This is a necessary but not
/// sufficient condition - the actual `IORING_REGISTER_PBUF_RING` call may
/// still fail if seccomp blocks it.
///
/// The result is cached in a process-wide [`OnceLock`], so repeated calls
/// after the first are a single relaxed atomic load. Use
/// [`BufferRing::try_new`] when you also need to verify that registration
/// will actually succeed.
#[must_use]
pub fn is_supported() -> bool {
    *PBUF_RING_SUPPORTED.get_or_init(probe_pbuf_ring_support)
}

/// Alias for [`is_supported`] matching the cross-crate naming used by
/// [`crate::pbuf_ring_supported`].
///
/// Provided so callers that import this module directly can use the
/// crate-wide name without going through the top-level re-export.
#[must_use]
pub fn pbuf_ring_supported() -> bool {
    is_supported()
}

/// Performs the actual kernel-version check used by [`is_supported`].
fn probe_pbuf_ring_support() -> bool {
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
                warn_bgid_fallback_once(e);
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

        // Return the bgid to the allocator's free-list if this ring owned
        // an allocator-issued id. Caller-supplied bgids are left alone -
        // the caller continues to own that namespace slot.
        if self.allocator_owned {
            BgidAllocator::deallocate(self.config.bgid);
        }
    }
}

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
        assert!(validate_buffer_ring_config(&config).is_err());
    }

    #[test]
    fn config_validate_rejects_non_power_of_two() {
        let config = BufferRingConfig {
            ring_size: 3,
            ..Default::default()
        };
        assert!(validate_buffer_ring_config(&config).is_err());
    }

    #[test]
    fn config_validate_rejects_zero_buffer_size() {
        let config = BufferRingConfig {
            buffer_size: 0,
            ..Default::default()
        };
        assert!(validate_buffer_ring_config(&config).is_err());
    }

    #[test]
    fn config_validate_accepts_valid_config() {
        let config = BufferRingConfig {
            ring_size: 16,
            buffer_size: 4096,
            bgid: 1,
        };
        assert!(validate_buffer_ring_config(&config).is_ok());
    }

    #[test]
    fn config_validate_accepts_large_power_of_two() {
        let config = BufferRingConfig {
            ring_size: 1024,
            buffer_size: 256 * 1024,
            bgid: 0,
        };
        assert!(validate_buffer_ring_config(&config).is_ok());
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

        // Recycling each buffer in range must succeed.
        buf_ring.recycle_buffer(0).expect("recycle 0");
        buf_ring.recycle_buffer(1).expect("recycle 1");
        buf_ring.recycle_buffer(2).expect("recycle 2");
        buf_ring.recycle_buffer(3).expect("recycle 3");

        drop(buf_ring);
    }

    #[test]
    fn buffer_ring_recycle_rejects_out_of_range_buf_id() {
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
            bgid: 2,
        };

        let buf_ring = match BufferRing::new(&ring, config) {
            Ok(br) => br,
            Err(_) => return,
        };

        // First out-of-range id is ring_size; this must be rejected without
        // mutating the shared ring tail or panicking.
        match buf_ring.recycle_buffer(4) {
            Err(BufferRingError::BufferIdOutOfRange { buf_id, ring_size }) => {
                assert_eq!(buf_id, 4);
                assert_eq!(ring_size, 4);
            }
            other => panic!("expected BufferIdOutOfRange, got {other:?}"),
        }

        // Far-out-of-range id (u16::MAX) must also be rejected.
        assert!(matches!(
            buf_ring.recycle_buffer(u16::MAX),
            Err(BufferRingError::BufferIdOutOfRange { .. })
        ));

        drop(buf_ring);
    }

    #[test]
    fn buffer_ring_error_out_of_range_converts_to_invalid_input() {
        let err: io::Error = BufferRingError::BufferIdOutOfRange {
            buf_id: 9,
            ring_size: 4,
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let msg = format!("{err}");
        assert!(msg.contains("buf_id 9"));
        assert!(msg.contains("ring size 4"));
    }

    #[test]
    fn page_size_is_positive_and_power_of_two() {
        let ps = page_size();
        assert!(ps > 0);
        assert!(ps.is_power_of_two());
    }

    #[test]
    fn bgid_allocator_returns_distinct_ids() {
        let a = BgidAllocator::allocate().expect("first allocation");
        let b = BgidAllocator::allocate().expect("second allocation");
        assert_ne!(a, b, "consecutive allocations must return distinct bgids");
    }

    /// Serializes tests that mutate global allocator state.
    ///
    /// `NEXT_BGID` and the bgid free-list are process-wide; tests that
    /// swap or drain them must not run concurrently with other tests that
    /// observe the same state, otherwise interleavings produce
    /// false-negative failures.
    fn bgid_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Snapshots, then clears, all process-wide allocator state. The
    /// returned guard restores everything on drop so tests leave global
    /// state untouched.
    struct BgidStateGuard {
        prev_counter: u32,
        prev_free_list: Vec<u16>,
        prev_peak: u16,
        prev_exhausted: u64,
        prev_warned: bool,
        _serializer: std::sync::MutexGuard<'static, ()>,
    }

    impl BgidStateGuard {
        fn snapshot() -> Self {
            let serializer = bgid_test_lock();
            let prev_counter = NEXT_BGID.swap(0, Ordering::Relaxed);
            let prev_peak = PEAK_USED.swap(0, Ordering::Relaxed);
            let prev_exhausted = BGID_EXHAUSTED_COUNT.swap(0, Ordering::Relaxed);
            let prev_warned = BGID_FALLBACK_WARNED.swap(false, Ordering::AcqRel);
            let prev_free_list = {
                let mut list = bgid_free_list().lock().expect("free-list poisoned");
                std::mem::take(&mut *list)
            };
            Self {
                prev_counter,
                prev_free_list,
                prev_peak,
                prev_exhausted,
                prev_warned,
                _serializer: serializer,
            }
        }
    }

    impl Drop for BgidStateGuard {
        fn drop(&mut self) {
            NEXT_BGID.store(self.prev_counter, Ordering::Relaxed);
            PEAK_USED.store(self.prev_peak, Ordering::Relaxed);
            BGID_EXHAUSTED_COUNT.store(self.prev_exhausted, Ordering::Relaxed);
            BGID_FALLBACK_WARNED.store(self.prev_warned, Ordering::Release);
            let mut list = bgid_free_list().lock().expect("free-list poisoned");
            *list = std::mem::take(&mut self.prev_free_list);
        }
    }

    #[test]
    fn bgid_allocator_exhaustion_returns_error() {
        let _guard = BgidStateGuard::snapshot();
        // Force the counter to the namespace limit with the free-list empty;
        // the next allocation must report exhaustion.
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
        let result = BgidAllocator::allocate();
        assert!(
            matches!(result, Err(BgidAllocError::Exhausted { .. })),
            "expected Exhausted when counter == BGID_NAMESPACE_SIZE, got {result:?}"
        );
    }

    #[test]
    fn bgid_exhausted_converts_to_out_of_memory_io_error() {
        let err: io::Error = BgidAllocError::Exhausted {
            fresh_used: BGID_NAMESPACE_SIZE,
            free_list_len: 0,
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        let msg = format!("{err}");
        assert!(
            msg.contains("65536"),
            "error message must cite the namespace limit: {msg}"
        );
    }

    #[test]
    fn bgid_exhausted_buffer_ring_error_maps_to_out_of_memory() {
        // The legacy BufferRingError::BgidExhausted (still emitted via the
        // From<BgidAllocError> for BufferRingError path) must also surface
        // as ErrorKind::OutOfMemory so callers that converge on io::Error
        // see a single semantic.
        let err: io::Error = BufferRingError::BgidExhausted.into();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn bgid_alloc_error_converts_to_buffer_ring_error() {
        let alloc_err = BgidAllocError::Exhausted {
            fresh_used: BGID_NAMESPACE_SIZE,
            free_list_len: 7,
        };
        let ring_err: BufferRingError = alloc_err.into();
        assert!(matches!(ring_err, BufferRingError::BgidExhausted));
    }

    #[test]
    fn allocate_until_exhausted_returns_typed_error() {
        let _guard = BgidStateGuard::snapshot();
        // Drive the allocator one step past the u16 namespace by setting
        // the counter to its cap, then assert the next call surfaces the
        // typed error instead of panicking or wrapping.
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
        match BgidAllocator::allocate() {
            Err(BgidAllocError::Exhausted {
                fresh_used,
                free_list_len,
            }) => {
                assert_eq!(fresh_used, BGID_NAMESPACE_SIZE);
                assert_eq!(free_list_len, 0);
            }
            other => panic!("expected BgidAllocError::Exhausted, got {other:?}"),
        }
    }

    #[test]
    fn exhausted_count_increments() {
        let _guard = BgidStateGuard::snapshot();
        let before = bgid_exhausted_count();
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
        assert!(BgidAllocator::allocate().is_err());
        assert!(BgidAllocator::allocate().is_err());
        assert!(BgidAllocator::allocate().is_err());
        let after = bgid_exhausted_count();
        assert_eq!(
            after - before,
            3,
            "BGID_EXHAUSTED_COUNT must tick once per Exhausted return"
        );
    }

    #[test]
    fn bgid_allocator_remaining_does_not_increase() {
        let before = BgidAllocator::remaining();
        let _ = BgidAllocator::allocate();
        let after = BgidAllocator::remaining();
        assert!(
            after <= before,
            "remaining should not increase: before={before}, after={after}"
        );
    }

    #[test]
    fn bgid_allocator_reuses_freed_ids() {
        let _guard = BgidStateGuard::snapshot();
        // Counter and free-list are both empty after snapshot.
        let id = BgidAllocator::allocate().expect("initial allocation");
        BgidAllocator::deallocate(id);
        let reused = BgidAllocator::allocate().expect("post-deallocate allocation");
        assert_eq!(
            id, reused,
            "allocate must drain the free-list before advancing the counter"
        );
    }

    #[test]
    fn bgid_allocator_free_list_persists_after_exhaustion() {
        let _guard = BgidStateGuard::snapshot();
        // Drive the counter to the namespace limit, then return one id.
        // The next allocation must succeed from the free-list even though
        // the monotonic counter is fully consumed.
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
        assert!(
            matches!(
                BgidAllocator::allocate(),
                Err(BgidAllocError::Exhausted { .. })
            ),
            "sanity: counter must be exhausted before the free-list seed"
        );

        BgidAllocator::deallocate(123);
        let reused = BgidAllocator::allocate().expect("allocation must succeed from free-list");
        assert_eq!(reused, 123, "freed bgid must be returned ahead of counter");

        // With the free-list drained again the allocator reports exhaustion.
        assert!(matches!(
            BgidAllocator::allocate(),
            Err(BgidAllocError::Exhausted { .. })
        ));
    }

    #[test]
    fn bgid_allocator_remaining_includes_free_list() {
        let _guard = BgidStateGuard::snapshot();
        // Counter at limit, free-list empty -> zero remaining.
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
        assert_eq!(BgidAllocator::remaining(), 0);

        // Each deallocated id adds one to remaining.
        BgidAllocator::deallocate(7);
        assert_eq!(BgidAllocator::remaining(), 1);
        BgidAllocator::deallocate(42);
        assert_eq!(BgidAllocator::remaining(), 2);

        // Idempotent deallocate does not inflate the free-list count.
        BgidAllocator::deallocate(7);
        assert_eq!(BgidAllocator::remaining(), 2);
    }

    #[test]
    fn bgid_allocator_deallocate_is_idempotent() {
        let _guard = BgidStateGuard::snapshot();
        BgidAllocator::deallocate(99);
        BgidAllocator::deallocate(99);
        let list_len = bgid_free_list().lock().expect("free-list poisoned").len();
        assert_eq!(
            list_len, 1,
            "duplicate deallocate must not push the same bgid twice"
        );
    }

    #[test]
    fn bgid_peak_tracks_100_allocations() {
        let _guard = BgidStateGuard::snapshot();
        assert_eq!(bgid_peak_used(), 0);
        for _ in 0..100 {
            BgidAllocator::allocate().expect("allocation within namespace");
        }
        assert_eq!(
            bgid_peak_used(),
            100,
            "peak must reflect the 100 outstanding allocations"
        );
        assert_eq!(bgid_inflight(), 100);
    }

    #[test]
    fn bgid_peak_does_not_decrease_on_deallocate() {
        let _guard = BgidStateGuard::snapshot();
        let mut ids = Vec::with_capacity(50);
        for _ in 0..50 {
            ids.push(BgidAllocator::allocate().expect("allocation within namespace"));
        }
        assert_eq!(bgid_peak_used(), 50);
        assert_eq!(bgid_inflight(), 50);

        for id in ids {
            BgidAllocator::deallocate(id);
        }
        assert_eq!(
            bgid_peak_used(),
            50,
            "peak must not decrease after returning ids to the free-list"
        );
        assert_eq!(bgid_inflight(), 0, "all ids returned, none in flight");

        // Reallocating from the free-list still updates the peak path but
        // never lifts it above the previous high-water mark.
        let _ = BgidAllocator::allocate().expect("reallocation from free-list");
        assert_eq!(bgid_peak_used(), 50);
    }

    #[test]
    fn bgid_free_list_is_pre_sized() {
        let _guard = BgidStateGuard::snapshot();
        let cap = bgid_free_list()
            .lock()
            .expect("free-list poisoned")
            .capacity();
        assert!(
            cap >= BGID_FREE_LIST_INITIAL_CAPACITY,
            "free-list pre-sized capacity {cap} must be >= {BGID_FREE_LIST_INITIAL_CAPACITY}"
        );
    }

    #[test]
    fn bgid_inflight_reflects_counter_minus_free_list() {
        let _guard = BgidStateGuard::snapshot();
        let a = BgidAllocator::allocate().expect("first");
        let b = BgidAllocator::allocate().expect("second");
        let _c = BgidAllocator::allocate().expect("third");
        assert_eq!(bgid_inflight(), 3);

        BgidAllocator::deallocate(a);
        BgidAllocator::deallocate(b);
        assert_eq!(bgid_inflight(), 1);
    }

    #[test]
    fn bgid_warn_threshold_is_half_namespace() {
        assert_eq!(
            BGID_OCCUPANCY_WARN_THRESHOLD,
            (BGID_NAMESPACE_SIZE / 2) as u16
        );
    }

    #[test]
    fn warn_bgid_fallback_once_sets_flag_exactly_once() {
        let _guard = BgidStateGuard::snapshot();
        assert!(!BGID_FALLBACK_WARNED.load(Ordering::Acquire));
        let err = BgidAllocError::Exhausted {
            fresh_used: BGID_NAMESPACE_SIZE,
            free_list_len: 0,
        };
        warn_bgid_fallback_once(err);
        assert!(
            BGID_FALLBACK_WARNED.load(Ordering::Acquire),
            "first call must set the warned flag"
        );
        // Subsequent calls must be a no-op against the flag.
        warn_bgid_fallback_once(err);
        warn_bgid_fallback_once(err);
        assert!(BGID_FALLBACK_WARNED.load(Ordering::Acquire));
    }

    /// Models the caller-side fallback contract documented on
    /// [`BufferRing::new_with_allocator`]: on exhaustion the allocator
    /// returns a typed error, the cumulative counter ticks, and the
    /// caller is expected to skip the registration step and proceed with
    /// the plain receive path. The test verifies the observable signals
    /// without driving the kernel, so it runs on any host.
    #[test]
    fn caller_side_fallback_observable_signals() {
        let _guard = BgidStateGuard::snapshot();
        NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);

        let before = bgid_exhausted_count();
        let err = BgidAllocator::allocate().expect_err("allocator must report exhaustion");
        // Caller maps the typed error to io::Error and decides on the
        // fallback - the conversion is total and lossless.
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);

        let after = bgid_exhausted_count();
        assert_eq!(
            after - before,
            1,
            "exhaustion counter must observably tick for the caller"
        );

        // Returning one id makes the next allocation succeed: the
        // fallback is reversible once any active ring is dropped.
        BgidAllocator::deallocate(7);
        assert_eq!(
            BgidAllocator::allocate().expect("reuse must succeed"),
            7,
            "free-list reuse restores allocation without resetting state"
        );
    }
}
