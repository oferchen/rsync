# SEC-1.q2 - Receiver `--delete` sandbox follow-up

- **Status**: OPEN
- **Date**: 2026-05-22
- **Scope owner**: SEC-1 audit chain
- **Predecessor**: SEC-1.q (PR #4728, merged 2026-05-22) added the
  `DeleteFs::*_at` trait surface, the
  [`fast_io::recursive_unlinkat`] direct entry point, and the
  `DeleteEmitter::with_sandbox` carrier slot. Engine-only; the receiver
  caller in `crates/transfer/` was intentionally deferred.
- **Sibling deferral**: SEC-1.q closure doc
  [`docs/design/sec-1-q-delete-emitter-sandbox-2026-05-22.md`](sec-1-q-delete-emitter-sandbox-2026-05-22.md)
  step 4 names this follow-up explicitly.
- **Audit rows closed when this lands**: PR #4710
  `docs/audits/sec-1-path-syscall-audit-2026-05-22.md` section 4 rows
  #5, #6, #7 (`crates/transfer/src/receiver/directory/deletion.rs:115`,
  `:157`, `:159`).

## 1. The gap

SEC-1.q wired the engine. The `DeleteEmitter` now opens a per-plan
parent dirfd off [`fast_io::DirSandbox::root_dirfd`] and routes each
deletion through `unlinkat` / `recursive_unlinkat`. That path is
end-to-end safe only when the caller actually goes through the emitter.

The receiver does not. The `--delete` traversal in
[`crates/transfer/src/receiver/directory/deletion.rs::delete_extraneous_files`](../../crates/transfer/src/receiver/directory/deletion.rs)
runs its own loop:

- `fs::read_dir(&dest_path)` at line 115 (audit row #5) scans each
  destination subdirectory by absolute path.
- `fs::remove_dir_all(&path)` at line 157 (audit row #6) recursively
  unlinks subtrees through `std::fs`.
- `fs::remove_file(&path)` at line 159 (audit row #7) unlinks regular
  files, symlinks, devices, and specials through `std::fs`.

`ReceiverContext` already carries `sandbox: Option<Arc<fast_io::DirSandbox>>`
([`receiver/mod.rs:896`](../../crates/transfer/src/receiver/mod.rs))
opened at `setup_transfer` time
([`receiver/transfer/setup.rs:181-206`](../../crates/transfer/src/receiver/transfer/setup.rs)),
so the carrier is in scope - it is simply not consulted from
`delete_extraneous_files`. The three syscalls above run with the same
unanchored absolute paths regardless of whether the sandbox opened
successfully.

Symptom: a TOCTOU symlink swap on any mid-path component of `dest_path`
or `path` redirects the unlink to an attacker-chosen inode. Bounded by
the SEC-1.p Landlock LSM layer on Linux 5.13+ when the daemon engages
the per-module allowlist, but the `*at` chain itself is open.

The receiver never constructs a `DeleteEmitter`. `ReceiverContext` has a
`delete_ctx: Option<Arc<DeleteContext>>` that publishes `DeletePlan`s
into a shared `DeletePlanMap`
([`receiver/mod.rs:268-280`](../../crates/transfer/src/receiver/mod.rs)),
but no consumer drains the map - the field is wired-in scaffolding for
the parallel-deterministic-delete pipeline (DDP-B3 / DDP-E1-E5, see
[`receiver/file_list/receive.rs:226-240`](../../crates/transfer/src/receiver/file_list/receive.rs)).
Production deletion still goes through the legacy batched-sweep path in
`delete_extraneous_files`.

So the SEC-1.q work is currently engine-only on a code path that no
production caller exercises. Closing the receiver gap is the work this
doc scopes.

## 2. Constraints

These constrain every option below; ignore them and the plan fails CI.

1. **Per-directory, parallel scan shape.** `delete_extraneous_files`
   uses [`parallel_io::map_blocking`](../../crates/transfer/src/parallel_io/mod.rs)
   across a threshold-gated worker pool. Anything we plug in must
   either preserve the parallel scan or accept the regression of
   serialising it.
2. **No `DeletePlan` in scope.** The receiver computes its kill list by
   diffing `read_dir` output against the file list inside the worker
   closure. There is no precomputed plan; `delete_ctx` only stores
   per-segment plans for the not-yet-active emitter pipeline.
3. **`single_component_leaf` is a hard precondition** for the
   `*_via_sandbox_or_fallback` helpers
   ([`crates/fast_io/src/dir_sandbox/at_syscalls.rs:286-303`](../../crates/fast_io/src/dir_sandbox/at_syscalls.rs)).
   A relative path with more than one `Component::Normal` falls back to
   the unanchored path-based syscall. The receiver iterates per
   directory, so the per-entry `name` is always single-component, but
   the dirfd for the *parent* must be opened against the sandbox
   first; otherwise the leaf-anchored call still walks the parent path
   through the kernel.
4. **Sandbox is `#[cfg(unix)]` only.** Windows keeps the path-based
   fallback; the SEC-1.l audit established NTFS handle semantics
   already close the symlink-swap window there.
5. **`DirSandbox` is not `Send` on every platform.** It is held as
   `Arc<DirSandbox>` in `ReceiverContext`, so capturing the `Arc` in
   the `move` closure passed to `map_blocking` works for both the
   parallel and the sequential paths.

## 3. Options

### Option A: Receiver constructs a `DeleteEmitter`

Migrate the receiver loop to the engine's emitter. Inside
`delete_extraneous_files`, build a `DeletePlanMap` from the diff,
publish one `DeletePlan` per destination directory, build a
`DirTraversalCursor`, then drive a
`DeleteEmitter::new(RealDeleteFs, ...).with_sandbox(self.sandbox.clone())`.

**Pros**

- One canonical delete path with SEC-1.q sandbox protection. The
  engine's `dispatch_dir` + `drain_plan` already handle `ENOTEMPTY`
  recursion, the cohort log, error policy, and stat counters.
- Forces the receiver onto the same `EmitterErrorPolicy` semantics that
  the DDP pipeline will eventually adopt, removing one divergence ahead
  of DDP-E1-E5 landing.
- The funnel design pays off: any future trait-level addition (telemetry,
  audit logging) reaches both call sites.

**Cons**

- The receiver scan is fundamentally a directory-listing pass with
  per-entry filter evaluation
  (`FilterChain::allows_deletion`, `max_delete` atomic counter,
  `normalize_filename_for_compare`); building a `DeletePlan` requires
  re-shaping that worker closure to *produce* plans rather than *act*
  on entries. Either the worker emits `DeletePlanEntry` records and a
  serial post-pass drives the emitter, or the emitter is invoked from
  inside each worker (which means N emitters with N sandbox dirfd
  opens, one per directory).
- The cohort, cursor, and pending-plan machinery on `DeleteEmitter` is
  designed for the DDP pipeline's INC_RECURSE segment ordering. The
  receiver's batched-sweep has no cohort index and no cursor; passing
  a synthetic single-segment cursor works but signals architectural
  drift.
- Itemize emission currently happens in a sequential post-pass over
  `per_dir_results` so MSG_INFO frames go through the non-`Send`
  writer in order
  ([`deletion.rs:188-203`](../../crates/transfer/src/receiver/directory/deletion.rs)).
  The emitter currently has no itemize hook; either the receiver keeps
  its post-pass and reads `DeleteEmitter::cohort_records` / `stats`,
  or the emitter grows a callback. Either way, the migration is not a
  drop-in.
- LoC scope: ~250-400 across receiver + tests. Medium-large refactor
  with non-trivial test churn (the existing
  `filter_chain.rs::delete_extraneous_files` tests assert on the
  legacy stat shape and itemize ordering).

**Risk**: medium-high. Touches the `--delete` semantics the interop
suite covers (max-delete enforcement, protect/risk evaluation, itemize
ordering, vanished-file tolerance). Regression surface is large.

**Test cost**: rewrite the two existing
`receiver/tests/file_list/filter_chain.rs` cases, add emitter dispatch
assertions, add a sandbox-on regression with symlink-swap, retain a
sandbox-off fallback test, and re-run the full interop matrix because
the dispatch shape changes for every `--delete` invocation.

### Option B: Receiver calls the sandbox helpers directly

Keep `delete_extraneous_files` shape unchanged. Replace the three
unanchored syscalls with the existing
[`fast_io`](../../crates/fast_io/src/dir_sandbox/at_syscalls.rs)
helpers that already exist:

- `fs::remove_file(&path)` (line 159) becomes
  `fast_io::unlink_via_sandbox_or_fallback(sandbox.as_deref(), dest_dir, &rel_path, &path, UnlinkFlags::File)`.
- `fs::remove_dir_all(&path)` (line 157) becomes
  `fast_io::recursive_unlinkat_via_sandbox_or_fallback(sandbox.as_deref(), dest_dir, &rel_path, &path)`.
- `fs::read_dir(&dest_path)` (line 115) gets a new
  `read_dir_via_sandbox_or_fallback` helper in `fast_io` that opens
  the directory via `openat(O_DIRECTORY | O_NOFOLLOW)` against the
  sandbox dirfd then drives `fdopendir`. This is the only piece
  `fast_io` does not already expose; SEC-1.s
  ([`docs/design/sec-1-s-recursive-unlinkat-helper-2026-05-22.md`](sec-1-s-recursive-unlinkat-helper-2026-05-22.md))
  already lays the groundwork.

The receiver clones `Arc<DirSandbox>` into the `move` closure that
`map_blocking` dispatches per directory, exactly the same pattern
`receiver/transfer/pipelined.rs:54-57` already uses to thread
`setup.sandbox.as_deref()` into `create_directories` and
`create_symlinks`.

**Pros**

- Mechanical. Three syscall sites swap to their `*_via_sandbox_or_fallback`
  siblings. No semantic change to deletion ordering, filter evaluation,
  `max_delete` enforcement, or itemize emission. Existing tests pass
  with the sandbox absent (every helper's documented fallback is the
  current path-based syscall).
- Mirrors the SEC-1.r `temp_guard` / `temp_cleanup` plumbing precedent
  exactly: receiver carries `Option<&Arc<DirSandbox>>` and forwards it
  through helper calls without changing the surrounding control flow
  (see
  [`crates/transfer/src/temp_guard.rs:147-216`](../../crates/transfer/src/temp_guard.rs)
  and
  [`crates/transfer/src/temp_cleanup.rs:102-200`](../../crates/transfer/src/temp_cleanup.rs)).
- Receiver-side parallel scan shape is preserved.
  `Arc<DirSandbox>` is cheap to clone into the closure; the dirfd
  cache inside `DirSandbox` already handles concurrent reads.
- LoC scope: ~80-150 in `deletion.rs` plus ~80-120 for the new
  `read_dir_via_sandbox_or_fallback` helper in `fast_io` (mirrors the
  shape of the existing `recursive_unlinkat_via_sandbox_or_fallback`).

**Cons**

- Two delete code paths persist (the engine's emitter and the
  receiver's `map_blocking` loop). The funnel from Option A does not
  arrive; future cross-cutting concerns get implemented twice.
- One helper still has to be added (`read_dir_via_sandbox_or_fallback`).
  Trivial - it is `openat(O_DIRECTORY | O_NOFOLLOW)` plus
  `fdopendir(3)` - but it is a SEC-1.s-shaped follow-up; treat it as
  part of this option's scope, not a separate doc.

**Risk**: low. Each call site has a documented identical-behaviour
fallback. The blast radius of a bug is a single deletion failing
exactly the way `std::fs` would have failed.

**Test cost**: add one symlink-swap regression test per call site
(file unlink, recursive unlink, directory open), add a sandbox-off
fallback assertion, keep all existing `filter_chain.rs` cases
unchanged. ~6 new test cases total.

### Option C: Defer

Document that receiver-deletion is bounded by the SEC-1.p Landlock LSM
layer and accept the open `*at` chain. Update SECURITY.md to surface
the gap explicitly.

**Pros**

- Zero implementation work. Landlock already enforces the per-module
  filesystem allowlist when the daemon engages it
  ([`crates/daemon/src/daemon/sections/module_access/transfer.rs::engage_landlock_sandbox`](../../crates/daemon/src/daemon/sections/module_access/transfer.rs)),
  so an attacker cannot redirect the unlink outside the module root
  even with a successful symlink swap.

**Cons**

- The SEC-1.q closure ships with a known unreachable end-to-end path:
  the trait surface is correct but no production caller exercises it
  on the receiver side. The investment was made; not collecting it is
  waste.
- Landlock is Linux 5.13+ only and requires the daemon to engage it.
  SSH and local-daemon clients on macOS, Windows-from-Unix, and older
  Linux kernels run with no kernel-side allowlist; the `*at` chain is
  their only defense.
- Adds a documented "Receiver wiring for SEC-1.q deferred" entry to
  SECURITY.md alongside the existing SEC-1.i / SEC-1.j carry-overs.
  Slows the path to full-Fixed status that the SECURITY.md target
  describes
  ([`SECURITY.md:117`](../../SECURITY.md)).

**Risk**: low for kernels with Landlock engaged; medium otherwise. A
future bug that drops Landlock engagement (configuration error,
non-daemon transfer mode) immediately re-opens the TOCTOU window the
SEC-1.q work was supposed to close.

## 4. Recommendation

**Ship Option B.**

Criteria, in priority order:

1. **Security uplift vs. blast radius**. Option B closes audit rows
   #5, #6, #7 with three mechanical syscall swaps. Each swap has a
   documented identical-behaviour fallback so a sandbox-construction
   failure (already a normal path on Unix when the dest dir does not
   yet exist) degrades to today's behaviour. Option A reshapes the
   `--delete` control flow and so carries a regression surface
   disproportionate to the security delta.
2. **Effort and CI churn**. Option B is ~200 LoC including the new
   `read_dir_via_sandbox_or_fallback` helper. Option A is ~250-400
   plus a new emitter-vs-receiver semantic merge. The interop suite
   covers `--delete` heavily; minimising changes to the dispatch
   shape minimises the interop-flake surface.
3. **Precedent**. SEC-1.r threaded `Arc<DirSandbox>` through
   `temp_guard` and `temp_cleanup` using exactly this pattern
   (carrier in scope, helper at the leaf, `*_via_sandbox_or_fallback`
   selector). SEC-1.g threaded the same carrier through
   `receiver/directory/links.rs`. The receiver-deletion loop is the
   last comparable shape; it should follow the same template.
4. **Path to the funnel**. The DDP pipeline (DDP-B3 / DDP-E1-E5) is
   the planned consumer of the emitter funnel. When that lands, the
   receiver's legacy `delete_extraneous_files` will be removed
   wholesale, not refactored. Spending effort now on Option A is paying
   twice for the same transition.
5. **Windows posture**. Option B's `#[cfg(unix)]` gating is local to
   the three call sites; Windows continues through the existing
   `fs::remove_*` calls. Option A would require either splitting the
   emitter migration by `cfg(unix)` or pulling the engine's trait
   surface through to Windows, neither of which buys anything (see the
   SEC-1.l audit conclusion on NTFS handle semantics).

Option A is the architecturally correct end state, but the planned DDP
emitter migration will reach it via a different route (the receiver
loop disappears, not gets refactored). Spending the budget twice is
not justified.

Option C is rejected: the SEC-1.q investment is already spent, and
non-daemon-Linux transfers do not get Landlock coverage.

## 5. Effort estimate (Option B)

**Lines**

- `crates/transfer/src/receiver/directory/deletion.rs`: ~60 net
  additions. Capture `Arc<DirSandbox>` into the worker closure,
  thread it into the existing diff loop, swap the three syscalls for
  their `_via_sandbox_or_fallback` siblings, compute each entry's
  destination-relative path once for the helper preconditions.
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs`: ~80 lines for
  `read_dir_via_sandbox_or_fallback` (the `openat(O_DIRECTORY |
  O_NOFOLLOW)` + `fdopendir` path) plus its rustdoc and `unix`
  cfg-gated tests. Public re-export in
  `crates/fast_io/src/lib.rs`.
- Receiver tests (`crates/transfer/src/receiver/tests/file_list/`):
  ~80 lines for the three symlink-swap regressions + the sandbox-off
  fallback assertion + the `read_dir` symlink-at-root rejection.

Total: ~220 LoC.

**Files touched**

- `crates/transfer/src/receiver/directory/deletion.rs` (call-site
  swap).
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` (new helper).
- `crates/fast_io/src/dir_sandbox/mod.rs` (re-export).
- `crates/fast_io/src/lib.rs` (public re-export entry).
- `crates/transfer/src/receiver/tests/file_list/filter_chain.rs`
  (extend with sandbox-on / sandbox-off assertions).
- `crates/transfer/src/receiver/tests/file_list/` (new
  `delete_sandbox_swap.rs` for the three TOCTOU regressions).

**Test categories**

- Sandbox dispatch: assert the three call sites route through the
  `*at` helpers when `ReceiverContext::sandbox` is `Some`. Use the
  same `DirSandbox::open_root` + tempdir pattern as the existing
  SEC-1.q `crates/engine/src/delete/emitter/tests/sandbox.rs`.
- Symlink-swap regression: per call site, plant a symlink at the
  parent component of the deletion target pointing outside the dest
  tree; assert the helper refuses to follow it (`ELOOP` or `ENOTDIR`)
  and that the target outside the dest tree is untouched. Mirrors
  the SEC-1.s tests at
  `crates/fast_io/src/dir_sandbox/at_syscalls.rs:2873-2950`.
- Sandbox-off fallback: drop the carrier and assert deletion proceeds
  through the `std::fs` path with byte-for-byte identical observable
  behaviour to the pre-change loop (stats, itemize lines, exit code).
- `read_dir_via_sandbox_or_fallback`: refuse-symlink-at-root,
  empty-relative-defaults-to-current-dirfd, multi-component fallback
  paths.

**Cross-platform plan**

- All new code is `#[cfg(unix)]`. Windows continues through
  `fs::remove_file` / `fs::remove_dir_all` / `fs::read_dir`. The
  receiver-side call sites already cfg-gate the sandbox argument
  (see the `setup.sandbox.as_deref()` threading pattern in
  `receiver/transfer/pipelined.rs:54-57`); follow the same shape.
- macOS validated implicitly: the SEC-1.k audit confirmed the
  `*at` syscall family behaves identically to Linux, and the SEC-1.s
  recursive helper has macOS coverage in its existing test suite.

## 6. Dispatch sequencing

1. **This doc lands first** (OPEN status, `docs:` PR). Captures the
   options and the recommendation so reviewers of the implementation
   PR have the rationale in hand.
2. **SEC-1.q2 implementation PR ships next** (`refactor:` or `feat:`
   prefix per content of the change). Promotes status to IN-PROGRESS
   in this doc on push, CLOSED on merge. Closes audit rows #5, #6, #7
   in
   [`docs/audits/sec-1-path-syscall-audit-2026-05-22.md`](../audits/sec-1-path-syscall-audit-2026-05-22.md).
3. **SECURITY.md update** in the same implementation PR or a
   trailing `docs:` PR: move the "Receiver wiring for SEC-1.q
   deferred" note (added implicitly by PR #4728) into the Shipped
   list under SEC-1.q2. The remaining SEC-1.i / SEC-1.j deferred
   wiring entries stay in place.
4. **Re-open trigger**: the same triggers as SEC-1.q apply (a CVE-class
   disclosure against the `--delete` traversal, a SEC-1 audit
   demonstrating a residual race). The DDP pipeline activation
   (DDP-E1-E5) supersedes Option B by removing the legacy loop
   entirely; at that point this follow-up is marked SUPERSEDED-BY-DDP.

## 7. References

- SEC-1.q closure doc:
  [`docs/design/sec-1-q-delete-emitter-sandbox-2026-05-22.md`](sec-1-q-delete-emitter-sandbox-2026-05-22.md).
- SEC-1.q implementation: PR #4728 (commit `899fd7863` on master).
- SEC-1.s recursive-unlinkat helper:
  [`docs/design/sec-1-s-recursive-unlinkat-helper-2026-05-22.md`](sec-1-s-recursive-unlinkat-helper-2026-05-22.md).
- SEC-1.r `temp_guard` / `temp_cleanup` sandbox plumbing (precedent
  for the carrier-threading pattern Option B follows): PR #4723.
- Receiver call site:
  [`crates/transfer/src/receiver/directory/deletion.rs:115`, `:157`,
  `:159`](../../crates/transfer/src/receiver/directory/deletion.rs).
- Receiver sandbox carrier:
  [`crates/transfer/src/receiver/mod.rs:880-897`](../../crates/transfer/src/receiver/mod.rs)
  and
  [`crates/transfer/src/receiver/transfer/setup.rs:181-206`](../../crates/transfer/src/receiver/transfer/setup.rs).
- Audit closures targeted:
  [`docs/audits/sec-1-path-syscall-audit-2026-05-22.md:108-110`](../audits/sec-1-path-syscall-audit-2026-05-22.md)
  (receiver-deletion rows).
- Upstream reference for receiver-side delete semantics:
  `target/interop/upstream-src/rsync-3.4.1/generator.c::delete_in_dir`
  and `delete.c::delete_item`.
