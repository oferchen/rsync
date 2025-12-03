//! Batch mode support for offline/disconnected transfer workflows.
//!
//! This module implements rsync's batch mode functionality, which allows
//! recording a transfer operation to a file and replaying it later. This
//! is useful for scenarios where the source and destination are not
//! simultaneously available, or for distributing the same changes to
//! multiple destinations.
//!
//! ## Batch File Format
//!
//! The batch file format matches upstream rsync's binary format:
//!
//! 1. **Header**:
//!    - Protocol version (i32)
//!    - Compat flags (varint, protocol >= 30)
//!    - Checksum seed (i32)
//!    - Stream flags bitmap (i32)
//!
//! 2. **File list**:
//!    - Encoded using the standard flist format
//!    - Includes all file metadata
//!
//! 3. **Delta operations**:
//!    - Copy and literal operations for each file
//!    - Checksums for verification
//!
//! 4. **Statistics** (at end):
//!    - Total bytes read/written
//!    - Transfer size
//!    - Timing information
//!
//! ## Shell Script
//!
//! In addition to the binary batch file, a shell script (.sh) is created
//! that contains the replay command. This script:
//! - Converts `--write-batch` to `--read-batch`
//! - Preserves all relevant options
//! - Includes filter rules if present
//!
//! ## Usage
//!
//! ### Writing a batch:
//! ```text
//! oc-rsync -a --write-batch=mybatch source/ /tmp/dest/
//! # Creates: mybatch (binary) and mybatch.sh (script)
//! ```
//!
//! ### Reading a batch:
//! ```text
//! ./mybatch.sh /actual/dest/
//! # Or manually:
//! oc-rsync --read-batch=mybatch /actual/dest/
//! ```
//!
//! ### Only write batch (no actual transfer):
//! ```text
//! oc-rsync -a --only-write-batch=mybatch source/ dest/
//! # Creates batch files but doesn't modify dest/
//! ```

pub mod format;
pub mod reader;
pub mod script;
pub mod writer;

#[cfg(test)]
mod tests;

pub use format::{BatchFlags, BatchHeader, FileEntry};
pub use reader::BatchReader;
pub use writer::BatchWriter;

use std::path::Path;

/// Batch mode operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchMode {
    /// Write batch file while performing transfer.
    Write,
    /// Write batch file without performing transfer.
    OnlyWrite,
    /// Read and replay batch file.
    Read,
}

/// Configuration for batch mode operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchConfig {
    /// The batch mode operation type.
    pub mode: BatchMode,
    /// Path to the batch file (without .sh extension).
    pub batch_path: String,
    /// Protocol version being used.
    pub protocol_version: i32,
    /// Compatibility flags (protocol >= 30).
    pub compat_flags: Option<u64>,
    /// Checksum seed for the transfer.
    pub checksum_seed: i32,
}

impl BatchConfig {
    /// Create a new batch configuration.
    pub fn new(mode: BatchMode, batch_path: String, protocol_version: i32) -> Self {
        Self {
            mode,
            batch_path,
            protocol_version,
            compat_flags: if protocol_version >= 30 {
                Some(0)
            } else {
                None
            },
            checksum_seed: 0,
        }
    }

    /// Set the compatibility flags.
    pub fn with_compat_flags(mut self, flags: u64) -> Self {
        if self.protocol_version >= 30 {
            self.compat_flags = Some(flags);
        }
        self
    }

    /// Set the checksum seed.
    pub fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Get the path to the binary batch file.
    pub fn batch_file_path(&self) -> &Path {
        Path::new(&self.batch_path)
    }

    /// Get the path to the shell script file.
    pub fn script_file_path(&self) -> String {
        format!("{}.sh", self.batch_path)
    }

    /// Check if this is a write mode (Write or OnlyWrite).
    pub fn is_write_mode(&self) -> bool {
        matches!(self.mode, BatchMode::Write | BatchMode::OnlyWrite)
    }

    /// Check if this is read mode.
    pub fn is_read_mode(&self) -> bool {
        matches!(self.mode, BatchMode::Read)
    }

    /// Check if actual transfer should occur.
    pub fn should_transfer(&self) -> bool {
        !matches!(self.mode, BatchMode::OnlyWrite)
    }
}
