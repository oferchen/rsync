# V61D-4 - ISI.c / ISI.d coverage of the v0.6.1 daemon-push regression

Tracking: V61D-4 (#2843). Closes the evidence gap before ISI.h flips the
sender-side INC_RECURSE default back on.

## Question

Do ISI.c (`tests/inc_recurse_single_segment_push_isi_c.rs`, PR #4842) and
ISI.d (`tests/inc_recurse_multi_segment_push_isi_d.rs`, PR #4846) exercise
the same code path that v0.6.1 regressed on, or would the 95-201x
push slowdown documented in `v061-daemon-push-regression.md` have slipped
past them?

## Symptom shape (V61D-1 recap)

PR #3557 flipped `ClientConfigBuilder` default
`inc_recursive_send = false -> true`. Push transfers (client-as-sender)
over **both SSH and daemon transports** completed with correct byte counts
and zero exit but ran **95-201x slower** than v0.6.0 on initial sync.
There was no hang, no error, no wire divergence detectable by a
correctness check - only a wall-clock regression on the sender-side
INC_RECURSE state machine.

## Coverage matrix

| Axis                              | ISI.c                          | ISI.d                          |
|-----------------------------------|--------------------------------|--------------------------------|
| Sender-side INC_RECURSE on        | Yes (feature gated)            | Yes (feature gated)            |
| Capability bit `'i'` asserted     | Yes (line 287-303, 311-319)    | Indirect (relies on ISI.c)     |
| Single-segment flist              | Yes (10-file flat tree)        | No                             |
| Multi-segment flist               | No                             | Yes (5 sub-list segments)      |
| SSH-shape transport (stdio pipes) | Yes (`run_pipe_push` 210-277)  | Yes (`run_pipe_push` 292-359)  |
| Daemon-shape transport (rsync://) | No (out of scope, see below)   | No (out of scope, see below)   |
| Push direction                    | Yes (`--server --sender`)      | Yes (`--server --sender`)      |
| Pull direction                    | No (gate is push-only by V61D-1) | No (gate is push-only by V61D-1) |
| Correctness assertion             | Yes (snapshot diff)            | Yes (snapshot diff + dir count) |
| Timing-vs-baseline assertion      | **No**                         | **No**                         |
| Upstream peers exercised          | rsync 3.4.1 only               | rsync 3.4.1 only               |

## Gap identified

**Neither ISI.c nor ISI.d would have caught the v0.6.1 regression.**
Both assert byte-identical destinations after a feature-gated
`sender-inc-recurse` push, which the regressed code path already
satisfied - v0.6.1 transfers were correct, only slow. The two missing
properties are:

1. **No wall-clock comparison.** ISI.c
   (`single_segment_push_to_upstream_3_4_1_byte_identical`, line 328-369)
   and ISI.d
   (`multi_segment_push_to_upstream_3_4_1_byte_identical`, line 368-422)
   call `run_pipe_push` and snapshot-diff. They never time the transfer
   nor compare it against a baseline. A 100x slowdown that still produces
   correct bytes passes both tests cleanly.
2. **Push transport is pipe-driven, not daemon-driven.** Both use stdio
   pipes to wire an oc-rsync `--server --sender` to an upstream rsync
   `--server` receiver. This matches the SSH transport's post-greeting
   stream shape but is not a true `rsync://` daemon handshake.

The transport-shape gap is **not material** for V61D-4: the regressed
capability bit comes from `build_capability_string(config.
inc_recursive_send())` at a shared call site
(`crates/transfer/src/setup/capability.rs:138`), and both push entry
points - daemon (`crates/core/src/client/remote/daemon_transfer/
orchestration/arguments.rs:167`) and SSH
(`crates/core/src/client/remote/invocation/builder.rs:184`) - read from
the same builder. Once the `-e.iLsfxCIvu` capability string is on the
wire, the sender-side INC_RECURSE state machine runs identically
regardless of which transport delivered the argv. Adding a duplicate
daemon-shape correctness pass would re-test the same post-handshake code
already covered by the pipe push.

The **timing gap is material** - but it is already closed elsewhere in
the ISI series.

## Remediation

**No test extension required.** The timing axis is covered by
ISI.g (`crates/transfer/benches/isi_g_sender_inc_recurse_start_time.rs`,
PR #4862), which measures three cells:

| Cell | Sender binary                                       | Metric                            |
|------|-----------------------------------------------------|-----------------------------------|
| A    | oc-rsync **without** `sender-inc-recurse` (baseline)| Time-to-first-data-bytes, total   |
| B    | oc-rsync **with** `sender-inc-recurse` (under test) | Time-to-first-data-bytes, total   |
| C    | upstream rsync 3.4.1 (reference)                    | Time-to-first-data-bytes, total   |

ISI.g asserts that the ratio `A / B` on first-byte latency tracks the
upstream reference `C`. A v0.6.1-shaped regression - sender-side
INC_RECURSE ON, but the state machine is un-tuned and 95-201x slower
than the OFF baseline - would land as cell B regressing dramatically
against cell A in the bench, which is exactly the signal v0.6.1
lacked.

ISI.h's flip-default checklist already requires "bench evidence that
push wall-clock is within 5% of upstream" (per
`v061-daemon-push-regression.md:48-49`), and ISI.g produces that
evidence. Adding a redundant timing assertion to ISI.c or ISI.d would
either (a) duplicate ISI.g's harness in a smaller fixture that cannot
distinguish start-up cost from steady-state throughput, or (b) require
ISI.c/d to grow the same baseline-vs-under-test binary matrix ISI.g
already runs - which the surgical-changes rule forbids for an
audit-only task.

## Conclusion

ISI.c and ISI.d **do not** cover the v0.6.1 wall-clock regression on
their own. They are intentionally narrow correctness tests for the
sender-side INC_RECURSE pipeline (capability bit, byte-identical
destination, single- and multi-segment flists). The timing coverage
that closes the v0.6.1 evidence gap lives in ISI.g (PR #4862), which
ISI.h's re-enable checklist already gates on. The ISI series is
self-consistent: correctness in ISI.c/d/e/f, timing in ISI.g, default
flip in ISI.h. No extension is required for ISI.c or ISI.d.

## Citations

- `tests/inc_recurse_single_segment_push_isi_c.rs:65` - feature + platform gate.
- `tests/inc_recurse_single_segment_push_isi_c.rs:210-277` - pipe-driven push.
- `tests/inc_recurse_single_segment_push_isi_c.rs:286-303` - capability-bit assert.
- `tests/inc_recurse_single_segment_push_isi_c.rs:328-369` - byte-identical assert (no timing).
- `tests/inc_recurse_multi_segment_push_isi_d.rs:76` - feature + platform gate.
- `tests/inc_recurse_multi_segment_push_isi_d.rs:105` - multi-segment tree layout.
- `tests/inc_recurse_multi_segment_push_isi_d.rs:292-359` - pipe-driven push.
- `tests/inc_recurse_multi_segment_push_isi_d.rs:368-422` - byte-identical assert (no timing).
- `crates/transfer/benches/isi_g_sender_inc_recurse_start_time.rs:1-131` - timing bench, three-cell matrix.
- `crates/transfer/src/setup/capability.rs:138` - shared capability builder.
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167` - daemon push call site.
- `crates/core/src/client/remote/invocation/builder.rs:184` - SSH push call site.
- `docs/audit/v061-daemon-push-regression.md` - V61D-1 root cause and re-enable criteria.
