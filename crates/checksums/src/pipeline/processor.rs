//! Builder and facade for the dual-path checksum processor.

use std::io::{self, Read};

use crate::strong::StrongDigest;

use super::pipelined::pipelined_checksum;
use super::sequential::sequential_checksum;
use super::types::{ChecksumInput, ChecksumResult, PipelineConfig};

/// Builder for creating a pipelined checksum processor.
#[derive(Default)]
pub struct PipelinedChecksumBuilder {
    config: PipelineConfig,
}

impl PipelinedChecksumBuilder {
    /// Creates a new builder with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the buffer size for reading chunks.
    #[must_use]
    pub fn buffer_size(mut self, size: usize) -> Self {
        self.config.buffer_size = size;
        self
    }

    /// Sets the minimum number of inputs for pipelining.
    #[must_use]
    pub fn threshold(mut self, threshold: usize) -> Self {
        self.config.threshold = threshold;
        self
    }

    /// Builds the pipelined checksum processor.
    #[must_use]
    pub fn build(self) -> PipelinedChecksum {
        PipelinedChecksum {
            config: self.config,
        }
    }
}

/// Pipelined checksum processor with dual-path execution.
///
/// Provides both sequential and pipelined execution paths, with runtime
/// selection based on the number of inputs relative to the threshold.
pub struct PipelinedChecksum {
    pub(super) config: PipelineConfig,
}

impl PipelinedChecksum {
    /// Creates a new pipelined checksum processor with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: PipelineConfig::default(),
        }
    }

    /// Returns a builder for configuring the processor.
    #[must_use]
    pub fn builder() -> PipelinedChecksumBuilder {
        PipelinedChecksumBuilder::new()
    }

    /// Computes checksums for the given inputs using automatic path selection.
    ///
    /// If `inputs.len() >= threshold`, uses pipelined processing.
    /// Otherwise, uses sequential processing.
    ///
    /// # Type Parameters
    ///
    /// * `D` - The strong digest algorithm (e.g., `Md5`, `Sha256`)
    /// * `R` - The reader type
    ///
    /// # Errors
    ///
    /// Returns an error if reading from any input fails.
    pub fn compute<D, R>(
        &self,
        inputs: Vec<ChecksumInput<R>>,
    ) -> io::Result<Vec<ChecksumResult<D::Digest>>>
    where
        D: StrongDigest,
        D::Seed: Default,
        R: Read + Send + 'static,
    {
        if inputs.len() >= self.config.threshold {
            pipelined_checksum::<D, R>(inputs, self.config)
        } else {
            sequential_checksum::<D, R>(inputs, self.config)
        }
    }

    /// Returns the configured buffer size.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.config.buffer_size
    }

    /// Returns the configured threshold.
    #[must_use]
    pub fn threshold(&self) -> usize {
        self.config.threshold
    }
}

impl Default for PipelinedChecksum {
    fn default() -> Self {
        Self::new()
    }
}
