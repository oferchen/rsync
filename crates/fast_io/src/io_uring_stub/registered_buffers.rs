//! Stub registered-buffer module mirroring
//! `io_uring::registered_buffers`. Not available on this platform.

pub use crate::io_uring_common::{RegisteredBufferStats, RegisteredBufferStatus};
use std::io;

/// Stub registered buffer group.
///
/// `try_new` always returns `None` and `new` always returns `Unsupported`.
#[derive(Debug)]
pub struct RegisteredBufferGroup {
    _private: (),
}

impl RegisteredBufferGroup {
    /// Always returns an `Unsupported` error on this platform.
    pub fn new(_ring: &(), _buffer_size: usize, _count: usize) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring buffer registration is not available on this platform",
        ))
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(_ring: &(), _buffer_size: usize, _count: usize) -> Option<Self> {
        None
    }

    /// Stub registration-aware constructor.
    ///
    /// Returns [`RegisteredBufferStatus::RegistrationFailed`] when the
    /// caller opts in (mirroring the Linux failure path) and
    /// [`RegisteredBufferStatus::Disabled`] when the caller opts out.
    pub fn try_new_with_status(
        _ring: &(),
        _buffer_size: usize,
        _count: usize,
        enabled: bool,
    ) -> (Option<Self>, RegisteredBufferStatus) {
        if enabled {
            (
                None,
                RegisteredBufferStatus::RegistrationFailed {
                    reason: "io_uring buffer registration is not available on this platform"
                        .to_string(),
                },
            )
        } else {
            (None, RegisteredBufferStatus::Disabled)
        }
    }

    /// Returns 0 on this platform (no group can exist).
    #[must_use]
    pub fn count(&self) -> usize {
        0
    }

    /// Returns 0 on this platform (no group can exist).
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        0
    }

    /// Returns 0 on this platform (no slots can be available).
    #[must_use]
    pub fn available(&self) -> usize {
        0
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn checkout(&self) -> Option<RegisteredBufferSlot<'_>> {
        None
    }

    /// Returns a zeroed snapshot on this platform.
    #[must_use]
    pub fn stats(&self) -> RegisteredBufferStats {
        RegisteredBufferStats {
            total_acquires: 0,
            total_misses: 0,
        }
    }

    /// No-op on this platform.
    pub fn unregister(&self, _ring: &()) -> io::Result<()> {
        Ok(())
    }
}

/// Stub registered buffer slot (never constructed).
pub struct RegisteredBufferSlot<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl RegisteredBufferSlot<'_> {
    /// Returns 0 (the slot cannot be constructed on this platform).
    #[must_use]
    pub fn buf_index(&self) -> u16 {
        0
    }

    /// Returns a null mutable pointer.
    #[must_use]
    pub fn as_mut_ptr(&self) -> *mut u8 {
        std::ptr::null_mut()
    }

    /// Returns a null pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        std::ptr::null()
    }

    /// Returns 0 (the slot cannot be constructed on this platform).
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        0
    }
}
