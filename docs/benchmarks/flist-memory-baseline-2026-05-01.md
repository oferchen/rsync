# Full vs incremental flist memory baseline (2026-05-01)

Baseline numbers for `scripts/benchmark_flist_memory.sh` at 100K and 1M
directory scales. Compares peak RSS of oc-rsync v0.5.9 against upstream
rsync 3.4.1 under three flist modes:

- **Mode A** - full flist (`--no-inc-recursive`)
- **Mode B** - default (receiver INC_RECURSE; sender always sends full list)
- **Mode C** - sender INC_RECURSE (pending opt-in flag, see #1862; skipped)

Refs: #1864 (this benchmark), #966/#971 (RSS gap context).

## Environment

- Container: `rsync-profile` (rust:latest, Debian, aarch64-linux).
- oc-rsync: v0.5.9 (revision #4206bc00) protocol version 32.
- upstream rsync: 3.4.1 protocol version 32.
- Fixture: empty files in 1000-files-per-dir trees (`/tmp/oc-rsync-bench`,
  not bind-mounted).
- Push target: local destination (single rsync process; parent tracks the
  sender role).
- Peak RSS: `/usr/bin/time -v` "Maximum resident set size (kbytes)".
- Runs per mode: 3, median reported.

## Invocation

```sh
podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh \
    --scales both --summary
```

For just one scale:

```sh
podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh \
    --scales 100k --summary
podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh \
    --scales 1m --summary
```

Script writes TSV/MD output to
`/workspace/target/benchmarks/flist_memory_<timestamp>.{tsv,md}`. Fixtures
and destinations live under `/tmp/oc-rsync-bench` inside the container so
no bind-mount paths are touched.

## Results

### 100K files (100 dirs x 1000 files)

| Mode | Binary | Wall (s) | Peak RSS (MB) |
|------|--------|----------|---------------|
| A_full_flist          | oc-rsync | 2.813 | 42.7 |
| B_default             | oc-rsync | 2.913 | 42.6 |
| C_sender_inc_recurse  | oc-rsync | n/a   | n/a  |
| A_full_flist          | upstream | 3.679 | 14.2 |
| B_default             | upstream | 4.394 |  7.9 |
| C_sender_inc_recurse  | upstream | n/a   | n/a  |

### 1M files (1000 dirs x 1000 files)

| Mode | Binary | Wall (s) | Peak RSS (MB) |
|------|--------|----------|---------------|
| A_full_flist          | oc-rsync | 53.605 | 218.2 |
| B_default             | oc-rsync | 53.597 | 218.5 |
| C_sender_inc_recurse  | oc-rsync | n/a    | n/a   |
| A_full_flist          | upstream | 48.959 |  76.8 |
| B_default             | upstream | 58.700 |   7.5 |
| C_sender_inc_recurse  | upstream | n/a    | n/a   |

## Observations

1. **upstream INC_RECURSE delivers a 10x receiver-RSS reduction at 1M:**
   76.8 MB (Mode A) -> 7.5 MB (Mode B). At 100K, the gain is ~1.8x
   (14.2 MB -> 7.9 MB). The benefit scales with file count because the
   sender streams flist segments instead of buffering the full list.
2. **oc-rsync sees no benefit between Mode A and Mode B** because the
   sender direction still buffers the full flist. The receiver-side
   INC_RECURSE that ships today only helps when oc-rsync is the
   *receiver* of an incremental list - which a local push never exercises
   (the parent process is always the sender). That gap is exactly what
   issue #1862 (sender INC_RECURSE opt-in) targets.
3. **oc-rsync RSS gap vs upstream:**
   - 100K: 42.7 MB vs 7.9 MB = 5.4x (improvement on the 11.8x cited at
     v0.5 era; tracks the trend in #966/#971).
   - 1M:   218.2 MB vs 7.5 MB = 29x. The gap widens with file count
     because oc-rsync's flist memory grows linearly while upstream's stays
     bounded under INC_RECURSE.
4. **Wall-clock parity:** oc-rsync is faster at 100K (2.81 s vs 3.68 s in
   Mode A; 2.91 s vs 4.39 s in Mode B) but slightly slower at 1M Mode A
   (53.6 s vs 49.0 s). The slowdown correlates with the larger RSS
   footprint and is likely paging/cache-eviction driven.
5. **Mode C (sender INC_RECURSE) is the next wire-compatible win.** Once
   the #1862 opt-in flag lands, this benchmark will measure whether
   oc-rsync can hit upstream's ~7.5 MB ceiling at 1M.

## Reproduction cost

| Scale | Generation | Per push (median) | Total wall (script) |
|-------|------------|-------------------|---------------------|
| 100K  |  3 s       | ~3 s              | ~50 s               |
| 1M    | 41 s       | ~50-58 s          | ~12 min             |

Disk: 1M scale uses ~1 GB inode space (empty files) under
`/tmp/oc-rsync-bench`. Container needs ~2 GB free in `/tmp` for safety
(includes per-mode destination trees that are rebuilt between runs).
