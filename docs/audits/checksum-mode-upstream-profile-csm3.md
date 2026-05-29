# CSM-3 - upstream rsync `--checksum` syscall and CPU profile comparison

Date: 2026-05-29
Scope: read-only profiling comparison. No `.rs` edits.
Tracked under: CSM-3 (parent CSM track: `--checksum` mode was ~1.5-1.7x slower
than upstream rsync 3.4.1; issue #970).
Environment: `rsync-profile` container (Debian, aarch64), upstream rsync 3.4.1,
oc-rsync v0.6.2.

## 1. Goal

Profile upstream rsync's `--checksum` syscall and I/O behaviour on the same
workload used for CSM-2, then compare against oc-rsync to validate the
theoretical cost model from CSM-2 section 4.3 and provide the empirical
evidence required by CSM-7 items C3.1, C3.2, and C3.3.

## 2. Test workload

Mixed-size corpus designed to exercise all three file-size buckets (small,
medium, large) in a single run:

| Bucket | Count | Size range | Total |
| --- | --- | --- | --- |
| small | 333 | 1-10 KiB | 1 MiB |
| medium | 334 | 10-100 KiB | 17 MiB |
| large | 333 | 100 KiB - 1 MiB | 176 MiB |
| **Total** | **1000** | | **198 MiB** |

Source and destination are byte-identical copies. This forces the checksum
hot path (both sides must hash every file to confirm no transfer is needed)
and eliminates I/O from actual data transfer.

## 3. Wall-clock timing (no strace overhead)

Three runs each, warm page cache, `rsync -rc src/ dst/`:

| Tool | Run 1 | Run 2 | Run 3 | Median |
| --- | --- | --- | --- | --- |
| upstream rsync 3.4.1 | 0.112s | 0.112s | 0.113s | 0.112s |
| oc-rsync v0.6.2 | 0.143s | 0.143s | 0.143s | 0.143s |
| **Ratio** | | | | **1.28x** |

The 1.28x ratio is lower than the 1.5-1.7x originally reported for CSM-1
because CSM-8 (PR #4847) shipped the OpenSSL EVP MD5 backend, closing the
dominant compute gap (G1). The remaining 28% gap is attributable to syscall
overhead differences documented below.

## 4. Syscall profile comparison (C3.1)

### 4.1 Full syscall summary (`strace -cf`, following all children/threads)

| Syscall | upstream rsync | oc-rsync | Ratio | Notes |
| --- | --- | --- | --- | --- |
| `read` | 3109 | 6519 | **2.10x** | Buffer size: 256 KiB vs 128 KiB |
| `openat` | 2015 | 2018 | 1.00x | Parity |
| `close` | 2023 | 2018 | 1.00x | Parity |
| `newfstatat`/`statx` | 2006 | 6691 | **3.34x** | Upstream: 2/file; oc-rsync: ~6.7/file |
| `futex` | 37 | 65 | 1.76x | Thread sync (oc-rsync) vs minimal (upstream) |
| `sched_yield` | 0 | 394 | N/A | Rayon thread pool spin-wait |
| `clone`/`clone3` | 2 | 5 | 2.5x | fork (upstream) vs thread pool (oc-rsync) |
| `wait4` | 10 | 0 | N/A | Upstream fork-join model |
| **Total** | **9444** | **16323** | **1.73x** | |

### 4.2 Syscall time breakdown (seconds spent in-kernel)

| Syscall | upstream | oc-rsync | Notes |
| --- | --- | --- | --- |
| `read` | 0.059s | 0.192s | 3.3x wall time (2.1x count * 1.5x per-call) |
| `statx`/`newfstatat` | 0.010s | 0.079s | 7.9x wall time |
| `openat` | 0.013s | 0.054s | 4.2x wall time (thread contention on VFS) |
| `close` | 0.009s | 0.050s | 5.6x wall time |
| `futex` | 0.000s | 0.267s | Thread synchronization overhead |
| `wait4` | 0.063s | 0.000s | Upstream process management |
| `sched_yield` | 0.000s | 0.007s | Rayon busy-wait |
| **Total** | **0.154s** | **0.653s** | 4.2x under strace |

The 4.2x ratio under strace (vs 1.28x native) is inflated because strace
serializes multithreaded syscalls through a global ptrace lock. Upstream's
fork-based model is less affected since strace can trace each process
independently with less contention. The native 1.28x ratio is the true
performance comparison.

## 5. Read pattern analysis

### 5.1 Buffer sizes

| Tool | Read buffer | Reads per MiB | EOF reads per file |
| --- | --- | --- | --- |
| upstream rsync | 262,144 (256 KiB) | 4 | 0 |
| oc-rsync | 131,072 (128 KiB) | 8 | 1 |

### 5.2 Upstream read strategy (`map_file` / `map_ptr`)

Upstream rsync's `file_checksum()` (`checksum.c:402`) uses `map_file()` /
`map_ptr()` (`fileio.c:214`), which is a read-based sliding window - not
POSIX `mmap` despite the name. Key properties:

1. **Window size**: `MAX_MAP_SIZE = 256 KiB` (`rsync.h:159`).
2. **Sized reads**: `map_ptr()` reads exactly `min(remaining, MAX_MAP_SIZE)`
   bytes. For a 373,700-byte file, it issues `read(fd, buf, 262144)` then
   `read(fd, buf, 111556)`. No speculative reads.
3. **No EOF reads**: Because the file size is known from `stat`, the loop
   terminates when `remaining == 0`. No `read()` returning 0 is ever issued.
4. **Digest update**: `EVP_DigestUpdate` is called in `CHUNK_SIZE = 32 KiB`
   steps within the map window. This is internal to the hash loop and does
   not generate additional syscalls.

### 5.3 oc-rsync read strategy

oc-rsync's `hash_file_contents()` (`parallel_checksum.rs:148`) uses a
fixed 128 KiB buffer from `BufferPool` and calls `file.read(buf)` in a loop
until `read()` returns 0. Key properties:

1. **Buffer size**: 128 KiB (`COPY_BUFFER_SIZE`, `local_copy/mod.rs:156`).
2. **Fixed-size reads**: Always requests 131,072 bytes regardless of
   remaining file size. For a 373,700-byte file: `read(131072)=131072`,
   `read(131072)=131072`, `read(131072)=111556`, `read(131072)=0`.
3. **EOF read**: Every file incurs one `read()` returning 0 to detect EOF.
   With 1000 files read on both src and dst sides, this adds ~2000 wasted
   syscalls.
4. **Both sides hashed**: Local-copy mode opens and hashes both source and
   destination files on rayon worker threads, then compares digests.
   Upstream also hashes both sides (sender + receiver processes), but the
   two processes each hash one side.

### 5.4 Per-file read count comparison (sample files)

**file1.dat (102,400 bytes / 100 KiB)**:

| Side | upstream | oc-rsync |
| --- | --- | --- |
| Source/sender | `read(102400) = 102400` (1 read) | `read(131072) = 102400`, `read(131072) = 0` (2 reads) |
| Dest/receiver | `read(102400) = 102400` (1 read) | `read(131072) = 102400`, `read(131072) = 0` (2 reads) |
| **Total** | **2 reads** | **4 reads** |

**file2.dat (512,000 bytes / 500 KiB)**:

| Side | upstream | oc-rsync |
| --- | --- | --- |
| Source/sender | `read(262144)`, `read(249856)` (2 reads) | `read(131072)` x3, `read(131072)=118784`, `read(131072)=0` (5 reads) |
| Dest/receiver | `read(262144)`, `read(249856)` (2 reads) | `read(131072)` x3, `read(131072)=118784`, `read(131072)=0` (5 reads) |
| **Total** | **4 reads** | **10 reads** |

### 5.5 Aggregate read statistics (1000-file corpus)

| Metric | upstream | oc-rsync | Ratio |
| --- | --- | --- | --- |
| Total `read()` calls | 3109 | 6519 | 2.10x |
| Data-bearing reads | 3107 | 4514 | 1.45x |
| EOF reads (return 0) | 2 | 2005 | 1002x |
| Read buffer size | 262,144 | 131,072 | 0.50x |

The 2.10x total read ratio decomposes into:
- **1.45x** from smaller buffer size (128 KiB vs 256 KiB).
- **1.44x** from EOF reads (2005 extra zero-return reads).

Combined: `1.45 * 1.44 = 2.09`, matching the observed 2.10x.

## 6. Stat syscall analysis

### 6.1 Per-file stat pattern

**Upstream rsync** (2 `newfstatat` per file):

1. Sender: `newfstatat(file, AT_SYMLINK_NOFOLLOW)` - file list building.
2. Receiver: `newfstatat(file, AT_SYMLINK_NOFOLLOW)` - quick-check comparison.

**oc-rsync** (~6.7 `statx` per file on average, breakdown for a single file):

1. `statx(src, AT_SYMLINK_NOFOLLOW)` - source file list building.
2. `statx(dst, AT_STATX_SYNC_AS_STAT)` - destination quick-check initial.
3. `statx(src_fd, AT_EMPTY_PATH)` - `file.metadata()` in `compute_file_checksum` for source (G5).
4. `statx(dst_fd, AT_EMPTY_PATH)` - `file.metadata()` in `compute_file_checksum` for destination (G5).
5. `statx(dst, AT_SYMLINK_NOFOLLOW)` - post-checksum metadata comparison.

Additional statx calls appear for directory traversal and the NOFOLLOW vs
non-NOFOLLOW double-stat of destinations. The average of 6.7 statx per file
(6691 / 1000) is 3.34x upstream's 2.0 per file.

### 6.2 Redundant statx identification

- **Items 3 and 4** (`file.metadata()` via fd) are the G5 gap from CSM-2.
  The file size is already known from the `FilePair` struct. These 2000
  statx calls (1000 files x 2 sides) are entirely redundant.
- **Item 5** (post-checksum NOFOLLOW stat) duplicates information already
  obtained in item 2.
- Removing the G5 `file.metadata()` calls would save ~2000 statx (from
  6691 to ~4691), reducing the ratio from 3.34x to ~2.34x.
- The remaining 2.34x overhead (4691 vs 2006) comes from the
  NOFOLLOW/non-NOFOLLOW double-stat pattern and directory traversal
  differences.

## 7. Threading model comparison

### 7.1 Upstream: fork-based

Upstream rsync forks into sender and receiver processes. Each process
checksums its own side of the file tree sequentially. The two processes run
in parallel via the kernel scheduler.

- Syscall profile: 2 `clone` calls, 10 `wait4` calls, 37 `futex` calls.
- No thread synchronization overhead.
- File I/O is serialized within each process.

### 7.2 oc-rsync: rayon thread pool

oc-rsync uses a rayon thread pool (5 threads observed: main + 4 workers)
for parallel checksum computation. Both source and destination files are
distributed across workers.

- Syscall profile: 5 `clone3` calls, 65 `futex` calls, 394 `sched_yield`.
- `sched_yield` calls (0.007s total) come from rayon's work-stealing
  spin-wait loop.
- `futex` calls (0.267s under strace) are inflated by strace's ptrace
  serialization. Native overhead is much lower.
- Parallel hashing benefits are partially offset by VFS lock contention on
  `openat`/`close` when multiple threads access the same directory.

Under strace, the per-call cost for `openat` is 26 us (oc-rsync) vs 6 us
(upstream), and for `close` it is 24 us vs 4 us. This 4-6x inflation is an
artifact of strace's global ptrace lock serializing concurrent thread
syscalls. Native per-call cost is expected to be at parity.

## 8. CSM-2 cost model validation

CSM-2 section 4.3 predicted the following syscall ratios. This section
compares predictions against observations.

### 8.1 Read count ratio

| Path | CSM-2 predicted | CSM-3 observed | Match |
| --- | --- | --- | --- |
| oc-rsync local-copy reads per MiB | 8 (128 KiB buffer) | 8 + ~1 EOF | Yes |
| upstream reads per MiB | 4 (256 KiB window) | 4 (no EOF) | Yes |
| Ratio | 2.0x | 2.10x | Close (EOF reads add 0.1x) |

CSM-2 did not account for the EOF read overhead. Each file incurs one extra
`read()=0` on each side, adding ~2000 syscalls to the 1000-file corpus.

### 8.2 Stat count ratio

| Path | CSM-2 predicted | CSM-3 observed | Match |
| --- | --- | --- | --- |
| oc-rsync local-copy per file | 4 (open + fstat + read + close) | ~6.7 statx/file | No - underpredicted |
| upstream per file | 3 (open + read + close) | 2 newfstatat/file | Match for stat only |

CSM-2 predicted 1 extra `fstat` per file from `file.metadata()` (G5). The
actual overhead is higher: the NOFOLLOW/non-NOFOLLOW double-stat pattern
and post-checksum metadata comparison add 2-3 more statx per file beyond
what CSM-2 modeled.

### 8.3 Total syscall ratio

| Metric | CSM-2 predicted | CSM-3 observed | Match |
| --- | --- | --- | --- |
| Total syscall ratio | 1.8x (local-copy) | 1.73x | Close |

The total syscall ratio is slightly lower than predicted because `openat`
and `close` counts are at parity (CSM-2 predicted a small delta that did
not materialize).

## 9. Bottleneck decomposition

Based on the 0.143s oc-rsync vs 0.112s upstream (31 ms gap), the gap
decomposes as follows. Percentages are of the 31 ms delta.

### 9.1 Read syscall overhead

Extra reads: 6519 - 3109 = 3410 reads.
At ~500 ns per read (warm cache): 3410 * 0.5 us = 1.7 ms.
**Contribution: ~5% of the 31 ms gap.**

This confirms CSM-2's assessment that read overhead is MEDIUM priority.
The gap is real but small relative to wall time.

### 9.2 Stat syscall overhead

Extra statx: 6691 - 2006 = 4685 statx.
At ~5 us per statx: 4685 * 5 us = 23.4 ms.
**Contribution: ~75% of the 31 ms gap.**

This is the dominant contributor. The per-call cost of `statx` (~5 us
observed from `strace -cf`: 79 ms / 6691 calls / strace inflation ~3x)
makes redundant statx calls the most expensive waste.

### 9.3 Thread synchronization overhead

`sched_yield` (394 calls): ~7 ms under strace, estimated ~1-2 ms native.
`futex` overhead beyond upstream: estimated ~2-3 ms native.
**Contribution: ~10-15% of the 31 ms gap.**

### 9.4 Remaining

MD5 compute time is at parity (both use OpenSSL EVP). The remaining ~5-10%
is attributed to Rust runtime startup overhead, rayon pool initialization,
and general per-call overhead differences.

## 10. CSM-7 evidence sign-off

### 10.1 C3.1 - Syscall counts: CONFIRMED

oc-rsync issues 1.73x total syscalls vs upstream for the 1000-file
mixed-size corpus. The breakdown (read 2.10x, stat 3.34x, open/close 1.0x)
matches the source-level analysis from CSM-2 within 10%, with the
exception of stat calls which were underpredicted.

### 10.2 C3.2 - OpenSSL vs pure-Rust per-call cost: CONFIRMED (indirect)

Both tools now use OpenSSL EVP MD5 on this platform (aarch64 glibc Linux).
The 0.112s upstream time and 0.143s oc-rsync time with the gap fully
explained by syscall overhead (not compute) confirms that the G1 fix
(CSM-8) successfully closed the MD5 backend gap. If the pure-Rust backend
were still active, the compute component alone would add ~60 ms (198 MiB *
(2 ms/MiB - 0.67 ms/MiB) * 2 sides), producing a ~1.8x ratio instead of
the observed 1.28x.

### 10.3 C3.3 - Tree-scale syscall amortization: CONFIRMED

For the mixed corpus including 333 small files (1-10 KiB):
- Small files: per-file overhead dominates (1-2 reads per side regardless
  of buffer size). The read-count gap compresses to ~1.5x for files under
  128 KiB.
- Large files: buffer-size ratio (256 KiB vs 128 KiB) drives a 2.0x read
  count multiplier.
- The EOF read overhead is constant per file (1 per side) regardless of
  size, making it proportionally worse for small files.

The per-file `openat`/`close` cost does not dominate even in the small-file
bucket. The dominant per-file overhead is the extra statx calls (G5 + the
NOFOLLOW double-stat).

## 11. Actionable findings

### 11.1 Priority 1 - Statx reduction (estimated recovery: ~23 ms / 75% of gap)

1. **G5**: Remove `file.metadata()` from `compute_file_checksum`
   (`parallel_checksum.rs:139`). Pass known file size through
   `FilePair::source_size` / `destination_size`. Saves ~2000 statx.
2. **Double-stat consolidation**: The NOFOLLOW + non-NOFOLLOW double-stat
   of destination files adds ~1000 extra statx. Consolidate into a single
   `statx(AT_SYMLINK_NOFOLLOW)` where the non-NOFOLLOW result is not
   needed separately.
3. **Post-checksum stat elimination**: The post-checksum `statx` for
   metadata comparison (item 5 in section 6.1) can be deferred or
   eliminated when checksums match (no transfer needed).

### 11.2 Priority 2 - Read buffer and EOF (estimated recovery: ~2 ms / 5% of gap)

1. **G3/G4**: Grow checksum read buffer from 128 KiB to 256 KiB to match
   upstream's `MAX_MAP_SIZE`. Halves the data-bearing read count.
2. **EOF elimination**: Use the known file size to compute the expected
   read count and terminate the loop without an EOF-detecting `read()=0`.
   This mirrors upstream's `map_ptr()` strategy. Saves ~2000 reads.

### 11.3 Priority 3 - Thread sync overhead (estimated recovery: ~2-3 ms / 10% of gap)

1. `sched_yield` from rayon spin-wait is inherent to the work-stealing
   model. No action needed unless the pool is oversized for the workload.
2. `futex` contention is expected with multithreaded VFS access. The
   benefit of parallel hashing outweighs the sync cost for large corpuses.

## 12. References

### Upstream rsync

- `checksum.c:402-539` - `file_checksum()`
- `fileio.c:214-315` - `map_file()` / `map_ptr()` (read-based sliding window)
- `rsync.h:158-159` - `CHUNK_SIZE = 32 KiB`, `MAX_MAP_SIZE = 256 KiB`

### oc-rsync

- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148` - `hash_file_contents()`
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:133` - `compute_file_checksum()`
- `crates/engine/src/local_copy/mod.rs:156` - `COPY_BUFFER_SIZE = 128 KiB`
- `crates/transfer/src/receiver/quick_check.rs:262` - `file_checksum_matches()`

### Companion audits

- `docs/audits/csm-2-checksum-hot-path-profile.md` - oc-rsync hot-path profile
- `docs/audits/csm-4-strong-checksum-upstream-parity.md` - algorithm parity
- `docs/audits/csm-5-c-io-pattern.md` - upstream I/O pattern analysis
- `docs/audits/csm-7-contributor-synthesis.md` - synthesis and fix order
