# v0.6.1 daemon-push regression root cause

Tracking: V61D-1 (#2840).

## Symptom

Push transfers (client-as-sender) over both SSH and daemon transports ran
**95-201x slower** than v0.6.0 on initial sync. No hang, no hard error, no
non-zero exit; transfers completed with correct byte counts but walked an
un-tuned code path, dragging wall-clock time into pathological territory.

## Root Cause

PR #3557 (commit `39d47722b`, `feat(transfer): enable INC_RECURSE sender
by default`) flipped the default of `ClientConfig::inc_recursive_send`
from `false` to `true` so the client-as-sender advertises the `'i'`
capability bit in the `-e.` string for push transfers, mirroring upstream
`compat.c:720 set_allow_inc_recurse()`. The flip removed a role-based wire
asymmetry but routed every push through the sender-side INC_RECURSE state
machine, which had never been validated against upstream interop and was
not performance-tuned. Pulls were unaffected: the gate governs only what
**we** advertise, and the remote sender on a pull is upstream rsync.

## Mitigation

PR #3744 (commit `b3a264061`, `fix(core): restore inc_recursive_send=false
default to fix v0.6.1 push regression`) reverted the default to `false` in
`ClientConfigBuilder::build()`. Today the default sits behind the
`sender-inc-recurse` cargo feature: `self.inc_recursive_send.unwrap_or(
cfg!(feature = "sender-inc-recurse"))`. CLI `--inc-recursive` /
`--no-inc-recursive` still override per invocation.

## Why It Works

`build_capability_string(allow_inc_recurse)` (`crates/transfer/src/setup/
capability.rs:138`) gates the `'i'` character on its single argument. With
`inc_recursive_send=false`, the push call site (`daemon_transfer/
orchestration/arguments.rs:167`) and the SSH call site (`invocation/
builder.rs:184`) emit a capability string without `'i'`, so the peer
clears `allow_inc_recurse` and both sides fall back to the fully-baked
non-INC_RECURSE sender path v0.6.0 used. Wire-safe: a missing capability
bit is a documented downgrade, not a protocol error.

## Re-enable Criteria

Tracked by the ISI series (#2737-#2746). ISI.h is the explicit flip-default
task and requires (1) full interop bake against rsync 3.0.9 / 3.1.3 /
3.4.1 / 3.4.2 through the sender-INC_RECURSE path and (2) bench evidence
that push wall-clock is within 5% of upstream.
