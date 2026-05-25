# ISI.h - Bake-window criteria for the sender-side INC_RECURSE default flip

Tracking: ISI.i.1 (#2978). Sibling: ISI.i (#2746). Parent series:
ISI (#2737). Follow-up: ISI.i.2 (#2979). Implementing siblings:
ISI.h.1 (#2976, default flip), ISI.h.2 (#2977, golden-test update).

Memory note: `[[project_v061_daemon_push_increcurse_disable]]`.

## 1. Scope

ISI.i.1 defines the bake-window for the sender-side INC_RECURSE
default flip landed by ISI.h. The bake window is the period between
"default flipped to enabled on master" and "feature flag and disable
path can be removed". This document defines:

- (a) the duration of the bake window;
- (b) the signals to monitor while the window is open;
- (c) the pass criteria that let ISI.i.2 retire the
  `sender-inc-recurse` cargo feature.

The flip target is the `allow_inc_recurse` argument passed into
`build_capability_string` at
`crates/transfer/src/setup/capability.rs:138`. Today that argument is
driven by `config.inc_recursive_send()`, which itself defaults via
`crates/core/src/client/config/builder/mod.rs:445-447`:

```rust
inc_recursive_send: self
    .inc_recursive_send
    .unwrap_or(cfg!(feature = "sender-inc-recurse")),
```

The two production call sites that read this value into the capability
string are:

- SSH push - `crates/core/src/client/remote/invocation/builder.rs:184`.
- Daemon push -
  `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`.

ISI.h flips the builder fallback from `cfg!(feature = "sender-inc-recurse")`
to an unconditional `true`. ISI.i.2 removes the feature flag and the
disable path after the bake window passes.

## 2. Pre-conditions for opening the bake window

ISI.h.1 may only land on master once all of the following are green
at the merge-base SHA:

- ISI.c single-segment push interop -
  `tests/inc_recurse_single_segment_push_isi_c.rs`.
- ISI.d multi-segment push interop -
  `tests/inc_recurse_multi_segment_push_isi_d.rs`.
- ISI.e wire-byte parity vs upstream sender -
  `tests/inc_recurse_sender_wire_parity_isi_e.rs`.
- ISI.f failure-mode test under flist `io_error` -
  `tests/inc_recurse_sender_flist_io_error_isi_f.rs`.
- V61D-2 regression test reproducing the v0.6.1 daemon-push failure
  mode -
  `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`.
- V61D-3 CI matrix cell exercising daemon push with
  `--features sender-inc-recurse` - green for at least 3 consecutive
  nightlies. Cell defined at
  `.github/workflows/_interop.yml:327` ("Run interop tests with
  sender-inc-recurse feature (V61D-3)").
- No open GitHub issues tagged `regression` referencing
  `sender-inc-recurse` or sender-side INC_RECURSE behaviour.

If any pre-condition is red, ISI.h.1 does not land and the bake
window does not open. The pre-conditions are themselves the entry
criteria; they are not part of the window duration.

## 3. Bake-window duration

- Minimum: **14 calendar days** OR **5 consecutive green nightly runs
  across every interop matrix cell**, whichever is later.
- During the window, the `sender-inc-recurse` cargo feature remains in
  the codebase as an emergency disable path. Operators can opt back
  out with `--no-inc-recursive` on the CLI tri-state at
  `crates/cli/src/frontend/command_builder/sections/build_base_command/transfer.rs:40,43-44`,
  or by building without the feature when invoking the builder
  directly.
- `build_capability_string` defaults to `true` for sender direction
  through the ISI.h.1 commit; the disable path is the explicit
  builder setter
  `crates/core/src/client/config/builder/performance.rs:234`
  (`inc_recursive_send(false)`).
- The bake window starts the calendar day the ISI.h.1 commit lands
  on master. Day 0 is the merge day in UTC.

## 4. Monitored signals during the bake window

All signals listed here must remain green for the full window. A red
signal that is conclusively traced to an unrelated cause does not
count against the window but must be documented in the closure note
(see Section 6).

- Required CI checks on every PR merging to master:
  - `fmt+clippy`
  - `nextest (stable)`
  - `Windows (stable)`
  - `macOS (stable)`
  - `Linux musl (stable)`
- Interop Validation workflow nightly runs across every supported
  upstream version: 3.0.9, 3.1.3, 3.4.1, 3.4.2.
- Daemon-mode interop nightly via `tools/ci/run_interop.sh`.
- V61D-3 CI cell (sender-inc-recurse feature flag exercise at
  `.github/workflows/_interop.yml:327`).
- ISI.g start-time bench at
  `crates/transfer/benches/isi_g_sender_inc_recurse_start_time.rs` -
  must continue to show the start-time win that motivated the flip.
- Production user reports: zero new GitHub issues tagged `bug` AND
  referencing INC_RECURSE, sender-side push, or daemon push.

## 5. Disqualifying signals

Any of the following resets the bake-window clock to day 0:

- Any required-check CI failure attributable to sender INC_RECURSE
  behaviour. Attribution is verified by reverting ISI.h.1 on a test
  branch and confirming the failure clears.
- Any user-reported correctness bug attributable to the flip:
  silent transfer corruption, missing files, wrong file content,
  wrong file permissions, wrong file modes, wrong hardlink graph.
- Any interop regression against the supported upstream version
  matrix (3.0.9, 3.1.3, 3.4.1, 3.4.2).
- Wire-byte divergence detected by the ISI.e parity test
  (`tests/inc_recurse_sender_wire_parity_isi_e.rs`) in nightly runs.
- Performance regression > 10% on the receiver-throughput bench
  (DPC or equivalent), measured against the pre-flip baseline.
  Enabling INC_RECURSE shifts file-list construction work onto the
  sender; a > 10% receiver-throughput loss is a signal that the
  pipeline is not actually overlapping as designed.

Reset semantics: the clock restarts only if the regression cannot be
fixed in a forward-fix PR within 7 calendar days. A forward-fix that
lands within 7 days keeps the existing clock; the window simply
absorbs the bug as part of the bake.

## 6. Bake-window outcome paths

The window ends in exactly one of three states.

### 6.1 Pass

All signals from Section 4 stayed green for the full window and no
disqualifying signal from Section 5 fired. Action:

- Fire ISI.i.2 (#2979).
- Remove the `sender-inc-recurse` cargo feature from
  workspace `Cargo.toml`, `crates/core/Cargo.toml`, and any
  per-crate forwarding entries.
- Remove the `cfg!(feature = "sender-inc-recurse")` fallback at
  `crates/core/src/client/config/builder/mod.rs:445-447` and replace
  it with an unconditional `unwrap_or(true)`.
- Delete the
  `#[cfg(not(feature = "sender-inc-recurse"))]`-gated tests in
  `crates/core/src/client/config/builder/tests.rs` and
  `crates/core/src/client/remote/invocation/tests.rs`; promote the
  positive-feature path to unconditional.
- Update the wire-format golden in
  `crates/transfer/src/setup/tests.rs` so the asserted capability
  string for the sender direction is the with-`'i'` variant
  unconditionally.
- Update memory note
  `[[project_v061_daemon_push_increcurse_disable]]` to RESOLVED with
  a pointer to the closure commit.

### 6.2 Fail with clear root cause

A disqualifying signal fired and the root cause is unambiguous (a
specific commit, a specific test, a specific upstream version). Action:

- Revert ISI.h.1 default flip on master (a single-commit revert).
- Fix the identified bug in a follow-up PR with a regression test
  that fails before the fix and passes after.
- Once the fix lands, restart the bake window from day 0 with the
  same pre-conditions in Section 2.

### 6.3 Fail with ambiguous symptom

A disqualifying signal fired but no clear root cause was found within
7 calendar days. Action:

- Keep the flip enabled on master.
- Extend the bake window by another full minimum window (14 days /
  5 nightlies), counted from the day the signal was first observed.
- Investigate in parallel.
- If no clear root cause is found within 2x the minimum window
  (28 calendar days or 10 nightly runs), revert ISI.h.1 and re-enter
  the pre-condition phase. The series cannot exit on an
  unexplained-but-stable signal.

## 7. Communication template

Release-notes snippet for the post-bake-window release. Fill in the
bracketed values at release time.

> **Sender-side INC_RECURSE is now enabled by default for push
> transfers.** This was previously gated behind the
> `sender-inc-recurse` cargo feature since v0.6.1 due to a daemon-push
> regression; the regression was fixed by `[related task ID]` and
> validated through a `[N]`-day bake window with zero attributable
> incidents. The temporary feature flag will be removed in
> `v[N+1]`. Operators who need to opt back out can pass
> `--no-inc-recursive` on the CLI.

## 8. References

- Parent series: ISI (#2737).
- Sibling: ISI.i (#2746).
- Implementing PRs: ISI.h.1 (#2976), ISI.h.2 (#2977).
- Follow-up: ISI.i.2 (#2979).
- Audit precedent: ISI.a call-graph
  (`docs/design/isi-a-sender-inc-recurse-call-graph.md`).
- Coverage audit: V61D-4
  (`docs/audit/v61d-4-isi-cd-coverage-of-v061.md`).
- Memory note: `[[project_v061_daemon_push_increcurse_disable]]`.
- Capability assembler: `crates/transfer/src/setup/capability.rs:138`
  (`build_capability_string(allow_inc_recurse: bool) -> String`).
- Builder fallback:
  `crates/core/src/client/config/builder/mod.rs:445-447`.
- SSH push call site:
  `crates/core/src/client/remote/invocation/builder.rs:184`.
- Daemon push call site:
  `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`.
- V61D-3 CI cell: `.github/workflows/_interop.yml:327`.
