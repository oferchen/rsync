//! Typed IOCP error variants for actionable failure handling.
//!
//! Windows surfaces overlapped-I/O failures as opaque `io::Error` values built
//! from raw OS codes. Two of those codes carry strong semantic meaning that
//! callers benefit from branching on directly rather than re-parsing
//! `raw_os_error()`:
//!
//! - `ERROR_INVALID_PARAMETER` (87) returned by `WriteFile` /
//!   `GetQueuedCompletionStatusEx` typically means the caller submitted a
//!   malformed `OVERLAPPED` or, more commonly, the file handle was not opened
//!   with `FILE_FLAG_OVERLAPPED`. The latter is the failure mode that #1929
//!   addresses; mapping it to a typed [`IocpError::InvalidOperation`] gives
//!   callers a clear error message instead of a bare "Invalid parameter".
//! - `ERROR_INSUFFICIENT_BUFFER` (122) is returned by
//!   `GetQueuedCompletionStatusEx` when the caller-supplied entry array is
//!   smaller than the number of completions the kernel wants to deliver.
//!   We surface this as [`IocpError::InsufficientBuffer`] so the pump
//!   drain-loop can grow its batch buffer and retry instead of bubbling a
//!   confusing OS error to user code.
//!
//! All variants implement `From<IocpError> for io::Error` so the existing
//! `io::Result`-returning APIs remain backwards compatible.

use std::io;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER};

/// Typed error variants raised by the IOCP code paths.
///
/// These wrap the few Windows error codes that warrant explicit handling.
/// All other errors are returned as plain `io::Error` to avoid bloating the
/// error surface with codes the caller can do nothing about.
#[derive(Debug, thiserror::Error)]
pub enum IocpError {
    /// The kernel reported `ERROR_INVALID_PARAMETER` for an overlapped op.
    ///
    /// Most often this means the file handle was not opened with
    /// `FILE_FLAG_OVERLAPPED`. The recommended fix is to construct the
    /// IOCP reader/writer through the path-based factory functions, or to
    /// reopen the file via [`super::file_factory::writer_from_file`] under
    /// the [`crate::IocpPolicy::Enabled`] policy, which performs the
    /// `FILE_FLAG_OVERLAPPED` reopen automatically (#1929).
    #[error("IOCP overlapped operation rejected with ERROR_INVALID_PARAMETER: {context}")]
    InvalidOperation {
        /// Free-form context describing the call site (e.g. `"WriteFile"`).
        context: &'static str,
    },

    /// `GetQueuedCompletionStatusEx` reported `ERROR_INSUFFICIENT_BUFFER`.
    ///
    /// The kernel had more completion entries available than the supplied
    /// entry array could hold. The pump drain-loop handles this by growing
    /// its batch buffer and retrying; surface this variant only when
    /// callers invoke `GetQueuedCompletionStatusEx` directly without going
    /// through the pump.
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
            IocpError::InsufficientBuffer { .. } => {
                io::Error::new(io::ErrorKind::OutOfMemory, err)
            }
        }
    }
}

/// Returns `true` when the given OS error code is `ERROR_INVALID_PARAMETER`.
///
/// `ERROR_INVALID_PARAMETER` is the canonical Win32 indicator that an
/// overlapped op was submitted against a non-overlapped handle. This helper
/// keeps the magic number out of caller code.
#[must_use]
pub fn is_invalid_parameter(err: &io::Error) -> bool {
    err.raw_os_error()
        .is_some_and(|code| code as u32 == ERROR_INVALID_PARAMETER)
}

/// Returns `true` when the given OS error code is `ERROR_INSUFFICIENT_BUFFER`.
#[must_use]
pub fn is_insufficient_buffer(err: &io::Error) -> bool {
    err.raw_os_error()
        .is_some_and(|code| code as u32 == ERROR_INSUFFICIENT_BUFFER)
}

/// Wraps an `io::Error` returned by an overlapped Win32 call, upgrading
/// `ERROR_INVALID_PARAMETER` into a typed [`IocpError::InvalidOperation`].
///
/// Other errors pass through unchanged. This is the canonical adapter used by
/// `WriteFile` / `ReadFile` call sites in the IOCP reader/writer.
pub fn classify_overlapped_error(err: io::Error, context: &'static str) -> io::Error {
    if is_invalid_parameter(&err) {
        return IocpError::InvalidOperation { context }.into();
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_operation_into_io_error_uses_invalid_input_kind() {
        let err: io::Error = IocpError::InvalidOperation {
            context: "WriteFile",
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("ERROR_INVALID_PARAMETER"));
        assert!(err.to_string().contains("WriteFile"));
    }

    #[test]
    fn insufficient_buffer_into_io_error_uses_oom_kind() {
        let err: io::Error = IocpError::InsufficientBuffer {
            requested: 128,
            capacity: 64,
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        assert!(err.to_string().contains("128"));
        assert!(err.to_string().contains("64"));
    }

    #[test]
    fn is_invalid_parameter_recognises_code_87() {
        let err = io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32);
        assert!(is_invalid_parameter(&err));

        let other = io::Error::from_raw_os_error(5);
        assert!(!is_invalid_parameter(&other));
    }

    #[test]
    fn is_insufficient_buffer_recognises_code_122() {
        let err = io::Error::from_raw_os_error(ERROR_INSUFFICIENT_BUFFER as i32);
        assert!(is_insufficient_buffer(&err));

        let other = io::Error::from_raw_os_error(2);
        assert!(!is_insufficient_buffer(&other));
    }

    #[test]
    fn classify_upgrades_invalid_parameter() {
        let err = io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32);
        let mapped = classify_overlapped_error(err, "WriteFile");
        assert_eq!(mapped.kind(), io::ErrorKind::InvalidInput);
        assert!(mapped.to_string().contains("WriteFile"));
    }

    #[test]
    fn classify_passes_through_other_errors() {
        let err = io::Error::from_raw_os_error(5); // ERROR_ACCESS_DENIED
        let mapped = classify_overlapped_error(err, "ReadFile");
        assert_eq!(mapped.raw_os_error(), Some(5));
    }
}
