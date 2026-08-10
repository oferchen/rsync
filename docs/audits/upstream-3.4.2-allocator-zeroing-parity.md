# Upstream 3.4.2 parity: allocator zeroing pattern

Tracking issue: #2228. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

The rsync 3.4.2 NEWS file (lines 41-44) records:

> Zero all new memory from internal allocations: `my_alloc()` now uses
> `calloc`, and `expand_item_list()` zeros the expanded portion after
> `realloc`. This gives more predictable behaviour if stale or
> uninitialised memory is ever accidentally read.

Two C sites are touched:

- `util2.c:73-89` `my_alloc()`: initial allocation switched from
  `malloc(num*size)` to `calloc(num, size)` so the first allocation is
  always zero-filled. Grows continue to use `realloc`, which preserves
  the existing prefix and leaves the new tail un-initialised - callers
  must zero the tail themselves.
- `util1.c:1697-1727` `expand_item_list()`: after `realloc_buf()`
  enlarges `lp->items`, the function now `memset`s the newly added
  bytes (`lp->malloced * item_size` ... `expand_size * item_size`) to
  zero before returning the new slot pointer.

Together these guarantee every item-list slot reads back as zero on
first use, even when the backing buffer was grown by `realloc` rather
than freshly `calloc`ed.

## 2. Rust counterparts in oc-rsync

Rust's standard library makes the bug very hard to reproduce:

- `Vec::with_capacity(n)` allocates uninitialised backing storage but
  does not expose it (`len() == 0`). The only way to read uninitialised
  bytes from a `Vec<T>` is `unsafe { set_len(n) }` without intervening
  initialisation.
- `Vec::resize(n, value)` and `Vec::extend(...)` always write every new
  element, so they cannot leak stale memory.
- Internal regrowth on `push`/`extend`/`reserve` moves the existing
  prefix and leaves the spare capacity inaccessible via the safe API.

The C `expand_item_list` bug therefore only has Rust analogues at
`Vec::set_len` (and `MaybeUninit` slice construction) sites.

## 3. Audited sites

A workspace-wide grep for `set_len`, `MaybeUninit`, `new_uninit`, and
`with_capacity ... set_len` patterns turned up the following
production sites. All `File::set_len` matches are filesystem
truncate calls and are not in scope.

### 3.1 `crates/protocol/src/multiplex/helpers.rs:83-122`

`read_payload_into` reserves `len` bytes, calls `set_len(len)`, then
reads into the buffer until either `len` bytes have been written or a
short read / error occurs. Every error path calls
`buffer.truncate(read_total)` before returning, so no uninitialised
byte is ever exposed.

Verdict: **SAFE**. Equivalent to upstream's `calloc + read_exact` for
fixed-size multiplex payloads.

### 3.2 `crates/engine/src/local_copy/buffer_pool/pool.rs:680-719`

`BufferPool::return_buffer` sets the returned buffer's length back to
`self.buffer_size` so the next borrower receives a `Vec<u8>` whose
`len() == capacity == buffer_size`. The contained bytes are stale
content from the previous transfer. The safety comment (lines
696-701) documents the invariant that borrowers fully overwrite the
buffer via `Read::read()` before any byte is observed downstream.

This deliberately mirrors upstream rsync's `iobuf` reuse pattern
(`io.c:perform_io`): the I/O buffer is re-used across blocks and the
caller consumes only `&buf[..n]` after each read. Forcing a
`resize(buffer_size, 0)` here is the documented #1 CPU hotspot
(see comment block). The invariant is enforced by every consumer
because the buffer is returned through `PooledBuffer`'s RAII guard,
not handed out as a raw `&mut [u8]`.

Verdict: **SAFE** by construction (consumer-side overwrite
invariant). Not analogous to the C bug, which leaked uninitialised
fresh memory; this site recycles already-written memory.

### 3.3 `crates/metadata/src/id_lookup/nss.rs` and `crates/platform/src/group.rs`

All four `MaybeUninit::<libc::passwd>::zeroed()` / `MaybeUninit::<libc::group>::zeroed()` sites
start zero-filled before being handed to `getpwnam_r`/`getgrnam_r` /
`getgrgid_r`. The FFI callee writes every field that matters.

Verdict: **NOT APPLICABLE**. These are POSIX struct-init scaffolds,
not growable arrays.

## 4. Sites that mirror upstream by construction

- File-list growth (`crates/transfer/src/generator/file_list/mod.rs`,
  `crates/protocol/src/flist/...`) uses `Vec::push` / `Vec::reserve`
  + `push`. Reservation never exposes the new tail; each appended
  entry is a fully constructed `FileEntry`.
- Segment buffers (`crates/core/src/message/segments/buffer.rs`) use
  `try_reserve_exact` followed by `extend_from_slice`.
- Multiplex framing (`crates/protocol/src/multiplex/codec.rs`) uses
  `reserve` + `BytesMut::put_slice`.

None of these need an explicit `memset` of a tail.

## 5. Conclusion

No production change required. The upstream 3.4.2 zeroing fix targets
a class of bug that does not have a live instance in oc-rsync: every
allocation site either initialises through `Vec::push`/`extend`/
`resize` (which write every new slot) or goes through `unsafe set_len`
under a documented "fully overwritten before observation" invariant
that holds at both call sites (multiplex payload read, buffer-pool
recycle).

This audit is recorded so future allocator changes - particularly any
new `unsafe set_len` site introduced for performance - explicitly
re-check the invariant against the upstream 3.4.2 expectation.
