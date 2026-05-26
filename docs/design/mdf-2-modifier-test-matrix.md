# MDF-2: Per-modifier wire-byte test matrix

Spec for a comprehensive test matrix covering every merge/dir-merge modifier
in the filter system - both at the parse layer and the wire-format layer.
Follows from MDF-1 findings in `docs/audit/merge-modifier-coverage.md`.

## Scope

Every modifier character accepted by upstream `exclude.c::parse_rule_tok()`
(lines 1215-1289) in the context of merge-file (`.`) and dir-merge (`:`)
rules. The test matrix validates:

1. Parse correctness - each modifier produces the expected internal state.
2. Wire-byte fidelity - serialized bytes match upstream's prefix layout.
3. Round-trip stability - serialize then deserialize yields the original rule.
4. Protocol gating - modifiers gated by protocol version are absent at older
   versions and present at newer ones.
5. Rejection semantics - invalid modifier combinations produce errors matching
   upstream's `goto invalid` paths.

## Complete modifier inventory

Source of truth: `target/interop/upstream-src/rsync-3.4.1/exclude.c:1215-1289`.

| Char | Flag constant | Protocol | Merge-only | Wire position |
|------|--------------|----------|------------|---------------|
| `/`  | `FILTRULE_ABS_PATH` | All | No | 1st (after type char) |
| `!`  | `FILTRULE_NEGATE` | All | No (rejected on merge) | 2nd |
| `C`  | `NO_PREFIXES \| WORD_SPLIT \| NO_INHERIT \| CVS_IGNORE` | All | Yes | 3rd |
| `n`  | `FILTRULE_NO_INHERIT` | All | Yes | 4th |
| `w`  | `FILTRULE_WORD_SPLIT` | All | Yes | 5th |
| `e`  | `FILTRULE_EXCLUDE_SELF` | All | Yes | 6th |
| `x`  | `FILTRULE_XATTR` | All | No | 7th |
| `s`  | `FILTRULE_SENDER_SIDE` | v29+ | No | 8th |
| `r`  | `FILTRULE_RECEIVER_SIDE` | v29+ | No | 9th |
| `p`  | `FILTRULE_PERISHABLE` | v30+ | No | 10th |
| `-`  | `FILTRULE_NO_PREFIXES` (exclude) | All | Yes (payload) | N/A (payload) |
| `+`  | `NO_PREFIXES \| INCLUDE` (include) | All | Yes (payload) | N/A (payload) |
| `,`  | (syntactic separator) | All | N/A | N/A (stripped) |

The `-` and `+` modifiers are "payload modifiers" - they force every rule
inside the merged file to be exclude-only or include-only respectively. They
do not appear in the wire prefix; they affect the semantics of child rules
read from the merge file.

## Wire prefix layout

Upstream `exclude.c:1522-1587` (`get_rule_prefix()`) emits the prefix in a
fixed order. Our `build_rule_prefix()` at
`crates/protocol/src/filters/prefix.rs:100-163` mirrors this:

```
<type-char>[/][!][C][n][w][e][x][s][r][p]<SP><pattern>[/]
```

Where:
- `<type-char>` is one of `+`, `-`, `:`, `.` (P/R are normalized to `-`/`+`
  with forced `r` flag)
- Modifier chars appear in the fixed order above - never reordered
- `<SP>` is a trailing space separating modifiers from pattern
- Trailing `/` indicates directory-only match

### Framing

Each rule is framed as:
```
[4-byte LE int32: len(prefix + pattern + trailing-slash)] [prefix bytes] [pattern bytes] [optional /]
```

The list is terminated by a 4-byte LE zero. This uses `write_int()`/`read_int()`
(fixed 4-byte LE), not varint encoding.

## Per-modifier detail

### `/` - anchored pattern

- **Wire byte**: ASCII `0x2F` immediately after type char
- **Behavior**: Pattern is anchored to the transfer root (like a leading `/` in
  the pattern itself)
- **Edge cases**: Interacts with the pattern's own leading `/` - upstream
  strips the leading `/` from the pattern and sets the flag instead
- **Context**: Valid on all rule types (include, exclude, merge, dir-merge)
- **Wire example**: `-/ *.log` - exclude anchored `*.log`

### `!` - negate

- **Wire byte**: ASCII `0x21` after optional `/`
- **Behavior**: Inverts match logic - if the pattern matches, the rule does NOT
  apply (upstream `FILTRULE_NEGATE`)
- **Edge cases**: REJECTED on merge/dir-merge parent rules (upstream
  `exclude.c:1241-1247` jumps to `invalid`). Valid on child rules inside
  merged files.
- **Context**: Include/exclude rules only when rule is a merge parent
- **Wire example**: `-! *.keep` - exclude-unless-matches `*.keep`

### `C` - CVS-compatible bundle

- **Wire byte**: ASCII `0x43` after `!` position
- **Behavior**: Sets four flags at once: `NO_PREFIXES`, `WORD_SPLIT`,
  `NO_INHERIT`, `CVS_IGNORE`. Forces default filename `.cvsignore`.
  Disables `#` comment parsing. Splits on whitespace.
- **Edge cases**: Mutually exclusive with `+`, `-` payload modifiers.
  Conflicts with side-specific prefixes (`H`, `S`, `P`, `R`).
- **Context**: Merge/dir-merge only
- **Wire example**: `:C .cvsignore` - dir-merge CVS mode

### `n` - no-inherit

- **Wire byte**: ASCII `0x6E` after `C` position
- **Behavior**: Rules from this merge file do not propagate to subdirectories.
  On re-entry to a directory, inherited rules are dropped (upstream
  `push_local_filters` `exclude.c:802-803`).
- **Edge cases**: Only valid with `FILTRULE_MERGE_FILE` (upstream rejects on
  non-merge rules at 1261-1265).
- **Context**: Merge/dir-merge only
- **Wire example**: `:n .rsync-filter` - non-inheriting dir-merge

### `w` - word-split

- **Wire byte**: ASCII `0x77` after `n` position
- **Behavior**: Tokenizes the merged file on whitespace instead of newlines.
  Each whitespace-delimited token becomes a separate rule. Disables `#`
  comment parsing.
- **Edge cases**: Only valid with `FILTRULE_MERGE_FILE` (upstream rejects
  otherwise at 1279-1283). oc-rsync currently accepts `w` on non-merge rules
  (divergence tracked as MDF-7).
- **Context**: Merge/dir-merge only
- **Wire example**: `:w .rsync-filter` - whitespace-split dir-merge

### `e` - exclude self from transfer

- **Wire byte**: ASCII `0x65` after `w` position
- **Behavior**: The merge/dir-merge file itself is excluded from the transfer
  (upstream `FILTRULE_EXCLUDE_SELF`). The file is still read for its rules
  but not transferred to the receiver.
- **Edge cases**: Only valid with `FILTRULE_MERGE_FILE`. oc-rsync conflates
  this with a per-rule "exclude-only" flag (divergence tracked as MDF-4).
- **Context**: Merge/dir-merge only
- **Wire example**: `:e .rsync-filter` - self-excluding dir-merge

### `x` - xattr scope

- **Wire byte**: ASCII `0x78` after `e` position
- **Behavior**: Rule applies only to xattr filtering, not to file
  include/exclude decisions. Sets the global `saw_xattr_filter` flag.
- **Edge cases**: Valid on all rule types per upstream (lines 1284-1287), but
  oc-rsync CLI rejects `:x`/`.x` on merge directives (divergence tracked as
  MDF-8).
- **Context**: All rule types (generic modifier)
- **Wire example**: `-x user.*` - exclude xattr `user.*`

### `s` - sender-side only

- **Wire byte**: ASCII `0x73` after `x` position (v29+ only)
- **Behavior**: Rule applies only on the sender side. Rejected when the prefix
  already implies a side (`H`, `S` = sender-side prefixes).
- **Edge cases**: When a merge rule carries `s`, all child rules it
  contributes inherit sender-side scope (upstream `FILTRULES_SIDES`
  propagation at 1293-1304). oc-rsync does not enforce rejection of
  side-specific children inside a side-specific merge file (MDF-6).
- **Context**: All rule types; protocol v29+ gated
- **Wire example**: `-s *.tmp` - sender-only exclude

### `r` - receiver-side only

- **Wire byte**: ASCII `0x72` after `s` position (v29+ only)
- **Behavior**: Rule applies only on the receiver side. Same side-prefix
  conflict guard as `s`. Protect (`P`) and Risk (`R`) rules force `r` on the
  wire because they are encoded as `-`/`+` with `r` modifier.
- **Edge cases**: Same child-inheritance gap as `s` (MDF-6).
- **Context**: All rule types; protocol v29+ gated
- **Wire example**: `:r .rsync-filter` - receiver-only dir-merge

### `p` - perishable

- **Wire byte**: ASCII `0x70` after `r` position (v30+ only)
- **Behavior**: Rule is removed from the filter list once the file it
  references is encountered. Generic modifier - applies to any rule kind.
- **Edge cases**: No conflict guards.
- **Context**: All rule types; protocol v30+ gated
- **Wire example**: `:p .rsync-filter` - perishable dir-merge

### `-` - exclude-only payload

- **Wire byte**: N/A (not encoded in wire prefix)
- **Behavior**: Forces every rule inside the merged file to be treated as a
  bare exclude pattern (no `+`/`-` prefixes honored in the merged file).
  Sets `FILTRULE_NO_PREFIXES` on the parent merge rule.
- **Edge cases**: Mutually exclusive with `+`. In the merge-file parser, a
  leading `-` is consumed as the exclude action rather than as a modifier -
  oc-rsync cannot currently apply the payload-forcing effect (MDF-2 gap).
- **Context**: Merge/dir-merge only (payload modifier)
- **Parse example**: `:-` or `:- .rsync-filter` on a CLI `--filter` argument

### `+` - include-only payload

- **Wire byte**: N/A (not encoded in wire prefix)
- **Behavior**: Forces every rule inside the merged file to be treated as a
  bare include pattern. Sets `FILTRULE_NO_PREFIXES | FILTRULE_INCLUDE`.
- **Edge cases**: Mutually exclusive with `-` and `C`. Same parse ambiguity
  as `-` in the merge-file parser.
- **Context**: Merge/dir-merge only (payload modifier)
- **Parse example**: `:+` or `:+ .rsync-filter` on a CLI `--filter` argument

### `,` - separator

- **Wire byte**: N/A (stripped during parsing, never serialized)
- **Behavior**: Syntactic separator between the rule character and modifiers.
  Upstream `exclude.c:1174-1178` permits a comma immediately after the rule
  character: `:,n filter` parses as dir-merge with `n` modifier.
- **Edge cases**: oc-rsync CLI parser handles this correctly. The merge-file
  parser treats `,` as a stop character, leaving it in the pattern (MDF-9).
- **Context**: Syntactic, pre-modifier position
- **Parse example**: `:,ne .rsync-filter` - dir-merge with `n` and `e`

## Test matrix structure

The matrix crosses two dimensions:

**Rows** - modifier characters (13 entries above)

**Columns** - context:
1. `include (+)` - modifier on an include rule
2. `exclude (-)` - modifier on an exclude rule
3. `merge (.)` - modifier on a merge-file rule
4. `dir-merge (:)` - modifier on a per-directory merge rule
5. `protect (P)` - modifier on a protect rule (wire-normalized to `-r`)
6. `risk (R)` - modifier on a risk rule (wire-normalized to `+r`)

Each cell is one of:
- **VALID** - modifier is accepted; test asserts correct parse + wire encoding
- **REJECT** - modifier must produce an error; test asserts the error
- **N/A** - modifier is a payload concept (not a wire flag) and tested separately

### Expected validity by cell

| Modifier | + | - | . | : | P | R |
|----------|---|---|---|---|---|---|
| `/`      | VALID | VALID | VALID | VALID | VALID | VALID |
| `!`      | VALID | VALID | REJECT | REJECT | VALID | VALID |
| `C`      | REJECT | REJECT | VALID | VALID | REJECT | REJECT |
| `n`      | REJECT | REJECT | VALID | VALID | REJECT | REJECT |
| `w`      | REJECT | REJECT | VALID | VALID | REJECT | REJECT |
| `e`      | REJECT | REJECT | VALID | VALID | REJECT | REJECT |
| `x`      | VALID | VALID | VALID | VALID | VALID | VALID |
| `s`      | VALID | VALID | VALID | VALID | REJECT | REJECT |
| `r`      | VALID | VALID | VALID | VALID | REJECT | REJECT |
| `p`      | VALID | VALID | VALID | VALID | VALID | VALID |
| `-`      | N/A | N/A | VALID | VALID | N/A | N/A |
| `+`      | N/A | N/A | VALID | VALID | N/A | N/A |
| `,`      | N/A | N/A | VALID | VALID | N/A | N/A |

Note: `s`/`r` on P/R are REJECT because P/R already imply receiver-side; adding
an explicit side flag conflicts per upstream `exclude.c:1269-1278`.

## Wire-byte assertions

Each VALID cell requires two assertion types:

### 1. Serialize assertion

Given a `FilterRuleWireFormat` with the modifier flag set, `build_rule_prefix()`
must produce the exact expected prefix string.

```rust
// Example: dir-merge with no-inherit + sender-only at protocol 32
let rule = FilterRuleWireFormat {
    rule_type: RuleType::DirMerge,
    pattern: ".rsync-filter".to_owned(),
    no_inherit: true,
    sender_side: true,
    ..Default::default()
};
let prefix = build_rule_prefix(&rule, proto(32)).unwrap();
assert_eq!(prefix, ":ns ");
```

### 2. Parse assertion

Given the raw wire bytes (prefix + pattern), `parse_wire_rule()` must produce
a `FilterRuleWireFormat` with exactly the expected flags set.

```rust
// Example: parse ":ns .rsync-filter" at protocol 32
let wire_bytes = b":ns .rsync-filter";
let rule = parse_wire_rule(wire_bytes, proto(32)).unwrap();
assert_eq!(rule.rule_type, RuleType::DirMerge);
assert!(rule.no_inherit);
assert!(rule.sender_side);
assert_eq!(rule.pattern, ".rsync-filter");
```

### 3. Round-trip assertion

```rust
let original = /* construct rule */;
let mut buf = Vec::new();
write_filter_list(&mut buf, &[original.clone()], protocol).unwrap();
let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
assert_eq!(parsed[0], original);
```

### 4. Protocol-gated absence

```rust
// s/r must NOT appear at protocol 28
let rule = FilterRuleWireFormat::exclude("*.tmp".to_owned()).with_sides(true, false);
let result = build_rule_prefix(&rule, proto(28));
assert!(result.is_none());

// p must NOT appear at protocol 29
let rule = FilterRuleWireFormat::exclude("*.tmp".to_owned()).with_perishable(true);
let result = build_rule_prefix(&rule, proto(29));
assert!(result.is_none());
```

## Modifier ordering test

A dedicated test validates that when multiple modifiers are active, the wire
prefix emits them in the canonical order `/!CnwexsrpSP`. This is a single
test constructing a rule with all flags set and asserting the prefix byte
sequence.

```rust
let rule = FilterRuleWireFormat {
    rule_type: RuleType::DirMerge,
    pattern: "test".to_owned(),
    anchored: true,
    negate: true,
    cvs_exclude: true,
    no_inherit: true,
    word_split: true,
    exclude_from_merge: true,
    xattr_only: true,
    sender_side: true,
    receiver_side: true,
    perishable: true,
    directory_only: true,
};
let prefix = build_rule_prefix(&rule, proto(32)).unwrap();
assert_eq!(prefix, ":/!Cnwexsrp ");
```

## Upstream parity validation

### Strategy 1: Capture upstream wire output

Run upstream rsync with known filter rules and capture the wire bytes using
`strace` or `tcpdump`. Compare byte-for-byte with oc-rsync's serialization.

```bash
# Capture upstream filter exchange on daemon pull
strace -e trace=write -s 4096 -f \
  rsync --filter=':n .rsync-filter' rsync://localhost/mod/src/ /tmp/dst/ \
  2>&1 | grep 'write.*:n '
```

### Strategy 2: Differential fuzzer

The existing `crates/filters/fuzz/fuzz_targets/fuzz_filter_parse.rs` can be
extended to generate random modifier combinations, serialize them, then parse
with both oc-rsync and upstream (via subprocess) and compare results.

### Strategy 3: Golden byte fixtures

Capture known-good wire bytes from upstream rsync for each modifier and store
them as golden fixtures. Tests assert our serializer produces identical output.

File location: `crates/protocol/tests/golden/filter_modifiers/`

Each fixture is a `.bin` file containing the raw framed bytes (4-byte LE
length + prefix + pattern) for a single rule, plus a companion `.json` file
describing the expected parsed fields.

## Test file structure

### Location

```
crates/protocol/tests/
  filter_modifier_wire_matrix.rs    # Wire-byte serialize/parse/roundtrip
  golden/
    filter_modifiers/
      dir_merge_no_inherit.bin      # Golden wire bytes
      dir_merge_no_inherit.json     # Expected parsed fields
      ...

crates/filters/tests/
  modifier_parse_matrix.rs          # Merge-file parser modifier tests
  modifier_rejection_matrix.rs      # Invalid modifier combinations
```

### Naming convention

Test functions follow:
```
test_wire_{rule_type}_{modifier}_{protocol_version}
test_wire_{rule_type}_{modifier}_roundtrip
test_wire_{rule_type}_{modifier}_rejected
test_parse_{rule_type}_{modifier}
test_parse_{rule_type}_{modifier}_rejected
```

Examples:
```rust
#[test] fn test_wire_dir_merge_no_inherit_v32() { ... }
#[test] fn test_wire_dir_merge_no_inherit_roundtrip() { ... }
#[test] fn test_wire_exclude_negate_v32() { ... }
#[test] fn test_wire_merge_negate_rejected() { ... }
#[test] fn test_parse_dir_merge_comma_separator() { ... }
```

### Test organization

Each test file is organized by modifier, with sub-sections for each rule type
context:

```rust
// --- `/` (anchored) ---

mod anchored {
    #[test] fn wire_include_anchored() { ... }
    #[test] fn wire_exclude_anchored() { ... }
    #[test] fn wire_dir_merge_anchored() { ... }
    #[test] fn wire_merge_anchored() { ... }
    #[test] fn roundtrip_anchored_v29() { ... }
    #[test] fn roundtrip_anchored_v32() { ... }
}
```

## Fixture design

### Merge-file fixtures

Sample `.rsync-filter` files exercising each modifier, stored at
`crates/filters/tests/fixtures/modifiers/`:

| Fixture file | Content | Exercises |
|---|---|---|
| `no_inherit.filter` | `:n .child-filter` | `n` on dir-merge |
| `word_split.filter` | `:w .patterns` | `w` on dir-merge |
| `exclude_self.filter` | `:e .rsync-filter` | `e` on dir-merge |
| `cvs_mode.filter` | `:C .cvsignore` | `C` bundle |
| `sender_only.filter` | `:s .rsync-filter` | `s` side scoping |
| `receiver_only.filter` | `:r .rsync-filter` | `r` side scoping |
| `perishable.filter` | `:p .rsync-filter` | `p` |
| `exclude_payload.filter` | `:- .rsync-filter` | `-` payload mod |
| `include_payload.filter` | `:+ .rsync-filter` | `+` payload mod |
| `comma_separator.filter` | `:,ne .rsync-filter` | `,` before mods |
| `all_modifiers.filter` | `:Cnwexsrp .rsync-filter` | Full combination |
| `anchored_merge.filter` | `:/ .rsync-filter` | `/` on dir-merge |
| `xattr_scope.filter` | `:x .rsync-filter` | `x` on dir-merge |

### Content files for word-split tests

```
# word_split_content.txt
*.o *.a *.so *.dylib
```

When merged with `:w`, produces four separate exclude rules.

### Content files for payload modifier tests

```
# exclude_payload_content.txt
# No +/- prefixes honored - all patterns are excludes
*.bak
*.tmp
~*
```

When the parent merge rule carries `-`, these are unconditionally exclude
patterns regardless of any `+` prefix appearing in the file.

## Implementation priority

Tests should be implemented in this order:

1. **Wire-byte ordering test** (single test, high value) - validates canonical
   modifier order.
2. **Protocol-gated tests** (`s`/`r` at v28, `p` at v29) - regression guards
   for protocol compatibility.
3. **Per-modifier serialize + parse** (the bulk) - one test per VALID cell.
4. **Rejection tests** - one test per REJECT cell, asserting specific error.
5. **Golden byte fixtures** - captured from upstream for long-term regression.
6. **Round-trip property tests** - proptest-based fuzzing of random modifier
   combinations.
7. **Payload modifier tests** (`-`, `+`, `,`) - require merge-file parser
   improvements tracked by MDF-2/MDF-9.

## Relation to other MDF tasks

| Task | Dependency |
|------|-----------|
| MDF-3 | `C` modifier parse-side fidelity - tests here validate wire encoding; MDF-3 fixes the semantic gap |
| MDF-4 | `e` field rename - tests here document the expected behavior; MDF-4 fixes the implementation |
| MDF-5 | `n` chain pop behavior - wire tests pass today; MDF-5 fixes runtime semantics |
| MDF-6 | `s`/`r` child rejection - wire tests document the expected error; MDF-6 implements it |
| MDF-7 | `w` non-merge rejection - rejection tests document the expected error; MDF-7 implements it |
| MDF-8 | `x` on merge CLI - tests document the expected acceptance; MDF-8 removes the rejection |
| MDF-9 | `,` in merge parser - tests document the expected parse; MDF-9 fixes it |

## Success criteria

- Every VALID cell in the matrix has a passing serialize + parse + round-trip
  test at protocol 32.
- Every REJECT cell has a test asserting a specific error type/message.
- Protocol-gated modifiers (`s`, `r`, `p`) have tests at the boundary
  protocol versions (v28/v29/v30).
- The canonical modifier ordering test passes.
- At least one golden byte fixture is validated against upstream rsync wire
  capture for each modifier.
- No test relies on unimplemented features (tests for MDF-3 through MDF-9
  gaps are marked `#[ignore]` with a tracking comment until the corresponding
  task ships).
