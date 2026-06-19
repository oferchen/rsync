# Merge / dir-merge per-modifier coverage vs upstream (MDF-1)

Scope: every modifier accepted by upstream rsync's filter-rule parser
`exclude.c::parse_rule_tok()` for the merge-file (`.` / `merge`) and
per-directory merge-file (`:` / `dir-merge`) rule families, mapped to the
oc-rsync parse site and the existing test coverage. The upstream
`parse_filter_str()` dispatches every line through `parse_rule_tok()`, so
the modifier set is the same whether the rule comes from the command line,
an `--include-from`/`--exclude-from` file, or a merged filter file.

Upstream source of truth: `target/interop/upstream-src/rsync-3.4.1/exclude.c`.

## Upstream modifier surface

Per `parse_rule_tok()` (exclude.c lines 1092 - 1338) the parser:

1. Picks a rule kind from the first character or long keyword
   (`exclude.c:1137-1179`). Both `.` (`merge`) and `:` (`dir-merge`) land on
   the same modifier-loop. `dir-merge` additionally sets
   `FILTRULE_PERDIR_MERGE | FILTRULE_FINISH_SETUP` (lines 1181-1188).
2. Treats a comma immediately after the rule character as a permitted
   separator before modifier letters: `default: ch = *s; if (s[1] == ',')
   s++;` (lines 1174-1178). So `:,n filter` parses as `dir-merge` with
   the `n` modifier.
3. Loops over modifier characters until a space or `_` separator is hit
   (lines 1215-1289). The loop also short-circuits on whitespace when the
   parent rule is in `FILTRULE_WORD_SPLIT` mode.

The modifier letters accepted by the loop are:

| Char | Upstream flag | Notes |
|---|---|---|
| `-` | `FILTRULE_NO_PREFIXES` (exclude-only) | Only valid with `FILTRULE_MERGE_FILE`; exclude.c:1227-1231 |
| `+` | `FILTRULE_NO_PREFIXES \| FILTRULE_INCLUDE` (include-only) | Only valid with `FILTRULE_MERGE_FILE`; exclude.c:1232-1237 |
| `/` | `FILTRULE_ABS_PATH` | Anchors the pattern to the transfer root; exclude.c:1238-1240 |
| `!` | `FILTRULE_NEGATE` | Invalid on merge rules (lines 1241-1247) |
| `C` | `FILTRULE_NO_PREFIXES \| FILTRULE_WORD_SPLIT \| FILTRULE_NO_INHERIT \| FILTRULE_CVS_IGNORE` | Equivalent to `-C`; lines 1248-1255 |
| `e` | `FILTRULE_EXCLUDE_SELF` | Merge-only; lines 1256-1260 |
| `n` | `FILTRULE_NO_INHERIT` | Merge-only; lines 1261-1265 |
| `p` | `FILTRULE_PERISHABLE` | Generic; lines 1266-1268 |
| `r` | `FILTRULE_RECEIVER_SIDE` | Rejected when prefix already implies a side; lines 1269-1273 |
| `s` | `FILTRULE_SENDER_SIDE` | Same prefix guard as `r`; lines 1274-1278 |
| `w` | `FILTRULE_WORD_SPLIT` | Merge-only; lines 1279-1283 |
| `x` | `FILTRULE_XATTR` | Generic; lines 1284-1287 |

Plus the implicit rule-family selector itself: `:` for dir-merge
(`FILTRULE_PERDIR_MERGE`, line 1183) versus `.` for merge
(`FILTRULE_MERGE_FILE` only, line 1187). Both share the modifier loop.

## Per-modifier coverage

| Modifier | Upstream semantics | oc-rsync parse site | Test coverage sites | Verdict |
|---|---|---|---|---|
| `:` (dir-merge rule kind) | Sets `FILTRULE_PERDIR_MERGE \| FILTRULE_FINISH_SETUP \| FILTRULE_MERGE_FILE`; per-dir merge-files are re-parsed on every directory entry; `exclude.c:1181-1188`, `push_local_filters` `exclude.c:759-825` | Merge-file parser: `crates/filters/src/merge/parse.rs:246` (`ShortFormAction::DirMerge`), `crates/filters/src/merge/parse.rs:294` (short-form `:`), `crates/filters/src/merge/parse.rs:332` (long-form `dir-merge `). CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:143-144` (short `:`), `crates/cli/src/frontend/filter_rules/parsing/mod.rs:559,585` (long `dir-merge`). Chain expansion: `crates/filters/src/chain/mod.rs:200-241` (re-read on `enter_directory`). | `crates/filters/src/merge/tests.rs:53` (`parse_dir_merge_short`), `crates/filters/tests/dir_merge_rules.rs:142,154,194,210,236,256,270,277,285` (long-form + recursion + side-specific), `crates/filters/tests/dir_merge_parsing_comprehensive.rs:15-413` (32 scenarios), `crates/filters/src/chain/tests.rs:158-356` (chain enter/leave). | Full |
| `.` (merge rule kind) | Sets `FILTRULE_MERGE_FILE`; merged inline once at parse time; `exclude.c:1186-1188`, `parse_filter_file` `exclude.c:1455-1525` | Merge-file parser: `crates/filters/src/merge/parse.rs:245` (`ShortFormAction::Merge`), `crates/filters/src/merge/parse.rs:292` (short-form `.`), `crates/filters/src/merge/parse.rs:331` (long-form `merge `). CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:142` (short `.`), recursive expansion `crates/filters/src/merge/read.rs`. | `crates/filters/src/merge/tests.rs:45` (`parse_merge_short`), `crates/filters/tests/dir_merge_rules.rs:166-180,210-234` (short + long + recursive expansion), `crates/filters/tests/dir_merge_parsing_comprehensive.rs:116-156,210-234,282-329` (interleaving, abs paths, mixed case). | Full |
| `-` (exclude-only payload) | `FILTRULE_NO_PREFIXES` - lines in the merged file are treated as bare exclude patterns; `exclude.c:1227-1231` | Merge-file parser: not represented in `RuleModifiers` (`crates/filters/src/merge/parse.rs:211-236`); a leading `-` on a merge line is consumed as the exclude action by `try_parse_short_form` (line 287), so the merge-file parser cannot apply the upstream "force the merged file to be all-exclude" effect. CLI parser handles it for `:`/`.` directives at `crates/cli/src/frontend/filter_rules/parsing/merge.rs:28-39` and stores `DirMergeEnforcedKind::Exclude` (used by `crates/filters/src/chain/config.rs::apply_modifiers`). | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:204-208` (`parse_merge_modifiers_exclude`), `crates/cli/src/frontend/filter_rules/parsing/merge.rs:217-225` (mutual-exclusion with `+`). Merge-file parser: no test exercises `:-` or `.-` on a merged file (search `:-` in `crates/filters/tests` returns hits only as raw patterns, never as a per-dir rule modifier). | Partial - CLI surface tested; the merge-file parser does not model the `FILTRULE_NO_PREFIXES` enforcement when a nested `:`/`.` rule carries `-`. |
| `+` (include-only payload) | `FILTRULE_NO_PREFIXES \| FILTRULE_INCLUDE` - lines in the merged file are forced-include; `exclude.c:1232-1237` | Same shape as `-`. CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:40-51`. Merge-file parser: no `+` channel on `RuleModifiers`; a leading `+` is the include action, not a merge modifier. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:210-214` (`parse_merge_modifiers_include`), `217-225` (`+`/`-` exclusion), `238-241` (`+C` rejected). Merge-file parser: no test exercises `:+` or `.+` modifier path. | Partial - CLI surface tested; merge-file parser cannot stamp `FILTRULE_NO_PREFIXES \| FILTRULE_INCLUDE` on nested merge rules. |
| `/` (anchor to transfer root) | `FILTRULE_ABS_PATH` on the parent merge rule, then on every rule the merged file contributes; `exclude.c:1238-1240` | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:114-116` (sets `anchor_root`), stored on `DirMergeConfig` via `crates/filters/src/chain/config.rs:108-111` and consumed by `crates/filters/src/chain/mod.rs` when locating merged files. Merge-file parser: not in `RuleModifiers` - inside `.rsync-filter`, a leading `/` on a pattern is treated as the anchored pattern itself, which is the same wire effect. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:293-296` (`parse_merge_modifiers_anchor_root`), `crates/cli/src/frontend/filter_rules/parsing/mod.rs:604-617` (`dir_merge_leading_slash_strips_and_sets_anchor_root`), `crates/filters/src/chain/tests.rs:43-48` (`dir_merge_config_anchor_root`). | Full at the CLI/chain boundary; merge-file parser does not need a dedicated channel because the same anchor is encoded into each pattern as it is parsed. |
| `!` (negate) | `FILTRULE_NEGATE`; rejected on `FILTRULE_MERGE_FILE` parent (lines 1241-1247) | Merge-file parser: `crates/filters/src/merge/parse.rs:216` (per-rule `negate`). CLI parser: handled by the generic rule modifier set, not by `parse_merge_modifiers` (which has no `!` arm). | `crates/filters/src/merge/tests.rs:221-237,338-354` (negate on include/exclude), `crates/filters/tests/negated_rules.rs` (broad coverage). Negative case `:! file` (`!` on a merge directive) is not exercised in `parse_merge_modifiers` or `try_parse_short_form`. | Partial - happy-path tested; the upstream `goto invalid` for `!` on a merge parent has no regression test. |
| `C` (cvs-compatible bundle) | Sets `FILTRULE_NO_PREFIXES \| FILTRULE_WORD_SPLIT \| FILTRULE_NO_INHERIT \| FILTRULE_CVS_IGNORE`; conflicts with `+` / `-` / side-specific prefixes (lines 1248-1255 + 1249 guard) | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:52-71` (full bundle: whitespace, no comments, no inherit, `.cvsignore` default name). Merge-file parser: `crates/filters/src/merge/parse.rs:224` records `cvs_mode` on `RuleModifiers`, but `RuleModifiers::apply` (`parse.rs:151-167`) never propagates it to the produced rule, so a merge file containing `:C .rsync-filter` would parse but the cvs bundle would be silently dropped. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:229-241` (`parse_merge_modifiers_cvsignore`, `_with_include_error`), `crates/filters/tests/cvs_exclude.rs:155-433` and `crates/filters/tests/cvs_patterns_advanced.rs:115-...` (effective `-C` semantics via `FilterSet::from_rules_with_cvs`). Merge-file parser: `crates/filters/src/merge/tests.rs:380-391,443-447` (`parse_modifiers_all_flags`, `parse_cvs_mode_modifier`) record that the bit is recognised but assert no downstream side-effect. | Partial - CLI path is covered; the merge-file parser drops `C` on the floor. |
| `e` (exclude .rsync-filter itself) | `FILTRULE_EXCLUDE_SELF`; rejected outside `FILTRULE_MERGE_FILE` (lines 1256-1260) | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:72-86` (gated on `allow_extended`, i.e. only for `:` rules) plumbs through `DirMergeOptions::exclude_filter_file` to `DirMergeConfig::with_exclude_self` (`crates/filters/src/chain/config.rs:83`). Merge-file parser: `crates/filters/src/merge/parse.rs:221` records `exclude_only` (note the name collision: the field maps the upstream "exclude self" bit, but `RuleModifiers::apply` calls `with_exclude_only` which decorates the rule with `is_exclude_only`, not the dir-merge "exclude this filter file" effect). | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:244-252` (extended on, not on `.`), `crates/filters/src/chain/tests.rs:21-25,225-240` (`dir_merge_config_exclude_self`, `filter_chain_enter_directory_exclude_self`). Merge-file parser: `crates/filters/src/merge/tests.rs:394-399` (`parse_exclude_only_modifier`) and `crates/filters/tests/dir_merge_parsing_comprehensive.rs:60-71` (`dir_merge_exclude_only_modifier`) only assert the rule-level `is_exclude_only` decoration, which is a different feature from upstream `FILTRULE_EXCLUDE_SELF`. | Partial - CLI path is wired to the chain feature and covered. Merge-file parser conflates `e` with a per-rule "exclude-only" flag instead of with the upstream "exclude the .rsync-filter file from the transfer" semantics. |
| `n` (non-inheriting) | `FILTRULE_NO_INHERIT`; rejected outside `FILTRULE_MERGE_FILE` (lines 1261-1265). When the parent dir is re-entered, inherited rules are dropped (`push_local_filters` exclude.c:802-803). | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:87-101` (gated on `allow_extended`) drives `DirMergeOptions::inherit(false)` and `DirMergeConfig::with_inherit(false)`. Merge-file parser: `crates/filters/src/merge/parse.rs:222` records `no_inherit` on `RuleModifiers`, surfaced on the produced rule via `with_no_inherit` (`parse.rs:157`). | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:256-265` (extended on, not on `.`), `crates/filters/src/chain/tests.rs:15-19` (`dir_merge_config_no_inherit`). Merge-file parser: `crates/filters/src/merge/tests.rs:402-406` (`parse_no_inherit_modifier`), `crates/filters/tests/dir_merge_rules.rs:52-56` (`dir_merge_with_no_inherit`), `crates/filters/tests/dir_merge_parsing_comprehensive.rs:15-29,102-113` (with other modifiers, no separator). Chain pop/push behaviour: not exercised - `crates/filters/src/chain/mod.rs:212-220` does not consult `is_no_inherit` when popping per-dir scopes, so inherited rules survive the directory boundary in oc-rsync. | Partial - parsing surface is covered; chain semantics (drop inherited rules on re-entry) are unverified. |
| `p` (perishable) | `FILTRULE_PERISHABLE`; generic, applies to every rule kind (lines 1266-1268) | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:111-113` (`mark_perishable`). Merge-file parser: `crates/filters/src/merge/parse.rs:217` (per-rule `perishable`) via `with_perishable` on the produced rule. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:287-290`, `crates/filters/src/chain/tests.rs:50-55` (`dir_merge_config_perishable`). Merge-file parser: `crates/filters/src/merge/tests.rs:380-391`, `crates/filters/tests/perishable_rules.rs` (whole suite), `crates/filters/tests/dir_merge_rules.rs:46-50` (`dir_merge_with_perishable`), `crates/filters/tests/dir_merge_parsing_comprehensive.rs:15-29,73-99` (with no_inherit, underscore/space separators). | Full |
| `r` (receiver-only) | `FILTRULE_RECEIVER_SIDE`; rejected on side-specific prefixes (`H`, `S`, `P`, `R`); inherited by every rule the merged file contributes (lines 1269-1273 + `FILTRULES_SIDES` propagation at 1293-1304) | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:108-110` (`receiver_modifier`) flips `DirMergeConfig::with_receiver_only` (`crates/filters/src/chain/config.rs:101-105`). Merge-file parser: `crates/filters/src/merge/parse.rs:219` (per-rule `receiver_only`) consumed by `RuleModifiers::apply` lines 161-163; merge-file parser rejects `r` on `H`/`S`/`P`/`R` prefixes via `validate_side_modifiers` (`parse.rs:177-203`). | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:280-284`, `crates/filters/src/chain/tests.rs:35-41` (`dir_merge_config_receiver_only`). Merge-file parser: `crates/filters/src/merge/tests.rs:655-674,700-710` (`parse_include_receiver_only`, rejection on side-specific prefixes), `crates/filters/tests/sender_receiver_sides.rs`, `crates/filters/tests/dir_merge_parsing_comprehensive.rs:32-58` (receiver-only, both-sides). Propagation of the parent-rule side bit to merged-file children (upstream `FILTRULES_SIDES` inheritance) is not directly asserted - `DirMergeConfig::apply_modifiers` (`crates/filters/src/chain/config.rs:140-...`) does call `with_sides` on each child rule, but no test composes a `:r merge.rules` parent with mixed side rules inside `merge.rules` and asserts the child rules inherit `r`. | Partial - own-rule semantics covered; the upstream "side-specific merge file rejects side-specific children" guard (exclude.c:1294-1304) has no equivalent error in oc-rsync. |
| `s` (sender-only) | `FILTRULE_SENDER_SIDE`; symmetric with `r` (lines 1274-1278) | Same shape as `r`. CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:105-107`. Merge-file parser: `crates/filters/src/merge/parse.rs:218`. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:274-278`, `crates/filters/src/chain/tests.rs:27-33` (`dir_merge_config_sender_only`). Merge-file parser: `crates/filters/src/merge/tests.rs:662-666,687-710` (sender-only happy-path + rejection on side prefixes), `crates/filters/tests/dir_merge_parsing_comprehensive.rs:15-29` (`:psn` includes sender-only). | Partial - same gap as `r` (side-specific-merge-file template rejection unverified). |
| `w` (whitespace word-split) | `FILTRULE_WORD_SPLIT`; rejected outside `FILTRULE_MERGE_FILE` (lines 1279-1283). When set, the parent file's tokenizer splits on whitespace instead of newlines (`parse_filter_file` exclude.c:1480-1494). | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:102-104` (`use_whitespace`, disables comments). Merge-file parser: `crates/filters/src/merge/parse.rs:223` records `word_split`; `parse_rule_line_expanded` (`parse.rs:60-116`) implements the expansion by splitting the trailing pattern on whitespace and producing one rule per token. | CLI: `crates/cli/src/frontend/filter_rules/parsing/merge.rs:267-272`. Merge-file parser: `crates/filters/src/merge/tests.rs:409-440` (`parse_word_split_modifier`, `parse_word_split_with_other_modifiers`, `parse_word_split_include`). Negative case "`w` modifier outside a `:` or `.` rule should fail" (upstream `goto invalid`) is NOT asserted - oc-rsync silently accepts `-w pat1 pat2` and expands it, which is by design here but diverges from upstream's syntax-error on `+w` outside a merge file. | Partial - happy path full; upstream's "`w` requires `FILTRULE_MERGE_FILE`" rejection is not enforced. |
| `x` (xattr scope) | `FILTRULE_XATTR`; generic, sets the global `saw_xattr_filter` flag (lines 1284-1287). | CLI parser: handled by the generic rule modifier set (`crates/cli/src/frontend/filter_rules/parsing/modifiers.rs`); not in `parse_merge_modifiers` (so `:x file` is rejected at `crates/cli/src/frontend/filter_rules/parsing/merge.rs:117-128`, even though upstream accepts it). Merge-file parser: `crates/filters/src/merge/parse.rs:220` records `xattr_only`. | Merge-file parser: `crates/filters/src/merge/tests.rs:380-391` (recognised). CLI rejection: no test asserts upstream parity. | Partial - `:x`/`.x` on a CLI `--filter` argument is rejected by oc-rsync but accepted by upstream; merge-file parser records the bit but the chain never composes it with xattr scoping for merged children. |
| `,` (separator before modifiers) | Permitted as a syntactic separator immediately after the rule character (exclude.c:1174-1178). | CLI parser: `crates/cli/src/frontend/filter_rules/parsing/helpers.rs:14-35` (`split_short_rule_modifiers`), `42-77` (`split_short_merge_modifiers`), `79-85` (`split_keyword_modifiers`). Merge-file parser: NOT supported - `parse_modifiers` (`crates/filters/src/merge/parse.rs:211-236`) treats `,` as the "stop" character and leaves it in the pattern. | CLI: `crates/cli/src/frontend/filter_rules/parsing/helpers.rs:108-219` (comma prefix, comma-separated modifiers, keyword `,` split). Merge-file parser: no test covers `:,n file` or `.,e file`. | Partial - CLI surface full; merged `.rsync-filter` files containing `:,n .rsync-filter` would mis-parse. |

`H` / `S` / `P` / `R` / `!` (clear) rule prefixes are listed here as the
"prefix" inputs to `parse_rule_tok()` for completeness, but they are not
merge-modifiers - they select a non-merge rule kind. They are covered by
`crates/filters/src/merge/tests.rs:29-101,338-354` and the dedicated
suites in `crates/filters/tests/protect_risk_rules.rs`, `negated_rules.rs`,
and `clear_rules.rs`.

## Implicit modifier hooks

Modifier-shaped behaviours that are not parsed from a literal character but
applied implicitly to rules expanded from merge / dir-merge files. Listed
separately because they do not occupy a slot in `parse_rule_tok()`'s
modifier-character loop; they are gates upstream OR's onto every per-token
rule produced from a merge file under specific runtime conditions.

| Modifier / hook | Upstream | oc-rsync | Wire flag |
|---|---|---|---|
| Implicit FILTRULE_SENDER_SIDE under `delete_excluded` | `exclude.c:1324-1332 parse_rule_tok` | `chain/mod.rs:537 apply_merge_implicit_sender_side` | `s` short-prefix on the wire (`-s pattern`) |

The implicit flip OR's `FILTRULE_SENDER_SIDE` onto include/exclude rules
expanded from per-token merge content when the runtime `delete_excluded`
flag is set and the rule does not already carry an explicit `s`/`r` side
modifier. Without it, the receiver's `--delete-excluded` pass would observe
`applies_to_receiver = true` on the expanded exclude and skip the matching
files instead of deleting them. Coverage:

- Decision API: `crates/filters/src/chain/tests.rs::cvs_dir_merge_expands_to_sender_side_under_delete_excluded`.
- Wire bytes: `crates/protocol/tests/mdf_2_2_delete_excluded_sender_side_wire.rs`.

## At-risk modifiers and follow-on tasks

The "Partial" rows above isolate the gaps. Grouped by the smallest unit of
work that closes each:

- `-` and `+` on the merge-file parser (`FILTRULE_NO_PREFIXES`
  enforcement when a nested `:` / `.` line carries `-` or `+`): no
  channel exists from `RuleModifiers` to the merged children. Tracked by
  MDF-2 (parse-side fidelity for merge-payload prefix forcing).
- `C` modifier on the merge-file parser silently drops the bit even
  though `parse_modifiers` records it; `RuleModifiers::apply` discards
  `cvs_mode`. Tracked by MDF-3 (merge-file `:C` semantics: whitespace
  tokenisation, no inherit, no prefixes, default `.cvsignore` name).
- `e` (`FILTRULE_EXCLUDE_SELF`) on the merge-file parser is conflated
  with a per-rule "exclude-only" flag named identically (`exclude_only`
  vs `excludes_self`); the chain consumes the right config field only on
  the CLI path. Tracked by MDF-4 (rename / split the field, wire the
  merge-file parser into `DirMergeConfig::with_exclude_self`).
- `n` (`FILTRULE_NO_INHERIT`) is parsed but the chain
  (`crates/filters/src/chain/mod.rs:212-241`) does not drop inherited
  rules on directory re-entry when the parent merge rule carries `n`.
  Tracked by MDF-5 (`pop_filter_list` parity with upstream
  exclude.c:802-803).
- `r` / `s` "side-specific merge contains side-specific child" rejection
  (upstream exclude.c:1294-1304) has no oc-rsync counterpart. Tracked by
  MDF-6 (template-vs-child side conflict diagnostic).
- `w` on a non-merge rule should be a syntax error; oc-rsync accepts and
  expands. Tracked by MDF-7 (reject `w` outside `:` / `.`).
- `x` on CLI merge directives is rejected (upstream accepts), and no
  test composes merged children with the parent `x` bit. Tracked by
  MDF-8 (xattr scoping propagates through merge / dir-merge).
- `,` separator inside merged filter files. Tracked by MDF-9 (accept
  `:,mods file` and `.,mods file` in `parse_modifiers`).

The dir-merge / merge rule-kind selectors themselves (`:` and `.`) and
the `/`, `!`, `p`, `r`, `s` (own-rule), `n` (own-rule), `e` (own-rule)
modifiers are covered end-to-end and need no follow-up.
