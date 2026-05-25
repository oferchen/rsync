# RP28.f - Client-mode rsync 2.6.9 interop outcome tracker

Status: Active tracker
Task: RP28.f.3 (#2968)
Parent: RP28.f (#2731)
Grandparent: RP28 (#2725)

## 1. Scope

RP28.f.3 documents the rolling outcome of the rsync 2.6.9 client-mode
interop harness, where rsync 2.6.9 runs as `--daemon --no-detach` and
oc-rsync drives the conversation as the client (both pull and push
directions). This document tracks per-fixture pass/fail status across CI
runs since the harness lands.

Topology recap (full detail in the RP28.f.1 spec): the legacy daemon is
the peer under test; oc-rsync is the system under test. The fixture
matrix exercises proto-28 DECODE / ENCODE paths on the client side -
flist, zlib, checksum negotiation fallback, delete-stats absence,
non-INC_RECURSE sender, name encoding, hardlinks, quick-check + delta,
rolling + strong checksum DECODE, capability-string back-negotiation,
and filter encoding.

Scope is restricted to harness-level outcomes. Wire-byte regression
work and capability-string edits live under RP28.g / RP28.h / RP28.i
and are out of scope here.

## 2. Harness reference

- Spec: `docs/design/rp28-f-1-client-2-6-9-daemon-harness.md` (RP28.f.1,
  PR #4929).
- Implementation: pending RP28.f.2. When that PR lands, this section
  must be updated to cite:
  - `scripts/rp28_f_1_setup.sh` (fixture generator)
  - `scripts/rp28_f_1_run.sh` (daemon orchestration + per-fixture
    runner)
  - The `rp28_f_client_2_6_9_interop` job step in
    `.github/workflows/_interop.yml` (CI integration)
- Cached daemon binary: `rsync-2.6.9` built via
  `scripts/build_rsync_2_6_9.sh` (RP28.b.1, PR #4903) and cached via
  RP28.b.3 (PR #4947).

Until RP28.f.2 lands, the only authoritative reference for fixture
content, pass/fail criteria, and stderr-allowlist semantics is the
RP28.f.1 spec.

## 3. Fixture matrix

All fixtures are defined in the RP28.f.1 spec, section 4. Initial
status for every row is `PENDING`; the first nightly run after RP28.f.2
lands populates `Last status` and `Last run date`.

| Fixture | Direction | What it exercises | Initial status | Last status | Last run date |
|---------|-----------|-------------------|----------------|-------------|---------------|
| F1: empty dir | pull + push | smoke test - daemon greeting, module list, empty flist | PENDING | PENDING | - |
| F2: 100 small files | pull + push | flist DECODE / ENCODE at proto 28 | PENDING | PENDING | - |
| F3: file with `-z` (client requests) | pull | zlib DECODE at proto 28 without cursor-advance assumption | PENDING | PENDING | - |
| F4: file with `--checksum` (client requests) | pull | MD5 fallback when daemon lacks checksum-negotiation | PENDING | PENDING | - |
| F5: file with `--delete` on local | pull + push | delete-stats absence handling at proto < 31 | PENDING | PENDING | - |
| F6: directory tree (3 levels) | push | non-INC_RECURSE sender path against legacy receiver | PENDING | PENDING | - |
| F7: file with extended chars in name | pull + push | name DECODE at proto 28 (no UTF-8 hint frame) | PENDING | PENDING | - |
| F8: hardlink group (2 files) | pull + push | hardlink wire DECODE at proto 28 | PENDING | PENDING | - |
| F9: incremental update | pull (twice) | quick-check + delta DECODE at proto 28 | PENDING | PENDING | - |
| F10: 1 MiB file with delta | pull | rolling + strong checksum DECODE at proto 28 | PENDING | PENDING | - |
| F11: oc-rsync sends `-e` capability string | push | verify back-negotiation to 28 in capability string | PENDING | PENDING | - |
| F12: `--exclude` / `--filter` | pull + push | filter encoding at proto 28 | PENDING | PENDING | - |

Fixture-direction count: 12 fixtures, 17 transfer runs total (pull+push
plus the F9 double-pull), matching the RP28.f.1 spec.

## 4. Update procedure

After every nightly Interop Validation workflow run:

1. Open the latest `Interop Validation` workflow run on GitHub Actions.
2. Locate the `Run rsync 2.6.9 client-mode daemon interop (RP28.f.2)`
   step in the `rp28_f_client_2_6_9_interop` job.
3. Read the per-fixture summary printed by `scripts/rp28_f_1_run.sh`.
4. For each fixture F1 through F12, update this document's matrix:
   - `Last status`: one of `PASS`, `FAIL`, `SKIP`.
   - `Last run date`: `YYYY-MM-DD` in UTC.
5. If a previously-passing fixture starts failing:
   - Open a new issue labelled `regression rp28`.
   - Title format: `RP28.f client-mode regression: F<N> failing`.
   - Body must link the failing workflow run URL and quote the relevant
     stderr lines.
   - Add an entry to section 5 (Resolution history).
6. If a previously-failing fixture starts passing:
   - Leave the prior failure context intact in section 5 (Resolution
     history).
   - Append a new section-5 entry recording the transition to PASS and
     citing the run URL that first saw the green status.
7. If the harness skips a fixture (for example, because the cached
   `rsync-2.6.9` binary is unavailable), record `SKIP` and add a
   section-5 entry explaining the skip cause. SKIP is not a substitute
   for FAIL when the daemon is reachable but the fixture itself does
   not pass.

Edits to this document are normal `docs:` PRs against master; no
out-of-band merge process applies.

## 5. Resolution history

Append-only log of fixture state transitions. Each entry uses the
following schema:

    ### YYYY-MM-DD: F<N> {PASS|FAIL} after {cause}

    - Before: <prior status and last run date>
    - Cause: <what changed in oc-rsync, the daemon binary, or the
      harness>
    - Action: <PR or commit reference, issue link if filed>
    - Outcome: <new status, workflow-run URL>

Entries are ordered most-recent first under each fixture; new entries
append to the top of the section.

(No entries yet. First entry will be added once RP28.f.2 lands and the
harness produces its first per-fixture result.)

## 6. Known divergences from upstream client behavior

(Initially empty. Populated as the harness surfaces protocol-28
DECODE-side divergences between oc-rsync's client path and upstream
rsync 2.6.9 client behavior.)

Note: oc-rsync as client may negotiate down to proto 28 differently
than upstream 2.6.9 client, since oc-rsync's capability-string handling
predates 2.6.9 and is governed by `build_capability_string`. Any
intentional deviation that the harness tolerates - for example, a
stderr keyword the runner's allowlist suppresses - is documented here
with a link to the originating code path and the rationale for the
deviation.

Entries use the schema:

    ### Divergence: <short title>

    - Surface: <fixture F<N>, stderr substring, or wire bytes>
    - Upstream behavior: <what 2.6.9 client does>
    - oc-rsync behavior: <what oc-rsync client does>
    - Rationale: <why the deviation is intentional, or link to the
      tracking issue if it is a bug not yet fixed>
    - Tolerance: <allowlist entry or fixture override that keeps the
      harness green>

## 7. Closure criteria for RP28.f parent (#2731)

RP28.f is DONE when all of the following hold simultaneously:

- All 12 fixtures show `PASS` for 5 consecutive nightly Interop
  Validation runs.
- No issue labelled `regression rp28` is open that references
  client-mode.
- This document (RP28.f.3) reflects the green state in section 3,
  including the most recent `Last status` and `Last run date` columns.

When the criteria are met, the closing PR must:

- Cite the 5 consecutive green run URLs in the RP28.f closure note.
- Move RP28.f.3 from `Status: Active tracker` to `Status: Closed`.
- Cross-link the RP28 parent (#2725) closure record under RP28.k.

## 8. Cross-references

- RP28.f.1 spec (PR #4929): `docs/design/rp28-f-1-client-2-6-9-daemon-harness.md`
- RP28.f.2 implementation: pending PR (cite PR# here when known); scope
  is `scripts/rp28_f_1_setup.sh`, `scripts/rp28_f_1_run.sh`, and the
  `rp28_f_client_2_6_9_interop` job in `.github/workflows/_interop.yml`.
- RP28.e.3 outcome tracker: sibling document for the daemon-mode
  direction (oc-rsync as daemon, rsync 2.6.9 as client). Same schema
  and update procedure; this document mirrors that pattern for the
  client-mode direction.
- RP28.b.1 build script (PR #4903): `scripts/build_rsync_2_6_9.sh`.
- RP28.b.2 smoke test (PR #4941).
- RP28.b.3 cache wiring (PR #4947).
- RP28.k.2 decision-execution record (PR #4942): feeds into the RP28
  parent closure tracked under #2725.
