# FFL-4: flat-flist default-on flip decision matrix

> **STATUS 2026-06-27 - RESOLVED: Option C REVERT.** The 1M-file in-memory bench
> measured flat at 1.255x WORSE RSS than legacy (95.8 vs 76.3 MiB; 48 B header,
> not the projected 24 B). The -63% premise is invalidated, so the flat path was
> removed and the legacy `Vec<FileEntry>` kept. Supersedes the "awaiting benches"
> status below.

Date: 2026-06-11
Scope: deciding whether to flip `flat-flist` to a default Cargo feature, hold dual-keep,
or revert the flat-side implementation entirely.
Status: RECOMMENDATION ISSUED, awaiting RSS-A.LAND.1/.2/.3/.4 bench numbers to execute.

## 1. Inputs

- FFL-1 audit (PR #5649, `docs/audits/ffl-1-dualfilelist-overhead.md`): inventories the
  `DualFileList` wrapper's runtime + compile-time cost, finds zero read-side overhead in
  the default build, substantial write-side amplification with `--features flat-flist`
  on, and a critical read-side validation gap - no production code path reads from the
  flat store today.
- RSS-A.LAND scope (tasks #3627-#3638): the flat-flist landing series gates the
  default-on flip on 1M-file RSS + throughput bench cells (RSS-A.LAND.1 baseline,
  RSS-A.LAND.2 flat-flist, RSS-A.LAND.3 throughput baseline, RSS-A.LAND.4 throughput
  flat-flist, RSS-A.LAND.5 decision). The flip task (RSS-A.LAND.6) and bake monitor
  (RSS-A.LAND.9) come after.
- FFL-FLIP scope (tasks #4008-#4013): the execution arm that follows RSS-A.LAND.5 -
  RSS profile delta (FFL-FLIP.1), throughput regression check (FFL-FLIP.2), execute the
  decision (FFL-FLIP.3), wire the default (FFL-FLIP.4), bake (FFL-FLIP.5), then begin
  the FFL-7..15 dual-path removal sweep (FFL-FLIP.6).
- Prior RSS-A audits at `docs/design/`:
  - `rss-a6-dual-emit-pattern.md` - dual emit design used by `DualFileList::push`.
  - `rss-a7-fileentry-read-sites.md` - read-site inventory the flat path was designed
    to absorb; not yet wired into the flat store.
  - `rss-a8a-inc-recurse-segment-audit.md` and `rss-a8b-arena-growth-strategy.md` -
    segment growth strategy whose `reclaim_segment` contract the flat path does not
    yet honor.
  - `rss-a11a-rayon-flat-flist-compat.md`, `rss-a-11b-parallel-flat-flist-builder.md` -
    parallel builder support already wired through the flat path.
  - `flat-flist-representation.md`, `flat-flist-rss-bench-fixture.md`,
    `flat-flist-rss-comparison.md`, `flat-flist-rss-measurement.md`,
    `flat-flist-throughput-baseline.md`, `flat-flist-throughput-post-migration.md` -
    design + measurement plan the RSS-A.LAND benches are meant to execute.
- Current dual-write state (from FFL-1 sec. 3 + 5): with `--features flat-flist` on,
  every `push` writes the legacy `Vec<FileEntry>` AND emits a `FileEntryHeader` plus
  cloned `FlatExtras` into a parallel `FlatFileList`. Read consumers still go to the
  legacy `Vec`; the flat store is build-only insurance. `reclaim_segment` reclaims
  only the legacy side, so INC_RECURSE segment savings degrade under
  `--features flat-flist`.

## 2. Options table

| Option | RSS impact | Dual-write cost | Removes legacy when? | Risk | Verdict |
|--------|------------|-----------------|----------------------|------|---------|
| A. Flip default-on now (RSS-A.LAND.6 / FFL-FLIP.4 immediately) | Unknown - flips production to a flat path that has no read traffic yet; if flat-side RSS regresses at 1M files the default build inherits it | Eliminated (single-write becomes flat side only); but reads still go to legacy unless FFL-7..10 follow within the same flip | After FFL-7..15 sweep; flip alone keeps DualFileList around | High. RSS-A.LAND.1/.2 bench numbers not captured; production users absorb the regression risk; PIP-7 corruption was the cautionary tale on flipping concurrent code paths without bench data per feedback_concurrent_path_discipline.md | Reject |
| B. Hold dual-keep until RSS-A.LAND benches land | Known: matches today's default (flat off); `--features flat-flist` matrix cell continues to absorb the dual-write penalty | Persists ONLY on the explicit opt-in build, which is the matrix-only cell; default build is unaffected | Blocked by RSS-A.LAND.5 decision then FFL-FLIP.3..6 | Low for default users (FFL-1 sec. 5: zero runtime overhead off-feature); moderate for the opt-in cell (continued double-store + arena allocation), but that cell already exists for exactly this purpose | Accept |
| C. Revert flat-flist feature entirely | Forfeits the 26x RSS amplification fix RSS-A series targets; reopens RSS-A.LAND scope without a path forward | Eliminated by removing the side that emits the second store | Immediately - delete `DualFileList`, FlatFileList, accessor `flat_impl`, the 21 cfg sites, and the RSS-A.5/.6/.7/.8/.11 implementation | Throws away shipped work; rss_arena_not_landed memory note already flags the prototype/production gap, but the RSS-A.5.a-f flat backing IS landed and used by the bench cell; reverting it abandons the only measured path toward the 26x RSS close | Reject |

## 3. Open evidence needed

The decision between Option A and Option B turns on bench numbers that are NOT yet
captured. RSS-A.LAND.5 cannot fire without them. The minimum data set:

- **RSS-A.LAND.1**: 1M-file flist build RSS, default features (flat-flist OFF). Control
  value. Must be a podman-container number per `feedback_use_container_for_linux_bench.md`
  so the result is reproducible across hosts.
- **RSS-A.LAND.2**: same workload, `--features flat-flist` ON, dual-write path
  exercised. Expected: HIGHER than .1 (FFL-1 sec. 3 predicts dual-write costs); the
  question is by how much.
- **RSS-A.LAND.3**: 1M-file end-to-end transfer throughput, default features. Control.
- **RSS-A.LAND.4**: same workload, `--features flat-flist` ON. Throughput should not
  regress materially even with dual-write (writes touch arenas + a second growable
  Vec; reads are unchanged), but the magnitude must be measured.
- A delta between (.2 - .1) under 10% RSS and (.4 - .3) under 5% throughput would
  argue Option A is safe to schedule. A delta above either bar would force Option B
  to persist while the dual-write path is removed (i.e., advance FFL-7..10 BEFORE
  flipping the default), since today's dual-write is the source of the regression
  rather than the flat path itself.

Two pieces of evidence beyond the bench cells:

- **Read-side validation gap (FFL-1 sec. 5)**: there is currently NO production
  code path that consumes `DualFileList::flat()`. Flipping the default before wiring
  reads through the flat path means production absorbs the write-side cost without
  exercising the read-side win. FFL-FLIP.4 is therefore not safe to schedule before
  FFL-7..10 land - or before a deliberate cutover task wires reads through
  FileEntryAccessor's flat impl in the same flip PR.
- **`reclaim_segment` contract (FFL-1 sec. 3)**: with dual-write on, the flat side
  retains every byte even after a segment is reclaimed, which breaks the
  `rss-a8b-arena-growth-strategy.md` invariant. RSS-A.LAND.2 must measure
  steady-state RSS across multiple INC_RECURSE segments (not just peak during a
  single build) or the bench will under-state the dual-write cost.

## 4. Recommended path

**Option B revised: hold dual-keep, make RSS-A.LAND.1/.2/.3/.4 priority-1.**

The FFL-1 audit's read-side validation gap is decisive. The wrapper's stated purpose
is to validate the flat store against the legacy store in production, but no
production read path consults the flat side today. Flipping the default-on without
bench coverage trades the FFL-1-confirmed write-side amplification for a flat path
that has never carried production read traffic. That is exactly the pattern
`feedback_concurrent_path_discipline.md` (PIP-7 corruption post-mortem) warns
against: flipping a parallel/alternate code path default-on before adversarial-ordering
and stress evidence exists.

Running the four bench cells is the cheapest insurance. They are scoped, the
fixture exists (RSS-A.9.a delivered the 1M-file flist fixture per task #3228; throughput
fixtures are in `docs/design/flat-flist-throughput-baseline.md`), and the
container-based execution model from `feedback_use_container_for_linux_bench.md`
removes host-dependence. Without those four numbers, no rational decision can be
made between Option A's promise and its risk.

Concretely, the recommended sequence:

1. Run RSS-A.LAND.1 and RSS-A.LAND.3 (baselines, default features) in the bench
   container. Capture peak RSS and throughput.
2. Run RSS-A.LAND.2 and RSS-A.LAND.4 (`--features flat-flist`) under the same
   harness. Capture the same metrics.
3. Compute deltas. If RSS delta is positive (flat path inflates RSS - expected
   from dual-write), document that the bench is measuring dual-write overhead,
   NOT the flat representation in isolation.
4. Decide:
   - If dual-write delta <= 10% RSS / 5% throughput: schedule FFL-FLIP.4 (default
     flip) only after FFL-7..10 land (read-side cutover). The flip + dual-removal
     should be one PR to avoid an intermediate state where reads still go legacy
     in default builds.
   - If dual-write delta is higher: keep `flat-flist` opt-in until FFL-7..10
     remove the dual-write cost; then re-bench (FFL-13 already on the schedule)
     and re-evaluate the default flip.

Option B revised is NOT "hold forever". The bench gate is the discriminator, and
the FFL-FLIP series stays the execution path. The recommendation is to NOT flip the
default until the bench numbers exist AND the read-side cutover ships in the same
PR as the flip.

## 5. Action items (immediate priority)

- **FFL-FLIP.1 (#4008)** - escalate to priority 1: profile RSS at 1M files comparing
  the 80-byte `FileEntry` legacy layout against the 24-byte `FileEntryHeader`
  flat layout under the same workload. Use the RSS-A.9.a fixture and the
  RSS-A.LAND container harness.
- **FFL-FLIP.2 (#4009)** - escalate to priority 1: throughput regression check at
  1M files with `--features flat-flist` against the default baseline.
- **RSS-A.LAND.1 (#3630)** and **RSS-A.LAND.3 (#3631)** - schedule alongside
  FFL-FLIP.1/.2 (baselines are shared).
- **RSS-A.LAND.2 (#3632)** and **RSS-A.LAND.4 (#3633)** - schedule alongside
  FFL-FLIP.1/.2 (measurement runs are shared).
- **RSS-A.LAND.5 (#3634)** - synthesize benches, gated by the four cells above.
- Defer **RSS-A.LAND.6 (#3635)** / **FFL-FLIP.4 (#4011)** until RSS-A.LAND.5 fires.
- Defer **FFL-7..15** until the flip decision lands.

## 6. Rollback criteria

For Option B revised (recommended) - no immediate rollback risk; the recommendation is
to NOT change the default. If a later FFL-FLIP.4 ships the default-on flip on the
basis of these benches, the rollback criteria for that flip are:

- Any production RSS regression on the 1M-file workload exceeds the bench-predicted
  ceiling by more than 1.25x. The bake monitor (FFL-FLIP.5 / RSS-A.LAND.9) catches
  this; rollback is a `feat:` revert of the default-feature toggle in
  `crates/protocol/Cargo.toml`.
- Any throughput regression on the 1M-file workload exceeds 5% vs the RSS-A.LAND.3
  control captured before the flip.
- Any new wire-byte divergence under interop tests (must remain
  byte-identical to legacy per RSS-A.6.g round-trip test).
- Any RSS regression on the INC_RECURSE multi-segment workload exceeds the
  RSS-A.LAND.2 ceiling, indicating `reclaim_segment` on the flat path is not
  honoring the RSS-A.8 contract.

For the hypothetical Option A flip (rejected) - if it were ever executed without
running the benches first, rollback criteria would be the same set above, but the
revert window would need to be measured in days (not the standard 14-day bake)
because the gating evidence never existed.

For Option C revert (rejected) - not applicable; rollback would mean re-landing
the RSS-A.5/.6/.7/.8/.11 implementation work, which the project memory notes
already flag as the only measured path to closing the 26x RSS gap.

## 7. Cited files

- `docs/audits/ffl-1-dualfilelist-overhead.md`
- `docs/design/flat-flist-representation.md`
- `docs/design/flat-flist-rss-bench-fixture.md`
- `docs/design/flat-flist-rss-comparison.md`
- `docs/design/flat-flist-rss-measurement.md`
- `docs/design/flat-flist-throughput-baseline.md`
- `docs/design/flat-flist-throughput-post-migration.md`
- `docs/design/rss-a6-dual-emit-pattern.md`
- `docs/design/rss-a7-fileentry-read-sites.md`
- `docs/design/rss-a8a-inc-recurse-segment-audit.md`
- `docs/design/rss-a8b-arena-growth-strategy.md`
- `docs/design/rss-a11a-rayon-flat-flist-compat.md`
- `docs/design/rss-a-11b-parallel-flat-flist-builder.md`
- `crates/protocol/src/flist/dual.rs`
- `crates/protocol/src/flist/accessor.rs`
- `crates/protocol/Cargo.toml`
