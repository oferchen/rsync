# MDF-4 - anchor-inside-merged-rule wire-byte test

Design spec for regression tests verifying that anchored patterns inside
per-directory merge files scope to the merge-file's directory - not to
the transfer root. This is the single most subtle semantic in upstream
rsync's filter engine and a common source of user confusion.

## 1. Upstream behaviour

### Source of truth

`exclude.c:push_local_filters()` (lines 759-825 in rsync 3.4.1) reads
each per-directory merge file relative to the current directory being
traversed. When rules are parsed from that file, `FILTRULE_ABS_PATH`
(the anchored flag triggered by a leading `/`) scopes to the directory
containing the merge file, not to the transfer root.

Concretely, `exclude.c:rule_matches()` (line 593-680) strips the
transfer-root prefix from the candidate path, then matches the
remaining relative path against the rule. For per-directory rules, the
"remaining relative path" is computed relative to the directory where
the merge file lives. Upstream accomplishes this by maintaining a
`dirbuf` / `predir_len` offset that adjusts the path fed into the
matcher.

### What this means in practice

Given a transfer rooted at `/src/`:

```
/src/
    top.txt
    foo.txt
    subdir/
        .rsync-filter    # contains: - /foo.txt
        foo.txt
        bar.txt
        nested/
            .rsync-filter    # contains: - /baz
            baz
            qux
```

The anchored pattern `/foo.txt` inside `subdir/.rsync-filter` excludes
**only** `subdir/foo.txt` - it does NOT exclude `foo.txt` at the
transfer root. The anchor is scoped to the directory `subdir/` because
that is where the merge file lives.

Similarly, `/baz` inside `subdir/nested/.rsync-filter` excludes only
`subdir/nested/baz`.

An unanchored pattern `foo.txt` (no leading slash) inside
`subdir/.rsync-filter` would match `foo.txt` at any depth under
`subdir/` (including `subdir/nested/foo.txt` if it existed).

## 2. Current oc-rsync state

`FilterChain::enter_directory()` (`crates/filters/src/chain/mod.rs:172`)
reads the merge file and calls `config.apply_modifiers(rule)`. When
`DirMergeConfig::anchor_root` is set, `apply_modifiers` calls
`rule.anchor_to_root()` which prepends `/` to the pattern -
effectively anchoring it to the transfer root.

However, the per-directory scoping mechanism is implicit: paths passed
to `FilterChain::allows()` are transfer-root-relative, and per-dir
scope filter sets evaluate those full paths. Patterns inside a per-dir
merge file that start with `/` (anchored by the user) get compiled
into glob matchers via `normalise_pattern()` which strips the leading
`/` and marks the rule as anchored. The compiled rule then matches only
at position 0 of the path - which is the transfer root, not the merge
file's directory.

This means oc-rsync currently has a semantics gap: an anchored pattern
`/foo` inside `subdir/.rsync-filter` would match `foo` at the transfer
root (position 0 of the path string `foo`) but would NOT match
`subdir/foo` (because `subdir/foo` does not start with `foo`). The
upstream behavior is the opposite.

## 3. Test scenarios

### 3.a) Anchored pattern scoped to merge-file dir

- `/foo` inside `subdir/.rsync-filter`
- Must exclude `subdir/foo`
- Must NOT exclude `foo` at the transfer root
- Must NOT exclude `other/foo`

### 3.b) Unanchored pattern matches everywhere under subdir

- `foo` (no leading slash) inside `subdir/.rsync-filter`
- Must exclude `subdir/foo`
- Must exclude `subdir/deep/foo`
- Must NOT exclude `foo` at the transfer root (per-dir rules only
  apply within their directory scope and below)

### 3.c) Anchored pattern with wildcard scoped to merge dir

- `/foo*` inside `subdir/.rsync-filter`
- Must exclude `subdir/foobar`
- Must exclude `subdir/foo.txt`
- Must NOT exclude `foobar` at the transfer root
- Must NOT exclude `other/foobar`

### 3.d) Nested merge files with independent anchor scoping

- `/keep` inside `a/.rsync-filter`
- `/keep` inside `a/b/.rsync-filter`
- The first scopes to `a/keep`, the second scopes to `a/b/keep`
- Each anchor is independent - they do not interfere

### 3.e) Anchored directory-only pattern inside merge file

- `/build/` inside `project/.rsync-filter`
- Must exclude `project/build/` (directory)
- Must NOT exclude `build/` at root
- Must NOT match `project/build` (file, not directory)

### 3.f) Anchored pattern with internal slash inside merge file

- `/src/tmp` inside `subdir/.rsync-filter`
- Must exclude `subdir/src/tmp`
- Must NOT exclude `src/tmp` at root

## 4. Wire-byte format for anchored patterns in merged context

When filter rules are transmitted over the wire
(`protocol::filters::write_filter_list`), per-directory merge rules are
NOT sent as individual include/exclude rules. Instead, the `dir-merge`
directive itself is transmitted as a `:` rule:

```
len (i32 LE) | ':' | modifiers | ' ' | filename
```

For example, `:e .rsync-filter` on the wire is:

```
00 12 00 00   # length = 18 bytes
3a 65 20      # ':' 'e' ' '
2e 72 73 79 6e 63 2d 66 69 6c 74 65 72  # ".rsync-filter"
00 00 00 00   # terminator (end of filter list)
```

The receiver then reads the named file from each directory during
traversal. The anchored patterns inside those files are never
serialized on the wire - they are parsed locally from the filesystem on
the side that performs the file-list generation (usually the sender).

The wire-byte test therefore validates:
1. The `:` dir-merge directive round-trips correctly with its modifiers.
2. The patterns inside the merge file - read from disk - scope anchors
   to their containing directory, NOT to the transfer root.

### Wire bytes for the dir-merge directive

| Byte offset | Value | Meaning |
|-------------|-------|---------|
| 0-3 | `nn 00 00 00` | Length (LE i32) of payload |
| 4 | `3a` (`:`) | Rule type: dir-merge |
| 5..N | modifier chars | e.g. `65` (`e`), `6e` (`n`) |
| N+1 | `20` (space) | Separator |
| N+2..end | filename bytes | UTF-8 filename |

## 5. Test fixture layout

```
tests/fixtures/filter-rules/mdf-4-anchor-scope/
    source/
        root.txt
        foo.txt
        bar.txt
        subdir/
            .rsync-filter       # "- /foo.txt\n- /build/\n- /src/tmp\n"
            foo.txt
            bar.txt
            build/
                output.bin
            src/
                tmp
                keep.rs
            nested/
                .rsync-filter   # "- /baz\n+ /qux\n- *\n"
                baz
                qux
                other.txt
        project/
            .rsync-filter       # "- /logs*\n"
            logs.txt
            logs-old/
                data.bin
            src.txt
```

### Merge file contents

`source/subdir/.rsync-filter`:
```
- /foo.txt
- /build/
- /src/tmp
```

`source/subdir/nested/.rsync-filter`:
```
- /baz
+ /qux
- *
```

`source/project/.rsync-filter`:
```
- /logs*
```

## 6. Expected assertions

### Scenario 3.a - anchored `/foo.txt` in `subdir/.rsync-filter`

| Path | is_dir | Expected | Reason |
|------|--------|----------|--------|
| `foo.txt` | false | included | Anchor scopes to subdir, not root |
| `subdir/foo.txt` | false | excluded | Anchor matches here |
| `bar.txt` | false | included | No matching rule |
| `subdir/bar.txt` | false | included | Not matched by `/foo.txt` |

### Scenario 3.c - anchored wildcard `/logs*` in `project/.rsync-filter`

| Path | is_dir | Expected | Reason |
|------|--------|----------|--------|
| `project/logs.txt` | false | excluded | `/logs*` matches |
| `project/logs-old` | true | excluded | `/logs*` matches |
| `project/src.txt` | false | included | No match |
| `logs.txt` | false | included | Not in project scope |

### Scenario 3.d - nested merge with its own anchor

| Path | is_dir | Expected | Reason |
|------|--------|----------|--------|
| `subdir/nested/baz` | false | excluded | `/baz` in nested/.rsync-filter |
| `subdir/nested/qux` | false | included | `/qux` included explicitly |
| `subdir/nested/other.txt` | false | excluded | `- *` catch-all |
| `subdir/baz` | false | included | Anchor scoped to nested/ |

### Scenario 3.e - anchored directory-only `/build/` in `subdir/.rsync-filter`

| Path | is_dir | Expected | Reason |
|------|--------|----------|--------|
| `subdir/build` | true | excluded | Directory matches `/build/` |
| `subdir/build/output.bin` | false | excluded | Contents of excluded dir |
| `build` | true | included | Not in subdir scope |

### Scenario 3.f - anchored with internal slash `/src/tmp`

| Path | is_dir | Expected | Reason |
|------|--------|----------|--------|
| `subdir/src/tmp` | false | excluded | Anchored path matches |
| `subdir/src/keep.rs` | false | included | Does not match |
| `src/tmp` | false | included | Not in subdir scope |

## 7. Upstream verification protocol

Run the same fixture tree through upstream rsync 3.4.1 and compare
the transferred file list:

```bash
#!/bin/bash
# Upstream verification script for MDF-4 anchor-inside-merged-rule
set -euo pipefail

UPSTREAM="${UPSTREAM_RSYNC:-rsync}"  # path to upstream rsync 3.4.1+
SRC="tests/fixtures/filter-rules/mdf-4-anchor-scope/source/"
DST=$(mktemp -d)

# Transfer with per-directory merge file support
"$UPSTREAM" -av --filter=':e .rsync-filter' "$SRC" "$DST/" \
    --list-only 2>/dev/null | sort > /tmp/mdf4-upstream.list

# Compare against expected file list
diff -u tests/fixtures/filter-rules/mdf-4-anchor-scope/expected.list \
    /tmp/mdf4-upstream.list

rm -rf "$DST"
```

The `expected.list` file should contain exactly the paths that survive
filtering - no `subdir/foo.txt`, no `subdir/build/`, no
`subdir/nested/baz`, etc.

## 8. Implementation notes

The fix requires `FilterChain::allows()` (and `allows_deletion()`) to
strip the per-directory prefix from the candidate path before matching
against a per-dir scope's rules. Each `DirScope` needs to record the
relative path of the directory it was pushed for, so that anchored
patterns are evaluated against the path suffix after that prefix.

Alternatively, the `enter_directory()` method could rewrite anchored
patterns at parse time by prepending the directory's relative path
(e.g., `/foo.txt` in `subdir/.rsync-filter` becomes
`subdir/foo.txt` anchored to root). This avoids changing the hot-path
`allows()` method but changes the stored pattern.

The upstream approach (adjusting `predir_len` at match time) is more
faithful and avoids pattern duplication. The implementation PR should
choose one approach and document the rationale.

## 9. Acceptance criteria

1. All assertions from section 6 pass in a unit test using
   `FilterChain` with `DirMergeConfig`.
2. The same fixture tree passes the upstream verification script
   (section 7) with zero diff.
3. Wire-format round-trip test: a `:e .rsync-filter` directive
   serializes and deserializes correctly through
   `write_filter_list` / `read_filter_list`.
4. No regression in existing filter tests (full `cargo nextest run -p
   filters --all-features`).
5. The fix does not change behaviour for the `anchor_root` modifier on
   the dir-merge config itself (that anchors ALL patterns to the
   transfer root, a distinct feature from per-pattern `/` anchoring
   inside the file).

## 10. Related work

- MDF-1 audit:
  [`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md) -
  identified the `/` modifier gap in the merge-file parser.
- MDF-3 nested depth test:
  [`docs/design/mdf-3-nested-merge-depth-test.md`](mdf-3-nested-merge-depth-test.md) -
  exercises deep nesting but does not test anchor scoping.
- MDF-8 diff harness:
  [`docs/design/mdf-8-filter-diff-harness.md`](mdf-8-filter-diff-harness.md) -
  the parity-check tool that validates oc-rsync vs upstream output.
- Existing anchor tests:
  `crates/filters/tests/anchored_patterns.rs` - covers transfer-root
  anchoring but not per-dir-scoped anchoring.
- Chain tests:
  `crates/filters/src/chain/tests.rs` - covers push/pop and merge file
  reading but all anchored assertions use transfer-root scope.
