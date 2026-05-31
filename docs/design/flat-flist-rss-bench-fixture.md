# Flat flist RSS bench fixture design (RSS-A.9.a)

Task: RSS-A.9.a. Branch: `docs/flat-flist-rss-bench-fixture`.
Prerequisites: RSS-A.8 (INC_RECURSE segment growth in flat flist).
Downstream: RSS-A.9.b (execute benchmark), RSS-A.9.c (CI regression gate).

## Summary

This document defines the benchmark fixture, measurement methodology,
and comparison baselines for validating that the flat file-list backing
store (RSS-A.5) closes the RSS gap against upstream rsync at
million-file scale. Prior work (RSS-1.b/1.c, 2026-05-29) established
a 25.9x RSS gap with INC_RECURSE and 2.6x without at 1M files. The
flat flist is the structural fix - this benchmark validates it delivers
the expected reduction.

## 1. Fixture generation

### 1.1 Entry count

The primary fixture is **1,000,000 synthetic file entries** - matching
the RSS-1.b/1.c measurement scale. A secondary 100K fixture provides a
fast-feedback tier for CI.

### 1.2 Directory depth and structure

Three path distribution models exercise different interning behaviors:

| Distribution | Structure | Unique dirs | Files/dir | Avg depth |
|-------------|-----------|-------------|-----------|-----------|
| **shared** | `workspace/pkg_NNN/src/mod_NN/item_NNNNNN.rs` | 1,000 | 1,000 | 5 |
| **deep** | `project/d0_X/d1_Y/.../file_NNNNNN.rs` | ~50,000 | ~20 | 4-8 |
| **wide** | `dir_NNNNN/file_NNNNN.dat` | 100,000 | 10 | 2 |

The **shared** distribution is the primary benchmark case: it models a
monorepo with high dirname sharing (best case for PathArena interning,
matching upstream's `lastdir` cache behavior). The **deep** distribution
models realistic projects. The **wide** distribution is the worst case
for interning - each directory appears only ~10 times.

### 1.3 Name length distribution

Basename lengths follow a realistic distribution matching measured source
trees:

| Percentile | Length | Example |
|-----------|--------|---------|
| 60% | 12-20 chars | `item_000001.rs`, `config.toml` |
| 25% | 20-35 chars | `test_integration_helper.rs` |
| 10% | 35-50 chars | `benchmark_flat_flist_allocation.rs` |
| 5% | 50-80 chars | Long generated names, test fixtures |

Dirname component lengths: 3-15 characters per component, 2-8
components deep. Total dirname string length: 10-80 bytes.

### 1.4 Extras variety

Not all entries carry extras - matching real-world distributions:

| Category | Percentage | Extras content |
|----------|-----------|----------------|
| Regular files (no extras) | 85% | `ExtrasRef::NO_EXTRAS` |
| Files with checksum | 5% | 16-byte MD5 or 32-byte XXH3 |
| Symlinks | 5% | link target path (20-60 bytes) |
| Files with ACL/xattr | 3% | ACL index + xattr index (8 bytes) |
| Devices/hardlinks | 2% | rdev major/minor or hardlink idx |

This ensures the ExtrasArena is exercised with realistic occupancy
(~15% of entries carry extras tails, matching observed production
distributions from large source trees).

### 1.5 Metadata field distributions

```rust
fn synthetic_size(i: usize) -> u64 {
    match i % 100 {
        0..=59 => (i % 4096) as u64,           // 60%: < 4 KiB
        60..=84 => ((i % 256) * 1024) as u64,  // 25%: 4-256 KiB
        85..=94 => ((i % 2048) * 1024) as u64, // 10%: 256 KiB - 2 MiB
        _ => ((i % 10240) * 1024) as u64,      // 5%:  2-10 MiB
    }
}

fn synthetic_mtime(i: usize) -> i64 {
    let base = 1_672_531_200_i64; // 2023-01-01 UTC
    let spread = 94_608_000_i64;  // ~3 years
    base + ((i as i64 * 7919) % spread)
}

fn synthetic_mode(i: usize) -> u32 {
    match i % 20 {
        0 => 0o100755,     // 5%: executable
        1..=2 => 0o100600, // 10%: private
        3..=4 => 0o040755, // 10%: directory
        _ => 0o100644,     // 75%: regular
    }
}
```

UID/GID values span 0-999 (uid = i % 1000, gid = i % 100) to exercise
the full u32 range without unrealistic cardinality.

## 2. Memory measurement methodology

### 2.1 Primary metric: peak RSS

Peak RSS is the high-water mark of the process's resident set size -
the same metric upstream rsync developers use for memory efficiency
assessment.

#### Linux

Read `/proc/self/status` field `VmHWM` (high-water mark) or use
`/usr/bin/time -v` "Maximum resident set size (kbytes)":

```rust
#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> usize {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kb: usize = rest.trim().trim_end_matches(" kB")
                    .trim().parse().unwrap_or(0);
                return kb * 1024;
            }
        }
    }
    0
}
```

#### macOS

Use `mach_task_info` with `MACH_TASK_BASIC_INFO` flavor to read
`resident_size_max`:

```rust
#[cfg(target_os = "macos")]
fn peak_rss_bytes() -> usize {
    // mach_task_self() + task_info(MACH_TASK_BASIC_INFO)
    // Returns resident_size_max field
}
```

#### getrusage (cross-platform fallback)

```rust
fn peak_rss_getrusage() -> usize {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
        // Linux: kilobytes, macOS: bytes
        #[cfg(target_os = "linux")]
        { usage.ru_maxrss as usize * 1024 }
        #[cfg(target_os = "macos")]
        { usage.ru_maxrss as usize }
    } else { 0 }
}
```

### 2.2 Differential measurement

To isolate flist memory from binary overhead and runtime baseline, the
benchmark uses a two-phase approach:

1. **Baseline RSS**: Measure RSS after process initialization, before
   building the file list. This captures binary text, stack, runtime
   structures, and allocator metadata.
2. **Loaded RSS**: Measure RSS after building the full flist (1M entries)
   and holding it live.
3. **Delta**: `loaded_rss - baseline_rss` = flist-attributable memory.

This isolates the per-entry cost from fixed overhead (oc-rsync binary
is ~6.3 MB vs upstream's ~543 KB, creating a constant offset).

### 2.3 Sampling and stability

- **Runs**: 5 iterations, report median.
- **Warm-up**: 1 discarded run to populate filesystem cache and
  warm the allocator.
- **Observed variance**: < 2% at 1M entries (RSS-1.c validated this).
- **Deterministic fixture**: No randomness - same i generates same
  fields across runs for bitwise reproducibility.

### 2.4 External validation

Wrap the benchmark binary with `/usr/bin/time -v` as an independent
cross-check against the in-process measurement:

```bash
/usr/bin/time -v target/release/flat-flist-rss-bench 2>&1 \
    | grep "Maximum resident"
```

## 3. Comparison baselines

### 3.1 Upstream rsync 3.4.1

Upstream peak RSS at 1M files (from RSS-1.b/1.c):

| Mode | Peak RSS | Per-entry cost |
|------|----------|----------------|
| INC_RECURSE (default) | 7.6 MB | ~0.6 B/entry (streaming) |
| --no-inc-recursive | 76.8 MB | ~70 B/entry |

Upstream uses pool allocation (`pool_alloc` in `pool_alloc.c`) with
contiguous 32 KB extents, plus `lastdir` caching for dirname sharing.
Under INC_RECURSE, entries are sent in streaming segments and freed
after transmission, never holding the full list simultaneously.

Re-measurement methodology: run `scripts/benchmark_flist_memory.sh`
against the upstream binary in the `rsync-profile` container with the
same 1M-file fixture.

### 3.2 oc-rsync legacy Vec<FileEntry>

Measured peak RSS at 1M files (RSS-1.b/1.c, v0.6.2):

| Mode | Peak RSS | Per-entry cost |
|------|----------|----------------|
| INC_RECURSE (default, push) | 197 MB | ~182 B/entry |
| --no-inc-recursive (dry-run) | 19 MB | ~12 B/entry |

The legacy representation stores:
- `Vec<FileEntry>` inline: 88 B/entry (with capacity doubling waste)
- Per-entry `PathBuf` name: 24 B inline + heap allocation (avg 45 B)
- `Arc<Path>` dirname: 8 B pointer (shared via PathInterner)
- `Option<Box<FileEntryExtras>>`: 224 B heap when populated

Total estimated: ~160-180 B/entry, matching observed 182 B/entry.

### 3.3 oc-rsync flat flist (target)

The flat flist representation stores:
- `FileEntryHeader`: 48 B/entry (contiguous, no capacity slack)
- `PathHandle` name: 4 B inline (arena-backed)
- `PathHandle` dirname: 4 B inline (arena-backed, deduplicated)
- `ExtrasRef`: 4 B inline (NO_EXTRAS for 85% of entries)
- PathArena overhead: amortized ~12-20 B/entry (shared strings)
- ExtrasArena overhead: amortized ~3-5 B/entry (15% occupancy)

**Target per-entry cost**: 63-73 B/entry (under --no-inc-recursive).

**Target RSS at 1M files**:
- --no-inc-recursive: ~70 MB (within 10% of upstream's 76.8 MB)
- INC_RECURSE: depends on segment streaming (streaming not yet wired)

**Target ratio**: < 1.1x upstream for non-INC_RECURSE; this validates
the backing store itself without requiring the streaming optimization.

## 4. INC_RECURSE vs non-INC_RECURSE configurations

### 4.1 Non-INC_RECURSE (primary validation target)

Under `--no-inc-recursive`, both upstream and oc-rsync hold the entire
file list in memory simultaneously. This isolates the per-entry storage
efficiency comparison without confounding from segment streaming:

- Upstream: 76.8 MB (70 B/entry in pool allocator)
- Legacy oc-rsync: 198 MB (182 B/entry in Vec + heap)
- Flat flist target: ~70 MB (63-73 B/entry, matching upstream)

This is the primary validation mode because it directly measures the
backing-store memory efficiency without streaming effects.

### 4.2 INC_RECURSE (secondary validation target)

Under INC_RECURSE (the default), upstream streams segments and frees
entries after transmission, holding only the current segment (~8 KB RSS
at 1M files). oc-rsync's sender-side INC_RECURSE (re-enabled PR #5085)
appends segments via `FlatFileList::append_segment()` but does not yet
free completed segments.

Benchmark configurations:

| Config | Description | Validates |
|--------|-------------|-----------|
| `flat-no-inc` | FlatFileList, all 1M entries held | Per-entry cost |
| `flat-inc-append` | FlatFileList, 10 segments appended | Segment growth |
| `flat-inc-streaming` | FlatFileList with segment drop | Streaming ceiling |
| `legacy-no-inc` | Vec<FileEntry>, baseline | Reference point |
| `legacy-inc` | Vec<FileEntry>, INC_RECURSE | Current behavior |

The `flat-inc-streaming` configuration exercises the future segment-drop
optimization (RSS-A.10+) where completed segments are released. For now,
it serves as a projection baseline by measuring one-segment-at-a-time RSS.

### 4.3 Segment parameters

INC_RECURSE segment sizing follows upstream:
- Segment size: ~1000 entries (upstream default flist segment)
- Total segments for 1M files: ~1000 segments
- Concurrent segments held: 1 (streaming) to all (append-only)

## 5. Per-entry memory accounting breakdown

### 5.1 FileEntryHeader (48 bytes)

```
Offset  Size  Field
------  ----  -----
 0       8    mtime: i64
 8       8    size: u64
16       4    uid: u32
20       4    gid: u32
24       4    name: PathHandle (u32)
28       4    dirname: PathHandle (u32)
32       4    extras: ExtrasRef (u32)
36       4    mtime_nsec: u32
40       4    mode: u32
44       2    flags: u16
46       2    present: u16
------  ----
Total: 48 bytes (no tail padding on 64-bit)
```

At 1M entries: 48 * 1,000,000 = **45.8 MiB** for headers alone.

Vec backing with exact capacity (no doubling): 48 MB.
With capacity doubling worst case: up to 96 MB. The benchmark uses
`with_capacity(count)` to avoid this - matching the production decode
path where capacity is pre-allocated from the advertised file count.

### 5.2 PathArena

The PathArena stores each unique string once in a contiguous byte
buffer, plus a spans table of `(offset: u32, len: u32)` per unique
string, plus a HashMap for dedup lookup.

Memory breakdown for the **shared** distribution (1M entries):
- Unique basenames: ~1,000,000 (each basename is unique per-entry)
- Unique dirnames: ~1,000 (high sharing)
- Average basename: 15 bytes (`item_000001.rs`)
- Average dirname: 35 bytes (`workspace/pkg_042/src/mod_07`)
- Arena bytes: 1M * 15 + 1K * 35 = **15.0 MB** basenames + 35 KB dirnames
- Spans table: (1M + 1K) * 8 = **7.6 MB**
- Dedup HashMap: ~(1M + 1K) * 56 = **53.4 MB** (HashMap overhead)

Total PathArena: ~76 MB for shared distribution.

For the **deep** distribution:
- Unique basenames: ~1,000,000
- Unique dirnames: ~50,000
- Arena bytes: 1M * 15 + 50K * 45 = 15.0 MB + 2.25 MB = **17.3 MB**
- Spans + HashMap scales similarly.

**Optimization note**: The dedup HashMap is only needed during build.
After construction is complete, it can be dropped (the `freeze()` model)
to reclaim ~53 MB. The benchmark must measure both states:
1. Peak RSS during build (includes HashMap)
2. Steady-state RSS after freeze (HashMap dropped)

### 5.3 ExtrasArena

The ExtrasArena stores length-prefixed blob records for entries with
extras (symlinks, devices, checksums, ACLs, xattrs):

At 15% extras occupancy (150,000 entries with extras):
- Average extras record size: ~24 bytes (2B presence + variable fields)
- Arena bytes: 150K * 24 = **3.4 MB**
- No per-entry overhead for the 85% without extras (just the 4-byte
  `ExtrasRef::NO_EXTRAS` sentinel in the header).

### 5.4 Sort index

The flat flist uses a `Vec<u32>` permutation index for sort order
(headers are never moved, only the index is permuted):

- At 1M entries: 1M * 4 = **3.8 MB**

### 5.5 Total accounting

| Component | Shared (1M) | Deep (1M) | Notes |
|-----------|------------|-----------|-------|
| Headers | 45.8 MB | 45.8 MB | Fixed per-entry |
| PathArena bytes | 15.0 MB | 17.3 MB | Strings only |
| PathArena spans | 7.6 MB | 8.0 MB | (offset, len) table |
| PathArena dedup | 53.4 MB | 56.0 MB | Dropped after build |
| ExtrasArena | 3.4 MB | 3.4 MB | 15% occupancy |
| Sort index | 3.8 MB | 3.8 MB | u32 permutation |
| **Total (during build)** | **129 MB** | **134 MB** | HashMap live |
| **Total (after freeze)** | **76 MB** | **78 MB** | HashMap dropped |

**Comparison at steady state (after freeze)**:
- Upstream (no-inc): 76.8 MB - target: **parity**
- Legacy oc-rsync: 198 MB - improvement: **2.5x reduction**
- Flat flist: ~76-78 MB - ratio vs upstream: **~1.0x**

## 6. CI integration for regression detection

### 6.1 Dedicated benchmark binary

A standalone binary (not a criterion bench) that:
1. Records baseline RSS
2. Builds the FlatFileList with 1M entries (shared distribution)
3. Optionally freezes the PathArena (drops dedup HashMap)
4. Records loaded RSS
5. Prints structured JSON output for CI parsing

```rust
// benches/flat_flist_rss_ci.rs or a binary in tools/
fn main() {
    let baseline = peak_rss_bytes();
    let flat = build_flat_entries(1_000_000, shared_path);
    flat.paths().freeze(); // drop dedup HashMap
    let loaded = peak_rss_bytes();
    let delta_mb = (loaded - baseline) / 1_048_576;

    println!(r#"{{"entries":1000000,"rss_mb":{},"baseline_mb":{}}}"#,
        delta_mb, baseline / 1_048_576);

    // Hold flat live until measurement complete
    std::hint::black_box(&flat);
}
```

### 6.2 Threshold-based CI gate

Follows the RSS-12.a pattern: a checked-in JSON baseline with a
percentage threshold.

Baseline file at `.github/baselines/rss-flat-flist-1m.json`:

```json
{
  "fixture": "flat-flist-1m-shared",
  "peak_rss_delta_mb": 76,
  "threshold_percent": 10,
  "updated": "2026-06-01",
  "notes": "Post RSS-A.8 flat flist baseline (after PathArena freeze)"
}
```

Pass/fail logic:

```
PASS: measured_delta_mb <= baseline * 1.10
FAIL: measured_delta_mb >  baseline * 1.10
```

At 76 MB baseline with 10% threshold: ceiling is 83.6 MB. This catches
regressions of > 7.6 MB (~7.6 B/entry) - sufficient to detect an
accidental re-introduction of per-entry heap allocation.

### 6.3 Workflow triggers

- **On PR**: Only when paths matching `crates/protocol/src/flist/**`,
  `crates/protocol/benches/**`, or `.github/baselines/rss-*` are
  modified. Avoids unnecessary runs on unrelated changes.
- **On push to master**: Always, to update the trend line.
- **Scheduled**: Weekly, to catch regressions from transitive dependency
  updates.

### 6.4 Platform

Linux-only (ubuntu-latest). `/proc/self/status` provides deterministic
RSS measurement. macOS measurement via `mach_task_info` is informational
only (Darwin page-cache makes RSS non-deterministic at this scale).

### 6.5 Bake-in period

Ships as `continue-on-error: true` for 2 weeks after landing. Once the
baseline stabilizes (3+ master runs within threshold), promote to a
required check (RSS-A.9.c).

### 6.6 Regression diagnostics

When the check fails, the workflow uploads:
- The raw JSON measurement output
- A per-component breakdown (headers, PathArena, ExtrasArena, sort index)
  computed from `FlatFileList` accessors
- A diff against the baseline showing which component grew

This enables immediate root-cause identification without reproducing
locally.

## 7. Implementation plan

| Step | Description | Deliverable |
|------|-------------|-------------|
| RSS-A.9.a | This design document | `docs/design/flat-flist-rss-bench-fixture.md` |
| RSS-A.9.b | Implement benchmark binary | `tools/bench_flat_flist_rss.rs` or similar |
| RSS-A.9.c | CI workflow + baseline | `.github/workflows/rss-flat-flist.yml` |
| RSS-A.9.d | Execute at 1M, validate ratio | Results in `docs/benchmarks/` |
| RSS-A.9.e | PathArena freeze optimization | If dedup HashMap dominates |

## 8. Success criteria

| Metric | Target | Rationale |
|--------|--------|-----------|
| Flat flist RSS (1M, no-inc, post-freeze) | < 85 MB | Within 10% of upstream 76.8 MB |
| Flat flist RSS (1M, no-inc, during build) | < 140 MB | Acceptable build-phase peak |
| Per-entry cost (steady state) | < 80 B | Matches upstream's ~70 B/entry |
| Flat/legacy ratio | < 0.45 | 2.2x+ improvement over 198 MB legacy |
| CI variance | < 3% | Stable enough for 10% threshold |

## 9. Cross-references

- RSS-1.b/1.c results: `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md`
- Flat flist design: `docs/design/flat-flist-representation.md`
- Existing criterion bench: `crates/protocol/benches/flat_flist_rss.rs`
- Legacy fixture bench: `crates/protocol/benches/flist_rss_fixture.rs`
- RSS CI workflow spec: `docs/design/rss-12a-ci-rss-regression-workflow.md`
- RSS measurement script: `scripts/benchmark_rss_flist.sh`
- INC_RECURSE segment growth: `docs/design/rss-a8a-inc-recurse-segment-audit.md`
