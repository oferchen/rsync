# SEC-1.r - temp_guard / temp_cleanup sandbox follow-up

- **Status**: OPEN - new follow-up surfaced by the SEC-1 path-syscall
  coverage audit (PR #4710,
  `docs/audits/sec-1-path-syscall-audit-2026-05-22.md`, GAP rows 17-21).
- **Date**: 2026-05-22
- **Scope owner**: SEC-1 audit chain
- **Closes (when implemented)**: 5 daemon-reachable GAP sites in
  `crates/transfer/src/temp_guard.rs` and
  `crates/transfer/src/temp_cleanup.rs`.

## Summary

Every receiver-side regular-file write lands in a temporary file
before the post-commit rename. The temp create and the unlink-on-drop
both run through path-based syscalls (`fs::OpenOptions::create_new`
and `std::fs::remove_file`), neither of which is anchored on the
sandbox dirfd the rest of the receiver pipeline now uses. A symlink
swap on the temp-file parent between the receiver's decide-to-create
moment and the kernel reaching the inode can therefore redirect the
write or the cleanup to an attacker-chosen target.

## Scope - sites covered by this follow-up

| # | file:line | syscall |
|---|---|---|
| 17 | `crates/transfer/src/temp_guard.rs:130` | `fs::OpenOptions::new().create_new(true).open(&concrete_path)` |
| 18 | `crates/transfer/src/temp_guard.rs:142` | `fs::create_dir_all(parent)` (ENOENT recovery) |
| 19 | `crates/transfer/src/temp_guard.rs:217` | `std::fs::remove_file(&self.path)` (Drop) |
| 20 | `crates/transfer/src/temp_cleanup.rs:95` | `fs::read_dir(dest_dir)` (orphan scan) |
| 21 | `crates/transfer/src/temp_cleanup.rs:137` | `fs::remove_file(&path)` (orphan cleanup) |

The two callers that funnel into `open_tmpfile` already pass paths
down and would be the wiring points once the helper lands:

- `crates/transfer/src/disk_commit/process.rs:248`
  (`open_tmpfile(&begin.file_path, config.temp_dir.as_deref())`).
- `crates/transfer/src/transfer_ops/response.rs:91`
  (`open_tmpfile(&file_path, ctx.config.temp_dir)`).

Site #18 is treated as Carrier-only mitigated: a multi-component
parent walk cannot be anchored on a single dirfd without a
`mkpath_via_sandbox` helper, and the audit's accepted-residual
carve-out for `create_dir_all` recovery branches applies.

## Why deferred

Temp-file creation predates the SEC-1 carrier work. Three reasons
kept it out of the SEC-1 ship:

1. **Prerequisite helper missing.** `at_syscalls.rs` ships ten
   `*_via_sandbox_or_fallback` helpers today (lstat, unlink,
   mkdirat, symlinkat, linkat, fchmodat, fchownat, utimensat,
   renameat, plus the `fstatat_nofollow` primitive). There is **no**
   `openat` adaptor. Per the audit's section 6, a new
   `openat_via_sandbox_or_fallback` also closes ~11 other GAPs
   (rows #9-#13 inplace/basis opens), so it is the right
   standalone unit of work.
2. **Drop cleanup needs a sandbox handle, not just a path.**
   `TempFileGuard` stores only a `PathBuf` today and unlinks via
   `std::fs::remove_file(&self.path)` from its `Drop` impl. For the
   unlink-on-drop to be anchored on the same parent dirfd as the
   create, `TempFileGuard` must hold a sandbox handle for the
   lifetime of the guard.
3. **One caller is on the deferred SEC-1.j cross-thread set.**
   `disk_commit/process.rs:248` runs on a worker thread whose
   `DiskCommitConfig` does not yet carry an `Arc<DirSandbox>` (see
   the SEC-1.j cross-thread closure for the sibling inplace opens
   at `:232/:236/:354`). Sandbox plumbing for `temp_guard` and the
   SEC-1.j set should land together.

## Recommended approach

1. **Ship `openat_via_sandbox_or_fallback` (prerequisite, standalone
   PR).** Mirror the existing helper shape:

   ```rust
   pub fn openat_via_sandbox_or_fallback(
       sandbox: Option<&super::DirSandbox>,
       dest_dir: &Path,
       relative_path: &Path,
       link_path: &Path,
       open_options: &fs::OpenOptions,
   ) -> io::Result<fs::File>;
   ```

   `Some(_)` + single-component leaf: issue
   `libc::openat(sandbox.current_dirfd(), leaf, flags, mode)` with
   `OpenOptions` translated to `O_*` flags. Otherwise fall back to
   `open_options.open(link_path)` verbatim.
2. **Extend `TempFileGuard`** with an optional anchor field
   (`{ sandbox: Arc<DirSandbox>, dest_dir: PathBuf, leaf: OsString }`)
   so the Drop cleanup can route through
   `unlink_via_sandbox_or_fallback` against the same parent. The
   path-based branch remains for callers without a sandbox.
3. **Thread the sandbox through `open_tmpfile`** with a new
   optional sandbox + `dest_dir` parameter pair, dispatching the
   create through `openat_via_sandbox_or_fallback` and constructing
   the guard with the matching anchor. The ENOENT recovery branch
   keeps the path-based `create_dir_all` fallback (accepted
   residual).
4. **Wire the two callers**:
   - `transfer_ops/response.rs:91`: carry
     `Option<Arc<DirSandbox>>` on `ResponseContext` next to
     `dest_dir` (PR #4697 pattern).
   - `disk_commit/process.rs:248`: extend `DiskCommitConfig` with
     `sandbox: Option<Arc<DirSandbox>>` per the SEC-1.j cross-thread
     closure. All four sites (three inplace opens plus the
     `temp_guard` open) flip together.
5. **Optional, same PR window**: add
   `read_dir_via_sandbox_or_fallback` (open via the new `openat`,
   then `fdopendir`) and wire `temp_cleanup`, closing rows #20-#21.

## Estimated effort

~200-300 LoC: ~80 for the `openat` helper + tests, ~60 for the
`read_dir` helper + tests, ~60 for the `TempFileGuard` anchor + Drop
change + tests, ~40 for `open_tmpfile` signature + dispatch + tests,
~50 for `ResponseContext` / `DiskCommitConfig` / `temp_cleanup`
wires. Test fixtures reuse the SEC-1.f/.g/.h pattern with a
TOCTOU-swap fixture that races a symlink replacement against the
temp create, the drop, and the orphan scan.

## Re-open trigger

1. Any audit, advisory, or fuzz finding that demonstrates an
   attacker can race the temp-file create or the unlink-on-drop to
   redirect a write onto an attacker-chosen inode.
2. `openat_via_sandbox_or_fallback` lands - the prerequisite is
   gone and Step 2-5 become a single ~150-LoC PR.
3. `DiskCommitConfig` gains `Arc<DirSandbox>` for an unrelated
   reason (e.g. SEC-1.j cross-thread closure ships first); the
   `temp_guard` site at line 248 becomes a same-PR addition.

## Cross-platform notes

Helper and `TempFileGuard` anchor field are Unix-only.

- **Linux**: `libc::openat` is always available. Direct cutover.
- **macOS**: `libc::openat` has been available since 10.10 (Yosemite),
  comfortably below the project floor.
- **Windows**: no direct `openat` equivalent. Per SEC-1.l
  (`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`),
  the Windows path uses `CreateFileW` with handle semantics that
  refuse reparse-point traversal (`FILE_FLAG_OPEN_REPARSE_POINT`,
  `OBJ_DONT_REPARSE`-equivalent). The fallback branch is already the
  right shape - pass through to `open_options.open(link_path)`
  where Windows-specific callers have set those flags.

## Coordination note

`openat_via_sandbox_or_fallback` is the single prerequisite shared
by SEC-1.r (this doc, 5 GAPs), SEC-1.s (open-helper closure named in
audit section 6, rows #9 and #12-#13), and the basis-file open
cluster (rows #10-#11). Recommended sequence:

1. Land `openat_via_sandbox_or_fallback` as a standalone PR (no
   call-site changes).
2. Land SEC-1.r (this doc) closing the 5 temp-file GAPs.
3. Land SEC-1.s in parallel or sequentially closing the remaining
   open-helper GAPs.

## References

- `docs/audits/sec-1-path-syscall-audit-2026-05-22.md` - post-chain
  GAPs survey (PR #4710); rows #17-#21 are this follow-up's scope.
- `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md` - template
  for this closure doc's structure.
- `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md` -
  `ResponseContext::dest_dir` plumbing pattern (PR #4697) used in
  the Step 4 wire.
- `docs/design/sec-1-b-dirfd-carrier.md` - carrier-design root doc.
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` - host crate for
  the new `openat_via_sandbox_or_fallback` helper.
- `crates/transfer/src/temp_guard.rs:122` - `open_tmpfile` entry
  point that gains the optional sandbox argument.
- `crates/transfer/src/temp_guard.rs:212-220` - `TempFileGuard::Drop`
  impl that grows the sandbox anchor.
- `crates/transfer/src/temp_cleanup.rs` - orphan scan / cleanup
  closed by the `read_dir_via_sandbox_or_fallback` helper.
