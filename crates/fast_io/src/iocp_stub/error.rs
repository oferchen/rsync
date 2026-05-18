//! Typed IOCP error variants (stub).
//!
//! Mirrors the public surface of [`crate::iocp::error`] so cross-platform
//! callers can name the same variants regardless of which backend is
//! compiled. The stub never constructs these values because the IOCP backend
//! is never wired in on non-Windows platforms; the type exists purely to
//! keep cross-platform code compiling.

use std::io;

/// Typed IOCP error variants.
///
/// On non-Windows platforms the IOCP backend is never constructed, so this
/// type exists purely to keep cross-platform callers compiling. Both variants
/// implement `From<IocpError> for io::Error` to match the Windows surface.
#[derive(Debug, thiserror::Error)]
pub enum IocpError {
    /// Mirrors the Windows `ERROR_INVALID_PARAMETER` mapping.
    #[error("IOCP overlapped operation rejected with ERROR_INVALID_PARAMETER: {context}")]
    InvalidOperation {
        /// Free-form context describing the call site.
        context: &'static str,
    },
    /// Mirrors the Windows `ERROR_INSUFFICIENT_BUFFER` mapping.
    #[error(
        "IOCP completion drain ran out of buffer space ({requested} entries requested, capacity {capacity})"
    )]
    InsufficientBuffer {
        /// Number of completion entries the kernel wanted to deliver.
        requested: u32,
        /// Number of entries the buffer could hold.
        capacity: u32,
    },
}

impl From<IocpError> for io::Error {
    fn from(err: IocpError) -> Self {
        match err {
            IocpError::InvalidOperation { .. } => io::Error::new(io::ErrorKind::InvalidInput, err),
            IocpError::InsufficientBuffer { .. } => io::Error::new(io::ErrorKind::OutOfMemory, err),
        }
    }
}
