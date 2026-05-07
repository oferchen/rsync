# Metadata Apply Time at 100K Files

Task: #1046 - profile per-file metadata application time at the 100K-file
scale and identify reductions before benchmarking.

## Scope

This audit traces every syscall that fires when the receiver re-applies
ownership, permissions, timestamps, ACLs, and extended attributes for a
single file or directory entry, summarises the parallelism that already
exists in the receiver, and proposes reductions that can be tested before
the next interop benchmark.

The receiver-side entry points covered are:

- `metadata::apply_file_metadata` family and the `FileEntry`-driven
  `apply_metadata_from_file_entry` flow.
- The directory-batch wiring in
  `crates/transfer/src/receiver/directory/creation.rs`.
- The per-file finalisation path in
  `crates/transfer/src/disk_commit/process.rs::apply_metadata_acls_and_xattrs`.

## 1. Apply Flow

`crates/metadata/src/apply/mod.rs` is the orchestration facade. It
re-exports four families of operations, each with `_with_options`,
`_with_fd`, and `_if_changed` variants:

- `apply_directory_metadata_with_options` - directories.
- `apply_file_metadata_with_options` / `apply_file_metadata_with_fd` -
  regular files.
- `apply_symlink_metadata_with_options` - symlinks (`AT_SYMLINK_NOFOLLOW`,
  no chmod).
- `apply_metadata_from_file_entry` /
  `apply_metadata_with_attrs_flags` - protocol-driven path used by the
  receiver.

All variants call into three submodules in the same upstream-mandated
order:

1. `ownership::set_owner_like` or `apply_ownership_from_entry`
   (`rsync.c:set_file_attrs()` chown stage).
2. `permissions::apply_permissions_with_chmod[_fd]` or
   `apply_permissions_from_entry`.
3. `timestamps::set_timestamp_like` / `set_timestamp_with_fd` /
   `apply_timestamps_from_entry`, then crtime when `--crtimes` is set.

ACLs and xattrs are applied **outside** `apply/mod.rs` by the caller, in
the order `metadata -> ACLs -> xattrs`:

- Disk-commit path: `disk_commit/process.rs::apply_metadata_acls_and_xattrs`
  invokes `metadata::apply_metadata_from_file_entry`,
  `metadata::apply_acls_from_cache`, then
  `metadata::apply_xattrs_from_list`.
- Streaming receiver path: `receiver/transfer.rs:445-463` runs the same
  three steps inline.
- Directory creation path: `receiver/directory/creation.rs:122-148` runs
  them inside the closure that `parallel_io::map_blocking` evaluates.

`apply_metadata_from_file_entry` issues an unconditional
`fs::metadata(destination)` (one extra `lstat`/`stat` syscall) before
delegating to `apply_metadata_with_cached_stat`. The fd-aware variants
(`apply_file_metadata_with_fd[_if_changed]`) skip that probe but only
fire while a `BorrowedFd` is in scope - the streaming receiver path drops
the fd before metadata application and so always pays the extra `stat`.

## 2. Per-File Syscalls

Every entry that requires preservation produces the following syscalls
on a Linux destination with default flags (`-a -X -A`):

| Stage | Syscall | Source | Skip Conditions |
|-------|---------|--------|------------------|
| Probe | `lstat` / `statx` | `apply_metadata_from_file_entry` | none in current code |
| Owner | `chownat` (`AT_SYMLINK_NOFOLLOW` for symlinks) | `apply/ownership.rs:110` | both `uid` and `gid` `None`, or `ownership_matches(existing)` |
| Perms | `fchmod` (fd path) or `chmod` (path) | `apply/permissions.rs:130-166` | `permissions_match(mode, existing)` |
| Times | `utimensat` (`futimens` on fd, `lutimensat` on symlinks) | `apply/timestamps.rs:24-87` | mtime equal to `existing` (atime ignored unless `--atimes`) |
| ACL access | `acl_set_file(ACL_TYPE_ACCESS)` via `exacl` | `acl_exacl::apply_acls_from_cache` | `acl_ndx() == None` or symlink |
| ACL default | `acl_set_file(ACL_TYPE_DEFAULT)` | same | non-directory or `def_acl_ndx() == None` |
| Xattr probe | `llistxattr` | `xattr_unix::apply_xattrs_from_list` | `XattrList` empty |
| Xattr write | `lsetxattr` per attribute | same | per-name diff |
| Xattr prune | `lremovexattr` per stale name | same | none stale |
| crtime (macOS) | `setattrlist` | `apply/timestamps.rs:219` | `--crtimes` off, entry crtime zero |

Per-FS variation worth noting:

- **ext4 / xfs**: every operation hits the inode and journals a small
  metadata commit. With default `data=ordered` the chmod/chown/utimensat
  triplet typically batches into one transaction, but
  `lsetxattr`/`lremovexattr` each force a fresh transaction. At 100K
  files the journal-commit cost dominates everything else.
- **btrfs**: chown + chmod + utimensat trigger COW of the inode item.
  Xattrs live inline in the item, so consecutive `lsetxattr` calls
  rewrite the same metadata block N times. Batching wins are largest here.
- **APFS / HFS+**: `setattrlist` on macOS replaces utimensat for crtime
  and is path-based only; there is no fd variant.
- **NFSv4**: every syscall is a network round-trip. The triple
  chown/chmod/utimensat plus per-xattr `setxattr` is the worst case.
- **SMB / CIFS**: chown is usually a no-op or returns `EPERM`; utimensat
  is one round-trip, xattrs are unsupported on most mounts. Skipping
  the `lstat` probe matters most here because it is the only avoidable
  syscall.
- **tmpfs**: pure memory; the cost is syscall entry/exit, so reducing
  count matters more than reducing per-syscall cost.

For 100K plain files with ownership + perms + times preservation and no
xattrs/ACLs, the floor is `1 stat + 1 chown + 1 chmod + 1 utimensat`
syscalls per file, i.e. **400K syscalls**. Adding xattrs adds at least
one `llistxattr` and one or more `lsetxattr` per file; ACLs add one or
two `acl_set_file` calls.

## 3. Parallelism Status

`crates/transfer/src/parallel_io.rs` provides the shared
`map_blocking(items, min_parallel, f)` helper that runs work on rayon's
work-stealing pool when the input length meets `min_parallel`, otherwise
falls back to sequential iteration. `ParallelThresholds::metadata`
defaults to 64 (`DEFAULT_METADATA_THRESHOLD`).

Where the metadata path is parallel today:

- **Directory metadata batch**:
  `receiver/directory/creation.rs:122-148` runs
  `apply_metadata_from_file_entry`, `apply_acls_from_receiver_cache`,
  and `apply_xattrs_from_list` inside `map_blocking` keyed by
  `self.parallel_thresholds.metadata`. This is the only place where
  metadata application actually runs on the rayon pool.

Where the metadata path is sequential today:

- **Streaming file finalisation**: `receiver/transfer.rs:445-463` calls
  `apply_metadata_from_file_entry` -> `apply_xattrs_from_list` ->
  `apply_acls_from_receiver_cache` inline inside the per-file transfer
  loop. Each chmod/chown/utimensat fires on the receiver thread before
  the next file is read.
- **Disk-commit pipeline**:
  `disk_commit/process.rs:apply_metadata_acls_and_xattrs` is invoked
  per-entry from the SPSC commit consumer; there is no batch boundary,
  so syscalls run one file at a time on the commit thread.
- **Symlinks and special files**: `apply_symlink_metadata_with_options`
  is called sequentially from the symlink/special creation paths; no
  batching exists.

The result at 100K files is that only the directory subset benefits
from rayon. Regular-file metadata application, which is the majority of
the entries in a typical transfer, runs on a single thread.

## 4. Proposed Reductions

The five candidates below are ordered by expected ratio of saved
syscalls to engineering cost.

### 4.1 Conditional Skip When Unchanged Across All Paths

`apply_metadata_with_attrs_flags` already passes `cached_meta` into the
ownership, permissions, and timestamps submodules, but only the
directory-creation closure populates `cached_meta` from the existing
`stat` it has already done. The streaming and disk-commit paths call
`apply_metadata_from_file_entry`, which always re-issues
`fs::metadata(destination)`. Fix:

- Plumb the destination metadata that the receiver already obtained
  during `quick_check` / temp-file rename through to
  `apply_metadata_with_cached_stat`. The temp-file path on Linux returns
  the `statx` result from the `linkat`/`renameat2` step; for in-place
  writes the writer's own `fstat` can be reused.
- Remove the unconditional `fs::metadata` probe in
  `apply_metadata_from_file_entry` once all callers supply `cached_meta`,
  cutting one `lstat` per file (100K calls saved).

### 4.2 fd-Relative Operations on the Streaming Path

`apply_file_metadata_with_fd[_if_changed]` already exists and uses
`fchmod`/`fchown`/`futimens` on a `BorrowedFd`. The streaming receiver
in `receiver/transfer.rs` opens the temp file via the writer pipeline
and then drops the fd before applying metadata, falling back to
path-based syscalls. Refactor the writer-finalisation handoff so the
fd survives until after metadata application:

- Have `disk_commit::process` and `receiver/transfer.rs` keep the
  `OwnedFd` alive across the rename and call
  `apply_file_metadata_with_fd_if_changed` plus an fd-based
  `fsetxattr` loop.
- On Linux this also enables `O_PATH` dirfd batching (see 4.3) when an
  open file is unavailable (e.g. symlinks).

This converts 3 path lookups per file into 3 fd-relative syscalls and
removes the path-resolution cost that currently dominates on deep
directory trees.

### 4.3 dirfd-Relative Batching per Directory

The receiver visits files grouped by parent directory. Open each parent
once with `O_DIRECTORY | O_PATH`, then issue `fchmodat`, `fchownat`,
`utimensat(dirfd, name, ..., 0)`, and `setxattrat` (Linux 6.6+) using
that dirfd for every child. Concretely:

- Add a small `DirfdCache` keyed by parent path inside
  `receiver::transfer` and `disk_commit::process`.
- Switch `apply/permissions.rs`, `apply/ownership.rs`, and
  `apply/timestamps.rs` to accept an optional `(BorrowedFd, &CStr)`
  pair and prefer the `*at` variants from `rustix::fs` when supplied.
- Fall back to the existing path API on platforms or kernels that
  lack a particular `*at` variant.

This collapses the per-file path walk (which traverses every component
from the destination root) into a single `openat` per directory amortised
across all of its children. For a balanced tree of 100K files in 10K
directories the syscall path-resolution cost drops by an order of
magnitude.

### 4.4 Attribute Deduplication for Identical Source Vectors

In typical workloads (uniform owner / mode / xattr namespace) the
receiver re-applies the same chown args, the same chmod mode, and the
same xattr key/value set hundreds of times. Today `apply_xattrs_from_list`
runs `llistxattr` + per-name diff for every file independently, even
when the previous file in the same directory has the identical xattr
list.

Proposal: introduce a small per-batch `LastAppliedCache` keyed by
`(uid, gid, mode, xattr_list_hash, acl_ndx, def_acl_ndx)`. When the
incoming entry matches the previous applied tuple and `cached_meta`
already matches the destination, skip every syscall except the
unconditional permissions check that upstream does. The cache lives
inside the metadata-apply batch and is dropped at the end of each
directory or pipeline burst.

This is most valuable for xattrs (where the list comparison is
expensive) and for transfers between systems with shared `/etc/passwd`
where ownership rarely changes between adjacent files.

### 4.5 Deferred Metadata Phase

Upstream rsync delays directory-mtime application until the very end of
the transfer to avoid invalidating in-progress `mkdir`/`rename` work.
We mirror that for directory mtimes only. Extend the deferral to a
generalised metadata phase:

- During the transfer loop, record `(path, FileEntry, cached_meta)`
  tuples in a per-thread `Vec` instead of issuing the chmod/chown/
  utimensat/xattr/ACL syscalls inline.
- After the data plane drains, run the recorded actions through
  `parallel_io::map_blocking` with `parallel_thresholds.metadata`,
  matching the existing directory-batch behaviour.
- Coordinate with the existing failed-directory tracking so that a
  failed data write does not enqueue metadata work for a missing path.

Effect: the streaming receiver and the disk-commit pipeline both gain
the same rayon parallelism that the directory creator already has.
The deferral also opens space for combining 4.3 (dirfd batching) and
4.4 (attribute deduplication) by sorting the deferred queue by parent
dirfd and by `(uid, gid, mode, xattr_hash)` before draining.

## Next Steps

1. Add a benchmark harness driving 100K small files into a clean
   destination on tmpfs, ext4, and NFSv4. Report syscall count via
   `strace -c` and wall time.
2. Land 4.1 first (no API change) and re-measure.
3. Prototype 4.2 behind a config flag; verify on Linux + macOS.
4. Implement 4.3 + 4.5 together; they share the deferred queue.
5. Land 4.4 last; it depends on the queue introduced in 4.5.

Cross-references:

- `crates/metadata/src/apply/mod.rs`
- `crates/metadata/src/apply/ownership.rs`
- `crates/metadata/src/apply/permissions.rs`
- `crates/metadata/src/apply/timestamps.rs`
- `crates/metadata/src/apply_batch.rs`
- `crates/transfer/src/parallel_io.rs`
- `crates/transfer/src/receiver/directory/creation.rs`
- `crates/transfer/src/receiver/transfer.rs`
- `crates/transfer/src/disk_commit/process.rs`
