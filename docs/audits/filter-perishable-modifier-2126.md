# Audit: filter-rule perishable (`p`) modifier vs upstream rsync 3.4.1

Status: documentation only - no code changes.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/exclude.c`,
`delete.c`, `flist.c`.

## Scope

This audit verifies that oc-rsync's handling of the `p` (perishable) rule
modifier matches upstream rsync 3.4.1 across every layer of the filter
pipeline:

1. Filter-rule struct field and builder API.
2. Short-form (`+p`, `-p`, `Hp`, `Sp`, `Pp`, `Rp`) and long-form parsing.
3. Per-directory merge-file (`:p`, `dir-merge,p`) propagation.
4. Compile-time preservation through `CompiledRule`.
5. Match-time skip during the `Deletion` evaluation context.
6. Wire encoding/decoding gated on protocol version 30+.
7. Test coverage at unit, integration, and engine layers.

The conclusion is that the feature is fully implemented and wire-compatible.
No code changes are required for issue #2126; this document captures the
implementation map and the upstream parity table.

## Upstream behaviour (rsync 3.4.1)

All line numbers reference `target/interop/upstream-src/rsync-3.4.1/`.

### What "perishable" means

A perishable rule is one that does NOT keep its parent directory alive when
the receiver evaluates whether a directory can be removed. The mechanism is
the `ignore_perishable` global (`delete.c:33`), which is set to `1` only
while scanning a directory's contents for the purpose of deciding emptiness.

`exclude.c:1043-1045` skips perishable rules entirely while
`ignore_perishable` is set:

```c
for (ent = listp->head; ent; ent = ent->next) {
    if (ignore_perishable && ent->rflags & FILTRULE_PERISHABLE)
        continue;
    ...
}
```

`flist.c:1265,1333` increments `non_perishable_cnt` when a file is excluded
while `ignore_perishable` is set, signalling that a non-perishable exclude
still pins the parent directory.

`delete.c:144-152` toggles the flag around `delete_dir_contents()`:

```c
if (S_ISDIR(mode) && !(flags & DEL_DIR_IS_EMPTY)) {
    ignore_perishable = 1;
    ret = delete_dir_contents(fbuf, flags);
    ignore_perishable = 0;
    ...
}
```

### Parsing the `p` modifier

`exclude.c:1261-1268` accepts `p` as a per-rule modifier. It is allowed on
every action type (include/exclude/protect/risk/hide/show/dir-merge), unlike
`e`, `n`, and `w` which require a merge-file context.

```c
case 'p':
    rule->rflags |= FILTRULE_PERISHABLE;
    break;
```

`exclude.c:1080-1082` includes `FILTRULE_PERISHABLE` in the set of flags
that flow from a container (merge-file rule) into the rules it parses, so a
`:p ./.rsync-filter` propagates perishability to every rule loaded from
that file.

### Wire-format constraints

`exclude.c:1573-1578` emits `p` only when the protocol is >= 30. On older
protocols the sender drops perishable rules entirely rather than send them
as non-perishable:

```c
if (rule->rflags & FILTRULE_PERISHABLE) {
    if (!for_xfer || protocol_version >= 30)
        *op++ = 'p';
    else if (am_sender)
        return NULL;
}
```

## oc-rsync implementation map

The table below pairs each upstream concern with the oc-rsync site that
implements it. All paths are relative to the workspace root.

| Concern | Upstream | oc-rsync site |
|---|---|---|
| Rule field | `exclude.c` `FILTRULE_PERISHABLE` | `crates/filters/src/rule.rs:63` (`perishable: bool`) |
| Builder API | `exclude.c:1267` | `crates/filters/src/rule.rs:322,362` (`is_perishable`, `with_perishable`) |
| Short-form parser | `exclude.c:1261-1268` | `crates/filters/src/merge/parse.rs:180` (`'p' => mods.perishable = true`) |
| Dir-merge container propagation | `exclude.c:1080-1082` (`FILTRULES_FROM_CONTAINER`) | `crates/filters/src/chain.rs:131-136,161-162` (`DirMergeConfig::with_perishable`, `apply_modifiers`) |
| Dir-merge `:p` line parsing | `exclude.c:1261-1268` | `crates/engine/src/local_copy/dir_merge/parse/line.rs:50-52,90-91` |
| Compiled-rule preservation | `exclude.c` filter list traversal | `crates/filters/src/compiled/rule.rs:28`, `crates/filters/src/compiled/mod.rs:41,82` |
| Deletion-context skip | `exclude.c:1044` (`ignore_perishable`) | `crates/filters/src/decision.rs:155-162` (`include_perishable` flag), `crates/engine/src/local_copy/filter_program/segments.rs:77,100` |
| Wire encode (>=30 only) | `exclude.c:1573-1578` | `crates/protocol/src/filters/prefix.rs:157-158` |
| Wire decode (>=30 only) | `exclude.c:1261-1268` | `crates/protocol/src/filters/wire.rs:406-407` |
| Protocol capability gate | `exclude.c:1574` | `crates/protocol/src/codec/protocol/mod.rs:168-170` (`supports_perishable_modifier`) |

### Deletion-context flow

The `ignore_perishable = 1` toggle that upstream uses around
`delete_dir_contents()` is mirrored in oc-rsync by passing
`DecisionContext::Deletion` (or `FilterContext::Deletion` in the engine)
into the filter evaluation. The matcher then drops perishable rules from
consideration:

`crates/filters/src/decision.rs:160-162`:

```rust
rules.iter().find(|rule| {
    (include_perishable || !rule.perishable) && applies(rule) && rule.matches(path, is_dir)
})
```

`crates/engine/src/local_copy/filter_program/segments.rs:77-79,100-102`:

```rust
if matches!(context, FilterContext::Deletion) && rule.perishable {
    continue;
}
```

This produces the upstream behaviour where a directory containing only
perishable-matched files is treated as empty for the purpose of `rmdir`.

### `--delete-excluded` interaction

oc-rsync's `excluded_for_delete_excluded` probe in
`crates/filters/src/decision.rs:52-62` deliberately calls
`first_matching_rule` with `include_perishable: true`. This matches upstream
behaviour: `--delete-excluded` treats every exclude (perishable or not) as
permission to delete the path itself. Perishability only affects whether
a non-matched sibling file pins the parent directory, not whether a matched
file is removed.

### Protocol gating

`crates/protocol/src/codec/protocol/mod.rs:168-170`:

```rust
fn supports_perishable_modifier(&self) -> bool {
    self.protocol_version() >= 30
}
```

This drives both the encoder (`prefix.rs:157`) and decoder (`wire.rs:406`).
When negotiating protocol 28 or 29, perishability stays local to the rule
set on each side; the modifier never crosses the wire, matching upstream's
silent drop behaviour at `exclude.c:1577`.

## Test coverage

| Layer | Test | File |
|---|---|---|
| Builder | `with_perishable`, `negate_default_false` | `crates/filters/src/rule.rs:561-593` |
| Compile preservation | `compiled_rule_perishable` | `crates/filters/src/compiled/mod.rs:130-144` |
| Decision skip | `perishable_rules_skipped_for_deletion` | `crates/filters/tests/filter_rule_syntax.rs:1099-1108` |
| Short-form parser | `include_with_perishable_modifier`, `exclude_with_perishable_modifier`, `hide_with_perishable`, `show_with_perishable` | `crates/filters/tests/filter_rule_syntax.rs:115,244,614,621` |
| Combined modifiers | `perishable_and_non_perishable_combined`, negate + perishable cases | `crates/filters/tests/filter_rule_syntax.rs:870-935` |
| Dir-merge propagation | `dir_merge_config_perishable`, `chain.add_merge_config(... .with_perishable(true))` | `crates/filters/src/chain.rs:600-603,904-908` |
| Complex combinations | `perishable_in_complex_combinations`, `perishable_exclude_with_protect` | `crates/filters/tests/complex_combinations.rs:283-325` |
| Engine segment | `perishable_rules_are_ignored_for_deletion_context` | `crates/engine/src/local_copy/filter_program_internal_tests.rs:114-138` |
| Wire encode (v30) | `perishable_v30`, `v28_cannot_represent_perishable`, `v29_supports_sender_receiver_but_not_perishable` | `crates/protocol/src/filters/prefix.rs:238-275` |
| Wire decode | `perishable_filter_v30` | `crates/protocol/src/filters/wire.rs:560-580` |
| Codec capability | protocol 28/29/30/32 capability assertions | `crates/protocol/src/codec/protocol/tests.rs:248-260` |

## Parity table

| Behaviour | Upstream | oc-rsync | Match |
|---|---|---|---|
| `p` modifier accepted on `+`/`-`/`H`/`S`/`P`/`R` | yes | yes | yes |
| `p` modifier accepted on long-form keywords | yes | yes (via short-form expansion in `merge/parse.rs`) | yes |
| `:p` propagates perishability into merged rules | yes (`FILTRULES_FROM_CONTAINER`) | yes (`DirMergeConfig::apply_modifiers`) | yes |
| Perishable rules skipped during emptiness probe | yes (`ignore_perishable`) | yes (`DecisionContext::Deletion` + `include_perishable=false`) | yes |
| Perishable rules still apply during transfer | yes | yes | yes |
| `--delete-excluded` removes perishable-excluded paths | yes (perishability is irrelevant when checking the path itself) | yes (`include_perishable=true` for the `excluded_for_delete_excluded` probe) | yes |
| Wire-encoded `p` gated on protocol >= 30 | yes (`exclude.c:1574`) | yes (`supports_perishable_modifier`) | yes |
| Sender drops perishable rule entirely on proto < 30 | yes (`exclude.c:1577`) | yes (encoder emits no `p` and no surrogate flag) | yes |
| AppleDouble auto-exclusions marked perishable on proto >= 30 | yes | yes (`crates/filters/src/apple_double.rs:17-22`) | yes |
| CVS auto-exclusions marked perishable | yes | yes (`crates/filters/src/cvs.rs:19-22`) | yes |

## Conclusion

The perishable modifier is implemented to upstream parity. The parser, the
compiled rule, the deletion-context skip, the wire encoder/decoder, the
protocol capability gate, and the AppleDouble/CVS auto-exclusion sources
all honour the flag. No production code change is required for issue
\#2126; this audit serves as the closure record.
