//! Batched parallel apply path for the receive-side delta applier (SPL-38.d).
//!
//! Extracted from `parallel_apply/mod.rs` as part of the SPL-38 module
//! decomposition. Sibling to [`super::slot_barrier::SlotBarrier`] and
//! [`super::decrement_guard::DecrementGuard`]; reuses both via the per-slot
//! handle returned by [`ParallelDeltaApplier::slot_for`].
//!
//! # Contract
//!
//! [`ParallelDeltaApplier::apply_batch_parallel`] runs the strong-checksum
//! verify step for every chunk in parallel via rayon's `into_par_iter`,
//! bounded by [`ParallelDeltaApplier::concurrency`]. Once the rayon
//! `collect` returns the full `Vec<VerifiedChunk>` (or short-circuits on
//! the first [`ParallelApplyError::ChecksumMismatch`]), the per-file write
//! step runs serially on the calling thread: for each verified chunk the
//! applier acquires the per-slot [`std::sync::Mutex`] via
//! [`super::SlotHandle::lock_slot`] and feeds the chunk through the
//! per-file [`super::FileSlot::ingest`] reorder buffer. The Mutex
//! preserves per-file write exclusivity and the reorder buffer preserves
//! per-file `chunk_sequence` order, mirroring the invariants documented
//! at the module root.
//!
//! Cross-chunk parallelism lives in the verify step; the write loop is
//! deliberately serial so the per-file byte stream stays deterministic.

use std::sync::Arc;

use rayon::prelude::*;

use super::{DeltaChunk, ParallelApplyError, ParallelDeltaApplier, VerifiedChunk};

impl ParallelDeltaApplier {
    /// Applies a batch of chunks, fanning the verify step across the rayon
    /// pool subject to [`Self::concurrency`]. Order-preserving per file.
    ///
    /// Chunks belonging to different files run independently; chunks for the
    /// same file are merged back through the per-file reorder buffer before
    /// they reach the destination writer.
    ///
    /// # Errors
    ///
    /// Returns the first [`std::io::Error`] encountered while applying the
    /// batch, including any per-chunk strong-checksum mismatch surfaced by
    /// [`Self::verify_chunk`].
    pub fn apply_batch_parallel(&self, chunks: Vec<DeltaChunk>) -> std::io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let total = chunks.len();
        let cap = if self.concurrency == 0 {
            total
        } else {
            self.concurrency.min(total)
        };
        let min_len = total.div_ceil(cap.max(1)).max(1);
        let strategy = Arc::clone(&self.strategy);
        let verified: Result<Vec<VerifiedChunk>, ParallelApplyError> = chunks
            .into_par_iter()
            .with_min_len(min_len)
            .map(|chunk| Self::verify_chunk(strategy.as_ref(), chunk))
            .collect();
        let verified = verified?;

        for v in verified {
            let ndx = v.chunk.ndx;
            let handle = self.slot_for(ndx)?;
            let mut slot = handle.lock_slot(ndx, "apply_batch_parallel")?;
            slot.ingest(v.chunk)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use checksums::strong::strategy::{
        ChecksumAlgorithmKind, ChecksumDigest, ChecksumStrategy, ChecksumStrategySelector,
    };

    use super::super::super::types::FileNdx;
    use super::super::tests::{VecSink, chunk_with_correct_digest, collect_file, sequential_apply};
    use super::super::{DeltaChunk, ParallelDeltaApplier};

    #[test]
    fn batch_apply_matches_sequential_byte_for_byte() {
        let applier = ParallelDeltaApplier::new(8);
        let (sink_a, buf_a) = VecSink::new();
        let (sink_b, buf_b) = VecSink::new();
        applier.register_file(0u32, Box::new(sink_a)).unwrap();
        applier.register_file(1u32, Box::new(sink_b)).unwrap();

        let mut chunks = Vec::new();
        for i in 0..24u64 {
            let payload: Vec<u8> = (0..16).map(|b| (i as u8).wrapping_add(b)).collect();
            chunks.push(DeltaChunk::literal(0u32, i, payload.clone()));
            chunks.push(DeltaChunk::matched(1u32, i, payload));
        }
        let expected_a = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(0))
                .cloned()
                .collect::<Vec<_>>(),
        );
        let expected_b = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(1))
                .cloned()
                .collect::<Vec<_>>(),
        );

        applier.apply_batch_parallel(chunks).unwrap();
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf_a), expected_a);
        assert_eq!(collect_file(&applier, FileNdx::new(1), buf_b), expected_b);
    }

    #[test]
    fn verify_batch_rejects_mismatched_digest() {
        // BR-3i.c error path under the batch entry point. The rayon
        // parallel `collect` short-circuits on the first error, surfacing
        // the typed `ChecksumMismatch` instead of any successful write.
        let applier = ParallelDeltaApplier::new(4);
        let strategy = Arc::clone(applier.strategy());
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let good_a = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![1u8; 32]);
        let bad = DeltaChunk::literal(0u32, 1, vec![2u8; 32])
            .with_expected_strong(ChecksumDigest::new(&[0u8; 16]));
        let good_b = chunk_with_correct_digest(strategy.as_ref(), 0, 2, vec![3u8; 32]);

        let err = applier
            .apply_batch_parallel(vec![good_a, bad, good_b])
            .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn parallel_apply_with_real_digests_matches_sequential_byte_for_byte() {
        // BR-3i.e regression test: parallel apply with real per-chunk
        // strong-checksum verification produces the same destination byte
        // stream as the sequential reference path. Guards against future
        // regressions where the verify path mutates `chunk.data` or
        // reorders writes when the strategy short-circuits.
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0),
        );
        let applier = ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy));
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..32u64)
            .map(|i| {
                let payload: Vec<u8> = (0..64u8).map(|b| b.wrapping_add(i as u8)).collect();
                chunk_with_correct_digest(strategy.as_ref(), 0, i, payload)
            })
            .collect();
        let expected = sequential_apply(&chunks);

        // Deterministic non-trivial permutation: rotate by 5 so workers
        // see chunks out of submission order; the reorder buffer must
        // still emit them in `chunk_sequence` order.
        let mut shuffled = chunks;
        shuffled.rotate_left(5);
        applier.apply_batch_parallel(shuffled).unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
        assert_eq!(buf.lock().unwrap().clone(), expected);
    }
}
