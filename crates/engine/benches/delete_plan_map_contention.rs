//! Criterion micro-benchmark: backing-store alternatives for `DeletePlanMap`.
//!
//! # Why this exists
//!
//! `crates/engine/src/delete/plan_map.rs::DeletePlanMap` currently wraps a
//! single `Mutex<HashMap<PathBuf, DeletePlan>>`. The choice is explicitly
//! noted as a first cut (task DDP-A3, #2253) pending bench-driven evidence.
//! This file is the DDP-B4 (#2258) evidence: it pits the production layout
//! against two alternatives the team has flagged as candidates.
//!
//! # Cross-references
//!
//! - #2253 (DDP-A3) - chooses the long-term backing store. If any
//!   alternative below shows a >2x speedup at high thread counts (8, 16)
//!   on the insert-heavy or mixed workloads, file a follow-up PR swapping
//!   `plan_map.rs` to the winner. Otherwise the existing `Mutex<HashMap>`
//!   stays as the simplest correct shape.
//! - `crates/engine/benches/drain_parallel_alternatives.rs` - sibling
//!   bench that follows the same three-way "current vs dashmap vs shards"
//!   layout for the WorkQueue drain path.
//!
//! # What it measures
//!
//! Three concurrent-map implementations, all behind the same minimal
//! `PlanStore` trait so the only variable is the backing data structure:
//!
//! 1. `mutex_hashmap` - `Mutex<HashMap<PathBuf, DeletePlan>>`. This is
//!    what `DeletePlanMap` does today; it is the baseline.
//! 2. `dashmap` - `dashmap::DashMap<PathBuf, DeletePlan>`. Drop-in,
//!    lock-free reads, internally sharded.
//! 3. `sharded_mutex_hashmap` - `Vec<Mutex<HashMap<PathBuf, DeletePlan>>>`
//!    with 16 shards keyed by `hash(path) % 16`. Manual sharding with the
//!    standard library only, no extra dependency.
//!
//! # Workloads (Criterion group `delete_plan_map_contention`)
//!
//! - `100k_inserts_single_thread` - serial baseline, no contention.
//! - `100k_inserts_4_threads`     - rayon scope, disjoint key ranges.
//! - `100k_inserts_8_threads`     - rayon scope, disjoint key ranges.
//! - `100k_inserts_16_threads`    - rayon scope, disjoint key ranges.
//! - `mixed_50_50_insert_take`    - 8 threads, half insert, half take.
//!
//! Keys are deterministic `PathBuf::from(format!("dir/{group}/{n}"))`
//! values so runs are reproducible across machines.
//!
//! Pre-allocated `DeletePlan` values live outside the timed section so the
//! benchmark only captures map ops, not plan construction.
//!
//! Run: `cargo bench -p engine --bench delete_plan_map_contention`

#![deny(unsafe_code)]
#![cfg(unix)]

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dashmap::DashMap;
use rayon::ThreadPoolBuilder;

use engine::delete::{DeleteEntry, DeleteEntryKind, DeletePlan};

/// Total insert ops per benchmark iteration.
const TOTAL_OPS: usize = 100_000;

/// Number of shards in the `sharded_mutex_hashmap` strategy.
///
/// 16 matches the rayon worker upper bound used elsewhere in this crate's
/// benches and keeps the shard count comfortably above the largest thread
/// sweep below (16), so each thread can usually land on its own shard
/// under uniform hashing.
const SHARD_COUNT: usize = 16;

/// Thread counts swept for the contended insert workload.
const THREAD_COUNTS: &[usize] = &[4, 8, 16];

/// Minimal contract every candidate must satisfy.
///
/// All three implementations agree on `insert(plan)` returning the
/// previously published value (matching `DeletePlanMap::insert`) and
/// `take(dir)` returning the removed value (matching
/// `DeletePlanMap::take`). That is all the bench exercises, which keeps
/// the trait surface honest.
trait PlanStore: Send + Sync {
    fn insert(&self, plan: DeletePlan) -> Option<DeletePlan>;
    fn take(&self, dir: &Path) -> Option<DeletePlan>;
}

/// Baseline: single global `Mutex<HashMap>`. Mirrors the current
/// `DeletePlanMap` backing store exactly.
struct MutexHashMapStore {
    inner: Mutex<HashMap<PathBuf, DeletePlan>>,
}

impl MutexHashMapStore {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity)),
        }
    }
}

impl PlanStore for MutexHashMapStore {
    fn insert(&self, plan: DeletePlan) -> Option<DeletePlan> {
        let key = plan.directory.clone();
        self.inner
            .lock()
            .expect("MutexHashMapStore mutex poisoned")
            .insert(key, plan)
    }

    fn take(&self, dir: &Path) -> Option<DeletePlan> {
        self.inner
            .lock()
            .expect("MutexHashMapStore mutex poisoned")
            .remove(dir)
    }
}

/// Drop-in candidate: `dashmap::DashMap`. Internally sharded with
/// lock-free reads; pinned to the workspace version (6.1).
struct DashMapStore {
    inner: DashMap<PathBuf, DeletePlan>,
}

impl DashMapStore {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: DashMap::with_capacity(capacity),
        }
    }
}

impl PlanStore for DashMapStore {
    fn insert(&self, plan: DeletePlan) -> Option<DeletePlan> {
        let key = plan.directory.clone();
        self.inner.insert(key, plan)
    }

    fn take(&self, dir: &Path) -> Option<DeletePlan> {
        self.inner.remove(dir).map(|(_, plan)| plan)
    }
}

/// Manual sharded candidate: `Vec<Mutex<HashMap>>` keyed by
/// `hash(path) % SHARD_COUNT`. No extra dependency; hash is the
/// standard-library `RandomState` so two equal `PathBuf`s land in the
/// same shard.
struct ShardedMutexStore {
    shards: Vec<Mutex<HashMap<PathBuf, DeletePlan>>>,
    hasher: RandomState,
}

impl ShardedMutexStore {
    fn with_capacity(capacity: usize) -> Self {
        let per_shard = capacity.div_ceil(SHARD_COUNT);
        let mut shards = Vec::with_capacity(SHARD_COUNT);
        for _ in 0..SHARD_COUNT {
            shards.push(Mutex::new(HashMap::with_capacity(per_shard)));
        }
        Self {
            shards,
            hasher: RandomState::new(),
        }
    }

    fn shard_for(&self, dir: &Path) -> usize {
        let mut h = self.hasher.build_hasher();
        std::hash::Hash::hash(dir, &mut h);
        (h.finish() as usize) % SHARD_COUNT
    }
}

impl PlanStore for ShardedMutexStore {
    fn insert(&self, plan: DeletePlan) -> Option<DeletePlan> {
        let idx = self.shard_for(&plan.directory);
        let key = plan.directory.clone();
        self.shards[idx]
            .lock()
            .expect("ShardedMutexStore mutex poisoned")
            .insert(key, plan)
    }

    fn take(&self, dir: &Path) -> Option<DeletePlan> {
        let idx = self.shard_for(dir);
        self.shards[idx]
            .lock()
            .expect("ShardedMutexStore mutex poisoned")
            .remove(dir)
    }
}

/// Deterministic key for slot `n` under `group`. Reproducible across runs
/// and machines so bench numbers compose.
fn key_for(group: &str, n: usize) -> PathBuf {
    PathBuf::from(format!("dir/{group}/{n}"))
}

/// Builds a `DeletePlan` for `(group, n)` with one synthetic file entry.
/// Construction stays outside the timed section.
fn make_plan(group: &str, n: usize) -> DeletePlan {
    let mut plan = DeletePlan::new(key_for(group, n));
    plan.push(DeleteEntry::new(
        std::ffi::OsString::from(format!("entry-{n}")),
        DeleteEntryKind::File,
    ));
    plan
}

/// Pre-builds `count` plans under `group` so the timed loop does no
/// allocation beyond what the map itself drives.
fn build_plans(group: &str, count: usize) -> Vec<DeletePlan> {
    (0..count).map(|n| make_plan(group, n)).collect()
}

/// Pre-builds the key list for the take side of the mixed workload.
fn build_keys(group: &str, count: usize) -> Vec<PathBuf> {
    (0..count).map(|n| key_for(group, n)).collect()
}

/// Strategy identifier used in Criterion benchmark ids.
const STRATEGIES: &[&str] = &["mutex_hashmap", "dashmap", "sharded_mutex_hashmap"];

/// Builds an empty store of the requested strategy with the given capacity.
fn build_store(strategy: &str, capacity: usize) -> Arc<dyn PlanStore> {
    match strategy {
        "mutex_hashmap" => Arc::new(MutexHashMapStore::with_capacity(capacity)),
        "dashmap" => Arc::new(DashMapStore::with_capacity(capacity)),
        "sharded_mutex_hashmap" => Arc::new(ShardedMutexStore::with_capacity(capacity)),
        other => panic!("unknown strategy: {other}"),
    }
}

/// Pre-fills `store` with `plans.len()` clones so the mixed workload's
/// take side has something to drain.
fn prefill(store: &Arc<dyn PlanStore>, plans: &[DeletePlan]) {
    for plan in plans {
        let _ = store.insert(plan.clone());
    }
}

/// Single-thread insert baseline at `TOTAL_OPS` keys.
fn bench_single_thread_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_plan_map_contention");
    group.throughput(Throughput::Elements(TOTAL_OPS as u64));

    for &strategy in STRATEGIES {
        let id = BenchmarkId::new("100k_inserts_single_thread", strategy);
        group.bench_function(id, |b| {
            let plans = build_plans(strategy, TOTAL_OPS);
            b.iter_batched(
                || (build_store(strategy, TOTAL_OPS), plans.clone()),
                |(store, plans)| {
                    for plan in plans {
                        let _ = store.insert(plan);
                    }
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

/// Multi-thread insert sweep at 4, 8, 16 threads with disjoint key ranges.
///
/// Each rayon worker owns a contiguous slice of pre-built plans, so the
/// only contention is on the map itself. Total work stays at `TOTAL_OPS`
/// across thread counts so the per-strategy numbers compare directly.
fn bench_multi_thread_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_plan_map_contention");
    group.throughput(Throughput::Elements(TOTAL_OPS as u64));

    for &threads in THREAD_COUNTS {
        for &strategy in STRATEGIES {
            let id = BenchmarkId::new(format!("100k_inserts_{threads}_threads"), strategy);
            group.bench_function(id, |b| {
                let pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("failed to build rayon pool");
                let plans = build_plans(strategy, TOTAL_OPS);
                let chunk = TOTAL_OPS.div_ceil(threads);

                b.iter_batched(
                    || (build_store(strategy, TOTAL_OPS), plans.clone()),
                    |(store, plans)| {
                        pool.scope(|s| {
                            for chunk_plans in plans.chunks(chunk) {
                                let store = Arc::clone(&store);
                                let chunk_plans = chunk_plans.to_vec();
                                s.spawn(move |_| {
                                    for plan in chunk_plans {
                                        let _ = store.insert(plan);
                                    }
                                });
                            }
                        });
                    },
                    criterion::BatchSize::LargeInput,
                );
            });
        }
    }

    group.finish();
}

/// Mixed 50/50 insert/take at 8 threads: half the threads insert into a
/// disjoint upper-half key range while the other half drains the
/// pre-populated lower-half range. Models the steady-state DDP pipeline
/// where producers and the emitter overlap.
fn bench_mixed_insert_take(c: &mut Criterion) {
    const THREADS: usize = 8;
    const HALF: usize = TOTAL_OPS / 2;

    let mut group = c.benchmark_group("delete_plan_map_contention");
    group.throughput(Throughput::Elements(TOTAL_OPS as u64));

    for &strategy in STRATEGIES {
        let id = BenchmarkId::new("mixed_50_50_insert_take", strategy);
        group.bench_function(id, |b| {
            let pool = ThreadPoolBuilder::new()
                .num_threads(THREADS)
                .build()
                .expect("failed to build rayon pool");
            // Lower-half: pre-filled, to be drained by take threads.
            let prefill_plans = build_plans("prefill", HALF);
            let take_keys = build_keys("prefill", HALF);
            // Upper-half: built once, cloned per iter, to be inserted.
            let insert_plans = build_plans("insert", HALF);

            let insert_workers = THREADS / 2;
            let take_workers = THREADS - insert_workers;
            let insert_chunk = HALF.div_ceil(insert_workers);
            let take_chunk = HALF.div_ceil(take_workers);

            b.iter_batched(
                || {
                    let store = build_store(strategy, TOTAL_OPS);
                    prefill(&store, &prefill_plans);
                    (store, insert_plans.clone(), take_keys.clone())
                },
                |(store, insert_plans, take_keys)| {
                    pool.scope(|s| {
                        for chunk_plans in insert_plans.chunks(insert_chunk) {
                            let store = Arc::clone(&store);
                            let chunk_plans = chunk_plans.to_vec();
                            s.spawn(move |_| {
                                for plan in chunk_plans {
                                    let _ = store.insert(plan);
                                }
                            });
                        }
                        for chunk_keys in take_keys.chunks(take_chunk) {
                            let store = Arc::clone(&store);
                            let chunk_keys = chunk_keys.to_vec();
                            s.spawn(move |_| {
                                for key in chunk_keys {
                                    let _ = store.take(&key);
                                }
                            });
                        }
                    });
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_single_thread_inserts,
    bench_multi_thread_inserts,
    bench_mixed_insert_take,
);
criterion_main!(benches);
