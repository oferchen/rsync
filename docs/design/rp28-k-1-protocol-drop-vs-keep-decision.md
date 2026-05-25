# RP28.k.1 - Drop-vs-Keep Decision Matrix for Protocol < 30 Support

Design / decision document. No code changes. Synthesises the RP28.a..j evidence
into an explicit recommendation that RP28.k.2 will execute.

## 1. Scope

RP28.k.1 enumerates a single decision: should `oc-rsync` continue to support
rsync wire protocol versions 28 and 29 (rsync 2.6.x and rsync 3.0.x) once the
RP28 series is complete, or should the floor move to protocol 30+ and the older
peers be unsupported?

The decision affects:

- Whether RP28.b (build rsync 2.6.9 from source), RP28.c (CI cell for 2.6.9
  push interop), RP28.e (daemon mode against 2.6.9 client), and RP28.f (client
  mode against 2.6.9 daemon) are worth completing.
- Whether the protocol < 30 wire-byte regression tests (RP28.g, RP28.h, RP28.i)
  remain in-tree indefinitely or are removed.
- The phrasing in the supported-protocols matrix that RP28.j.1 (README) and
  RP28.j.2 (man page) already publish.
- The cost ceiling on every future protocol-touching PR, because each gated
  branch is a maintenance surface.

Memory link: [[project_protocol_compat]].

## 2. Evidence Summary

Synthesised from the completed RP28 sub-tasks.

### 2.1 Code-path inventory (RP28.a)

The inventory at `docs/design/rp28-a-pre30-code-paths-inventory.md` enumerates
**39 distinct behavioural branches** that gate on `protocol_version < 30`
(or `< 29`, or the `(28..30)` band). Severity breakdown:

| Severity | Count | Definition |
|----------|-------|------------|
| HIGH | 19 | Silent data loss or transfer corruption if mis-gated |
| MED  | 13 | User-visible error or skipped capability if mis-gated |
| LOW  | 7  | Cosmetic / doc-only references |
| **Total** | **39** | |

Raw `grep` counts (informational, include doc references and tests):

| Pattern | Hits |
|---------|------|
| `protocol_version < 30` | 16 |
| `protocol.as_u8() < 30` | 11 |
| `protocol.as_u8() >= 30` | 23 |
| `protocol_version >= 30` | 37 |
| `protocol_version >= 29` | 9 |
| `protocol_version < 29` / `as_u8() < 29` | 4 |
| `(28..30)` / `(28..=29)` | 1 |

The floor is encoded in `crates/protocol/src/version/constants.rs:7`
(`OLDEST_SUPPORTED_PROTOCOL = 28`). Upstream's own floor is
`MIN_PROTOCOL_VERSION = 20` at `target/interop/upstream-src/rsync-3.4.1/rsync.h:147`,
so our floor is already tighter than upstream's.

### 2.2 Wire-byte regression coverage already shipped

- **RP28.g** - `crates/protocol/tests/flist_wire_flags_rp28g.rs` -
  flist xflags bit-layout regression for protocols 28-29 (pre-30 bit reuse of
  `XMIT_RDEV_MINOR_8_PRE30` and `XMIT_SAME_DEV_PRE30`).
- **RP28.h** - `crates/protocol/tests/flist_sort_keys_rp28h.rs` -
  `flist/sort.rs` comparator regression for `t_PATH` (>=29) vs `t_ITEM`
  (pre-29).
- **RP28.i** - `crates/protocol/tests/zlib_codec_proto_lt_31.rs` -
  zlib codec chunk advance regression for protocol < 31 (which includes all
  pre-30 peers as a strict subset).

These three goldens are the durable in-tree evidence that the HIGH-severity
gates W9-W11, F1, and Z1 remain correct under maintenance churn. They cost
~zero CPU per CI run.

### 2.3 Supported-protocols matrix already published

- **RP28.j.1** - `README.md:123-149` publishes the per-protocol status table
  with protocol 28 listed as "Wire-level support" pending RP28 series
  completion, and `<= 27` listed as "Not supported".
- **RP28.j.2** - `docs/oc-rsync.1.md:46-67` mirrors the same matrix in the man
  page.

Both documents already commit publicly to the protocol-28-floor stance.
Lowering the floor (Drop case) is a user-visible regression on a published
support matrix.

### 2.4 Maintenance cost evidence

Per RP28.a's severity table, **every PR that touches** any of:

- `crates/protocol/src/codec/ndx/`
- `crates/protocol/src/codec/protocol/`
- `crates/protocol/src/wire/file_entry/`
- `crates/protocol/src/flist/{read,write,sort}.rs`
- `crates/protocol/src/varint/`
- `crates/transfer/src/setup/{mod.rs,restrictions.rs}`
- `crates/transfer/src/{generator,receiver}/file_list/`
- `crates/transfer/src/receiver/wire.rs`
- `crates/transfer/src/generator/{protocol_io.rs,item_flags.rs,transfer/}`
- `crates/checksums/src/strong/strategy/`
- `crates/signature/src/{block_size.rs,layout.rs}`
- `crates/batch/src/{format/,script.rs,replay/}`
- `crates/daemon/src/daemon/sections/`
- `crates/core/src/auth/`
- `crates/core/src/client/remote/daemon_transfer/orchestration/`

...must consider the pre-30 path. The cost is bounded but not zero. The
RP28.g/h/i goldens act as a safety net for the HIGH-severity subset.

### 2.5 Real-world deployment evidence

- rsync 2.6.x: 2004 - 2009. End of life, no upstream maintenance.
- rsync 3.0.x: 2008 - 2014. End of life, no upstream maintenance.
- rsync 3.1.x: 2014+. Ships in every mainstream distro at or above
  RHEL 7 / Debian 8 / Ubuntu 14.04 LTS. All advertise protocol 30 or higher.
- rsync 3.2.x / 3.3.x: 2020+. Ship in current LTS distros (RHEL 8/9,
  Debian 11/12, Ubuntu 20.04/22.04/24.04). All advertise protocol 31.
- rsync 3.4.x: 2024+. Latest stable. Advertises protocol 32.

Peers that *only* advertise protocol 28-29 are necessarily either rsync 2.6.x
or 3.0.x. Both are **outside upstream's release-notes scope** as of rsync
3.4.x; they only appear in the wild on:

- Frozen embedded firmware (router back-ends, NAS appliances, set-top boxes).
- Long-frozen package repositories.
- Air-gapped systems running ancient enterprise Linux derivatives.

There is no telemetry stream from oc-rsync, so the size of this user base is
unknown.

## 3. Drop Case

Arguments for **dropping** protocol 28-29 support and moving the floor to 30:

- **Surface reduction.** Removes 39 gated branches (RP28.a). 19 of those are
  HIGH-severity sites where a regression silently corrupts the wire stream.
- **CI simplification.** Removes the need for the RP28.b series (build rsync
  2.6.9 from source, including upstream patches to make it compile on modern
  toolchains, glibc, and OpenSSL). The 2.6.9 source build is non-trivial:
  rsync 2.6.x predates many modern build conventions, and CI must maintain
  patches to keep it building.
- **Closes a regression class.** Every pre-30 gate is a place where a future
  refactor could silently break compatibility. Removing the gates closes the
  class entirely.
- **Out-of-scope upstream.** rsync 2.6.x and 3.0.x are not in upstream's
  release-notes scope. Users on those versions are already running unsupported
  software; we are guarding against breakage that the upstream project has
  itself stopped guarding against.
- **Modern distros suffice.** Every mainstream distro at or above RHEL 7,
  Debian 8, or Ubuntu 14.04 LTS ships rsync 3.1.x or later. The pre-30 user
  base is structurally narrowing year-over-year and has no replenishment.

## 4. Keep Case

Arguments for **keeping** protocol 28-29 support:

- **Sunk-cost is shipped.** The wire-byte regression tests (RP28.g, RP28.h,
  RP28.i) are already in-tree. The only cost actually saved by dropping is
  their deletion - the maintenance debt is mostly already paid.
- **Legacy appliances are forever.** Router firmware, NAS appliances, set-top
  boxes, and frozen package repositories ship rsync 2.6.x indefinitely. The
  user count is unknown but non-zero, and these are typically deployments
  where the user *cannot* upgrade the peer.
- **Bidirectional interop matters.** Dropping back-negotiation breaks
  `oc-rsync` not just as a server against old clients, but also as a *client*
  against an old `rsync` server. Many users invoke `oc-rsync` to pull from
  legacy fileshares they do not control.
- **Bounded maintenance.** RP28.a enumerated a *finite* list of 39 gates.
  Once each is regression-tested, the marginal maintenance per PR is bounded.
  The RP28.g/h/i goldens already cover the HIGH-severity subset that matters.
- **Public commitment.** README.md:123-149 and `docs/oc-rsync.1.md:46-67`
  already publish the protocol-28 floor. Lowering it is a user-visible
  regression on a documented support matrix.
- **Differentiation.** Maintaining rsync 2.6.x interop is a feature; few
  modern alternatives can claim wire-level compatibility back that far.

## 5. Decision Matrix

Weighting scale: `-3` to `+3`. Positive = argues for that column.

| Consideration | Keep weight | Drop weight | Notes |
|---|---|---|---|
| Maintenance burden | -2 | +2 | RP28.a's 39 gates each demand attention on every protocol-touch PR; 19 HIGH-severity sites are silent-corruption risks. |
| Wire-byte test cost | -1 | +1 | RP28.g/h/i are already shipped; their CPU cost is negligible. Only the line-count is saved by deletion. |
| CI matrix cost | -2 | +2 | Building rsync 2.6.9 from source (RP28.b series) requires patches to compile on modern toolchains, glibc, OpenSSL. Non-trivial to keep green. |
| User-base coverage | +3 | -3 | Legacy embedded / appliance deployments cannot upgrade their rsync; dropping cuts them off entirely. |
| Upstream rsync stance | -1 | +1 | Upstream's release-notes scope effectively excludes 2.6.x and 3.0.x; we are guarding what upstream itself has stopped guarding. |
| Bidirectional client interop | +2 | -2 | Dropping back-negotiation breaks `oc-rsync` as a *client* against old `rsync` daemons / SSH peers in unmanaged environments. |
| Public commitment / docs | +2 | -2 | README + man page already publish the protocol-28 floor; lowering it is a user-visible documented-support regression. |
| Compatibility flag potential | +2 | 0 | Path B (cargo feature `legacy-proto-28`) lets binary-size-conscious distributions opt out without forcing the choice on everyone. |
| Sunk cost of RP28.a..j | +1 | -1 | The audit, wire goldens, and supported-protocols matrix have already been built; deletion wastes that work. |
| Differentiation vs alternatives | +1 | -1 | Few modern rsync re-implementations support wire interop back to 2.6.x; this is a differentiator. |

**Aggregate**: Keep = +5, Drop = -3. Strong net-positive for Keep.

## 6. Recommendation

Three paths considered:

### Path A (recommended for v0.7.x): KEEP with completion of RP28 validation

- KEEP wire-level back-negotiation to protocol 28 inclusive.
- COMPLETE RP28.b (rsync 2.6.9 source build), RP28.c (2.6.9 push CI cell),
  RP28.e (daemon mode against 2.6.9 client), RP28.f (client mode against
  2.6.9 daemon) as scheduled validation.
- DO NOT invest beyond the CI matrix + RP28.g/h/i regression coverage.
- KEEP the README and man-page matrix as-is: protocol 28 = "Wire-level
  support", `<= 27` = "Not supported".

### Path B (recommended for v1.0.x or later): KEEP behind cargo feature

- Introduce a cargo feature `legacy-proto-28`, default ON.
- Move all RP28.a-enumerated gates inside `#[cfg(feature = "legacy-proto-28")]`
  with stub fallbacks that reject pre-30 peers cleanly when the feature is
  off.
- Allow binary-size-conscious downstream distributions (embedded builds,
  minimal containers) to opt out at compile time without forcing the choice
  on everyone.
- Removes the feature surface *only when a user explicitly asks for it*.

### Path C (rejected): DROP back-negotiation below 30

- Move `OLDEST_SUPPORTED_PROTOCOL` from 28 to 30.
- Delete the RP28.a-enumerated gates and the RP28.g/h/i goldens.
- Cuts off legacy embedded users and bidirectional client-side interop
  against old daemons.
- Rejected: the user-base-coverage and bidirectional-client-interop weights
  (-3 and -2 respectively) outweigh the maintenance and CI savings.

**Selected: Path A for v0.7.x. Flag Path B for post-v1.0 evolution.**

## 7. What RP28.k.2 Must Execute

Based on the Path A recommendation, RP28.k.2 (issue #2972) shall:

1. **Complete RP28.b/c/e/f interop validation.**
   - RP28.b.2 / RP28.b.3: harness for the rsync 2.6.9 source build.
   - RP28.c: CI matrix cell for 2.6.9 push interop.
   - RP28.e: daemon mode against 2.6.9 client.
   - RP28.f: client mode against 2.6.9 daemon.
2. **Do NOT remove any protocol < 30 code paths.** The 39 gates from RP28.a
   stay in-tree.
3. **Update the supported-protocols matrix in README + man page only if**
   RP28.b/c/e/f surfaces an unfixable interop gap. Otherwise:
   - Protocol 28 status moves from "Wire-level support" to a stronger
     statement once RP28.b/c/e/f land green.
   - Protocol 29 status (currently "Full support" with RP28.h note) stays.
4. **File a follow-up task: "Path B evaluation for v1.0"** so the
   `legacy-proto-28` cargo feature decision is captured for the next major
   release cycle and not lost in the planning backlog.

## 8. Rollback / Re-evaluation Criteria

This decision is not permanent. It must be revisited when any of the
following triggers:

- **Interop gap surfaces.** If RP28.b/c/e/f uncovers a substantive interop
  bug whose fix requires significant new code (more than a localised gate
  flip), re-evaluate whether the fix is worth shipping versus dropping the
  affected protocol band.
- **Regression recurrence.** If a wire-byte regression in the protocol < 30
  path slips past CI more than twice (i.e., the RP28.g/h/i goldens fail to
  catch it), the maintenance-cost ceiling has been breached and the gate
  count needs to be reduced - either by Path B (cargo feature) or by raising
  the floor.
- **Zero-usage window.** If post-v1.0 telemetry or user reports indicate
  zero observed protocol < 30 connections over a 12-month window, switch
  from Path A to Path B (default-on cargo feature) to let downstream
  distributions opt out cleanly. Switch to Path C only after a further
  12-month zero-usage window with Path B's feature flag visible.
- **Upstream movement.** If upstream rsync's own `MIN_PROTOCOL_VERSION`
  rises above 28 in a future release, follow upstream and raise the
  oc-rsync floor in lockstep. (Upstream's current floor of 20 sits well
  below ours; the lockstep is currently slack.)

## 9. Cross-References

- **RP28.a inventory**: `docs/design/rp28-a-pre30-code-paths-inventory.md`
  - 39 gated branches; severity table; raw grep totals; per-item upstream
    references.
- **RP28.g wire-byte test**:
  `crates/protocol/tests/flist_wire_flags_rp28g.rs`
- **RP28.h wire-byte test**:
  `crates/protocol/tests/flist_sort_keys_rp28h.rs`
- **RP28.i wire-byte test**:
  `crates/protocol/tests/zlib_codec_proto_lt_31.rs`
- **RP28.j.1 README matrix**: `README.md:123-149`
  ("Supported rsync protocol versions" and "Supported rsync wire protocol
  versions").
- **RP28.j.2 man-page matrix**: `docs/oc-rsync.1.md:46-67`
  ("Protocol Compatibility").
- **Floor constant**:
  `crates/protocol/src/version/constants.rs:7`
  (`OLDEST_SUPPORTED_PROTOCOL = 28`).
- **Upstream floor reference**:
  `target/interop/upstream-src/rsync-3.4.1/rsync.h:147`
  (`MIN_PROTOCOL_VERSION = 20`).
- **Memory note**: [[project_protocol_compat]].
