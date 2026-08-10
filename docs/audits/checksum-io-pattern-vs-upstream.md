# Checksum file I/O pattern vs upstream rsync 3.4.1

Tracks task #1043. Profiles the file I/O patterns oc-rsync uses while
computing whole-file checksums for `--checksum` (`-c`) mode and compares
them, byte-for-byte and syscall-for-syscall, with upstream rsync 3.4.1.

The aim is to identify divergences that cost wall time on cold-cache
runs where checksum computation is the dominant cost (millions of small
files, NVMe storage, single-threaded `mdfour`/`md5`).

## 1. oc-rsync read paths for `--checksum` mode

### 1.1 Receiver-side quick-check (active path)

`crates/transfer/src/receiver/quick_check.rs` is the primary `--checksum`
hot path on the destination. `quick_check_matches()` short-circuits on
size mismatch, then for `always_checksum` calls `file_checksum_matches()`
(line 225):

```
let mut file = fs::File::open(path)?;
let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
let mut buf = [0u8; 64 * 1024];
let mut remaining = file_size;
while remaining > 0 {
    let to_read = buf.len().min(remaining as usize);
    file.read_exact(&mut buf[..to_read]).is_err() ? return false;
    hasher.update(&buf[..to_read]);
    remaining -= to_read as u64;
}
```

Key properties:

- Stack-allocated 64 KiB buffer, no heap.
- Sequential `read_exact()` only - no `mmap`, no `posix_fadvise`,
  no `readahead`.
- File descriptor is `std::fs::File` (raw `O_RDONLY`, no `O_NOATIME`,
  no `O_DIRECT`).
- One `open()` and one `close()` (via `Drop`). No `lseek` after open.

### 1.2 Generator-side flist sender (NOT WIRED)

`crates/transfer/src/generator/file_list/entry.rs` populates `FileEntry`
fields (mode, mtime, uid/gid, hardlink dev/ino, xattrs) but never calls
`set_checksum()`. `crates/transfer/src/generator/mod.rs:552` toggles the
writer's `with_always_checksum(...)` length, but
`protocol/src/flist/write/encoding.rs:288` falls back to writing
`flist_csum_len` zero bytes when `entry.checksum()` returns `None`.

Net effect: when oc-rsync is the sender and `--checksum` is requested,
the file list still carries the `flist_csum_len` field per regular file
but every digest is all-zero. Receivers that compare against this field
(upstream rsync 3.4.1 with `always_checksum && am_sender == false`) will
treat every regular file as a checksum mismatch and re-transfer.

This is a separate functional gap from the I/O pattern audit and is
filed for follow-up; it is NOT covered by the alignment recommendations
below, which focus on the existing receiver-side read path.

### 1.3 Whole-file streaming during transfer

`crates/transfer/src/generator/delta.rs::stream_whole_file_transfer()`
(line ~196) reads the source file with `read_exact` against a buffer
sized `min(file_size, 256 KiB)` and feeds the `ChecksumVerifier` inline
while the bytes are streamed onto the wire. This is a separate path
from `--checksum` mode and already matches upstream's `MAX_MAP_SIZE`
window of 256 KiB.

### 1.4 Parallel hasher (capable but unused for `--checksum`)

`crates/checksums/src/parallel/files.rs::hash_file_internal()` already
implements the upstream-equivalent strategy with three tiers:

- `size <= max_memory_file_size` (1 MiB default): `read_to_end()` into
  a single `Vec`.
- `size >= MMAP_THRESHOLD` (64 KiB; `crates/fast_io/src/mmap_reader.rs`):
  `MmapReader::open()` + `advise_sequential()` (`MADV_SEQUENTIAL`),
  then `D::digest(mmap.as_slice())` in one shot.
- Fallback: `BufReader::with_capacity(buffer_size)` + 64 KiB streaming.

This routine is invoked by signature builders, not by the receiver
quick-check, so the `--checksum` hot path bypasses it.

### 1.5 Pipelined double-buffered reader

`crates/checksums/src/pipelined/reader.rs::DoubleBufferedReader` spawns
a worker thread that reads 64 KiB blocks ahead while the consumer
hashes the previous block. Activated above 256 KiB by default. Wired
into `compute_checksums_pipelined()` and `PipelinedChecksumIterator`.
Like the parallel hasher, it is currently unused by the
`--checksum` quick-check path.

## 2. Upstream rsync 3.4.1 read paths

### 2.1 Quick-check path (`flist.c:1412`, `checksum.c:402`)

Upstream's `quick_check_ok()` in `generator.c` calls `file_checksum()`
(`checksum.c:402`) with the destination stat:

```c
fd = do_open_checklinks(fname);
buf = map_file(fd, len, MAX_MAP_SIZE, CHUNK_SIZE);
for (i = 0; i + CHUNK_SIZE <= len; i += CHUNK_SIZE)
    EVP_DigestUpdate(evp, map_ptr(buf, i, CHUNK_SIZE), CHUNK_SIZE);
remainder = (int32)(len - i);
if (remainder > 0)
    EVP_DigestUpdate(evp, map_ptr(buf, i, remainder), remainder);
EVP_DigestFinal_ex(evp, sum, NULL);
close(fd); unmap_file(buf);
```

Constants from `rsync.h:158`:

- `CHUNK_SIZE = 32 * 1024` (32 KiB hash feed)
- `MAX_MAP_SIZE = 256 * 1024` (256 KiB sliding window)

### 2.2 Sliding window via `map_file`/`map_ptr`

`fileio.c:218 map_file()` and `fileio.c:236 map_ptr()` implement a
read-backed sliding window. Despite the name, this is NOT `mmap(2)` -
the comment on line 215 explains the avoidance: another writer
truncating the file could deliver `SIGBUS`. The window:

- Allocates one heap buffer aligned to `CHUNK_SIZE` boundaries.
- Issues `lseek` only when the requested window is non-contiguous
  with the prior window (cache miss).
- For contiguous sequential access (the only pattern in
  `file_checksum`), the buffer is filled with sequential `read()`
  calls of up to 256 KiB.
- Reuses overlapping bytes from the previous window via `memmove`
  when the new window starts inside the prior buffer.

For `file_checksum`, where `i` advances monotonically by
`CHUNK_SIZE = 32 KiB` and the window is `MAX_MAP_SIZE = 256 KiB`, the
window misses every 8 chunks. Steady state: 1 `read(256 KiB)` per 8
hash updates of 32 KiB.

### 2.3 Sender-side flist (`flist.c:1412-1416`)

`always_checksum && am_sender && S_ISREG()` triggers
`file_checksum(thisname, &st, tmp_sum)` during `make_file()`,
populating `F_SUM(file)` (line 1505) so each `FileEntry` writes
`flist_csum_len` non-zero bytes via `flist.c:670`.

## 3. Read-size and seek pattern differences

### 3.1 Buffer size

| | Upstream `file_checksum` | oc-rsync `file_checksum_matches` |
|-|-|-|
| Hash update granularity | 32 KiB (`CHUNK_SIZE`) | 64 KiB |
| Underlying read size | up to 256 KiB (`MAX_MAP_SIZE`) | 64 KiB |
| Reads per 1 MiB | 4 | 16 |
| `read()` syscall count for 1 GiB | ~4 K | ~16 K |

oc-rsync issues 4x more `read()` calls than upstream for the same
file. On NVMe (5 us per syscall), this is ~60 ms additional kernel
time per GiB - small individually but compounds with millions of
files.

### 3.2 Seek pattern

Upstream emits zero `lseek` calls for sequential whole-file
checksumming because `map_ptr` only seeks on cache miss with a
discontiguous offset.

oc-rsync emits zero `lseek` calls (each `read_exact` advances the
descriptor naturally). Parity here.

### 3.3 Page-cache hints

Upstream issues no `posix_fadvise`/`madvise` calls for `file_checksum`.

oc-rsync's `parallel/files.rs` calls `MmapReader::advise_sequential()`
(`MADV_SEQUENTIAL`) when mmap is used, which triggers kernel
read-ahead. This is an oc-rsync win - but only on the unused parallel
path. The active receiver quick-check path issues no advisory hints.

### 3.4 Hash drive granularity

`mdfour` requires 64-byte (512-bit) blocks; `md5` and `sha1` require
64 byte; `xxh3` accepts arbitrary input. Upstream's 32 KiB feed and
oc-rsync's 64 KiB feed are both well above any internal block boundary
so the actual digest output is identical regardless of feed size.

### 3.5 Open flags

Both implementations open `O_RDONLY` without `O_NOATIME` or
`O_DIRECT`. Parity. (Upstream's `do_open_checklinks` honours
`--copy-links` mode, which is irrelevant for the destination-side
quick-check.)

## 4. Alignment recommendations

The recommendations below are ordered by expected impact for the
common case: cold-cache scan of millions of small-to-medium regular
files on local SSD/NVMe.

### 4.1 Match upstream's 256 KiB sliding-window read size

**Action.** Change the buffer in
`receiver/quick_check.rs::file_checksum_matches` from
`[0u8; 64 * 1024]` to `[0u8; 256 * 1024]` (or a heap-allocated
`Vec<u8>` reused across files).

**Rationale.** Upstream sized `MAX_MAP_SIZE = 256 KiB` deliberately so
8 hash updates amortise into 1 `read()`. Matching the read size cuts
syscall count 4x with no algorithmic risk and zero protocol impact.
Hash state is unaffected because the digest update granularity
(64-byte mdfour / md5 internal block) is far below either buffer size.

### 4.2 Issue `posix_fadvise(POSIX_FADV_SEQUENTIAL)` on the file

**Action.** After `fs::File::open()`, on Unix call
`libc::posix_fadvise(fd, 0, 0, POSIX_FADV_SEQUENTIAL)`. On Linux also
emit `POSIX_FADV_WILLNEED` for the first 1-2 MiB. Wrap in a helper in
`fast_io` so the unsafe is contained per the unsafe-code policy.

**Rationale.** Upstream does not do this, but oc-rsync's
`MmapReader::advise_sequential()` already establishes the helper. For
the read-based path the equivalent is `posix_fadvise`, which causes
the kernel to double the read-ahead window and drop pages aggressively
after consumption. On checksums-bound workloads this typically
delivers 10-25% throughput on cold cache.

### 4.3 Use mmap for files above a threshold (large-file fast path)

**Action.** When `file_size >= 1 MiB`, route through
`fast_io::MmapReader` and feed `ChecksumVerifier::update(mmap.as_slice())`
in one call. Fall back to the read-based path when `MmapReader::open()`
fails (NFS, FUSE, procfs, files truncating mid-scan).

**Rationale.** Upstream explicitly avoids mmap for safety
(`fileio.c:215` `SIGBUS` comment), so this is a deliberate divergence.
The same risk exists in oc-rsync, so the fallback is mandatory; treat
mmap failure as soft and retry with `read()`. On warm cache the
zero-copy benefit is small; on cold cache with files in the
1-128 MiB range, eliminating the per-chunk `read()`/`copy_to_user`
round-trip is measurable. The parallel hasher already implements
exactly this logic and can be lifted as the shared helper.

### 4.4 Pipeline reads with hashing for large files

**Action.** For files above ~256 KiB, route through
`crates/checksums/src/pipelined/PipelinedChecksumIterator` (or an
inline single-block double buffer). The reader thread fills the next
64 KiB while the main thread feeds the prior block to the digest.

**Rationale.** mdfour/md5 single-thread throughput on aarch64 is
~600 MB/s (no CPU-supported MD5); NVMe sequential read is ~3-7 GB/s.
With a single-threaded read+hash loop the digest is the bottleneck
for these algorithms, so pipelining buys nothing. For xxh3 (~12 GB/s
on AVX2), the read becomes the bottleneck and pipelining wins ~20%.
Cost is one std `mpsc` channel and a worker thread per file - so
gate on size and on the negotiated digest being xxh3/xxh128.

### 4.5 io_uring batched reads on Linux

**Action.** Behind the existing `fast_io` cfg gate
(`#[cfg(all(target_os = "linux", feature = "io_uring"))]`), submit
multiple `IORING_OP_READ` SQEs of `MAX_MAP_SIZE` each, draining the
CQ while the digest catches up. Gate by file size (eg `>= 4 MiB`)
and by checksum cost (xxh3/xxh128).

**Rationale.** Upstream has no equivalent. On a single 1 GiB file
the syscall savings of io_uring over read+pipeline are modest, but
when many files are checksummed concurrently (the receiver quick-check
loop already supports rayon parallelism) io_uring removes the
per-syscall context switch cost. Aligns with the existing fast_io
direction without affecting the read-based fallback. Implementation
complexity is the highest of the five recommendations, so order this
last.

## Summary

oc-rsync's `--checksum` mode receiver is functionally correct but
issues 4x more `read()` syscalls than upstream and emits no page-cache
advisory hints, despite already having mmap, pipelined, and parallel
helpers built and tested in `crates/checksums/`. The simplest gain
(rec 4.1) is a one-line buffer-size change. The next two
(`posix_fadvise` and large-file mmap) are extensions of
helpers that already exist in `fast_io` and the parallel hasher.
Recommendations 4.4 and 4.5 are conditional on digest cost and Linux
availability and should be measured before being made default.

Sender-side flist checksum population (section 1.2) is a separate
functional gap and is tracked outside this audit.
