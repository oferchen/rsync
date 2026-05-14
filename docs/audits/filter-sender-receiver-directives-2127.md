# Audit: filter-rule sender (`s`) and receiver (`r`) modifiers

Status: RESOLVED in #2127.
Upstream reference: `target/interop/upstream-src/rsync-3.4.2/exclude.c`.

## Scope

This audit verifies that oc-rsync recognises upstream rsync's `s` (sender-side)
and `r` (receiver-side) filter-rule modifiers and that the rule fires only on
the matching side at evaluation time.

## Upstream behaviour

`exclude.c` parses two rule-side flag bits on `filter_rule`:

- `FILTRULE_SENDER_SIDE` (set by the `s` modifier or by the `H`/`S` prefix at
  lines 1194-1200).
- `FILTRULE_RECEIVER_SIDE` (set by the `r` modifier or by the `P`/`R` prefix
  at lines 1201-1207).

The modifiers are suffix flags placed between the action character and the
pattern, e.g. `-r *.tmp` or `+s logs/**`. When both bits are set on the same
rule (`-sr *.tmp`), upstream's `send_rules` elide computation at lines
1605-1612 cancels them out: the rule transmits and fires on both sides, which
is identical to setting neither bit.

A side-specifying prefix (`H`, `S`, `P`, `R`) sets `prefix_specifies_side` at
parse time (lines 1199, 1206) and any subsequent `s` or `r` modifier is a
syntax error (lines 1269-1278). For example `Hr foo` is rejected.

## oc-rsync implementation map

| Concern | Location |
|---------|----------|
| Side fields on the rule struct | `crates/filters/src/rule.rs` -- `applies_to_sender`, `applies_to_receiver` |
| Builder API | `FilterRule::with_sender`, `with_receiver`, `with_sides` |
| Modifier parsing | `crates/filters/src/merge/parse.rs::parse_modifiers` (`s`, `r` chars) |
| Prefix-side validation | `crates/filters/src/merge/parse.rs::validate_side_modifiers` |
| Evaluate-time gating | `crates/filters/src/decision.rs` (`applies_to_sender`/`applies_to_receiver` predicates) |
| Wire encoding | `crates/protocol/src/filters/prefix.rs` (emits `s`/`r` under protocol >= 29) |

## Test coverage

- Unit: `crates/filters/src/merge/tests.rs` -- `parse_include_default_both_sides`,
  `parse_include_sender_only`, `parse_include_receiver_only`,
  `parse_both_side_modifiers_collapses_to_both`,
  `parse_rejects_s_modifier_on_side_specific_prefix`,
  `parse_rejects_r_modifier_on_side_specific_prefix`,
  `parse_rejects_side_modifier_with_word_split_on_side_prefix`.
- Integration: `crates/filters/tests/sender_receiver_sides.rs` covers
  show/hide, protect/risk, and mixed combinations through `FilterSet::allows`
  and `allows_deletion`.

## Wire compatibility

No wire-format changes. Encoding of the `s`/`r` modifier on the wire is
gated by `protocol_version >= 29` per `exclude.c` lines 1567 and 1570; the
oc-rsync encoder mirrors that check in `prefix.rs::build_rule_prefix`.
