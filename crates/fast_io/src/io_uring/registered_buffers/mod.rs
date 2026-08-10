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
//! [`RegisteredBufferGroup`] does not hold a reference to the [`io_uring::IoUring`]
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
//! `super::file_reader` and `super::file_writer`.
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
//!
//! # Submodule layout
//!
//! - `registry` - the [`RegisteredBufferGroup`] coordinator, slot allocator,
//!   and [`RegisteredBufferSlot`] handle.
//! - `stats` - re-exports the telemetry types from `io_uring_common`.
//! - `submit` - `ReadFixed`/`WriteFixed` batch submission helpers and the
//!   [`RegisteredBufferSlotInfo`] passed between callers and helpers.

mod registry;
mod stats;
mod submit;

#[cfg(test)]
mod tests;

/// Maximum number of buffers that can be registered with io_uring.
///
/// The kernel typically allows up to 1024 registered buffers. We cap at this
/// limit to avoid kernel rejections.
pub(super) const MAX_REGISTERED_BUFFERS: usize = 1024;

/// Returns the system page size.
pub(crate) fn page_size() -> usize {
    // Safety: sysconf is always safe to call with _SC_PAGESIZE.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        4096 // Fallback to 4K, the most common page size.
    } else {
        size as usize
    }
}

pub use registry::{RegisteredBufferGroup, RegisteredBufferSlot};
pub use stats::{RegisteredBufferStats, RegisteredBufferStatus};
#[doc(hidden)]
pub use submit::{RegisteredBufferSlotInfo, submit_read_fixed_batch};
