//! Input, output, configuration, and internal message types for the checksum pipeline.

use std::io;

/// Minimum number of files to enable pipelined processing.
///
/// Below this threshold, sequential processing is used to avoid
/// thread overhead for trivial workloads.
pub const PIPELINE_THRESHOLD: usize = 4;

/// Default buffer size for reading chunks (64 KiB).
pub(super) const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// Input specification for a single checksum computation.
///
/// Pairs a reader with an optional size hint so the pipeline can
/// decide whether to enable double-buffering for this input.
#[derive(Debug)]
pub struct ChecksumInput<R> {
    /// The reader to compute checksums from.
    pub reader: R,
    /// Optional size hint in bytes.
    pub size_hint: Option<u64>,
}

impl<R> ChecksumInput<R> {
    /// Creates a new checksum input.
    ///
    /// # Arguments
    ///
    /// * `reader` - The reader to process
    /// * `size` - Expected size in bytes (0 if unknown)
    #[must_use]
    pub fn new(reader: R, size: u64) -> Self {
        Self {
            reader,
            size_hint: if size > 0 { Some(size) } else { None },
        }
    }

    /// Creates a new checksum input without size hint.
    #[must_use]
    pub fn without_hint(reader: R) -> Self {
        Self {
            reader,
            size_hint: None,
        }
    }
}

/// Result of computing a checksum for a single input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecksumResult<D> {
    /// The computed strong checksum digest.
    pub digest: D,
    /// Number of bytes processed.
    pub bytes_processed: u64,
}

/// Configuration for the pipelined checksum processor.
#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    /// Size of each buffer for reading chunks.
    pub(super) buffer_size: usize,
    /// Minimum number of inputs to enable pipelining.
    pub(super) threshold: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_BUFFER_SIZE,
            threshold: PIPELINE_THRESHOLD,
        }
    }
}

impl PipelineConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the buffer size for reading chunks.
    #[must_use]
    pub const fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    /// Sets the minimum number of inputs for pipelining.
    #[must_use]
    pub const fn with_threshold(mut self, threshold: usize) -> Self {
        self.threshold = threshold;
        self
    }
}

/// Message sent from I/O thread to compute thread in pipelined mode.
pub(super) enum PipelineMessage {
    /// A chunk of data was read from an input.
    Chunk {
        /// Index of the input this chunk belongs to.
        input_index: usize,
        /// The data that was read.
        data: Vec<u8>,
    },
    /// An input has been fully read.
    InputComplete {
        /// Index of the completed input.
        input_index: usize,
    },
    /// All inputs have been processed.
    AllComplete,
    /// An I/O error occurred.
    Error(io::Error),
}
