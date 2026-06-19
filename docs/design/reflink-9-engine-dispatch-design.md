# REFLINK-9: Engine Local-Copy Reflink Dispatch Wiring

Status: DESIGN (REFLINK-9). Codifies the dispatch contract that wires
the Linux `FICLONE` reflink primitive into the local-copy executor and
locks in the symmetric shape macOS `clonefile(2)` (REFLINK-8) and
Windows ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (REFLINK-6) already
follow. The Linux dispatch arm referenced here is live in master; this
document captures the contract so that REFLINK-4 (`FICLONERANGE`
delta-apply range clone, sometimes referred to as REFLINK-10) and
REFLINK-13 (the `--reflink={auto,always,never}` CLI gate) land against
a stable interface.

Related:

- REFLINK-1.e foundation: `docs/design/reflink-acceleration.md`
- REFLINK-1.a-c inventory: `docs/design/reflink-1-local-copy-dispatch-audit.md`
- REFLINK-2.a survey: `docs/design/reflink-2-cow-detection-survey.md`
- Windows ReFS reflink: `docs/design/windows-refs-reflink.md`

## Scope

Executor-side dispatch contract for **whole-file** reflink (`FICLONE`
on Linux, `clonefile(2)` on macOS, `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
on Windows ReFS). Does NOT cover range-clone (`FICLONERANGE`) for
delta-apply `COPY` tokens; that lives in the delta-apply path and is
REFLINK-4 / REFLINK-10.

The Linux dispatch arm has already shipped under the REFLINK-3 series:
the wrapper REFLINK-3.a is `fast_io::try_ficlone` at
`crates/fast_io/src/platform_copy/mod.rs:239`; the executor impl
REFLINK-3.b is
`crates/engine/src/local_copy/executor/file/copy/transfer/execute/ficlone.rs::try_clone`,
invoked from
`.../execute/mod.rs:228-249`. This document records the contract those
shipped pieces realise so REFLINK-13 (CLI gate) and REFLINK-4 / 10
(range clone) extend it without drift.

## Dispatch insertion point

`execute_transfer` in
`crates/engine/src/local_copy/executor/file/copy/transfer/execute/mod.rs`
is the single dispatch site for regular-file local copies. The function
fans out to per-OS fast-path arms before falling through to the generic
`open_source_file` + `copy_file_contents` read/write loop. The arms,
in dispatch order:

1. macOS `clonefile(2)` (`#[cfg(target_os = "macos")]`, lines 173-195).
2. Windows `CopyFileExW` + ReFS reflink (`#[cfg(target_os = "windows")]`,
   lines 197-223).
3. Linux `FICLONE` (`#[cfg(target_os = "linux")]`, lines 225-249).
4. Linux `io_uring` registered-buffer write path (when the
   `iouring-data-writes` feature is enabled).
5. Generic `copy_file_contents` / `copy_file_range` fallback.

Each reflink arm shares the same two-step shape:

```rust
if <platform>::eligible(context, existing_metadata, flags, copy_source_override.is_some())
    && <platform>::try_clone(context, source, destination, /* ... */)?
{
    return Ok(());
}
```

`eligible` is a cheap predicate over `TransferFlags` and `CopyContext`;
`try_clone` performs the platform call and returns
`Result<bool, LocalCopyError>` where `Ok(false)` means soft fall-through
and `Err(_)` means the reflink succeeded but a follow-up bookkeeping
step failed (a real abort).

## Decision flow at runtime

The dispatch contract is identical across platforms; only the underlying
primitive changes.

1. Caller invokes `execute_transfer` with `(source, destination)`,
   `TransferFlags`, `CopyContext`.
2. Dispatcher evaluates the platform `eligible(...)` predicate; on
   Linux see `transfer/execute/ficlone.rs::eligible` lines 41-82.
3. If eligible, the dispatcher consults
   `fast_io::platform_copy::cow_detect::detect_cow_support(parent)`
   (REFLINK-2.b). Cache is `OnceLock<Mutex<HashMap<u64, CowSupport>>>`
   keyed by `statfs.f_fsid`, so the second probe on any path under the
   same mount is syscall-free. Outcomes: `Yes` (btrfs, bcachefs) ->
   attempt directly; `Probable` (XFS, ZFS) -> attempt as confirming
   probe, write result back via `record_probe_outcome`; `No`
   (ext4/tmpfs/NFS/FUSE/overlayfs/sysfs/unknown) -> skip and fall
   through. See `docs/design/reflink-2-cow-detection-survey.md` lines
   178-205.
4. On reflink success: zero bytes move through user space. The
   executor records summary + change set then proceeds to metadata-only
   apply via `finalize_guard_and_metadata`. `normalize_cloned_metadata`
   (`transfer/execute/ficlone.rs:197-229`) resets mtime to "now" under
   `--no-times` so observable results match the regular copy path.
5. On reflink failure (`ENOTSUP`, `EXDEV`, `EOPNOTSUPP`, `EINVAL`,
   `EPERM`, read-only FS, cross-device): `try_clone` unlinks the
   half-created destination and returns `Ok(false)`; the dispatcher
   continues to the next arm. A `debug_log!` entry records the
   soft-fail for `--debug=reflink`.

The src/dst FS-id pair is one `statfs(2)` per `f_fsid`, process-wide
cached in `cow_detect`. No dispatcher-level cache is needed - the
per-mount cache is the single source of truth.

## Eligibility predicate (`eligible`)

`eligible` is a constant-time predicate over `TransferFlags` and
`CopyContext`. It MUST stay aligned across platforms - divergence
would let `clonefile` accept a transfer that `ficlone` rejects (or
vice versa). Refuses the reflink fast path when ANY of:
`existing_metadata.is_some()`, `!whole_file_enabled`, `inplace_enabled`,
`partial_enabled`, `use_sparse_writes`, `compress_enabled`,
`copy_source_override_present`, `context.has_bandwidth_limiter()`,
`context.delay_updates_enabled()`, `context.temp_directory_path().is_some()`,
or (under `feature = "xattr"`) the filter program has xattr rules. New
arms (e.g. REFLINK-4 range clone) MUST keep matching this shape or
split it explicitly; silent divergence breaks `--no-whole-file`,
`--partial`, `--inplace`, and `--sparse` invariants.

## `try_clone` interface

```rust
pub(super) fn try_clone(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_type: fs::FileType,
    relative: Option<&Path>,
    mode: LocalCopyExecution,
    flags: TransferFlags,
) -> Result<bool, LocalCopyError>;
```

Return values:

- `Ok(true)`: reflink succeeded; the executor returns immediately from
  `execute_transfer`. Bookkeeping (summary counters, change set, hard
  link tracking, batch-file finalization, metadata apply) ran cleanly.
- `Ok(false)`: reflink rejected by the kernel (any of `ENOTSUP`,
  `EXDEV`, `EOPNOTSUPP`, `EINVAL`, `EPERM`, `EACCES`); destination has
  been unlinked; caller falls through to the next dispatch arm.
- `Err(LocalCopyError::Io { .. })`: reflink succeeded but a follow-up
  bookkeeping step (metadata apply, change-set record, batch
  finalization) failed; the transfer aborts with the usual error
  propagation.

The `Ok(false)` vs `Err(_)` split is the contract that lets the
dispatcher remain straight-line. Future arms MUST preserve it.

## CLI gate (REFLINK-13)

The `--reflink={auto,always,never}` flag tracked by REFLINK-13 (the
`RequireCowPlatformCopy` knob in the REFLINK-1 inventory) gates
dispatch:

- `auto` (default): evaluate `eligible` + the CoW pre-flight; soft-fail
  falls through silently. Current behaviour.
- `always`: evaluate `eligible` (an ineligible transfer becomes a user
  error), then attempt the platform reflink. Soft-fail becomes
  `LocalCopyError::ReflinkRequired { src, dst, reason }` instead of a
  fall-through.
- `never`: skip the reflink arm entirely. `eligible` is not evaluated,
  `cow_detect` is not consulted, `try_clone` is not called. Falls
  straight through to io_uring / generic write.

The flag flows CLI parse (`crates/cli`) -> `CoreConfig` (`crates/core`)
-> `CopyContext` as a `ReflinkPolicy` field. The dispatcher reads it
once at the top of `execute_transfer`. The `never` branch MUST be a
single boolean check at the dispatch site - not pushed into `eligible`
- so "reflink path completely disabled" stays trivially auditable.

## Cross-platform symmetry

| Platform | Primitive | Eligibility module | Pre-flight | Status |
| --- | --- | --- | --- | --- |
| Linux | `ioctl(FICLONE)` | `transfer/execute/ficlone.rs` | `cow_detect::detect_cow_support` (REFLINK-2.b) | SHIPPED |
| macOS APFS | `clonefile(2)` | `transfer/execute/clonefile.rs` | none (cross-volume returns `EXDEV` from the syscall) | SHIPPED (REFLINK-8) |
| Windows ReFS | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | `transfer/execute/wincopy.rs` | `refs_detect::is_refs_filesystem` | SHIPPED (REFLINK-6) |

This spec is Linux-focused; the `eligible` predicate and
`try_clone(...) -> Result<bool, LocalCopyError>` contract are
intentionally identical across platforms. macOS (REFLINK-8) and
Windows ReFS (REFLINK-6) are already implemented but should reconfirm
against this contract when REFLINK-13 lands so the `auto/always/never`
gate threads through all three arms identically.

## Interaction with delta-apply COPY tokens

This task is whole-file only. The delta-apply path (generator emits
`MATCH(block)` + `COPY(range)` tokens, receiver reconstructs from the
basis) is REFLINK-4 / REFLINK-10. The contract there will be: whole-
basis reflink fails the eligibility check (the destination is
reconstructed range-by-range, not cloned); each `COPY` range becomes a
`FICLONERANGE` candidate when `cow_detect` reports `Yes`/`Probable`
AND range alignment satisfies the kernel block size. The decision lives
in the COPY-token writer, not `execute_transfer`. The two arms share
the same `cow_detect` `f_fsid` cache but live in separate modules.
This spec only commits to NOT introducing a parallel `same_fs(src,
dst)` predicate; REFLINK-4 / REFLINK-10 MUST reuse `cow_detect`.

## Benchmark harness pointer (REFLINK-11)

REFLINK-11 owns the in-process bench comparing FICLONE +
`detect_cow_filesystem` against the `copy_file_range` fallback across
file counts and sizes, captured under `crates/engine/benches`. This
spec sets no thresholds; once REFLINK-11 publishes numbers the `auto`
default may be revisited (e.g. gating on minimum file size when
FICLONE setup cost dominates sub-4 KiB files). Any such change ships
as a separate PR with bench evidence.

## Blockers and follow-ups

- **REFLINK-3.a** (FICLONE ioctl wrapper): SHIPPED, `fast_io::try_ficlone`.
- **REFLINK-3.b** (whole-file impl): SHIPPED, `ficlone::try_clone`.
- **REFLINK-4 / REFLINK-10** (range dispatch for delta-apply COPY
  tokens): pending. Reuses `cow_detect`; lives in the delta-apply path,
  not `execute_transfer`.
- **REFLINK-11** (bench harness): pending.
- **REFLINK-13** (CLI flag): pending. Wires
  `--reflink={auto,always,never}` from CLI parse through `CoreConfig`
  into the dispatch site per "CLI gate" above.

## Out of scope

- `FICLONERANGE` delta-apply path (REFLINK-4 / REFLINK-10).
- macOS-specific wiring details (REFLINK-8).
- Windows ReFS-specific wiring details (REFLINK-6).
- A generic `same_fs(src, dst) -> bool` helper. The per-mechanism
  pre-flights (`cow_detect` on Linux, `refs_detect` on Windows,
  `EXDEV` from `clonefile(2)` on macOS) already cover the same-FS
  question at the right granularity; introducing a fourth gate would
  duplicate state.
