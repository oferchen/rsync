# SQM-2.a: scoring the three SQPOLL+mmap workaround candidates

Tracking task: SQM-2.a (audit-only). Predecessors: SQM-1.c candidate
enumeration; SMR-2 decision framework
(`docs/design/mmap-vs-sqpoll-decision.md`); SMR resolution catalogue
(`docs/design/mmap-vs-sqpoll-conflict-resolution.md`). Successor:
SQM-2.b will formalise the winning option in a dispatch-site spec.

This doc does not change any source. It scores the three workaround
candidates surfaced by SQM-1.c against six measurable axes and picks
one for SQM-2.b. The current dispatch surface lives at:

- `crates/fast_io/src/io_uring/config.rs:325-373` - `IoUringConfig::build_ring`,
  the defensive disable site. `sqpoll_safe = sqpoll_requested && !mmap_basis_active`.
- `crates/fast_io/src/io_uring_common.rs:110-118` - `IoUringConfig::mmap_basis_active`,
  the plan-level flag.
- `crates/fast_io/src/adaptive_dispatch.rs` - SMR-3c per-file dispatch
  (EWMA throughput tracker), feature-gated behind
  `adaptive-basis-dispatch`. Status-quo dispatch reuses this seam.
- `crates/transfer/src/delta_apply/applicator.rs:154-184` - the
  `BasisWriterKind` selector that forces `BufferedMap` whenever the
  writer is io_uring-backed; the primary defence, with `build_ring`
  being the secondary.

Test seam: `crates/fast_io/tests/repro_sqpoll_mmap.rs` (the SQM-1.b
reproducer; documented in `project_sqpoll_disabled_with_mmap.md`).

## The three candidates (verbatim from SQM-1.c)

1. **`MADV_WILLNEED` prefetch.** Issue `posix_madvise(MADV_WILLNEED)`
   on the basis range before the SQPOLL ring submits an SQE that
   references it. Lets kernel readahead populate PTEs ahead of the
   SQPOLL kthread's first dereference. Hook already exists at
   `crates/fast_io/src/mmap_reader.rs:139-143`
   (`MmapReader::advise_willneed`).

2. **`mlock` the basis window.** Pin the basis pages with `mlock(2)`
   (or `mlock2(MLOCK_ONFAULT)` on Linux 4.4+) for the SQPOLL
   submission lifetime, then `munlock` after completion. Pages cannot
   fault if they are wired; the SQPOLL kthread dereferences resident
   memory in every case.

3. **Per-basis dispatch (status quo).** Keep
   `config.rs:343-370`: when `mmap_basis_active` is set, refuse SQPOLL
   and fall back to a regular ring. The hazard is closed by avoiding
   the combination entirely. SMR-3c (`adaptive_dispatch.rs`) extends
   the same seam with EWMA-driven per-file selection, gated behind a
   Cargo feature.

## Scoring axes

| Axis | Candidate 1: MADV_WILLNEED | Candidate 2: mlock | Candidate 3: per-basis dispatch |
|---|---|---|---|
| NVMe perf retained | 60-85% provisional (best-effort; readahead may be dropped under pressure) | 100% provisional (no fault possible once `mlock` returns) | 0% by construction (SQPOLL stays off whenever mmap is in play) |
| Implementation complexity | Low: ~30 LoC at one site, hook exists | Medium-high: ~120-180 LoC, RLIMIT_MEMLOCK probe, RAII guard, error budget | Zero net new code; SMR-3c already in tree behind feature flag |
| Kernel-version coverage | Linux 2.6+ (every supported kernel); Darwin honours it stronger | `mlock` 2.6+, `mlock2(MLOCK_ONFAULT)` 4.4+; Darwin lacks `MLOCK_ONFAULT` | Any kernel that supports `IORING_SETUP_SQPOLL` (>= 5.13 for `IORING_FEAT_SQPOLL_NONFIXED`); status quo is already gated |
| Failure modes | Pages silently not resident under memory pressure -> race re-opens; no observable failure -> regression hides | EAGAIN on RLIMIT_MEMLOCK exhaustion; ENOMEM under cgroup `memory.high`; partial mlock on signal interruption | SQPOLL never lit; latent perf loss (~10-15% NVMe per `project_sqpoll_disabled_with_mmap.md`) |
| Test surface | `repro_sqpoll_mmap.rs` cannot prove the race is closed (negative result indistinguishable from "kernel happened to readahead in time") | `repro_sqpoll_mmap.rs` can assert wired pages stay resident across SQPOLL submit window; deterministic | `repro_sqpoll_mmap.rs` already covers the status-quo gate; SMR-3c adds EWMA dispatch unit tests |
| Rollback story | Delete the `advise_willneed` call site; one-line revert | Remove the wire/unwire RAII guard; revert the RLIMIT probe; multi-file revert | No rollback needed - it is the baseline; SMR-3c can be turned off via Cargo feature or env var |

## Per-candidate detail

### Candidate 1: MADV_WILLNEED prefetch

**Mechanism.** `posix_madvise(addr, len, POSIX_MADV_WILLNEED)` (Linux
glibc maps to `madvise(MADV_WILLNEED)`) schedules asynchronous
readahead on the file backing the VMA. Returns once readahead is
queued, not once pages are resident. Hint, not guarantee.

**NVMe perf retained.** Provisional 60-85% of the SQPOLL-on baseline.
The readahead population is racy with the SQPOLL kthread's first
touch. On a warm page cache (after the first slide) the hint is a
no-op and SQPOLL runs at full speed. On a cold cache with deep
queue-depth, the kthread can outrun readahead and still take a
fault inside `task_work`, costing the syscall the hint was meant to
save. Per `docs/audits/madvise-willneed-prefault.md`, this is
expected to recover most of the loss only when the prefault window is
sized to absorb the SQPOLL queue depth - which puts the hint on the
critical path and adds its own latency. No real-hardware number
exists; the 60-85% band is the spread between "kernel readahead wins
the race" (best) and "memory pressure drops the request" (worst).

**Implementation complexity.** Low. The hook already exists
(`MmapReader::advise_willneed`,
`crates/fast_io/src/mmap_reader.rs:139-143`). Wire it from the
`MmapStrategy::map_ptr` site
(`crates/transfer/src/map_file/mmap.rs:50-66`) immediately before
SQE submission. Roughly 30 LoC plus a size-threshold guard so the
hint is skipped on small mappings where one readahead window already
covers the slide. Errors are deliberately ignored (`let _ =`); the
hint is advisory.

**Kernel-version coverage.** `posix_madvise` is POSIX-2001 and
present on every supported Linux, macOS, and BSD; Windows no-ops via
`mmap_reader_stub.rs:71-87`. No kernel-version trip-wire. The hint's
*effectiveness* varies (older Linux pre-5.10 has weaker readahead
batching), but the call never fails on supported platforms.

**Failure modes.**

- *Memory pressure.* Under `memory.high` cgroup or global pressure,
  the kernel can satisfy only a partial prefix or drop the request
  entirely (`mm/madvise.c::force_page_cache_readahead`). The SQPOLL
  hazard then re-opens silently - no error from the hint, no CQE
  error from the kthread until the fault stalls and falls through to
  `task_work`.
- *Large basis.* On multi-GiB basis files the hint's window is
  bounded by kernel readahead heuristics (typically 1 MiB at a time);
  multi-MiB SQE windows can outrun the populated range.
- *Signal interruption.* `posix_madvise` does not block on I/O; not
  interruptible. Safe.
- *Transparent hugepages.* `khugepaged` collapses 4 KiB pages into
  2 MiB pages after the hint, invalidating PTEs the SQPOLL kthread
  was about to dereference. Not closed by `MADV_WILLNEED`; the
  audit `io_uring_sqpoll_mmap_pagefault.md` flags this as residual
  risk under "Transparent-hugepage NUMA migrations".

**Test surface.** `repro_sqpoll_mmap.rs` can demonstrate the hint
*runs* and that pages *can* become resident, but cannot prove the
race is closed: a negative result (no `-EFAULT`, no stall) is
indistinguishable from "the kernel happened to readahead in time on
this run". A meaningful test requires either fault injection via
`memory.high` cgroup squeeze (Linux 5.13+, root or `CAP_SYS_ADMIN`)
or instrumenting the SQPOLL kthread via tracepoints
(`io_uring/io_sq_thread:fault`) - both are CI-fragile.

**Rollback story.** Single-site revert. Delete the
`mmap.advise_willneed(...)` call and the size-threshold guard; the
hook itself stays in place (it is also a candidate for future
non-SQPOLL prefetch tuning).

### Candidate 2: `mlock` the basis window

**Mechanism.** `mlock(addr, len)` faults every page in the range and
pins it (`VM_LOCKED`) until `munlock` or process exit. `mlock2(addr,
len, MLOCK_ONFAULT)` (Linux 4.4+) defers the population until the
first touch but still pins on fault. Either way, the SQPOLL kthread
sees resident, non-evictable PTEs for the wired window.

**NVMe perf retained.** Provisional 100% of the SQPOLL-on baseline.
The race is structurally closed: there is no page fault for the
kthread to take. The cost is paid once at `mlock` time (synchronous
fault-in) and once at `munlock` time (unpin); both stay on the user
task's context, off the SQPOLL kthread. For multi-GiB basis files
the per-slide `mlock` cost dominates the SQPOLL win if naively
applied to the whole basis; a windowed `mlock`/`munlock` around each
SQE batch is required to keep the win.

**Implementation complexity.** Medium-high. Roughly 120-180 LoC:

- RLIMIT_MEMLOCK probe at ring-construction time
  (`getrlimit(RLIMIT_MEMLOCK)`), with a sane fallback when the limit
  is too small (default for non-root processes is 64 KiB on most
  distros, 8 MiB on a few - too small for any realistic basis
  window). Without `CAP_IPC_LOCK` this is a hard constraint.
- RAII guard that wires the window before SQE submission and unwires
  on Drop, exception-safe against panics and early returns. Mirrors
  the `RegisteredBufferSlot` ownership pattern at
  `crates/fast_io/src/io_uring/registered_buffers.rs:251-315`.
- Per-window state on `MmapStrategy` to track the currently-wired
  range so consecutive overlapping slides do not re-wire the same
  pages.
- Error budget: ENOMEM / EAGAIN must downgrade to the regular ring
  (the existing `SQPOLL_FALLBACK` path), not abort the transfer.
- New `#[allow(unsafe_code)]` site - `mlock` is a libc call.
  Per the unsafe-code policy this lives in `fast_io` (allowed)
  behind a safe wrapper.

**Kernel-version coverage.** `mlock` is POSIX and present on every
supported kernel. `mlock2(MLOCK_ONFAULT)` requires Linux 4.4+; the
plain `mlock` path is the portable fallback. Darwin has `mlock` but
no `MLOCK_ONFAULT`; FreeBSD has both. Windows uses
`VirtualLock(2)` with similar semantics; cross-platform wrapper
would live in `fast_io`. The SQPOLL path is Linux-only regardless,
so the cross-platform burden is bounded.

**Failure modes.**

- *RLIMIT_MEMLOCK exhaustion.* On `mlock` returning `EAGAIN` or
  `EPERM`, the wrapper must downgrade to the regular ring. The
  downgrade path is the existing `SQPOLL_FALLBACK` flag - same exit
  as the per-basis dispatch.
- *Memory pressure.* `mlock` succeeds or fails atomically; once
  wired, pages are guaranteed resident. Failure surfaces immediately
  rather than as a silent race.
- *Large basis.* Per-window wiring keeps the resident set bounded;
  windows match SQE batch size (typically 64 KiB to 1 MiB), so the
  pinned working set tracks SQPOLL queue depth times window size.
  At depth 32 and 1 MiB windows that is 32 MiB pinned per ring,
  well within typical `RLIMIT_MEMLOCK` once raised.
- *Signal interruption.* `mlock` is not interruptible on Linux
  (kernel ignores `EINTR` for memlock); deterministic.
- *Truncation race.* Wiring does not protect against `truncate(2)`
  shrinking the file beneath the wired range; that surfaces as
  `SIGBUS` in kernel context exactly as for the unmitigated mmap.
  This is the residual risk that the existing mitigation 4
  (`BufferedMap` for io_uring-backed writers, per
  `docs/design/basis-file-io-policy.md`) closes by construction.
  `mlock` alone does not close it.

**Test surface.** `repro_sqpoll_mmap.rs` can deterministically prove
the wired window stays resident across the SQPOLL submit window:
post-mlock, `mincore(2)` (Linux) or `/proc/self/pagemap` confirms
every page is resident, and the SQE submission completes without
falling through to `task_work`. The fault path can be exercised
negatively by attempting to wire under an artificially low
`RLIMIT_MEMLOCK` and asserting the downgrade-to-regular-ring path
fires. Both checks are CI-portable on any Linux runner with
`prlimit` available; no `CAP_SYS_ADMIN` needed for the negative
case.

**Rollback story.** Multi-file revert. Remove the RAII guard, the
wiring call site, the RLIMIT probe, and the unsafe-wrapper module.
Approximately the same surface as forward. The downgrade-to-regular
path is the existing `SQPOLL_FALLBACK`, which stays in place either
way - so a rollback degrades to candidate 3 rather than to a broken
state.

### Candidate 3: per-basis dispatch (status quo, official)

**Mechanism.** The defensive disable at
`config.rs:343-370` is already production. Promoting it from "defence
in depth" to "the official path" means closing the SMR follow-up
tasks (SMR-3a/3b/3c) with "no change - keep the disable", documenting
the decision, and removing the bench-conditional ambiguity from the
SMR catalogue.

**NVMe perf retained.** 0% of the SQPOLL-on baseline by construction.
The ~10-15% NVMe throughput loss documented in
`project_sqpoll_disabled_with_mmap.md` is the accepted cost. Note
that today *no production caller flips `sqpoll: true`* (per audit
`io_uring_sqpoll_mmap_pagefault.md` section D, all three presets ship
with `sqpoll: false`), so the perf loss is *latent*: the workaround
only matters once someone enables SQPOLL on the receiver preset,
which is precisely what SMR step 5 contemplates.

**Implementation complexity.** Zero net new code. The disable lives
at `config.rs:343-370`; the `mmap_basis_active` flag lives at
`io_uring_common.rs:110-118`; the per-file SMR-3c adaptive layer
lives behind a Cargo feature in `adaptive_dispatch.rs`. All three
ship today.

**Kernel-version coverage.** Whatever
`IORING_SETUP_SQPOLL` supports is covered. The defensive disable is
unconditional, so kernel-version gating is moot.

**Failure modes.**

- *Latent perf loss.* SQPOLL never lit when mmap basis is in play;
  the ~10-15% NVMe loss persists. No correctness failure.
- *No new failure modes.* The path is already in production.

**Test surface.** Covered today by
`build_ring_sqpoll_with_mmap_basis_disables_sqpoll` and
`build_ring_no_sqpoll_with_mmap_basis_no_warning`
(`crates/fast_io/src/io_uring/config.rs:779-810`) plus the SMR-3c
unit tests in `adaptive_dispatch.rs`. `repro_sqpoll_mmap.rs`
asserts the gate fires under the hazardous combination.

**Rollback story.** None needed; this is the baseline. The SMR-3c
adaptive layer can be turned off either via Cargo feature
(default-off) or runtime env var (`OC_RSYNC_ADAPTIVE_BASIS_DISPATCH=0`).

## Recommendation

**Recommend candidate 2 (`mlock` the basis window) for SQM-2.b
formalisation, with candidate 3 retained as the
ring-construction-time guard and unconditional fallback.**

Reasoning, against the scoring axes:

1. **NVMe perf retention is the only axis that distinguishes
   candidate 2 from candidate 3.** Both close the hazard
   correctly; both have a clean downgrade path; both have
   deterministic tests. The choice between them is "do we want the
   10-15% NVMe throughput back, or accept the latent loss?" The
   `project_sqpoll_disabled_with_mmap.md` entry explicitly flags the
   loss as a regression worth closing; that is the trigger.

2. **Candidate 1 (MADV_WILLNEED) is not a workaround, it is an
   optimisation of the existing hazardous path.** The race is not
   closed by a hint that the kernel may ignore under pressure; a
   negative test result is indistinguishable from luck. Candidate 1
   is reasonable as a *secondary* tuning layer once candidate 2 is
   in place (wire first, hint second on the wired pages to nudge
   sequential readahead), but it is not a load-bearing fix.

3. **Candidate 3 alone leaves throughput on the table.** Acceptable
   if and only if SMR-3a hardware numbers show the NVMe loss is
   smaller than the recorded 10-15%. Without that data, candidate
   2 is the conservative move that recovers the most perf with the
   lowest residual risk.

4. **The dispatch surface is already in place.** SMR-3c's
   `adaptive_dispatch::pick` returns `BasisReadBackend::IoUring` for
   the large-file path today; the only addition needed for candidate
   2 is to wrap the SQE submission window with the wire/unwire
   guard and clear the `mmap_basis_active` flag once `mlock` has
   pinned the working set. Candidate 3 then composes as the
   *fallback* when `mlock` returns `EAGAIN` / `EPERM` /
   `ENOMEM`: the existing `SQPOLL_FALLBACK` path lights up exactly
   as it does today.

5. **Truncation race stays the load-bearing invariant.** Neither
   candidate closes the `truncate(2)` -> kernel-context `SIGBUS`
   failure mode; only `BufferedMap` (the existing mitigation 4)
   does. Candidate 2 must compose with mitigation 4: `mlock` the
   basis window *when* the basis is mmap'd, not as a justification
   to undo the buffered-basis policy for io_uring writers.

## Is the scoring inconclusive?

No. Candidate 2 strictly dominates candidate 1 on the load-bearing
axis (NVMe perf retained without a hidden-race regression) and
strictly dominates candidate 3 on the only axis where they differ
(NVMe perf retained vs ~10-15% loss). Candidate 1 vs candidate 3 is
also unambiguous: candidate 1 has worse failure-mode visibility than
candidate 3 (silent re-open of the race vs documented
disable) and only partially recovers perf, so it is not preferable
to either of the two non-recommendations.

A tie-breaker bench is therefore not required for SQM-2.b. If
SMR-3a hardware numbers later show the NVMe loss from candidate 3
is < 5% on the target host, the recommendation downgrades to
candidate 3; that re-evaluation is SMR-3b's job, not SQM-2.a's.

## What SQM-2.b must specify

The follow-up spec needs to nail down, at minimum:

1. **Wire/unwire granularity.** Per-SQE window vs per-batch vs
   per-file. The cost model in this doc assumes per-batch
   (SQE-window-sized) to keep the pinned set bounded; SQM-2.b
   should confirm against the SMR-1 bench harness.
2. **`mlock` vs `mlock2(MLOCK_ONFAULT)` selection.** Runtime probe
   via `kernel_version::is_at_least(4, 4)` from
   `crates/fast_io/src/kernel_version.rs`; fall back to plain
   `mlock` on older kernels.
3. **RLIMIT_MEMLOCK probe and operator guidance.** Document the
   `/etc/security/limits.conf` change required for non-root SQPOLL,
   or auto-downgrade silently with a `debug_log!` entry.
4. **Composition with `mmap_basis_active`.** Define the moment the
   flag flips: stays `true` until `mlock` succeeds, drops to
   `false` for the wired window's lifetime, reasserts on
   `munlock`. The `build_ring` check at
   `config.rs:343-370` is then accurate for both the wired and
   unwired cases.
5. **Test plan for `repro_sqpoll_mmap.rs`.** Add `mincore`-based
   residency assertion across the SQPOLL submit window, plus a
   negative case under `prlimit --memlock=64K` that asserts the
   downgrade-to-regular-ring path fires without an error to the
   transfer.

## Non-goals

- This doc does not change source. No code, no Cargo feature, no
  config knob is touched.
- This doc does not decide between `mlock` and `mlock2`; that is
  SQM-2.b.
- This doc does not commit to enabling SQPOLL on the receiver
  preset. That remains SMR step 5, gated on candidate 2 landing.
- This doc does not change the SMR-3c per-file dispatch
  recommendation. Candidate 2 augments the dispatch by making the
  io_uring arm safe to take *with mmap basis present*; SMR-3c still
  governs the per-file mmap-vs-io_uring choice.

## References

- `crates/fast_io/src/io_uring/config.rs:325-373` - defensive disable
  site, the hook for candidate 2's compose-with-fallback story.
- `crates/fast_io/src/adaptive_dispatch.rs` - SMR-3c per-file
  dispatch, the seam candidate 2 plugs into.
- `crates/fast_io/src/mmap_reader.rs:139-143` -
  `advise_willneed`, candidate 1's existing hook.
- `crates/fast_io/tests/repro_sqpoll_mmap.rs` - SQM-1.b reproducer
  and the test seam for candidate 2's residency assertion.
- `docs/design/mmap-vs-sqpoll-decision.md` - SMR-2 decision
  framework that this doc layers under for the "which workaround"
  axis.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - SMR resolution
  catalogue (Options 1/2/3 for basis-read dispatch, distinct from
  the SQM workaround scoring here).
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form audit
  of the page-fault hazard.
- `docs/audits/madvise-willneed-prefault.md` - candidate 1's
  underlying audit; explicitly treats `MADV_WILLNEED` as
  best-effort.
- `docs/design/basis-file-io-policy.md` - mitigation 4, the
  load-bearing invariant that closes the truncate-`SIGBUS` failure
  mode regardless of which SQM candidate ships.
