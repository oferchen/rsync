# Checksum Wall-Clock Rebench Design (STX-10)

Date: 2026-06-01
Series: STX (statx/syscall overhead in checksum mode)
Depends: CSM-8, STX-6, STX-8, STX-9
Status: planned

## 1. Purpose

Measure the combined wall-clock improvement from CSM-8 (XXH3 algorithm
fix), STX-6 (pre-sized reads), and STX-8 (cached metadata) against
upstream rsync 3.4.1 in `--checksum` mode. This is the final validation
that the full fix chain brings oc-rsync within the 1.05x target.

## 2. Relationship to Adjacent Work

| Item | Scope | Status |
|------|-------|--------|
| CSM-8 (PR #5128) | Algorithm mismatch - MD5 to XXH3/128 | merged |
| STX-6 | Pre-sized reads replacing BufReader EOF probes | merged |
| STX-8 | Cached flist metadata eliminating redundant stat | merged |
| STX-9 | Syscall count measurement (strace -c) | merged |
| CSM-9.a | Algorithm-only rebench (XXH3 vs MD5) | merged |
| **STX-10** | **Combined wall-clock time (this document)** | planned |

STX-9 validates the syscall reduction quantitatively (expected: stat
ratio drops from 3.34x toward 2.0x, read ratio from 2.04x toward 1.0x).
CSM-9.a measured the algorithm contribution in isolation. STX-10
measures the user-visible outcome - end-to-end wall-clock time - with
all three fixes active together.

## 3. Test Fixtures

Reuse the CSM-9.a fixture corpus to enable direct comparison with prior
results. All files are pre-generated random data (incompressible).

| Fixture | File count | File size | Total size | Exercises |
|---------|-----------|-----------|------------|-----------|
| small-many | 10,000 | 1 KB | 10 MB | Per-file overhead dominance |
| medium-many | 1,000 | 1 MB | 1 GB | Balanced CPU + I/O |
| large-few | 10 | 100 MB | 1 GB | Streaming throughput |
| single-huge | 1 | 1 GB | 1 GB | Pure hash bandwidth |

### 3.1 Fixture generation

```sh
mkdir -p /tmp/stx10-bench/{small,medium,large,huge}/{src,dst}

# small-many: 10K x 1KB
for i in $(seq 1 10000); do
  dd if=/dev/urandom of=/tmp/stx10-bench/small/src/f$i bs=1024 count=1 2>/dev/null
done

# medium-many: 1K x 1MB
for i in $(seq 1 1000); do
  dd if=/dev/urandom of=/tmp/stx10-bench/medium/src/f$i bs=1M count=1 2>/dev/null
done

# large-few: 10 x 100MB
for i in $(seq 1 10); do
  dd if=/dev/urandom of=/tmp/stx10-bench/large/src/f$i bs=1M count=100 2>/dev/null
done

# single-huge: 1 x 1GB
dd if=/dev/urandom of=/tmp/stx10-bench/huge/src/f1 bs=1M count=1024 2>/dev/null

# Pre-sync destinations (no-change scenario)
for d in small medium large huge; do
  rsync -a /tmp/stx10-bench/$d/src/ /tmp/stx10-bench/$d/dst/
done
```

### 3.2 Scenario

No-change re-sync (all files identical at source and destination). This
isolates checksum computation cost - no data transfer occurs, no temp
files are written, no delta encoding runs.

## 4. Measurement Method

### 4.1 Wall-clock via hyperfine

```sh
hyperfine --warmup 3 --runs 10 --export-json /tmp/stx10-results.json \
  'rsync -a --checksum /tmp/stx10-bench/{fixture}/src/ /tmp/stx10-bench/{fixture}/dst/' \
  'oc-rsync -a --checksum /tmp/stx10-bench/{fixture}/src/ /tmp/stx10-bench/{fixture}/dst/'
```

Run for each fixture (small, medium, large, huge). Report mean, stddev,
and min/max.

### 4.2 Phase breakdown

To attribute time to specific phases, use `--debug=TIME` (oc-rsync
timing output) combined with `perf stat` for CPU attribution:

| Phase | What it measures | How to isolate |
|-------|-----------------|----------------|
| Flist build | File enumeration + stat | `--dry-run --list-only` timing |
| Checksum compute | Hashing file content | Total minus flist minus comparison |
| Comparison | Comparing digests, deciding skip/transfer | Negligible at no-change |

For flist-build isolation:

```sh
hyperfine --warmup 3 --runs 10 \
  'rsync -a --checksum --dry-run --list-only /tmp/stx10-bench/{fixture}/src/' \
  'oc-rsync -a --checksum --dry-run --list-only /tmp/stx10-bench/{fixture}/src/'
```

Checksum compute time = total no-change time minus flist build time.
Comparison phase is O(n) integer comparison of digests and contributes
sub-millisecond overhead for all fixtures.

### 4.3 CPU utilization

```sh
perf stat -e task-clock,cycles,instructions,cache-misses \
  oc-rsync -a --checksum /tmp/stx10-bench/{fixture}/src/ /tmp/stx10-bench/{fixture}/dst/
```

This reveals whether oc-rsync's multicore utilization (rayon) compensates
for any remaining per-thread overhead vs upstream's fork model.

## 5. Isolation of Individual Contributions

### 5.1 STX-6 vs STX-8 separation

Both fixes are independently togglable via compile-time feature flags:

| Configuration | STX-6 (pre-sized reads) | STX-8 (cached metadata) | Build command |
|---------------|------------------------|------------------------|---------------|
| Baseline (pre-fix) | off | off | `cargo build --no-default-features -F legacy-checksum-io` |
| STX-6 only | on | off | `cargo build --no-default-features -F presized-reads` |
| STX-8 only | off | on | `cargo build --no-default-features -F cached-flist-meta` |
| Both (default) | on | on | `cargo build` (default features) |

Run the full hyperfine suite for each configuration to measure:

- STX-6 contribution = (baseline time) - (STX-6-only time)
- STX-8 contribution = (baseline time) - (STX-8-only time)
- Interaction effect = (baseline time) - (both time) - STX-6 - STX-8

If interaction is negative (fixes are sub-additive), document why.
Expected: minimal interaction since STX-6 targets read syscalls and
STX-8 targets stat syscalls - orthogonal paths.

### 5.2 CSM-8 contribution (algorithm)

Already measured in CSM-9.a. For completeness, include a single run
reverting to MD5 via `--checksum-choice=md5` to confirm the algorithm
dominates the improvement:

```sh
hyperfine --warmup 3 --runs 5 \
  'oc-rsync -a --checksum --checksum-choice=md5 /tmp/stx10-bench/large/src/ /tmp/stx10-bench/large/dst/' \
  'oc-rsync -a --checksum /tmp/stx10-bench/large/src/ /tmp/stx10-bench/large/dst/'
```

## 6. Comparison Matrix

### 6.1 Expected results table

| Fixture | Upstream 3.4.1 | oc-rsync (all fixes) | Ratio | Target |
|---------|---------------|---------------------|-------|--------|
| small-many (10K x 1KB) | TBD | TBD | TBD | <= 1.05x |
| medium-many (1K x 1MB) | TBD | TBD | TBD | <= 1.05x |
| large-few (10 x 100MB) | TBD | TBD | TBD | <= 1.05x |
| single-huge (1 x 1GB) | TBD | TBD | TBD | <= 1.05x |

### 6.2 Baseline (pre-STX-10, from CSM-9.a)

Prior measurements showed:

- Mixed files (111 MB, 1670 files): 2.07x slower (post-CSM-8, pre-STX-6/8)
- Large single file (500 MB): 2.92x slower (post-CSM-8, pre-STX-6/8)

The 2-3x residual gap after CSM-8 is attributed to:

- 2.04x more read syscalls (BufReader EOF probes) - fixed by STX-6
- 3.34x more stat syscalls (redundant metadata) - fixed by STX-8
- Smaller read buffer (64 KB vs 256 KB) - partial contribution

## 7. Pass Criteria

The combined CSM-8 + STX-6 + STX-8 fix chain passes if:

1. **All four fixtures** show oc-rsync within 1.05x of upstream rsync
   3.4.1 wall-clock time (mean over 10 runs).
2. **No fixture** exceeds 1.10x in any individual run (guards against
   high-variance outliers masking a systemic issue).
3. **small-many** fixture (per-file overhead dominant) is within 1.05x.
   This is the hardest target because per-file constant factors
   (open/close/stat) compound over 10K files.

### 7.1 Partial pass

If large-few and single-huge pass (compute-dominated) but small-many
fails, the residual is per-file overhead not addressed by STX-6/8.
Document as partial pass and open follow-up work.

## 8. Failure Analysis

If any fixture exceeds the 1.05x target after all fixes:

### 8.1 Residual analysis procedure

1. **Identify the dominant phase.** Use section 4.2 phase breakdown to
   determine if the gap is in flist build, checksum compute, or
   comparison.

2. **Syscall attribution.** Run STX-9's strace comparison on the failing
   fixture. Identify which syscall class (read, stat, open, futex) has
   the largest absolute time contribution.

3. **CPU profile.** Use `perf record -g` + `perf report` to find the
   hottest call stacks in oc-rsync that have no upstream equivalent.

4. **Thread overhead.** If futex/sched_yield dominate, rayon's thread
   pool may be over-subscribing for the fixture's file count. Measure
   with `RAYON_NUM_THREADS=1` to isolate threading overhead.

### 8.2 Known residual candidates

If the 1.05x target is not met, the next optimization targets in
priority order:

| Priority | Candidate | Expected gain | Effort |
|----------|-----------|---------------|--------|
| P1 | Increase read buffer from 64 KB to 256 KB (match upstream) | ~50% read syscall reduction | low |
| P2 | Eliminate directory-traversal double-stat | ~500 fewer stats per 10K files | medium |
| P3 | Pool file descriptors across checksum + transfer | 1 fewer open/close per file | medium |
| P4 | Reduce rayon thread pool for small file sets | lower futex/yield overhead | low |
| P5 | Batch stat calls via io_uring statx (Linux 5.6+) | amortized kernel transitions | high |

### 8.3 Decision tree

```
ratio <= 1.05x for all fixtures?
  YES -> STX-10 passes. Close CSM/STX series. Update issue #970.
  NO  -> Which fixture fails?
    small-many only -> Per-file overhead. Pursue P1, P2, P4.
    large/huge only -> Compute or I/O bandwidth. Pursue P1, check mmap.
    all fixtures    -> Systemic overhead. Profile, pursue P1 first.
```

## 9. Environment Requirements

- **Platform:** Linux x86_64 (bare metal or dedicated VM, not container
  under strace). macOS acceptable for relative comparisons but Linux is
  authoritative due to strace + perf availability.
- **Upstream rsync:** 3.4.1 built from source with default options.
- **oc-rsync:** Built with `--release` profile, default features.
- **hyperfine:** >= 1.18.0.
- **Isolation:** No background I/O. Drop caches between fixture changes
  (`echo 3 > /proc/sys/vm/drop_caches`). Warm filesystem caches before
  measurement (hyperfine `--warmup 3` handles this).
- **CPU governor:** `performance` (not `powersave`/`schedutil`).

## 10. Execution Sequence

1. Generate fixtures (section 3.1).
2. Run full hyperfine suite with default oc-rsync build (section 4.1).
3. Run phase breakdown (section 4.2).
4. If pass criteria met (section 7) - done, record results.
5. If not met - run isolation builds (section 5.1) to identify which
   fix contributed less than expected.
6. Run residual analysis (section 8.1).
7. Open follow-up issues for identified gaps.

## 11. Output Artifacts

- `docs/design/checksum-wallclock-rebench.md` - this design (committed).
- Benchmark results appended to `docs/design/csm-bench-results.md` as a
  new section (STX-10 results).
- Issue #970 updated with final ratio and closed if pass criteria met.
