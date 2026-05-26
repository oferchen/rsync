# Wire-byte capture harness for sequential delete (DEL-3.a)

Status: Design (task DEL-3.a; foundation for the DEL-3.b parallel
vs sequential parity gate; depends on DEL-1.a audit + DEL-2.c parallel
consumer implementation)
Audience: engine and transfer maintainers validating that the
`parallel-delete-consumer` feature flag produces wire-identical output.
Scope: capture infrastructure for the sequential `DeleteEmitter` path -
recording raw bytes at specified pipeline taps, deterministic fixture
generation, and replay/comparison mechanism.

Out of scope: the parallel consumer capture (DEL-3.b), the actual
byte-comparison assertion logic (DEL-3.c), and any performance
characterisation of the harness overhead (the harness runs only in
`#[cfg(test)]` and CI, never in production).

## 1. Goal

Provide a **baseline byte capture** of the sequential delete consumer's
wire output so that DEL-3.b can feed identical inputs to the parallel
consumer (DEL-2.c) and assert byte-for-byte equivalence. The harness
must capture:

1. The `NDX_DEL_STATS` frame - the ndx sentinel (`-3`) encoded via the
   ndx codec, followed by five varints (files, dirs, symlinks, devices,
   specials).
2. The `MSG_DELETED` notifications - per-entry path bytes emitted on
   the multiplex side-channel (MSG_INFO).
3. The `NDX_DONE` sentinel that closes the goodbye cohort.

The captures must be deterministic: given the same input file tree and
the same delete plan, the sequential emitter must produce identical byte
sequences across runs, platforms, and Rust compiler versions (no
non-determinism from HashMap iteration, timestamp races, or parallel
stat ordering).

## 2. Harness architecture

### 2.1 Design principle

The harness interposes a **recording writer** between the delete
emitter's wire-emission logic and the actual I/O sink. The recording
writer implements `std::io::Write` and captures every byte written to it
in an append-only `Vec<u8>`. After the drain completes, the captured
bytes are the ground truth for parity comparison.

The recording layer is zero-cost in production: it exists only behind
`#[cfg(test)]` (unit tests) and the `wire-capture-harness` test-only
feature flag (integration tests in `crates/engine/tests/`).

### 2.2 Component diagram

```text
+---------------------+
|  Test fixture       |  deterministic file tree + flist segment
+---------------------+
         |
         v
+---------------------+
|  DeleteContext      |  observe_segment_for_delete (phase 1)
+---------------------+
         |
         v
+---------------------+        +---------------------+
|  DeleteEmitter      | -----> |  RecordingWriter    |
|  (sequential)       |        |  (captures bytes)   |
+---------------------+        +---------------------+
         |                              |
         v                              v
+---------------------+        +---------------------+
|  DeleteFs dispatch  |        |  CapturedWireImage  |
|  (RecordingDeleteFs)|        |  { ndx_bytes,       |
+---------------------+        |    msg_bytes,       |
                               |    stats_bytes }    |
                               +---------------------+
```

### 2.3 `RecordingWriter`

```rust
/// Captures all bytes the emitter writes to the ndx and msg channels.
///
/// Wraps a `Vec<u8>` per channel so the caller can inspect raw wire
/// bytes without parsing. Implements `Write` for the channel the
/// emitter targets; the emitter's goodbye writer routes ndx traffic
/// through one instance and msg traffic through another.
#[derive(Debug, Default)]
pub(crate) struct RecordingWriter {
    buf: Vec<u8>,
}

impl std::io::Write for RecordingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
```

### 2.4 `CapturedWireImage`

The harness produces a `CapturedWireImage` that bundles the three byte
streams:

- `ndx_channel: Vec<u8>` - raw bytes written to the ndx stream (the
  `NDX_DEL_STATS` frame + `NDX_DONE` sentinel).
- `msg_channel: Vec<u8>` - raw bytes written to the MSG_INFO
  side-channel (`MSG_DELETED` notifications).
- `stats_only: Vec<u8>` - a convenience sub-slice of `ndx_channel`
  isolating just the five varints after the `NDX_DEL_STATS` sentinel.

The image is opaque bytes; the comparison in DEL-3.c uses `==` on the
byte vectors directly (no semantic parsing). This gives the strongest
possible guarantee: any divergence in varint encoding, byte order,
flush boundaries, or trailing padding is caught.

## 3. Capture points

The delete pipeline has three layers where interposition could occur.
The harness taps at two of them, providing both pre-framing and
post-framing visibility.

### 3.1 Pre-multiplex capture (primary)

**Location:** Between the emitter's `write_del_stats` / `send_msg`
calls and the multiplex framing layer.

This is the layer where `NDX_DEL_STATS` varints and `MSG_DELETED`
payloads exist as raw, unframed bytes. Capturing here means the
comparison is independent of the multiplex envelope format (which is
shared infrastructure and already covered by `crates/protocol/tests/`).

**Tap mechanism:** The goodbye writer in
`crates/transfer/src/generator/transfer/goodbye.rs` takes generic
`W: Write` parameters. The harness instantiates a `RecordingWriter`
as the ndx writer and a second `RecordingWriter` as the msg writer.

### 3.2 Post-multiplex capture (secondary, opt-in)

**Location:** After the multiplex framing layer wraps the raw bytes
into `MSG_DATA` envelopes.

This tap captures the final socket-ready byte stream. It is useful for
debugging framing-level divergences but is NOT the primary comparison
target because multiplex flush boundaries are implementation-defined
and upstream does not guarantee specific framing granularity.

**Tap mechanism:** An optional `RecordingWriter` wrapping the final
output `Write` sink. Enabled only when the test explicitly requests
`CaptureMode::Full` (see section 7).

### 3.3 Stats-only capture (derived)

After the pre-multiplex capture completes, the harness extracts the
`NDX_DEL_STATS` payload (the five varints) by slicing `ndx_channel`
at the known offset past the ndx sentinel encoding. This gives tests
a focused comparison point that ignores `MSG_DELETED` ordering (which
DEL-1.a section 5.1 proves is reorderable within a cohort).

## 4. Fixture design

Deterministic test fixtures must cover the following dimensions to
exercise the sequential emitter's full dispatch table and ordering
logic. Each fixture is a `(source_flist, dest_tree)` pair that
produces a predictable set of extras (entries present on destination
but absent from source).

### 4.1 Fixture catalog

| ID | Name | Structure | Delete set | Tests |
|----|------|-----------|------------|-------|
| F1 | `flat_alpha` | 10 regular files, names a-j | All 10, alphabetical order | Basic capture, byte stability |
| F2 | `nested_dirs` | 3-level nesting: `d1/d2/d3/` with 2 files per level | 6 files + 3 dirs, depth-first order | Recursive peel, ENOTEMPTY fallback |
| F3 | `mixed_types` | 5 files + 2 symlinks + 1 device + 1 special + 1 dir | All 10, mixed kinds | Per-kind stats accumulation |
| F4 | `size_varied` | 8 files, sizes 0B to 1 MiB (powers of 2) | All 8 | No size-dependent ordering (size is irrelevant to delete, but verifies capture stability) |
| F5 | `empty_set` | Source matches destination exactly | Empty delete set | Zero-entry stats frame (all varints = 0) |
| F6 | `single_file` | One regular file on destination, absent from source | 1 file | Minimum non-trivial capture |
| F7 | `dirs_only` | 4 nested empty directories | 4 dirs, leaves first | Directory-only stats (files=0, dirs=4) |
| F8 | `symlinks_and_specials` | 3 symlinks + 2 FIFOs | 5 entries | Symlink/special stat buckets |
| F9 | `unicode_names` | Files with multi-byte UTF-8 names | All entries | Wire-byte encoding of non-ASCII paths |
| F10 | `large_flat` | 1000 regular files, names 0000-0999 | All 1000 | Varint overflow at stats > 127, buffer growth |

### 4.2 Fixture construction API

```rust
/// Builds a deterministic fixture pair and returns the expected
/// CapturedWireImage after the sequential emitter drains.
pub(crate) fn build_fixture(id: FixtureId) -> WireCaptureFixture {
    // 1. Create TempDir for destination.
    // 2. Populate destination with the fixture's file tree.
    // 3. Build flist segment (source side) - deliberately missing
    //    the entries that should be deleted.
    // 4. Run observe_segment_for_delete to compute extras.
    // 5. Instantiate DeleteEmitter with RecordingDeleteFs +
    //    RecordingWriter pair.
    // 6. Call emit_all + write_del_stats.
    // 7. Return CapturedWireImage + metadata.
}
```

### 4.3 Ordering guarantee

All fixtures rely on the `DirTraversalCursor`'s deterministic depth-first
order (the cursor walks directories in `f_name_cmp`-ascending order per
upstream `generator.c:272-347`). Within a directory, extras are sorted by
name ascending (matching `compute_extras` which sorts via `BTreeSet`
semantics). This double-determinism means repeated fixture runs produce
identical byte sequences regardless of filesystem readdir ordering.

## 5. Replay mechanism

The replay mechanism is the bridge between DEL-3.a (sequential capture)
and DEL-3.b (parallel capture). It works in three steps:

### 5.1 Capture baseline (DEL-3.a)

```text
fixture ──> sequential emitter ──> CapturedWireImage (baseline)
```

The baseline is captured once per test invocation. It is NOT persisted
to disk as a golden file because the varint encoding is
platform-independent and a saved golden would drift if `DeleteStats`
field order ever changes. Instead, the baseline is always freshly
computed from the fixture.

### 5.2 Capture parallel (DEL-3.b)

```text
same fixture ──> parallel emitter ──> CapturedWireImage (candidate)
```

The parallel consumer (DEL-2.c `ParallelDeleteEmitter`) is fed the
exact same `DeletePlanMap` + `DirTraversalCursor` state that the
sequential emitter consumed. The harness achieves this by cloning the
fixture state before draining (both plan map and cursor are cloneable
in test configurations).

### 5.3 Compare (DEL-3.c)

```rust
assert_eq!(
    baseline.ndx_channel,
    candidate.ndx_channel,
    "NDX_DEL_STATS + NDX_DONE bytes diverged"
);
assert_eq!(
    baseline.stats_only,
    candidate.stats_only,
    "varint-encoded deletion counts diverged"
);
// MSG_DELETED ordering is reorderable within a cohort per DEL-1.a
// section 5.1, so sort before comparing:
assert_eq!(
    sorted(&baseline.msg_channel),
    sorted(&candidate.msg_channel),
    "MSG_DELETED set diverged (order-independent)"
);
```

## 6. NDX_DEL_STATS verification

The harness includes focused assertions on the `NDX_DEL_STATS` frame
independent of the full wire-image comparison.

### 6.1 Varint round-trip

After capture, the harness decodes the `stats_only` bytes back into a
`DeleteStats` struct via `DeleteStats::read_from` and asserts each
field matches the emitter's `stats()` accessor. This catches:

- Varint encoding bugs (wrong byte count for values crossing 127/16383
  boundaries).
- Field ordering divergence (if files/dirs/symlinks/devices/specials
  are serialised in a different order between paths).
- Accumulation errors (if the parallel consumer's atomic-merge or
  fold-batch logic under-counts).

### 6.2 Cross-path stats equality

```rust
assert_eq!(
    sequential_emitter.stats(),
    parallel_emitter_outcome.stats,
    "DeleteStats counters diverged between sequential and parallel"
);
```

This is a semantic check (not wire-byte) that catches accumulation
bugs even when the wire encoding happens to be correct.

### 6.3 Wire encoding stability

For each fixture, the harness asserts that `stats_only.len()` equals
the expected varint byte count given the known fixture counts. For
example, fixture F1 (10 files, 0 everything else) should produce
exactly 5 bytes: one single-byte varint per counter (all <= 127).
Fixture F10 (1000 files) should produce 7 bytes: a 2-byte varint for
files (1000 > 127) plus four 1-byte varints for the zero counters.

## 7. Edge cases

The harness explicitly tests the following boundary conditions:

### 7.1 Empty delete set (fixture F5)

- The emitter is invoked with a plan map containing zero extras.
- No `MSG_DELETED` notifications are emitted.
- `NDX_DEL_STATS` is still written (all-zero varints) when `do_stats`
  is true, matching upstream's unconditional `write_del_stats` call
  even on a zero-deletion run (the gate is timing mode + `--stats`,
  not deletion count).
- When `do_stats` is false, no `NDX_DEL_STATS` frame is written at
  all (the ndx channel contains only `NDX_DONE`).

### 7.2 Single file (fixture F6)

- Minimum non-trivial case. Exactly one `MSG_DELETED` notification.
- `stats_only` contains `[1, 0, 0, 0, 0]` (one file, zeros for the
  other four kinds as single-byte varints).

### 7.3 Directory-only deletes (fixture F7)

- Tests that `rmdir` dispatch maps to `DeleteStats.dirs` without
  incrementing any other counter.
- ENOTEMPTY recursive fallback (non-empty nested dir) exercises the
  recursive peel path; the harness confirms the recursed entries still
  land in the expected wire image.

### 7.4 Mixed types (fixture F3)

- Every `DeleteEntryKind` variant is exercised in a single capture.
- Per-kind varint values span different bit-widths.

### 7.5 Protocol version gate

- Protocol 30 (no NDX_DEL_STATS support): harness confirms the ndx
  channel contains only the `NDX_DONE` frame with no preceding stats.
- Protocol 31+: full `NDX_DEL_STATS` + `NDX_DONE` sequence.

### 7.6 Non-fatal error mid-drain

- Fixture with a `ScriptedDeleteFs` that injects `NotFound` for one
  entry. The emitter continues (default `EmitterErrorPolicy`); the
  wire image reflects the reduced stats (N-1 files) and the
  `io_error` bitmask is non-zero.
- Both sequential and parallel paths must produce the same reduced
  wire image when fed the same failure schedule.

## 8. Module placement and integration

### 8.1 Source layout

```text
crates/engine/src/delete/
    wire_capture/
        mod.rs          - CapturedWireImage, CaptureMode, harness entry point
        writer.rs       - RecordingWriter implementation
        fixtures.rs     - FixtureId enum, build_fixture factory
        compare.rs      - assertion helpers (byte-eq, sorted msg compare)
    wire_capture.rs     - (re-export for #[cfg(test)] visibility)
```

The entire `wire_capture/` subtree is gated behind
`#[cfg(any(test, feature = "wire-capture-harness"))]` so production
builds carry zero overhead.

### 8.2 Integration test file

```text
crates/engine/tests/
    del_3a_wire_capture.rs  - exercises all 10 fixtures, asserts
                              deterministic capture across 5 repeated
                              invocations per fixture
```

### 8.3 Feature flag wiring

The `wire-capture-harness` feature is test-only and declared in
`crates/engine/Cargo.toml`:

```toml
[features]
wire-capture-harness = []
parallel-delete-consumer = []
```

The integration test enables both features:

```toml
[[test]]
name = "del_3a_wire_capture"
required-features = ["wire-capture-harness"]
```

### 8.4 Relationship to existing test infrastructure

- **`RecordingDeleteFs`** (existing) - records dispatch events. The
  harness reuses it to confirm syscall ordering without touching the
  filesystem.
- **`ScriptedDeleteFs`** (existing) - injects failures. The harness
  reuses it for edge case 7.6.
- **`DeleteContext` tests** (existing) - exercise observe/drain
  lifecycle. The harness builds on the same `TempDir` + segment
  pattern but adds the `RecordingWriter` interposition.
- **Golden byte tests** (`crates/protocol/tests/golden/`) - static
  `.bin` fixtures checked into the repo. The wire-capture harness does
  NOT use persisted goldens (see section 5.1 rationale); it computes
  baselines at test time for maximum flexibility.

### 8.5 CI integration

The harness runs as part of the standard `cargo nextest` matrix since
it is a regular integration test gated by a feature flag. The CI
workflow enables the feature explicitly:

```yaml
- run: cargo nextest run -p engine --features wire-capture-harness
```

No additional binary, container, or network dependency is needed.

## 9. Determinism guarantees

The harness relies on three determinism invariants:

1. **Cursor order is deterministic.** `DirTraversalCursor` surfaces
   directories in `f_name_cmp`-ascending pre-order (upstream
   `generator.c:272-347`). This is a property of the cursor's BTree
   internals, not filesystem readdir order.

2. **Extras within a directory are sorted.** `compute_extras` produces
   entries sorted by name (the `BTreeSet` difference operation
   preserves sort order). No hash-map iteration randomness.

3. **Varint encoding is deterministic.** `write_varint` in
   `crates/protocol/src/varint.rs` uses a fixed algorithm with no
   platform-dependent branches. The output for a given `i32` value is
   identical on all targets.

If any of these invariants is violated, the harness will surface it as
a test failure (the repeated-invocation check in section 8.2 catches
non-determinism by running the same fixture 5 times and asserting all
5 captures are byte-identical).

## 10. Implementation checklist

| Step | Deliverable | Depends on |
|------|-------------|------------|
| 1 | `RecordingWriter` + `CapturedWireImage` types | None |
| 2 | `FixtureId` enum + `build_fixture` factory for F1-F10 | Step 1 |
| 3 | Sequential capture entry point (runs emitter + goodbye writer with recording writers) | Steps 1-2 |
| 4 | `del_3a_wire_capture.rs` integration test with all 10 fixtures + 5x determinism check | Steps 1-3 |
| 5 | Edge-case tests (7.1-7.6) wired into the integration test | Steps 1-4 |
| 6 | NDX_DEL_STATS varint round-trip assertion (section 6.1) | Step 3 |
| 7 | Wire encoding stability assertions (section 6.3) | Steps 3, 6 |
| 8 | CI workflow update to enable `wire-capture-harness` feature | Step 4 |

## 11. Upstream references

- `target/interop/upstream-src/rsync-3.4.1/main.c:225-247` -
  `write_del_stats` / `read_del_stats` wire format.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:2376-2381` -
  early goodbye del_stats path.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:2420-2425` -
  late goodbye del_stats path.
- `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225` -
  `delete_item` dispatch.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347` -
  `delete_in_dir` traversal order.
- `target/interop/upstream-src/rsync-3.4.1/log.c:839-876` -
  `log_delete` / `send_msg(MSG_DELETED)`.
- `target/interop/upstream-src/rsync-3.4.1/io.c:1549-1601` -
  receiver-side `MSG_DELETED` handler.
