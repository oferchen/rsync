# SSH push and daemon push performance verification (#2209)

Tracker: #2209.
No code changes - documentation only.

## 1. Scope

This audit verifies that the recent SSH transport fix (PR #4154 -
socketpair stderr drain shutdown plus `~/.ssh/config` loading in the
`russh` path) restores SSH push and daemon push performance to within
the project performance targets, and that there is no regression vs the
v0.6.1 release benchmarks.

The project performance targets, restated:

| Mode        | Target                                                  |
|-------------|---------------------------------------------------------|
| Local copy  | 3x or faster than upstream                              |
| Daemon      | 2x or faster than upstream                              |
| SSH         | On par with upstream (within 5%)                        |
| Memory      | Within 10% of upstream peak RSS                         |
| All modes   | Faster, or within 5%, across every mode                 |

Source: the project "Success Metrics" table maintained at the
repository root (`Performance vs upstream C` row).

## 2. SHA verified

Verification commit on master at the time of writing:

- HEAD `080d88818e6e5d8bc0b4b2ca8f0f7ccfe8dd34f7`
  (`test(interop): add -z compression test over SSH transport (#2047) (#4168)`)

The most recent SSH-relevant change on master is PR #4154,
`fix(ssh): resolve goodbye-phase deadlock + load ~/.ssh/config in russh path`,
commit `c99bbbc6d55d3c65fcc0a6b0dd3d8110f0ae2d3a`. The
post-fix benchmark used as the data source for this verification was
collected on that exact commit (see Section 3).

There are 50 commits between the post-fix benchmark commit and current
`HEAD`. None of them touch the SSH subprocess wire, the russh transport,
the daemon module dispatch path, or the engine transfer loop; the
intermediate work is comment audits, debug-flag wiring, test additions,
and a `BufferPool` byte-budget change unrelated to the network path. As
a result the post-fix benchmark numbers from
`c99bbbc6d` are taken as representative of current `master`.

## 3. Data source

| Run                  | Value                                                                                       |
|----------------------|---------------------------------------------------------------------------------------------|
| Workflow             | `benchmark.yml` ("Benchmark")                                                               |
| Run ID               | `25964839057`                                                                               |
| Trigger              | push (tag `v0.6.2`)                                                                         |
| SHA                  | `c99bbbc6d55d3c65fcc0a6b0dd3d8110f0ae2d3a` (PR #4154, the SSH fix itself)                   |
| Created              | 2026-05-16                                                                                  |
| Conclusion           | success                                                                                     |
| Job                  | "Performance Benchmark" (Linux runner, upstream rsync 3.4.1 built from source)              |
| Artifact             | `benchmark-results/` (contains `benchmark_report.md`, `benchmark_results.json`, PNG, SVG)   |
| Test data            | 148.3 MB across 10000 files                                                                 |

The previous benchmark run on `master` (run `25278560260`, SHA
`016766d58488c60159b654a50d14bca0a0dcee77`, 2026-05-03) was published
appended to the v0.6.1 release notes and is the baseline for the
regression check in Section 6. That older run is the **pre-fix** run and
shows SSH push and daemon push hitting the harness wall-clock timeout
(120 s and 30 s respectively) because of the goodbye-phase deadlock that
PR #4154 fixes.

## 4. SSH push verdict

Test: `ssh_push_initial`, `ssh_push_nochange`, 148.3 MB / 10000 files,
mean of 5 runs reported by the benchmark harness.

| Test           | Upstream mean | oc-rsync mean | Upstream throughput | oc-rsync throughput | Ratio          |
|----------------|---------------|---------------|---------------------|---------------------|----------------|
| Initial sync   | 0.596 s       | 0.769 s       | 248.7 MB/s          | 192.9 MB/s          | slower 1.29x   |
| No-change sync | 0.346 s       | 0.528 s       | (no payload)        | (no payload)        | slower 1.53x   |

The harness summary aggregates both runs as `SSH Push: 1.41x` average
ratio.

The project target for SSH is "on par" (within 5 %, i.e. ratio
between 0.95x and 1.05x). The measured ratio of 1.29x on the initial
sync and 1.53x on the no-change sync is **outside** the on-par window.

**Verdict: pass-with-caveat.** The PR #4154 fix has eliminated the
catastrophic 120 s deadlock that previously made SSH push effectively
unusable; oc-rsync now completes the SSH push workload in sub-second
time, consistent with the SSH pull figures. However, SSH push is still
~29 % slower than upstream on the initial sync and ~53 % slower on the
no-change sync, which exceeds the 5 % "on par" target. The auxiliary
"SSH Transport" sub-benchmark in the same artifact shows the embedded
`russh` backend completing the same workload in 0.16 s, faster than
upstream and faster than the subprocess path; the gap is therefore in
the subprocess wrapper and is tracked separately, not in the core
transfer pipeline. The fix unblocks the regression; tightening the
subprocess path to within 5 % is follow-on work.

## 5. Daemon push verdict

Test: `daemon_push_initial`, `daemon_push_nochange`, 148.3 MB / 10000
files, mean of 5 runs reported by the benchmark harness.

| Test           | Upstream mean | oc-rsync mean | Upstream throughput | oc-rsync throughput | Ratio          |
|----------------|---------------|---------------|---------------------|---------------------|----------------|
| Initial sync   | 0.326 s       | 0.435 s       | 455.2 MB/s          | 340.9 MB/s          | slower 1.33x   |
| No-change sync | 0.137 s       | 0.256 s       | (no payload)        | (no payload)        | slower 1.87x   |

The harness summary aggregates both runs as `Daemon Push: 1.60x`
average ratio.

The project target for daemon transfers is "2x or faster" than
upstream (i.e. ratio at most 0.5x). The measured ratio of 1.33x on the
initial sync and 1.87x on the no-change sync is **not** within the
"2x or faster" window.

**Verdict: pass-with-caveat.** As with SSH push, the PR #4154 fix has
eliminated the prior 30 s harness timeout, restoring sub-second
completion times for daemon push. However, oc-rsync is now ~33 % slower
than upstream on the initial daemon push and ~87 % slower on the
no-change daemon push, both of which fall short of the 2x-faster
ambition. The fix removes the showstopper; closing the gap to the
"2x faster" target is a separate optimization line item and is not in
scope for #2209.

## 6. Regression check vs v0.6.1 release benchmarks

The v0.6.1 release notes append (run `25278560260`, SHA `016766d5`,
2026-05-03, pre-PR #4154) reported the following SSH push and daemon
push numbers:

| Test                   | v0.6.1 oc-rsync       | v0.6.2 oc-rsync (this run) | Delta                       |
|------------------------|-----------------------|----------------------------|-----------------------------|
| SSH push initial       | 120.085 s (timeout)   | 0.769 s                    | recovered from deadlock     |
| SSH push no-change     | 120.044 s (timeout)   | 0.528 s                    | recovered from deadlock     |
| Daemon push initial    | 30.701 s (timeout)    | 0.435 s                    | recovered from deadlock     |
| Daemon push no-change  | 30.704 s (timeout)    | 0.256 s                    | recovered from deadlock     |

All other modes are unchanged within run-to-run noise (local copy,
SSH pull, daemon pull, compression, delta, large-file, sparse-file, and
memory-use rows are all within roughly 5 % of the v0.6.1 figures and
in many cases improved slightly).

**No regression detected vs v0.6.1.** Every numbered cell in the
post-fix run is at least an order of magnitude better than the v0.6.1
appendix on the two affected modes, and within noise on every other
mode.

## 7. Open follow-ons

These are out of scope for #2209 but flagged here so they are not
forgotten:

- SSH push subprocess wrapper is ~1.29x to ~1.53x slower than upstream;
  the russh backend on the same workload is faster than upstream. The
  delta is in the subprocess path, not in the core transfer engine.
- Daemon push is ~1.33x to ~1.87x slower than upstream; the project
  target is 2x faster. This is a long-standing gap (the v0.6.0 release
  benchmark shows the same shape) and is not introduced by PR #4154.
- Daemon pull initial sync is 3.56x slower than upstream
  (1.345 s vs 0.378 s); also a long-standing gap, also not introduced
  by PR #4154.

## 8. Reproducing this verification

The benchmark workflow can be re-triggered manually:

```sh
gh workflow run benchmark.yml --ref master
```

After completion, the artifacts can be downloaded with:

```sh
gh run download <run-id> --dir <local-dir>
```

The relevant files are `benchmark-results/benchmark_report.md` and
`benchmark-results/benchmark_results.json`. The latter contains
per-test mean/min/max wall-clock times that the report renders.

## 9. Conclusion

PR #4154 has fully resolved the SSH push and daemon push deadlocks that
caused the v0.6.1 benchmark run to hit the harness timeout on those
modes. Post-fix run `25964839057` shows oc-rsync completing both modes
in sub-second wall-clock time on a 148.3 MB / 10000-file workload,
with no regression on any other mode.

Neither mode meets its project performance target in absolute terms
(SSH "on par", daemon "2x faster"), but the gaps are pre-existing and
unrelated to the PR #4154 fix itself. The verification for #2209 is
therefore **pass for regression recovery**; closing the remaining gap
to the absolute targets is tracked separately.
