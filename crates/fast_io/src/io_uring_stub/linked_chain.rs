//! Stub linked-chain primitive mirroring [`crate::io_uring::linked_chain`].
//!
//! The chain cannot be constructed because the stub `super::session_pool::RingLease`
//! cannot exist on this platform.

use super::session_pool::RingLease;
use std::io;
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(not(unix))]
type RawFd = std::os::raw::c_int;

/// Stub completion-queue entry result.
///
/// Field layout matches the Linux struct so cross-platform callers can
/// destructure or pattern-match the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CqeResult {
    /// Chain position of the originating SQE.
    pub index: u32,
    /// Raw kernel completion result.
    pub result: i32,
}

impl CqeResult {
    /// Maps the stub result to an [`io::Result`].
    ///
    /// Always succeeds with `0` because the stub cannot run a chain;
    /// callers that need real error mapping must be on Linux with the
    /// `io_uring` feature.
    pub fn into_io_result(self) -> io::Result<u32> {
        if self.result < 0 {
            Err(io::Error::from_raw_os_error(-self.result))
        } else {
            Ok(self.result as u32)
        }
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn is_chain_cancellation(self) -> bool {
        false
    }
}

/// Stub linked chain. Cannot be constructed on this platform because the
/// stub [`RingLease`] cannot exist.
pub struct LinkedChain<'r> {
    _lease: RingLease<'r>,
}

impl<'r> LinkedChain<'r> {
    /// Builds a stub chain wrapping the (unreachable) lease.
    #[must_use]
    pub fn new(lease: RingLease<'r>) -> Self {
        Self { _lease: lease }
    }

    /// Stub `read` builder; never appends because the chain cannot be
    /// constructed.
    #[must_use]
    pub fn read(self, _fd: RawFd, _buf: &'r mut [u8], _offset: u64) -> Self {
        self
    }

    /// Stub `write` builder; never appends because the chain cannot be
    /// constructed.
    #[must_use]
    pub fn write(self, _fd: RawFd, _buf: &'r [u8], _offset: u64) -> Self {
        self
    }

    /// Stub chain length; always zero.
    #[must_use]
    pub fn len(&self) -> usize {
        0
    }

    /// Stub emptiness check; always `true`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        true
    }

    /// Always returns `Unsupported` on this platform.
    pub fn submit_and_wait(self) -> io::Result<Vec<CqeResult>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring linked chains are not available on this platform",
        ))
    }
}

/// Stub one-shot read-then-write helper; always returns `Unsupported`.
pub fn read_then_write(
    _lease: RingLease<'_>,
    _src_fd: RawFd,
    _src_offset: u64,
    _dst_fd: RawFd,
    _dst_offset: u64,
    _buf: &mut [u8],
) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "io_uring linked chains are not available on this platform",
    ))
}
