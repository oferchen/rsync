# DMB.e - Profile DashMap shard contention at peak thread count

Date: 2026-06-01
Status: Design spec
Tracker: DMB.e. Predecessor: DMB.c (DashMap vs Mutex 100K delete bench).
Follow-up: DMB.f (apply mitigation if contention exceeds threshold).

## 1. Motivation

DMB.c established the throughput crossover between DashMap and
`Mutex<HashMap>` at 100K entries across thread counts 1-32. The missing
dimension is *intra-DashMap* contention: at peak thread counts (32-64
workers), how do the internal shard locks behave? Even when DashMap
outperforms a single Mutex, its sharding may be suboptimal for the delete
workload's access pattern.

DashMap's default shard count (`available_parallelism() * 4`, rounded to
the next power of two) was chosen for general-purpose workloads. The delete
pipeline's key distribution - sequential `FileNdx` values and
path-derived directory keys - may cluster into a subset of shards, leaving
others idle while hot shards serialize concurrent accessors.

This document designs the profiling methodology to quantify shard
contention and evaluate whether custom shard counts or alternative hashers
improve throughput at the 32-64 thread tier.

## 2. DashMap internal shard architecture

### 2.1 Shard layout

DashMap partitions its key space into N shards, each protected by an
independent `RwLock`. The architecture:

```
DashMap<K, V>
  ├── shards: Vec<RwLock<HashMap<K, V>>>  (N shards)
  ├── hasher: S (default: RandomState / SipHash-1-3)
  └── shard selection: hash(key) % N
```

- **Default shard count:** `available_parallelism() * 4` rounded up to
  the next power of two. On a 16-core host: 128 shards. On an 8-core
  CI runner: 64 shards.
- **Lock type:** Each shard uses `parking_lot::RwLock` (not `std::sync`).
  Readers are concurrent within a shard; writers are exclusive.
- **Hasher:** Default is `RandomState` (per-map random seed + SipHash-1-3).
  This provides uniform distribution for arbitrary keys but adds per-op
  overhead compared to simpler hashers.

### 2.2 Per-operation cost breakdown

For a single `insert` or `get`:

1. Hash the key (SipHash-1-3: ~15 ns for small keys).
2. Compute shard index: `hash & (num_shards - 1)` (power-of-two mask).
3. Acquire `RwLock` on the selected shard (uncontended: ~20 ns with
   `parking_lot`; contended: OS futex wait).
4. Perform HashMap operation on the shard's internal HashMap.
5. Release lock.

The fixed overhead (steps 1-3) is ~35-40 ns per op in the uncontended
case, compared to ~20 ns for a single `Mutex<HashMap>` acquisition. This
per-op tax is the price of sharding - it only pays off when contention
savings exceed the overhead.

### 2.3 Contention model

Given T threads and S shards performing uniformly distributed operations:

- **Expected threads per shard:** T / S.
- **Collision probability** (two threads hitting the same shard
  simultaneously): approximately `1 - ((S-1)/S)^(T-1)`.
- **At 32 threads, 128 shards:** ~22% chance of at least one collision
  per operation. Manageable.
- **At 64 threads, 128 shards:** ~39% chance. Contention becomes visible
  in tail latency.
- **At 64 threads, 16 shards:** ~98% chance. Near-certain serialisation
  on every operation.

The critical question is whether the delete workload's keys distribute
uniformly or cluster.

## 3. Profiling methodology

### 3.1 Tier 1: `perf lock` contention analysis (Linux bare-metal)

`perf lock` instruments futex operations to report per-lock contention
events, wait time, and hold time:

```sh
# Record lock contention during 64-thread DashMap bench:
perf lock record -a -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*64_threads' --measurement-time 30

# Report sorted by total wait time:
perf lock report --sort wait_total

# Report sorted by contention count:
perf lock report --sort acquired
```

**Expected output columns:**
- `Name` - lock identifier (address or symbol).
- `acquired` - number of times the lock was taken.
- `contended` - number of times a thread had to wait.
- `avg wait (ns)` - mean wait time per contended acquisition.
- `total wait (ns)` - cumulative wait time across all contended events.

**Interpretation:**
- If contention concentrates on 2-3 shard locks while others show zero
  contention, keys are clustering.
- If contention is evenly spread across shards, the workload distributes
  well and the issue is simply insufficient shard count.
- If `total wait` for DashMap's shard locks exceeds 10% of wall-clock
  measurement time, contention is the primary bottleneck.

### 3.2 Tier 2: Flamegraph on lock-wait paths

Generate a flamegraph filtered to lock-acquisition call stacks to
visualise where threads spend time waiting:

```sh
# Record with dwarf call graphs for accurate flamegraphs:
perf record -g --call-graph dwarf -F 999 -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*64_threads' --measurement-time 30

# Generate flamegraph:
perf script | stackcollapse-perf.pl | flamegraph.pl \
    --title "DashMap 64-thread lock contention" \
    --grep 'futex_wait\|parking_lot\|RwLock' \
    > dashmap-64t-contention.svg
```

**What to look for:**
- Wide `parking_lot::raw_rwlock::RawRwLock::lock_exclusive_slow` bars
  indicate write-lock contention.
- Stacks ending in `futex_wait` with DashMap shard addresses identify
  which operations (insert/get/remove) are most contended.
- Compare flamegraph width ratios: lock-wait frames should be < 5% of
  total CPU for healthy sharding.

### 3.3 Tier 3: Custom instrumentation - shard hit histogram

Add a diagnostic wrapper that counts per-shard access frequency to detect
distribution skew:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// Diagnostic shard-hit counter. Bench-only, not production code.
struct ShardHistogram {
    /// One counter per shard. Index = shard_index.
    hits: Vec<AtomicU64>,
    num_shards: usize,
}

impl ShardHistogram {
    fn new(num_shards: usize) -> Self {
        Self {
            hits: (0..num_shards).map(|_| AtomicU64::new(0)).collect(),
            num_shards,
        }
    }

    /// Record a hit on the shard that `key` maps to.
    fn record<K: std::hash::Hash>(&self, key: &K, hasher: &impl std::hash::BuildHasher) {
        use std::hash::Hasher;
        let mut h = hasher.build_hasher();
        key.hash(&mut h);
        let shard = (h.finish() as usize) % self.num_shards;
        self.hits[shard].fetch_add(1, Ordering::Relaxed);
    }

    /// Compute coefficient of variation (stddev / mean).
    /// 0.0 = perfectly uniform. > 0.3 = significant skew.
    fn coefficient_of_variation(&self) -> f64 {
        let counts: Vec<f64> = self.hits.iter()
            .map(|c| c.load(Ordering::Relaxed) as f64)
            .collect();
        let n = counts.len() as f64;
        let mean = counts.iter().sum::<f64>() / n;
        if mean == 0.0 { return 0.0; }
        let variance = counts.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / n;
        variance.sqrt() / mean
    }

    /// Report top-N hottest shards.
    fn top_shards(&self, n: usize) -> Vec<(usize, u64)> {
        let mut indexed: Vec<(usize, u64)> = self.hits.iter()
            .enumerate()
            .map(|(i, c)| (i, c.load(Ordering::Relaxed)))
            .collect();
        indexed.sort_by(|a, b| b.1.cmp(&a.1));
        indexed.truncate(n);
        indexed
    }
}
```

**Diagnostic thresholds:**
- CoV < 0.1: excellent distribution. No tuning needed.
- CoV 0.1-0.3: mild skew. Custom shard count may help.
- CoV > 0.3: significant clustering. Hasher change required.

### 3.4 Tier 4: Cache-line false sharing detection

DashMap's shard `RwLock` structures are laid out contiguously in memory.
Adjacent shards may share a cache line (64 bytes on x86), causing false
sharing when threads write to neighbouring shard locks:

```sh
# L1 data cache miss rate during 64-thread run:
perf stat -e L1-dcache-load-misses,L1-dcache-loads,cache-misses,cache-references \
    -- cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*64_threads' --measurement-time 15

# Compare against single-threaded baseline:
perf stat -e L1-dcache-load-misses,L1-dcache-loads \
    -- cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*1_threads' --measurement-time 15
```

**Interpretation:**
- If L1 miss rate increases super-linearly with thread count beyond what
  working-set growth explains, false sharing is present.
- DashMap does not pad shard locks to cache-line boundaries. At 128 shards,
  each shard occupies ~16 bytes for the `RwLock` metadata, fitting 4 locks
  per cache line. Four threads competing for adjacent shards will
  invalidate each other's cache lines even without logical contention.

## 4. Shard distribution analysis

### 4.1 Delete workload key patterns

**Target A (DeletePlanMap):** Keys are `PathBuf` values representing
destination-relative directory paths. Characteristics:
- Paths share common prefixes (`src/`, `lib/`, `tests/`).
- SipHash on paths with shared prefixes still distributes well because
  SipHash is not prefix-sensitive.
- Key space is relatively small (one entry per unique directory, not per
  file). At 100K files with ~500 unique directories, only 500 keys.
- Low key count means low contention regardless of shard distribution.

**Target B (ParallelDeltaApplier):** Keys are `FileNdx` (u32) values
representing file indices from the wire. Characteristics:
- Sequential integers: 0, 1, 2, ..., N-1.
- SipHash on sequential u32 values distributes uniformly (no clustering).
- FxHash or identity hash on sequential u32 values would cluster terribly
  (low bits repeat modulo shard count).
- Key count equals file count: 100K keys for 100K files.

### 4.2 Expected shard distribution for sequential u32 keys

With SipHash (DashMap's default) and 128 shards, hashing sequential u32
values 0..100_000:

- Expected hits per shard: 100_000 / 128 = 781.
- Standard deviation (Poisson): sqrt(781) = 28.
- CoV: 28 / 781 = 0.036.
- Conclusion: near-perfect uniformity. SipHash eliminates sequential
  clustering.

### 4.3 Adversarial case: what would cause clustering?

Clustering occurs if the hasher produces correlated outputs for the
workload's key sequence:
- **Identity hash** (key as-is): sequential u32 mod 128 produces runs of
  same-shard hits (keys 0, 128, 256 all hit shard 0).
- **FxHash** (multiply-shift): better than identity but still shows
  patterns for sequential integers at power-of-two shard counts.
- **Production key ordering:** Wire protocol delivers file indices in
  order. If rayon distributes sequential index ranges to workers (e.g.,
  worker 0 gets indices 0-999, worker 1 gets 1000-1999), and SipHash maps
  those ranges to the same shard, contention spikes. However, SipHash's
  avalanche property prevents this.

**Verdict:** The default SipHash hasher provides excellent distribution for
the delete workload's key patterns. The profiling will likely confirm this
and point to raw shard count (not distribution) as the limiting factor.

## 5. Custom shard count evaluation

### 5.1 Shard count matrix

DashMap's `with_capacity_and_shard_amount(cap, shards)` allows explicit
shard count selection. Bench the following configurations at 32 and 64
threads:

| Shards | Threads-per-shard (32T) | Threads-per-shard (64T) | Expected effect |
|--------|------------------------|------------------------|-----------------|
| 16 | 2.0 | 4.0 | Heavy contention. Baseline for worst case. |
| 32 | 1.0 | 2.0 | Moderate contention at 64T. |
| 64 | 0.5 | 1.0 | Crossover: ~1 thread per shard at peak. |
| 128 | 0.25 | 0.5 | DashMap default on 16-core host. Expected sweet spot. |
| 256 | 0.125 | 0.25 | Over-sharded. Tests if extra shards still help or if per-shard HashMap overhead dominates. |

### 5.2 Bench invocation

```sh
# Requires parameterised bench that accepts shard count as an argument.
# Each shard count is a separate criterion group:
cargo bench -p engine --bench dmb_e_shard_sweep \
    -- --filter '100k.*(16_shards|32_shards|64_shards|128_shards|256_shards)'
```

### 5.3 Expected outcome

- **16 shards at 64 threads:** Significant degradation. 4 threads per
  shard means frequent write-lock contention. Throughput drops 30-50%
  versus 128 shards.
- **32 shards at 64 threads:** Mild improvement over 16. Still 2 threads
  per shard on average.
- **64 shards at 64 threads:** Near-optimal for 64 threads. One thread
  per shard means collisions are probabilistic, not guaranteed.
- **128 shards (default):** Slight improvement over 64 at 64 threads due
  to reduced collision probability. Diminishing returns.
- **256 shards:** Marginal or negative improvement. Each shard's internal
  HashMap is smaller (fewer entries per bucket array), increasing memory
  overhead per entry. The per-operation cost of hashing + shard selection
  is unchanged but the reduced entries per shard decrease HashMap lookup
  efficiency due to worse cache locality.

### 5.4 Memory overhead per shard count

Each shard adds:
- `RwLock` metadata: 16 bytes (parking_lot).
- HashMap backing array: at least one allocation (minimum capacity / shards
  entries pre-allocated per shard).
- Allocator overhead: ~16 bytes per allocation.

At 100K entries:

| Shards | Entries per shard | HashMap backing per shard | Total shard overhead |
|--------|------------------|--------------------------|--------------------|
| 16 | 6,250 | ~100 KB | ~1.6 MB |
| 64 | 1,562 | ~25 KB | ~1.6 MB |
| 128 | 781 | ~12.5 KB | ~1.6 MB |
| 256 | 390 | ~6.3 KB | ~1.6 MB |

Memory overhead is roughly constant because total entries are fixed.
The difference is in cache efficiency: fewer entries per shard means
smaller working sets per lock-acquisition, improving L1 residency for
the HashMap probe within each shard.

## 6. Expected hotspots

### 6.1 Adjacent ndx values hashing to the same shard

If the hasher maps a contiguous range of file indices to the same shard,
a worker processing that range will serialize all its operations on a
single shard lock while other shards sit idle.

With SipHash this is unlikely (section 4.2), but a weaker hasher or an
unfortunate random seed could produce short runs (4-8 consecutive keys on
the same shard). The diagnostic histogram (section 3.3) will detect this.

### 6.2 Phase transitions: bulk insert then bulk read

The delete workload has two distinct phases:
1. **Phase 1 (insert):** Rayon workers insert entries concurrently. All
   operations are write-locks.
2. **Phase 2 (drain):** Single consumer removes entries sequentially. All
   operations are write-locks on a single thread.

Contention is concentrated in phase 1. In phase 2, the single consumer
holds write locks without contention but pays the full shard-selection and
lock-acquisition overhead for every operation (overhead without benefit).

**Hotspot:** If phase 1 workers temporarily synchronise (e.g., rayon
work-stealing causes two workers to process adjacent indices
simultaneously), they may compete for the same shard lock. The
`perf lock` trace will show bursty contention events correlated with
rayon scheduling decisions.

### 6.3 RwLock writer starvation under read-heavy workloads

Target B (ParallelDeltaApplier) has a read-heavy access pattern: after
initial registration, most operations are lookups (read locks) with
occasional finish calls (write locks). `parking_lot::RwLock` is
write-preferring by default, so a pending writer blocks new readers. At
high thread counts, a single `finish_file` write-lock on a hot shard can
temporarily block all concurrent `get` readers on that shard.

**Detection:** The flamegraph (section 3.2) will show
`lock_shared_slow` stacks (reader waiting) correlated with
`lock_exclusive` holds on the same shard address.

### 6.4 Hash computation overhead at scale

At 64 threads processing 100K operations each (6.4M total operations),
SipHash computation adds ~96 ms of pure CPU time (15 ns * 6.4M). This is
non-trivial but not the bottleneck - lock contention wait time should
dominate. However, if the profiling shows hash computation as a significant
fraction (>10% of total CPU), switching to a faster hasher (FxHash,
AHash) is warranted provided distribution remains uniform.

## 7. Mitigation strategies

### 7.1 Custom hasher: AHash (default in newer DashMap versions)

DashMap 5.5+ uses `ahash::RandomState` by default (if the `ahash` feature
is enabled). AHash provides:
- Faster hashing than SipHash (~5 ns vs ~15 ns for small keys).
- AES-NI hardware acceleration on x86_64.
- Excellent distribution for integer keys.

```rust
use ahash::RandomState;
use dashmap::DashMap;

let map: DashMap<u32, V, RandomState> = DashMap::with_hasher(RandomState::new());
```

**Trade-off:** AHash is not cryptographically secure (not relevant for this
use case) and adds a dependency (already in workspace via DashMap's default
features).

### 7.2 Custom hasher: FxHash (identity-like for integers)

For `u32` keys specifically, `rustc_hash::FxHasher` is extremely fast
(~3 ns) but produces poor distribution for sequential integers at
power-of-two shard counts. NOT recommended for DashMap's shard selection
unless combined with a prime shard count (which DashMap does not support
natively).

**Verdict:** Do not use FxHash with DashMap. The distribution failure
would negate any hashing speed gain.

### 7.3 Shard count tuning

Based on the profiling results, select the optimal shard count:

```rust
use dashmap::DashMap;

// Production: use measured-optimal shard count.
let optimal_shards = std::thread::available_parallelism()
    .map(|p| p.get())
    .unwrap_or(16)
    .next_power_of_two() * 4;  // Default heuristic; adjust based on bench.

let map: DashMap<FileNdx, SlotEntry> =
    DashMap::with_capacity_and_shard_amount(expected_files, optimal_shards);
```

**Rule of thumb from profiling literature:**
- Optimal shards = 2-4x the maximum expected concurrent writers.
- At 64 peak writers: 128-256 shards.
- Below 16 writers: shard count matters less than per-op overhead.

### 7.4 Key distribution improvement

If the histogram (section 3.3) reveals clustering despite SipHash, add a
bit-mixing step before shard selection:

```rust
/// Bit-mix a u32 key to improve shard distribution.
/// Uses the finaliser from MurmurHash3.
#[inline]
fn mix_key(mut key: u32) -> u32 {
    key ^= key >> 16;
    key = key.wrapping_mul(0x85eb_ca6b);
    key ^= key >> 13;
    key = key.wrapping_mul(0xc2b2_ae35);
    key ^= key >> 16;
    key
}
```

This is a last resort. SipHash already provides excellent avalanche; if
clustering is observed, the root cause is more likely a bug in shard
selection logic than hash quality.

### 7.5 Per-shard capacity pre-allocation

Reduce per-shard HashMap resizes by pre-allocating capacity evenly:

```rust
let map: DashMap<K, V> = DashMap::with_capacity_and_shard_amount(
    expected_entries,  // Total capacity distributed across shards.
    shard_count,
);
// DashMap internally allocates: expected_entries / shard_count per shard.
```

This eliminates resize-induced write-lock holds (a resize requires
exclusive access to the shard and can take microseconds for large shard
HashMaps).

### 7.6 Sharded architecture bypass for phase 2

Phase 2 (single-consumer drain) does not benefit from sharding. The
consumer could drain directly from the internal shard HashMaps via
`DashMap::into_iter()` (consuming iterator that avoids lock acquisition)
or collect all entries into a `Vec` first and process without locking:

```rust
// Drain without per-entry lock overhead:
let entries: Vec<(K, V)> = map.into_iter().collect();
for (key, value) in entries {
    process(key, value);
}
```

This eliminates phase 2's per-op lock overhead entirely but requires
the consumer to take ownership of the map.

## 8. Decision criteria

### 8.1 When to tune shard count

Tune if ALL of the following hold:
1. `perf lock` shows > 5% of wall-clock time in DashMap shard lock waits
   at 32+ threads.
2. Shard histogram CoV < 0.2 (distribution is acceptable; the problem is
   raw shard count, not skew).
3. Increasing shard count from 128 to 256 yields > 10% throughput gain
   in the bench sweep.

**Action:** Set explicit shard count in production constructors. Document
the measured-optimal value.

### 8.2 When to change hasher

Change hasher if ALL of the following hold:
1. Shard histogram CoV > 0.3 (significant distribution skew).
2. The skew is reproducible across runs (not a one-off seed issue).
3. Switching to AHash reduces CoV below 0.15 without throughput regression.

**Action:** Add explicit `ahash::RandomState` hasher to DashMap
constructors. Verify no performance regression at low thread counts.

### 8.3 When to accept contention (do nothing)

Accept if ANY of the following hold:
1. `perf lock` shows < 2% of wall-clock time in shard lock waits at 64
   threads. Contention exists but is not the bottleneck.
2. The production workload never exceeds 16 concurrent writers (making
   64-thread profiling academic).
3. Throughput at 32-64 threads scales > 0.7x linearly (i.e., doubling
   threads from 32 to 64 yields > 40% throughput gain). The sharding is
   working well enough.

**Action:** Close DMB.e with "contention within acceptable bounds" and
do not pursue DMB.f (mitigation).

### 8.4 Decision matrix

| Lock wait % | Shard CoV | Scaling 32->64T | Action |
|-------------|-----------|-----------------|--------|
| < 2% | Any | Any | Accept. Close DMB.e. |
| 2-5% | < 0.2 | < 0.4x linear | Tune shard count (7.3). |
| 2-5% | >= 0.2 | < 0.4x linear | Change hasher (7.2) + tune shards. |
| > 5% | < 0.2 | Any | Increase shards to 256+. |
| > 5% | >= 0.3 | Any | Change hasher + increase shards. |
| > 10% | Any | Any | Fundamental redesign needed. Consider per-thread local maps with merge. |

## 9. Bench harness specification

### 9.1 New bench file

```
crates/engine/benches/dmb_e_shard_contention.rs
```

Registered in `crates/engine/Cargo.toml`:

```toml
[[bench]]
name = "dmb_e_shard_contention"
harness = false
```

### 9.2 Parameterised shard sweep

The bench sweeps shard counts {16, 32, 64, 128, 256} at thread counts
{32, 64} for both Target A and Target B. Each (shard_count, thread_count)
pair is a separate criterion group:

```rust
fn bench_shard_sweep(c: &mut Criterion) {
    let shard_counts = [16, 32, 64, 128, 256];
    let thread_counts = [32, 64];

    for &shards in &shard_counts {
        for &threads in &thread_counts {
            let group_name = format!(
                "dmb_e_target_b/100k/{shards}_shards/{threads}_threads"
            );
            let mut group = c.benchmark_group(&group_name);
            group.throughput(Throughput::Elements(300_000));  // Target B ops
            group.sample_size(20);
            group.measurement_time(Duration::from_secs(10));

            group.bench_function("throughput", |b| {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let map: DashMap<u32, SlotEntry> =
                    DashMap::with_capacity_and_shard_amount(100_000, shards);

                b.iter_batched(
                    || prepare_workload(100_000),
                    |workload| pool.install(|| execute_workload(&map, workload)),
                    BatchSize::LargeInput,
                );
            });
            group.finish();
        }
    }
}
```

### 9.3 Shard histogram collection

After the throughput bench completes, a separate diagnostic pass runs the
same workload with the `ShardHistogram` wrapper (section 3.3) and prints
the distribution report:

```sh
cargo bench -p engine --bench dmb_e_shard_contention -- --filter 'histogram'
```

Output (example):
```
Shard histogram (128 shards, 100K ops, SipHash):
  CoV: 0.034
  Top-5 shards: [47: 832, 91: 819, 3: 814, 112: 811, 66: 809]
  Bottom-5 shards: [88: 748, 29: 752, 104: 755, 17: 757, 63: 759]
  Max/min ratio: 1.11
```

## 10. Invocation

### 10.1 Full shard sweep

```sh
cargo bench -p engine --bench dmb_e_shard_contention
```

### 10.2 Single configuration

```sh
# 128 shards at 64 threads (production default):
cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter '128_shards.*64_threads'
```

### 10.3 Contention profiling (Linux bare-metal)

```sh
# Step 1: perf lock on the hot configuration (16 shards, 64 threads):
perf lock record -a -- \
    cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter '16_shards.*64_threads' --measurement-time 30

perf lock report --sort wait_total > dmb-e-lock-report-16s-64t.txt

# Step 2: flamegraph on contention paths:
perf record -g --call-graph dwarf -F 999 -- \
    cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter '16_shards.*64_threads' --measurement-time 30

perf script | stackcollapse-perf.pl | flamegraph.pl \
    --title "DashMap 16-shard 64-thread contention" \
    --grep 'futex\|parking_lot\|RwLock' \
    > dmb-e-flamegraph-16s-64t.svg

# Step 3: compare with 128 shards (expected low contention):
perf lock record -a -- \
    cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter '128_shards.*64_threads' --measurement-time 30

perf lock report --sort wait_total > dmb-e-lock-report-128s-64t.txt
```

### 10.4 Shard histogram diagnostic

```sh
cargo bench -p engine --bench dmb_e_shard_contention -- --filter 'histogram'
```

## 11. Results template

### 11.1 Throughput by shard count (Target B, 100K entries)

| Shards | 32 threads (Mops/s) | 64 threads (Mops/s) | 32->64 scaling |
|--------|--------------------|--------------------|---------------|
| 16 | | | |
| 32 | | | |
| 64 | | | |
| 128 | | | |
| 256 | | | |

### 11.2 Shard distribution (coefficient of variation)

| Shards | Hasher | CoV | Max/min ratio |
|--------|--------|-----|---------------|
| 16 | SipHash | | |
| 64 | SipHash | | |
| 128 | SipHash | | |
| 128 | AHash | | |

### 11.3 Lock contention (perf lock, 64 threads)

| Shards | Contention events | Total wait (ms) | Wait % of wall-clock |
|--------|-------------------|-----------------|---------------------|
| 16 | | | |
| 64 | | | |
| 128 | | | |
| 256 | | | |

### 11.4 Decision outcome

Pending profiling results.

## 12. Cross-references

- `docs/design/dashmap-vs-mutex-100k-delete-bench.md` - DMB.c. Establishes
  the DashMap vs Mutex crossover point that motivates this deeper shard
  analysis.
- `docs/design/dmb-b-dashmap-thread-sweep.md` - DMB.b. DashMap-only
  cores-vs-throughput curve at 100K.
- `docs/design/dmb-a-dashmap-delete-bench-harness.md` - DMB.a. Unified
  harness spec.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier` DashMap usage (Target B).
- `crates/engine/src/delete/plan_map.rs` - `DeletePlanMap` Mutex usage
  (Target A, DashMap migration candidate).
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - Original
  DashMap selection audit.
