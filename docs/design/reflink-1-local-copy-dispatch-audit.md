# REFLINK-1: Local-Copy Reflink Dispatch Audit

Status: AUDIT (REFLINK-1). Tracks the current state of CoW reflink
primitives in `fast_io`, how the engine local-copy executor currently
calls into them, and what REFLINK-9 must wire to close per-OS gaps.

This is a docs-only inventory. It does not change behaviour.

## Scope

- Local-copy whole-file dispatch only (engine local-copy executor).
- Delta-apply COPY-token reflink ranges are out of scope here (tracked
  by REFLINK-4 / PR #5824).
- CLI surface (`--reflink={auto,always,never}`) is out of scope here
  (tracked by PR #5823 via `RequireCowPlatformCopy`).

## Table 1: Per-OS reflink primitives in `fast_io`

The `PlatformCopy` trait abstracts whole-file copy; `DefaultPlatformCopy`
auto-selects per platform. Standalone helpers wrap each OS's reflink
syscall directly.

| OS | Primitive | Standalone helper | File | Public API | Current callers |
| --- | --- | --- | --- | --- | --- |
| Linux | `FICLONE` ioctl (Btrfs / XFS reflink / bcachefs) | `try_ficlone(src, dst)` | `crates/fast_io/src/platform_copy/mod.rs` (declared, body in `dispatch::try_ficlone_impl`) | `pub fn try_ficlone(src, dst) -> io::Result<()>` | `DefaultPlatformCopy::copy_file` only (via `platform_copy_impl`). No engine call site. |
| macOS | `clonefile(2)` (APFS CoW) | `try_clonefile(src, dst)` | `crates/fast_io/src/platform_copy/mod.rs` (body in `dispatch::clonefile_impl`) | `pub fn try_clonefile(src, dst) -> io::Result<()>` | `DefaultPlatformCopy::copy_file`; engine local-copy `transfer/execute/clonefile.rs` (via `context.options().platform_copy().copy_file`); `engine/src/local_copy/clonefile.rs::try_clonefile` re-export. |
| macOS | `fcopyfile(3)` (kernel-accelerated) | `try_fcopyfile(src, dst)` | `crates/fast_io/src/platform_copy/mod.rs` (body in `dispatch::fcopyfile_impl`) | `pub fn try_fcopyfile(src, dst) -> io::Result<()>` | `DefaultPlatformCopy::copy_file` fallback chain only. |
| Windows | ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | `try_refs_reflink(src, dst)` / `try_refs_reflink_range(...)` | `crates/fast_io/src/platform_copy/mod.rs` (body in `dispatch::try_refs_reflink_impl`) | `pub fn try_refs_reflink(src, dst) -> io::Result<()>` | `DefaultPlatformCopy::copy_file`; engine local-copy `transfer/execute/wincopy.rs`. |
| Windows | `CopyFileExW` (with `COPY_FILE_NO_BUFFERING` >4 MiB) | none separate | `crates/fast_io/src/platform_copy/dispatch.rs` | (internal) | `DefaultPlatformCopy::copy_file`; reported as `CopyMethod::CopyFileEx` to engine. |

`PlatformCopy` reports outcome via `CopyResult { bytes_copied, method }`
plus `is_zero_copy()`; engine callers use this to distinguish a true
CoW reflink from a portable `std::fs::copy` fallback.

## Table 2: Engine local-copy executor dispatch flow

Entry: `crates/engine/src/local_copy/executor/file/copy/mod.rs::copy_file`.
Hands off regular-file transfers to `transfer::execute_transfer` in
`executor/file/copy/transfer/execute/mod.rs`. That module gates two
OS-specific fast paths through `PlatformCopy` before falling into the
generic delta/write-strategy loop.

| Step | File | Function | Calls into reflink? |
| --- | --- | --- | --- |
| 1. Per-file entry | `executor/file/copy/mod.rs` | `copy_file` | No - dispatch to dry-run / link / `execute_transfer`. |
| 2. Transfer orchestrator | `executor/file/copy/transfer/execute/mod.rs` | `execute_transfer` | Indirect: dispatches the OS-gated fast-path arms below. |
| 3a. macOS fast path | `transfer/execute/clonefile.rs` | `clonefile::eligible` + `clonefile::try_clone` | YES - `context.options().platform_copy().copy_file(src, dst, size)`. Commits only when `is_zero_copy()` (true clonefile/FICLONE/ReFS). Falls through on `StandardCopy` or error. |
| 3b. Windows fast path | `transfer/execute/wincopy.rs` | `wincopy::eligible` + `wincopy::try_copy` | YES - same trait call; commits when method is `CopyFileEx` or `ReFsReflink`; falls through on `StandardCopy`. |
| 3c. Linux fast path | (none) | - | NO. There is no `target_os = "linux"` reflink arm in `execute/mod.rs`. Linux FICLONE is reachable only through `DefaultPlatformCopy` when the macOS or Windows arm happens to call `copy_file`, which never fires on Linux. |
| 4. Linux io_uring data-write | `transfer/execute/iouring.rs` | `iouring::eligible` + `iouring::try_dispatch` | NO - registered-buffer write fast path, not a reflink. |
| 5. Generic write strategy | `transfer/execute/write_strategy.rs` and `transfer/execute/mod.rs` continuation | `select_write_strategy` + `copy_file_contents` | NO - read/write loop via `fast_io::copy_file_range::copy_file_contents_buffered`. |

Adjacent: `engine/src/local_copy/clonefile.rs::clone_or_copy` is a thin
helper that calls `PlatformCopy::copy_file` once and reports outcome; it
is reachable from copy-dest / link-dest pre-population paths but is not
the executor entry. It already routes through `DefaultPlatformCopy` so
Linux FICLONE fires from those callers today.

## Table 3: Gap matrix vs PR #5823 / PR #5824

Legend: WIRED = arm exists in `execute_transfer`. AVAILABLE = primitive
exists in `fast_io` and is exposed on `PlatformCopy`. POLICY = will
become respectable once PR #5823's `RequireCowPlatformCopy` lands and
`--reflink=always` can fail loudly instead of falling through.

| OS / FS | Whole-file reflink primitive | Available in `fast_io` | Wired into local-copy executor today | Notes / gaps |
| --- | --- | --- | --- | --- |
| Linux Btrfs / XFS-reflink / bcachefs | FICLONE | YES (`try_ficlone_impl`) | NO - no `#[cfg(target_os = "linux")]` reflink arm in `execute/mod.rs`. Reachable only through `clone_or_copy` adjacent helper. | REFLINK-9 must add a Linux arm symmetric to clonefile.rs and wincopy.rs that calls `PlatformCopy::copy_file` and accepts `is_zero_copy()`. |
| Linux delta-apply COPY tokens | FICLONERANGE | Pending (PR #5824) | NO | Out of REFLINK-1 scope; REFLINK-10 wires the range path. |
| Linux `--reflink=always` enforcement | n/a (policy) | Pending (PR #5823's `RequireCowPlatformCopy`) | NO | Once landed, REFLINK-9 must read the policy and pick `RequireCowPlatformCopy` for `always` instead of `DefaultPlatformCopy`. |
| macOS APFS | clonefile | YES (`clonefile_impl`) | YES via `clonefile::try_clone` | Already commits on `is_zero_copy()`; normalises mode/mtime via `normalize_cloned_metadata`. |
| Windows ReFS | FSCTL_DUPLICATE_EXTENTS | YES (`try_refs_reflink_impl`, `try_refs_reflink_range_impl`) | YES via `wincopy::try_copy` (accepts `ReFsReflink`) | Range variant unused at the executor entry (covered by REFLINK-7 separately). |
| Windows NTFS fallback | CopyFileExW | YES | YES via `wincopy::try_copy` | Not a CoW reflink; included only because `wincopy` accepts the method. |

## Suggested REFLINK-9 wiring (no code change in this PR)

Add a Linux-only sibling submodule `transfer/execute/ficlone.rs`,
declared in `transfer/execute/mod.rs` behind `#[cfg(target_os = "linux")]`
and dispatched after the existing macOS / Windows arms and before the
io_uring data-write arm. Mirror `clonefile::eligible` for preconditions:
no existing destination, `whole_file_enabled`, no inplace, no partial,
no sparse writes, no compression, no bandwidth limiter, no delay-updates,
no temp directory, no copy-source override. Dispatch through
`context.options().platform_copy().copy_file(src, dst, size_hint)`,
treating `Ok(result)` as success only when `result.is_zero_copy()` (so
the `StandardCopy` fallback drops through to the existing write
strategy, identical to clonefile.rs). On commit, call `register_created_path`,
`record_hard_link`, summary recorders, and `finalize_guard_and_metadata`
in the same order as `clonefile::try_clone`; FICLONE preserves source
mode/mtime/xattrs like clonefile, so reuse a Linux-specific
`normalize_cloned_metadata` (or share clonefile.rs's helper behind a
neutral name). Once PR #5823 lands, select between `DefaultPlatformCopy`
and `RequireCowPlatformCopy` based on the `--reflink` policy carried in
`LocalCopyOptions::platform_copy` rather than introducing a new field.
Eligibility-test the change against `engine` nextest under
`--all-features` to keep the existing clonefile and wincopy
fast-path test matrix green; no public API changes are required.
