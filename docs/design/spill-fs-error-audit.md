# Spill module FS-error edge audit (SPL-32)

Tracking task: SPL-32. Companion follow-ups: SPL-33 (ENOSPC fault
injection) and SPL-34 (temp-vanish fault injection).

## Purpose

The reorder buffer spill layer
(`crates/engine/src/concurrent_delta/spill/`) writes and reads
short-lived tempfiles whenever the in-memory ring exceeds its byte
budget. Under realistic heavy-transfer pressure the disk can refuse a
write mid-record (ENOSPC), a sandbox janitor can unlink the tempfile
between create and read (ENOENT / temp-vanish), or a stale fd can be
referenced after a panic-triggered drop (EBADF). This document
enumerates every syscall site, classifies the existing error path, and
hands SPL-33 / SPL-34 a concrete checklist of behaviours to assert.

This audit is read-only: no production source is modified.

## Inventory

Every filesystem syscall reachable from a `SpillableReorderBuffer`
caller is cataloged below. "Syscall" here means anything that can
return an `io::Error` other than codec / serialization failures.
Citations are `file:line` against the worktree at audit time.

### 1. Backend construction

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 1 | `spill/tempfile.rs:58` | `::tempfile::tempfile_in(dir)` (directory backend) | Bubbles up `io::Error` to `write_record` caller |
| 2 | `spill/tempfile.rs:59` | `::tempfile::SpooledTempFile::new(1 MiB)` (spooled backend) | Infallible at construction; first write may spill to disk via the crate's internal rollover |
| 3 | `spill/buffer/lifecycle.rs:69` | `fs::create_dir_all(&dir)` (called from `with_spill_dir`) | Bubbles up via constructor return |
| 4 | `spill/buffer/spill.rs:314` | `fs::create_dir_all(&dir)` (called from `recreate_spill_dir`) | Bubbles up to `spill_item` / `spill_candidates_whole_batch`, then to caller as `SpillError::Io` |

### 2. Write hot path (per-item granularity)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 5 | `spill/buffer/spill.rs:288` | Lazy `open_backend(dir.as_deref())` inside `write_record` | Bubbles up to `spill_item`; on `NotFound` with a `spill_dir`, one `recreate_spill_dir` retry attempt |
| 6 | `spill/buffer/spill.rs:291` | `file.seek(SeekFrom::Start(spill_write_pos))` | Bubbles up; caller re-inserts the in-flight item via `force_insert` so the buffer retains the data |
| 7 | `spill/buffer/spill.rs:292` | `file.write_all(header)` (5-byte tag + LE len) | Bubbles up; partial write surfaces as `ErrorKind::WriteZero` per stdlib contract |
| 8 | `spill/buffer/spill.rs:293` | `file.write_all(payload)` (compressed-or-raw bytes) | Bubbles up; partial write surfaces as `ErrorKind::WriteZero`. Header may have already committed - see ENOSPC analysis below |

### 3. Write hot path (whole-batch granularity)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 9 | `spill/buffer/spill.rs:147` | First `write_record(len_bytes, payload)` for the packed batch | Bubbles up; `restore_taken` re-inserts every item from the batch on failure |
| 10 | `spill/buffer/spill.rs:161` | Retry `write_record(len_bytes, payload)` after `recreate_spill_dir` (only when `spill_index` was empty) | Bubbles up; same `restore_taken` recovery |

### 4. Read hot path (single-item reload)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 11 | `spill/buffer/reload.rs:137` | `spill_file.as_mut().ok_or(NotFound)` guard | Bubbles up as synthetic `NotFound`; never silent |
| 12 | `spill/buffer/reload.rs:140` | `file.seek(SeekFrom::Start(offset))` | Bubbles up to `next_in_order` / `drain_ready` |
| 13 | `spill/buffer/reload.rs:144` | `file.read_exact(&mut tag_buf)` (1-byte codec tag) | Bubbles up; `ErrorKind::UnexpectedEof` on truncation |
| 14 | `spill/buffer/reload.rs:149` | `file.read_exact(&mut len_buf)` (4-byte LE length) | Bubbles up; `ErrorKind::UnexpectedEof` on truncation |
| 15 | `spill/buffer/reload.rs:154` | `file.read_exact(&mut payload)` (length-prefixed bytes) | Bubbles up; `ErrorKind::UnexpectedEof` on truncation |

### 5. Read hot path (whole-batch reload)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 16 | `spill/buffer/reload.rs:166` | `spill_file.as_mut().ok_or(NotFound)` guard | Bubbles up as synthetic `NotFound` |
| 17 | `spill/buffer/reload.rs:171` | `file.seek(SeekFrom::Start(offset))` | Bubbles up to `next_in_order` |
| 18 | `spill/buffer/reload.rs:174` | `file.read_exact(&mut len_buf)` (4-byte LE total length) | Bubbles up; `ErrorKind::UnexpectedEof` on truncation |
| 19 | `spill/buffer/reload.rs:178` | `file.read_exact(&mut payload)` (`total_len` bytes) | Bubbles up; `ErrorKind::UnexpectedEof` on truncation |

### 6. Tempfile lifecycle (RAII)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 20 | `spill/tempfile.rs:32` | `Drop` for `tempfile::SpooledTempFile` | Silent; unlinks the disk side if rollover happened, otherwise frees the in-memory buffer |
| 21 | `spill/tempfile.rs:33` | `Drop` for `tempfile_in`-returned `File` | Silent; the crate unlinks via the kernel because `tempfile_in` opens with `O_TMPFILE` or via immediate unlink-after-create |
| 22 | `spill/buffer/spill.rs:313` | Explicit `self.spill_file = None` (in `recreate_spill_dir`) | Silent; drops fd before reopening so the OS can release the inode |

### 7. RSS probe (out-of-band)

| # | Site | Operation | Error path |
|---|------|-----------|------------|
| 23 | `spill/rss.rs:137` | `fs::read_to_string("/proc/self/statm")` (Linux only) | Silent on the spill path; `should_force_spill_for_rss` collapses any `Err(_)` to `false` and keeps the byte-budget knob in charge |

### 8. Explicitly absent operations

The audit also verified that none of the following appear in the spill
module: `File::create`, `File::open`, `OpenOptions::open`,
`fs::remove_file`, `fs::rename`, `flush()`, `sync_data()`,
`sync_all()`. The spill backend never opens or renames files by path
in steady state - all writes go through the cached `SpillBackend`
handle, and unlink happens via `tempfile` crate RAII. There is no
explicit fsync: the spill file is treated as scratch space that must
not survive process death, so durability is intentionally not
requested.

## Classification summary

Totalling sites 1-23:

- **Recoverable** (caller can retry or shut down cleanly): 9 sites
  (1, 3, 5, 6, 7, 9, 10, plus 22 and 23 which never bubble in the
  first place - 22 because drop is infallible, 23 because the RSS
  probe silently degrades).
- **Bubbles up** to the receiver-level error path as `SpillError::Io`:
  17 sites (1, 3, 4-10, 11-19). Every read-path site falls in this
  bucket.
- **Panics** (unwrap / expect / `?` in an infallible-looking context):
  **0**. No `.unwrap()`, `.expect()`, or "this can never fail"
  comments cover any of the syscall sites. The closest the module
  comes to a panic is `debug_assert!` on post-reload reorder semantics
  (`reload.rs:73, 87-90`), which only fires in debug builds.
- **Silent** (logged or swallowed): 3 sites (20, 21, 23). The two
  drops are intentional RAII unlinks and the RSS probe silently
  degrades to "no pressure" - this is documented in `rss.rs:18-23` and
  `spill.rs:201-206`.

(Sites 6 and 7 are double-counted across recoverable / bubbles-up
because the recovery is "re-insert the item, bubble up the error" -
both halves apply.)

The headline finding: **no spill site panics on FS error**, which is
exactly what the earlier hardening pass intended. Every site either
returns an `io::Error` typed as `SpillError::Io` or, for the three
deliberately silent sites, has a clearly documented degradation.

## ENOSPC analysis

### Vulnerable hot paths

ENOSPC can be raised by the kernel on any of the following sites:

- Site 1 (`tempfile_in`) - directory exists but the filesystem is
  full at backend creation time. Behaviour: backend construction
  fails, `spill_excess` returns `SpillError::Io`, in-flight items
  remain in memory via `restore_taken` (whole-batch path) or
  `force_insert` (per-item path).
- Site 7 (`file.write_all(header)`) - 5-byte write rejected. Header
  may complete with partial bytes; stdlib's `write_all` returns
  `WriteZero`. The spill file is left with garbage trailing bytes at
  `spill_write_pos` but `spill_write_pos` is not advanced, so the next
  `seek` overwrites the garbage.
- Site 8 (`file.write_all(payload)`) - **the highest-volume
  ENOSPC-vulnerable site**. Once the header has committed, a payload
  failure leaves a length-prefixed-but-truncated record on disk.
  Because `spill_write_pos` is only advanced after `write_all` returns
  `Ok`, the next spill will overwrite this byte range, so no reader
  will ever decode the partial record - but until then, an external
  inspector sees a malformed record.
- Site 9 / Site 10 (whole-batch `write_record`) - same payload-half
  failure mode as site 8, but with potentially many items
  rolled into one record.

### State left after ENOSPC

For every ENOSPC site:

- `spill_write_pos` stays at the pre-write value. The next `seek` /
  `write_all` overwrites the truncated bytes, so on-disk corruption
  is invisible to legitimate readers.
- `spill_index` and `batch_members` are mutated **only after**
  `write_record` returns `Ok` (`spill.rs:183-188` for whole-batch,
  `spill.rs:238` / `253-254` for per-item). On failure the index
  reflects only previously-committed records.
- Per-item path: `inner.force_insert(seq, item)` (`spill.rs:92`) puts
  the item back in memory; `memory_used` is unchanged.
- Whole-batch path: `restore_taken(taken)` (`spill.rs:133, 138, 152,
  155, 164, 170`) reinserts every taken item via `force_insert`;
  `memory_used` is unchanged.
- `SpillError::is_out_of_space()` (`error.rs:53`) checks for
  `io::ErrorKind::StorageFull`; production callers can distinguish
  ENOSPC from other FS failures for telemetry.

The net contract: **ENOSPC mid-record never corrupts the buffer's
in-memory accounting and never loses an item**. The on-disk tempfile
may carry a partial trailing record, but it is unreachable.

### Gaps

1. `WriteZero` from a partial header (site 7) and `WriteZero` from a
   partial payload (sites 8-10) are not distinguished anywhere. A
   user-facing log line would help operators distinguish "disk is
   completely full" from "kernel quota tripped between header and
   payload".
2. `tempfile` crate's `SpooledTempFile` rollover (site 2 -> site 5)
   can itself raise ENOSPC the first time data exceeds 1 MiB. The
   error surfaces at the next `write_all`, not at construction, so
   tests must trigger spillover to exercise this path.

## Temp-vanish analysis

### Vulnerable hot paths

A janitor (`tmpwatch`, `systemd-tmpfiles`, container sandbox rotation,
operator-issued `rm -rf`) can unlink the spill backing file or its
parent directory while the buffer holds the fd. Linux semantics: the
unlinked file remains readable via the fd until close, but a fresh
open of the path will fail with ENOENT. The current code reads and
writes through the cached `SpillBackend` handle, so:

- **Sites 5, 12, 17 (seek)** and **6 (seek)** keep working against the
  unlinked-but-open inode. The kernel does not revoke the fd.
- **Sites 13-15, 18-19 (read_exact)** keep working against the
  unlinked-but-open inode.
- **Sites 7-8, 9-10 (write_all)** keep working against the
  unlinked-but-open inode.
- **Site 5 (lazy `open_backend`)** is the only path that re-opens by
  path. This is reached on the first spill of a given buffer, or
  after `recreate_spill_dir` cleared `spill_file = None`. If the
  spill directory has vanished between construction and the first
  spill, this site raises `ErrorKind::NotFound`, which `spill_item`
  (`spill.rs:242-257`) catches and routes through `recreate_spill_dir`
  + retry. The retry creates a **fresh** tempfile in the recreated
  directory; **any items previously spilled to the vanished inode are
  unreachable**, and the recovery path explicitly refuses to engage
  (`spill.rs:249-251`) when `spill_index` is non-empty.

### Read-after-write ordering

The reload path (`reload.rs:133-186`) always uses the cached fd
captured at the first spill. There is no `path -> open -> read`
sequence after the initial backend construction, so a deletion that
happens **after** the buffer has cached its fd cannot make a reload
fail with ENOENT. The only race window is:

- Backend not yet constructed (`spill_file is None`).
- First spill arrives.
- Janitor unlinks the directory between two ticks of `open_backend`.

This is a small window, and the existing `recreate_spill_dir` retry
handles it for the directory-backed flavour (sites 5 + 4). The
spooled flavour cannot be retried because it has no caller-supplied
directory.

### Gaps

1. The current retry refuses to recover when `spill_index` is
   non-empty (`spill.rs:249-251` for per-item, `spill.rs:150-153` for
   whole-batch). This is correct - silently dropping previously
   spilled items would corrupt the transfer - but the message bubbled
   up to the receiver is just a generic `NotFound`. A typed error
   variant ("`PriorSpillsLost`") would let the receiver log a more
   useful diagnostic.
2. Recovery retries exactly **once** (`spill.rs:252, 304-320`); a
   janitor that wipes the directory a second time within the same
   spill burst surfaces the second `NotFound` as a fatal
   `SpillError::Io`. This matches the documented contract but is not
   tested.
3. `SpooledTempFile` (site 2) has no caller-supplied directory and
   stores its rolled-over file under the system tempdir
   (`$TMPDIR` / `/tmp`). A janitor wiping `/tmp` mid-transfer races
   with the crate's internal handling, which is opaque to this audit.
   SPL-34 should exercise both flavours.

## EBADF analysis

### Vulnerable hot paths

EBADF requires the kernel-level fd to be closed while a syscall is
in flight. In Rust this can only happen if `SpillBackend` is dropped
on one thread while another thread holds a `&mut` to its inner file.
`SpillableReorderBuffer<T>` is **not** `Sync` and exposes `&mut self`
on every mutating method, so the borrow checker forbids concurrent
mutation. The cached `spill_file` is `Option<SpillBackend>`; the only
places it is set to `None` are:

- `spill.rs:313` in `recreate_spill_dir`, called from `spill_item`
  (`spill.rs:252`) and `spill_candidates_whole_batch`
  (`spill.rs:154`).
- `Drop` of the parent buffer.

Both paths run under `&mut self`, so no thread can hold a stale
borrow.

### Panic-during-write

A panic in `T::encode` (`spill.rs:132, 224`) is caught **before** any
syscall is issued, because encoding writes into a local `Vec<u8>`
buffer that is fed to `write_record` only on success. A panic in
`write_all` itself unwinds across `write_record`, `spill_item`,
`spill_excess`, and `insert`; the unwind drops `SpillableReorderBuffer`
(if it was the only `&mut` holder), which drops `spill_file`, which
closes the fd. No subsequent operation can target the closed fd
because the buffer itself is gone.

### Gaps

EBADF is not reachable through the current API. The audit found no
sites that could lower the buffer's invariants. **No EBADF tests
required for SPL-34.** This finding deliberately narrows the test
scope.

## Recommended SPL-33 (ENOSPC injection) test cases

### Simulation methods

- **tmpfs + size cap**: `mount -t tmpfs -o size=4M tmpfs $DIR`
  followed by `with_spill_dir($DIR)`. The size cap forces ENOSPC at
  predictable byte counts. Linux-only, gated on `cfg(target_os =
  "linux")` and requires root or user-namespace permissions; in CI
  the gate can degrade to `#[ignore]` when the mount fails.
- **`LD_PRELOAD` shim** intercepting `write(2)` / `writev(2)` to
  return `-1 / errno=ENOSPC` after a configurable byte count. Works
  cross-flavour (spooled + directory) and on macOS via
  `DYLD_INSERT_LIBRARIES`.
- **Custom `SpillBackend`**: extend the existing `tempfile.rs` test
  surface with a `Cursor`-like in-process backend that fails after N
  bytes. Cleanest unit-test approach and avoids platform gates; pair
  it with one `tmpfs` integration test to cover the real kernel path.

### Test matrix

| Scenario | Inject at | Expected behaviour | Assertion |
|----------|-----------|---------------------|-----------|
| ENOSPC on first `open_backend` (site 5) | Pre-allocate tmpfs to 0 free bytes before first insert | `insert` returns `SpillError::Io`; `is_out_of_space()` is `true` | `assert!(matches!(err, SpillError::Io(e) if e.kind() == ErrorKind::StorageFull))` |
| ENOSPC on header `write_all` (site 7) | Fail at offset `cumulative + 0` for the next record | `insert` returns `SpillError::Io`; item is back in memory; `spill_write_pos` unchanged | `assert_eq!(buf.buffered_count(), N_before); assert_eq!(stats.spill_events, prior)` |
| ENOSPC on payload `write_all` (site 8, per-item) | Fail at offset `cumulative + 5` | Same as above; on-disk tempfile carries unreachable trailing bytes | Same assertion; **plus** a subsequent `insert` that fits triggers a successful spill at the same `spill_write_pos` |
| ENOSPC on payload `write_all` (sites 9-10, whole-batch) | Fail at offset `cumulative + 4` | All N taken items returned to memory via `restore_taken`; `spill_index` unchanged | `assert_eq!(buf.buffered_count(), N_before); assert!(buf.spill_stats().spilled_items == 0)` |
| ENOSPC on `tempfile_in` (site 1) | Pre-fill the directory | Constructor / first-spill returns `SpillError::Io` | Same `is_out_of_space()` assertion |
| ENOSPC during `SpooledTempFile` rollover (site 2) | Force >1 MiB of spill data on a full `/tmp` | First `write_all` after the rollover boundary returns `SpillError::Io` | Same |
| ENOSPC followed by free-space restoration | Inject once, then clear | First `insert` errors; next `insert` succeeds, `spill_events` increments | `assert!(buf.spill_stats().spill_events > prior)` |

Every test should additionally assert `buf.next_in_order()` returns
the originally-inserted items in the right order, proving the
in-memory backup survived the error.

## Recommended SPL-34 (temp-vanish injection) test cases

### Simulation methods

- **Atomic-rename race**: spawn a thread that calls
  `fs::remove_dir_all(spill_dir)` immediately after the buffer is
  constructed but before the first spill. Deterministic if the test
  uses `std::sync::Barrier` to synchronise the unlink with the
  receiver's first `insert` that triggers `spill_excess`.
- **`tmpwatch`-style janitor**: spawn a thread that loops on
  `fs::remove_file(spill_dir.join(entry))` for every entry in the
  directory. Stresses the read-after-write race when the cached fd is
  already open.
- **Manual unlink via `nix::unistd::unlinkat`**: targeted single-file
  unlink that respects `O_TMPFILE` semantics.
- **In-process janitor on `SpooledTempFile`**: not feasible because
  the file path is opaque to the caller. SPL-34 should restrict
  spooled-flavour tests to **construction-time** vanishing
  (`$TMPDIR` removed before first spill) and document that
  steady-state spooled vanishing is not testable from oc-rsync.

### Test matrix

| Scenario | Simulation | Expected behaviour | Assertion |
|----------|------------|---------------------|-----------|
| Directory removed before first spill (directory backend) | Race: drop spill dir between `with_spill_dir` and first `insert` that crosses threshold | `recreate_spill_dir` fires; spill succeeds; `dir_recreate_count == 1` | `assert_eq!(buf.spill_stats().dir_recreate_events, 1)` and the inserted item drains in order |
| Directory removed after first successful spill | Drop dir after `spill_index` non-empty; trigger a second spill | `SpillError::Io` with `ErrorKind::NotFound`; `recreate_spill_dir` refuses (prior spills lost) | `assert!(matches!(err, SpillError::Io(e) if e.kind() == ErrorKind::NotFound))` and `dir_recreate_count == 0` |
| Directory removed after spill, before reload | Drop dir after spill; call `next_in_order` | Reload succeeds because the fd is still open against the unlinked inode | `assert_eq!(reloaded.ndx().get(), expected)` |
| Underlying tempfile unlinked (kept fd open) | Use `unlinkat` to drop the file; keep buffer holding fd | Same as above: read keeps working | Same assertion |
| Directory removed and recreated twice in one burst | First removal racing with recovery attempt | First `recreate_spill_dir` retries once; second removal surfaces fatal `NotFound` | `assert_eq!(dir_recreate_count, 1); assert!(matches!(err, SpillError::Io(e) if e.kind() == ErrorKind::NotFound))` |
| `$TMPDIR` removed before spooled rollover | Pre-removal on spooled flavour | First `write_all` after spillover returns `SpillError::Io` | Same `NotFound` assertion |
| Concurrent janitor unlinks during sustained burst | Background thread looping `remove_dir_all` during 10k inserts | At least one of: (a) all inserts succeed via the cached fd, (b) a `SpillError::Io` surfaces and the receiver aborts. No panic, no silent data loss. | `assert!(result.is_ok() || matches!(result.unwrap_err(), SpillError::Io(_)))` and `assert_eq!(buf.buffered_count(), inserted - drained)` |

Every test should additionally assert that no thread panicked and
that `buf.spill_stats().dir_recreate_events <= 1` (the documented
single-retry contract).

## Hardening priority

Single highest-impact follow-up: **distinguish "prior spills lost"
(`spill.rs:150-153, 249-251`) from generic `NotFound`**. The current
behaviour is correct (refuse to recover when items would be silently
dropped) but the bubble-up uses a generic `io::Error`, which the
receiver cannot tell apart from a transient kernel-level ENOENT. A
typed `SpillError::PriorSpillsLost { dir, count }` variant would let
the receiver emit an actionable diagnostic for the operator and would
make SPL-34's recovery-refusal test unambiguous.
