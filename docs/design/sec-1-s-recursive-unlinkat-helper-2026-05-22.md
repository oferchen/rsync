# SEC-1.s - `recursive_unlinkat_via_sandbox_or_fallback` helper

- **Status**: OPEN - new sub-task surfaced by the SEC-1.q closure doc and
  PR #4710's post-chain GAPs survey.
- **Date**: 2026-05-22
- **Scope owner**: SEC-1 audit chain
- **Predecessors**:
  - PR #4710 -
    [`docs/audits/sec-1-path-syscall-audit-2026-05-22.md`](../audits/sec-1-path-syscall-audit-2026-05-22.md)
    (GAP row #27, "Carrier missing - recursive `*at` peel").
  - PR #4711 -
    [`docs/design/sec-1-q-delete-emitter-sandbox-2026-05-22.md`](sec-1-q-delete-emitter-sandbox-2026-05-22.md)
    (names this helper as the prerequisite for `DeleteFs::remove_dir_all_at`).
- **Closes (when implemented)**: unblocks SEC-1.q step 2 -
  `DeleteFs::remove_dir_all_at` cannot ship without this helper, and the
  receiver-side `delete_extraneous_files` recursive branch (audit row #6,
  `crates/transfer/src/receiver/directory/deletion.rs:157`) folds into
  the same call surface.

## 1. Summary

`DeleteFs::remove_dir_all_at` (SEC-1.q, step 1) and the receiver's
`delete_extraneous_files` recursive branch both need to remove a
non-empty directory subtree anchored on a sandbox parent dirfd. No
single `*at` syscall removes a non-empty directory: every level of the
descent must reopen through `openat` so the path cannot be redirected
by a TOCTOU symlink swap between listing and unlinking. SEC-1.s ships
the helper that performs that walk, mirroring upstream rsync's
`delete_dir_contents` semantics while keeping every syscall anchored on
a dirfd. Helper-only; the trait wiring and caller cutovers ship under
SEC-1.q.

## 2. Proposed signature

Mirrors the SEC-1.g/.h/.j/(`openat`)/(`readlinkat`)
`*_via_sandbox_or_fallback` shape (sandbox option + `dest_dir` +
`relative_path` + already-joined absolute path), so call sites match
existing helpers byte-for-byte and the cutover diff for SEC-1.q is
mechanical.

```rust
/// Recursively remove the entry at `target_path` anchored on the
/// sandbox parent dirfd.
///
/// SEC-1.s adaptor that closes the symlink-swap TOCTOU window on the
/// `--delete` recursive-fallback site (audit row #27) and on the
/// receiver's `delete_extraneous_files` recursive branch (audit row
/// #6). Mirrors upstream rsync's `delete_dir_contents` + `delete_item`
/// pair (see `delete.c:48-176`) while pinning every level of the
/// descent on its own dirfd:
///
/// 1. When `sandbox` is `Some`, `target_path` equals
///    `dest_dir.join(relative_path)`, and `relative_path` has a single
///    component, the helper opens the leaf through
///    `openat(sandbox.current_dirfd(), leaf, O_DIRECTORY |
///    O_NOFOLLOW | O_CLOEXEC)` and walks the subtree using only
///    `*at` syscalls anchored on the freshly opened dirfd at each
///    level. After the inner loop drains, the helper closes the
///    walked dirfd and removes the now-empty directory through
///    [`unlinkat`] with [`UnlinkFlags::Dir`] against the sandbox
///    parent.
/// 2. In every other case the helper falls back to
///    [`std::fs::remove_dir_all`] on `target_path`. The fallback is
///    vulnerable to the symlink-swap class the carrier closes and is
///    intended only for the no-sandbox contexts (test fixtures,
///    client-side `--local` callers, callers that have not yet
///    plumbed a [`DirSandbox`]).
///
/// `target_path` must point at a directory; a non-directory leaf is
/// surfaced verbatim from the kernel as `ENOTDIR`. A symlink at the
/// leaf is refused with `ELOOP` (sandbox path, via `O_NOFOLLOW`) or
/// returned verbatim from [`std::fs::remove_dir_all`] (fallback path).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim on the sandbox path
/// and the [`std::fs::remove_dir_all`] error verbatim on the fallback
/// path. Notable cases:
/// - `ENOENT` on the descent root: returned as `Ok(())` (idempotent
///   delete, matching upstream `delete_item` line 201-206).
/// - `ELOOP` when `target_path` resolves to a symlink (sandbox path
///   only): never followed.
/// - `EACCES` on an individual child entry: per upstream, logged and
///   stepped over; the descent continues. The helper does not abort.
/// - `ENOTEMPTY` on the final `unlinkat(AT_REMOVEDIR)` after the
///   inner loop drained: surfaced verbatim. This indicates either a
///   concurrent writer outraced the helper or an entry was skipped
///   for `EACCES`; mirrors upstream's `DR_NOT_EMPTY` return.
/// - [`io::ErrorKind::FilesystemLoop`] when the cycle detector trips
///   on a previously-visited `(dev, ino)` pair (Linux + root only;
///   hardlink-to-directory is the only way to construct this).
pub fn recursive_unlinkat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    target_path: &Path,
) -> io::Result<()>;
```

The helper composes existing primitives, not new syscalls:

| Step | Primitive | Already shipped in `at_syscalls.rs`? |
|---|---|---|
| open the descent root | `openat(parent_dirfd, leaf, O_DIRECTORY \| O_NOFOLLOW \| O_CLOEXEC)` | yes - [`openat`] (raw) |
| list children | `fdopendir(dirfd)` + `readdir(3)` | no - inline; uses `libc::fdopendir` + `libc::readdir64` |
| classify each child | [`fstatat_nofollow`] | yes - SEC-1.f |
| recurse on subdir | self-call with the freshly opened dirfd | self |
| unlink non-dir | [`unlinkat`] with [`UnlinkFlags::File`] | yes - SEC-1.g |
| rmdir empty subdir | [`unlinkat`] with [`UnlinkFlags::Dir`] | yes - SEC-1.g |

Only `fdopendir` + `readdir64` add new `unsafe` to the file; both are
local to a single helper function and the `unsafe` budget extension
follows the same pattern as [`openat`] and [`readlinkat`].

## 3. Algorithm

The helper mirrors upstream rsync's `delete_dir_contents` /
`delete_item` (`target/interop/upstream-src/rsync-3.4.1/delete.c:48-176`)
but switches the path-based primitives for their dirfd-anchored
siblings.

```text
recursive_unlinkat_via_sandbox(parent_dirfd, leaf, visited):
  1. dirfd = openat(parent_dirfd, leaf,
                    O_DIRECTORY | O_NOFOLLOW | O_RDONLY | O_CLOEXEC)
     - ELOOP    -> propagate (symlink at leaf, refuse to descend)
     - ENOENT   -> return Ok (idempotent)
     - ENOTDIR  -> propagate (caller asked for recursive remove of a
                   non-directory; the surrounding DeleteFs dispatch
                   should never reach this branch for non-dirs)
  2. stat = fstatat_nofollow(parent_dirfd, leaf)
     - inspect (dev, ino); reject if already in `visited`
       (cycle detector, see section 3.1)
     - insert (dev, ino) into `visited` for the recursion
  3. for entry in fdopendir(dirfd):
       skip "." and ".."
       child_stat = fstatat(dirfd, entry.d_name, AT_SYMLINK_NOFOLLOW)
         - ENOENT -> entry vanished mid-walk; skip and continue
       if child_stat.is_dir():
         recursive_unlinkat_via_sandbox(dirfd, entry.d_name, visited)
       else:
         unlinkat(dirfd, entry.d_name, UnlinkFlags::File)
           - ENOENT  -> skip (vanished mid-walk)
           - EISDIR/EPERM -> classifier raced; retry once with
             UnlinkFlags::Dir, then skip on second failure
           - EACCES  -> log and skip per upstream
  4. close dirfd
  5. unlinkat(parent_dirfd, leaf, UnlinkFlags::Dir)
     - ENOTEMPTY -> propagate verbatim (real residual, caller decides)
     - ENOENT    -> return Ok (idempotent, mirrors step 1)
```

The outer adaptor handles the "no sandbox / multi-component
`relative_path`" decision exactly like every other
`_via_sandbox_or_fallback` helper, falling back to
`std::fs::remove_dir_all(target_path)`.

### 3.1 Cycle detection

`visited` is a `HashSet<(u64, u64)>` keyed on `(dev, ino)` of every
directory the helper has entered, threaded through the recursion. The
set is constructed on the outer call and grows by one entry per
descent. Hardlink-to-directory is non-portable and on Linux requires
`CAP_SYS_ADMIN`, but the detector closes the failure mode at no
material cost: a re-entered inode aborts the descent with
[`io::ErrorKind::FilesystemLoop`] before any destructive syscall fires
inside the cycle.

The detector intentionally does **not** prune by mountpoint. Upstream
gates `delete_dir_contents` on `FLAG_MOUNT_DIR` (lines 89-97 of
`delete.c`), which is established by the file-list walk, not by the
delete helper itself. Mountpoint pinning belongs in the SEC-1.q caller
(it has the `FileEntry` flags), not in the SEC-1.s helper.

### 3.2 TOCTOU race handling

Between `fstatat_nofollow` (classify) and `unlinkat` (act) an attacker
with write access to the directory could swap an entry's type. Two
failure modes and the matching response:

- **Was-file-now-dir**: `unlinkat(.., UnlinkFlags::File)` returns
  `EISDIR` (Linux) or `EPERM` (other Unix). The helper retries once
  with `UnlinkFlags::Dir`; if that fails too (the entry is a non-empty
  dir, or the swap continued), the helper logs and skips the entry.
  The skipped entry will produce `ENOTEMPTY` at step 5, which the
  caller can surface or retry.
- **Was-dir-now-file**: `openat(O_DIRECTORY | O_NOFOLLOW)` returns
  `ENOTDIR` or `ELOOP`. The helper skips the entry (it is no longer a
  directory; the recursive descent is moot) and continues. The leaf
  will be picked up by the parent loop's unlink path.

In neither case does the helper escalate to a path-based fallback,
escalate to non-`*at` syscalls, or follow symlinks. The descent stays
strictly within the dirfd-anchored surface.

## 4. Fallback

The fallback path runs when `sandbox` is `None` or when
`single_component_leaf(dest_dir, relative_path, target_path)` returns
`None` (multi-component relative path or path mismatch). The fallback
calls `std::fs::remove_dir_all(target_path)` verbatim, matching the
existing `unlink_via_sandbox_or_fallback` / `mkdirat_via_sandbox_or_fallback`
shape.

The fallback path is vulnerable to symlink-swap TOCTOU between the
listing and the unlinking. The carrier work (SEC-1.q caller wiring,
plus the carrier-only mitigated rows in PR #4710 section 4) closes
the gap by threading `Some(&DirSandbox)` into every daemon-reachable
call site; the fallback remains for:

- test fixtures and the SEC-1.q `RecordingDeleteFs` test fake (no
  sandbox, never touches the filesystem);
- client-side `--local` copies routed through
  `engine::local_copy::*` paths that are out of SEC-1 scope (see the
  PR #4710 audit, section 2 "Excluded");
- `--inplace` without daemon module gating, where the threat model
  does not apply.

The behaviour difference between the sandbox and fallback paths is
documented on the helper's rustdoc so callers can choose the carrier
path explicitly.

When `openat` returns `ELOOP` (symlink at the descent root, sandbox
path), the helper does **not** fall back to `std::fs::remove_dir_all`:
it returns the error verbatim. Following the symlink would defeat
the security contract that motivates the helper.

## 5. Error semantics

Mirrors upstream `delete_item` / `delete_dir_contents`
(`delete.c:130-209`) and is at least as strict as the C code:

| `errno` / shape | Upstream `delete.c` | SEC-1.s helper |
|---|---|---|
| `ENOENT` on root or on a child | `DR_SUCCESS` (line 206) | `Ok(())` (idempotent) |
| `ENOTEMPTY` after inner loop | `DR_NOT_EMPTY` (line 200) | propagate verbatim |
| `EACCES` on a single child | logged and stepped over (`rsyserr` path) | logged and stepped over; descent continues |
| `EBUSY` (Linux mountpoint pin) | `DR_NOT_EMPTY` indirectly (mount-dir skip) | propagate verbatim from `unlinkat`; caller decides |
| `ELOOP` on descent root | refused via `S_ISLNK` pre-check | refused via `O_NOFOLLOW` (sandbox path) |
| cycle detector trip | n/a (upstream relies on `FLAG_MOUNT_DIR` skip) | `io::ErrorKind::FilesystemLoop` |
| `ENOTDIR` on root | n/a (caller guarantees mode is dir) | propagate verbatim |

The helper never silently ignores an error other than `ENOENT` (a
vanished entry is idempotent-success per upstream) and `EACCES` on a
single child (matches upstream's permissive descent). All other errors
propagate to the caller, which decides whether `--ignore-errors`
applies (SEC-1.q caller wiring forwards the existing per-entry
behaviour from `DeleteEmitter`).

The `EACCES`-skip policy is bounded: the helper does not retry, does
not chmod, and does not call out to the receiver's
`!am_root && fp->flags & FLAG_OWNED_BY_US` chmod-recovery branch
(`delete.c:100-101`). That branch is the caller's responsibility; the
SEC-1.s helper performs the descent only.

## 6. Cross-platform plan

Matches the rest of `at_syscalls.rs`: the module is `#[cfg(unix)]`
inside `fast_io::dir_sandbox`, and the helper compiles on every Unix
target.

- **Linux**: full implementation. `openat2_supported()` is **not**
  required: SEC-1.s uses plain `openat(O_DIRECTORY | O_NOFOLLOW |
  O_CLOEXEC)` to stay portable across the existing helper family.
  Upgrading the descent root open to `openat2(RESOLVE_BENEATH |
  RESOLVE_NO_SYMLINKS)` is a deliberate follow-up (would harden mid-
  path `..` traversal at the cost of a new syscall surface and a
  kernel-version branch); deferred so SEC-1.q can ship first.
- **macOS**: full implementation. `openat`, `fdopendir`, `readdir`,
  `fstatat`, and `unlinkat` have been stable since 10.10 (Yosemite),
  comfortably below the project floor. No code-path divergence from
  Linux.
- **Other Unix (FreeBSD, NetBSD, OpenBSD, illumos)**: full
  implementation. Same syscall set; `ENOTEMPTY` may surface as
  `EEXIST` on some BSDs, mirroring the existing [`unlinkat`] helper's
  documented behaviour.
- **Windows**: no `_via_sandbox_or_fallback` helper compiles on
  Windows today (the whole module is `#[cfg(unix)]`). The Windows
  port of `DeleteFs` (SEC-1.q step 4) continues to dispatch through
  `std::fs::remove_dir_all`. Per the SEC-1.l Windows NTFS handle
  audit
  (`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`),
  NTFS handle-based deletion sidesteps the symlink-swap class this
  helper targets, so the Windows fallback is the correct posture
  rather than a gap.

## 7. Test plan

Unit tests live in the existing `#[cfg(test)] mod tests` at the bottom
of `crates/fast_io/src/dir_sandbox/at_syscalls.rs`, matching the
[`unlink_via_sandbox_or_fallback`] layout:

1. **happy path** -
   `recursive_unlinkat_removes_three_deep_tree`. Build
   `root/a/b/c/file` plus siblings at each level; call the helper;
   assert the root is empty and `root/a` is gone; assert
   `secure_open_dir(root).read_dir()` returns zero entries.
2. **symlink-swap mid-descent** -
   `recursive_unlinkat_refuses_to_follow_swapped_symlink`. Build
   `root/sub/{a, b}` plus a sentinel file outside the tree
   (`outside/sentinel`). Use a `RaceHook` (existing test pattern,
   see `unlink_via_sandbox_or_fallback` tests) that, between the
   helper's first `fstatat` and the matching `unlinkat`, swaps
   `root/sub` for a symlink to `outside`. Assert the helper returns
   `ELOOP` (or skips and propagates `ENOTEMPTY`) and the sentinel is
   intact.
3. **idempotent ENOENT** -
   `recursive_unlinkat_treats_missing_root_as_success`. Call the
   helper on a path that does not exist; assert `Ok(())`.
4. **idempotent vanish mid-descent** -
   `recursive_unlinkat_skips_entries_that_vanish_mid_walk`. Build
   `root/{a, b, c}`; race-hook to unlink `root/b` between listing
   and per-entry unlink; assert the helper returns `Ok(())` and the
   tree is gone.
5. **cycle detector (Linux + root only, gated)** -
   `recursive_unlinkat_breaks_hardlink_cycle`. Skip with
   `cap_sys_admin` not available; otherwise create `root/cycle/inner`,
   hardlink `root/cycle` into `root/cycle/inner/cycle` (requires
   root + ext4 mounted with `link_dir`, or use the existing
   `linux_capabilities::hardlink_dir_supported()` probe), call the
   helper, assert `io::ErrorKind::FilesystemLoop`.
6. **fallback equivalence** -
   `recursive_unlinkat_fallback_matches_std_remove_dir_all`. Pass
   `sandbox = None`; assert the result and the resulting filesystem
   state are identical to a separate call to
   `std::fs::remove_dir_all` on a parallel tree.
7. **TOCTOU classify-vs-act race** -
   `recursive_unlinkat_handles_classify_then_swap_to_dir`. Build
   `root/entry` as a regular file; race-hook to replace `entry`
   with an empty directory between `fstatat_nofollow` and
   `unlinkat`; assert the helper retries with `UnlinkFlags::Dir` and
   succeeds (or skips and produces `ENOTEMPTY` at step 5).

Integration test lives in `crates/transfer/tests/`, gated on Unix and
exercising the SEC-1.q caller path once the trait wiring lands:

8. **end-to-end via DeleteFs** -
   `delete_emitter_routes_recursive_remove_through_sandbox`. Build a
   destination tree with a deep subdir; drive `DeleteEmitter::emit_all`
   with `Some(&DirSandbox)`; install a probe `DeleteFs` that asserts
   `remove_dir_all_at` was called with the expected parent dirfd and
   leaf; assert the tree is gone.

## 8. Effort estimate

- Helper implementation in
  `crates/fast_io/src/dir_sandbox/at_syscalls.rs`: ~250 LoC.
  - Outer adaptor: ~30 LoC.
  - Recursive descent loop: ~120 LoC (open, list via
    `fdopendir`/`readdir64`, classify, recurse, unlink, rmdir).
  - Cycle detector + visited set plumbing: ~30 LoC.
  - TOCTOU race-retry shim (file-vs-dir swap): ~20 LoC.
  - Rustdoc: ~50 LoC.
- Unit tests (cases 1-7 above): ~150 LoC.
- Integration test (case 8): ~80 LoC.
- **Total: ~480 LoC**.

Wall-clock: one engineer-day plus CI cycles, on the same order as
[`openat_via_sandbox_or_fallback`] (PR #4716 shipped at ~400 LoC
including tests).

## 9. Dispatch sequencing

1. **SEC-1.s design doc** (this PR, OPEN status, docs-only).
2. **SEC-1.s implementation PR** ships
   `recursive_unlinkat_via_sandbox_or_fallback` plus the unit tests
   listed in section 7. No caller wiring, mirroring the
   `openat_via_sandbox_or_fallback` / `readlinkat_via_sandbox_or_fallback`
   precedent in PR #4716.
3. **SEC-1.q implementation PR** lands the
   `DeleteFs::remove_dir_all_at` trait extension, the `RealDeleteFs`
   wire-up that dispatches through the SEC-1.s helper, and the
   caller wiring in `engine::delete::DeleteEmitter::emit_all` plus
   `crates/transfer/src/receiver/directory/deletion.rs` (closing
   audit rows #5-#7 alongside the six trait sites).

The sequencing keeps each PR self-contained and bisectable: the
helper exists with full tests before any caller depends on it, and
the trait refactor lands as one diff that exercises every site that
needs it.

## 10. References

- PR #4710 -
  [`docs/audits/sec-1-path-syscall-audit-2026-05-22.md`](../audits/sec-1-path-syscall-audit-2026-05-22.md)
  (audit row #27 names the missing recursive `*at` peel as the
  Carrier blocker).
- PR #4711 -
  [`docs/design/sec-1-q-delete-emitter-sandbox-2026-05-22.md`](sec-1-q-delete-emitter-sandbox-2026-05-22.md)
  (section "Recommended approach" step 2 commits to
  `recursive_unlinkat_via_sandbox_or_fallback` as the prerequisite
  for `remove_dir_all_at`).
- PR #4712 -
  [`docs/design/sec-1-r-temp-file-sandbox-2026-05-22.md`](sec-1-r-temp-file-sandbox-2026-05-22.md)
  (sibling SEC-1.r helper closure; mirrored structure).
- PR #4716 -
  `crates/fast_io/src/dir_sandbox/at_syscalls.rs::openat_via_sandbox_or_fallback`
  and `::readlinkat_via_sandbox_or_fallback` (precedent for the
  helper-only PR shape this doc proposes).
- [`docs/design/sec-1-b-dirfd-carrier.md`](sec-1-b-dirfd-carrier.md)
  section 3 "recursive remove_dir_all" - earlier sketch naming
  `dirfd_remove_dir_all` as the hand-rolled peel.
- Upstream `delete.c:48-176`
  (`target/interop/upstream-src/rsync-3.4.1/delete.c`) - reference
  semantics for `delete_dir_contents` + `delete_item`. The SEC-1.s
  helper preserves the `ENOENT`-is-success, `ENOTEMPTY`-on-residual,
  `EACCES`-step-over behaviour and adds dirfd anchoring plus cycle
  detection on top.
