//! Error categorization for delta transfer operations
//!
//! This module provides error types and categorization helpers to distinguish
//! between fatal errors (abort transfer), recoverable errors (skip file), and
//! data corruption risks.

use std::io;
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
    /// Disk full - abort to prevent data loss.
    ///
    /// When the filesystem runs out of space, continuing the transfer
    /// risks partial file writes and data corruption. The transfer must
    /// abort immediately.
    #[error("Disk full at {}{}", path.display(), bytes_needed.map(|b| format!(" ({b} bytes needed)")).unwrap_or_default())]
    DiskFull {
        /// Path where disk full was detected.
        path: PathBuf,
        /// Number of bytes needed (if known).
        bytes_needed: Option<u64>,
    },

    /// Read-only filesystem.
    ///
    /// Cannot write to a read-only filesystem. This is fatal because
    /// it affects all subsequent file operations.
    #[error("Read-only filesystem at {}", path.display())]
    ReadOnlyFilesystem {
        /// Path where read-only filesystem was detected.
        path: PathBuf,
    },

    /// Wire protocol error.
    ///
    /// Protocol violations indicate a fundamental communication problem
    /// that cannot be recovered from.
    #[error("Protocol error: {message}")]
    ProtocolError {
        /// Description of the protocol error.
        message: String,
    },

    /// Other fatal I/O error.
    ///
    /// Catch-all for I/O errors that should abort the transfer.
    #[error("I/O error: {0}")]
    Io(#[source] io::Error),
}

/// Recoverable errors that allow skipping the current file.
#[derive(Debug, Error)]
pub enum DeltaRecoverableError {
    /// File not found (disappeared during transfer).
    ///
    /// If a file is deleted between the file list being generated and
    /// the actual transfer, we can safely skip it.
    #[error("File not found: {}", path.display())]
    FileNotFound {
        /// Path to the missing file.
        path: PathBuf,
    },

    /// Permission denied (insufficient privileges).
    ///
    /// Permission errors on individual files can be skipped. The user
    /// will see a warning but the transfer continues.
    #[error("Permission denied for {operation} on {}", path.display())]
    PermissionDenied {
        /// Path where permission was denied.
        path: PathBuf,
        /// Operation that was attempted (e.g., "open", "read", "write").
        operation: String,
    },

    /// Other I/O error that allows continuing.
    ///
    /// Catch-all for I/O errors that affect only the current file.
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
        // Transient errors - treat as recoverable for now
        // (future: could implement retry logic)
        WouldBlock | Interrupted => DeltaTransferError::Recoverable(DeltaRecoverableError::Io {
            path: path.to_path_buf(),
            error: err,
        }),

        // Recoverable - skip file
        NotFound => DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound {
            path: path.to_path_buf(),
        }),
        PermissionDenied => {
            DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
                path: path.to_path_buf(),
                operation: operation.to_owned(),
            })
        }

        // Fatal - abort transfer
        StorageFull => DeltaTransferError::Fatal(DeltaFatalError::DiskFull {
            path: path.to_path_buf(),
            bytes_needed: None,
        }),

        // Read-only filesystem is fatal
        ReadOnlyFilesystem => DeltaTransferError::Fatal(DeltaFatalError::ReadOnlyFilesystem {
            path: path.to_path_buf(),
        }),

        // Default to fatal for unknown errors
        _ => DeltaTransferError::Fatal(DeltaFatalError::Io(err)),
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
}
