# Audit: per-directory merge-file inheritance vs upstream rsync 3.4.1

Status: documentation only - no code changes.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/exclude.c`.

## Scope

This audit narrows in on the lifecycle of per-directory merge files
(commonly `.rsync-filter`, declared with `:` / `dir-merge`):

1. When parent rules are inherited into a child directory and when they
   are reset.
2. The four modifiers that change inheritance shape: `n`, `e`, `w`, and
   the `+` / `-` no-prefix overrides.
3. Parent-dir scanning order for absolute or pathful merge filenames.
4. Pop semantics when leaving a directory.
5. A parity table for the oc-rsync implementation.
6. Edge cases: nested dir-merges, modifier conflicts, missing files.

A separate audit at `docs/audits/rsync-filter-inheritance.md` covers
match-time evaluation order, side modifiers (`s` / `r`), perishability,
and the existing `FilterChain` bugs (Findings 1-3 there). This document
does not duplicate those findings; it focuses on the parsing / push /
pop pipeline.

## Upstream behaviour (rsync 3.4.1)

All line numbers reference `target/interop/upstream-src/rsync-3.4.1/exclude.c`.

### Inheritance model

A `dir-merge` rule owns a `filter_rule_list` that mixes inherited and
local content into a single list with two pointers (lines 85-114):

```
head -> Local1 -> Local2 -> Parent1 -> Parent2 -> NULL
tail ---------^
```

`tail` marks where the local segment ends. Walking from `head` always
sees the deepest directory's rules first, so first-match semantics
mean innermost rules win. This shape is built up step by step:

- On directory entry, `lp->tail = NULL` reclassifies what used to be
  local rules as inherited (line 801).
- Parsing the new merge file appends fresh local items between `head`
  and the inherited segment.
- On directory exit, `pop_filter_list` (line 574) frees only items
  added since the last push, never crossing into the inherited tail.

The bookkeeping is mirrored in `mergelist_parents[]` (lines 76-79),
which keeps one slot per active dir-merge filename. This is what
allows several dir-merges to coexist (e.g.
`--filter=':n .a' --filter=': .b'`) without their stacks colliding.

### Lifecycle: enter / leave a directory

`push_local_filters(dir, dirlen)` (lines 759-825):

1. `set_filter_dir` builds an absolute `dirbuf` ending in `/`
   (lines 657-679).
2. A `local_filter_state` snapshot of every active mergelist's
   `head`/`tail` is allocated (lines 775-785). The push record is the
   value popped later.
3. For each mergelist:
   - `lp->tail = NULL` to convert local rules into inherited rules
     (line 801).
   - If the dir-merge rule has `FILTRULE_NO_INHERIT` (the `n`
     modifier), `lp->head = NULL` discards inherited rules entirely
     for this child directory (lines 802-803).
   - `parse_filter_file` reads the merge file off `dirbuf`
     (lines 811-820). Missing files are silently ignored:
     `parse_filter_file` only emits an error when `XFLG_FATAL_ERRORS`
     is set, which it is *not* for per-dir merges (lines 1476-1485).

`pop_local_filters(mem)` (lines 827-873):

1. Iterates the *current* `mergelist_cnt` (which can be larger than
   the saved count because parsing may declare new dir-merges) and
   calls `pop_filter_list` for each (line 848).
2. For mergelists that exist beyond the saved state, pop again to free
   `parent_dirscan` rules that the new dir-merge inherited
   (lines 849-859).
3. Restore each preserved `head`/`tail` snapshot (lines 865-870).

`change_local_filter_dir(dname, dlen, dir_depth)` (lines 875-901)
maintains a depth-indexed stack of pushes. When the caller backs up the
tree, every entry deeper than the new depth is popped before pushing
the new directory. Passing `dname == NULL` pops everything (the
"transfer end" signal).

### Modifier semantics

Modifiers are parsed in `parse_rule_tok` (lines 1215-1289). The four
that this audit covers:

| Char  | Flag                                          | Meaning on a `dir-merge` rule                                                          |
|-------|-----------------------------------------------|----------------------------------------------------------------------------------------|
| `n`   | `FILTRULE_NO_INHERIT`                         | Discard inherited rules at every child directory (`lp->head = NULL`, lines 802-803).   |
| `e`   | `FILTRULE_EXCLUDE_SELF`                       | Add an exclude rule for the merge file's basename (lines 1409-1418).                   |
| `w`   | `FILTRULE_WORD_SPLIT`                         | Tokenise the merge file on whitespace instead of newlines (lines 1280-1283, 1499-1502).|
| `+`/`-` | `FILTRULE_NO_PREFIXES` (`+` adds `INCLUDE`) | Force every rule in the merge file to act as include (`+`) or exclude (`-`) regardless of `+`/`-` prefixes inside the file (lines 1227-1237). |

Constraints enforced by upstream:

- `n`, `e`, `w` are rejected unless `FILTRULE_MERGE_FILE` is set
  (lines 1257, 1262, 1280); they only make sense on `merge` /
  `dir-merge` directives.
- `+` and `-` cannot be combined (`BITS_SETnUNSET` test at lines
  1228, 1233): `+ -` or `- +` exits with `RERR_SYNTAX`.
- `C` (CVS mode) implies `FILTRULE_NO_PREFIXES | FILTRULE_WORD_SPLIT |
  FILTRULE_NO_INHERIT | FILTRULE_CVS_IGNORE` and is mutually exclusive
  with `+` / `-` and explicit-side prefixes (lines 1248-1255).
- A specified-side merge file may not contain rules that themselves
  specify a side: rejected with `RERR_SYNTAX` (lines 1293-1305).

`FILTRULE_NO_INHERIT` does *not* propagate from the dir-merge
declaration to rules parsed out of the file: the `FILTRULES_FROM_CONTAINER`
mask at lines 1080-1083 omits it. The flag therefore travels with the
declaring directive only, never with individual rules - which is why it
can do its job (zero `lp->head` per push) without polluting the rule
stream.

### Parent-directory pre-scan (`setup_merge_file`)

When a dir-merge filename has a path component or is absolute,
`setup_merge_file` (lines 686-748) walks every directory from the
filename's anchor down to (but not including) the transfer root
*before* the transfer begins:

1. `parent_dirscan = True` is set (line 720).
2. For each parent directory `y`, the candidate merge file is
   constructed and `parse_filter_file` runs against it
   (lines 721-740).
3. After each parse, `lp->tail = NULL` so the rules just appended
   become "inherited" for the next iteration (line 737).
4. If the dir-merge has `FILTRULE_NO_INHERIT`, `free_filters(lp->head)`
   plus `lp->head = NULL` discards the parent rules at every step
   (lines 729-736), keeping only any nested mergelists those rules
   declared (so `pop_local_filters` can tear them down later).
5. `parent_dirscan = False` on exit (line 741).

While `parent_dirscan` is true, `parse_filter_str` adds the rule
literally (without a recursive `parse_filter_file`) at lines 1419-1428;
non-pre-scan operation falls into the recursive branch at lines
1429-1436. This is what stops `setup_merge_file` from infinitely
recursing on a `dir-merge` declared inside a parent's merge file.

### Word splitting (`w`) and no-prefix overrides (`+`/`-`)

Word splitting is implemented inside `parse_filter_file`: when the
template carries `FILTRULE_WORD_SPLIT`, the per-character read loop
breaks on any `isspace(ch)` (lines 1499-1502) so each whitespace-
separated token becomes one rule.

`FILTRULE_NO_PREFIXES` is a parser-side flag: when set on the template,
`parse_rule_tok` skips its own prefix dispatch and treats the entire
line as a pattern. The `+` variant additionally OR-s in
`FILTRULE_INCLUDE`. The combination has two effects:

- A merge file declared `dir-merge,+ NAMES` cannot define exclude
  rules; every line is an include.
- The `!` clear directive is still honoured because clear is detected
  by trailing length (lines 1315-1323), not by line prefix.

## oc-rsync behaviour

Three crates contribute to the dir-merge pipeline:

- `filters` - the `FilterChain` push/pop stack and parsing for filter
  strings.
- `engine` - the local-copy executor's per-directory merge stack
  (`crates/engine/src/local_copy/dir_merge/` and
  `context_impl/transfer.rs`).
- `transfer` - the wire decoder that translates incoming filter rules
  into `DirMergeConfig` instances.

### Parsing modifiers

`crates/filters/src/merge/parse.rs` parses the four modifiers from a
short-form rule prefix:

- `n` -> `RuleModifiers::no_inherit` (line 185).
- `e` -> `RuleModifiers::exclude_only` (line 184). Note: oc-rsync names
  this `exclude_only` in the per-rule struct because the same modifier
  is reused for include/exclude rules; the `FilterRule::with_no_inherit`
  flag carries the dir-merge-level signal.
- `w` -> `RuleModifiers::word_split` (line 186) and is honoured for
  short-form rules at parse time
  (`parse_rule_line_expanded`, lines 83-112).
- `+` and `-` are rule sigils, not merge-file modifiers, in
  `crates/filters/src/merge/parse.rs`. The dir-merge "no prefix"
  override lives in the engine path
  (`crates/engine/src/local_copy/dir_merge/parse/modifiers.rs`).

`crates/engine/src/local_copy/dir_merge/parse/modifiers.rs::parse_merge_modifiers`
recognises the dir-merge modifier set:

- `n` -> `DirMergeOptions::inherit(false)` (line 170).
- `e` -> `DirMergeOptions::exclude_filter_file(true)` (line 160).
- `w` -> `DirMergeOptions::use_whitespace().allow_comments(false)`
  (line 180), matching upstream's tokenisation switch.
- `+` -> `DirMergeEnforcedKind::Include` (line 140).
- `-` -> `DirMergeEnforcedKind::Exclude` (line 130).
- `+` and `-` together return an error (lines 122-128, 132-138)
  matching upstream's `BITS_SETnUNSET` rejection.
- `C` is rejected when combined with `+` (line 142-148), matching
  upstream's CVS-vs-prefix exclusion.
- Plain `merge` directives (non-extended) reject `n`, `e`, `w`, `s`,
  `r`, `/` with a clear error (lines 162-216), matching upstream's
  "modifier requires `FILTRULE_MERGE_FILE`" guards.

### Push: enter a directory

`crates/filters/src/chain.rs::FilterChain::enter_directory`
(lines 304-379):

1. Bumps `current_depth`.
2. For each registered `DirMergeConfig`, joins
   `directory.join(config.filename())` and reads the file. Missing
   files (`NotFound` and `PermissionDenied`) are silently skipped
   (lines 318-330), matching upstream's behaviour at `parse_filter_file`
   lines 1476-1485.
3. Parses with `parse_rules`.
4. Applies `config.apply_modifiers` (lines 350-353): anchor-root,
   perishable, sender/receiver-only.
5. Appends an explicit exclude for the merge filename when
   `config.excludes_self()` is true (lines 355-358), matching upstream
   `exclude.c:1409-1418`.
6. Pushes a `DirScope { depth, filter_set }` if the result is non-empty
   (line 369-372).

`crates/engine/src/local_copy/context_impl/transfer.rs::enter_directory`
(lines 30-175):

1. Pushes a fresh ephemeral frame onto each ephemeral stack
   (lines 56-57).
2. For each declared dir-merge rule, resolves the candidate path with
   `resolve_dir_merge_path` (line 60) - which handles upstream's
   absolute-path-stripped-of-root convention
   (`load.rs:30-38`).
3. `fs::metadata` on a missing file -> `continue` (line 64); on any
   other error -> propagate (lines 65-73). Non-files are skipped
   (line 76).
4. Calls `load_dir_merge_rules_recursive` with a per-call `visited`
   stack to detect merge cycles (`load.rs:115-129`); cycle returns
   `LocalCopyError::io("parse filter file", ...)`.
5. Builds a `FilterSegment` from the parsed rules. If `excludes_self`
   is set, appends a `FilterRule::exclude(filename)` (lines 103-110).
6. If the loader reported `clear_inherited`, drops all parent layers
   for that mergelist index and rolls back any indices already added
   in this push (lines 118-125).
7. Routes the segment based on `inherit_rules()`:
   - inherit: push onto the persistent layer for that index
     (lines 132-134).
   - no-inherit: push onto the per-call ephemeral frame
     (lines 142-145).

This routing mirrors upstream's `lp->head = NULL` choice: persistent
layers play the role of `lp->head`, ephemeral frames are the local
content that disappears on exit.

### Pop: leave a directory

`crates/filters/src/chain.rs::FilterChain::leave_directory`
(lines 389-392) walks `pop_scopes_at_depth(depth)` and decrements
`current_depth`. Scopes are removed by their `depth` field, not by
position, so out-of-order pops still work (line 419-421).

`crates/engine/src/local_copy/context.rs::DirectoryFilterGuard::Drop`
(lines 425-453):

1. Pops the ephemeral frames if `ephemeral_active` is set
   (lines 427-432).
2. Pops `marker_counts` from the marker layers in reverse order
   (lines 434-443).
3. Pops the per-index segment from the persistent layer in reverse
   order (lines 445-452).

Drop ordering matters: the ephemeral frame is popped first because the
matcher checks ephemeral last; popping persistent layers first would
expose stale rules during the brief interval before the ephemeral
frame is gone. Upstream sidesteps this by using a single linked list
per mergelist; oc-rsync's split-stack design is functionally
equivalent if Drop runs to completion, which `Drop` guarantees.

### Wire decoding

`crates/transfer/src/generator/filters.rs:262-301` translates a wire
dir-merge rule into a `DirMergeConfig`:

- Leading `/` on the merge filename -> `with_anchor_root(true)`
  (lines 266-273).
- `n` -> `with_inherit(false)` (line 277).
- `e` -> `with_exclude_self(true)` (line 282).
- `s` / `r` -> `with_sender_only(true)` / `with_receiver_only(true)`.
- `p` -> `with_perishable(true)`.

`w` and the `+` / `-` no-prefix overrides are not represented in
`DirMergeConfig` and therefore are not propagated to remote senders or
receivers via this code path. They are only honoured when the
dir-merge originates locally and is parsed by the engine path. That
gap is logged as Edge Case 4 below.

## Parity table

| Behaviour                                                                | Upstream ref                | oc-rsync ref                                                                               | Status              |
|--------------------------------------------------------------------------|-----------------------------|--------------------------------------------------------------------------------------------|---------------------|
| Push: snapshot mergelist state on directory entry                         | exclude.c:775-785           | filters/src/chain.rs:308-310; engine: context_impl/transfer.rs:50-57                        | match               |
| Push: convert local rules to inherited (`lp->tail = NULL`)               | exclude.c:801               | engine ephemeral-vs-persistent split (transfer.rs:131-152)                                  | match (different shape) |
| Push: `n` discards inherited rules in engine path                         | exclude.c:802-803           | engine: dir_merge/load.rs apply via inherit_rules; transfer.rs:118-125, 131-152             | match               |
| Push: `n` discards inherited rules in `FilterChain`                       | exclude.c:802-803           | filters/src/chain.rs:304-379 (does not consult `config.inherits()`)                         | bug (rsync-filter-inheritance.md Finding 1) |
| Push: `e` adds exclude for merge filename                                 | exclude.c:1409-1418         | filters/src/chain.rs:355-358; engine: context_impl/transfer.rs:103-110                      | match               |
| Push: `w` tokenises merge file on whitespace                              | exclude.c:1280-1283, 1499   | engine: dir_merge/parse/modifiers.rs:178-187, dir_merge/load.rs:151-238                    | match in engine; not exposed via FilterChain |
| Push: `+` / `-` force include / exclude on every rule in file             | exclude.c:1227-1237         | engine: dir_merge/parse/modifiers.rs:121-141 + load.rs:172-178, 267-273                    | match in engine; absent from `DirMergeConfig` |
| Push: `+` and `-` mutually exclusive                                      | exclude.c:1228, 1233        | engine: dir_merge/parse/modifiers.rs:122-138                                                | match               |
| Push: `C` implies `+- w n cvs` and rejects explicit `+`                   | exclude.c:1248-1254         | engine: dir_merge/parse/modifiers.rs:142-156                                                | match               |
| Push: missing merge file silently skipped                                 | exclude.c:1476-1485         | filters/src/chain.rs:318-330; engine: context_impl/transfer.rs:64                           | match               |
| Push: nested dir-merge declared inside a merge file                       | exclude.c:1419-1428         | engine: dir_merge/load.rs:212-234, 283-304 (inline merge); FilterChain does not register a new dir-merge config | partial (engine recurses into `merge`; nested `dir-merge` declarations untracked at runtime) |
| Pop: per-mergelist tail restoration                                       | exclude.c:842-848           | filters/src/chain.rs:419-421; engine: context.rs:425-453                                    | match (different shape) |
| Pop: free `parent_dirscan` segments not in saved state                    | exclude.c:849-859           | engine: visited stack discarded per push; ephemeral frame popped                             | match in engine; FilterChain has no analogue |
| Pop: snapshot restored on exit                                            | exclude.c:865-870           | filters: depth-keyed retain; engine: drained `indices` and `marker_counts`                   | match               |
| Parent-dir pre-scan for absolute / pathful merge filenames                 | exclude.c:686-748           | not implemented (`with_anchor_root` strips root only)                                        | intentional divergence (rsync-filter-inheritance.md Finding 4) |
| Modifier validation: `n`, `e`, `w` rejected on non-merge directives        | exclude.c:1257, 1262, 1280  | engine: dir_merge/parse/modifiers.rs:158-186                                                | match               |
| Specified-side merge containing specified-side rule                       | exclude.c:1293-1305         | engine: filter_program reports a parse error via `FilterParseError`                          | match (rejected at parse time) |
| Cycle detection: merge-file recursion                                     | upstream uses fixed XFLG / depth via fopen reentry checks | engine: load.rs:115-129 explicit `visited` stack                                | divergence (oc-rsync detects cycles upstream does not, returns explicit error) |
| Wire-decoded `w` / `+` / `-` propagated to remote                         | exclude.c:1525-1580 (`get_rule_prefix`) | transfer/src/generator/filters.rs:262-301 (only `n`, `e`, `s`, `r`, `p`, anchor)             | gap (Edge Case 4)   |

## Edge cases

### 1. Nested dir-merges (`: .a` inside `.rsync-filter`)

A merge file may declare a new `dir-merge`. Upstream handles this by
detecting `parent_dirscan == False` at `exclude.c:1419-1428`,
allocating a fresh `mergelist_parents` slot, and letting the next
`push_local_filters` activate the new dir-merge in every descendant.
On exit, `pop_local_filters` runs an extra `pop_filter_list` for the
new mergelist (lines 849-859) to clean up rules accumulated during
`parent_dirscan`.

oc-rsync handles the simpler case correctly: a `merge` directive inside
a merge file is recursively expanded by
`crates/engine/src/local_copy/dir_merge/load.rs:212-234` (whitespace
mode) and lines 283-304 (line mode). The recursion uses the same
`visited` stack to detect cycles.

A `dir-merge` directive parsed inside a `.rsync-filter`, however, is
turned into a `FilterRule::dir_merge` rule
(`crates/filters/src/merge/parse.rs:251-252`) and stored in the rule
list, but neither `FilterChain::enter_directory` nor the engine path
re-registers that rule as a new active dir-merge config for subsequent
directory pushes. Upstream's behaviour - "the rule fires every time
this and any descendant directory is entered" - therefore does not
hold. This is the gap behind Finding 5 in
`rsync-filter-inheritance.md`; mentioned here for completeness.

### 2. Modifier conflicts

Upstream rejects the following at parse time with `RERR_SYNTAX`:

- `+-` / `-+` on a merge directive (lines 1228, 1233).
- `+C` / `-C` (lines 1249-1254 + the `+` / `-` guard).
- `n`, `e`, `w` on a non-merge directive (lines 1257, 1262, 1280).
- A specified-side merge file containing a specified-side rule
  (lines 1293-1305).
- A pure `!` rule with trailing characters (lines 1316-1322).

oc-rsync's engine modifier parser
(`crates/engine/src/local_copy/dir_merge/parse/modifiers.rs:102-233`)
returns a `FilterParseError` for each of the first four cases. The
`!`-with-trailing-characters case is handled at the directive parser
level
(`crates/engine/src/local_copy/dir_merge/parse/dir_merge.rs`) where
the line is split into `clear` plus residue and rejected.

`crates/filters/src/merge/parse.rs::parse_modifiers` is more
permissive: it accepts unknown characters silently, returning the
unconsumed tail as the pattern (lines 192-194). That is acceptable for
short-form rules (`+!p pattern`) where the modifier set is small and
upstream itself short-circuits at the first non-modifier, but it does
mean a typo in a `--filter` argument can be silently absorbed into the
pattern. Filed as a soft divergence; upstream exits cleanly via
`exit_cleanup(RERR_SYNTAX)` (line 1213).

### 3. File-not-found

Both the engine and `FilterChain` paths skip a missing merge file
silently, matching upstream's `parse_filter_file` behaviour at
`exclude.c:1476-1485`. Permission denied is also treated as "skip" in
`FilterChain::enter_directory` (line 321), which is more lenient than
upstream's `fopen` failure cascade (which would return `errno`
unchanged); since per-dir merges set no `XFLG_FATAL_ERRORS`, upstream
also does not abort, so the user-visible behaviour is the same. The
engine path returns `LocalCopyError::io("inspect filter file", ...)`
on permission errors (transfer.rs:65-73), which is stricter than
upstream and surfaces as exit code 23 via the standard error mapping.
This is documented here so the divergence is not papered over.

### 4. `w` and `+` / `-` not propagated over the wire

`crates/transfer/src/generator/filters.rs::dir_merge_config_from_rule`
(lines 262-301) maps the wire-form modifiers to `DirMergeConfig`. It
recognises `n`, `e`, `s`, `r`, `p`, and the anchor-root prefix `/`. It
does *not* recognise `w` or the `+` / `-` no-prefix overrides. As a
result:

- A locally-declared `--filter=':w- skiplist'` parses correctly via
  the engine path, but the same rule received from a remote peer is
  decoded as a plain `: skiplist` and the `w` / `-` are dropped.
- Senders running oc-rsync that produce `dir-merge,+- NAMES` rules
  via `--filter` produce wire output without the modifier letters in
  the `dir-merge` prefix because the rule emitter
  (`crates/protocol/src/filter/emit.rs`, mirrors upstream
  `get_rule_prefix`) only emits `n`, `e`, `s`, `r`, `p`, and the
  anchor `/`.

Whether this matters depends on which peer originates the dir-merge.
For pull transfers where the receiver emits filters and the sender
applies them, both ends compile the rule string before pushing, so
`w` / `+` / `-` survive locally. For push transfers where the sender
sends rules to the receiver to interpret, the gap is observable. No
interop test currently exercises this; recommend covering at least
`:+ NAMES` and `:w names.txt` in
`tests/interop/filters_dir_merge_modifiers.sh`.

## Findings summary

This audit catalogues the parsing / push / pop pipeline rather than
re-litigating the matching-side bugs. The match-time bugs (Findings
1-3 in `docs/audits/rsync-filter-inheritance.md`) remain the highest
priority. New gaps observed here:

1. Wire-decoded dir-merge rules drop `w` and `+` / `-` modifiers.
   Add the missing fields to `DirMergeConfig` and route them through
   to the engine's `DirMergeOptions`. (Edge case 4.)
2. `crates/filters/src/merge/parse.rs::parse_modifiers` silently
   absorbs unknown characters into the pattern. Tighten to match
   upstream's `RERR_SYNTAX` exit. (Edge case 2.)
3. `FilterChain::enter_directory` does not re-register a `dir-merge`
   rule parsed inside a merge file as an active dir-merge config for
   descendant directories. Mirrors the existing Finding 5 in the
   match-time audit; cross-referenced rather than duplicated.

## Tally

- Push/pop behaviours that match upstream: 11.
- Modifier behaviours that match upstream: 7 (`n` engine path, `e`,
  `w` engine path, `+`, `-`, `+/-` exclusion, `C` implications).
- Intentional divergences: 2 (no parent_dirscan, cycle detection
  stricter than upstream).
- Bugs cross-referenced from `rsync-filter-inheritance.md`: 3
  (Findings 1-3 there).
- New gaps: 3 (wire decoder gap, lenient modifier parse,
  nested-dir-merge runtime registration).
