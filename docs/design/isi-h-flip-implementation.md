# ISI.h - Sender INC_RECURSE default-flip implementation

Tracking: ISI.h (#2745). Implementing siblings: ISI.h.1 (#2976, default
flip) and ISI.h.2 (#2977, golden-test update). Parent series: ISI
(#2737). Bake criteria: ISI.i.1 (#2978, `docs/design/isi-h-bake-window-criteria.md`).
Follow-up: ISI.i.2 (#2979, feature-flag retirement + memory-note SHIPPED).

Memory note: `[[project_v061_daemon_push_increcurse_disable]]`.

## 1. Scope

ISI.h specifies the implementation of the sender-side INC_RECURSE
default flip. It covers:

- **ISI.h.1** - the one-line semantic change in the builder fallback
  that drives `build_capability_string(allow_inc_recurse)`.
- **ISI.h.2** - the corresponding capability-string golden / assertion
  test updates.

ISI.h.1 and ISI.h.2 LAND TOGETHER as the same PR; splitting them would
break CI between the code change and the test fixture updates. This
document is the design spec; a follow-up code PR applies the changes
once the ISI.i.1 bake-window pre-conditions are satisfied.

ISI.h does **not** retire the `sender-inc-recurse` cargo feature - that
is ISI.i.2's domain after the bake window closes.

## 2. Pre-conditions

All of the following must be true on master before the ISI.h.1/h.2 code
PR is opened:

- ISI.c, ISI.d, ISI.e, ISI.f, ISI.g all green at HEAD (single-segment
  interop, multi-segment interop, wire-byte parity, failure-mode test,
  bench).
- V61D-2 (regression test) and V61D-3 (CI matrix cell exercising
  `--features sender-inc-recurse`) green at HEAD.
- The ISI.i.1 bake-window entry pre-conditions are satisfied: 5
  consecutive green nightlies of the Interop matrix running with
  `--features sender-inc-recurse`.
- No open GitHub issues labelled `regression sender-inc-recurse`.

Note on bake-window timing: the ISI.i.1 bake window starts the day
ISI.h merges. The 5-nightly pre-condition above is the *entry* gate
that lets ISI.h land; the bake window itself begins after the merge
and runs per ISI.i.1's duration spec.

## 3. Current state

Three change sites are involved. Each must be re-read at PR-open time
to confirm the snippets have not drifted.

### 3.1 Builder fallback (the semantic site)

`crates/core/src/client/config/builder/mod.rs:445-447` currently reads:

```rust
inc_recursive_send: self
    .inc_recursive_send
    .unwrap_or(cfg!(feature = "sender-inc-recurse")),
```

The comment block immediately above (lines 437-444) labels this as the
"ISI.b temporary gate" and references this design doc's predecessors.

### 3.2 Capability-string builder (unchanged by this PR)

`crates/transfer/src/setup/capability.rs:138`:

```rust
pub fn build_capability_string(allow_inc_recurse: bool) -> String {
```

The signature is unchanged. The flip happens in the caller (3.1), not
here. Two production call sites read `config.inc_recursive_send()` into
this argument:

- SSH push: `crates/core/src/client/remote/invocation/builder.rs:184`.
- Daemon push/pull: `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`.

### 3.3 Cargo feature definition

- Workspace: `Cargo.toml:79-85` defines
  `sender-inc-recurse = ["core/sender-inc-recurse", "transfer/sender-inc-recurse"]`.
- `crates/transfer/Cargo.toml:126-134` defines the transfer-crate marker
  feature `sender-inc-recurse = []` used to gate interop tests.

## 4. Change A: capability.rs caller (the one-line flip)

Edit `crates/core/src/client/config/builder/mod.rs:445-447`.

**Before:**

```rust
inc_recursive_send: self
    .inc_recursive_send
    .unwrap_or(cfg!(feature = "sender-inc-recurse")),
```

**After:**

```rust
inc_recursive_send: self.inc_recursive_send.unwrap_or(true),
```

Also rewrite the preceding comment block (lines 437-444) to reflect
ISI.h's post-flip semantics: the default is now `true`; the
`sender-inc-recurse` cargo feature is retained for one release as an
emergency-disable no-op and will be retired in ISI.i.2; per-call API
override `inc_recursive_send: Some(false)` still works.

**Rationale.** The `sender-inc-recurse` cargo feature becomes a no-op
for the default (Cargo features cannot subtract behavior, so they
cannot re-disable the new default). Per-call API consumers can still
pass `inc_recursive_send: Some(false)` to disable, and the CLI
`--no-inc-recursive` flag continues to populate `Some(false)`. The
feature flag stays in Cargo.toml for one release as an
emergency-disable escape hatch documented in Change C; removal is
ISI.i.2.

## 5. Change B: capability-string golden / assertion tests

Grep audit (already performed for this spec):

```sh
grep -rln 'build_capability_string\|capability_string\|"-e\.' \
  crates/transfer/tests/ crates/transfer/src/setup/tests.rs \
  crates/core/src/client/remote/
```

The repo has no `crates/protocol/tests/golden/capability/` directory
and no `crates/transfer/tests/capability_*.rs` files; the capability
string is asserted inline in unit/integration tests instead of via
on-disk byte goldens. The inventory below enumerates every assertion
file that must be reviewed when the flip lands.

### 5.1 Files that must be updated

These tests assert on the sender-direction or default-direction
capability string and currently use `build_capability_string(false)`
or `#[cfg(not(feature = "sender-inc-recurse"))]` guards.

- `crates/core/src/client/remote/invocation/tests.rs`
  - `builds_receiver_invocation_with_sender_flag` (lines 15-33). Drop
    the `_(false)` expectation; assert against `_(true)` because the
    default is now ON.
  - `builds_sender_invocation_no_sender_flag` (lines 35-55). Same. Drop
    the `#[cfg(not(feature = "sender-inc-recurse"))]` guard; the test
    must run unconditionally after the flip.
  - `capability_string_present_in_sender_args` (lines 1664-1677). Drop
    the `cfg(not(...))` guard; assert against `_(true)`.
  - `capability_string_present_in_receiver_args` (lines 1679-1692).
    Same.
  - `ssh_sender_omits_inc_recurse_capability_by_default` (lines
    57-78). DELETE or invert: the post-flip default is to ADVERTISE
    `'i'` on sender. Replace with
    `ssh_sender_advertises_inc_recurse_capability_by_default` asserting
    `caps_str.contains('i')`.
  - `ssh_receiver_omits_inc_recurse_capability_by_default` (lines
    100-120). Same: invert to assert presence of `'i'` on receiver
    capability by default.
  - `ssh_sender_omits_inc_recurse_when_no_inc_recursive_set` (lines
    80-98). KEEP unchanged - this tests the explicit
    `inc_recursive_send(false)` override path, which still suppresses
    `'i'`.
  - `ssh_receiver_omits_inc_recurse_when_no_inc_recursive_set` (lines
    122-139). KEEP unchanged - same as above for receiver.
  - All-flags test asserting
    `args.contains(&build_capability_string(true))` (line 2188). KEEP
    unchanged - the all-flags fixture already enables the flag
    explicitly.

- `crates/core/src/client/remote/daemon_transfer/orchestration/tests.rs`
  - `build_full_args_push_omits_inc_recurse_capability_by_default`
    (lines 80-110). DELETE or invert: rename to
    `build_full_args_push_advertises_inc_recurse_capability_by_default`
    and assert `caps_default.contains('i')`. The explicit
    `inc_recursive_send(true)` sub-assertion can be kept as a
    redundant sanity check or merged into the rename.
  - `build_full_args_pull_omits_inc_recurse_capability_by_default`
    (lines 112-142). Same inversion for pull direction.

- `crates/transfer/src/setup/tests.rs`
  - `build_capability_string_without_inc_recurse` (lines 996-1006).
    KEEP unchanged - this tests the function with the argument
    explicitly false, which is still a valid code path.
  - `build_capability_string_with_inc_recurse` (lines 1031-1036). KEEP
    unchanged.
  - `build_capability_string_matches_mapping_order` (line 1038+). KEEP
    unchanged.
  - `build_capability_string_includes_symlink_iconv_when_iconv_compiled_in`
    (lines 1008-1017). KEEP unchanged.
  - `build_capability_string_omits_symlink_iconv_when_iconv_disabled`
    (lines 1019-1029). KEEP unchanged.
  - `capability_string_always_includes_xattr_marker` (line 1450). KEEP
    unchanged.
  - `capability_string_does_not_contain_acl_xattr_transfer_flags`
    (line 1464). KEEP unchanged.
  - Other tests in this file pass literal `-e.LsfxCIvu` /
    `-e.LsfxCIVu` strings as INPUTS to the negotiator (lines 19, 438,
    864, 909, 940, 958); these test the parser, not the builder
    default. KEEP unchanged.

### 5.2 Direction discipline

Per the task brief: do NOT update assertions that capture the
receiver-direction string in contexts where it already includes `'i'`.
Inspection of the codebase shows the SSH builder takes its
`allow_inc_recurse` argument from `config.inc_recursive_send()` for
**both** directions (the receiver tests at 100-120 and 122-139 confirm
this). So the flip affects sender AND receiver default capability
strings symmetrically. The "sender-only" framing in the task brief is
historical from earlier RFC drafts; the actual implementation uses one
shared flag for both directions, matching upstream's single
`allow_inc_recurse` global (`options.c:3003-3050`). The test inversions
above cover both.

### 5.3 V61D-2 regression test

`crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`
is gated by `#![cfg(all(unix, not(target_os = "macos"), feature = "sender-inc-recurse"))]`.
Post-flip, the feature is a no-op and the gate becomes redundant. KEEP
the gate untouched in ISI.h's PR - ISI.i.2 removes it together with
the feature retirement.

## 6. Change C: feature-flag transition

Retain the `sender-inc-recurse` cargo feature in both Cargo.toml files
as a no-op for ONE release after ISI.h merges. The retention is an
emergency-disable hatch: if a regression surfaces in the bake window
and a full revert is undesirable, downstream consumers can pin a
release and use `--no-default-features` plus a curated feature list to
opt out (though the recommended remediation is the rollback procedure
in section 9).

**Workspace `Cargo.toml:79-85`** - replace the existing ISI.b comment
with:

```toml
# DEPRECATED: sender INC_RECURSE is now default-on (ISI.h).
# Flag retained for one release as an emergency-disable no-op so
# downstream consumers can pin a release if the bake window surfaces a
# regression. Remove in vX.Y+1 (ISI.i.2).
sender-inc-recurse = ["core/sender-inc-recurse", "transfer/sender-inc-recurse"]
```

**`crates/transfer/Cargo.toml:126-134`** - replace the existing ISI.b
marker comment with:

```toml
# DEPRECATED: sender INC_RECURSE is now default-on (ISI.h). Marker
# feature retained for one release to keep V61D-2 / V61D-3 interop
# gates compilable until ISI.i.2 retires both. Remove in vX.Y+1.
sender-inc-recurse = []
```

Substitute `vX.Y+1` with the concrete next minor version at PR-open
time (read `version` in workspace `Cargo.toml`).

## 7. Change D: V61D-3 CI cell

`.github/workflows/_interop.yml:327` runs the daemon-push interop with
`--features sender-inc-recurse`. With the flag now a no-op default-on,
the V61D-3 cell continues to PASS but becomes redundant (it now
exercises the same code path the default matrix exercises).

**Do NOT edit `.github/workflows/` in ISI.h's PR.** The current OAuth
scope on this session does not include workflow write. ISI.i.2 (which
will need a workflow edit anyway to retire the matrix cell when the
feature is removed) is the right place to delete or repurpose V61D-3.

The ISI.h PR description must explicitly call out that V61D-3 remains
intentionally untouched and continues to pass on the always-on path as
acceptance evidence that the flip did not regress the cell.

## 8. Acceptance criteria

The ISI.h.1/h.2 implementation PR is acceptable when ALL of:

- The `crates/core/src/client/config/builder/mod.rs:445-447` change is
  applied (one line of semantic code plus the comment-block rewrite
  from Change A).
- The test inversions/deletions enumerated in section 5.1 are applied
  (estimated 6-8 test functions across 2 files; the
  `crates/transfer/src/setup/tests.rs` file requires no changes).
- The Cargo.toml deprecation comments from Change C are added to both
  workspace `Cargo.toml` and `crates/transfer/Cargo.toml`.
- The PR description cites this design doc AND the ISI.i.1 bake
  criterion (`docs/design/isi-h-bake-window-criteria.md`) as gating
  evidence; the entry pre-conditions in section 2 are demonstrated
  green at PR-open.
- All required CI checks (fmt+clippy, nextest stable, Windows, macOS,
  Linux musl, Interop Validation) are green.
- The V61D-3 CI cell remains green, exercising the always-on path
  without source-tree changes.
- No new test gated on `#[cfg(feature = "sender-inc-recurse")]` is
  added (the feature is being retired; new tests should be
  unconditional).

## 9. Rollback procedure

If a regression surfaces during the ISI.i.1 bake window that traces to
the ISI.h flip:

1. **Revert.** Open a single revert PR that reverses both Change A and
   Change B. The revert PR must restore the original builder fallback
   (`unwrap_or(cfg!(feature = "sender-inc-recurse"))`), restore the
   test cfg-guards and assertion direction, and restore the original
   Cargo.toml comment blocks. Change C deprecation comments can stay
   or revert at reviewer discretion; the feature itself MUST remain
   defined so dependent gates continue to compile. Change D is
   untouched (workflow file was never edited).
2. **Diagnose.** File a regression issue tagged `regression
   sender-inc-recurse` with the failing wire capture or test output.
   Investigate under the parent ISI tracker (#2737). Triage whether
   the regression is in the sender state machine, the wire path, the
   capability negotiation, or a third-party (interop peer) bug.
3. **Re-enter bake.** Once a fix lands, restart the ISI.i.1 bake
   window per the re-entry procedure in
   `docs/design/isi-h-bake-window-criteria.md`. The 5-nightly entry
   gate restarts from the fix-merge day.

The revert PR must be openable and mergeable in a single pass; this
spec keeps the change surface narrow (one semantic line, ~6 test
inversions, two comment blocks) precisely to make rollback safe and
mechanical.

## 10. Why this matters

- Sender-side INC_RECURSE was disabled by default after the v0.6.1
  regression (see memory note
  `[[project_v061_daemon_push_increcurse_disable]]` and
  `docs/audit/v061-daemon-push-regression.md`). The ISI.c through
  ISI.g sub-tasks built and shipped the validation evidence -
  single-segment + multi-segment interop against 3.0.9 / 3.1.3 /
  3.4.1 / 3.4.2, wire-byte parity, failure-mode coverage, and a
  benchmark - confirming it is safe to re-enable.
- With INC_RECURSE on, the sender ships file-list segments
  incrementally instead of building the full list before any wire
  transmission. Cold-start latency on large source trees drops
  materially (cf. the ISI.g bench result).
- The flip brings oc-rsync's default behavior in line with upstream
  rsync 3.4.x, removing a quiet behavioral divergence that surprises
  users migrating from upstream.

## 11. Cross-references

- `docs/design/isi-a-sender-inc-recurse-call-graph.md` - the call-graph
  audit that opened the ISI series.
- `docs/design/isi-f-1-sender-inc-recurse-failure-modes.md` (PR
  #4936) - failure-mode test spec.
- `docs/design/isi-h-bake-window-criteria.md` (PR #4917) - ISI.i.1
  bake-window entry/exit criteria.
- `docs/design/inc-recurse-sender-reenable-audit.md` - the broader
  re-enable audit.
- `docs/audit/v061-daemon-push-regression.md` - the original v0.6.1
  regression audit.
- V61D-2 regression test:
  `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`.
- V61D-3 CI cell: `.github/workflows/_interop.yml:327` (not edited by
  ISI.h; retired in ISI.i.2).
- Memory note `[[project_v061_daemon_push_increcurse_disable]]` - will
  receive a SHIPPED update in ISI.i.2 once the bake window closes.
- Upstream reference: `options.c:3003-3050 maybe_add_e_option()` and
  `compat.c:712-732` for the `-e` capability negotiation.
