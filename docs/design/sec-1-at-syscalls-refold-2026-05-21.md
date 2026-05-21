# SEC-1 `*at` helper modules - re-fold plan post SEC-1.j ship

Date: 2026-05-21
Scope: Hygiene follow-up to consolidate the three SEC-1 `*at` helper
modules in `crates/fast_io/src/dir_sandbox/` back into a single
`at_syscalls` namespace once SEC-1.j is on master.
Status: SCHEDULED - execute after SEC-1.j (#4693) is merged. SEC-1.j is
already on `origin/master` as commit `95a62dde9` (PR #4693), so the
trigger is met and this follow-up is ready to run as a separate,
behaviour-free refactor PR. The re-fold is held back from being bundled
into SEC-1.j itself so the security-cutover PR stays minimal and
auditable.
Predecessors:
  - SEC-1.h (PR #4683, merged) - `at_syscalls.rs`
    (`fstatat` / `unlinkat` / `mkdirat` / `symlinkat` / `linkat`).
  - SEC-1.i (PR #4690, merged) - `at_syscalls_metadata.rs`
    (`fchmodat` / `fchownat` / `utimensat`).
  - SEC-1.j (PR #4693, merged) - `at_syscalls_rename.rs` (`renameat` /
    `renameat2` with `RENAME_NOREPLACE`).
Branch name (suggested): `refactor/fast-io-at-syscalls-refold`
PR title (suggested): `refactor(fast_io): re-fold SEC-1 *at helper
modules into a single at_syscalls namespace`

## 1. Rationale

The three-file split was an explicit concurrency hack to let SEC-1.h,
SEC-1.i, and SEC-1.j land in parallel without rebase wars on a single
file. Both later module headers say so in plain text:

- `at_syscalls_metadata.rs`:
  > "The split keeps the file mid-flight with sibling SEC-1.h additions
  > mergeable without conflicts; once both PRs land the two modules may
  > be re-folded into a single `at_syscalls` namespace."
- `at_syscalls_rename.rs`:
  > "Companion to `super::at_syscalls` and `super::at_syscalls_metadata`:
  > ... once all three modules land they may be re-folded into a single
  > `at_syscalls` namespace."

With all three PRs on master, the split has discharged its purpose. The
natural reading order is one consolidated namespace: every helper takes
a parent dirfd plus a single-component leaf, every helper has the same
TOCTOU symlink-swap safety story, every helper exposes a
`*_via_sandbox_or_fallback` companion. Splitting them by syscall
*category* (lstat/unlink/create vs metadata-application vs rename) was
never a design decision, only a merge-conflict avoidance trick.

Consolidation also collapses three identical `use` preambles, three
copies of the "leaf must be NUL-free" rustdoc convention, and three
`#[cfg(unix)]` cliffs in `dir_sandbox/mod.rs` into one. It removes the
load-bearing-but-temporary cross-reference paragraphs in the two
companion module headers (which only exist to point readers at the
sibling files the split created).

## 2. Re-fold plan

Pure file move plus re-export consolidation. Zero behaviour changes.
Single PR, single commit acceptable.

1. **Cat the bodies into `at_syscalls.rs`.** Move
   `at_syscalls_metadata.rs` contents under a section divider:
   ```rust
   // ---------------------------------------------------------------
   // chmod / chown / utimes helpers (SEC-1.i)
   // ---------------------------------------------------------------
   ```
   Then move `at_syscalls_rename.rs` contents under a second divider:
   ```rust
   // ---------------------------------------------------------------
   // rename helpers (SEC-1.j)
   // ---------------------------------------------------------------
   ```
   Preserve every doc string verbatim, every `#[cfg(target_os = "linux")]`
   gate, every `RENAME_NOREPLACE` constant. The two
   sibling-pointer paragraphs in the module headers (see Section 1
   quotes) are deleted because they no longer have a sibling to point
   at; their content is replaced by the section dividers above.
2. **Consolidate `use` statements.** Both companion files have identical
   `use std::ffi::{CString, OsStr}; use std::io; use std::os::fd::...;
   use std::os::unix::ffi::OsStrExt; use std::path::Path;` preambles
   that already exist at the top of `at_syscalls.rs`. The
   `use filetime::FileTime;` line from `at_syscalls_metadata.rs` is the
   only addition.
3. **Delete the two sibling files.** `git rm
   crates/fast_io/src/dir_sandbox/at_syscalls_metadata.rs` and
   `git rm crates/fast_io/src/dir_sandbox/at_syscalls_rename.rs`.
4. **Collapse `dir_sandbox/mod.rs` re-exports.** Today the file has
   three `pub mod` declarations and three `pub use` blocks. After
   re-fold:
   ```rust
   pub mod at_syscalls;

   pub use at_syscalls::{
       AtMetadata, LstatOutcome, UnlinkFlags, fchmodat,
       fchmodat_via_sandbox_or_fallback, fchownat,
       fchownat_via_sandbox_or_fallback, fstatat_nofollow, linkat,
       linkat_via_sandbox_or_fallback, lstat_via_sandbox_or_fallback,
       mkdirat, mkdirat_via_sandbox_or_fallback, renameat,
       renameat_via_sandbox_or_fallback, symlinkat,
       symlinkat_via_sandbox_or_fallback,
       unlink_via_sandbox_or_fallback, unlinkat, utimensat,
       utimensat_via_sandbox_or_fallback,
   };
   ```
   The two `#[cfg(unix)] pub mod` lines and the two `#[cfg(unix)]
   pub use` blocks collapse into one block. The crate is already
   `#[cfg(unix)]` for `dir_sandbox` so a single `pub use` is sufficient.
5. **`crates/fast_io/src/lib.rs` requires no changes.** It already
   re-exports the consolidated set through `pub use dir_sandbox::{...}`
   covering all symbols, so the lib-level re-export block is unaffected
   by the move - it was authored anticipating the re-fold.
6. **Update the module header on `at_syscalls.rs`.** Replace the
   "Today this module carries: ... SEC-1.i-j will extend it ..." block
   with a consolidated list reflecting the final state:
   ```text
   //! Carries the SEC-1.f-j `*at` cutover sites:
   //! - lstat-class (SEC-1.f): `fstatat(AT_SYMLINK_NOFOLLOW)`.
   //! - unlink-class (SEC-1.g): `unlinkat(dirfd, name, 0 | AT_REMOVEDIR)`.
   //! - create-class (SEC-1.h): `mkdirat`, `symlinkat`, `linkat`.
   //! - metadata-application (SEC-1.i): `fchmodat`, `fchownat`,
   //!   `utimensat`.
   //! - rename-class (SEC-1.j): `renameat`, `renameat2` with
   //!   `RENAME_NOREPLACE`.
   ```

## 3. Acceptance

- Zero behaviour changes. Pure file move plus re-export consolidation.
- Every existing call site through `fast_io::*` symbol (`fchmodat`,
  `renameat`, `linkat`, etc.) keeps the same path because the crate-root
  re-export block is unchanged.
- The only call sites that import through
  `crates/fast_io/src/dir_sandbox/at_syscalls_metadata.rs` or
  `at_syscalls_rename.rs` directly are the in-tree `#[cfg(test)] mod
  tests` and the module's own rustdoc cross-links; both move with the
  consolidation. No downstream crate has a direct path through the
  sibling modules.
- CI gates: `cargo fmt --all` plus `cargo nextest run -p fast_io` (run
  by CI per the repo's push-and-let-CI-verify rule). No new tests are
  needed because the change is a pure rename of module paths.

## 4. Estimated LoC and the cap question

| File | LoC on master (`wc -l`) |
|------|-------------------------|
| `at_syscalls.rs` (pre-fold) | 1027 |
| `at_syscalls_metadata.rs` | 670 |
| `at_syscalls_rename.rs` | 514 |
| **Combined `at_syscalls.rs` (post-fold)** | **~2211** |

The combined file is large but the historical
`tools/enforce_limits.sh` LoC cap was removed on 2026-05-18 (the
project memory entry `feedback_loc_limits.md` records "enforce-limits
removed 2026-05-18; LoC is wrong metric; decompose only truly large
files as one-shot hygiene"). No hard cap currently fires on the
consolidated file. The "decompose only truly large files" guidance does
not apply because the post-fold size (~2.2k lines) sits inside the same
order of magnitude as several already-existing
`crates/fast_io/src/dir_sandbox/`-adjacent modules and below
`mod.rs`-style aggregators elsewhere in the repo. The file remains a
flat list of independent `*at` helpers with no internal coupling, so
readability scales linearly with size rather than super-linearly.

**Result: re-fold is recommended.** The combined size is acceptable
under the current (post-2026-05-18) policy. The "N/A path" below is the
contingency the original task brief asked for; it does not apply here.

## 5. N/A path (contingency, not triggered)

If a future LoC policy is reintroduced with a cap below ~2200 lines, or
if the consolidated file develops internal coupling that no longer
scales linearly with reader effort, this follow-up closes as N/A and
the three-file split becomes the durable design. The rationale to
document in that case would be:

- `at_syscalls.rs` (lstat / unlink / create) - reachable from every
  receiver entry creation site.
- `at_syscalls_metadata.rs` (chmod / chown / utimes) - reachable from
  the metadata-application pass only.
- `at_syscalls_rename.rs` (rename / rename2) - reachable from the
  temp-file commit path only.

Those three call-site clusters could justify the split on a "one module
per consumer cluster" reading even though they share the same dirfd /
single-leaf safety story. Under the current policy that justification
is unnecessary and the re-fold wins.

## 6. Re-open trigger

This follow-up re-opens (in the N/A direction) if either of the
following lands first:

1. A new LoC enforcement (xtask or CI lint) with a cap that the
   consolidated file violates.
2. A future SEC-1.k+ task that adds a *fourth* category of `*at`
   helpers with substantially different safety invariants (for example,
   an `openat2`-only path that takes a `RESOLVE_BENEATH` flag the
   sibling helpers cannot use). The current SEC-1.k and SEC-1.l items
   tracked in `docs/security/` are Windows-side and do not add Unix
   `*at` helpers, so this trigger is not active today.

Absent both, the re-fold ships as the natural cleanup once a PR slot is
free.

## 7. References

- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` - SEC-1.f/g/h home,
  carries the lstat/unlink/create-class helpers (1027 LoC at the
  re-fold base).
- `crates/fast_io/src/dir_sandbox/at_syscalls_metadata.rs` - SEC-1.i
  home, carries the chmod/chown/utimes helpers (670 LoC); module
  header explicitly schedules the re-fold once SEC-1.h lands.
- `crates/fast_io/src/dir_sandbox/at_syscalls_rename.rs` - SEC-1.j
  home, carries the rename helpers (514 LoC); module header explicitly
  schedules the re-fold once all three land.
- `crates/fast_io/src/dir_sandbox/mod.rs` - parent-dirfd carrier and
  the `pub mod` / `pub use` block this plan collapses.
- `crates/fast_io/src/lib.rs` - crate-root re-export block; already
  consolidated per the post-fold layout, so no churn there.
- SEC-1.h (PR #4683) - `feat(fast_io): mkdirat/symlinkat/linkat sandbox
  helpers (SEC-1.h)` - created `at_syscalls.rs`.
- SEC-1.i (PR #4690) - `feat(fast_io): fchmodat/fchownat/utimensat
  sandbox helpers (SEC-1.i)` - created `at_syscalls_metadata.rs`.
- SEC-1.j (PR #4693, commit `95a62dde9`) - `feat(fast_io): renameat
  sandbox helper (SEC-1.j)` - created `at_syscalls_rename.rs`.
- `docs/design/sec-1-b-dirfd-carrier.md` - parent design doc for the
  SEC-1 `DirSandbox` carrier the helpers attach to.
- `feedback_loc_limits.md` (project memory) - enforce-limits removal on
  2026-05-18; the reason no LoC cap blocks this re-fold.
