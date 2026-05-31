# Checksum Mode Re-benchmark After CSM-8 Fix (CSM-9.a)

Re-benchmark `--checksum` mode performance after the CSM-8 fix (PR #5128)
that corrected the checksum negotiation path - oc-rsync was selecting MD5
instead of XXH3/XXH128 for the strong checksum when both sides support
checksum negotiation. The original profiling (CSM-2.a flamegraphs,
CSM-3.a/b syscall+CPU comparison) showed a 1.5-1.7x wall-clock regression
against upstream rsync 3.4.1 in `--checksum` mode. Target: <= 1.05x
upstream after the fix.

## 1. Bench Fixture

### 1.1 File Corpus

Four fixture sizes exercise different cost profiles - small files stress
per-file overhead (stat, open, close syscalls), large files stress
sustained checksum throughput:

| Fixture | File size | File count | Total data | Purpose |
|---------|-----------|------------|------------|---------|
| tiny | 1 KB | 10,000 | ~10 MB | Per-file overhead dominance |
| medium | 1 MB | 1,000 | ~1 GB | Balanced CPU + I/O |
| large | 100 MB | 10 | ~1 GB | Sustained checksum throughput |
| huge | 1 GB | 1 | 1 GB | Single-file peak throughput |

### 1.2 Directory Structure

```
bench-checksum/
  tiny/        # 10,000 x 1KB files: file_0000..file_9999
  medium/      # 1,000 x 1MB files: file_000..file_999
  large/       # 10 x 100MB files: file_00..file_09
  huge/        # 1 x 1GB file: file_00
```

Files are generated with deterministic pseudo-random content (seeded PRNG)
so that repeated runs produce byte-identical fixtures. This prevents
quick-check from short-circuiting - source and destination have identical
mtime and size, forcing `--checksum` to actually compute and compare
checksums (the mode under test).

### 1.3 Fixture Generation

```bash
#!/bin/bash
# generate_checksum_bench_fixtures.sh
set -euo pipefail

BASE="$1"  # e.g., /tmp/bench-checksum

for dir in tiny medium large huge; do
  mkdir -p "$BASE/src/$dir" "$BASE/dst/$dir"
done

# Deterministic content via openssl with fixed seed
gen_file() {
  local path="$1" size="$2" seed="$3"
  openssl enc -aes-256-ctr -nosalt -pass "pass:$seed" \
    </dev/zero 2>/dev/null | head -c "$size" > "$path"
}

# tiny: 10,000 x 1KB
for i in $(seq 0 9999); do
  gen_file "$BASE/src/tiny/file_$(printf '%04d' $i)" 1024 "tiny-$i"
done

# medium: 1,000 x 1MB
for i in $(seq 0 999); do
  gen_file "$BASE/src/medium/file_$(printf '%03d' $i)" 1048576 "medium-$i"
done

# large: 10 x 100MB
for i in $(seq 0 9); do
  gen_file "$BASE/src/large/file_$(printf '%02d' $i)" 104857600 "large-$i"
done

# huge: 1 x 1GB
gen_file "$BASE/src/huge/file_00" 1073741824 "huge-0"

# Destination is a full copy (same content, same mtime via cp -a)
cp -a "$BASE/src/"* "$BASE/dst/"
```

The key invariant: `src` and `dst` are identical. With `--checksum` both
sides compute the full-file checksum and compare. No delta transfer occurs
- this isolates pure checksum computation cost.

## 2. Measurement Methodology

### 2.1 Metrics

| Metric | Tool | Unit |
|--------|------|------|
| Wall-clock time | `hyperfine --warmup 3 --runs 10` | seconds |
| CPU time (user + sys) | `/usr/bin/time -v` (Linux) | seconds |
| Syscall count | `strace -c -S calls` | count by syscall |
| Throughput | (total bytes) / (wall-clock) | MB/s |
| Peak RSS | `/usr/bin/time -v` maxrss | KB |
| Cache misses | `perf stat -e cache-misses,cache-references` | count + ratio |

### 2.2 Commands Under Test

For each fixture `$FIX` in {tiny, medium, large, huge}:

```bash
# oc-rsync (post-CSM-8)
oc-rsync --checksum -r "$BASE/src/$FIX/" "$BASE/dst/$FIX/"

# upstream rsync 3.4.1
rsync --checksum -r "$BASE/src/$FIX/" "$BASE/dst/$FIX/"
```

Both commands run with identical arguments. No `-v` (avoids per-file
output I/O). No `--progress`. Local-to-local transfer only - this
isolates checksum computation from network or SSH overhead.

### 2.3 Hyperfine Invocation

```bash
hyperfine \
  --warmup 3 \
  --runs 10 \
  --export-json "results-$FIX.json" \
  --prepare 'sync && echo 3 | sudo tee /proc/sys/vm/drop_caches > /dev/null' \
  "oc-rsync --checksum -r $BASE/src/$FIX/ $BASE/dst/$FIX/" \
  "rsync --checksum -r $BASE/src/$FIX/ $BASE/dst/$FIX/"
```

The `--prepare` step drops page cache between runs so that file reads hit
disk consistently. On macOS use `sudo purge` instead.

### 2.4 Strace Capture

```bash
strace -c -S calls -o "strace-oc-$FIX.txt" \
  oc-rsync --checksum -r "$BASE/src/$FIX/" "$BASE/dst/$FIX/"

strace -c -S calls -o "strace-up-$FIX.txt" \
  rsync --checksum -r "$BASE/src/$FIX/" "$BASE/dst/$FIX/"
```

Compare total syscall counts. Key syscalls to watch: `read`, `stat`/
`statx`, `open`/`openat`, `close`, `mmap`. A disproportionate `read`
count in oc-rsync relative to upstream indicates buffering strategy
differences (CSM-3.a documented this for the pre-fix state).

## 3. Checksum Algorithm Breakdown

### 3.1 Purpose

CSM-8 fixed the negotiation so that both sides use XXH3/XXH128 when
capability strings include checksum seed negotiation. This section
validates which algorithm is actually running and measures per-algorithm
cost in isolation.

### 3.2 Algorithm Microbenchmark

Use `criterion` benchmarks in `crates/checksums/benches/` to measure
per-byte cost of each algorithm on the bench host:

| Algorithm | Role | Expected throughput (modern x86) |
|-----------|------|----------------------------------|
| XXH3-128 | Strong checksum (post-CSM-8) | ~30 GB/s |
| MD5 | Strong checksum (pre-CSM-8, fallback) | ~600 MB/s |
| MD4 | Legacy strong checksum (protocol <= 29) | ~800 MB/s |

The XXH3-to-MD5 throughput ratio (~50x) means that even a small fraction
of time spent in the strong checksum path should show a dramatic
improvement post-fix.

### 3.3 Validation That XXH3 Is Active

Run with `RUST_LOG=debug` (or equivalent trace output) and verify log
lines confirm:

- Checksum negotiation succeeded (both sides advertise XXH3 support).
- Selected strong checksum is XXH3-128, not MD5.
- No fallback to MD5 occurred.

If the negotiation log is unavailable in release builds, verify indirectly
by comparing the per-file CPU time against the microbenchmark baseline.
A file checksummed at ~25+ GB/s effective rate confirms XXH3; ~500 MB/s
confirms MD5 is still in use (indicating the fix did not take effect).

### 3.4 Per-File CPU Attribution

For the `large` fixture (10 x 100MB), use `perf record` + `perf report`
to attribute CPU time:

```bash
perf record -g -- oc-rsync --checksum -r "$BASE/src/large/" "$BASE/dst/large/"
perf report --stdio --sort=symbol | head -40
```

Expected post-fix: the strong checksum function (XXH3) should consume
< 5% of total CPU. Pre-fix: MD5 consumed 40-60% of CPU in
CSM-2.a flamegraphs.

## 4. Comparison Matrix

Results table to fill after benchmarking:

| Fixture | oc-rsync (s) | upstream (s) | Ratio | Throughput oc (MB/s) | Throughput up (MB/s) | Pass? |
|---------|-------------|-------------|-------|---------------------|---------------------|-------|
| tiny | | | | | | |
| medium | | | | | | |
| large | | | | | | |
| huge | | | | | | |

Additional columns captured but reported separately: syscall count delta,
peak RSS delta, CPU user+sys breakdown.

## 5. Validation Criteria

### 5.1 Pass/Fail

| Criterion | Threshold | Action on fail |
|-----------|-----------|----------------|
| Wall-clock ratio (all fixtures) | <= 1.05x upstream | Identify remaining hot path |
| Syscall count ratio | <= 1.10x upstream | Investigate extra syscalls |
| Peak RSS ratio | <= 1.50x upstream | Flag for RSS work (separate) |
| XXH3 CPU fraction | < 10% total CPU | Confirm fix is active |

### 5.2 Triage If Ratio > 1.05x

If any fixture exceeds the 1.05x threshold after the fix:

1. Re-run `perf record` and generate flamegraph for the failing fixture.
2. Compare against CSM-2.a pre-fix flamegraph.
3. Identify the top contributor(s) to the remaining gap.
4. Likely candidates (from CSM-3.a/b analysis):
   - Per-file stat overhead (extra `statx` calls from Rust std).
   - BufReader EOF probe adding spurious `read` syscalls.
   - File open/close overhead differences (upstream reuses fd more aggressively).
   - Metadata serialization cost differences.
5. File follow-up issue for each remaining contributor with its
   percentage of the gap.

### 5.3 Regression Gate

Once the benchmark passes (<= 1.05x), the results become the baseline
for future `--checksum` mode regressions. The `ci/nightly-checksum-bench`
workflow should consume the fixture definition from this document and
alert on regressions exceeding 5% from the established baseline.

## 6. Reproducibility

### 6.1 Environment

| Requirement | Specification |
|-------------|---------------|
| OS | Linux (Ubuntu 22.04 or Arch) |
| Kernel | >= 5.15 (consistent `statx` behavior) |
| CPU | Fixed frequency (disable turbo boost) |
| CPU pinning | `taskset -c 0-1` for both commands |
| Filesystem | ext4, mounted with `noatime` |
| Memory | >= 16 GB (fixture fits in RAM after first read) |
| Other load | System idle, no background compilation |

### 6.2 CPU Frequency Locking

```bash
# Disable turbo boost (Intel)
echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo

# Fix frequency to base clock
sudo cpupower frequency-set -g performance
sudo cpupower frequency-set -d 3.0GHz -u 3.0GHz
```

On AMD:
```bash
echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost
```

### 6.3 Cache Dropping

Between every run (handled by hyperfine `--prepare`):

```bash
sync
echo 3 | sudo tee /proc/sys/vm/drop_caches > /dev/null
```

This ensures each run reads from disk, not page cache. For the `huge`
fixture (1 GB), page cache effects dominate without this step.

### 6.4 Iteration Count and Statistical Validity

- Warmup: 3 runs (discarded).
- Measured: 10 runs.
- Report: median, mean, stddev, min, max.
- Outlier rejection: none (report all; investigate if stddev > 10% of
  median).

### 6.5 Binary Versions

| Binary | Version | Build |
|--------|---------|-------|
| oc-rsync | Post-CSM-8 (commit hash recorded) | `--release` profile, LTO=thin |
| rsync | 3.4.1 (from upstream source tarball) | Default `./configure && make` |

Both must be built on the same host. Record `rustc --version`, `gcc
--version`, and `uname -r` in the results.

## 7. Execution Checklist

- [ ] Build oc-rsync at post-CSM-8 commit with release profile.
- [ ] Build upstream rsync 3.4.1 from source.
- [ ] Generate fixtures with `generate_checksum_bench_fixtures.sh`.
- [ ] Verify fixtures: `diff -rq src/ dst/` shows no differences.
- [ ] Lock CPU frequency, disable turbo boost.
- [ ] Run hyperfine for each fixture with cache dropping.
- [ ] Run strace captures for each fixture.
- [ ] Run perf record for the `large` fixture (both binaries).
- [ ] Run checksums microbenchmark (`cargo bench -p checksums`).
- [ ] Verify XXH3 is the active algorithm (debug log or perf attribution).
- [ ] Fill comparison matrix.
- [ ] Evaluate pass/fail criteria.
- [ ] If pass: record baseline, close CSM-9.a.
- [ ] If fail: generate flamegraph, file follow-up issues.

## 8. Expected Outcome

Given that XXH3-128 is ~50x faster than MD5 per byte, and the CSM-2.a
flamegraph showed MD5 consuming 40-60% of CPU time in `--checksum` mode,
the fix should eliminate the dominant contributor to the 1.5-1.7x gap.
Residual gap (if any) is expected to come from per-file overhead
differences rather than checksum computation.

Conservative prediction: 1.00-1.03x upstream for the `large` and `huge`
fixtures (checksum-dominated), 1.02-1.05x for `tiny` (per-file overhead
dominated).
