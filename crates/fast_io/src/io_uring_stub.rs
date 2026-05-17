//! Portable io_uring fallback for non-Linux platforms or when the feature is disabled.
//!
//! Provides the same public API as the real `io_uring` module but always falls
//! back to standard buffered I/O. The [`is_io_uring_available`] function always
//! returns `false`. This module is compiled when either:
//!
//! - The target OS is not Linux, or
//! - The `io_uring` cargo feature is not enabled
//!
//! All cross-platform plain-data types (configs, kernel UAPI constants, error
//! enums, telemetry structs) live in [`crate::io_uring_common`] so they
//! compile identically on every target. This module hosts only the
//! opaque-handle types and "always Unsupported" entry points that are unique
//! to the stub backend - which is the only thing the Linux backend cannot
//! share with us.

#![allow(dead_code)]

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::io_uring_common::IoBackend;
pub use crate::io_uring_common::{
    BufferRingConfig, BufferRingError, IORING_OP_LINKAT, IORING_OP_RENAMEAT, IORING_OP_STATX,
    IoUringConfig, IoUringKernelInfo, LINKAT_MIN_KERNEL, OpTag, RENAME_EXCHANGE, RENAME_NOREPLACE,
    RENAME_WHITEOUT, RegisteredBufferStats, RegisteredBufferStatus, STATX_MIN_KERNEL,
    SharedCompletion, SharedRingConfig, buffer_id_from_cqe_flags,
};
use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

/// Marker type implementing [`IoBackend`] for the no-op stub backend.
///
/// Used by code that needs to query availability through the cross-platform
/// trait without caring which backend was compiled. Always reports the
/// backend as unavailable on this platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubIoUringBackend;

impl IoBackend for StubIoUringBackend {
    fn is_available() -> bool {
        false
    }

    fn availability_reason() -> String {
        "io_uring: disabled (not built for this target)".to_string()
    }
}

/// Check whether io_uring is available (always `false` on this platform).
#[must_use]
pub fn is_io_uring_available() -> bool {
    false
}

/// Returns whether SQPOLL was requested but fell back (always `false` on this platform).
#[must_use]
pub fn sqpoll_fell_back() -> bool {
    false
}

/// Public accessors for kernel version detection used by `--version` output.
///
/// Mirrors the real Linux module so cross-platform callers can use the same
/// import path; every function reports unavailability on this platform.
pub mod config_detail {
    use super::{IoUringKernelInfo, StubIoUringBackend};
    use crate::io_uring_common::IoBackend;

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn parse_kernel_version(_release: &str) -> Option<(u32, u32)> {
        None
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn get_kernel_release_string() -> Option<String> {
        None
    }

    /// Returns a human-readable reason for io_uring unavailability.
    #[must_use]
    pub fn io_uring_availability_reason() -> String {
        StubIoUringBackend::availability_reason()
    }

    /// Returns a stub [`IoUringKernelInfo`] populated for unavailability.
    #[must_use]
    pub fn io_uring_kernel_info() -> IoUringKernelInfo {
        IoUringKernelInfo {
            available: false,
            kernel_major: None,
            kernel_minor: None,
            supported_ops: 0,
            pbuf_ring_supported: false,
            reason: io_uring_availability_reason(),
        }
    }
}

/// Stub module for provided buffer ring (not available on this platform).
///
/// Re-exports the shared [`BufferRingConfig`] and [`BufferRingError`] from
/// [`crate::io_uring_common`] and supplies the opaque [`BufferRing`] /
/// [`BgidAllocator`] handles that only exist as compile-time placeholders
/// here.
pub mod buffer_ring {
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
}

/// Stub module for registered buffer types (not available on this platform).
pub mod registered_buffers {
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
}

pub use buffer_ring::{BgidAllocator, BufferRing, pbuf_ring_supported};
pub use linkat::{
    LinkAtArgs, build_linkat_sqe, build_linkat_sqe_unchecked, linkat_supported,
    submit_linkat_blocking,
};
pub use registered_buffers::{RegisteredBufferGroup, RegisteredBufferSlot};
pub use renameat2::{
    RenameAt2Args, build_renameat2_sqe, build_renameat2_sqe_unchecked, renameat2_blocking,
    renameat2_supported,
};
pub use statx::{
    StatxArgs, StatxResult, build_statx_sqe, build_statx_sqe_unchecked, statx_supported,
    submit_statx_batch, submit_statx_blocking,
};

/// Stub `linkat` module mirroring [`crate::io_uring::linkat`] on non-Linux
/// platforms or when the `io_uring` cargo feature is disabled.
pub mod linkat {
    pub use crate::io_uring_common::{IORING_OP_LINKAT, LINKAT_MIN_KERNEL};
    use std::ffi::CStr;
    use std::io;

    /// Borrowed arguments for an `IORING_OP_LINKAT` submission. On the stub
    /// the struct exists only so cross-platform call sites compile; no SQE
    /// is ever built.
    #[derive(Debug)]
    pub struct LinkAtArgs<'a> {
        /// Directory file descriptor that resolves `old_path`.
        pub old_dirfd: i32,
        /// Source path of the existing inode being hardlinked.
        pub old_path: &'a CStr,
        /// Directory file descriptor that resolves `new_path`.
        pub new_dirfd: i32,
        /// Destination path of the new hardlink.
        pub new_path: &'a CStr,
        /// Flags passed to the kernel.
        pub flags: i32,
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn linkat_supported() -> bool {
        false
    }

    /// Always returns `Unsupported` on this platform.
    pub fn build_linkat_sqe(_args: LinkAtArgs<'_>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_LINKAT is not available on this platform",
        ))
    }

    /// Stub mirror of the Linux `build_linkat_sqe_unchecked`. No-op.
    pub fn build_linkat_sqe_unchecked(_args: LinkAtArgs<'_>) {}

    /// Always returns `Unsupported` on this platform.
    pub fn submit_linkat_blocking(_args: LinkAtArgs<'_>) -> io::Result<i32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_LINKAT is not available on this platform",
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stub_reports_unsupported() {
            assert!(!linkat_supported());
        }

        #[test]
        fn stub_constants_match_linux_uapi() {
            assert_eq!(IORING_OP_LINKAT, 39);
            assert_eq!(LINKAT_MIN_KERNEL, (5, 15));
        }

        #[test]
        fn stub_build_linkat_sqe_returns_unsupported() {
            let old = CStr::from_bytes_with_nul(b"/tmp/old\0").unwrap();
            let new = CStr::from_bytes_with_nul(b"/tmp/new\0").unwrap();
            let err = build_linkat_sqe(LinkAtArgs {
                old_dirfd: 0,
                old_path: old,
                new_dirfd: 0,
                new_path: new,
                flags: 0,
            })
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        }
    }
}

/// Stub `IORING_OP_RENAMEAT` module mirroring the Linux module.
pub mod renameat2 {
    pub use crate::io_uring_common::{
        IORING_OP_RENAMEAT, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT,
    };
    use std::ffi::CStr;
    use std::io;
    use std::os::raw::c_int;

    /// Stub argument struct mirroring the Linux `RenameAt2Args`.
    #[derive(Debug, Clone, Copy)]
    pub struct RenameAt2Args<'a> {
        /// Directory fd that `old_path` is resolved against.
        pub old_dir_fd: c_int,
        /// Old path (CStr borrow).
        pub old_path: &'a CStr,
        /// Directory fd that `new_path` is resolved against.
        pub new_dir_fd: c_int,
        /// New path (CStr borrow).
        pub new_path: &'a CStr,
        /// Bitwise OR of `RENAME_NOREPLACE`, `RENAME_EXCHANGE`,
        /// `RENAME_WHITEOUT`.
        pub flags: u32,
    }

    /// Stub opaque SQE returned by [`build_renameat2_sqe_unchecked`] on
    /// platforms that lack io_uring. Carries no kernel state; exists only
    /// so cross-platform code compiles.
    #[derive(Debug, Clone, Copy)]
    pub struct StubSqe {
        _private: (),
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn renameat2_supported() -> bool {
        false
    }

    /// Always returns `Unsupported` on this platform.
    pub fn build_renameat2_sqe(_args: RenameAt2Args<'_>) -> io::Result<StubSqe> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_RENAMEAT is not available on this platform",
        ))
    }

    /// Returns a stub SQE; only useful as a constructor smoke test on
    /// non-Linux platforms.
    #[must_use]
    pub fn build_renameat2_sqe_unchecked(_args: RenameAt2Args<'_>) -> StubSqe {
        StubSqe { _private: () }
    }

    /// Always returns `Unsupported` on this platform.
    pub fn renameat2_blocking(_args: RenameAt2Args<'_>) -> io::Result<i32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_RENAMEAT is not available on this platform",
        ))
    }
}

/// Stub `IORING_OP_STATX` module mirroring the Linux module.
pub mod statx {
    pub use crate::io_uring_common::{IORING_OP_STATX, STATX_MIN_KERNEL};
    use std::ffi::CStr;
    use std::io;
    use std::path::Path;

    /// Borrowed arguments for an `IORING_OP_STATX` submission. Stub
    /// definition mirrors the Linux module's struct shape so cross-platform
    /// call sites compile without `cfg`-gating; the stub never submits an
    /// SQE.
    #[derive(Debug)]
    pub struct StatxArgs<'a> {
        /// Directory file descriptor that resolves `pathname`.
        pub dirfd: i32,
        /// Path to stat.
        pub pathname: &'a CStr,
        /// Flags passed to the kernel.
        pub flags: i32,
        /// Mask of fields to request.
        pub mask: u32,
        /// Output buffer (unused on this platform).
        pub statx_buf: &'a mut [u8; 256],
    }

    /// Result of a single statx operation within a batch.
    pub type StatxResult = io::Result<()>;

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn statx_supported() -> bool {
        false
    }

    /// Always returns `Unsupported` on this platform.
    pub fn build_statx_sqe(_args: &mut StatxArgs<'_>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_STATX is not available on this platform",
        ))
    }

    /// Stub mirror of the Linux `build_statx_sqe_unchecked`.
    pub fn build_statx_sqe_unchecked(_args: &mut StatxArgs<'_>) {}

    /// Always returns `Unsupported` on this platform.
    pub fn submit_statx_blocking(
        _dirfd: i32,
        _pathname: &CStr,
        _flags: i32,
        _mask: u32,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_STATX is not available on this platform",
        ))
    }

    /// Always returns `Unsupported` for each path on this platform.
    pub fn submit_statx_batch(
        paths: &[&Path],
        _follow_symlinks: bool,
    ) -> io::Result<Vec<StatxResult>> {
        Ok(paths
            .iter()
            .map(|_| {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "statx is not available on this platform",
                ))
            })
            .collect())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stub_reports_unsupported() {
            assert!(!statx_supported());
        }

        #[test]
        fn stub_constants_match_linux_uapi() {
            assert_eq!(IORING_OP_STATX, 21);
            assert_eq!(STATX_MIN_KERNEL, (5, 11));
        }

        #[test]
        fn stub_submit_statx_batch_returns_unsupported_for_each_path() {
            let dir = tempfile::tempdir().unwrap();
            let p1 = dir.path().join("a.txt");
            let p2 = dir.path().join("b.txt");
            std::fs::write(&p1, b"a").unwrap();
            std::fs::write(&p2, b"b").unwrap();

            let paths: Vec<&Path> = vec![p1.as_path(), p2.as_path()];
            let results = submit_statx_batch(&paths, true).unwrap();
            assert_eq!(results.len(), 2);
            for result in results {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
            }
        }

        #[test]
        fn stub_submit_statx_batch_empty() {
            let paths: &[&Path] = &[];
            let results = submit_statx_batch(paths, true).unwrap();
            assert!(results.is_empty());
        }
    }
}

/// Stub shared-ring module mirroring the Linux module on non-Linux platforms
/// or when the `io_uring` cargo feature is disabled.
///
/// Every constructor returns `None` / `Unsupported`, so callers fall back to
/// the per-channel ring path or to standard buffered I/O.
pub mod shared_ring {
    pub use crate::io_uring_common::{OpTag, SharedCompletion, SharedRingConfig};
    use std::io;
    use std::os::raw::c_int;

    /// Stub `SharedRing`. All constructors return `Unsupported` / `None`.
    pub struct SharedRing {
        _private: (),
    }

    impl SharedRing {
        /// Always returns `None` on this platform.
        #[must_use]
        pub fn try_new(
            _reader_fd: c_int,
            _writer_fd: c_int,
            _config: &SharedRingConfig,
        ) -> Option<Self> {
            None
        }

        /// Always returns `Unsupported` on this platform.
        pub fn new(
            _reader_fd: c_int,
            _writer_fd: c_int,
            _config: &SharedRingConfig,
        ) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring shared ring is not available on this platform",
            ))
        }

        /// Always returns `false` on this platform.
        #[must_use]
        pub fn poll_add_supported(&self) -> bool {
            false
        }

        /// Always returns `false` on this platform.
        #[must_use]
        pub fn has_registered_buffers(&self) -> bool {
            false
        }

        /// Always returns `-1` on this platform.
        #[must_use]
        pub fn reader_slot(&self) -> i32 {
            -1
        }

        /// Always returns `-1` on this platform.
        #[must_use]
        pub fn writer_slot(&self) -> i32 {
            -1
        }

        /// Always returns `Unsupported` on this platform.
        pub fn submit_read(
            &mut self,
            _op_id: u64,
            _offset: u64,
            _buf: &mut [u8],
        ) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring shared ring is not available on this platform",
            ))
        }

        /// Always returns `Unsupported` on this platform.
        pub fn submit_poll_write(&mut self, _op_id: u64) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring shared ring is not available on this platform",
            ))
        }

        /// Always returns `Unsupported` on this platform.
        pub fn submit_send(&mut self, _op_id: u64, _data: &[u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring shared ring is not available on this platform",
            ))
        }

        /// Always returns `Unsupported` on this platform.
        pub fn submit_and_wait(&mut self, _wait_for: usize) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring shared ring is not available on this platform",
            ))
        }

        /// Always returns an empty vector on this platform.
        pub fn reap(&mut self) -> io::Result<Vec<SharedCompletion>> {
            Ok(Vec::new())
        }
    }
}

pub use shared_ring::SharedRing;

/// Stub session ring pool (not available on this platform).
///
/// Mirrors the Linux [`crate::io_uring::session_pool`] surface so
/// cross-platform callers can build against the same types. The
/// constructors always fail; [`SessionRingPool::try_new`] returns `None`,
/// [`SessionRingPool::new`] returns `Unsupported`, and
/// [`SessionRingPool::acquire`] always returns `None`.
pub mod session_pool {
    use super::IoUringConfig;
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
}

pub use session_pool::{RingLease, SessionPoolConfig, SessionRingPool};

/// Stub batched io_uring disk writer (not available on this platform).
#[derive(Debug)]
pub struct IoUringDiskBatch {
    _private: (),
}

impl IoUringDiskBatch {
    /// Always returns an `Unsupported` error on this platform.
    pub fn new(_config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring batched disk writer is not available on this platform",
        ))
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(_config: &IoUringConfig) -> Option<Self> {
        None
    }

    /// Begins a new file for writing (always fails on this platform).
    pub fn begin_file(&mut self, _file: std::fs::File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Writes data to the current file (always fails on this platform).
    pub fn write_data(&mut self, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Flushes buffered data (always fails on this platform).
    pub fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Commits the current file (always fails on this platform).
    pub fn commit_file(&mut self, _do_fsync: bool) -> io::Result<(std::fs::File, u64)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Returns bytes written (always 0 on this platform).
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        0
    }

    /// Returns bytes written including pending buffer (always 0 on this platform).
    #[must_use]
    pub fn bytes_written_with_pending(&self) -> u64 {
        0
    }
}

impl Write for IoUringDiskBatch {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

/// Stub io_uring reader (not available on this platform).
///
/// Opening always fails with `Unsupported`.
pub struct IoUringReader {
    _private: (),
}

impl IoUringReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn open<P: AsRef<Path>>(_path: P, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads data at the specified offset (always fails on this platform).
    pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads the entire file into a vector (always fails on this platform).
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Read for IoUringReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl FileReader for IoUringReader {
    fn size(&self) -> u64 {
        0
    }

    fn position(&self) -> u64 {
        0
    }

    fn seek_to(&mut self, _pos: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

/// Stub io_uring writer (not available on this platform).
///
/// Creating always fails with `Unsupported`.
pub struct IoUringWriter {
    _private: (),
}

impl IoUringWriter {
    /// Always returns an `Unsupported` error on this platform.
    pub fn create<P: AsRef<Path>>(_path: P, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Creates a file with preallocated space (always fails on this platform).
    pub fn create_with_size<P: AsRef<Path>>(
        _path: P,
        _size: u64,
        _config: &IoUringConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Writes data at the specified offset (always fails on this platform).
    pub fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Write for IoUringWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Seek for IoUringWriter {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl FileWriter for IoUringWriter {
    fn bytes_written(&self) -> u64 {
        0
    }

    fn sync(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn preallocate(&mut self, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

/// Factory that creates io_uring readers (always falls back to standard I/O).
#[derive(Debug, Clone, Default)]
pub struct IoUringReaderFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl IoUringReaderFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IoUringConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O (no-op on this platform, always falls back).
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used (always `false`).
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        false
    }
}

/// Reader that can be either io_uring-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IoUringOrStdReader {
    /// io_uring-based reader (never constructed on this platform).
    IoUring(IoUringReader),
    /// Standard buffered reader.
    Std(StdFileReader),
}

impl std::fmt::Debug for IoUringOrStdReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoUring(_) => f.debug_tuple("IoUring").field(&"<io_uring>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Read for IoUringOrStdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.read(buf),
            IoUringOrStdReader::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for IoUringOrStdReader {
    fn size(&self) -> u64 {
        match self {
            IoUringOrStdReader::IoUring(r) => r.size(),
            IoUringOrStdReader::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            IoUringOrStdReader::IoUring(r) => r.position(),
            IoUringOrStdReader::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.seek_to(pos),
            IoUringOrStdReader::Std(r) => r.seek_to(pos),
        }
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        match self {
            IoUringOrStdReader::IoUring(r) => r.read_all(),
            IoUringOrStdReader::Std(r) => r.read_all(),
        }
    }
}

impl FileReaderFactory for IoUringReaderFactory {
    type Reader = IoUringOrStdReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        Ok(IoUringOrStdReader::Std(StdFileReader::open(path)?))
    }
}

/// Factory that creates io_uring writers (always falls back to standard I/O).
#[derive(Debug, Clone, Default)]
pub struct IoUringWriterFactory {
    config: IoUringConfig,
    force_fallback: bool,
}

impl IoUringWriterFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IoUringConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O (no-op on this platform, always falls back).
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether io_uring will be used (always `false`).
    #[must_use]
    pub fn will_use_io_uring(&self) -> bool {
        false
    }
}

/// Writer that can be either io_uring-based or standard I/O.
///
/// On this platform, always uses standard I/O.
pub enum IoUringOrStdWriter {
    /// io_uring-based writer (never constructed on this platform).
    IoUring(IoUringWriter),
    /// Standard buffered writer.
    Std(StdFileWriter),
}

impl std::fmt::Debug for IoUringOrStdWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoUring(_) => f.debug_tuple("IoUring").field(&"<io_uring>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Write for IoUringOrStdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.write(buf),
            IoUringOrStdWriter::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.flush(),
            IoUringOrStdWriter::Std(w) => w.flush(),
        }
    }
}

impl Seek for IoUringOrStdWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.seek(pos),
            IoUringOrStdWriter::Std(w) => w.seek(pos),
        }
    }
}

impl FileWriter for IoUringOrStdWriter {
    fn bytes_written(&self) -> u64 {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.bytes_written(),
            IoUringOrStdWriter::Std(w) => w.bytes_written(),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.sync(),
            IoUringOrStdWriter::Std(w) => w.sync(),
        }
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        match self {
            IoUringOrStdWriter::IoUring(w) => w.preallocate(size),
            IoUringOrStdWriter::Std(w) => w.preallocate(size),
        }
    }
}

impl FileWriterFactory for IoUringWriterFactory {
    type Writer = IoUringOrStdWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        Ok(IoUringOrStdWriter::Std(StdFileWriter::create(path)?))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        Ok(IoUringOrStdWriter::Std(StdFileWriter::create_with_size(
            path, size,
        )?))
    }
}

/// Creates a writer from an existing file handle, respecting the io_uring policy.
///
/// On non-Linux platforms, `Enabled` returns an error since io_uring is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn writer_from_file(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdWriter> {
    writer_from_file_with_depth(file, buffer_capacity, policy, None)
}

/// Like [`writer_from_file`] but accepts an explicit submission queue depth.
pub fn writer_from_file_with_depth(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
    _depth: Option<u32>,
) -> io::Result<IoUringOrStdWriter> {
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    Ok(IoUringOrStdWriter::Std(
        StdFileWriter::from_file_with_capacity(file, buffer_capacity),
    ))
}

/// Creates a reader from a file path, respecting the io_uring policy.
///
/// On non-Linux platforms, `Enabled` returns an error since io_uring is unavailable.
/// `Auto` and `Disabled` both use standard buffered I/O.
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdReader> {
    reader_from_path_with_depth(path, policy, None)
}

/// Like [`reader_from_path`] but accepts an explicit submission queue depth.
pub fn reader_from_path_with_depth<P: AsRef<Path>>(
    path: P,
    policy: crate::IoUringPolicy,
    _depth: Option<u32>,
) -> io::Result<IoUringOrStdReader> {
    if matches!(policy, crate::IoUringPolicy::Enabled) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring requested via --io-uring but not available on this platform",
        ));
    }
    Ok(IoUringOrStdReader::Std(StdFileReader::open(path.as_ref())?))
}

/// Reads an entire file using standard I/O (io_uring not available).
pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(path.as_ref())?;
    reader.read_all()
}

/// Writes data to a file using standard I/O (io_uring not available).
pub fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
mod socket_stub {
    use std::io::{self, BufReader, Read, Write};
    use std::os::unix::io::RawFd;

    /// Stub io_uring socket reader (not available on this platform).
    pub struct IoUringSocketReader {
        _private: (),
    }

    impl IoUringSocketReader {
        /// Always returns an `Unsupported` error on this platform.
        pub fn from_raw_fd(_fd: RawFd, _config: &super::IoUringConfig) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    impl Read for IoUringSocketReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    /// Stub io_uring socket writer (not available on this platform).
    pub struct IoUringSocketWriter {
        _private: (),
    }

    impl IoUringSocketWriter {
        /// Always returns an `Unsupported` error on this platform.
        pub fn from_raw_fd(_fd: RawFd, _config: &super::IoUringConfig) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    impl Write for IoUringSocketWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this platform",
            ))
        }
    }

    /// Socket reader that falls back to `BufReader` (io_uring unavailable).
    pub enum IoUringOrStdSocketReader {
        /// io_uring variant (never constructed on this platform).
        IoUring(IoUringSocketReader),
        /// Standard buffered reader.
        Std(BufReader<Box<dyn Read + Send>>),
    }

    impl Read for IoUringOrStdSocketReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self {
                Self::IoUring(r) => r.read(buf),
                Self::Std(r) => r.read(buf),
            }
        }
    }

    /// Socket writer that falls back to standard `Write` (io_uring unavailable).
    pub enum IoUringOrStdSocketWriter {
        /// io_uring variant (never constructed on this platform).
        IoUring(IoUringSocketWriter),
        /// Standard writer.
        Std(Box<dyn Write + Send>),
    }

    impl Write for IoUringOrStdSocketWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self {
                Self::IoUring(w) => w.write(buf),
                Self::Std(w) => w.write(buf),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match self {
                Self::IoUring(w) => w.flush(),
                Self::Std(w) => w.flush(),
            }
        }
    }

    /// Thin Read adapter over a raw fd (does not take ownership).
    struct FdReader(RawFd);

    impl Read for FdReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let ret =
                unsafe { libc::read(self.0, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if ret < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(ret as usize)
            }
        }
    }

    // SAFETY: The fd is just an integer; the caller guarantees validity.
    unsafe impl Send for FdReader {}

    /// Thin Write adapter over a raw fd (does not take ownership).
    struct FdWriter(RawFd);

    impl Write for FdWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let ret =
                unsafe { libc::write(self.0, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
            if ret < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(ret as usize)
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // SAFETY: The fd is just an integer; the caller guarantees validity.
    unsafe impl Send for FdWriter {}

    /// Creates a socket reader, always using standard buffered I/O.
    ///
    /// On non-Linux platforms, `Enabled` returns an error. `Auto` and `Disabled`
    /// both return a `BufReader` wrapping the fd.
    pub fn socket_reader_from_fd(
        fd: RawFd,
        buffer_capacity: usize,
        policy: crate::IoUringPolicy,
    ) -> io::Result<IoUringOrStdSocketReader> {
        if matches!(policy, crate::IoUringPolicy::Enabled) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring requested via --io-uring but not available on this platform",
            ));
        }
        let reader = FdReader(fd);
        Ok(IoUringOrStdSocketReader::Std(BufReader::with_capacity(
            buffer_capacity,
            Box::new(reader),
        )))
    }

    /// Creates a socket writer, always using standard I/O.
    ///
    /// On non-Linux platforms, `Enabled` returns an error. `Auto` and `Disabled`
    /// both return a standard writer wrapping the fd.
    pub fn socket_writer_from_fd(
        fd: RawFd,
        buffer_capacity: usize,
        policy: crate::IoUringPolicy,
    ) -> io::Result<IoUringOrStdSocketWriter> {
        let _ = buffer_capacity;
        if matches!(policy, crate::IoUringPolicy::Enabled) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring requested via --io-uring but not available on this platform",
            ));
        }
        let writer = FdWriter(fd);
        Ok(IoUringOrStdSocketWriter::Std(Box::new(writer)))
    }
}

#[cfg(unix)]
pub use socket_stub::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IoUringPolicy;
    use crate::traits::{FileReader, FileWriter};
    use std::io::{Read, Write};
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn io_uring_unavailable_on_stub_platform() {
        assert!(!is_io_uring_available());
    }

    #[test]
    fn buffer_ring_is_not_supported_on_stub() {
        assert!(!buffer_ring::is_supported());
    }

    #[test]
    fn buffer_ring_try_new_returns_none_on_stub() {
        let config = BufferRingConfig::default();
        assert!(BufferRing::try_new(&(), config).is_none());
    }

    #[test]
    fn buffer_ring_new_returns_error_on_stub() {
        let config = BufferRingConfig::default();
        let err: io::Error = BufferRing::new(&(), config).unwrap_err().into();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn buffer_ring_new_with_allocator_returns_error_on_stub() {
        let config = BufferRingConfig::default();
        let err: io::Error = BufferRing::new_with_allocator(&(), config)
            .unwrap_err()
            .into();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn bgid_allocator_reports_exhausted_on_stub() {
        let err: io::Error = BgidAllocator::allocate().unwrap_err().into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(BgidAllocator::remaining(), 0);
        BgidAllocator::deallocate(0);
        BgidAllocator::deallocate(u16::MAX);
        assert_eq!(BgidAllocator::remaining(), 0);
    }

    #[test]
    fn buffer_id_from_cqe_flags_extracts_id_when_flag_set() {
        // Common helper returns `Some(buf_id)` when IORING_CQE_F_BUFFER is set.
        let flags = (1234u32 << 16) | 1;
        assert_eq!(buffer_id_from_cqe_flags(flags), Some(1234));
    }

    #[test]
    fn buffer_id_from_cqe_flags_returns_none_when_flag_clear() {
        let no_flag = 1234u32 << 16;
        assert_eq!(buffer_id_from_cqe_flags(no_flag), None);
    }

    #[test]
    fn buffer_ring_config_default_has_valid_values() {
        let config = BufferRingConfig::default();
        assert!(config.ring_size > 0);
        assert!(config.buffer_size > 0);
        assert_eq!(config.bgid, 0);
    }

    #[test]
    fn registered_buffer_group_try_new_returns_none() {
        let result = RegisteredBufferGroup::try_new(&(), 4096, 4);
        assert!(result.is_none());
    }

    #[test]
    fn registered_buffer_group_new_returns_unsupported() {
        let result = RegisteredBufferGroup::new(&(), 4096, 4);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn disk_batch_try_new_returns_none() {
        let config = IoUringConfig::default();
        assert!(IoUringDiskBatch::try_new(&config).is_none());
    }

    #[test]
    fn disk_batch_new_returns_unsupported() {
        let config = IoUringConfig::default();
        let result = IoUringDiskBatch::new(&config);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn config_has_register_buffers_fields() {
        let config = IoUringConfig::default();
        assert!(config.register_buffers);
        assert_eq!(config.registered_buffer_count, 8);

        let large = IoUringConfig::for_large_files();
        assert!(large.register_buffers);
        assert_eq!(large.registered_buffer_count, 16);

        let small = IoUringConfig::for_small_files();
        assert!(small.register_buffers);
        assert_eq!(small.registered_buffer_count, 8);
    }

    #[test]
    fn policy_disabled_writer_uses_std() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_disabled_reader_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("disabled_reader.txt");
        std::fs::write(&path, b"hello").unwrap();

        let reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_writer() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"").unwrap();
        let file = tmp.reopen().unwrap();

        let writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_auto_falls_back_to_std_reader() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auto_reader.txt");
        std::fs::write(&path, b"world").unwrap();

        let reader = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn policy_enabled_writer_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let file = tmp.reopen().unwrap();

        let result = writer_from_file(file, 8192, IoUringPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("io_uring"));
    }

    #[test]
    fn policy_enabled_reader_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("enabled_reader.txt");
        std::fs::write(&path, b"data").unwrap();

        let result = reader_from_path(&path, IoUringPolicy::Enabled);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("io_uring"));
    }

    #[test]
    fn writer_parity_disabled_vs_auto() {
        let test_data: Vec<u8> = (0..4096).map(|i| ((i * 7 + 13) % 256) as u8).collect();

        let dir = tempdir().unwrap();
        let path_disabled = dir.path().join("parity_disabled.bin");
        {
            let file = std::fs::File::create(&path_disabled).unwrap();
            let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let path_auto = dir.path().join("parity_auto.bin");
        {
            let file = std::fs::File::create(&path_auto).unwrap();
            let mut writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let content_disabled = std::fs::read(&path_disabled).unwrap();
        let content_auto = std::fs::read(&path_auto).unwrap();

        assert_eq!(content_disabled.len(), test_data.len());
        assert_eq!(content_disabled, content_auto);
        assert_eq!(content_disabled, test_data);
    }

    #[test]
    fn reader_parity_disabled_vs_auto() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("parity_read.bin");
        let test_data: Vec<u8> = (0..8192).map(|i| ((i * 11 + 3) % 256) as u8).collect();
        std::fs::write(&path, &test_data).unwrap();

        let mut reader_disabled = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        let data_disabled = reader_disabled.read_all().unwrap();

        let mut reader_auto = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
        let data_auto = reader_auto.read_all().unwrap();

        assert_eq!(data_disabled.len(), test_data.len());
        assert_eq!(data_disabled, data_auto);
        assert_eq!(data_disabled, test_data);
    }

    #[test]
    fn writer_bytes_written_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bytes_tracking.bin");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();

        assert_eq!(writer.bytes_written(), 0);
        writer.write_all(b"hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);
        writer.write_all(b" world").unwrap();
        assert_eq!(writer.bytes_written(), 11);
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 11);
    }

    #[test]
    fn reader_size_and_position_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("position_tracking.bin");
        let data = b"abcdefghijklmnop";
        std::fs::write(&path, data).unwrap();

        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        assert_eq!(reader.size(), 16);
        assert_eq!(reader.position(), 0);

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.position(), 4);
        assert_eq!(reader.remaining(), 12);
    }

    #[test]
    fn write_then_read_roundtrip_via_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = writer_from_file(file, 16384, IoUringPolicy::Disabled).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
        let read_back = reader.read_all().unwrap();

        assert_eq!(read_back.len(), test_data.len());
        assert_eq!(read_back, test_data);
    }

    #[test]
    fn factory_reader_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback.txt");
        std::fs::write(&path, b"factory test").unwrap();

        let factory = IoUringReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }

    #[test]
    fn factory_writer_forced_fallback_produces_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_fallback_write.txt");

        let factory = IoUringWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_io_uring());

        let writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
    }

    #[test]
    fn policy_default_is_auto() {
        assert_eq!(IoUringPolicy::default(), IoUringPolicy::Auto);
    }

    #[cfg(unix)]
    #[test]
    fn socket_reader_disabled_policy_uses_std() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let reader = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(reader, IoUringOrStdSocketReader::Std(_)));

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }

    #[cfg(unix)]
    #[test]
    fn socket_writer_disabled_policy_uses_std() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let writer = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Disabled).unwrap();
        assert!(matches!(writer, IoUringOrStdSocketWriter::Std(_)));

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }

    #[cfg(unix)]
    #[test]
    fn socket_enabled_policy_returns_error() {
        let (fd_a, fd_b) = {
            let mut fds = [0i32; 2];
            let ret =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(ret, 0);
            (fds[0], fds[1])
        };

        let reader_result = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Enabled);
        match reader_result {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported error for reader"),
        }

        let writer_result = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Enabled);
        match writer_result {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            Ok(_) => panic!("expected Unsupported error for writer"),
        }

        unsafe {
            libc::close(fd_a);
            libc::close(fd_b);
        }
    }

    #[test]
    fn stub_backend_reports_unavailable() {
        assert!(!StubIoUringBackend::is_available());
        assert!(!StubIoUringBackend::sqpoll_fell_back());
        assert!(StubIoUringBackend::availability_reason().contains("disabled"));
    }
}
