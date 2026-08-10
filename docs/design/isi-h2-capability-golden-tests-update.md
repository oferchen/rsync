# ISI.h.2 - Capability-string golden tests update

Tracking: ISI.h.2 (#2977). Sibling: ISI.h.1 (#2976, default flip).
Parent: ISI.h (#2745). Bake window: ISI.i.1 (#2978). Feature
retirement: ISI.i.2 (#2979).

## 1. Scope

ISI.h.2 updates every test that asserts on the default capability
string to reflect the ISI.h.1 semantic change: `inc_recursive_send`
defaults to `true`, so `build_capability_string(true)` is the new
default output. The `'i'` capability character (INC_RECURSE) now
appears in both sender and receiver capability strings by default.

ISI.h.2 lands in the **same PR** as ISI.h.1. Splitting them would
break CI between the code change and the test updates.

## 2. Prerequisite

ISI.h.1 must be applied first (or simultaneously). ISI.h.1 changes
`crates/core/src/client/config/builder/mod.rs:445-447` from:

```rust
inc_recursive_send: self
    .inc_recursive_send
    .unwrap_or(cfg!(feature = "sender-inc-recurse")),
```

to:

```rust
inc_recursive_send: self.inc_recursive_send.unwrap_or(true),
```

## 3. Capability string before vs after

On Unix without the `iconv` feature (standard build):

| Argument            | Before (default build) | After ISI.h.1       |
|---------------------|------------------------|----------------------|
| `allow_inc_recurse` | `false`                | `true`               |
| Output              | `"-e.LfxCIvu"`        | `"-e.iLfxCIvu"`     |

The only difference is the insertion of `'i'` at the front of the
character list (position matches `CAPABILITY_MAPPINGS` table order
in `crates/transfer/src/setup/capability.rs`).

On non-Unix platforms, `'L'` (SYMLINK_TIMES) is omitted:

| Argument            | Before (default build) | After ISI.h.1       |
|---------------------|------------------------|----------------------|
| `allow_inc_recurse` | `false`                | `true`               |
| Output              | `"-e.fxCIvu"`          | `"-e.ifxCIvu"`      |

## 4. Test inventory

### 4.1 Tests that must be updated

#### `crates/core/src/client/remote/invocation/tests.rs`

1. **`builds_receiver_invocation_with_sender_flag`** (line 14-33)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     against `build_capability_string(false)`.
   - Action: Remove the `cfg` gate. Change expected to
     `build_capability_string(true)`.

2. **`builds_sender_invocation_no_sender_flag`** (line 36-55)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     against `build_capability_string(false)`.
   - Action: Remove the `cfg` gate. Change expected to
     `build_capability_string(true)`.

3. **`ssh_sender_omits_inc_recurse_capability_by_default`** (line 58-78)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     `!caps_str.contains('i')` and
     `caps == build_capability_string(false)`.
   - Action: Remove the `cfg` gate. Rename to
     `ssh_sender_advertises_inc_recurse_capability_by_default`. Invert
     to assert `caps_str.contains('i')` and
     `caps == build_capability_string(true)`.

4. **`ssh_receiver_omits_inc_recurse_capability_by_default`** (line 101-120)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     `!caps_str.contains('i')`.
   - Action: Remove the `cfg` gate. Rename to
     `ssh_receiver_advertises_inc_recurse_capability_by_default`.
     Invert to assert `caps_str.contains('i')`.

5. **`capability_string_present_in_sender_args`** (line 1665-1677)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     against `build_capability_string(false)`.
   - Action: Remove the `cfg` gate. Change expected to
     `build_capability_string(true)`.

6. **`capability_string_present_in_receiver_args`** (line 1680-1692)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     against `build_capability_string(false)`.
   - Action: Remove the `cfg` gate. Change expected to
     `build_capability_string(true)`.

#### `crates/core/src/client/remote/daemon_transfer/orchestration/tests.rs`

7. **`build_full_args_push_omits_inc_recurse_capability_by_default`** (line 80-110)
   - Currently: asserts `!caps_default.contains('i')` for default config.
   - Action: Rename to
     `build_full_args_push_advertises_inc_recurse_by_default`. Invert
     the default-config assertion to `caps_default.contains('i')`. Keep
     the `inc_recursive_send(true)` sub-assertion as a sanity check (or
     merge into the main body).

8. **`build_full_args_pull_omits_inc_recurse_capability_by_default`** (line 112-142)
   - Currently: asserts `!caps_default.contains('i')` for default config.
   - Action: Rename to
     `build_full_args_pull_advertises_inc_recurse_by_default`. Invert
     the default-config assertion to `caps_default.contains('i')`. Keep
     the `inc_recursive_send(true)` sub-assertion.

#### `crates/core/src/client/config/builder/tests.rs`

9. **`default_inc_recursive_send_is_false`** (line 1592-1601)
   - Currently: `#[cfg(not(feature = "sender-inc-recurse"))]`, asserts
     `!config.inc_recursive_send()`.
   - Action: Remove entirely. The companion test
     `default_inc_recursive_send_is_true_under_sender_inc_recurse_feature`
     (line 1604-1612) becomes the unconditional default test.

10. **`default_inc_recursive_send_is_true_under_sender_inc_recurse_feature`** (line 1604-1612)
    - Currently: `#[cfg(feature = "sender-inc-recurse")]`, asserts
      `config.inc_recursive_send()`.
    - Action: Remove the `cfg` gate. Rename to
      `default_inc_recursive_send_is_true`. Update the comment to
      reflect that this is the permanent default after ISI.h.

### 4.2 Tests that must NOT be updated

These tests exercise explicit overrides, parser inputs, or the
`build_capability_string` function with hardcoded arguments. They
remain correct after the flip.

#### `crates/core/src/client/remote/invocation/tests.rs`

- **`ssh_sender_omits_inc_recurse_when_no_inc_recursive_set`** (line 80-98):
  Tests explicit `inc_recursive_send(false)` override. Keep unchanged.
- **`ssh_receiver_omits_inc_recurse_when_no_inc_recursive_set`** (line 122-139):
  Tests explicit `inc_recursive_send(false)` override. Keep unchanged.
- **All-flags test** (line 2188): Already uses
  `build_capability_string(true)` via explicit `.inc_recursive_send(true)`.
  Keep unchanged.

#### `crates/transfer/src/setup/tests.rs`

- **`build_capability_string_without_inc_recurse`** (line 996-1006):
  Tests `build_capability_string(false)` directly. Valid code path.
- **`build_capability_string_with_inc_recurse`** (line 1031-1036):
  Tests `build_capability_string(true)` directly.
- **`build_capability_string_matches_mapping_order`** (line 1038+):
  Tests ordering with `build_capability_string(true)`.
- **iconv-gated tests** (line 1008-1029): Feature-gate tests, not
  default-dependent.
- **`capability_string_always_includes_xattr_marker`** (line 1450):
  Uses `build_capability_string(true)` directly.
- **`capability_string_does_not_contain_acl_xattr_transfer_flags`** (line 1464):
  Uses `build_capability_string(true)` directly.
- All tests using literal `-e.LsfxCIvu` / `-e.LsfxCIVu` strings as
  INPUTS to `parse_client_info` or `write_compat_flags` (lines 19,
  438, 789, 864, 909, 940, 958). These simulate upstream client
  messages, not our builder output.

#### `crates/core/src/client/config/builder/tests.rs`

- **`inc_recursive_send_sets_flag`** (line 1577): Explicit `true`.
- **`inc_recursive_send_false_clears_flag`** (line 1583): Explicit
  override chain.

#### `crates/protocol/tests/golden_handshakes.rs`

- All golden compat flags tests (lines 556-650): Test wire encoding of
  `CompatibilityFlags` bitfields, not the builder default. Unaffected.

#### `crates/protocol/tests/debug_cmd_emissions.rs` and `crates/protocol/src/cmd/trace.rs`

- Hardcoded `-logDtpre.LsfxCIvu` strings are test inputs simulating
  upstream client argument parsing. Not assertions on our builder.

#### `crates/core/tests/daemon_client_interop.rs`

- Hardcoded `b"-e.LsfxCIvu\0"` at lines 491 and 784 simulate upstream
  rsync 3.4.1 client capability strings sent during daemon handshake
  testing. These are parser inputs, not builder output assertions.

#### `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`

- Gated by `#![cfg(all(unix, not(target_os = "macos"), feature = "sender-inc-recurse"))]`.
  Post-flip the feature is a no-op so this gate becomes redundant, but
  it is not broken - the test still compiles and runs when the feature
  is enabled (which it always effectively is after the flip). Removal
  of the gate is ISI.i.2 scope, not ISI.h.2.

## 5. Verification checklist

After applying both ISI.h.1 and ISI.h.2:

1. `cargo nextest run -p core --all-features -E 'test(inc_recursive)'`
   - All renamed/updated tests pass.
   - No `cfg(not(feature = "sender-inc-recurse"))` guarded tests remain
     for the affected functions.

2. `cargo nextest run -p core --all-features -E 'test(capability)'`
   - All capability string assertion tests pass.

3. `cargo nextest run -p transfer --all-features -E 'test(build_capability)'`
   - Direct builder tests still pass for both `true` and `false`
     arguments.

4. `cargo nextest run -p core --all-features -E 'test(daemon_transfer)'`
   - Daemon orchestration tests pass with inverted assertions.

5. `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings`
   - No dead-code warnings from removed `cfg` gates.

6. Full CI matrix (fmt+clippy, nextest, Windows, macOS, Linux musl)
   must pass.

7. Grep audit confirming no remaining assertions that assume
   `build_capability_string(false)` is the default output:
   ```sh
   grep -rn 'build_capability_string(false)' \
     crates/core/src/client/remote/ \
     crates/core/src/client/config/builder/tests.rs
   ```
   Expected: only the explicit-override tests
   (`ssh_sender_omits_inc_recurse_when_no_inc_recursive_set` and
   `ssh_receiver_omits_inc_recurse_when_no_inc_recursive_set`) and
   the direct function tests in `transfer/src/setup/tests.rs`.

8. Grep audit confirming no remaining `cfg(not(feature = "sender-inc-recurse"))`
   guards on capability-related tests:
   ```sh
   grep -rn 'cfg(not(feature = "sender-inc-recurse"))' \
     crates/core/src/client/remote/invocation/tests.rs \
     crates/core/src/client/config/builder/tests.rs
   ```
   Expected: zero matches.

## 6. Non-affected golden tests

The `crates/protocol/tests/golden/` directory does not contain
capability-string snapshot files. All capability string assertions are
inline in Rust test functions. The protocol crate's golden tests cover:

- Wire encoding of `CompatibilityFlags` bitfields (handshake goldens)
- Protocol greeting byte format
- Multiplex header encoding
- File list wire format
- Compression codec wire format

None of these reference `build_capability_string()` or depend on the
default value of `inc_recursive_send`. They are unaffected by ISI.h.

## 7. Summary of changes

| # | File | Test | Action |
|---|------|------|--------|
| 1 | `invocation/tests.rs` | `builds_receiver_invocation_with_sender_flag` | Remove `cfg`, assert `_(true)` |
| 2 | `invocation/tests.rs` | `builds_sender_invocation_no_sender_flag` | Remove `cfg`, assert `_(true)` |
| 3 | `invocation/tests.rs` | `ssh_sender_omits_inc_recurse_...` | Rename+invert, remove `cfg` |
| 4 | `invocation/tests.rs` | `ssh_receiver_omits_inc_recurse_...` | Rename+invert, remove `cfg` |
| 5 | `invocation/tests.rs` | `capability_string_present_in_sender_args` | Remove `cfg`, assert `_(true)` |
| 6 | `invocation/tests.rs` | `capability_string_present_in_receiver_args` | Remove `cfg`, assert `_(true)` |
| 7 | `orchestration/tests.rs` | `build_full_args_push_omits_...` | Rename+invert |
| 8 | `orchestration/tests.rs` | `build_full_args_pull_omits_...` | Rename+invert |
| 9 | `builder/tests.rs` | `default_inc_recursive_send_is_false` | Delete |
| 10 | `builder/tests.rs` | `default_inc_recursive_send_is_true_under_...` | Remove `cfg`, rename |
