//! Integration coverage for adaptive DashMap shard sizing (DMC-CON.3, #3997).
//!
//! Runs the existing `concurrent_register_and_dispatch`-style smoke
//! workload twice: once under the default heuristic (no env var) and once
//! with `OC_RSYNC_DASHMAP_SHARDS=8` forced. Both passes must complete
//! cleanly with matching byte accounting, proving the new shard-count
//! plumbing does not perturb correctness of the parallel apply path.
//!
//! The env-touching pass uses a process-wide mutex so a second test in the
//! same binary cannot race the `set_var` / `remove_var` calls. The applier
//! reads the env variable in its constructor; tests build a fresh applier
//! inside the guarded scope, so the override only affects the applier this
//! test owns.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};
use rayon::prelude::*;

const NUM_FILES: u32 = 64;
const TOTAL_OPS: u32 = 1_024;
const WORKERS: usize = 4;
const CHUNK_BYTES: usize = 16;
const SHARDS_ENV: &str = "OC_RSYNC_DASHMAP_SHARDS";

/// Serialises env mutation so tests in this binary do not stomp on each
/// other's `OC_RSYNC_DASHMAP_SHARDS` reads. The applier reads the env var
/// once at construction time, so holding the lock across `new()` plus the
/// dispatch loop is enough.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

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

struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
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

/// Pre-registers `NUM_FILES` sinks, dispatches `TOTAL_OPS` chunks across
/// `WORKERS` rayon workers in monotonic per-file sequence order, then
/// finalises every file. Returns the aggregate byte count for the caller
/// to compare against the expected total.
fn drive_workload(applier: Arc<ParallelDeltaApplier>) -> u64 {
    let counters: Vec<Arc<AtomicU64>> = (0..NUM_FILES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    for (ndx, counter) in counters.iter().enumerate() {
        let sink = CountingSink::new(Arc::clone(counter));
        applier
            .register_file(ndx as u32, Box::new(sink))
            .expect("register_file");
    }

    let per_file_seq: Vec<Arc<AtomicU64>> = (0..NUM_FILES)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let ops_per_worker = (TOTAL_OPS as usize).div_ceil(WORKERS);
    (0..WORKERS).into_par_iter().for_each(|worker| {
        let mut rng = Xorshift::new(0xDC0C_0000 ^ worker as u64);
        let start = worker * ops_per_worker;
        let end = ((worker + 1) * ops_per_worker).min(TOTAL_OPS as usize);
        for _ in start..end {
            let ndx_raw = (rng.next_u64() % NUM_FILES as u64) as u32;
            let seq = per_file_seq[ndx_raw as usize].fetch_add(1, Ordering::SeqCst);
            let chunk = DeltaChunk::literal(ndx_raw, seq, vec![worker as u8; CHUNK_BYTES]);
            applier
                .apply_one_chunk(chunk)
                .expect("apply_one_chunk under shard-sizing test");
        }
    });

    for ndx in 0..NUM_FILES {
        let _writer = applier.finish_file(ndx).expect("finish_file");
    }

    counters.iter().map(|c| c.load(Ordering::Relaxed)).sum()
}

#[test]
fn dispatch_succeeds_under_default_shard_count() {
    // No env override -> the constructor goes through the adaptive
    // heuristic. Worker count = 4 -> 16 shards (per the spec table).
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: env access serialised via ENV_MUTEX.
    unsafe {
        std::env::remove_var(SHARDS_ENV);
    }
    let applier = Arc::new(ParallelDeltaApplier::new(WORKERS));
    let total = drive_workload(applier);
    assert_eq!(total, TOTAL_OPS as u64 * CHUNK_BYTES as u64);
}

#[test]
fn dispatch_succeeds_under_env_shard_override() {
    // OC_RSYNC_DASHMAP_SHARDS=8 -> the resolver clamps up to MIN_SHARDS
    // (4) at first parse and the value is then passed verbatim because
    // 8 itself is a power of two within [4, 1024]. The override path is
    // exercised end-to-end through the applier constructor.
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: env access serialised via ENV_MUTEX.
    unsafe {
        std::env::set_var(SHARDS_ENV, "8");
    }
    let applier = Arc::new(ParallelDeltaApplier::new(WORKERS));
    // Drop the env var before running the workload so a panic on the
    // workload side does not leak the override into a subsequent test.
    // The applier already captured its shard count at construction.
    // SAFETY: env access serialised via ENV_MUTEX.
    unsafe {
        std::env::remove_var(SHARDS_ENV);
    }
    let total = drive_workload(applier);
    assert_eq!(total, TOTAL_OPS as u64 * CHUNK_BYTES as u64);
}
