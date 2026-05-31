# DMB.d - DashMap vs Mutex throughput comparison at 1M delete scale

Date: 2026-06-01
Status: Design spec
Tracker: DMB.d. Predecessor: DMB.c (DashMap vs Mutex comparison at 100K).
Related: DMB.a (harness), DMB.b (DashMap thread sweep at 100K).

## 1. Motivation

DMB.c establishes the crossover point between DashMap and `Mutex<HashMap>` at
100K entries - the production-relevant tier for receiver file counts.
DMB.d extends this comparison to 1,000,000 entries to answer whether
DashMap's sharding advantage grows, saturates, or degrades at the
million-file tier.

At 1M entries, effects invisible at 100K become dominant:

- **Memory allocator pressure.** 1M entries force multiple arena expansions
  and increase TLB pressure. DashMap's per-shard `HashMap` instances hit
  growth/rehash at different times, creating allocation bursts that a single
  `HashMap` avoids.
- **Cache capacity effects.** 1M entries with ~200 bytes per entry exceed L2
  cache (typically 256 KB - 1 MB) and pressure L3 (4-32 MB). Access patterns
  that fit in cache at 100K cause frequent evictions at 1M, amplifying the
  cost of cross-shard access.
- **Shard rebalancing under high load.** DashMap distributes entries by hash.
  At 1M entries with 128 shards (16-core default), each shard holds ~7,800
  entries. If the hash function produces skew, hot shards grow
  disproportionately, increasing per-shard lock hold times and degrading
  throughput for threads competing on those shards.
- **NUMA effects.** On multi-socket hosts, shards allocated on one NUMA node
  incur remote-memory latency when accessed by threads on another node. At
  1M entries the working set cannot fit in a single node's LLC, making NUMA
  placement visible.

The million-file scale is realistic: large deployments (package mirrors,
media archives, CI artifact stores) routinely transfer 1M+ files in a single
sync. The delete pipeline processes the full file list, so map performance at
this scale directly impacts `--delete` wall-clock time.

## 2. Bench harness

### 2.1 Source file

```
crates/engine/benches/dmb_a_dashmap_delete_bench.rs
```

The same unified harness from DMB.a. The 1M tier is already defined in the
harness's scale-tier axis (DMB.a section 4). DMB.d executes this tier with
both backing stores and captures the comparison.

### 2.2 Invocation

Full 1M sweep (all stores, all thread counts):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench -- '1M'
```

DashMap-only at 1M (baseline capture):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-d-dashmap '1M.*dashmap'
```

Mutex comparison against DashMap baseline:

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --baseline dmb-d-dashmap '1M.*mutex_hashmap'
```

Single-cell drill-down (e.g., Mutex at 64 threads on 1M):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'mutex_hashmap.*1M.*64_threads'
```

## 3. Thread count matrix

Same power-of-two sweep as DMB.b/DMB.c:

| Threads | 1M-specific considerations |
|---------|---------------------------|
| 1 | Uncontended baseline. At 1M entries, single-mutex vs DashMap overhead difference is pure per-op bookkeeping cost (shard hash + shard selection). DashMap may show slight regression due to extra indirection. |
| 4 | Light contention. 1M entries across 128 shards means ~7,800 entries/shard. Shard-level `HashMap` growth is amortised. Mutex may still win due to fewer cache lines touched per op. |
| 8 | Moderate contention. Expected crossover zone - DashMap's parallel shard access should begin to offset its per-op overhead at this scale. |
| 16 | DashMap shard-count boundary (on 4-core host). At 1M entries, Mutex hold time is longer (larger bucket scan) making contention more costly - this should amplify DashMap's advantage vs 100K. |
| 32 | Over-subscription on most hosts. Mutex convoy effects at 1M should be severe (longer critical sections). DashMap advantage expected to be wider than at 100K. |
| 64 | Heavy over-subscription. Tests whether DashMap's shard contention at 1M (more entries per shard, longer shard-level rehash) narrows the gap vs Mutex convoy. |

## 4. Workload sizing

### 4.1 Entry count

1,000,000 entries in the map. This is 10x the DMB.c production tier.

### 4.2 DeletePlanEntry payloads (Target A)

Same construction as DMB.b/DMB.c but at 1M scale:

- **Directory key:** `PathBuf::from(format!("dir/{group}/{n}"))` where
  `group = n / 1000` (1,000 groups of 1,000 entries each). The grouped
  structure exercises hash distribution across the shard space.
- **Entries per plan:** 1 `DeleteEntry` with synthetic filename and
  `DeleteEntryKind::File`. Keeps per-plan cost constant to isolate map ops.
- **Pre-built plans:** All 1M `DeletePlan` values constructed outside the
  timed section via `iter_batched` with `BatchSize::LargeInput`.

Total memory for pre-built plans: ~200 MB (1M entries x ~200 bytes each).
This requires sufficient system memory - the bench should be run on hosts
with 8+ GB RAM.

### 4.3 SlotEntry payloads (Target B)

- **Key:** `FileNdx::new(n as u32)` - integer key, zero allocation.
- **Value:** `SlotEntry` wrapping `CountingSink` writer.
- **Three phases per iteration:** register (1M inserts), lookup (1M random
  accesses), finish (1M removes + Arc unwrap).

### 4.4 RNG seed

Fixed seed root `0xDEAD_BEEF_CAFE_D00D` (from DMB.a). Combined with
`(workload_tag, group, n)` per key for reproducibility.

## 5. 1M-specific measurement considerations

### 5.1 Criterion configuration

The 1M tier uses extended timings per DMB.a section 7.1:

```rust
group.throughput(Throughput::Elements(total_ops as u64));
group.sample_size(10);
group.measurement_time(Duration::from_secs(15));
group.warm_up_time(Duration::from_secs(5));
```

Reduced sample size (10 vs 20 at 100K) keeps total bench wall-clock time
under 30 minutes for the full thread sweep across both stores. Extended
warm-up (5s vs 3s) ensures the allocator reaches steady state after the 1M
pre-population.

### 5.2 Memory overhead measurement

Capture peak RSS at each thread count for both stores. At 1M entries the
absolute memory difference is meaningful:

```rust
// Before timed section (after map population):
let rss_populated = peak_rss_kb();

// After timed section (map drained):
let rss_drained = peak_rss_kb();
```

Expected memory at 1M:

| Store | Estimated RSS (populated) | Notes |
|-------|--------------------------|-------|
| `mutex_hashmap` | ~200-250 MB | Single HashMap allocation, one growth cycle. |
| `dashmap` (128 shards) | ~210-270 MB | 128 shard HashMap allocations, slight fragmentation overhead. |
| `dashmap` (256 shards) | ~215-280 MB | 256 shard allocations; per-shard entry count halved (~3,900/shard). |

The overhead delta should be < 15% at 1M. If DashMap exceeds this, the
per-shard allocation fragmentation is a concern worth investigating.

### 5.3 Cache pressure quantification

At 1M entries the working set vastly exceeds L2 cache. Measure via perf:

```sh
perf stat -e L1-dcache-load-misses,LLC-load-misses,LLC-loads,dTLB-load-misses \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '1M.*32_threads'
```

Key metrics per store:

| Metric | Implication |
|--------|-------------|
| L1-dcache-load-misses/op | Higher for DashMap if shard metadata evicts data cache lines. |
| LLC-load-misses/op | Dominates latency at 1M. If DashMap spreads entries across more LLC sets (via sharding), this may be lower. |
| dTLB-load-misses/op | 1M entries span many pages. DashMap's multiple HashMap allocations increase TLB pressure vs a single contiguous HashMap. |

### 5.4 Shard rebalancing under load

At 1M entries, individual shard HashMaps undergo growth/rehash as entries
are inserted. Under concurrent load this means:

- A thread holding a shard's write lock during rehash blocks all other
  threads attempting to access that shard.
- At 128 shards with 1M entries, each shard rehashes approximately
  log2(7800) = ~13 times during population.
- Rehash frequency is 10x higher per shard than at 100K (780 entries/shard).

The bench pre-populates the map outside the timed section (via
`iter_batched`), so rehash cost is excluded from throughput numbers.
However, the mixed insert/take workload (Target A phase-2 pattern where
inserts and takes interleave) does trigger shard-level rehash within the
timed section when the map grows past its pre-allocated capacity.

To isolate rehash effects, add a pre-capacity variant:

```rust
// Pre-capacity to prevent rehash during timed section:
let map = DashMap::with_capacity_and_shard_amount(1_200_000, shard_count);
```

Compare throughput with and without pre-capacity to quantify rehash cost
under contention at 1M scale.

## 6. Expected outcome patterns

### 6.1 Crossover shift prediction

At 100K (DMB.c), the DashMap vs Mutex crossover is expected at ~4-8 threads.
At 1M, the crossover should shift left (lower thread count) because:

1. **Longer Mutex hold times.** A single `HashMap` with 1M entries has larger
   bucket chains (even with load factor < 1.0, the absolute lookup cost is
   higher due to cache misses). Each thread holding the Mutex blocks all
   others for longer.
2. **Higher contention cost.** At 1M entries, the time spent waiting for the
   Mutex (spinning + parking) accumulates faster because the critical section
   is longer. Even 2-4 threads may see measurable queuing delay.
3. **DashMap amortises cache misses.** With 128 shards, each shard's HashMap
   fits in ~1.5 MB (7,800 entries x ~200 bytes). This may fit partially in
   L3, reducing per-access latency relative to a single 200 MB HashMap.

Predicted crossover at 1M: **2-4 threads** (vs 4-8 at 100K).

### 6.2 Scaling shape at 1M

```
ops/sec
  ^
  |                    DashMap (1M)
  |                .---*---*----.----*
  |              ./                    \
  |            ./
  |          ./
  |        ./        Mutex (1M)
  |      ./    .*---.----.----.----.
  |    ./    ./
  |   /    ./
  |  /   ./
  | /  ./
  |/ ./
  +--+--+--+--+--+--+--+--> threads
     1  2  4  8  16 32 64
```

Expected patterns:

| Region | DashMap behaviour | Mutex behaviour |
|--------|-------------------|-----------------|
| 1 thread | Slight overhead vs Mutex (shard hash + selection). ~5-10% slower. | Baseline - no contention, single HashMap. |
| 2-4 threads | Crosses over Mutex. Shard parallelism offsets overhead. | Severe queueing. 1M entries mean ~50-100 us critical sections, causing convoy. |
| 4-16 threads | Near-linear scaling. Each thread operates on its own shard subset. | Flat or declining. Convoy worsens. |
| 16-32 threads | Sub-linear. Some shard contention as threads exceed shard count / 4. | Flat. Mutex is fully serialised regardless of thread count. |
| 32-64 threads | Diminishing returns. Shard contention + OS scheduling overhead. | Flat or slightly worse (context-switch overhead on queued waiters). |

### 6.3 Comparison with 100K patterns

| Metric | 100K (DMB.c) | 1M (DMB.d) prediction |
|--------|--------------|----------------------|
| Crossover thread count | 4-8 | 2-4 |
| DashMap advantage at 16 threads | 2-4x | 5-10x |
| DashMap advantage at 64 threads | 3-6x | 8-15x |
| DashMap single-thread overhead | 2-5% | 5-10% |
| Memory overhead (DashMap vs Mutex) | < 5% | 5-15% |

The wider advantage at 1M validates DashMap's selection for any workload
that reaches million-file scale. If the advantage is narrower than
predicted, the longer per-shard bucket chains at 1M may be negating the
sharding benefit (each shard's HashMap is still a sequential data structure
under its shard lock).

## 7. Shard tuning at 1M

### 7.1 Shard count variants

At 1M entries, shard count directly affects entries-per-shard and thus
per-shard lock hold time:

| Shard count | Entries/shard | Per-shard HashMap RSS | Expected effect |
|-------------|--------------|----------------------|-----------------|
| 64 | ~15,600 | ~3.1 MB | Longer hold times; may saturate earlier. |
| 128 (default on 16-core) | ~7,800 | ~1.5 MB | Baseline. |
| 256 | ~3,900 | ~780 KB | Shorter hold times; fits better in L2/L3. May reduce contention at 32+ threads. |
| 512 | ~1,950 | ~390 KB | Each shard fits in L2. Maximum parallelism but 512 RwLock allocations add fixed overhead. |

### 7.2 Decision gate

The DMB.a section 9 decision gate applies at 1M:

- If `with_shard_amount(256)` shows > 15% throughput improvement over the
  default at 32+ threads, expose a `shard_amount` configuration parameter.
- If `with_shard_amount(512)` shows > 25% improvement, consider it as the
  new default for the 1M+ tier (auto-scaled by entry count at construction).

### 7.3 Pre-capacity interaction

At 1M entries, pre-allocating capacity eliminates rehash:

```rust
DashMap::with_capacity_and_shard_amount(1_200_000, 256)
```

This pre-allocates ~1.2M slots across 256 shards (~4,700 slots/shard). Each
shard's HashMap avoids growth reallocation during population. Compare
pre-capacity vs default-capacity to isolate rehash cost from contention cost.

## 8. Memory overhead analysis

### 8.1 Fixed overhead

DashMap per-shard metadata (RwLock + HashMap struct + allocation header):

| Shard count | Fixed overhead |
|-------------|---------------|
| 64 | ~16 KB |
| 128 | ~32 KB |
| 256 | ~64 KB |
| 512 | ~128 KB |

At 1M entries (~200 MB total), fixed overhead is negligible (< 0.1%).

### 8.2 Fragmentation overhead

The significant memory difference at 1M comes from allocation fragmentation:

- **Mutex<HashMap>:** Single allocation, grows via doubling. At 1M entries
  with load factor 0.875, the backing array is ~1.14M slots. One allocation
  of ~228 MB (1.14M x 200 bytes).
- **DashMap (128 shards):** 128 independent HashMap allocations. Each grows
  independently. If entries are not perfectly distributed, some shards
  over-allocate. Worst-case fragmentation: 128 shards each at 75% capacity
  means ~33% wasted space vs the theoretical minimum.

Expected fragmentation overhead: 5-15% at 1M with realistic hash
distribution. Measure empirically via:

```sh
# Capture allocator stats (jemalloc):
MALLOC_CONF="stats_print:true" cargo bench -p engine \
    --bench dmb_a_dashmap_delete_bench -- --filter '1M.*dashmap.*1_thread'
```

### 8.3 Decision threshold

Memory overhead > 20% vs Mutex<HashMap> at 1M triggers evaluation of
whether the throughput advantage justifies the RSS cost. For the delete
workload, the map is short-lived (populated during transfer, drained during
delete phase), so peak RSS is transient. A 20% overhead at 1M is ~40 MB -
acceptable for the throughput improvement.

## 9. Latency percentiles at 1M

### 9.1 Expected p99 behaviour

At 1M entries, p99 latency is dominated by:

- **DashMap:** Shard-level RwLock contention (waiting for a writer to finish
  rehash or a concurrent writer on the same shard). Expected p99: 1-10 us at
  16 threads, 10-50 us at 64 threads.
- **Mutex<HashMap>:** Queue wait time. At 16 threads with ~50-100 us critical
  sections, p99 is `(threads - 1) * critical_section_time` in the worst case.
  Expected p99: 500 us - 5 ms at 16 threads, 3-30 ms at 64 threads.

### 9.2 Tail latency ratio

| Threads | Expected p99/p50 (DashMap) | Expected p99/p50 (Mutex) |
|---------|---------------------------|--------------------------|
| 1 | 1.5-2x | 1.5-2x |
| 4 | 2-3x | 5-10x |
| 8 | 3-5x | 10-50x |
| 16 | 5-10x | 50-200x |
| 32 | 8-15x | 100-500x |
| 64 | 10-20x | 200-1000x |

The Mutex tail latency at 1M should be dramatically worse than at 100K
because the critical section is longer (more cache misses per operation
inside the lock). This is the strongest argument for DashMap at scale.

## 10. Decision criteria

### 10.1 DashMap justified at 1M (expected outcome)

DashMap remains the correct choice when:

- Throughput advantage over Mutex is >= 2x at 8+ threads.
- Throughput advantage grows with thread count (not saturating).
- Memory overhead is < 20%.
- No negative scaling below 32 threads.
- p99 latency is < 10x Mutex p99 at single thread (overhead acceptable).

### 10.2 DashMap advantage saturates

If the DashMap advantage at 1M is no wider than at 100K (e.g., crossover
stays at 4-8 threads, advantage plateaus at the same multiple), then:

- The longer critical sections at 1M are being offset by higher per-shard
  overhead (cache misses within each shard's larger HashMap).
- Shard tuning (section 7) becomes the priority to reduce per-shard size.
- Consider pre-capacity as a mandatory optimisation for 1M+ workloads.

### 10.3 DashMap disadvantaged at 1M

If DashMap shows throughput regression vs Mutex at any thread count, or if
memory overhead exceeds 30%, investigate:

1. Per-shard HashMap rehash during mixed workload (section 5.4).
2. Hash distribution skew causing hot shards (check via
   `DashMap::shards()` length variance if exposed, or synthetic analysis).
3. TLB pressure from 128+ independent allocations (dTLB-load-misses metric).

Remediation options:
- Increase shard count to reduce per-shard size.
- Pre-capacity to eliminate rehash.
- Evaluate `flurry` or `papaya` (DMB.a section 11.3) if contention is
  fundamental rather than configuration-related.

### 10.4 Summary decision matrix

| Outcome | Action |
|---------|--------|
| DashMap 5x+ advantage at 16 threads, memory < 20% | Keep DashMap, close DMB series. |
| DashMap 2-5x advantage, shard tuning adds > 15% | Expose shard_amount knob, default scaled by entry count. |
| DashMap < 2x advantage at 16 threads | Root-cause per-shard overhead; pre-capacity may be sufficient. |
| DashMap regression at any thread count | Evaluate lock-free alternatives (flurry/papaya). |
| Memory overhead > 20% | Reduce shard count or accept tradeoff if throughput justifies it. |

## 11. Offline capture procedure

### 11.1 Target hardware

Bare-metal Linux host with 16+ physical cores, following the BR-3j.f
procedure. The 1M tier requires:

- 16+ GB RAM (pre-built plans + map + OS overhead).
- CPU frequency governor set to `performance` (no turbo variance).
- No concurrent workloads during capture.

### 11.2 Commands

```sh
# Full 1M comparison sweep:
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-d-$(hostname)-$(date +%Y%m%d) '1M'

# DashMap shard tuning (256 shards):
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '1M.*dashmap_256'

# Cache pressure profiling:
perf stat -e L1-dcache-load-misses,LLC-load-misses,LLC-loads,dTLB-load-misses \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '1M.*64_threads'

# Lock contention profiling:
perf lock record -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '1M.*dashmap.*64_threads'
perf lock report --sort acquired
```

### 11.3 Wall-clock budget

At 1M entries with 15s measurement time and 10 samples per cell:

- Per cell: ~5s warm-up + (15s x 10 samples) = ~155s
- Per store per thread count: ~155s
- Full sweep (2 stores x 7 thread counts): ~36 minutes

With shard tuning variants (3 additional DashMap configs x 7 thread counts):
~72 minutes total. Schedule as a dedicated offline run.

## 12. CI integration

### 12.1 CI does not run the 1M tier

Per DMB.a section 10.3, CI runs only the 100K tier to stay within the
5-minute bench soft cap. The 1M tier is reserved for offline capture.

### 12.2 Nightly regression detection

The nightly schedule (06:17 UTC) runs the 100K tier only. If a PR touches
DashMap-related code paths and the 100K tier shows > 10% regression, trigger
a manual 1M run via `workflow_dispatch`:

```sh
gh workflow run bench-dashmap-delete.yml \
    --ref feature-branch \
    -f scale_filter='1M'
```

This requires extending the workflow to accept a `scale_filter` input
parameter (tracked as a follow-up to DMB.a).

## 13. Results template

Numbers captured offline and appended once measured.

### 13.1 Target A - DeletePlanMap (1M entries)

| Threads | Store | Ops/sec (median) | Efficiency | p50 (ns) | p99 (ns) | p99/p50 | CoV | RSS (MB) |
|---------|-------|-----------------|------------|----------|----------|---------|-----|----------|
| 1 | mutex_hashmap | | | | | | | |
| 1 | dashmap | | | | | | | |
| 4 | mutex_hashmap | | | | | | | |
| 4 | dashmap | | | | | | | |
| 8 | mutex_hashmap | | | | | | | |
| 8 | dashmap | | | | | | | |
| 16 | mutex_hashmap | | | | | | | |
| 16 | dashmap | | | | | | | |
| 32 | mutex_hashmap | | | | | | | |
| 32 | dashmap | | | | | | | |
| 64 | mutex_hashmap | | | | | | | |
| 64 | dashmap | | | | | | | |

### 13.2 Target B - ParallelDeltaApplier files map (1M entries)

| Threads | Store | Ops/sec (median) | Efficiency | p50 (ns) | p99 (ns) | p99/p50 | CoV | RSS (MB) |
|---------|-------|-----------------|------------|----------|----------|---------|-----|----------|
| 1 | mutex_hashmap | | | | | | | |
| 1 | dashmap | | | | | | | |
| 4 | mutex_hashmap | | | | | | | |
| 4 | dashmap | | | | | | | |
| 8 | mutex_hashmap | | | | | | | |
| 8 | dashmap | | | | | | | |
| 16 | mutex_hashmap | | | | | | | |
| 16 | dashmap | | | | | | | |
| 32 | mutex_hashmap | | | | | | | |
| 32 | dashmap | | | | | | | |
| 64 | mutex_hashmap | | | | | | | |
| 64 | dashmap | | | | | | | |

### 13.3 Crossover summary

| Target | Crossover thread count (1M) | Crossover thread count (100K, from DMB.c) | DashMap advantage at 16 threads | Memory overhead |
|--------|----------------------------|------------------------------------------|-------------------------------|----------------|
| A (DeletePlanMap) | | | | |
| B (Applier files) | | | | |

### 13.4 Shard tuning results (1M, DashMap only)

| Threads | 128 shards (ops/sec) | 256 shards (ops/sec) | 512 shards (ops/sec) | Best config |
|---------|---------------------|---------------------|---------------------|-------------|
| 8 | | | | |
| 16 | | | | |
| 32 | | | | |
| 64 | | | | |

## 14. Cross-references

- `docs/design/dmb-a-dashmap-delete-bench-harness.md` - DMB.a harness spec.
  Defines bench file layout, backing-store trait, scale tiers, CI workflow.
- `docs/design/dmb-b-dashmap-thread-sweep.md` - DMB.b DashMap-only sweep at
  100K. Captures the DashMap baseline that DMB.c/DMB.d compare against.
- `crates/engine/benches/dmb_a_dashmap_delete_bench.rs` - The bench file
  (created by DMB.a) that DMB.d invokes at the 1M tier.
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4 micro-bench.
  Predecessor to Target A.
- `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` - BR-3j.f
  re-bench. Predecessor to Target B.
- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap production code
  (`Mutex<HashMap>` backing store).
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  ParallelDeltaApplier production code (`DashMap<FileNdx, SlotEntry>`).
- `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - BR-3j.f methodology
  and offline number-capture procedure.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap selection
  audit with contention model assumptions.
- `.github/workflows/bench-dashmap-delete.yml` - CI workflow (DMB.a section
  10).
