# WPG-9 - registered-buffer equivalent on Windows (file side)

Audit-only design for the P0 headline gap surfaced by WPG-7.c row 1:
`IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` have no direct Win32
peer for **file** handles. RIO covers sockets only. This document
catalogues the Linux primitive, the Windows status quo, the four
mechanical workarounds available without inventing new kernel APIs, and
the recommended sequencing.

Inputs:

- WPG-7.a opcode inventory (`docs/design/wpg-7-iouring-opcode-inventory.md`).
- WPG-7.b io_uring -> IOCP mapping (`docs/design/wpg-7b-iouring-iocp-mapping.md`).
- WPG-7.c prioritised IOCP gap list (`docs/design/wpg-7c-iocp-gap-list.md`).
- Existing crate surfaces:
  - `crates/fast_io/src/io_uring/registered_buffers/` - Linux side.
  - `crates/fast_io/src/iocp/` - Windows file I/O today.
  - `crates/fast_io/src/page_aligned.rs` - cross-platform page-aligned buffer.
  - `crates/engine/src/local_copy/buffer_pool/` - userspace pool that
    feeds both backends.

No source changes are made by this task. WPG-9.a/b/c implementation
sub-tasks are scoped at the end.

## 1. What `READ_FIXED` / `WRITE_FIXED` actually do on Linux

The Linux registered-buffer protocol has two phases - a one-time
registration and a per-op submission that references the registration
by integer index.

### 1.1 Registration phase

`io_uring_register(ring_fd, IORING_REGISTER_BUFFERS, iovecs, n)` hands
the kernel an `iovec` array. For each entry:

1. The kernel walks the virtual range and calls `get_user_pages()` to
   pin every backing page in physical memory.
2. It builds a `struct io_mapped_ubuf` that caches the pinned pages,
   their `bvec` representation, and the user-visible iov base/len.
3. The mapping lives in `ctx->user_bufs[index]` until either an explicit
   `IORING_UNREGISTER_BUFFERS` or the ring fd is closed
   (`fs/io_uring.c:io_sqe_buffers_unregister`).

The pinning is the structural cost: `get_user_pages()` is the per-byte
expense io_uring is engineered to amortise across many ops. See
`crates/fast_io/src/io_uring/registered_buffers/mod.rs` lines 1-89 for
the in-crate description of the contract.

### 1.2 Per-op submission

`IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` SQEs carry a 16-bit
**buffer index** (`sqe->buf_index`) and a user-visible pointer + length
that must lie inside the registered range. The kernel:

1. Looks up the cached `io_mapped_ubuf` by index in O(1).
2. Builds an `iov_iter` directly over the pre-pinned `bvec`s. No
   `get_user_pages()` runs, no `MDL` is allocated, no per-op refcount
   update on user pages.
3. Hands the iter to the filesystem read/write path exactly as a
   `READ` / `WRITE` would.

Concrete call sites:
- `crates/fast_io/src/io_uring/registered_buffers/submit.rs:62`
  (`ReadFixed::new(fd, slot.ptr, want as u32, slot.buf_index)`).
- `crates/fast_io/src/io_uring/registered_buffers/submit.rs:194`
  (`WriteFixed::new(...)`).

### 1.3 Where the win comes from

The per-op cost the registered path eliminates is dominated by
`get_user_pages_fast()` (which still has to walk the page tables and
take page refcounts on the unregistered path), plus the small but real
allocation of the per-op `bio_vec` / `iov_iter`. Published kernel
measurements put the saving at roughly **5-15 percent of CPU time per
4 KiB op** under high IOPS, with the proportional saving growing as
the syscall surface shrinks (the rest of the wins of io_uring -
SQPOLL, batched completions - magnify it). The win is largest when
the **same buffers are reused across many ops**, which is precisely
the buffer-pool pattern this crate uses everywhere.

### 1.4 Slot pool sizing in this crate

`MAX_REGISTERED_BUFFERS` is capped at 1024
(`crates/fast_io/src/io_uring/registered_buffers/mod.rs:88`). The
`RegisteredBufferGroup` allocates page-aligned slabs via
`alloc::alloc_zeroed(layout)` with a page-multiple length, then hands
the pointer array to `register_buffers` in one call
(`registry.rs:189-216`). Slots are checked out lock-free via an atomic
free-bitset (`registry.rs:325-352`); a slot miss falls back to plain
`READ` / `WRITE` SQEs (the "Conditional" classification in WPG-7.a
lines 81-82).

## 2. Windows status today (the structural gap)

### 2.1 Per-op page locking is unavoidable on the file path

Win32's overlapped `ReadFile` / `WriteFile` against a handle opened
with `FILE_FLAG_OVERLAPPED` work like this:

1. The Win32 wrapper invokes `NtReadFile` / `NtWriteFile` with the
   user buffer pointer.
2. The I/O manager constructs an **IRP** (I/O Request Packet) and a
   **Memory Descriptor List (MDL)** that pins the user buffer for the
   duration of the request via `MmProbeAndLockPages`.
3. The IRP is queued to the storage stack; on completion the MDL is
   unlocked and `IoCompleteRequest` posts the result to the
   associated completion port.

The MDL build + page-lock in step 2 is the Windows analogue of
`get_user_pages()`, and it runs **once per overlapped submission**.
There is no Win32 API that says "pre-pin this buffer once and reference
it by index in subsequent ReadFile / WriteFile calls" for file
handles.

Today's call sites that pay this cost on every op:
- `crates/fast_io/src/iocp/file_reader.rs:103-111` (single-shot read).
- `crates/fast_io/src/iocp/file_reader.rs:190-216` (batched read; the
  batch amortises completion drain but not per-op pinning).
- `crates/fast_io/src/iocp/file_writer.rs:170-179` (write).
- `crates/fast_io/src/iocp/disk_batch/writer.rs:243-252` (the batched
  disk writer; one `WriteFile` per chunk, one MDL build per chunk).

### 2.2 RIO is socket-only

Winsock Registered I/O (`RIORegisterBuffer`, `RIOSend`, `RIOReceive`,
`RIO_BUF`) is the closest conceptual peer of `READ_FIXED` /
`WRITE_FIXED` - it pre-pins a buffer and lets subsequent calls
reference it by `RIO_BUFFERID` + offset + length. However, RIO is
implemented inside `ws2_32.dll` against socket handles only; the
underlying RIO functions are obtained via
`WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER, WSAID_MULTIPLE_RIO, ...)`
and dispatch through `afd.sys` / `tcpip.sys`. They cannot be invoked
against a `HANDLE` returned by `CreateFileW`.

This is the WPG-7.b row that drives this document
(`docs/design/wpg-7b-iouring-iocp-mapping.md:45-46, 65-66, 81-82`).

### 2.3 What this costs in practice

The IRP + MDL build is roughly **2-5 microseconds of kernel time per
overlapped op** on modern x86 hardware (the dominant cost is the page
walk plus the `MmProbeAndLockPages` IPI on multi-socket boxes). At 64
KiB chunks and a 32-deep submission window that is **~40 microseconds
of kernel time per 2 MiB transferred**, or roughly **2 percent of a
CPU at 1 GiB/s**. The fraction grows linearly as chunk size shrinks;
at 4 KiB it dominates.

## 3. Workaround options

Five mechanical options exist within Win32 today. None of them is a
direct peer of `READ_FIXED`; each removes a different component of the
per-op cost.

### 3.a Status quo - accept the cost

Keep the current `WriteFile` / `ReadFile` dispatch unchanged. Document
the per-op pinning delta in the IOCP design notes and stop. Cheapest
option in implementation cost; zero change to the buffer pool surface.

- **Pros**: nothing to write, nothing to test, nothing to ship.
- **Cons**: the file-side data path will always be 2-15% slower than
  the Linux io_uring path for IOPS-bound workloads. The gap widens as
  chunk size shrinks.
- **Implementation effort**: zero.
- **Performance win**: zero.

### 3.b Locked-memory buffer pool (`VirtualLock`)

Use `VirtualLock(addr, len)` on every buffer the pool hands out. The
documented effect is to lock the range into the **process working
set**, preventing the pages from being paged out and (importantly for
us) keeping them resident so the per-op MDL build does not have to
fault them in. `VirtualLock` does **not** eliminate the MDL
construction itself, but it removes the page-fault + pageout-races
component of `MmProbeAndLockPages`.

Per Microsoft docs (`learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtuallock`):

- The locked range must be a subset of a region committed via
  `VirtualAlloc(MEM_COMMIT)`. Our `PageAlignedBuffer` already uses
  `VirtualAlloc(MEM_COMMIT | MEM_RESERVE)` on Windows
  (`crates/fast_io/src/page_aligned.rs:142-164`), so the precondition
  is satisfied.
- The process must call `SetProcessWorkingSetSizeEx` to raise its
  minimum working set if the locked total exceeds the per-process
  default minimum (~250 pages, i.e. ~1 MiB on a 4 KiB system).
- No special privilege is required for ranges up to the per-process
  working-set minimum; above that the process needs
  `SE_LOCK_MEMORY_NAME` (`SeLockMemoryPrivilege`), which is **not**
  granted to standard users.

This is the same `VirtualLock` pattern documented for the SQM
SQPOLL+mmap workaround spec (`docs/design/sqm-1c-workaround-spec.md`
line 489).

- **Pros**: small, mechanical, no kernel dependency, no driver
  install, no admin requirement for typical pool sizes.
- **Cons**: removes only the page-fault tail of the per-op cost, not
  the MDL build. Requires bumping the process working-set minimum on
  startup. Above ~1 MiB total locked the user needs
  `SeLockMemoryPrivilege` - in practice that limits us to roughly
  **16 buffers of 64 KiB each** without privilege escalation.
- **Implementation effort**: **M** (one Windows-only wrapper + Drop
  unlocker + working-set probe + capability test).
- **Performance win**: **modest** (5-10% throughput on IOPS-bound
  workloads; near-zero on bandwidth-bound 1 MiB+ transfers).

### 3.c Direct I/O (`FILE_FLAG_NO_BUFFERING`)

Open the handle with `FILE_FLAG_NO_BUFFERING` so the system cache is
bypassed entirely. Read/write goes straight to the storage stack with
no cache copy. Buffers and offsets must be sector-aligned (4 KiB on
NTFS by default, queryable via `GetDiskFreeSpaceW`).

This path is **already implemented** in the codebase via
`IocpConfig::unbuffered` and the page-aligned buffer pool:

- `crates/fast_io/src/iocp/disk_batch/writer.rs:39-46` reopens handles
  with `FILE_FLAG_NO_BUFFERING` when `config.unbuffered`.
- `crates/fast_io/src/page_aligned.rs:108-196` provides
  `PageAlignedBuffer` backed by `VirtualAlloc` on Windows.
- `crates/engine/src/local_copy/buffer_pool/page_aligned.rs` exposes
  `PageAlignedBufferPool` with a lock-free reservoir.
- `BOUNCE_COPIES_AVOIDED`
  (`crates/fast_io/src/iocp/disk_batch/writer.rs:241`) already counts
  the saved bounce copies.

The MDL build still runs per op, but the **cache copy** that would
otherwise dominate large sequential I/O is gone. This is the largest
single win available without changing the kernel API surface, and the
crate already ships it - this row is included for completeness.

- **Pros**: shipped; the largest perf win for big sequential
  transfers; no privilege requirement; works on all NTFS volumes.
- **Cons**: requires sector-aligned buffers and offsets - small
  reads/writes that are not a multiple of the sector size fail with
  `ERROR_INVALID_PARAMETER`. The fallback path (regular `WriteFile`)
  is needed for non-aligned ops.
- **Implementation effort**: **L** historically; already done.
- **Performance win**: **large** for >=64 KiB sequential ops; zero or
  negative for small / unaligned ops.

### 3.d `ReadFileScatter` / `WriteFileGather`

Vectored I/O for page-aligned buffers. Submits a single overlapped op
that scatters reads (or gathers writes) across an array of page-sized
buffer pointers. The kernel still locks each page per op, but the IRP
+ completion overhead is amortised across N pages instead of paid per
page.

- Requires `FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED` on the
  handle, page-aligned buffer pointers, and a sector-aligned offset.
- Each scatter/gather element must be exactly one page. The buffer
  array is terminated by a NULL pointer entry.

The natural integration point is `submit_one_write` in
`crates/fast_io/src/iocp/disk_batch/writer.rs:210-269` - replace the
per-chunk `WriteFile` with `WriteFileGather` of `chunk_size /
page_size` page pointers.

- **Pros**: cuts IRP and OVERLAPPED bookkeeping by N. Stacks
  multiplicatively with 3.c. No new privilege requirement.
- **Cons**: only legal on `FILE_FLAG_NO_BUFFERING` handles; tail
  fragments still need a separate fallback. The kernel still builds
  per-page MDLs internally, so the per-page pinning cost is unchanged.
- **Implementation effort**: **M** (new submit path inside
  `disk_batch::writer`; needs page-list bookkeeping).
- **Performance win**: **modest** for current 64 KiB chunk size (~16
  pages per op = ~16x amortisation of IRP overhead, but IRP is a
  small fraction of the cost). Larger for 1 MiB+ chunks.

### 3.e IOCP + completion-port batching tuning

Increase the in-flight submission window and the
`GetQueuedCompletionStatusEx` drain depth so each kernel transition
processes more completions. The submission window is already tunable
via `IocpConfig::concurrent_ops` (auto-sized to `cpus * 4` between 8
and 64; `crates/fast_io/src/iocp/config.rs:31-74`); the drain batch
is in `DEFAULT_BATCH_SIZE = 64`
(`crates/fast_io/src/iocp/pump.rs`).

This does not eliminate the per-op MDL build - it amortises the
*completion* delivery cost. Listed for completeness; the depth is
already at a reasonable value.

- **Pros**: small, mechanical, already partly applied.
- **Cons**: addresses a different bottleneck (completion drain, not
  per-op pinning). Diminishing returns beyond ~32 in-flight ops.
- **Implementation effort**: **S** (one constant + a benchmark).
- **Performance win**: **small** (5% on completion-bound paths only).

## 4. Recommendation

Sequence: **3.c is shipped**, **add 3.b**, **layer 3.d on top**,
**defer 3.a in writing**, **skip 3.e**.

### 4.1 Cost / win matrix

| Option | Effort | Per-op MDL eliminated? | Throughput delta (1 MiB ops) | Throughput delta (64 KiB ops) | Status |
|---|---|---|---|---|---|
| 3.a status quo | 0 | No | 0 | 0 | always available |
| 3.b VirtualLock pool | M | Partially (no page-fault tail) | +0-2% | +5-10% | proposed (WPG-9.a) |
| 3.c FILE_FLAG_NO_BUFFERING | L | No (cache copy eliminated) | +30-60% | +10-25% | already shipped |
| 3.d ReadFileScatter / WriteFileGather | M | No (IRP amortised) | +2-5% | +5-10% | proposed (WPG-9.b) |
| 3.e larger IOCP drain batch | S | No (completion drain only) | +0-5% | +0-5% | partly applied |

### 4.2 Why 3.b first

`VirtualLock`'d slabs are a pure additive enhancement to the existing
`PageAlignedBufferPool` (`crates/engine/src/local_copy/buffer_pool/page_aligned.rs`).
The change is local: a new `WindowsLockedBufferPool` (or a feature on
`PageAlignedBufferPool`) that calls `VirtualLock` after `VirtualAlloc`
and `VirtualUnlock` before `VirtualFree`. No call-site refactor on the
IOCP side; the writer keeps handing the same `*mut u8` to `WriteFile`.
The privilege test is a one-shot probe at pool construction; failure
falls back to the unlocked pool transparently.

### 4.3 Why 3.d second

`WriteFileGather` requires changing the submit shape inside
`crates/fast_io/src/iocp/disk_batch/writer.rs::submit_one_write`. The
chunk is split into a page array, the OVERLAPPED carries a page-list
pointer instead of a single buffer pointer, and the resubmit-on-short-
write path needs to advance through pages instead of bytes. Moderate
code surface, but it stacks with 3.c on the unbuffered handle the
disk batcher already opens.

### 4.4 Why 3.e is deferred

The drain batch is already at 64; the submission window auto-sizes
between 8 and 64. Both match the io_uring SQ depth and the io_uring
CQE batch sizing (`MAX_CONCURRENT_OPS = 64`,
`COMPLETION_DRAIN_BATCH = 64`). Further tuning would be a benchmark
exercise, not a structural fix.

## 5. Implementation surface

### 5.1 Where the buffer pool lives today

Two coexisting pools, both consumed by the IOCP writer:

- **`engine::local_copy::buffer_pool::BufferPool`**
  (`crates/engine/src/local_copy/buffer_pool/pool.rs`) - the
  general-purpose `Vec<u8>` pool with a two-level (thread-local +
  central ArrayQueue) cache. Used everywhere the I/O does not need
  sector alignment.
- **`engine::local_copy::buffer_pool::PageAlignedBufferPool`**
  (`crates/engine/src/local_copy/buffer_pool/page_aligned.rs`) - a
  lock-free pool of `fast_io::PageAlignedBuffer` instances backed by
  `VirtualAlloc` on Windows. Used by the
  `FILE_FLAG_NO_BUFFERING` path in `iocp::disk_batch`.

The `fast_io` side exposes the raw building block:
- **`fast_io::PageAlignedBuffer`**
  (`crates/fast_io/src/page_aligned.rs:108-196`) - a single
  page-aligned heap allocation; `VirtualAlloc(MEM_COMMIT |
  MEM_RESERVE, PAGE_READWRITE)` on Windows, `alloc_zeroed` with a
  page-aligned `Layout` elsewhere.

### 5.2 What changes for 3.b

Add a Windows-only **`WindowsLockedBufferPool`** alongside
`PageAlignedBufferPool`, with the same lock-free reservoir shape but
with the following additions:

1. **Per-buffer lock on allocate.** After `PageAlignedBuffer::new`,
   call `VirtualLock(ptr, capacity)`. On failure (privilege denied,
   working-set minimum exceeded), record the error and fall back to
   the unlocked pool transparently - the pool advertises "best-effort
   locking" rather than "guaranteed locked".
2. **Per-buffer unlock on Drop.** Before `VirtualFree`, call
   `VirtualUnlock(ptr, capacity)`. The current Drop in
   `PageAlignedBuffer` (lines 242-269) becomes a small refactor: the
   Windows arm gets an `unlock_then_free` helper. Unlocking is
   idempotent on already-unlocked memory.
3. **Working-set minimum bump.** Once per process, call
   `SetProcessWorkingSetSizeEx(GetCurrentProcess(),
   min_working_set, max_working_set, QUOTA_LIMITS_HARDWS_MIN_ENABLE)`
   to raise the minimum working set high enough for the configured
   pool size. Compute `min_working_set` as
   `slot_count * round_up_to_page(buffer_size) + 1 MiB` (margin for
   the rest of the process).
4. **Capability detection.** A one-shot probe at pool construction:
   try `VirtualLock` on a single buffer; if it fails with
   `ERROR_WORKING_SET_QUOTA` (1453), the pool downgrades to unlocked
   mode and surfaces a structured warning. If it fails with
   `ERROR_PRIVILEGE_NOT_HELD` (1314) above the working-set ceiling,
   same downgrade.

The IOCP writer code path does not change. The pool's slot pointers
are still raw `*mut u8` page-aligned addresses; `WriteFile` /
`ReadFile` accept them unchanged.

### 5.3 What changes for 3.d

Add a `submit_one_write_gather` peer in
`crates/fast_io/src/iocp/disk_batch/writer.rs` that:

1. Splits the input chunk into page-sized pointers (one per page;
   relies on `PageAlignedBufferPool` for the source buffer).
2. Builds the `FILE_SEGMENT_ELEMENT[]` array (one entry per page, NUL
   terminator at the end).
3. Calls `WriteFileGather(handle, segments_ptr, total_bytes,
   reserved=NULL, &overlapped)`.
4. On short completion, advances the per-page tail and resubmits the
   remaining pages.

Selection rule: prefer `WriteFileGather` when
`config.unbuffered && chunk_size >= 2 * page_size`; otherwise stay on
`WriteFile`. The 2-page floor avoids the corner case where the
overhead of the segment-array build outweighs the saved IRP cost.

### 5.4 Capability detection summary

| Capability | API | Cost | Probe cache |
|---|---|---|---|
| IOCP available | `CreateIoCompletionPort` on a junk handle | one call | `IOCP_STATUS` `AtomicU8` in `iocp::config` |
| `FILE_FLAG_NO_BUFFERING` works on volume | open + close a test handle | one syscall pair | already cached via `IocpConfig::unbuffered` toggle |
| `VirtualLock` works for pool size | `VirtualLock` on first buffer | one call | new `WINDOWS_LOCK_STATUS` `AtomicU8` |
| Working-set min is sufficient | `SetProcessWorkingSetSizeEx` return | one call at startup | best-effort; failure -> downgrade |
| `WriteFileGather` available | static (Vista+) | none | none needed |

### 5.5 Feature-gate naming

Per WPG-9 specification:

- **`iocp-locked-buffer-pool`** - default on Windows; no-op
  elsewhere. Controls whether `WindowsLockedBufferPool` is built; the
  selection between locked and unlocked at runtime stays based on the
  `VirtualLock` capability probe.
- **`iocp-gather-write`** - default on Windows; no-op elsewhere.
  Controls whether `submit_one_write_gather` is compiled. Runtime
  selection between scalar and gather submission is governed by
  `config.unbuffered` plus the 2-page chunk floor described in 5.3.

Both feature names follow the existing crate convention of
`iouring-*` for io_uring-specific code paths
(`iouring-send-zc`, etc.).

## 6. Performance projections

Numbers below are paper estimates anchored to the published kernel
costs and to existing in-tree benchmarks. Real measurements are
WPG-9.c.

### 6.1 Per-op kernel-time savings (best case, 64 KiB op)

| Path | MDL build | Page lock | Cache copy | Notes |
|---|---|---|---|---|
| Today: `WriteFile` buffered | ~1 us | ~2-4 us | ~5-10 us | dominated by cache copy |
| 3.c `WriteFile` `NO_BUFFERING` (shipped) | ~1 us | ~2-4 us | 0 | bypasses cache; needs aligned buffer |
| 3.b + 3.c: locked aligned pool | ~1 us | ~0.5-1 us | 0 | working set already resident, MDL build still runs |
| 3.b + 3.c + 3.d: gather of N pages | ~1 us / N | ~0.5-1 us | 0 | IRP amortised across pages |
| Linux baseline: `WRITE_FIXED` | 0 | 0 | 0 | direct iov_iter over registered bvec |

### 6.2 Throughput delta projections

Assume 32-deep submission window, NVMe-class storage (~3 GiB/s),
sequential 1 GiB transfer.

| Op size | Today (3.c only) | + 3.b | + 3.b + 3.d | Linux `WRITE_FIXED` |
|---|---|---|---|---|
| 4 KiB | ~0.4 GiB/s | ~0.45 GiB/s | ~0.55 GiB/s | ~0.7 GiB/s |
| 64 KiB | ~2.2 GiB/s | ~2.4 GiB/s | ~2.5 GiB/s | ~2.7 GiB/s |
| 1 MiB | ~2.9 GiB/s | ~2.95 GiB/s | ~2.95 GiB/s | ~3.0 GiB/s |
| 10 MiB | ~3.0 GiB/s | ~3.0 GiB/s | ~3.0 GiB/s | ~3.0 GiB/s |
| 100 MiB | ~3.0 GiB/s | ~3.0 GiB/s | ~3.0 GiB/s | ~3.0 GiB/s |

The gap is structurally largest at small op sizes and vanishes at
bandwidth-bound op sizes. Implementing 3.b + 3.d closes roughly **two
thirds of the remaining gap** at 4 KiB and effectively closes it at
64 KiB and above.

### 6.3 Comparison vs Linux `READ_FIXED` baseline

For oc-rsync's typical 128 KiB - 1 MiB chunk sizes (adaptive buffer
sizing, `crates/engine/src/local_copy/buffer_pool/mod.rs:130-156`),
the projected post-WPG-9 delta vs Linux is **<3% on bandwidth-bound
workloads and <10% on metadata-heavy workloads**. The remaining gap
is the unavoidable MDL build; closing it would require a Microsoft
API change.

## 7. Test plan

### 7.1 Cross-platform throughput benchmark

A new benchmark wired into `scripts/benchmark_hyperfine.sh` (or a
peer):

1. **Setup**: 1 GiB random file on tmpfs (Linux) and on the system
   drive (Windows). Source and destination on the same host to
   isolate I/O from network.
2. **Cases**:
   - Linux io_uring with `READ_FIXED` / `WRITE_FIXED` (existing).
   - Windows IOCP today (`WriteFile` buffered).
   - Windows IOCP + 3.c (`NO_BUFFERING`; already shipped).
   - Windows IOCP + 3.b + 3.c (locked pool + `NO_BUFFERING`).
   - Windows IOCP + 3.b + 3.c + 3.d (full stack).
3. **Metric**: wall time + bytes per second, three runs averaged.
4. **Acceptance**: 3.b + 3.c + 3.d closes the Linux gap to within
   10% at 128 KiB chunks and within 5% at 1 MiB chunks.

### 7.2 Regression test - no functional change vs current path

The existing `iocp_disk_full_simulation.rs` and the IOCP
`disk_batch::tests` cover write semantics, short writes, fault
injection, and disposition. New tests for WPG-9:

- `windows_locked_pool_unlocks_on_drop`: allocate, lock, drop,
  re-allocate at the same address (best-effort) and assert no
  `VirtualUnlock` `ERROR_NOT_LOCKED` (158) on the second drop.
- `windows_locked_pool_downgrades_on_quota`: deliberately request a
  pool that exceeds the per-process working-set minimum without
  privilege; assert the pool downgrades silently and writes still
  succeed.
- `write_file_gather_short_completion`: inject a short completion
  on the middle page; assert the resubmit advances by whole pages
  and the final byte count matches.

### 7.3 Memory safety - unlock-on-panic

`VirtualLock`'d ranges must be `VirtualUnlock`'d before
`VirtualFree`. The Drop impl on `PageAlignedBuffer` must run the
unlock first; a panic between `VirtualLock` and the returned
`PageAlignedBuffer` (e.g., the `assert!` on null `ptr` already in
the constructor) must not leak a locked range. The constructor
ordering becomes: `VirtualAlloc` -> `assert!` -> `VirtualLock` ->
return `Self`. `VirtualLock` failure is non-fatal and downgrades
the buffer to unlocked (the Drop still calls `VirtualUnlock` but
ignores `ERROR_NOT_LOCKED`).

Test: a `#[should_panic]` regression that wraps allocation in a
panic-checked closure and asserts no resident-memory leak via
`GetProcessWorkingSetSize` before/after.

## 8. Follow-up tasks

- **WPG-9.a** - `WindowsLockedBufferPool` implementation. Scope: the
  pool type, the `VirtualLock` / `VirtualUnlock` lifecycle, the
  capability probe, the working-set bump, and the unit tests from
  7.2. Feature gate: `iocp-locked-buffer-pool`. Acceptance: pool
  passes the regression suite on Windows; no-op on other platforms.
- **WPG-9.b** - `WriteFileGather` / `ReadFileScatter` submit path
  inside `iocp::disk_batch::writer`. Scope: the new submit helper,
  the page-list builder, the short-completion resubmit logic, and
  the tests from 7.2. Feature gate: `iocp-gather-write`.
  Acceptance: gather path engages on unbuffered handles with chunks
  >= 2 pages; bench shows >=5% throughput uplift at 64 KiB chunks.
- **WPG-9.c** - cross-platform throughput benchmark + acceptance
  matrix. Scope: the script from 7.1 plus a results doc parked at
  `docs/audits/wpg-9-windows-file-io-perf.md`. Acceptance: results
  uploaded as a CI artifact on the Windows job; the matrix lands in
  the release notes for the version that ships WPG-9.b.

## 9. Cross-references

- WPG-7.a opcode inventory: `docs/design/wpg-7-iouring-opcode-inventory.md`
  (lines 38-39 for `READ_FIXED` / `WRITE_FIXED`, 61-62 for the
  `IORING_REGISTER_BUFFERS` / `IORING_UNREGISTER_BUFFERS` pair).
- WPG-7.b mapping: `docs/design/wpg-7b-iouring-iocp-mapping.md`
  (lines 45-46 for the file-side gap, 65-66 for the RIO socket peer,
  113-120 for the cross-reference to this doc).
- WPG-7.c gap list: `docs/design/wpg-7c-iocp-gap-list.md`
  (row 1 of the prioritised table; rationale in lines 41-48).
- SQM SQPOLL+mmap workaround spec
  `docs/design/sqm-1c-workaround-spec.md` (line 489 - the
  cross-platform `VirtualLock` analogue note used in 3.b).
- io_uring registered-buffer crate docs:
  `crates/fast_io/src/io_uring/registered_buffers/mod.rs` (lines
  1-89 - lifecycle and Drop-order invariants).
- Linux call sites for the registered-buffer fast path:
  `crates/fast_io/src/io_uring/registered_buffers/submit.rs` (lines
  29-152 `submit_read_fixed_batch`, 159-243 `submit_write_fixed_batch`).
- IOCP file-I/O surfaces today:
  `crates/fast_io/src/iocp/file_reader.rs` (lines 88-159 single-shot,
  161-258 batched), `crates/fast_io/src/iocp/file_writer.rs` (lines
  158-219), `crates/fast_io/src/iocp/disk_batch/writer.rs` (lines
  96-269).
- Page-aligned buffer + pool:
  `crates/fast_io/src/page_aligned.rs` (lines 108-269),
  `crates/engine/src/local_copy/buffer_pool/page_aligned.rs` (the
  lock-free `PageAlignedBufferPool`).
