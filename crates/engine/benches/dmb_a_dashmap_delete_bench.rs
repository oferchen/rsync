//! Criterion bench: DMB.a unified delete-map scalability harness.
//!
//! # Why this exists
//!
//! `docs/design/dashmap-scalability-decision.md` (DMB.f) drives a go/no-go
//! decision on `dashmap::DashMap` for two production sites, and section 2.1
//! pins this file as the DMB.a harness that produces the raw numbers the
//! decision consumes (DMB.b sweep, DMB.c 100K comparison, DMB.d 1M
//! comparison, DMB.e shard tuning). Per that spec the harness provides:
//!
//! - A `MapStore` trait abstracting `DashMap` vs `Mutex<HashMap>` vs a
//!   sharded `Mutex<HashMap>` (section 2.1, and the three alternatives the
//!   decision weighs in section 3).
//! - Scale tiers 10K / 100K / 1M entries (section 2.1).
//! - A thread sweep of 1 / 2 / 4 / 8 / 16 / 32 / 64 workers (section 2.1).
//! - Throughput (ops/sec) via criterion; latency percentiles fall out of
//!   criterion's sample distribution under `target/criterion/`. The
//!   OS-level axes DMB.a also lists - VmHWM memory overhead (section 6)
//!   and `perf lock` contention events (section 10.1, approach 3) - are
//!   not values a criterion routine can emit; they are captured by the
//!   external nightly wrapper that runs this same bench binary under
//!   `perf lock record` / RSS sampling. The harness deliberately keeps the
//!   timed region to pure map operations so those external captures
//!   attribute cleanly.
//!
//! # The two production sites (design doc section 4)
//!
//! The harness mirrors each site's *real* access pattern rather than a
//! synthetic strawman:
//!
//! - **Target A - `DeletePlanMap`** (`crates/engine/src/delete/plan_map.rs`,
//!   today a `Mutex<HashMap<PathBuf, DeletePlan>>`). Asymmetric
//!   producer/consumer (section 4.1): N rayon workers insert `DeletePlan`
//!   values keyed by sequential directory paths, then a single emitter
//!   drains via `take()`; the phases overlap. Modelled by two workload
//!   shapes - `insert_heavy` (phase-1 producers, disjoint keys) and
//!   `mixed_insert_take` (steady-state producer/consumer overlap).
//! - **Target B - `ParallelDeltaApplier`**
//!   (`crates/engine/src/concurrent_delta/parallel_apply/`, migrated to
//!   `DashMap<FileNdx, SlotEntry>` in BR-3j). Symmetric register / lookup /
//!   finish (section 4.2): every worker inserts a slot, looks it up, then
//!   removes it, all interleaved. Modelled by the
//!   `register_lookup_finish` shape keyed by sequential `FileNdx` values.
//!   The slot value is stood in for by a `u64` payload - the decision axis
//!   is the map operation cost (hash + shard select + lock + bucket op),
//!   not the slot's own contents.
//!
//! Both sites are keyed with sequential keys because that is what the
//! production code produces (directory traversal order for Target A,
//! monotonically increasing NDX for Target B), matching the hash-shard
//! distribution the decision doc reasons about in sections 4.1 and 4.2.
//!
//! # Structure (single-responsibility, Strategy over variants)
//!
//! - `MapStore<K, V>` is the common contract; `MutexHashMapStore`,
//!   `DashMapStore`, and `ShardedMutexStore` are the three interchangeable
//!   strategies. `build_store` selects one by `MapVariant`.
//! - `Op<K, V>` and the `plan_*` functions are the workload generator:
//!   one generator per op-mix, each parameterised by thread count, driving
//!   every variant identically. No per-variant workload duplication.
//! - `run_plan` is the single executor for every (variant, shape, tier,
//!   threads) cell.
//!
//! # Reproducibility
//!
//! All inputs are derived from the entry index, never from the clock or an
//! unseeded RNG: Target A keys are `dir/<shard>/<index>` paths, Target B
//! keys are `FileNdx(index)`, and both value payloads are index-derived.
//! Runs are therefore byte-identical across machines.
//!
//! # Cross-references
//!
//! - `docs/design/dashmap-scalability-decision.md` - DMB.f decision doc.
//! - `docs/design/dmb-a-dashmap-delete-bench-harness.md` - harness design.
//! - `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4
//!   predecessor micro-bench (Target A only, 100K only).
//! - `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` -
//!   BR-3j.f applier re-bench (Target B, DashMap only).
//!
//! Run: `cargo bench -p engine --bench dmb_a_dashmap_delete_bench`
//! Smoke: `cargo bench -p engine --bench dmb_a_dashmap_delete_bench -- --test`

#![deny(unsafe_code)]

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dashmap::DashMap;
use rayon::{ThreadPool, ThreadPoolBuilder};

use engine::concurrent_delta::FileNdx;
use engine::delete::{DeleteEntry, DeleteEntryKind, DeletePlan};

/// Scale tiers from DMB.a section 2.1: entry counts populated per cell.
const SCALE_TIERS: &[usize] = &[10_000, 100_000, 1_000_000];

/// Worker sweep from DMB.a section 2.1. 32 and 64 over-subscribe on modest
/// hosts on purpose - that is the regime where a single mutex serialises
/// and shard distribution is expected to pay off.
const THREAD_COUNTS: &[usize] = &[1, 2, 4, 8, 16, 32, 64];

/// Shard count for the manual `sharded_mutex_hashmap` alternative.
///
/// 128 mirrors the shard region the decision doc reasons about for the
/// applier's DashMap (section 4.2 / 6.1) so the std-only sharded baseline
/// is a like-for-like structural comparison rather than an artificially
/// coarse one.
const SHARD_COUNT: usize = 128;

/// The three interchangeable backing stores the decision weighs.
#[derive(Clone, Copy, Debug)]
enum MapVariant {
    /// Baseline: single global `Mutex<HashMap>` (Target A production shape).
    MutexHashMap,
    /// `dashmap::DashMap` (Target B production shape).
    DashMap,
    /// Manual `Vec<Mutex<HashMap>>` sharded by key hash, std-only.
    ShardedMutex,
}

impl MapVariant {
    /// Stable identifier used in criterion benchmark ids.
    fn label(self) -> &'static str {
        match self {
            MapVariant::MutexHashMap => "mutex_hashmap",
            MapVariant::DashMap => "dashmap",
            MapVariant::ShardedMutex => "sharded_mutex_hashmap",
        }
    }
}

/// Every variant swept in the harness.
const VARIANTS: &[MapVariant] = &[
    MapVariant::MutexHashMap,
    MapVariant::DashMap,
    MapVariant::ShardedMutex,
];

/// Minimal contract every backing store must satisfy.
///
/// The three operations map one-to-one onto both production sites:
/// `insert` is Target A publish / Target B register, `lookup` is the
/// applier slot read, and `remove` is Target A `take` / Target B finish.
/// `lookup` returns presence rather than a cloned value because neither
/// production reader clones the value out - the applier holds a shard
/// guard - so cloning here would tax the read path with cost the real code
/// never pays.
trait MapStore<K, V>: Send + Sync {
    fn insert(&self, key: K, value: V) -> Option<V>;
    fn lookup(&self, key: &K) -> bool;
    fn remove(&self, key: &K) -> Option<V>;
}

/// Baseline: single global `Mutex<HashMap>`, matching `DeletePlanMap`.
struct MutexHashMapStore<K, V> {
    inner: Mutex<HashMap<K, V>>,
}

impl<K: Eq + Hash + Send, V: Send> MapStore<K, V> for MutexHashMapStore<K, V> {
    fn insert(&self, key: K, value: V) -> Option<V> {
        self.inner
            .lock()
            .expect("mutex poisoned")
            .insert(key, value)
    }

    fn lookup(&self, key: &K) -> bool {
        self.inner.lock().expect("mutex poisoned").contains_key(key)
    }

    fn remove(&self, key: &K) -> Option<V> {
        self.inner.lock().expect("mutex poisoned").remove(key)
    }
}

/// `dashmap::DashMap`: internally sharded, lock-free reads.
struct DashMapStore<K, V> {
    inner: DashMap<K, V>,
}

impl<K: Eq + Hash + Clone + Send + Sync, V: Send + Sync> MapStore<K, V> for DashMapStore<K, V> {
    fn insert(&self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }

    fn lookup(&self, key: &K) -> bool {
        self.inner.get(key).is_some()
    }

    fn remove(&self, key: &K) -> Option<V> {
        self.inner.remove(key).map(|(_, v)| v)
    }
}

/// Manual sharded alternative: `Vec<Mutex<HashMap>>` keyed by
/// `hash(key) % SHARD_COUNT`, standard library only.
struct ShardedMutexStore<K, V> {
    shards: Vec<Mutex<HashMap<K, V>>>,
    hasher: RandomState,
}

impl<K: Eq + Hash + Send, V: Send> ShardedMutexStore<K, V> {
    fn shard_for(&self, key: &K) -> usize {
        (self.hasher.hash_one(key) as usize) % self.shards.len()
    }
}

impl<K: Eq + Hash + Send, V: Send> MapStore<K, V> for ShardedMutexStore<K, V> {
    fn insert(&self, key: K, value: V) -> Option<V> {
        let idx = self.shard_for(&key);
        self.shards[idx]
            .lock()
            .expect("mutex poisoned")
            .insert(key, value)
    }

    fn lookup(&self, key: &K) -> bool {
        let idx = self.shard_for(key);
        self.shards[idx]
            .lock()
            .expect("mutex poisoned")
            .contains_key(key)
    }

    fn remove(&self, key: &K) -> Option<V> {
        let idx = self.shard_for(key);
        self.shards[idx].lock().expect("mutex poisoned").remove(key)
    }
}

/// Builds an empty store of the requested variant, sized for `capacity`.
fn build_store<K, V>(variant: MapVariant, capacity: usize) -> Arc<dyn MapStore<K, V>>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    match variant {
        MapVariant::MutexHashMap => Arc::new(MutexHashMapStore {
            inner: Mutex::new(HashMap::with_capacity(capacity)),
        }),
        MapVariant::DashMap => Arc::new(DashMapStore {
            inner: DashMap::with_capacity(capacity),
        }),
        MapVariant::ShardedMutex => {
            let per_shard = capacity.div_ceil(SHARD_COUNT);
            let shards = (0..SHARD_COUNT)
                .map(|_| Mutex::new(HashMap::with_capacity(per_shard)))
                .collect();
            Arc::new(ShardedMutexStore {
                shards,
                hasher: RandomState::new(),
            })
        }
    }
}

/// One map operation the workload emits.
enum Op<K, V> {
    Insert(K, V),
    Lookup(K),
    Remove(K),
}

/// Splits `0..len` into `threads` contiguous, disjoint index ranges.
///
/// Contiguous ranges match how both production sites partition work: rayon
/// hands each worker a slice of the file list rather than interleaving.
fn split_ranges(len: usize, threads: usize) -> Vec<std::ops::Range<usize>> {
    let chunk = len.div_ceil(threads.max(1));
    (0..threads)
        .map(|t| {
            let start = (t * chunk).min(len);
            let end = ((t + 1) * chunk).min(len);
            start..end
        })
        .collect()
}

/// Target A phase 1 / producers: each worker inserts a disjoint key range.
fn plan_insert_heavy<K: Clone, V: Clone>(corpus: &[(K, V)], threads: usize) -> Vec<Vec<Op<K, V>>> {
    split_ranges(corpus.len(), threads)
        .into_iter()
        .map(|range| {
            corpus[range]
                .iter()
                .map(|(k, v)| Op::Insert(k.clone(), v.clone()))
                .collect()
        })
        .collect()
}

/// Target B: each worker registers, looks up, then finishes each of its
/// disjoint keys - the symmetric insert / lookup / remove interleave.
fn plan_register_lookup_finish<K: Clone, V: Clone>(
    corpus: &[(K, V)],
    threads: usize,
) -> Vec<Vec<Op<K, V>>> {
    split_ranges(corpus.len(), threads)
        .into_iter()
        .map(|range| {
            let mut ops = Vec::with_capacity(range.len() * 3);
            for (k, v) in &corpus[range] {
                ops.push(Op::Insert(k.clone(), v.clone()));
                ops.push(Op::Lookup(k.clone()));
                ops.push(Op::Remove(k.clone()));
            }
            ops
        })
        .collect()
}

/// Target A steady state: producer/consumer overlap. Half the workers
/// insert the `upper` (not-yet-published) range while the other half drain
/// the `lower` (pre-published) range via remove. `lower` must already be
/// resident in the store when this plan runs.
fn plan_mixed_insert_take<K: Clone, V: Clone>(
    lower: &[(K, V)],
    upper: &[(K, V)],
    threads: usize,
) -> Vec<Vec<Op<K, V>>> {
    let insert_workers = (threads / 2).max(1);
    let take_workers = (threads - insert_workers).max(1);

    let mut plan = Vec::with_capacity(insert_workers + take_workers);
    for range in split_ranges(upper.len(), insert_workers) {
        plan.push(
            upper[range]
                .iter()
                .map(|(k, v)| Op::Insert(k.clone(), v.clone()))
                .collect(),
        );
    }
    for range in split_ranges(lower.len(), take_workers) {
        plan.push(
            lower[range]
                .iter()
                .map(|(k, _)| Op::Remove(k.clone()))
                .collect(),
        );
    }
    plan
}

/// Executes a per-thread op plan across `pool`. This is the single timed
/// region for every cell; each worker runs its own op list to completion.
fn run_plan<K, V>(store: &Arc<dyn MapStore<K, V>>, plan: Vec<Vec<Op<K, V>>>, pool: &ThreadPool)
where
    K: Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pool.scope(|s| {
        for thread_ops in plan {
            let store = Arc::clone(store);
            s.spawn(move |_| {
                for op in thread_ops {
                    match op {
                        Op::Insert(k, v) => {
                            black_box(store.insert(k, v));
                        }
                        Op::Lookup(k) => {
                            black_box(store.lookup(&k));
                        }
                        Op::Remove(k) => {
                            black_box(store.remove(&k));
                        }
                    }
                }
            });
        }
    });
}

/// Target A corpus: sequential directory keys with a one-entry plan each,
/// mirroring `DeletePlanMap`'s `PathBuf -> DeletePlan` shape. Keys are
/// grouped under `SHARD_COUNT` parent directories so the path prefixes vary
/// the way real traversal output does.
fn build_corpus_a(count: usize) -> Vec<(PathBuf, DeletePlan)> {
    (0..count)
        .map(|i| {
            let dir = PathBuf::from(format!("dir/{}/{i}", i % SHARD_COUNT));
            let mut plan = DeletePlan::new(dir.clone());
            plan.push(DeleteEntry::new(
                std::ffi::OsString::from(format!("entry-{i}")),
                DeleteEntryKind::File,
            ));
            (dir, plan)
        })
        .collect()
}

/// Target B corpus: sequential `FileNdx` keys with an index-derived `u64`
/// slot payload, mirroring the applier's `FileNdx -> SlotEntry` shape.
fn build_corpus_b(count: usize) -> Vec<(FileNdx, u64)> {
    (0..count)
        .map(|i| (FileNdx::new(i as u32), i as u64))
        .collect()
}

/// Builds a rayon pool pinned to `threads` workers for one cell.
fn build_pool(threads: usize) -> ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("dmb-a-{i}"))
        .build()
        .expect("rayon pool")
}

/// Modest sample budget: the full matrix is 189 cells, so keep each cell's
/// wall time bounded. Offline number-capture runs widen this via criterion
/// CLI flags per the DMB.c/DMB.d capture procedures.
fn configure_group(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
}

/// Target A sweep: both workload shapes x all variants x tiers x threads.
fn bench_target_a(c: &mut Criterion) {
    for &shape in &["insert_heavy", "mixed_insert_take"] {
        let mut group = c.benchmark_group(format!("dmb_a/target_a/{shape}"));
        configure_group(&mut group);
        for &tier in SCALE_TIERS {
            let corpus = build_corpus_a(tier);
            group.throughput(Throughput::Elements(tier as u64));
            for &threads in THREAD_COUNTS {
                let pool = build_pool(threads);
                for &variant in VARIANTS {
                    let id = BenchmarkId::new(variant.label(), format!("{tier}/threads={threads}"));
                    group.bench_with_input(id, &threads, |b, _| {
                        b.iter_batched(
                            || {
                                let store = build_store::<PathBuf, DeletePlan>(variant, tier);
                                let plan = if shape == "insert_heavy" {
                                    plan_insert_heavy(&corpus, threads)
                                } else {
                                    let (lower, upper) = corpus.split_at(corpus.len() / 2);
                                    for (k, v) in lower {
                                        let _ = store.insert(k.clone(), v.clone());
                                    }
                                    plan_mixed_insert_take(lower, upper, threads)
                                };
                                (store, plan)
                            },
                            |(store, plan)| run_plan(&store, plan, &pool),
                            criterion::BatchSize::LargeInput,
                        );
                    });
                }
            }
        }
        group.finish();
    }
}

/// Target B sweep: symmetric register/lookup/finish x variants x tiers x
/// threads. Throughput unit is map operations (three per entry).
fn bench_target_b(c: &mut Criterion) {
    let mut group = c.benchmark_group("dmb_a/target_b/register_lookup_finish");
    configure_group(&mut group);
    for &tier in SCALE_TIERS {
        let corpus = build_corpus_b(tier);
        group.throughput(Throughput::Elements((tier * 3) as u64));
        for &threads in THREAD_COUNTS {
            let pool = build_pool(threads);
            for &variant in VARIANTS {
                let id = BenchmarkId::new(variant.label(), format!("{tier}/threads={threads}"));
                group.bench_with_input(id, &threads, |b, _| {
                    b.iter_batched(
                        || {
                            let store = build_store::<FileNdx, u64>(variant, tier);
                            let plan = plan_register_lookup_finish(&corpus, threads);
                            (store, plan)
                        },
                        |(store, plan)| run_plan(&store, plan, &pool),
                        criterion::BatchSize::LargeInput,
                    );
                });
            }
        }
    }
    group.finish();
}

criterion_group!(benches, bench_target_a, bench_target_b);
criterion_main!(benches);
