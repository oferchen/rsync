# DMC-CON - Adaptive DashMap shard sizing for ParallelDeltaApplier

Date: 2026-06-10
Status: Implementation spec (DMC-CON.2/.3/.5 shipped)
Tracker: DMC-CON.1 (#3995, contention profile), DMC-CON.2 (#3996, heuristic
spec), DMC-CON.3 (#3997, implementation), DMC-CON.5 (#3999, operator
documentation). Predecessor: DMB.f
(`docs/design/dashmap-scalability-decision.md`). Sibling audit:
DMB.e (`docs/design/dashmap-shard-contention-profile.md`).

## 1. Motivation

BR-3j (#2496, PRs #4634-#4636) migrated `ParallelDeltaApplier::files`
from `Mutex<HashMap<FileNdx, SlotEntry>>` to
`DashMap<FileNdx, SlotEntry>`. The migration was constructed with
`DashMap::new()`, which delegates shard sizing to DashMap's default:
`available_parallelism() * 4`, rounded up to the next power of two.

That default tracks the **host's** CPU count, not the **applier's** actual
worker concurrency. Two consequences emerge at the edges:

1. **Million-file workloads.** When the receiver opens hundreds of
   thousands of files in flight, the per-op cost (hash + shard select +
   `RwLock` acquire) starts to matter against the per-shard fixed cost.
   DashMap on a 128-core host produces 512 shards by default; if the
   applier is only dispatching 8 workers, those 512 `RwLock` allocations
   buy nothing and waste L1d capacity on lock metadata nobody contends.
2. **Low-concurrency callers.** Tests, micro-benches, and the `concurrency=1`
   shutdown path build appliers that will never have more than a single
   chunk in flight. DashMap's `available_parallelism()`-derived default
   still allocates ~64 shards on an 8-core dev box, again paying the per-
   shard fixed cost with no contention to amortise it.

The DMB.f decision document (section 8.3) anticipated this dimension and
sketched a "tune" outcome:

> If DMB.e shows that non-default shard counts provide > 15% throughput
> improvement at 32+ threads:
> 1. Expose `shard_amount` as a constructor parameter.
> 2. Default to the DashMap library default.
> 3. Document the tuning knob in rustdoc.

DMC-CON.2 refines that direction: rather than expose a constructor
parameter (which forks every call site between callers who know
`worker_count` and callers who don't), the heuristic adapts automatically
inside `ParallelDeltaApplier::with_strategy` based on the supplied
`concurrency` value, and an environment variable handles the rare cases
where an operator wants to override.

## 2. Heuristic

```rust
shard_count = (worker_count * 4)
    .next_power_of_two()
    .clamp(MIN_SHARDS, MAX_SHARDS)
where
    MIN_SHARDS = 4
    MAX_SHARDS = 1024
```

### 2.1 Table

| `worker_count` | Raw (`* 4`) | Next pow2 | After clamp |
|---------------:|------------:|----------:|------------:|
| 0 (ambient pool) | 0 | 1 | **4** (MIN) |
| 1 | 4 | 4 | **4** (MIN) |
| 4 | 16 | 16 | **16** |
| 8 | 32 | 32 | **32** |
| 16 | 64 | 64 | **64** |
| 32 | 128 | 128 | **128** |
| 64 | 256 | 256 | **256** |
| 128 | 512 | 512 | **512** |
| 256 | 1024 | 1024 | **1024** (MAX) |
| 1024+ | clamps | clamps | **1024** (MAX) |

### 2.2 Why `worker_count * 4`?

The factor four matches DashMap's own internal default factor (`* 4`
relative to `available_parallelism()`). DashMap chose four because the
contention-vs-overhead crossover under uniform key distribution lands
around four shards per concurrent thread (see DMB.e section 2.3 contention
model: at four-shards-per-thread the collision probability is ~22% per op,
which the shared `RwLock` reader path absorbs cheaply). DMC-CON keeps the
same factor; only the input value changes from "host CPU count" to
"applier worker count".

### 2.3 Why `MIN_SHARDS = 4`?

DashMap loses its partitioning advantage entirely below four shards: the
hash + modulus per op approaches the cost of a single `Mutex<HashMap>`,
without delivering any reader concurrency. The lower bound also covers
the `concurrency = 0` ambient-pool sentinel: a stale `worker_count = 0`
read would otherwise produce a 1-shard `DashMap` and panic
`DashMap::with_shard_amount` (which requires `> 0`).

### 2.4 Why `MAX_SHARDS = 1024`?

Caps the per-shard fixed cost. Each empty shard contributes one `RwLock`
(~8 bytes) and one empty `HashMap` (~64 bytes), so 1024 shards costs
~72 KiB of fixed allocation - the same order of magnitude DashMap allocates
at default settings on a 128-core host (`512 shards * ~72 bytes ~= 36 KiB`).
The cap also defends against malformed `worker_count` values (e.g.
`usize::MAX` from a future API mistake) reaching `with_shard_amount` and
allocating unbounded memory.

## 3. Operator override: `OC_RSYNC_DASHMAP_SHARDS`

### 3.1 Behaviour

```text
OC_RSYNC_DASHMAP_SHARDS=<n>  ->  applier uses n shards (clamped, rounded)
OC_RSYNC_DASHMAP_SHARDS=0    ->  parse rejected, falls back to heuristic
OC_RSYNC_DASHMAP_SHARDS=foo  ->  parse fails, falls back to heuristic
OC_RSYNC_DASHMAP_SHARDS unset ->  heuristic
```

The override is clamped to `[MIN_SHARDS, MAX_SHARDS]` and rounded up to
the next power of two before reaching `DashMap::with_shard_amount`. DashMap
6.1 panics on non-power-of-two shard counts, so the rounding is
non-negotiable; choosing "round up" instead of "round to nearest" matches
the heuristic's own rounding direction so the override never silently
under-shards relative to the default.

### 3.2 When to use it

| Scenario | Recommended setting |
|----------|---------------------|
| Research / micro-bench A/B against DMB.b nightly baselines | Pin a value matching the baseline (commonly `128`). |
| Production tuning on a host with 32-128 cores where the default heuristic appears to undershoot | `OC_RSYNC_DASHMAP_SHARDS=512` after profiling under `perf lock` confirms contention. |
| Diagnosing a contention regression | Try `4`, `16`, `64`, `256` in turn; sustained throughput dip past one of the steps localises the contention to that shard tier. |
| Default deployment | **Leave unset.** The heuristic is the recommended path. |

### 3.3 What it does **not** do

- It does not change `concurrency` (the rayon worker fan-out). The two
  knobs are independent: concurrency caps how many chunks the applier
  dispatches in parallel; shard count caps how finely the per-file map
  partitions those dispatches.
- It does not affect `DeletePlanMap` or any other DashMap consumer outside
  `ParallelDeltaApplier`. Each DashMap consumer that needs tuning would
  add its own override.

## 4. Implementation

### 4.1 Source layout

```text
crates/engine/src/concurrent_delta/parallel_apply/
    shard_sizing.rs      <- DMC-CON.2/.3 helper module
    mod.rs               <- with_strategy() calls resolve_shard_count()
```

`shard_sizing.rs` exposes (crate-internal):

- `const MIN_SHARDS: usize = 4`
- `const MAX_SHARDS: usize = 1024`
- `const SHARDS_ENV: &str = "OC_RSYNC_DASHMAP_SHARDS"`
- `fn default_shard_count(worker_count: usize) -> usize`
- `fn resolve_shard_count(worker_count: usize) -> usize`

The two functions are split so the unit tests can verify the heuristic
without touching environment state.

### 4.2 Constructor wiring

```rust
pub fn with_strategy(concurrency: usize, strategy: Arc<dyn ChecksumStrategy>) -> Self {
    let shard_count = shard_sizing::resolve_shard_count(concurrency);
    Self {
        files: DashMap::with_shard_amount(shard_count),
        ...
    }
}
```

`ParallelDeltaApplier::new(concurrency)` delegates to `with_strategy`, so
both constructors pick up the heuristic with no caller-side change.

### 4.3 Feature gating

Behind the unconditionally-compiled `parallel-receive-delta` path (PFF-7).
No new feature gate; the heuristic is always active.

## 5. Tests

### 5.1 Unit (`shard_sizing.rs`)

| Test | Coverage |
|------|----------|
| `default_shard_count_clamps_to_min_for_low_worker_count` | `worker_count = 0, 1 -> 4` |
| `default_shard_count_matches_table_for_typical_worker_counts` | `4 -> 16`, `16 -> 64`, `64 -> 256`, etc. |
| `default_shard_count_rounds_up_to_power_of_two` | `3 -> 16`, `5 -> 32`, `17 -> 128` |
| `default_shard_count_clamps_to_max_for_huge_worker_count` | `256, 1024, usize::MAX -> 1024` |
| `resolve_shard_count_uses_env_override_when_set` | `OC_RSYNC_DASHMAP_SHARDS=64, worker=4 -> 64` |
| `resolve_shard_count_rounds_env_override_to_power_of_two` | `=42 -> 64` |
| `resolve_shard_count_clamps_env_override_to_max` | `=999999 -> 1024` |
| `resolve_shard_count_clamps_env_override_to_min` | `=2 -> 4` |
| `resolve_shard_count_falls_back_on_invalid_env` | `="not-a-number" -> heuristic` |
| `resolve_shard_count_falls_back_on_zero_env` | `="0" -> heuristic` |
| `resolve_shard_count_uses_heuristic_when_env_unset` | unset -> heuristic |
| `resolve_shard_count_trims_whitespace` | `="  64  " -> 64` |

Env-touching tests serialise via a `static Mutex` because `std::env::set_var`
is process-wide.

### 5.2 Integration (`crates/engine/tests/parallel_apply_shard_sizing.rs`)

| Test | Coverage |
|------|----------|
| `dispatch_succeeds_under_default_shard_count` | 64 files, 4 workers, 1024 ops via `apply_one_chunk` under the default heuristic (16 shards). Asserts total byte count. |
| `dispatch_succeeds_under_env_shard_override` | Same workload with `OC_RSYNC_DASHMAP_SHARDS=8` set during construction. Asserts the override path produces identical bytes. |

The existing `concurrent_register_and_dispatch_on_overlapping_files` test
(`crates/engine/tests/parallel_apply_concurrent.rs`) continues to validate
the high-fanout stress shape under whatever shard count the heuristic
picks; no changes required there.

## 6. Operator runbook

### 6.1 Detecting shard contention

DashMap does not emit per-shard metrics. Indicators that the heuristic
may be under-sharding for a workload:

1. `perf lock report --sort acquired` shows the applier's `files` DashMap
   shards near the top of the contention list.
2. Comparing the applier's throughput against the `dmb_a_dashmap_delete_bench`
   nightly baseline shows > 10% regression at the relevant worker count.
3. A scale-up (e.g. doubling `--concurrent-files`) produces sub-linear
   throughput gain on hosts with otherwise idle CPUs.

### 6.2 Tuning procedure

1. Reproduce the workload under `perf lock record` with the heuristic
   default. Capture `events / op` for the applier's DashMap shards.
2. Re-run with `OC_RSYNC_DASHMAP_SHARDS=2 * heuristic`. If contention
   events per op drop > 50% and end-to-end throughput improves, the
   heuristic is under-sharding for this workload.
3. Re-run with `OC_RSYNC_DASHMAP_SHARDS=4 * heuristic`. If throughput
   does not improve further, the previous step was the right target.
4. File an issue tagged `dmc-con` with the workload description and the
   tuned value; sustained signal across multiple deployments will inform
   a heuristic update.

### 6.3 Reverting to DashMap's default

Set `OC_RSYNC_DASHMAP_SHARDS=` to a value matching
`available_parallelism() * 4` for the host (e.g. `OC_RSYNC_DASHMAP_SHARDS=128`
on a 32-core host). Useful for direct A/B against pre-DMC-CON behaviour.

## 7. Cross-references

- `crates/engine/src/concurrent_delta/parallel_apply/shard_sizing.rs` -
  Implementation.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` - Constructor
  wiring.
- `crates/engine/tests/parallel_apply_shard_sizing.rs` - Integration
  coverage.
- `docs/design/dashmap-scalability-decision.md` (DMB.f) - Outer decision
  framework; this doc implements section 8.3 in the form of an automatic
  heuristic + env override.
- `docs/design/dashmap-shard-contention-profile.md` (DMB.e) - Contention
  model and per-shard cost breakdown that motivates the heuristic's
  `* 4` factor and 1024-shard cap.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap
  selection audit for the applier files map.
