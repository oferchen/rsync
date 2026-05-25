# MDF-7 complex `.rsync-filter` fixture

This fixture is the canonical regression baseline for the merge /
dir-merge filter rule family. Every PR that modifies anything under
`crates/filters/` is expected to add a test that consumes this fixture,
or to update the expected lists in this directory in the same PR that
ships the behaviour change.

## Purpose

Each subdirectory under `source/` stresses one or more of the gaps
catalogued in the MDF-1 audit
([`docs/audit/merge-modifier-coverage.md`](../../../../docs/audit/merge-modifier-coverage.md))
and the user-facing status doc
([`docs/user/filter-rules-status.md`](../../../../docs/user/filter-rules-status.md)).
Together they exercise every modifier letter the upstream parser
accepts on a `merge` (`.`) or `dir-merge` (`:`) directive, the
inheritance rules across directory boundaries, and the anchor / scope
semantics inside merged files.

The fixture is data-only. It ships no Rust test code. MDF-2 (issue
#2895) is chartered to wire up the consumer.

## How to consume

The CVS subtree needs a literal `.git/` directory to exercise the
upstream `default_cvsignore` bundle, and git cannot track a nested
`.git/`. The fixture ships `dot-git/` on disk; `materialize.sh`
copies the tree into a working location and applies the rename:

```sh
./materialize.sh tests/fixtures/filter-rules/mdf-7-complex \
    "$WORKDIR/mdf-7-complex"
```

After materialization, the canonical invocation is the per-directory
dir-merge equivalent of `-F`:

```sh
oc-rsync -av \
    --filter='dir-merge /.rsync-filter' \
    "$WORKDIR/mdf-7-complex/source/" \
    "$DEST/"
```

After the transfer:

1. Compare the destination tree against
   [`expected/expected.tree`](expected/expected.tree).
2. Compare the sorted set of transferred paths against
   [`expected/transfer-list.txt`](expected/transfer-list.txt).
3. Verify that none of the paths listed in
   [`expected/exclude-list.txt`](expected/exclude-list.txt) appear in
   the destination tree.

Paths in `transfer-list.txt` and `exclude-list.txt` are relative to the
destination root (the parent of the copied `source/` contents). Both
files are sorted lexicographically; one entry per line; trailing slash
denotes a directory.

The expected lists encode what UPSTREAM rsync 3.4.x produces, not what
oc-rsync currently produces. Several entries will fail today; those
failures are the MDF-2..MDF-6 fix scope.

## Subdirectory guide

Each subdirectory's purpose is summarised here. Every `.rsync-filter`
file inside the fixture carries inline comments naming the modifier or
audit finding under test, so test failures point at the specific
gap.

### `source/` (root)
Sanity layer: blank lines, comments, basic include / exclude prefix
rules. Confirms the canonical
`--filter='dir-merge /.rsync-filter'` invocation discovers and applies
the root rules at all.

### `source/docs/`
Short-form dir-merge `:` with no modifiers, plus a directory-scoped
`- private/` rule and a same-directory `- draft.md`. Tests basic
per-directory rule discovery and recursive subtree exclusion.

### `source/docs/images/`
Nested dir-merge at depth 2 (parent already merged a `.rsync-filter`).
Stresses
[`filter-rules-status.md` section 3.7](../../../../docs/user/filter-rules-status.md)
- nested merge depth coverage. See also `source/deep-nesting/` for
depth 4.

### `source/src/`
One-shot `.` (merge) directive that pulls a sibling rule file
(`.build-excludes`) into the parent chain at parse time. Confirms that
the merged rules apply alongside direct rules below the merge
directive. Upstream reference: `exclude.c:1186-1188`
(`FILTRULE_MERGE_FILE`).

### `source/n-modifier/`
Dir-merge `:n` (non-inheriting). The merge file's rules MUST NOT
inherit into child directories.
Stresses
[`filter-rules-status.md` section 3.3](../../../../docs/user/filter-rules-status.md)
- the `n` modifier ignored on directory leave.
Upstream reference: `exclude.c:1261-1265`, `push_local_filters`
`exclude.c:802-803`.

### `source/e-modifier/`
Dir-merge `:e` (exclude self). The `.rsync-filter` file itself MUST be
excluded from the transfer.
Stresses
[`filter-rules-status.md` section 3.2](../../../../docs/user/filter-rules-status.md)
- `e` not wired through the merge-file parser.
Upstream reference: `exclude.c:1256-1260`.

### `source/w-modifier/`
Dir-merge `:w` (whitespace word-split). The merged file is tokenised
on whitespace rather than newlines. Includes a file name with embedded
spaces to confirm word-split applies to the rule source, not to file
names being matched.
Upstream reference: `exclude.c:1279-1283`, `parse_filter_file`
`exclude.c:1480-1494`.

### `source/cvs/`
Dir-merge `:C` (CVS bundle). The modifier expands into upstream's
`default_cvsignore` set (CVS/, RCS, SCCS, .svn/, .git/, .hg/, .bzr/,
core, `*.o`, `*~`, etc).
Stresses
[`filter-rules-status.md` section 3.1](../../../../docs/user/filter-rules-status.md)
- the `C` bit silently dropped by the merge-file parser.
Upstream reference: `exclude.c:1248-1255`.

### `source/anchored/`
Anchored patterns (leading `/`) inside a merged `.rsync-filter`.
Upstream scopes the anchor to the merge-file's directory, NOT to the
transfer root. The fixture confirms `/anchored-here.txt` matches only
the direct child of `anchored/` and not the deeper sibling under
`anchored/subdir/`.
Stresses
[`filter-rules-status.md` section 3.8](../../../../docs/user/filter-rules-status.md)
- anchor semantics inside merged rules unaudited.

### `source/per-dir-scope/`
Per-directory rule scope - rules contributed by `per-dir-scope/`'s
`.rsync-filter` must go out of scope when the chain leaves the
directory, so a sibling subtree (`sibling/file.txt`) is unaffected.
Stresses
[`filter-rules-status.md` section 3.9](../../../../docs/user/filter-rules-status.md)
- per-directory rule scope unverified.
Upstream reference: `pop_filter_list` `exclude.c:802-803`.

### `source/deep-nesting/`
Depth-4 dir-merge: every level (`l1/`, `l2/`, `l3/`) carries its own
`.rsync-filter` that re-merges. Stresses the recursive
`enter_directory` hook at depth 4. Pairs with `docs/images/` (depth 2)
to bracket the
[`filter-rules-status.md` section 3.7](../../../../docs/user/filter-rules-status.md)
coverage.

### `source/symlinked-rsyncfilter/`
Edge case: the `.rsync-filter` in this directory is a symlink to
`../docs/.rsync-filter`. Audits the receiver's behaviour when it
encounters a symlinked merge file (resolve and apply, or treat as
opaque). Stresses
[`filter-rules-status.md` section 3.10](../../../../docs/user/filter-rules-status.md)
- `.rsync-filter` discovery edge cases.

## Update protocol

The expected lists encode the upstream-compatible target, not the
current oc-rsync behaviour. When a fix from MDF-2..MDF-6 ships:

1. Run the consumer test against this fixture.
2. If the consumer test newly passes a previously-failing assertion,
   the corresponding entry in `transfer-list.txt` /
   `exclude-list.txt` is now satisfied; no update needed.
3. If the consumer test exposes a real divergence that the fix did not
   address, the expected lists must NOT be changed to match - update
   the fix instead.
4. If a fix legitimately changes the upstream-compatible target (e.g.
   a new modifier ships), update the expected lists in the same PR
   that ships the change.

When extending the fixture (adding a new modifier subtree, deeper
nesting, etc.):

1. Add the new subtree under `source/`.
2. Update `expected/transfer-list.txt`, `expected/exclude-list.txt`,
   and `expected/expected.tree` in lockstep.
3. Add a paragraph to the "Subdirectory guide" section above.
4. Cross-reference the audit finding or upstream source line that the
   new subtree stresses inside the new `.rsync-filter`'s inline
   comments.
