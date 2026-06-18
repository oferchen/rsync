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
