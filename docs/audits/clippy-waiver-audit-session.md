# Clippy waiver audit - current session

Audit of `#[allow(clippy::*)]` and `#[allow(dead_code)]` attributes
introduced in the last 100 commits on `master`
(`ee0b26dd7..ea4ba556a`). The project policy is to fix clippy lints
with code changes rather than silence them, so every waiver added in
this window deserves a justification or a follow-up fix.

## Scope

- 85 Rust files changed in `HEAD~100..HEAD`.
- Source: `git diff HEAD~100 HEAD --name-only -- '*.rs'`.
- Waivers detected on added lines: 14 attributes across 7 files.
- Crates touched: `core`, `fast_io`, `rsync_io`, plus integration
  tests under `tests/`.

The remaining `#[allow(clippy::*)]` / `#[allow(dead_code)]`
occurrences found in the same files (37 more) pre-date this
window; this audit deliberately limits itself to attributes
*introduced* in the last 100 commits.

## Findings

| # | File:line | Waiver | Verdict | Recommended fix |
|---|-----------|--------|---------|-----------------|
| 1 | `crates/core/src/client/remote/async_ssh_transport.rs:562` | `#[allow(dead_code)] // REASON: reserved for upcoming dispatch helpers.` | Remove | Either wire `looks_like_ssh_operand` into a real call site in the same PR that lands the helper, or move the function under `#[cfg(test)]` next to the only consumer (the dispatch tests). "Reserved for upcoming" is exactly the pattern the policy forbids - either the code is needed now or it should not be in the tree yet. |
| 2 | `crates/fast_io/benches/iocp_vs_iouring_matched.rs:143` (`make_payload`) | `#[allow(dead_code)]` | Keep, narrow scope | The helper is only consumed by the `#[cfg(all(target_os = "linux", feature = "io_uring"))]` and `#[cfg(all(target_os = "windows", feature = "iocp"))]` cells, so the warning fires on macOS and on Linux without the feature. Drop the broad attribute and cfg-gate the function itself with the union of the two consumer cfgs (`#[cfg(any(all(target_os = "linux", feature = "io_uring"), all(target_os = "windows", feature = "iocp")))]`). |
| 3 | `crates/fast_io/benches/iocp_vs_iouring_matched.rs:165` (`prepare_workload`) | `#[allow(dead_code)]` | Keep, narrow scope | Same fix as #2. |
| 4 | `crates/fast_io/benches/iocp_vs_iouring_matched.rs:178` (`run_stdfs`) | `#[allow(dead_code)]` | Keep, narrow scope | Same fix as #2. |
| 5 | `crates/fast_io/benches/iocp_vs_stdio.rs:189` (`bench_iocp_vs_stdio`) | `#[allow(clippy::missing_panics_doc)]` | Remove | Add a `# Panics` section to the rustdoc explaining that the bench `.expect()`s on tempdir/IO setup. The lint is pointing at missing user-facing docs, which a one-line doc section satisfies properly. |
| 6 | `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:203` (`bench_stdfs`) | `#[allow(clippy::missing_panics_doc)]` | Remove | Add a `# Panics` section to the rustdoc. Same rationale as #5. |
| 7 | `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:230` (`bench_iouring_regular`) | `#[allow(clippy::missing_panics_doc)]` | Remove | Add a `# Panics` section to the rustdoc. Same rationale as #5. |
| 8 | `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:267` (`bench_iouring_sqpoll`) | `#[allow(clippy::missing_panics_doc)]` | Remove | Add a `# Panics` section to the rustdoc. Same rationale as #5. |
| 9 | `crates/fast_io/src/io_uring/session_pool.rs:298` (`THREAD_RINGS` thread-local) | `#[allow(clippy::type_complexity)]` | Keep with justification | The waiver has a docstring above it explaining why the boxed `RefCell<Option<RawIoUring>>` shape is load-bearing for pointer stability. Acceptable, but rename the rationale into a one-line `// REASON:` comment on the attribute itself to match the project's house style (the rest of the codebase uses inline `// REASON:` comments after waivers). |
| 10 | `crates/fast_io/src/kqueue_stub.rs:14` (module level) | `#![allow(dead_code)]` | Remove | The stub mirrors a public API surface. Mark each unused item with `#[allow(dead_code)]` plus a `// REASON: stub mirrors macOS kqueue API surface` comment, or expose every type via `pub use` from `crates/fast_io/src/lib.rs` so the dead-code lint is satisfied by real reachability. Module-level `#![allow]` hides any future genuinely dead code that creeps in. |
| 11 | `crates/rsync_io/src/ssh/async_transport.rs:202` (`ssh_binary_available`) | `#[allow(dead_code)]` | Remove | Both helpers are only used inside the `OC_RSYNC_SSH_NET` gated test. Move them inside that test or feature-gate the whole test mod on a `cfg(feature = "ssh-net-tests")` so the helpers are reachable when the test is. |
| 12 | `crates/rsync_io/src/ssh/async_transport.rs:207` (`rt`) | `#[allow(dead_code)]` | Remove | Same fix as #11. |
| 13 | `crates/rsync_io/src/ssh/async_transport.rs:285` (`_assert_traits`) | `#[allow(dead_code)]` | Keep with justification | This is the standard "compile-time trait check inside a `#[test]` body" pattern. Add `// REASON: compile-time trait check, never invoked` so the intent is documented. |
| 14 | `tests/integration/delete_event_order_harness.rs:66` (file-level) | `#![allow(dead_code)]` | Remove | This harness lives in `tests/integration/` and is included by multiple test binaries. Each binary uses a subset of the harness, which is what triggers the lint. Split the harness into focused submodules and include only what each binary needs via `mod` declarations, or annotate individual unused-in-this-binary items with `#[allow(dead_code)]` once the harness shape stabilises. A file-level `#![allow]` blocks the lint from catching genuinely orphaned helpers. |

## Priority fixes

1. `crates/core/src/client/remote/async_ssh_transport.rs:562` -
   `looks_like_ssh_operand` is the only "reserved for upcoming"
   waiver. This is the strongest policy violation: the helper is
   either dead and should be deleted, or it has a real consumer that
   the same PR must add.
2. `crates/fast_io/benches/iocp_vs_stdio.rs:189` and
   `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:{203,230,267}` -
   four `missing_panics_doc` waivers that should be rustdoc
   additions, not silenced lints. Cheap one-PR cleanup.
3. `tests/integration/delete_event_order_harness.rs:66` -
   file-level `#![allow(dead_code)]` is a blunt instrument that
   hides every future dead helper in the harness. Replace with
   targeted annotations or a split.
4. `crates/fast_io/benches/iocp_vs_iouring_matched.rs:{143,165,178}` -
   three benches that should be `#[cfg(...)]`-gated on the platforms
   that actually consume them, not silenced.
5. `crates/fast_io/src/kqueue_stub.rs:14` - module-level dead-code
   waiver should become per-item or be eliminated by re-exporting
   the stub types from `lib.rs`.

## Recommendation

Batch fixes 2 through 5 into a single follow-up PR
(`refactor: tighten clippy waivers in fast_io and integration
tests`). They are mechanical and share the same review surface.
Open priority 1 (`async_ssh_transport` dead helper) as its own PR
since the right resolution depends on whether the dispatch helper
work has landed: the choices are "delete the function" or "land the
caller in the same PR", and reviewers should see that decision in
isolation.

Total waivers in scope: 14. Recommended for removal: 10. Recommended
to keep with a tightened justification comment: 4 (entries 2, 3, 4
keep the attribute but narrow it via cfg; entries 9 and 13 keep the
attribute and add a `// REASON:` rationale).
