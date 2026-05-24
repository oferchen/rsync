# CSM-5 - `--checksum` whole-file I/O pattern vs upstream

Date: 2026-05-24
Scope: read-only research, no `.rs` edits
Tracked under: CSM-5 (parent CSM track: `--checksum` mode is ~1.5-1.7x slower
than upstream rsync 3.4.1; upstream issue #970)
Feeds: CSM-7 (synthesis), CSM-8 (fix)
Related: CSM-4 (`docs/audits/csm-4-strong-checksum-upstream-parity.md`),
CSP audit (`docs/audits/checksum-mode-computation-cost.md`)

## Goal

CSM-4 audited the strong-checksum algorithm parity and noted "I/O pattern
divergence (HIGH on warm cache)" as item O1 in its ranked list. This audit
zooms into that single item: the per-file I/O pattern of the `-c` whole-file
rehash. The questions answered here are:

1. What buffer size does oc-rsync read the basis file in for `-c` mode?
2. Does it use `mmap`, `read`, `pread`, or something else?
3. If io_uring is available, does the `-c` path use it (READ_FIXED, registered
   buffers, batch SQEs)?
4. What is the syscall mix per N-MB file vs upstream's mix?
5. Is the gap from CSM-4's item O1 confirmed, refuted, or quantitatively
   different?

This is a docs-only audit. It does not modify any `.rs` files.

## 1. Code paths under audit

### 1.1 oc-rsync

Two distinct `-c` rehash sites exist:

| Mode | Entry point | Hash dispatcher |
| --- | --- | --- |
| Network receiver (default `-c` path; runs on the receiver of `--checksum`) | `crates/transfer/src/receiver/transfer/candidates.rs:128-206` decides `always_checksum`, then calls `quick_check_matches` in `crates/transfer/src/receiver/quick_check.rs:46-89`, which delegates to `file_checksum_matches` at `crates/transfer/src/receiver/quick_check.rs:262-287` | `crates/transfer/src/delta_apply/checksum.rs::ChecksumVerifier::for_algorithm` (per-file fresh enum-dispatched hasher) |
| Local-copy executor (`-c` on a local copy without network) | `crates/engine/src/local_copy/executor/directory/recursive/checksum.rs:92-110` invokes `prefetch_directory_checksums` which builds a `ChecksumCache` via `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs::prefetch_checksums:88-130`; each pair flows through `compute_file_checksum:133-145` and `hash_file_contents:148-242` | `checksums::strong::{Md4, Md5, Xxh3, ...}` (per-file fresh hasher) |

There is **no third path**. The standalone `crates/checksums/src/parallel/files.rs::hash_file_internal` is the only oc-rsync code that uses `MmapReader` for whole-file hashing, but it is **not wired** to either `-c` mode (no production callers; only invoked from its own crate tests).

### 1.2 Upstream rsync 3.4.1

One path serves both sender and receiver:

| Mode | Entry point | Dispatcher |
| --- | --- | --- |
| Sender (when `always_checksum && am_sender && S_ISREG`) | `target/interop/upstream-src/rsync-3.4.1/flist.c:1412-1413` `file_checksum(thisname, &st, tmp_sum)` during file-list build | `target/interop/upstream-src/rsync-3.4.1/checksum.c:402 file_checksum` |
| Receiver/generator (when `always_checksum > 0`) | `target/interop/upstream-src/rsync-3.4.1/generator.c:626-628` `if (always_checksum > 0) { file_checksum(fn, st, sum); ... }` inside `quick_check_ok` | `target/interop/upstream-src/rsync-3.4.1/checksum.c:402 file_checksum` |

Both upstream sites go through the **same** `file_checksum()` and thus share
the same I/O pattern.

## 2. The oc-rsync I/O pattern

### 2.1 Network receiver (`quick_check.rs::file_checksum_matches`)

`crates/transfer/src/receiver/quick_check.rs:262-287`:

```rust
fn file_checksum_matches(path, file_size, algorithm, expected) -> bool {
    let Ok(mut file) = fs::File::open(path) else { return false; };
    let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
    let mut buf = [0u8; 64 * 1024];                       // 64 KiB on the stack
    let mut remaining = file_size;
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        if file.read_exact(&mut buf[..to_read]).is_err() { return false; }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    ...
}
```

Per-file syscall trace (Linux, warm cache):

| Syscall | Count | Notes |
| --- | --- | --- |
| `openat(O_RDONLY)` (via `fs::File::open`) | 1 | std opens with `O_RDONLY`, no `O_NOFOLLOW`, no `O_NOATIME` |
| `statx` (NOT `fstat` here; the size was captured upstream by `quick_check_matches` from `dest_meta`) | 0 | already in caller |
| `read(fd, stack_buf, 65536)` (via `std::io::Read::read_exact`) | `ceil(size / 65536)` (best case 1 per chunk; `read_exact` may loop on short reads) | the buffer is allocated on the stack each call and reads land in userspace, then are passed to the digest |
| `close(fd)` (via `Drop`) | 1 | |
| `mmap` / `munmap` / `madvise` | 0 | path does not call `mmap` |
| `io_uring_enter` | 0 | path does not use io_uring |
| `pread`, `readv`, `preadv` | 0 | plain blocking `read` |

The buffer is `[0u8; 64 * 1024]` zero-initialised on the stack on every call.
No `BufferPool`, no page-aligned allocation, no `O_NOATIME`, no
`posix_fadvise(POSIX_FADV_SEQUENTIAL)`, no `madvise(MADV_SEQUENTIAL)`.

### 2.2 Local-copy executor (`parallel_checksum.rs::hash_file_contents`)

`crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148-242`:

```rust
fn hash_file_contents(mut file: File, algorithm, buffer_pool) -> io::Result<Vec<u8>> {
    let mut buffer = BufferPool::acquire_from(Arc::clone(buffer_pool));   // 128 KiB (default)
    ...
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    ...
}
```

The buffer is pulled from `global_buffer_pool()` whose default capacity is set
by `crates/engine/src/local_copy/buffer_pool/global.rs:73-88` to
`super::super::COPY_BUFFER_SIZE` = **128 KiB** (`crates/engine/src/local_copy/mod.rs:156`).
The caller `compute_file_checksum` at `parallel_checksum.rs:133-145` does:

```rust
let file = File::open(path).ok()?;
let metadata = file.metadata().ok()?;          // extra fstat per file
let size = metadata.len();
let digest = hash_file_contents(file, algorithm, buffer_pool).ok()?;
```

Per-file syscall trace (Linux, warm cache):

| Syscall | Count | Notes |
| --- | --- | --- |
| `openat(O_RDONLY)` | 1 | |
| `statx`/`fstat` (`file.metadata()`) | 1 | extra over the receiver path because this site is called outside `quick_check_matches`'s caller-provided stat |
| `read(fd, pooled_buf, 131072)` | `ceil(size / 131072)` (best case) | uses `read`, not `read_exact`. Short reads loop naturally |
| `close(fd)` | 1 | |
| `mmap` / `madvise` | 0 | path does not call `mmap` |

The buffer is acquired from a process-wide `Arc<Mutex<Vec<Vec<u8>>>>` pool so
the allocation cost is amortised across files (one of the few wins over the
receiver path). Same I/O model otherwise.

### 2.3 io_uring path

`-c` rehash does not currently dispatch through io_uring on either site.

- `quick_check.rs::file_checksum_matches` calls `fs::File::open` and
  `std::io::Read::read_exact`. Neither routes through `fast_io::io_uring`.
- `parallel_checksum.rs::hash_file_contents` calls `std::fs::File::read`.
  Same outcome.
- `fast_io::IoUringReader` and `fast_io::IoUringReadFixed` exist for the
  receiver-side data-write path; they are **not** consumed by either rehash
  site. There are no callers of `READ_FIXED` or registered buffers for `-c`
  rehash today.

Implication: every `-c` rehash on Linux pays a full `read(2)` syscall per
buffer-sized chunk and gets zero benefit from `IORING_REGISTER_BUFFERS`,
`IORING_OP_READ_FIXED`, or batched SQE submission. The `-c` rehash is a
pure-blocking, one-fd-at-a-time loop (parallelised across files via rayon in
the local-copy path; sequential per file in the receiver path).

## 3. The upstream rsync I/O pattern

### 3.1 `file_checksum()` in `target/interop/upstream-src/rsync-3.4.1/checksum.c:402-539`

```c
void file_checksum(const char *fname, const STRUCT_STAT *st_p, char *sum) {
    fd = do_open_checklinks(fname);                            // open(O_RDONLY|O_NOFOLLOW)
    if (fd == -1) { memset(sum, 0, file_sum_len); return; }
    buf = map_file(fd, len, MAX_MAP_SIZE, CHUNK_SIZE);        // alloc only, no syscall
    ...
    for (i = 0; i + CHUNK_SIZE <= len; i += CHUNK_SIZE)
        EVP_DigestUpdate(evp, (uchar *)map_ptr(buf, i, CHUNK_SIZE), CHUNK_SIZE);
    remainder = (int32)(len - i);
    if (remainder > 0)
        EVP_DigestUpdate(evp, (uchar *)map_ptr(buf, i, remainder), remainder);
    EVP_DigestFinal_ex(evp, (uchar *)sum, NULL);
    close(fd);
    unmap_file(buf);                                          // free only, no syscall
}
```

Constants (`target/interop/upstream-src/rsync-3.4.1/rsync.h:158-159`):

- `CHUNK_SIZE`     = 32 KiB (= 32 * 1024) - the **digest-update chunk** the for-loop walks in
- `MAX_MAP_SIZE`   = 256 KiB (= 256 * 1024) - the **read-window size** the `map_file` machinery uses
- `CSUM_CHUNK`     = 64 bytes (`target/interop/upstream-src/rsync-3.4.1/lib/md-defines.h:23`) - MD4 only

### 3.2 `map_file()` / `map_ptr()` is NOT mmap

Despite the name, upstream's `map_file()` is a **sliding-window `read`-based
buffer**, not POSIX `mmap`. The relevant comment is at
`target/interop/upstream-src/rsync-3.4.1/fileio.c:214-217`:

```c
/* This provides functionality somewhat similar to mmap() but using read().
 * It gives sliding window access to a file.  mmap() is not used because of
 * the possibility of another program (such as a mailer) truncating the
 * file thus giving us a SIGBUS. */
```

`map_file()` itself only allocates the bookkeeping struct
(`fileio.c:218-232`). The first `map_ptr()` call sees the requested offset is
outside the window, allocates `window_size = ALIGNED_LENGTH(MAX_MAP_SIZE)` =
256 KiB, calls `read(fd, p, 256 KiB)` once, then satisfies all subsequent
in-window `map_ptr(buf, i, CHUNK_SIZE)` requests by returning a pointer into
the resident buffer. When `i` advances past the window, the next `map_ptr`
slides the window by another 256 KiB - one more `read()` - and so on
(`fileio.c:236-315`).

Important consequences of `map_file()` for the `-c` path:

- **Window stride is 256 KiB, not 32 KiB.** The 32 KiB `for`-loop step is the
  digest's update size, NOT the read size. The OS sees `read(fd, p, 256 KiB)`,
  not `read(fd, p, 32 KiB)`.
- **No `lseek`.** The window slides sequentially (`p_fd_offset` advances by
  `nread`), so the `if (map->p_fd_offset != read_start)` branch in
  `fileio.c:287` is never taken on a `-c` walk.
- **One `read()` per 256 KiB**, not per 32 KiB. So a 1 MiB file generates
  about `1024 / 256 = 4` `read()` syscalls, not 32.
- **No `mmap`/`munmap`/`madvise`.** None of these are issued.

### 3.3 `do_open_checklinks` is one syscall

`target/interop/upstream-src/rsync-3.4.1/syscall.c:669-687`: with `O_NOFOLLOW`
defined (true on every supported platform) the call collapses to
`open(pathname, O_RDONLY|O_NOFOLLOW)`. One syscall.

### 3.4 Per-file syscall trace (warm cache)

| Syscall | Count | Notes |
| --- | --- | --- |
| `open(O_RDONLY \| O_NOFOLLOW)` | 1 | |
| `read(fd, p, 256 KiB)` | `ceil(size / 262144)` | full 256 KiB window per `read`; kernel page cache typically returns the full request on a hot file |
| `close(fd)` | 1 | |
| `lseek` | 0 | window never rewinds during sequential `-c` walk |
| `mmap` / `munmap` / `madvise` | 0 | name is misleading; no real `mmap` |
| `pread`, `readv`, `preadv` | 0 | plain blocking `read` |

## 4. Per-file syscall delta by file-size bucket

`R = read()` syscall count in the rehash loop, warm-cache (no short reads).
`Other` counts the `open + close + extra-fstat` collar around the loop. Both
implementations are compared with the **default build** of each binary: pure
Rust `md-5` for oc-rsync; OpenSSL EVP `md5` for upstream Debian/Fedora/macOS
Homebrew packages (this column matters for CPU but does not change syscall
counts).

| Bucket | File size | oc-rsync receiver (`quick_check.rs`) | oc-rsync local-copy (`parallel_checksum.rs`) | upstream `file_checksum` | Delta (receiver vs upstream) |
| --- | --- | --- | --- | --- | --- |
| Tiny | 4 KiB | `1 open + 1 read + 1 close` = **3** | `1 open + 1 fstat + 1 read + 1 close` = **4** | `1 open + 1 read + 1 close` = **3** | **+0 reads, +0 total** |
| Small | 64 KiB | `1 open + 1 read + 1 close` = **3** | `1 open + 1 fstat + 1 read + 1 close` = **4** | `1 open + 1 read + 1 close` = **3** | **+0 reads, +0 total** |
| Small | 256 KiB | `1 open + 4 reads + 1 close` = **6** | `1 open + 1 fstat + 2 reads + 1 close` = **5** | `1 open + 1 read + 1 close` = **3** | **+3 reads** |
| Medium | 1 MiB | `1 open + 16 reads + 1 close` = **18** | `1 open + 1 fstat + 8 reads + 1 close` = **11** | `1 open + 4 reads + 1 close` = **6** | **+12 reads** |
| Medium | 4 MiB | `1 open + 64 reads + 1 close` = **66** | `1 open + 1 fstat + 32 reads + 1 close` = **35** | `1 open + 16 reads + 1 close` = **18** | **+48 reads** |
| Large | 16 MiB | `1 open + 256 reads + 1 close` = **258** | `1 open + 1 fstat + 128 reads + 1 close` = **131** | `1 open + 64 reads + 1 close` = **66** | **+192 reads** |
| Large | 64 MiB | `1 open + 1024 reads + 1 close` = **1026** | `1 open + 1 fstat + 512 reads + 1 close` = **515** | `1 open + 256 reads + 1 close` = **258** | **+768 reads** |
| Large | 256 MiB | `1 open + 4096 reads + 1 close` = **4098** | `1 open + 1 fstat + 2048 reads + 1 close` = **2051** | `1 open + 1024 reads + 1 close` = **1026** | **+3072 reads** |

Receiver-path overhead is **exactly 4x** upstream's `read()` count (64 KiB vs
256 KiB buffer). Local-copy-path overhead is **exactly 2x** upstream's
`read()` count plus the per-file extra `fstat` (128 KiB vs 256 KiB buffer).

### 4.1 Per-MiB syscall overhead delta

| Path | reads/MiB (oc-rsync) | reads/MiB (upstream) | extra reads/MiB |
| --- | --- | --- | --- |
| Receiver `quick_check.rs` | 16 | 4 | **+12** |
| Local-copy `parallel_checksum.rs` | 8 | 4 | **+4** |

### 4.2 Per-file syscall overhead at tree scale

Typical `--checksum` benchmark trees are 10k-100k files. For a 10k-file tree
where the average file is 1 MiB (kernel sources, build artifacts, photo
libraries), the receiver path issues:

- oc-rsync receiver:  10k * 18 = **180k syscalls** on the rehash loop
- oc-rsync local-copy: 10k * 11 = **110k syscalls**
- upstream:           10k * 6  = **60k syscalls**

Receiver path has **3x** the syscall traffic of upstream for this profile;
local-copy has **1.8x**. At 4 MiB average file size the ratios stretch to
**3.7x** and **1.9x** respectively (receiver is heavier because of the smaller
buffer).

## 5. Cost model: how much of the 1.5-1.7x perf gap is this?

A `read(2)` from the Linux page cache costs ~300-600 ns on modern x86_64
(syscall enter/exit + page-table walk + page-cache lookup). Call it 500 ns
amortised. The extra reads per MiB are:

- Receiver path: 12 reads/MiB * 500 ns = **6 microseconds/MiB** of pure
  syscall overhead
- Local-copy:     4 reads/MiB * 500 ns = **2 microseconds/MiB**

For the receiver path on a 1 MiB file, that 6 us is ~0.6% of the total hash
time when MD5 runs at ~1 GB/s (1 MiB takes ~1 ms in MD5 time alone) or ~0.3%
when MD5 runs at ~500 MB/s pure-Rust (~2 ms). At face value the syscall delta
is **small** as a fraction of the total `-c` runtime when the CPU side is
already the bottleneck.

**But** the syscall delta becomes meaningful in two cases:

1. **Hardware-accelerated digest path.** OpenSSL EVP MD5 with SHA-NI hits
   ~3 GB/s on Cooper Lake / Sapphire Rapids / Apple Silicon. At that rate a
   1 MiB hash takes ~340 us and the 6 us syscall overhead is ~1.8% per file.
   The local-copy path's 2 us delta becomes ~0.6%. Still not the dominant
   factor.
2. **Massive small-file trees.** When the average file size approaches the
   buffer size (e.g., 100k files all ~64 KiB), the per-file syscall fixed
   cost dominates over the hash time. There the **per-file** open/close
   overhead matters more than the read count.

In short: the I/O-pattern delta is **not** the primary contributor to the
1.5-1.7x `--checksum` gap. CSM-4's ranking holds: **the dominant contributor
is the pure-Rust vs OpenSSL EVP MD5 backend choice (CSM-4 item O7), then
`simd_batch` not being wired (CSM-4 item O4), then the I/O pattern audited
here**. The I/O pattern is worth fixing, but it is unlikely to close more
than ~10-20% of the gap by itself on a typical workload, and only on the
warm-cache case. On cold cache the kernel paginates either way; per-syscall
overhead is dwarfed by the disk-or-network fault.

That said, **the receiver path is the worse of oc-rsync's two `-c` paths**.
It has 2x the syscall count of the local-copy path because its buffer is
64 KiB instead of 128 KiB. The receiver path also lacks the buffer-pool
reuse: every call freshly zero-initialises a 64 KiB stack frame.

## 6. Summary findings

| Finding | Severity | Quantified |
| --- | --- | --- |
| F1 - Receiver path uses 64 KiB stack buffer vs upstream's 256 KiB window | MEDIUM | 4x reads/MiB; +12 reads/MiB; +6 us/MiB syscall overhead |
| F2 - Local-copy path uses 128 KiB pooled buffer vs upstream's 256 KiB window | LOW | 2x reads/MiB; +4 reads/MiB; +2 us/MiB syscall overhead |
| F3 - Local-copy path issues an extra `fstat` per file (`file.metadata()`) that the receiver path avoids | LOW | +1 syscall/file |
| F4 - Neither path uses real `mmap` (matches upstream, which also does not despite the misleading name) | NONE | matches upstream |
| F5 - Neither path uses `posix_fadvise(POSIX_FADV_SEQUENTIAL)` / `madvise(MADV_SEQUENTIAL)` (upstream also omits) | NONE | matches upstream |
| F6 - Neither path uses io_uring, `READ_FIXED`, or registered buffers (upstream has no io_uring either) | NONE | matches upstream |
| F7 - Neither path uses `O_NOATIME` (upstream also omits) | NONE | matches upstream |
| F8 - Receiver path zero-initialises a fresh 64 KiB stack buffer per call (no `BufferPool`) | LOW | one-off stack frame setup; trivial vs read cost |
| F9 - The standalone `checksums::parallel::files::hash_file_internal` (which does use mmap above `MMAP_THRESHOLD`) is NOT wired to either `-c` site | INFORMATIONAL | already noted in CSM-4 item O5; should not be wired up blindly because upstream avoids `mmap` deliberately for SIGBUS safety |

## 7. Confirmed vs refuted vs new

| CSM-4 claim | Status after CSM-5 |
| --- | --- |
| O1: "Extra `read()` per chunk vs zero syscalls after `mmap`; userspace bounce buffer vs direct slice into mapped pages" | **Partially refuted**. Upstream does NOT use `mmap`. It also calls `read()` per window; the window is just larger (256 KiB vs 64 KiB / 128 KiB). The "zero syscalls after mmap" framing was incorrect; upstream is `4x` ahead on reads, not `infinity x`. |
| O1 severity: HIGH | **Downgrade to MEDIUM**. Quantified at ~6 us/MiB receiver overhead, ~2 us/MiB local-copy overhead. Material on warm cache with hardware-accelerated digests; near-noise on cold cache or pure-Rust MD5. |
| O5: "mmap not used by receiver path" | **Confirmed**, but **the implication that wiring mmap up would help is wrong**: upstream itself does not use mmap; doing so would diverge from upstream's deliberate SIGBUS-avoidance choice and would need its own audit. The right fix is to grow the read window, not to introduce mmap. |

## 8. Next steps

CSM-7 (synthesis) should pick from:

- **CSM-7-A**: grow `quick_check.rs::file_checksum_matches`'s buffer from
  64 KiB to 256 KiB to match upstream's window. Use a `BufferPool` or a
  page-aligned heap buffer rather than the stack to avoid a 256 KiB stack
  frame. This is a one-line change inside `file_checksum_matches` plus a
  reach into `engine::local_copy::buffer_pool` (or a new local pool inside
  `transfer`). Expected gain: receiver `-c` walks ~3x fewer syscalls per
  MiB, recovering ~4 us/MiB of overhead. Wire-compatible, no protocol
  change.
- **CSM-7-B**: grow `parallel_checksum.rs::hash_file_contents`'s buffer from
  128 KiB to 256 KiB by passing `buffer_size: 256 * 1024` through
  `GlobalBufferPoolConfig` when checksum mode is active, or by allocating a
  one-off 256 KiB pool inside the checksum-prefetch caller. Expected gain:
  local-copy `-c` walks 2x fewer syscalls per MiB, recovering ~2 us/MiB.
- **CSM-7-C**: drop the per-file `file.metadata()` `fstat` in
  `compute_file_checksum:139` by passing the already-known size in via
  `FilePair::source_size`/`destination_size` (the caller already has both).
  Trivial change, saves 1 syscall/file.
- **Defer**: introducing real `mmap` for `-c`. Upstream avoided it for
  SIGBUS safety; matching upstream is the cheaper move. If a future audit
  argues for mmap, it must compare against upstream's stated reasoning, not
  just against a `read()` baseline.
- **Defer**: io_uring for `-c`. Upstream is single-threaded blocking on this
  path; a per-file `READ_FIXED` chain would help only if many files were
  hashed in parallel without already being parallelised across CPU cores
  (the local-copy path is already rayon-parallel; the receiver path is
  not). Bigger win than this would come from finishing CSM-4's item O4
  (`simd_batch` wiring).

CSM-8 (fix) should implement at minimum CSM-7-A. CSM-7-B and CSM-7-C are
small and can ride along.

## 9. References

### oc-rsync

- `crates/transfer/src/receiver/quick_check.rs:46-89` - `quick_check_matches` dispatcher
- `crates/transfer/src/receiver/quick_check.rs:262-287` - `file_checksum_matches` (the receiver `-c` rehash)
- `crates/transfer/src/receiver/transfer/candidates.rs:128-206` - `-c` decision site
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:88-130` - `prefetch_checksums` (parallel dispatcher)
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:133-145` - `compute_file_checksum` (per-file)
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148-242` - `hash_file_contents` (the local-copy `-c` rehash)
- `crates/engine/src/local_copy/executor/directory/recursive/checksum.rs:38-110` - local-copy `-c` wiring
- `crates/engine/src/local_copy/buffer_pool/global.rs:73-88` - `global_buffer_pool` default config
- `crates/engine/src/local_copy/mod.rs:156` - `COPY_BUFFER_SIZE = 128 KiB`
- `crates/checksums/src/parallel/files.rs:22-73` - the standalone `hash_file_internal` (mmap-capable, not wired to `-c`)
- `crates/fast_io/src/mmap_reader.rs:33` - `MMAP_THRESHOLD = 64 KiB`

### Upstream rsync 3.4.1

- `target/interop/upstream-src/rsync-3.4.1/checksum.c:402-539` - `file_checksum`
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:214-232` - `map_file` (alloc only)
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:236-315` - `map_ptr` (sliding `read()` window)
- `target/interop/upstream-src/rsync-3.4.1/syscall.c:669-687` - `do_open_nofollow`
- `target/interop/upstream-src/rsync-3.4.1/syscall.c:804-810` - `do_open_checklinks`
- `target/interop/upstream-src/rsync-3.4.1/generator.c:617-630` - `quick_check_ok` (the `-c` decision site)
- `target/interop/upstream-src/rsync-3.4.1/flist.c:1412-1413` - sender-side `file_checksum` call
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:158-159` - `CHUNK_SIZE = 32 KiB`, `MAX_MAP_SIZE = 256 KiB`
- `target/interop/upstream-src/rsync-3.4.1/lib/md-defines.h:23` - `CSUM_CHUNK = 64`

### Related audits

- `docs/audits/csm-4-strong-checksum-upstream-parity.md` - parent algorithm-parity audit
- `docs/audits/checksum-mode-computation-cost.md` - CSP profiling audit
- `docs/audits/checksum-io-pattern-vs-upstream.md` - earlier I/O audit (pre-CSM-tracked)
