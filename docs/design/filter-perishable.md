# Filter rule perishable annotation

Internal design note for issue #2126. Documents the `perishable` rule
modifier (`p`), the parsing and storage layout, and the gating contract
that keeps perishable rules out of the delete-walk path. No upstream C
references are needed beyond the `exclude.c` modifier table that the
parser already cites; this is a Rust-side surface design.

This is a docs-only change. The runtime field already exists; this note
fixes the design of record so future work (extending the modifier set,
moving the gate into the filter program) does not regress the behaviour.

## Problem

Rsync filter rules can be marked perishable with the `p` modifier
(`-p *.log`). A perishable rule still applies to the transfer phase, but
the delete-walk on the receiver MUST ignore it: an exclude rule that is
perishable does not promote the matching destination paths into the
"safe to delete" set, and does not protect them from a `--delete` sweep
that would otherwise have removed them.

The risk if perishable is treated as a normal rule during deletion:

- A user-supplied `-p .git/` exclude would block the transfer of `.git/`
  and also accidentally protect the destination `.git/` directory from
  a sync that the user expected to clean up.
- Auto-generated rules from `:` dir-merge files (which mark themselves
  perishable, see `chain.rs:161`) would leak into deletion semantics
  and tie the delete-walk to per-directory state that may not exist on
  the receiver.

The design must therefore route perishable rules through transfer
evaluation while excluding them from deletion evaluation, with no leak
between the two contexts.

## Where rule modifiers are parsed today

`crates/filters/src/merge/parse.rs` is the single parsing site:

- `RuleModifiers` (lines 135-146) holds one boolean per modifier:
  `negate`, `perishable`, `sender_only`, `receiver_only`, `xattr_only`,
  `exclude_only`, `no_inherit`, `word_split`, `cvs_mode`.
- `parse_modifiers()` (lines 174-199) consumes single-character
  modifiers between the action character (`+`/`-`/`P`/`R`/`H`/`S`/`.`/
  `:`) and the pattern, terminating on whitespace, `_`, or any
  unrecognised character.
- `RuleModifiers::apply()` (lines 150-165) folds the parsed modifiers
  into a `FilterRule` via the builder methods on `FilterRule`
  (`with_negate`, `with_perishable`, `with_xattr_only`,
  `with_exclude_only`, `with_no_inherit`, plus side flags).

`FilterRule` itself lives in `crates/filters/src/rule.rs` and already
carries the `perishable: bool` field (line 63), with constructors
defaulting it to `false` and a `with_perishable()` builder method
(lines 361-365). The compiled-rule layer (`crates/filters/src/compiled/
mod.rs:35-99`) propagates `perishable` from `FilterRule` into
`CompiledRule` so the runtime evaluator can see it without re-parsing.

`DirMergeConfig` (in `crates/filters/src/chain.rs`) also exposes a
`perishable` toggle (lines 63-134) that marks every rule loaded from a
particular per-directory merge file as perishable. This is how
`.rsync-filter` rules become perishable by default - the dir-merge
config that owns them sets the bit during `apply_modifiers()` at
`chain.rs:157-162`.

## Gating contract

The single gate is in `crates/filters/src/decision.rs`:

- `first_matching_rule(..., include_perishable: bool)` (lines 150-163)
  walks the compiled chain and accepts a rule only when
  `include_perishable || !rule.perishable`. Two callers per evaluation:
  one for the include/exclude chain, one for the protect/risk chain.
- `FilterSetInner::decision()` (lines 27-105) selects
  `include_perishable=true` for `DecisionContext::Transfer` and
  `include_perishable=false` for `DecisionContext::Deletion`. The
  `--delete-excluded` lookup at lines 52-62 explicitly re-runs the
  scan with `include_perishable=true` so the delete-excluded promotion
  uses the same rule set as transfer.
- `DecisionContext` is a private enum (lines 170-174) so the
  perishable axis is not part of the public API; callers select the
  context indirectly via `FilterSet::allows_transfer()` vs
  `FilterSet::allows_deletion()` / `allows_deletion_when_excluded_removed()`.

The chain pattern is "first match wins"; perishable filtering happens
inside the predicate so a perishable rule never preempts a
non-perishable rule that comes after it during deletion.

### Delete-walk consumer sites

The delete-walk reaches the gate via the engine's filter facade:

- `crates/engine/src/local_copy/context_impl/options.rs:477-505` -
  `LocalCopyContext::allows_deletion()` is the single call from the
  delete-walk into the filter layer. It dispatches to either the
  filter-program path (`FilterContext::Deletion`) or the legacy
  `FilterSet::allows_deletion()` path; both ultimately call
  `FilterSetInner::decision()` with `DecisionContext::Deletion`.
- Callers of `context.allows_deletion(...)` are the actual delete-walk
  loops:
  - `crates/engine/src/local_copy/executor/cleanup.rs:102` - top-level
    extraneous-entry removal during `--delete`.
  - `crates/engine/src/local_copy/executor/cleanup.rs:255` - recursive
    descent into directories scheduled for deletion.
  - `crates/engine/src/local_copy/executor/sources/orchestration.rs:345`
    - per-source delete pass.

These three sites are the only ones that need to honour the perishable
gate, and they already do so transitively. New delete-walk paths must
go through `LocalCopyContext::allows_deletion()` and never inspect
`CompiledRule::perishable` directly.

## Design

### Field layout

`FilterRule.perishable: bool` is the canonical field. It is:

- A first-class field on the public type (already shipped) so the
  builder API (`with_perishable`) and merge parser stay symmetric.
- Propagated through `CompiledRule` (compile time) and `DirMergeConfig`
  (loader time) so the evaluator has no parser dependency.
- Never serialised over the wire. Filter rules are exchanged as their
  textual form during the protocol's filter-rule exchange phase, and
  the receiver re-parses them, recovering the `p` bit from the
  modifier string. See "Wire interaction" below.

### Parser surface

The `p` modifier is recognised in two places:

1. `parse_modifiers()` for short-form rules (`-p pattern`,
   `+pe pattern`, `:p .rsync-filter`). Already implemented.
2. The long-form keyword path (`include`, `exclude`, ...) does NOT
   accept modifiers - upstream behaviour. `try_parse_long_form()` in
   `parse.rs:279-301` therefore never produces a perishable rule, and
   intentionally so.

For dir-merge rules, the perishable bit can come from either the
modifier string on the `:` line itself or from `DirMergeConfig::with_perishable(true)`.
When both are set, the rule stays perishable; the bit is monotone (no
"unset" path), which matches upstream's "promotion only" semantic.

### Evaluator surface

The two-path read at the decision site is the entire mechanism:

```text
DecisionContext::Transfer  -> include_perishable=true
DecisionContext::Deletion  -> include_perishable=false
DecisionContext::Deletion + --delete-excluded scan -> include_perishable=true
```

The third row is the subtle one: `--delete-excluded` deletes destination
files that the user-supplied filters would have excluded from transfer.
That promotion uses transfer-style semantics (perishable rules count),
and its scan in `FilterSetInner::decision()` runs with
`include_perishable=true` even though the outer context is `Deletion`.

### Why the gate is at the rule walk, not the rule list

An alternative is to maintain two compiled chains - one with all rules
and one with perishable rules filtered out - and pick the chain by
context. Rejected because:

- The chain length is small (typically a handful of rules) so the
  per-rule predicate cost is negligible.
- Rule order between perishable and non-perishable rules matters for
  first-match-wins semantics. Splitting chains would force re-merging
  by index at every evaluation, which is more code than the predicate.
- The protect/risk chain is independent and would need the same split,
  doubling the data structure.

Keeping the gate in `first_matching_rule()` keeps the data path simple
and the test surface small.

## Edge cases

### Perishable + `--delete-excluded`

The interaction between `p` and `--delete-excluded` is the load-bearing
case. The contract:

| Rule | Mode | Path matches? | Result |
|------|------|---------------|--------|
| `-p *.log` | `--delete` only | yes | NOT deleted: perishable rule does not protect, and without `--delete-excluded` we only delete missing-from-source paths. The exclude is invisible to the delete-walk, so `*.log` files on the receiver are left alone (treated as "not excluded" for deletion purposes). |
| `-p *.log` | `--delete --delete-excluded` | yes | DELETED: the `--delete-excluded` scan re-runs with `include_perishable=true`, finds the exclude, and `allows_deletion_when_excluded_removed()` returns true. |
| `-  *.log` (no `p`) | `--delete` only | yes | NOT deleted: same as perishable case; receiver-side excludes don't drive deletion without `--delete-excluded`. |
| `-  *.log` (no `p`) | `--delete --delete-excluded` | yes | DELETED: the standard scan finds the exclude. |
| `P *.log` | any delete mode | yes | NOT deleted: protect rule wins (perishable does not apply to protect; protect rules are evaluated as `Risk`/`Protect` regardless of perishable flag). |
| `-p *.log` followed by `+ /keep.log` | `--delete --delete-excluded` | `/keep.log` | NOT deleted: the include comes first in the standard scan and the perishable exclude is suppressed in the deletion scan, so neither path treats `/keep.log` as excluded. |

The first row is the entire reason the modifier exists: a user can mark
an exclude rule as perishable to say "skip these files in the transfer,
but do not let this rule influence deletion at all."

### Perishable on protect/risk rules

Upstream syntax allows `Pp pattern` and `Rp pattern`. The current parser
applies the perishable bit to whatever rule the action character builds,
including protect/risk. The evaluator then sees `rule.perishable=true`
on the protect/risk chain. The gate suppresses these rules during the
deletion scan, which is the intended behaviour: a perishable protect
rule means "transfer-time hint only, do not influence deletion." The
`Transfer` context still sees protect/risk rules; this is consistent
with how upstream evaluates them but rare in practice.

### Perishable + dir-merge

Per-directory `.rsync-filter` rules are perishable by default when
`DirMergeConfig::with_perishable(true)` is set. The dir-merge program
(`crates/engine/src/local_copy/filter_program`) layers per-directory
rules on top of the global chain at evaluation time. The same
`include_perishable` predicate applies inside the program-segment
walker (`segments.rs:155-180`), so dir-merge rules inherit the gate
without extra code. This is the path that makes
`.rsync-filter` rules transparent to `--delete` while still affecting
the transfer.

### Perishable + `!` clear

A `! ` clear rule is never marked perishable: `FilterRule::clear()`
constructs the rule with `perishable: false` (rule.rs:191-202). Clears
must always run during both transfer and deletion evaluation -
otherwise a perishable clear could leave stale rules visible in the
deletion chain. This is encoded by construction; the parser path for
`!`/`clear` never reaches `parse_modifiers()`.

### Negation interaction

`!` modifier (negate) and `p` are independent. `-!p *.log` builds a rule
with `negate=true, perishable=true`. The negation is applied inside
`CompiledRule::matches()`; the perishable gate is applied earlier in
`first_matching_rule()`. Neither short-circuits the other: a negated
perishable rule is suppressed during deletion just like a non-negated
perishable rule.

## Wire interaction

Filter rules cross the wire as text during the protocol's filter-rule
exchange. The receiver re-parses them with the same merge parser, so
the perishable bit survives transit by virtue of the `p` modifier
character being reproduced in the textual form. Two rules:

- The serialiser (golden test fixtures live in
  `crates/protocol/tests/golden/`) MUST emit `p` whenever
  `rule.perishable && supports_modifiers(rule.action)`. The current
  serialiser does this; the test plan below pins it down.
- Long-form keywords (`include pattern`) cannot carry modifiers so
  perishable rules MUST be serialised in short form. The short-form
  path is already the default for all generated rules; long form is
  only accepted on read.

## Test plan

### Unit tests (already in tree, retained as regression anchors)

- `crates/filters/src/merge/tests.rs::parse_perishable_modifier` -
  covers `-p`, `+p`, and combined modifier orderings.
- `crates/filters/src/decision.rs` evaluator tests cover
  `DecisionContext::Transfer` vs `Deletion` outcomes and the
  `--delete-excluded` promotion path.
- `crates/filters/src/chain.rs::dir_merge_config_perishable` covers
  the `DirMergeConfig::with_perishable(true)` path.

### New tests required for #2126

1. **Golden filter exchange test.** Encode a filter list containing
   `-p *.log`, `-!p *.tmp`, `:p .rsync-filter`, send it through the
   filter-rule exchange serialiser, decode, and assert each decoded
   rule preserves `is_perishable() == true`. Lives in
   `crates/protocol/tests/golden/filter_exchange_perishable.rs` with a
   byte-level golden file under `crates/protocol/tests/golden/data/`.

2. **Delete-walk behaviour test.** End-to-end test in
   `crates/engine/tests/delete_perishable.rs`:
   - Source contains `a.log`, `b.txt`. Receiver contains `a.log`,
     `b.txt`, `c.log` (extraneous).
   - With `-p *.log --delete`: `c.log` MUST be deleted (exclude is
     perishable so deletion does not see it as excluded; `c.log` is
     still extraneous w.r.t. the file list, so it is removed).
   - With `-p *.log --delete --delete-excluded`: `c.log` AND `a.log`
     deleted from the receiver. The perishable rule re-emerges in the
     `--delete-excluded` scan and promotes `a.log` to deletable.
   - With `- *.log --delete` (no `p`): same as the perishable case
     for this scenario; receiver-side exclude does not drive deletion
     without `--delete-excluded`. Confirms the perishable bit is not
     a behaviour change in `--delete`-only mode.
   - With `- *.log --delete --delete-excluded` (no `p`): same as the
     perishable + `--delete-excluded` case. Confirms parity.

3. **Protect interaction test.** Append to
   `crates/filters/src/tests.rs` (next to
   `delete_excluded_only_removes_excluded_matches`): a rule sequence
   `Pp /keep`, `- *` and assert `keep` is not deleted under any
   `--delete` mode. Pins the "perishable protect is suppressed during
   deletion, but the underlying transfer still treats it correctly"
   contract.

4. **Property test.** In `crates/filters/proptest/`, generate random
   rule lists with random `perishable` bits and random paths. Assert
   for every path:
   - `Transfer` context decision is invariant under flipping the
     perishable bit (perishable bit is read-only at transfer time).
   - `Deletion` context decision is monotone: setting more rules
     perishable can only weaken deletion (it never DELETES MORE).
   - `Deletion + --delete-excluded` re-introduces deleted-by-exclude
     paths exactly when the corresponding rule's perishable bit is
     respected by the secondary scan.

5. **Modifier-order fuzz target.** Extend
   `crates/filters/fuzz/fuzz_targets/fuzz_filter_chain.rs` to vary
   modifier orderings (`-p!`, `-!p`, `-pen`, ...) and assert parser
   acceptance and runtime semantics agree with a model implementation.

### Coverage acceptance

Per workspace policy (`> 95%` line coverage), `cargo llvm-cov` over the
filters crate must show the perishable predicate branch
(`include_perishable || !rule.perishable`) hit by both arms in the
unit test suite. The parser modifier table must show all entries
exercised. The new delete-walk test pulls coverage on
`LocalCopyContext::allows_deletion()` and the cleanup loops.

## Open questions

- Should `DecisionContext` move from `pub(crate)` to `pub` so external
  callers can request a specific context? Current answer: no - the
  public `allows_deletion()` / `allows_transfer()` methods are the
  intended surface and adding a third method
  `allows_deletion_when_excluded_removed()` already covers the
  `--delete-excluded` branch.
- Should `FilterSet::compile()` reject combinations that upstream
  rejects (e.g., `Pp` is technically valid but rare)? Current answer:
  no - upstream accepts the combination silently and so do we.
  Document the behaviour, do not fail compilation.
