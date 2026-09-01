//! Error categorization for delta transfer operations
//!
//! This module provides error types and categorization helpers to distinguish
//! between fatal errors (abort transfer), recoverable errors (skip file), and
//! data corruption risks.
//!
//! It also provides retry utilities for transient errors like EINTR (interrupted
//! system calls), matching upstream rsync's behavior.

use std::io::{self, Read, Seek, Write};
use std::path::PathBuf;

use thiserror::Error;

/// Error categories for delta transfer operations.
///
/// Delta transfer can encounter various error conditions that require different
/// handling strategies. This enum categorizes errors into:
///
/// - **Fatal**: Abort the entire transfer to prevent data loss
/// - **Recoverable**: Skip the current file but continue with others
/// - **DataCorruption**: Critical risk requiring immediate abort
#[derive(Debug, Error)]
pub enum DeltaTransferError {
    /// Fatal error that should abort the entire transfer.
    #[error("Fatal: {0}")]
    Fatal(#[from] DeltaFatalError),

    /// Recoverable error - skip file and continue.
    #[error("Recoverable: {0}")]
    Recoverable(#[from] DeltaRecoverableError),

    /// Data corruption risk - abort immediately.
    #[error("Data corruption risk: {0}")]
    DataCorruption(String),
}

/// Fatal errors that require aborting the entire transfer.
#[derive(Debug, Error)]
pub enum DeltaFatalError {
    /// Disk full - abort to prevent partial writes and data corruption.
    #[error("Disk full at {}{}", path.display(), bytes_needed.map(|b| format!(" ({b} bytes needed)")).unwrap_or_default())]
    DiskFull {
        /// Path where disk full was detected.
        path: PathBuf,
        /// Number of bytes needed (if known).
        bytes_needed: Option<u64>,
    },

    /// Read-only filesystem - fatal because it affects all subsequent writes.
    #[error("Read-only filesystem at {}", path.display())]
    ReadOnlyFilesystem {
        /// Path where read-only filesystem was detected.
        path: PathBuf,
    },

    /// Wire protocol error indicating a fundamental communication failure.
    #[error("Protocol error: {message}")]
    ProtocolError {
        /// Description of the protocol error.
        message: String,
    },

    /// Other fatal I/O error that should abort the transfer.
    #[error("I/O error: {0}")]
    Io(#[source] io::Error),
}

/// Recoverable errors that allow skipping the current file.
#[derive(Debug, Error)]
pub enum DeltaRecoverableError {
    /// File disappeared between file-list generation and transfer.
    #[error("File not found: {}", path.display())]
    FileNotFound {
        /// Path to the missing file.
        path: PathBuf,
    },

    /// Permission denied on an individual file - skip and continue.
    #[error("Permission denied for {operation} on {}", path.display())]
    PermissionDenied {
        /// Path where permission was denied.
        path: PathBuf,
        /// Operation that was attempted (e.g., "open", "read", "write").
        operation: String,
    },

    /// Other I/O error affecting only the current file.
    #[error("I/O error on {}: {error}", path.display())]
    Io {
        /// Path where the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        error: io::Error,
    },
}

/// Categorize an io::Error into DeltaTransferError.
///
/// This helper examines the ErrorKind to determine whether the error is
/// fatal (abort transfer) or recoverable (skip file and continue).
///
/// # Examples
///
/// ```ignore
/// use std::io;
/// use std::path::Path;
/// use transfer::error::categorize_io_error;
///
/// let path = Path::new("/tmp/file.txt");
///
/// // Disk full is fatal
/// let err = io::Error::from(io::ErrorKind::StorageFull);
/// let categorized = categorize_io_error(err, path, "write");
/// // assert!(matches!(categorized, DeltaTransferError::Fatal(_)));
///
/// // Permission denied is recoverable
/// let err = io::Error::from(io::ErrorKind::PermissionDenied);
/// let categorized = categorize_io_error(err, path, "open");
/// // assert!(matches!(categorized, DeltaTransferError::Recoverable(_)));
/// ```
pub fn categorize_io_error(
    err: io::Error,
    path: &std::path::Path,
    operation: &str,
) -> DeltaTransferError {
    use io::ErrorKind::*;

    match err.kind() {
        WouldBlock | Interrupted => DeltaTransferError::Recoverable(DeltaRecoverableError::Io {
            path: path.to_path_buf(),
            error: err,
        }),

        NotFound => DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound {
            path: path.to_path_buf(),
        }),
        PermissionDenied => {
            DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
                path: path.to_path_buf(),
                operation: operation.to_owned(),
            })
        }

        StorageFull => DeltaTransferError::Fatal(DeltaFatalError::DiskFull {
            path: path.to_path_buf(),
            bytes_needed: None,
        }),

        ReadOnlyFilesystem => DeltaTransferError::Fatal(DeltaFatalError::ReadOnlyFilesystem {
            path: path.to_path_buf(),
        }),

        _ => DeltaTransferError::Fatal(DeltaFatalError::Io(err)),
    }
}

/// Reads exactly `buf.len()` bytes, retrying on EINTR (interrupted system call).
///
/// This matches upstream rsync's behavior in `util1.c:315-317` where reads are
/// retried immediately when interrupted by a signal.
///
/// # Upstream Reference
///
/// ```c
/// do {
///     n_chars = read(desc, ptr, len);
/// } while (n_chars < 0 && errno == EINTR);
/// ```
pub fn read_exact_retry<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<()> {
    let mut total_read = 0;
    while total_read < buf.len() {
        match reader.read(&mut buf[total_read..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            Ok(n) => total_read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes all bytes, retrying on `EINTR` (interrupted system call) and
/// `EAGAIN`/`EWOULDBLOCK` (transient back-pressure).
///
/// This matches upstream rsync's behavior in `fileio.c:60-65` (EINTR) and
/// `io.c::writefd_unbuffered` (EAGAIN via `select()`/`poll()`). When the
/// kernel reports a pipe buffer is temporarily full, yield the current thread
/// and retry instead of aborting the transfer.
///
/// # Upstream Reference
///
/// ```c
/// do {
///     ret = write(f, "", 1);
/// } while (ret < 0 && errno == EINTR);
/// ```
pub fn write_all_retry<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<()> {
    let mut total_written = 0;
    while total_written < buf.len() {
        match writer.write(&buf[total_written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ));
            }
            Ok(n) => total_written += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // EAGAIN/EWOULDBLOCK indicates a transient back-pressure
            // situation (kernel pipe buffer full). Yield and retry to match
            // upstream io.c::writefd_unbuffered's select/poll behaviour.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::yield_now();
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Seeks to a position, retrying on EINTR (interrupted system call).
pub fn seek_retry<S: Seek>(seeker: &mut S, pos: io::SeekFrom) -> io::Result<u64> {
    loop {
        match seeker.seek(pos) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Flushes a writer, retrying on EINTR (interrupted system call).
pub fn flush_retry<W: Write>(writer: &mut W) -> io::Result<()> {
    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Maps a fatal transfer failure onto the upstream `errcode.h` `RERR_*` value
/// that `exit_cleanup()` would have been called with at the failing site.
///
/// Upstream picks the code per call site (`exit_cleanup(RERR_FILEIO)` and
/// friends); oc carries one `io::Error` up the stack instead, so the class is
/// recovered here from the error's tag or `ErrorKind`. This lives in `transfer`,
/// below `core`, because both the client's `ExitCode::from_io_error` and the
/// server's `MSG_ERROR_EXIT` producer need the same answer, and the server half
/// runs inside this crate.
///
/// # Mapping Rules
///
/// - a tagged [`protocol::ProtocolViolation`] - `RERR_PROTOCOL` (2)
/// - a tagged [`protocol::SyntaxViolation`] - `RERR_SYNTAX` (1)
/// - a failed commit-path backup - `RERR_FILEIO` (11), whatever the errno
/// - `NotFound`, `PermissionDenied`, `AlreadyExists` - `RERR_FILESELECT` (3)
/// - the connection-class kinds - `RERR_SOCKETIO` (10)
/// - `TimedOut` - `RERR_TIMEOUT` (30)
/// - `UnexpectedEof`, other `InvalidData` - `RERR_STREAMIO` (12)
/// - `Interrupted` - `RERR_SIGNAL` (20)
/// - `Unsupported` - `RERR_UNSUPPORTED` (4)
/// - everything else - `RERR_FILEIO` (11)
///
/// # Upstream Reference
///
/// - `errcode.h` - the `RERR_*` values.
/// - `rsync.c:900` - `finish_transfer()` answers a failed `make_backup()` with
///   `exit_cleanup(RERR_FILEIO)` whatever the errno was.
#[must_use]
pub fn rerr_for_io_error(error: &io::Error) -> i32 {
    // upstream: errcode.h RERR_PROTOCOL=2 - a tagged protocol violation
    // outranks the generic InvalidData => RERR_STREAMIO mapping below.
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>())
    {
        return 2;
    }

    // upstream: errcode.h RERR_SYNTAX=1 - an option-usage refusal is tagged at
    // its call site, exactly as the protocol-violation class above; without the
    // tag it would fall through to the `_ => RERR_FILEIO` arm.
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::SyntaxViolation>())
    {
        return 1;
    }

    // upstream: rsync.c:900 - a denied backup must not be graded by `ErrorKind`
    // like the per-file open failures below. Without this arm the usual `EACCES`
    // reaches the `PermissionDenied => RERR_FILESELECT` arm and reports 3.
    if matches!(
        crate::temp_guard::commit_op_failure(error),
        Some((crate::temp_guard::CommitOp::Backup, _))
    ) {
        return 11;
    }

    match error.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::AlreadyExists => 3,

        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::AddrInUse
        | io::ErrorKind::AddrNotAvailable
        | io::ErrorKind::NotConnected => 10,

        io::ErrorKind::TimedOut => 30,

        io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData => 12,

        io::ErrorKind::Interrupted => 20,

        io::ErrorKind::Unsupported => 4,

        _ => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn categorize_disk_full_as_fatal() {
        let err = io::Error::from(io::ErrorKind::StorageFull);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "write");

        match categorized {
            DeltaTransferError::Fatal(DeltaFatalError::DiskFull { path: p, .. }) => {
                assert_eq!(p, path);
            }
            _ => panic!("Expected fatal disk full error"),
        }
    }

    #[test]
    fn categorize_permission_denied_as_recoverable() {
        let err = io::Error::from(io::ErrorKind::PermissionDenied);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "open");

        match categorized {
            DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
                path: p,
                operation: op,
            }) => {
                assert_eq!(p, path);
                assert_eq!(op, "open");
            }
            _ => panic!("Expected recoverable permission denied error"),
        }
    }

    #[test]
    fn categorize_not_found_as_recoverable() {
        let err = io::Error::from(io::ErrorKind::NotFound);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "open");

        match categorized {
            DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound { path: p }) => {
                assert_eq!(p, path);
            }
            _ => panic!("Expected recoverable file not found error"),
        }
    }

    #[test]
    fn categorize_readonly_filesystem_as_fatal() {
        let err = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "write");

        match categorized {
            DeltaTransferError::Fatal(DeltaFatalError::ReadOnlyFilesystem { path: p }) => {
                assert_eq!(p, path);
            }
            _ => panic!("Expected fatal read-only filesystem error"),
        }
    }

    #[test]
    fn categorize_interrupted_as_recoverable() {
        let err = io::Error::from(io::ErrorKind::Interrupted);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "read");

        assert!(matches!(
            categorized,
            DeltaTransferError::Recoverable(DeltaRecoverableError::Io { .. })
        ));
    }

    #[test]
    fn categorize_would_block_as_recoverable() {
        let err = io::Error::from(io::ErrorKind::WouldBlock);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "read");

        assert!(matches!(
            categorized,
            DeltaTransferError::Recoverable(DeltaRecoverableError::Io { .. })
        ));
    }

    #[test]
    fn categorize_unknown_error_as_fatal() {
        let err = io::Error::from(io::ErrorKind::Other);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "write");

        assert!(matches!(
            categorized,
            DeltaTransferError::Fatal(DeltaFatalError::Io(_))
        ));
    }

    #[test]
    fn display_disk_full_error() {
        let err = DeltaFatalError::DiskFull {
            path: PathBuf::from("/tmp/test.txt"),
            bytes_needed: Some(1024),
        };

        let s = format!("{err}");
        assert!(s.contains("Disk full"));
        assert!(s.contains("/tmp/test.txt"));
        assert!(s.contains("1024"));
    }

    #[test]
    fn display_permission_denied_error() {
        let err = DeltaRecoverableError::PermissionDenied {
            path: PathBuf::from("/tmp/test.txt"),
            operation: "open".to_owned(),
        };

        let s = format!("{err}");
        assert!(s.contains("Permission denied"));
        assert!(s.contains("open"));
        assert!(s.contains("/tmp/test.txt"));
    }

    #[test]
    fn read_exact_retry_succeeds_on_normal_read() {
        let data = b"hello world";
        let mut cursor = std::io::Cursor::new(data);
        let mut buf = [0u8; 11];

        read_exact_retry(&mut cursor, &mut buf).unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn read_exact_retry_handles_partial_reads() {
        struct ChunkyReader {
            data: &'static [u8],
            pos: usize,
            chunk_size: usize,
        }

        impl Read for ChunkyReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let remaining = self.data.len() - self.pos;
                let to_read = remaining.min(self.chunk_size).min(buf.len());
                buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
                self.pos += to_read;
                Ok(to_read)
            }
        }

        let mut reader = ChunkyReader {
            data: b"hello world",
            pos: 0,
            chunk_size: 3,
        };
        let mut buf = [0u8; 11];

        read_exact_retry(&mut reader, &mut buf).unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn read_exact_retry_returns_eof_on_short_read() {
        let data = b"short";
        let mut cursor = std::io::Cursor::new(data);
        let mut buf = [0u8; 10];

        let result = read_exact_retry(&mut cursor, &mut buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn write_all_retry_succeeds_on_normal_write() {
        let mut buf = Vec::new();
        write_all_retry(&mut buf, b"hello world").unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn write_all_retry_handles_wouldblock() {
        // Mirrors the upstream io.c::writefd_unbuffered behaviour: a single
        // EAGAIN must not abort the transfer. The writer reports WouldBlock
        // on the first call, then accepts all bytes on the second.
        struct FlakyWriter {
            inner: Vec<u8>,
            blocked: bool,
        }

        impl Write for FlakyWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if !self.blocked {
                    self.blocked = true;
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                self.inner.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = FlakyWriter {
            inner: Vec::new(),
            blocked: false,
        };
        write_all_retry(&mut writer, b"hello world").expect("retry survives WouldBlock");
        assert_eq!(&writer.inner, b"hello world");
    }

    #[test]
    fn seek_retry_succeeds() {
        let data = b"hello world";
        let mut cursor = std::io::Cursor::new(data);

        let pos = seek_retry(&mut cursor, io::SeekFrom::Start(6)).unwrap();
        assert_eq!(pos, 6);

        let mut buf = [0u8; 5];
        cursor.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn flush_retry_succeeds() {
        let mut buf = Vec::new();
        buf.write_all(b"test").unwrap();
        flush_retry(&mut buf).unwrap();
    }
}
