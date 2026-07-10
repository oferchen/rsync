//! Cross-platform io_uring data types, constants, and backend trait.
//!
//! This module compiles on every target and hosts the plain-data definitions
//! shared by both the Linux io_uring backend (`crate::io_uring`) and the
//! portable fallback (`io_uring_stub`). Centralising these
//! definitions removes the mechanical duplication that previously forced the
//! stub to redeclare every config struct, every UAPI constant, and every
//! enum variant just so cross-platform callers compiled without `cfg`-gating.
//!
//! What lives here:
//!
//! - Plain-data configuration structs ([`IoUringConfig`], [`SharedRingConfig`],
//!   [`BufferRingConfig`]) and their `Default` / factory helpers.
//! - Reporting types ([`IoUringKernelInfo`], [`RegisteredBufferStats`],
//!   [`RegisteredBufferStatus`]).
//! - Wire-format helpers ([`OpTag`], [`SharedCompletion`],
//!   [`buffer_id_from_cqe_flags`]).
//! - Kernel UAPI opcode and flag constants (`IORING_OP_*`, `RENAME_*`,
//!   `*_MIN_KERNEL`).
//! - The [`IoBackend`] trait that both the real Linux backend and the no-op
//!   stub implement.
//!
//! What does **not** live here: anything that mentions the `io_uring` crate
//! or otherwise depends on Linux-only FFI. Those types stay in
//! `crate::io_uring` (real) and `crate::io_uring_stub` (no-op).

use std::io;

/// Numeric value of `IORING_OP_LINKAT`.
///
/// Kernel UAPI constant from `include/uapi/linux/io_uring.h`, stable since
/// Linux 5.15. Exposed on every platform so cross-platform callers can
/// reference the opcode without `cfg`-gating.
pub const IORING_OP_LINKAT: u8 = 39;

/// Minimum Linux kernel version that ships `IORING_OP_LINKAT`.
pub const LINKAT_MIN_KERNEL: (u32, u32) = (5, 15);

/// Numeric value of `IORING_OP_RENAMEAT`.
///
/// Kernel UAPI constant from `include/uapi/linux/io_uring.h`, stable since
/// Linux 5.11.
pub const IORING_OP_RENAMEAT: u8 = 35;

/// `RENAME_NOREPLACE` flag (kernel UAPI constant).
pub const RENAME_NOREPLACE: u32 = 1;

/// `RENAME_EXCHANGE` flag (kernel UAPI constant).
pub const RENAME_EXCHANGE: u32 = 2;

/// `RENAME_WHITEOUT` flag (kernel UAPI constant).
pub const RENAME_WHITEOUT: u32 = 4;

/// Numeric value of `IORING_OP_STATX`.
///
/// Kernel UAPI constant from `include/uapi/linux/io_uring.h`, stable since
/// Linux 5.11.
pub const IORING_OP_STATX: u8 = 21;

/// Minimum Linux kernel version that ships `IORING_OP_STATX`.
pub const STATX_MIN_KERNEL: (u32, u32) = (5, 11);

/// Numeric value of `IORING_OP_ASYNC_CANCEL`.
///
/// Kernel UAPI constant from `include/uapi/linux/io_uring.h`, stable since
/// Linux 5.5. The classic opcode cancels a single in-flight SQE matched by
/// `user_data`. The extended match modes (cancel-by-fd, cancel-all) require
/// Linux 5.19 and are surfaced via the `AsyncCancel2` builder.
pub const IORING_OP_ASYNC_CANCEL: u8 = 14;

/// Minimum Linux kernel version that ships `IORING_OP_ASYNC_CANCEL`
/// (cancel-by-`user_data`).
pub const ASYNC_CANCEL_MIN_KERNEL: (u32, u32) = (5, 5);

/// Minimum Linux kernel version that ships the extended cancel match modes
/// (`IORING_ASYNC_CANCEL_FD`, `IORING_ASYNC_CANCEL_ALL`).
pub const ASYNC_CANCEL_FD_MIN_KERNEL: (u32, u32) = (5, 19);

/// CQE flag set by the kernel when a provided-buffer ID is encoded in the
/// CQE flags word.
pub(crate) const IORING_CQE_F_BUFFER: u32 = 1 << 0;

/// Bit position of the buffer ID inside the CQE flags word.
pub(crate) const IORING_CQE_BUFFER_SHIFT: u32 = 16;

/// Plain-data configuration for an io_uring instance.
///
/// The struct is identical on every platform; the Linux backend adds a
/// `build_ring` constructor in `crate::io_uring::config`, while the stub
/// simply exposes the fields for inspection.
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Number of submission queue entries (should be a power of 2).
    pub sq_entries: u32,
    /// Size of read/write buffers in bytes.
    pub buffer_size: usize,
    /// Whether to use direct I/O (`O_DIRECT`) on the file fd.
    pub direct_io: bool,
    /// Whether to register the file descriptor with the ring
    /// (`IORING_REGISTER_FILES`).
    pub register_files: bool,
    /// Whether to enable kernel-side SQ polling (`IORING_SETUP_SQPOLL`).
    pub sqpoll: bool,
    /// Idle timeout (milliseconds) for the SQPOLL kernel thread.
    pub sqpoll_idle_ms: u32,
    /// Signals that an `mmap`-backed reader is live on this transfer plan
    /// and may share buffers with this ring.
    ///
    /// Set by the caller before constructing the ring when the upstream
    /// selector cannot guarantee `BufferedMap` for the basis file. On Linux
    /// this only disables SQPOLL when the default `sqpoll-mlock-basis` feature
    /// is compiled out; with the feature on (the default) the basis window is
    /// mlock-pinned so SQPOLL and an mmap'd basis coexist. Note SQPOLL is off
    /// by default (`sqpoll` is `false`), so this flag has no effect in a stock
    /// build. See `IoUringConfig::build_ring` for the full rule. The stub
    /// stores the flag for parity but never acts on it.
    pub mmap_basis_active: bool,
    /// Whether to register fixed buffers
    /// (`IORING_REGISTER_BUFFERS` + `READ_FIXED` / `WRITE_FIXED`).
    pub register_buffers: bool,
    /// Number of fixed buffers to register when `register_buffers` is true.
    pub registered_buffer_count: usize,
    /// Zero-copy policy for socket sends (`IORING_OP_SEND_ZC`).
    pub zero_copy_policy: crate::ZeroCopyPolicy,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024,
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
            mmap_basis_active: false,
            register_buffers: true,
            registered_buffer_count: 8,
            zero_copy_policy: crate::ZeroCopyPolicy::Auto,
        }
    }
}

impl IoUringConfig {
    /// Creates a config optimised for large file transfers.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            sq_entries: 256,
            buffer_size: 256 * 1024,
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
            mmap_basis_active: false,
            register_buffers: true,
            registered_buffer_count: 16,
            zero_copy_policy: crate::ZeroCopyPolicy::Auto,
        }
    }

    /// Creates a config optimised for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            sq_entries: 128,
            buffer_size: 16 * 1024,
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
            mmap_basis_active: false,
            register_buffers: true,
            registered_buffer_count: 8,
            zero_copy_policy: crate::ZeroCopyPolicy::Auto,
        }
    }

    /// Returns whether socket writers may attempt `IORING_OP_SEND_ZC`.
    ///
    /// True only when the configured policy is
    /// [`ZeroCopyPolicy::Enabled`](crate::ZeroCopyPolicy::Enabled). `Auto`
    /// and `Disabled` both yield false so the default path uses regular
    /// `IORING_OP_SEND`.
    #[must_use]
    pub fn allow_send_zc(&self) -> bool {
        matches!(self.zero_copy_policy, crate::ZeroCopyPolicy::Enabled)
    }
}

/// Backing configuration for a [`SharedRing`](crate::io_uring::SharedRing).
///
/// Defaults match [`IoUringConfig::default`] for parity with the per-channel
/// rings; callers tuning a session should override via the field below.
#[derive(Debug, Clone, Default)]
pub struct SharedRingConfig {
    /// Backing io_uring configuration (SQ depth, registered buffer count, ...).
    pub ring: IoUringConfig,
}

/// Structured kernel information for io_uring availability reporting.
///
/// Returned by [`crate::io_uring_kernel_info`]
/// on Linux and by the equivalent stub on every other platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoUringKernelInfo {
    /// Whether io_uring is usable on this system.
    pub available: bool,
    /// Detected kernel major version, if parseable.
    pub kernel_major: Option<u32>,
    /// Detected kernel minor version, if parseable.
    pub kernel_minor: Option<u32>,
    /// Number of supported io_uring opcodes (0 if unavailable or probe failed).
    pub supported_ops: u32,
    /// Whether the kernel supports `IORING_REGISTER_PBUF_RING`.
    pub pbuf_ring_supported: bool,
    /// Human-readable reason for the reported availability.
    pub reason: String,
}

/// SQE `user_data` tag identifying the source channel of a completion.
///
/// Stored in the high 8 bits of `user_data`. Encoding/decoding is pure
/// arithmetic and works the same on every platform.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpTag {
    /// File-side read completion tag.
    Read = 1,
    /// File-side write completion tag.
    Write = 2,
    /// Socket-side send completion tag.
    Send = 3,
    /// Write-readiness probe completion tag.
    PollWrite = 4,
    /// `IORING_OP_ASYNC_CANCEL` completion tag. Used by the cancel
    /// primitive in `crate::io_uring::cancel` so cancel CQEs are
    /// distinguishable from target-op CQEs in the same drain pass.
    Cancel = 5,
}

impl OpTag {
    /// Encodes the tag and a 56-bit op id into a SQE-style `user_data`.
    #[inline]
    #[must_use]
    pub fn encode(self, op_id: u64) -> u64 {
        debug_assert!(op_id < (1 << 56), "op_id {op_id} overflows 56 bits");
        ((self as u64) << 56) | (op_id & ((1u64 << 56) - 1))
    }

    /// Decodes a `user_data` field into the source tag and op id.
    ///
    /// Returns `None` when the high 8 bits do not match a known tag value.
    #[inline]
    pub fn decode(user_data: u64) -> Option<(Self, u64)> {
        let tag = (user_data >> 56) as u8;
        let op_id = user_data & ((1u64 << 56) - 1);
        let parsed = match tag {
            1 => Self::Read,
            2 => Self::Write,
            3 => Self::Send,
            4 => Self::PollWrite,
            5 => Self::Cancel,
            _ => return None,
        };
        Some((parsed, op_id))
    }
}

/// Demultiplexed CQE result drained from a shared ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedCompletion {
    /// File read completed.
    Read {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// File write completed.
    Write {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// Socket send completed.
    Send {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// Write-readiness signalled by the kernel for the registered writer fd.
    PollWrite {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// `revents` bitmask returned by the kernel.
        revents: i16,
    },
}

/// Extracts the buffer ID from CQE flags.
///
/// Returns `Some(buf_id)` when `IORING_CQE_F_BUFFER` is set; otherwise
/// `None`. The function is pure arithmetic and yields the same result on
/// every platform.
#[inline]
#[must_use]
pub fn buffer_id_from_cqe_flags(flags: u32) -> Option<u16> {
    if flags & IORING_CQE_F_BUFFER != 0 {
        Some((flags >> IORING_CQE_BUFFER_SHIFT) as u16)
    } else {
        None
    }
}

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

    /// `buf_id` argument is outside the configured ring range.
    #[error("buf_id {buf_id} out of range for ring size {ring_size}")]
    BufferIdOutOfRange {
        /// Offending buffer ID supplied by the caller.
        buf_id: u16,
        /// Configured ring size at the time of the call.
        ring_size: u32,
    },

    /// The buffer group ID namespace (u16, 65 536 values) is exhausted.
    ///
    /// Produced when [`BgidAllocError::Exhausted`] propagates through
    /// [`From<BgidAllocError> for BufferRingError`]. Retained as a
    /// `BufferRingError` variant so the existing
    /// `BufferRing::new_with_allocator` signature stays compatible.
    #[error("io_uring buffer group ID namespace exhausted (limit: 65535)")]
    BgidExhausted,

    /// PBUF_RING (or the surrounding io_uring subsystem) is unavailable
    /// on this platform. Only produced by the non-Linux stub.
    #[error("PBUF_RING is not available on this platform")]
    Unsupported,
}

impl From<BufferRingError> for io::Error {
    fn from(e: BufferRingError) -> Self {
        match &e {
            BufferRingError::KernelTooOld { .. }
            | BufferRingError::KernelVersionUnknown
            | BufferRingError::Unsupported => io::Error::new(io::ErrorKind::Unsupported, e),
            BufferRingError::InvalidRingSize(_)
            | BufferRingError::InvalidBufferSize
            | BufferRingError::BufferIdOutOfRange { .. } => {
                io::Error::new(io::ErrorKind::InvalidInput, e)
            }
            BufferRingError::BgidExhausted => io::Error::new(io::ErrorKind::OutOfMemory, e),
            BufferRingError::MmapFailed(_)
            | BufferRingError::RegisterFailed(_)
            | BufferRingError::AllocationFailed(_) => io::Error::other(e),
        }
    }
}

/// Typed error returned by [`BgidAllocator::allocate`] when no buffer
/// group ID is available.
///
/// The io_uring buffer-group ID namespace is fixed at 65 536 values (the
/// kernel stores `bgid` as `u16` inside `struct io_uring_buf_reg`). A
/// long-running process that continuously registers new provided-buffer
/// rings without recycling the bgids it issued will eventually hit this
/// ceiling. Reporting the failure as a typed value (rather than panicking
/// or returning a bare [`io::Error`]) lets callers downgrade gracefully:
/// log a warning, skip the buffer-ring registration, and continue serving
/// with plain `recv`/`read` on that connection.
///
/// The variant carries the live counters at the time of the failure so
/// operators can correlate the warning with the upstream pressure source.
///
/// [`BgidAllocator::allocate`]: crate::io_uring::buffer_ring::BgidAllocator::allocate
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BgidAllocError {
    /// All 65 536 BGIDs have been issued and none are available for reuse.
    ///
    /// - `fresh_used` is the monotonic counter value at the time of the
    ///   failure (capped at `u16::MAX + 1`).
    /// - `free_list_len` is the length of the deallocator free-list at the
    ///   time of the failure (always zero when `fresh_used` is at the cap,
    ///   reported for symmetry with the success path).
    #[error(
        "io_uring buffer group ID namespace exhausted \
         (fresh_used={fresh_used}, free_list_len={free_list_len}, limit=65536)"
    )]
    Exhausted {
        /// Number of bgids issued by the monotonic counter so far.
        fresh_used: u32,
        /// Number of bgids sitting on the free-list at failure time.
        free_list_len: usize,
    },
}

impl From<BgidAllocError> for io::Error {
    /// Maps [`BgidAllocError::Exhausted`] to
    /// [`io::ErrorKind::OutOfMemory`].
    ///
    /// `OutOfMemory` is the closest standard kind for "a finite resource
    /// namespace has been used up". `Other` would be lossy and `InvalidInput`
    /// would mislead callers into blaming their arguments.
    fn from(e: BgidAllocError) -> Self {
        io::Error::new(io::ErrorKind::OutOfMemory, e)
    }
}

impl From<BgidAllocError> for BufferRingError {
    /// Allows allocator failures to flow through call sites that return
    /// [`BufferRingError`] via the `?` operator.
    fn from(_: BgidAllocError) -> Self {
        BufferRingError::BgidExhausted
    }
}

/// Configuration for a provided buffer ring.
#[derive(Debug, Clone)]
pub struct BufferRingConfig {
    /// Number of entries in the ring (must be a power of 2).
    pub ring_size: u32,
    /// Size of each individual buffer in bytes.
    pub buffer_size: u32,
    /// Buffer group ID for this ring.
    pub bgid: u16,
}

impl Default for BufferRingConfig {
    fn default() -> Self {
        Self {
            ring_size: 64,
            buffer_size: 64 * 1024,
            bgid: 0,
        }
    }
}

/// Snapshot of registered-buffer telemetry counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredBufferStats {
    /// Total number of `checkout` calls (whether they succeeded or not).
    pub total_acquires: u64,
    /// Number of `checkout` calls that returned `None`.
    pub total_misses: u64,
}

impl RegisteredBufferStats {
    /// Returns the miss rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` when no acquires have been recorded yet.
    #[must_use]
    pub fn miss_rate(&self) -> f64 {
        if self.total_acquires == 0 {
            return 0.0;
        }
        self.total_misses as f64 / self.total_acquires as f64
    }
}

/// Owner-side state of fixed-buffer registration on an io_uring instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredBufferStatus {
    /// Registration succeeded; `READ_FIXED` / `WRITE_FIXED` are in use.
    Enabled,
    /// The caller disabled registration via
    /// [`IoUringConfig::register_buffers`] = false.
    Disabled,
    /// Registration was attempted and rejected. `reason` carries the
    /// kernel's `errno` string for telemetry.
    RegistrationFailed {
        /// Reason returned by the registration attempt.
        reason: String,
    },
}

impl RegisteredBufferStatus {
    /// True when `READ_FIXED` / `WRITE_FIXED` opcodes are active.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// True when the caller explicitly disabled registration.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// True when registration was attempted but rejected.
    #[must_use]
    pub fn is_registration_failed(&self) -> bool {
        matches!(self, Self::RegistrationFailed { .. })
    }
}

/// Cross-platform contract for an io_uring-style I/O backend.
///
/// Implemented by the real Linux backend (`io_uring`) and the no-op
/// stub (`io_uring_stub`). The trait captures the small set of
/// runtime queries that callers need without binding them to a specific
/// platform's concrete reader/writer types - those types still live in their
/// respective modules and are surfaced through the existing crate-level
/// re-exports for backwards compatibility.
///
/// The trait has no associated types so it can be used in generic contexts
/// without monomorphisation explosions. New methods should be plain queries
/// with default implementations to keep both backends in lockstep without
/// forcing a coordinated edit.
pub trait IoBackend {
    /// Returns whether the backend can perform real io_uring submissions on
    /// this host.
    fn is_available() -> bool;

    /// Returns a human-readable reason for the current availability state,
    /// suitable for diagnostic logging or `--version` output.
    fn availability_reason() -> String;

    /// Returns whether SQPOLL was requested but fell back to regular
    /// submission. Always `false` on the stub backend.
    fn sqpoll_fell_back() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_tag_roundtrip() {
        let encoded = OpTag::Send.encode(42);
        let (tag, id) = OpTag::decode(encoded).unwrap();
        assert_eq!(tag, OpTag::Send);
        assert_eq!(id, 42);
    }

    #[test]
    fn op_tag_decode_rejects_unknown_tag() {
        let bad = (200u64) << 56;
        assert!(OpTag::decode(bad).is_none());
    }

    #[test]
    fn buffer_id_extraction_matches_cqe_layout() {
        let flags = (1234u32 << IORING_CQE_BUFFER_SHIFT) | IORING_CQE_F_BUFFER;
        assert_eq!(buffer_id_from_cqe_flags(flags), Some(1234));
        let no_flag = 1234u32 << IORING_CQE_BUFFER_SHIFT;
        assert_eq!(buffer_id_from_cqe_flags(no_flag), None);
    }

    #[test]
    fn registered_buffer_stats_miss_rate_handles_zero() {
        let stats = RegisteredBufferStats {
            total_acquires: 0,
            total_misses: 0,
        };
        assert_eq!(stats.miss_rate(), 0.0);

        let stats = RegisteredBufferStats {
            total_acquires: 4,
            total_misses: 1,
        };
        assert!((stats.miss_rate() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn config_factories_match_documented_defaults() {
        let large = IoUringConfig::for_large_files();
        assert_eq!(large.sq_entries, 256);
        assert_eq!(large.registered_buffer_count, 16);

        let small = IoUringConfig::for_small_files();
        assert_eq!(small.sq_entries, 128);
        assert_eq!(small.registered_buffer_count, 8);
    }

    #[test]
    fn rename_flag_constants_match_kernel_uapi() {
        assert_eq!(RENAME_NOREPLACE, 1);
        assert_eq!(RENAME_EXCHANGE, 2);
        assert_eq!(RENAME_WHITEOUT, 4);
        assert_eq!(IORING_OP_RENAMEAT, 35);
        assert_eq!(IORING_OP_LINKAT, 39);
        assert_eq!(IORING_OP_STATX, 21);
        assert_eq!(IORING_OP_ASYNC_CANCEL, 14);
        assert_eq!(LINKAT_MIN_KERNEL, (5, 15));
        assert_eq!(STATX_MIN_KERNEL, (5, 11));
        assert_eq!(ASYNC_CANCEL_MIN_KERNEL, (5, 5));
        assert_eq!(ASYNC_CANCEL_FD_MIN_KERNEL, (5, 19));
    }
}
