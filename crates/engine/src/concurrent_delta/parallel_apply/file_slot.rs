//! Per-file destination writer plus its reorder buffer.
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. Holds [`FileSlot`] (the per-file writer + reorder ring
//! that re-establishes submission order after the out-of-order rayon
//! verify step) and [`IngestError`], which distinguishes per-file
//! reorder-ring saturation from generic writer I/O so the applier can
//! update its ROB-3 telemetry without parsing error strings.

use std::io::{self, Write};

use super::super::reorder::ReorderBuffer;
use super::DeltaChunk;

/// Error outcomes for [`FileSlot::ingest`].
///
/// Distinguishes the per-file reorder-ring saturation case from generic
/// writer I/O failures so [`ParallelDeltaApplier`] can update its ROB-3
/// telemetry without parsing error strings. Both variants convert into a
/// plain [`io::Error`] for the existing `io::Result`-shaped public API.
///
/// [`ParallelDeltaApplier`]: super::ParallelDeltaApplier
#[derive(Debug)]
pub(in crate::concurrent_delta::parallel_apply) enum IngestError {
    /// The per-file reorder buffer rejected the chunk because its offset
    /// from `next_expected` exceeded the configured ring capacity. The
    /// chunk is not committed and the writer remains untouched.
    ReorderSaturated {
        /// Per-file chunk sequence that overflowed the ring.
        chunk_sequence: u64,
        /// Underlying [`ReorderBuffer`] capacity bound that was hit.
        capacity: usize,
    },
    /// Writer-side I/O failure while draining ready chunks.
    Io(io::Error),
}

impl From<IngestError> for io::Error {
    fn from(value: IngestError) -> Self {
        match value {
            IngestError::ReorderSaturated {
                chunk_sequence,
                capacity,
            } => io::Error::other(format!(
                "parallel apply reorder full: chunk_sequence={chunk_sequence} exceeds per-file ring capacity={capacity}"
            )),
            IngestError::Io(e) => e,
        }
    }
}

/// Per-file destination writer plus the reorder buffer that re-establishes
/// submission order after the rayon verify step completes out of order.
pub(super) struct FileSlot {
    pub(super) writer: Box<dyn Write + Send>,
    pub(super) reorder: ReorderBuffer<DeltaChunk>,
    bytes_written: u64,
}

impl FileSlot {
    pub(super) fn new(writer: Box<dyn Write + Send>, reorder_capacity: usize) -> Self {
        Self {
            writer,
            reorder: ReorderBuffer::new(reorder_capacity),
            bytes_written: 0,
        }
    }

    /// Inserts `chunk` into the reorder buffer and drains any contiguous run
    /// that is now ready, writing each ready chunk to the destination.
    ///
    /// The reorder buffer is the single source of truth for per-file
    /// sequencing; the writer only sees chunks once they have arrived in
    /// strict `chunk_sequence` order.
    ///
    /// Returns [`IngestError::ReorderSaturated`] when the per-file ring is
    /// full; the applier's [`ParallelDeltaApplier::note_reorder_saturation`]
    /// reads this to update the ROB-3 telemetry counter and emit the
    /// one-shot warning before mapping back to [`io::Error`] for the caller.
    ///
    /// [`ParallelDeltaApplier::note_reorder_saturation`]: super::ParallelDeltaApplier::note_reorder_saturation
    pub(super) fn ingest(&mut self, chunk: DeltaChunk) -> Result<(), IngestError> {
        let seq = chunk.chunk_sequence;
        let capacity = self.reorder.capacity();
        self.reorder
            .insert(seq, chunk)
            .map_err(|_| IngestError::ReorderSaturated {
                chunk_sequence: seq,
                capacity,
            })?;
        let ready: Vec<DeltaChunk> = self.reorder.drain_ready().collect();
        for chunk in ready {
            self.write_chunk(chunk).map_err(IngestError::Io)?;
        }
        Ok(())
    }

    fn write_chunk(&mut self, chunk: DeltaChunk) -> io::Result<()> {
        self.writer.write_all(&chunk.data)?;
        self.bytes_written = self
            .bytes_written
            .checked_add(chunk.data.len() as u64)
            .ok_or_else(|| io::Error::other("parallel apply byte counter overflow"))?;
        Ok(())
    }

    /// Returns the bytes-written counter.
    pub(super) fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns true if every submitted chunk for this file has hit the writer.
    pub(super) fn drained(&self) -> bool {
        self.reorder.is_empty()
    }
}
