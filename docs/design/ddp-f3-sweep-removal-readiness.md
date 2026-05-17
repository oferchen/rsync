# DDP-F3: legacy batched-sweep removal readiness audit (#2272)

Status: audit only - removal does NOT land in this document.

This audit captures the inventory of code that PR #4280 fences behind
`cfg(feature = "legacy-batched-delete")`, the trigger conditions that
must be met before the legacy path is retired, the rollback surface, the
per-mode verification gates, and a concrete 5-step removal plan.

## 1. Scope and precondition

This audit assumes PR #4280
(`feat(engine): wire DeleteEmitter as live path for all --delete-* timing modes`)
has merged. PR #4280 introduces the `legacy-batched-delete` cargo
feature (off by default) and routes
`local_copy::executor::cleanup::delete_extraneous_entries` through the
new `DeleteEmitter` for all five timing modes. Without that merge there
is no feature flag to remove and DDP-F3 has nothing to act on.

Bench inputs for the go/no-go gate come from PR #4274 (merged), which
landed three criterion benches:

- `delete_plan_compute` (#2282) - phase-1 `compute_extras` scaling.
- `delete_emitter_unlink` (#2283) - phase-2 dispatch overhead.
- `delete_end_to_end` (#2284) - full pipeline vs legacy sweep on
  100K files / 100 dirs / 10% extras.

Run via `cargo bench -p engine --bench delete_end_to_end`.

## 2. Inventory: code gated by `cfg(feature = "legacy-batched-delete")`

After PR #4280 lands, the following call sites carry the gate. Every
item below must be removed (or, where indicated, simplified) by DDP-F3.

### 2.1 Cargo manifest

- `crates/engine/Cargo.toml`
  - Feature declaration: `legacy-batched-delete = []`.

### 2.2 `crates/engine/src/local_copy/executor/cleanup.rs`

- `delete_extraneous_entries<S: AsRef<OsStr>>` (public wrapper):
  contains the `cfg(feature = "legacy-batched-delete")` /
  `cfg(not(...))` branch that dispatches between
  `delete_extraneous_entries_batched` (legacy) and
  `delete_extraneous_entries_via_emitter` (emitter). After removal this
  collapses to an unconditional emitter call.
- `delete_extraneous_entries_via_emitter` - marked
  `#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]`.
  The attribute must be deleted when the feature is gone.
- `delete_extraneous_entries_batched` (private fn introduced by
  PR #4280; the original pre-DDP-E sweep body): entire function under
  `#[cfg(feature = "legacy-batched-delete")]`. Delete.
- `remove_extraneous_path` (private fn): only called by the batched
  sweep; gated `#[cfg(feature = "legacy-batched-delete")]`. Delete.
- `delete_directory_tree_recursive` (private fn): only called by the
  batched sweep; gated `#[cfg(feature = "legacy-batched-delete")]`.
  Delete.

### 2.3 `crates/engine/src/local_copy/executor/mod.rs`

- Conditional re-export pair:

```rust
#[cfg(not(feature = "legacy-batched-delete"))]
#[allow(unused_imports)]
pub(crate) use cleanup::delete_extraneous_entries_via_emitter;
pub(crate) use cleanup::{delete_extraneous_entries, remove_source_entry_if_requested};
```

After DDP-F3 the `#[cfg]` and `#[allow(unused_imports)]` go away and
`delete_extraneous_entries_via_emitter` is re-exported unconditionally
(or inlined into `delete_extraneous_entries` if it has no other
callers).

### 2.4 Callers (unchanged, no `cfg` to remove)

These are call sites of the public `delete_extraneous_entries` wrapper.
They are NOT gated and need no edits, but they are listed so a
reviewer can confirm none accidentally pin the legacy shape:

- `crates/engine/src/local_copy/context_impl/reporting.rs::flush_deferred_deletions`.
- `crates/engine/src/local_copy/executor/directory/recursive/deletion.rs::handle_post_transfer_deletions`.
- `crates/engine/src/local_copy/executor/directory/planner.rs::apply_pre_transfer_deletions`.

### 2.5 Benches

- `crates/engine/benches/delete_end_to_end.rs::run_legacy_sweep`
  reproduces the legacy syscall shape with `fs::read_dir` +
  `fs::remove_file` directly - it does NOT import the production
  legacy code and is not gated. It must stay after DDP-F3 so we
  retain the reference baseline that the removal gate was justified
  against. The doc comment will be updated to clarify the legacy
  production path no longer exists.

### 2.6 Tests

- `crates/engine/tests/delete_emitter_timing_modes.rs` (added by
  PR #4280) targets the emitter path directly; no `cfg` removal
  needed.
- `tests/delete_event_order_{during,before,after,delay,excluded}.rs`
  and `tests/integration/delete_event_order_harness.rs` are gated by
  the `OC_RSYNC_DELETE_INTEROP=1` env var, not by the cargo feature.
  They become permanently green after PR #4280 and stay so after
  DDP-F3.
- `crates/engine/tests/delete_determinism_property.rs` (same env-var
  gate). No change needed.

## 3. Bench-driven removal gate (DDP-I3)

Removal is conditional on three numbers landing inside the budgets
from `docs/design/parallel-deterministic-delete.md` section 9.6. All
three must be measured on the same host in a single bench session so
the comparison is apples-to-apples.

| Bench | Metric | Threshold |
|-------|--------|-----------|
| `delete_end_to_end / parallel_deterministic_delete_during_100k_files` | wall-clock per iteration | within +5% of `legacy_batched_delete_during_100k_files` on the same host |
| `delete_end_to_end` (both arms) | `getrusage` minor + major page faults | parallel arm not worse than legacy by more than 10% |
| `delete_plan_compute / parallel_compute_extras_100k_files_1k_dirs_8_threads` | wall-clock | faster than `serial_compute_extras_baseline` (any speedup) |

Acceptance: all three rows green in two consecutive bench runs on a
quiet host (no other tenants). Record the criterion JSON output under
the DDP-F3 issue (#2272) before opening the removal PR. If the
end-to-end arm misses by more than 5%, defer removal and file a
follow-up against the emitter dispatch overhead measured by
`delete_emitter_unlink`.

## 4. Rollback risk

The legacy path is intentionally retained for one release cycle so
operators can `--features legacy-batched-delete` back to it if a
regression slips through. Removing it before that cycle ends loses
this lever. The concrete regression classes the legacy fallback
protects against:

1. **Per-directory unlink order divergence vs upstream rsync.** The
   emitter sorts via `f_name_cmp` reversed; the legacy sweep relies on
   `fs::read_dir` order. A bug in `DeletePlan::sort_by_name` would
   surface as an interop test failure under
   `OC_RSYNC_DELETE_INTEROP=1`; the legacy fallback lets operators
   roll back without recompiling against an older tag.
2. **Cross-directory deletion interleaving with rename / mkdir.** The
   emitter drains directories in cursor order; if a corner case in
   `DirTraversalCursor` skips a directory or yields one twice, the
   legacy single-thread sweep is the known-good reference.
3. **`--max-delete` accounting under partial drains.** The emitter
   path threads the limit through `apply_delete_side_effects` before
   dispatch; the legacy path counts after each unlink. Any off-by-one
   under partial limits is mitigable by flipping the feature back on
   while the fix lands.
4. **`--partial-dir` protection.** Both paths protect the
   relative partial-dir name; the legacy path is the older, more
   exercised code if a filter regression appears.

Mitigation if removal regresses any of the above: the removal PR
should be revertible cleanly because every change is additive (gate
deletions and dead-code wipes), so a single `git revert` restores the
fallback. Document this in the removal PR body.

## 5. Per-mode verification checklist

The following must all pass with `OC_RSYNC_DELETE_INTEROP=1` set
BEFORE the DDP-F3 removal PR opens. Tests must be invoked against the
binary built with default features (no `legacy-batched-delete`):

- [ ] `tests/delete_event_order_during.rs` (DDP-E1, #2265).
- [ ] `tests/delete_event_order_before.rs` (DDP-E2, #2266).
- [ ] `tests/delete_event_order_after.rs` (DDP-E3, #2267).
- [ ] `tests/delete_event_order_delay.rs` (DDP-E4, #2268).
- [ ] `tests/delete_event_order_excluded.rs` (DDP-E5, #2269).
- [ ] `crates/engine/tests/delete_determinism_property.rs` (H1 + H2
      properties, #2280 / #2281).
- [ ] `crates/engine/tests/delete_emitter_timing_modes.rs` (12 unit
      tests added by PR #4280).
- [ ] `tools/ci/run_interop.sh` against rsync 3.0.9, 3.1.3, 3.4.1 with
      `--delete-during`, `--delete-before`, `--delete-after`,
      `--delete-delay`, and `--delete-excluded` exercised in the
      matrix.

Cross-platform sanity (run in CI, no env gate needed):

- [ ] `cargo nextest run -p engine -E 'test(delete) or test(deletion)'`
      on Linux, macOS, Windows.
- [ ] `cargo check -p engine` (default features) - confirms no
      reference to the removed feature lingers.
- [ ] `cargo check -p engine --features legacy-batched-delete` -
      MUST FAIL with "unknown feature" after DDP-F3 lands; this is
      the smoke test that proves removal completed.

## 6. Recommendation

**Defer DDP-F3 until the gate in section 3 is satisfied on two
consecutive bench runs.** Reasoning:

1. PR #4280 is still open at the time of this audit. The wiring is
   plausibly correct (12 integration tests, single-emitter invariant
   honoured) but has not yet had the
   `OC_RSYNC_DELETE_INTEROP=1` interop sweep run against the merged
   master. The legacy path is cheap to keep for one cycle and is the
   one-flag rollback if a corner case slips.
2. PR #4274 landed the bench scaffolding but not the measurements.
   The removal gate (within 5% end-to-end) is unproven on real
   hardware as of this audit.
3. Rollback risk class 1 (per-directory order divergence) is
   high-impact (interop break) and low-frequency (only surfaces under
   strace-based interop tests). One release cycle of soak time is
   cheap insurance.

Land DDP-F3 IMMEDIATELY after the first release that ships with
PR #4280 merged AND the bench gate green AND the per-mode interop
checklist clean. Do not block on a calendar date; block on the gate.

## 7. Five-step removal plan

Each step is a single commit. Steps 1-4 land in one PR; step 5 lands
in a follow-up cleanup PR if any callers showed up during review.

### Step 1: drop the feature declaration

`crates/engine/Cargo.toml`:

```diff
- # Legacy batched delete sweep - opt-in fallback to the pre-DDP-E code path
- # Benefits: Lets operators flip back to the batched sweep while the
- #   parallel-deterministic-delete emitter is bedding in.
- # Trade-offs: Diverges from upstream rsync's per-directory unlink order;
- #   slated for removal in DDP-F3.
- legacy-batched-delete = []
```

### Step 2: simplify the wrapper

`crates/engine/src/local_copy/executor/cleanup.rs`:

- Replace the `cfg`-branched body of `delete_extraneous_entries` with
  a direct call to the emitter helper:

```diff
- pub(crate) fn delete_extraneous_entries<S: AsRef<OsStr>>(
-     context: &mut CopyContext,
-     destination: &Path,
-     relative: Option<&Path>,
-     source_entries: &[S],
- ) -> Result<(), LocalCopyError> {
-     #[cfg(feature = "legacy-batched-delete")]
-     {
-         delete_extraneous_entries_batched(context, destination, relative, source_entries)
-     }
-     #[cfg(not(feature = "legacy-batched-delete"))]
-     {
-         delete_extraneous_entries_via_emitter(
-             context,
-             destination,
-             relative,
-             source_entries,
-             &RealDeleteFs,
-         )
-     }
- }
+ pub(crate) fn delete_extraneous_entries<S: AsRef<OsStr>>(
+     context: &mut CopyContext,
+     destination: &Path,
+     relative: Option<&Path>,
+     source_entries: &[S],
+ ) -> Result<(), LocalCopyError> {
+     delete_extraneous_entries_via_emitter(
+         context,
+         destination,
+         relative,
+         source_entries,
+         &RealDeleteFs,
+     )
+ }
```

- Drop the `#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]`
  attribute on `delete_extraneous_entries_via_emitter`.

### Step 3: delete dead helpers

`crates/engine/src/local_copy/executor/cleanup.rs`:

- Remove `fn delete_extraneous_entries_batched` (the entire body that
  was gated by `#[cfg(feature = "legacy-batched-delete")]`).
- Remove `fn remove_extraneous_path` and `fn delete_directory_tree_recursive`
  (both legacy-only).
- Remove the now-unused imports they relied on (`std::fs`,
  `std::io::ErrorKind`, `info_log!`, `debug_log!` may still be used
  by other helpers in this file; trim with `cargo check`).

### Step 4: collapse the conditional re-export

`crates/engine/src/local_copy/executor/mod.rs`:

```diff
- #[cfg(not(feature = "legacy-batched-delete"))]
- #[allow(unused_imports)]
- // REASON: kept available for direct tests that bypass the public wrapper
- pub(crate) use cleanup::delete_extraneous_entries_via_emitter;
  pub(crate) use cleanup::{delete_extraneous_entries, remove_source_entry_if_requested};
+ pub(crate) use cleanup::delete_extraneous_entries_via_emitter;
```

If `cargo check -p engine --all-targets` confirms
`delete_extraneous_entries_via_emitter` has no out-of-module callers
once tests run against the public wrapper, drop the re-export entirely
and downgrade the helper to a private function inside `cleanup.rs`.

### Step 5: bench-doc refresh and changelog

- `crates/engine/benches/delete_end_to_end.rs`: update the module
  doc comment (lines 23-36) to state the legacy production path has
  been removed; clarify the legacy bench arm now serves as the
  reference syscall-shape baseline only.
- `docs/design/parallel-deterministic-delete.md` section 10 step 5:
  mark DDP-F3 (#2272) as DONE with a one-line pointer to the removal
  PR.
- Add a `CHANGELOG.md` entry under "Other Changes": the
  `legacy-batched-delete` cargo feature is removed; operators must
  upgrade to the parallel-deterministic-delete emitter (default
  since the release after PR #4280).

## 8. Cross-references

- PR #4280 - wires the emitter and introduces the feature flag.
- PR #4274 - lands the three DDP-I benches.
- PR #4269 - adds the `OC_RSYNC_DELETE_INTEROP`-gated interop suite.
- PR #4264 - introduces `DeleteEmitter`, `DeletePlan`, `DeletePlanMap`,
  `DirTraversalCursor`.
- `docs/design/parallel-deterministic-delete.md` sections 2.3, 9.1,
  9.6, 10.
- Tasks #2265-#2269 (DDP-E1-E5), #2272 (DDP-F3), #2280-#2281
  (DDP-G properties), #2282-#2284 (DDP-I benches).
- Upstream source of truth:
  `target/interop/upstream-src/rsync-3.4.1/generator.c:272-387`
  (`delete_in_dir`, `do_delete_pass`),
  `target/interop/upstream-src/rsync-3.4.1/delete.c:82-225`
  (`delete_item`).
