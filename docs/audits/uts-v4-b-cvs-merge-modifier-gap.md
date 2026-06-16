# UTS-V4.B: `:C` per-dir merge modifier parser gap

## Summary

When a local user-supplied filter file (or `--filter` argv) contains
`:C` (or any `:`/`.` rule whose pattern field is empty), oc-rsync's
parser rejects the directive with "unrecognized filter rule" instead
of treating it as a CVS-style per-directory merge with the default
`.cvsignore` filename. The upstream `exclude-lsh` testsuite test
relies on this form at `target/interop/upstream-src/rsync-3.4.4/testsuite/exclude-lsh.test:86-88`.

The wire-format path (`transfer::generator::filters::wire_rule_to_dir_merge_config`)
handles `:C` correctly. The gap is local-only: rules parsed from a
`.filt`, `--exclude-from`, `--include-from`, or `--filter` argv never
carry the `C` modifier through to the `DirMergeConfig`.

Tracked by task #4428 (UTS-V4.B).

## Reproduction

The failing line in upstream `exclude-lsh.test`:

```sh
cat >"$fromdir/mid/.filt" <<EOF
:C
EOF
```

When the filter merge engine reads `mid/.filt` during traversal and
calls `filters::parse_rules(":C\n", ...)`, the parser walks:

1. `parse_rule_line` → `try_parse_short_form`
2. Strips the `:` prefix, sees `rest = "C"`
3. Calls `parse_modifiers("C")` → returns `(RuleModifiers { cvs_mode: true, .. }, "")`
4. The `pattern.is_empty()` check at `crates/filters/src/merge/parse.rs:305-307`
   forces `Ok(None)`
5. Falls through to `try_parse_long_form`, which does not match `:C`
6. Returns `Err(MergeFileError::ParseError { message: "unrecognized filter rule: :C", ... })`

The merge file load fails, the dir-merge scope is never pushed, and
the subsequent transfer skips the cvs-ignore semantics the test
asserts on (`one-in-one-out` should be excluded from `mid/`).

## Root cause

Two coupled defects in `crates/filters/src/merge/parse.rs`:

1. **Empty-pattern rejection for `:C`/`.C`** (line 305-307). Upstream
   `exclude.c:1404-1408` (rsync-3.4.4) treats an empty merge filename
   as a sentinel: when `FILTRULE_MERGE_FILE` is set and `pat_len == 0`,
   the filename defaults to `.cvsignore`. The oc-rsync parser instead
   short-circuits with `Ok(None)` whenever the modifiers consume the
   entire rest, regardless of the action prefix.

2. **`cvs_mode` is parser-internal**. `RuleModifiers::cvs_mode` is set
   in `parse_modifiers` (line 224) but never propagated into
   `FilterRule`. `RuleModifiers::apply` (line 151) drops it. The
   `FilterRule` struct in `crates/filters/src/rule.rs:57-70` has no
   `cvs_mode` field. Consumers that translate a parsed
   `FilterAction::DirMerge` rule into a `DirMergeConfig` (e.g.
   `cli::frontend::execution::drive::filters`, `engine::local_copy::filter_program::segments`)
   therefore have no signal to call `DirMergeConfig::with_cvs_mode(true)`.

The wire-format path is unaffected because `FilterRuleWireFormat`
carries `cvs_exclude: bool` explicitly and `wire_rule_to_dir_merge_config`
already calls `with_cvs_mode(true)`
(`crates/transfer/src/generator/filters.rs:488-493`).

## Upstream reference

- `target/interop/upstream-src/rsync-3.4.4/exclude.c:1248-1255` — `C`
  modifier sets `FILTRULE_NO_PREFIXES | FILTRULE_WORD_SPLIT | FILTRULE_NO_INHERIT | FILTRULE_CVS_IGNORE`.
- `target/interop/upstream-src/rsync-3.4.4/exclude.c:1324` — empty
  pattern is only an error when `FILTRULE_CVS_IGNORE` is not set.
- `target/interop/upstream-src/rsync-3.4.4/exclude.c:1404-1408` —
  default filename `.cvsignore` substituted when merge rule has empty
  pattern.
- `target/interop/upstream-src/rsync-3.4.4/testsuite/exclude-lsh.test:86-88` —
  failing fixture.

## PR #5817 scope (does NOT cover this)

PR #5817 (`fix(filters): fall through to outer scopes when non-inheriting scope has no match`)
changes scope evaluation in `FilterChain::allows`/`allows_deletion` to
stop short-circuiting on synthetic `pattern/**` descendant matchers
when no real rule fires in a non-inheriting (`:C`-style) scope. It
operates on already-loaded scopes. UTS-V4.B fails before any scope is
pushed because the merge file cannot be parsed. The two fixes are
orthogonal: #5817 fixes the post-load decision lookup, UTS-V4.B fixes
the pre-load parse.

## Fix specification

Three changes, all in `crates/filters`:

### 1. Add `cvs_mode` to `FilterRule`

```rust
// crates/filters/src/rule.rs
pub struct FilterRule {
    // ... existing fields ...
    /// `C` modifier on merge / dir-merge rules: parse target as
    /// whitespace-separated cvs-style ignore tokens with no prefixes.
    /// upstream: exclude.c:1248-1254 - FILTRULE_CVS_IGNORE.
    pub(crate) cvs_mode: bool,
}
```

Add builder method `with_cvs_mode(self, cvs_mode: bool) -> Self` and
accessor `is_cvs_mode(&self) -> bool`. Default to `false` in every
existing constructor. Equality/Debug already derive over all fields.

### 2. Propagate `cvs_mode` through `RuleModifiers::apply`

```rust
// crates/filters/src/merge/parse.rs
pub(crate) fn apply(self, rule: FilterRule) -> FilterRule {
    let mut rule = rule
        // ... existing chain ...
        .with_cvs_mode(self.cvs_mode);
    // ... existing side logic ...
}
```

### 3. Allow `:` / `.` short-form with `C` modifier and empty pattern

In `try_parse_short_form` (line 279-316), after `parse_modifiers`, add
a special case before the `pattern.is_empty()` reject:

```rust
if pattern.is_empty() {
    // upstream: exclude.c:1404-1408 - merge with empty pattern
    // and FILTRULE_CVS_IGNORE defaults to filename ".cvsignore".
    if mods.cvs_mode && matches!(action, ShortFormAction::Merge | ShortFormAction::DirMerge) {
        let rule = action.to_rule(".cvsignore");
        return Ok(Some(mods.apply(rule)));
    }
    return Ok(None);
}
```

### 4. Wire receiving end to consult `is_cvs_mode()`

CLI argv consumer (`crates/cli/src/frontend/execution/drive/filters.rs`
or equivalent) and engine local-copy program
(`crates/engine/src/local_copy/filter_program/segments.rs:34`) must
check `rule.is_cvs_mode()` when translating a `FilterAction::DirMerge`
rule into a `DirMergeConfig` and call
`DirMergeConfig::with_cvs_mode(true).with_inherit(false)`. This mirrors
`wire_rule_to_dir_merge_config`.

## Test plan

Unit tests in `crates/filters/src/merge/tests.rs`:

```rust
#[test]
fn parse_colon_C_empty_pattern_defaults_to_cvsignore() {
    let rules = parse_rules(":C\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".cvsignore");
    assert!(rules[0].is_cvs_mode());
}

#[test]
fn parse_colon_C_with_explicit_pattern_preserves_cvs_mode() {
    let rules = parse_rules(":C my.ignore\n", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "my.ignore");
    assert!(rules[0].is_cvs_mode());
}

#[test]
fn parse_dot_C_empty_pattern_defaults_to_cvsignore() {
    let rules = parse_rules(".C\n", Path::new("test")).unwrap();
    assert_eq!(rules[0].action(), FilterAction::Merge);
    assert_eq!(rules[0].pattern(), ".cvsignore");
    assert!(rules[0].is_cvs_mode());
}
```

Integration test extending
`crates/transfer/tests/exclude_lsh_cvs_wire_expansion.rs` to exercise
the local-parse path (today the test only covers wire delivery).

## Scope

Estimated LOC:
- `crates/filters/src/rule.rs`: ~30 lines (field + with/is methods + default updates).
- `crates/filters/src/merge/parse.rs`: ~10 lines (apply chain + empty-pattern branch).
- `crates/cli/src/frontend/execution/drive/filters.rs`: ~10 lines (wire `is_cvs_mode`).
- `crates/engine/src/local_copy/filter_program/segments.rs`: ~10 lines.
- Tests: ~80 lines.

Total: ~140 LOC. Within the surgical-fix cap but crosses 4 crates and
adds a public field to `FilterRule`. Filed as audit doc rather than
shipped as a fix because:

1. Public API change to `FilterRule` warrants a separate review pass
   on whether `cvs_mode` should be public or `pub(crate)`-only with an
   accessor.
2. The CLI/engine consumer wiring touches paths that are concurrently
   being modified by UTS-V3.B (exclude-lsh delete-during) and PR #5817;
   land those first to avoid merge churn.

Once #5817 lands, this can ship as a focused `fix:` PR with the four
edits above plus the regression tests.
