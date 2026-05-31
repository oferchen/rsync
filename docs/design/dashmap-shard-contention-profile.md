# DMB.e - Profile DashMap shard contention at peak thread count

Date: 2026-06-01
Status: Design spec
Tracker: DMB.e. Predecessor: DMB.d (comparison at 1M scale).
Follow-up: DMB.f (implement chosen mitigation).

## 1. Motivation

DMB.c and DMB.d established that DashMap outperforms `Mutex<HashMap>` at
16+ threads for the delete pipeline workload. However, the measurements
showed increasing p99 latency at 32-64 threads - the regime where shard
contention becomes the dominant bottleneck rather than global lock
contention.

DashMap internally shards its entries across `N` buckets, each guarded by
a separate `RwLock`. The default shard count is `num_cpus * 4` rounded up
to the next power of two. On a 16-core host this yields 128 shards; on an
8-core host, 64 shards. When 32-64 worker threads contend for these shards,
the probability of two threads hitting the same shard rises significantly.

This document designs the profiling approach to:
1. Quantify actual shard contention under peak thread counts.
2. Analyse key distribution across shards for the delete pipeline's
   sequential NDX access pattern.
3. Evaluate whether shard count tuning or hasher replacement reduces
   contention.
4. Produce a go/no-go decision on mitigation strategies.

## 2. DashMap internal architecture

### 2.1 Shard layout

DashMap stores entries in an array of `HashMap` segments, each wrapped in a
`RwLock`:

```
DashMap<K, V>
  shards: Box<[RwLock<HashMap<K, V>>]>  // length = shard_count
```

Key characteristics:
- **Default shard count:** `(available_parallelism() * 4).next_power_of_two()`.
  On 8-core: 64. On 16-core: 128. On 64-core: 512.
- **Shard selection:** `hash(key) % shard_count`. The hash feeds into a
  bit-mask operation (since shard count is power-of-two).
- **Lock granularity:** Each shard has an independent `RwLock`. Readers on
  different shards never contend. Writers block only readers/writers on the
  same shard.
- **Default hasher:** `SipHash` (via `RandomState`). Produces well-distributed
  hashes but has higher per-op cost than simpler hashers for integer keys.

### 2.2 Contention model

For `T` threads and `S` shards, the probability of at least one collision
(two threads hitting the same shard simultaneously) follows the birthday
problem approximation:

```
P(collision) ~= 1 - e^(-T^2 / (2 * S))
```

| Threads | 64 shards | 128 shards | 256 shards | 512 shards |
|---------|-----------|------------|------------|------------|
| 8       | 0.39      | 0.22       | 0.12       | 0.06       |
| 16      | 0.84      | 0.62       | 0.39       | 0.22       |
| 32      | 0.99+     | 0.96       | 0.84       | 0.62       |
| 64      | 0.99+     | 0.99+     | 0.99+     | 0.96       |

At 32+ threads on a 16-core host (128 shards), shard collisions are nearly
certain. The question is not whether contention exists but whether it
materially degrades throughput.

### 2.3 Access pattern in the delete pipeline

The `ParallelDeltaApplier` uses `DashMap<FileNdx, SlotEntry>` where
`FileNdx` is a `u32` file index assigned sequentially during file-list
construction. Workers register, lookup, and finish entries as chunks arrive
from the wire.

Key observations:
- **Sequential insertion:** NDX values are assigned 0..N sequentially.
  The SipHash of sequential integers distributes well across shards, but
  adjacent NDX values may cluster if the hasher's lower bits have patterns.
- **Temporal locality:** Chunks for recently-registered files arrive
  closely in time. Multiple workers processing consecutive NDX values may
  contend on the same shard if those NDX values hash to it.
- **Short hold times:** The locking discipline (section 3.3 of the
  `ParallelDeltaApplier` documentation) mandates dropping shard guards
  before doing any significant work. This limits critical-section duration
  to 2-3 `Arc::clone` operations (~10-50ns).

## 3. Profiling methodology

### 3.1 perf lock contention (Linux bare-metal)

Primary tool for measuring futex-level contention:

```sh
# Record lock contention events during the 32-thread bench:
perf lock record -a -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --exact 'dashmap.*100k.*32_threads'

# Report contention sorted by total wait time:
perf lock contention -t

# Report sorted by number of contentions:
perf lock contention -n

# Filter to RwLock sites only:
perf lock contention -t -Y 'rwlock*'
```

Metrics extracted:
- **Total contention events:** Number of times a thread blocked waiting
  for a shard RwLock.
- **Total wait time (us):** Cumulative time threads spent blocked on shard
  locks.
- **Max single-wait (us):** Longest individual lock-wait event. Indicates
  worst-case head-of-line blocking.
- **Contention-per-shard distribution:** Identifies hot shards.

### 3.2 Flamegraph on lock-wait paths

Generate a flamegraph focused on lock contention:

```sh
# Capture off-CPU events (time spent waiting for locks):
perf record -e sched:sched_switch --call-graph dwarf -p <PID> -- sleep 10

# Or use bpftrace for targeted RwLock contention:
bpftrace -e '
    tracepoint:lock:lock_acquired /comm == "dmb_a_dashmap"/ {
        @wait[ustack] = sum(args->wait_time_ns);
    }
'

# Generate flamegraph from perf data:
perf script | stackcollapse-perf.pl | flamegraph.pl \
    --title "DashMap Shard Contention (32 threads)" \
    --subtitle "Off-CPU time in RwLock::write/read" \
    > shard_contention_flamegraph.svg
```

Expected call stacks of interest:
- `parking_lot::raw_rwlock::RawRwLock::lock_exclusive_slow` - write
  contention on a shard.
- `parking_lot::raw_rwlock::RawRwLock::lock_shared_slow` - read contention
  (writer holding the shard).
- The parent frame reveals which DashMap operation triggered the wait
  (insert, get, remove).

### 3.3 Custom instrumentation via DashMap::shards()

DashMap exposes `shards()` returning `&[RwLock<HashMap<K, V>>]`. A test
harness can instrument per-shard access counts:

```rust
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};

struct InstrumentedMap<K, V> {
    inner: DashMap<K, V>,
    shard_hits: Vec<AtomicU64>,
}

impl<K: Eq + Hash, V> InstrumentedMap<K, V> {
    fn new(shard_count: usize) -> Self {
        Self {
            inner: DashMap::with_capacity_and_shard_amount(0, shard_count),
            shard_hits: (0..shard_count)
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }

    fn record_access(&self, key: &K) {
        let hash = self.inner.hasher().hash_one(key);
        let shard_idx = hash as usize % self.shard_hits.len();
        self.shard_hits[shard_idx].fetch_add(1, Ordering::Relaxed);
    }

    fn shard_distribution(&self) -> Vec<u64> {
        self.shard_hits
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    fn imbalance_ratio(&self) -> f64 {
        let dist = self.shard_distribution();
        let max = *dist.iter().max().unwrap_or(&1) as f64;
        let mean = dist.iter().sum::<u64>() as f64 / dist.len() as f64;
        max / mean
    }
}
```

Metrics:
- **Imbalance ratio:** `max_shard_hits / mean_shard_hits`. Ideal = 1.0.
  Values > 1.5 indicate problematic clustering.
- **Coefficient of variation:** `std_dev / mean`. Below 0.1 is healthy.
- **Empty shard count:** Shards with zero hits waste memory without
  reducing contention.

### 3.4 Criterion latency percentile capture

Extend the DMB.a harness to capture per-operation latency at 32 and 64
threads:

```rust
// Per-thread latency collection (pre-allocated):
let latencies: Vec<Mutex<Vec<Duration>>> =
    (0..num_threads).map(|_| Mutex::new(Vec::with_capacity(ops_per_thread))).collect();

// Inside timed section:
pool.install(|| {
    keys.par_iter().enumerate().for_each(|(i, key)| {
        let start = Instant::now();
        map.insert(*key, value.clone());
        let elapsed = start.elapsed();
        latencies[rayon::current_thread_index().unwrap()]
            .lock().unwrap()
            .push(elapsed);
    });
});
```

Post-processing extracts p50, p90, p99, p99.9 per (shard_count, hasher,
thread_count) cell.

## 4. Key distribution analysis

### 4.1 Sequential NDX distribution under SipHash

The production workload inserts `FileNdx` values 0, 1, 2, ..., N-1.
SipHash is designed to distribute sequential integers uniformly, but the
bit-mask shard selection (`hash & (shard_count - 1)`) uses only the low
bits of the hash.

Test procedure:
1. Hash NDX values 0..100_000 with SipHash (using DashMap's
   `RandomState`).
2. Compute shard assignment for each: `hash % shard_count`.
3. Build a histogram of entries-per-shard.
4. Compute imbalance ratio and CoV.

Expected result: SipHash produces near-uniform distribution for sequential
integers. Imbalance ratio should be < 1.05 for 128 shards and 100K keys.

### 4.2 Temporal access clustering

Even with uniform distribution, temporal locality can create contention.
If workers process files in NDX order, consecutive NDX values arrive at
the map simultaneously. The question is whether consecutive NDX values
land on the same shard:

```rust
// Check consecutive-pair shard collision rate:
let mut collisions = 0;
for ndx in 0..99_999u32 {
    let shard_a = hash(ndx) % shard_count;
    let shard_b = hash(ndx + 1) % shard_count;
    if shard_a == shard_b {
        collisions += 1;
    }
}
let collision_rate = collisions as f64 / 99_999.0;
// Expected for uniform hash: ~1/shard_count (0.78% for 128 shards)
```

If the collision rate exceeds `2 / shard_count`, the hasher has a
correlation problem for sequential keys and a different hasher should be
evaluated.

### 4.3 Burst access simulation

Simulate the production pattern where a batch of `B` consecutive NDX
values arrive simultaneously (modeling rayon chunk processing):

```rust
// For batch_size B, count max ops hitting any single shard:
fn max_shard_load(start_ndx: u32, batch_size: usize, shard_count: usize) -> usize {
    let mut shard_counts = vec![0usize; shard_count];
    for i in 0..batch_size {
        let ndx = start_ndx + i as u32;
        let shard = hash(ndx) % shard_count;
        shard_counts[shard] += 1;
    }
    *shard_counts.iter().max().unwrap()
}
```

Test with `batch_size` = 32, 64, 128 (matching typical rayon chunk sizes)
and `shard_count` = 64, 128, 256. The max shard load indicates worst-case
serialisation within a single batch.

## 5. Custom shard count evaluation

### 5.1 Test matrix

| Shard count | Rationale |
|-------------|-----------|
| 16          | Baseline. Worse than default; confirms sharding matters. |
| 32          | Matches default for 8-core hosts. |
| 64          | Default for 8-core (times 4, rounded). |
| 128         | Default for 16-core (production CI hosts). |
| 256         | 2x default. Tests whether over-sharding helps. |
| 512         | 4x default. Tests diminishing returns. |

### 5.2 Construction

```rust
// Explicit shard count:
let map: DashMap<FileNdx, SlotEntry> =
    DashMap::with_capacity_and_shard_amount(100_000, shard_count);
```

### 5.3 Metrics per shard count

For each shard count, measure at 32 and 64 threads:
- Throughput (ops/sec)
- p50 and p99 latency
- perf lock contention events
- Memory overhead (shards * per-shard HashMap allocation)

### 5.4 Expected results

- **16 shards:** Heavy contention at 32+ threads (2+ threads per shard on
  average). Throughput degrades significantly.
- **64 shards:** Moderate contention at 32 threads, acceptable at 16.
- **128 shards:** Low contention at 32, moderate at 64. Current default.
- **256 shards:** Marginal improvement over 128. Per-shard HashMap has
  fewer entries, reducing hash-table probe chains but increasing memory
  overhead from empty buckets.
- **512 shards:** Diminishing returns. Lock overhead per shard is
  irreducible; more shards just spread entries thinner.

Expected crossover: 128-256 shards is optimal for 32-thread workloads.
Beyond 256, memory cost grows faster than contention benefit.

## 6. Custom hasher evaluation

### 6.1 Candidate hashers

| Hasher | Cost per u32 hash | Distribution quality | Crate |
|--------|-------------------|---------------------|-------|
| SipHash (default) | ~15ns | Excellent. Cryptographic-grade uniformity. | `std::collections::hash_map::RandomState` |
| FxHash | ~2ns | Good for integers. Multiply-XOR-shift. Weak for strings. | `rustc-hash` (FxHasher) |
| Identity hash | ~0ns | No hashing - uses raw key bits directly. Perfect for sequential integers with power-of-two shard counts IF keys distribute well in low bits. | Custom impl |
| AHash | ~5ns | Excellent. AES-NI accelerated on x86_64. | `ahash` |

### 6.2 Identity hash analysis

For `FileNdx` (sequential u32), an identity hash uses the raw integer as
the hash value. Shard selection becomes `ndx % shard_count`.

With power-of-two shard counts, this is equivalent to
`ndx & (shard_count - 1)`, which simply takes the low bits of the NDX.
Sequential NDX values 0..N distribute perfectly evenly across shards:
each shard gets exactly `N / shard_count` entries (plus one for remainder
shards).

**Advantage:** Zero hash cost. Perfect distribution for sequential keys.
**Risk:** If keys are not sequential (e.g., sparse NDX space, or
non-zero-based), distribution may be uneven. Also, if workers process
entries in NDX-sequential batches, consecutive NDX values land on
consecutive shards - which may cause cache-line bouncing between adjacent
shards stored in contiguous memory.

### 6.3 FxHash analysis

FxHash applies a multiply-by-constant followed by a rotate-XOR. For
integer keys it produces well-distributed hashes with minimal cost:

```rust
impl Hasher for FxHasher {
    fn write_u32(&mut self, i: u32) {
        self.hash = (self.hash.rotate_left(5) ^ (i as u64))
            .wrapping_mul(0x517cc1b727220a95);
    }
}
```

For the delete pipeline:
- ~7x faster than SipHash per operation.
- Distribution quality sufficient for shard selection (only low bits
  matter).
- No security properties needed - keys are internally generated.

### 6.4 AHash analysis

AHash uses hardware AES-NI intrinsics on x86_64 for fast, high-quality
hashing. On aarch64 it falls back to a multiply-based algorithm similar
to FxHash.

- ~3x faster than SipHash on x86_64.
- Better distribution than FxHash (resistant to hash-flooding, though
  irrelevant for internal keys).
- Already a transitive dependency via DashMap's optional `ahash` feature.

### 6.5 Bench configuration

```rust
use dashmap::DashMap;
use rustc_hash::FxBuildHasher;
use ahash::RandomState as AHashState;

// Default SipHash:
let map_sip: DashMap<FileNdx, SlotEntry> = DashMap::new();

// FxHash:
let map_fx: DashMap<FileNdx, SlotEntry, FxBuildHasher> =
    DashMap::with_hasher(FxBuildHasher);

// AHash:
let map_ahash: DashMap<FileNdx, SlotEntry, AHashState> =
    DashMap::with_hasher(AHashState::default());

// Identity hash:
let map_id: DashMap<FileNdx, SlotEntry, IdentityBuildHasher> =
    DashMap::with_hasher(IdentityBuildHasher);
```

Each hasher is tested at 128 shards (default) and 256 shards, at 32 and
64 threads.

## 7. Expected hotspots

### 7.1 Sequential NDX with SipHash

SipHash distributes sequential integers well. No per-shard hotspot
expected. The primary contention source is probabilistic same-shard
collision when many threads access simultaneously.

### 7.2 Batch processing pattern

Rayon distributes work in chunks. If a chunk assigns consecutive NDX
values to one worker and those NDX values hash to the same shard, that
worker serialises its own operations behind a single shard lock while
other shards sit idle.

With SipHash, consecutive NDX values hash to unrelated shards (by design).
With identity hash, consecutive NDX values hit consecutive shards, which
spreads contention evenly but may cause false sharing on adjacent shard
RwLock cache lines.

### 7.3 Write-heavy registration phase

The `register_file` path performs a DashMap `entry().or_insert()` which
acquires a write lock on the target shard. During the initial burst when
all files are being registered, write locks dominate. Unlike read locks,
write locks are exclusive per shard - they block all other readers and
writers on that shard.

Expected: registration phase shows higher contention than the
lookup-dominated steady state.

### 7.4 finish_file removal phase

`finish_file` calls `DashMap::remove()` which also acquires a write lock.
If multiple files finish simultaneously, their NDX values may collide on
the same shard, creating a write-write contention spike.

Expected: the finish phase has lower overall contention than registration
(files finish asynchronously over a longer time window) but individual
contention events may have higher wait times due to the remove + drop
sequence inside the critical section.

## 8. Mitigation strategies

### 8.1 Shard count tuning

**Approach:** Increase shard count beyond the default to reduce collision
probability.

```rust
let optimal_shards = (max_worker_threads * 8).next_power_of_two();
let map = DashMap::with_capacity_and_shard_amount(expected_files, optimal_shards);
```

**Trade-offs:**
- Pro: Linear reduction in collision probability. 256 shards halves
  contention versus 128 shards.
- Con: More shards = more `RwLock` objects, more per-shard HashMap
  overhead, worse cache utilisation from spread-out entries.
- Con: Beyond `threads * 8`, diminishing returns - lock overhead per
  operation becomes the floor regardless of contention.

**Decision threshold:** If the profiling shows contention wait time
exceeds 5% of total operation time at the production thread count, double
the shard count and re-measure.

### 8.2 Custom hasher (FxHash or AHash)

**Approach:** Replace the default SipHash with a faster hasher for integer
keys.

**Trade-offs:**
- Pro: Reduces per-operation hash cost by 5-13ns. At 300K ops per
  iteration, this saves 1.5-3.9ms total. Significant at high thread
  counts where hash time is a larger fraction of the reduced critical
  section.
- Pro: FxHash and AHash both produce adequate distribution for integer
  keys with power-of-two shard counts.
- Con: Non-default hasher requires explicit type parameter on every
  DashMap usage (`DashMap<K, V, S>`), adding API complexity.
- Con: FxHash has known weaknesses for non-integer keys (not relevant for
  FileNdx but limits future generality).

**Decision threshold:** If hasher benchmark shows >= 10% throughput
improvement at 32+ threads, adopt FxHash for the `ParallelDeltaApplier`
files map. AHash is preferred if the improvement is >= 5% but < 10%
(better distribution properties with acceptable speed).

### 8.3 Read-biased access pattern optimisation

**Approach:** Restructure the access pattern to favour `DashMap::get()`
(shared read lock) over `DashMap::entry()` (exclusive write lock).

Current pattern:
1. `register_file` - `entry().or_insert()` (write lock)
2. `dispatch_chunk` - `get()` (read lock) + `get()` (read lock)
3. `finish_file` - `remove()` (write lock)

The registration phase is write-heavy. If files are pre-registered in
bulk before workers start processing chunks, the steady-state workload
becomes read-dominated, allowing maximum shard parallelism through shared
locks.

**Trade-offs:**
- Pro: Shared reads never contend with each other, only with writes.
  Eliminating writes from the hot path removes the primary contention
  source.
- Pro: Matches the natural protocol flow - file list arrives before delta
  data.
- Con: Requires knowing the file set upfront. INC_RECURSE mode discovers
  files incrementally, preventing bulk pre-registration.
- Con: Pre-registration allocates memory for all files before any are
  processed, increasing peak RSS.

**Decision threshold:** If write-lock contention accounts for >= 60% of
total contention wait time, pursue pre-registration for non-INC_RECURSE
mode.

### 8.4 Per-worker local maps with periodic merge

**Approach:** Each worker maintains a thread-local `HashMap` for its
assigned NDX range. Workers merge results into the shared DashMap only at
batch boundaries.

**Trade-offs:**
- Pro: Eliminates all contention within a batch. Workers only touch the
  shared map at synchronisation points.
- Con: Requires partitioning NDX ranges to workers upfront. Random chunk
  arrival order (the production case) means any worker may need to access
  any NDX, breaking the partitioning assumption.
- Con: Merge phase re-introduces contention in a burst.
- Con: Significant code complexity increase. Not justified unless
  contention is severe (> 20% throughput loss vs ideal).

**Decision threshold:** Only pursue if all other mitigations (shard
tuning, hasher, read-biased pattern) fail to bring contention below 10%
of operation time.

### 8.5 Alternative data structures

If DashMap shard contention remains problematic after tuning:

- **`flurry` (concurrent HashMap):** Lock-free reads, epoch-based
  reclamation. Higher single-op cost but no reader contention. Suitable
  if the workload is 90%+ reads.
- **`evmap` (eventually consistent):** Left-right pattern with zero-cost
  reads. Writes are deferred and applied in batches. Suitable if slight
  staleness is acceptable (not the case for the delete pipeline where
  finish_file must see the current state).
- **Sharded `Vec<Mutex<HashMap>>`:** Manual sharding with explicit shard
  selection. Same model as DashMap but without the abstraction overhead.
  Only worth pursuing if DashMap's internal bookkeeping is a measurable
  cost.

**Decision threshold:** Switch to an alternative only if DashMap at
optimal shard count + optimal hasher still shows > 15% throughput loss
versus the theoretical contention-free ceiling.

## 9. Decision criteria

### 9.1 Contention is negligible (no action needed)

All of the following hold at 32 threads:
- Contention wait time < 2% of total operation time.
- p99/p50 latency ratio < 3x.
- Throughput within 5% of the single-threaded extrapolation
  (ops/sec scales linearly).

Action: Document the finding. DashMap default configuration is adequate
for the delete pipeline's peak concurrency. Close DMB.e.

### 9.2 Contention is moderate (tune shard count or hasher)

Any of the following at 32 threads:
- Contention wait time 2-10% of total operation time.
- p99/p50 ratio 3-8x.
- Throughput plateaus at 70-95% of linear scaling.

Action: Apply the mitigation that produces the best throughput/complexity
trade-off:
1. First try: increase shard count to 256 or 512.
2. Second try: switch to FxHash or AHash.
3. Third try: pre-register files to make steady state read-dominated.

File a follow-up PR implementing the chosen mitigation.

### 9.3 Contention is severe (structural change needed)

Any of the following at 32 threads:
- Contention wait time > 10% of total operation time.
- p99/p50 ratio > 8x.
- Throughput at 32 threads is less than 2x throughput at 16 threads
  (negative scaling).

Action: Evaluate alternative data structures (section 8.5). If none
provide sufficient improvement, cap the worker pool at the thread count
where scaling remains positive and document the ceiling.

### 9.4 Summary decision table

| Contention level | Shard tuning helps? | Hasher helps? | Decision |
|-----------------|--------------------|--------------|---------| 
| Negligible (<2%) | N/A | N/A | No action. Close DMB.e. |
| Moderate, shard-fixable | Yes (>10% gain) | Maybe | Increase shard count. Ship as config knob. |
| Moderate, hasher-fixable | No | Yes (>10% gain) | Switch hasher. Ship in `ParallelDeltaApplier`. |
| Moderate, both help | Yes | Yes | Apply both. Shard count = `threads * 8`, hasher = FxHash. |
| Severe, fixable | Partial | Partial | Combine shard tuning + hasher + read-bias. Re-measure. |
| Severe, unfixable | No | No | Cap concurrency or switch data structure. |

## 10. Bench invocation

### 10.1 Shard count sweep

```sh
# Custom bench binary for shard-count experiments:
cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter 'shard_sweep.*32_threads'

# Individual shard count:
cargo bench -p engine --bench dmb_e_shard_contention \
    -- --exact 'shard_sweep/256_shards/32_threads'
```

### 10.2 Hasher comparison

```sh
cargo bench -p engine --bench dmb_e_shard_contention \
    -- --filter 'hasher_comparison.*32_threads'
```

### 10.3 Distribution analysis (unit test, not bench)

```sh
cargo nextest run -p engine --all-features \
    -E 'test(dmb_e_shard_distribution)'
```

### 10.4 Full profiling session (bare-metal Linux)

```sh
# Step 1: Run bench under perf lock:
perf lock record -a -- \
    cargo bench -p engine --bench dmb_e_shard_contention \
    -- --exact 'shard_sweep/128_shards/32_threads' \
    --measurement-time 30

# Step 2: Analyse contention:
perf lock contention -t > contention_128_shards_32t.txt

# Step 3: Generate flamegraph:
perf lock contention -t -F -- > lock_stacks.txt
flamegraph.pl lock_stacks.txt > shard_contention_128_32t.svg

# Step 4: Repeat for 256 shards, 64 threads, alternative hashers.
```

## 11. Implementation outline

### 11.1 Bench file structure

```
crates/engine/benches/
  dmb_e_shard_contention.rs       # Criterion bench
  dmb_e_shard_distribution.rs     # Distribution analysis (test, not bench)
```

### 11.2 Bench groups

```rust
criterion_group!(
    shard_sweep,
    bench_shard_16,
    bench_shard_32,
    bench_shard_64,
    bench_shard_128,
    bench_shard_256,
    bench_shard_512,
);

criterion_group!(
    hasher_comparison,
    bench_siphash_default,
    bench_fxhash,
    bench_ahash,
    bench_identity,
);
```

Each benchmark function iterates over thread counts [16, 32, 64] and
measures throughput + latency percentiles.

### 11.3 Distribution test

```rust
#[test]
fn dmb_e_shard_distribution_sequential_ndx() {
    for shard_count in [64, 128, 256, 512] {
        let map: DashMap<u32, (), RandomState> =
            DashMap::with_capacity_and_shard_amount(100_000, shard_count);

        for ndx in 0..100_000u32 {
            map.insert(ndx, ());
        }

        // Verify distribution via shards():
        let shard_lens: Vec<usize> = map.shards()
            .iter()
            .map(|s| s.read().len())
            .collect();

        let mean = 100_000.0 / shard_count as f64;
        let max = *shard_lens.iter().max().unwrap() as f64;
        let imbalance = max / mean;

        assert!(
            imbalance < 1.15,
            "Shard imbalance {imbalance:.3} exceeds 1.15 for {shard_count} shards"
        );
    }
}
```

## 12. Results template

Numbers captured offline and appended here once measured.

### 12.1 Shard count sweep (32 threads, SipHash default)

| Shards | Ops/sec | p50 (ns) | p99 (ns) | p99/p50 | Contention events | Wait (us) |
|--------|---------|----------|----------|---------|-------------------|-----------|
| 16     |         |          |          |         |                   |           |
| 32     |         |          |          |         |                   |           |
| 64     |         |          |          |         |                   |           |
| 128    |         |          |          |         |                   |           |
| 256    |         |          |          |         |                   |           |
| 512    |         |          |          |         |                   |           |

### 12.2 Shard count sweep (64 threads, SipHash default)

| Shards | Ops/sec | p50 (ns) | p99 (ns) | p99/p50 | Contention events | Wait (us) |
|--------|---------|----------|----------|---------|-------------------|-----------|
| 16     |         |          |          |         |                   |           |
| 32     |         |          |          |         |                   |           |
| 64     |         |          |          |         |                   |           |
| 128    |         |          |          |         |                   |           |
| 256    |         |          |          |         |                   |           |
| 512    |         |          |          |         |                   |           |

### 12.3 Hasher comparison (128 shards, 32 threads)

| Hasher | Ops/sec | p50 (ns) | p99 (ns) | vs SipHash |
|--------|---------|----------|----------|------------|
| SipHash |        |          |          | 1.00x      |
| FxHash  |        |          |          |            |
| AHash   |        |          |          |            |
| Identity |       |          |          |            |

### 12.4 Hasher comparison (128 shards, 64 threads)

| Hasher | Ops/sec | p50 (ns) | p99 (ns) | vs SipHash |
|--------|---------|----------|----------|------------|
| SipHash |        |          |          | 1.00x      |
| FxHash  |        |          |          |            |
| AHash   |        |          |          |            |
| Identity |       |          |          |            |

### 12.5 Key distribution (SipHash, 100K sequential keys)

| Shards | Mean entries/shard | Max entries/shard | Imbalance | CoV |
|--------|-------------------|-------------------|-----------|-----|
| 64     |                   |                   |           |     |
| 128    |                   |                   |           |     |
| 256    |                   |                   |           |     |
| 512    |                   |                   |           |     |

### 12.6 Decision outcome

Pending bench results.

## 13. Cross-references

- `docs/design/dashmap-vs-mutex-100k-delete-bench.md` - DMB.c spec.
  Established DashMap vs Mutex throughput crossover at 100K scale.
- `docs/design/dmb-b-dashmap-thread-sweep.md` - DMB.b thread sweep.
  DashMap-only scaling curve that motivated this shard investigation.
- `docs/design/dashmap-scalability-decision.md` - Architecture decision
  record for DashMap adoption. References shard contention as a future
  concern.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier` with `files: DashMap<FileNdx, SlotEntry>`.
  Primary production target for shard tuning.
- `crates/engine/src/delete/plan_map.rs` - `DeletePlanMap`. Currently
  `Mutex<HashMap>`; DashMap migration decision depends on DMB.c/d/e
  outcomes.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - Original
  DashMap selection audit. Predicted acceptable contention at production
  thread counts.
