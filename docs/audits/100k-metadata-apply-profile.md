# 100K Files Metadata Application Profile (#1046)

Profiles the per-file metadata apply pipeline at 100K-file scale and
identifies the syscall-floor budget plus optimization candidates.

Sources of truth: `crates/metadata/src/apply/`, upstream
`target/interop/upstream-src/rsync-3.4.1/rsync.c:574-625` (`set_file_attrs`).

## 1. Metadata Apply Pipeline

Public entry: `metadata::apply_metadata_with_attrs_flags`
(`crates/metadata/src/apply/mod.rs:262`). Order matches upstream
`rsync.c:set_file_attrs()`: chown, chmod, utimensat, crtime.

| Step      | Submodule                          | Syscall (path / fd variant)                | Rust call site                                        |
|-----------|------------------------------------|--------------------------------------------|-------------------------------------------------------|
| chown     | `apply/ownership.rs`               | `fchownat(AT_SYMLINK_NOFOLLOW)` / `fchown` | `ownership::set_owner_like{,_with_fd}`                |
| chmod     | `apply/permissions.rs`             | `fchmodat` / `fchmod`                      | `permissions::apply_permissions_with_chmod{,_fd}`     |
| utimensat | `apply/timestamps.rs`              | `utimensat` / `futimens`                   | `timestamps::set_timestamp_like{,_with_fd}`           |
| crtime    | `apply/timestamps.rs`              | macOS `setattrlist` (path-only)            | `timestamps::apply_crtime_from_*`                     |
| xattr     | `metadata::apply_xattrs_from_list` | `setxattr` (one per attr)                  | `crates/metadata/src/xattr_unix.rs`                   |
| ACL       | `metadata::apply_acls_from_cache`  | `acl_set_file` (POSIX) / NFSv4             | `crates/metadata/src/acl_exacl.rs:588`                |

Receiver invokes via `transfer/src/disk_commit/process.rs:376` (file
commit) and `transfer/src/receiver/directory/creation.rs:127,298`
(directory metadata pass after creation).

## 2. 100K-File Theoretical Syscall Floor

Baseline `-aHAX --owner --group` per-file cost (Linux, fd available
post-write):

| Op        | Calls / file | 100K total | Notes                                                         |
|-----------|-------------:|-----------:|---------------------------------------------------------------|
| fchown    |            1 |       100K | skipped via `if_changed` when uid+gid match (`set_owner_like`)|
| fchmod    |            1 |       100K | skipped when mode matches (`apply_permissions_without_chmod`) |
| futimens  |            1 |       100K | upstream `same_mtime` short-circuit replicated                |
| setxattr  |        N_xa  |   100K*N_xa| one syscall per stored xattr                                  |
| acl_set   |          0-1 |   <=200K   | up to 2 ACLs (access + default) per dir                       |

Floor: 300K syscalls for `chown+chmod+utimens` alone; with `-AX`
realistic budget reaches 600K-800K. Real cost on Linux ext4 (warm
cache, 4 KiB files) is ~3.5 us per syscall, so the metadata phase
alone consumes ~1.0-1.4 s/100K serially before parallelism.

## 3. Profile Plan (`strace -c`)

Run inside `rsync-profile` (Debian) container with 100K 1-byte files:

```sh
mkdir -p src && seq 1 100000 | xargs -I{} -P8 touch src/f{}
strace -c -f -e trace=fchmod,fchmodat,fchown,fchownat,utimensat,\
futimensat,setxattr,lsetxattr,fsetxattr,acl_set_file \
  oc-rsync -aHAX --owner --group src/ dst/ 2> trace.txt
```

Tabulate `% time`, `calls`, `usecs/call` per syscall. Capture dst-side
hot path (`fchmod`/`futimens`/`fchown`) vs `setxattr` ACL fan-out.
Compare against upstream `rsync 3.4.1` baseline; deltas identify
redundant chown/chmod paths missed by `apply_*_if_changed`.

Capture cold-cache numbers via `echo 3 > /proc/sys/vm/drop_caches`
between runs. Record `getdents64` and `statx` counts for context but
exclude from the apply-budget total.

## 4. Parallel Apply Wiring

`apply_metadata_from_file_entry` is invoked from
`crates/transfer/src/parallel_io.rs::map_blocking`, which dispatches to
rayon when `items.len() >= min_parallel`. The relevant threshold is
`ParallelThresholds::metadata` (default
`DEFAULT_METADATA_THRESHOLD = 64`, `parallel_io.rs:27`).

Call sites that flow through this dispatcher:

- `receiver/directory/creation.rs:122` (post-mkdir directory metadata).
- `receiver/transfer/candidates.rs:127` (candidate batch).
- `receiver/transfer/pipeline.rs:179` (signature pre-fetch).

Cross-ref #1083 stat-batch: stat dispatch uses the same
`map_blocking` helper with `ParallelThresholds::stat` (default 64);
metadata threshold is identical so workloads above 64 entries always
run on rayon's work-stealing pool. Ordering invariant
(`into_par_iter().map().collect()`) is enforced by proptest in
`parallel_io.rs::parallel_stat_preserves_ordering`.

## 5. Optimization Candidates

1. **io_uring `statx` + metadata batches** (#1083 follow-on). On Linux
   5.6+ via the `fast_io` crate, replace per-file `statx` with batched
   `IORING_OP_STATX`; queue depth 256 amortizes submission cost.
   Saves the cold-cache stat dominating Step 3's `% time` column.
2. **`fchownat(AT_EMPTY_PATH)` over O_PATH on dirs.** Directory pass
   currently re-resolves paths for each `fchownat`. Open dirs once
   with `O_PATH|O_DIRECTORY`, then issue chown via empty-path on the
   dir fd, eliminating one path lookup per directory.
3. **Skip-if-equal short-circuit before utimensat.** `set_timestamp_like`
   already cmp-skips when `existing` matches, but the FileEntry path
   forces an extra `stat` to populate `cached_meta`. Fold the stat
   into the receiver's quick-check result (already cached in
   `receiver/quick_check.rs`) and pass via `apply_metadata_with_cached_stat`.
4. **Coalesced xattr writes.** `setxattr` currently fires once per
   attribute; group by file fd and reuse the open fd from the file
   commit phase to avoid per-attr `open`+`close` cycles.
5. **NUMA-aware rayon pool** for the apply pass on >32-core hosts;
   current default pool may schedule chown+chmod for the same inode
   onto different NUMA nodes, defeating dcache locality.
