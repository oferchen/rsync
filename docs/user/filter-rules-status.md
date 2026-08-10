# Filter rules: support status and known gaps

Audience: users running `oc-rsync` with non-trivial `--filter`, `--include-from`,
`--exclude-from`, `--cvs-exclude`, `merge` / `dir-merge`, or `.rsync-filter`
configurations. This document records what works today vs upstream rsync 3.4.x
and what to expect when you hit one of the known gaps.

## 1. Overview

oc-rsync implements rsync's full filter-rule grammar: every include / exclude
prefix, the anchor and wildcard semantics, the `merge` and `dir-merge`
directives, all modifier letters that ride them, and the per-directory
`.rsync-filter` discovery mechanism. The majority of real-world filter rule
sets - including CVS-style exclude bundles, side-specific rule sets, and
multi-file merge compositions - work correctly and match upstream behaviour
byte-for-byte on the wire.

This document enumerates the gaps that remain relative to upstream rsync 3.4.x.
The audit that produced the gap list lives at
[`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md)
(task MDF-1). Each item below cross-references the follow-up task that is
chartered to close it.

## 2. Fully supported

The following are tested against the upstream interop matrix (rsync 3.0.9,
3.1.3, 3.4.1, 3.4.2) and behave identically:

- All include / exclude prefix rules: `-`, `+`, `H` (hide), `S` (show),
  `R` (risk), `P` (protect), `+x`, `-x`, and the `!` clear rule.
- Anchored patterns: a leading `/` anchors the pattern to the transfer root.
- Wildcards: `**` (multi-segment glob), `*` (single-segment), `?`
  (single-char), and character classes `[a-z]`, `[!abc]`.
- Comments (`# ...`) and blank lines are skipped in all merge inputs.
- `--include-from <file>`, `--exclude-from <file>`, `--files-from <file>` -
  external rule files with `-` recognised as stdin.
- `--cvs-exclude` (`-C`) standalone flag - inserts the CVS bundle
  (`.cvsignore`, common version-control directories, `$CVSIGNORE` env var).
- Basic `merge` directive: `. <file>` and full-word `merge <file>` - parses
  the named file once at startup and splices its rules into the parent chain.
- Basic `dir-merge` directive: `: <file>` and `.rsync-filter` - discovers the
  named per-directory merge file at every directory entry and re-scopes its
  rules until the directory is left.
- Anchor (`/`) and word-split (`w`) modifiers on merge / dir-merge directives
  given on the command line.
- The `n` (no-inherit), `e` (exclude-self), `s` (sender-only), `r`
  (receiver-only), and `p` (perishable) modifiers on the CLI parse path.

## 3. Partial support and known gaps

Each gap below is sourced from the MDF-1 audit
([`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md)).
The "Fix tracking" column points at the follow-up task that closes the gap.

### 3.1 CVS bundle dropped on dir-merge `:C`

A merge file containing a line like `:C .rsync-filter` is recorded but the
`C` modifier never propagates to the rule the chain consumes. The CVS
bundle (whitespace tokenisation, no-inherit, default `.cvsignore` name) is
silently lost. The CLI form `--filter=':C .rsync-filter'` is also affected
on the merge-file expansion path; only the standalone `--cvs-exclude` (`-C`)
flag is reliably wired today.

- Workaround: pass `--cvs-exclude` (`-C`) on the command line.
- Fix tracking: MDF-2.

### 3.2 `e` modifier on merge-file rules not wired through

The `e` modifier on a `dir-merge` rule tells upstream to exclude the merge
file itself from the transfer. On the CLI parse path this is wired through
to `DirMergeConfig::with_exclude_self`. In the merge-file parser the same
letter is consumed but maps to a per-rule "exclude-only" decoration that has
nothing to do with the upstream `FILTRULE_EXCLUDE_SELF` bit. As a result,
`.rsync-filter` files containing a nested `:e .rsync-filter` will transfer
the merge file along with the data.

- Workaround: explicitly add an exclude rule for the merge file, e.g.
  `- .rsync-filter`, as a sibling rule.
- Fix tracking: MDF-2.

### 3.3 `n` (no-inherit) modifier ignored on directory leave

The `n` modifier on a `dir-merge` parent tells upstream to drop the rules
that the merge file contributed when the receiver leaves the directory.
oc-rsync parses the bit but the chain's directory-pop step does not consult
`is_no_inherit`. As a result, rules from a `:n` dir-merge file remain in
scope after the chain has traversed back up, which can over-exclude files
in sibling subtrees if the merged rules used non-anchored patterns.

- Workaround: prefer explicit per-directory `.rsync-filter` files anchored
  with `/` over `n`-modified dir-merge rules in deep hierarchies.
- Fix tracking: MDF-2.

### 3.4 `w` modifier accepted on non-merge rules

Upstream rejects the `w` (word-split) modifier on anything other than a
merge or dir-merge directive (`exclude.c:1279-1283`). oc-rsync silently
accepts a non-merge rule with `w` and expands the trailing pattern as if it
were whitespace-tokenised. A configuration that relied on upstream's syntax
error to surface a typo would pass without comment under oc-rsync.

- Workaround: validate filter rule sets against upstream once before
  deploying them widely if you intend to use word-split semantics.
- Fix tracking: MDF-2.

### 3.5 CLI rejects `:x file` and `.x file`

The `x` modifier (xattr scope) is generic in upstream's parser and is
accepted on every rule kind including `dir-merge` and `merge`. oc-rsync's
CLI parser rejects `:x` and `.x` short forms, treating the `x` as an
unknown modifier on a merge directive.

- Workaround: use the long-form `--filter='dir-merge,x <file>'` or
  `--filter='merge,x <file>'` syntax, which is accepted by the CLI parser
  via the keyword-modifier split path.
- Fix tracking: MDF-2.

### 3.6 `,` separator not wired in merge files

The comma separator between the rule character and modifier letters
(`:,n file` instead of `:n file`) is permitted by upstream as a syntactic
convenience. The oc-rsync CLI parser handles it, but the merge-file parser
treats `,` as a stop character and leaves it in the pattern. A
`.rsync-filter` containing `:,n .rsync-filter` will mis-parse, treating
`,n` as part of the rule rather than as a modifier prefix.

- Workaround: use space separation in merge files
  (e.g. `:n .rsync-filter` instead of `:,n .rsync-filter`).
- Fix tracking: MDF-2.

### 3.7 Nested merge depth beyond two unvalidated

Merge and dir-merge files that themselves contain merge / dir-merge
directives are exercised at depth 2 (a top-level `merge` directive pulling
in a file that contains a `dir-merge`). Behaviour at depth 3 or greater is
parsed by the same code path but not asserted by the test suite.

- Workaround: keep merge nesting at depth 2 or less, or validate against
  upstream rsync for deeper compositions.
- Fix tracking: MDF-3.

### 3.8 Anchor semantics inside merged rules unaudited

When a merged file contains anchored patterns (e.g. `/foo`), upstream
scopes the anchor to the merge file's directory. oc-rsync's behaviour on
this scoping is unaudited and may scope the anchor to the transfer root
instead. A `.rsync-filter` placed at `src/subdir/.rsync-filter` containing
`- /foo` is the canonical case to verify against upstream before relying on
it.

- Workaround: prefer non-anchored patterns inside `.rsync-filter` files;
  use anchored patterns at the top-level rule chain only.
- Fix tracking: MDF-4.

### 3.9 Per-directory rule scope (enter / leave) unverified

The chain's enter-directory and leave-directory hooks are exercised for
single-level entry, but the regression test that pins down "rules leave
scope on directory traversal up" is pending. Behaviour is expected to
match upstream's `pop_filter_list` (`exclude.c:802-803`) but has not been
explicitly asserted.

- Workaround: none required if your filter rule set does not depend on
  rules going out of scope; verify against upstream if you have deep
  per-directory inheritance assumptions.
- Fix tracking: MDF-5.

### 3.10 `.rsync-filter` discovery edge cases

Several edge cases of the per-directory discovery loop are unaudited:
symlinked subdirectories (whether the merge file in a target dir is read
or the symlink is followed), missing or empty merge files (silent skip vs
diagnostic), and races with concurrent directory traversal in the receiver
pipeline.

- Workaround: avoid symlinked subdirectories under transfer roots that
  rely on `.rsync-filter` discovery; ensure every directory either has a
  well-formed `.rsync-filter` or none at all.
- Fix tracking: MDF-6.

## 4. Workaround patterns

Putting the gaps together, the following habits will keep filter rule sets
inside oc-rsync's tested envelope:

- For CVS-style projects, prefer `-C` (or `--cvs-exclude`) on the command
  line over `dir-merge :C .rsync-filter`. The standalone flag inserts the
  full CVS bundle reliably; the modifier form silently drops it (3.1).
- For inheritance control, prefer explicit per-directory `.rsync-filter`
  files over `n`-modified dir-merge directives. Until 3.3 ships, the
  `n` bit does not unwind rules on directory leave.
- For deeply nested filter rule sets (depth 3+), validate the resulting
  decisions against upstream rsync with `--debug=FILTER` on both sides
  before deploying. See section 5 for what is wired today.
- Inside merge files, use space separation between the rule character and
  modifiers (`:n filter` not `:,n filter`); the comma form mis-parses (3.6).
- Use the long-form `--filter='dir-merge,x ...'` instead of `--filter=':x ...'`
  if you need xattr-scoped per-directory rules (3.5).

## 5. Reporting bugs

If you hit a filter-rule discrepancy that is not listed here, please file
an issue under the MDF series with:

- The full `oc-rsync` invocation (or rsync flags reproduced through the
  daemon).
- The source tree layout, abbreviated to the files relevant to the
  discrepancy.
- Every `.rsync-filter` file in scope, plus any external `--filter`,
  `--include-from`, or `--exclude-from` inputs.
- The expected file list (what upstream rsync produced or what you expected
  given the rules) and the actual file list oc-rsync produced.

Diagnostics: oc-rsync accepts `--debug=FILTER` (level 1) and `--debug=FILTER2`
(level 2) on the CLI; both are parsed and recorded on the verbosity config.
Level 1 emits `[Filter] including <path> (matched rule)` and
`[Filter] excluding <path> (matched rule)` to stderr at the decision call
sites in `crates/filters/src/decision.rs`. Level 2 (upstream's `FILTER2`
detail tier) is accepted as a flag but the matching internal call sites that
print rule-merge / per-directory-load detail are not yet wired - the verbose
upstream traces (rule-parse echoes, merge-file load logs, dir-merge
enter/leave traces) are not mirrored at this time. Cross-validate complex
filter rule sets against upstream rsync 3.4.x with `--debug=FILTER1,2,3,4`
when you need that level of detail.

## 6. Cross-references

- [MDF-1 audit](../audit/merge-modifier-coverage.md) - per-modifier
  coverage table vs upstream `exclude.c`.
- MDF-2: parse-side and chain-side fixes for the six concrete gaps listed
  in section 3.1 - 3.6.
- MDF-3: nested merge depth regression coverage (section 3.7).
- MDF-4: anchor-inside-merged-rule semantics regression coverage
  (section 3.8).
- MDF-5: per-directory rule scope (enter / leave) regression coverage
  (section 3.9).
- MDF-6: `.rsync-filter` discovery edge-case coverage (section 3.10).
- MDF-7: complex `.rsync-filter` fixture suite.
- MDF-8: upstream `--debug=FILTER1,2,3,4` diff harness.
- Memory note: `[[project_merge_dir_merge_filter_incomplete]]`.
