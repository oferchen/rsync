# DEP-1 Dependabot Triage Record

Tracking issue: #2443

This document records the triage outcome for the open Dependabot PRs as of
2026-05-18.

## Summary

| PR    | Group              | Outcome | Reason                                                                  |
|-------|--------------------|---------|-------------------------------------------------------------------------|
| #4455 | cargo-metadata     | Skipped | Transitive MSRV violation (`cargo-platform@0.3.3` requires rustc 1.91). |
| #4456 | actions            | Skipped | Pre-existing master `fmt + clippy` failure; required checks red.        |
| #4457 | minor-and-patch    | Skipped | Pre-existing master `fmt + clippy` failure; required checks red.        |

No PRs were merged in this triage pass. All three are blocked on red required
CI checks. Two of them are blocked on the same upstream master regression in
`crates/fast_io/benches/nvme_data_path.rs` (clippy `doc_overindented_list_items`).
The third is blocked on its own dependency-induced MSRV bump.

## #4455 - `chore(deps): bump cargo_metadata from 0.18.1 to 0.23.1`

- Group: `cargo-metadata`
- Diff scope: `Cargo.lock`, `xtask/Cargo.toml` (xtask-only consumer).
- Required check status: `fmt + clippy` FAILURE, `Build oc-rsync and upstream rsync` FAILURE.
- Root cause of failure: dependency resolution fails before any code is
  compiled. The bump pulls in `cargo-platform 0.3.3`, which now requires
  rustc 1.91, while this workspace pins rustc 1.88.0 via `rust-toolchain.toml`.
  CI log excerpt:

  ```
  error: rustc 1.88.0 is not supported by the following package:
    cargo-platform@0.3.3 requires rustc 1.91
  ```

- Decision: skip. We cannot raise our MSRV just to satisfy an xtask helper.
- Follow-up options:
  1. Close the PR and let Dependabot re-open it once we move past rustc 1.91,
     or
  2. Pin `cargo-platform` to a 1.88-compatible version via `cargo update
     --precise` in `xtask/Cargo.toml` if we want to take this bump sooner, or
  3. Wait for upstream `cargo_metadata` to either restore broader MSRV support
     or pin `cargo-platform` more loosely.

## #4456 - `chore(deps): bump the actions group with 4 updates`

- Group: `actions`
- Diff scope: workflow YAML only (`.github/workflows/*.yml`). No Rust source
  or dependency-graph changes.
- Required check status: `fmt + clippy` FAILURE, `Build oc-rsync and upstream
  rsync` FAILURE, `Sequential vs Parallel output` FAILURE.
- Root cause of failure: master itself is currently red. The failing
  `fmt + clippy` job hits clippy's `doc_overindented_list_items` lint in
  `crates/fast_io/benches/nvme_data_path.rs` (lines 31-37). This is unrelated
  to the action-version bumps in the PR. The recent run on master
  (`headSha=1c916cf64`) shows the same Parallel Determinism and Interop
  Validation jobs failing.
- Decision: skip. Even though the change is mechanically a pure
  actions-version bump (Swatinem/rust-cache 2.8.2 to 2.9.1,
  msys2/setup-msys2, taiki-e/install-action, cargo-deny-action) and would
  normally be a green merge, we cannot land it while required checks are red.
- Follow-up: once the `nvme_data_path.rs` clippy regression is fixed on
  master and the affected required checks are green, rebase this PR and
  merge with `gh pr merge 4456 --squash --delete-branch --admin`.

## #4457 - `chore(deps): bump the minor-and-patch group across 1 directory with 6 updates`

- Group: `minor-and-patch`
- Diff scope: `Cargo.lock`, `Cargo.toml` (workspace).
- Bumps:
  - `dashmap` 6.1.0 to 6.2.1 (interim maintenance, MSRV raised to 1.85,
    still under our 1.88 floor).
  - `russh` 0.60.2 to 0.60.3 (CVE-2026-46673 fix - compression ZIP-bomb
    bypass; security-relevant, would be desirable to land).
  - `filetime` 0.2.28 to 0.2.29.
  - `assert_cmd` 2.2.1 to 2.2.2 (test-only).
  - `nix` 0.31.2 to 0.31.3.
  - `openssl` 0.10.79 to 0.10.80 (AES key-wrap-with-padding buffer overflow
    fix).
- Required check status: `fmt + clippy` FAILURE, `Build oc-rsync and upstream
  rsync` FAILURE, `Sequential vs Parallel output` FAILURE.
- Root cause of failure: same as #4456 - master's `nvme_data_path.rs` clippy
  regression. The bump itself does not introduce the lint failures.
- Decision: skip. Despite the russh and openssl security content being
  desirable, we cannot merge while required checks are red.
- Follow-up: prioritize unblocking master's `fmt + clippy` (fix the
  `doc_overindented_list_items` in `crates/fast_io/benches/nvme_data_path.rs`),
  then rebase #4457 and merge. The russh and openssl fixes have security
  implications and should not sit idle for long.

## Required Follow-up Actions

1. Fix the `doc_overindented_list_items` clippy errors at
   `crates/fast_io/benches/nvme_data_path.rs:31-37` on master so the
   `fmt + clippy` required check goes green. This unblocks both #4456 and
   #4457.
2. Decide on a path for #4455:
   - Close it and wait for MSRV to advance, or
   - Pin `cargo-platform` for xtask to keep the rest of the bump.
3. After master is green, re-run the triage and merge #4456 and #4457 with
   `gh pr merge <num> --squash --delete-branch --admin`.

## Triage Procedure

For traceability, the procedure applied to each PR was:

1. `gh pr view <num>` for body, files, and metadata.
2. `gh pr view <num> --json mergeable,mergeStateStatus,statusCheckRollup` to
   read the merge state.
3. `gh pr checks <num>` to enumerate required-check results.
4. For any FAILURE on a required check, inspect the relevant
   `gh run view <run-id> --log-failed --job <job-id>` to capture the root
   cause.
5. Apply the DEP-1 decision tree: green required checks merge with
   `--squash --delete-branch --admin`; red required checks are documented
   here and skipped.
