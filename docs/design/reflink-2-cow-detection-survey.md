# REFLINK-2.a: CoW Filesystem Detection Survey (Linux)

Status: SURVEY (REFLINK-2.a). Formalises the decision record behind the
runtime CoW-filesystem detection helper shipped under REFLINK-2 in
`crates/fast_io/src/platform_copy/cow_detect.rs`. Names the follow-up
sub-tasks (REFLINK-2.b through REFLINK-2.e) that consume this survey.

Related:

- REFLINK-1 audit / REFLINK-2 detection design: `docs/design/reflink-acceleration.md`
- REFLINK-1 dispatch inventory: `docs/design/reflink-1-local-copy-dispatch-audit.md`
- Windows ReFS reflink: `docs/design/windows-refs-reflink.md`

## Problem statement

`FICLONE`/`FICLONERANGE` only succeed on reflink-capable filesystems
(btrfs, XFS with `mkfs.xfs -m reflink=1`, bcachefs). Every other Linux
filesystem - ext4, tmpfs, NFS, FUSE, overlayfs, sysfs, proc - rejects
the ioctl with `EOPNOTSUPP`, `EXDEV`, or `EINVAL`. The reject itself is
cheap, but the dispatch path around it pays real costs:

- A destination file gets created and must be unlinked after the failed
  reflink.
- On a busy directory the syscall round-trip plus filesystem state
  churn is measurable on the sender critical path.

REFLINK-2 needs a cheap, per-mountpoint pre-flight that answers "can
this filesystem ever support reflink?" so the executor can skip the
FICLONE attempt entirely on filesystems that we know reject it. The
REFLINK-1 dispatch audit (`docs/design/reflink-acceleration.md` lines
240-302) confirmed FICLONE is wired today at
`crates/engine/src/local_copy/executor/file/copy/transfer/execute/mod.rs:228`
and is the consumer of this pre-flight.

## Upstream reference

Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`) does
not implement `FICLONE`, `FICLONERANGE`, `statfs(2)`-based filesystem
detection, or any reflink path. A grep for `FICLONE`, `BTRFS_IOC`,
`statfs`, and `reflink` across the upstream sources returns no matches.
Reflink acceleration is an oc-rsync extension that has no upstream
analogue, so there is no upstream wire-format or behaviour to mirror -
only the kernel `ioctl(2)` ABI and the `statfs(2)` magic numbers
documented in `include/uapi/linux/magic.h`.

## Mechanism survey

### 1. `statfs(2)` / `fstatfs(2)` with `f_type` magic

The `f_type` field of `struct statfs` is the kernel super-block magic
number, listed in `include/uapi/linux/magic.h`. Examples:

| FS | `f_type` magic | CoW capable? |
| --- | --- | --- |
| btrfs | `0x9123683E` | Yes (always) |
| bcachefs | `0xCA451A4E` | Yes (always) |
| XFS | `0x58465342` | Only with `mkfs.xfs -m reflink=1` |
| ZFS-on-Linux | `0x2FC12FC1` | Only with dataset `clone_blocks` feature |
| ext4 | `0xEF53` | No |
| tmpfs | `0x01021994` | No |
| NFS | `0x6969` | No |
| FUSE | `0x65735546` | No |
| overlayfs | `0x794C7630` | No |
| proc | `0x9FA0` | No |
| sysfs | `0x62656572` | No |

Pros:

- Single syscall, well under 1 us on a warm cache.
- Returns the kernel's authoritative super-block identity, not a
  user-space proxy.
- `statfs.f_fsid` is unique per mounted filesystem within a mount
  namespace, so it doubles as a stable cache key.
- Available on every Linux kernel oc-rsync supports.

Cons:

- The magic-number table is informal: `linux/magic.h` is the closest
  thing to an index but is not a public ABI promise, and new
  filesystems (or out-of-tree filesystems like ZFS) require manual
  entries.
- XFS reports the same magic whether reflink is enabled at mkfs time
  or not. Same for ZFS-on-Linux datasets - the magic match is
  necessary but not sufficient.

### 2. `statx(2)` with `STATX_ATTR_*` flags

`statx(2)` (kernel >= 4.11) returns a per-inode attribute mask
including `STATX_ATTR_VERITY`, `STATX_ATTR_DAX`, etc. Filesystem-type
discovery is via `stx_mnt_id` plus an out-of-band lookup, not via the
attribute mask directly.

Pros:

- Future-proof for per-inode flags the kernel exposes (e.g. immutable,
  append-only, encrypted).
- `stx_mnt_id` is a stable per-mount identifier.

Cons:

- Does not directly answer "is this filesystem reflink-capable" - no
  `STATX_ATTR_REFLINK` flag exists at any kernel level we target.
- Requires a `statx(2)` plus a mount-table consultation to resolve
  `stx_mnt_id` to a filesystem type, which is more expensive than
  `statfs(2)` alone.
- Kernel >= 4.11 floor is tighter than `statfs(2)` (POSIX) and rules
  out RHEL 7 / kernels older than 4.11.

### 3. Probe-by-attempt (try `ioctl(FICLONE)`)

Create a 0-byte temp file in the destination directory, run
`ioctl(FICLONE, src_fd)` against it, and treat success as proof.

Pros:

- Definitive: there is no false positive. If the ioctl succeeds, the
  filesystem genuinely supports reflink.
- Resolves the XFS-with-reflink and ZFS-with-clone_blocks ambiguity
  that `statfs(2)` cannot.

Cons:

- Requires a writable temp file (side effect on the destination
  directory, even if cleaned up).
- Costs one ioctl + open + unlink per probe - more expensive than any
  static check.
- Cannot be used as the primary mechanism on read-only destinations
  or sandboxed environments where temp-file creation is disallowed.

### 4. `/proc/mounts` (or `/proc/self/mountinfo`) parsing

Read the mount table from `/proc` and match the path's longest-prefix
mount entry. The `fs_type` column gives the filesystem name, and the
mount-options column reveals XFS `reflink=1` directly.

Pros:

- No FS-specific magic-number table to maintain - works off
  human-readable type strings (`btrfs`, `xfs`, `ext4`).
- Mount-options column resolves the XFS / ZFS ambiguity by exposing
  the `reflink=1` / `clone_blocks` options inline.
- Useful as a diagnostic fallback or for `--debug=reflink` logging.

Cons:

- Text parsing: every probe re-reads and re-tokenises a multi-KB file.
  On a host with 1K mounts the cost dominates.
- Races on remount: the file can change mid-read. Re-reading on every
  probe is required for correctness, which compounds the cost.
- Format is not a stable ABI: fields after the `-` separator in
  `mountinfo` change between kernel versions.
- Not available inside chroots or mount namespaces that hide the
  mount table - `statfs(2)` still works in those contexts.

### 5. Caching strategy

Independent of which mechanism above is chosen, the result must be
memoized per-mount to keep the hot path syscall-free.

Two viable keys:

- `statfs.f_fsid` (`u64`): unique per mounted filesystem in a mount
  namespace. Bind-mounts of the same source share fsid, so one entry
  covers all aliases. Requires `statfs(2)` to obtain - so a natural
  fit for mechanism #1.
- `(dev_t, fs_type)` from `stat(2)` + `statfs(2)`: requires two
  syscalls and only saves the `statfs` if we trust the FS-type cache,
  which we cannot if `stat.st_dev` is reused after umount/mount.

`OnceLock<Mutex<HashMap<u64, CowSupport>>>` with no eviction is the
right shape: filesystem type is immutable for the lifetime of a mount,
and an oc-rsync invocation does not see mount changes in practice. A
stale entry across umount/mount would cause one wasted FICLONE attempt
that re-populates the cache as `No` - no correctness hazard.

## Recommendation

**Primary mechanism: `statfs(2)` + magic-number table.** It is the
cheapest single-syscall probe, the magic numbers are stable (the values
in `linux/magic.h` have not changed across kernel versions in the
oc-rsync support window), and `f_fsid` doubles as the cache key.

**Fallback for the necessary-but-not-sufficient case (XFS, ZFS-on-Linux):
probe-by-attempt at the dispatch layer.** The first FICLONE attempt on
an XFS / ZFS mount confirms or refutes reflink support, and the
outcome is written back to the cache via `record_probe_outcome` so
subsequent callers on the same mount skip the FICLONE syscall. This
keeps the survey-step API surface minimal (one `statfs(2)` per cold
mount, zero syscalls per warm mount) while still resolving the XFS
ambiguity correctly.

**Three-valued classification** captures the distinction:

- `CowSupport::Yes`: btrfs, bcachefs - attempt FICLONE directly.
- `CowSupport::No`: ext4, tmpfs, NFS, FUSE, sysfs, ... - skip FICLONE
  entirely.
- `CowSupport::Probable`: XFS, ZFS - attempt FICLONE as a confirming
  probe and cache the result.

Magics not in the table collapse to `No`. That is conservative on
purpose: the cost is a `copy_file_range` fallback on an uncatalogued
CoW filesystem, which we will fix by adding the magic to the table
when we encounter it. The alternative (defaulting to `Probable`) would
generate spurious FICLONE attempts on every unfamiliar filesystem,
which is the cost we are trying to eliminate.

**Rejected: `/proc/mounts` parsing for the hot path.** Re-reading and
re-tokenising on every probe is more expensive than `statfs(2)`, the
format is not stable across kernels, and it does not work inside
restricted mount namespaces. Keep it available as a `--debug=reflink`
diagnostic fallback only.

**Rejected: `statx(2)`-only detection.** No `STATX_ATTR_REFLINK` flag
exists, and the `stx_mnt_id` -> filesystem-type lookup requires
re-parsing the mount table, which inherits all the `/proc/mounts`
drawbacks.

## Follow-up tasks

- **REFLINK-2.b**: Implement the helper. Provide
  `detect_cow_support(path: &Path) -> io::Result<CowSupport>`,
  `detect_cow_filesystem(path: &Path) -> io::Result<bool>` (collapsing
  `Yes`/`Probable` to `true`), and `record_probe_outcome(path, outcome)`
  for after-FICLONE cache writeback. Linux-only; non-Linux stubs return
  `CowSupport::No` / `false`. Lives in
  `crates/fast_io/src/platform_copy/cow_detect.rs`.
- **REFLINK-2.c**: Per-mount caching. Process-wide
  `OnceLock<Mutex<HashMap<u64, CowSupport>>>` keyed by `statfs.f_fsid`.
  `record_probe_outcome` overwrites the slot in place. No eviction.
- **REFLINK-2.d**: Unit tests for the helper and cache. Cover btrfs /
  bcachefs / XFS / ZFS / ext4 / tmpfs / proc magic mappings via a
  test-only hook on `classify`, plus tmpdir / root / missing-path /
  repeated-call / after-probe-overwrite cases. Linux-only tests gated
  behind `#[cfg(target_os = "linux")]`; cross-platform tests confirm
  the non-Linux stub returns `Ok(CowSupport::No)` / `Ok(false)` without
  side effects.
- **REFLINK-2.e**: Update `docs/design/reflink-acceleration.md` with
  the final API surface, magic table, and caching rationale once
  REFLINK-2.b/c/d land. Cross-link this survey from the new
  "Mechanism Survey (REFLINK-2.a)" section.

## Out of scope

This survey records the choice of detection mechanism only. The
following are tracked separately and are NOT in REFLINK-2:

- **REFLINK-3**: FICLONE whole-file dispatch arm in
  `engine::local_copy::executor::file::copy::transfer::execute::ficlone`.
- **REFLINK-4**: FICLONERANGE delta-apply COPY-token reflink ranges.
- **REFLINK-9**: `--reflink={auto,always,never}` CLI wiring against
  `RequireCowPlatformCopy`.
- **REFLINK-10**: in-process bench comparing FICLONE +
  `detect_cow_filesystem` against the `copy_file_range` fallback.
