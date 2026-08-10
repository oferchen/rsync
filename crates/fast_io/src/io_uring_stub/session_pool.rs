//! Stub session ring pool mirroring [`crate::io_uring::session_pool`].
//!
//! The constructors always fail; [`SessionRingPool::try_new`] returns `None`,
//! [`SessionRingPool::new`] returns `Unsupported`, and
//! [`SessionRingPool::acquire`] always returns `None`.

use crate::io_uring_common::IoUringConfig;
use std::io;
use std::ops::{Deref, DerefMut};

/// Stub plain-data configuration for the session ring pool.
///
/// Exposes the same field layout as the Linux struct so cross-platform
/// callers compile without `cfg`-gating. The fields are inert on this
/// platform.
#[derive(Debug, Clone)]
pub struct SessionPoolConfig {
    /// Number of rings the pool would allocate on Linux.
    pub ring_count: usize,
    /// Per-ring submission queue depth.
    pub entries_per_ring: u32,
    /// Ring setup flags.
    pub flags: u32,
    /// Idle timeout (milliseconds) for the SQPOLL kernel thread.
    pub sqpoll_idle_ms: u32,
}

impl Default for SessionPoolConfig {
    fn default() -> Self {
        Self::from_io_uring_config(&IoUringConfig::default())
    }
}

impl SessionPoolConfig {
    /// Derives a stub config from the per-ring [`IoUringConfig`].
    #[must_use]
    pub fn from_io_uring_config(config: &IoUringConfig) -> Self {
        Self {
            ring_count: 1,
            entries_per_ring: config.sq_entries,
            flags: 0,
            sqpoll_idle_ms: config.sqpoll_idle_ms,
        }
    }

    /// Returns a config with `ring_count` overridden.
    #[must_use]
    pub fn with_ring_count(mut self, ring_count: usize) -> Self {
        self.ring_count = ring_count.max(1);
        self
    }
}

/// Stub session ring pool. Cannot be constructed on this platform.
pub struct SessionRingPool {
    _private: (),
}

impl SessionRingPool {
    /// Always returns `Unsupported` on this platform.
    pub fn new(_config: SessionPoolConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring session ring pool is not available on this platform",
        ))
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(_config: SessionPoolConfig) -> Option<Self> {
        None
    }

    /// Returns 0 on this platform; the pool can never be constructed.
    #[must_use]
    pub fn ring_count(&self) -> usize {
        0
    }

    /// Stub configuration accessor; never callable in practice because the
    /// pool cannot be constructed on this platform.
    #[must_use]
    pub fn config(&self) -> &SessionPoolConfig {
        unreachable!("SessionRingPool cannot be constructed on this platform")
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn acquire(&self) -> Option<RingLease<'_>> {
        None
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn acquire_slot(&self, _slot: usize) -> Option<RingLease<'_>> {
        None
    }
}

/// Stub RAII lease handle. Cannot be constructed on this platform.
///
/// `Deref` / `DerefMut` impls match the Linux signatures so cross-platform
/// callers can name the type but the lease itself is unreachable.
pub struct RingLease<'pool> {
    _private: std::marker::PhantomData<&'pool ()>,
}

impl<'pool> RingLease<'pool> {
    /// Always unreachable on this platform.
    #[must_use]
    pub fn slot(&self) -> usize {
        unreachable!("RingLease cannot be constructed on this platform")
    }
}

impl<'pool> Deref for RingLease<'pool> {
    type Target = ();

    fn deref(&self) -> &Self::Target {
        unreachable!("RingLease cannot be constructed on this platform")
    }
}

impl<'pool> DerefMut for RingLease<'pool> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unreachable!("RingLease cannot be constructed on this platform")
    }
}

/// Stub per-thread ring pool. Cannot construct a ring on this platform.
///
/// Mirrors the Linux [`crate::io_uring::ThreadLocalRingPool`] surface so
/// cross-platform callers can build against the same type without
/// `cfg`-gating the call site. [`acquire`](ThreadLocalRingPool::acquire)
/// always returns `None`, matching the Linux behaviour when
/// `io_uring_setup(2)` is rejected.
#[derive(Clone)]
pub struct ThreadLocalRingPool {
    _private: (),
}

impl ThreadLocalRingPool {
    /// Constructs a stub pool. The instance is inert: no ring is ever
    /// built and [`acquire`](Self::acquire) always returns `None`.
    #[must_use]
    pub fn new(_config: SessionPoolConfig) -> Self {
        Self { _private: () }
    }

    /// Stub configuration accessor; never callable in practice because
    /// the pool never owns a ring on this platform.
    #[must_use]
    pub fn config(&self) -> &SessionPoolConfig {
        unreachable!("ThreadLocalRingPool::config is unreachable on this platform")
    }

    /// Returns 0 on this platform; no thread ever holds a ring.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        0
    }

    /// Returns the calling thread's id; provided for parity with the
    /// Linux implementation so tests can compile cross-platform.
    #[must_use]
    pub fn current_thread_id() -> std::thread::ThreadId {
        std::thread::current().id()
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn acquire(&self) -> Option<ThreadLocalRingLease<'_>> {
        None
    }
}

/// Stub RAII lease for the per-thread pool. Cannot be constructed on this
/// platform.
pub struct ThreadLocalRingLease<'pool> {
    _private: std::marker::PhantomData<&'pool ()>,
}

impl<'pool> Deref for ThreadLocalRingLease<'pool> {
    type Target = ();

    fn deref(&self) -> &Self::Target {
        unreachable!("ThreadLocalRingLease cannot be constructed on this platform")
    }
}

impl<'pool> DerefMut for ThreadLocalRingLease<'pool> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unreachable!("ThreadLocalRingLease cannot be constructed on this platform")
    }
}
