# CSM Benchmark Results - Checksum Mode Performance

## Summary

oc-rsync's `--checksum` mode uses MD5 for local transfers, while upstream rsync
3.4.1 negotiates XXH128 (xxhash). This algorithm mismatch is the root cause of
the observed 1.5-2.9x performance gap (issue #970). When both tools use the same
algorithm (MD5), oc-rsync is actually 2-2.7x faster due to rayon parallelism.

**Status**: Issue #970 remains open. The CSM-8 fix (OpenSSL acceleration for MD5)
improved absolute throughput but did not close the gap because upstream uses a
fundamentally faster algorithm.

**Fix required**: Switch oc-rsync's local `--checksum` default from MD5 to XXH128
(or XXH3), matching upstream's checksum negotiation for protocol 32.

## Environment

- Container: `rsync-profile` (Debian, aarch64 via podman on macOS host)
- Upstream: rsync 3.4.1, protocol 32, negotiated checksum: **xxh128**
- oc-rsync: v0.6.2, protocol 32, local checksum: **MD5** (OpenSSL EVP)
- OpenSSL: 3.5.6 (linked by both binaries)
- Dataset: 1670 files, 111 MB mixed sizes (10x5MB, 100x256KB, 500x50KB,
  1000x1KB, 60x100KB in subdirs)
- Large file: single 500 MB random file
- All benchmarks: no-change re-sync (files already identical)

## CSM-3.a - Upstream rsync Syscall + CPU Profile

### Syscall Profile (strace -f -c, no-change 111 MB dataset)

| Metric            | Upstream rsync | oc-rsync   | Ratio |
|-------------------|----------------|------------|-------|
| Total syscalls    | 14,208         | 25,252     | 1.78x |
| read calls        | 3,769          | 7,687      | 2.04x |
| stat calls        | 3,369          | 8,375      | 2.49x |
| openat calls      | 3,363          | 3,369      | 1.00x |
| close calls       | 3,375          | 3,369      | 1.00x |
| futex calls       | 37             | 233        | 6.30x |
| sched_yield calls | 0              | 1,885      | -     |

### Key Observations

- Upstream forks 2 processes (sender + receiver); oc-rsync spawns 5 threads.
- Both open each file twice (once per side for checksum).
- oc-rsync has 2x more reads due to smaller buffer (128 KB vs 256 KB).
- oc-rsync has 2.5x more stat calls from redundant metadata checks.
- Thread synchronization (futex + sched_yield) adds overhead.

### CPU Time (perf stat, no-change 111 MB dataset)

| Metric         | Upstream rsync | oc-rsync  | Ratio  |
|----------------|----------------|-----------|--------|
| Wall time      | 49 ms          | 125 ms    | 2.55x  |
| User CPU       | 14 ms          | 342 ms    | 24.4x  |
| System CPU     | 21 ms          | 37 ms     | 1.76x  |
| CPUs utilized  | 0.71           | 3.04      | 4.28x  |

## CSM-3.b - Algorithm Mismatch Analysis

The critical finding: upstream rsync 3.4.1 negotiates **XXH128** for `--checksum`
mode (confirmed via `--debug=ALL`: "Client negotiated checksum: xxh128"). oc-rsync
defaults to **MD5** (`SignatureAlgorithm::Md5` in
`crates/engine/src/local_copy/options/builder/definition.rs:229`).

XXH128 throughput is 10-30x higher than MD5 on the same hardware. This dwarfs
any syscall-level or threading differences.

### Fair Comparison: Forcing MD5 on Both (500 MB single file)

| Tool                             | Wall time | User CPU  | Ratio vs upstream XXH128 |
|----------------------------------|-----------|-----------|--------------------------|
| Upstream rsync (XXH128, default) | 189 ms    | 34 ms     | 1.00x                    |
| oc-rsync (MD5, current default)  | 862 ms    | 1,549 ms  | 4.57x slower             |
| Upstream rsync --checksum-choice=md5 | 1,766 ms | 1,537 ms | 9.37x slower          |

**When both use MD5**, oc-rsync is 2.0x faster than upstream (862 ms vs 1,766 ms)
thanks to rayon parallel checksumming.

### Mixed Files (111 MB, 1670 files)

| Tool                             | Wall time | User CPU  | Ratio vs upstream XXH128 |
|----------------------------------|-----------|-----------|--------------------------|
| Upstream rsync (XXH128, default) | 83 ms     | 13 ms     | 1.00x                    |
| oc-rsync (MD5, current default)  | 157 ms    | 342 ms    | 1.90x slower             |
| Upstream rsync --checksum-choice=md5 | 429 ms | 334 ms   | 5.20x slower             |

## CSM-9.a - Post-CSM-8 Benchmark

### hyperfine Results (no-change re-sync, 10 runs)

**111 MB mixed files (1670 files)**:

```
Benchmark 1: rsync -a --checksum /tmp/csm-bench-src/ /tmp/csm-bench-dst/
  Time (mean +/- s):  63.5 ms +/- 9.9 ms    [User: 13.3 ms, System: 22.2 ms]

Benchmark 2: oc-rsync -a --checksum /tmp/csm-bench-src/ /tmp/csm-bench-dst/
  Time (mean +/- s):  131.4 ms +/- 3.3 ms    [User: 345.6 ms, System: 39.2 ms]

Summary: rsync ran 2.07 +/- 0.33 times faster
```

**500 MB single file**:

```
Benchmark 1: rsync -a --checksum /tmp/csm-large-src/ /tmp/csm-large-dst/
  Time (mean +/- s):  306.6 ms +/- 104.7 ms  [User: 34.8 ms, System: 218.2 ms]

Benchmark 2: oc-rsync -a --checksum /tmp/csm-large-src/ /tmp/csm-large-dst/
  Time (mean +/- s):  894.8 ms +/- 35.4 ms   [User: 1566.4 ms, System: 148.6 ms]

Summary: rsync ran 2.92 +/- 1.00 times faster
```

**Initial copy (no existing destination, 111 MB)**:

```
Summary: oc-rsync ran 1.16 +/- 0.08 times faster than upstream rsync
```

### Performance Summary

| Scenario             | Ratio (oc-rsync / upstream) | Target | Status   |
|----------------------|-----------------------------|--------|----------|
| No-change, mixed     | 2.07x                       | 1.05x  | EXCEEDED |
| No-change, large     | 2.92x                       | 1.05x  | EXCEEDED |
| Initial copy, mixed  | 0.86x (faster)              | 1.05x  | MET      |

## CSM-9.b - Issue #970 Status

Issue #970 **cannot be closed** at the current 2.07-2.92x ratio. The 1.05x
target requires switching to XXH128/XXH3 for local `--checksum` mode.

## Root Cause and Fix

1. **Root cause**: oc-rsync uses MD5 for local `--checksum`; upstream uses XXH128.
2. **CSM-8 effect**: OpenSSL acceleration helped MD5 throughput but cannot bridge
   the algorithmic gap (MD5 is fundamentally ~10x slower than XXH128).
3. **Required fix**: Change `checksum_algorithm` default in
   `crates/engine/src/local_copy/options/builder/definition.rs` from
   `SignatureAlgorithm::Md5` to `SignatureAlgorithm::Xxh3_128` (or equivalent
   XXH128). This must match upstream's negotiation for protocol 32.
4. **Parallel checksumming** (CSM-8's rayon prefetch) is already valuable and
   makes oc-rsync 2x faster than upstream when both use MD5.
