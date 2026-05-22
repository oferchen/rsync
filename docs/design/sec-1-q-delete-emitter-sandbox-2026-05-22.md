# SEC-1.q - DeleteFs trait sandbox follow-up

- **Status**: OPEN - new follow-up surfaced by the SEC-1 path-syscall
  coverage audit (PR #4710).
- **Date**: 2026-05-22
- **Scope owner**: SEC-1 audit chain
- **Predecessor audit**:
  [`docs/audits/sec-1-path-syscall-audit-2026-05-22.md`](../audits/sec-1-path-syscall-audit-2026-05-22.md)
  - section 4 rows #22-#27, section 6 "DeleteFs trait" row.

## Summary

PR #4710 enumerated 27 GAP sites that survived the SEC-1.f/.g/.h/.i/.j
`*at` cutover. Six of them live in
[`crates/engine/src/delete/emitter/fs.rs`](../../crates/engine/src/delete/emitter/fs.rs)
inside `RealDeleteFs`. Every `--delete` operation flows through this
trait impl, so these are the last daemon-reachable path-based
`unlink` / `rmdir` calls that are not anchored on a `DirSandbox`
dirfd. The recursive `remove_dir_all` site is the highest-leverage
symlink-swap vector left in the receiver. SEC-1.q opens the trait-
shape refactor that closes the surface; no code ships with this doc.

## Scope

The six GAP sites from PR #4710 section 4:

| # | file:line | call | upstream peer |
|---|---|---|---|
| 22 | `crates/engine/src/delete/emitter/fs.rs:70` | `fs::remove_file(path)` (`unlink_file`) | `robust_unlink` (`delete.c:166`) |
| 23 | `crates/engine/src/delete/emitter/fs.rs:74` | `fs::remove_dir(path)` (`rmdir`) | `do_rmdir` (`delete.c:170`) |
| 24 | `crates/engine/src/delete/emitter/fs.rs:78` | `fs::remove_file(path)` (`unlink_symlink`) | `robust_unlink` |
| 25 | `crates/engine/src/delete/emitter/fs.rs:82` | `fs::remove_file(path)` (`unlink_device`) | `robust_unlink` |
| 26 | `crates/engine/src/delete/emitter/fs.rs:86` | `fs::remove_file(path)` (`unlink_special`) | `robust_unlink` |
| 27 | `crates/engine/src/delete/emitter/fs.rs:90` | `fs::remove_dir_all(path)` (recursive fallback) | `delete_dir_contents` (`delete.c:48-122`) |

Sites #22-#26 each map to one `unlinkat(2)` call; site #27 needs a
custom `openat(2)` + `readdir(2)` + `unlinkat(2)` peel loop. The
six-site count matches PR #4710's "Funnel" cluster (#22-#26) plus
the one Carrier-Funnel double-count for the recursive site (#27).

## Why deferred from SEC-1.g

SEC-1.g shipped
[`unlink_via_sandbox_or_fallback`](../../crates/fast_io/src/dir_sandbox/at_syscalls.rs)
and wired it into the receiver's per-entry delete sites
(`transfer::receiver::directory::links::*`). The `--delete`
traversal that flows through `DeleteFs` was treated as a separate
concern because three blockers all apply at once:

1. **Recursive peel has no single-helper shape.** The non-recursive
   trait methods each fold cleanly into one `unlinkat` call.
   `remove_dir_all` cannot: no `unlinkat`-class primitive removes a
   non-empty directory atomically. The sandbox-anchored replacement
   is a custom `openat(dirfd, name, O_DIRECTORY | O_NOFOLLOW)` +
   `fdopendir` + `readdir` + (recurse | `unlinkat(..., 0)`) +
   `unlinkat(parent, name, AT_REMOVEDIR)` walker with its own
   correctness surface (cycle detection, EBUSY on root-loop,
   ENOTEMPTY races). Shipping that in SEC-1.g would have doubled
   the patch size.
2. **Trait shape change ripples through every impl and every
   caller.** The current `DeleteFs` trait carries only `&self` +
   `&Path`. Adding `parent_fd: BorrowedFd<'_>` forces every
   implementor (production `RealDeleteFs`, `RecordingDeleteFs`
   test fake, blanket `&F` adaptor) to change shape, and forces
   `engine::delete::DeleteEmitter` to acquire a parent dirfd
   before dispatch. The receiver caller in
   `crates/transfer/src/receiver/directory/deletion.rs` (PR #4710
   rows #5-#7) has to land in the same change so the sandbox
   actually arrives.
3. **Test fake does not need sandbox enforcement.**
   `RecordingDeleteFs` asserts dispatch ordering and never touches
   the filesystem. Forcing it to carry a parent dirfd to satisfy a
   trait signature would degrade the emitter unit tests for no
   security benefit. The follow-up needs a shape that keeps the
   path-based methods for the mock impl and for the no-sandbox
   fallback while routing the production impl through dirfd-bearing
   variants.

## Recommended approach

1. **Extend `DeleteFs` with parent-dirfd-bearing methods.** Add
   Unix-only siblings taking a parent dirfd plus a single leaf:

   ```rust
   #[cfg(unix)]
   fn remove_file_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;
   #[cfg(unix)]
   fn remove_dir_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;
   #[cfg(unix)]
   fn remove_dir_all_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;
   ```

   Sites #22-#26 fold into `remove_file_at` / `remove_dir_at`. Site
   #27 gets `remove_dir_all_at` because the implementation shape
   differs (custom peel, step 2).
2. **Ship `recursive_unlinkat_via_sandbox_or_fallback` in `fast_io`.**
   Lives next to `unlink_via_sandbox_or_fallback`. Walks the subtree
   with `openat(O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` + `fdopendir`,
   recurses for child directories, calls `unlinkat(..., 0)` on
   non-directories, finally `unlinkat(parent_fd, name, AT_REMOVEDIR)`
   on the root once empty. Cycle protection follows the `nftw`
   FTW_PHYS pattern: never follow a symlink, never cross filesystem
   boundaries unless caller opts in. Fallback when sandbox is `None`
   stays `std::fs::remove_dir_all`.
3. **Update `RealDeleteFs` to thread the sandbox through.** New
   `*_at` bodies dispatch into the helpers from steps 1-2. Legacy
   path-based methods remain as the no-sandbox fallback. Blanket
   `&F` adaptor mirrors the six-method update.
4. **Thread `Option<&DirSandbox>` into the emitter caller.**
   `DeleteEmitter::emit_all` gains the dirfd-bearing parameter; the
   receiver caller in `crates/transfer/src/receiver/directory/deletion.rs`
   (PR #4710 rows #5-#7) is wired in the same change so SEC-1.q
   closes those three companion GAPs alongside the six trait sites.
5. **Keep `RecordingDeleteFs` path-based.** The fake's `*_at` impls
   discard `parent_fd` and reuse the existing recording logic on
   the leaf, preserving every emitter unit test.

## Estimated effort

~300-500 LoC total:

- Trait extension + blanket impl + `RealDeleteFs` wire-up in
  `crates/engine/src/delete/emitter/fs.rs`: ~80 LoC.
- Recursive peel helper + tests in
  `crates/fast_io/src/dir_sandbox/at_syscalls.rs`: ~200 LoC
  (cycle-detection happy path + ENOTEMPTY / ENOENT race handling).
- Caller wire-up in `DeleteEmitter::emit_all` and receiver
  `deletion.rs`: ~60 LoC.
- Regression tests (sandbox-on dispatch, sandbox-off fallback,
  symlink-swap race repro): ~80 LoC.

Wall-clock: one engineer-day plus CI cycles.

## Cross-platform

Helper and `*_at` trait methods are `#[cfg(unix)]`. Windows continues
dispatching through the existing path-based `DeleteFs` methods that
call `std::fs::remove_file` / `std::fs::remove_dir_all`. Per the
SEC-1.l Windows NTFS handle audit
(`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`),
NTFS handle-based deletion semantics sidestep the symlink-swap
TOCTOU class this follow-up targets, so the Windows fallback is the
correct posture rather than a gap.

## Re-open trigger

Promote OPEN to IN-PROGRESS when any of the following lands:

- A SEC-1 audit demonstrates an attacker can race the `--delete`
  traversal to redirect a destructive operation onto an attacker-
  chosen inode. Until then, SEC-1.g's coverage of the receiver's
  per-entry delete sites is the primary protection and this
  follow-up is defense-in-depth cleanup.
- SEC-1.s (open-helper closure, PR #4710 section 6) lands the
  `openat_via_sandbox_or_fallback` helper that the recursive peel
  loop in step 2 depends on.
- A CVE-class disclosure targets the `--delete` traversal rather
  than the per-entry unlink.

## References

- PR #4710 -
  `docs/audits/sec-1-path-syscall-audit-2026-05-22.md` (parent audit).
- PR #4683 -
  `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md` (closure-doc
  shape mirrored here).
- `crates/engine/src/delete/emitter/fs.rs` lines 68-92 - the six
  GAP sites scoped above.
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs::unlink_via_sandbox_or_fallback`
  - the SEC-1.g helper that the new `remove_file_at` /
  `remove_dir_at` bodies route through.
- Upstream `delete.c:48-176` - reference semantics for
  `delete_dir_contents` and `delete_item`.
