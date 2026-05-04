# Audit: `.rsync-filter` per-directory inheritance vs upstream rsync 3.4.1

Status: documentation only - no code changes.
Tracking issue: #2050.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/exclude.c`.

## Overview

Per-directory merge files (commonly `.rsync-filter`, declared with the `:` /
`dir-merge` filter directive) let users keep filter rules co-located with
the directories they apply to. As rsync descends a directory tree, each
matching merge file is loaded, its rules are pushed onto a stack, and
they are popped when leaving the directory. The stacked semantics make
inheritance, override, and cleanup subtle.

This document compares how oc-rsync handles that lifecycle to upstream
rsync 3.4.1 (the protocol-32 reference). The scope is intentionally
narrow: we audit inheritance, push/pop, and the modifiers that affect
inheritance (`n`, `e`, `p`, `s`, `r`, `!`/`clear`). General filter
matching, wildcard semantics, and side-channel concerns (xattr filters,
`--cvs-exclude`) are out of scope.

The audit was produced by reading both code bases. No tests were run as
part of this audit per the documentation-only contract.

## Upstream behaviour (rsync 3.4.1)

Code: `target/interop/upstream-src/rsync-3.4.1/exclude.c`. All line
references below are absolute line numbers in that file unless noted.

### Data model

Filter rules live on linked lists of type `filter_rule_list`. The list
shape used for per-directory merge files is unusual: the local list is
spliced in front of the inherited list using a tail pointer (lines
85-114). Visualised:

```
head -> L1 -> L2 -> P1 -> P2 -> NULL
tail ----------^
```

`L1`/`L2` are the local directory's rules; `P1`/`P2` are inherited from
parent directories. Iteration always starts at `head`, so deeper-dir
rules are evaluated before parent-dir rules - first-match-wins favours
the innermost scope.

A separate array, `mergelist_parents`, tracks each active per-dir merge
filter rule (lines 76-79). It is the bookkeeping that lets push/pop
operate on multiple merge filenames in parallel (e.g.
`--filter=':n .a' --filter=': .b'`).

### Lifecycle: enter/leave a directory

Two functions wrap directory transitions:

- `push_local_filters(dir, dirlen)` (lines 759-825): called whenever
  rsync changes into a new directory.
  1. Sets `dirbuf` via `set_filter_dir` so `dirbuf` is the absolute
     directory path with a trailing slash (line 657-679).
  2. Saves the current head/tail of every active mergelist into a
     `local_filter_state` push record (lines 779-785). This is the
     snapshot popped later.
  3. For each mergelist:
     - `lp->tail = NULL;` converts any rules that *were* local (in the
       parent dir) into "inherited" rules visible to the child (line 801).
     - If the dir-merge rule has `FILTRULE_NO_INHERIT` (the `n`
       modifier), `lp->head = NULL;` discards inherited rules entirely
       for this child dir (lines 802-803).
     - `parse_filter_file()` reads the merge file from `dirbuf`, parsing
       any rules into the now-empty local segment (lines 811-820).
     - Missing merge files are silently ignored - `parse_filter_file`
       calls `fopen` and only emits an error if `XFLG_FATAL_ERRORS` is
       set, which it is *not* for per-dir merges (lines 1466-1485).

- `pop_local_filters(mem)` (lines 827-873): called when leaving the
  directory.
  1. For every mergelist in *current* `mergelist_cnt` (which may have
     grown if the merge file declared a new dir-merge), call
     `pop_filter_list` (line 574-590) to free local items and restore
     the inherited tail.
  2. For mergelists that exist beyond the saved state (e.g. ones
     introduced inside this directory by setup_merge_file), pop again
     (lines 849-859).
  3. Restore each preserved head/tail snapshot (lines 865-870).

Depth-tracked traversal goes through `change_local_filter_dir`
(lines 875-901), which manages a stack indexed by `dir_depth`. When the
caller backs up the tree the function pops every saved state at deeper
levels before pushing the new one.

### Inheritance modifiers

Modifier characters parsed in `parse_rule_tok` (lines 1215-1289):

| Char    | Flag                                            | Meaning for per-dir merges                                                          |
|---------|-------------------------------------------------|-------------------------------------------------------------------------------------|
| `n`     | `FILTRULE_NO_INHERIT`                           | Discard inherited rules from parent at each child directory (line 1264).            |
| `e`     | `FILTRULE_EXCLUDE_SELF`                         | Exclude the merge file itself from transfer (line 1259, applied at lines 1409-1418). |
| `p`     | `FILTRULE_PERISHABLE`                           | Skipped when `ignore_perishable` is set; used during `--delete-excluded` walks (1267). |
| `s`/`r` | `FILTRULE_SENDER_SIDE`/`FILTRULE_RECEIVER_SIDE` | Side-restricted rules (1270-1278).                                                  |
| `!`     | `FILTRULE_CLEAR_LIST`                           | Drops the entire current list, including inherited rules (lines 1393-1402).         |
| `+`/`-` | `FILTRULE_NO_PREFIXES` (with `+`/`-`)           | Force every rule in the merge file to act as include or exclude (1227-1237).        |
| `w`     | `FILTRULE_WORD_SPLIT`                           | Tokenise the merge file on whitespace (1280-1283).                                  |

`FILTRULE_NO_INHERIT` is the heart of inheritance control. When applied
to a dir-merge rule, every `push_local_filters` call zeroes the
inherited list before parsing the new file (lines 802-803) so the rules
defined in dir A are invisible inside `A/B/`. It is propagated by
`add_rule` because `FILTRULES_FROM_CONTAINER` does *not* include
`FILTRULE_NO_INHERIT` (line 1080-1083); it travels with the dir-merge
declaration, not with each parsed line.

Perishability is checked at match time inside `check_filter`
(line 1043-1051): any rule with `FILTRULE_PERISHABLE` is skipped when
`ignore_perishable` is set. Inherited perishable rules behave
identically to local perishable rules.

### Match-time evaluation

`check_filter` (lines 1038-1065) walks `head -> next -> ...` and:

- Skips perishable rules under `ignore_perishable`.
- For a `FILTRULE_PERDIR_MERGE` entry, recurses into that mergelist's
  rules via `check_filter(ent->u.mergelist, ...)` (lines 1046-1051) and
  short-circuits on a match.
- For any other matching rule, returns immediately with `+1` (include)
  or `-1` (exclude). First match wins.

Because the local list is spliced in front of the inherited list, the
*innermost* directory's rules always evaluate first, and child rules
override parent rules without explicit precedence logic.

### Parent-directory pre-scan for absolute merge paths

`setup_merge_file` (lines 686-748) handles a corner case: when a
dir-merge rule names a file with an absolute path (e.g.
`:.rsync-filter` rooted under `/etc`) or a path component, rsync walks
every parent of the transfer root and parses the merge file in each one
*before* the transfer begins. The pre-scan toggles
`parent_dirscan = True` so `parse_merge_name` rewrites paths
appropriately (lines 599-654), and any inherited rules are kept across
parents unless `FILTRULE_NO_INHERIT` clears them (lines 729-736).

This feature is what allows a single `.rsync-filter` at `/srv/data/` to
apply to a transfer rooted at `/srv/data/projects/foo/`.

## oc-rsync behaviour

The implementation is split across three crates:

1. `filters` (parsing, `FilterChain`, `FilterSet`, `DirMergeConfig`).
2. `transfer` (sender-side generator that calls
   `FilterChain::enter_directory` / `leave_directory`).
3. `engine` (the local-copy executor with its own per-directory merge
   stack used for non-network copies).

### Parsing and the rule model

`crates/filters/src/merge/parse.rs:174-199` parses modifier characters
in lock-step with upstream's `exclude.c:1215-1289`. All of `!`, `p`,
`s`, `r`, `x`, `e`, `n`, `w`, `C` are recognised, and the
`RuleModifiers` struct mirrors `FILTRULE_*` flags one-to-one.

`crates/filters/src/rule.rs:63-99` defines the in-memory `FilterRule`
with the same flags (`perishable`, `no_inherit`, side bits, etc.). The
no-inherit flag is settable per-rule via
`FilterRule::with_no_inherit(true)` (line 429).

### `FilterChain` push/pop

`crates/filters/src/chain.rs` is the per-directory stack:

- `DirMergeConfig` (`chain.rs:53-171`) carries a per-config
  `inherit: bool` field configurable via `with_inherit(false)`. The doc
  comment at `chain.rs:90` calls out that this is the `n` modifier.
- `FilterChain::enter_directory` (`chain.rs:304-379`) reads each
  configured merge file from the directory, parses it, applies global
  modifiers from the `DirMergeConfig`, and pushes a `DirScope` onto the
  stack. Missing files are skipped silently (`chain.rs:318-330`),
  matching upstream.
- `FilterChain::leave_directory` (`chain.rs:389-392`) calls
  `pop_scopes_at_depth` and decrements `current_depth`.
- `FilterChain::allows` and `allows_deletion` (`chain.rs:258-282`)
  evaluate scopes from innermost (`scopes.iter().rev()`) outward, then
  fall through to the global `FilterSet`. Only scopes with a matching
  rule short-circuit; otherwise evaluation falls through.

The matching helper `has_matching_rule` (`chain.rs:450-463`) detects a
match by checking `allows`/`allows_deletion` against their default
behaviour. The function explicitly returns `false` for include-only
scopes (`chain.rs:459-462`) on the grounds that an include rule alone
cannot be distinguished from "no rule" because both default to allow.

### Engine-side merge stack

`crates/engine/src/local_copy/context_impl/transfer.rs:30-175` is a
parallel implementation used by the `engine` crate's local-copy
executor. Its design is closer to upstream's `mergelist_parents`:

- A separate stack per dir-merge index, with explicit "inherited" and
  "ephemeral" segments (`transfer.rs:50-152`).
- The `inherit_rules()` flag is consulted at `transfer.rs:118` and
  `131-152` and selects between the persistent layer and the per-call
  ephemeral stack. When `inherit_rules()` is false, parsed rules go on
  the ephemeral stack and are dropped when the directory is left.
- Clear directives (`!`/`clear`) clear inherited layers via
  `clear_inherited` (`transfer.rs:114-125` and
  `crates/engine/src/local_copy/dir_merge/load.rs:55-83`).
- Markers from `--exclude-if-present` are tracked in parallel layers.

The generator-side filter stack, used during file-list construction, is
the `FilterChain` from the `filters` crate
(`crates/transfer/src/generator/file_list/walk.rs:74-86, 180-193`). It
is *also* called for the receiver side via the multiplex path that
hands a `FilterChain` to the receiver.

### Wire decoding into `DirMergeConfig`

`crates/transfer/src/generator/filters.rs:262-301` translates the wire
form of a dir-merge rule into a `DirMergeConfig`. The mapping is:

- Leading `/` on the merge filename: stored as `with_anchor_root(true)`
  (lines 266-273). Note: oc-rsync does *not* implement upstream's
  parent-dir pre-scan; the anchor flag only affects matching.
- `n`: `with_inherit(false)` (line 277).
- `e`: `with_exclude_self(true)` (line 282).
- `s`/`r`: `with_sender_only` / `with_receiver_only`.
- `p`: `with_perishable(true)`.

## Comparison

The table reads "match" when oc-rsync mirrors upstream behaviour to the
level of fidelity required for the protocol-32 reference, "intentional
divergence" when oc-rsync deliberately differs, "bug" when oc-rsync
unintentionally differs, and "untested" when behaviour exists but is
not covered by tests.

| Behaviour                                                       | Upstream ref                                | oc-rsync ref                                                                                            | Status                                                       |
|-----------------------------------------------------------------|---------------------------------------------|---------------------------------------------------------------------------------------------------------|--------------------------------------------------------------|
| Per-dir merge file read on directory entry                      | exclude.c:759-825                           | filters/src/chain.rs:304-379, transfer/src/generator/file_list/walk.rs:74,180                           | match                                                        |
| Missing merge file silently ignored                             | exclude.c:1476-1485                         | filters/src/chain.rs:318-330                                                                            | match                                                        |
| Stack restored on directory exit                                | exclude.c:827-873                           | filters/src/chain.rs:389-392, 419-421                                                                   | match                                                        |
| Innermost rules evaluated first                                 | exclude.c:1043-1062 (head order)            | filters/src/chain.rs:259-266 (`scopes.iter().rev()`)                                                    | match                                                        |
| First-match-wins within a scope                                 | exclude.c:1058-1061                         | filters/src/set.rs:143-147 via decision.rs                                                              | match                                                        |
| Perishable rules skipped under `ignore_perishable`              | exclude.c:1043-1045                         | filters/src/decision.rs:155-161                                                                         | match                                                        |
| `e` modifier excludes the merge file itself                     | exclude.c:1409-1418                         | filters/src/chain.rs:355-358                                                                            | match                                                        |
| `s`/`r` side modifiers honoured                                 | exclude.c:1270-1278, 1605-1612              | filters/src/chain.rs:107-122, 158-168; filters/src/decision.rs                                          | match                                                        |
| `n` modifier discards inherited rules in `FilterChain`          | exclude.c:802-803                           | filters/src/chain.rs:304-379 (no use of `config.inherits()`)                                            | bug (Finding 1)                                              |
| `n` modifier discards inherited rules in engine local-copy      | exclude.c:802-803                           | engine/src/local_copy/context_impl/transfer.rs:118, 131-152                                             | match                                                        |
| `!` / `clear` directive in merge file drops inherited rules     | exclude.c:1393-1402                         | engine path: dir_merge/load.rs:55-83 + transfer.rs:114-125                                              | match in engine                                              |
| `!` / `clear` directive in `FilterChain` per-dir scope          | exclude.c:1393-1402                         | filters/src/chain.rs (no scope-clearing path)                                                           | bug (Finding 2)                                              |
| Include-only per-dir merge can override parent excludes         | exclude.c:1058-1061 (any match wins)        | filters/src/chain.rs:450-462 (`has_matching_rule` ignores include-only)                                 | bug (Finding 3)                                              |
| Parent-dir pre-scan for absolute / pathful merge filenames      | exclude.c:686-748                           | not implemented                                                                                          | intentional divergence (Finding 4)                           |
| Anchored merge filename (leading `/`) found only at xfer root   | exclude.c:686-748                           | DirMergeConfig::with_anchor_root, partial - no parent_dirscan logic                                     | intentional divergence (Finding 4)                           |
| Per-dir merge declared inside a per-dir merge (nested dir-merge)| exclude.c:1419-1428 (parent_dirscan branch) | filters/src/chain.rs adds rules but does not register a new dir-merge config at runtime                 | untested (Finding 5)                                         |
| Newly-introduced mergelists popped on directory exit            | exclude.c:849-859                           | filters/src/chain.rs:419-421 (single depth-keyed pop)                                                   | match for declared configs only; gap matches Finding 5       |
| Tail-splice inheritance preserves shared parent rules           | exclude.c:85-114                            | Cloned `FilterSet` per scope; per-scope copy avoids the splice issue                                    | intentional divergence (cost trade-off, behaviourally equivalent) |
| Word-split (`w`) merge files                                    | exclude.c:1280-1283                         | engine: dir_merge/load.rs:122-209; filters/merge/parse.rs:144                                           | match in engine, untested in `FilterChain`                   |
| `+`/`-` no-prefix merge modifiers                               | exclude.c:1227-1237                         | filters/src/merge/parse.rs (modifier struct only)                                                       | untested                                                     |

## Findings

### Finding 1 - `FilterChain::enter_directory` ignores `DirMergeConfig::inherit`

Severity: bug.

`DirMergeConfig` exposes `with_inherit(bool)` and stores the field in
`crates/filters/src/chain.rs:57-93`, and the wire decoder
(`crates/transfer/src/generator/filters.rs:276-278`) sets it correctly
when the `n` modifier is received. However,
`FilterChain::enter_directory` at `crates/filters/src/chain.rs:304-379`
never consults `config.inherits()` when pushing a new scope. As a
result, parent-dir rules from a dir-merge declared with `n` continue to
shadow the child directory's rules even though upstream zeroes
`lp->head` at `exclude.c:802-803`.

Reproducer (conceptual): a sender configured with
`--filter=':n .rsync-filter'`, where `top/.rsync-filter` says `- *.tmp`
and `top/sub/.rsync-filter` is missing or empty, will exclude
`top/sub/foo.tmp` in oc-rsync but include it in upstream rsync. The
engine path handles this correctly via
`crates/engine/src/local_copy/context_impl/transfer.rs:118, 131-152`,
so the bug is specific to network transfers driven by the generator.

Recommendation: in `enter_directory`, if `!config.inherits()`, drop all
previously-pushed scopes that originated from the same merge config
before pushing the new one. Add a unit test in
`crates/filters/src/chain.rs:tests` and an integration test under
`crates/filters/tests/filter_chain_edge_cases.rs` covering the `n`
modifier with at least one parent and one child directory.

### Finding 2 - `!` / `clear` directive inside a `.rsync-filter` does not clear inherited rules from `FilterChain` scopes

Severity: bug.

`crates/filters/src/merge/parse.rs:313-315` parses `!` / `clear` into a
`FilterRule::clear()`, and `FilterSet` honours it for rules in the same
file via `crates/filters/src/set.rs:102-113` (clear inside the same
compile pass). But when `enter_directory` parses a merge file
containing `!`, the resulting cleared rules only affect that single
dir's `FilterSet`. They do not pop or invalidate previously-pushed
scopes.

Upstream's `parse_filter_str` at `exclude.c:1393-1402` calls
`pop_filter_list(listp)` and `listp->head = NULL` immediately, which
removes inherited content along with any local content already in the
list.

The engine-side path mirrors upstream by tracking `clear_inherited` and
clearing parent layers in `transfer.rs:114-125`. The `FilterChain`
path lacks this hook. The two crates therefore diverge on the same
input.

Recommendation: thread a `clear_inherited` signal out of `parse_rules`
(or expose the rule list to chain so it can detect a clear) and, when
seen, call `pop_scopes_at_depth(d)` for every depth `d <= current_depth`
that was pushed by the same dir-merge config.

### Finding 3 - Include-only per-dir merge files cannot override parent excludes in `FilterChain`

Severity: bug.

`has_matching_rule` (`crates/filters/src/chain.rs:450-462`) detects a
match by checking whether `allows`/`allows_deletion` deviates from
default. Because the default is allow, an include-only scope can never
"deviate", so evaluation falls through to the next outer scope. The
inline comment at `chain.rs:459-461` acknowledges this and labels it
"matching upstream rsync's per-directory rule prepend semantics", which
is incorrect: upstream returns `+1` for any matching include rule and
stops evaluation at `exclude.c:1058-1061`.

In practice this means a child `.rsync-filter` containing only
`+ *.txt` cannot rescue files that an outer scope (or the global
`FilterSet`) excludes. Upstream would honour the include because the
local list is spliced in front of the inherited list.

Recommendation: rework `has_matching_rule` to use a tri-state -
`Match::Include`, `Match::Exclude`, `Match::None` - by walking the
compiled rule list once and returning whether any rule actually fires,
not by inferring it from the boolean `allows`. Tests should cover an
include-only child overriding an exclude in (a) the global rules and
(b) an outer scope.

### Finding 4 - No parent-dir pre-scan for absolute or pathful dir-merge filenames

Severity: intentional divergence (documented).

Upstream's `setup_merge_file` and the `parent_dirscan` flag
(`exclude.c:686-748, 70-72`) walk parent directories of the transfer
root and parse merge files there before the transfer begins. oc-rsync
treats `with_anchor_root(true)` purely as a "look in transfer root"
flag; there is no scan of `..`, `../..`, etc.

The omission is consistent with current scope: upstream uses the
pre-scan only when a dir-merge filename contains a slash or is
absolute. We do not advertise capability for pathful dir-merge filenames
in any released CLI, and no interop test currently exercises it.

Recommendation: keep the divergence but document it in
`docs/filter_compat.md` (or wherever interop notes live) and add a
regression test that confirms a pathful dir-merge name is rejected with
a clear error rather than silently succeeding.

### Finding 5 - Nested dir-merge declarations inside a per-dir merge file are untested

Severity: untested.

Upstream supports a `.rsync-filter` that itself declares another
dir-merge, e.g. a top-level merge file containing `: .more-filters`
which then activates `.more-filters` lookups in every sub-directory.
The handling lives at `exclude.c:1419-1428` (parent_dirscan path) and
the new mergelist is registered in `mergelist_parents` so
`pop_local_filters` will tear it down on exit (lines 849-859).

oc-rsync's `FilterChain` does parse a nested `DirMerge` rule via
`crates/filters/src/merge/parse.rs:251-252`, but
`FilterChain::enter_directory` does not register newly-introduced
dir-merge configs. Whether the resulting behaviour matches upstream
under realistic inputs is unverified.

Recommendation: add an integration test under
`crates/filters/tests/dir_merge_rules.rs` that constructs a directory
tree with a top-level `.rsync-filter` containing `: .extra` and a
nested `.extra` to verify the rules are picked up. If the test fails,
file a follow-up bug.

### Additional notes

- The tail-splice inheritance trick at upstream `exclude.c:85-114` is
  not used. oc-rsync clones each scope's `FilterSet` independently.
  This is more expensive memory-wise but behaviourally equivalent for
  evaluation, since the chain iterates scopes in deepest-first order
  before falling back to the global set. Listed as an intentional
  divergence; not a bug.
- The engine path
  (`crates/engine/src/local_copy/context_impl/transfer.rs`) duplicates
  much of the per-directory state machine. The two stacks have diverged
  on Findings 1, 2, and 3. Long-term, consider unifying on
  `FilterChain` so a single fix covers both call sites.

## Recommendations summary

1. Honour `DirMergeConfig::inherit` in `FilterChain::enter_directory`
   (Finding 1). Add unit + integration tests.
2. Propagate `!` / `clear` from a parsed merge file out to
   `FilterChain` so prior scopes are popped (Finding 2).
3. Replace `has_matching_rule` with a real "did any rule fire" probe so
   include-only per-dir merges can override outer excludes (Finding 3).
4. Document the parent-dir pre-scan gap and reject pathful dir-merge
   names with a clear error (Finding 4).
5. Add a regression test for nested dir-merge declarations
   (Finding 5).
6. Plan a future refactor that unifies the engine's per-directory merge
   stack with `FilterChain` to prevent further drift between the two
   implementations.

## Tally

- Matches: 9
- Intentional divergences: 3 (parent_dirscan, anchored-root semantics,
  tail-splice memory model)
- Bugs identified: 3 (Findings 1, 2, 3)
- Untested behaviours: 4 (Finding 5; word-split through FilterChain;
  no-prefix `+`/`-` merge modifiers in FilterChain; nested dir-merge
  declaration teardown)
