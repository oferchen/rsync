# ISI.h.1 - Sender INC_RECURSE default-on flip spec

Tracking: ISI.h.1 (#2976). Parent: ISI.h (#2745). Series: ISI (#2737).
Bake-window criteria: ISI.i.1 (#2978,
`docs/design/isi-h-bake-window-criteria.md`). Companion: ISI.h.2 (#2977,
golden-test update). Follow-up: ISI.i.2 (#2979, feature-flag retirement).

## 1. Objective

Flip the default value of `inc_recursive_send` from `false` to `true` so
that push transfers advertise the `'i'` (INC_RECURSE) capability bit
without requiring the `sender-inc-recurse` cargo feature. This aligns
oc-rsync's default behavior with upstream rsync 3.4.x, where
INC_RECURSE is unconditionally enabled for both directions.

Sender-side INC_RECURSE was disabled by default in v0.6.1 after a
daemon-push regression. The ISI series (ISI.a through ISI.g) validated
the sender path through call-graph audit, feature-flag gating,
single-segment and multi-segment interop against upstream 3.0.9 / 3.1.3
/ 3.4.1 / 3.4.2, wire-byte parity, failure-mode coverage, and a
start-time benchmark confirming the cold-start latency win.

## 2. Feature flag being flipped

**Cargo feature:** `sender-inc-recurse`

Defined in three locations:

| File | Current definition |
|------|--------------------|
| `Cargo.toml:85` | `sender-inc-recurse = ["core/sender-inc-recurse", "transfer/sender-inc-recurse"]` |
| `crates/core/Cargo.toml:108` | `sender-inc-recurse = []` |
| `crates/transfer/Cargo.toml:134` | `sender-inc-recurse = []` |

The flag is NOT in any `default` feature list. It activates only via
explicit `--features sender-inc-recurse` or `--all-features`. The
builder fallback at `crates/core/src/client/config/builder/mod.rs:447`
uses `cfg!(feature = "sender-inc-recurse")` to decide whether
`inc_recursive_send` defaults to `true` or `false`.

After the flip, the builder fallback becomes an unconditional `true`.
The cargo feature becomes a no-op (retained for one release as an
emergency escape hatch, retired in ISI.i.2).

## 3. Files that change

### 3.1 Builder fallback (the semantic change)

**File:** `crates/core/src/client/config/builder/mod.rs`
**Lines:** 437-447

Before:

```rust
// ISI.b: temporary gate for sender-side INC_RECURSE interop bake-up.
// The `sender-inc-recurse` cargo feature flips the default fallback
// from `false` to `true` so push transfers advertise the `'i'`
// capability bit. CLI `--inc-recursive` / `--no-inc-recursive`
// overrides still win because they populate `Some(_)`. Default-off
// behavior is bit-for-bit identical to today; ISI.h retires this
// gate once interop is validated against 3.0.9 / 3.1.3 / 3.4.1 /
// 3.4.2. See `docs/design/isi-a-sender-inc-recurse-call-graph.md`.
inc_recursive_send: self
    .inc_recursive_send
    .unwrap_or(cfg!(feature = "sender-inc-recurse")),
```

After:

```rust
// ISI.h: sender-side INC_RECURSE is now default-on, matching
// upstream rsync 3.4.x. CLI `--no-inc-recursive` still overrides
// to `false`. The `sender-inc-recurse` cargo feature is retained
// for one release as a no-op escape hatch; ISI.i.2 retires it.
inc_recursive_send: self.inc_recursive_send.unwrap_or(true),
```

This is the single line of semantic code that constitutes the flip. Both
production call sites that read `config.inc_recursive_send()` into
`build_capability_string(allow_inc_recurse)` are unchanged:

- SSH push: `crates/core/src/client/remote/invocation/builder.rs:184`
- Daemon push: `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`

### 3.2 Workspace Cargo.toml comment update

**File:** `Cargo.toml` (lines 79-85)

Replace the ISI.b comment block with a deprecation notice:

```toml
# DEPRECATED (ISI.h): sender INC_RECURSE is now default-on. Flag
# retained for one release as an emergency-disable no-op. CLI
# `--no-inc-recursive` is the recommended override. Remove in the
# next minor version (ISI.i.2).
sender-inc-recurse = ["core/sender-inc-recurse", "transfer/sender-inc-recurse"]
```

### 3.3 Per-crate Cargo.toml comment updates

**File:** `crates/core/Cargo.toml` (lines 101-108)

Replace the ISI.b comment with:

```toml
# DEPRECATED (ISI.h): sender INC_RECURSE is now default-on. Marker
# retained for one release until ISI.i.2 retires the feature flag.
sender-inc-recurse = []
```

**File:** `crates/transfer/Cargo.toml` (lines 126-134)

Replace the ISI.b comment with:

```toml
# DEPRECATED (ISI.h): sender INC_RECURSE is now default-on. Marker
# retained for one release to keep V61D-2 / V61D-3 interop gates
# compilable until ISI.i.2 retires both.
sender-inc-recurse = []
```

### 3.4 Test updates (ISI.h.2, same PR)

Tests that assert on the default capability string or the default value
of `inc_recursive_send` must be updated. Full inventory in
`docs/design/isi-h-flip-implementation.md` section 5.1; summary:

**`crates/core/src/client/config/builder/tests.rs`:**

- `default_inc_recursive_send_is_false` (line 1593) - gated with
  `#[cfg(not(feature = "sender-inc-recurse"))]`. DELETE: the default is
  now always `true`.
- `default_inc_recursive_send_is_true_under_sender_inc_recurse_feature`
  (line 1605) - gated with `#[cfg(feature = "sender-inc-recurse")]`.
  PROMOTE to unconditional and rename to `default_inc_recursive_send_is_true`.
- `inc_recursive_send_sets_flag` and `inc_recursive_send_false_clears_flag`
  - KEEP unchanged (they test explicit overrides, not defaults).

**`crates/core/src/client/remote/invocation/tests.rs`:**

- Tests asserting `build_capability_string(false)` for sender-direction
  defaults - UPDATE to assert `build_capability_string(true)`.
- Tests gated with `#[cfg(not(feature = "sender-inc-recurse"))]` -
  REMOVE gate or DELETE/INVERT per section 5.1 of
  `docs/design/isi-h-flip-implementation.md`.
- Tests asserting explicit `inc_recursive_send(false)` override - KEEP
  unchanged.

**`crates/transfer/src/setup/tests.rs`:**

- No changes required. Tests here exercise `build_capability_string`
  with explicit `true`/`false` arguments, not the builder default.

### 3.5 Files NOT changed in ISI.h

- **`crates/transfer/src/setup/capability.rs`** - the
  `build_capability_string(allow_inc_recurse: bool)` signature is
  unchanged; the flip happens in the caller (the builder fallback).
- **`.github/workflows/_interop.yml`** - V61D-3 CI cell is left
  untouched. It becomes redundant (exercises the same always-on path)
  but continues to pass. Retirement deferred to ISI.i.2 which edits
  workflow files anyway.
- **`crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`**
  - feature gate `#![cfg(..., feature = "sender-inc-recurse")]` is left
  intact; ISI.i.2 removes it.

## 4. Pre-conditions before the flip

All of the following must be green on master at the merge-base SHA before
the ISI.h.1 PR is opened:

| Pre-condition | Artifact |
|---------------|----------|
| ISI.c single-segment push interop | `tests/inc_recurse_single_segment_push_isi_c.rs` |
| ISI.d multi-segment push interop | `tests/inc_recurse_multi_segment_push_isi_d.rs` |
| ISI.e wire-byte parity vs upstream sender | `tests/inc_recurse_sender_wire_parity_isi_e.rs` |
| ISI.f failure-mode test under flist io_error | `tests/inc_recurse_sender_flist_io_error_isi_f.rs` |
| ISI.g start-time bench | `crates/transfer/benches/isi_g_sender_inc_recurse_start_time.rs` |
| V61D-2 regression test | `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs` |
| V61D-3 CI cell green for 3+ consecutive nightlies | `.github/workflows/_interop.yml` (line 421) |
| No open issues tagged `regression` referencing `sender-inc-recurse` | GitHub issue tracker |

## 5. CI cells that exercise the flipped path

After the flip, sender-side INC_RECURSE is on by default. The following
CI cells exercise it without any special feature flags:

| CI cell | Workflow | What it exercises |
|---------|----------|-------------------|
| `nextest (stable)` Linux | `ci.yml:170` | `--workspace --all-features` - runs all ISI.c/d/e/f tests with `sender-inc-recurse` active via `--all-features` |
| `nextest (stable)` Windows | `ci.yml:254` | `-p core -p engine -p cli --all-features` - exercises builder default in core |
| `nextest (stable)` macOS | `ci.yml:470` | `-p core -p engine -p cli -p metadata -p apple-fs --all-features` - exercises builder default |
| `fmt+clippy` | `ci.yml` | `--all-features` ensures compilation of feature-gated code |
| Interop Validation | `interop-validation.yml` | Default binary (no `--features` needed post-flip) exercises sender INC_RECURSE on daemon push/pull against 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 |
| V61D-3 cell | `_interop.yml:421` | `--features sender-inc-recurse` - now redundant (flag is a no-op) but continues to pass, confirming no regression from the flag's presence |
| Test Feature Combinations | `_test-features.yml` | Cross-OS matrix rows exercise various feature subsets |
| Daemon cold-start bench | `bench-daemon-coldstart.yml` | Default binary exercises INC_RECURSE in daemon mode |

**Key observation:** Because `--all-features` activates
`sender-inc-recurse`, the Linux `nextest (stable)` cell already exercises
the sender INC_RECURSE path today. The flip makes the default binary
(without `--all-features`) also exercise it, expanding coverage to the
interop validation workflow and any ad-hoc testing with default builds.

## 6. Verification that the flip worked

### 6.1 Builder default check

After the flip, the capability string includes `'i'` for sender
direction without any explicit feature flag or builder override:

```rust
// This must hold without --features sender-inc-recurse:
let config = CoreConfigBuilder::new().build();
assert!(config.inc_recursive_send());
```

The promoted test `default_inc_recursive_send_is_true` (formerly
`default_inc_recursive_send_is_true_under_sender_inc_recurse_feature`)
asserts this unconditionally.

### 6.2 Capability string verification

```rust
use transfer::setup::build_capability_string;

// Sender direction: 'i' must be present
let caps = build_capability_string(true);
assert!(caps.contains('i'), "sender capability string must include 'i'");
```

This is already asserted by `build_capability_string_with_inc_recurse`
in `crates/transfer/src/setup/tests.rs:1031`. The builder default now
always passes `true` into this call for the sender direction.

### 6.3 CLI end-to-end verification

Run a default-build oc-rsync in verbose mode and inspect the capability
string on the wire:

```sh
cargo build --profile dist
target/dist/oc-rsync -vvv /tmp/src/ /tmp/dst/ 2>&1 | grep '\-e\.'
```

The `-e.` string must include `i`. Without the flip, sender-direction
transfers omit `i` from the capability string (unless built with
`--features sender-inc-recurse`).

### 6.4 Interop smoke test

```sh
# Daemon push against upstream rsync 3.4.1 receiver:
target/dist/oc-rsync -avvv /tmp/src/ rsync://localhost/test/ 2>&1 \
  | grep -E 'inc.recurse|capability'
```

The negotiation log must show INC_RECURSE enabled for the sender.

## 7. Rollback plan

If a regression surfaces during the ISI.i.1 bake window:

### 7.1 Revert procedure

1. Open a single revert PR that reverses the builder fallback change
   (section 3.1) and the test updates (section 3.4). The revert restores
   `unwrap_or(cfg!(feature = "sender-inc-recurse"))`, restores the
   `#[cfg(not(feature = "sender-inc-recurse"))]` test guards, and
   restores the original test assertion directions.

2. The Cargo.toml deprecation comments (sections 3.2, 3.3) can stay or
   revert at reviewer discretion - the feature definition itself MUST
   remain so that dependent gates compile.

3. The V61D-3 CI cell and V61D-2 regression test are untouched (they
   were never edited by ISI.h).

The revert is a mechanical inversion of a one-line semantic change plus
roughly 6 test function edits. No workflow files, no Cargo dependency
changes, no new crate code.

### 7.2 Diagnosis

File a regression issue tagged `regression sender-inc-recurse` under the
ISI parent tracker (#2737). Include the failing wire capture or test
output. Triage whether the regression is in the sender state machine,
the wire path, the capability negotiation, or an interop peer.

### 7.3 Re-entry

Once a fix lands, restart the ISI.i.1 bake window. The 5-nightly entry
gate restarts from the fix-merge day. Full re-entry criteria in
`docs/design/isi-h-bake-window-criteria.md` section 6.2-6.3.

### 7.4 Emergency runtime disable (no revert needed)

Users can disable sender INC_RECURSE at runtime without reverting:

- **CLI:** `--no-inc-recursive` populates `Some(false)`, overriding the
  builder default.
- **Builder API:** `CoreConfigBuilder::new().inc_recursive_send(false)`
  explicitly disables the sender path.

These overrides work regardless of the builder default, providing an
immediate mitigation path while the root cause is diagnosed.

## 8. Post-flip cleanup (ISI.i.2, separate PR)

After the bake window passes (14 calendar days or 5 consecutive green
nightlies, whichever is later):

- Remove `sender-inc-recurse` feature from all three Cargo.toml files.
- Replace `cfg!(feature = "sender-inc-recurse")` references with
  unconditional code.
- Remove `#[cfg(feature = "sender-inc-recurse")]` test gates.
- Retire the V61D-3 CI cell in `_interop.yml`.
- Update memory note `[[project_v061_daemon_push_increcurse_disable]]`
  to RESOLVED.

## 9. References

- Parent series: ISI (#2737)
- ISI.a call-graph audit: `docs/design/isi-a-sender-inc-recurse-call-graph.md`
- ISI.f failure-mode tests: `docs/design/isi-f-1-sender-inc-recurse-failure-modes.md`
- ISI.h implementation detail: `docs/design/isi-h-flip-implementation.md`
- ISI.i.1 bake-window criteria: `docs/design/isi-h-bake-window-criteria.md`
- V61D-3 CI cell: `.github/workflows/_interop.yml:421`
- Builder fallback: `crates/core/src/client/config/builder/mod.rs:445-447`
- Capability assembler: `crates/transfer/src/setup/capability.rs:138`
- SSH push call site: `crates/core/src/client/remote/invocation/builder.rs:184`
- Daemon push call site: `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:167`
- v0.6.1 regression audit: `docs/audit/v061-daemon-push-regression.md`
- Upstream reference: `options.c:3003-3050 maybe_add_e_option()`
