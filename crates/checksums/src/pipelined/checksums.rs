//! Block checksum types and pipelined computation functions.
//!
//! Provides `BlockChecksums` for per-block rolling+strong pairs, a batch
//! `compute_checksums_pipelined` function, and a streaming
//! `PipelinedChecksumIterator` for interleaving with network writes.

use std::io::{self, Read};

use crate::RollingDigest;
use crate::strong::StrongDigest;

use super::config::PipelineConfig;
use super::reader::DoubleBufferedReader;

/// Result of computing both rolling and strong checksums for a single block.
///
/// Contains the same data as upstream rsync's per-block checksum pair
/// sent during delta-transfer.
#[derive(Clone, Debug)]
pub struct BlockChecksums<D> {
    /// Rolling checksum (weak hash) for fast block matching.
    pub rolling: RollingDigest,
    /// Strong checksum digest for collision verification.
    pub strong: D,
    /// Number of bytes in this block (may be less than block size for the final block).
    pub len: usize,
}

/// Computes checksums for all blocks in a reader using double-buffering.
///
/// Combines `DoubleBufferedReader` with checksum computation, overlapping
/// I/O with hashing for throughput improvement on CPU-intensive checksums.
///
/// # Type Parameters
///
/// * `D` - The strong digest algorithm (e.g., `Md5`, `Sha256`)
/// * `R` - The reader type
///
/// # Errors
///
/// Returns an error if reading from the input fails.
///
/// # Example
///
/// ```
/// use checksums::pipelined::{compute_checksums_pipelined, PipelineConfig};
/// use checksums::strong::Md5;
/// use std::io::Cursor;
///
/// let data = vec![0u8; 256 * 1024];
/// let config = PipelineConfig::default().with_block_size(64 * 1024);
/// let checksums = compute_checksums_pipelined::<Md5, _>(
///     Cursor::new(data),
///     config,
///     None,
/// )?;
/// assert_eq!(checksums.len(), 4);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn compute_checksums_pipelined<D, R>(
    reader: R,
    config: PipelineConfig,
    size_hint: Option<u64>,
) -> io::Result<Vec<BlockChecksums<D::Digest>>>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read + Send + 'static,
{
    let mut buffered_reader = DoubleBufferedReader::with_size_hint(reader, config, size_hint);
    let mut results = Vec::new();

    while let Some(block) = buffered_reader.next_block()? {
        let rolling = RollingDigest::from_bytes(block);
        let strong = D::digest(block);
        results.push(BlockChecksums {
            rolling,
            strong,
            len: block.len(),
        });
    }

    Ok(results)
}

/// Streaming iterator for pipelined checksum computation.
///
/// Unlike `compute_checksums_pipelined`, this processes checksums one at
/// a time without collecting into a vector - useful when the caller needs
/// to interleave checksum results with network writes.
pub struct PipelinedChecksumIterator<D, R>
where
    D: StrongDigest,
{
    reader: DoubleBufferedReader<R>,
    _phantom: std::marker::PhantomData<D>,
}

impl<D, R> PipelinedChecksumIterator<D, R>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read + Send + 'static,
{
    /// Creates a new pipelined checksum iterator.
    #[must_use]
    pub fn new(reader: R, config: PipelineConfig) -> Self {
        Self::with_size_hint(reader, config, None)
    }

    /// Creates a new pipelined checksum iterator with a size hint.
    #[must_use]
    pub fn with_size_hint(reader: R, config: PipelineConfig, size_hint: Option<u64>) -> Self {
        Self {
            reader: DoubleBufferedReader::with_size_hint(reader, config, size_hint),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Returns the next block's checksums, or `None` if EOF reached.
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    pub fn next_block_checksums(&mut self) -> io::Result<Option<BlockChecksums<D::Digest>>> {
        match self.reader.next_block()? {
            Some(block) => {
                let rolling = RollingDigest::from_bytes(block);
                let strong = D::digest(block);
                Ok(Some(BlockChecksums {
                    rolling,
                    strong,
                    len: block.len(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Returns whether the iterator is using pipelined reading.
    #[must_use]
    pub fn is_pipelined(&self) -> bool {
        self.reader.is_pipelined()
    }
}
