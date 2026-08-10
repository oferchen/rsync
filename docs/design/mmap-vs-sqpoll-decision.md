# mmap vs SQPOLL: decision framework for basis-file reads

Tracking issue: oc-rsync task #2287 (SMR-2).

Companion documents already in tree:

- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - the resolution-strategy
  doc that enumerates Options 1, 2, and 3. This document layers a decision
  framework on top.
- `docs/design/basis-file-io-policy.md` (#1666) - selector rule that forbids
  `MmapStrategy` whenever an io_uring writer is active on the same plan.
- `docs/audits/iouring-sqpoll-bench-plan.md` (#1626) - SQPOLL bench plan
  predating the dedicated mmap-vs-READ_FIXED scaffold.
- `docs/audits/io-uring-sqpoll-mmap-interaction.md` - re-verification of the
  SQPOLL + mmap hazard.
- `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs` - the criterion bench
  scaffold added by SMR-1 (PR #4387). Six cells covering combinations of
  basis-file size, chunk size, and submission mode. Harness only; no
  real-hardware numbers yet.

This document does not change any wired dispatch. It exists to make the
decision criteria explicit so the choice between Options 1, 2, and 3
becomes mechanical once SMR-1 hardware numbers land.

## Scope and intent

SMR-1 (PR #4387) landed the bench harness. SMR-2 (this doc) records the
decision framework. SMR-3a/3b/3c (follow-ups, see "Follow-up tasks"
below) implement whichever option the bench data selects.

The framework's job is to keep the decision honest. Without hardware
numbers, the only intellectually defensible choice is the **most
conservative** option - one that does not commit to a deletion or a new
adaptive code path before the data is in. That is Option 2 (size-threshold)
as a provisional default, because it preserves today's mmap-for-small,
buffered-for-hazardous behaviour and only requires picking one tunable
constant.

## The three options, one-line trade-off each

Restated from `docs/design/mmap-vs-sqpoll-conflict-resolution.md` for
self-containment. Full pros/cons live in that document.

| Option | One-line trade-off |
|---|---|
| 1: Drop mmap, use `READ_FIXED` + SQPOLL for all basis reads | Smallest dispatch surface, but pays full kernel-async cost on tiny basis files where mmap's page-cache win is largest. |
| 2: Size-threshold heuristic | Preserves mmap below the threshold and `READ_FIXED`+SQPOLL above it; one magic number to defend, no adaptive state. |
| 3: Per-file dispatch with rolling throughput feedback | Best long-run throughput on heterogeneous hosts, but adds per-file state, non-deterministic test surface, and only amortises on long transfers. |

## Decision matrix keyed off measurable inputs

The matrix is keyed off four inputs that every host exposes through `stat`,
`uname`, `/proc/meminfo`, and `/proc/mounts`. Each row is a workload
fingerprint; the recommended option assumes SMR-1 hardware numbers are
available for the matching cell.

Inputs:

- `avg_basis_size`: arithmetic mean basis-file size over the transfer plan.
  Computed from the file-list pass in `crates/protocol/src/flist/`.
- `storage_class`: NVMe / SATA SSD / spinning rust / network. Derived from
  the existing `fast_io::detect_network_fs` probe plus a one-shot
  `BLKROTATIONAL` check.
- `kernel_version`: `uname -r` major.minor. Affects SQPOLL behaviour
  materially at the 5.13 (`IORING_FEAT_SQPOLL_NONFIXED`), 5.18 (better
  `task_work` batching), and 6.x (post-`io_sq_thread()` rework) boundaries.
- `page_cache_pressure`: `MemAvailable / MemTotal` from `/proc/meminfo`.
  Mmap wins shrink fast as available memory approaches zero.

Cells without bench data fall back to the **provisional default** column.

| `avg_basis_size` | `storage_class` | `kernel_version` | `page_cache_pressure` | Recommended option (with bench data) | Provisional default (no bench data) |
|---|---|---|---|---|---|
| < 1 MiB | any | any | any | none of the three apply | mmap is already disallowed below `MMAP_THRESHOLD` (`crates/transfer/src/map_file/mod.rs:55`). Selector returns `BufferedMap`. |
| 1 MiB - 64 KiB threshold band | NVMe / SATA SSD | >= 5.13 | low (< 30%) | Option 1 if bench `read_fixed` >= 90% of mmap throughput; else Option 2 | Option 2 with threshold = 64 KiB. Mmap stays in service below the band. |
| 1 MiB - 64 KiB threshold band | spinning rust | any | any | Option 2; spinning-rust seek penalty dwarfs SQPOLL win | Option 2 with threshold = 64 KiB. |
| 1 MiB - 64 KiB band | network FS (NFS, SMB, FUSE) | any | any | none of the three; mmap already forbidden by `basis-file-io-policy.md` | `BufferedMap` (selector rule). |
| 64 KiB - 1 GiB | NVMe / SATA SSD | >= 6.0 | low | Option 1 if bench shows SQPOLL win > 10% on this band; else Option 2 | Option 2 with threshold = 64 KiB; falls into the "above threshold" arm and dispatches to `READ_FIXED`+SQPOLL. |
| 64 KiB - 1 GiB | NVMe / SATA SSD | 5.13 - 5.17 | low | Option 2; SQPOLL `task_work` batching is poorer pre-5.18 and erodes the syscall-saving win | Option 2 with threshold = 64 KiB. |
| 64 KiB - 1 GiB | NVMe / SATA SSD | any | high (> 70%) | Option 1 if bench shows mmap regresses under page-cache pressure; else Option 2 | Option 2 with threshold = 64 KiB. |
| >= 1 GiB | NVMe | >= 6.0 | low | Option 1; SQPOLL win is largest here per `iouring-sqpoll-bench-plan.md` | Option 2 with threshold = 64 KiB; same dispatch as 64 KiB - 1 GiB band. |
| >= 1 GiB | spinning rust | any | any | Option 2; submission savings are dominated by seek cost | Option 2 with threshold = 64 KiB. |
| any | any | < 5.13 | any | none of the three; SQPOLL gate trips on `IORING_FEAT_SQPOLL_NONFIXED` | Defensive disable in `crates/fast_io/src/io_uring/config.rs:343-370` already covers this; no change needed. |

Rule of thumb: **without SMR-1 hardware numbers, the rightmost column is
the recommendation, and it is uniformly Option 2 with a 64 KiB threshold**.
The bench data promotes individual cells to Option 1 or, in the long run
and only with telemetry evidence, Option 3.

## Provisional recommendation: Option 2 with a 64 KiB threshold

Option 2 is the conservative pick because:

1. **It does not delete anything.** Option 1 deletes the mmap dispatch
   path for basis reads. That is irreversible without re-auditing the
   `MmapStrategy` interactions documented in
   `docs/audits/mmap-iouring-co-usage.md`. A bench-driven later promotion
   to Option 1 still costs a deletion PR; a bench-driven later promotion
   to Option 3 costs an addition PR. The conservative move is to neither
   delete nor add until data justifies it.
2. **It preserves the existing fast path below the threshold.** The
   selector in `crates/transfer/src/map_file/adaptive.rs:36-54` already
   uses `MMAP_THRESHOLD = 1 MiB`. The 64 KiB threshold lives one
   abstraction higher: it gates the SQPOLL+`READ_FIXED` dispatch, not the
   buffered-vs-mmap selector. For files below 64 KiB the path is
   unchanged from today; for files above it, `READ_FIXED`+SQPOLL takes
   over.
3. **It composes with the existing defensive disable.** The
   `mmap_basis_active` gate at
   `crates/fast_io/src/io_uring/config.rs:343-370` is unaffected, because
   Option 2 above the threshold uses heap-backed registered buffers and
   never sets `mmap_basis_active = true`.
4. **The threshold is a single tunable constant.** It can be tweaked
   without a wire-protocol change, without a new SQE class, and without
   any new persisted state.

**Threshold choice: 64 KiB.** Rationale:

- 64 KiB matches the existing `chunk_size` used by `IoUringDiskBatch`
  in disk-commit (`crates/transfer/src/disk_commit/config.rs:46`), so a
  basis read above the threshold consumes exactly one SQE per chunk
  with no fragmentation.
- 64 KiB is the smallest size where SQPOLL's per-submit syscall saving
  exceeds the cost of one extra `READ_FIXED` completion on cold cache,
  per `docs/audits/iouring-sqpoll-bench-plan.md` section "SQPOLL
  trade-offs" (subject to confirmation on the SMR-1 hardware run).
- 64 KiB is a small enough threshold that the band of basis files where
  mmap is preferred (between `MMAP_THRESHOLD = 1 MiB` and the new
  64 KiB gate) is empty in practice, so the provisional default is
  observationally indistinguishable from today for the mmap-eligible
  population. This is intentional: the provisional default should not
  silently regress production transfers before the bench data is in.

The threshold is provisional in the strict sense: it must be re-tuned
once SMR-1 runs the bench scaffold on real hardware (see "Open questions"
below). The 64 KiB starting point is a defensible choice based on
existing audit work, not a measured optimum.

## What this doc can and cannot conclude

**Can conclude**:

- The shape of the decision (matrix keyed off four measurable inputs).
- The conservative default (Option 2, 64 KiB threshold) that does not
  commit to a deletion before data justifies it.
- The follow-up tasks that gate any promotion to Option 1 or Option 3,
  with explicit pass/fail criteria.

**Cannot conclude** without SMR-1 hardware runs:

- Whether Option 1 beats Option 2 on real NVMe + Linux 6.x. The bench
  scaffold exists but has not been executed.
- The optimal threshold for Option 2. 64 KiB is the starting point; the
  real number falls out of the per-chunk-size breakdown in the bench.
- Whether Option 3 ever justifies its additional code surface. That
  requires per-host throughput variance numbers from production
  telemetry, which oc-rsync does not currently emit.

## Open questions that SMR-1 hardware runs must answer

Each question maps to a specific cell or set of cells in the decision
matrix. The bench must produce data for every question before any cell
can be promoted off the provisional default.

1. **Does `READ_FIXED` + SQPOLL match mmap throughput on a 1 GiB
   basis-apply, cold cache, NVMe, kernel 6.x?** Drives the >= 1 GiB
   NVMe row. Pass criterion: `read_fixed` >= 90% of `mmap` throughput.
2. **Does the SQPOLL syscall-saving win exceed 10% on the 64 KiB - 1 GiB
   band, kernel 6.0+?** Drives the 64 KiB - 1 GiB NVMe row. Pass
   criterion: SQPOLL throughput - regular-ring throughput >= 10% on at
   least one chunk size in [16 KiB, 64 KiB, 256 KiB].
3. **Does mmap regress under page-cache pressure (`page_cache_pressure
   > 70%`)?** Drives the high-pressure row. Pass criterion: mmap
   throughput at `MemAvailable / MemTotal < 0.3` falls below
   `read_fixed` throughput on the same workload.
4. **Does the SQPOLL win evaporate on kernel 5.13 - 5.17?** Drives the
   kernel-band split. Pass criterion: SQPOLL throughput on kernel 5.17
   is within +/-5% of the regular-ring throughput on the same workload.
5. **Does spinning-rust I/O dominate the SQPOLL saving on any basis-file
   size?** Drives all spinning-rust rows. Pass criterion: SQPOLL vs
   regular-ring delta on `BLKROTATIONAL=1` storage is < 5% across all
   chunk sizes.

Until every open question has an answer, the provisional default applies
to every cell.

## Follow-up tasks

Three follow-up tasks gate the implementation work. Each has explicit
acceptance criteria; the task is done when every bullet is checked.

### SMR-3a: Run SMR-1 bench on representative hardware

**Goal**: produce the data table that promotes cells off the provisional
default.

**Acceptance criteria**:

- [ ] Bench `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs` runs to
      completion on Linux 6.x, NVMe, with
      `OC_RSYNC_BENCH_IOURING_RING=1` and
      `OC_RSYNC_BENCH_IOURING_SQPOLL=1`.
- [ ] Bench runs to completion on Linux 5.17 (or closest available) on
      the same hardware, with the same env gates.
- [ ] Bench runs on a spinning-rust block device (USB-attached HDD is
      acceptable for the sanity check) on either kernel.
- [ ] All six cells (basis-file size x chunk size x submission mode)
      report mean throughput and p99 latency.
- [ ] Results land in
      `docs/audits/mmap-vs-read-fixed-basis-bench-results.md` with the
      per-cell table.
- [ ] Five open questions above are answered yes/no.

### SMR-3b: Promote cells off provisional default per bench data

**Goal**: apply the decision matrix using SMR-3a data.

**Acceptance criteria**:

- [ ] Each decision-matrix cell has a definitive recommendation (Option
      1, 2, or 3) recorded in
      `docs/audits/mmap-vs-read-fixed-basis-bench-results.md`.
- [ ] If any cell selects Option 1, this doc is updated to record the
      promotion and the threshold in
      `mmap-vs-sqpoll-conflict-resolution.md` "Implementation plan
      (option 1)" steps 3-5 become unblocked.
- [ ] If every cell selects Option 2, this doc's recommendation moves
      from "provisional" to "confirmed", and the 64 KiB threshold is
      either confirmed or replaced with the bench-derived optimum.
- [ ] No cell selects Option 3 without telemetry evidence; if Option 3
      is recommended, defer to SMR-3c.

### SMR-3c: Implement chosen option in `DeltaApplicator`

**Goal**: wire the chosen dispatch into the live code path.

**Acceptance criteria**:

- [ ] `crates/transfer/src/delta_apply/applicator.rs:161-176` selector
      grows the third arm specified by SMR-3b.
- [ ] New strategy lives alongside `BufferedMap` and `MmapStrategy` in
      `crates/transfer/src/map_file/`, implements `MapStrategy::map_ptr`
      via `submit_read_fixed_batch` when `SQPOLL` is requested above the
      threshold.
- [ ] `mmap_basis_active` flag semantics are revisited per
      `mmap-vs-sqpoll-conflict-resolution.md` step 4.
- [ ] Existing tests in `crates/transfer/src/map_file/tests.rs` pass
      unchanged.
- [ ] New tests cover the threshold boundary: one cell at threshold - 1,
      one at threshold, one at threshold + 1.
- [ ] `iouring_sqpoll_vs_regular` and `mmap_vs_read_fixed_basis` benches
      both pass on the SMR-3a reference host with no regression > 5%.
- [ ] The defensive disable at
      `crates/fast_io/src/io_uring/config.rs:343-370` is preserved or
      explicitly downgraded with a documented rationale in
      `mmap-vs-sqpoll-conflict-resolution.md`.

## Non-goals

- This document does not pick Option 1 or Option 3. The honest answer
  without hardware data is "Option 2 provisionally"; promotions are
  SMR-3b's job.
- This document does not change the wired dispatch. It is pure
  documentation; the SMR-3c work is a separate PR.
- This document does not change the defensive disable at
  `crates/fast_io/src/io_uring/config.rs:343-370`. That gate stays in
  place regardless of which option ships, because it covers callers
  outside the basis-read path (e.g. the parallel checksum digest's
  `MmapReader` consumers).
- This document does not propose a wire-protocol change. Basis-read
  dispatch is local to the receiver.
