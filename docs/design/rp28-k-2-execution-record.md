# RP28.k.2 - Execution Record for Path A Decision

Execution / record-keeping document. No code changes. Captures the actions
taken in response to the RP28.k.1 decision, the closure criteria for the
parent RP28 series, and the criteria under which the keep-vs-drop choice
should be re-evaluated.

## 1. Scope

RP28.k.2 executes the Path A recommendation that RP28.k.1
(`docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md`, PR #4921) ratified
for the v0.7.x line. This record:

- Documents the concrete execution actions taken in this PR.
- Files the deferred Path B re-evaluation as a follow-up.
- Records the closure criteria for the parent #2725 RP28 series.
- States the conditions under which the keep-vs-drop choice should be revisited.

Path A is intentionally a no-code-change execution: the decision is to keep
wire-level back-negotiation to protocol 28 inclusive, complete the in-flight
validation tasks (RP28.b / RP28.c / RP28.e / RP28.f), and rely on the existing
wire-byte regression tests (RP28.g / RP28.h / RP28.i) as the safety net.

Memory link: [[project_protocol_compat]].

## 2. Decision recap

Path A (chosen): keep the existing protocol floor at 28 inclusive. oc-rsync
continues to back-negotiate to rsync wire protocol versions 28 through 32,
matching upstream rsync 2.6.x through 3.4.x. No code is removed, no
`legacy-proto-28` Cargo feature is introduced for the v0.7.x line, and the
39 protocol-version-gated branches inventoried in RP28.a stay in place.
Completion of RP28.b (build rsync 2.6.9 from source), RP28.c (push CI cell),
RP28.e (daemon mode against 2.6.9 client), and RP28.f (client mode against
2.6.9 daemon) is treated as the validation work that ratifies the kept
floor. Path B (gate protocol < 30 behind a Cargo feature, default off) is
deferred to v1.0 and re-evaluated against the criteria in section 5.

Rationale and the full Path A vs Path B comparison live in
`docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md` (PR #4921). This
record does not restate the matrix; it executes its outcome.

## 3. Actions taken in this PR

- Documented the Path A execution in this file
  (`docs/design/rp28-k-2-execution-record.md`).
- Augmented `.github/RELEASE_TEMPLATE.md` with a "Supported rsync protocol
  versions" scaffold announcing the Path A supported-protocols range
  (28 through 32 inclusive) so the next minor release of the v0.7.x line
  carries the messaging end-to-end.
- Filed a follow-up "Path B evaluation for v1.0" item in the task tracker
  (to be filed separately alongside the v1.0 milestone planning; not part
  of this PR). The Path B work item is gated on the re-evaluation criteria
  in section 5 firing, not on a calendar date.
- Did not touch protocol code, CI configuration, or wire-byte regression
  tests. Path A is intentionally a documentation-and-records-only change.

## 4. Closure criteria for parent RP28 (#2725)

The parent #2725 RP28 series closes when **all** of the following hold
green simultaneously:

- **RP28.b - rsync 2.6.9 build from source:** .b.1 (done) plus .b.2 (in
  flight) plus .b.3 (pending) all green; the 2.6.9 binary is available to
  the CI matrix without manual intervention.
- **RP28.c - push interop CI cell against rsync 2.6.9:** the cell is green
  for at least three consecutive nightly Interop runs (rolling window) with
  no flake-and-rerun gating.
- **RP28.d - pull interop CI cell against rsync 2.6.9:** done; remains
  green in the same rolling window.
- **RP28.e - daemon mode serving a 2.6.9 client:** .e.1 (done) plus .e.2
  and .e.3 (pending) complete and green.
- **RP28.f - client mode against a 2.6.9 daemon:** .f.1 (done) plus .f.2
  and .f.3 (pending) complete and green.
- **RP28.g / .h / .i wire-byte regression tests:** done; remain green in
  the same rolling window. Any regression here is a release blocker, not
  a closure blocker; closure assumes they continue to pass.
- **RP28.j.1 / .j.2 - README and man-page coverage:** done; the published
  supported-protocols matrix matches the wording in section 6 below.
- **RP28.k.1 - decision matrix:** done (PR #4921).
- **RP28.k.2 - decision execution:** this task; closes with this PR.

Once the above are green, #2725 is closed with a one-line summary
referencing this execution record and the RP28.k.1 decision doc. Path B
re-evaluation is tracked under its own follow-up task and is not a
prerequisite for closing #2725.

## 5. Path B re-evaluation criteria

Path B (drop protocol < 30 behind a Cargo feature, default off) is
revisited when **any** of the following triggers fires:

- **12-month usage telemetry window:** twelve months after the first v1.0
  release, aggregated telemetry / user-report data shows zero observed
  use of protocol versions below 30 in the field. Absence-of-evidence is
  the gate; positive evidence of pre-30 use suppresses the trigger.
- **Maintenance-burden incident threshold:** three or more regressions
  within any rolling six-month window are caught only by the RP28.g /
  RP28.h / RP28.i wire-byte regression tests (i.e. they would have shipped
  if the pre-30 branches were absent), indicating the maintained surface
  is producing recurring incidents rather than dormant code.
- **Upstream protocol floor move:** upstream rsync's `MIN_PROTOCOL_VERSION`
  constant in `compat.c` moves above 28. Considered unlikely on the
  current upstream release cadence, but tracked via the upstream-version
  audit done at each release-prep step. If upstream moves, oc-rsync
  follows.

A new design doc (`rp28-k-3-*` or equivalent) is opened when any trigger
fires; the doc reuses the RP28.k.1 matrix structure with refreshed evidence.

## 6. Release-notes scaffold

The following snippet is added to `.github/RELEASE_TEMPLATE.md` so the
next minor release of the v0.7.x line carries the Path A messaging:

> ### Supported rsync protocol versions
>
> oc-rsync continues to support protocol versions 28-32 inclusive, matching
> upstream rsync 2.6.x through 3.4.x. Protocol back-negotiation to 28 is
> exercised by wire-byte regression tests and a periodic CI matrix against
> rsync 2.6.9 (built from source). Protocols `<= 27` (rsync 2.5.x and
> earlier) remain unsupported. See
> `docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md` for the decision
> rationale.

The same snippet is mirrored verbatim in the release template so release
authors do not need to copy it from this design doc at release time. If
the template diverges from the snippet above, this design doc is the
source of truth and the template is updated to match.

## 7. Cross-references

- Decision matrix: `docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md`
  (PR #4921, RP28.k.1).
- Parent series: #2725 RP28.
- Sibling shipped tasks:
  - RP28.b.1: PR #4903 (rsync 2.6.9 source vendoring, first cut).
  - RP28.c.a: PR #4923 (push CI cell scaffolding against 2.6.9).
  - RP28.d: existing CI cell (pull interop against 2.6.9).
  - RP28.e.1: PR #4928 (daemon-mode 2.6.9 client, first cut).
  - RP28.f.1: PR #4929 (client-mode 2.6.9 daemon, first cut).
  - RP28.g / RP28.h / RP28.i: wire-byte regression tests (shipped).
  - RP28.j.1: PR #4897 (README supported-protocols matrix).
  - RP28.j.2: PR #4913 (man-page supported-protocols matrix).
- Memory note: [[project_protocol_compat]] - oc-rsync targets wire-equivalence
  with upstream rsync v3.4.1 (protocol 32); the kept 28-32 range is the
  back-negotiation window around that target.
