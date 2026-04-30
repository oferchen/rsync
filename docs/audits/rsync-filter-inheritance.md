# Audit: `.rsync-filter` per-directory inheritance vs upstream rsync 3.4.1

This audit walks every place where upstream rsync 3.4.1 builds, scopes,
inherits, or evaluates per-directory merge-file rules and maps them to the
oc-rsync Rust code path. The goal is to determine whether the receiver and
sender views of `.rsync-filter` files (and `dir-merge` rules in general)
match upstream, byte for byte, across the matrix of inheritance, no-inherit,
clear-inherit, anchor, and `-F` shorthand combinations.

References:

- `target/interop/upstream-src/rsync-3.4.1/exclude.c`
- `target/interop/upstream-src/rsync-3.4.1/options.c`
- `target/interop/upstream-src/rsync-3.4.1/flist.c`

## 1. Overview

A `.rsync-filter` file is a regular file whose lines are filter rules in the
same syntax used by `--filter=...`. The semantics of "per-directory"
filtering are driven by:

1. A `dir-merge` rule (short prefix `:`, long form `dir-merge`) registered
   in the global filter list. The pattern of the dir-merge rule is the merge
   filename to look up while traversing.
2. The traversal layer (`flist.c::send_file_list` on the sender,
   `generator.c` on the receiver/generator) calling
   `change_local_filter_dir()` whenever it descends into a new directory.
3. `change_local_filter_dir()` calling `push_local_filters()` /
   `pop_local_filters()` to read each registered merge file in the new
   directory and stack its rules with the correct visibility.

Inheritance behaviour is encoded as flags on the dir-merge rule itself:

- `FILTRULE_NO_INHERIT` (modifier `n`): rules from this merge file do not
  propagate into subdirectories.
- `FILTRULE_EXCLUDE_SELF` (modifier `e`): the merge file itself is excluded
  from the transfer.
- `FILTRULE_WORD_SPLIT` (modifier `w`): each whitespace-delimited token in a
  rule line becomes its own rule.
- `FILTRULE_PERISHABLE` (modifier `p`): rules are perishable (skipped during
  delete-excluded).
- `FILTRULE_FINISH_SETUP`: requests `parent_dirscan` for absolute or
  multi-segment merge-file paths.
- `--cvs-exclude` / `-C`: synthesises a built-in dir-merge for `.cvsignore`.
- `-F`: first occurrence expands to `: /.rsync-filter`, the second adds
  `- .rsync-filter`.

The contract that oc-rsync must mirror is:

> When the sender or receiver enters directory D, the active filter list is
> the merge-file list of D plus, in deeper-to-shallower order, the merge-file
> lists of every ancestor that did not declare `n` (no-inherit). When it
> leaves D, the list reverts exactly to the state at the point of entry.

## 2. Upstream semantics

| Concern | Upstream symbol (file:line) | Behaviour |
|---|---|---|
| Mergelist storage model | `exclude.c:85-114` (comment) | One `filter_rule_list` per dir-merge rule. Local rules at the head, inherited rules at the tail. Local list is "switched to inherited" by setting `tail = NULL` on push. |
| Push on directory entry | `exclude.c:759-825` `push_local_filters()` | For every active dir-merge rule, switch local to inherited, optionally drop ancestor list when `FILTRULE_NO_INHERIT` (line 802-803), then call `parse_filter_file()` to read the merge file in the new dir. |
| Pop on directory leave | `exclude.c:827-873` `pop_local_filters()` | Pop the per-mergelist state in reverse, then restore the saved `filter_rule_list` from the snapshot. Frees rules left in the inherited tail when restoring across a `setup_merge_file()` boundary (line 849-859). |
| Scope driver | `exclude.c:875-901` `change_local_filter_dir()` | Maintains a static array `filt_array[cur_depth]`. When the requested depth is shallower than `cur_depth`, pops every level above. Always pushes the new dir on top. |
| No-inherit drop on push | `exclude.c:802-803` | `if (ex->rflags & FILTRULE_NO_INHERIT) lp->head = NULL;`. After this point the deeper directory cannot see *any* rule from the parent merge file. |
| No-inherit drop in `parent_dirscan` | `exclude.c:729-736` | Same drop applied while walking from absolute parent down to current directory. |
| Parent-directory scan for slashed merge names | `exclude.c:686-748` `setup_merge_file()` | If the merge filename contains a `/`, scan every ancestor directory of the transfer root for that file before the first `push_local_filters()`. Required for `: /.rsync-filter` (the `-F` shorthand). |
| Rule parsing inside merge file | `exclude.c:1447-1521` `parse_filter_file()` | Tokenises with `parse_rule_tok`, accepts both short-form (`+ pat`, `-! pat`) and long-form (`include pat`, `exclude pat`) entries. Comment lines start with `;` or `#` (line 1514). Empty lines are skipped. |
| Modifier characters | `exclude.c:1220-1288` (modifier loop) | Recognises `! p s r x e n w C` plus `,` and `_` as terminators. Order is free. |
| `!` clear directive | `exclude.c:1530` (`parse_filter_str`) | Clears the *local* per-directory rules of the merge file currently being parsed; does not affect inherited rules still on the tail. |
| Anchored vs unanchored patterns | `exclude.c:903-1066` `rule_matches()` | A pattern with a leading `/` (after modifier stripping) is anchored to the transfer root. Without `/`, `**` or no slashes, the rule matches the basename. |
| `-F` shorthand expansion | `options.c:1589-1598` | First `-F`: `parse_filter_str(&filter_list, ": /.rsync-filter", rule_template(0), 0)`. Second `-F`: `parse_filter_str(&filter_list, "- .rsync-filter", rule_template(0), 0)`. Third or later `-F`: ignored. |
| Sender vs receiver scoping | `flist.c::send_file_list`, `generator.c`, `flist.c:1583` `f_name()` | Both sides call `change_local_filter_dir()` while walking. Modifier `s` confines a rule to the sender side, `r` to the receiver side. |
| File list scan order | `flist.c:1583-1665` (sender), `generator.c:1300+` (generator) | Pre-order DFS. Merge file is parsed *before* the dir's children are evaluated. |

## 3. oc-rsync implementation map

Two parallel implementations exist:

### 3a. `FilterChain` (network-transfer path)

Used by the daemon and the SSH/TCP client transports via `crates/transfer`.

| Concern | oc-rsync symbol | Lines | Notes |
|---|---|---|---|
| Per-merge-file config | `crates/filters/src/chain.rs::DirMergeConfig` | 53-171 | Stores `inherit`, `exclude_self`, `sender_only`, `receiver_only`, `anchor_root`, `perishable`. |
| Scope stack | `crates/filters/src/chain.rs::FilterChain` | 213-465 | `Vec<DirScope>` keyed by `current_depth`. |
| Enter directory | `crates/filters/src/chain.rs::FilterChain::enter_directory` | 304-379 | Reads each registered merge file, parses, applies modifiers, pushes a `DirScope`. |
| Leave directory | `crates/filters/src/chain.rs::FilterChain::leave_directory` | 381-392 | Pops every scope at the guard's depth. |
| Rule evaluation | `crates/filters/src/chain.rs::FilterChain::evaluate_with_action` / `evaluate_with_action_for_kind` | 252-281 | Walks scopes from innermost to outermost (`scopes.iter().rev()`), then global. First match wins. |
| Parser | `crates/filters/src/merge/parse.rs::parse_rules` | 38-115 | Skips empty lines and `#` / `;` comments. Long-form keywords supported via `try_parse_long_form()`. Short-form via `try_parse_short_form()`. Word-split (`w`) expansion done at parse time. |
| Modifier struct | `crates/filters/src/merge/parse.rs::RuleModifiers` | 135-199 | Recognises `! p s r x e n w C`. Terminators: ` ` and `_`. |
| Reader | `crates/filters/src/merge/read.rs::read_rules`, `read_rules_recursive` | 30-92 | Used for non-per-dir merges. |

### 3b. `local_copy` engine (local-copy path)

Used when both source and destination are local paths (no network).

| Concern | oc-rsync symbol | Lines | Notes |
|---|---|---|---|
| Layered stack with persistent + ephemeral | `crates/engine/src/local_copy/context_impl/transfer.rs::enter_directory` | 30-175 | Honours `inherit_rules()`. `inherit=true` rules go into the persistent layer; `inherit=false` rules go into an ephemeral stack popped on leave. |
| Clear-inherited propagation | `crates/engine/src/local_copy/dir_merge/load.rs::DirMergeEntries::extend` | 73-83 | When a nested merge file contained `!`, propagates `clear_inherited` to all ancestor layers. |
| Dir-merge config flags | `crates/engine/src/local_copy/dir_merge/parse/modifiers.rs` | full file | Same modifier set as the network parser. |

### 3c. CLI plumbing

| Concern | oc-rsync symbol | Lines | Notes |
|---|---|---|---|
| `-F` first/second/third occurrence | `crates/cli/src/frontend/filter_rules/arguments.rs::push_rsync_filter_shortcut` | 95-106 | Mirrors `options.c:1589-1598` exactly: occurrence 0 emits `: /.rsync-filter`, occurrence 1 emits `- .rsync-filter`, occurrence 2+ is ignored. |
| `--cvs-exclude` / `-C` | `crates/cli/src/frontend/filter_rules/cvs.rs` | full file | Adds the standard CVS exclusions plus the `:C` dir-merge for `.cvsignore`. |
| `--filter-from` / `--include-from` / `--exclude-from` | `crates/cli/src/frontend/filter_rules/from_file.rs` | full file | Routed through `merge::read_rules`. |

## 4. Test matrix

The tests below extend `tools/ci/run_interop.sh`. Each scenario builds an
identical fixture, runs upstream rsync 3.4.1 against itself, runs oc-rsync
against itself, then compares the destination tree byte-for-byte.

| # | Name | Fixture | Asserted property |
|---|---|---|---|
| 1 | `rsync-filter-deeply-nested` | 5 levels of `.rsync-filter`, each excluding `level<N>.skip` | Files match the union of every ancestor's exclusion and the local one. |
| 2 | `rsync-filter-anchored-cross-dir` | Top-level `.rsync-filter` with `- /a/b/c.txt`, deeper dirs unaware | Anchored pattern matches one path only, never the same basename in cousin dirs. |
| 3 | `rsync-filter-dir-merge-minus-modifier` | `:- .rsync-filter` (rules-are-excludes) at root | Bare patterns in the merge file behave as excludes. |
| 4 | `rsync-filter-dir-merge-plus-modifier` | `:+ .rsync-filter` (rules-are-includes) at root | Bare patterns in the merge file behave as includes. |
| 5 | `rsync-filter-ff-shorthand` | `-FF` flag alone | `.rsync-filter` is honoured AND excluded from the transfer. |
| 6 | `rsync-filter-mid-tree-only` | Single `.rsync-filter` only in `src/sub/` | Rules apply only at and below `src/sub/`; siblings unaffected. |
| 7 | `rsync-filter-source-root-only` | Single `.rsync-filter` at the transfer root | Rules apply throughout the tree. |
| 8 | `rsync-filter-conflicting-depths` | Root excludes `*.bak`, sub-dir includes `*.bak` | First-match-wins from innermost: deeper include beats parent exclude. |
| 9 | `rsync-filter-empty-noop` | A zero-byte `.rsync-filter` next to other files | Empty file is a no-op; everything transfers. |
| 10 | `rsync-filter-comments-and-blanks` | `.rsync-filter` containing only comments, blanks, and one rule | Only the single rule is honoured; comments/blanks ignored. |
| 11 | `rsync-filter-no-trailing-newline` | `.rsync-filter` without a final `\n` | Last rule is still parsed and honoured. |
| 12 | `rsync-filter-no-inherit-modifier` | `:n .rsync-filter` at root, plus a deeper `.rsync-filter` | Deeper directory does NOT inherit the root's rules. |

Each scenario is wired into the existing standalone-test array in
`run_interop.sh` and contributes one expected-pass entry. Failing scenarios
go to `tools/ci/known_failures.conf` with a citation of both upstream and
oc-rsync code.

## 5. Findings

### Finding 1 (behavioural divergence): `FilterChain::enter_directory` ignores `DirMergeConfig::inherits`

**Severity**: medium for non-cvs, non-CLI users on the network-transfer
code path (daemon push/pull, SSH push/pull). Local-copy is unaffected
because the engine path uses a separate stack model (3b) that honours the
flag.

`crates/filters/src/chain.rs:304-379` reads each registered merge file in
the new directory and unconditionally pushes its rules as a `DirScope`. It
never inspects `DirMergeConfig::inherits()`. The `inherit` flag is stored,
exposed via `inherits()` (line 144-148), and tested by unit tests, but the
production code never reads it. As a result, `:n .rsync-filter` (and any
`with_inherit(false)` config) silently behaves as if it were inheriting:
ancestor rules remain on the scope stack and continue to match in
descendant directories.

Upstream `exclude.c:802-803` drops `lp->head` when `FILTRULE_NO_INHERIT`
is set. The equivalent oc-rsync behaviour would be: when pushing the new
directory's scope for a no-inherit merge config, also mask all ancestor
scopes contributed by that same config.

**Reproducer (functional)**:

```text
src/.rsync-filter   ->  - *.bak
src/sub/.rsync-filter ->  (empty, but the dir-merge rule was registered with `n`)
src/foo.bak           (excluded, correct)
src/sub/foo.bak       (currently excluded; upstream with `:n` includes it)
```

**Recommendation**: route `FilterChain::enter_directory` through the same
"inherit vs ephemeral layer" model already implemented in
`crates/engine/src/local_copy/context_impl/transfer.rs:30-175`, or, more
narrowly, when a config has `inherit=false` and a new scope is pushed,
also push a sentinel that masks ancestor scopes contributed by the same
`config_index` when evaluating from inside the deeper subtree. Add a
regression test that drives `enter_directory` with a no-inherit config
and asserts ancestor rules are not visible.

### Finding 2 (parity, with caveat): `-F` shorthand expansion

`crates/cli/src/frontend/filter_rules/arguments.rs:95-106` mirrors
`options.c:1589-1598` exactly. First `-F` -> `: /.rsync-filter`. Second
`-F` -> `- .rsync-filter`. Third and later `-F` flags are ignored.

Caveat: the leading `/` in `: /.rsync-filter` enables upstream's
`parent_dirscan` (`exclude.c:686-748`), which walks every directory from
the absolute parent of the transfer down to the transfer root and parses
`.rsync-filter` in each. oc-rsync's `FilterChain` does not call any
equivalent of `setup_merge_file()`. In practice this only matters when
the transfer root is several levels deep inside a tree that already has
`.rsync-filter` files above it; for the common case (transfer root is
also the project root, with `.rsync-filter` at that root and below) the
behaviour is identical.

**Recommendation**: track this as a known low-impact gap. Add a regression
test for the case where `.rsync-filter` exists at *and above* the
transfer root.

### Finding 3 (parity): comments, blanks, no trailing newline

`crates/filters/src/merge/parse.rs:38-54` skips lines that, after `trim()`,
are empty or start with `#` or `;`. This matches upstream
`exclude.c:1514`. The parser uses `str::lines()`, which does not require a
trailing newline, so the final line is parsed correctly with or without a
terminator. Tests 9, 10, and 11 confirm parity.

### Finding 4 (parity): clear directive `!`

`crates/engine/src/local_copy/dir_merge/load.rs:73-83` correctly propagates
a nested merge file's `!` to ancestor layers via the `clear_inherited`
flag. `crates/filters/src/chain.rs` represents `!` as a `FilterRule::clear`
inside the per-dir scope; because evaluation iterates scopes
innermost-first and stops at the first match, a clear in a deeper scope
ends evaluation before reaching the parent scope, achieving the same
effect.

### Finding 5 (parity): modifier coverage

`crates/filters/src/merge/parse.rs:174-199` recognises every modifier
upstream's `exclude.c:1220-1288` recognises: `! p s r x e n w C`, plus the
` ` and `_` terminators. Word-split (`w`) is expanded eagerly in the
parser, matching upstream's tokenisation order.

### Finding 6 (architectural): two parallel filter stack implementations

oc-rsync has two independent stack implementations: `FilterChain` (used by
network transfers) and `dir_merge_layers` (used by local copy). They
implement the same upstream contract from different entry points. Finding
1 demonstrates the cost: a feature (`inherit=false`) was added to one
stack but not the other.

**Recommendation**: consolidate. Either move the local-copy engine onto
`FilterChain`, or extract the layered model from
`crates/engine/src/local_copy/context_impl/transfer.rs` into the
`crates/filters` crate and have both transports drive it. This is a
larger refactor and out of scope for this audit.

## 6. Recommendations

1. **Fix Finding 1** by honouring `DirMergeConfig::inherit` in
   `FilterChain::enter_directory`. Pair the fix with a unit test that
   directly exercises the network-transfer path and an interop scenario
   for `:n .rsync-filter`.
2. **Document Finding 2** in `docs/feature_matrix.md` as a known-minor gap
   pending an implementation of `setup_merge_file()`-equivalent
   parent-directory scan.
3. **Plan Finding 6** as a follow-up refactor to remove the dual filter
   stack. The audit does not depend on that work landing.
4. **Add the 12 interop scenarios** from section 4 to the harness so any
   future regressions are caught against upstream 3.4.1.
