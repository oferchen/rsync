//! Stub per-thread io_uring ring mirroring [`crate::io_uring::per_thread_ring`].
//!
//! The Linux backend lazily constructs an `io_uring::IoUring` per thread on
//! first call to `with_ring`. On every other platform the primitive is
//! inert: `with_ring` always returns [`std::io::ErrorKind::Unsupported`]
//! without invoking the closure so callers can compile cross-platform
//! against a single surface and dispatch on the returned error.
//!
//! See `docs/design/iur-2-per-thread-rings.md` for the hybrid topology this
//! primitive backs (IUR-3.a foundational piece).

use std::io;

/// Default submission queue depth declared for cross-platform API parity.
///
/// The Linux backend uses this value when lazily constructing the
/// per-thread ring. On non-Linux targets the constant is retained so
/// callers can compile against the same surface; the stub `with_ring`
/// always returns [`io::ErrorKind::Unsupported`].
pub const DEFAULT_RING_DEPTH: u32 = 64;

/// Always returns [`io::ErrorKind::Unsupported`] on this platform.
///
/// Mirrors the Linux signature so call sites compile cross-platform
/// without `cfg`-gating. The closure is intentionally never invoked on
/// the stub path so callers cannot accidentally rely on stub-side ring
/// construction.
pub fn with_ring<F, R>(_f: F) -> io::Result<R>
where
    F: FnOnce(&mut ()) -> io::Result<R>,
{
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "per-thread io_uring ring is not available on this platform",
    ))
}
