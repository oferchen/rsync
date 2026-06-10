# EDG-SANDBOX silent-skip audit

Audits the `Ok(None)` / `Err(_) => continue` / "log-only" patterns across
the receiver and local-copy executor that can swallow sandbox refusals,
filesystem race errors, or other security-relevant `io::Error` classes
and let the process exit `rc=0` with no work done.

## Scope

PR #5565 found one site (`crates/transfer/src/receiver/directory/creation.rs`
lines 371-385) where a non-`PermissionDenied` error returned by
`mkdirat_via_sandbox_or_fallback` was coerced to `Ok(None)`. The
chdir-symlink-race upstream regression test surfaced the defect: an
`ELOOP` from a mid-syscall symlink swap, an `EOPNOTSUPP`/`Unsupported`
from a sandbox refusal, or an `EEXIST` from a planted leaf was dropped
on the floor and the receiver exited with the directory missing and the
exit code clean.

This audit sweeps the rest of `crates/transfer/src/receiver/` and
`crates/engine/src/local_copy/` for sibling sites that share the shape.
Each hit is classified against four buckets:

- **FIXED by PR #5565** - the same site PR #5565 already targets.
- **SIBLING DEFECT** - the same pattern, security-relevant. Listed as a
  follow-up issue for a separate PR (this PR documents but does not fix
  to avoid bundling unrelated changes).
- **LEGITIMATE SKIP** - upstream-parity continue-on-vanished,
  feature-detection fallback, parser end-of-stream, or a Result-typed
  no-op. No action needed.
- **STYLE** - the swallow returns `()` so the caller has no way to
  surface the error. Changing requires a function-signature change;
  listed as a follow-up but not security-critical given upstream behaves
  the same way.

## Methodology

1. Grep `crates/transfer/src/receiver/` and `crates/engine/src/local_copy/`
   for `return Ok(None)`, `Err(_) => continue`, `Err(_) => return`,
   `.ok()?`, and `let _ = <sandbox-helper>(...)` patterns.
2. For each hit, read the surrounding 20 lines, identify the
   `io::Error` class being swallowed, and decide whether the caller has
   any other channel to learn about the failure (exit code, io_error
   bit, debug log).
3. Cross-reference against the SEC-1.f-q audit rows in
   `crates/fast_io/src/dir_sandbox/at_syscalls.rs` to confirm which
   sites already route through `*_via_sandbox_or_fallback`.
4. Classify each hit against the four buckets above.

## Findings - Phase 1 (receiver + local_copy)

| File:line | Pattern | Err class swallowed | Loop? | rc=0 risk? | Classification |
|-----------|---------|---------------------|-------|------------|----------------|
| `transfer/receiver/directory/creation.rs:373-384` | `if let Err(e) = create_result { ... return Ok(None); }` | ALL classes (ELOOP, EOPNOTSUPP, EACCES, EEXIST, ENOTDIR) from `mkdirat_via_sandbox_or_fallback` | per-entry | YES (silent missing dir + rc=0) | **FIXED by PR #5565** |
| `transfer/receiver/directory/creation.rs:142-159` | `if e.kind() == PermissionDenied { failed_dir_paths.insert; continue } return Err(e);` | EACCES only (rest propagate) | per-entry | NO (upstream parity) | LEGITIMATE SKIP |
| `transfer/receiver/directory/creation.rs:286-296` | `if let Err(e) = fs::create_dir(&dir_path) { if !AlreadyExists { debug_log; break; } }` in `ensure_relative_parents` | ALL non-AlreadyExists classes | per-ancestor | downstream catches | STYLE (function returns `()`; subsequent file create surfaces the real error) |
| `transfer/receiver/directory/deletion.rs:158` | `Err(_) => return (DeleteStats::new(), Vec::new())` after `read_dir_via_sandbox_or_fallback` | ALL classes (ELOOP, EOPNOTSUPP, EACCES) | per-dir worker | YES (deletes skipped, rc=0) | **SIBLING DEFECT** - tracked as follow-up |
| `transfer/receiver/directory/deletion.rs:164` | `Err(_) => return (DeleteStats::new(), Vec::new())` after `std::fs::read_dir` | ALL classes | per-dir worker | YES (deletes skipped, rc=0) | **SIBLING DEFECT** - tracked as follow-up |
| `transfer/receiver/directory/deletion.rs:173,183` | `Err(_) => continue` on `read_dir` entry iteration | ALL classes (per-entry stat race) | per-entry | upstream parity | LEGITIMATE SKIP (matches upstream `generator.c:delete_in_dir`) |
| `transfer/receiver/directory/deletion.rs:302-305` | `Err(e) => debug_log!(...)` after unlink/recursive_unlinkat | ALL non-NotFound classes | per-entry | YES (file persists, rc=0) | **SIBLING DEFECT** - tracked as follow-up |
| `transfer/receiver/directory/links.rs:107,131,342,348` | `let _ = unlink_via_sandbox_or_fallback(...)` (obstacle removal) | ALL classes | per-symlink/hlink | downstream catches | LEGITIMATE SKIP (subsequent `symlinkat`/`linkat` fails with EEXIST and is debug-logged) |
| `transfer/receiver/directory/links.rs:154-160` | `if let Err(e) = symlinkat_via_sandbox_or_fallback(...) { debug_log!(...) }` | ALL classes (incl. sandbox ELOOP) | per-symlink | YES (missing symlink, rc=0) | **SIBLING DEFECT** - tracked as follow-up; `create_symlinks` returns `()` |
| `transfer/receiver/directory/links.rs:382-390` | `if let Err(e) = link_result { debug_log!(...) }` after `linkat_via_sandbox_or_fallback` | ALL classes | per-follower | YES (missing hardlink, rc=0) | **SIBLING DEFECT** - tracked as follow-up; `create_hardlinks` returns `()` |
| `transfer/receiver/file_list/incremental.rs:76` | `return Ok(None)` on `finished_reading` | n/a (stream end) | iterator | NO | LEGITIMATE SKIP (iterator end-of-stream contract) |
| `transfer/receiver/transfer/setup.rs:299` | `Ok(None)` after `DirSandbox::open_root` failure in non-strict mode | ALL classes | once | NO (call site re-routes through path-based) | LEGITIMATE SKIP (`open_sandbox_for_dest_strict(strict=false)` documented soft-fallback; strict mode is the SEC-1 hardening) |
| `transfer/receiver/transfer/sync.rs:321,361` | `renameat_via_sandbox_or_fallback(...)?` | n/a (propagates) | per-file | NO | LEGITIMATE SKIP (errors propagate via `?`) |
| `engine/local_copy/context_impl/state.rs:385,422` | `return Ok(None)` in `link_dest_target` | empty list, ENOENT during stat | per-candidate | NO (upstream parity) | LEGITIMATE SKIP (`--link-dest` candidate vanished mid-walk) |
| `engine/local_copy/context_impl/transfer.rs:8` | `return Ok(None)` when `compress=false` | n/a (no compressor) | per-file | NO | LEGITIMATE SKIP (no-compression path) |
| `engine/local_copy/dir_merge/parse/line.rs:132,137` | `return Ok(None)` for blank/comment lines | n/a (parser) | per-line | NO | LEGITIMATE SKIP (parser "no match") |
| `engine/local_copy/dir_merge/parse/merge.rs:20,25,83,89` | `return Ok(None)` when keyword does not match | n/a (parser) | per-line | NO | LEGITIMATE SKIP |
| `engine/local_copy/dir_merge/parse/dir_merge.rs:31,38` | `return Ok(None)` when keyword does not match | n/a (parser) | per-line | NO | LEGITIMATE SKIP |
| `engine/local_copy/executor/cleanup.rs:175` | `Err(error) if NotFound => return Ok(None)` in `build_plan_for_directory` | ENOENT only | per-dir | NO (upstream `continue-on-vanished`) | LEGITIMATE SKIP |
| `engine/local_copy/executor/file/copy/transfer/execute/iouring.rs:189,204` | `Ok(None)` on io_uring unavailability / `Unsupported` | feature detection | per-file | NO (transparent fallback) | LEGITIMATE SKIP |
| `engine/local_copy/executor/file/copy/transfer/open.rs:79,87` | `Ok(None)` for O_NOATIME rejection | EPERM/EACCES/EINVAL/ENOTSUP/EROFS | per-file | NO (best-effort hint) | LEGITIMATE SKIP (`try_open_noatime` documented as advisory) |
| `engine/local_copy/executor/file/comparison.rs:73,84,106,111` | `Ok(None)` on empty file / signature gen failure / no index | non-IO signature errors | per-file | NO (full copy fallback) | LEGITIMATE SKIP (delta signature degrades to whole-file transfer) |
| `engine/local_copy/executor/file/partial.rs:160-174` | `Ok(None)` when partial basis absent | mode/path lookup | per-file | NO | LEGITIMATE SKIP (mode-driven partial-file search) |
| `engine/local_copy/executor/reference.rs:107` | `Ok(None)` on no match | n/a | per-candidate | NO | LEGITIMATE SKIP |
| `transfer/receiver/basis.rs:134-135` | `fs::File::open(path).ok()?; file.metadata().ok()?` | ALL classes | per-basis | downstream falls back | LEGITIMATE SKIP (alternative basis source; receiver continues with the next candidate or full transfer) |
| `transfer/receiver/basis.rs:209` | `Err(_) => return BasisFileResult::EMPTY` | ALL classes | per-basis | NO (full transfer) | LEGITIMATE SKIP |

## Findings - Phase 2 (DeleteFs trait impls)

`crates/engine/src/delete/emitter/fs.rs` hosts the SEC-1.q delete
dispatch trait. The production impl (`RealDeleteFs`) is a thin
forwarder over `std::fs::remove_*` and `fast_io::unlinkat` /
`fast_io::recursive_unlinkat`. Every method propagates the underlying
`io::Error` verbatim - no `Ok(())` is returned mid-Err handling.

The emitter dispatch loop in `crates/engine/src/delete/emitter/mod.rs`
applies a documented error policy:

- `is_fatal_error` (line 540) classifies `PermissionDenied` as fatal
  and aborts the drain. Mirrors upstream `delete.c:201-205 rsyserr +
  cleanup_and_exit`.
- `record_nonfatal` (line 521) routes every other class into either
  `IOERR_VANISHED_ONLY` (NotFound) or `IOERR_GENERAL` (all other
  classes) so the io_error bit is set and the receiver exits non-zero
  per upstream's `g_exit_code = RERR_PARTIAL` semantics.

| File:line | Pattern | Risk | Classification |
|-----------|---------|------|----------------|
| `engine/delete/emitter/fs.rs:165-221` | `RealDeleteFs` impls forward to `fs::remove_*` / `fast_io::unlinkat` | n/a | LEGITIMATE (every err propagates verbatim) |
| `engine/delete/emitter/fs.rs:334-403` | `RecordingDeleteFs` test fake returns `Ok(())` | n/a | LEGITIMATE (test infrastructure) |
| `engine/delete/emitter/mod.rs:382-394` | `record_nonfatal + continue-on-error` policy after `dispatch` | sandbox refusal classified as non-fatal but io_error bit set, rc!=0 | LEGITIMATE (upstream parity; non-zero exit code surfaces the failure) |
| `engine/delete/emitter/mod.rs:540-542` | `is_fatal_error` only flags `PermissionDenied` | ELOOP/EOPNOTSUPP from sandbox refusal classed as non-fatal | LEGITIMATE (matches upstream; io_error bit drives non-zero exit) |

No silent-skip defects in the trait impls or the emitter dispatch. The
sandbox refusal classes route into `IOERR_GENERAL` via `record_nonfatal`
which sets the receiver's io_error bit and produces a non-zero exit.

## DirSandbox contract (Phase 3)

The audit's classification depends on the documented behaviour of
`DirSandbox::enter`. The unit tests in
`crates/fast_io/src/dir_sandbox/tests.rs` lock the contract:

- `enter_through_symlink_to_outside_refuses` - chdir-symlink-race
  trap. Plants a `subdir -> <outside-tempdir>` symlink and asserts
  `enter("subdir")` returns `ELOOP` (Linux + `openat2` /
  `RESOLVE_NO_SYMLINKS`, also Linux + plain `openat(O_NOFOLLOW)`),
  `ENOTDIR` (macOS/BSD evaluate `O_DIRECTORY` before `O_NOFOLLOW`), or
  `EXDEV` (Linux + `openat2(RESOLVE_BENEATH)` when `..` escapes).
  Stack depth must stay at zero so the receiver's subsequent
  `current_dirfd()` call still anchors on the sandbox root.
- `enter_to_legitimate_subdir_returns_ok` - sibling positive test
  preventing a fail-closed regression on the happy path.

Pre-existing tests (`enter_rejects_symlink_child`,
`enter_rejects_missing_child`, `enter_rejects_file_child`) cover the
intra-tempdir symlink, ENOENT, and ENOTDIR-on-file cases. The new
tests extend the contract to the cross-root trap shape PR #5565 was
written to defend against.

The contract is the foundation for the refined error discrimination
PR #5565 introduces: callers that want to distinguish "EACCES on
destination, continue per upstream `receiver.c:693-700`" from
"sandbox-class refusal, fail loud" must be able to rely on the kernel
returning a stable, documented error code from `DirSandbox::enter` and
the `*_via_sandbox_or_fallback` helpers.

## Follow-up work

Each sibling defect listed below is a separate PR. Bundling would
mix unrelated security fixes and complicate review.

1. **deletion.rs:158/164 read_dir swallow** - `delete_extraneous_files`
   parallel worker drops the read_dir error and returns
   `(DeleteStats::new(), Vec::new())`. A sandbox refusal on a
   destination directory therefore looks like "no deletes needed".
   Fix: thread the error out of the closure (return
   `(DeleteStats, Vec<PathBuf>, Option<io::Error>)`), accumulate into
   an io_error bit, and surface as `Err` from the outer
   `delete_extraneous_files` when the policy demands fail-loud.

2. **deletion.rs:302-305 unlink-failure swallow** - per-entry unlink
   errors are debug-logged but never propagated. The receiver exits
   `rc=0` even when the file the user asked to delete persists. Fix
   by routing through the emitter's `record_nonfatal` policy so
   `IOERR_GENERAL` is set.

3. **links.rs:154-160 symlinkat-failure swallow** - `create_symlinks`
   debug-logs and discards the error. The function returns `()` so a
   real fix requires changing the signature to `io::Result<()>` and
   wiring the caller (`apply_directory_metadata`) to propagate.

4. **links.rs:382-390 linkat-failure swallow** - identical shape to
   `create_symlinks`. Same signature change needed on
   `create_hardlinks`.

5. **creation.rs:286-296 ensure_relative_parents break-on-error** -
   stops at the first non-AlreadyExists error but the function
   returns `()`. The actual mkdir later catches the failure, so this
   is style-grade rather than security-grade.

## References

- PR #5565 - the seed fix for `creation.rs:373-384`.
- SEC-1.f-q audit rows in
  `crates/fast_io/src/dir_sandbox/at_syscalls.rs` - documents every
  `*_via_sandbox_or_fallback` helper and its single-component-leaf
  precondition.
- Upstream `delete.c:48-122 delete_dir_contents`,
  `delete.c:144-176 delete_item`, `delete.c:201-205 rsyserr +
  cleanup_and_exit` - the canonical fatal/non-fatal split the
  emitter mirrors.
- Upstream `receiver.c:693-700` - documents EACCES on `mkdir` as the
  non-fatal increment-io_error-and-continue branch.
