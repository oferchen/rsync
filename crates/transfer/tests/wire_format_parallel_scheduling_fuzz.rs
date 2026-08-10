//! Wire-format determinism fuzz test for the parallel delta dispatch path.
//!
//! Goal (#1649): catch any non-determinism introduced by the parallel dispatch
//! path. The receiver-facing contract for `ParallelDeltaPipeline` is that the
//! ordered `Vec<DeltaResult>` returned by `flush()` must be byte-identical to
//! the `SequentialDeltaPipeline` baseline for the same input batch, regardless
//! of:
//!
//! - the rayon worker count chosen at pipeline construction
//! - the bounded work-queue capacity multiplier (default 2x via
//!   `ParallelDeltaPipeline::new`, 4x/8x via `new_adaptive`)
//! - the per-item completion order produced by rayon's work-stealing scheduler
//!
//! The audit `docs/audits/parallel-dispatch-wire-format-verification.md`
//! identifies G2 ("no golden test compares sequential vs parallel byte
//! streams") as an open gap. This test closes G2 at the result-vector layer:
//! the receiver consumes `Vec<DeltaResult>` strictly in submission order, so
//! when those vectors serialize identically the downstream wire emissions in
//! `receiver.c:recv_files()` must also match upstream byte-for-byte.
//!
//! # Approach
//!
//! Property-based fuzz using `proptest`:
//!
//! 1. Generate a small input batch (1..=32 work items) with deterministic,
//!    seed-derived sizes and a mix of whole-file and pre-computed delta kinds.
//! 2. Run the batch through `SequentialDeltaPipeline` to get the baseline.
//! 3. Run the same batch through `ParallelDeltaPipeline::new(N)` for several
//!    `N` (1, 2, 4, 8). The constructor uses `N` to size the bounded work
//!    queue at `2 * N` slots (and the matching reorder-buffer window), so
//!    varying `N` cycles every backpressure regime: from queue-full on every
//!    submit (N = 1) to a queue large enough to hold the full batch with
//!    headroom (N = 8). The `new_adaptive` variant additionally selects
//!    2x/4x/8x based on the batch's average target size, exercising each
//!    branch of `transfer::delta_pipeline::adaptive_capacity`.
//! 4. Encode every result via `SpillCodec::encode` - the engine's
//!    deterministic, on-disk byte representation for `DeltaResult`. The encoded
//!    stream is the proxy for the wire stream the receiver would emit when
//!    folding these results into the multiplexed protocol output.
//! 5. Assert the byte streams are identical across all parallel runs and the
//!    sequential baseline.
//!
//! Actual worker concurrency comes from rayon's global pool; the runs share
//! that pool and therefore exercise whatever degree of parallelism the host
//! provides. CI matrix runners cover Linux, macOS, and Windows with 2-4
//! logical cores, which is sufficient to reorder small batches through
//! rayon's work-stealing scheduler.
//!
//! # Non-deterministic byte fields that are stripped
//!
//! None remain after constraining the input to whole-file and pre-computed
//! delta work items:
//!
//! - `DeltaWork::sequence` is producer-stamped by the pipeline implementation
//!   itself (`SequentialDeltaPipeline::submit_work` and
//!   `ParallelDeltaPipeline::submit_work` both increment a monotonic counter),
//!   so identical submission orders yield identical sequence numbers in both
//!   paths.
//! - `DeltaResult::ndx` is taken verbatim from the input `DeltaWork`.
//! - `DeltaResult::bytes_written`, `literal_bytes`, `matched_bytes` are pure
//!   functions of the input fields: `WholeFileStrategy` returns
//!   `(target_size, target_size, 0)`; pre-computed `DeltaTransferStrategy`
//!   returns `(target_size, literal_bytes, matched_bytes)` straight from the
//!   work item (see `crates/engine/src/concurrent_delta/strategy.rs:99-148`).
//! - `DeltaResult::status` is `Success` in every case because no I/O is
//!   performed for the chosen work-item shapes (no `delta_with_source`, no
//!   real basis files). The `NeedsRedo { reason }` and `Failed { reason }`
//!   variants would otherwise carry a non-deterministic error string derived
//!   from the underlying `io::Error::to_string()`; deliberately excluded.
//!
//! No wall-clock timestamps, random checksum seeds, or hostname/PID values
//! are read by either pipeline's hot path - both call only
//! `engine::concurrent_delta::strategy::dispatch`, which is a pure function of
//! the work item. This is verified by inspection of `strategy.rs:275-279`.
//!
//! # Cross-platform notes
//!
//! Uses only `std`, `proptest`, and the public APIs of the `engine` and
//! `transfer` crates. No filesystem I/O, no OS-specific syscalls, no
//! Unix-only paths. Compiles and runs identically on Linux, macOS, and
//! Windows.

use std::path::PathBuf;

use engine::concurrent_delta::{DeltaResult, DeltaWork, SpillCodec};
use proptest::collection::vec as prop_vec;
use proptest::prelude::*;
use transfer::delta_pipeline::{
    ParallelDeltaPipeline, ReceiverDeltaPipeline, SequentialDeltaPipeline,
};

/// Maximum batch size kept small so individual proptest cases stay fast even
/// under the largest worker count and adaptive capacity multiplier.
const MAX_BATCH: usize = 32;

/// Worker-count seeds passed to `ParallelDeltaPipeline::new(N)`. Each value
/// sizes the bounded work queue at `2 * N` slots, so the sweep covers the
/// queue-full-on-every-submit edge case (N = 1, capacity 2) through to a
/// queue large enough to swallow the entire batch without blocking
/// (N = 8, capacity 16 = MAX_BATCH / 2). Together with the `new_adaptive`
/// variant the sweep visits every reorder-window size the production code
/// can choose.
const WORKER_COUNTS: &[usize] = &[1, 2, 4, 8];

/// Generator describing one work item. NDX is assigned positionally after
/// the batch is generated so each spec carries only seed-derived data.
#[derive(Debug, Clone)]
struct WorkSpec {
    target_size: u64,
    literal_bytes: u64,
    matched_bytes: u64,
    is_delta: bool,
}

impl WorkSpec {
    /// Builds a `DeltaWork` matching this spec with the given NDX. Paths
    /// are derived from the NDX so the input is fully deterministic per
    /// seed-and-position.
    fn to_work(&self, ndx: u32) -> DeltaWork {
        if self.is_delta {
            DeltaWork::delta(
                ndx,
                PathBuf::from(format!("dest/{ndx}")),
                PathBuf::from(format!("basis/{ndx}")),
                self.target_size,
                self.literal_bytes,
                self.matched_bytes,
            )
        } else {
            DeltaWork::whole_file(ndx, PathBuf::from(format!("dest/{ndx}")), self.target_size)
        }
    }
}

/// Proptest strategy for a single `WorkSpec`. Bounds chosen to cover the
/// interesting input space without bloating shrinking time:
///
/// - `target_size` includes 0 (empty file) and crosses both the 64 KiB
///   small-file threshold and the 1 MiB large-file threshold used by
///   `adaptive_queue_depth`, so the property exercises every multiplier
///   branch in `transfer::delta_pipeline::adaptive_capacity`.
/// - `literal + matched` may exceed `target_size`; the strategies do not
///   validate the relationship and we want to exercise the raw byte
///   carry-through.
fn work_spec_strategy() -> impl Strategy<Value = WorkSpec> {
    (
        prop_oneof![Just(0u64), 1u64..32_768, 32_768u64..2_097_152],
        0u64..1_048_576,
        0u64..1_048_576,
        any::<bool>(),
    )
        .prop_map(
            |(target_size, literal_bytes, matched_bytes, is_delta)| WorkSpec {
                target_size,
                literal_bytes,
                matched_bytes,
                is_delta,
            },
        )
}

/// Strategy producing a batch of 1..=`MAX_BATCH` items. NDX values are
/// assigned positionally by the test bodies in submission order.
fn batch_strategy() -> impl Strategy<Value = Vec<WorkSpec>> {
    prop_vec(work_spec_strategy(), 1..=MAX_BATCH)
}

/// Runs the batch through the given pipeline and returns the ordered results.
fn run_pipeline(pipeline: Box<dyn ReceiverDeltaPipeline>, specs: &[WorkSpec]) -> Vec<DeltaResult> {
    let mut p = pipeline;
    for (i, spec) in specs.iter().enumerate() {
        p.submit_work(spec.to_work(i as u32))
            .expect("submit must succeed for non-shutdown pipeline");
    }
    p.flush()
}

/// Serialises a result vector into a single byte buffer via the engine's
/// deterministic `SpillCodec` encoding. This is the byte stream that would
/// flow through any cross-thread persistence layer for `DeltaResult` values,
/// and it captures every field the consumer would later fold into wire
/// emissions: NDX, sequence, byte counts, and status tag.
fn encode_results(results: &[DeltaResult]) -> Vec<u8> {
    let mut buf = Vec::new();
    // Length prefix so two encodings of different lengths cannot collide
    // even if the trailing items happen to align.
    buf.extend_from_slice(&(results.len() as u64).to_le_bytes());
    for r in results {
        r.encode(&mut buf)
            .expect("encoding into Vec<u8> cannot fail");
    }
    buf
}

proptest! {
    // Tighter case count than the default 256: each case spawns up to 8
    // background threads per worker-count variant, and CI runs this on every
    // platform. 64 cases give meaningful coverage without inflating test time.
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// The parallel dispatch path must produce a byte-identical encoded
    /// result stream to the sequential baseline for every worker count and
    /// every adaptive-capacity branch.
    #[test]
    fn parallel_dispatch_matches_sequential_baseline(specs in batch_strategy()) {
        // Baseline: sequential pipeline. By construction this matches
        // upstream rsync's `receiver.c:recv_files()` 1:1 dispatch order.
        let baseline = run_pipeline(Box::new(SequentialDeltaPipeline::new()), &specs);
        let baseline_bytes = encode_results(&baseline);

        // Sanity: every result must carry a valid NDX/sequence pairing.
        prop_assert_eq!(baseline.len(), specs.len(), "baseline length mismatch");
        for (i, r) in baseline.iter().enumerate() {
            prop_assert_eq!(r.sequence(), i as u64, "baseline sequence drift at {}", i);
            prop_assert_eq!(r.ndx().get(), i as u32, "baseline ndx drift at {}", i);
        }

        // Sweep worker counts. `new(N)` exercises the default 2x capacity
        // multiplier; the loop below also covers the adaptive variant that
        // selects 2x/4x/8x based on the batch's average target size.
        for &workers in WORKER_COUNTS {
            let parallel = run_pipeline(
                Box::new(ParallelDeltaPipeline::new(workers)),
                &specs,
            );
            let parallel_bytes = encode_results(&parallel);
            prop_assert_eq!(
                parallel_bytes.len(),
                baseline_bytes.len(),
                "encoded length diverged at workers={}", workers
            );
            prop_assert!(
                parallel_bytes == baseline_bytes,
                "byte stream diverged from sequential baseline at workers={}", workers
            );

            // Adaptive capacity selects a different queue depth based on the
            // batch's average target size; the choice must not leak into the
            // encoded byte stream.
            let avg_size = average_target_size(&specs);
            let adaptive = run_pipeline(
                Box::new(ParallelDeltaPipeline::new_adaptive(workers, avg_size)),
                &specs,
            );
            let adaptive_bytes = encode_results(&adaptive);
            prop_assert!(
                adaptive_bytes == baseline_bytes,
                "adaptive byte stream diverged from baseline at workers={}, avg_size={}",
                workers, avg_size
            );
        }
    }

    /// Re-running the parallel pipeline with the same input must yield the
    /// same byte stream every time. This catches any latent dependence on
    /// thread scheduling order, allocator state, or rayon worker affinity.
    #[test]
    fn parallel_dispatch_is_reproducible(specs in batch_strategy(), workers in 1usize..=8) {
        let first = encode_results(&run_pipeline(
            Box::new(ParallelDeltaPipeline::new(workers)),
            &specs,
        ));
        // Three repeats: gives rayon's work-stealing scheduler ample
        // opportunity to choose a different completion order while the
        // encoded output must stay constant.
        for run in 0..3 {
            let again = encode_results(&run_pipeline(
                Box::new(ParallelDeltaPipeline::new(workers)),
                &specs,
            ));
            prop_assert!(
                again == first,
                "parallel encoding diverged on repeat run {} at workers={}", run, workers
            );
        }
    }
}

/// Mirror of `transfer::delta_pipeline::average_target_size` for test inputs.
/// Kept in-test rather than exported because the pipeline's helper is private
/// and exporting it solely for testing would broaden the public API.
fn average_target_size(specs: &[WorkSpec]) -> u64 {
    if specs.is_empty() {
        return 0;
    }
    let total: u128 = specs
        .iter()
        .map(|s| u128::from(s.target_size))
        .fold(0u128, |a, b| a.saturating_add(b));
    let avg = total / specs.len() as u128;
    u64::try_from(avg).unwrap_or(u64::MAX)
}
