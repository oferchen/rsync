# SEC-1.b - Parent-dirfd carrier for the receiver pipeline

**Date:** 2026-05-21
**Scope:** decide how a sandboxed parent directory file descriptor is plumbed through the receiver / local-copy / metadata-apply pipeline so SEC-1.c-j can convert the 107 path-based syscalls catalogued in `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` to `*at` siblings.
**Status:** design, no Rust changes in this PR.
**Inputs:** `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` (107 sites / 36 files / 7 surprises).

## 1. The pick - hybrid stack + small LRU side cache (option 4)

The carrier is a single `DirSandbox` value that combines:

1. an in-tree **dirfd stack** mirroring the receiver's depth-first descent (push on enter, pop on exit); the top of the stack is the parent dirfd for the entry currently being processed, and
2. a fixed-capacity **side cache** of `Arc<OwnedFd>` keyed by relative directory path for the secondary roots required by `--backup-dir`, `--temp-dir`, `--link-dest`, `--copy-dest`, and `--compare-dest`.

Rationale - the receiver and the local-copy executor both walk a tree depth-first under a single sandbox root; the parent dirfd for the entry being applied is almost always either the root itself or the most-recently-entered subdirectory. A pure stack (option 3) models that exactly and lends a `BorrowedFd<'_>` with zero per-entry opens, which is the right answer for the 67 engine and 22 transfer sites that operate on the destination tree. The audit's surprises 4 and 6, however, rule out a pure stack: `--backup-dir` / `--temp-dir` need a second simultaneous dirfd for `renameat` (audit rows `crates/engine/src/local_copy/context_impl/state.rs:522`, `crates/engine/src/local_copy/executor/file/guard.rs:318`, `crates/transfer/src/receiver/transfer/sync.rs:293`), and `--link-dest` / `--copy-dest` need a long-lived dirfd against the reference tree that is not on the descent path (audit rows `crates/engine/src/local_copy/executor/reference.rs:71`, `crates/transfer/src/receiver/quick_check.rs:150`, `:198`, `:200`). Option 1 (open-on-demand) burns syscalls in the hot per-entry loop and reintroduces a TOCTOU window between the open and the use; option 2 (pure LRU keyed by relative path) loses the cheap "current parent" invariant the stack gives for free and forces every site to consult a map.

The hybrid keeps the in-tree common case allocation-free and lock-free (the stack lives on the receiver's own thread; `BorrowedFd<'_>` is `Send + Sync` so child rayon workers can borrow it through the closure capture without contention), while still letting the four out-of-tree operands resolve through the side cache. The side cache is small and bounded (one entry per configured operand, typically <= 4), so a `DashMap<PathBuf, Arc<OwnedFd>>` covers it without the `Mutex<HashMap>` outer-lock anti-pattern called out in BR-3j; insertion happens once at receiver setup and lookups are pure reads.

The whole thing is `#[cfg(unix)]`; the Windows port (audit row `crates/engine/src/local_copy/win_copy.rs:136`) continues to use handle-based NTFS APIs and is not addressed by SEC-1.

## 2. API sketch

`DirSandbox` lives in a new module `crates/engine/src/local_copy/dirfd/` (closest existing tree to the executor) and is re-exported through `engine::local_copy::DirSandbox`. The `transfer` and `metadata` crates depend on `engine` only transitively today; SEC-1.b's plumbing PR (SEC-1.c, see section 7) will lift the type into a small leaf crate or into `metadata` itself so the dependency direction stays acyclic. The shape below is what the consuming code sees regardless of where the type ultimately lives.

```rust
#[cfg(unix)]
pub struct DirSandbox {
    // Root of the destination tree, opened O_DIRECTORY | O_NOFOLLOW once at session start.
    root: Arc<OwnedFd>,
    // Stack of (relative-path, OwnedFd) frames pushed on entering a subdirectory.
    // The top is the parent dirfd for the entry being applied. Single-owner; the
    // receiver state owns this and lends BorrowedFd<'_> to workers.
    stack: Vec<DirFrame>,
    // Operand roots: --backup-dir, --temp-dir, --link-dest, --copy-dest, --compare-dest.
    // Keyed by the absolute, canonicalised root path. Populated lazily; never evicted
    // during a session (capacity is bounded by CLI config).
    operands: DashMap<PathBuf, Arc<OwnedFd>>,
}

#[cfg(unix)]
struct DirFrame {
    leaf: OsString,
    fd: OwnedFd,
}

#[cfg(not(unix))]
pub struct DirSandbox; // zero-sized stub; methods compile but route to path-based fallbacks
```

Public surface (all `#[cfg(unix)]`; the stub exposes the same names returning `io::Error::from(io::ErrorKind::Unsupported)` so call sites can stay platform-uniform):

```rust
impl DirSandbox {
    /// Open `root` with O_DIRECTORY | O_NOFOLLOW and seed an empty stack.
    pub fn open_root(root: &Path) -> io::Result<Self>;

    /// Borrow the dirfd at the top of the stack (or the root if the stack is empty).
    /// Hot-path accessor; returns in O(1) without syscalls.
    pub fn current_parent(&self) -> BorrowedFd<'_>;

    /// Push a frame for `leaf` by openat'ing the subdirectory off the current parent.
    /// Used by the depth-first walker when entering a subdirectory.
    pub fn enter(&mut self, leaf: &OsStr) -> io::Result<()>;

    /// Pop the top frame. No-op on an empty stack (callers must balance).
    pub fn leave(&mut self);

    /// Run `f` with the parent dirfd for `rel` (a path relative to the sandbox root).
    /// For the in-tree common case `rel` is a single leaf and `f` sees `current_parent()`;
    /// for deeper paths the helper walks frames or peels via openat without mutating the stack.
    pub fn with_parent_dirfd<R>(
        &self,
        rel: &Path,
        f: impl FnOnce(BorrowedFd<'_>, &OsStr) -> io::Result<R>,
    ) -> io::Result<R>;

    /// Register a secondary root (backup-dir, temp-dir, link-dest, ...). Idempotent.
    /// Called once per operand during receiver setup.
    pub fn register_operand(&self, root: &Path) -> io::Result<Arc<OwnedFd>>;

    /// Look up a registered operand's dirfd. Returns None if `register_operand`
    /// was not called for this root.
    pub fn operand(&self, root: &Path) -> Option<Arc<OwnedFd>>;
}
```

Carrier wiring:

- **`engine::local_copy::context_impl::CopyContext`** gains a `sandbox: Arc<DirSandbox>` field. `CopyContext` is the per-transfer state object every executor sees (see audit row prefix `crates/engine/src/local_copy/context_impl/`), so a single field propagates to all 67 engine sites without changing any function signatures outside the executor's leaf calls.
- **`transfer::receiver::Receiver`** gains the same `sandbox: Arc<DirSandbox>` field. The receiver's directory / links / quick-check / transfer / sync paths (audit rows under `crates/transfer/src/receiver/`) already take `&self` or `&mut self`; they reach the carrier through `self.sandbox`.
- **`metadata::apply::MetadataOptions`** gains an `Option<BorrowedFd<'a>>` parent-fd field threaded through `apply_metadata_from_file_entry`, `apply_permissions_from_entry`, `apply_ownership_from_entry`, `apply_timestamps_from_entry`. Existing callers that don't have a sandbox (test fixtures, legacy paths) pass `None` and the metadata layer falls back to the path-based call - preserving today's behaviour and keeping the surface bisectable.

The `Arc` on the engine and transfer carriers is for cheap cloning into rayon workers (see section 5), not for shared mutation; the stack is mutated only from the owning receiver thread.

## 3. Call-site adaptation patterns

For each SEC-1.a syscall family, the before/after shape. Audit row citations use `file:line` from `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md`.

**lstat / fstatat-NOFOLLOW** (e.g. `crates/engine/src/delete/extras.rs:115`, `crates/engine/src/local_copy/executor/file/copy/mod.rs:86`):

```text
before: let meta = fs::symlink_metadata(&path)?;
after:  let meta = sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::statat(dirfd, leaf, AtFlags::SYMLINK_NOFOLLOW))?;
```

**stat / fstatat** (e.g. `crates/engine/src/local_copy/context_impl/state.rs:264`, `crates/metadata/src/apply/mod.rs:223`):

```text
before: let meta = fs::metadata(&path)?;
after:  let meta = sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::statat(dirfd, leaf, AtFlags::empty()))?;
```

**unlink / unlinkat** (e.g. `crates/engine/src/delete/emitter/fs.rs:70`, `crates/engine/src/local_copy/executor/file/guard.rs:32`):

```text
before: fs::remove_file(&path)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::unlinkat(dirfd, leaf, AtFlags::empty()))?;
```

**rmdir / unlinkat AT_REMOVEDIR** (e.g. `crates/engine/src/delete/emitter/fs.rs:74`, `crates/engine/src/local_copy/context_impl/state.rs:471`):

```text
before: fs::remove_dir(&path)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::unlinkat(dirfd, leaf, AtFlags::REMOVEDIR))?;
```

**recursive remove_dir_all** (audit rows `crates/engine/src/delete/emitter/fs.rs:90`, `crates/engine/src/local_copy/context_impl/state.rs:633`, `crates/engine/src/local_copy/executor/sources/orchestration.rs:388`, `crates/transfer/src/receiver/directory/deletion.rs:157`):

```text
before: fs::remove_dir_all(&path)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            dirfd_remove_dir_all(dirfd, leaf))?;
```

`dirfd_remove_dir_all` is the hand-rolled `openat`-peel helper called out in SEC-1.a section 4.2; it lives next to `DirSandbox`. It mirrors upstream `delete.c:delete_dir_contents`: openat with `O_DIRECTORY | O_NOFOLLOW`, readdir, recurse on subdirs via the freshly opened dirfd, then `unlinkat(.., REMOVEDIR)` the now-empty dir.

**mkdir / mkdirat** (e.g. `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:131`, `crates/transfer/src/receiver/directory/creation.rs:252`):

```text
before: fs::create_dir(&path)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::mkdirat(dirfd, leaf, mode))?;
```

**create_dir_all / mkdirat per component** (e.g. `crates/engine/src/local_copy/context_impl/options/dirs.rs:97`, `crates/transfer/src/receiver/directory/links.rs:92`, `:256`):

```text
before: fs::create_dir_all(&parent)?;
after:  sandbox.create_dir_all(&parent)?; // helper walks components, mkdirat-ing each one
```

`DirSandbox::create_dir_all` is a thin wrapper that walks the components of `parent` relative to the sandbox root, calling `mkdirat` on each missing one off the previous dirfd. It does not push frames onto the stack (these are upfront pre-creations, not enters).

**chmod / fchmodat** (audit rows in `crates/metadata/src/apply/permissions.rs`, e.g. `:91`, `:267`, `:305`):

```text
before: fs::set_permissions(&path, perms)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::chmodat(dirfd, leaf, mode, AtFlags::empty()))?;
```

**lchown / chownat** (audit rows `crates/metadata/src/apply/ownership.rs:193`, `:349` - already `*at` but `CWD`):

```text
before: unix_fs::chownat(CWD, &path, uid, gid, ChownatFlags::AT_SYMLINK_NOFOLLOW)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            unix_fs::chownat(dirfd, leaf, uid, gid, ChownatFlags::AT_SYMLINK_NOFOLLOW))?;
```

**utimes / utimensat** (audit rows in `crates/metadata/src/apply/timestamps.rs`, e.g. `:40`, `:127`, `:172`). Surprise 1: the `filetime` crate has no dirfd entry point, so these sites switch directly to `rustix::fs::utimensat`:

```text
before: filetime::set_file_times(&path, atime, mtime)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::utimensat(dirfd, leaf, &[atime_ts, mtime_ts], AtFlags::empty()))?;
```

A one-liner adapter `fn to_timespec(ft: FileTime) -> Timespec` keeps the call site terse. The no-follow variant at `crates/metadata/src/apply/timestamps.rs:43` passes `AtFlags::SYMLINK_NOFOLLOW`.

**rename / renameat** (audit rows `crates/engine/src/local_copy/context_impl/state.rs:522`, `:535`, `crates/engine/src/local_copy/executor/file/guard.rs:318`, `:329`, `crates/transfer/src/receiver/transfer/sync.rs:293`). Surprise 4: two dirfds needed - see section 4.

**symlink / symlinkat** (audit rows `crates/engine/src/local_copy/executor/special/symlink.rs:466`, `crates/transfer/src/receiver/directory/links.rs:96`). Surprise 5: std has no `*at` sibling, use rustix directly:

```text
before: unix_fs::symlink(&target, &link_path)?;
after:  sandbox.with_parent_dirfd(&link_path, |dirfd, leaf|
            rustix::fs::symlinkat(target, dirfd, leaf))?;
```

`target` is the sender-supplied symlink contents and stays a path string; only the link's parent directory is anchored.

**mknod / mknodat** (sites in `crates/metadata/src/special.rs:146`, `:194`, reached from `crates/engine/src/local_copy/executor/special/fifo.rs:219` and `device.rs:215`):

```text
before: rustix::fs::mknodat(CWD, &path, kind, mode, dev)?;
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::mknodat(dirfd, leaf, kind, mode, dev))?;
```

**link / linkat** (audit row `crates/engine/src/local_copy/hard_links/cohort.rs:90`, `:156`; receiver side `crates/transfer/src/receiver/directory/links.rs:251` clears the dest, then a later `linkat` populates it). Two dirfds needed when leader and follower live under different operand roots; see section 4.

**open** (read - audit rows `crates/engine/src/local_copy/context_impl/delta_transfer.rs:30`, `crates/transfer/src/receiver/quick_check.rs:268`):

```text
before: let f = fs::File::open(&path)?;
after:  let f = sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::openat(dirfd, leaf, OFlags::RDONLY | OFlags::NOFOLLOW, Mode::empty())
                .map(File::from))?;
```

**open** (write / create - audit rows `crates/engine/src/local_copy/executor/file/guard.rs:164`, `:185`, `crates/transfer/src/temp_guard.rs:130`, `:143`):

```text
before: let f = OpenOptions::new().write(true).create_new(true).open(&path)?;
after:  let f = sandbox.with_parent_dirfd(&path, |dirfd, leaf|
            rustix::fs::openat(dirfd, leaf,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL, mode)
                .map(File::from))?;
```

**readlink / readlinkat** (audit rows `crates/engine/src/local_copy/context_impl/state.rs:546`, `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:116`, `crates/transfer/src/receiver/directory/links.rs:74`):

```text
before: let target = fs::read_link(&path)?;
after:  let target = sandbox.with_parent_dirfd(&path, |dirfd, leaf| {
            let mut buf = [0u8; libc::PATH_MAX as usize];
            rustix::fs::readlinkat(dirfd, leaf, &mut buf[..])
                .map(|slice| OsString::from_vec(slice.to_vec()))
        })?;
```

**read_dir / openat + fdopendir** (audit rows `crates/engine/src/delete/extras.rs:107`, `crates/engine/src/local_copy/executor/directory/support.rs:44`, `:78`, `crates/engine/src/local_copy/executor/cleanup.rs:338`):

```text
before: for entry in fs::read_dir(&path)? { ... }
after:  sandbox.with_parent_dirfd(&path, |dirfd, leaf| {
            let sub = rustix::fs::openat(dirfd, leaf,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW, Mode::empty())?;
            for entry in DirIter::new(sub)? { ... }
            Ok(())
        })?;
```

`DirIter` wraps `rustix::fs::Dir` over an `OwnedFd`. Per-entry stats inside the iterator should call `rustix::fs::statat(sub_fd, entry_name, ...)` rather than reconstructing the full path.

## 4. Cross-directory operations - two dirfds without deadlock

`renameat`, `linkat`, and the synthetic `copy_via_fds` helper need two dirfds simultaneously. The carrier guarantees deadlock-freedom by construction:

- The **in-tree dirfd** (source for backup, destination for commit-rename, follower for hardlinks) is obtained via `sandbox.with_parent_dirfd(rel_path, ...)` and is either the top of the stack or a freshly opened dirfd resolved off the stack. It is borrowed for the duration of the syscall.
- The **out-of-tree dirfd** (`--backup-dir` root, `--temp-dir` root, `--link-dest` root) is fetched up front through `sandbox.operand(root_path)` returning `Arc<OwnedFd>`. Operand fds are opened once during receiver setup and live for the whole session.

There is no mutex around either side. The in-tree fd is a `BorrowedFd<'_>` whose lifetime is the `with_parent_dirfd` closure. The operand fd is an `Arc<OwnedFd>` whose `BorrowedFd<'_>` is obtained by `Arc::as_fd()` (or `arc.as_ref().as_fd()`). No lock crosses the syscall, so two-dirfd operations cannot deadlock against each other or against the descent stack.

For commit-rename, the pattern is:

```text
let temp_fd  = sandbox.operand(&temp_dir_root).expect("registered at setup");
let final_rel = ...; // path of the final file relative to the sandbox root
sandbox.with_parent_dirfd(&final_rel, |final_dirfd, final_leaf| {
    rustix::fs::renameat(temp_fd.as_fd(), &temp_leaf, final_dirfd, final_leaf)
})?;
```

For the in-tree case (no `--temp-dir`, no `--backup-dir`), `temp_fd` and the parent dirfd produced by `with_parent_dirfd` are the same fd (the receiver's current parent). The syscall is valid - `renameat` accepts identical old and new dirfds - and no special-casing is needed.

For `linkat` follower-to-leader (audit row `crates/engine/src/local_copy/hard_links/cohort.rs:90`), the leader and follower may share a parent or not. When both are in-tree, both resolve through `with_parent_dirfd` against the descent stack. When `--link-dest` puts the leader in a reference tree, the leader's dirfd is fetched via `sandbox.operand(link_dest_root)` and the follower's parent comes from `with_parent_dirfd` against the destination root. Same shape as renameat.

Re-entrancy is not a hazard: `with_parent_dirfd` takes `&self` (not `&mut self`), reads the stack snapshot, and returns the closure's result. The stack is mutated only by `enter` / `leave`, which the depth-first walker calls on directory boundaries, never inside a per-entry operation.

## 5. Rayon worker pattern

The two `par_iter` call sites flagged by SEC-1.a surprise 6 are:

- `crates/engine/src/local_copy/executor/directory/support.rs:108`
- `crates/transfer/src/receiver/transfer/candidates.rs:147`

Both already operate inside a single bounded scope per directory (`par_iter` over the entries of one parent). The pattern:

```text
let parent = sandbox.current_parent(); // BorrowedFd<'_>, Send + Sync
entries.par_iter().try_for_each(|entry| {
    // Closure captures `parent` by copy (BorrowedFd is Copy + Send + Sync).
    let meta = rustix::fs::statat(parent, &entry.leaf, AtFlags::empty())?;
    process(meta)
})?;
```

`BorrowedFd<'_>` is `Copy + Send + Sync` (it is a transparent wrapper around `RawFd` with a phantom lifetime), so each worker captures it by value at no synchronisation cost. The lifetime constraint - the borrow must outlive the closure - is satisfied because `par_iter().try_for_each` is a blocking call: it returns only after every worker has dropped its closure, and the `sandbox` outlives that call.

For the parallel-planner site at `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:110`, where the workers need three syscalls (`statat`, `readlinkat`, `statat` follow) and each must anchor at the parent dirfd, the same `BorrowedFd<'_>` capture covers all three. No `Arc` clone, no per-task open.

Where a worker needs to descend further (open a sub-directory and stat its children), it issues a fresh `openat` against the captured parent dirfd to get a worker-local `OwnedFd`, walks it, and drops it on closure exit. The `DirSandbox` stack is not mutated from worker threads.

## 6. Windows fallback

The audit annotates 14 sites with `#[cfg(not(unix))]` or "non-Unix; out of scope" (e.g. `crates/metadata/src/apply/permissions.rs:35`, `:42`, `:48`, `:343`, `:352`; `crates/engine/src/local_copy/executor/special/fifo.rs:227`; `crates/engine/src/local_copy/win_copy.rs:136`). The audit's own surprise 8 documents the policy: Windows uses `SetFileInformationByHandle` on an open handle, which is already TOCTOU-safe by construction, so no carrier is needed on Windows.

The carrier is therefore declared `#[cfg(unix)]`. The non-Unix build keeps a zero-sized stub `pub struct DirSandbox;` with the same method names returning `io::Error::from(io::ErrorKind::Unsupported)` so that platform-uniform callers compile, and the path-based legacy code under `#[cfg(not(unix))]` remains the active code on Windows. SEC-1.l (the planned Windows-handle audit cited in the task description) will revisit whether any Windows-side TOCTOU surface needs its own carrier; if so it will be a parallel type, not a variant of `DirSandbox`.

This matches the `#[cfg(unix)]` convention already used in `crates/engine/src/local_copy/clonefile.rs` and `crates/metadata/src/apply/ownership.rs` for the existing `chownat` paths.

## 7. Sequencing for SEC-1.c-j

The audit estimated ~850 LoC of diff. The work splits cleanly into the following bisectable PRs, ordered so each one only depends on its predecessors. PR sizes are LoC estimates from SEC-1.a section 5 plus the section's own helper budget.

**SEC-1.c - Carrier plumbing (~120 LoC, no behaviour change).** Land `DirSandbox`, `DirFrame`, `dirfd_remove_dir_all`, and the `BorrowedFd`-returning accessors. Wire it through `CopyContext`, `Receiver`, and `MetadataOptions` as `Option<...>` so every existing caller keeps compiling and falling back to today's path-based path. No syscall changes yet. Tests: unit tests for `enter` / `leave` balance, operand registration idempotency, and `with_parent_dirfd` resolving a single-leaf path against `current_parent()`.

**SEC-1.d - Receiver in-tree leaf swaps (~180 LoC).** Convert the in-tree, single-dirfd, no-helper sites in `crates/transfer/src/receiver/directory/` (creation.rs, deletion.rs except the recursive peel, links.rs except the cross-tree hardlink) plus `crates/transfer/src/temp_guard.rs`. These are the simplest swaps (`statat`, `unlinkat`, `mkdirat`, `openat`, `symlinkat`) and exercise the carrier in production for the first time. Adds the TOCTOU regression matrix scaffold so subsequent PRs only need to extend it.

**SEC-1.e - Engine in-tree leaf swaps (~220 LoC).** Convert `crates/engine/src/local_copy/executor/file/guard.rs` (10 sites, the densest file), `crates/engine/src/local_copy/executor/file/copy/`, `crates/engine/src/local_copy/executor/special/`, `crates/engine/src/local_copy/executor/directory/recursive/`, and `crates/engine/src/local_copy/executor/directory/support.rs` (including the rayon worker site at `:108`). Same single-dirfd shape as SEC-1.d, but inside the local-copy executor.

**SEC-1.f - Metadata-apply swaps (~160 LoC).** Convert `crates/metadata/src/apply/permissions.rs` (14 sites), `crates/metadata/src/apply/ownership.rs` (the 2 already-`*at` sites, just swap `CWD` for the sandbox dirfd), `crates/metadata/src/apply/timestamps.rs` (5 sites, including the `filetime` -> `rustix::fs::utimensat` migration from surprise 1), `crates/metadata/src/apply/mod.rs:223`, and `crates/metadata/src/special.rs` (`mknodat` `CWD` swaps). This PR also drops the `filetime` direct dependency at these call sites; the crate stays for sender-side reads.

**SEC-1.g - Cross-directory operations (~110 LoC).** Convert the renameat / linkat / `--backup-dir` / `--temp-dir` / `--link-dest` sites that need two dirfds simultaneously: `crates/engine/src/local_copy/context_impl/state.rs:522`, `:535`, `crates/engine/src/local_copy/executor/file/guard.rs:318`, `:329`, `crates/engine/src/local_copy/hard_links/cohort.rs:90`, `:156`, `crates/transfer/src/receiver/directory/links.rs:251`, `crates/transfer/src/receiver/transfer/sync.rs:282`, `:293`, `crates/transfer/src/receiver/quick_check.rs:200`. Builds on SEC-1.c's operand-registration API and the carrier's `Arc<OwnedFd>` operand lookup.

**SEC-1.h - Recursive peel + remaining edges (~120 LoC).** Land the four `dirfd_remove_dir_all` callers (`crates/engine/src/delete/emitter/fs.rs:90`, `crates/engine/src/local_copy/context_impl/state.rs:633`, `crates/engine/src/local_copy/executor/sources/orchestration.rs:388`, `crates/transfer/src/receiver/directory/deletion.rs:157`), the `read_dir` -> `openat + fdopendir` swaps in `crates/engine/src/delete/extras.rs`, `crates/engine/src/local_copy/executor/cleanup.rs`, and `crates/transfer/src/receiver/quick_check.rs` callers not covered above. The recursive helper carries its own targeted tests for the symlink-substitution TOCTOU case.

**SEC-1.i - Sender-side flagged sites and `engine/src/walk/` (~40 LoC, optional).** The audit flagged several sites as "sender-side; may be in/out of scope" (e.g. `crates/engine/src/local_copy/executor/file/copy/dry_run.rs:82`, `crates/engine/src/local_copy/executor/cleanup.rs:398`, `:407`, `crates/engine/src/local_copy/dir_merge/load.rs:133`, `crates/engine/src/local_copy/filter_program/rules.rs:70`, `crates/engine/src/local_copy/executor/sources/orchestration.rs:222`, `crates/engine/src/walk/walkdir_impl.rs:113`). Convert opportunistically once the carrier is universal; defer if the SEC-1 charter explicitly excludes them. Splittable from SEC-1.c-h if it slips.

**SEC-1.j - Remove the legacy fallback path (~30 LoC, gate flip).** Once SEC-1.c-h have landed and the TOCTOU regression matrix is green, drop the `Option<...>` fallback in `CopyContext`, `Receiver`, and `MetadataOptions` so the carrier is mandatory on Unix. Update the per-crate denylist in `crates/*` (a clippy lint disallowing `fs::remove_file`, `fs::symlink_metadata`, etc. without a sandbox companion) to enforce no regressions. The lint is the closing brace on SEC-1.

The total stays under the 850 LoC estimate. The first PR (SEC-1.c) is small (~120 LoC) and pure plumbing; the largest is SEC-1.e (~220 LoC) but is mechanical leaf-swaps following the SEC-1.d template. Bisectability is preserved because every PR leaves the carrier consistent with every consumer's current expectations.

## 8. Open questions deferred to implementation

- **`DirSandbox` crate location.** SEC-1.c will decide whether the type lives in a new leaf crate (`crates/sandbox/`), in `metadata`, or in `engine` with a re-export. The decision is mechanical; it does not change the API.
- **`OFlags::NOFOLLOW` on every openat.** Default is yes; surprises around `--copy-links` may need a per-call override. SEC-1.e will codify the rule.
- **`dirfd_remove_dir_all` mount-crossing.** Upstream `delete.c:delete_dir_contents` checks `st_dev` against the parent and refuses to descend across mount points unless `--one-file-system` semantics allow it. The helper must match this. SEC-1.h carries the test.

## 9. References

- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` - the call-site inventory and the seven surprises this design addresses.
- Upstream `delete.c:48-122` (`delete_dir_contents`) - reference implementation for the recursive `openat` peel.
- Upstream commit history for rsync 3.4.3 - CVE-2026-29518 / CVE-2026-43619 patches.
- `rustix::fs` (`openat`, `statat`, `unlinkat`, `mkdirat`, `chmodat`, `chownat`, `utimensat`, `symlinkat`, `mknodat`, `linkat`, `renameat`) - the `*at` syscall surface.
- `std::os::fd::{BorrowedFd, OwnedFd, AsFd}` - the type-system anchor that makes `Send + Sync` correctness checkable at compile time.
