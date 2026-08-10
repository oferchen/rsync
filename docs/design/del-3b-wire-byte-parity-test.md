# Wire-byte parity test for parallel delete consumer (DEL-3.b)

Status: Design (task DEL-3.b; depends on DEL-3.a capture harness +
DEL-2.c parallel consumer implementation; gate for DEL-4 default-on
promotion)
Audience: engine and transfer maintainers validating that the
`parallel-delete-consumer` feature produces wire-identical output to
the sequential baseline.
Scope: test architecture, comparison strategy, fixture reuse from
DEL-3.a, failure diagnostics, edge-case coverage, and CI integration
for the parallel vs sequential parity gate.

Out of scope: the capture harness itself (DEL-3.a owns
`RecordingWriter`, `CapturedWireImage`, and the fixture catalog), the
parallel consumer implementation (DEL-2.c), and the batching policy
(DEL-1.c).

## 1. Goal

Prove that the `ParallelDeleteEmitter` (behind `--features
parallel-delete-consumer`) produces a wire byte stream
indistinguishable from the sequential `DeleteEmitter` for every
deterministic fixture in the DEL-3.a catalog. The test is the
promotion gate: the feature flag stays off-by-default until this
suite passes across the full CI matrix (Linux, macOS, Windows) and
the full rayon thread-width sweep.

The parity requirement is:

1. **NDX channel bytes identical.** The `NDX_DEL_STATS` frame (sentinel
   + five varints) and the closing `NDX_DONE` must be byte-for-byte
   equal between sequential and parallel captures.
2. **MSG channel set-equivalent.** `MSG_DELETED` notifications carry
   per-entry path bytes. Within a goodbye cohort, their order is
   reorderable (DEL-1.a section 5.1), so the comparison uses
   order-independent set equality - but the byte content of each
   notification must be identical.
3. **Stats counters equal.** The `DeleteStats` struct returned by both
   emitters must have identical per-kind counts (semantic check on top
   of the wire-byte check).

## 2. Test architecture

### 2.1 Dual-capture pattern

Every parity test runs the same fixture through both paths and
compares the resulting `CapturedWireImage` instances:

```text
fixture ──> clone state A ──> sequential emitter ──> CapturedWireImage (baseline)
        └─> clone state B ──> parallel emitter   ──> CapturedWireImage (candidate)

assert_parity(baseline, candidate)
```

Both paths receive an identical `DeletePlanMap` and
`DirTraversalCursor` state. The DEL-3.a harness's `build_fixture`
factory returns cloneable fixture state (both plan map and cursor
implement `Clone` in test configurations). The test clones once and
feeds each clone to its respective emitter, guaranteeing input
equivalence.

### 2.2 Emitter dispatch

The sequential path uses the existing `DeleteEmitter::emit_all` +
goodbye writer with `RecordingWriter` instances (the DEL-3.a pattern
unchanged).

The parallel path uses `ParallelDeleteEmitter::emit_all` - the
DEL-2.c implementation compiled under the `parallel-delete-consumer`
feature flag - with a separate pair of `RecordingWriter` instances.
The parallel emitter spawns the reorder buffer (DEL-1.b), the rayon
producer scope, and the consumer thread, then joins before returning
the captured image.

Both paths share the same `RecordingDeleteFs` (or
`ScriptedDeleteFs` for error-injection fixtures), ensuring identical
syscall schedules.

### 2.3 Thread-width sweep

The parallel consumer's ordering correctness depends on the number
of rayon workers. A bug that only manifests at a specific
concurrency level would escape a single-width test. The parity test
sweeps `RAYON_NUM_THREADS` across `{1, 2, 4, host_width}`:

- **1 thread:** Degenerates to sequential scheduling inside rayon.
  Tests the parallel consumer's code path with zero actual
  concurrency, catching synchronisation scaffolding bugs.
- **2 threads:** Minimum concurrency. Exercises the reorder buffer's
  head-of-line blocking with one producer ahead and one behind.
- **4 threads:** The smallest width that can saturate the N=64
  reorder ring on small fixtures. Tests backpressure.
- **host_width:** (`std::thread::available_parallelism`). Exercises
  the production configuration.

The sweep is implemented as a parametrised test that sets
`RAYON_NUM_THREADS` via `EnvGuard` before building the rayon thread
pool. Each width runs all 10 DEL-3.a fixtures.

## 3. Comparison strategy

### 3.1 NDX channel: byte-identical

The `ndx_channel` field of `CapturedWireImage` must be `==` between
baseline and candidate. This covers:

- The `NDX_DEL_STATS` sentinel encoding (the ndx codec for `-3`).
- The five varint-encoded deletion counters (files, dirs, symlinks,
  devices, specials) in exact byte order and byte count.
- The `NDX_DONE` sentinel.
- Any other ndx traffic the goodbye writer emits (protocol-version
  dependent).

Strict byte equality is correct here because the ndx channel is
fully deterministic: the parallel consumer's `fold_batch` aggregates
per-cohort `DeleteStats` in the same consumer thread, and the
goodbye writer serialises from the aggregated struct identically to
the sequential path. The ndx codec has no non-determinism.

### 3.2 MSG channel: sorted set equality

`MSG_DELETED` notifications within a single goodbye cohort may arrive
in any order (DEL-1.a section 5.1). The parallel consumer's
producers fill cohort slots in rayon-scheduled order, and the
consumer drains in strict `cohort_idx` order - but the intra-cohort
`MSG_DELETED` frame order depends on the producer's dispatch order,
which matches the sequential emitter's per-directory order by
construction (DEL-1.b section 3.1). Two levels of comparison are
provided:

**Level 1 - Strict byte-identical (default):**

The parallel consumer's design guarantees per-cohort frame order
matches the sequential emitter (DEL-1.b section 3.1: "every
producer's local batch is already in upstream's intra-cohort order
before publication"). The default comparison asserts `msg_channel`
byte-for-byte equality. This is the strongest check and catches any
ordering regression in the producer's dispatch logic.

**Level 2 - Sorted set equality (fallback diagnostic):**

If Level 1 fails, the diagnostic output includes a Level 2 check
that splits `msg_channel` into individual `MSG_DELETED` frames
(using the length-prefix protocol), sorts each set by payload bytes,
and compares sorted sets. If Level 2 passes but Level 1 fails, the
failure is an intra-cohort ordering regression (likely a producer
dispatch-order bug), not a missing or corrupted frame.

```rust
fn assert_msg_parity(baseline: &[u8], candidate: &[u8]) {
    if baseline == candidate {
        return; // Level 1: strict byte-identical
    }
    // Level 2: sorted set equality for diagnostics
    let baseline_set = parse_msg_deleted_frames(baseline);
    let candidate_set = parse_msg_deleted_frames(candidate);
    let mut baseline_sorted = baseline_set.clone();
    let mut candidate_sorted = candidate_set.clone();
    baseline_sorted.sort();
    candidate_sorted.sort();
    assert_eq!(
        baseline_sorted, candidate_sorted,
        "MSG_DELETED frame set diverged (order-independent check)"
    );
    // If we reach here, Level 2 passed but Level 1 failed:
    // report the intra-cohort ordering difference.
    panic!(
        "MSG_DELETED frames are set-equivalent but order-divergent; \
         this is an intra-cohort ordering regression in the parallel \
         producer's dispatch logic"
    );
}
```

### 3.3 Stats counters: semantic equality

Independent of the wire-byte comparison, the test asserts:

```rust
assert_eq!(
    sequential_emitter.stats(),
    parallel_emitter_outcome.stats,
    "DeleteStats counters diverged between sequential and parallel"
);
```

This catches accumulation bugs in the parallel consumer's
`fold_batch` path (DEL-1.b section 3.2) even when the wire encoding
happens to produce the same bytes by coincidence (e.g., two different
counter distributions that varint-encode to the same byte sequence -
astronomically unlikely but the semantic check costs nothing).

### 3.4 Varint round-trip verification

Both captures decode their `stats_only` bytes back into `DeleteStats`
via `DeleteStats::read_from` and assert field equality against the
emitter's `stats()` accessor. This is inherited from DEL-3.a
section 6.1 and runs for both sequential and parallel captures,
catching codec bugs that affect one path but not the other.

## 4. Fixture reuse from DEL-3.a

All 10 deterministic fixtures from DEL-3.a section 4.1 are reused
without modification:

| ID | Name | Delete count | Parity-relevant property |
|----|------|-------------|--------------------------|
| F1 | `flat_alpha` | 10 regular files | Basic ordering, alphabetical names |
| F2 | `nested_dirs` | 6 files + 3 dirs | ENOTEMPTY recursive peel, depth-first ordering |
| F3 | `mixed_types` | 5 files + 2 sym + 1 dev + 1 spec + 1 dir | Per-kind stats accumulation across parallel workers |
| F4 | `size_varied` | 8 regular files | No size-dependent ordering (confirms capture stability) |
| F5 | `empty_set` | 0 entries | Zero-entry stats frame, NDX_DONE-only ndx channel |
| F6 | `single_file` | 1 regular file | Minimum non-trivial: one producer, one consumer drain |
| F7 | `dirs_only` | 4 nested empty dirs | Directory-only stats (files=0, dirs=4), leaf-first ordering |
| F8 | `symlinks_and_specials` | 3 sym + 2 FIFOs | Non-file stat buckets |
| F9 | `unicode_names` | Multi-byte UTF-8 | Wire encoding of non-ASCII path bytes |
| F10 | `large_flat` | 1000 regular files | Varint overflow (>127), buffer growth, reorder ring pressure |

The fixtures are instantiated via DEL-3.a's `build_fixture(FixtureId)`
factory. The parity test calls `build_fixture` once, clones the
returned `WireCaptureFixture` state, and feeds each clone to its
respective emitter path.

### 4.1 Additional parity-specific fixtures

Three fixtures are added specifically for parallel parity testing.
They are not part of the DEL-3.a baseline catalog because they
exercise parallel-specific edge cases that have no sequential
counterpart.

| ID | Name | Structure | Parity-relevant property |
|----|------|-----------|--------------------------|
| P1 | `wide_shallow` | 64 directories, 1 file each | Saturates the reorder ring (N=64) with minimal per-cohort work; tests backpressure under `claim_cohort` contention |
| P2 | `deep_narrow` | 1 directory chain, depth 50, 1 file per level | 50 single-entry cohorts in strict depth-first order; tests that the ring's monotonic drain matches sequential |
| P3 | `uneven_cohorts` | 5 directories: 1, 10, 100, 500, 1 entries respectively | Exposes head-of-line blocking when a large cohort (500 entries) delays the consumer while fast cohorts pile up behind it |

## 5. Feature-flag dispatch

### 5.1 Compile-time gating

The parity test file is gated behind both feature flags:

```toml
[[test]]
name = "del_3b_wire_parity"
required-features = ["wire-capture-harness", "parallel-delete-consumer"]
```

This ensures the test only compiles when both the capture
infrastructure (DEL-3.a) and the parallel consumer (DEL-2.c) are
available. The test binary is never built in the default feature set.

### 5.2 Runtime path selection

Inside the test, the sequential and parallel paths are dispatched
explicitly:

```rust
// Sequential baseline (DEL-3.a path)
let baseline = capture_sequential(&fixture);

// Parallel candidate (DEL-2.c path)
let candidate = capture_parallel(&fixture, rayon_threads);
```

`capture_sequential` instantiates `DeleteEmitter` directly (the
non-`#[cfg(feature = "parallel-delete-consumer")]` path).
`capture_parallel` instantiates `ParallelDeleteEmitter` (the
`#[cfg(feature = "parallel-delete-consumer")]` path). Both are
always available inside this test because `required-features`
guarantees both features are enabled.

### 5.3 No runtime feature detection

The test does NOT use `cfg!` to conditionally skip the parallel
path at runtime. Both paths execute unconditionally when the test
binary is built. If the parallel consumer feature is not compiled
in, the entire test binary is absent from the test suite (enforced
by `required-features`), so there is no dead-code or conditional
skip to maintain.

## 6. Failure diagnostics

When parity fails, the test must produce actionable output that
pinpoints the divergence. Generic `assert_eq!` on multi-kilobyte
byte vectors produces an unreadable diff. The harness provides
structured diagnostics at three levels.

### 6.1 NDX channel divergence

On `ndx_channel` mismatch, the diagnostic:

1. **Identifies the first divergent byte offset.** Scans both byte
   vectors and reports the first index where they differ.
2. **Decodes the surrounding context.** Parses the ndx codec around
   the divergent offset to identify whether the mismatch is in the
   `NDX_DEL_STATS` sentinel, one of the five varints, or the
   `NDX_DONE` sentinel.
3. **Reports decoded stats.** Decodes both captures' `stats_only`
   bytes into `DeleteStats` structs and prints a side-by-side
   comparison:

```text
NDX channel divergence at byte offset 7:

  Sequential: NDX_DEL_STATS varints = [1000, 4, 3, 1, 2]
  Parallel:   NDX_DEL_STATS varints = [999, 4, 3, 1, 2]

  First divergent varint: files (index 0)
  Sequential value: 1000
  Parallel value:   999

  Raw bytes at offset 7:
    Sequential: [e8 07 ...]
    Parallel:   [e7 07 ...]
```

### 6.2 MSG channel divergence

On `msg_channel` mismatch, the diagnostic:

1. **Parses both channels into frame lists.** Each `MSG_DELETED`
   frame is split by the length-prefix protocol into
   `(ndx_offset, path_bytes)` tuples.
2. **Identifies missing, extra, and reordered frames.**
   - Frames present in baseline but absent from candidate:
     "MISSING in parallel".
   - Frames present in candidate but absent from baseline:
     "EXTRA in parallel".
   - Frames present in both but at different positions:
     "REORDERED" (with position delta).
3. **Groups by cohort.** If cohort boundaries are available from the
   fixture metadata, the diagnostic groups divergences by
   `cohort_idx` to localise the bug to a specific producer.

```text
MSG channel divergence (13 frames baseline, 12 frames parallel):

  Cohort 3 (dir: "subdir/nested"):
    MISSING in parallel: "subdir/nested/file_7.txt" (baseline frame #9)

  Cohort 5 (dir: "other"):
    REORDERED: "other/a.txt" at position 11 (baseline) vs 12 (parallel)
```

### 6.3 Stats divergence

On `DeleteStats` mismatch, the diagnostic prints a per-field table:

```text
DeleteStats divergence:

  Field     Sequential  Parallel  Delta
  files     1000        999       -1
  dirs      4           4         0
  symlinks  3           3         0
  devices   1           1         0
  specials  2           2         0
```

### 6.4 Combined output

All three diagnostics fire independently. A single test failure may
produce all three reports if the divergence affects multiple layers
(e.g., a lost `MSG_DELETED` frame also reduces the file count in
`NDX_DEL_STATS`). The combined output lets the developer see the
causal chain without re-running with different assertions enabled.

## 7. Edge cases

### 7.1 Zero-delete fixture (F5)

- Both paths produce an empty `msg_channel`.
- When `do_stats` is true: `ndx_channel` contains `NDX_DEL_STATS`
  (all-zero varints) + `NDX_DONE`. Must be byte-identical.
- When `do_stats` is false: `ndx_channel` contains only `NDX_DONE`.
  Must be byte-identical.
- The parallel consumer's reorder buffer is created but never
  receives any `claim_cohort` calls. The consumer loop exits
  immediately on `producers_done && buffer.is_empty()`.

### 7.2 Single-file fixture (F6)

- One producer, one cohort, one `MSG_DELETED` frame.
- Exercises the degenerate case where the parallel consumer has
  no concurrency benefit but must still match the sequential output.
- Tests that the reorder buffer handles a single slot correctly
  (no off-by-one on ring indexing with `head == 0, tail == 1`).

### 7.3 Large flat fixture (F10) - 1000 files

- 1000 entries in a single directory produce 1000 `MSG_DELETED`
  frames in one cohort. The parallel consumer processes this as a
  single producer task (one cohort = one rayon task per DEL-1.c
  section 3.1), so there is no inter-cohort reordering to test.
- Validates that intra-cohort frame order is preserved: the
  producer's dispatch order within a single directory must match
  the sequential emitter's `extras` iteration order.
- Tests varint encoding for stats values > 127 (1000 files produces
  a 2-byte varint for the file count).

### 7.4 Large multi-directory fixture (P1) - 64 directories

- 64 cohorts saturate the reorder ring (N=64). Under 4+ rayon
  threads, producers contend on `claim_cohort` and some block on
  `not_full`. The consumer drains in strict order despite the
  contention.
- Tests that ring-full backpressure does not alter the wire output:
  a blocked producer that resumes after the consumer frees a slot
  must produce the same batch it would have produced without
  blocking.

### 7.5 Deep narrow fixture (P2) - 50 levels

- 50 single-entry cohorts in strict depth-first order. With work-
  stealing, rayon may process cohort 49 before cohort 3. The
  reorder buffer must block the consumer until cohort 3 arrives.
- Tests the head-of-line blocking path (DEL-1.b section 2.3)
  under realistic scheduling jitter.

### 7.6 Uneven cohorts fixture (P3) - mixed sizes

- The 500-entry cohort takes significantly longer than the 1-entry
  cohorts. The consumer blocks waiting for the large cohort while
  small-cohort batches pile up in the ring behind it.
- Tests that the drain-batch-cap (DEL-1.c section 3.2, N=8) does
  not alter the wire output: after the large cohort drains, the
  consumer drains up to 8 queued small cohorts per wake-up, and
  the concatenated byte stream must still match sequential.

### 7.7 Non-fatal error mid-drain

- Uses `ScriptedDeleteFs` to inject `NotFound` for specific entries,
  identical injection schedule for both paths.
- The sequential emitter skips the failed entry and reduces the
  stats count. The parallel emitter's producer skips the same entry
  (same `ScriptedDeleteFs` instance, same schedule).
- Wire images must match: same reduced `MSG_DELETED` set, same
  reduced `NDX_DEL_STATS` varints.

### 7.8 Protocol version 30 (no NDX_DEL_STATS)

- Both paths emit only `NDX_DONE` on the ndx channel.
- `msg_channel` frames must still match (per-entry `MSG_DELETED`
  notifications are protocol-version-independent).
- `stats_only` is empty for both captures.

### 7.9 do_stats=false (no NDX_DEL_STATS frame)

- When `--stats` is not requested, the goodbye writer skips the
  `NDX_DEL_STATS` frame entirely. Both paths must produce an
  `ndx_channel` containing only `NDX_DONE`.
- `msg_channel` parity is unaffected by the `do_stats` flag.

### 7.10 RAYON_NUM_THREADS=1 (sequential fallback)

- With a single rayon thread, the parallel consumer degenerates to
  sequential execution inside the rayon scope. The wire output must
  be byte-identical to the true sequential path.
- This is the strongest single test: any difference between "parallel
  code running on 1 thread" and "sequential code" is a code-path bug
  in the parallel implementation, not a concurrency issue.

## 8. Property-based testing

Beyond the deterministic fixture suite, the parity test includes a
`proptest` variant that generates random fixture configurations and
asserts parity for each.

### 8.1 Strategy

```rust
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn parallel_parity_prop(
        dir_count in 1usize..=32,
        entries_per_dir in 1usize..=50,
        kind_weights in prop::array::uniform4(1u32..=10),
        error_rate in 0.0f64..=0.1,
        rayon_threads in prop::sample::select(vec![1, 2, 4, 8]),
    ) {
        // 1. Generate a random fixture with `dir_count` directories,
        //    each containing `entries_per_dir` entries of random kinds
        //    (weighted by `kind_weights`).
        // 2. Optionally inject errors at `error_rate` probability per
        //    entry via ScriptedDeleteFs with a deterministic PRNG
        //    seeded by the proptest seed.
        // 3. Run sequential + parallel captures.
        // 4. assert_parity(baseline, candidate).
    }
}
```

### 8.2 Shrinking

On failure, proptest shrinks the fixture toward the minimal
reproduction: fewest directories, fewest entries, simplest kinds,
zero errors, 1 rayon thread. The shrunk case is the minimal input
that exhibits the parity violation, which directly identifies the
failing code path.

### 8.3 Seed pinning

Every proptest failure prints its seed. The CI log captures the seed
so a developer can replay the exact failing case locally:

```sh
PROPTEST_SEED=<seed> cargo nextest run -p engine \
    --features wire-capture-harness,parallel-delete-consumer \
    -E 'test(parallel_parity_prop)'
```

## 9. CI integration

### 9.1 Test matrix

The parity test runs in two CI matrix cells:

| Cell | Features | Purpose |
|------|----------|---------|
| `nextest-sequential` | `wire-capture-harness` only | Runs DEL-3.a sequential capture tests (no parallel consumer compiled) |
| `nextest-parallel` | `wire-capture-harness,parallel-delete-consumer` | Runs DEL-3.b parity tests (both consumers compiled) |

The `nextest-sequential` cell validates that the sequential harness
itself is stable and deterministic. The `nextest-parallel` cell runs
the parity comparison.

### 9.2 Platform coverage

Both cells run on the standard CI platform matrix:

- Linux (x86_64, stable Rust)
- macOS (aarch64, stable Rust)
- Windows (x86_64, stable Rust)
- Linux musl (x86_64, stable Rust)

Platform-specific filesystem behaviour (e.g., macOS case-insensitive
HFS+ vs Linux case-sensitive ext4) could cause ordering differences
in fixture construction. The `build_fixture` factory uses explicit
`BTreeSet`-based sorting (DEL-3.a section 4.3) to eliminate
platform-dependent readdir order, so the parity test is expected to
pass identically on all platforms.

### 9.3 Workflow snippet

```yaml
jobs:
  del-parity:
    strategy:
      matrix:
        include:
          - name: sequential-harness
            features: wire-capture-harness
          - name: parallel-parity
            features: wire-capture-harness,parallel-delete-consumer
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: taiki-e/install-action@nextest
      - run: |
          cargo nextest run -p engine \
            --features ${{ matrix.features }} \
            --color never \
            -E 'test(del_3)'
```

### 9.4 Failure policy

The parity test is a **required check** for any PR that touches:

- `crates/engine/src/delete/` (emitter, traversal, plan_map)
- `crates/transfer/src/generator/transfer/goodbye.rs` (NDX_DEL_STATS
  emission)
- `crates/transfer/src/receiver/directory/deletion.rs` (receiver-side
  deletion driver)
- `crates/protocol/src/stats/delete.rs` (wire codec)

PRs that touch none of the above skip the parity cell (standard CI
path filtering).

## 10. Module placement

### 10.1 Source layout

```text
crates/engine/tests/
    del_3b_wire_parity.rs       - main parity test file
    del_3b_wire_parity/
        fixtures.rs             - P1/P2/P3 parity-specific fixtures
        compare.rs              - assert_parity, assert_msg_parity,
                                  diagnostic formatters
        prop.rs                 - proptest strategy and property
```

The parity-specific fixtures (P1-P3) extend the DEL-3.a fixture
catalog. The comparison logic in `compare.rs` uses the
`CapturedWireImage` type from DEL-3.a's `wire_capture` module and
adds the Level 1 / Level 2 comparison and the structured diagnostic
output described in section 6.

### 10.2 Dependency on DEL-3.a

The parity test imports from the capture harness:

```rust
use engine::delete::wire_capture::{
    build_fixture, capture_sequential, CapturedWireImage, FixtureId,
};
```

The `capture_parallel` function is defined in the parity test module
itself (not in the engine crate) because it requires the
`parallel-delete-consumer` feature to compile:

```rust
#[cfg(feature = "parallel-delete-consumer")]
fn capture_parallel(
    fixture: &WireCaptureFixture,
    rayon_threads: usize,
) -> CapturedWireImage {
    // Set RAYON_NUM_THREADS, build pool, run ParallelDeleteEmitter
    // with RecordingWriter pair, return CapturedWireImage.
}
```

## 11. Implementation checklist

| Step | Deliverable | Depends on |
|------|-------------|------------|
| 1 | `capture_parallel` function with `EnvGuard`-managed `RAYON_NUM_THREADS` | DEL-2.c impl, DEL-3.a harness |
| 2 | `assert_parity` + `assert_msg_parity` with Level 1/Level 2 comparison | DEL-3.a `CapturedWireImage` |
| 3 | NDX divergence diagnostic formatter (section 6.1) | Step 2 |
| 4 | MSG divergence diagnostic formatter (section 6.2) | Step 2 |
| 5 | Stats divergence diagnostic formatter (section 6.3) | Step 2 |
| 6 | Parity-specific fixtures P1, P2, P3 | DEL-3.a `build_fixture` pattern |
| 7 | Deterministic parity tests for F1-F10 + P1-P3 across thread-width sweep | Steps 1-6 |
| 8 | Edge-case tests (sections 7.1-7.10) | Steps 1-6 |
| 9 | `proptest` strategy and property (section 8) | Steps 1-2 |
| 10 | CI workflow matrix update (section 9.3) | Step 7 |
| 11 | Required-check path filter for delete-related paths | Step 10 |

## 12. Cross-references

- DEL-3.a capture harness (baseline infrastructure this test
  builds on): `docs/design/del-3a-wire-byte-capture-harness.md`.
- DEL-1.a upstream ordering audit (the invariants being verified):
  `docs/design/del-1a-upstream-ordering-audit.md`.
- DEL-1.b reorder buffer (the buffer whose strict-order drain this
  test validates): `docs/design/del-1b-reordering-buffer.md`.
- DEL-1.c cohort batching (the batching policy layered on top of
  the reorder buffer): `docs/design/del-1c-cohort-batching-strategy.md`.
- DEL-2.c (forthcoming): the `ParallelDeleteEmitter` implementation
  this test exercises.
- DDP design (parent context for the single-emitter invariant):
  `docs/design/parallel-deterministic-delete.md`.
- Strict-order gate (the constraint the parallel path eventually
  retires): `docs/design/delete-during-strict-order-gate.md`.
- Parallel-receive-delta parity precedent (the same gate pattern
  applied to the delta pipeline): memory note
  `project_parallel_interop_parity_gap.md`.
- Source surface under test:
  - `crates/engine/src/delete/emitter/mod.rs` (sequential emitter).
  - `crates/engine/src/delete/wire_capture/` (DEL-3.a harness modules).
  - `crates/transfer/src/generator/transfer/goodbye.rs` (NDX_DEL_STATS
    writer).
  - `crates/transfer/src/receiver/directory/deletion.rs` (receiver-side
    deletion driver, plug-in point for parallel consumer).
  - `crates/protocol/src/stats/delete.rs` (wire codec for five-varint
    frame).
- Upstream references:
  - `target/interop/upstream-src/rsync-3.4.1/main.c:225-247` -
    `write_del_stats` / `read_del_stats`.
  - `target/interop/upstream-src/rsync-3.4.1/generator.c:2376-2425` -
    early/late goodbye del_stats paths.
  - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225` -
    `delete_item` dispatch.
  - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347` -
    `delete_in_dir` traversal order.
