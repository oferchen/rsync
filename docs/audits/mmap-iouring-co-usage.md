# mmap + io_uring co-usage audit

Task: #1660. Branch: `docs/mmap-iouring-audit`.

## Scope

Audit every site in `crates/fast_io/`, `crates/engine/`, `crates/match/` (and
their immediate transfer-side callers in `crates/transfer/` and
`crates/checksums/`) where memory obtained from `mmap(2)` is - or could be -
handed to `io_uring` for read or write submission. The audit answers four
questions per site:

1. Is the address range backed by a file mapping (`memmap2::Mmap` /
   `MmapMut`) or by a heap allocation?
2. Does it cross the boundary into an `io_uring` SQE (kernel pinning via
   `get_user_pages`, registered-buffer copy, or SQPOLL kernel-thread
   reference)?
3. What can go wrong: page-fault stall under SQPOLL, `SIGBUS` on
   mid-transfer truncate, ordering between the ring fd and the buffer
   lifetime, registered-buffer-id mismatch?
4. Is the code path actually reachable in production today, or is it
   dormant (exported but unwired)?

Source files inspected (all paths repository-relative):

- `crates/fast_io/src/lib.rs`
- `crates/fast_io/src/mmap_reader.rs`
- `crates/fast_io/src/io_uring/mod.rs`
- `crates/fast_io/src/io_uring/file_writer.rs`
- `crates/fast_io/src/io_uring/file_reader.rs`
- `crates/fast_io/src/io_uring/disk_batch.rs`
- `crates/fast_io/src/io_uring/batching.rs`
- `crates/fast_io/src/io_uring/registered_buffers.rs`
- `crates/fast_io/src/io_uring/buffer_ring.rs`
- `crates/fast_io/src/io_uring/socket_writer.rs`
- `crates/transfer/src/map_file/mod.rs`
- `crates/transfer/src/map_file/buffered.rs`
- `crates/transfer/src/map_file/mmap.rs`
- `crates/transfer/src/map_file/adaptive.rs`
- `crates/transfer/src/map_file/wrapper.rs`
- `crates/transfer/src/transfer_ops/response.rs`
- `crates/transfer/src/transfer_ops/streaming.rs`
- `crates/transfer/src/transfer_ops/token_loop.rs`
- `crates/transfer/src/disk_commit/thread.rs`
- `crates/transfer/src/delta_apply/applicator.rs`
- `crates/transfer/src/generator/mod.rs`
- `crates/checksums/src/parallel/files.rs`
- `crates/engine/src/lib.rs`
- `crates/engine/src/local_copy/context_impl/transfer.rs`
- `crates/match/` (no mmap or io_uring usage; cross-checked)

Upstream references consulted (per project rules - upstream C is the source
of truth):

- `target/interop/upstream-src/rsync-3.4.1/fileio.c` lines 214-217
  (`map_file()` doc comment), 219-315 (`map_file` / `map_ptr` / `unmap_file`
  bodies), 304-307 (zero-on-short-read recovery).
- `target/interop/upstream-src/rsync-3.4.1/sender.c`
  (`send_files`/`receive_sums`) and `receiver.c` (`receive_data`,
  `recv_files`) for the basis-file access pattern.

## TL;DR

**Today there is no live mmap + io_uring co-usage on the production transfer
path.** Every place where the receiver hands data to an `IoUringWriter` or
`IoUringDiskBatch` is fed from a heap-backed `Vec<u8>` (`MapFile<BufferedMap>`
sliding-`read(2)` window, or a freshly allocated channel buffer). The single
mmap site in `fast_io` (`MmapReader::open`,
`crates/fast_io/src/mmap_reader.rs:84`) is consumed only by:

- the parallel checksum digest path
  (`crates/checksums/src/parallel/files.rs:42, 237, 340`), which never
  touches `io_uring`; and
- `MapFile<MmapStrategy>` / `MapFile<AdaptiveMapStrategy>`, which the live
  receiver path does **not** instantiate today (the only constructor invoked
  from `transfer_ops::response`/`streaming` is `MapFile::open`, which is
  `BufferedMap` by definition).

There is one **dormant** code path - `DeltaApplicator` in
`crates/transfer/src/delta_apply/applicator.rs` - that opens its basis with
`MapFile::open_adaptive` (`AdaptiveMapStrategy` - mmap when file size >=
1 MiB on Unix) and writes through a plain `std::fs::File`. It is exported but
no production caller wires it. If a future change attaches an
`IoUringWriter` to this applicator, the mmap'd basis pointer would reach the
ring on writes >= the writer's batch threshold (256 KiB by default), so this
is the load-bearing hazard for the audit.

Upstream rsync 3.4.1 deliberately does not use real `mmap(2)` for basis
files, citing the `SIGBUS` truncate hazard
(`fileio.c:214-217`). Our `BufferedMap` strategy mirrors that decision; our
`MmapStrategy` is an opt-in fast path that diverges from upstream.

Severity legend: **HIGH** = active hazard on a wired path,
**MEDIUM** = latent hazard if dormant code is wired, **LOW** = defensive
hardening, **INFO** = observation, no action needed.

## Inventory

### A. `mmap(2)` of file pages

| # | Site (file:line) | Mapping | Range | Producer of the slice |
|---|------------------|---------|-------|-----------------------|
| A1 | `crates/fast_io/src/mmap_reader.rs:84` | `MmapOptions::new().map(&file)` (read-only, file-backed) | Whole file, faulted lazily by the kernel | Returned via `MmapReader::as_slice() -> &[u8]` |
| A2 | `crates/transfer/src/map_file/mmap.rs:38` | Wraps A1; `MapFile<MmapStrategy>` | Window slice into A1 | `map_ptr(offset, len) -> &[u8]` |
| A3 | `crates/transfer/src/map_file/adaptive.rs` | Selects A2 when `size >= MMAP_THRESHOLD` (1 MiB), else heap `BufferedMap` | Window slice into A1 | `map_ptr` returns mmap slice on the Mmap branch only |
| A4 | `crates/checksums/src/parallel/files.rs:42, 237, 340` | Wraps A1 for parallel digest | Whole file | Fed to scalar/SIMD digesters; never reaches `io_uring` |

### B. `mmap(2)` of io_uring kernel ring memory (NOT file mmap)

| # | Site | Notes |
|---|------|-------|
| B1 | `crates/fast_io/src/io_uring/buffer_ring.rs:313-322` | `libc::mmap` of `IORING_OFF_PBUF_RING` on the ring fd. This is kernel-managed io_uring metadata, not a file mapping. Out of scope for this audit beyond noting it exists. |

### C. `io_uring` data-submission sites that take a caller `&[u8]` / `*const u8`

| # | Site (file:line) | Submission op | Source of bytes today |
|---|------------------|---------------|-----------------------|
| C1 | `crates/fast_io/src/io_uring/file_writer.rs::write_all_batched` (line 211) | `IORING_OP_WRITE` via `submit_write_batch` (`batching.rs:53`) | Caller's `&[u8]`, pointer submitted directly |
| C2 | `crates/fast_io/src/io_uring/file_writer.rs::Write::write` (line 330) | Either internal-buffer copy (small) or direct `write_all_batched` (`buf.len() >= self.buffer_size`, default 256 KiB) | Caller-owned slice on the bypass branch only |
| C3 | `crates/fast_io/src/io_uring/file_writer.rs::flush_buffer` | `IORING_OP_WRITE` of the writer's owned internal buffer | Heap, owned by `IoUringWriter` |
| C4 | `crates/fast_io/src/io_uring/disk_batch.rs::flush_current` (line 223) | `IORING_OP_WRITE` of the batch's internal buffer | Heap, copied from caller in `write_data` (`disk_batch.rs:140-141`) |
| C5 | `crates/fast_io/src/io_uring/file_reader.rs::read_at` / `read_all_batched` | `IORING_OP_READ` | Destination is caller heap buffer; source is the file (kernel side). Not a co-usage path. |
| C6 | `crates/fast_io/src/io_uring/registered_buffers.rs::submit_read_fixed_batch` / `submit_write_fixed_batch` (lines 425, 544) | `IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` | Page-aligned heap buffers from `alloc::alloc_zeroed` (line 238) registered via `IORING_REGISTER_BUFFERS`. Caller data is `memcpy`'d in/out. Never touches mmap. |
| C7 | `crates/fast_io/src/io_uring/socket_writer.rs::Write::write` | `IORING_OP_SEND` / `sendmsg` via `submit_send_batch` | Internal heap buffer for small writes; bypass to caller slice when `buf.len() >= self.buffer_size`. **Not yet wired into transfer or rsync_io.** |

### D. Cross-reference: where do A and C meet today?

| # | Caller site (file:line) | Basis (mmap?) | Output (io_uring?) | Co-usage today? |
|---|--------------------------|---------------|---------------------|-----------------|
| D1 | `transfer_ops/response.rs:108` (output) + `:120` (basis) + `:237` (`map_ptr`) + `:242` (`output.write_all`) | `MapFile::open` -> `BufferedMap` (heap Vec) | `fast_io::writer_from_file` (io_uring when policy permits) | **No.** Heap basis -> io_uring writer. Safe. |
| D2 | `transfer_ops/streaming.rs:124` (basis) + `transfer_ops/token_loop.rs:179, 186` (`map_ptr` -> `extend_from_slice`) | `MapFile::open` -> `BufferedMap` | None directly. The `Vec<u8>` is `extend_from_slice`'d into a fresh recycled `Vec<u8>` and shipped over the SPSC channel to the disk thread, which feeds `IoUringDiskBatch`. | **No.** Defensive copy (`extend_from_slice`) and `IoUringDiskBatch` always copies into its own buffer (`disk_batch.rs:140-141`). Safe. |
| D3 | `delta_apply/applicator.rs:92` (basis) + `:64` (output) + `:195-202` (`map_ptr` -> `output.write_all`) | `MapFile::open_adaptive` -> `AdaptiveMapStrategy` (mmap >= 1 MiB on Unix) | Plain `std::fs::File`. **Not** io_uring. | **No, today.** But this is the dormant high-risk seam. See Finding F1. |
| D4 | `generator/mod.rs:714-741` (`open_source_reader`) | None (sender reads source via `fast_io::reader_from_path`) | `IoUringReader` for files >= 1 MiB | **No.** Source reads land in heap buffers. Safe. |
| D5 | `disk_commit/thread.rs:65-129` (`try_create_disk_batch` + reuse) | `IoUringDiskBatch` only | Buffer copy is internal (C4). Caller payloads are heap `Vec<u8>` from D2. | **No.** Safe by construction. |
| D6 | `checksums/parallel/files.rs:42, 237, 340` | A1/A4 mmap | None (CPU digesters only) | **No.** Not an io_uring path. |

`crates/engine/` and `crates/match/` were searched and contain no direct
`mmap` of files and no submission of caller-owned slices into `io_uring`.
The only engine reference to io_uring is a comment in
`engine/src/local_copy/context_impl/transfer.rs:286` documenting that
`copy_file_range` / io_uring do not respect seek positions - i.e. the
engine intentionally stays out of the picture.

## Risk per site

| # | Risk class | Notes |
|---|------------|-------|
| A1 | `SIGBUS` on mid-transfer truncate; cold-page fault stall on first access | Whole-file mapping with no `MAP_POPULATE`, no default `madvise WILLNEED`. `advise_sequential` / `advise_willneed` exist but are not invoked by transfer code. |
| A2 / A3 | Inherits A1 risk for any caller that holds the slice across a syscall. | Today only consumed by D3 (dormant). |
| A4 | `SIGBUS` on truncate mid-digest. | CPU-only path, but a rogue concurrent truncate of a basis file could still SIGBUS-kill the process. Not an io_uring concern; flagged for completeness. |
| B1 | None for this audit. | Kernel ring metadata; `RawIoUring` field-ordering invariant in `IoUringWriter` ensures the ring fd is dropped before any `RegisteredBufferGroup`, which is the only ordering hazard relevant to io_uring teardown. |
| C1 / C2 (bypass branch) | Caller-owned slice submitted directly. If the caller passes a mmap'd file slice, the kernel can fault on it during the SQE service path; under SQPOLL this stalls the kernel poller, and on truncate it raises `SIGBUS` *inside* a kernel context, which is the worst case. Today no production caller does this with mmap. | Latent. Triggers only at `len >= 256 KiB` on the bypass. |
| C3 / C4 / C6 | None - all heap-owned, isolated from any caller mmap. | Safe by construction. |
| C5 | Read destination is heap; source is the kernel's page cache. No mmap exposure. | Safe. |
| C7 | Same shape as C2; not yet wired. | Watch list for future work. |
| D1 / D2 / D5 / D6 | None today (BufferedMap or internal-copy isolates io_uring from any mmap). | Safe by construction. |
| D3 | `MEDIUM` (latent). The applicator is exported but not wired; if it ever gains an io_uring writer, large block-ref copies would submit a mmap'd basis pointer to the ring (C2 bypass) and inherit A1's `SIGBUS` and page-fault risks. | See Finding F1. |
| D4 | None. Sender source reads are always into heap buffers. | Safe. |

## Upstream comparison

Upstream rsync 3.4.1 does **not** use `mmap(2)` for basis files, despite the
function being named `map_file`. The header comment at
`fileio.c:214-217` is explicit:

```
/* This provides functionality somewhat similar to mmap() but using read().
 * It gives sliding window access to a file.  mmap() is not used because of
 * the possibility of another program (such as a mailer) truncating the
 * file thus giving us a SIGBUS. */
struct map_struct *map_file(int fd, OFF_T len, int32 read_size, int32 blk_size)
```

`map_ptr` (`fileio.c:236-315`) is a sliding-window `read(2)` with `pread(2)`
fallback; on a short read it zero-fills the rest of the buffer
(`fileio.c:304-307`) - "the file has changed mid transfer!" - rather than
risking a fault on a mapped page that no longer exists. Upstream therefore
sidesteps every issue in this audit by never letting file pages reach a
kernel I/O submission path that doesn't already cope with `read(2)` errors.

Our `BufferedMap` strategy
(`crates/transfer/src/map_file/buffered.rs`) is a one-to-one match for
upstream's `map_file` semantics: heap buffer, sliding `pread(2)` window,
zero-fill on short read. The wired transfer path (D1, D2) uses `BufferedMap`
exclusively, so our production behaviour is identical to upstream's wrt
this hazard. `MmapStrategy` (A2) is a deliberate divergence from upstream
and is opt-in only via `open_mmap` / `open_adaptive`.

## Findings

### F1 - Dormant seam: `DeltaApplicator` would expose mmap'd basis to io_uring if wired

- **Severity:** MEDIUM (latent; HIGH if wired without changes)
- **Evidence:**
  - `crates/transfer/src/delta_apply/applicator.rs:92` -
    `MapFile::open_adaptive(path)?` (Unix: mmap when size >= 1 MiB).
  - `crates/transfer/src/delta_apply/applicator.rs:64` - `output: File`
    (plain `std::fs::File`, no io_uring).
  - `crates/transfer/src/delta_apply/applicator.rs:195-202` -
    `let block_data = basis_map.map_ptr(...)?;` then
    `self.output.write_all(block_data)?;`.
  - Repository search: no production caller of `DeltaApplicator::new` /
    `apply_block_ref` outside of unit tests; the live receiver path uses
    `process_file_response` / `process_file_response_streaming` instead.
- **Impact:** If a future patch wires this applicator's `output` to a
  `fast_io` io_uring writer (the obvious next step for performance work),
  any block-ref copy >= 256 KiB will hit `IoUringWriter::write`'s bypass
  branch (C2) and submit a mmap'd basis pointer directly to the ring.
  Two failure modes follow:
  1. Cold-page faults on the basis file are serviced under the SQE
     submission thread (worse, the SQPOLL kernel thread when SQPOLL is on),
     adding latency exactly where io_uring was supposed to remove it.
  2. A concurrent truncation of the basis file
     (the upstream-cited mailer/external-modifier case) raises `SIGBUS`
     while the kernel is dereferencing the page on our behalf; recovery
     from in-kernel `SIGBUS` is not signal-safe.
- **Recommendation:** Before wiring `DeltaApplicator` with an io_uring
  output, add one of (in order of preference):
  - Force `MapFile::open` (`BufferedMap`) when the writer is io_uring;
    this matches upstream and the existing wired transfer paths.
  - If the mmap performance is desirable, copy `block_data` into the
    writer's owned buffer before submission (the same defense-in-depth as
    `token_loop.rs:186`).
  - As a last resort, pre-fault with `madvise(MADV_WILLNEED)` plus
    `MAP_POPULATE` and document the residual `SIGBUS`-on-truncate hazard;
    note that `MAP_POPULATE` only addresses fault stalls, not truncate.

### F2 - `MmapReader::open` lacks `madvise` defaults; `advise_*` helpers exist but are unused

- **Severity:** LOW
- **Evidence:**
  - `crates/fast_io/src/mmap_reader.rs:81-83` - safety comment "we assume
    the file won't be modified while mapped".
  - `crates/fast_io/src/mmap_reader.rs:124-143` - `advise_sequential`,
    `advise_random`, `advise_willneed` are defined (Unix) but no caller in
    `crates/transfer/`, `crates/checksums/`, `crates/fast_io/`, or
    `crates/engine/` invokes them.
- **Impact:** Even on the safe-by-construction CPU digest path
  (`checksums/parallel/files.rs`), a basis or source file truncated mid-run
  raises `SIGBUS` and aborts the process. Page-fault latency on first
  access of large files is also unmitigated.
- **Recommendation:** Have `MmapReader::open` call
  `advise(libc::MADV_SEQUENTIAL)` by default (or expose a typed `Hint`
  enum to the caller), and document - including a clear pointer to upstream
  `fileio.c:214-217` - that mmap basis files are vulnerable to truncate
  `SIGBUS`. This costs nothing on read-mostly paths and matches the access
  pattern of every existing caller.

### F3 - `IoUringWriter::write` bypass is a permanent latent hazard for any future caller

- **Severity:** LOW
- **Evidence:**
  - `crates/fast_io/src/io_uring/file_writer.rs:330-350` - `Write::write`
    copies small buffers into its internal buffer but *bypasses the copy*
    when `buf.len() >= self.buffer_size` (default 256 KiB), calling
    `write_all_batched(buf, ...)` directly. `write_all_batched` submits the
    caller's pointer in its SQE.
- **Impact:** Today no production caller passes a mmap'd slice through
  this path (the wired receiver always goes through a heap-owned chunk
  buffer). The hazard is one carelessly added caller away, and nothing in
  the type signature warns the next contributor.
- **Recommendation:** Either (a) document at the function level that
  callers must own the buffer for the duration of the SQE (today's
  contract is implicit in the `&mut self` lifetime, which is necessary
  but not sufficient under SQPOLL), or (b) gate the bypass on a feature
  flag / explicit `write_zero_copy` method so the default `Write::write`
  always copies. (a) is cheaper and matches the rest of the crate's
  comment style.

### F4 - `IoUringSocketWriter` not yet wired; same shape as F3 when it is

- **Severity:** INFO
- **Evidence:**
  - `crates/fast_io/src/io_uring/socket_writer.rs` - `Write::write` copies
    small buffers; bypass to `submit_send_batch` for `buf.len() >=
    self.buffer_size`. No callers in `crates/transfer/`,
    `crates/rsync_io/`, or elsewhere.
- **Impact:** None today. Listed so that whoever wires it remembers the
  C2-class hazard.
- **Recommendation:** Add the same docstring guard as F3 before the first
  caller is written.

### F5 - `IoUringDiskBatch` and the streaming token-loop already do the right thing

- **Severity:** INFO (positive finding)
- **Evidence:**
  - `crates/fast_io/src/io_uring/disk_batch.rs:140-141` - `write_data`
    always `copy_from_slice`'s caller bytes into the batch's owned buffer
    before any SQE.
  - `crates/transfer/src/transfer_ops/token_loop.rs:185-186` -
    `let mut buf = recycle_or_alloc(...); buf.extend_from_slice(block_data);`
    before the channel send. Even if `block_data` ever came from mmap, the
    copy isolates io_uring from the file mapping.
- **Impact:** Defense in depth: the production streaming path is robust
  against future changes that switch the basis to `MapFile<MmapStrategy>`,
  because the mmap pointer never crosses the channel.
- **Recommendation:** Document both invariants in a short module-level
  comment so they survive future refactors. (The token_loop line already
  has a `simple_recv_token` upstream reference; add a one-line note that
  this copy is also load-bearing for io_uring safety.)

### F6 - `RawIoUring` drop ordering is correct and load-bearing; do not refactor casually

- **Severity:** INFO (positive finding)
- **Evidence:**
  - `crates/fast_io/src/io_uring/file_writer.rs` - `IoUringWriter` declares
    `RawIoUring` *before* `RegisteredBufferGroup`, so Rust's field-drop
    order tears down the ring fd first; the kernel un-pins registered
    buffers as part of `close(ring_fd)`, after which the user-side
    deallocation in `RegisteredBufferGroup`'s `Drop` is sound.
- **Impact:** This is the single ordering invariant that makes
  registered-buffer io_uring sound under panic / early return. Reordering
  the fields is a use-after-free.
- **Recommendation:** Add a `// SAFETY: drop order matters - ring fd
  must close before RegisteredBufferGroup's heap allocation is freed`
  comment at the field site and a matching note in any future doc that
  describes the type.

## Recommendations summary

The five concrete next steps requested by the task brief, mapped to the
findings above:

1. **`MAP_POPULATE` pre-fault**
   - Not required today: nothing on the wired path passes mmap to
     io_uring (D1, D2, D5).
   - Required *if and only if* F1 is wired without first switching the
     basis to `BufferedMap`. In that case, pair `MAP_POPULATE` with
     `madvise(MADV_WILLNEED)` and document that `MAP_POPULATE` does **not**
     fix the truncate-`SIGBUS` issue (it only fixes cold faults).

2. **`madvise(MADV_WILLNEED)` / `MADV_SEQUENTIAL`**
   - Already exposed as `MmapReader::advise_*` (`mmap_reader.rs:124-143`)
     but unused. F2 recommends invoking `MADV_SEQUENTIAL` by default in
     `MmapReader::open` so the existing CPU-digest mmap path benefits even
     before any io_uring work lands.

3. **Switch off mmap when io_uring is active**
   - Wired path: already the case. `transfer_ops::response` and
     `transfer_ops::streaming` call `MapFile::open` (`BufferedMap`),
     never `open_mmap` / `open_adaptive`.
   - Dormant path: F1 - if `DeltaApplicator` ever gets an io_uring
     output, replace `open_adaptive` with `open` (or copy block_data into
     the writer's buffer before submission, as token_loop does).

4. **Sites that are already safe**
   - D1, D2, D5 (the live receiver path): heap-only basis + io_uring
     writer is fine.
   - D4 (sender source reads): heap-only destination buffers.
   - D6 (parallel checksum digest): no io_uring at all.
   - C3, C4, C6, C7-internal: io_uring submissions of writer/batch-owned
     heap buffers; mmap-isolated by construction.

5. **Documentation hardening**
   - F3 / F4: document the `Write::write` bypass invariant on
     `IoUringWriter` and `IoUringSocketWriter`.
   - F5 / F6: add comments at `token_loop.rs:185-186`,
     `disk_batch.rs:140-141`, and the `RawIoUring` field site so future
     contributors do not accidentally undo the safety properties.

No code changes are made by this audit. The remediation work is recorded
as Findings F1-F6 and is intentionally outside the scope of #1660.

## Conclusion

The current production transfer path is free of mmap + io_uring co-usage:
every io_uring submission either owns its buffer (`IoUringWriter` flush
buffer, `IoUringDiskBatch` internal buffer, registered buffers) or is fed
from a heap-backed `MapFile<BufferedMap>` window that mirrors upstream
rsync's deliberate avoidance of `mmap(2)` for basis files. The single
realistic hazard is the dormant `DeltaApplicator` path (F1), which today
does not reach io_uring but is the obvious next target for an
io_uring-output rewrite; whoever does that work must keep the basis on
`BufferedMap` (or copy before submission) so we continue to match upstream's
`SIGBUS`-on-truncate immunity. The smaller findings (F2-F6) are
documentation and defensive-defaults hardening and can be picked up
opportunistically.
