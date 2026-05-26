# IUS-8.b.3 - IoUringBackend trait migration perf-regression bench

Date: 2026-05-26
Tracking: #2956. Parent: IUS-8 series (IoUringBackend trait abstraction).
Predecessor: IUS-8.b.2 (caller migration to trait, PR #5015 merged).
Upstream specs: IUS-7.a (trait surface, 57 methods across 5 traits),
IUS-7.b (zero-cost guarantee, 2 % CI gate, asm-diff methodology),
IUS-8.a (trait definition), IUS-8.b.1 (Linux impl skeleton).
Status: **SPEC DRAFT** - no source changes in this PR.

---

## 0. Why this bench exists

IUS-8.b.2 migrated all callers in `engine`, `transfer`, `core`, and
`daemon` from direct free-function calls to generic-dispatch through
`IoUringBackend`. IUS-7.b promised zero-cost on Linux via
monomorphization: LLVM should inline every `#[inline(always)]` trait
method, producing assembly identical to the pre-trait direct-call path.

**This spec designs the empirical verification that the promise held.**

The IUS-7.b spec (section 3) defined the verification methodology -
asm-diff, micro-bench, CI gate - but targeted the IUS-8.b.1 impl PR
(trait coexisting with free functions). IUS-8.b.2 is the riskier
change: it rewired real callers, introduced generic bounds across crate
boundaries, and passed the backend through several layers of function
calls. Any of these could prevent inlining:

- A generic bound at a crate boundary that rustc does not monomorphize
  without thin LTO (the workspace uses thin LTO in release; dev builds
  do not).
- An accidental `&dyn IoUringBackend` in a migrated caller.
- A function boundary too deep for `#[inline(always)]` to propagate
  through.
- A new `Arc` clone or `Box` allocation introduced during migration.

The bench in this spec catches these regressions with four layers:
assembly inspection, micro-benchmarks, end-to-end transfer benchmarks,
and syscall profiling.

## 1. Workload selection

Three workload classes, chosen to stress different parts of the trait
surface that IUS-8.b.2 migrated:

### 1.1 Metadata-heavy (high-IOPS)

Targets the `statx`, `linkat`, `renameat2` trait methods and the probe
cache (`probe_op`, `statx_supported`, etc.). These methods fire once
per file on the receiver path. File count is the scaling dimension.

| Cell ID | Files | Per-file | Total | Trait methods exercised |
|---------|-------|----------|-------|------------------------|
| `meta_10k` | 10,000 | 4 KiB | ~40 MiB | `submit_statx_batch`, `statx_supported`, `submit_one(Statx)`, `submit_one(Renameat2)` |
| `meta_100k` | 100,000 | 4 KiB | ~400 MiB | Same, at 10x scale |

These cells match IUB-3 (high-IOPS bench design) section 2.1 fixtures.
The 10K cell runs on every PR; the 100K cell is env-gated
(`OC_RSYNC_BENCH_IOURING_LARGE=1`).

### 1.2 Data-heavy (throughput-bound)

Targets `submit_one(Write)`, `submit_batch`, `submit_and_wait`,
`drain_completions`, and `DiskBatch::write_data` - the per-chunk hot
loop on the receiver disk-commit path.

| Cell ID | Files | Per-file | Total | Trait methods exercised |
|---------|-------|----------|-------|------------------------|
| `data_2g` | 1 | 2 GiB | 2 GiB | `submit_one(Write)`, `submit_batch`, `submit_and_wait`, `drain_completions` |
| `data_10g` | 1 | 10 GiB | 10 GiB | Same, at 5x scale |

These cells match IUB-2 (multi-GB bench design) section 2 fixtures.
The 2 GiB cell is the CI default; the 10 GiB cell is env-gated
(`OC_RSYNC_BENCH_IOURING_LARGE=1`).

### 1.3 Mixed (real-world transfer shape)

An end-to-end daemon pull transfer that exercises the full migrated
code path: handshake through trait-dispatched backend, file list via
`submit_statx_batch`, delta transfer via `DiskBatch::write_data`,
commit via `submit_one(Renameat2)`.

| Cell ID | Files | Per-file | Total | Mode |
|---------|-------|----------|-------|------|
| `e2e_daemon_10k` | 10,000 | mixed (1 KiB - 64 KiB) | ~150 MiB | daemon pull, cold start |
| `e2e_local_10k` | 10,000 | mixed | ~150 MiB | local copy |

The daemon cell reuses the DIS-8.a cold-start fixture shape (scaled to
10K files). The local-copy cell exercises the `engine` crate's
`local_copy` executor, which was the largest migration surface in
IUS-8.b.2.

## 2. Before/after comparison approach

### 2.1 Baseline: pre-trait commit

The baseline is the commit immediately before IUS-8.b.2 merged - the
last commit where callers used direct free-function calls. The bench
script checks out this commit, builds in release mode, and runs the
full cell matrix. Results are saved to
`target/bench/ius-8b3/baseline/`.

```sh
BASELINE_REF=$(git log --oneline --ancestry-path IUS-8.b.1..IUS-8.b.2~1 | tail -1 | cut -d' ' -f1)
```

### 2.2 Post-trait: current HEAD

The post-trait measurement builds current HEAD (with all IUS-8.b.2
caller migrations in place) and runs the same cell matrix. Results are
saved to `target/bench/ius-8b3/post-trait/`.

### 2.3 Comparison script

`tools/ci/ius_8b3_compare.py` reads both result directories and
produces a markdown summary table with per-cell deltas. The script:

1. Parses criterion JSON estimates from both baseline and post-trait.
2. Computes the ratio `post_trait_mean / baseline_mean` per cell.
3. Computes delta percentages for throughput, p50 latency, p99 latency.
4. Flags cells that exceed the acceptable regression threshold.
5. Outputs a markdown summary suitable for the PR body.

### 2.4 Reproducibility

Both baseline and post-trait builds use the same:

- Rust toolchain (pinned in `rust-toolchain.toml`, currently 1.88.0).
- Cargo profile (`--release`, thin LTO, `opt-level = 3`).
- OS kernel (the CI runner does not reboot between measurements).
- CPU governor (`performance` pinned via `cpupower frequency-set`).
- File system (ext4 `noatime,data=ordered` on dedicated loopback).
- Memory cgroup (`MemoryMax=4G`, `MemorySwapMax=0`).

Cache drops (`echo 3 > /proc/sys/vm/drop_caches`) run between every
cell iteration.

## 3. Metrics

### 3.1 Throughput (MB/s)

Wall-clock bytes transferred per second. Primary metric for data-heavy
and end-to-end cells. Measured by criterion (micro-bench cells) and
hyperfine (end-to-end cells).

### 3.2 Latency (p50 / p99)

Per-operation latency for micro-bench cells: the time from SQE
submission to CQE reap. Measured via `hdrhistogram` inside the
criterion bench body. The p99 captures tail-latency regressions from
unexpected allocations or cache misses introduced by the trait layer.

### 3.3 Syscall count

Total `io_uring_enter` calls per transfer. Measured via
`perf stat -e syscalls:sys_enter_io_uring_enter`. A higher syscall
count would indicate that the trait layer disrupted SQE batching (e.g.,
by forcing per-SQE `submit_and_wait` instead of batched submission).

Expected: identical syscall count before and after. Any delta is a bug
in the migration, not an expected cost of the trait layer.

### 3.4 Instructions per operation

`perf stat -e instructions,cycles` on the micro-bench cells. The
instruction count is the most stable cross-run metric and the best
proxy for "did the compiler emit the same code." A non-zero delta in
instruction count at the per-op level proves a codegen regression
regardless of wall-clock noise.

### 3.5 I-cache miss rate

`perf stat -e L1-icache-load-misses` on the data-heavy cells.
IUS-7.b section 2.1 mandates this check: `#[inline(always)]` on 38
methods risks I-cache pressure. A >2 % regression in I-cache miss rate
triggers investigation of which methods should drop to `#[inline]`.

## 4. Acceptable regression thresholds

The thresholds below apply per-cell. A cell that exceeds the fail
threshold blocks the IUS-8.b.2 migration from shipping (or triggers a
rollback if already merged).

| Metric | Target | Warn | Fail |
|--------|--------|------|------|
| Throughput (MB/s) | 0 % regression | < -2 % | < -5 % |
| Latency p50 | 0 % regression | > +2 % | > +5 % |
| Latency p99 | 0 % regression | > +5 % | > +10 % |
| Syscall count | 0 delta | any delta | any delta |
| Instructions/op | 0 delta | > +1 % | > +3 % |
| I-cache miss rate | 0 % regression | > +2 % | > +5 % |

The 0 % target reflects the IUS-7.b promise: the trait is supposed to
be zero-cost. The warn/fail bands account for measurement noise on
shared CI hardware.

**Headline acceptance criterion:** no cell exceeds the fail threshold
on any metric. Cells in the warn band require asm-diff investigation
before merge.

### 4.1 Noise floor calibration

Before the first production run, the bench harness runs 5 identity
comparisons (same binary, same commit, two consecutive runs). The
maximum observed delta across all cells establishes the noise floor. If
the noise floor exceeds 2 % on any metric, the warn threshold for that
metric is raised to `noise_floor + 1 %`. The fail threshold is always
`warn + 3 %`.

## 5. Assembly inspection

### 5.1 Hot-path asm-diff (from IUS-7.b section 3.2)

For each of the 12 hot-path methods identified in IUS-7.b section 5.6,
the bench produces an assembly diff between the pre-trait and post-trait
builds:

```sh
# Pre-trait: build at baseline ref, dump asm for the hot-path caller
git checkout $BASELINE_REF
cargo build --release -p fast_io
cargo asm --release -p fast_io --lib disk_commit::write_chunk > baseline.s

# Post-trait: build at HEAD, dump asm for the same caller
git checkout HEAD
cargo build --release -p fast_io
cargo asm --release -p fast_io --lib disk_commit::write_chunk > post_trait.s

# Normalize and diff
tools/ci/normalize_asm.sh baseline.s > baseline_norm.s
tools/ci/normalize_asm.sh post_trait.s > post_trait_norm.s
diff -u baseline_norm.s post_trait_norm.s > asm_diff.txt
```

The normalization script (`tools/ci/normalize_asm.sh`):

1. Strips function prologue/epilogue boilerplate.
2. Canonicalizes register names (e.g., `rax` and `rcx` allocation
   differences are not regressions).
3. Removes `.cfi_*` directives and `.Ltmp*` labels.
4. Sorts instruction blocks by address to tolerate basic-block
   reordering.

**Acceptance:** the normalized diff is empty for all 12 hot-path
methods. A non-empty diff means LLVM produced different code through
the trait path - investigate before declaring the migration zero-cost.

### 5.2 Vtable absence check

A targeted grep on the post-trait release binary confirms no vtable
dispatch exists in the hot-path callers:

```sh
objdump -d target/release/oc-rsync | \
  grep -A5 'disk_commit\|transfer_ops\|parallel_apply' | \
  grep 'callq\s*\*' > vtable_calls.txt
```

Expected: empty output. Any indirect `callq *%reg` in the hot-path
functions indicates a `dyn` dispatch that the migration introduced.

### 5.3 Monomorphization witness

`cargo llvm-lines -p fast_io --release` counts the number of LLVM IR
lines per generic instantiation. For
`<LinuxIoUringOpsBackend as IoUringBackend>::submit_one`, there must
be exactly one instantiation. Multiple instantiations indicate the
trait is being used with multiple concrete types (unexpected) or that
a `dyn` adapter is generating a second code path (acceptable for cold
callers, a bug for hot callers).

## 6. CI integration

### 6.1 Workflow file

A new workflow `.github/workflows/bench-iouring-trait-regression.yml`
runs the bench matrix. It triggers on:

- `pull_request` when paths touch `crates/fast_io/src/io_uring/backend*`
  or `crates/fast_io/src/io_uring/mod.rs` or any file that IUS-8.b.2
  migrated (callers in `engine`, `transfer`, `core`, `daemon`).
- `workflow_dispatch` for manual runs.
- `schedule` nightly at 02:17 UTC for ongoing regression detection.

### 6.2 Job structure

```yaml
jobs:
  micro-bench:
    name: io_uring trait micro-bench
    runs-on: ubuntu-latest
    timeout-minutes: 45
    steps:
      - checkout
      - install rust 1.88.0
      - install cargo-show-asm, hyperfine, perf-tools
      - build baseline (pre-trait ref)
      - run micro-bench cells (meta_10k, data_2g)
      - build post-trait (HEAD)
      - run micro-bench cells (same)
      - compare results
      - upload criterion artifacts

  asm-diff:
    name: io_uring trait asm-diff
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - checkout
      - install rust 1.88.0, cargo-show-asm
      - build baseline, dump asm for 12 hot-path methods
      - build post-trait, dump asm for same
      - normalize and diff
      - fail if non-empty normalized diff

  e2e-bench:
    name: io_uring trait end-to-end
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - checkout
      - install rust 1.88.0, hyperfine, rsync
      - build baseline and post-trait binaries
      - run e2e_daemon_10k and e2e_local_10k with hyperfine
      - compare wall-clock times
      - fail if any cell exceeds fail threshold

  vtable-check:
    name: io_uring vtable absence
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - checkout
      - build post-trait release binary
      - objdump hot-path functions for indirect calls
      - fail if any indirect callq found
```

### 6.3 Gate policy

| Job | Gate level | Applies to |
|-----|-----------|------------|
| `asm-diff` | required | PRs touching `fast_io::io_uring::backend*` |
| `vtable-check` | required | PRs touching `fast_io::io_uring::backend*` |
| `micro-bench` | required for IUS-8.b.2 merge; advisory for other PRs | all PRs touching `fast_io` |
| `e2e-bench` | advisory (warn-only) | nightly + manual |

The `asm-diff` and `vtable-check` jobs are fast (< 20 min) and
deterministic - suitable as required checks. The `micro-bench` job is
noisier on shared CI runners; it is required only for the IUS-8.b.2
merge itself and advisory afterward to avoid flake-gating unrelated PRs.

### 6.4 Ongoing regression detection

After IUS-8.b.2 merges, the nightly schedule keeps the bench warm. The
comparison baseline shifts from the pre-trait commit to the last known
good nightly run. The comparison script stores results in a rolling
`target/bench/ius-8b3/nightly/` directory. A regression that exceeds
the warn threshold on two consecutive nightly runs triggers a GitHub
issue via `gh issue create`.

## 7. Bench harness details

### 7.1 Micro-bench file

`crates/fast_io/benches/iouring_trait_regression.rs`. Uses criterion.
Structure:

```rust
fn bench_submit_one(c: &mut Criterion) {
    let backend = LinuxIoUringOpsBackend::with_eager_probe();
    let mut ring = backend.build_ring(&IoUringConfig::default()).unwrap();

    let mut group = c.benchmark_group("submit_one");
    group.throughput(Throughput::Elements(1_000_000));
    group.sample_size(100);

    // Through-trait path (the post-migration production path)
    group.bench_function("through_trait", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                let sqe = SubmissionEntry::Nop { user_data: 0 };
                black_box(backend.submit_one(&mut ring, sqe));
            }
        })
    });

    group.finish();
}

fn bench_drain_completions(c: &mut Criterion) { /* ... */ }
fn bench_submit_batch(c: &mut Criterion) { /* ... */ }
fn bench_probe_op(c: &mut Criterion) { /* ... */ }
fn bench_write_data(c: &mut Criterion) { /* ... */ }
```

Each bench group measures one hot-path method. The `NOP` opcode
isolates userspace dispatch cost from kernel I/O (per IUS-7.b
section 3.3). For `drain_completions`, the bench submits a batch of
NOPs, then times the drain loop.

### 7.2 End-to-end bench script

`tools/ci/ius_8b3_e2e_bench.sh`. Uses hyperfine with 10 warmup runs
and 30 measured runs. The script:

1. Generates the fixture (10K files, mixed sizes, deterministic seed).
2. Runs baseline binary: `hyperfine -w 10 -r 30 --export-json`.
3. Runs post-trait binary: same parameters.
4. Drops caches between every iteration.
5. Outputs JSON results to `target/bench/ius-8b3/e2e/`.

### 7.3 Syscall profiling

For each end-to-end cell, the script runs one iteration under
`perf stat -e syscalls:sys_enter_io_uring_enter` and records the count.
The comparison script asserts the count is identical between baseline
and post-trait. This catches batching regressions that wall-clock
measurements might miss (e.g., 2x the syscalls at the same throughput
because the kernel is faster than the regression).

### 7.4 Instruction profiling

For each micro-bench cell, one iteration runs under
`perf stat -e instructions,cycles,L1-icache-load-misses`. The raw
counts are saved alongside criterion results. The comparison script
computes per-op instruction count by dividing total instructions by
iteration count.

## 8. Rollback criteria

If any cell exceeds the fail threshold after IUS-8.b.2 has merged:

### 8.1 Immediate triage (within 24 hours)

1. Run the asm-diff to identify the codegen regression.
2. Check for accidental `dyn` dispatch or `Arc` clones in the migrated
   callers.
3. If the regression is localized to one caller, fix the caller (e.g.,
   add `#[inline(always)]` to a missing intermediate function, or
   replace `&dyn` with a generic bound).

### 8.2 Rollback (if triage does not resolve within 48 hours)

Revert the IUS-8.b.2 migration PR. The pre-trait free-function path
remains functional because IUS-8.b.1 was additive (trait coexists with
free functions). The rollback does not touch any other IUS-8 work.

### 8.3 Re-attempt protocol

After rollback, the failing cell must be root-caused and a fix must be
demonstrated on a branch before re-merging. The fix PR must include:

- The asm-diff showing the regression is resolved.
- The micro-bench showing the cell is within the warn threshold.
- An explanation of what caused the regression and why the fix prevents
  recurrence.

### 8.4 Threshold revision

If the noise floor calibration (section 4.1) shows that the fail
thresholds are too tight for the CI hardware, the thresholds may be
relaxed - but only by updating this spec with the calibration data and
getting review approval. The 2 % warn / 5 % fail targets are derived
from IUS-7.b section 1.2 and should not be relaxed without evidence
that the measurement infrastructure cannot support them.

## 9. Relationship to existing bench infrastructure

### 9.1 IUB-2 / IUB-3 cells

The data-heavy and metadata-heavy cells in this spec reuse the fixture
shapes from IUB-2 (multi-GB bench design) and IUB-3 (high-IOPS bench
design). The distinction is that IUB-2/3 compare io_uring vs stdlib;
this spec compares pre-trait io_uring vs post-trait io_uring. The bench
harness can share fixture generation scripts but must maintain separate
criterion groups so results are not conflated.

### 9.2 IUS-7.b backend_dispatch bench

IUS-7.b section 3.3 defined a `backend_dispatch.rs` criterion bench
that compares through-trait vs direct-call on the same binary. That
bench validates the trait impl itself (IUS-8.b.1). This spec validates
the caller migration (IUS-8.b.2) - a different question. The two
benches are complementary:

- `backend_dispatch`: "does the trait method compile to the same code
  as the direct call?" (codegen question)
- `iouring_trait_regression`: "did the caller migration change the
  end-to-end performance?" (integration question)

### 9.3 DIS-8.a daemon cold-start bench

The `e2e_daemon_10k` cell in this spec overlaps with the DIS-8.a
workflow. The difference: DIS-8.a compares oc-rsync vs upstream rsync;
this spec compares pre-trait oc-rsync vs post-trait oc-rsync. Both
measure wall-clock daemon pull time. A regression detected by this
spec but not by DIS-8.a would indicate the trait migration degraded
oc-rsync without changing its relative position vs upstream.

### 9.4 Existing fast_io benches

The 12 existing bench files in `crates/fast_io/benches/` cover
io_uring topology comparisons (per-file vs shared, SQPOLL, SEND_ZC,
IOCP, NVMe data path). None of them measure pre-trait vs post-trait
codegen. The new `iouring_trait_regression.rs` bench is additive and
does not modify any existing bench file.

## 10. Deliverables

| # | Deliverable | File | LoC estimate |
|---|-------------|------|-------------|
| 1 | Micro-bench harness | `crates/fast_io/benches/iouring_trait_regression.rs` | ~300 |
| 2 | End-to-end bench script | `tools/ci/ius_8b3_e2e_bench.sh` | ~150 |
| 3 | Comparison script | `tools/ci/ius_8b3_compare.py` | ~200 |
| 4 | Asm normalization script | `tools/ci/normalize_asm.sh` | ~60 |
| 5 | CI workflow | `.github/workflows/bench-iouring-trait-regression.yml` | ~120 |
| 6 | Cargo.toml bench entry | `crates/fast_io/Cargo.toml` | +5 |

**Total: ~835 LoC across 6 files.**

## 11. Open questions

### 11.1 Self-hosted vs GitHub-hosted runner

The micro-bench requires stable performance. GitHub-hosted `ubuntu-latest`
runners have variable co-tenancy that inflates the noise floor. Options:

1. **GitHub-hosted with noise-floor calibration.** Accept higher warn
   thresholds (per section 4.1). Simpler to maintain.
2. **Self-hosted `oc-rsync-bench` container.** Lower noise, but
   requires dedicated hardware. The existing benchmark.yml uses
   `ubuntu-latest`; the bench-daemon-coldstart.yml also uses
   `ubuntu-latest`.

Recommendation: start with GitHub-hosted and calibrate. If the noise
floor exceeds 3 % on throughput, migrate the micro-bench job to
self-hosted.

### 11.2 Thin LTO dependency

The workspace uses thin LTO in release mode. The zero-cost guarantee
assumes thin LTO is sufficient for cross-crate inlining of
`#[inline(always)]` methods. If a future workspace change disables
thin LTO, the guarantee may break.

Recommendation: add a CI assertion that `[profile.release] lto`
is `"thin"` or `"fat"` in the workspace `Cargo.toml`. The assertion
lives in the `asm-diff` job.

### 11.3 Baseline drift

After several months, the pre-trait baseline commit will be far from
HEAD. Code changes unrelated to the trait migration will accumulate,
making the before/after comparison noisy.

Recommendation: after the IUS-8.b.2 migration is validated, freeze the
baseline as a tagged artifact (`ius-8b3-baseline-v1`) containing the
pre-built release binary and criterion results. Subsequent nightly runs
compare against the frozen artifact, not a rebuilt baseline. The
artifact is rebuilt only when the Rust toolchain or the bench harness
changes.

### 11.4 aarch64 coverage

The bench is specified for x86_64 Linux (the primary CI target).
aarch64 Linux coverage is desirable but not blocking for IUS-8.b.2
merge. aarch64 asm-diff uses different register names and instruction
mnemonics; the normalization script must handle both ISAs.

Recommendation: add aarch64 as a follow-up job after the x86_64 bench
stabilizes. The micro-bench cells are portable; only the asm-diff and
vtable-check scripts need ISA-specific logic.

---

**Headline:** four verification layers - asm-diff (codegen identity),
micro-bench (2 % throughput / 5 % p99 gate), end-to-end transfer bench
(5 % wall-clock gate), and syscall profiling (zero-delta gate) - prove
the IUS-8.b.2 caller migration preserved the zero-cost guarantee from
IUS-7.b. Rollback is immediate if any cell exceeds the fail threshold.
