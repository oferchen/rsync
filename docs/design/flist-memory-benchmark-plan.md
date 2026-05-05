# Memory benchmark plan: full vs incremental flist (100K and 1M directory push)

Task: #1864. Branch: `docs/flist-memory-benchmark-1864`. Companion tasks:
#1037 (`FileEntry` 100K bench, completed), #1865 (sender INC_RECURSE state
machine, completed), #1862 (sender INC_RECURSE flag, in progress), #966
(RSS gap context), #971 (1M-file RSS scaling), #1048 (PathBuf/Arc<Path>
RSS overhead, completed), #1049 (string interning, completed), #1050
(`Vec<FileEntry>` vs upstream pool allocator, pending).

## 1. Goal

Quantify the resident-set delta between full file list and incremental file
list (`INC_RECURSE`) at the 100K and 1M directory-push scales for oc-rsync,
benchmarked against upstream rsync 3.4.1. The pre-existing 100K
in-process bench in `crates/protocol/benches/file_entry_memory.rs:21-110`
proved the inline `FileEntry` size invariant (`<= 96 B`) under #1037 but
covers neither (a) 1M scale, (b) end-to-end peak RSS during a push, nor
(c) the receiver-side INC_RECURSE pathway in
`crates/protocol/src/flist/incremental/mod.rs:80-94`.

The `docs/audits/pathbuf-arc-path-rss-overhead.md:212-238` audit (PR
#3704) established that path-related fields contribute 27-36 % of the
observed 34.7 MB RSS gap at 100 K files and forecast a 9-13 MB savings
ceiling at 100 K and 90-130 MB at 1 M; this benchmark plan supplies the
empirical control surface against which future arena/pool changes (#1050)
will be evaluated.

The reference target is upstream rsync's 7.4-7.9 MB peak RSS at 100 K
files in receiver INC_RECURSE mode (`docs/audits/file-entry-rss-snapshot.md`
not yet present; the empirical anchor is
`docs/benchmarks/flist-memory-baseline-2026-05-01.md:50-58` where
upstream Mode B = 7.9 MB at 100 K, 7.5 MB at 1 M). #966 tracks closing
that gap.

The deliverable from this benchmark plan is a reproducible TSV/MD report
landed under `target/benchmarks/flist_memory_*.{tsv,md}`, and a CI
regression gate that fails if oc-rsync's Mode B peak RSS regresses by
more than 10 % from the prior baseline at either scale.

## 2. Reuse strategy

The benchmark plan extends two existing harnesses rather than inventing a
new one:

- **`crates/protocol/benches/file_entry_memory.rs`** (the #1037 bench).
  Today it measures only inline `size_of::<FileEntry>()` and bulk
  allocation of 100 K entries via Criterion. Plan extends it with three
  new cases: (a) 1M entries, (b) post-decomposition path interning
  toggle, (c) per-entry footprint with extras boxed vs absent.
- **`scripts/benchmark_flist_memory.sh`** (introduced for the
  2026-05-01 baseline,
  `docs/benchmarks/flist-memory-baseline-2026-05-01.md:26-38`). The
  harness already runs `/usr/bin/time -v` against oc-rsync and upstream
  in three modes; plan adds Mode C (sender INC_RECURSE) once #1862's
  opt-in flag lands and tightens the median sampling to N=5 for the CI
  gate variant.

Reuse is preferred over a fresh harness because:

1. The Criterion bench provides allocation-pattern visibility
   (`crates/protocol/benches/file_entry_memory.rs:99-106` already prints
   `VmRSS`/`VmHWM` on Linux) that a shell script cannot.
2. The shell harness exercises the actual transfer wire path including
   the receiver-side `IncrementalFileList::push`/`pop` state machine
   (`crates/protocol/src/flist/incremental/mod.rs:147-201`) and the
   sender-side flist serialisation in
   `crates/protocol/src/flist/write/mod.rs`, which an in-process bench
   cannot reach without simulating the full pipeline.
3. The two harnesses cross-validate: discrepancy between the per-process
   `/usr/bin/time` peak and the in-process VmHWM read in
   `file_entry_memory.rs:104` flags either allocator overhead the inline
   bench misses or transient peaks the shell script captures but
   Criterion does not.

Net code addition is a fourth Criterion case (`allocate_1m_regular_files`),
plus a fifth optional one (`allocate_100k_with_extras`) gated behind
`bench_extras` to avoid blowing out CI bench wall time. Shell-script
edits are limited to (a) wiring Mode C through the
`build_capability_string(false)` toggle for `oc-rsync` once #1862 lands,
and (b) emitting machine-readable TSV that the CI gate can diff.

## 3. Workload generator

Directory tree shape mirrors the existing baseline harness
(`scripts/benchmark_flist_memory.sh:154-198` `generate_fixture`). Two
scales:

| Scale | Dirs | Files/dir | Total | Avg path | Tree depth |
|-------|-----:|----------:|------:|---------:|-----------:|
| 100K  |  100 |     1 000 |  100 000 | `dir_NNN/file_NNNNN.dat` ~ 22 B | 2 |
| 1M    | 1 000 |    1 000 | 1 000 000 | `dir_NNNN/file_NNNNN.dat` ~ 23 B | 2 |

Naming pattern: `dir_{:05d}/file_{:05d}.dat`. Five-digit zero-padded
names normalise the per-entry path footprint at ~22 B (matches the
audit's per-entry math in
`docs/audits/pathbuf-arc-path-rss-overhead.md:74-99`). Files are empty
(`pathlib.Path.touch()`,
`scripts/benchmark_flist_memory.sh:179-188`); the focus is file-list
memory, not transfer bytes.

Depth distribution is deliberately flat (depth = 2). Deeper trees would
amortise dirname interning across more entries per directory and
suppress the dirname cost the audit calls out
(`docs/audits/pathbuf-arc-path-rss-overhead.md:198-209`). A future
pass tracked under #1050 should add a deep-tree variant (depth = 5,
20 entries/dir) once the arena work lands; that variant exercises the
worst case for upstream's `lastdir` cache turnover
(`target/interop/upstream-src/rsync-3.4.1/flist.c:697-773`) and is the
limiting case for an arena-based redesign.

Fixture is generated once per scale and reused via the marker file
`scripts/benchmark_flist_memory.sh:162-169`. Fixture path is forced
under `/tmp/oc-rsync-bench` to avoid the bind-mount accident class
documented in the project pitfalls section
(`scripts/benchmark_flist_memory.sh:128-137`).

For the in-process Criterion path the workload mirrors upstream's
sort-stable layout: entries are emitted in
`(dirname, basename)`-sorted order matching
`crates/protocol/src/flist/sort.rs` so that the path interner
(`crates/protocol/src/flist/intern.rs:42-94`) hits its single-allocation
fast path per directory; otherwise the bench would overstate dirname
heap by re-Arc-ing the same path string
(`crates/protocol/src/flist/entry/core.rs:78-83` `extract_dirname`).

## 4. Measurement points

Three independent measurement axes, each with its own tool. Numbers are
reported per-mode-per-scale-per-binary as the median of N=5 runs.

### 4.1 Peak RSS (process level)

Source: `/usr/bin/time -v` "Maximum resident set size (kbytes)" on Linux,
parsed by `parse_rss_linux` in
`scripts/benchmark_flist_memory.sh:104-107`. macOS uses `time -l` with
the byte conversion in `parse_rss_macos:109-112`. Reported in MB rounded
to 1 decimal.

This captures everything the kernel attributes to the top-level
oc-rsync process: heap, stack, text, mmaps, kernel page cache for
file-backed mmaps, and any `Vec<FileEntry>` allocator slack. For local
push the parent process is the sender role
(`docs/benchmarks/flist-memory-baseline-2026-05-01.md:80-81`); the
receiver/generator children fork off and their RSS is *not* captured.
This is intentional - the sender holds the full flist when Mode A or
Mode B is active and is therefore the relevant memory consumer for the
issue this plan tracks (#966's RSS gap is sender-driven).

For Mode C (sender INC_RECURSE) the parent's peak RSS should drop
toward the per-segment ceiling (`SMALL_EXTENT = 128 KiB` per segment in
upstream's pool, `target/interop/upstream-src/rsync-3.4.1/rsync.h:936-937`,
or oc-rsync's `IncrementalFileList::ready` + `pending` queues sized to
one directory's worth of entries in
`crates/protocol/src/flist/incremental/mod.rs:80-94`).

### 4.2 In-process VmHWM and allocation count

Source: `/proc/self/status` `VmHWM` read at the end of the Criterion
bench function (`crates/protocol/benches/file_entry_memory.rs:99-106`).
This is monotonic peak high-water-mark since process start; it
sandwiches the bench body and gives a per-iteration ceiling Criterion
itself does not expose.

Allocation count source: dhat profiling under
`tools/dhat-profile/src/main.rs`. The harness today only profiles a
synthetic 100-file workload (`tools/dhat-profile/src/main.rs:43-46`);
this plan adapts it to (a) accept a fixture path, (b) run an in-process
oc-rsync `core::client::run_local_copy` against the fixture, (c) emit
`dhat-heap.json` keyed by mode+scale.

Run with `cargo run --release --profile dhat
--manifest-path tools/dhat-profile/Cargo.toml -- --scale 100k --mode B`.
Output JSON is compared against the prior run with `dhat-viewer`; a
flame graph showing `PathBuf::join` or `Arc::from(path)` dominating the
small-allocation count signals an audit hit
(`docs/audits/profiling-100k-files.md:144-148`).

### 4.3 Per-FileEntry footprint

Source: `std::mem::size_of::<FileEntry>()` plus the audit's per-entry
heap math (`docs/audits/pathbuf-arc-path-rss-overhead.md:88-99`).
Asserted at bench startup
(`crates/protocol/benches/file_entry_memory.rs:37-40`); plan
tightens the assertion target from `<= 96` to `<= 88` to lock in the
post-decomposition layout (`#2787`,
`crates/protocol/src/flist/entry/core.rs:32-72`) and prevent silent
inline growth.

Heap component is computed as
`(VmHWM_at_end - VmHWM_at_start) / N_entries`. With 100 K entries the
expected value is **~110-138 B per entry** (interned dirname,
audit table at
`docs/audits/pathbuf-arc-path-rss-overhead.md:198-209`); with 1 M
entries the same per-entry math should hold, validating linearity.

## 5. Comparison

Three configurations evaluated at each scale:

| Mode | oc-rsync | upstream rsync 3.4.1 | Notes |
|------|----------|----------------------|-------|
| A `--no-inc-recursive` | full flist sender + receiver | full flist sender + receiver | Pre-INC_RECURSE behaviour (protocol < 30). |
| B default | receiver INC_RECURSE; sender always full | full sender; receiver INC_RECURSE if negotiated | Today's default; tracks `IncrementalFileList::with_incremental_recursion` (`crates/protocol/src/flist/incremental/mod.rs:127-131`). |
| C sender INC_RECURSE | both sides incremental (#1862 opt-in) | both sides incremental (default for protocol >= 30) | Compatible once `build_capability_string(!is_sender)` is replaced with `build_capability_string(true)` for both directions; covered by #1862. |

Sender INC_RECURSE in oc-rsync is unblocked once the segment writer
(`crates/protocol/src/flist/write/mod.rs`) emits flists in directory
batches keyed by `parent_dir_ndx`
(`crates/protocol/src/flist/segment.rs:21-32`). The receiver-side
machinery in `IncrementalFileList::release_pending_children`
(`crates/protocol/src/flist/incremental/mod.rs:179-191`) and the
`StreamingFileList` reader (`incremental/streaming.rs:18-80`) are
already in place; the gating is purely a sender-direction capability
flip.

Sample table (anchored against the
2026-05-01 baseline,
`docs/benchmarks/flist-memory-baseline-2026-05-01.md:50-68`):

```
                       100K                         1M
                       ----                         --
                  oc-rsync  upstream         oc-rsync   upstream
Mode A (full)      42.7 MB    14.2 MB         218.2 MB   76.8 MB
Mode B (default)   42.6 MB     7.9 MB         218.5 MB    7.5 MB
Mode C (sender)    pending    7.4 MB          pending     7.4 MB  <- target
```

The Mode C target for oc-rsync is **<= 1.25x upstream**: 9.3 MB at
100 K (target 7.4 * 1.25 + 0.0 fudge) and 9.3 MB at 1 M. Achieving that
closes #966. Anything above 1.5x is a regression.

## 6. Tooling

Three tools, one per measurement axis, all already present in the
workspace:

- **dhat** (allocation profiling). Workspace member at
  `tools/dhat-profile/` (declared at workspace root
  `Cargo.toml:166`). The `[profile.dhat]` profile inherits from
  `release` with `debug = 1` for symbol fidelity
  (`Cargo.toml:337`). Output is `dhat-heap.json` consumable by
  `dhat-viewer`. Use case: identify whether the residual gap above
  the audit's predicted ceiling is `Vec<FileEntry>` capacity slack,
  HashMap rebucketing in `IncrementalFileList::pending`
  (`crates/protocol/src/flist/incremental/mod.rs:84-86`), or
  `PathInterner` HashMap headroom
  (`crates/protocol/src/flist/intern.rs:43-48`).

- **`/proc/self/status`** (RSS sampling). Read by
  `crates/protocol/benches/file_entry_memory.rs:99-106` (Linux only).
  Cross-process variant uses `/usr/bin/time -v` parsed by
  `scripts/benchmark_flist_memory.sh:104-107`. macOS lacks the
  in-process VmHWM key in `kern.proc.pid` ABI; the bench downgrades to
  a single Criterion measurement on macOS and the shell harness uses
  `time -l` for cross-process numbers.

- **Criterion** (timing harness, secondary). Existing benches in
  `crates/protocol/benches/` declare `harness = false` in
  `crates/protocol/Cargo.toml:60-74`. Used here only to (a) drive the
  bulk allocation, (b) report per-iteration wall time as a sanity
  check that the same workload is being measured across runs, and (c)
  honour `--save-baseline` / `--baseline` for diff-style invocation
  (`docs/audits/pathbuf-arc-path-rss-overhead.md:411-417`). Memory
  numbers are *not* reported through Criterion's CI integration; they
  go through the TSV path.

## 7. Pass/fail criteria

Anchored against upstream's 7.4-7.9 MB at 100 K (Mode B,
`docs/benchmarks/flist-memory-baseline-2026-05-01.md:56`) and 7.5 MB at
1 M (`:67`), both of which are upstream's INC_RECURSE pool ceiling. The
ceiling is upstream's pool extent rounding
(`target/interop/upstream-src/rsync-3.4.1/rsync.h:936-937`,
`SMALL_EXTENT = 128 KiB`, `NORMAL_EXTENT = 256 KiB`) and is the
ceiling #966 targets.

| Criterion | Threshold | Source |
|-----------|-----------|--------|
| Inline `size_of::<FileEntry>()` | `<= 88 B` (was `<= 96`) | `crates/protocol/src/flist/entry/tests.rs:296-304` |
| Mode A oc-rsync 100 K | `<= 50 MB` (current 42.6 MB + 17 % headroom) | `docs/benchmarks/flist-memory-baseline-2026-05-01.md:50` |
| Mode B oc-rsync 100 K | `<= 47 MB` (current 42.6 MB + 10 % headroom; tightens once #1050 lands) | `:51` |
| Mode C oc-rsync 100 K | `<= 9.3 MB` (1.25x upstream 7.4 MB; #966 closing target) | upstream Mode B baseline |
| Mode A oc-rsync 1 M | `<= 240 MB` (current 218.2 MB + 10 %) | `:63` |
| Mode B oc-rsync 1 M | `<= 240 MB` (same as A; no benefit until Mode C) | `:64` |
| Mode C oc-rsync 1 M | `<= 9.4 MB` (1.25x upstream 7.5 MB) | upstream Mode B baseline |
| Wall regression | `<= 10 %` vs prior baseline at the same scale and mode | TSV diff |
| dhat allocation count | `<= 1.10x` of prior `dhat-heap.json` total alloc count | `tools/dhat-profile` JSON |

Pass/fail decision logic:

- **Pre-#1862 landing** Mode C is N/A; the gate enforces only Modes A
  and B. Mode B regression is the primary signal.
- **Post-#1862 landing** Mode C becomes mandatory and Modes A/B
  thresholds tighten as the Mode C result demonstrates that arena/pool
  work is shipping; the spec calls for Mode B `<= 33 MB` at 100 K and
  `<= 200 MB` at 1 M post-#1050 (consistent with the
  9-13 MB/90-130 MB savings ceiling forecast in
  `docs/audits/pathbuf-arc-path-rss-overhead.md:251-254`).
- **Post-#1050 landing** The inline assertion tightens to `<= 80 B`
  (option A) or `<= 72 B` (option B) per the audit's
  recommendation matrix (`:392-396`). Threshold table is updated in
  the same commit that tightens the assertion.

A regression beyond threshold blocks merge; a regression within
threshold but beyond half-threshold opens an issue tagged
`memory-baseline` and is reviewed at the next planning checkpoint.

## 8. Run instructions and expected duration

Local development run (one scale):

```sh
podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh \
    --scales 100k --summary
```

Full run (both scales):

```sh
podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh \
    --scales both --summary
```

In-process Criterion variant (per-entry footprint, allocation count):

```sh
podman exec rsync-profile cargo bench \
    --manifest-path /workspace/Cargo.toml \
    -p protocol --bench file_entry_memory \
    -- file_entry_memory --save-baseline pre-1864
# apply change
podman exec rsync-profile cargo bench \
    --manifest-path /workspace/Cargo.toml \
    -p protocol --bench file_entry_memory \
    -- file_entry_memory --baseline pre-1864
```

dhat allocation profile:

```sh
podman exec rsync-profile cargo run --release --profile dhat \
    --manifest-path /workspace/tools/dhat-profile/Cargo.toml \
    -- --scale 100k --mode B
# move dhat-heap.json to /workspace/target/profiles/100k-modeB.json
```

Expected duration (anchored against
`docs/benchmarks/flist-memory-baseline-2026-05-01.md:97-105`):

| Phase | 100 K | 1 M |
|-------|-------|-----|
| Fixture generation | 3 s | 41 s |
| Per push (median of 5 runs, 6 modes) | ~3 s x 30 = 90 s | ~50 s x 30 = 25 min |
| Criterion bench | 10 s | 90 s |
| dhat profile | 30 s | 5 min |
| **Total wall (one scale)** | ~3 min | ~30 min |
| **Total wall (both scales)** | ~33 min | - |

Disk: 1 M scale uses ~1 GB inode space under `/tmp/oc-rsync-bench`
inside the container; container needs ~2 GB free in `/tmp` for safety
(per-mode destination trees rebuilt between runs,
`scripts/benchmark_flist_memory.sh:271-294`).

## 9. Hookpoint for CI regression gate

The gate runs only on the `memory-baseline` workflow trigger (manual or
nightly cron), not on every PR. Reasons:

1. The 1 M scale is 30 minutes per run; fitting that into the per-PR
   matrix would dominate runner cost.
2. RSS is sensitive to kernel version and allocator (glibc vs musl vs
   jemalloc); the gate requires a fixed image to be meaningful.
3. The gate's job is to catch regressions across releases, not to
   block individual PRs. Per-PR signal comes from the inline-size
   assertion in
   `crates/protocol/src/flist/entry/tests.rs:296-304`, which runs on
   every nextest invocation.

Workflow shape:

```
.github/workflows/memory-baseline.yml
  trigger: workflow_dispatch + cron('0 4 * * 1')   # weekly Monday 04:00 UTC
  job: run-bench
    container: ghcr.io/.../oc-rsync-bench:latest   # arch base-devel image (existing)
    steps:
      - checkout
      - cargo bench file_entry_memory --message-format=json > criterion.json
      - bash scripts/benchmark_flist_memory.sh --scales both --summary
      - cargo run --profile dhat --manifest-path tools/dhat-profile/Cargo.toml -- --scale 100k --mode B
      - python3 tools/ci/diff_memory_baseline.py \
            --prior target/benchmarks/flist_memory_prior.tsv \
            --current target/benchmarks/flist_memory_*.tsv \
            --thresholds tools/ci/memory_thresholds.toml
      - upload-artifact: target/benchmarks/, target/profiles/, criterion.json
```

`tools/ci/diff_memory_baseline.py` is new; signature mirrors the
existing `tools/ci/run_interop.sh` style. Threshold table lives in
`tools/ci/memory_thresholds.toml` and is updated in the same commit
that bumps any threshold (Section 7).

`target/benchmarks/flist_memory_prior.tsv` is the canonical baseline,
checked into the repo at
`docs/benchmarks/flist-memory-baseline-prior.tsv` and refreshed only
when a passing run promotes a new baseline (manual review). This avoids
ratcheting on noisy days.

The gate produces a markdown comment on the triggering PR (or a GitHub
issue when the trigger is cron) with the comparison table and a
flag list of any criterion exceeded.

## 10. Open questions

1. **Container vs bare-metal numbers.** The
   `docs/benchmarks/flist-memory-baseline-2026-05-01.md:14-15` baseline
   was taken on aarch64 in the `rsync-profile` container
   (rust:latest, Debian). The CI runner is x86_64 Ubuntu in the
   `oc-rsync-bench` container (Arch base-devel). Allocator differs
   (glibc both but version skew). Need a one-time cross-arch
   calibration run to establish the constant offset between the two
   environments, or pin the gate to one image. Recommendation: pin to
   the `oc-rsync-bench` Arch image since the existing benchmark
   automation in `scripts/benchmark.sh` already uses it.

2. **Receiver-only RSS measurement.** The shell harness measures the
   parent (sender) process. For a fair comparison against upstream's
   Mode B (where the receiver is the memory-bounded side), need to
   add a receiver-side `/usr/bin/time` wrapper. This requires either
   `--rsh` injection or a daemon mode push. Recommendation: add a
   `--target receiver` flag to the script that uses `--protocol=29` to
   force a fork-then-exec path where the child can be timed.

3. **Effect of `MALLOC_ARENA_MAX`.** glibc's per-thread arena
   allocation (default 8 x num_cpus) inflates VmRSS without inflating
   actual heap. The audit's per-entry math implicitly assumes a single
   arena. Recommendation: set
   `MALLOC_ARENA_MAX=2` for the gate runs and document the choice in
   the workflow file; report both with-and-without numbers in the
   first calibration cycle.

4. **`Vec<FileEntry>` capacity slack.** The audit forecasts up to
   4-5 MB at 100 K from `Vec` doubling
   (`docs/audits/pathbuf-arc-path-rss-overhead.md:222-230`).
   `FileListReader` already pre-sizes via `Vec::with_capacity` on most
   paths but not all. Open question: which call site causes the
   residual slack? Plan to instrument with dhat and file the answer
   into `docs/audits/file-entry-rss-snapshot.md` as a follow-up audit.

5. **Mode C wall-clock cost, extras footprint, and skew.** Mode C
   trades RSS for protocol round trips (1 K extra round trips at the
   1 M scale, negligible on a local pipe but dominant on a WAN SSH
   transfer); collect at 0/50/200 ms RTTs once #1862 lands as a
   follow-up under that issue. The benchmark uses empty regular files
   so `Box<FileEntryExtras>`
   (`crates/protocol/src/flist/entry/core.rs:51-55`) stays `None` and
   contributes 0 B of heap; an `extras-on` fixture (hardlinks or
   xattrs) is filed as a follow-up alongside a Pareto-distributed
   variant for real-world skew (kernel checkout, photo library,
   package mirror).

## References

- `crates/protocol/benches/file_entry_memory.rs:1-110` - existing 100 K
  Criterion bench (#1037).
- `crates/protocol/src/flist/entry/core.rs:32-83` - `FileEntry` post-
  decomposition layout, `extract_dirname` helper.
- `crates/protocol/src/flist/entry/extras.rs` - `Box<FileEntryExtras>`
  rare-field container (#1275).
- `crates/protocol/src/flist/entry/tests.rs:296-304` - `<= 96 B` inline
  assertion (this plan tightens to `<= 88 B`).
- `crates/protocol/src/flist/incremental/mod.rs:80-94,127-131,147-201`
  - `IncrementalFileList` state machine; `incremental/streaming.rs:18-80`
  `StreamingFileList` wire reader; `segment.rs:21-64` `FileListSegment`
  matching upstream `flist->ndx_start`.
- `crates/protocol/src/flist/intern.rs:42-94` - `PathInterner` (#1049).
- `crates/protocol/src/flist/sort.rs` - sort-stable layout that
  `extract_dirname` relies on for one-Arc-per-directory.
- `crates/protocol/Cargo.toml:60-74` - bench harness declarations.
- `crates/core/src/client/remote/invocation/builder.rs:180` and
  `transfer::setup::build_capability_string` - the toggle that #1862
  flips for sender INC_RECURSE.
- `scripts/benchmark_flist_memory.sh:1-388` - shell harness (#1864
  baseline; this plan extends Mode C and TSV emission).
- `tools/dhat-profile/src/main.rs:34-66` - allocation profiler harness
  (this plan adapts to accept fixture path and mode).
- `docs/audits/pathbuf-arc-path-rss-overhead.md` (PR #3704) - per-entry
  cost model anchoring the pass/fail thresholds.
- `docs/audits/profiling-100k-files.md:23-104` - companion audit on
  100 K-file CPU hot paths and `PathBuf::join` allocation pressure.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` - the empirical
  baseline this plan ratchets against.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697-773,1018-1027,2914-2937,2969-2971`
  - upstream pool layout, `lastdir` cache, `pool_alloc`/`pool_destroy`.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-870,936-937` -
  `struct file_struct`, `union file_extras`, `SMALL_EXTENT`,
  `NORMAL_EXTENT` pool extent constants.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:574-594` -
  `setup_protocol` initialising `file_extra_cnt`.
