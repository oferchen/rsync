# Resolving the mmap vs SQPOLL conflict on basis-file reads

Tracking issue: oc-rsync follow-up to #2158.

Companion documents already in tree:

- `docs/design/basis-file-io-policy.md` (#1666) - selector rule that forbids
  `MmapStrategy` whenever an io_uring writer is active on the same plan.
- `docs/audits/io-uring-sqpoll-mmap-interaction.md` - re-verification of the
  SQPOLL + mmap hazard that motivated #2158.
- `docs/audits/mmap-page-fault-iouring-sqpoll.md` (#1661) - original kernel
  page-fault analysis.
- `docs/audits/iouring-sqpoll-bench-plan.md` (#1626) - SQPOLL bench plan
  predating #2158.
- `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs` - existing SQPOLL
  bench scaffold (small-file write workload).

This document does not change any wired dispatch. The implementation is
deferred until the bench scaffold added in this PR
(`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`) produces data.

## Current state: the defensive disable site

`IoUringConfig::build_ring` refuses `setup_sqpoll(..)` whenever the caller
has flagged `mmap_basis_active: true` on the same transfer plan:

- `crates/fast_io/src/io_uring/config.rs:343-370` - the gate. The relevant
  predicate is `let sqpoll_safe = sqpoll_requested && !self.mmap_basis_active;`
  on line 345. When SQPOLL was requested but the mmap-basis flag is set,
  the constructor emits a `debug_log!(Io, 1, ..)` warning, sets the global
  `SQPOLL_FALLBACK` flag, and falls through to a plain (non-SQPOLL) ring
  via `RawIoUring::new(self.sq_entries)`.
- `crates/fast_io/src/io_uring_common.rs:110-118` - the `mmap_basis_active`
  field on `IoUringConfig` and its doc-string. Same field appears on the
  non-Linux stub for source-level parity.
- `crates/fast_io/src/io_uring/config.rs:779-810` - the unit tests
  `build_ring_sqpoll_with_mmap_basis_disables_sqpoll` and
  `build_ring_no_sqpoll_with_mmap_basis_no_warning` pin the behaviour.

The flag is set by callers that cannot prove the upstream selector
(`AdaptiveMapStrategy::open_buffered` in
`crates/transfer/src/map_file/adaptive.rs:70`) successfully forced
`BufferedMap` for the basis file on this transfer plan. The flag is the
last line of defence; the primary defence is the basis-file selector in
`crates/transfer/src/delta_apply/applicator.rs:161-176`, which downgrades
to `BufferedMap` whenever the writer is io_uring-backed.

## Why the disable is correct

Pairing `IORING_SETUP_SQPOLL` with a file-backed `mmap(2)` region is a
documented kernel hazard. The SQPOLL kernel thread (`kthread`) services
SQEs from a different `task_struct` than the userspace caller. It does
not own the caller's `mm` (memory descriptor), so two failure modes
appear when an SQE references a userspace address that lives in a
file-backed VMA:

1. **Cold-page faults bounce to `task_work`.** When the SQPOLL kthread
   dereferences an mmap'd address whose PTE is not yet populated, the
   fault handler cannot resolve it inside the kthread because the kthread
   has no `mm`. The fault is queued onto `task_work` of the originating
   user task, which only runs on the user task's return-to-userspace
   boundary. On pre-6.x kernels this is a deadlock loop when the user
   task is itself blocked waiting on the SQPOLL ring. See `io_uring(7)`
   under `IORING_SETUP_SQPOLL`, kernel commit `b3a87e5b16cb` ("io_uring:
   use poll for fixed buffers when SQPOLL is set"), and the long-form
   analysis in `docs/audits/io_uring_sqpoll_mmap_pagefault.md`.

2. **Concurrent truncation surfaces as in-kernel `SIGBUS`.** A third
   party `truncate(2)` shrinks the backing file beneath the SQPOLL
   kthread's iovec. The page-fault path delivers `SIGBUS` to the
   originating task, which the user task did not expect because it
   never made the offending syscall. Upstream rsync sidesteps this
   class of bug by reading basis files via `read(2)` rather than
   `mmap(2)` (`fileio.c:214-217`); see
   `docs/design/basis-file-io-policy.md` section "Correctness against
   concurrent truncation" for the upstream-fidelity reasoning.

The defensive disable in `config.rs:343-370` is therefore correct: it
removes the dangerous combination at ring-construction time, before any
SQE is built. It is a configuration-time guard, never a runtime failure,
and it surfaces the downgrade via `SQPOLL_FALLBACK` so callers can
report it in diagnostics.

## The design tension

The defensive disable closes the hazard but creates a performance
inversion that the bench scaffold added in this PR is designed to
quantify:

- **Large files benefit most from SQPOLL.** SQPOLL eliminates the
  per-batch `io_uring_enter(2)` syscall. The win scales with submission
  rate, and submission rate scales with file size at fixed chunk size.
  A 1 GiB basis file delta-applied in 64 KiB chunks issues ~16 k reads;
  removing the syscall on every batch matters proportionally.
- **Large files are most likely mmap'd.** `AdaptiveMapStrategy::open`
  selects `MmapStrategy` for files at or above `MMAP_THRESHOLD` (1 MiB),
  precisely because mmap pays for itself only when amortised over a
  large region (`crates/transfer/src/map_file/adaptive.rs:36-54`).
- **Result.** Exactly the workloads that would benefit most from SQPOLL
  are the workloads where the defensive disable trips. Today both
  strategies are unconditionally bypassed for io_uring-backed writers:
  the `DeltaApplicator` forces `BufferedMap` (no mmap), and SQPOLL
  is off in every preset, so the inversion is latent. The moment a
  caller turns SQPOLL on for a transfer with a large basis file, the
  defensive disable activates and we lose the SQPOLL throughput win.

## Resolution strategies

Three resolutions are viable. Each removes the inversion by removing
either the mmap dependency, the SQPOLL request, or both at the right
granularity.

### Option 1: Drop mmap for SQPOLL-enabled basis reads, use READ_FIXED

Replace `MmapStrategy` for the basis file with
`IORING_OP_READ_FIXED` against the existing `RegisteredBufferGroup`
(`crates/fast_io/src/io_uring/registered_buffers/submit.rs:29-100`).
Registered buffers are heap-backed pages pinned by the kernel via
`get_user_pages_fast`; they are not file-backed VMAs, so the SQPOLL
kthread can read into them without triggering the page-fault hazard
above. The basis-file read becomes a normal kernel-async read into a
pre-registered destination buffer, indistinguishable in shape from the
delta-apply write path that already runs under io_uring.

**Pros.**

- Removes the race by removing the mmap. SQPOLL becomes free to enable
  on the same plan.
- Uses an existing, hardened code path (`submit_read_fixed_batch`).
  No new SQE class, no new kernel surface, no new audit material.
- Aligns the read and write paths on the same dispatch shape, so
  `IoUringDiskBatch`-style batching can amortise across both.

**Cons.**

- Removes the userspace caching that mmap gives non-SQPOLL paths if we
  go all-in. The parallel-checksum digest path
  (`crates/checksums/src/parallel/files.rs:42, 237, 340`) currently
  benefits from page-cache sharing across digest workers via mmap. A
  blanket switch would force those readers onto explicit I/O.
- The basis read becomes a kernel-async submission with SQE depth equal
  to `MMAP_THRESHOLD / chunk_size`. For 1 MiB and 64 KiB that is 16
  SQEs per window slide, which is fine, but tail latency on small
  windows is now bounded by completion latency rather than page-cache
  hit time.

### Option 2: Size-threshold heuristic

Keep `MmapStrategy` below a tunable size threshold `N` and force the
buffered (`BufferedMap`) or `READ_FIXED` path above. The threshold
captures the practical observation that mmap's setup cost
amortises poorly on multi-GiB files, and that the SQPOLL benefit
dominates at multi-GiB scale.

**Pros.**

- Pragmatic and tunable per host / per workload via the existing
  `IoUringConfig` knobs.
- Preserves mmap's win on the "small but eligible" basis files
  (1 MiB to N MiB) where neither SQPOLL nor `READ_FIXED` are a clear
  win.
- Composes cleanly with the existing `AdaptiveMapStrategy::open_with_threshold`
  entry point (`crates/transfer/src/map_file/adaptive.rs:45`).

**Cons.**

- The threshold is a magic number; it will drift as kernel versions
  and storage stacks change.
- Still relies on the operator to opt in to SQPOLL; if they do not,
  the threshold buys nothing.

### Option 3: Per-file dispatch on observed throughput

Decide mmap vs SQPOLL+READ_FIXED per basis file based on size at open
time plus rolling per-host throughput statistics. New basis files start
on the size-threshold heuristic from option 2 and migrate toward the
faster path as the running average accumulates evidence.

**Pros.**

- Captures genuine per-host variance (NVMe vs spinning rust, ext4 vs
  XFS, kernel 5.6 vs 6.10).
- Best long-term throughput once the statistics warm up.

**Cons.**

- More logic, more state to persist across transfers, more code paths
  to audit. Per-file dispatch interacts non-trivially with the per-plan
  `mmap_basis_active` flag.
- Hard to test deterministically; the rolling statistics path is
  inherently non-reproducible.
- Cost only pays off on long-running transfers with many basis files.
  CLI single-shot invocations rarely accumulate enough samples to
  beat the static heuristic.

## Recommendation

**Adopt option 1 if and only if the bench scaffold added in this PR
shows `IORING_OP_READ_FIXED` matches or exceeds `mmap` throughput on
the 1 GiB random-read workload. Otherwise adopt option 2 with the
threshold set at the smallest basis-file size where the bench shows a
SQPOLL win exceeding 10%.**

Rationale: option 1 is the minimal-surface fix. It deletes a dispatch
mode rather than adding one, and it uses code paths that are already
audited and tested. The only thing standing in its way is whether
`READ_FIXED` can keep up with mmap on the read-heavy basis-apply
pattern; that is exactly what the bench measures.

Option 3 is explicitly out of scope until option 1 or 2 has shipped and
real-world telemetry shows the static heuristic leaves measurable
throughput on the table.

## Trigger conditions

Bench results from `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`
gate the decision:

| Bench result | Action |
|---|---|
| `read_fixed_basis_with_sqpoll` >= `mmap_basis_read` throughput | Adopt option 1. Implement per the plan below with `target = ReadFixed`. |
| `read_fixed_basis_with_sqpoll` within -10% of `mmap_basis_read` | Adopt option 1. The SQPOLL syscall savings compensate the small per-read regression on long transfers; revisit if telemetry shows otherwise. |
| `read_fixed_basis_with_sqpoll` regresses by >10% vs `mmap_basis_read` | Adopt option 2. Threshold-tune from the bench's per-chunk-size breakdown. |
| Bench cannot run on the target host (kernel < 5.13, no `CAP_SYS_NICE`, etc.) | Hold the recommendation; keep the defensive disable. Re-run on a host that meets the prerequisites. |

## Implementation plan (option 1)

Each step is a separate PR for ease of review. Steps 2-5 are unblocked
once step 1 lands.

1. **Land this PR.** Adds the design doc and the bench scaffold. No
   code change to live dispatch. CI runs the bench in skip mode (env
   gates off) so the bench compiles on every PR but only executes when
   an operator opts in.

2. **Run the bench on a representative host.** Linux 6.x, NVMe, with
   `OC_RSYNC_BENCH_IOURING_RING=1` and
   `OC_RSYNC_BENCH_IOURING_SQPOLL=1` set. Record the per-chunk-size
   throughput table in a follow-up audit entry under
   `docs/audits/mmap-vs-read-fixed-basis-bench-results.md`. Pick the
   resolution path per the trigger table above.

3. **Wire the chosen path into `DeltaApplicator`.** Add a third arm
   to the selector at `crates/transfer/src/delta_apply/applicator.rs:161-176`
   that constructs a `RegisteredBufferGroup`-backed `MapStrategy`
   implementation when SQPOLL is requested and the basis file exceeds
   the threshold (option 1: threshold is "always"; option 2: threshold
   is the bench-derived number). The new strategy lives next to
   `BufferedMap` and `MmapStrategy` in `crates/transfer/src/map_file/`
   and implements `MapStrategy::map_ptr` by issuing one
   `submit_read_fixed_batch` per window slide.

4. **Flip `mmap_basis_active` semantics.** With option 1 wired, the
   flag stops being "we have an mmap on this plan, please disable
   SQPOLL" and becomes "we have a file-backed mmap somewhere on this
   plan, even non-basis (e.g. parallel checksum digest)". Re-audit the
   call sites that set the flag and downgrade it to a no-op for the
   basis-only path. Keep the gate in
   `config.rs:343-370` for callers that still pass file-backed mmap
   into the ring (the digest path; see audit
   `docs/audits/io-uring-sqpoll-mmap-interaction.md` row "MmapReader
   consumers - parallel checksum digest").

5. **Enable SQPOLL on the io_uring receiver preset by default.** Flip
   `sqpoll: true` on the receiver-side preset constructed in
   `crates/transfer/src/disk_commit/config.rs` once steps 3 and 4 ship.
   Validate with the existing
   `iouring_sqpoll_vs_regular` bench plus the new
   `mmap_vs_read_fixed_basis` bench on the same host. Roll back if
   either regresses; the per-plan opt-in stays available via
   `IoUringConfig::sqpoll`.

## Non-goals

- This document does not propose a wire-protocol change. The
  basis-read dispatch is entirely a local concern.
- This document does not propose changes to the non-basis mmap
  consumers (parallel checksum digest, `BufferRing` provided-buffer
  region, `IORING_OFF_PBUF_RING` mapping). Those remain governed by
  `docs/audits/io-uring-sqpoll-mmap-interaction.md`.
- This document does not propose dropping the defensive disable at
  `config.rs:343-370`. The disable remains the load-bearing guard for
  any future caller that combines SQPOLL with a file-backed mmap
  outside the basis-read path.

## Addendum: Option 3 prototype behind a feature flag (SMR-3c, #2290)

Option 3 (per-file dispatch on observed throughput) is now available
behind the `adaptive-basis-dispatch` Cargo feature on the `fast_io`
crate. The feature is **not** in the default set; default builds remain
byte-identical to today.

When compiled in, `fast_io::adaptive_dispatch` exposes:

- `ThroughputTracker` - maintains an exponentially-weighted moving
  average (`alpha = 0.2`) of bytes-per-second per backend
  (`BasisReadBackend::Mmap` vs `BasisReadBackend::IoUring`). EWMAs are
  stored in plain `AtomicU64`s so reads from the dispatch path do not
  serialise on a mutex; the only mutex is on the last-sample timestamp,
  taken only on the slow record path.
- `record_sample(backend, bytes, elapsed)` - callers wrap the chosen
  backend's read call and fold the result into the process-wide
  tracker.
- `pick(size, mmap_available, iouring_available)` - returns whichever
  backend has the higher EWMA when both have samples; otherwise falls
  back to the Option 2 size-threshold rule (`size_threshold_pick`).
  Setting `OC_RSYNC_ADAPTIVE_BASIS_DISPATCH=0` (or `off`, `false`,
  `no`) in the environment disables the adaptive path at runtime
  without rebuilding, reverting to the static threshold.

The adaptive path is intended for experimentation on hardware before
promotion. Measure with the SMR-1 bench harness
(`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`) on a
representative host. Compare adaptive-on vs adaptive-off on the same
workload; promote to default only if the EWMA-driven choice
demonstrably and consistently beats the static threshold across the
per-chunk-size breakdown.

This addendum does not change the recommendation in the body above
(adopt Option 1 if the bench permits, else Option 2). Option 3 remains
explicitly out of scope as the default until that recommendation has
shipped and real-world telemetry shows the static heuristic leaves
measurable throughput on the table.
