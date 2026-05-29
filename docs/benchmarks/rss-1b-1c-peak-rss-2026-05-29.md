# RSS-1.b/1.c: Peak RSS measurement at million-file scale (2026-05-29)

Tasks: RSS-1.b (capture peak RSS), RSS-1.c (validate gap reproducibility).
Companion: RSS-1.a (million-file flist fixture, completed).

## Environment

- Container: `rsync-profile` (rust:latest, Debian, aarch64-linux).
- oc-rsync: v0.6.2 (revision #6ece5e08e) protocol version 32.
- upstream rsync: 3.4.1 protocol version 32.
- Fixture: empty files in shared-prefix directory trees (`/tmp/rss_1m`
  with 1000 dirs x 1000 files = 1M files; `/tmp/rss_100k` with 100
  dirs x 1000 files = 100K files).
- Peak RSS: `/usr/bin/time -v` "Maximum resident set size (kbytes)".
- Runs per configuration: 3-7, median reported.
- All measurements are local push (single process, sender+receiver).

## Results

### Actual push (`-a`, real transfer of empty files)

| Scale | Mode | Upstream RSS | oc-rsync RSS | Ratio |
|-------|------|-------------|-------------|-------|
| 100K | Default (INC_RECURSE) | 7.7 MB | 26.6 MB | **3.5x** |
| 1M | Default (INC_RECURSE) | 7.6 MB | 197 MB | **25.9x** |
| 1M | --no-inc-recursive | 76.8 MB | 198 MB | **2.6x** |

### Dry-run (`-an`, no actual transfer)

| Scale | Mode | Upstream RSS | oc-rsync RSS | Ratio |
|-------|------|-------------|-------------|-------|
| 100K | Default (INC_RECURSE) | 7.9 MB | 17.1 MB | **2.2x** |
| 1M | Default (INC_RECURSE) | 7.9 MB | 19.1 MB | **2.4x** |
| 1M | --no-inc-recursive | 78.6 MB | 19.1 MB | **0.24x** |
| 0 (empty dir) | Default | 7.0 MB | 14.8 MB | **2.1x** |

### Stability (RSS-1.c)

Reproducibility across 5-7 runs, 1M files:

| Binary | Mode | Median | Range | Spread |
|--------|------|--------|-------|--------|
| Upstream | Default | 7,956 KB | 7,924 - 8,000 KB | 1.0% |
| Upstream | --no-inc-recursive | 78,672 KB | 78,576 - 78,696 KB | 0.2% |
| oc-rsync | Default (push) | 197,200 KB | 196,672 - 199,720 KB | 1.5% |
| oc-rsync | Default (dry-run) | 19,120 KB | 19,052 - 19,240 KB | 1.0% |
| oc-rsync | --no-inc-recursive (dry-run) | 19,116 KB | 19,036 - 19,232 KB | 1.0% |

All measurements are highly stable (spread < 2%). The gap reproduces
reliably across runs.

## Analysis

### Push mode: 3.5-26x gap, scaling linearly with file count

In actual push transfer, oc-rsync's RSS grows linearly with file count:
~27 MB at 100K, ~197 MB at 1M. Upstream stays bounded at ~7.6 MB
regardless of file count (thanks to INC_RECURSE streaming).

The per-file overhead is:
- oc-rsync: (197,000 - 14,800) / 1,000,000 = **182 bytes/file**
- upstream: (7,600 - 7,000) / 1,000,000 = **0.6 bytes/file** (streaming)
- upstream (no-inc): (76,800 - 7,000) / 1,000,000 = **70 bytes/file**

oc-rsync's 182 bytes/file in push mode is consistent with the RSS-3
audit's estimate of ~128 bytes/entry for the Vec<FileEntry> inline +
heap cost, plus sender-side buffers and protocol overhead.

### Dry-run mode: 2.1-2.4x gap, flat (dominated by binary overhead)

In dry-run mode, oc-rsync's RSS is nearly constant (~17-19 MB) regardless
of file count. The flist is not fully materialized or is freed during
dry-run processing. The ~2x gap vs upstream is dominated by the larger
binary (6.3 MB vs 543 KB) and Rust runtime overhead.

### The INC_RECURSE effect

Upstream's INC_RECURSE is highly effective: 78.6 MB -> 7.9 MB at 1M
files (10x reduction). oc-rsync's sender-side INC_RECURSE (re-enabled
PR #5085, 2026-05-28) does not yet deliver the same benefit in push
mode - the sender still buffers the full flist before transmitting.

### Comparison with v0.5.9 baseline (2026-05-01)

| Scale | Metric | v0.5.9 | v0.6.2 | Change |
|-------|--------|--------|--------|--------|
| 100K | oc-rsync push RSS | 42.7 MB | 27 MB | -37% |
| 1M | oc-rsync push RSS | 218 MB | 197 MB | -10% |
| 1M | Ratio vs upstream | 29x | 26x | Marginal |

Some improvement from v0.5.9, but the core issue - linear flist growth -
remains. The reduction at 100K suggests per-entry overhead decreased
(possibly from FileEntry size reduction or better interning), but the
1M scaling behavior is unchanged.

### Root cause confirmation

The RSS gap in push mode is confirmed to be driven by `Vec<FileEntry>`
with `PathBuf`/`Arc<Path>` per entry, as documented in RSS-3 audit.
At 1M files, the Vec backing store alone is 88 * 1M = 84 MB, plus
~100 MB of heap-allocated path buffers and allocator metadata.

Upstream's pool allocator avoids this by bump-allocating entries
contiguously in 8-32 KB extents, and INC_RECURSE further avoids holding
the full list by streaming segments.

## Scripts

- `scripts/rss_gen_files.c` - creates the 1M-file fixture
- `scripts/rss_measure.sh` - automated RSS measurement (dry-run mode)
- `scripts/benchmark_flist_memory.sh` - comprehensive push-mode benchmark

## Cross-references

- RSS-1.a (million-file fixture): `crates/protocol/benches/flist_rss_fixture.rs`
- RSS-3 audit: `docs/audits/rss-3-fileentry-size-breakdown.md`
- v0.5.9 baseline: `docs/benchmarks/flist-memory-baseline-2026-05-01.md`
- RSS gap tracking: `project_rss_3_11x_upstream.md`
