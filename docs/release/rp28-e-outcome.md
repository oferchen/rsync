# RP28.e Daemon-Mode rsync 2.6.9 Interop Outcome Tracker

## 1. Scope

This document records the rolling outcome of the rsync 2.6.9 daemon-mode interop
harness, in which `oc-rsync` runs as the daemon and rsync 2.6.9 acts as the
client. It tracks per-fixture pass/fail status across CI runs since the harness
landed in PR #4951, and serves as the durable evidence trail that the RP28.e
parent issue (#2730) uses to declare daemon-mode legacy-client parity.

Scope boundaries:

- Only the daemon-as-server direction is covered here. Client-mode runs against
  a 2.6.9 daemon are out of scope (handled by other RP28 sub-series).
- Only fixtures D1-D10 from the RP28.e.1 spec are tracked. New fixtures land
  via a follow-up sub-task and extend the matrix below.
- "PASS" means the harness step exits 0 and the per-fixture verifier asserts
  byte-equivalent destination trees. "FAIL" means a verifier or transfer
  failure. "SKIP" means the harness skipped the fixture (for example, an
  environmental precondition was not met).

## 2. Harness Reference

- Spec: [`docs/design/rp28-e-1-daemon-2-6-9-client-harness.md`](../design/rp28-e-1-daemon-2-6-9-client-harness.md)
  (shipped in PR #4928, RP28.e.1).
- Setup script: [`scripts/rp28_e_1_setup.sh`](../../scripts/rp28_e_1_setup.sh)
  (shipped in PR #4951, RP28.e.2). Prepares the temp daemon root, writes the
  `oc-rsyncd.conf` module definition, and stages all D1-D10 fixture sources.
- Run script: [`scripts/rp28_e_1_run.sh`](../../scripts/rp28_e_1_run.sh)
  (shipped in PR #4951, RP28.e.2). Starts the `oc-rsync` daemon, invokes the
  rsync 2.6.9 client for each fixture, captures per-fixture stdout/stderr, and
  diffs the result.
- CI integration: the `Run rsync 2.6.9 daemon-mode client interop (RP28.e.2)`
  step in [`.github/workflows/_interop.yml`](../../.github/workflows/_interop.yml).
  Runs on the nightly Interop Validation schedule.

## 3. Fixture Matrix

Initial status applies to the first run after PR #4951 lands. "Last status"
and "Last run date" are refreshed by the update procedure in section 4.

| Fixture | Direction  | What it exercises                                              | Initial status | Last status | Last run date |
|---------|------------|----------------------------------------------------------------|----------------|-------------|---------------|
| D1      | pull+push  | smoke (empty dir)                                              | PENDING        | -           | -             |
| D2      | pull+push  | 100 small files: flist encoding at proto 28                    | PENDING        | -           | -             |
| D3      | pull       | file with -z: zlib codec at proto 28                           | PENDING        | -           | -             |
| D4      | pull       | --checksum: negotiation absence at proto 28                    | PENDING        | -           | -             |
| D5      | pull+push  | --delete: NDX_DEL_STATS absence at proto < 31                  | PENDING        | -           | -             |
| D6      | push       | dir tree depth 3: non-INC_RECURSE flist                        | PENDING        | -           | -             |
| D7      | pull+push  | extended-char filenames: name encoding at proto 28             | PENDING        | -           | -             |
| D8      | pull+push  | hardlink group: hardlink wire encoding at proto 28             | PENDING        | -           | -             |
| D9      | pull twice | incremental update: quick-check + delta at proto 28            | PENDING        | -           | -             |
| D10     | pull       | 1 MiB delta: rolling+strong checksum at proto 28               | PENDING        | -           | -             |

## 4. Update Procedure

Refresh this document whenever the nightly Interop Validation workflow produces
a meaningful result for the RP28.e.2 step.

1. Open the latest run of `.github/workflows/_interop.yml` and locate the step
   labeled `Run rsync 2.6.9 daemon-mode client interop (RP28.e.2)`.
2. Read the per-fixture summary printed by `scripts/rp28_e_1_run.sh` (one line
   per fixture: `D<N>: PASS|FAIL|SKIP`).
3. For each fixture D1-D10, update "Last status" and "Last run date"
   (YYYY-MM-DD) in section 3.
4. If a previously-passing fixture starts failing:
   - Open a tracking issue with labels `regression rp28` and `interop`.
   - Link the failing workflow run URL and the offending commit/PR.
   - Add a Resolution history entry in section 5 once root cause is known.
5. If a previously-failing fixture starts passing:
   - Update the matrix row.
   - Leave the prior failure context in Resolution history (section 5);
     do not delete it.
6. If the harness is skipped for a transient reason (cache miss, network
   outage), record `SKIP` and the cause in the next Resolution history entry,
   but do not gate closure on a skipped run.

## 5. Resolution History

Append-only log of fixture state changes. Newest entry first. Entry template:

```
### YYYY-MM-DD: D<N> {PASS|FAIL} after {cause}
- Before: <prior state>
- Cause: <PR / commit / dependency change>
- Action: <fix PR or workaround>
- Outcome: <new state>
```

_No entries yet. The first nightly run after PR #4951 lands populates the
initial baseline; subsequent state changes append here._

## 6. Known Divergences from Upstream Behavior

This section catalogs intentional behavior differences between `oc-rsync` and
rsync 2.6.9 that the harness explicitly tolerates. Each entry documents why the
fixture asserts "no regression" rather than strict parity.

_No entries yet. Populated as the harness surfaces protocol-28 wire-byte or
behavior differences that are intentional (for example, a feature `oc-rsync`
supports that upstream 2.6.9 does not, where the fixture only asserts the
legacy client is not broken)._

## 7. Closure Criteria for RP28.e Parent (#2730)

RP28.e is considered DONE when all of the following hold simultaneously:

- All 10 fixtures (D1-D10) report PASS for 5 consecutive nightly Interop
  Validation runs.
- No open GitHub issues carry the `regression rp28` label and reference
  daemon-mode behavior.
- The fixture matrix in section 3 reflects the green state, with "Last run
  date" within the last 7 days.

Once closure is declared, update section 1 to note the closure date and PR.

## 8. Cross-References

- RP28.e.1 spec: PR #4928 (design doc for the harness).
- RP28.e.2 implementation: PR #4951 (scripts plus workflow integration).
- RP28.b.1 build script: PR #4903 (rsync 2.6.9 build helper).
- RP28.b.2 smoke: PR #4941 (build-helper smoke test).
- RP28.b.3 cache: PR #4947 (build artifact caching).
- RP28.k.2 decision-execution record: PR #4942 (parent RP28 series tracker;
  this outcome doc feeds its closure criteria).
- Parent issue: RP28.e (#2730).
- Tracking task: RP28.e.3 (#2965).
