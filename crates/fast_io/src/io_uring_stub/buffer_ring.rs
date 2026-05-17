//! Stub provided-buffer ring mirroring [`crate::io_uring::buffer_ring`].
//!
//! Re-exports the shared [`BufferRingConfig`] and [`BufferRingError`] from
//! [`crate::io_uring_common`] and supplies the opaque [`BufferRing`] /
//! [`BgidAllocator`] handles that only exist as compile-time placeholders
//! here.

pub use crate::io_uring_common::{BufferRingConfig, BufferRingError, buffer_id_from_cqe_flags};

/// Stub provided buffer ring.
///
/// [`new`](Self::new) always returns an error and [`try_new`](Self::try_new)
/// always returns `None` on this platform.
#[derive(Debug)]
pub struct BufferRing {
    _private: (),
}

impl BufferRing {
    /// Always returns `BufferRingError::Unsupported` on this platform.
    pub fn new(_ring: &(), _config: BufferRingConfig) -> Result<Self, BufferRingError> {
        Err(BufferRingError::Unsupported)
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(_ring: &(), _config: BufferRingConfig) -> Option<Self> {
        None
    }

    /// Always returns `BufferRingError::Unsupported` on this platform.
    ///
    /// Mirrors the Linux signature so cross-platform callers compile
    /// without `cfg`-gating.
    pub fn new_with_allocator(
        _ring: &(),
        _config: BufferRingConfig,
    ) -> Result<Self, BufferRingError> {
        Err(BufferRingError::Unsupported)
    }

    /// Returns 0 (the stub never constructs an instance).
    #[must_use]
    pub fn bgid(&self) -> u16 {
        0
    }

    /// Returns 0 (the stub never constructs an instance).
    #[must_use]
    pub fn ring_size(&self) -> u32 {
        0
    }

    /// Returns 0 (the stub never constructs an instance).
    #[must_use]
    pub fn buffer_size(&self) -> u32 {
        0
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn buffer_ptr(&self, _buf_id: u16) -> Option<*const u8> {
        None
    }

    /// No-op on this platform; mirrors the real signature so cross-platform
    /// callers can use `?` without `cfg`-gating.
    pub fn recycle_buffer(&self, _buf_id: u16) -> Result<(), BufferRingError> {
        Ok(())
    }

    /// Stub configuration accessor; never callable in practice because
    /// `BufferRing` cannot be constructed on this platform.
    #[must_use]
    pub fn config(&self) -> &BufferRingConfig {
        unreachable!("BufferRing cannot be constructed on this platform")
    }
}

/// Returns `false` on non-Linux platforms.
#[must_use]
pub fn is_supported() -> bool {
    false
}

/// Returns `false` on non-Linux platforms.
///
/// Cross-platform alias for [`is_supported`] matching the
/// [`crate::pbuf_ring_supported`] re-export.
#[must_use]
pub fn pbuf_ring_supported() -> bool {
    false
}

/// Stub allocator for buffer group IDs.
///
/// Always reports the namespace as exhausted so callers exercise their
/// fall-back paths.
pub struct BgidAllocator;

impl BgidAllocator {
    /// Always returns [`BufferRingError::BgidExhausted`] on this platform.
    pub fn allocate() -> Result<u16, BufferRingError> {
        Err(BufferRingError::BgidExhausted)
    }

    /// No-op on this platform.
    pub fn deallocate(_bgid: u16) {}

    /// Always returns 0 on this platform.
    #[must_use]
    pub fn remaining() -> u32 {
        0
    }
}

/// Returns 0 on non-Linux platforms; no bgids are ever issued here.
///
/// Mirrors the Linux accessor so cross-platform callers and metrics
/// exporters compile without `cfg`-gating.
#[must_use]
pub fn bgid_peak_used() -> u16 {
    0
}

/// Returns 0 on non-Linux platforms; no bgids are ever issued here.
///
/// Mirrors the Linux accessor so cross-platform callers and metrics
/// exporters compile without `cfg`-gating.
#[must_use]
pub fn bgid_inflight() -> u16 {
    0
}
