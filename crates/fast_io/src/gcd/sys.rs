//! Raw FFI bindings for the subset of Grand Central Dispatch (`libdispatch`)
//! needed by the `dispatch_io` async file I/O primitives.
//!
//! Only the symbols consumed by [`super::GcdQueue`], [`super::GcdReader`], and
//! [`super::GcdWriter`] are declared here. Every opaque dispatch object
//! (`dispatch_io_t`, `dispatch_data_t`, `dispatch_queue_t`) is reference
//! counted; callers balance every create/copy with a
//! [`dispatch_release`]. The completion and cleanup handlers are Objective-C
//! blocks, supplied by the safe wrappers as `block2::RcBlock` pointers.
//!
//! References: `<dispatch/io.h>`, `<dispatch/data.h>`, `<dispatch/queue.h>`
//! from the macOS SDK.

#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int, c_long, c_ulong};

/// Opaque `dispatch_object_t` family pointer. All dispatch handles below are
/// distinct opaque pointers; we model them as raw `*mut c_void` because the
/// concrete struct layouts are private to `libdispatch`.
pub type dispatch_object_t = *mut c_void;
/// Opaque `dispatch_queue_t`.
pub type dispatch_queue_t = *mut c_void;
/// Opaque `dispatch_io_t` channel handle.
pub type dispatch_io_t = *mut c_void;
/// Opaque `dispatch_data_t` immutable byte buffer.
pub type dispatch_data_t = *mut c_void;

/// ABI-compatible stand-in for the C99 `_Bool` `done` flag passed to the
/// `dispatch_io` completion block. `_Bool` is a single byte (0 / 1) and is
/// passed identically to `u8` in the block ABI on all supported targets;
/// `objc2` intentionally does not implement `Encode` for Rust `bool`, so the
/// handler closures take this byte and test `!= 0`.
pub type dispatch_bool_t = u8;

/// `dispatch_io_type_t` - `DISPATCH_IO_STREAM` (0) reads/writes sequentially
/// from the fd's current logical position, matching a `Read`/`Write` stream.
pub const DISPATCH_IO_STREAM: dispatch_io_type_t = 0;
/// `dispatch_io_type_t`.
pub type dispatch_io_type_t = c_ulong;

/// `dispatch_get_global_queue` priority for a default-priority concurrent
/// queue (`DISPATCH_QUEUE_PRIORITY_DEFAULT` == 0).
pub const DISPATCH_QUEUE_PRIORITY_DEFAULT: c_long = 0;

/// Sentinel meaning "read/write until EOF / all bytes consumed"
/// (`SIZE_MAX`), accepted by `dispatch_io_read`/`dispatch_io_write` `length`.
pub const DISPATCH_IO_READ_ALL: usize = usize::MAX;

unsafe extern "C" {
    /// Returns a global concurrent queue at the given priority. Global queues
    /// are process-wide singletons and must **not** be released.
    pub fn dispatch_get_global_queue(identifier: c_long, flags: c_ulong) -> dispatch_queue_t;

    /// Creates a new serial dispatch queue. Returned handle is owned by the
    /// caller and must be balanced with [`dispatch_release`].
    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> dispatch_queue_t;

    /// Releases (decrements the reference count of) any dispatch object.
    pub fn dispatch_release(object: dispatch_object_t);

    /// Creates a `dispatch_io_t` channel over `fd` of the given type. The
    /// `cleanup_handler` block runs once the channel is closed and no longer
    /// references `fd`, receiving the final error code. Returns a
    /// caller-owned handle to be balanced with [`dispatch_release`], or null
    /// on failure.
    pub fn dispatch_io_create(
        io_type: dispatch_io_type_t,
        fd: c_int,
        queue: dispatch_queue_t,
        cleanup_handler: *mut c_void,
    ) -> dispatch_io_t;

    /// Schedules an asynchronous read of up to `length` bytes starting at
    /// `offset`. For `DISPATCH_IO_STREAM` channels `offset` is ignored and the
    /// read advances the fd position. The `io_handler` block is invoked
    /// (possibly repeatedly) on `queue` with `(done, data, error)`.
    pub fn dispatch_io_read(
        channel: dispatch_io_t,
        offset: i64,
        length: usize,
        queue: dispatch_queue_t,
        io_handler: *mut c_void,
    );

    /// Schedules an asynchronous write of `data` starting at `offset`. For
    /// `DISPATCH_IO_STREAM` channels `offset` is ignored. The `io_handler`
    /// block is invoked with `(done, remaining_data, error)`.
    pub fn dispatch_io_write(
        channel: dispatch_io_t,
        offset: i64,
        data: dispatch_data_t,
        queue: dispatch_queue_t,
        io_handler: *mut c_void,
    );

    /// Closes a channel. `flags` of 0 performs a graceful close after pending
    /// I/O completes; the cleanup handler fires afterwards.
    pub fn dispatch_io_close(channel: dispatch_io_t, flags: c_ulong);

    /// Creates an immutable `dispatch_data_t` wrapping `size` bytes copied
    /// from `buffer` (when `destructor` is `DISPATCH_DATA_DESTRUCTOR_DEFAULT`,
    /// i.e. null, the bytes are copied and owned by the data object). Returns a
    /// caller-owned handle to be balanced with [`dispatch_release`].
    pub fn dispatch_data_create(
        buffer: *const c_void,
        size: usize,
        queue: dispatch_queue_t,
        destructor: *mut c_void,
    ) -> dispatch_data_t;

    /// Returns the logical byte length of a `dispatch_data_t`.
    pub fn dispatch_data_get_size(data: dispatch_data_t) -> usize;

    /// Returns a new `dispatch_data_t` whose bytes are mapped into one
    /// contiguous region. Writes the contiguous pointer and length through the
    /// out-params. The mapped pointer stays valid until the returned data
    /// object is released. Returns a caller-owned handle.
    pub fn dispatch_data_create_map(
        data: dispatch_data_t,
        buffer_ptr: *mut *const c_void,
        size_ptr: *mut usize,
    ) -> dispatch_data_t;
}
