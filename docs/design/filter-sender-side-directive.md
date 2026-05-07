# Sender-side / receiver-side filter directives

Design for consolidating the `s` (sender-only) and `r` (receiver-only) filter
modifiers behind a single `Side` enum and for documenting where each side
takes effect: at filter-list send time on the sender, versus at evaluation
time on the receiver. This note pins down the type-level shape, the gating
sites, the wire-format invariants, and the test plan that protects them.

This is a design note. The current implementation already handles both
modifiers correctly via paired booleans; the change proposed here is a
representational refactor (boolean pair -> enum) plus a clearer division
of responsibilities between "elide before transmit" and "skip during
evaluate". No wire bytes change.

## 1. Problem statement

Upstream rsync stores side applicability as two flag bits on `filter_rule`:

- `FILTRULE_SENDER_SIDE` (set by the `s` modifier).
- `FILTRULE_RECEIVER_SIDE` (set by the `r` modifier, and forced for
  `protect`/`risk` rules per `exclude.c:1198,1205`).

The two bits are mutually exclusive in practice. Setting both is equivalent
to "applies to both sides" (the default), which is the same as setting
neither. Upstream's `send_rules()` at `exclude.c:1605-1612` uses the bits
to decide whether a rule survives the wire crossing:

```c
if (ent->rflags & FILTRULE_SENDER_SIDE)
    elide = am_sender ? LOCAL_RULE : REMOTE_RULE;
if (ent->rflags & FILTRULE_RECEIVER_SIDE)
    elide = elide ? 0 : am_sender ? REMOTE_RULE : LOCAL_RULE;
```

oc-rsync currently stores the same information in three places, all as
paired `bool`s:

- `crates/filters/src/rule.rs::FilterRule` -- `applies_to_sender`,
  `applies_to_receiver` (lines 328-358 of the file).
- `crates/core/src/client/config/filters.rs::FilterRuleSpec` -- the
  client-facing spec, also a paired bool (lines 28-29, 196-203).
- `crates/protocol/src/filters/wire.rs::FilterRuleWireFormat` --
  `sender_side`, `receiver_side` on the wire-format struct (lines 84-86).

Three independent representations of the same two-bit fact. Each call
site that copies rules from one layer to the next must remember to copy
both bits in lockstep (`flags.rs:161-162`, `flags.rs:180-181`,
`generator/filters.rs:197,219`, `chain.rs:165-167`,
`merge/parse.rs:158-162`). A single missed copy silently flips the
applicability of a rule.

Two structural problems follow:

1. The state space is not enforced. A `FilterRule` with
   `applies_to_sender=false, applies_to_receiver=false` is constructible,
   exists in tests (`compiled/clear.rs:77-78`), and means "matches
   nothing". The current evaluator handles this case, but nothing in the
   type system says it must.
2. The two gating responsibilities are not separated by name. A reader
   of `flags.rs` cannot tell at a glance whether `sender_side` controls
   "send this rule over the wire" or "evaluate this rule on the sending
   side". The answer is "both, depending on whose perspective the field
   is read from", which is exactly the ambiguity that produces copy-bugs.

## 2. Goal

Replace the three paired-bool representations with a single `Side` enum
that carries the same information by construction:

```rust
/// Which side of the transfer a filter rule applies to.
///
/// upstream: exclude.c FILTRULE_SENDER_SIDE / FILTRULE_RECEIVER_SIDE.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Side {
    /// Default. Applies to sender and receiver. No `s` or `r` modifier.
    #[default]
    Both,
    /// `s` modifier. Applies on the sending side only.
    SenderOnly,
    /// `r` modifier. Applies on the receiving side only. Forced for
    /// `protect`/`risk` rules per upstream `exclude.c:1198,1205`.
    ReceiverOnly,
}
```

`Side` is a closed set of three states; the unrepresentable
"neither side" combination disappears from the API surface. The two
gating responsibilities become explicit: "send-time gate" and
"evaluate-time gate".

## 3. Scope

### 3.1 In scope

- New `Side` enum in `crates/filters/src/rule.rs`.
- `FilterRule::side` accessor and `with_side(Side)` setter, replacing
  the paired `applies_to_sender` / `applies_to_receiver` booleans.
- Mirror `Side` in `FilterRuleSpec`
  (`crates/core/src/client/config/filters.rs`) and in
  `FilterRuleWireFormat` (`crates/protocol/src/filters/wire.rs`).
- Parsing path: `crates/filters/src/merge/parse.rs::parse_modifiers`
  collapses the paired `sender_only` / `receiver_only` booleans on
  `RuleModifiers` into a `Side` (with the same conflict-resolution rules
  as today: simultaneous `s` and `r` produce `Side::Both`, matching
  upstream behaviour where both bits set is equivalent to neither).
- Send-time gate: a single function
  `protocol::filters::should_elide(side, am_sender)` returning the
  upstream `LOCAL_RULE` / `REMOTE_RULE` / "transmit" verdict, called
  once at the wire-format layer.
- Evaluate-time gate: `decision::iter_applicable(side, role)` filters
  the compiled rule list by `Role::Sender` or `Role::Receiver` before
  the chain-of-responsibility walk in
  `crates/filters/src/decision.rs::decision`.
- Deprecation aliases: keep `applies_to_sender()` / `applies_to_receiver()`
  as `#[doc(hidden)]` `const` accessors that compute their answer from
  `Side`. They have callers across two crates and several call sites in
  third-party tests; immediate removal is out of scope for this design.

### 3.2 Out of scope

- The `delete_excluded` interaction at `exclude.c:1609-1612`. That
  branch promotes plain rules to receiver-elided when the sender has
  `--delete-excluded`; it is orthogonal to the `s` / `r` modifiers and
  is handled elsewhere in this codebase
  (`flags.rs::build_wire_format_rules` does not yet account for it; see
  the note at `flags.rs:130-196`). A separate design tracks that gap.
- Daemon-side filter injection. The daemon assembles its own rule chain
  before any client filters arrive
  (`crates/daemon/src/daemon/sections/module_access/helpers.rs:223-280`).
  Daemon rules pass through the same `Side` plumbing once the refactor
  lands; no daemon-specific representation is introduced here.
- xattr-only (`x`) modifier interaction. `x` is orthogonal to side
  applicability and short-circuits before compilation
  (`compiled/mod.rs:48`).
- `protect` / `risk` forced-receiver semantics. Already handled at the
  wire-prefix layer (`prefix.rs:151-154`); the refactor preserves the
  forcing logic byte-for-byte, no behaviour change.

## 4. Type-level shape

### 4.1 `FilterRule` (filters crate)

```rust
pub struct FilterRule {
    action: FilterAction,
    pattern: String,
    side: Side,                  // was: applies_to_sender, applies_to_receiver
    perishable: bool,
    xattr_only: bool,
    negate: bool,
    exclude_only: bool,
    no_inherit: bool,
}

impl FilterRule {
    pub const fn side(&self) -> Side { self.side }
    pub const fn with_side(mut self, side: Side) -> Self {
        self.side = side; self
    }

    // Deprecated, doc(hidden), kept for migration:
    #[doc(hidden)]
    pub const fn applies_to_sender(&self) -> bool {
        matches!(self.side, Side::Both | Side::SenderOnly)
    }
    #[doc(hidden)]
    pub const fn applies_to_receiver(&self) -> bool {
        matches!(self.side, Side::Both | Side::ReceiverOnly)
    }
}
```

The `with_sides(sender: bool, receiver: bool)` constructor used by the
parsers becomes a thin adapter:

```rust
const fn with_sides(self, sender: bool, receiver: bool) -> Self {
    let side = match (sender, receiver) {
        (true,  true)  | (false, false) => Side::Both,
        (true,  false) => Side::SenderOnly,
        (false, true)  => Side::ReceiverOnly,
    };
    self.with_side(side)
}
```

Today `(false, false)` is reachable only via `Clear` directive
construction in `compiled/clear.rs:77-78`, where the test asserts
"clear directive applies to neither side". The semantics there is
"clear strips side modifiers from earlier rules", not "clear matches
nothing on its own"; this design folds it into `Side::Both` (clear
applies on both sides) since the clear logic at
`compiled/clear.rs::apply_clear` is where the bits are actually
zeroed for *other* rules, not for the clear itself. That test moves
to assert `clear.side() == Side::Both` and a separate assertion that
the cleared targets have their sides reset.

### 4.2 `FilterRuleSpec` (core crate)

Same `Side` field with the same accessors. The current
`with_sender(bool)` / `with_receiver(bool)` builder methods on
`FilterRuleSpec` (config/filters.rs:226-264) become a single
`with_side(Side)` plus the existing `with_sides(bool, bool)` adapter.

### 4.3 `FilterRuleWireFormat` (protocol crate)

```rust
pub struct FilterRuleWireFormat {
    rule_type: RuleType,
    pattern: String,
    anchored: bool,
    directory_only: bool,
    no_inherit: bool,
    cvs_exclude: bool,
    word_split: bool,
    exclude_from_merge: bool,
    xattr_only: bool,
    side: Side,           // was: sender_side, receiver_side
    perishable: bool,
    negate: bool,
}
```

The wire-format struct gets `Side` directly so the `flags.rs::build_wire_format_rules`
copy at `flags.rs:161-162,180-181` becomes one assignment instead of
two. The wire encoder at `prefix.rs:147-155` already needs to know
whether `protect`/`risk` forces receiver-only; that logic is folded
into a helper:

```rust
fn effective_side(rule: &FilterRuleWireFormat) -> Side {
    match rule.rule_type {
        RuleType::Protect | RuleType::Risk => match rule.side {
            Side::SenderOnly => Side::Both,        // upstream forces 'r'
            other => match other {
                Side::Both => Side::ReceiverOnly,  // forced
                Side::ReceiverOnly => Side::ReceiverOnly,
                Side::SenderOnly => Side::Both,
            },
        },
        _ => rule.side,
    }
}
```

The match preserves upstream's `exclude.c:1569-1572` semantics where
`r` is appended for `protect`/`risk` regardless of the input modifier.

## 5. Send-time gate

The send-time gate decides whether a rule is transmitted to the peer at
all. This is the upstream `send_rules()` `elide` computation at
`exclude.c:1605-1612`, evaluated by the *sending* end of the filter
list (which, for filter rules, is always the client; the server never
sends filter rules to the client).

oc-rsync evaluates this gate inside
`crates/protocol/src/filters/wire.rs::write_filter_list` via a new
private function:

```rust
fn should_transmit(side: Side, am_sender: bool) -> Transmit {
    match (side, am_sender) {
        (Side::Both, _) => Transmit::Yes,
        (Side::SenderOnly, true)  => Transmit::Local,   // keep, do not send
        (Side::SenderOnly, false) => Transmit::Remote,  // send only
        (Side::ReceiverOnly, true)  => Transmit::Remote,
        (Side::ReceiverOnly, false) => Transmit::Local,
    }
}

enum Transmit {
    Yes,     // emit on the wire AND keep locally
    Local,   // keep locally, do not transmit
    Remote,  // transmit, do not retain locally
}
```

This collapses the three-way decision upstream encodes as
`elide = LOCAL_RULE | REMOTE_RULE | 0` into a typed enum.

`should_transmit` is called once per rule at the wire-format boundary.
Rules tagged `Transmit::Local` are filtered out of the wire iterator
before encoding; rules tagged `Transmit::Remote` are encoded but
removed from the post-send local rule list (matching upstream's
`ent->elide = REMOTE_RULE` behaviour at `exclude.c:1614`).

The gate runs *before* the `RuleType::Protect|Risk` forced-receiver
adjustment, because upstream's `send_rules()` reads `FILTRULE_SENDER_SIDE`
and `FILTRULE_RECEIVER_SIDE` directly off the rule, not the prefix
encoder's view of them. Forced-receiver only affects what bytes go on
the wire; it does not change which rules are sent at all.

## 6. Evaluate-time gate

The evaluate-time gate decides whether a *retained* rule fires for a
given path on the local side. This is what
`crates/filters/src/decision.rs::decision` does today via the
`applies_to_sender` / `applies_to_receiver` predicates passed to
`first_matching_rule`. The refactor replaces those predicates with a
single role-aware filter:

```rust
pub enum Role { Sender, Receiver }

fn rule_applies(side: Side, role: Role) -> bool {
    match (side, role) {
        (Side::Both, _) => true,
        (Side::SenderOnly, Role::Sender) => true,
        (Side::ReceiverOnly, Role::Receiver) => true,
        _ => false,
    }
}
```

`decision::decide_for_role(role, path)` calls
`first_matching_rule(rules, |r| rule_applies(r.side, role) && r.matches(path))`
and the rest of the chain logic stays identical. The free
predicates `|rule| rule.applies_to_sender` and
`|rule| rule.applies_to_receiver` at
`crates/filters/src/decision.rs:40,47,57,80,87,121` collapse to one
parameterised predicate.

The two gates are independent. A `Side::SenderOnly` rule on the
*sender's* side passes both gates: it is retained (Send-time
`Transmit::Local`) and it fires (Evaluate-time `rule_applies` true).
A `Side::SenderOnly` rule on the *receiver's* side is gated out at
send time on the sender, never reaches the receiver, and so the
receiver's evaluate-time gate never sees it -- but if the receiver is
the local end and a `Side::SenderOnly` rule somehow appears in its
local list (only via local-only filter sources, never via wire), the
evaluate gate still rejects it.

## 7. Wire format invariants

The refactor changes no wire bytes. The encoder at
`prefix.rs::build_rule_prefix` reads `Side` instead of two booleans and
emits the same modifier characters under the same protocol-version
guards (`s` for `Side::SenderOnly`, `r` for `Side::ReceiverOnly` or
forced-receiver, both gated by
`protocol.supports_sender_receiver_modifiers()` which corresponds to
upstream's `protocol_version >= 29` check at `exclude.c:1567,1570`).

Specifically:

- A rule with `Side::Both` emits no `s` and no `r`.
- A rule with `Side::SenderOnly` emits exactly `s`.
- A rule with `Side::ReceiverOnly` emits exactly `r`.
- `Protect`/`Risk` rules emit `r` regardless of `Side`, matching
  upstream's `exclude.c:1198,1205` forced-receiver and the existing
  comment in `prefix.rs:145-146`.

The protocol-version gate stays at the encoder, not at the type. A
v28 transfer with a `Side::SenderOnly` rule encodes the rule without
the `s` modifier (modifier silently dropped), exactly as upstream
does at `exclude.c:1566-1568`. The `Side` field on the rule is
unchanged; only the wire bytes differ.

## 8. Parser path

`crates/filters/src/merge/parse.rs::parse_modifiers` becomes:

```rust
pub(crate) struct RuleModifiers {
    pub(crate) negate: bool,
    pub(crate) perishable: bool,
    pub(crate) side: Side,            // was: sender_only, receiver_only
    pub(crate) xattr_only: bool,
    pub(crate) exclude_only: bool,
    pub(crate) no_inherit: bool,
    pub(crate) word_split: bool,
    pub(crate) cvs_mode: bool,
}

fn handle_char(mods: &mut RuleModifiers, ch: char) {
    match ch {
        // ...
        's' => mods.side = match mods.side {
            Side::ReceiverOnly => Side::Both,    // s+r => both
            _                  => Side::SenderOnly,
        },
        'r' => mods.side = match mods.side {
            Side::SenderOnly => Side::Both,
            _                => Side::ReceiverOnly,
        },
        // ...
    }
}
```

The conflict-resolution rule (`s` then `r` -> `Both`) matches today's
behaviour at `merge/parse.rs:158-162` and upstream's behaviour at
`exclude.c` where setting both flags is identical to setting neither
(both branches of `send_rules()`'s elide computation are skipped, and
the evaluator's side check is "applies on whichever side is asking").

The `DirMergeConfig` modifier path at `chain.rs:106-122,164-168`
mirrors the same enum collapse:

```rust
pub const fn with_side(mut self, side: Side) -> Self { self.side = side; self }
```

replacing `with_sender_only(bool)` and `with_receiver_only(bool)`.

## 9. Migration plan

Three commits, each independently green:

### Commit 1 -- introduce `Side`

Add the `Side` enum to `crates/filters/src/rule.rs` next to
`FilterAction`. Add `FilterRule::side()` and `FilterRule::with_side()`
that return / set a synthesised `Side` derived from the existing two
booleans. The pair of booleans stays as the storage; `Side` is a
view-only API on top.

This commit is a pure addition. No existing call site changes. Tests
add coverage for the four `(applies_to_sender, applies_to_receiver)`
input combinations mapping to the three `Side` variants (with both-true
and both-false collapsing to `Side::Both`).

### Commit 2 -- migrate storage

Replace the two booleans with a single `Side` field on `FilterRule`,
`FilterRuleSpec`, and `FilterRuleWireFormat`. The deprecated
`applies_to_sender()` / `applies_to_receiver()` accessors keep working
as documented in section 4.1. All existing call sites continue to
compile.

This commit touches the parsers (`merge/parse.rs`, `chain.rs`), the
core spec layer (`config/filters.rs`), and the wire layer (`wire.rs`,
`prefix.rs`, `flags.rs`). All changes are mechanical: the
`with_sides(bool, bool)` shim from section 4.1 absorbs the conversion.

### Commit 3 -- consolidate gates

Introduce `should_transmit` in
`crates/protocol/src/filters/wire.rs` and `Role` plus
`rule_applies` in `crates/filters/src/decision.rs`. Convert the four
free predicates at `decision.rs:40,47,57,80,87,121` to use the
parameterised version. Convert the wire-format iterator at
`wire.rs::write_filter_list` to call `should_transmit` once per rule.

Remove the deprecated `applies_to_sender()` / `applies_to_receiver()`
free-function callers; the methods themselves remain `#[doc(hidden)]`
because they are part of the public API contract of the `filters`
crate and removing them in this PR would be a breaking change. They
graduate to `#[deprecated]` here and are removed in the next semver
bump (tracked separately).

## 10. Test plan

The refactor preserves wire bytes; the test plan therefore covers two
axes:

### 10.1 Golden wire format

`crates/protocol/tests/golden/filter_list/` already contains
golden-byte files for filter-list encoding. Three new golden files
exercise the `Side` paths:

- `side_both.golden` -- one `- *.tmp` rule with `Side::Both`. Bytes
  identical to today's encoding for an unmodified exclude rule.
- `side_sender_only.golden` -- one `- *.tmp` rule with
  `Side::SenderOnly` under protocol 32. Encoding `[0x07] -s *.tmp\0`
  (length prefix in the upstream LE-int format, matching
  `wire.rs::write_filter_list`).
- `side_receiver_only.golden` -- one `- *.tmp` rule with
  `Side::ReceiverOnly` under protocol 32. Encoding
  `[0x07] -r *.tmp\0`.

Plus three protocol-28 golden files to confirm the modifier is
silently dropped under `protocol_version < 29`:

- `side_sender_only_v28.golden` -- bytes `[0x06] - *.tmp\0`.
- `side_receiver_only_v28.golden` -- bytes `[0x06] - *.tmp\0`.
- `side_protect_v28.golden` -- a `protect` rule under v28 verifies
  `protect`/`risk` forced-receiver also drops at v28 (forced or not).

The golden test harness lives at
`crates/protocol/tests/filter_list_golden.rs` (existing) and uses
`assert_eq` against pre-computed byte vectors. The three new files
add to that test's corpus without modifying its harness.

### 10.2 Evaluate-time gate parity

`crates/filters/src/decision.rs::decision` already has unit tests at
`crates/filters/src/tests.rs:214-261` covering sender-only, receiver-only,
and the protect/risk forced-receiver edge cases. The refactor reuses
those tests verbatim; the test bodies still call
`set.allows(...)` and `set.allows_deletion(...)`, which dispatch
internally through `Role::Sender` / `Role::Receiver`. Test coverage
of the three `Side` variants comes for free.

A new property test in
`crates/filters/tests/proptest_rule_evaluation.rs` asserts the
representation invariant:

```text
forall rule, role:
    decision_using_side(rule.side, role, path)
    == decision_using_bools(rule.applies_to_sender, rule.applies_to_receiver, role, path)
```

verifying that the deprecated boolean accessors are bit-equivalent to
the `Side` enum for the three legal states. The fourth "neither side"
state (both bools false) is exercised separately by a unit test that
asserts both representations agree on "rule never fires".

### 10.3 Send-time gate parity

A new unit test in
`crates/protocol/tests/filter_list_wire.rs` exercises
`should_transmit` against the upstream `send_rules()` decision table:

| `Side` | `am_sender` | Expected |
|--------|-------------|----------|
| `Both` | true | `Transmit::Yes` |
| `Both` | false | `Transmit::Yes` |
| `SenderOnly` | true | `Transmit::Local` |
| `SenderOnly` | false | `Transmit::Remote` |
| `ReceiverOnly` | true | `Transmit::Remote` |
| `ReceiverOnly` | false | `Transmit::Local` |

Plus a higher-level test that constructs a `FilterRuleWireFormat`
list with a mix of all three sides and asserts the wire bytes contain
exactly the rules whose verdict was `Transmit::Yes` or
`Transmit::Remote`, in original order.

### 10.4 Interop test

A new interop case in
`crates/transfer/tests/interop_filter_sides.rs` (mirroring the layout
of `crates/transfer/tests/interop_filter_*.rs`) exercises the full
client-server filter-side handshake against upstream rsync 3.4.1:

1. **Sender-only on push.** Push from oc-rsync to upstream rsyncd
   with `--filter='-s *.tmp'`. Source contains `keep.txt`, `skip.tmp`.
   Assert `skip.tmp` is excluded on the sender (oc-rsync), the
   filter rule is *not* sent to the receiver (upstream rsyncd never
   sees `*.tmp`), and `keep.txt` lands at the destination. Verify
   via tcpdump capture on loopback that the wire payload after
   filter-list encoding contains zero filter rule bytes for `*.tmp`.
2. **Receiver-only on push.** Same setup with
   `--filter='-r *.tmp'`. Assert the filter is sent to the receiver
   (upstream rsyncd) and excludes `skip.tmp` on the receiver side,
   while the sender (oc-rsync) does not exclude it. Both endpoints
   end with `keep.txt` only.
3. **Sender-only on pull.** Pull from upstream rsyncd to oc-rsync
   with `--filter='-s *.tmp'`. Assert the rule fires on oc-rsync
   (the sender from oc-rsync's view of the wire is the daemon, but
   the *client* applies sender-only rules on its own local view of
   the file list -- this exercises the asymmetry highlighted at
   `arguments.rs:122-124` where `am_sender` from upstream's
   perspective is `!is_sender` from oc-rsync's). Wire capture
   confirms the rule is transmitted (because oc-rsync is the
   receiver in protocol terms) but elided locally on the daemon
   side (because the daemon is the sender in protocol terms).
4. **Mixed side rules.** A filter list combining `-s sender_skip.tmp`,
   `-r recv_skip.tmp`, and `- both_skip.tmp`. Assert each rule
   fires on the correct side, verified by separate file fixtures
   per side.
5. **Protect/risk forced-receiver.** A `P keep_dir/` rule (protect)
   under `--delete`. Assert the rule reaches the receiver as
   `[len] -r keep_dir/\0` (forced receiver-side) and prevents
   deletion of `keep_dir/` on the receiver, matching upstream's
   `exclude.c:1198`.

The harness uses the existing `tools/ci/run_interop.sh` daemon
scaffolding plus `scripts/rsync-interop-server.sh`. Wire captures
use the loopback tcpdump pattern documented in
`docs/design/protocol-capture-replay-harness.md` (sibling design
note).

### 10.5 Property test for wire-bytes invariance

A `proptest` in
`crates/protocol/tests/proptest_filter_side_invariance.rs` asserts
that for every legal `Side` and protocol version, the encoded bytes
of a `FilterRuleWireFormat` constructed via the new `Side` field
equal the bytes of the same rule constructed via the deprecated
`with_sides(bool, bool)` builder. This locks down the migration
contract: the boolean-pair API and the enum API produce
byte-identical output.

## 11. Wire-compat invariants

- No new wire bytes.
- No removed wire bytes.
- Modifier prefix order unchanged. `s` and `r` continue to follow `x`
  and precede `p` per `prefix.rs:147-159`, matching upstream's
  `get_rule_prefix()` ordering at `exclude.c:1564-1572`.
- Protocol-version gating unchanged. v28 still drops both modifiers;
  v29+ emits them.
- Forced-receiver semantics for `protect`/`risk` unchanged.

## 12. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Test coverage gap on the `(false, false)` boolean state | The migration-period property test in section 10.2 explicitly covers it; the unit test asserts both boolean and enum representations agree on "never matches". |
| External crates depending on `applies_to_sender()` / `applies_to_receiver()` accessors | Keep them `#[doc(hidden)]` and behaviour-equivalent for one release cycle; remove in the next semver bump with a CHANGELOG entry. |
| Inconsistent migration of the three layers | Commits 1-3 in section 9 enforce ordering; CI runs the full nextest suite after each commit. The wire-format golden tests guard the boundary. |
| Wire-byte drift introduced by the refactor | Six golden files (section 10.1) plus the property test (section 10.5) lock the bytes. |
| Upstream behaviour drift on `protect`/`risk` forced-receiver | Section 4.3's `effective_side` helper preserves the forcing logic in one place; existing test at `prefix.rs:320-338` asserts the bytes. |
| Daemon-side filter chain mistakenly receives client-side `Side` semantics | `build_daemon_filter_rules` runs before client filters arrive and never crosses the wire boundary; this design changes only the client path. The daemon path uses the same `FilterRule` type and inherits `Side::Both` for all daemon-injected rules (matching today's behaviour at `helpers.rs:223-280`). |

## 13. Open questions

- Should the public API expose `Side` directly, or wrap it in a
  newtype to allow future extension (e.g., a hypothetical
  "third-party" side for proxied transfers)? Recommendation: expose
  `Side` directly. The three states map 1:1 to upstream's two flag
  bits and there is no realistic fourth state on the wire.
- Should `FilterRule::with_sides(true, true)` and
  `with_sides(false, false)` continue to compile during the migration
  window, or be removed in commit 2? Recommendation: keep both during
  the migration window; remove only in the same commit that removes
  `applies_to_sender()` / `applies_to_receiver()`. Callers who pass
  `(false, false)` today are presumably intentional (clear-rule
  construction) and should migrate in a single follow-up.
- Is the wire-bytes invariance property test (section 10.5) needed
  given the golden files (section 10.1)? The property test catches
  any rule type the goldens forgot. Recommendation: yes, both. The
  goldens are the regression net; the property test is the
  exhaustiveness net.

## 14. Tracking (follow-up TODOs, not added to the persistent list)

The implementation work breaks into three merge-sized PRs as
described in section 9. Each PR cites this design note in its
description. None of the three PRs introduce wire-protocol
extensions; all are representational refactors plus test additions.

A fourth follow-up PR (post-deprecation window) removes the
`applies_to_sender()` / `applies_to_receiver()` accessors entirely
and bumps the `filters` crate semver minor. That removal is gated on
no internal callers remaining, verified by a workspace-wide grep
across crates and tests.

## 15. Success criteria

The refactor is complete when:

- `Side` is the sole representation of side applicability in
  `FilterRule`, `FilterRuleSpec`, and `FilterRuleWireFormat`.
- The send-time gate `should_transmit` is the sole site that decides
  whether a filter rule is transmitted; the evaluate-time gate
  `rule_applies(side, role)` is the sole site that decides whether
  a retained rule fires.
- All six wire-format golden files (section 10.1) pass on every CI
  platform.
- The interop suite (section 10.4) passes against upstream rsync
  3.0.9, 3.1.3, and 3.4.1 in both push and pull directions.
- The wire-bytes invariance property test (section 10.5) runs 1,024
  iterations per CI build with zero failures.
- No `applies_to_sender` or `applies_to_receiver` storage field
  remains in any production crate; only the `#[doc(hidden)]`
  view-only accessors persist.

These criteria establish that the refactor is purely representational
and that wire compatibility with upstream rsync is preserved
byte-for-byte across the three protocol versions tested in CI.
