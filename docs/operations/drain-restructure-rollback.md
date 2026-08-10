# Per-worker drain channels rollback runbook (DPC-4)

Audience: SREs and oncall responders investigating a suspected regression introduced by the per-worker drain channels feature shipped under the `per-worker-drain-channels` Cargo feature flag. Use this runbook to confirm the symptom, capture diagnostics, and roll back affected deployments.

## 1. Scope

This runbook covers operational recovery for the per-worker drain channels feature originally specified in DPC-3 (`docs/design/per-worker-drain-channels.md`, merged via PR #4909) and shipped through the DPC-5 / DPC-6 / DPC-7 follow-up series. It applies once DPC-7 flips the `per-worker-drain-channels` feature default from OFF to ON in a tagged release.

Apply this runbook when a drain-related regression is suspected in a release that ships the feature on by default. Before that flip, the feature is opt-in and rollback is the user toggling the flag off; this runbook becomes the authoritative procedure once `default = ["per-worker-drain-channels", ...]` lands in the workspace `Cargo.toml`.

The design doc this runbook complements:

- `docs/design/per-worker-drain-channels.md` - DPC-3 design, including the in-tree rollback criteria section that informs the triggers below.

Out of scope: rollback of unrelated parallel-apply or reorder-buffer changes. Those have their own runbooks (see cross-references at the end).

## 2. Pre-conditions for rollback

Any one of the following symptoms is sufficient to open this runbook. Stop and capture diagnostics (section 3) before changing any deployment state.

- **Wire-byte parity test failure.** The CI parity gate `crates/engine/tests/per_worker_drain_parity.rs` (added in DPC-5) or its post-DPC-5 successor reports a diff between the Mutex baseline and the per-worker drain path after the `ReorderBuffer` sort step. A single failure is sufficient.
- **Throughput regression > 5% on the cores-vs-throughput bench.** The BR-3i.f harness reports a sustained regression beyond the 5% margin DPC-3 section 8 names. Single-run noise must be discounted; require three consecutive runs above threshold on the reference host before treating it as a rollback trigger.
- **Stress test livelock or starvation at high worker counts.** Observed under T >= 64 worker configurations - the `per_worker_drain_parity.rs` stress shape (T = 64, N = 100_000) blows out its 3x wall-clock multiplier, or an out-of-CI report shows a worker thread that never drains.
- **User-reported intermittent transfer hang during multi-file delta apply.** A hang that reproduces against a multi-file delta workload, disappears when the feature is built off, and is not explained by an io_uring, IOCP, or SSH-transport runbook.

If none of the four triggers fit, this is not a DPC drain regression and a different runbook applies.

## 3. Diagnostic checklist

Capture evidence before changing deployment state. The follow-up investigation (section 7) needs it; without it the regression cannot be reproduced in CI and the feature cannot be safely re-flipped.

- **Reproduce against a known-good baseline.** Rebuild with `cargo build --release -p oc-rsync --no-default-features --features <minimal-set>`, explicitly omitting `per-worker-drain-channels` from the feature list. Confirm the symptom is absent in the baseline build before attributing it to the drain restructure.
- **Capture stack traces from all worker threads if the symptom is a hang.** On Linux: `gdb -p $PID -ex "thread apply all bt" -ex detach -ex quit` or `pstack $PID`. On macOS: `lldb -p $PID -o "thread backtrace all" -o detach -o quit` or `sample $PID 10`. Save the output verbatim; do not summarise.
- **Save the bench output for the regression workload.** For throughput regressions, archive the raw `hyperfine` / `criterion` output, not just the summary. The follow-up investigation needs the per-run distribution to distinguish a true regression from a tail-latency artefact.
- **Note the workload shape.** Record:
  - File count and file-size distribution (min / median / p95 / max).
  - `--workers` and other parallelism flags on the command line.
  - Source and destination types (local FS, NFS, SMB, daemon, SSH).
  - Kernel version (`uname -a`), filesystem (`stat -f`), and CPU topology (`nproc`, `lscpu`).
  - oc-rsync version and the exact `cargo build` feature flags used.
- **Confirm the regression is feature-attributable.** Run the same workload against a binary built with `--no-default-features` (omitting only `per-worker-drain-channels`) and confirm the symptom disappears. If it persists, the drain restructure is not the root cause and a different runbook applies.

## 4. Rollback procedure

Concrete steps to disable the feature for affected users. Apply in order; verify after each step.

1. **Source-build users: rebuild with the feature disabled.** Instruct users to install with `cargo install --git https://github.com/<repo> oc-rsync --no-default-features --features <minimal-set-without-per-worker-drain-channels>`, substituting the published minimal feature set documented for the affected release. Distribution packagers should rebuild their package with the same flags and ship a point release.
2. **Pre-built binary users: pin to the prior release.** Users on pre-built binaries from GitHub Releases (or distro mirrors) should downgrade to the last release tagged before the DPC-7 flip. The release notes for the flipped version cite the prior tag; if not present, `git log --oneline --grep "DPC-7"` against the source identifies the flip commit and the parent tag.
3. **In-tree workaround for ad-hoc builds.** For developers building from the workspace directly, set `default-features = false` for the `engine` crate in their local `Cargo.toml` override, or pass `--no-default-features` to the workspace build and re-enable the unrelated default features explicitly. The minimum feature set required for parity with the default build is published alongside the affected release.
4. **One-line revert for distributions tracking master.** If the feature was flipped via a `default = ["per-worker-drain-channels", ...]` line in the workspace `Cargo.toml`, the corrective revert is a one-line change removing only that entry from the default list. Distribution maintainers tracking master can cherry-pick the revert PR (section 5) to their package recipe.
5. **Verify the rollback.** Re-run the diagnostic workload from section 3 against the rolled-back binary. Confirm:
   - Wire-byte parity test passes (if the trigger was parity).
   - Throughput is back within 5% of the pre-flip baseline (if the trigger was throughput).
   - The hang or livelock does not reproduce across at least three runs (if the trigger was hang or starvation).

Do not declare the rollback complete until at least one of the diagnostic measurements that triggered the runbook is back within tolerance on the rolled-back binary.

## 5. Permanent revert procedure

If the regression is unresolvable in a short cycle (target: one release), revert the default flip and keep the feature opt-in until the root cause is fixed.

1. **Open a revert PR for the DPC-7 default-flip commit.** Use `git revert <flip-commit-sha>` to produce the minimal diff. Title: `revert: per-worker-drain-channels default-on (DPC-7)`. Body cites the rollback trigger from section 2 and links the diagnostic evidence captured in section 3.
2. **Confirm the feature still compiles with the flag explicitly enabled.** After the revert lands, run `cargo build -p engine --features per-worker-drain-channels` (and the corresponding test command) in the CI matrix to confirm opt-in users can still consume the feature. The revert removes the default; it does not delete the implementation.
3. **Update the DPC series tracking task with the revert rationale.** The DPC-4 / DPC-7 / DPC-8 tracking notes get an entry recording the trigger, the diagnostic summary, and the planned re-flip criterion. Without this entry the next DPC-7 attempt has no anchor.
4. **Update the design doc with a "FLIPPED THEN REVERTED" status banner.** Edit `docs/design/per-worker-drain-channels.md` and add a status banner immediately under the title summarising: the flip release, the revert release, the trigger, and a link to the revert PR. The banner stays until DPC-7 successfully re-flips.

The revert does not retire the feature flag. The implementation remains available behind `--features per-worker-drain-channels` so opt-in users (typically multi-GB delta-apply hosts that benchmarked the win locally) can keep it. The default is reverted only because the population-wide signal failed; the local win is still real for hosts that measured it.

## 6. Communication template

Use the following snippet as the basis for release notes and the GitHub issue announcing the revert. Substitute the bracketed values; keep the structure and the workaround pointer.

> The `per-worker-drain-channels` feature, enabled by default in v`<X.Y.Z>`, has been reverted in v`<X.Y.Z+1>` due to `<symptom-summary-from-section-2>`. Users affected can either upgrade to v`<X.Y.Z+1>` (recommended) or build with `--no-default-features --features <minimal-set>` as a temporary workaround. The DPC-7 default flip will be retried after the root cause is fixed; see issue #`<N>` for tracking.

Augment the release notes entry with:

- The exact diagnostic command users can run to confirm whether they are affected (e.g. the BR-3i.f bench invocation, or a `cargo test --features per-worker-drain-channels -p engine -- per_worker_drain_parity` line for the parity case).
- The reference host and workload that surfaced the regression, so users can compare against their own shape.
- A note that opt-in users with measured local wins can still enable the feature explicitly.

## 7. Post-rollback investigation

After the revert lands, the DPC follow-up tasks own three commitments before any re-flip:

- **Reproduce the regression in a CI bench cell so it cannot recur silently.** Add a dedicated bench cell to the BR-3i.f harness (DPC-6 / DPC-8 scope) that exercises the failing workload shape captured in section 3. Without a CI signal the next DPC-7 attempt is blind.
- **Add the regression-triggering workload to DPC-6 / DPC-8 bench coverage.** The workload shape (file count, size distribution, worker count, source / dest type) becomes a permanent bench fixture, not a one-off reproducer. The fixture file name should reference the revert PR for traceability.
- **Document the failure mode in `docs/design/per-worker-drain-channels.md` "Known issues" section.** Add the section if it does not yet exist. Each known issue entry names: the symptom, the workload shape that triggers it, the diagnostic command that confirms it, and the gate that must pass before the next DPC-7 attempt.

The next DPC-7 attempt is blocked on all three commitments. The status banner from section 5 stays in place until the re-flip ships.

## 8. Cross-references

- `docs/design/per-worker-drain-channels.md` - DPC-3 design doc. Section 8 of that doc lists the in-tree rollback criteria this runbook operationalises.
- `docs/design/lockfree-mpsc-drain-design.md` - prior-art MPSC sketch the DPC-3 design extends.
- `docs/design/drain-parallel-consumer-thread.md` - related drain consumer design.
- `docs/audits/drain-parallel-contention.md` - DPC-1 contention audit that motivates the restructure.
- `docs/architecture/drain-error-recovery.md` - error-recovery contract the drain must preserve under both implementations.
- `[[project_drain_parallel_mutex_vec_contention]]` - memory note tracking the contention shape across releases.
- `[[project_apply_batch_write_serial]]` - sibling design point on the apply pipeline. Read DPC-6's bench numbers alongside the apply-batch numbers; both sit on the same critical path.
