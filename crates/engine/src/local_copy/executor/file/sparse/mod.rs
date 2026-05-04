//! Sparse file support - zero-run detection, hole punching, and sparse I/O.
//!
//! Implements `--sparse` semantics: detects contiguous zero-byte regions in
//! file data and writes them as filesystem holes rather than allocating blocks.
//! Uses `SEEK_HOLE`/`SEEK_DATA` for reading and `fallocate(PUNCH_HOLE)` on
//! Linux for post-write hole creation.
//!
//! // upstream: fileio.c:write_sparse() - sparse write with seek-past-zeros

mod detect;
mod hole_punch;
mod reader;
mod state;
mod writer;

#[cfg(test)]
mod tests;

pub use detect::SparseDetector;
pub use reader::SparseReader;
pub use writer::SparseWriter;

pub(crate) use state::{SparseWriteState, write_sparse_chunk};

use detect::{leading_zero_run, trailing_zero_run};

/// Represents a region in a file, either containing data or a hole (sparse region).
///
/// Used by sparse file detection and reading operations to efficiently identify
/// and process zero-filled regions in files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparseRegion {
    /// A region containing non-zero data.
    Data {
        /// Starting offset of the data region.
        offset: u64,
        /// Length of the data region in bytes.
        length: u64,
    },
    /// A sparse hole region (all zeros).
    Hole {
        /// Starting offset of the hole.
        offset: u64,
        /// Length of the hole in bytes.
        length: u64,
    },
}

impl SparseRegion {
    /// Returns the starting offset of this region.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        match self {
            Self::Data { offset, .. } | Self::Hole { offset, .. } => *offset,
        }
    }

    /// Returns the length of this region in bytes.
    #[must_use]
    pub const fn length(&self) -> u64 {
        match self {
            Self::Data { length, .. } | Self::Hole { length, .. } => *length,
        }
    }

    /// Returns true if this is a hole (sparse) region.
    #[must_use]
    pub const fn is_hole(&self) -> bool {
        matches!(self, Self::Hole { .. })
    }

    /// Returns true if this is a data region.
    #[must_use]
    pub const fn is_data(&self) -> bool {
        matches!(self, Self::Data { .. })
    }
}

/// Threshold for detecting sparse (all-zeros) regions during file writes.
///
/// A run of zeros at least this size will be converted to a sparse hole
/// using fallocate(PUNCH_HOLE) or seek past on supported systems.
///
/// Matches upstream rsync's CHUNK_SIZE (32KB) for consistent behavior.
/// Using a larger threshold reduces syscall overhead for small zero runs
/// while still efficiently handling large sparse regions.
const SPARSE_WRITE_SIZE: usize = 32 * 1024;

/// Buffer size for writing zeros when fallocate is not supported.
/// Matches upstream rsync's do_punch_hole fallback buffer size.
const ZERO_WRITE_BUFFER_SIZE: usize = 4096;
