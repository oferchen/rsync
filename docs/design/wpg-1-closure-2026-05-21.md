# WPG-1 closure - deferred to post-beta Windows hardware capture

Date: 2026-05-21
Scope: closure note for WPG-1 (oc-rsync #2300, "Windows IOCP hotspot
profile vs MSYS2 rsync"). The methodology and the dependent code
improvements have shipped; the only remaining step - running the
profiler on production-class Windows hardware - cannot be executed in
CI and is deferred to a post-beta hardware-capture cycle.
Status: CLOSED as DEFERRED. Re-opens when a dedicated Windows
benchmark host (bare-metal NVMe) becomes available.
Predecessors and satisfying PRs:

- `docs/audits/windows-iocp-hotspots-methodology.md` - the WPG-1
  profiling runbook. Defines the ETW + WPA capture, the five-workload
  matrix, the hotspot classification buckets, and the comparison
  harness against MSYS2 `rsync 3.4.2`. The Results section was
  intentionally left empty pending a Windows operator run.
- WPG-2 - Windows zero-copy `TransmitFile` fast path in
  `crates/fast_io/src/iocp/transmit_file.rs`; design notes in
  `docs/design/windows-transmitfile.md` and
  `docs/design/windows-transmitfile-zerocopy.md`.
- WPG-3 / PR #4358 - auto-sized IOCP completion-queue depth in
  `crates/fast_io/src/iocp/pump.rs` and `iocp/disk_batch.rs`.
- WPG-4 - Windows-specific buffer-pool tuning: page-aligned buffers,
  bounded pre-allocated buffer ring, double-buffer drain path
  (`crates/fast_io/src/iocp/file_writer.rs`, `iocp/disk_batch.rs`).
- WPG-5 / #2304, PR #4332 - synchronous-blocking audit of the entire
  IOCP module: 14-row inventory plus the three top mitigations
  (M1 `CompletionPump`-backed writer, M2 background fsync finalizer,
  M3 socket-side pipelining). See
  `docs/audits/iocp-sync-blocking-audit.md`.
- WPG-6 - per-hotspot Windows bench cells driven by
  `scripts/windows_throughput_bench.sh` with the
  `OC_RSYNC_BENCH_DRILLDOWN=1` drilldown extension; the five-workload
  matrix `single_huge`, `large_1gib`, `small_10000`,
  `many_small_nested`, `mixed` is enumerated in
  `docs/audits/windows-iocp-hotspots-methodology.md`.

No source changes in this PR. No new design surface added; this note
discharges WPG-1 against shipped artifacts and records the trigger
for re-opening the task.

## 1. Why WPG-1 was the original framing

WPG-1 was filed as the entry point in the WPG ("Windows Performance
Gap") chain. The five sibling tasks (WPG-2 .. WPG-6) were keyed off
its hotspot buckets: each downstream task targeted one source of
synchronous stall in `crates/fast_io/src/iocp/` (see the Hotspot
classification table in
`docs/audits/windows-iocp-hotspots-methodology.md`, Step 5):

- kernel-side bulk copy -> WPG-2 (Windows `TransmitFile` zero-copy
  fast path);
- CQ-depth saturation -> WPG-3 (auto-sized
  `GetQueuedCompletionStatusEx` entry array);
- per-IO blocking drain and bounce-buffer copy -> WPG-4
  (page-aligned buffers, bounded ring, double-buffer drain);
- per-file fsync stall and single-send / single-recv socket pinning
  -> WPG-5 (synchronous-blocking audit and M1/M2/M3 mitigations);
- workload-faithful measurement surface -> WPG-6 (per-hotspot
  Windows bench cells in `scripts/windows_throughput_bench.sh`).

The shipped mitigations cover every bucket the methodology enumerates.
What WPG-1 was also meant to produce - quotable wall-clock numbers
comparing `oc-rsync.exe` against MSYS2 `rsync 3.4.2` on the same
NTFS volume - is the part that depends on hardware not present in CI.

## 2. Why this is deferred and not abandoned

GitHub-actions Windows runners are virtualised, share their NVMe
namespace with other tenants on the host hypervisor, and do not
guarantee storage queue depth or interrupt-coalescing behaviour
between consecutive runs. The variance observed on the existing
`scripts/windows_throughput_bench.sh` runs in CI (already noted in
`docs/audits/windows-iocp-benchmark.md`) is larger than the IOCP
deltas WPG-1 is supposed to measure. Numbers captured under those
conditions are not reproducible across reruns, are not comparable
across host moves, and cannot be quoted in release notes without
caveats large enough to defeat the point.

Quotable numbers require:

- a dedicated bare-metal Windows host (or a hosted runner with a
  guaranteed isolated NVMe namespace);
- AV exclusions configured per the methodology's "Constraints and
  gotchas" section;
- a sustained workload run with the Windows ADK Performance Toolkit
  (`wpr.exe`, `wpa.exe`, `xperf.exe`) attached, which requires
  Administrator and `SeSystemProfilePrivilege`;
- the operator paste-back step from Step 6 of the methodology, where
  the classified hotspot table lands in the audit's empty Results
  section.

None of those preconditions exists in the current CI fleet. The
session author runs macOS / Linux only and has no Windows hardware to
proxy the capture from. The WPG chain progressed as far as it can
without that hardware; closing WPG-1 as deferred is the honest
status, not silent abandonment.

## 3. Coverage already shipped

The IOCP path that WPG-1's measurement was meant to validate has had
every bucket addressed:

| WPG | Outcome | Code / doc citation |
|-----|---------|---------------------|
| WPG-2 | Windows `TransmitFile` zero-copy fast path for socket egress of file payloads | `crates/fast_io/src/iocp/transmit_file.rs`; `docs/design/windows-transmitfile.md`; `docs/design/windows-transmitfile-zerocopy.md` |
| WPG-3 | Auto-sized `GetQueuedCompletionStatusEx` entry array; never-shrinks behaviour replaced with bounded growth | `crates/fast_io/src/iocp/pump.rs` `drain_loop`; `iocp/disk_batch.rs::drain_completions`; PR #4358 |
| WPG-4 | Windows-specific buffer-pool tuning: page-aligned buffers, bounded pre-allocated buffer ring, double-buffer drain path | `crates/fast_io/src/iocp/file_writer.rs`; `iocp/disk_batch.rs` double-buffer path; issue #2303 |
| WPG-5 | Synchronous-blocking audit of all 14 sync points in `crates/fast_io/src/iocp/`; M1 `CompletionPump`-backed writer, M2 background fsync finalizer, M3 socket pipelining (>=2 in flight) | `docs/audits/iocp-sync-blocking-audit.md` (#2304, PR #4332); `iocp/file_writer.rs`; `iocp/disk_batch.rs`; `iocp/socket.rs` |
| WPG-6 | Per-hotspot Windows bench cells across the five-workload matrix (`single_huge`, `large_1gib`, `small_10000`, `many_small_nested`, `mixed`) | `scripts/windows_throughput_bench.sh` with the `OC_RSYNC_BENCH_DRILLDOWN=1` drilldown extension; `docs/audits/windows-iocp-hotspots-methodology.md` Workloads table |

Each row corresponds to a bucket the WPG-1 methodology was meant to
rank. The IOCP path is shipped; the gap is only that the comparison
numbers vs MSYS2 `rsync` are not captured.

## 4. Re-open trigger

WPG-1 re-activates when **any** of the following becomes true:

- a dedicated Windows benchmark host (bare-metal, NVMe-class storage,
  AV exclusions configured) joins the bench fleet;
- a sponsored hosted runner with a documented isolated NVMe namespace
  becomes available (some providers offer dedicated Windows hosts
  with single-tenant NVMe);
- a contributor with a production-class Windows workstation
  volunteers to run the methodology end-to-end and PR the Results
  section.

The first such operator runs Steps 1 to 6 of
`docs/audits/windows-iocp-hotspots-methodology.md` against the
shipped WPG-4 .. WPG-6 binary, pastes the per-workload metrics tables
and the classified hotspot tables into the empty Results sections,
and attaches the `.etl` and `.json` artifacts to issue #2300. WPG-1
then closes on the merit of the captured numbers, not on the deferral
recorded here.

## 5. Beta impact

BR-6 (Final beta-readiness sign-off) currently lists WPG-1 as a
blocker (TaskList: "blocked by #2300 [WPG-1]"). Closing WPG-1 as
deferred unblocks BR-6 on these terms:

- the Windows IOCP path is shipped (WPG-2 .. WPG-6 above) and exercised
  by the existing throughput bench in CI;
- the beta release notes must explicitly call out that the Windows
  IOCP path is shipped but **unprofiled against MSYS2 `rsync` on
  production hardware**, and defer any comparative throughput claims
  to a post-beta hardware-capture cycle;
- the release notes link to this closure document and to
  `docs/audits/windows-iocp-hotspots-methodology.md` so that
  downstream consumers know how to reproduce the comparison
  themselves on their own hardware.

This is consistent with the project memory note
`project_no_windows_io_uring.md`: the Windows IOCP path exists and is
wired, but it is not a full peer of Linux io_uring in terms of
measured performance. The beta posture stays "Windows shipped, not
benchmarked", not "Windows benchmarked at parity".

## 6. Summary table

| Task | Status | Satisfied by | Re-open trigger |
|------|--------|--------------|-----------------|
| WPG-1 (#2300) | CLOSED as DEFERRED | WPG-2 .. WPG-6 shipped the IOCP improvements; methodology runbook landed in `docs/audits/windows-iocp-hotspots-methodology.md` with Results section empty | dedicated Windows benchmark host (bare-metal NVMe) becomes available, or contributor runs the methodology end-to-end |
| BR-6 | unblocked on WPG-1 | this closure note plus release-note caveat that Windows IOCP is shipped but unprofiled vs MSYS2 | comparative throughput claims required before re-opening |

## 7. Cross-references

- `docs/audits/windows-iocp-hotspots-methodology.md` - the runbook
  whose Results section the re-opener fills.
- `docs/audits/iocp-sync-blocking-audit.md` - the WPG-2 inventory the
  WPG-4 / WPG-5 / WPG-6 mitigations addressed.
- `docs/audits/windows-iocp-benchmark.md` and
  `docs/audits/windows-iocp-benchmark-plan.md` - the
  CI-runnable throughput harness that exists today (and whose
  reproducibility gap motivates this deferral).
- `docs/benchmarks/windows-throughput.md` - the per-tag throughput
  job whose numbers are CI-quality, not hardware-capture quality.
- `docs/design/windows-transmitfile.md` and
  `docs/design/windows-transmitfile-zerocopy.md` - the TransmitFile
  fast path cited under WPG-4 / WPG-6 coverage.
- `project_no_windows_io_uring.md` - upstream context on why IOCP is
  not a full peer of io_uring; informs the beta release-note caveat.
