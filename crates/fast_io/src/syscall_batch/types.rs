//! Type definitions for batched metadata operations.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

/// A metadata operation to be performed on a file.
#[derive(Debug, Clone)]
pub enum MetadataOp {
    /// Stat a file (follow symlinks).
    Stat(PathBuf),
    /// Lstat a file (don't follow symlinks).
    Lstat(PathBuf),
    /// Set file times.
    SetTimes {
        /// Path to the file.
        path: PathBuf,
        /// Access time (None = don't change).
        atime: Option<SystemTime>,
        /// Modification time (None = don't change).
        mtime: Option<SystemTime>,
    },
    /// Set file permissions.
    SetPermissions {
        /// Path to the file.
        path: PathBuf,
        /// Unix permission mode bits.
        mode: u32,
    },
}

/// Result of a metadata operation.
#[derive(Debug)]
pub enum MetadataResult {
    /// Result of a Stat or Lstat operation.
    Stat(io::Result<fs::Metadata>),
    /// Result of a SetTimes operation.
    SetTimes(io::Result<()>),
    /// Result of a SetPermissions operation.
    SetPermissions(io::Result<()>),
}
