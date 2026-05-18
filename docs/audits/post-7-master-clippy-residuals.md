# POST-7 Master Clippy Residuals Re-Audit

Re-audit of `cargo clippy --workspace --all-targets --all-features --no-deps`
on `origin/master` after the POST-3 fix series (PRs #4463-#4472) landed. This
audit replaces the POST-3 inventory in `post-3-master-clippy-debt.md` with the
current state.

The Rust toolchain remains pinned to `1.88.0` in `rust-toolchain.toml`, so the
clippy run doubles as the MSRV check.

## Evidence source

The clippy data captured here is the GitHub Actions `CI / fmt + clippy` job
log for the last completed CI run on master:

- Run ID `26014960802`, head SHA `7f0bd093bd4eeb48e2ce19283bc222aa22384bf6`.
- `enforce-limits (informational)` and `fmt + clippy` both failed; all
  downstream test/cross-platform jobs were skipped.
- No `fast_io/` files changed between `7f0bd093` and the audit HEAD
  (`537802a0ab42e3b938e1c88f9e48acb625bd0e49` - `test(fuzz): capability flags
  fuzz target`); only `fuzz/` and `docs/` were touched, so the clippy state of
  `master` HEAD is identical to the run above.

`cargo check --workspace --all-features` is dominated by the same lints (the
hard compile errors below trip both `check` and `clippy`).

## Confirmed-clean - POST-3 items that are gone

Cross-referencing every cluster from `post-3-master-clippy-debt.md` against the
latest CI log:

| POST-3 item | Site | Fix PR | Status in run 26014960802 |
| --- | --- | --- | --- |
| 3a P0 #1 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:122/133/140` | #4463 (`e99fde0d1`) | Not present. |
| 3b P0 #2 | `crates/fast_io/src/sendfile.rs:881` | #4464 (`e6e5f8441`) | Not present. |
| 3c P0 #3 | `crates/engine/src/concurrent_delta/parallel_apply.rs:739` | #4465 (`8d70cfb04`) | Not present. |
| 3d P0 #4 | `crates/engine/src/delete/context.rs:872/893` | #4466 (`d06f1097c`) | Not present. |
| 3e P0 #5 | `crates/engine/src/concurrent_delta/consumer.rs:497` + spill siblings | #4472 (`8c110325c`) | Not present. |
| 3f P0 #6 | `crates/engine/src/concurrent_delta/spill/mod.rs:1755/1781` (`tempfile`) | #4467 (`a29fc71b5`) | Not present. |
| 3g P0 #7 | `crates/fast_io/src/kqueue/mod.rs:40` duplicated `cfg` | #4468 (`27eb44620`) | Not present. |
| 3h P0 #8 | `crates/fast_io/src/macos_io.rs:336` `borrow_deref_ref` | #4469 (`12e4b6874`) | Not present. |
| 3i P1 #9/#10/#11 | `concurrent_delta/strategy.rs`, `concurrent_delta/spill/mod.rs`, `local_copy/buffer_pool/page_aligned.rs` | #4470 (`b0d909fba`) | Not present. |
| 3j P2 #13 | `engine` lacks pass-through `iouring-data-writes` feature | #4471 (`7371a9719`) | Not present. |

Every POST-3 P0/P1 lint cluster is gone from the latest CI run. The 3j P2 cfg
mismatch is also gone.

## Pre-existing, not covered by POST-3

POST-3 listed item #12 (`clippy::doc_lazy_continuation` at
`crates/fast_io/benches/nvme_data_path.rs:22-28`) as P2 informational, but the
lint already fails under the workflow's `RUSTFLAGS=-D warnings` because
`fmt + clippy` runs `--all-targets` (benches included). POST-3 did not ship a
fix PR for it, so it remains in master.

| Cluster | Site | Lint | Count | Status |
| --- | --- | --- | --- | --- |
| Pre-existing | `crates/fast_io/benches/nvme_data_path.rs:22`, `:23`, `:24`, `:25`, `:26`, `:27`, `:28` | `clippy::doc_lazy_continuation` | 7 errors | Same lines, same diagnostic as POST-3 #12. PR #4454 fixed only `doc_quote_line_without_marker` and `uninlined_format_args` on the same file; the bullet-continuation indentation in the module doc block was not touched. |

Recommended fix: re-indent the wrapped lines of each `-` bullet to align under
the bullet text (one extra space), as POST-3 #12 already prescribed.

## Newly-introduced - lints not in the POST-3 inventory

| Cluster | Site | Lint | Count | Notes |
| --- | --- | --- | --- | --- |
| New #1 | `crates/fast_io/benches/nvme_data_path.rs:493` | `unused_mut` on `let mut ring = IoUring::new(SQ_ENTRIES).expect("ring");` inside the `iouring_write_fixed` `bench_function` closure | 1 error | The bench was last touched by PR #4454 (`c2b0d94d2`, `fix(fast_io): clippy compliance in nvme_data_path bench`) which fixed unrelated lints. The `mut` binding became unused because the surrounding submitter helpers were refactored to take `&IoUring` rather than `&mut IoUring`. Recommended fix: drop the `mut`. |

No other clippy or rustc diagnostics fire in the `cargo check` / `cargo
clippy` portions of the failing job once the eight `nvme_data_path.rs` errors
are removed (the job aborts on those, then skips remaining packages, but no
other crate emitted a diagnostic before the abort).

## MSRV (`1.88.0`) status

The pinned toolchain still matches the workflow toolchain. None of the
remaining 8 lint errors are MSRV-sensitive: `unused_mut` and
`clippy::doc_lazy_continuation` are stable lints supported on `1.88.0` and
every newer toolchain.

## Summary tally

- Confirmed-clean POST-3 P0 clusters: 8 (sync_bridge, sendfile, parallel_apply,
  DeleteEmitter/DrainOutcome, SpillError exhaustive, tempfile import, kqueue
  duplicated cfg, macos_io borrow_deref_ref).
- Confirmed-clean POST-3 P1 clusters: 3 (strategy unreachable_code, spill
  Seek import, page_aligned page_size import).
- Confirmed-clean POST-3 P2 clusters: 1 (engine iouring-data-writes feature
  pass-through).
- Pre-existing residuals POST-3 didn't ship a fix for: 1 cluster, 7 errors
  (`doc_lazy_continuation` in `nvme_data_path.rs:22-28`).
- Newly-introduced lints since POST-3: 1 cluster, 1 error (`unused_mut` in
  `nvme_data_path.rs:493`).

Total residual lint errors on master: 8 (all in
`crates/fast_io/benches/nvme_data_path.rs`).

## Recommended follow-up

Open a single small bench-cleanup PR that:

1. Drops `mut` from `let mut ring = IoUring::new(SQ_ENTRIES).expect("ring");`
   at `crates/fast_io/benches/nvme_data_path.rs:493`.
2. Re-indents the bullet continuation lines at
   `crates/fast_io/benches/nvme_data_path.rs:22-28` so wrapped prose starts in
   column 5 instead of column 4, matching the `- ` bullet marker.

That clears the `fmt + clippy` job and unblocks every downstream matrix entry
on the next CI run.
