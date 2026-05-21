//! Concurrency stress test for [`ParallelDeltaApplier`] (BR-3j.e #2507).
//!
//! Exercises the DashMap-backed outer map under high-fanout load. N worker
//! threads each pick a random `FileNdx` from a pool of 1000 files, look up
//! (or open-and-register) the per-file slot via the applier's public API,
//! acquire the inner per-file `Mutex` briefly, append a small payload, and
//! release. Total ops are pre-computed so the test can assert byte counts
//! match exactly at finish.
//!
//! What this test guards against:
//!
//! * Outer-map lock contention re-entering the design: every worker calls
//!   `register_file` / `apply_one_chunk` concurrently on overlapping
//!   NDX values; the DashMap shard scheme is the only thing keeping
//!   register/lookup off a single central lock.
//! * Per-file slot corruption: the inner `Mutex<FileSlot>` is still the
//!   sole gate for `ingest`, so a mistake in either layer would surface as
//!   either a panic (poisoned slot) or a `bytes_written` mismatch.
//! * `slot_for` releasing its DashMap guard before the per-file mutex is
//!   taken: any guard held across the inner lock would deadlock under this
//!   load.

#![cfg(feature = "parallel-receive-delta")]

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};
use rayon::prelude::*;

/// Number of unique files in the pool. Sized to exercise DashMap shards.
const NUM_FILES: u32 = 1_000;
/// Total operations across all workers.
const TOTAL_OPS: u32 = 10_000;
/// Worker fan-out for the concurrent dispatch phase.
const WORKERS: usize = 8;
/// Bytes per chunk; small so the test runs quickly and the byte-counter
/// arithmetic stays trivial to verify.
const CHUNK_BYTES: usize = 16;

/// In-memory sink that mirrors the test sink in `parallel_apply.rs` so the
/// integration test does not depend on private fixtures.
struct CountingSink {
    written: Arc<AtomicU64>,
}

impl CountingSink {
    fn new(counter: Arc<AtomicU64>) -> Self {
        Self { written: counter }
    }
}

impl Write for CountingSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Deterministic xorshift64 RNG so a failure reproduces from the seed.
struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero absorbing state.
        Self(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn concurrent_files_under_dashmap_shards_match_expected_bytes() {
    // Pre-register every file so the workers exercise the shared-read
    // (lookup-only) path of `slot_for`, which is the hot path the
    // DashMap migration optimises. Per-file byte counters are kept in
    // parallel with the applier so we can compare without depending on
    // `bytes_written` itself.
    let applier = Arc::new(ParallelDeltaApplier::new(WORKERS));
    let counters: Vec<Arc<AtomicU64>> = (0..NUM_FILES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    for (ndx, counter) in counters.iter().enumerate() {
        let sink = CountingSink::new(Arc::clone(counter));
        applier
            .register_file(ndx as u32, Box::new(sink))
            .expect("register_file");
    }

    // Assign per-file submission sequence numbers so chunks for the same
    // file arrive in strict monotonic order across all workers (the
    // reorder buffer requires this).
    let per_file_seq: Vec<Arc<AtomicU64>> = (0..NUM_FILES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let expected_per_file: Vec<Arc<AtomicU64>> = (0..NUM_FILES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    // Worker dispatch via rayon. Each worker pulls its slice of TOTAL_OPS
    // and emits CHUNK_BYTES bytes against a random NDX.
    let ops_per_worker = (TOTAL_OPS as usize).div_ceil(WORKERS);
    let start = Instant::now();
    (0..WORKERS).into_par_iter().for_each(|worker| {
        let mut rng = Xorshift::new(0xA5A5_0000 ^ worker as u64);
        let start = worker * ops_per_worker;
        let end = ((worker + 1) * ops_per_worker).min(TOTAL_OPS as usize);
        for _ in start..end {
            let ndx_raw = (rng.next_u64() % NUM_FILES as u64) as u32;
            let seq = per_file_seq[ndx_raw as usize].fetch_add(1, Ordering::SeqCst);
            expected_per_file[ndx_raw as usize].fetch_add(CHUNK_BYTES as u64, Ordering::Relaxed);
            let chunk = DeltaChunk::literal(ndx_raw, seq, vec![worker as u8; CHUNK_BYTES]);
            applier
                .apply_one_chunk(chunk)
                .expect("apply_one_chunk under concurrent dispatch");
        }
    });
    let elapsed = start.elapsed();
    let ops_per_sec = (TOTAL_OPS as f64) / elapsed.as_secs_f64();
    eprintln!(
        "[parallel_apply_concurrent] {WORKERS} workers, {NUM_FILES} files, {TOTAL_OPS} ops: \
         elapsed={elapsed:?} ops/sec={ops_per_sec:.0}"
    );

    // Every file that received at least one op must report matching
    // bytes via the applier's own counter. Files with zero ops are
    // checked too so we catch any accidental cross-file writes.
    let total_expected: u64 = expected_per_file
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum();
    assert_eq!(
        total_expected,
        TOTAL_OPS as u64 * CHUNK_BYTES as u64,
        "test bookkeeping mismatch"
    );

    let mut total_actual = 0u64;
    for ndx in 0..NUM_FILES {
        let expected = expected_per_file[ndx as usize].load(Ordering::Relaxed);
        let actual = applier
            .bytes_written(ndx)
            .expect("bytes_written for registered ndx");
        assert_eq!(
            actual, expected,
            "byte mismatch for ndx={ndx}: actual={actual} expected={expected}"
        );
        total_actual += actual;
    }
    assert_eq!(total_actual, total_expected);

    // Finalise every file. Each finish drops the shard entry; if the
    // DashMap migration failed to release its guard before the
    // `Arc::try_unwrap` step this loop would error with
    // `ApplierStillReferenced`.
    for ndx in 0..NUM_FILES {
        let _writer = applier
            .finish_file(ndx)
            .unwrap_or_else(|e| panic!("finish_file({ndx}) failed: {e}"));
    }

    // The CountingSink fan-in must equal the per-file expected sum.
    let total_sink: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    assert_eq!(total_sink, total_expected);
}

#[test]
fn concurrent_register_and_dispatch_on_overlapping_files() {
    // Variant that mixes register, dispatch, and finish on the same NDX
    // pool. Each NDX is registered exactly once (via a leader thread)
    // while dispatcher threads race to land chunks. The applier must
    // either succeed (writer registered before the chunk lands) or
    // return the typed "unknown" io::Error - never panic or corrupt.
    const SMALL_NUM: u32 = 256;
    const SMALL_OPS: u32 = 4_000;

    let applier = Arc::new(ParallelDeltaApplier::new(WORKERS));
    let registered: Vec<Arc<AtomicU64>> = (0..SMALL_NUM)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let dropped = Arc::new(AtomicU64::new(0));
    let expected = Arc::new(AtomicU64::new(0));

    // Dedicated registrar: registers all NDX values up front (the
    // dispatchers may race with the registration of LATER ndx values).
    let registrar_applier = Arc::clone(&applier);
    let registered_arcs: Vec<Arc<AtomicU64>> = registered.iter().map(Arc::clone).collect();
    let registrar = std::thread::spawn(move || {
        for ndx in 0..SMALL_NUM {
            let counter = Arc::clone(&registered_arcs[ndx as usize]);
            let sink = CountingSink::new(counter);
            registrar_applier
                .register_file(ndx, Box::new(sink))
                .expect("register_file");
            // Small yield so dispatchers can race with later registrations.
            std::thread::yield_now();
        }
    });

    let per_file_seq: Vec<Arc<AtomicU64>> = (0..SMALL_NUM)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let ops_per_worker = (SMALL_OPS as usize).div_ceil(WORKERS);
    (0..WORKERS).into_par_iter().for_each(|worker| {
        let mut rng = Xorshift::new(0xC3C3_0000 ^ worker as u64);
        for _ in 0..ops_per_worker {
            let ndx_raw = (rng.next_u64() % SMALL_NUM as u64) as u32;
            let seq = per_file_seq[ndx_raw as usize].fetch_add(1, Ordering::SeqCst);
            let chunk = DeltaChunk::literal(ndx_raw, seq, vec![worker as u8; CHUNK_BYTES]);
            match applier.apply_one_chunk(chunk) {
                Ok(()) => {
                    expected.fetch_add(CHUNK_BYTES as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    // Only the "unknown" race is tolerated - any other
                    // error means the migration corrupted slot state.
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unknown"),
                        "unexpected applier error under race: {msg}"
                    );
                    // Roll the per-file sequence back so the registrar's
                    // first-chunk seq starts at the right number once
                    // the file lands. We cannot fix-up `per_file_seq`
                    // because other workers may have already incremented
                    // past us; instead, track the drop and skip the file
                    // in the final byte check.
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    registrar.join().expect("registrar thread");

    // Finish every registered file. Any file that hit a sequence gap
    // due to a dropped chunk will surface as `UndrainedChunks`; that
    // is acceptable for this stress shape - we only require that the
    // applier remain consistent (no panics, typed errors only).
    for ndx in 0..SMALL_NUM {
        match applier.finish_file(ndx) {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("still buffered") || msg.contains("unknown"),
                    "unexpected finish_file error: {msg}"
                );
            }
        }
    }
    // Sanity: we observed at least one successful op.
    assert!(expected.load(Ordering::Relaxed) > 0);
}
