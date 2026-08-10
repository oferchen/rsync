//! Sparse file support - zero-run detection, hole punching, and sparse I/O.
//!
//! Implements `--sparse` semantics: detects contiguous zero-byte regions in
//! file data and writes them as filesystem holes rather than allocating blocks.
//! Uses `SEEK_HOLE`/`SEEK_DATA` for reading and `fallocate(PUNCH_HOLE)` on
//! Linux for post-write hole creation.

// upstream: fileio.c:write_sparse() - sparse write with seek-past-zeros

mod detect;
mod hole_punch;
mod reader;
mod state;
mod writer;

#[cfg(test)]
mod tests;

pub use detect::SparseDetector;
pub use reader::SparseReader;
pub use writer::{SparseWriteStats, SparseWriter, ZeroScanStrategy};

/// Selects the mechanism used by [`SparseReader::detect_holes`] to identify
/// sparse regions in a file.
///
/// Mirrors upstream rsync's `--sparse` semantics by separating the *whether*
/// (controlled by `--sparse` / `-S`) from the *how* (controlled by
/// `--sparse-detect`). The strategy only affects detection of pre-existing
/// holes when reading source files; downstream write paths continue to honour
/// `--sparse` independently.
///
/// # Variants
///
/// - [`SparseDetectStrategy::Auto`] keeps the historical behaviour: prefer
///   `SEEK_HOLE` / `SEEK_DATA` on Linux, fall back to byte scanning when those
///   syscalls fail or are unsupported.
/// - [`SparseDetectStrategy::Seek`] forces the seek-based path; on platforms
///   without `SEEK_HOLE` support this still yields a single data region (no
///   hole detection).
/// - [`SparseDetectStrategy::Map`] requests filesystem extent mapping (FIEMAP
///   on Linux). On non-Linux platforms it gracefully degrades to seek-based
///   detection.
/// - [`SparseDetectStrategy::None`] disables hole detection entirely. Zero
///   runs are written verbatim by the destination writer rather than skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SparseDetectStrategy {
    /// Default behaviour: probe `SEEK_HOLE`/`SEEK_DATA`, fall back to scanning.
    #[default]
    Auto,
    /// Force `SEEK_HOLE`/`SEEK_DATA` based detection.
    Seek,
    /// Use filesystem extent mapping (FIEMAP) where available.
    Map,
    /// Disable hole detection; treat the file as fully populated data.
    None,
}

impl SparseDetectStrategy {
    /// Returns the canonical lowercase token used on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Seek => "seek",
            Self::Map => "map",
            Self::None => "none",
        }
    }

    /// Parses a CLI token into a [`SparseDetectStrategy`].
    ///
    /// The match is case-insensitive. Unknown tokens are returned as the
    /// original input for the caller to surface in an error.
    ///
    /// # Errors
    ///
    /// Returns the original input string when no variant matches.
    pub fn parse(value: &str) -> Result<Self, &str> {
        let lowered = value.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "auto" => Ok(Self::Auto),
            "seek" => Ok(Self::Seek),
            "map" => Ok(Self::Map),
            "none" => Ok(Self::None),
            _ => Err(value),
        }
    }
}

impl std::fmt::Display for SparseDetectStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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

/// Scan window for sparse zero-run detection during file writes.
///
/// Upstream `write_file()` hands `write_sparse()` at most this many bytes per
/// call (`fileio.c:156`, `int len1 = MIN(len, SPARSE_WRITE_SIZE)`), so only the
/// leading and trailing zeros of each 1 KB window become holes. Matching the
/// window is required for allocated-block parity with upstream: a larger window
/// writes sub-window interior zero runs as literal data, leaving them allocated
/// where upstream deallocates them.
///
/// Matches upstream rsync's `SPARSE_WRITE_SIZE` (1024) in `rsync.h`.
const SPARSE_WRITE_SIZE: usize = 1024;

/// Buffer size for writing zeros when fallocate is not supported.
/// Matches upstream rsync's do_punch_hole fallback buffer size.
const ZERO_WRITE_BUFFER_SIZE: usize = 4096;
