# v0.6.1 sender INC_RECURSE: disable rationale and un-disable plan

Tracking: V61D-5 (#2844). Memory: `[[project_v061_daemon_push_increcurse_disable]]`.

Companion documents:

- Audit: `docs/audit/v061-daemon-push-regression.md` (V61D-1, #2840, PR #4873)
- Sender call-graph: `docs/design/isi-a-sender-inc-recurse-call-graph.md` (ISI.a)
- Re-enable audit: `docs/design/inc-recurse-sender-reenable-audit.md`

## Decision

Sender-side INC_RECURSE is **disabled by default** for both push transports
(client-as-sender over SSH and daemon push) and has been since the v0.6.2
mitigation. The behaviour is gated behind the `sender-inc-recurse` cargo
feature, which is off in default builds. CLI `--inc-recursive` /
`--no-inc-recursive` continue to override per invocation. Pulls
(client-as-receiver) are unaffected: the gate governs only what we
advertise on the wire as sender; on a pull the remote sender is upstream
rsync and chooses its own behaviour.

This decision is intentionally conservative. It trades a potential
start-time win on huge file lists for guaranteed wire compatibility and
performance parity with v0.6.0 on the push path, and it stays in force
until the ISI series produces evidence that the sender INC_RECURSE state
machine is interop-safe and bench-competitive.

## Background

V61D-1 (`docs/audit/v061-daemon-push-regression.md`) traced the v0.6.1
regression:

- **Symptom**: push transfers ran **95-201x slower** than v0.6.0 on
  initial sync, with no hang, no hard error, and a zero exit code. Byte
  counts were correct; only wall-clock time was pathological.
- **Trigger**: PR #3557 / commit `39d47722b`
  (`feat(transfer): enable INC_RECURSE sender by default`) flipped
  `ClientConfig::inc_recursive_send` from `false` to `true` so the
  client-as-sender advertised the `'i'` capability bit in the `-e.`
  string for push transfers, mirroring upstream
  `compat.c:720 set_allow_inc_recurse()`.
- **Mechanism**: the flip removed a role-based wire asymmetry but routed
  every push through the sender-side INC_RECURSE state machine, which
  had never been validated against upstream interop and was not
  performance-tuned.
- **Mitigation**: PR #3744 / commit `b3a264061`
  (`fix(core): restore inc_recursive_send=false default to fix v0.6.1
  push regression`) reverted the default to `false` in
  `ClientConfigBuilder::build()`.

## Disable mechanism

The disable is implemented as a capability-string omission, not a code
path removal. The gate lives in `build_capability_string(allow_inc_recurse)`
at `crates/transfer/src/setup/capability.rs:138`. The single boolean
argument controls whether `'i'` is emitted in the `-e.` string.

Call sites supply `!is_sender` for push:

- Daemon push: `crates/transfer/src/daemon_transfer/orchestration/
  arguments.rs:167`
- SSH push: `crates/transfer/src/invocation/builder.rs:184`

The default value of `ClientConfig::inc_recursive_send` is resolved in
`ClientConfigBuilder::build()` as
`self.inc_recursive_send.unwrap_or(cfg!(feature = "sender-inc-recurse"))`.
The `sender-inc-recurse` cargo feature is off by default, so the
unwrapped value is `false` and the `'i'` bit is suppressed on push.

The sender INC_RECURSE code path itself is not removed; it remains
reachable via:

1. Explicit `--inc-recursive` on the command line, or
2. A build with `--features sender-inc-recurse`.

This preserves the code as an escape hatch for diagnosis and bench
work without exposing it to default users.

## Why this works

A capability bit omission is a **wire-safe downgrade**, not a protocol
error. The capability string is a documented negotiation surface; both
ends inspect the bits they receive and clear local flags when bits are
absent.

When we omit `'i'` from the `-e.` string on push:

1. The peer parses the capability string, sees no `'i'`, and clears
   `allow_inc_recurse` on its side.
2. Both sides then fall back to the non-INC_RECURSE sender path that
   v0.6.0 used and that has full interop coverage across rsync 3.0.9,
   3.1.3, 3.4.1, and 3.4.2.
3. The file list is sent in a single block before delta transfer rather
   than streamed in segments. This costs start-time latency on huge
   trees but matches upstream's pre-INC_RECURSE behaviour exactly.

No wire framing changes, no protocol-version downgrade, no fallback
handshake: just the well-defined absence of an optional capability.

## Un-disable plan

Re-enabling the default is owned by the ISI series (#2738-#2746). ISI.h
(#2745) is the explicit flip-default task. The series gates the flip
behind earlier tasks that build the evidence base:

- **ISI.a (#2738)** - sender INC_RECURSE call-graph audit. Done; see
  `docs/design/isi-a-sender-inc-recurse-call-graph.md`.
- **ISI.b (#2739)** - inventory the sender state machine and identify
  divergence points from upstream `flist.c` / `sender.c`.
- **ISI.c (#2740)** - full interop bake against rsync 3.0.9, 3.1.3,
  3.4.1, and 3.4.2 with the sender INC_RECURSE path forced on. Required
  green for ISI.h.
- **ISI.d (#2741)** - filter and exclude interop through the
  INC_RECURSE sender path. Required green for ISI.h.
- **ISI.e (#2742)** - hardlink, symlink, and deletion ordering interop
  through the INC_RECURSE sender path. Required green for ISI.h.
- **ISI.f (#2743)** - failure-mode coverage: mid-transfer aborts,
  ENOSPC, peer disconnects, partial file-list segments. Required green
  for ISI.h.
- **ISI.g (#2744)** - bench evidence. Compare push wall-clock with the
  sender INC_RECURSE path against upstream on representative trees
  (large file count, deep nesting, mixed sizes). Target: within 5% of
  upstream on all tracked workloads; start-time win on >100k-file trees.
- **ISI.h (#2745)** - flip the `sender-inc-recurse` feature on by
  default and remove the `cfg!(feature = ...)` gate. Pre-flip criteria:
  ISI.c / ISI.d / ISI.e green, ISI.f failure-mode green, ISI.g bench
  shows acceptable start-time and no regression vs the disabled
  default.
- **ISI.i (#2746)** - documentation sweep: update CHANGELOG, audit
  doc, and this design doc to record the flip and link the evidence.

The `sender-inc-recurse` cargo feature stays in the build matrix after
ISI.h as a long-term escape hatch.

## Rollback contract

If a daemon-push or SSH-push regression resurfaces after ISI.h flips
the default, the rollback is mechanical and well-defined:

1. Revert the ISI.h default flip. The `sender-inc-recurse` feature gate
   stays in place, so default builds immediately stop advertising
   `'i'` on push again and behaviour returns to the v0.6.2 mitigation
   state.
2. File a follow-up issue citing this design doc, the V61D-1 audit, and
   the bench numbers that triggered the rollback.
3. Re-open the ISI.g bench and ISI.f failure-mode tasks to capture the
   regression in the standing harness so the next flip attempt cannot
   re-introduce the same fault class.

The rollback explicitly **does not** require ripping out the sender
INC_RECURSE code path, the `sender-inc-recurse` feature flag, or the
CLI overrides. Those remain as the escape hatch and as the substrate
for the next flip attempt.

## Re-evaluation triggers

The disable decision is reviewed when any of these occur:

- **Upstream rsync changes the INC_RECURSE sender semantics**: a fix or
  clarification in `compat.c`, `flist.c`, or `sender.c` that resolves a
  divergence ISI.b flagged. Track via upstream commit feed.
- **A new bench result lands in ISI.g**: either showing the sender
  INC_RECURSE path is now within target, or showing it is not and
  further work is needed.
- **User complaints about push start-time on huge trees**: if real
  users report unacceptable start-time on >100k-file pushes, raise the
  priority of ISI.g and consider an interim opt-in path (CLI override
  documented in the release notes) before ISI.h.
- **A protocol-32 successor lands upstream**: protocol changes may
  alter the capability negotiation surface and require revisiting the
  disable mechanism itself, not just the default.
- **Daemon push regression resurfaces from an unrelated change**: if
  any future PR re-introduces the v0.6.1 symptom class, treat that as
  evidence the disable should stay until ISI.f failure-mode coverage
  catches the regression class in tests.

Any re-evaluation that changes the decision must update this document,
the V61D-1 audit, and the CHANGELOG, and must cite the ISI ticket that
produced the new evidence.
