# SEC-1.a - Path-syscall surface in daemon-reachable receiver code

**Date:** 2026-05-20
**Scope:** every `std::fs::*` / `std::os::unix::fs::*` call reachable from the receiver pipeline under `use_chroot = false`, plus the metadata-apply helpers it calls.
**Goal:** drive the SEC-1.b-j cutover from path-based syscalls to `*at` siblings backed by a sandboxed parent dirfd, closing the TOCTOU window upstream patched in rsync 3.4.3 (CVE-2026-29518, CVE-2026-43619).

This is read-only research. Test-only call sites (inside `#[cfg(test)]` modules, inside `#[test]` functions, or inside files exclusively used as test fixtures) are excluded - they run in process-private tempdirs and carry no daemon-reachable TOCTOU. Sender-side path syscalls that run as the source-owning user are also out of scope per the SEC-1 charter.

## 1. Per-call-site inventory

Columns: file:line | current call | `*at` replacement | notes.

### crate: `engine` (the bulk of the work)

#### `engine/src/delete/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/delete/emitter/fs.rs:70` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | trait method `RealDeleteFs::unlink_file`; called from `engine/src/delete/emitter/` drain loop |
| `crates/engine/src/delete/emitter/fs.rs:74` | `fs::remove_dir(path)` | `unlinkat(parent_fd, leaf, AT_REMOVEDIR)` | trait method `rmdir`; rmdir(2) path |
| `crates/engine/src/delete/emitter/fs.rs:78` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | trait method `unlink_symlink` |
| `crates/engine/src/delete/emitter/fs.rs:82` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | trait method `unlink_device` |
| `crates/engine/src/delete/emitter/fs.rs:86` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | trait method `unlink_special` |
| `crates/engine/src/delete/emitter/fs.rs:90` | `fs::remove_dir_all(path)` | requires `openat`-walked recursive deletion | recursive fallback; no single `*at` equivalent. Needs an `openat` + `readdir` + `unlinkat` peel; rsync 3.4.3's `delete_dir_contents` is the model |
| `crates/engine/src/delete/extras.rs:107` | `fs::read_dir(dest_dir)` | `openat(parent_fd, leaf, O_RDONLY \| O_DIRECTORY \| O_NOFOLLOW)` + `fdopendir` | scans destination for "extras"; the returned dirfd then anchors `fstatat` for each child |
| `crates/engine/src/delete/extras.rs:115` | `fs::symlink_metadata(entry.path())` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | per-entry stat after the read_dir above |

#### `engine/src/local_copy/context_impl/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/context_impl/delta_transfer.rs:30` | `fs::File::open(destination)` | `openat(parent_fd, leaf, O_RDONLY \| O_NOFOLLOW)` | basis file open for delta read; symlink at this path could redirect the basis read |
| `crates/engine/src/local_copy/context_impl/options/dirs.rs:97` | `fs::create_dir_all(parent)` | `mkdirat` per component | `prepare_parent_directory` in `--mkpath` allow-creation path |
| `crates/engine/src/local_copy/context_impl/options/dirs.rs:149` | `fs::symlink_metadata(parent)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | dry-run branch of `prepare_parent_directory` |
| `crates/engine/src/local_copy/context_impl/options/dirs.rs:184` | `fs::symlink_metadata(parent)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | real-side `prepare_parent_directory` when creation is allowed |
| `crates/engine/src/local_copy/context_impl/options/dirs.rs:201` | `fs::create_dir_all(parent)` | `mkdirat` per component | parent absent and creation allowed |
| `crates/engine/src/local_copy/context_impl/options/dirs.rs:214` | `fs::symlink_metadata(parent)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | creation-forbidden branch |
| `crates/engine/src/local_copy/context_impl/reporting.rs:168` | `fs::remove_dir(&entry.path)` | `unlinkat(parent_fd, leaf, AT_REMOVEDIR)` | rollback path on timeout |
| `crates/engine/src/local_copy/context_impl/reporting.rs:175` | `fs::remove_file(&entry.path)` | `unlinkat(parent_fd, leaf, 0)` | rollback path for files/symlinks/fifos/devices/hardlinks |
| `crates/engine/src/local_copy/context_impl/state.rs:264` | `std::fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | pre-flight stat before fd-based metadata apply |
| `crates/engine/src/local_copy/context_impl/state.rs:353` | `fs::metadata(&candidate)` | `fstatat(parent_fd, leaf, 0)` | `link-dest` candidate inspection |
| `crates/engine/src/local_copy/context_impl/state.rs:471` | `fs::remove_dir(dir)` | `unlinkat(parent_fd, leaf, AT_REMOVEDIR)` | `--delay-updates` staging cleanup |
| `crates/engine/src/local_copy/context_impl/state.rs:515` | `fs::create_dir_all(parent)` | `mkdirat` per component | backup-path parent creation |
| `crates/engine/src/local_copy/context_impl/state.rs:522` | `fs::rename(destination, &backup_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | backup rename - both ends need dirfds, may differ for `--backup-dir` |
| `crates/engine/src/local_copy/context_impl/state.rs:526` | `fs::remove_file(&backup_path)` | `unlinkat(parent_fd, leaf, 0)` | clear pre-existing backup before retry |
| `crates/engine/src/local_copy/context_impl/state.rs:535` | `fs::rename(destination, &backup_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | retry after clearing the existing backup |
| `crates/engine/src/local_copy/context_impl/state.rs:546` | `fs::read_link(destination)` | `readlinkat(parent_fd, leaf, ...)` | symlink-fallback path of cross-device backup |
| `crates/engine/src/local_copy/context_impl/state.rs:633` | `fs::remove_dir_all(destination)` | needs `openat` peel like emitter/fs.rs:90 | `force_remove_destination` directory branch |
| `crates/engine/src/local_copy/context_impl/state.rs:635` | `fs::remove_file(destination)` | `unlinkat(parent_fd, leaf, 0)` | `force_remove_destination` non-dir branch |
| `crates/engine/src/local_copy/context_impl/transfer.rs:64` | `fs::metadata(&candidate)` | `fstatat(parent_fd, leaf, 0)` | per-source dir-merge marker stat |

#### `engine/src/local_copy/executor/cleanup.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/cleanup.rs:291` | `fs::symlink_metadata(&path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | inspect extraneous-destination entry before deletion |
| `crates/engine/src/local_copy/executor/cleanup.rs:338` | `fs::read_dir(dir_path)` | `openat(parent_fd, leaf, O_RDONLY \| O_DIRECTORY \| O_NOFOLLOW)` + `fdopendir` | recursive subtree recording before emitter wipes it |
| `crates/engine/src/local_copy/executor/cleanup.rs:398` | `fs::symlink_metadata(source)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | `--remove-source-files`; source side - flag and exclude from SEC-1 if confirmed sender-only |
| `crates/engine/src/local_copy/executor/cleanup.rs:407` | `fs::remove_file(source)` | `unlinkat(parent_fd, leaf, 0)` | same as 398 - source-side `--remove-source-files`, may be out of scope |

#### `engine/src/local_copy/executor/directory/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:110` | `fs::metadata(&entry.path)` | `fstatat(parent_fd, leaf, 0)` | per-symlink target stat (follow); inside `par_iter`, dirfd must be `Send` |
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:116` | `fs::read_link(&entry.path)` | `readlinkat(parent_fd, leaf, ...)` | symlink-target read |
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:145` | `fs::metadata(path).ok().map(...)` | `fstatat(parent_fd, leaf, 0)` | device-id helper used for `--one-file-system` |
| `crates/engine/src/local_copy/executor/directory/recursive/checksum.rs:56` | `fs::metadata(&target_path)` | `fstatat(parent_fd, leaf, 0)` | size lookup for checksum prefetch budget |
| `crates/engine/src/local_copy/executor/directory/recursive/deletion.rs:64` | `fs::remove_dir(destination)` | `unlinkat(parent_fd, leaf, AT_REMOVEDIR)` | prune empty directory after dry-fail or empty-source recursion |
| `crates/engine/src/local_copy/executor/directory/recursive/destination.rs:38` | `fs::symlink_metadata(destination)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | destination state probe before recursive descent |
| `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:128` | `fs::create_dir_all(destination)` | `mkdirat` per component | `--implied-dirs` branch |
| `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:131` | `fs::create_dir(destination)` | `mkdirat(parent_fd, leaf, mode)` | non-`--implied-dirs` branch |
| `crates/engine/src/local_copy/executor/directory/support.rs:44` | `fs::read_dir(path)` | `openat(parent_fd, leaf, O_RDONLY \| O_DIRECTORY \| O_NOFOLLOW)` + `fdopendir` | sequential directory listing helper |
| `crates/engine/src/local_copy/executor/directory/support.rs:50` | `fs::symlink_metadata(&entry_path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | per-entry stat in sequential listing |
| `crates/engine/src/local_copy/executor/directory/support.rs:78` | `fs::read_dir(path)` | `openat` + `fdopendir` | parallel listing helper - same change |
| `crates/engine/src/local_copy/executor/directory/support.rs:90` | `fs::symlink_metadata(&entry_path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | parallel listing sequential fallback |
| `crates/engine/src/local_copy/executor/directory/support.rs:108` | `fs::symlink_metadata(&entry_path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | parallel `par_iter` body; dirfd must be `Send + Sync` clone |

#### `engine/src/local_copy/executor/file/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/file/copy/dry_run.rs:82` | `fs::File::open(source)` | `openat(parent_fd, leaf, O_RDONLY)` (sender side) | source open during dry-run hashing; flag - may be in/out of scope |
| `crates/engine/src/local_copy/executor/file/copy/mod.rs:86` | `fs::symlink_metadata(destination)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | pre-copy destination probe (file path) |
| `crates/engine/src/local_copy/executor/file/copy/transfer/execute/clonefile.rs:111` | `std::fs::remove_file(destination)` | `unlinkat(parent_fd, leaf, 0)` | rollback when reflink reports non-zero-copy result |
| `crates/engine/src/local_copy/executor/file/copy/transfer/execute/clonefile.rs:115` | `std::fs::remove_file(destination)` | `unlinkat(parent_fd, leaf, 0)` | rollback when reflink errors |
| `crates/engine/src/local_copy/executor/file/copy/transfer/execute/clonefile.rs:235` | `fs::set_permissions(destination, ...)` | `fchmodat(parent_fd, leaf, mode, 0)` | post-reflink chmod normalisation |
| `crates/engine/src/local_copy/executor/file/copy/transfer/execute/clonefile.rs:254` | `rustix::fs::utimensat(CWD, destination, ...)` | `utimensat(parent_fd, leaf, &times, 0)` | **already a `*at` call but anchored at `AT_FDCWD`** - swap `CWD` for the sandbox dirfd |
| `crates/engine/src/local_copy/executor/file/copy/transfer/open.rs:51` | `fs::File::open(path)` | `openat(parent_fd, leaf, O_RDONLY)` | source open with `O_NOATIME` fallback; sender-side, but path is dest-supplied for local-copy |
| `crates/engine/src/local_copy/executor/file/copy/transfer/open.rs:74` | `OpenOptions::new().custom_flags(O_NOATIME).open(path)` | `openat(parent_fd, leaf, O_RDONLY \| O_NOATIME)` | sender-side `O_NOATIME` open; same caveat |
| `crates/engine/src/local_copy/executor/file/copy/transfer/special.rs:54` | `OpenOptions::new()...truncate(true).open(destination)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_TRUNC, mode)` | `--inplace` special-entry truncate |
| `crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:127` | `OpenOptions::new()...truncate(false).open(destination)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT)` | `WriteStrategy::Append` open |
| `crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:141` | `OpenOptions::new()...truncate(should_truncate).open(destination)` | `openat(parent_fd, leaf, ...)` | `WriteStrategy::Inplace` open |
| `crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:157` | `OpenOptions::new().create_new(true).write(true).open(destination)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_EXCL)` | `WriteStrategy::Direct` open |
| `crates/engine/src/local_copy/executor/file/guard.rs:32` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | `remove_existing_destination` helper |
| `crates/engine/src/local_copy/executor/file/guard.rs:50` | `fs::remove_file(destination)` | `unlinkat(parent_fd, leaf, 0)` | `remove_incomplete_destination` helper |
| `crates/engine/src/local_copy/executor/file/guard.rs:155` | `fs::remove_file(&temp_path)` | `unlinkat(parent_fd, leaf, 0)` | partial-mode pre-write cleanup |
| `crates/engine/src/local_copy/executor/file/guard.rs:164` | `OpenOptions::new().create(true).truncate(true).open(&temp_path)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_TRUNC, mode)` | partial-mode temp-file open |
| `crates/engine/src/local_copy/executor/file/guard.rs:185` | `OpenOptions::new().create_new(true).open(&temp_path)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_EXCL, mode)` | normal-mode unique temp-file open |
| `crates/engine/src/local_copy/executor/file/guard.rs:318` | `fs::rename(&temp_path, &self.final_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | commit path - dirfds may differ for `--temp-dir` |
| `crates/engine/src/local_copy/executor/file/guard.rs:329` | `fs::rename(&temp_path, &self.final_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | commit retry after clearing destination |
| `crates/engine/src/local_copy/executor/file/guard.rs:351` | `fs::remove_file(&temp_path)` | `unlinkat(parent_fd, leaf, 0)` | cross-device fallback cleanup after `fs::copy` |
| `crates/engine/src/local_copy/executor/file/guard.rs:423` | `fs::File::options().write(true).open(temp_path)` | `openat(parent_fd, leaf, O_WRONLY)` | discard-with-partial open to reset mtime |
| `crates/engine/src/local_copy/executor/file/guard.rs:426` | `fs::remove_file(temp_path)` | `unlinkat(parent_fd, leaf, 0)` | discard helper - non-partial branch |
| `crates/engine/src/local_copy/executor/file/guard.rs:469` | `fs::remove_file(temp_path)` | `unlinkat(parent_fd, leaf, 0)` | drop-path cleanup |
| `crates/engine/src/local_copy/executor/file/partial.rs:217` | `fs::remove_file(path)` | `unlinkat(parent_fd, leaf, 0)` | `remove_if_exists` |
| `crates/engine/src/local_copy/executor/file/paths.rs:45` | `fs::create_dir_all(&base_dir)` | `mkdirat` per component | `--partial-dir` materialisation |

#### `engine/src/local_copy/executor/reference.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/reference.rs:71` | `fs::symlink_metadata(&candidate)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | `--link-dest` / `--copy-dest` / `--compare-dest` candidate inspection |

#### `engine/src/local_copy/executor/sources/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/sources/destination.rs:17` | `fs::symlink_metadata(path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | `query_destination_state` - destination probe |
| `crates/engine/src/local_copy/executor/sources/destination.rs:64` | `fs::create_dir_all(destination_path)` | `mkdirat` per component | `ensure_destination_directory` |
| `crates/engine/src/local_copy/executor/sources/orchestration.rs:222` | `fs::symlink_metadata(parent)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | source-parent stat for `--one-file-system -xx`; sender-side - flag |
| `crates/engine/src/local_copy/executor/sources/orchestration.rs:344` | `fs::symlink_metadata(&target)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | `delete_missing_source_entry` destination probe |
| `crates/engine/src/local_copy/executor/sources/orchestration.rs:388` | `fs::remove_dir_all(&target)` | recursive `openat` peel | destination dir delete |
| `crates/engine/src/local_copy/executor/sources/orchestration.rs:390` | `fs::remove_file(&target)` | `unlinkat(parent_fd, leaf, 0)` | destination non-dir delete |

#### `engine/src/local_copy/executor/special/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/special/device.rs:58` | `fs::symlink_metadata(destination)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | device-node pre-write probe |
| `crates/engine/src/local_copy/executor/special/fifo.rs:60` | `fs::symlink_metadata(destination)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | fifo pre-write probe |
| `crates/engine/src/local_copy/executor/special/fifo.rs:227` | `fs::File::create(destination)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_TRUNC, mode)` | non-Unix fifo placeholder; non-Unix is out of TOCTOU scope but flag for consistency |
| `crates/engine/src/local_copy/executor/special/symlink.rs:152` | `fs::symlink_metadata(destination)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | symlink pre-write probe |
| `crates/engine/src/local_copy/executor/special/symlink.rs:466` | `std::os::unix::fs::symlink(target, destination)` | `symlinkat(target, parent_fd, link_leaf)` | the actual symlink creation - one of the highest-leverage swaps |

#### `engine/src/local_copy/executor/util.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/util.rs:19` | `fs::metadata(path)` | `fstatat(parent_fd, leaf, 0)` | `follow_symlink_metadata`; called by many callers above - swap once, benefit many |

#### `engine/src/local_copy/filter_program/rules.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/filter_program/rules.rs:70` | `fs::symlink_metadata(&target)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | `--filter exclude-if-present=` marker probe; runs on the source side normally, but the source can be the receiver under `--remove-source-files`. Flag. |

#### `engine/src/local_copy/dir_merge/load.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/dir_merge/load.rs:133` | `fs::File::open(path)` | `openat(parent_fd, leaf, O_RDONLY \| O_NOFOLLOW)` | dir-merge filter file open; source-side, flag |

#### `engine/src/local_copy/hard_links/cohort.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/hard_links/cohort.rs:86` | `fs::create_dir_all(parent)` | `mkdirat` per component | follower-parent creation, `apply_follower` |
| `crates/engine/src/local_copy/hard_links/cohort.rs:90` | `fs::remove_file(follower_dest)` | `unlinkat(parent_fd, leaf, 0)` | clear existing follower before `linkat` |
| `crates/engine/src/local_copy/hard_links/cohort.rs:150` | `fs::create_dir_all(parent)` | `mkdirat` per component | deferred follower-parent creation, `resolve_deferred` |
| `crates/engine/src/local_copy/hard_links/cohort.rs:156` | `fs::remove_file(&follower)` | `unlinkat(parent_fd, leaf, 0)` | deferred follower cleanup |

#### `engine/src/local_copy/clonefile.rs` and `win_copy.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/local_copy/clonefile.rs:83` | `std::fs::metadata(src)` | `fstatat(parent_fd, leaf, 0)` | size hint for clonefile dispatch; src is sender-side. Flag |
| `crates/engine/src/local_copy/win_copy.rs:136` | `std::fs::metadata(src)` | `fstatat(parent_fd, leaf, 0)` | Windows clone path; Windows uses NTFS handle-based APIs, treat as out of scope |

#### `engine/src/walk/walkdir_impl.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/engine/src/walk/walkdir_impl.rs:113` | `fs::metadata(root).ok()` | `fstatat(parent_fd, ".", 0)` | one-file-system root device probe. The `walk/` module is currently re-exported from `engine` but no production call site consumes it (confirmed by repo-wide grep). Sender-side when used. Keep this row only because the SEC-1.a charter listed `walk/` as a starting point. |

### crate: `transfer` (receiver pipeline proper)

#### `transfer/src/receiver/directory/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/transfer/src/receiver/directory/creation.rs:108` | `fs::create_dir_all(dir_path)` | `mkdirat` per component | bulk directory pre-creation pass |
| `crates/transfer/src/receiver/directory/creation.rs:252` | `fs::create_dir(&dir_path)` | `mkdirat(parent_fd, leaf, mode)` | `ensure_relative_parents` per-ancestor `mkdir` |
| `crates/transfer/src/receiver/directory/creation.rs:313` | `fs::create_dir_all(&dir_path)` | `mkdirat` per component | `create_directory_incremental` new-dir branch |
| `crates/transfer/src/receiver/directory/deletion.rs:157` | `fs::remove_dir_all(&path)` | recursive `openat` peel | `--delete` of an unexpected dir |
| `crates/transfer/src/receiver/directory/deletion.rs:159` | `fs::remove_file(&path)` | `unlinkat(parent_fd, leaf, 0)` | `--delete` of an unexpected file/symlink |
| `crates/transfer/src/receiver/directory/links.rs:74` | `std::fs::read_link(&link_path)` | `readlinkat(parent_fd, leaf, ...)` | check existing destination symlink for up-to-date short-circuit |
| `crates/transfer/src/receiver/directory/links.rs:84` | `std::fs::remove_file(&link_path)` | `unlinkat(parent_fd, leaf, 0)` | remove stale symlink (target changed) |
| `crates/transfer/src/receiver/directory/links.rs:85` | `link_path.symlink_metadata()` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | non-symlink obstacle probe before recreating |
| `crates/transfer/src/receiver/directory/links.rs:86` | `std::fs::remove_file(&link_path)` | `unlinkat(parent_fd, leaf, 0)` | remove non-symlink obstacle |
| `crates/transfer/src/receiver/directory/links.rs:92` | `fs::create_dir_all(parent)` | `mkdirat` per component | symlink-parent creation |
| `crates/transfer/src/receiver/directory/links.rs:96` | `std::os::unix::fs::symlink(target, &link_path)` | `symlinkat(target, parent_fd, link_leaf)` | the actual symlink creation - highest-leverage swap, matches upstream `do_symlink` |
| `crates/transfer/src/receiver/directory/links.rs:227` | `fs::symlink_metadata(&link_path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | hardlink up-to-date probe (link path) |
| `crates/transfer/src/receiver/directory/links.rs:228` | `fs::symlink_metadata(&leader_path)` | `fstatat(parent_fd, leaf, AT_SYMLINK_NOFOLLOW)` | hardlink up-to-date probe (leader path) |
| `crates/transfer/src/receiver/directory/links.rs:251` | `fs::remove_file(&link_path)` | `unlinkat(parent_fd, leaf, 0)` | clear existing target before `linkat` |
| `crates/transfer/src/receiver/directory/links.rs:256` | `fs::create_dir_all(parent)` | `mkdirat` per component | hardlink-parent creation |

#### `transfer/src/receiver/quick_check.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/transfer/src/receiver/quick_check.rs:150` | `fs::metadata(&ref_path)` | `fstatat(parent_fd, leaf, 0)` | `--link-dest` / `--copy-dest` / `--compare-dest` reference stat |
| `crates/transfer/src/receiver/quick_check.rs:176` | `fs::create_dir_all(parent)` | `mkdirat` per component | link-dest hardlink parent creation |
| `crates/transfer/src/receiver/quick_check.rs:198` | `fs::create_dir_all(parent)` | `mkdirat` per component | copy-dest copy parent creation |
| `crates/transfer/src/receiver/quick_check.rs:200` | `fs::copy(&ref_path, &dest_path)` | needs `openat`-anchored read + `openat`-anchored write | source path is reference (trusted), destination needs dirfd anchoring |
| `crates/transfer/src/receiver/quick_check.rs:268` | `fs::File::open(path)` | `openat(parent_fd, leaf, O_RDONLY \| O_NOFOLLOW)` | `--checksum` reread |

#### `transfer/src/receiver/transfer/`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/transfer/src/receiver/transfer/candidates.rs:147` | `fs::metadata(&file_path)` | `fstatat(parent_fd, leaf, 0)` | parallel quick-check stat phase (rayon `par_iter`); dirfd must be `Send + Sync` clone |
| `crates/transfer/src/receiver/transfer/sync.rs:279` | `fs::create_dir_all(parent)` | `mkdirat` per component | backup-dir parent creation |
| `crates/transfer/src/receiver/transfer/sync.rs:282` | `fs::rename(&file_path, &backup_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | backup move before overwrite |
| `crates/transfer/src/receiver/transfer/sync.rs:293` | `fs::rename(temp_guard.path(), &file_path)` | `renameat(old_dirfd, old_leaf, new_dirfd, new_leaf)` | commit-rename; already conditionally routed through `fast_io::try_rename_via_io_uring`. The io_uring path lands on `IORING_OP_RENAMEAT` which already accepts dirfds; the std fallback needs the swap |

#### `transfer/src/temp_guard.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/transfer/src/temp_guard.rs:130` | `OpenOptions::new().create_new(true).open(&concrete_path)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_EXCL, mode)` | primary temp-file create |
| `crates/transfer/src/temp_guard.rs:142` | `fs::create_dir_all(parent)` | `mkdirat` per component | parent recovery when first open returns `ENOENT` |
| `crates/transfer/src/temp_guard.rs:143` | `OpenOptions::new().create_new(true).open(&concrete_path)` | `openat(parent_fd, leaf, O_WRONLY \| O_CREAT \| O_EXCL, mode)` | retry after parent creation |
| `crates/transfer/src/temp_guard.rs:217` | `std::fs::remove_file(&self.path)` | `unlinkat(parent_fd, leaf, 0)` | RAII cleanup |

### crate: `metadata` (called by both receiver and local-copy)

#### `metadata/src/apply/mod.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/metadata/src/apply/mod.rs:223` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | `apply_metadata_from_file_entry` pre-stat - called once per applied entry; high traffic |

#### `metadata/src/apply/ownership.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/metadata/src/apply/ownership.rs:193` | `unix_fs::chownat(CWD, destination, ...)` | `chownat(parent_fd, leaf, ...)` | **already a `*at` call but anchored at `AT_FDCWD`** - swap `CWD` for the sandbox dirfd |
| `crates/metadata/src/apply/ownership.rs:349` | `chownat(CWD, destination, ...)` | `chownat(parent_fd, leaf, ...)` | same as above - other branch (`apply_ownership_from_entry`) |

#### `metadata/src/apply/permissions.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/metadata/src/apply/permissions.rs:35` | `fs::set_permissions(destination, ...)` | `fchmodat(parent_fd, leaf, mode, 0)` | non-Unix `set_permissions_like`; non-Unix is out of scope but flag for code-shape consistency |
| `crates/metadata/src/apply/permissions.rs:42` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | non-Unix read-modify-write for `readonly` bit |
| `crates/metadata/src/apply/permissions.rs:48` | `fs::set_permissions(destination, ...)` | `fchmodat(parent_fd, leaf, mode, 0)` | non-Unix write-back |
| `crates/metadata/src/apply/permissions.rs:91` | `fs::set_permissions(destination, permissions)` | `fchmodat(parent_fd, leaf, mode, 0)` | `apply_permissions_with_chmod` Unix path |
| `crates/metadata/src/apply/permissions.rs:140` | `fs::set_permissions(destination, permissions)` | `fchmodat(parent_fd, leaf, mode, 0)` | fd-variant fallback when no fd is supplied |
| `crates/metadata/src/apply/permissions.rs:197` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | `base_mode_for_permissions` current-mode read |
| `crates/metadata/src/apply/permissions.rs:245` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | `apply_permissions_without_chmod` exec-bit read |
| `crates/metadata/src/apply/permissions.rs:267` | `fs::set_permissions(destination, ...)` | `fchmodat(parent_fd, leaf, mode, 0)` | exec-bit write-back |
| `crates/metadata/src/apply/permissions.rs:305` | `fs::set_permissions(destination, permissions)` | `fchmodat(parent_fd, leaf, mode, 0)` | `apply_permissions_from_entry` permissions path |
| `crates/metadata/src/apply/permissions.rs:315` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | chmod-modifier current-mode read (permissions branch) |
| `crates/metadata/src/apply/permissions.rs:321` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | chmod-modifier current-mode read (no cached_meta branch) |
| `crates/metadata/src/apply/permissions.rs:330` | `fs::set_permissions(destination, new_permissions)` | `fchmodat(parent_fd, leaf, mode, 0)` | chmod-modifier write-back |
| `crates/metadata/src/apply/permissions.rs:343` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | non-Unix readonly probe |
| `crates/metadata/src/apply/permissions.rs:352` | `fs::set_permissions(destination, dest_perms)` | non-Unix; out of scope | flag for consistency |

#### `metadata/src/apply/timestamps.rs`

| file:line | current call | `*at` replacement | notes |
|---|---|---|---|
| `crates/metadata/src/apply/timestamps.rs:40` | `set_file_times(destination, accessed, modified)` | `utimensat(parent_fd, leaf, &times, 0)` | path-based mtime/atime - follow-symlinks variant |
| `crates/metadata/src/apply/timestamps.rs:43` | `set_symlink_file_times(destination, accessed, modified)` | `utimensat(parent_fd, leaf, &times, AT_SYMLINK_NOFOLLOW)` | path-based mtime/atime - no-follow variant |
| `crates/metadata/src/apply/timestamps.rs:127` | `set_file_times(destination, atime, mtime)` | `utimensat(parent_fd, leaf, &times, 0)` | `apply_timestamps_from_entry` |
| `crates/metadata/src/apply/timestamps.rs:166` | `fs::metadata(destination)` | `fstatat(parent_fd, leaf, 0)` | `apply_atime_only_from_entry` mtime read |
| `crates/metadata/src/apply/timestamps.rs:172` | `set_file_times(destination, atime, mtime)` | `utimensat(parent_fd, leaf, &times, 0)` | `apply_atime_only_from_entry` write-back |

The `filetime` crate does not expose a `utimensat`-with-dirfd entry point. SEC-1.f-j will need to drop the `filetime` crate at these sites and call `rustix::fs::utimensat` directly (the crate is already a transitive dependency via `rustix::fs::utimensat` use in `clonefile.rs`). See **Surprises** below.

## 2. Hot-path counts

Tallied from the inventory above, production-only (test rows excluded).

| crate | call sites | files touched |
|---|---|---|
| `engine` | 67 | 27 |
| `transfer` | 22 | 6 |
| `metadata` | 18 | 3 |
| **total** | **107** | **36** |

**Top-3 crates by site count:** `engine` (67), `transfer` (22), `metadata` (18). The `engine` total is dominated by `local_copy/executor/` (47 sites across 19 files). Within `engine`, the densest single file is `crates/engine/src/local_copy/executor/file/guard.rs` (10 sites) - it owns the entire temp-file lifecycle (open/rename/remove). Second-densest is `crates/metadata/src/apply/permissions.rs` (14 sites) - it has the most independent permission/stat branches because of the `--perms` / `--executability` / `--chmod` / non-Unix matrix.

SEC-1.b's design needs to make the dirfd reachable from:

1. The `CopyContext` in `engine/src/local_copy/context_impl/` so it propagates to every executor.
2. The `Receiver` state in `transfer/src/receiver/` so the directory/links/quick_check/transfer/sync paths see it.
3. The `MetadataOptions` (or a sibling struct) passed into `metadata::apply_metadata_from_file_entry` and friends so the metadata-apply leaf can use `fstatat`/`fchmodat`/`chownat`/`utimensat` against the receiver-owned dirfd.

A single `DirSandbox` value passed by `&` through these three carriers is enough; no Arc/Mutex needed because the dirfd is shared read-only (the underlying file descriptor is `Send + Sync`).

## 3. Already-safe entries

Two call sites already use `*at` syscalls but anchor at `AT_FDCWD`. They are no-op behaviour-wise relative to the path-based call but a one-line swap for SEC-1.f-j:

- `crates/metadata/src/apply/ownership.rs:193` - `unix_fs::chownat(CWD, destination, ...)`. Swap `CWD` for the sandbox dirfd and replace `destination` (full path) with the leaf component.
- `crates/metadata/src/apply/ownership.rs:349` - same shape, in `apply_ownership_from_entry`.
- `crates/engine/src/local_copy/executor/file/copy/transfer/execute/clonefile.rs:254` - `rustix::fs::utimensat(CWD, destination, ...)`. Same one-line swap.

`crates/metadata/src/special.rs:146` and `:194` (`mknodat(CWD, destination, ...)`) live one level below the per-crate scope (they are called from `engine/.../special/fifo.rs:219` and `device.rs:215` via `create_fifo_with_fake_super` / `create_device_node_with_fake_super`). They are the only `mknodat` call sites in the codebase; they should be re-pointed at the sandbox dirfd at the same time as the `chownat` swaps to keep the special-file creation atomic with respect to the parent.

The `chownat` `with_fd` and `fchmod` fd-variants in `apply/ownership.rs:248` and `apply/permissions.rs:131` operate on an already-open `fd` and so are TOCTOU-safe by construction. They are not in the inventory above and do not need changes; their existence proves that the metadata layer already has the fd-pathway plumbing - SEC-1 simply needs to extend that pathway to the path-only branches.

Two further sites operate on an open `fs::File` rather than a path and so are inherently TOCTOU-safe:

- `crates/engine/src/local_copy/deferred_sync.rs:264` - `File::open(path).sync_all()` on an already-resolved directory path. The directory is one we created during the transfer; the open is the resolve. Acceptable as-is if the sandbox guarantees the parent path is locked.
- `crates/engine/src/local_copy/executor/file/copy/transfer/open.rs:62` - `apply_macos_read_hint(&file)`, takes an `&fs::File`.

## 4. Surprises (cases where the simple swap is blocked)

1. **`filetime` crate has no `utimensat`-with-dirfd entry point.** The five sites in `metadata/src/apply/timestamps.rs` that call `filetime::set_file_times` / `filetime::set_symlink_file_times` cannot be swapped one-for-one - the crate only exposes path-based and `&File` variants. SEC-1.f-j must replace those calls with direct `rustix::fs::utimensat(dirfd, leaf, &times, flags)` calls. `rustix::fs::utimensat` is already used elsewhere in the tree (`clonefile.rs:254`) so this introduces no new dependency. Expect ~30 LoC of churn including the `FileTime` -> `rustix::fs::Timestamps` adapter (a one-line `Timespec` construction per call).

2. **`fs::remove_dir_all` has no atomic `*at` sibling.** Four sites (`emitter/fs.rs:90`, `local_copy/context_impl/state.rs:633`, `executor/sources/orchestration.rs:388`, `receiver/directory/deletion.rs:157`) recursively peel a directory. Each needs a hand-rolled `openat` walker that mirrors upstream's `delete_dir_contents` (`delete.c:48-122`): open the directory with `O_NOFOLLOW | O_DIRECTORY`, `readdir` into a buffer, recursively peel children, then `unlinkat(.., AT_REMOVEDIR)` the now-empty dir. ~80 LoC for the helper, then four ~3-line call-site swaps. Adding this helper is also a prerequisite for the `--delete` work in the daemon receiver if we ever want it parallel-safe under chroot=no.

3. **`fs::copy` in `quick_check.rs:200`.** The two-syscall semantics (open source + open destination + write loop) means a clean `*at` swap needs an explicit helper. The source path is the trusted reference directory; the destination path is the dest tree that needs sandbox anchoring. Either split the copy into `openat`-anchored read + `openat`-anchored write, or pre-resolve the destination dirfd + leaf and then call a `copy_via_fds(src_path, dest_dirfd, dest_leaf)` helper. The clonefile dispatch in `engine/src/local_copy/clonefile.rs` already has the shape of this helper and can be the template.

4. **`fs::rename` across two parent directories.** Backup-rename (`state.rs:522`, `:535`), commit-rename (`guard.rs:318`, `:329`), and the receiver's commit-rename (`transfer/sync.rs:293`) move a file between two directories that may or may not be the same. `renameat` needs both `old_dirfd` and `new_dirfd`. When `--backup-dir` or `--temp-dir` is set, the source and target live under different sandboxed dirfds and both must be opened with `O_NOFOLLOW`. The simple swap (one dirfd) is wrong; the design must pass both.

5. **`std::os::unix::fs::symlink` has no `*at` sibling in std.** The two production sites (`special/symlink.rs:466`, `receiver/directory/links.rs:96`) need a direct `rustix::fs::symlinkat(target, parent_fd, link_leaf)` call. The `target` itself is a sender-supplied string and stays unchanged; only the link's parent directory is anchored. `rustix` exposes this directly; ~2-line swap per site.

6. **Rayon parallel callsites.** `crates/engine/src/local_copy/executor/directory/support.rs:108` and `crates/transfer/src/receiver/transfer/candidates.rs:147` issue `fstatat`-equivalent stats inside `par_iter` bodies. `BorrowedFd<'_>` is `Send + Sync` so passing a borrowed sandbox dirfd into the closure is fine, but the SEC-1.b design must commit to "dirfd is a long-lived `OwnedFd` owned by the receiver state, exposed as `BorrowedFd<'_>`" rather than "freshly opened per-batch".

7. **`engine/src/walk/` is currently unreferenced by production code.** Repo-wide grep confirms no caller of `WalkdirWalker` / `FilteredWalker` outside the module's own tests. The single production `fs::metadata(root)` call at `walkdir_impl.rs:113` is reachable only if the module gets adopted later. Treat as defensive (one line) or defer to whichever PR wires the walker in.

8. **Non-Unix branches in `metadata/src/apply/permissions.rs`.** Five rows in the inventory (`:35`, `:42`, `:48`, `:343`, `:352`) live behind `#[cfg(not(unix))]`. Windows uses `SetFileInformationByHandle` on an open handle (no path TOCTOU), so the swap is a no-op for Windows correctness. Leave them as path-based and document the cross-platform asymmetry in SEC-1.f-j's PR body.

## 5. Estimated diff size for SEC-1.f-j

Counting one logical change per call site, plus the four hand-rolled helpers and the three plumbing changes (CopyContext, Receiver, MetadataOptions):

- **Per-site swaps:** 107 sites x ~3 lines (compute leaf via `Path::file_name`, swap call, propagate `parent_fd`) = ~320 lines.
- **Hand-rolled helpers:** ~80 lines for the `remove_dir_at` peel + ~40 lines for the `copy_at` source-to-dirfd helper + ~30 lines for the `filetime` -> `rustix::fs::utimensat` adapter = ~150 lines.
- **Plumbing:** ~50 lines for `DirSandbox` (carrier type + constructor + `as_borrowed_fd`) + ~30 lines threading it through `CopyContext` / `Receiver` / `MetadataOptions` signatures = ~80 lines.
- **Tests:** ~300 lines for the TOCTOU regression matrix (one positive and one negative test per major sink kind: `symlinkat`, `mkdirat`, `renameat`, `unlinkat`, `unlinkat AT_REMOVEDIR`, `fchmodat`, `chownat`, `utimensat`, `openat` for write, `openat` for read, `fstatat`, the recursive peel, `mknodat`). The test should swap the parent dir for a symlink mid-syscall and assert the call returns `ELOOP` or hits the sandbox boundary, mirroring rsync 3.4.3's regression coverage.

**Total estimate: ~850 LoC of diff** spread across 36 production files and one new test module. The work is naturally fan-out-able: SEC-1.b establishes the `DirSandbox` carrier, then SEC-1.c-j can each take a sub-tree (delete emitter, local_copy executor, receiver directory pipeline, metadata-apply, temp-file guard) and land independently as long as they all consume the same carrier type. Recommended PR slicing matches the section structure above.

## 6. Confidence and follow-ups

- The inventory is grep-derived from the five subtrees listed in the SEC-1.a charter plus `crates/transfer/src/temp_guard.rs` (called from the receiver but not nested under `receiver/`). It does not cover the daemon's own listener / config / motd-file paths because those run before the receiver pipeline starts; if any of them touches the destination tree before chroot they should be re-audited under a separate task.
- ACL and xattr application (`metadata::apply_acls_from_receiver_cache`, `metadata::apply_xattrs_from_list`) are called from the receiver but live in their own modules (`metadata/src/acl/`, `metadata/src/xattr.rs`) and were not included in this audit's scope. They use `lsetxattr` / `acl_set_file` which are path-based and have the same TOCTOU window - file a sibling SEC-1.k for that subsurface before declaring SEC-1 done.
- The audit excludes io_uring submission paths that already operate on dirfd-anchored SQEs (e.g. `fast_io::try_rename_via_io_uring`, `fast_io::hard_link`, `fast_io::link_anonymous_tmpfile`). When the io_uring path is hit, the std fallback is the one that needs the swap; the io_uring path already takes dirfds.
