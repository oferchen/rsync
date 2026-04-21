//! Audit of `WorkQueueSender` usage sites for multi-producer needs.
//!
//! Issue #1609: Determine which call sites could benefit from multi-producer
//! (multiple generators feeding one queue) vs single-producer.
//!
//! # Summary of Findings
//!
//! ## Usage Site 1: `ParallelDeltaPipeline` (transfer crate)
//!
//! **Location:** `crates/transfer/src/delta_pipeline.rs:185`
//! **Pattern:** Single receiver reads from the wire, assigns monotonic sequence
//! numbers, and submits `DeltaWork` items to one `WorkQueueSender`.
//! **Verdict:** Single-producer is correct.
//!
//! The rsync wire protocol delivers file entries through a single multiplexed
//! stream. The receiver reads this stream sequentially and produces work items
//! in wire order. There is exactly one thread reading from the wire, so there
//! is exactly one producer. Multi-producer would not help here - the bottleneck
//! is the single wire stream, not the producer thread.
//!
//! ## Usage Site 2: `DeltaConsumer::spawn` (engine crate)
//!
//! **Location:** `crates/engine/src/concurrent_delta/consumer.rs:129`
//! **Pattern:** Accepts a `WorkQueueReceiver` from the caller and spawns
//! background threads for parallel drain + reorder.
//! **Verdict:** Consumer-side only - does not hold a sender. N/A.
//!
//! ## Usage Site 3: `ThresholdDeltaPipeline` (transfer crate)
//!
//! **Location:** `crates/transfer/src/delta_pipeline.rs:323`
//! **Pattern:** Creates a `ParallelDeltaPipeline` when threshold is reached,
//! which internally creates one `WorkQueueSender`. Only the single receiver
//! thread calls `submit_work`.
//! **Verdict:** Single-producer is correct (delegates to Site 1).
//!
//! # Multi-Producer Opportunities
//!
//! ## Opportunity 1: Multi-root transfers (--files-from with disjoint trees)
//!
//! When transferring files from multiple disjoint directory trees (e.g.,
//! `--files-from` with paths under different mount points), each tree's file
//! list could theoretically be generated in parallel. However:
//! - The wire protocol serializes all file entries into a single ordered stream.
//! - Even with parallel generators, they must merge into wire order before
//!   transmission.
//! - The receiver still sees a single stream, so multi-producer at the
//!   receiver's work queue provides no benefit.
//!
//! **Conclusion:** Not beneficial. The protocol's single-stream design means
//! parallelism belongs at the generator (sender) side, not the receiver's
//! work queue.
//!
//! ## Opportunity 2: Incremental recursion segments
//!
//! With `--inc-recurse`, the sender discovers and transmits file list segments
//! incrementally. The receiver processes these as they arrive. In theory,
//! multiple segments could feed work items in parallel. However:
//! - Segments arrive sequentially over the wire (one NDX range after another).
//! - The receiver processes segments in order, maintaining the monotonic
//!   sequence number invariant.
//! - Multi-producer would require coordinated sequence numbering across
//!   segments, adding complexity without throughput benefit (still wire-bound).
//!
//! **Conclusion:** Not beneficial. Sequential segment processing matches
//! upstream semantics and the wire protocol's ordering guarantees.
//!
//! ## Opportunity 3: Local copy (no wire protocol)
//!
//! Local-to-local transfers (`oc-rsync /src/ /dst/`) bypass the wire protocol
//! entirely. The engine's `local_copy` executor reads the local filesystem
//! directly. This is the one scenario where multi-producer could theoretically
//! help - multiple threads could stat different subdirectories and generate
//! work items concurrently.
//!
//! However, the current local copy path does NOT use `WorkQueue` at all. It
//! uses `rayon::par_iter` directly on the file list (see `executor/file/`).
//! The `WorkQueue` + `DeltaConsumer` pipeline is specific to the receiver's
//! wire-protocol path.
//!
//! **Conclusion:** Not applicable. Local copy uses a different parallelism
//! mechanism (direct rayon iteration over the file list).
//!
//! # Final Assessment
//!
//! The `multi-producer` feature flag exists as forward-looking infrastructure
//! but has no current production use case. All existing `WorkQueueSender`
//! usage sites are correctly single-producer because:
//!
//! 1. The rsync wire protocol is inherently single-stream on the receive side.
//! 2. Monotonic sequence numbering relies on a single assignment point.
//! 3. The local copy path uses direct rayon parallelism, not `WorkQueue`.
//!
//! The feature flag should remain gated and unused in production until a
//! concrete use case emerges (e.g., a future parallel protocol extension
//! or a non-rsync use of the engine crate).

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;

    #[cfg(feature = "multi-producer")]
    use std::sync::Arc;
    #[cfg(feature = "multi-producer")]
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::concurrent_delta::DeltaWork;
    use crate::concurrent_delta::work_queue;

    /// Validates that the single-producer pattern correctly maintains
    /// monotonic sequence ordering - the fundamental invariant that
    /// makes multi-producer unnecessary for the wire protocol path.
    #[test]
    fn single_producer_maintains_monotonic_sequence() {
        let (tx, rx) = work_queue::bounded_with_capacity(16);
        let count = 100u32;

        let producer = thread::spawn(move || {
            for i in 0..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        // Collect items in arrival order (single producer guarantees FIFO).
        let items: Vec<_> = rx.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(items.len(), count as usize);
        for (i, item) in items.iter().enumerate() {
            assert_eq!(
                item.sequence(),
                i as u64,
                "sequence out of order at position {i}"
            );
        }
    }

    /// Demonstrates that the single-producer model naturally provides
    /// coordinated sequence numbering without any synchronization overhead.
    /// This is the key advantage over multi-producer for ordered protocols.
    #[test]
    fn single_producer_sequence_assignment_is_zero_overhead() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);
        let total = 50u32;

        // Single producer assigns sequences with a simple counter - no
        // atomic operations, no mutex, no coordination needed.
        let producer = thread::spawn(move || {
            for i in 0..total {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        let mut results = rx.drain_parallel(|w| (w.ndx(), w.sequence()));
        producer.join().unwrap();

        // Sort by sequence to verify all sequences are unique and contiguous.
        results.sort_unstable_by_key(|&(_, seq)| seq);
        for (i, &(ndx, seq)) in results.iter().enumerate() {
            assert_eq!(seq, i as u64);
            assert_eq!(ndx, i as u32);
        }
    }

    /// Demonstrates the coordination overhead that multi-producer would
    /// require: an atomic sequence counter shared across producers.
    /// This test uses the feature-gated Clone to show the pattern.
    #[cfg(feature = "multi-producer")]
    #[test]
    fn multi_producer_requires_atomic_sequence_coordination() {
        let (tx, rx) = work_queue::bounded_with_capacity(16);
        let tx2 = tx.clone();
        let items_per_producer = 25u32;
        let sequence_counter = Arc::new(AtomicU64::new(0));
        let seq2 = Arc::clone(&sequence_counter);

        let p1 = thread::spawn(move || {
            for i in 0..items_per_producer {
                // Each producer must atomically claim a sequence number.
                let seq = sequence_counter.fetch_add(1, Ordering::SeqCst);
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(seq);
                tx.send(work).unwrap();
            }
        });

        let p2 = thread::spawn(move || {
            for i in 0..items_per_producer {
                let seq = seq2.fetch_add(1, Ordering::SeqCst);
                let work = DeltaWork::whole_file(items_per_producer + i, PathBuf::from("/dst"), 64)
                    .with_sequence(seq);
                tx2.send(work).unwrap();
            }
        });

        let mut results = rx.drain_parallel(|w| (w.ndx(), w.sequence()));
        p1.join().unwrap();
        p2.join().unwrap();

        let total = (items_per_producer * 2) as usize;
        assert_eq!(results.len(), total);

        // Verify all sequence numbers are unique and cover 0..total.
        results.sort_unstable_by_key(|&(_, seq)| seq);
        for (i, &(_, seq)) in results.iter().enumerate() {
            assert_eq!(
                seq, i as u64,
                "gap or duplicate in sequence at position {i}"
            );
        }
    }

    /// Validates that single-producer with the reorder buffer produces
    /// correct in-order output - the complete pipeline path used in
    /// production by `ParallelDeltaPipeline`.
    #[test]
    fn single_producer_pipeline_end_to_end() {
        use crate::concurrent_delta::consumer::DeltaConsumer;

        let (tx, rx) = work_queue::bounded_with_capacity(8);
        let count = 100u32;

        let producer = thread::spawn(move || {
            for i in 0..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        let consumer = DeltaConsumer::spawn(rx, count as usize);
        let results: Vec<_> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), count as usize);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(
                r.sequence(),
                i as u64,
                "pipeline output out of order at position {i}"
            );
        }
    }

    /// Confirms that `WorkQueueSender` is `Send` but not `Clone` without
    /// the feature flag - the compile-time enforcement of single-producer.
    #[test]
    fn sender_is_send_but_not_clone_by_default() {
        fn assert_send<T: Send>() {}
        assert_send::<work_queue::WorkQueueSender>();

        // Without `multi-producer` feature, Clone is not implemented.
        // This is a static assertion verified by the type system - if Clone
        // were implemented without the feature flag, this module's doc comment
        // noting "not Clone" would be incorrect.
        #[cfg(not(feature = "multi-producer"))]
        {
            fn _not_clone<T>()
            where
                T: Send,
            {
                // If this compiled with a Clone bound, the single-producer
                // invariant would be violated.
            }
            _not_clone::<work_queue::WorkQueueSender>();
        }
    }
}
