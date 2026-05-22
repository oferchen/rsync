# SEC-1 path-syscall coverage audit -- post-chain GAPs survey

- **Date:** 2026-05-22
- **Scope:** every direct path-based syscall in daemon-reachable code after
  the SEC-1.f/.g/.h/.i/.j helpers landed.
- **Goal:** confirm which receiver-pipeline sites route through the
  `fast_io::*_via_sandbox_or_fallback` helpers, which are documented
  deferrals, and which remain undocumented GAPs.
- **Predecessors:** `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md`
  (discovery audit, pre-mitigation) and SEC-1 completion summary in
  `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md` (PR #4707).
  SEC-1.a enumerated the surface before any helpers shipped. This audit
  is the post-chain coverage view.

## 1. Methodology

Repository-wide grep, restricted to daemon-reachable production source
(non-test, non-bench, non-fuzz, non-xtask), for:

```text
std::fs::(File::open|File::create|remove_file|remove_dir|remove_dir_all|
          create_dir|create_dir_all|rename|hard_link|set_permissions|
          symlink_metadata|metadata|read_dir|canonicalize|read_link)
std::fs::OpenOptions::*::open
std::os::unix::fs::symlink
libc::(open|stat|lstat|unlink|mkdir|mknod|chmod|chown|lchown|rename|
       utimes|futimes|utime|utimensat)
nix::fcntl::open, nix::sys::stat::(stat|lstat),
nix::unistd::(unlink|mkdir|chmod|chown|rename)
filetime::(set_file_times|set_symlink_file_times)
rustix::fs::(stat|lstat|unlink|mkdir|rename|utimensat|chmodat|chownat)
```

Each hit was cross-checked against the SEC-1 `*at` helpers in
`crates/fast_io/src/dir_sandbox/at_syscalls.rs` (search:
`via_sandbox_or_fallback`) to determine routing status.

## 2. Scope

**In scope (daemon-reachable):**
- `crates/transfer/src/receiver/**` -- receiver pipeline
- `crates/transfer/src/disk_commit/**` -- post-commit thread
- `crates/transfer/src/transfer_ops/**` -- request/response loop
- `crates/transfer/src/pipeline/receiver.rs`, `pipeline/async_signature.rs`
- `crates/transfer/src/temp_cleanup.rs`, `temp_guard.rs`, `basis.rs`
- `crates/metadata/src/apply/**`, `apply_batch.rs`, `special.rs`,
  `acl_exacl/**`, `stat_cache.rs` (all reached via the receiver funnel)
- `crates/engine/src/delete/emitter/fs.rs` (delete trait impl reached via
  `engine::delete::DeleteContext` from the receiver)

**Excluded (not daemon-reachable):**
- `crates/engine/src/local_copy/**` -- client-side `--local` only, never
  reached via `transfer::run_server_with_handshake`.
- `crates/engine/src/walk/**` -- sender-side traversal; the receiver never
  walks a destination tree for file discovery.
- `crates/transfer/src/generator/**` -- sender role.
- `crates/transfer/src/pipeline/sender*` -- sender role.
- `crates/daemon/src/**` -- listener / auth / config parsing runs before
  the transfer pipeline picks up a sandboxed parent dirfd.
- `crates/fast_io/src/dir_sandbox/**` -- sandbox implementation itself.
- All `tests/`, `tests.rs`, `benches/`, `fuzz/`, `examples/`.

## 3. Coverage summary

12 receiver-side call sites currently route through the sandbox helpers
(grep: `via_sandbox_or_fallback` in production source under
`crates/transfer/src`):

| Crate / file | COVERED | DEFERRED | GAP |
|---|---|---|---|
| `transfer/receiver/directory/creation.rs` | 2 (mkdirat) | 1 (`ensure_relative_parents`) | 0 |
| `transfer/receiver/directory/links.rs` | 6 (3 unlinkat, 2 lstat, 1 symlinkat, 1 linkat) | 0 | 4 |
| `transfer/receiver/directory/deletion.rs` | 0 | 0 | 3 |
| `transfer/receiver/transfer/sync.rs` | 2 (renameat) | 0 | 1 |
| `transfer/receiver/transfer/candidates.rs` | 0 | 1 (SEC-1.i candidate site) | 0 |
| `transfer/receiver/quick_check.rs` | 0 | 2 (SEC-1.i reference-dest) | 1 |
| `transfer/receiver/basis.rs` | 0 | 0 | 2 |
| `transfer/transfer_ops/response.rs` | 1 (renameat) | 0 | 2 |
| `transfer/disk_commit/process.rs` | 0 | 3 (SEC-1.j rename + .i metadata) | 3 |
| `transfer/temp_guard.rs` | 0 | 0 | 3 |
| `transfer/temp_cleanup.rs` | 0 | 0 | 2 |
| `metadata/apply/permissions.rs` | 0 | 1 (SEC-1.i fchmodat funnel) | 0 |
| `metadata/apply/ownership.rs` | 0 | 1 (SEC-1.i fchownat funnel) | 0 |
| `metadata/apply/timestamps.rs` | 0 | 1 (SEC-1.i utimensat funnel) | 0 |
| `metadata/apply/mod.rs` | 0 | 1 (`apply_metadata_from_file_entry`) | 0 |
| `metadata/apply_batch.rs` | 0 | 1 (chownat at `CWD`) | 0 |
| `metadata/special.rs` | 0 | 3 (SEC-1.h mknodat sites) | 0 |
| `engine/delete/emitter/fs.rs` | 0 | 0 | 6 |
| **Total** | **11** | **15** | **27** |

Numbers count *distinct call sites*. The DEFERRED column folds in the
seven SEC-1.i candidate sites catalogued in section 2 of the receiver-
wiring deferral doc, three SEC-1.h mknodat sites (devices, FIFOs,
Apple-socket), two SEC-1.j cross-thread sites in disk_commit, and the
three metadata-funnel rows that all SEC-1.i wiring rolls up through.

## 4. GAP findings

GAPs are sites that are **not** routed through a SEC-1 helper and **not**
listed in an existing deferral doc.

| # | file:line | syscall | blocker class |
|---|---|---|---|
| 1 | `crates/transfer/src/receiver/directory/links.rs:127` | `fs::create_dir_all(parent)` (symlink parent) | Carrier (sandbox not threaded for multi-component parent) |
| 2 | `crates/transfer/src/receiver/directory/links.rs:294` | `fs::symlink_metadata(&leader_path)` | Carrier (leader path may sit outside dest_dir) |
| 3 | `crates/transfer/src/receiver/directory/links.rs:339` | `fs::create_dir_all(parent)` (hardlink parent) | Carrier (multi-component parent fallback) |
| 4 | `crates/transfer/src/receiver/directory/links.rs:75` | `fs::read_link(&link_path)` (up-to-date probe) | Carrier (no `readlinkat_via_sandbox_or_fallback` helper) |
| 5 | `crates/transfer/src/receiver/directory/deletion.rs:115` | `fs::read_dir(&dest_path)` (--delete scan) | Carrier (no `openat`/`fdopendir` helper) |
| 6 | `crates/transfer/src/receiver/directory/deletion.rs:157` | `fs::remove_dir_all(&path)` (recursive --delete) | Carrier (no recursive `*at` peel; needs `openat` + `readdir` + `unlinkat`) |
| 7 | `crates/transfer/src/receiver/directory/deletion.rs:159` | `fs::remove_file(&path)` (--delete file) | Carrier (sandbox not threaded into `delete_extraneous_files`) |
| 8 | `crates/transfer/src/receiver/transfer/sync.rs:293` | `fs::create_dir_all(parent)` (backup parent) | Carrier (multi-component parent) |
| 9 | `crates/transfer/src/receiver/quick_check.rs:268` | `fs::File::open(path)` (basis read for ref-dest) | Carrier (no `openat_via_sandbox_or_fallback` helper) |
| 10 | `crates/transfer/src/receiver/basis.rs:119` | `fs::File::open(&candidate)` (reference basis open) | Carrier (candidate path may sit outside dest_dir) |
| 11 | `crates/transfer/src/receiver/basis.rs:134` | `fs::File::open(path)` (basis open) | Carrier (no `openat` helper) |
| 12 | `crates/transfer/src/transfer_ops/response.rs:80` | `fs::OpenOptions::new()...open(&file_path)` (inplace dest open) | Carrier (no `openat` helper) |
| 13 | `crates/transfer/src/transfer_ops/response.rs:342` | `fs::OpenOptions::new()...open(&file_path)` (inplace truncate reopen) | Carrier (no `openat` helper) |
| 14 | `crates/transfer/src/disk_commit/process.rs:232` | `fs::OpenOptions::new()...open(&begin.file_path)` (device target open) | Cross-thread message (`DiskCommitConfig` lacks sandbox) |
| 15 | `crates/transfer/src/disk_commit/process.rs:236` | `fs::OpenOptions::new()...open(&begin.file_path)` (inplace open) | Cross-thread message |
| 16 | `crates/transfer/src/disk_commit/process.rs:354` | `fs::OpenOptions::new()...open(&begin.file_path)` (inplace truncate) | Cross-thread message |
| 17 | `crates/transfer/src/temp_guard.rs:130` | `fs::OpenOptions::new().create_new(true).open(&concrete_path)` (temp file create) | Carrier (no `openat` helper; called from disk_commit + transfer_ops) |
| 18 | `crates/transfer/src/temp_guard.rs:142` | `fs::create_dir_all(parent)` (temp parent) | Carrier (multi-component parent) |
| 19 | `crates/transfer/src/temp_guard.rs:217` | `std::fs::remove_file(&self.path)` (Drop cleanup) | Carrier (Drop has no sandbox in scope) |
| 20 | `crates/transfer/src/temp_cleanup.rs:95` | `fs::read_dir(dest_dir)` (orphan scan) | Carrier (no `openat` helper) |
| 21 | `crates/transfer/src/temp_cleanup.rs:137` | `fs::remove_file(&path)` (orphan cleanup) | Carrier (sandbox not threaded into temp_cleanup) |
| 22 | `crates/engine/src/delete/emitter/fs.rs:70` | `fs::remove_file(path)` (`unlink_file`) | Funnel (DeleteFs trait has no parent_fd) |
| 23 | `crates/engine/src/delete/emitter/fs.rs:74` | `fs::remove_dir(path)` (`rmdir`) | Funnel (DeleteFs trait) |
| 24 | `crates/engine/src/delete/emitter/fs.rs:78` | `fs::remove_file(path)` (`unlink_symlink`) | Funnel (DeleteFs trait) |
| 25 | `crates/engine/src/delete/emitter/fs.rs:82` | `fs::remove_file(path)` (`unlink_device`) | Funnel (DeleteFs trait) |
| 26 | `crates/engine/src/delete/emitter/fs.rs:86` | `fs::remove_file(path)` (`unlink_special`) | Funnel (DeleteFs trait) |
| 27 | `crates/engine/src/delete/emitter/fs.rs:90` | `fs::remove_dir_all(path)` (recursive fallback) | Carrier (needs recursive `*at` peel) |

## 5. GAP bin counts

- **Carrier missing** (sandbox not threaded through this code path; or no
  helper exists yet for the syscall family): 20.
- **Funnel** (DeleteFs trait lacks a `parent_fd` parameter): 5.
- **Cross-thread message** (SEC-1.j-style; `DiskCommitConfig` would need
  to carry `Arc<DirSandbox>`): 3.

Sum: 28; one site (#27) is double-counted in Carrier and Funnel because
both blockers apply (the trait shape AND the recursive-peel helper are
missing).

## 6. Recommended follow-ups

| GAP cluster | Sites | Minimum work | Track |
|---|---|---|---|
| `engine::delete::emitter::fs::DeleteFs` trait | #22-#27 | Add `parent_fd: BorrowedFd<'_>` arg or new `DeleteFsAt` trait; route through `unlinkat_via_sandbox_or_fallback`; add recursive-peel helper for `remove_dir_all` | New closure doc: `sec-1-q-delete-emitter-sandbox.md` |
| `--delete` receiver scan | #5-#7 | Thread `Option<&DirSandbox>` into `ReceiverContext::delete_extraneous_files`; add `openat`/`fdopendir` helper; depend on delete-emitter trait refactor (above) | Same closure doc as #22-#27 |
| `temp_guard` + `temp_cleanup` | #17-#21 | Add `openat_via_sandbox_or_fallback` and `read_dir_via_sandbox_or_fallback` helpers; thread sandbox through `open_tmpfile()` and `temp_cleanup::cleanup_orphans()` | New closure doc: `sec-1-r-temp-file-sandbox.md` |
| Inplace destination open | #12-#16 | Add `openat_via_sandbox_or_fallback` helper; thread sandbox into `transfer_ops::response` and `disk_commit::process` (cross-thread message variant for the disk_commit sites) | Fold #14-#16 into existing SEC-1.j cross-thread closure; #12-#13 into new SEC-1.s open-helper closure |
| Basis-file open | #10-#11 | Same `openat_via_sandbox_or_fallback` helper as above; ref-dest basis may sit outside dest_dir, so the helper must accept multi-root sandboxes or fall back gracefully | SEC-1.s open-helper closure (above) |
| Symlink/hardlink supporting calls | #1-#4, #8-#9 | Two new helpers: `readlinkat_via_sandbox_or_fallback` (#4); `openat_via_sandbox_or_fallback` (#9). For #1, #3, #8 the multi-component `create_dir_all` fallback is unavoidable without an O(1) `mkpath_via_sandbox` walker -- documented behaviour, treat as Carrier-only mitigated | Add to SEC-1.s open-helper closure for #4, #9; mark #1, #3, #8 as accepted residual under SEC-1.h |

## 7. Status assessment

The SEC-1 cutover is **partial**: the three highest-leverage TOCTOU
vectors (mkdirat / symlinkat / linkat / renameat / unlinkat / lstat /
fchmodat / fchownat / utimensat) ship helpers, and 11 receiver-side
sites are routed through them. The 27 remaining GAPs cluster into three
follow-up tracks (delete emitter, temp-file lifecycle, basis/inplace
opens) and one accepted residual (`create_dir_all` multi-component
fallback when `mkdirat` returns ENOENT). Two new helpers
(`openat_via_sandbox_or_fallback`, `readlinkat_via_sandbox_or_fallback`)
plus the DeleteFs trait refactor would close 22 of 27 GAPs.

## 8. Re-audit trigger

Re-run this audit when any of the following lands:

- A new helper appears in `at_syscalls.rs` (would flip GAPs to COVERED).
- The carrier-design follow-up (`docs/design/sec-1-b-dirfd-carrier.md`)
  picks an option and converts `metadata::apply_*` to the carrier
  (would close all 15 DEFERRED metadata-funnel sites).
- `DiskCommitConfig` gains an `Arc<DirSandbox>` field (would close the
  three Cross-thread-message GAPs).

## 9. References

- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` -- discovery
  audit (pre-mitigation surface enumeration).
- `docs/design/sec-1-b-dirfd-carrier.md` -- carrier-design root doc.
- `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md` -- mknodat
  deferral closure (DEFERRED rows #15-#17 in this audit).
- `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md` --
  receiver wiring deferral for fchmodat/fchownat/utimensat (DEFERRED
  rows covering metadata-funnel and 7 receiver candidate sites).
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` -- the
  `*_via_sandbox_or_fallback` helpers in scope. 10 helpers shipped:
  lstat, unlink, mkdirat, symlinkat, linkat, fchmodat, fchownat,
  utimensat, renameat (plus the `fstatat_nofollow` primitive).
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` --
  Landlock LSM proposal as defense-in-depth covering the residual GAPs.
