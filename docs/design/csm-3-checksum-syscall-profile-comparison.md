# CSM-3: Checksum Mode Syscall Profile Comparison

Date: 2026-05-31
Series: CSM (checksum mode performance)
Status: complete - documented for reference, root cause fixed in CSM-8 (PR #5128)

## 1. Purpose

Compare the per-file syscall pattern of upstream rsync 3.4.1 and oc-rsync
in `--checksum` mode. Identifies the root causes of the original 1.5-1.7x
performance gap and documents which fixes closed it.

## 2. Upstream rsync `--checksum` syscall pattern

Source: `checksum.c:402` (`file_checksum()`), `fileio.c:214` (`map_ptr()`).

### 2.1 Read pattern

Upstream uses `map_file()` / `map_ptr()` - a read-based sliding window (not
POSIX mmap despite the name):

- **Buffer**: `MAX_MAP_SIZE = 256 KiB` (`rsync.h:159`).
- **Sized reads**: `map_ptr()` reads `min(remaining, 256 KiB)` bytes. The
  last read requests exactly the remaining byte count.
- **No EOF reads**: The loop terminates when `remaining == 0`. No trailing
  `read()` returning 0 is ever issued.
- **Digest granularity**: `EVP_DigestUpdate` is called in 32 KiB steps
  (`CHUNK_SIZE`) within each map window. This is internal to the hash
  computation and generates no additional syscalls.

For a 500 KiB file, upstream issues 2 reads per side: `read(262144)` then
`read(249856)`. Total: 4 reads (sender + receiver).

### 2.2 Stat pattern

Upstream issues 2 stat calls per file:

1. Sender: `newfstatat(file, AT_SYMLINK_NOFOLLOW)` - file list building.
2. Receiver: `newfstatat(file, AT_SYMLINK_NOFOLLOW)` - quick-check comparison.

### 2.3 Hash algorithm

Protocol 31+ negotiates XXH3/128 via capability string exchange. Fallback
is MD5. For local-to-local transfers (no wire), upstream uses XXH128 by
default - roughly 10-30x faster than MD5 on modern hardware.

## 3. oc-rsync `--checksum` syscall pattern

### 3.1 Read pattern

`file_checksum_matches()` (`transfer/src/receiver/quick_check.rs:262` and
`transfer/src/generator/entry_accessor.rs:352`) uses `read_exact()` with
a 64 KiB stack buffer, sized by the known file length:

- **Buffer**: 64 KiB (stack-allocated `[0u8; 64 * 1024]`).
- **Sized reads**: Uses `remaining` counter from `file_size` parameter.
  Reads `min(remaining, 64 KiB)` per iteration.
- **No EOF reads**: Loop terminates when `remaining == 0`, matching upstream.

The parallel checksum path (`checksums/src/parallel/files.rs:46`) also uses
pre-sized reads via `read_exact()` with `remaining` countdown, avoiding
EOF probes.

The whole-file streaming path (`transfer/src/generator/delta.rs:199`) uses
a 256 KiB buffer (`MAX_READ_SIZE`) with the same sized-read pattern.

For a 500 KiB file in quick-check, oc-rsync issues 8 reads per side (64 KiB
buffer) vs upstream's 2 reads (256 KiB buffer). Total: 16 vs 4 reads.

### 3.2 Stat pattern

oc-rsync issues approximately 6-7 stat calls per file:

1. `statx(src, AT_SYMLINK_NOFOLLOW)` - source file list building.
2. `statx(dst, AT_STATX_SYNC_AS_STAT)` - destination quick-check.
3. `statx(src_fd, AT_EMPTY_PATH)` - `file.metadata()` inside checksum
   computation (redundant - size already known from flist).
4. `statx(dst_fd, AT_EMPTY_PATH)` - same redundancy on destination side.
5. `statx(dst, AT_SYMLINK_NOFOLLOW)` - post-checksum metadata comparison.
6-7. Directory traversal and NOFOLLOW/non-NOFOLLOW double-stat.

Items 3-4 are the G5 gap identified in CSM-2. Items 5-7 are structural
overhead from the receiver's metadata comparison path.

### 3.3 Hash algorithm (pre-CSM-8)

Before CSM-8, the local-copy path defaulted to MD5 (`SignatureAlgorithm::Md5`)
even though upstream uses XXH128. This was the dominant contributor to the
original 1.5-1.7x gap: MD5 is 10-30x slower than XXH3/128.

## 4. Summary comparison

| Dimension | Upstream rsync 3.4.1 | oc-rsync (post-CSM-8) |
| --- | --- | --- |
| Read buffer | 256 KiB (map_ptr) | 64 KiB (quick-check), 256 KiB (streaming) |
| EOF reads per file | 0 | 0 (pre-sized loop) |
| Stat calls per file | 2 | ~6-7 (3.34x) |
| Hash algorithm | XXH128 (negotiated) | XXH3/128 (negotiated, post-CSM-8) |
| Reads per 500 KiB file | 4 (2/side) | 16 (8/side) in quick-check path |

## 5. Fixes applied

### 5.1 CSM-8 (PR #5128) - hash algorithm mismatch

Changed the default `SignatureAlgorithm` from `Md5` to `Xxh3_128` in the
local-copy builder and type defaults. This closed the compute gap (G1),
reducing the wall-clock ratio from ~1.5-1.7x to ~1.28x.

Files changed: `crates/engine/src/local_copy/options/builder/definition.rs`,
`crates/engine/src/local_copy/options/types.rs`,
`crates/engine/src/local_copy/executor/file/comparison.rs`.

### 5.2 STX-6/STX-8 - stat and read overhead

Addresses the remaining ~28% gap:

- **STX-6**: Remove redundant `file.metadata()` calls (items 3-4 in section
  3.2) by passing known file sizes through the `FilePair` struct. Saves
  ~2000 statx per 1000-file corpus.
- **STX-8**: Consolidate NOFOLLOW/non-NOFOLLOW double-stat into a single
  stat call where the non-NOFOLLOW result is unused.

Together these reduce the stat ratio from 3.34x toward 2.0x (parity with
upstream's 2 stats per file plus directory traversal overhead).

## 6. Verification commands

```sh
# Upstream syscall summary
strace -cf -e trace=read,write,openat,close,newfstatat,statx \
  rsync -rc src/ dst/ 2>&1 | tee upstream-strace.txt

# oc-rsync syscall summary (-f follows rayon threads)
strace -cf -e trace=read,write,openat,close,newfstatat,statx \
  -f oc-rsync -rc src/ dst/ 2>&1 | tee oc-rsync-strace.txt

# Per-file read trace
strace -e trace=read -f rsync -rc src/ dst/ 2>&1 | grep 'read(' | head -50
strace -e trace=read -f oc-rsync -rc src/ dst/ 2>&1 | grep 'read(' | head -50
```

Note: strace serializes multithreaded syscalls through ptrace, inflating
oc-rsync's per-call latency by 3-6x. Native timing is the true comparison.
