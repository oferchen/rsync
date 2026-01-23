#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Batch mode support for offline/disconnected transfer workflows.
//!
//! This crate implements rsync's batch mode functionality, which allows
//! recording a transfer operation to a file and replaying it later. This
//! is useful for scenarios where the source and destination are not
//! simultaneously available, or for distributing the same changes to
//! multiple destinations.
//!
//! # Overview
//!
//! Batch mode enables a "capture and replay" workflow for file transfers:
//!
//! 1. **Capture**: Record a transfer to a batch file (binary) and companion script
//! 2. **Distribute**: Copy the batch file to other machines (USB, network, etc.)
//! 3. **Replay**: Apply the recorded changes to different destinations
//!
//! This pattern is particularly useful for:
//! - Air-gapped systems without direct network connectivity
//! - Distributing identical updates to many machines
//! - Auditing and reviewing changes before applying them
//! - Bandwidth-constrained environments where batch files can be compressed
//!
//! # Batch File Format
//!
//! The batch file format matches upstream rsync's binary format for compatibility:
//!
//! 1. **Header** ([`BatchHeader`]):
//!    - Stream flags bitmap (i32) - see [`BatchFlags`]
//!    - Protocol version (i32)
//!    - Compat flags (varint, protocol >= 30)
//!    - Checksum seed (i32)
//!
//! 2. **File list** ([`FileEntry`]):
//!    - Encoded using the standard flist format
//!    - Includes all file metadata (path, mode, size, mtime, uid/gid)
//!
//! 3. **Delta operations** ([`DeltaOp`]):
//!    - Copy and literal operations for each file
//!    - Checksums for verification
//!
//! 4. **Statistics** (at end):
//!    - Total bytes read/written
//!    - Transfer size
//!    - Timing information
//!
//! # Shell Script
//!
//! In addition to the binary batch file, a shell script (.sh) is created
//! that contains the replay command. This script:
//! - Converts `--write-batch` to `--read-batch`
//! - Preserves all relevant options
//! - Includes filter rules if present
//!
//! See the [`script`] module for script generation functions.
//!
//! # Command Line Usage
//!
//! ## Writing a batch (with transfer)
//! ```text
//! oc-rsync -a --write-batch=mybatch source/ /tmp/dest/
//! # Creates: mybatch (binary) and mybatch.sh (script)
//! # Also transfers files to /tmp/dest/
//! ```
//!
//! ## Writing a batch (without transfer)
//! ```text
//! oc-rsync -a --only-write-batch=mybatch source/ dest/
//! # Creates batch files but doesn't modify dest/
//! # Useful for creating batches without affecting the destination
//! ```
//!
//! ## Reading/replaying a batch
//! ```text
//! # Using the generated script:
//! ./mybatch.sh /actual/dest/
//!
//! # Or manually:
//! oc-rsync --read-batch=mybatch /actual/dest/
//! ```
//!
//! # Programmatic Usage
//!
//! ## Writing a batch file
//!
//! ```no_run
//! use batch::{BatchConfig, BatchMode, BatchWriter, BatchFlags};
//!
//! // Configure the batch operation
//! let config = BatchConfig::new(
//!     BatchMode::Write,
//!     "/tmp/mybatch".to_string(),
//!     31,  // protocol version
//! )
//! .with_checksum_seed(12345);
//!
//! // Create the writer
//! let mut writer = BatchWriter::new(config)?;
//!
//! // Write the header with stream flags
//! let mut flags = BatchFlags::default();
//! flags.recurse = true;
//! flags.preserve_uid = true;
//! writer.write_header(flags)?;
//!
//! // Write file data (normally done during transfer)
//! writer.write_data(b"file content data")?;
//!
//! // Finalize and close the batch file
//! writer.finalize()?;
//! # Ok::<(), batch::BatchError>(())
//! ```
//!
//! ## Reading a batch file
//!
//! ```no_run
//! use batch::{BatchConfig, BatchMode, BatchReader};
//!
//! // Configure to read the batch
//! let config = BatchConfig::new(
//!     BatchMode::Read,
//!     "/tmp/mybatch".to_string(),
//!     31,  // must match the protocol version used to write
//! );
//!
//! // Open the batch file
//! let mut reader = BatchReader::new(config)?;
//!
//! // Read and validate the header
//! let flags = reader.read_header()?;
//! println!("Batch uses recursive mode: {}", flags.recurse);
//!
//! // Read file data
//! let mut buf = vec![0u8; 1024];
//! let bytes_read = reader.read_data(&mut buf)?;
//! # Ok::<(), batch::BatchError>(())
//! ```
//!
//! # Error Handling
//!
//! All batch operations return [`BatchResult<T>`], which wraps potential
//! [`BatchError`] variants:
//!
//! - [`BatchError::Io`] - File system or I/O errors
//! - [`BatchError::InvalidFormat`] - Malformed or incompatible batch files
//! - [`BatchError::Unsupported`] - Features not yet implemented
//!
//! # Thread Safety
//!
//! [`BatchReader`] and [`BatchWriter`] are not thread-safe and should be
//! used from a single thread. For concurrent batch processing, create
//! separate reader/writer instances for each thread.

mod error;

/// Binary format definitions for batch files.
///
/// This module contains the low-level structures for reading and writing
/// the batch file format: [`BatchHeader`], [`BatchFlags`], and [`FileEntry`].
pub mod format;

/// Batch file reader for replaying recorded transfers.
///
/// See [`BatchReader`] for the main reader type.
pub mod reader;

/// Shell script generation for batch replay.
///
/// This module provides functions to generate executable shell scripts
/// that can replay batch files. See [`script::generate_script`] and
/// [`script::generate_script_with_args`].
pub mod script;

/// Batch file writer for recording transfers.
///
/// See [`BatchWriter`] for the main writer type.
pub mod writer;

#[cfg(test)]
mod tests;

// Re-exports for convenient access to commonly used types

pub use error::{BatchError, BatchResult};
pub use format::{BatchFlags, BatchHeader, FileEntry};
pub use reader::BatchReader;
pub use writer::BatchWriter;

/// Delta operation type re-exported from protocol for [`BatchReader::read_all_delta_ops`].
///
/// Delta operations represent the instructions for reconstructing a file
/// from a combination of existing data (copy operations) and new data
/// (literal operations).
pub use protocol::wire::DeltaOp;

use std::path::Path;

/// Batch mode operation type.
///
/// Determines how the batch system behaves during a transfer operation.
/// This enum maps directly to the rsync command-line options:
///
/// - `--write-batch=FILE` maps to [`BatchMode::Write`]
/// - `--only-write-batch=FILE` maps to [`BatchMode::OnlyWrite`]
/// - `--read-batch=FILE` maps to [`BatchMode::Read`]
///
/// # Examples
///
/// ```
/// use batch::BatchMode;
///
/// let mode = BatchMode::Write;
/// assert!(matches!(mode, BatchMode::Write));
///
/// // BatchMode is Copy, so it can be used without cloning
/// let mode2 = mode;
/// assert_eq!(mode, mode2);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchMode {
    /// Write batch file while performing transfer.
    ///
    /// This mode records the transfer operations to a batch file while
    /// simultaneously applying them to the destination. Use this when
    /// you want to both update the destination and create a batch for
    /// distributing the same changes elsewhere.
    ///
    /// Corresponds to `--write-batch=FILE` on the command line.
    Write,

    /// Write batch file without performing transfer.
    ///
    /// This mode only creates the batch file without modifying the
    /// destination. Useful for:
    /// - Creating batches without affecting production systems
    /// - Testing batch generation
    /// - Preparing updates in advance
    ///
    /// Corresponds to `--only-write-batch=FILE` on the command line.
    OnlyWrite,

    /// Read and replay batch file.
    ///
    /// This mode reads a previously created batch file and applies its
    /// recorded operations to the destination. The batch file must have
    /// been created with the same protocol version.
    ///
    /// Corresponds to `--read-batch=FILE` on the command line.
    Read,
}

/// Configuration for batch mode operations.
///
/// This struct holds all the configuration needed to read or write a batch
/// file. It encapsulates the batch mode, file paths, protocol version, and
/// other settings that affect batch file generation and parsing.
///
/// # Creating a Configuration
///
/// Use [`BatchConfig::new`] to create a basic configuration, then chain
/// builder methods to customize it:
///
/// ```
/// use batch::{BatchConfig, BatchMode};
///
/// let config = BatchConfig::new(
///     BatchMode::Write,
///     "/tmp/backup.batch".to_string(),
///     31,  // protocol version
/// )
/// .with_checksum_seed(42)
/// .with_compat_flags(0x01);
///
/// assert!(config.is_write_mode());
/// assert!(config.should_transfer());
/// ```
///
/// # File Paths
///
/// A batch operation creates two files:
/// - The binary batch file at `batch_path`
/// - A shell script at `batch_path.sh`
///
/// ```
/// use batch::{BatchConfig, BatchMode};
///
/// let config = BatchConfig::new(
///     BatchMode::Write,
///     "/tmp/mybatch".to_string(),
///     31,
/// );
///
/// assert_eq!(config.batch_file_path().to_str(), Some("/tmp/mybatch"));
/// assert_eq!(config.script_file_path(), "/tmp/mybatch.sh");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchConfig {
    /// The batch mode operation type.
    ///
    /// Determines whether this configuration is for reading or writing
    /// a batch file, and whether actual file transfer should occur.
    pub mode: BatchMode,

    /// Path to the batch file (without .sh extension).
    ///
    /// This is the base path for the batch operation. The binary batch
    /// data is written to this path, and the shell script is written
    /// to `{batch_path}.sh`.
    pub batch_path: String,

    /// Protocol version being used.
    ///
    /// Must match between writer and reader. Protocol version affects
    /// which features are available (e.g., compat_flags requires >= 30).
    pub protocol_version: i32,

    /// Compatibility flags (protocol >= 30).
    ///
    /// These flags are automatically set based on protocol version in
    /// [`BatchConfig::new`]. For protocol versions < 30, this is `None`.
    pub compat_flags: Option<u64>,

    /// Checksum seed for the transfer.
    ///
    /// Used to initialize the rolling checksum algorithm. Must match
    /// between writer and reader for checksums to validate correctly.
    pub checksum_seed: i32,
}

impl BatchConfig {
    /// Create a new batch configuration.
    ///
    /// Creates a configuration with default values for `compat_flags` and
    /// `checksum_seed`. Use the builder methods to customize these.
    ///
    /// # Arguments
    ///
    /// * `mode` - The batch operation mode (read, write, or only-write)
    /// * `batch_path` - Path to the batch file (without .sh extension)
    /// * `protocol_version` - The rsync protocol version to use
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// // Create a write configuration
    /// let write_config = BatchConfig::new(
    ///     BatchMode::Write,
    ///     "/tmp/backup".to_string(),
    ///     31,
    /// );
    ///
    /// // Create a read configuration
    /// let read_config = BatchConfig::new(
    ///     BatchMode::Read,
    ///     "/tmp/backup".to_string(),
    ///     31,
    /// );
    /// ```
    ///
    /// # Protocol Version Notes
    ///
    /// - Protocol >= 30: `compat_flags` is initialized to `Some(0)`
    /// - Protocol < 30: `compat_flags` is `None`
    pub const fn new(mode: BatchMode, batch_path: String, protocol_version: i32) -> Self {
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
    ///
    /// Compatibility flags control protocol-specific behavior. This is only
    /// applicable for protocol version 30 and above; for earlier versions,
    /// this method has no effect.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let config = BatchConfig::new(BatchMode::Write, "/tmp/batch".to_string(), 31)
    ///     .with_compat_flags(0x01);
    ///
    /// assert_eq!(config.compat_flags, Some(0x01));
    /// ```
    pub const fn with_compat_flags(mut self, flags: u64) -> Self {
        if self.protocol_version >= 30 {
            self.compat_flags = Some(flags);
        }
        self
    }

    /// Set the checksum seed.
    ///
    /// The checksum seed is used to initialize the rolling checksum algorithm
    /// during delta encoding. When replaying a batch, the same seed must be
    /// used to correctly verify file integrity.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let config = BatchConfig::new(BatchMode::Write, "/tmp/batch".to_string(), 31)
    ///     .with_checksum_seed(0xDEADBEEFu32 as i32);
    ///
    /// assert_eq!(config.checksum_seed, 0xDEADBEEFu32 as i32);
    /// ```
    pub const fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Get the path to the binary batch file.
    ///
    /// Returns the path where the binary batch data is stored. This is
    /// the same as `batch_path` but as a [`Path`] reference.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    /// use std::path::Path;
    ///
    /// let config = BatchConfig::new(BatchMode::Write, "/tmp/mybatch".to_string(), 31);
    /// assert_eq!(config.batch_file_path(), Path::new("/tmp/mybatch"));
    /// ```
    pub fn batch_file_path(&self) -> &Path {
        Path::new(&self.batch_path)
    }

    /// Get the path to the shell script file.
    ///
    /// Returns the path where the replay shell script is stored. This is
    /// always `{batch_path}.sh`.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let config = BatchConfig::new(BatchMode::Write, "/tmp/mybatch".to_string(), 31);
    /// assert_eq!(config.script_file_path(), "/tmp/mybatch.sh");
    /// ```
    pub fn script_file_path(&self) -> String {
        format!("{}.sh", self.batch_path)
    }

    /// Check if this is a write mode (Write or OnlyWrite).
    ///
    /// Returns `true` if the configuration is for writing a batch file,
    /// regardless of whether actual file transfer will occur.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let write = BatchConfig::new(BatchMode::Write, "/tmp/b".to_string(), 31);
    /// let only_write = BatchConfig::new(BatchMode::OnlyWrite, "/tmp/b".to_string(), 31);
    /// let read = BatchConfig::new(BatchMode::Read, "/tmp/b".to_string(), 31);
    ///
    /// assert!(write.is_write_mode());
    /// assert!(only_write.is_write_mode());
    /// assert!(!read.is_write_mode());
    /// ```
    pub const fn is_write_mode(&self) -> bool {
        matches!(self.mode, BatchMode::Write | BatchMode::OnlyWrite)
    }

    /// Check if this is read mode.
    ///
    /// Returns `true` if the configuration is for reading and replaying
    /// a batch file.
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let read = BatchConfig::new(BatchMode::Read, "/tmp/b".to_string(), 31);
    /// let write = BatchConfig::new(BatchMode::Write, "/tmp/b".to_string(), 31);
    ///
    /// assert!(read.is_read_mode());
    /// assert!(!write.is_read_mode());
    /// ```
    pub const fn is_read_mode(&self) -> bool {
        matches!(self.mode, BatchMode::Read)
    }

    /// Check if actual transfer should occur.
    ///
    /// Returns `true` if the batch mode involves actually transferring
    /// files to the destination. This is `true` for [`BatchMode::Write`]
    /// and [`BatchMode::Read`], but `false` for [`BatchMode::OnlyWrite`].
    ///
    /// # Examples
    ///
    /// ```
    /// use batch::{BatchConfig, BatchMode};
    ///
    /// let write = BatchConfig::new(BatchMode::Write, "/tmp/b".to_string(), 31);
    /// let only_write = BatchConfig::new(BatchMode::OnlyWrite, "/tmp/b".to_string(), 31);
    /// let read = BatchConfig::new(BatchMode::Read, "/tmp/b".to_string(), 31);
    ///
    /// assert!(write.should_transfer());      // Write mode transfers files
    /// assert!(!only_write.should_transfer()); // OnlyWrite just creates batch
    /// assert!(read.should_transfer());        // Read mode applies changes
    /// ```
    pub const fn should_transfer(&self) -> bool {
        !matches!(self.mode, BatchMode::OnlyWrite)
    }
}
