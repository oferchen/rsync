# Reflink Acceleration

Status: FOUNDATION (REFLINK-2). Tracks the runtime CoW filesystem
detection layer that the REFLINK-3 (`FICLONE` whole-file), REFLINK-4
(`FICLONERANGE` delta-apply COPY-token), and REFLINK-9 (engine
local-copy wiring) tiers build on. This document does not specify the
FICLONE wiring itself.

Related:
- REFLINK-1 inventory: `docs/design/reflink-1-local-copy-dispatch-audit.md`
- Windows ReFS reflink: `docs/design/windows-refs-reflink.md`

## Motivation

`FICLONE`/`FICLONERANGE` succeed only on reflink-capable filesystems
(btrfs, XFS with `mkfs.xfs -m reflink=1`, bcachefs, ZFS-on-Linux with
`clone_blocks`). Every other filesystem - ext4, tmpfs, NFS, FUSE,
overlayfs, sysfs - rejects the ioctl with `EOPNOTSUPP`, `EXDEV`, or
`EINVAL`. The reject is cheap on its own, but the dispatch path pays
extra costs around it: a destination file gets created (and must be
unlinked after the failed reflink), and on a busy directory the
syscall round-trip plus filesystem state churn is measurable on the
sender critical path.

A cheap per-mountpoint pre-flight that says "this filesystem cannot
ever support reflink" lets the dispatch skip those costs and fall
straight through to the next tier (`copy_file_range` on Linux,
`std::fs::copy` everywhere else).

## Mechanism Survey (REFLINK-2.a)

Two viable Linux mechanisms for "what filesystem is this path on":

| Mechanism | Pros | Cons | Verdict |
| --- | --- | --- | --- |
| `statfs(2)` / `fstatfs(2)` with the `f_type` magic | One syscall per probe; returns the kernel's authoritative super-block magic; pairs naturally with `f_fsid` for cache keying. | Magic-to-name mapping is informal (no public header lists all of them); XFS reports the same magic whether reflink is enabled or not. | **Picked.** Cheapest, most direct, and the XFS ambiguity is handled at the dispatch layer via a single FICLONE confirming probe. |
| Parse `/proc/self/mountinfo` and match by `fs_type` string | No syscall per probe (file read); includes mount options like `crc=1,reflink=1` for XFS so the ambiguity collapses. | mountinfo is line-oriented text; parsing every line on a 1K-mount host costs more than `statfs(2)`; format changes between kernels (e.g. fields after the dash separator); not available inside chroots / mount namespaces that hide the mount table. | Rejected for the hot path; kept as a possible diagnostic fallback. |

Reference: `statfs(2)` man page; magic constants in
`include/uapi/linux/magic.h` in the Linux kernel tree.

## API (REFLINK-2.b)

`crates/fast_io/src/platform_copy/cow_detect.rs` exposes:

```rust
pub enum CowSupport {
    Yes,       // btrfs, bcachefs
    No,        // ext4, tmpfs, NFS, FUSE, overlayfs, sysfs, proc, ...
    Probable,  // XFS, ZFS - confirm with a single FICLONE probe
}

impl CowSupport {
    pub fn may_attempt_reflink(self) -> bool { ... }
}

pub fn detect_cow_support(path: &Path) -> io::Result<CowSupport>;
pub fn detect_cow_filesystem(path: &Path) -> io::Result<bool>;
pub fn record_probe_outcome(path: &Path, outcome: CowSupport) -> io::Result<()>;
```

`detect_cow_filesystem` is the boolean front door for the REFLINK-3/4
wiring; it collapses `Yes` and `Probable` to `true` and `No` to
`false`. Callers that need to distinguish "attempt FICLONE and treat
failure as a real error" from "attempt FICLONE as a confirming probe
and cache the outcome" use `detect_cow_support` instead.

### Magic table

| FS | Magic | Mapping |
| --- | --- | --- |
| btrfs | `0x9123683E` | `Yes` |
| bcachefs | `0xCA451A4E` | `Yes` |
| XFS | `0x58465342` | `Probable` (reflink gated on `-m reflink=1` at mkfs) |
| ZFS-on-Linux | `0x2FC12FC1` | `Probable` (reflink gated on dataset `clone_blocks` feature) |
| ext4 | `0xEF53` | `No` |
| tmpfs | `0x01021994` | `No` |
| proc | `0x9FA0` | `No` |
| sysfs | `0x62656572` | `No` |
| NFS | `0x6969` | `No` |
| FUSE | `0x65735546` | `No` |
| overlayfs | `0x794C7630` | `No` |

Magics not in this list collapse to `No`. That is conservative on
purpose - it costs us nothing more than a `copy_file_range` fallback
on a CoW filesystem we have not catalogued, and it avoids spurious
FICLONE attempts on filesystems whose reflink semantics differ from
the Linux kernel's.

### Platform stubs

- **Linux**: full `statfs(2)`-backed probe and cache, as described
  above.
- **Non-Linux** (macOS, Windows, FreeBSD, illumos): the public
  functions are still defined and link, but `detect_cow_support`
  always returns `Ok(CowSupport::No)`, `detect_cow_filesystem` always
  returns `Ok(false)`, and `record_probe_outcome` is a no-op.
  Cross-platform callers do not need `#[cfg]` branching at the call
  site. macOS APFS reflink is handled by the `clonefile` dispatch
  layer in `platform_copy::dispatch`, and Windows ReFS reflink is
  handled by `fast_io::refs_detect` plus the `try_refs_reflink`
  dispatch arm; neither path consults `cow_detect`.

## Caching (REFLINK-2.c)

A process-wide `OnceLock<Mutex<HashMap<u64, CowSupport>>>` keyed by
the `statfs.f_fsid` value returned by the kernel. `f_fsid` is unique
per mounted filesystem within a Linux mount namespace - bind mounts
of the same source filesystem share the same fsid, so a single cache
entry covers all bind-mount aliases.

- First probe on a mountpoint: 1 `statfs(2)` syscall, populates the
  cache.
- Subsequent probes on any path within the same mountpoint: pure
  in-memory `HashMap` lookup, no syscall.
- `record_probe_outcome` lets the dispatch layer overwrite the
  cached value after a confirming FICLONE attempt resolves the
  XFS / ZFS ambiguity. The slot is overwritten in place, so the
  next caller on the same mount skips the FICLONE syscall too.

### Key choice rationale

`f_fsid` was picked over `(dev_t, fs_type)` from `stat(2)`. The
trade-off:

- `stat(2)` returns `st_dev` which is unique per *device*, but a
  device can host multiple filesystems via subvolumes (btrfs) or
  partitions, and the FS type is reported separately via
  `f_type` from `statfs(2)` anyway. So we are calling `statfs(2)`
  regardless, and using its `f_fsid` saves one syscall over the
  `stat(2)` + `statfs(2)` pairing.
- `f_fsid` is opaque (kernel-internal) and we treat it that way;
  the cache key is the raw 8 bytes interpreted as a `u64` in
  native byte order. The interpretation matters only for hash
  uniqueness, not for any semantic meaning.

### Eviction

None. The cache lives for the process lifetime. Filesystem type is
immutable for a mounted filesystem - it cannot change without
unmounting and remounting, at which point the `f_fsid` changes too.
An rsync invocation never sees a mount change in practice, and even
if it did, the stale cache entry would only cause a benign FICLONE
attempt failure that gets cached as `No`.

## Tests (REFLINK-2.d)

`crates/fast_io/src/platform_copy/cow_detect.rs::tests` covers:

- `detect_handles_tmpdir_without_error` - the probe runs on the
  process tmpdir and returns one of the three states without
  panicking. Cross-platform.
- `detect_cow_filesystem_handles_tmpdir` - the boolean front door
  agrees with `detect_cow_support(...).may_attempt_reflink()`.
- `detect_handles_root_without_error` - the probe runs on `/`.
  Linux-only.
- `detect_cache_is_consistent_for_repeated_calls` - the second
  call returns the same answer as the first. Cross-platform.
- `detect_returns_enoent_for_missing_path` - missing paths
  surface `ErrorKind::NotFound` cleanly. Linux-only.
- `classify_known_magics` - btrfs/bcachefs map to `Yes`, XFS/ZFS
  to `Probable`, ext4/tmpfs/proc to `No`. Reached via a test-only
  hook so the magic constants and the `classify` function stay
  private to the Linux `imp` submodule. Linux-only.
- `record_probe_outcome_overrides_classification` - the dispatch
  layer's after-probe write is honoured by the next lookup.
  Linux-only.
- `may_attempt_reflink_collapses_correctly` - the boolean
  collapse maps `Yes`/`Probable` to `true` and `No` to `false`.
  Cross-platform.
- `non_linux_stub_always_returns_false` - the non-Linux stub
  returns `Ok(CowSupport::No)` / `Ok(false)`. macOS/Windows only.
- `non_linux_record_probe_is_noop` - the non-Linux
  `record_probe_outcome` does not error and does not alter the
  answer. macOS/Windows only.

## Out of scope (NOT in REFLINK-2)

This PR ships the detection layer only. The following are tracked
separately:

- **REFLINK-3**: FICLONE whole-file dispatch arm in
  `engine::local_copy::executor::file::copy::transfer::execute::ficlone`,
  consuming `detect_cow_filesystem` for pre-flight gating and
  `record_probe_outcome` for after-FICLONE caching.
- **REFLINK-4**: FICLONERANGE delta-apply COPY-token reflink ranges,
  symmetric to ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` ranges
  (PR #5824).
- **REFLINK-9**: wiring `RequireCowPlatformCopy` against the
  `--reflink={auto,always,never}` CLI flag.
- **REFLINK-10**: in-process bench comparing FICLONE +
  `detect_cow_filesystem` against the existing `copy_file_range`
  fallback at a range of file counts and sizes.

## REFLINK-1: Local-copy dispatch audit (current master)

Refreshes the stale `docs/design/reflink-1-local-copy-dispatch-audit.md`
inventory against actual master. The earlier audit asserted "no Linux
FICLONE arm in `execute_transfer`"; that gap has since been closed.
This section reflects the production code in
`crates/engine/src/local_copy/executor/file/copy/`.

### Current dispatch site

Entry: `crates/engine/src/local_copy/executor/file/copy/mod.rs::copy_file`
hands regular-file transfers to `execute_transfer` in
`crates/engine/src/local_copy/executor/file/copy/transfer/execute/mod.rs`.
That module evaluates per-OS fast-path arms before falling through to
`open_destination_writer` + `copy_file_contents`. Quoting the dispatch
block (lines 173-249):

```rust
// Fast path: macOS clonefile for new whole-file copies.
#[cfg(target_os = "macos")]
if clonefile::eligible(...) && clonefile::try_clone(...)? {
    return Ok(());
}
// Fast path: Windows CopyFileExW / ReFS reflink for new whole-file
#[cfg(target_os = "windows")]
if wincopy::eligible(...) && wincopy::try_copy(...)? {
    return Ok(());
}
// Fast path: Linux FICLONE reflink for new whole-file copies on
// Btrfs, XFS (reflink enabled), and bcachefs.
#[cfg(target_os = "linux")]
if ficlone::eligible(...) && ficlone::try_clone(...)? {
    return Ok(());
}
```

Submodules: `transfer/execute/clonefile.rs`, `transfer/execute/wincopy.rs`,
and `transfer/execute/ficlone.rs` are all wired today.

### Copy-method inventory matrix

Matrix of whole-file copy primitives exposed by `fast_io` and their
status in the local-copy executor (not counting delta or read-loop
paths). Wired = arm exists in `execute_transfer`. Available = primitive
exists in `fast_io` but is not reached directly from the executor.

| Method | Platform | Wired into executor? | fast_io entry | Notes |
| --- | --- | --- | --- | --- |
| `clonefile(2)` | macOS APFS | YES (`transfer/execute/clonefile.rs::try_clone`) | `PlatformCopy::copy_file` -> `dispatch::clonefile_impl` | Zero-copy; gated on `is_zero_copy()`; falls through on `StandardCopy`. |
| `fcopyfile(3)` | macOS | NO (executor) / YES (DefaultPlatformCopy fallback) | `dispatch::fcopyfile_impl` | Kernel-accelerated data-copy fallback when clonefile is ineligible. |
| `ioctl(FICLONE)` | Linux Btrfs/XFS-reflink/bcachefs | YES (`transfer/execute/ficlone.rs::try_clone`) | `fast_io::try_ficlone` -> `dispatch::try_ficlone_impl` | Pre-gated by `cow_detect::detect_cow_support` (REFLINK-2); EOPNOTSUPP/EXDEV/EINVAL cached and translate to fall-through. |
| `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | Windows ReFS | YES (`transfer/execute/wincopy.rs::try_copy`, accepts `ReFsReflink`) | `fast_io::try_refs_reflink` | Pre-gated by `refs_detect::is_refs_filesystem`. |
| `CopyFileExW` | Windows | YES (`wincopy::try_copy`, accepts `CopyFileEx`) | `PlatformCopy::copy_file` -> `dispatch::copy_file_ex_impl` | Kernel-side data copy with `COPY_FILE_NO_BUFFERING` for files > 4 MiB. |
| `copy_file_range(2)` | Linux | NO direct arm; reached via the generic `copy_file_contents_buffered` loop | `fast_io::copy_file_range::copy_file_contents_buffered` | Last-resort read/write loop fallback after FICLONE/iouring arms decline. |
| `sendfile(2)` | Linux/macOS (socket-target) | NO (engine local-copy is file-to-file) | `fast_io::platform_sendfile` / `sendfile_macos` | Used in network sender path, not the local-copy executor. |
| `splice(2)` | Linux (pipe-target) | NO | `fast_io::splice` | Network sender path only. |
| `vmsplice(2)` | Linux | NO | `fast_io::vmsplice_writer` | Network sender path only. |
| `io_uring registered-buffer writes` | Linux (`iouring-data-writes` feature) | YES (`transfer/execute/iouring.rs::try_dispatch`) | `fast_io::io_uring_ops` | Not a reflink; routes after the FICLONE arm declines and before the generic write strategy. |
| `std::io::copy` portable read/write | All | NO direct arm; reached via `copy_file_contents` | std | Final fallback inside `select_write_strategy`. |

### Recommended reflink insertion point (REFLINK-9 already landed here)

The insertion point that REFLINK-3/9 chose - and that REFLINK-1 should
reconfirm rather than relocate - is
`crates/engine/src/local_copy/executor/file/copy/transfer/execute/mod.rs:228`,
where the `#[cfg(target_os = "linux")]` FICLONE arm sits between the
Windows `wincopy::try_copy` arm and the `open_source_file` /
delta-signature continuation. Any additional reflink dispatch (for
example a future cross-fs `copy_file_range`-as-reflink probe under
REFLINK-4) should reuse the same gating shape - `eligible` returning
`bool` against `TransferFlags`, `try_clone` returning
`Result<bool, LocalCopyError>` so a soft-fail falls through to the
existing generic write path. The eligibility predicate must keep
matching `clonefile::eligible` and `wincopy::eligible`: new
destination, whole-file enabled, no inplace / partial / sparse /
compression / bandwidth limiter / delay-updates / temp-dir / copy-source
override. Diverging from that shape risks breaking the `--no-whole-file`
and `--partial` invariants the existing arms enforce.

### Same-filesystem detection assessment

`fast_io` does not implement a generic `same_fs(src, dst)` helper that
compares `st_dev`. Instead it relies on per-mechanism guard layers:

- **Linux FICLONE**: pre-flighted by
  `fast_io::platform_copy::cow_detect::detect_cow_support(parent)` and
  cached per `statfs.f_fsid` (REFLINK-2). EXDEV / EOPNOTSUPP / EINVAL
  outcomes are written back via `record_probe_outcome` so the next
  caller on the same mount skips the ioctl. Cross-fs source/destination
  surfaces as `EXDEV` from the kernel and gets cached.
- **Windows ReFS reflink**: pre-flighted by
  `fast_io::refs_detect::is_refs_filesystem(path)` which calls
  `GetVolumeInformationByHandleW` and caches results keyed on volume
  root path in a process-wide `Mutex<HashMap<PathBuf, bool>>`.
- **macOS clonefile**: cross-volume calls return `EXDEV` from
  `clonefile(2)`; the wrapper returns the error, the executor's
  `try_clone` treats any error as fall-through, and there is no cached
  same-volume predicate.

Recommendation: keep this shape. A generic `same_fs(src, dst)` helper
that compares `st_dev` (Linux/macOS) or volume serial number (Windows)
would let the executor short-circuit before opening files, but the
existing per-mechanism caching already collapses the ioctl cost on
subsequent calls. If REFLINK-4 (FICLONERANGE) lands a partial-reflink
path that needs the same gate, share `cow_detect::detect_cow_support`
rather than introducing a parallel `st_dev` cache. Windows ReFS
detection is already cached per volume root, which is the right
granularity for FSCTL_DUPLICATE_EXTENTS.

### Sequencing for REFLINK-3 / REFLINK-9 (status)

- REFLINK-3 (FICLONE whole-file dispatch arm) - SHIPPED. Lives at
  `transfer/execute/ficlone.rs`; the executor calls it at
  `transfer/execute/mod.rs:228-249`.
- REFLINK-9 (executor wiring) - SHIPPED in the same series as
  REFLINK-3. The symmetry with `clonefile::try_clone` and
  `wincopy::try_copy` is intentional and should be preserved when
  REFLINK-4 lands.
- REFLINK-4 (FICLONERANGE for delta-apply COPY tokens) - still pending.
  When it lands, the call site is the delta-apply COPY-token writer in
  the engine, not `execute_transfer`. The same-fs gate should reuse
  `cow_detect`.
- `--reflink=auto|always|never` (PR #5823's `RequireCowPlatformCopy`) -
  still pending. Once it lands, both `transfer/execute/ficlone.rs` and
  `transfer/execute/wincopy.rs` should switch between
  `DefaultPlatformCopy` and `RequireCowPlatformCopy` based on the
  parsed `LocalCopyOptions::platform_copy` value rather than
  introducing a new field.

The stale `docs/design/reflink-1-local-copy-dispatch-audit.md`
recommendation block predates the FICLONE-arm landing; this section
supersedes it.
