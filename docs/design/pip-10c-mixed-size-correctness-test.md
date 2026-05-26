# PIP-10.c - Multi-file mixed-size parallel receive-delta correctness test

Date: 2026-05-26
Status: design spec
Tracking: PIP-10.c (#3025)
Parent: PIP-10 (end-to-end validation series for parallel receive-delta)
Predecessors:
- PIP-10.a (PR #5030): full upstream interop matrix spec
- PIP-10.b (PR #5029): adversarial chunk ordering stress test spec
- PIP-9: production wire-up of parallel receive-delta into receiver pipeline
- PIP-7: corruption investigation and mitigation

Related code:
- `crates/transfer/src/delta_pipeline/threshold.rs` -
  `ThresholdDeltaPipeline` (auto-selects sequential vs parallel)
- `crates/transfer/src/delta_pipeline/mod.rs` -
  `DEFAULT_PARALLEL_THRESHOLD = 64`
- `crates/transfer/src/delta_pipeline/parallel.rs` -
  `adaptive_capacity()`, `SMALL_FILE_THRESHOLD = 64 KB`,
  `LARGE_FILE_THRESHOLD = 1 MB`
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier`, `DEFAULT_PER_FILE_REORDER_CAPACITY = 64`
- `crates/engine/src/concurrent_delta/strategy.rs` -
  `WholeFileStrategy`, `DeltaTransferStrategy`

## 1. Motivation

PIP-10.a validates the parallel path against upstream rsync across the
full interop matrix. PIP-10.b stress-tests the reorder buffer under
adversarial chunk orderings. Neither exercises the workload shape that
dominates real-world transfers: a mix of many tiny files, a moderate
number of medium files, and a few large files - all in a single
transfer.

This mixed-size distribution hits three distinct code paths
simultaneously:

1. **Files below the parallel dispatch threshold** (< 64 files total):
   the `ThresholdDeltaPipeline` stays in `Buffering` mode and falls
   back to `SequentialDeltaPipeline` on flush. This path is only
   exercised when the total file count stays below
   `DEFAULT_PARALLEL_THRESHOLD = 64`.

2. **Small files in a parallel transfer**: when the threshold is
   crossed and the pipeline promotes to `ParallelDeltaPipeline`, tiny
   files (0 B - 1 KB) still flow through the parallel work queue and
   reorder buffer. The adaptive capacity heuristic
   (`adaptive_capacity()`) selects the 8x multiplier when
   `avg_target_size < SMALL_FILE_THRESHOLD (64 KB)`, producing deeper
   queues. These files generate whole-file work items
   (`WholeFileStrategy`) with zero or minimal delta tokens.

3. **Large files in a parallel transfer**: files above
   `LARGE_FILE_THRESHOLD (1 MB)` produce deep per-file chunk streams
   that exercise the per-file `ReorderBuffer`
   (`DEFAULT_PER_FILE_REORDER_CAPACITY = 64`). Large files also
   trigger `DeltaTransferStrategy` on delta updates, running the full
   signature-generation, block-matching, and delta-application
   pipeline across rayon workers.

A single mixed-size transfer stresses the interaction between these
paths: the adaptive queue depth is computed from the average file size
across the buffered batch, and that average is sensitive to the mix.
A workload dominated by tiny files with a handful of large files
produces a low average, selecting the deep-queue (8x) path - but the
large files then flood the deep queue with high-latency work items,
potentially causing head-of-line blocking.

PIP-10.c defines a deterministic correctness test suite that exercises
these interactions and verifies byte-identical reconstruction against
the sequential path.

## 2. File size distribution

The test fixture uses a realistic size distribution modeled on common
sync workloads (source trees, dotfile repos, media projects).

### 2.1 Size tiers

| Tier | Size | Count | Purpose |
|------|------|-------|---------|
| Empty | 0 B | 10 | Zero-length files: no delta tokens, no chunks |
| Tiny | 1 B | 10 | Single-byte files: degenerate chunk (1 byte payload) |
| Small | 100 B | 30 | Config-like files: single whole-file token |
| Medium-small | 1 KB | 30 | Source files: single block, whole-file strategy |
| Medium | 64 KB | 20 | At `SMALL_FILE_THRESHOLD` boundary: adaptive queue pivot |
| Large | 1 MB | 10 | At `LARGE_FILE_THRESHOLD` boundary: multiple blocks |
| Very large | 10 MB | 3 | Deep per-file chunk stream: exercises reorder buffer |
| Extra large | 100 MB | 1 | Sustained I/O: exercises backpressure and spill |

**Total file count: 114.** This exceeds `DEFAULT_PARALLEL_THRESHOLD`
(64) by a factor of ~1.8x, ensuring the `ThresholdDeltaPipeline`
always promotes to parallel mode in the main test. A sub-threshold
variant (Section 4.3) uses a 50-file subset to exercise the
sequential fallback path.

**Total data volume: ~134 MB.** Dominated by the 3 x 10 MB and
1 x 100 MB files. The small-file tail contributes < 2 MB. This
asymmetry is intentional: it mirrors real workloads where a small
fraction of files accounts for the bulk of bytes transferred.

### 2.2 Content generation

Each file's content is deterministically generated from its size tier,
index within the tier, and a fixed seed. This ensures reproducibility
across platforms and runs.

```rust
fn generate_content(tier: &str, index: usize, size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed
        ^ (tier.as_bytes().iter().fold(0u64, |a, &b| a.wrapping_mul(31).wrapping_add(u64::from(b))))
        ^ (index as u64);
    (0..size)
        .map(|_| {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect()
}
```

The pseudo-random content ensures files are not trivially
compressible and that delta transfers produce meaningful block-match
ratios rather than degenerate all-literal or all-match outcomes.

### 2.3 Directory structure

Files are organized into a directory tree that exercises recursive
traversal and per-directory batching:

```
mixed_size_test/
  empty/
    empty_000.dat .. empty_009.dat        (10 x 0 B)
  tiny/
    tiny_000.dat .. tiny_009.dat          (10 x 1 B)
  small/
    small_000.dat .. small_029.dat        (30 x 100 B)
  medium_small/
    ms_000.dat .. ms_029.dat              (30 x 1 KB)
  medium/
    med_000.dat .. med_019.dat            (20 x 64 KB)
  large/
    large_000.dat .. large_009.dat        (10 x 1 MB)
  very_large/
    vl_000.dat .. vl_002.dat              (3 x 10 MB)
  extra_large/
    xl_000.dat                            (1 x 100 MB)
```

Eight directories, one per tier. The directory names are chosen to
sort alphabetically in a different order than the size tiers, which
exercises the file-list sort order (by path) rather than by size.

## 3. Threshold boundary cases

### 3.1 Adaptive capacity pivot

The `adaptive_capacity()` function in
`crates/transfer/src/delta_pipeline/parallel.rs` selects queue depth
based on average target size:

| Average size | Multiplier | Queue depth (8 cores) |
|-------------|-----------|----------------------|
| 0 (unknown) | 2x | 16 |
| < 64 KB | 8x | 64 |
| 64 KB - 1 MB | 4x | 32 |
| > 1 MB | 2x | 16 |

The mixed-size fixture's average file size across all 114 files:

```
total_bytes = 10*0 + 10*1 + 30*100 + 30*1024 + 20*65536
            + 10*1048576 + 3*10485760 + 1*104857600
            = 0 + 10 + 3000 + 30720 + 1310720
            + 10485760 + 31457280 + 104857600
            = 148145090
avg = 148145090 / 114 = 1299518 bytes (~1.24 MB)
```

The average is above `LARGE_FILE_THRESHOLD` (1 MB), selecting the 2x
multiplier. This is a realistic outcome: a handful of large files
dominate the average even when the file count is dominated by small
files.

The test must verify that the 2x depth is sufficient for the
workload. If small files starve behind large-file work items in the
bounded queue, the reorder buffer stalls - a correctness concern (not
just performance) because the consumer's `force_insert` path engages
and must preserve ordering.

### 3.2 Threshold-exact files

The fixture includes files at exact threshold boundaries:

- **64 KB (medium tier)**: `SMALL_FILE_THRESHOLD` boundary. A
  64 KB file with avg_target_size exactly at 64 KB selects the 4x
  multiplier (the `<` comparison excludes the boundary itself).

- **1 MB (large tier)**: `LARGE_FILE_THRESHOLD` boundary. A 1 MB
  file lands exactly at the boundary; the `>` comparison excludes
  it, so 1 MB selects 4x, not 2x. This is verified in the test.

### 3.3 Per-file reorder capacity boundary

`DEFAULT_PER_FILE_REORDER_CAPACITY = 64`. For the parallel path to
stress the per-file reorder buffer, a single file must produce more
than 64 chunks. With the upstream default block size of 700 bytes for
files under 512 KB, a 64 KB file produces ~94 blocks. For larger
files the block size scales up (sqrt-based in
`signature/block_size.rs`), so a 10 MB file produces ~4500 blocks at
~1468-byte block size, and a 100 MB file produces ~14000 blocks at
~7000-byte block size. All files in the large/very_large/extra_large
tiers exercise the per-file reorder buffer beyond its default capacity.

## 4. Test scenarios

### 4.1 Initial sync (whole-file transfer)

**Setup**: source tree populated per Section 2. Destination is empty.

**Transfer**: equivalent to `oc-rsync -av <src>/ <dst>/`.

**Pipeline dispatch**: `ThresholdDeltaPipeline` buffers the first 64
work items, then promotes to `ParallelDeltaPipeline` and flushes.
All files are whole-file transfers (`WholeFileStrategy`) since no
basis exists.

**Verification**:
1. SHA-256 of every destination file matches source file.
2. File count in destination matches source (114 files, 8 directories).
3. Zero-byte files exist in destination with size 0.
4. One-byte files have the correct single byte.
5. Transfer statistics: `literal_bytes == source file size` and
   `matched_bytes == 0` for every file (whole-file transfer invariant).

### 4.2 Delta update (selective modification)

**Setup**: initial sync completed (Section 4.1). Then modify a subset
of source files:

| Tier | Files modified | Modification |
|------|---------------|-------------|
| Empty | 2 of 10 | Write 16 bytes (empty -> non-empty) |
| Tiny | 2 of 10 | Flip the single byte |
| Small | 5 of 30 | Append 50 bytes |
| Medium-small | 5 of 30 | Overwrite first 512 bytes |
| Medium | 5 of 20 | Overwrite middle 4 KB |
| Large | 3 of 10 | Overwrite first 64 KB |
| Very large | 2 of 3 | Overwrite 1 MB at offset 5 MB |
| Extra large | 1 of 1 | Append 1 MB |

**Transfer**: equivalent to `oc-rsync -av --no-whole-file <src>/ <dst>/`.

**Pipeline dispatch**: 25 modified files produce `DeltaWork` items.
The remaining 89 unmodified files are skipped by the quick-check
(matching size + mtime). However, files whose sizes changed
(empty -> 16 B, append operations) produce fresh work items. The
`DeltaTransferStrategy` processes files that have a basis (all
non-empty destinations from the initial sync).

**Verification**:
1. SHA-256 of every destination file matches the (updated) source.
2. Modified files have `matched_bytes > 0` (delta path exercised).
3. Large-file modifications have `matched_bytes > literal_bytes`
   (block-matching found basis blocks for the unmodified regions).
4. Unmodified files remain unchanged in the destination.

### 4.3 Sub-threshold transfer (sequential fallback)

**Setup**: a 50-file subset of the fixture (10 empty + 10 tiny +
10 small + 10 medium-small + 5 medium + 3 large + 1 very_large +
1 extra_large). Total: 50 files, below `DEFAULT_PARALLEL_THRESHOLD`
(64).

**Transfer**: same as Section 4.1.

**Pipeline dispatch**: `ThresholdDeltaPipeline` remains in `Buffering`
mode. On flush, it falls back to `SequentialDeltaPipeline`.

**Verification**:
1. SHA-256 of every destination file matches source.
2. Output is byte-identical to the parallel-path output for the same
   50 files. (This establishes that the threshold fallback produces
   identical results.)

### 4.4 Parallel vs sequential parity

**Setup**: full 114-file fixture.

**Transfer**: run twice with the same source tree:
- **Run A**: force sequential path (`SequentialDeltaPipeline`).
- **Run B**: force parallel path (`ParallelDeltaPipeline`).

**Verification**:
1. SHA-256 manifest of Run A destination matches SHA-256 manifest of
   Run B destination (zero divergence).
2. Per-file transfer statistics (literal_bytes, matched_bytes) match
   between Run A and Run B. Note: matched_bytes may differ if the
   parallel path reorders block-match evaluation, but the
   reconstructed file content must be byte-identical regardless.

### 4.5 Second delta pass (idempotency)

**Setup**: delta update (Section 4.2) completed. No further source
modifications.

**Transfer**: re-run `oc-rsync -av <src>/ <dst>/`.

**Verification**:
1. Zero files transferred (quick-check: all sizes and mtimes match).
2. Destination files unchanged from Section 4.2 state.
3. Transfer statistics: `total_bytes_written == 0`.

### 4.6 Mixed whole-file and delta in one transfer

**Setup**: initial sync completed. Then:
- Delete 10 destination files (from different tiers).
- Modify 10 other source files (from different tiers).
- Add 5 new source files (2 x 0 B, 1 x 1 KB, 1 x 64 KB, 1 x 1 MB).

**Transfer**: `oc-rsync -av --no-whole-file --delete <src>/ <dst>/`.

**Pipeline dispatch**: the work queue receives a mix of:
- Whole-file items for the 10 deleted-then-recreated files (no basis).
- Whole-file items for the 5 new files (no basis).
- Delta items for the 10 modified files (basis exists).
- Skip for the ~89 unchanged files.

Both `WholeFileStrategy` and `DeltaTransferStrategy` run concurrently
in the same rayon pool. The reorder buffer interleaves whole-file and
delta results.

**Verification**:
1. SHA-256 of every destination file matches updated source.
2. Deleted source files are removed from destination.
3. New files appear in destination with correct content.
4. Delta-updated files have `matched_bytes > 0`.

## 5. Edge cases

### 5.1 Empty files

Empty files (0 bytes) produce zero delta tokens and zero chunks. The
parallel applier receives a `DeltaWork` with `target_size == 0` and
the `WholeFileStrategy` returns a `DeltaResult` with `bytes_written
== 0`. The destination file must exist with size 0.

Pathological edge: a transfer of only empty files (e.g., the 10-file
`empty/` subdirectory alone) stays below the parallel threshold and
exercises the sequential fallback. Test that the sequential path
handles zero-byte files correctly.

### 5.2 Single-byte files

A 1-byte file generates exactly one chunk of 1 byte. The per-file
reorder buffer is allocated but processes exactly one insert at
`chunk_sequence == 0`, which drains immediately. No reorder pressure.

Pathological edge: 64+ single-byte files in a single transfer trip
the parallel threshold and dispatch to rayon workers. The overhead of
the parallel path (work queue push, rayon dispatch, reorder buffer
insert, result channel send) per 1-byte file must not corrupt the
output. A timing-sensitive race between the trivially fast verify
step and the file-commit step is unlikely but checked by the SHA-256
sweep.

### 5.3 Files at exact threshold boundaries

- **64 files**: exactly at `DEFAULT_PARALLEL_THRESHOLD`. The
  threshold comparison is `>=`, so 64 files triggers parallel mode.
  Test with exactly 64 files to verify the boundary.

- **63 files**: one below threshold. Sequential fallback. Verify
  output matches.

- **64 KB file**: at `SMALL_FILE_THRESHOLD` (64 * 1024 = 65536).
  The adaptive capacity comparison is `< SMALL_FILE_THRESHOLD`, so a
  file of exactly 65536 bytes does not qualify as "small". Test that
  the queue depth transitions correctly.

- **1 MB file**: at `LARGE_FILE_THRESHOLD` (1024 * 1024). The
  comparison is `> LARGE_FILE_THRESHOLD`, so a file of exactly
  1048576 bytes does not qualify as "large". Test that the 4x
  multiplier applies, not 2x.

### 5.4 Extremely heterogeneous transfer

A transfer with 100 x 1-byte files and 1 x 100 MB file. The average
size is ~990 KB, selecting the 4x multiplier. But the 100 x 1-byte
files finish nearly instantly while the 100 MB file occupies a rayon
worker for the duration. Test that the reorder buffer does not stall
waiting for the large file's result while 100 small-file results are
queued.

### 5.5 Symlinks and special files alongside regular files

Add 5 symlinks and 2 directories to the fixture. Symlinks do not
flow through the delta pipeline (they are handled by the metadata
path). Verify that their presence in the file list does not perturb
the NDX sequencing of regular files through the parallel pipeline.

## 6. Correctness oracle

### 6.1 Primary oracle: SHA-256 comparison

Every test scenario computes SHA-256 of each reconstructed
destination file and compares it against the SHA-256 of the
corresponding source file. This is the primary correctness assertion.

```rust
fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

fn verify_tree(src: &Path, dst: &Path) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() { continue; }
        let rel = entry.path().strip_prefix(src).unwrap();
        let dst_path = dst.join(rel);
        if !dst_path.exists() {
            errors.push(format!("missing: {}", rel.display()));
            continue;
        }
        let src_hash = sha256_file(entry.path()).unwrap();
        let dst_hash = sha256_file(&dst_path).unwrap();
        if src_hash != dst_hash {
            errors.push(format!(
                "sha256 mismatch: {} (src: {}, dst: {})",
                rel.display(),
                hex::encode(src_hash),
                hex::encode(dst_hash),
            ));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
```

### 6.2 Secondary oracle: sequential path comparison

For every parallel-path transfer, the same transfer is run through
`SequentialDeltaPipeline` and the SHA-256 manifests are compared.
This catches subtle divergences where both parallel and sequential
paths produce valid-looking files but with different content (e.g., a
block-match discrepancy that happens to pass individual file
integrity checks).

### 6.3 Tertiary oracle: transfer statistics

Transfer statistics (literal_bytes, matched_bytes, bytes_written) are
collected from the pipeline results and cross-checked against
expected values:

| File state | Expected literal | Expected matched |
|-----------|-----------------|-----------------|
| Whole-file (no basis) | == file size | == 0 |
| Delta (basis exists, unmodified) | < file size | > 0 |
| Delta (basis exists, modified) | > 0 | > 0 |
| Empty file | == 0 | == 0 |

The statistics check is a soft assertion (logged warning, not test
failure) because the parallel path's block-match order may differ
from the sequential path in ways that change the literal/matched
split without affecting the reconstructed content.

## 7. Test implementation

### 7.1 Test location

`crates/transfer/tests/mixed_size_correctness.rs` - integration test
in the `transfer` crate's test directory. This location exercises the
full pipeline through the public `ReceiverDeltaPipeline` trait
without reaching into engine internals.

### 7.2 Test structure

```rust
// Pseudocode - actual implementation will use tempfile::TempDir,
// the fixture generator from Section 2, and the verification
// functions from Section 6.

#[test]
fn mixed_size_initial_sync_parallel() {
    // Setup: generate 114-file fixture
    // Transfer: ThresholdDeltaPipeline (auto-promotes to parallel)
    // Verify: SHA-256 sweep, file count, stats
}

#[test]
fn mixed_size_delta_update_parallel() {
    // Setup: initial sync + modify subset
    // Transfer: ThresholdDeltaPipeline with delta-enabled work items
    // Verify: SHA-256 sweep, matched_bytes > 0 for modified files
}

#[test]
fn mixed_size_sub_threshold_sequential_fallback() {
    // Setup: 50-file subset
    // Transfer: ThresholdDeltaPipeline (stays in Buffering, flushes sequential)
    // Verify: SHA-256 sweep, byte-identical to parallel output
}

#[test]
fn mixed_size_parallel_vs_sequential_parity() {
    // Setup: full 114-file fixture
    // Transfer A: SequentialDeltaPipeline
    // Transfer B: ParallelDeltaPipeline
    // Verify: SHA-256 manifests identical
}

#[test]
fn mixed_size_idempotent_resync() {
    // Setup: delta update completed
    // Transfer: re-run, expect zero files transferred
    // Verify: destination unchanged
}

#[test]
fn mixed_size_whole_file_and_delta_mixed() {
    // Setup: initial sync + delete some dst + modify some src + add new src
    // Transfer: ThresholdDeltaPipeline with --delete semantics
    // Verify: SHA-256 sweep, deletions applied, new files present
}

#[test]
fn threshold_boundary_exactly_64_files() {
    // Setup: 64 files from mixed tiers
    // Transfer: ThresholdDeltaPipeline
    // Verify: parallel mode triggered (assert via pipeline debug output),
    //         SHA-256 sweep
}

#[test]
fn threshold_boundary_63_files_sequential() {
    // Setup: 63 files from mixed tiers
    // Transfer: ThresholdDeltaPipeline
    // Verify: sequential fallback, SHA-256 sweep
}

#[test]
fn heterogeneous_100_tiny_1_huge() {
    // Setup: 100 x 1 B + 1 x 100 MB
    // Transfer: ThresholdDeltaPipeline
    // Verify: SHA-256 sweep, no reorder stall (bounded by timeout)
}

#[test]
fn empty_files_only_below_threshold() {
    // Setup: 10 x 0 B
    // Transfer: ThresholdDeltaPipeline (sequential fallback)
    // Verify: all 10 files exist with size 0
}
```

### 7.3 Feature gating

Tests that exercise the parallel path directly require the
`parallel-receive-delta` feature. The test module is gated:

```rust
#![cfg(feature = "parallel-receive-delta")]
```

The sequential-fallback and parity tests run unconditionally since
they exercise the `ThresholdDeltaPipeline`'s public API regardless of
feature flags.

### 7.4 Timeout

Each test has a 120-second nextest timeout. The 100 MB file dominates
the wall-clock time. On CI runners (GitHub Actions ubuntu-latest,
~2 cores), the full fixture generation + transfer + verification
should complete in under 60 seconds. The 120-second timeout provides
2x headroom.

### 7.5 Platform considerations

- **macOS**: `sha256sum` is not available by default. The test uses
  the `sha2` Rust crate instead of shelling out.
- **Windows**: file paths use backslashes. The fixture generator uses
  `Path::join` throughout.
- **tmpfs**: on Linux CI, `$TMPDIR` may point to tmpfs with limited
  space. The 134 MB source tree + 134 MB destination requires
  ~270 MB. CI runners have at least 10 GB of temporary space.

## 8. CI integration

### 8.1 Nextest filter

The mixed-size correctness tests are included in the standard
`cargo nextest run --workspace --all-features` CI step. No new CI
workflow is needed.

The 100 MB file tests are gated behind
`#[cfg(not(debug_assertions))]` to avoid excessive runtime in debug
builds. They run only in the release/dist CI profiles.

### 8.2 Interaction with PIP-10.a

PIP-10.a's interop matrix tests the parallel path against upstream
rsync. PIP-10.c tests the parallel path's internal correctness in
isolation (no upstream peer). The two suites are complementary:

- PIP-10.a catches wire-format divergences between oc-rsync and
  upstream.
- PIP-10.c catches reorder, dispatch, and reconstruction bugs that
  produce valid wire output but corrupted files.

### 8.3 Interaction with PIP-10.b

PIP-10.b uses adversarial orderings with uniform chunk sizes.
PIP-10.c uses realistic orderings (as produced by the actual pipeline)
with heterogeneous file sizes. Together they cover the two axes:

| Dimension | PIP-10.b | PIP-10.c |
|-----------|---------|---------|
| Ordering | Adversarial | Realistic (pipeline-natural) |
| File sizes | Uniform per test | Mixed in every test |
| File count | Parameterized sweep | Fixed at 114 (+ variants) |
| Scope | ReorderBuffer + applier | Full pipeline (threshold -> dispatch -> apply -> verify) |

## 9. Success criteria

PIP-10.c is complete when all of the following hold:

| Criterion | Verification method |
|-----------|-------------------|
| Zero SHA-256 mismatches across all scenarios | Per-file hash comparison (Section 6.1) |
| Parallel vs sequential byte-identical output | Manifest diff (Section 6.2) |
| Sub-threshold fallback produces identical results | 50-file subset vs parallel output match |
| Delta updates produce matched_bytes > 0 for modified files | Statistics check (Section 6.3) |
| Whole-file transfers produce matched_bytes == 0 | Statistics check |
| Empty and single-byte files reconstructed correctly | Individual assertions in Sections 5.1-5.2 |
| Threshold boundaries behave per specification | Exact-count tests (Section 5.3) |
| Idempotent re-sync transfers zero bytes | Statistics check (Section 4.5) |
| All tests pass on Linux, macOS, and Windows CI | Nextest matrix green |
| No test exceeds 120-second timeout | Nextest per-test timeout |

## 10. Risk catalogue

| Risk | Impact | Mitigation |
|------|--------|-----------|
| R1: 100 MB file makes CI slow or OOM | Test timeout or runner OOM | Gate behind `#[cfg(not(debug_assertions))]`; 120s timeout; CI has 7+ GB RAM |
| R2: Adaptive queue depth 2x too shallow for mixed workload | Reorder buffer stall, force_insert, potential deadlock | Verify completion within timeout; the consumer's `force_insert` is a correctness-preserving fallback |
| R3: Quick-check races on fast CI (same-second mtime) | Delta update test sees zero transfers | Backdate destination files by 2 seconds using `filetime` crate; use different sizes for modified files |
| R4: SHA-256 computation overhead for 100 MB files | Test wall-clock dominated by hashing | Accept: hashing 134 MB takes < 1s on modern hardware |
| R5: Per-file reorder buffer overflow on very large files | `CapacityExceeded` error from `ReorderBuffer::insert` | The pipeline's `force_insert` fallback handles this; PIP-10.b covers the adversarial case |
| R6: Platform-specific fs behavior (sparse, case sensitivity) | False positives on specific platforms | Use `Path::join`, avoid case-colliding names, skip sparse assertions |

## 11. Implementation punch list

| Task | Deliverable | Depends on |
|------|------------|-----------|
| PIP-10.c.1 | Fixture generator: `mixed_size_fixture()` function | - |
| PIP-10.c.2 | SHA-256 tree verifier: `verify_tree()` function | - |
| PIP-10.c.3 | Initial sync test (Section 4.1) | PIP-10.c.1, PIP-10.c.2 |
| PIP-10.c.4 | Delta update test (Section 4.2) | PIP-10.c.3 |
| PIP-10.c.5 | Sub-threshold fallback test (Section 4.3) | PIP-10.c.1, PIP-10.c.2 |
| PIP-10.c.6 | Parallel vs sequential parity test (Section 4.4) | PIP-10.c.1, PIP-10.c.2 |
| PIP-10.c.7 | Idempotent re-sync test (Section 4.5) | PIP-10.c.4 |
| PIP-10.c.8 | Mixed whole-file + delta test (Section 4.6) | PIP-10.c.1, PIP-10.c.2 |
| PIP-10.c.9 | Threshold boundary tests (Section 5.3) | PIP-10.c.1 |
| PIP-10.c.10 | Heterogeneous workload test (Section 5.4) | PIP-10.c.1, PIP-10.c.2 |
| PIP-10.c.11 | Empty-files-only test (Section 5.1) | PIP-10.c.1 |
| PIP-10.c.12 | CI validation: all tests green on Linux, macOS, Windows | PIP-10.c.3-11 |

## 12. References

### Code citations

- `crates/transfer/src/delta_pipeline/mod.rs:54` -
  `DEFAULT_PARALLEL_THRESHOLD = 64`.
- `crates/transfer/src/delta_pipeline/threshold.rs:37` -
  `ThresholdDeltaPipeline`.
- `crates/transfer/src/delta_pipeline/parallel.rs:147-159` -
  `adaptive_capacity()`, `SMALL_FILE_THRESHOLD`, `LARGE_FILE_THRESHOLD`.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:427` -
  `DEFAULT_PER_FILE_REORDER_CAPACITY = 64`.
- `crates/engine/src/concurrent_delta/strategy.rs:109-119` -
  `WholeFileStrategy`.
- `crates/engine/src/concurrent_delta/strategy.rs:143-164` -
  `DeltaTransferStrategy`.
- `crates/transfer/src/parallel_io.rs:16` -
  `DEFAULT_STAT_THRESHOLD = 64`.

### Design documents

- `docs/design/pip-10a-parallel-interop-matrix.md` - PIP-10.a spec.
- `docs/design/pip-10b-adversarial-chunk-ordering-stress.md` - PIP-10.b spec.
- `docs/design/pip-9-f-1-bake-criterion.md` - bake window criteria.
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md` -
  PIP-7 corruption investigation.
- `docs/design/parallel-receive-delta-application.md` - umbrella design.
- `docs/design/parallel-receive-delta-tuning.md` - tuning guidelines.

### Related PRs

- PIP-10.a (PR #5030) - full upstream interop matrix spec.
- PIP-10.b (PR #5029) - adversarial chunk ordering stress test spec.
- PIP-9.b.2 (PR #4776) - cfg-gated dispatch sketch.
- PIP-8 (#4731) - dead scaffolding teardown.
- PIP-7 (#4730, #4725) - corruption investigation and mitigation.
