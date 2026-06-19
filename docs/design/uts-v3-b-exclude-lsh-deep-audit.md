# UTS-V3.B exclude-lsh deep-audit + fix-scope decision

Audit task: UTS-V3.B.F (parent: UTS-V3.B, task #4304). Covers tasks
#4386-#4398. Audit-only - this doc does not change behavior.

Source under test: upstream rsync 3.4.4 `testsuite/exclude.test`
(invoked through `exclude-lsh.test` symlink which forces
`RSYNC_RSH=$scratchdir/src/support/lsh.sh` and `host='lh:'`). Symptom:
oc-rsync over-deletes on the `--delete-during` leg despite PR #5749
(descendants suppression for directory-only unanchored wildcards) and
PR #5751 (`normalise_recursive_wildcards`) shipping. See
`target/interop/upstream-src/rsync-3.4.4/testsuite/exclude.test:1` and
`exclude-lsh.test` symlink target.

## 1. Filter rule and transfer-leg matrix

The upstream `exclude.test` script builds eight distinct filter inputs
and exercises seven `$RSYNC` invocations whose outputs feed the
`checkit` and `checkdiff` harnesses. The `-lsh` variant runs every
remote-shell-eligible invocation through `lsh.sh`.

### 1.a. Filter rules from `$excl` (exclude-from)

Source: `exclude.test:84-99`. The exclude file lives at
`$scratchdir/exclude-from` and is the rule-set under test for the
`--delete-during` leg:

| Line | Pattern        | Anchored | Dir-only | Action  | Notes                                           |
|------|----------------|----------|----------|---------|-------------------------------------------------|
| 84   | `!`            | -        | -        | reset   | clears predecessors (none here)                 |
| 86   | `+ **/bar`     | no       | no       | include | leading `**` (per #5751 no extra `/` prefix)    |
| 87   | `- /bar`       | yes      | no       | exclude | "if line 87 does anything it's a bug"           |
| 89   | `+ foo**too`   | no       | no       | include | bare `**`; PR #5751 rewrites to `foo/**/too`    |
| 91   | `+ foo/s?b/`   | no       | yes      | include | dir-only unanchored wildcard                    |
| 92   | `- foo/*/`     | no       | yes      | exclude | dir-only unanchored wildcard                    |
| 94   | `- new/keep/**`| no       | no       | exclude | bare `**` suffix; trailing `**`                 |
| 95   | `- new/lose/***`| no      | yes      | exclude | `/***` -> directory-only + descendants          |
| 97   | `+ t[o]/`      | no       | yes      | include | bracket-class dir-only                          |
| 98   | `- to`         | no       | no       | exclude | plain literal                                   |
| 99   | `+ file4`      | no       | no       | include | plain literal                                   |
| 100  | `- file[2-9]`  | no       | no       | exclude | bracket-class                                   |
| 101  | `- /mid/for/foo/extra` | yes | no    | exclude | anchored literal                                |

### 1.b. Per-directory `.filt` and `.filt2` merge files

Built at `exclude.test:43-78`. These are picked up by the
`-f dir-merge_.filt` / `:s_.filt` directives later. The cluster matters
because PR #5749's "directory-only unanchored wildcards suppress
descendants" gate must coexist with these:

| File location              | Rule body                                        |
|----------------------------|--------------------------------------------------|
| `$fromdir/.filt`           | `exclude down` / `: .filt-temp` / `clear` / `- .filt` / `- *.bak` / `- *.old` |
| `$fromdir/foo/.filt`       | `include .filt` / `- /file1`                     |
| `$fromdir/bar/.filt`       | `- home-cvs-exclude` / `dir-merge .filt2` / `+ to` |
| `$fromdir/bar/down/to/.filt2`     | `- .filt2`                               |
| `$fromdir/bar/down/to/foo/.filt2` | `+ *.junk`                               |
| `$fromdir/bar/down/to/bar/.filt2` | `- *.deep`                               |
| `$fromdir/mid/.filt2`      | `- extra` (the test asserts this is INEFFECTUAL) |
| `$fromdir/mid/.filt`       | `:C`                                             |

### 1.c. Transfer legs (every `$RSYNC` invocation)

Citations from `exclude.test`. "Affected by --lsh" means lsh.sh is on
the transport because `*-lsh.*` matches `$0`.

| Leg | Line | Mode             | Filters                                              | Affected by --lsh |
|-----|------|------------------|------------------------------------------------------|-------------------|
| L1  | 102-103 | `--prune-empty-dirs` checkit | `-f -_foo/too/ -f -_foo/down/ ...`        | Yes               |
| L2  | 116  | `checkit -avv`   | (build chk-tree skeleton)                            | Yes               |
| L3  | 132  | `--existing`     | `--include='*/' --exclude='*'`                       | No (chk prep)     |
| L4  | 136-137 | **`checkit --delete-during`** | **`--exclude-from='$excl'`**                 | **Yes** (failing) |
| L5  | 149-150 | `checkit --delete-during` | `--filter='merge $excl' -f:C -f-C --delete-excluded` | Yes               |
| L6  | 154  | `--existing`     | `-f 'show .filt*' -f 'hide,! */' --del`              | Yes               |
| L7  | 165-167 | `checkit --delete-during` | piped exclude-from `sed '/!/d'` -> `dir-merge_.filt` + `merge_-` | Yes |
| L8  | 178-180 | `checkit --delete-before` | `-f :s_.filt -f .s_- -f P_nodel.deep` | Yes               |
| L9  | 192-193 | `checkit`        | `-f dir-merge,-_.excl`                               | Yes               |
| L10 | 198-200 | `checkit relative_opts` | `--exclude='$fromdir/foo/down'`                | Yes               |

Total filter directives across legs: 10 distinct rule combinations
operating on the 8 merge files above + the 13-rule `$excl` file =
~21 filter inputs evaluated under 10 transfer legs.

## 2. Likely failing sub-transfer

**L4** (`exclude.test:136-137`) is the highest-likelihood failure for
the symptom described:

```sh
$RSYNC -avv$rpath --exclude-from='$excl' \
    --delete-during '$host$fromdir/' '$todir/'
```

Hypothesis: under `--delete-during` the receiver walks `$todir/` and
for each path consults the rule set in 1.a against
`DecisionContext::Deletion`. The interesting rules collide with the
`.filt` and `.filt2` files that the chk-tree has copied into the
receiver in L2 - the bug shape is "files that should be retained on
the receiver get deleted". Strong indicators that L4 is the failing
leg:

- L4 is the first `--delete-during` invocation in the script.
- Its exclude file mixes `+ /bar/`-style inclusion with `- /bar`
  exclusion at line 86-87 of the exclude file, exactly the pair
  PR #5751's `normalise_recursive_wildcards` reshapes.
- The `+ foo/s?b/` rule and the `- foo/*/` rule at lines 91-92
  collide at `foo/sub/` - this is the path #5749 redirected to "no
  synthetic descendants for directory-only unanchored wildcards"
  (see `crates/filters/src/compiled/mod.rs:108-111`).
- L5 wraps the same exclude file in `merge` and adds
  `--delete-excluded`, which is harder to fail in isolation unless L4
  already fails - so L4 stays the upstream "first divergence."

L7 and L8 are secondary candidates because they exercise the per-
directory `.filt`/`.filt2` merges plus a `dir-merge` rule that pushes
descendant scope per directory. Their failure would still trace back
to descendant logic, just expressed via per-dir scopes.

## 3. Descendants logic citation and Deletion-path interaction

Source: `crates/filters/src/compiled/mod.rs:72-121`. Build path:

```text
72  let mut descendant_patterns = HashSet::new();
...
98  let has_glob_wildcard =
99      core_pattern.contains('*') || core_pattern.contains('?') || core_pattern.contains('[');
100 let slash_anchored = pattern.starts_with('/');
108 let is_directory_only_unanchored_wildcard =
109     directory_only && !slash_anchored && has_glob_wildcard;
110 let is_anchored_wildcard = slash_anchored && has_glob_wildcard;
111 let suppress_descendants = is_directory_only_unanchored_wildcard || is_anchored_wildcard;
112 if matches!(
113     action,
114     FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
115 ) && !suppress_descendants
116 {
117     descendant_patterns.insert(format!("{core_pattern}/**"));
118     if !anchored && !has_double_star {
119         descendant_patterns.insert(format!("**/{core_pattern}/**"));
120     }
121 }
```

Cross-reference with the Deletion path in
`crates/filters/src/decision.rs:47-101`. Two switches matter:

- `decision_with_traversal(..., traversal: bool)` flips
  `check_descendants = !traversal` (line 69). For
  `DecisionContext::Deletion`, the caller
  `FilterSetInner::decision_with_traversal` is invoked with
  `traversal: true` only from `set.rs:240` (a tree-walk variant) and
  `traversal: false` from `set.rs:220` and `set.rs:255` (single-path
  variants). The deletion-during walk in
  `crates/transfer/src/receiver/directory/deletion.rs:226` runs
  through `FilterChain::allows_deletion` which calls
  `allows_deletion_during_traversal` (chain/mod.rs:200), so descendants
  ARE suppressed for the walked tree. Good.
- But for the chain's "scope-applies" predicate
  (`scope_has_deletion_match` -> `has_matching_rule`,
  decision.rs:160-192), descendant matchers are unconditionally
  skipped via `check_descendants: false` on line 177 and line 188.

The compiled-rule gate at `compiled/mod.rs:108-115` was designed for
*Transfer* (sender walk) - upstream `exclude.c:rule_matches()` returns
"no match" for `FILTRULE_DIRECTORY` against a non-dir entry (cited at
lines 87-97 of `compiled/mod.rs`). For Deletion the receiver pass
through `allows_deletion` already skips synthetic descendants per the
`traversal` switch.

Result: the descendant suppression for `is_directory_only_unanchored_wildcard`
is correct in spirit but redundant on the Deletion path - and worse,
it is unconditional, so it also kills the receiver's *single-path*
deletion query (set.rs:220) used by the global-rules tail of
`chain.allows_deletion`. That means a rule like `- foo/*/` cannot
fire against `foo/sub/file1` from outside the tree walk - exactly the
"over-deletion" symptom seen.

## 4. `normalise_recursive_wildcards` citation and per-pattern trace

Source: `crates/filters/src/compiled/pattern.rs:79-144`. The function:

- Returns `Cow::Borrowed(pattern)` early if no `**` present
  (line 80-82). So `+ /bar/`, `- /bar`, `+ foo/s?b/`, `- foo/*/`, and
  `+ t[o]/` are all UNTOUCHED by this function.
- Only patterns with `**` (line 89-94 of the exclude file:
  `+ foo**too`, `- new/keep/**`, `- new/lose/***`) are rewritten.
- `- new/lose/***` is rewritten via `normalise_pattern`
  (pattern.rs:166-209) to dir-only `new/lose` first - the
  `normalise_recursive_wildcards` pass never sees `***` because the
  caller strips `/***` upstream (pattern.rs:172-174).

Per-pattern trace:

| Source pattern   | Has `**` | Normalised form        | Direct matchers added                  | Descendants added           |
|------------------|----------|------------------------|----------------------------------------|-----------------------------|
| `+ /bar/`        | no       | dir-only `bar`         | `bar`                                  | (none - Include)            |
| `- /bar`         | no       | `bar` (anchored)       | `bar`                                  | `bar/**`                    |
| `+ foo/s?b/`     | no       | dir-only `foo/s?b`     | `**/foo/s?b`, `foo/s?b`                | (none - Include)            |
| `- foo/*/`       | no       | dir-only `foo/*`       | `**/foo/*`, `foo/*`                    | NONE (#5749 suppress gate)  |
| `+ foo**too`     | YES      | `foo/**/too`           | `**/foo/**/too`, `foo/**/too`          | (none - Include)            |
| `- new/keep/**`  | YES      | `new/keep/**` (intact) | `**/new/keep/**`, `new/keep/**`        | NONE (`**`-suffix gate)     |
| `- new/lose/***` | -        | dir-only `new/lose`    | `new/lose`                             | `new/lose/**`               |

The PR #5751 rewrite does NOT touch the legs that trigger
over-deletion. PR #5749's descendant suppression DOES touch them
(`- foo/*/`, `- new/keep/**` indirectly).

## 5. Two specific path traces

### 5.a. `./bar/.filt` under L4

L4 rules at `DecisionContext::Deletion` against `bar/.filt`:

1. `+ **/bar` is an Include - traversal mode skips the synthesised
   descendant `**/bar/**`, so this rule does not match `bar/.filt`
   (only `bar` itself).
2. `- /bar` is an Exclude - direct matcher `bar` matches the path
   `bar` but NOT `bar/.filt`. Synthetic descendant `bar/**` is what
   should fire for `bar/.filt`. Under traversal the receiver skips
   descendants, but the *.filt* is a separate file the receiver should
   delete only if the sender skipped sending it.

The problem: after PR #5749 / #5751, the receiver sees rule 1 silently
(its scope match in `has_matching_rule` is descendant-free), then rule
2's direct matcher `bar` against `bar/.filt` returns false. The
receiver falls through to "no rule matched -> delete" - matching
upstream's `check_filter` outcome for `bar/.filt` exactly, because
upstream relies on the sender having included `bar/` first and the
delete-pass running INSIDE `bar/` (where rule 2 cannot match
`/.filt`). So the trace is correct in isolation but `bar/` itself was
spuriously included earlier in the walk.

**Likely failure**: oc-rsync deletes `./bar/.filt` because the
synthesised include descendants for `+ **/bar` (from PR #5749 NOT
suppressing wildcard-free unanchored patterns) include `**/bar/**`,
which matches `bar/.filt`, masking the receiver's exclusion of `.filt`
by the per-directory `.filt` merge file.

### 5.b. `./foo/sub/file1` under L4

1. `+ /bar/` - no match.
2. `- /bar` - no match.
3. `+ foo**too` rewritten to `foo/**/too` - no match.
4. `+ foo/s?b/` - direct matcher `**/foo/s?b` is directory-only,
   `foo/sub/file1` is not a directory -> no match.
5. `- foo/*/` - direct matcher `**/foo/*` is directory-only,
   `foo/sub/file1` is not a directory -> no match. PR #5749 also
   suppresses the synthetic descendant `foo/*/**` here.

Upstream behavior: rule 5 fires when the *sender* walks `foo/sub/`,
marks the directory as excluded, and prunes the subtree. The receiver
then deletes `foo/sub/file1` correctly because its `.filt` already
records the exclusion. PR #5749's descendant suppression means the
receiver's `allows_deletion("foo/sub/file1", false)` returns `true`
(allow delete) when called from the single-path API - which is the
receiver's behavior. So this leg passes only if the sender pruned
`foo/sub/` first.

**Likely failure**: the sender did NOT prune `foo/sub/` because
`+ foo/s?b/` at rule 4 fires first under `wildmatch_array` semantics
(upstream `exclude.c:917-922`); but oc-rsync's `**/foo/s?b` glob
matcher applied to the directory `foo/sub/` works correctly. The
sub-failure mode is more subtle - rule 4's Include status under
`allows_deletion`'s receiver-side `excluded_for_delete_excluded`
machinery (decision.rs:90-101) means the receiver records
"not excluded" for `foo/sub`, then proceeds to walk into it for
`--delete-during`, and rule 5 *cannot* fire on `file1`. This matches
upstream.

The two traces above isolate the bug to L4's `bar/.filt` and the
synthetic-descendant leak from `+ **/bar` plus `- /bar` - NOT from
`foo/s?b/` normalisation.

## 6. Fix-scope decision

Three options were on the table. The audit picks one:

### 6.a. Option A - narrow-descendants (chosen)

Tighten the PR #5749 gate so descendants are only suppressed under
`DecisionContext::Transfer` traversal queries. The single-path
`DecisionContext::Deletion` query needs the synthetic descendant for
`bar/**` to fire on `bar/.filt` correctly when the anchored exclude
`- /bar` is paired with an include like `+ **/bar`.

Concrete shape (audit only - not implemented):

```text
// compiled/mod.rs:108-111 - retain suppression as today
let is_directory_only_unanchored_wildcard = ...;
let is_anchored_wildcard = slash_anchored && has_glob_wildcard;
let suppress_descendants_for_transfer =
    is_directory_only_unanchored_wildcard || is_anchored_wildcard;

// but: ALWAYS emit descendants into the rule's matcher set; the
// runtime decision path already gates them via `check_descendants =
// !traversal` (decision.rs:69). Move the suppression up to the
// match-time predicate keyed on context.
```

This is the safest change: the compile-time emission becomes
unconditional again, restoring upstream parity for the single-path
receiver case. The runtime `check_descendants` gate is already wired
correctly on the Transfer side (decision.rs:69 / set.rs:164,
chain.allows -> allows_during_traversal). The receiver's
`allows_deletion` single-path entry (set.rs:220) regains descendant
matching, which closes the `./bar/.filt` leak.

### 6.b. Option B - deletion-override (rejected)

Add a `DecisionContext::Deletion`-specific branch in
`compiled/mod.rs:112-121` that emits descendants regardless of
`suppress_descendants`. Rejected because `CompiledRule` is shared
across both decision contexts; the context is not visible at compile
time. Achievable only by duplicating the matcher set.

### 6.c. Option C - revert normalisation (rejected)

Revert PR #5751 `normalise_recursive_wildcards`. Rejected because the
trace above shows #5751 does not touch the rules involved in the
failing path. Reverting it would regress UTS-20 `foo**too`
cross-segment match without addressing the `bar/.filt` leak.

## 7. Test plan (nextest fixture, audit-only spec)

When Option A is implemented, the regression test should:

- Build a fixture matching `exclude.test:36-50` (the directory shape
  with `bar/down/to/...`, `foo/sub/file1`, `foo/file1`, etc.).
- Apply the `$excl` body from section 1.a verbatim as `--exclude-from`.
- Run `--delete-during` from `$fromdir/` to a pre-populated `$todir/`
  that mirrors the `chk` skeleton after L2 + L3 prep.
- Assert: `bar/.filt` is RETAINED on the receiver (not deleted),
  `foo/sub/file1` is RETAINED (matched by `+ foo/s?b/`).
- Wire-byte assertion: no `*deleting bar/.filt` itemize line should
  appear in `--itemize-changes` output.
- Place fixture in `crates/transfer/tests/` alongside
  existing receiver deletion tests; cite this design doc and
  UTS-V3.B.F task numbers.

## References

- Upstream: `target/interop/upstream-src/rsync-3.4.4/testsuite/exclude.test`
  (the `-lsh` symlink simply sets `RSYNC_RSH`).
- PR #5749 (descendants suppression for directory-only unanchored
  wildcards) - see `crates/filters/src/compiled/mod.rs:72-121`.
- PR #5751 (`normalise_recursive_wildcards`) - see
  `crates/filters/src/compiled/pattern.rs:79-144`.
- Receiver delete-during entry: `crates/transfer/src/receiver/directory/deletion.rs:226`.
- Chain-level deletion API: `crates/filters/src/chain/mod.rs:190-205`.
- Decision context: `crates/filters/src/decision.rs:47-101, 261-265`.
- Tasks: parent #4304 (UTS-V3.B), sub-tasks #4386-#4401 (UTS-V3.B.F).
