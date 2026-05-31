# STX-9: Re-benchmark --checksum Syscall Count After STX-6/STX-8 Fixes

Date: 2026-06-01
Series: STX (statx overhead in --checksum mode)
Status: pending - awaiting execution after STX-6/STX-8 merge

## 1. Purpose

Validate that STX-6 (pre-sized reads eliminating BufReader EOF probes) and
STX-8 (cached flist metadata eliminating redundant stat calls) together
bring oc-rsync's total syscall count in `--checksum` mode to within 1.1x
of upstream rsync 3.4.1. STX-1 measured a 3.34x statx gap (6,691 vs 2,006
syscalls on a 1000-file corpus). This re-benchmark confirms the fixes
close that gap.

## 2. Measurement Methodology

### 2.1 Tool

Use `strace -c -S calls` to produce a per-syscall summary sorted by call
count. The `-f` flag is required for oc-rsync to capture rayon worker
threads.

```sh
# Upstream rsync 3.4.1
strace -c -S calls -f rsync -rc --checksum src/ dst/ 2> upstream.strace

# oc-rsync (post STX-6/STX-8)
strace -c -S calls -f oc-rsync -rc --checksum src/ dst/ 2> oc-rsync.strace
```

### 2.2 Environment

- Linux x86_64, kernel 5.15+ (statx available).
- Single-threaded rsync vs potentially multithreaded oc-rsync - both traced
  with `-f` for consistency.
- Filesystem: ext4 or tmpfs (avoid network filesystems that add RPC calls).
- No other I/O-intensive processes during measurement.

### 2.3 Warm cache requirement

Run each command twice; discard the first run. The second run measures
steady-state behavior with filesystem metadata and data pages in cache.
This eliminates variance from cold-start readahead patterns.

## 3. Test Fixture

Reproduce the STX-1 corpus exactly:

```sh
mkdir -p /tmp/stx9/{src,dst}
for i in $(seq 1 1000); do
  dd if=/dev/urandom of=/tmp/stx9/src/file_$i bs=512 count=$((RANDOM % 20 + 1)) 2>/dev/null
done
# Pre-populate dst with identical content (forces full checksum comparison)
rsync -rc /tmp/stx9/src/ /tmp/stx9/dst/
```

Properties:
- 1000 files, sizes between 512 bytes and 10 KiB.
- Destination pre-populated with identical content - forces full checksum
  computation on both sides without triggering any transfers.
- No symlinks, hardlinks, or special files (isolates the checksum path).
- No subdirectories (avoids directory traversal variance).

## 4. Syscall Categories to Track

| Category | Syscalls | What it reveals |
| --- | --- | --- |
| Stat/statx | `stat`, `fstat`, `lstat`, `statx`, `newfstatat` | Metadata lookups per file |
| Open | `open`, `openat` | File descriptor acquisitions |
| Read | `read`, `pread64`, `readv` | Data I/O calls per file |
| Write | `write`, `pwrite64`, `writev` | Wire output and file writes |
| Close | `close` | File descriptor releases |
| Fstat | `fstat` (subset of stat) | Redundant post-open metadata |

Report each category separately plus the total.

## 5. Expected Improvements

### 5.1 STX-6: Pre-sized reads (fewer read syscalls)

Before: `BufReader` issued a trailing `read()` returning 0 on every file
to detect EOF. For 1000 files this added approximately 2000 extra reads
(1 per file per side - sender and receiver).

After: `read_exact()` with a `remaining` counter sized from the flist's
known file length. Loop terminates when `remaining == 0`. Zero trailing
reads.

Expected reduction: ~2000 fewer read syscalls.

### 5.2 STX-8: Cached flist metadata (fewer stat/statx syscalls)

Before: Each checksum computation called `file.metadata()` to obtain the
file size, issuing `statx(fd, AT_EMPTY_PATH)` even though the size was
already available from the flist entry built during enumeration. This
doubled the stat count on both sender and receiver sides (~4000 extra
stats for 1000 files).

After: File size is passed through the `FilePair` struct from cached flist
metadata. No `file.metadata()` call inside the checksum path.

Expected reduction: ~4000 fewer statx syscalls.

### 5.3 Combined expected outcome

| Category | STX-1 baseline (oc-rsync) | Expected post-fix | Upstream |
| --- | --- | --- | --- |
| statx/stat | ~6,691 | ~2,200 | ~2,006 |
| read | ~6,000+ | ~4,000 | ~4,000 |
| Total | ~16,000+ | ~9,500 | ~8,500 |

Target ratio: total(oc-rsync) / total(upstream) <= 1.1.

## 6. Pass/Fail Criteria

### 6.1 Primary gate (must pass)

Total syscall count (all categories summed) for oc-rsync must be <= 1.1x
the upstream rsync total on the same fixture.

### 6.2 Per-category breakdown (reported, not gating)

Each category is reported as a ratio. Categories exceeding 1.2x are
flagged for investigation but do not block the pass.

### 6.3 Reporting format

```
Category        upstream    oc-rsync    ratio
stat/statx      2,006       2,180       1.09x
read            4,012       4,100       1.02x
open/openat     2,004       2,050       1.02x
write           1,520       1,580       1.04x
close           2,004       2,050       1.02x
fstat           0           20          -
TOTAL           11,546      12,000      1.04x   PASS (<= 1.1x)
```

## 7. Decision Tree on Failure

If total ratio exceeds 1.1x:

```
ratio > 1.1x
├── stat/statx ratio > 1.2x
│   ├── fstat calls from openat+fstat pattern?
│   │   └── STX-10: eliminate fstat-after-open via O_PATH or AT_EMPTY_PATH
│   ├── directory traversal double-stat?
│   │   └── STX-11: cache dir stat in traversal iterator
│   └── post-transfer metadata comparison stat?
│       └── STX-12: defer metadata comparison to use already-open fd
│
├── read ratio > 1.2x
│   ├── buffer size smaller than upstream (64 KiB vs 256 KiB)?
│   │   └── STX-13: increase quick-check buffer to 256 KiB
│   ├── BufReader still present in an untouched path?
│   │   └── STX-14: audit all checksum entry points for EOF probes
│   └── rayon worker reads (thread startup overhead)?
│       └── Acceptable if count matches (1 extra read per thread spawn)
│
├── open/close ratio > 1.2x
│   ├── double-open (sender re-opens for checksum after enumeration)?
│   │   └── STX-15: keep fd open from enumeration through checksum
│   └── temp file creation in non-transfer path?
│       └── Investigate unexpected temp file opens
│
└── write ratio > 1.2x
    ├── per-file multiplex framing overhead?
    │   └── Related to MIF series - coalesce MSG_INFO frames
    └── logging writes?
        └── Suppress debug writes in benchmark mode
```

## 8. Execution Checklist

1. Confirm STX-6 and STX-8 are both merged to the branch under test.
2. Build oc-rsync in release mode (`cargo build --release`).
3. Create the fixture (section 3).
4. Run warm-up pass for both binaries.
5. Run strace measurement pass for both binaries.
6. Parse strace output into the reporting table (section 6.3).
7. Evaluate pass/fail (section 6.1).
8. If fail: follow decision tree (section 7), file next STX ticket.
9. If pass: update `project_statx_overhead_checksum_mode.md` status to
   resolved, referencing this document.

## 9. Notes

- strace's ptrace mechanism serializes multithreaded syscalls, inflating
  per-call latency. The syscall count is accurate but wall-clock time under
  strace is not representative of native performance.
- If oc-rsync uses rayon for parallel checksum computation, thread creation
  syscalls (clone3, mmap for stack) appear in the trace. These are excluded
  from the per-category comparison since upstream rsync is single-threaded.
  Only I/O-related syscalls are compared.
- The 1.1x threshold allows for structural differences (Rust runtime init,
  allocator syscalls, thread pool setup) that are unavoidable but amortize
  at scale.
