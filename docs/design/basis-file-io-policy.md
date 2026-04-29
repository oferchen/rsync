# Basis-file I/O policy: mmap vs buffered, with io_uring

Tracking issue: oc-rsync task #1666.
Companion audit: `docs/audits/mmap-iouring-co-usage.md` (#3440).

## Summary

oc-rsync reads from "basis files" - the destination file already on disk -
during delta application to copy unchanged blocks into the new file. There
are three viable strategies to read those blocks: a sliding-window heap
buffer (`BufferedMap`), an anonymous private memory map (`MmapStrategy`),
and io_uring registered buffers (`RegisteredBufferGroup`).

This document defines when each is selected. The policy locks in
upstream-faithful behaviour (no `SIGBUS` exposure on basis-file truncation,
no co-issue of mmap pointers with io_uring SQEs) and leaves room for the
existing `MmapStrategy` opt-in to keep working for read-only,
non-io_uring workloads.

## Goal

A delta-apply call site asks "give me bytes `[off, off+len)` of the basis
file" via `MapStrategy::map_ptr` (`crates/transfer/src/map_file/mod.rs:71`).
The chosen backing strategy must satisfy:

1. **Correctness against concurrent truncation.** A reader other than us
   may shrink, rotate, or replace the basis file mid-transfer. Upstream
   rsync sidesteps this by reading via `read(2)` instead of mmap
   (`fileio.c:214-217`). Our policy must do the same on every path that
   could see a third-party writer (network filesystems,
   user-visible destinations on inplace transfers, log files, mailspools).
2. **No mmap pointer escapes into io_uring.** A pointer derived from a
   `memmap2::Mmap` must never be submitted as an iovec, registered buffer,
   or `READ_FIXED`/`WRITE_FIXED` target. The kernel will fault in pages
   on demand; if the file is later truncated, the page-fault delivery
   path inside the io_uring worker can either kill the worker or surface
   `SIGBUS` to the calling thread.
3. **Reuse upstream's window-reuse trick** when buffered. `BufferedMap`
   already mirrors `fileio.c:268-279`'s `memmove` of overlapping bytes
   on forward slides (`crates/transfer/src/map_file/buffered.rs:119-141`).
4. **Take the zero-copy win where it is safe.** Read-only basis-file
   access on a local filesystem with no concurrent writer and no
   io_uring data-path participation is the textbook mmap case.

## Decision matrix

Inputs:

- `file_size`: size of the basis file in bytes.
- `io_uring_active`: the receiver has an `IoUringReader`/`IoUringWriter`
  in this transfer (`crates/fast_io/src/io_uring/file_reader.rs:30-41`,
  `crates/transfer/src/disk_commit/config.rs:73-81`).
- `sparse_likelihood`: source uses `--sparse` or `use_sparse=true`
  (`crates/transfer/src/disk_commit/config.rs:46`).
- `--inplace`: writer overwrites the basis path
  (`crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:71`).
- `--copy-devices` / `--write-devices`: the basis is a block or character
  device (size from `stat` is unreliable, mmap is undefined).
- `--append`: append-mode resume reuses the live destination as basis.
- `basis_on_network_fs`: NFS, SMB, FUSE, sshfs, cephfs. Detected by the
  same probe used in `fast_io::AdaptiveReaderFactory::open`
  (`crates/fast_io/src/mmap_reader.rs:257-275`) - mmap may either fail or
  silently downgrade to read-on-fault.

`B` = `BufferedMap`, `M` = `MmapStrategy`, `RB` = io_uring registered
buffers (read via `submit_read_fixed_batch`).

| `file_size` | `io_uring_active` | `sparse_likelihood` | `--inplace` | `--copy-devices` | `--append` | `basis_on_network_fs` | Strategy | Why |
|---|---|---|---|---|---|---|---|---|
| 0 | any | any | any | any | any | any | `B` (no-op) | `BufferedMap::map_ptr` short-circuits at `len == 0`. |
| < 1 MiB | false | any | false | false | false | false | `B` | Buffered win is dominant under 1 MiB; matches `MMAP_THRESHOLD` (`crates/transfer/src/map_file/mod.rs:55`). |
| < 1 MiB | true | any | any | any | any | any | `B` | Mmap setup cost not amortised, and we never hand the pointer to io_uring. |
| >= 1 MiB | false | false | false | false | false | false | `M` | Pure read of a stable, local basis with no io_uring. Existing `AdaptiveMapStrategy` path. |
| >= 1 MiB | false | true | false | false | false | false | `B` | `--sparse` triggers many small, non-sequential reads in `apply_block_ref`; the sliding window with reuse beats mmap TLB churn. |
| >= 1 MiB | false | any | true | false | false | false | `B` | `--inplace` may have another oc-rsync (or a foreign writer) actively rewriting blocks, exposing SIGBUS. |
| >= 1 MiB | false | any | false | true | false | false | `B` | Devices have no stable size; mmap is undefined for `S_IFBLK`/`S_IFCHR` outside specific drivers. |
| >= 1 MiB | false | any | false | false | true | false | `B` | `--append` opens the destination read-write while we read it as basis - same SIGBUS class as `--inplace`. |
| >= 1 MiB | false | any | false | false | false | true | `B` | NFS/FUSE mmap silently faults via `read` and turns concurrent server-side truncation into `SIGBUS`. Upstream `fileio.c:214-217` cites this exact case. |
| >= 1 MiB | true | any | any | any | any | any | `B` (window) and `RB` (writer side only) | Never mmap when io_uring is active for any I/O on this transfer. The basis read stays buffered; io_uring's registered buffers cover the *output* path only. |

Rule of thumb: **`MmapStrategy` is allowed only when every column except
the first is `false`**. This is intentionally narrow; it preserves the
existing speedup for the local-copy / pull-from-stable-source case
without ever crossing into the io_uring data path or any path that can
truncate under us.

The `AdaptiveMapStrategy` enum
(`crates/transfer/src/map_file/adaptive.rs:21-26`) becomes the matrix's
implementation: it currently picks `Mmap` whenever `size >= MMAP_THRESHOLD`,
which is too permissive. The selector function must consult the new
inputs and downgrade to `Buffered` when any hazard column is true.

## Hazards revisited

These are the F1-F6 findings from `docs/audits/mmap-iouring-co-usage.md`,
restated with the policy clause that prevents each.

### F1 - SIGBUS on basis truncation

`memmap2::Mmap` lazily faults pages from the underlying file. If a third
party (mailer, log rotator, second rsync, the user) truncates the file
before we touch the page, the kernel raises `SIGBUS`. Upstream rsync
calls this out verbatim at `fileio.c:214-217`.

**Policy:** any column predicting third-party writes (`--inplace`,
`--append`, `basis_on_network_fs`) forces `B`. New files transferred to
a stable local destination - the dominant case - keep `M`.

### F2 - mmap pointer submitted to io_uring

A pointer from `MmapReader::as_slice()` looks like a normal `&[u8]` to a
caller in `apply_block_ref`. If a future change pipes that slice into
`submit_read_fixed_batch` or any iovec-taking SQE, the kernel pins the
mmap pages. On truncation, the io_uring worker either kills the
submission or signals back into the rsync process.

**Policy:** when `io_uring_active` is true, no path that touches the
basis file may use `MmapStrategy`. Enforced statically by the selector
returning the `AdaptiveMapStrategy::Buffered` variant; the
`MmapStrategy` constructor is unreachable from the io_uring code paths.

### F3 - registered-buffer aliasing

`RegisteredBufferGroup` allocates page-aligned heap memory and
`IORING_REGISTER_BUFFERS`'s it with the kernel
(`crates/fast_io/src/io_uring/registered_buffers.rs:243-307`). Aliasing
that memory with an `&[u8]` returned from `MapStrategy::map_ptr` would
create a buffer the kernel believes is exclusively owned, but that user
code may concurrently read from a different mmap - undefined behaviour
under the io_uring buffer-registration contract.

**Policy:** `MapStrategy` returns slices into either heap-owned
`Vec<u8>` (`BufferedMap`) or kernel-mapped pages (`MmapStrategy`).
Registered buffers are constructed from a separate, dedicated
allocation in `RegisteredBufferGroup::new` and never share memory with a
mapper. The selector enforces this by ensuring `MmapStrategy` never
co-exists with an `io_uring_active` transfer.

### F4 - cross-thread mmap publication

`memmap2::Mmap` is `Send`, so the lifetime is tied to a Rust borrow but
the kernel mapping outlives any Rust `&[u8]`. If the basis-file mapper
were moved across threads while io_uring read SQEs were in flight (the
disk-commit thread holding `IoUringDiskBatch`), a single munmap-on-drop
would invalidate kernel-pinned addresses.

**Policy:** `MmapStrategy` is constructed and dropped on the same
thread that calls `apply_block_ref`. The disk-commit thread
(`crates/transfer/src/disk_commit/process.rs:26`) does not own a
`MapStrategy` - basis access stays on the network thread. This is true
today and the policy makes it explicit.

### F5 - sparse + mmap interaction

Sparse-aware delta application issues many small `map_ptr` calls at
non-contiguous offsets. With mmap, every fault that hits a hole
materialises a zero page; under memory pressure the kernel may evict
those zero pages and refault repeatedly, costing more than a buffered
read of the same range.

**Policy:** `sparse_likelihood == true` forces `B`. The sliding-window
reuse logic (`buffered.rs:119-141`) handles non-contiguous offsets
gracefully because the holes never enter the window.

### F6 - `MAP_POPULATE` is not a fix

A reasonable instinct is to use `MAP_POPULATE` to prefault all pages at
mmap time, eliminating the late-truncation race. This does not solve
F1: the file can still be truncated *after* `mmap()` returns (the
populated pages get demoted to anonymous if the file shrinks below
them, and subsequent access raises `SIGBUS`). It also adds latency at
open time proportional to file size, defeating the win.

**Policy:** never use `MAP_POPULATE` to attempt to make mmap "safe."
Use `B` instead.

## Implementation hooks

This is a design document only. The intended code-level changes are
sketched here for review; no code changes ship in this PR.

### Selector signature

`AdaptiveMapStrategy::open` currently inspects only file size
(`crates/transfer/src/map_file/adaptive.rs:36-54`). The policy adds
hazard inputs:

```text
// Sketch only - not implemented in this PR.
pub struct BasisMapInputs {
    pub file_size: u64,
    pub io_uring_active: bool,
    pub sparse_likelihood: bool,
    pub inplace: bool,
    pub copy_devices: bool,
    pub append: bool,
    pub basis_on_network_fs: bool,
}

impl AdaptiveMapStrategy {
    pub fn select(path: &Path, inputs: BasisMapInputs) -> io::Result<Self> { ... }
}
```

The selector returns `Buffered` whenever any hazard column is `true`,
and `Mmap` only in the narrow safe case. Existing call sites keep
working: `MapFile::open_adaptive` becomes a wrapper that fills
`BasisMapInputs` with all-`false` hazards (preserving today's behaviour
for callers that have not yet plumbed io_uring).

### DeltaApplicator wiring

`DeltaApplicator::new` (`crates/transfer/src/delta_apply/applicator.rs:83-112`)
currently calls `MapFile::open_adaptive(path)` blindly. Under the
policy, the receive setup passes a `BasisMapInputs` from the same
config that drives `DiskCommitConfig`
(`crates/transfer/src/disk_commit/config.rs:42-81`):

```text
// Sketch only.
let inputs = BasisMapInputs {
    file_size: metadata.len(),
    io_uring_active: disk_config.io_uring_policy.is_active(),
    sparse_likelihood: disk_config.use_sparse,
    inplace: transfer_flags.inplace_enabled,
    copy_devices: transfer_flags.copy_devices,
    append: append_offset > 0,
    basis_on_network_fs: fast_io::detect_network_fs(path),
};
let basis_map = MapFile::with_strategy(AdaptiveMapStrategy::select(path, inputs)?);
```

`fast_io::detect_network_fs` is the only new helper required. It
piggybacks on the existing fallback in
`fast_io::mmap_reader::AdaptiveReaderFactory::open`
(`crates/fast_io/src/mmap_reader.rs:257-275`), which already tolerates
mmap failures on NFS/FUSE - the new helper exposes the detection so we
can downgrade *before* attempting mmap rather than fielding the error.

### Transfer-flag plumbing

`TransferFlags` (`crates/engine/src/local_copy/executor/file/copy/transfer/mod.rs`)
already carries `inplace_enabled`, `use_sparse_writes`, and append data.
The policy needs `--copy-devices` exposed there too. This is one
boolean field added to `TransferFlags` and threaded through
`execute_transfer`
(`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:65-80`).

### Disk-commit invariant

`crates/transfer/src/disk_commit/process.rs:26-104` writes via
`ReusableBufWriter` from owned `Vec<u8>` chunks. The disk thread does
not (and must not) hold a `MapStrategy`. Audit the chunk path on
follow-up to confirm no message variant ever carries an
`MmapStrategy::as_slice()` reference - today it does not, and the
policy preserves that.

## Alternatives rejected

### `madvise(MADV_DONTFORK)`

Prevents child processes from inheriting our mmap. Does nothing about
truncation by another process; does nothing about io_uring. Solves a
problem we do not have (we do not fork after mapping basis files).

### `MAP_POPULATE`

Covered under F6 above. Eager-faults at `mmap()` time, but the file
can still be truncated afterward. Adds open-time latency without
removing the SIGBUS class.

### `MAP_LOCKED` / `mlock(2)`

Would pin pages in RAM, eliminating the *eviction* refault cost (F5)
but not the truncation hazard (F1). Also requires `CAP_IPC_LOCK` or a
generous `RLIMIT_MEMLOCK` and competes with the kernel's page cache
heuristics for io_uring-fronted writes. Net negative.

### Runtime feature flag (`OC_RSYNC_BASIS_MMAP=1`)

A user-facing toggle to opt into mmap looks tempting because it pushes
the safety call to operators. In practice it shifts blame without
reducing risk: the dangerous combinations (mmap + io_uring,
mmap + `--inplace`) are still expressible. The selector's job is to
make those combinations unrepresentable in code; an env var defeats
the whole point. Diagnostic/observability flags (e.g. log which
strategy fired per file at debug level) are fine and orthogonal.

### "Just disable mmap"

Removing `MmapStrategy` entirely would match upstream exactly and
simplify the codebase. We keep it because the safe case is real and
measurable: pulling a stable, local, large basis file from cold cache
is materially faster via mmap than via 256 KiB sliding-window reads.
The policy's job is to fence off the unsafe combinations without
forfeiting that win.

## Test plan

Existing fixtures cover the foundations; new fixtures cover the policy
seams.

### Existing coverage to preserve

- `crates/transfer/src/map_file/tests.rs` - `BufferedMap` window slide,
  reuse on forward slide, EOF clamp.
- `crates/transfer/benches/map_file_benchmark.rs` -
  `MapFile::open` vs `open_adaptive` throughput at multiple sizes.
- `crates/fast_io/src/mmap_reader.rs:284-348` - `AdaptiveReaderFactory`
  threshold dispatch, mmap-on-NFS fallback.
- `crates/fast_io/src/io_uring/registered_buffers.rs:719-1226` - drop
  ordering, panic safety, short-read handling for `READ_FIXED`.

### New tests for the selector

1. **`selector_picks_buffered_when_io_uring_active`** - all hazard
   columns false except `io_uring_active`; expect `Buffered`.
2. **`selector_picks_buffered_for_inplace`** - `inplace=true`; expect
   `Buffered` regardless of size.
3. **`selector_picks_buffered_for_append`** - `append=true`; expect
   `Buffered`.
4. **`selector_picks_buffered_for_devices`** - `copy_devices=true`;
   expect `Buffered`.
5. **`selector_picks_buffered_for_sparse`** -
   `sparse_likelihood=true`; expect `Buffered`.
6. **`selector_picks_mmap_only_when_all_safe`** - size >= 1 MiB and
   every hazard column false; expect `Mmap`.
7. **`selector_picks_buffered_under_threshold`** - size < 1 MiB,
   regardless of other inputs; expect `Buffered` (mmap is never the
   right answer here).

### New integration tests

8. **`delta_apply_with_io_uring_does_not_mmap`** - run a synthetic
   delta apply with `io_uring_policy=Auto`, assert via test seam
   (e.g. expose `MapFile::is_mmap()` already at
   `crates/transfer/src/map_file/wrapper.rs:91-100`) that the basis
   mapper is `Buffered`.
9. **`delta_apply_inplace_does_not_mmap`** - same, with
   `--inplace`.
10. **`delta_apply_truncated_basis_buffered_path`** - third process
    truncates the basis between `open` and `apply_block_ref`; the
    buffered path returns `UnexpectedEof`, never `SIGBUS`. (mmap path
    is intentionally not covered because the policy forbids it in
    this scenario.)
11. **`delta_apply_network_fs_falls_back`** - basis on tmpfs labelled
    via the network-fs detector returns `Buffered` from the
    selector and the transfer succeeds.

### Property test

12. **`selector_invariant_no_mmap_under_hazard`** - quickcheck-style:
    for any input where any hazard column is `true`, the selector
    output is `Buffered`. This is the load-bearing invariant of the
    whole policy.

### Manual probes

- **strace check on Linux**: run a transfer with a 100 MiB basis on
  ext4, `--inplace`, no io_uring; confirm `pread64`/`read` syscalls
  to the basis fd, no `mmap`.
- **strace check, opposite case**: same 100 MiB basis, no `--inplace`,
  no io_uring, no other hazards; confirm exactly one `mmap` to the
  basis fd and zero `read` syscalls against it.

## Follow-up tasks

- Implement `BasisMapInputs` and the new `select` constructor; wire
  through `DeltaApplicator::new`.
- Add `fast_io::detect_network_fs` (statvfs `f_type` on Linux,
  `getmntinfo` on BSD/macOS, GetVolumeInformation on Windows).
- Surface `--copy-devices` on `TransferFlags`.
- Resolve the merge conflict markers currently in
  `crates/fast_io/src/io_uring/registered_buffers.rs:14-85` and
  `:429-475` before any io_uring-related change ships - they will
  block a clean implementation of the policy.
- Add the test list above as a tracking checklist on the
  implementation issue.
