# Filter merge-file recursion and .rsync-filter inheritance

Tracking issue: oc-rsync task #2128.

Audit of how oc-rsync handles filter merge files (`.rsync-filter`,
per-directory filter files) and their inheritance/recursion compared to
upstream rsync 3.4.1.

## 1. Merge file types

### Upstream (exclude.c)

Upstream rsync supports two merge file classes via `parse_rule_tok()`
(lines 1135-1288):

| Prefix | Keyword | Flag | Behaviour |
|---|---|---|---|
| `.` | `merge` | `FILTRULE_MERGE_FILE` | Single-instance: read once, rules inlined at point of definition. |
| `:` | `dir-merge` | `FILTRULE_PERDIR_MERGE \| FILTRULE_MERGE_FILE \| FILTRULE_FINISH_SETUP` | Per-directory: scanned in every traversed directory, rules scoped to that directory tree. |

There is no standalone `;` prefix for dir-merge with no-inherit. The
no-inherit behaviour is achieved via the `n` modifier on a dir-merge
rule (e.g., `:n .excludes` or `dir-merge,n .excludes`). The upstream
parser at line 1264 sets `FILTRULE_NO_INHERIT` when the `n` modifier
character is encountered on any merge rule.

### oc-rsync

Two implementations exist in parallel:

1. **`filters` crate** (`merge/parse.rs`, `chain.rs`) - Parses `.` as
   `ShortFormAction::Merge` and `:` as `ShortFormAction::DirMerge`.
   `FilterChain` manages push/pop of per-directory scopes via
   `enter_directory()` / `leave_directory()`. `DirMergeConfig` models
   the `n` (no-inherit), `e` (exclude-self), `s`/`r` (side), `p`
   (perishable) modifiers.

2. **`engine` crate** (`local_copy/filter_program/`,
   `local_copy/dir_merge/`) - `FilterProgram` maintains an ordered
   instruction list interleaving `FilterSegment` and `DirMerge { index }`
   placeholders. At runtime, `CopyContext::enter_directory()` reads
   merge files via `load_dir_merge_rules_recursive()`, compiles rules
   into `FilterSegment` instances, and pushes them onto per-rule-index
   layer stacks (`dir_merge_layers`).

**Status:** Conformant. Both `.` (merge) and `:` (dir-merge) are
recognised. The `n` modifier correctly disables inheritance. No
standalone `;` prefix is expected or needed since upstream does not
define one.

## 2. Per-directory .rsync-filter pickup

### Upstream (flist.c)

`send_directory()` (flist.c line 1687) calls `push_local_filters(fbuf,
len)` before descending into a directory. This iterates all registered
`mergelist_parents` entries, constructs the per-directory merge file path
by appending the merge filename to the current `dirbuf`, and calls
`parse_filter_file()` on it (exclude.c lines 811-821). Missing files
are silently skipped. On return, `pop_local_filters()` restores state.

For incremental recursion (`send1extra()`, flist.c line 2023),
`change_local_filter_dir()` is used instead, which internally calls
`push_local_filters()` with depth tracking.

The receiver side uses `change_local_filter_dir()` during
`recv_file_list()` processing (flist.c lines 1958, 1968) to keep
per-directory filters in sync when processing received file list entries.

### oc-rsync

**Sender side (generator):** `walk.rs` calls
`self.filter_chain.enter_directory(&dir_path)` at lines 74 and 180
before scanning directory children, and `leave_directory(guard)` after
the directory is fully processed. `FilterChain::enter_directory()` reads
merge files by joining `DirMergeConfig.filename()` to the directory path
and calling `fs::read_to_string()`. Missing files produce
`ErrorKind::NotFound` which is silently skipped (chain.rs line 321).

**Local copy (engine):** `CopyContext::enter_directory()` in
`context_impl/transfer.rs` iterates `program.dir_merge_rules()`, calls
`resolve_dir_merge_path()` to locate each merge file, and loads rules
via `load_dir_merge_rules_recursive()`. A `DirectoryFilterGuard` RAII
object tracks pushed layer indices and pops them on drop.

**Receiver side:** The receiver does not independently scan for
per-directory merge files. In remote transfers, the sender builds the
file list with per-directory filter rules already applied. The receiver
applies received filter rules but does not re-read `.rsync-filter` files
from its own filesystem during file list processing.

**Status:** Partially conformant. Sender-side pickup is correct.
Receiver-side pickup is missing - see Gap G-1 below.

## 3. Rule inheritance

### Upstream (exclude.c)

Per-directory rules are inherited through a linked-list append mechanism
(lines 86-114). When entering a subdirectory:

1. `push_local_filters()` saves the current `filter_rule_list` state
   for each mergelist entry (line 784).
2. It sets `tail = NULL` to convert all local rules into inherited rules
   (line 801).
3. If `FILTRULE_NO_INHERIT` is set, it also sets `head = NULL`,
   discarding inherited rules (line 803).
4. It then reads the new directory's merge file. New local rules are
   prepended before inherited rules, giving them higher priority (line
   4249 of rsync.1.md: "newest rules a higher priority than the
   inherited rules").

When leaving (`pop_local_filters()`), local rules are freed and the
saved state is restored (lines 827-873).

### oc-rsync

**`filters` crate path:** `FilterChain` uses a `Vec<DirScope>` stack.
`enter_directory()` pushes new scopes with the current depth. `allows()`
iterates scopes in reverse (innermost first), checking each scope's
`FilterSet`. This approximates upstream's inheritance: parent rules
remain visible through lower-indexed scopes.

However, `FilterChain` does not physically prepend child rules to parent
rules. Instead it uses separate scopes evaluated in reverse order. This
produces the same first-match-wins outcome for exclude rules, but
`has_matching_rule()` (chain.rs line 450) only detects matches for
exclude and protect rules - not include rules. An include-only per-dir
scope silently falls through to the next scope. This is noted in the
code comment (line 459-462) as intentional for upstream's "prepend
semantics," but it diverges from upstream when a per-directory include
rule should shadow a parent-directory exclude.

**`engine` crate path:** `FilterProgram.evaluate()` processes
instructions in order. DirMerge instructions evaluate their layer stack
(`dir_merge_layers[index]`), which contains one `FilterSegment` per
ancestor directory where the merge file was found. When `inherit` is
true, layers accumulate; when false, rules are placed on the ephemeral
stack and only visible at the current depth.
`CopyContext::enter_directory()` handles `clear_inherited` by clearing
`layers[index]` (transfer.rs line 119).

**Status:** Largely conformant for the engine path. The `filters` crate
path has a semantic gap with include-only per-directory scopes (see Gap
G-2).

## 4. Rule scoping

### Upstream

Rules from a per-directory merge file apply to that directory and all
descendants (unless `FILTRULE_NO_INHERIT` is set). Anchored rules
(leading `/`) are relative to the merge file's directory, not the
transfer root (rsync.1.md line 4258-4259). When `pop_local_filters()`
is called, local rules are freed and only inherited rules persist.

### oc-rsync

**`filters` crate:** Scoping is depth-based.
`pop_scopes_at_depth(depth)` removes all scopes at the specified depth.
Anchored patterns in per-directory merge files are not re-anchored
relative to the merge file's directory - they remain relative to the
transfer root as written.

**`engine` crate:** When `inherit` is true, `FilterSegment` instances
are pushed onto `layers[index]` and persist across subdirectories until
either `clear_inherited` is encountered or the guard pops them on scope
exit. When `inherit` is false, segments go onto `ephemeral` and are
visible only at the current depth.

**Status:** Conformant for scoping lifetime. Gap exists for anchored
pattern re-anchoring - see Gap G-3.

## 5. -F shortcut

### Upstream (options.c lines 1589-1598)

- First `-F`: adds `--filter='dir-merge /.rsync-filter'`
- Second `-F` (i.e., `-FF`): additionally adds `--filter='exclude .rsync-filter'`
- Third and subsequent `-F`: no-ops.

The leading `/` on `/.rsync-filter` anchors the merge filename to the
transfer root, causing rsync to look for `.rsync-filter` only at the
transfer root on the first entry, but then in every subdirectory during
traversal (since `push_local_filters` always looks for per-dir merge
files in each directory).

### oc-rsync

`cli/src/frontend/filter_rules/arguments.rs` implements
`push_rsync_filter_shortcut()`:

```
0 => target.push(OsString::from("dir-merge /.rsync-filter")),
1 => target.push(OsString::from("exclude .rsync-filter")),
_ => {} // third+ -F is a no-op
```

Test coverage in the same file confirms:
- Single `-F` produces `dir-merge /.rsync-filter`.
- `-FF` additionally produces `exclude .rsync-filter`.
- Interleaving with `--filter` preserves definition order.

**Status:** Conformant.

## 6. Merge file nesting

### Upstream

A merge file (`.` prefix) can contain another merge or dir-merge rule.
`parse_filter_str()` handles `FILTRULE_MERGE_FILE` at lines 1404-1437:
single-instance merge rules are immediately expanded via
`parse_filter_file()`, and dir-merge rules are registered on the
mergelist for per-directory scanning. There is no explicit depth limit;
the filesystem stack depth is the practical bound.

### oc-rsync

**`filters` crate:** `read_rules_recursive()` in `merge/read.rs`
accepts a `max_depth` parameter (typically 10) and tracks recursion
depth. Exceeding it returns `MergeFileError`. Only `FilterAction::Merge`
rules trigger recursion; `DirMerge` rules are returned as-is.

**`engine` crate:** `load_dir_merge_rules_recursive()` in
`dir_merge/load.rs` uses a `visited: Vec<PathBuf>` stack for cycle
detection via canonical path comparison. Nested merge directives (both
`Merge` and `DirMerge`) encountered inside a merge file are recursively
loaded with the visited stack preventing infinite loops.

**Status:** Conformant. The oc-rsync implementation is arguably
stricter than upstream (explicit depth limits and cycle detection vs.
upstream's implicit stack-depth limit).

## 7. Sender vs receiver

### Upstream

Per-directory merge files are scanned **on the sender side** during
`send_directory()` / `send_file_list()`. The sender applies per-dir
filters to determine which files enter the file list.

On the **receiver side**, `change_local_filter_dir()` is called during
file list processing to maintain per-directory filter state. This is
necessary for `--delete` operations: the receiver must know which files
are "expected" in each directory to determine which extra files to
remove. The receiver re-reads per-directory merge files from its own
directory tree.

rsync.1.md (line 4207-4210): "These per-directory rule files must be
created on the sending side because it is the sending side that is being
scanned for the available files to transfer. These rule files may also
need to be transferred to the receiving side if you want them to affect
what files don't get deleted."

### oc-rsync

**Sender side:** Per-directory merge file handling is fully implemented
in the generator (`walk.rs`) via `FilterChain` and in the local copy
engine via `FilterProgram`.

**Receiver side:** The receiver applies filter rules received over the
wire but does not independently read per-directory merge files from the
destination tree. For local copies, `FilterProgram` handles both
transfer filtering and deletion filtering in the same process.

For remote transfers with `--delete`, the receiver currently relies on
the filter rules transmitted by the sender. It does not re-read
`.rsync-filter` files from the destination to determine which files are
protected from deletion by per-directory rules in the destination tree.

**Status:** Gap exists for remote `--delete` with per-directory merge
files - see Gap G-4.

## 8. Daemon filters

### Upstream (clientserver.c, exclude.c)

Daemon-side configuration (`rsyncd.conf`) supports `filter`, `exclude`,
`include`, `exclude from`, and `include from` directives. These populate
`daemon_filter_list` (exclude.c line 51). The daemon filter list is
checked **before** the regular filter list in `name_is_excluded()`
(exclude.c lines 1012-1017):

```c
if (daemon_filter_list.head && check_filter(&daemon_filter_list, ...) < 0)
    return 1;  // excluded by daemon
if (filter_list.head && check_filter(&filter_list, ...) < 0)
    return 1;  // excluded by user
```

Per-directory merge files referenced in client filters are subject to
the daemon filter list check: `parse_filter_file()` (exclude.c lines
1458-1466) checks `daemon_filter_list` before opening the file.

### oc-rsync

Daemon module configuration (`rsyncd_config/sections.rs`) exposes
`filter()`, `exclude()`, `include()`, `exclude_from()`, and
`include_from()` methods. In the generator
(`transfer/src/generator/filters.rs` lines 64-75), daemon filter rules
are prepended to client wire rules before conversion to `FilterChain`:

```rust
let combined = if daemon_rules.is_empty() {
    wire_rules
} else {
    let mut combined = daemon_rules.clone();
    combined.extend(wire_rules);
    combined
};
```

This achieves the upstream precedence (daemon rules checked first) since
the combined list is evaluated sequentially with first-match-wins. Test
coverage exists in
`daemon/src/tests/chunks/daemon_filter_merge_with_client_filters.rs`.

Per-directory merge files in client filters are not subject to daemon
filter list access control. Upstream's `parse_filter_file()` consults
`daemon_filter_list` before opening merge files to prevent clients from
reading files outside the module path via crafted merge file paths.

**Status:** Partially conformant. Precedence is correct. Merge file
access control is missing - see Gap G-5.

## 9. Summary of gaps

| ID | Description | Risk | Priority |
|---|---|---|---|
| G-1 | Receiver does not re-read per-directory merge files from destination during remote `--delete` sweeps. | Medium - per-directory protect/risk rules in destination-only merge files are not honoured. Only affects `--delete` with per-dir merge files in remote mode. | P2 |
| G-2 | `FilterChain::has_matching_rule()` only detects exclude/protect matches, not include matches. Include-only per-dir scopes fall through silently. | Low - in practice per-dir files rarely contain only include rules without corresponding excludes. The engine crate path (`FilterProgram`) is unaffected. | P3 |
| G-3 | Anchored patterns (`/foo`) in per-directory merge files are not re-anchored relative to the merge file's directory. They match relative to the transfer root. | Medium - anchored rules like `/build` in a subdirectory's `.rsync-filter` would match `/build` at the transfer root, not `subdir/build`. | P2 |
| G-4 | Remote transfers with `--delete` do not pick up per-directory merge files from the destination tree for deletion decisions. | Medium - same underlying issue as G-1; destination-side per-dir rules are ignored. | P2 |
| G-5 | Daemon filter list is not consulted when opening per-directory merge files referenced by client filter rules. A malicious client could craft a merge file path to read files outside the module. | High - security issue for daemon deployments. | P1 |

## 10. Evaluation order conformance

### Upstream

`check_filter()` (exclude.c lines 1038-1065) evaluates rules
sequentially. When it encounters a `FILTRULE_PERDIR_MERGE` rule, it
recursively calls `check_filter()` on the per-dir merge list (line
1047-1050). This means per-dir merge rules are evaluated **in-place**
within the global filter list, exactly where the `:` directive was
specified. Rules before the merge directive take precedence; rules after
are fallbacks.

### oc-rsync

**`engine` crate:** `FilterProgram.evaluate()` processes instructions in
definition order. `DirMerge { index }` instructions are interleaved with
`Segment` instructions at the position where the dir-merge rule was
specified. This preserves upstream's in-place evaluation order.

**`filters` crate:** `FilterChain.allows()` evaluates per-directory
scopes from innermost to outermost, then global rules. This does not
preserve the positional relationship between global rules and dir-merge
rules - per-dir scopes always take precedence over all global rules.

**Status:** The engine crate path (used by local copies) is conformant.
The `FilterChain` path (used by the generator in remote transfers) does
not preserve positional ordering between global rules and per-dir merge
rules - see Gap G-6.

### Additional gap

| ID | Description | Risk | Priority |
|---|---|---|---|
| G-6 | `FilterChain` evaluates all per-directory scopes before global rules. Upstream evaluates per-dir rules at their definition position within the global list, allowing earlier global rules to override per-dir results. | Medium - affects transfers where a global rule intentionally precedes a dir-merge directive to override per-dir rules. | P2 |

## 11. Test coverage

### Existing coverage

- **`filters` crate:** `chain.rs` tests cover `DirMergeConfig` builder
  methods, scope push/pop, nested scope evaluation, `exclude_self`
  behaviour, and guard lifecycle. `merge/tests.rs` covers rule parsing
  for all short-form and long-form prefixes including modifiers.

- **`engine` crate:** `dir_merge/load.rs` tests cover path resolution,
  `DirMergeEntries` merge/extend semantics, and `clear_inherited`
  propagation. `filter_program/` tests cover `FilterSegment` evaluation,
  `FilterOutcome` state transitions, and `CompiledRule` matching.

- **Daemon integration:** `daemon_filter_merge_with_client_filters.rs`
  tests verify daemon rule precedence over client rules for inline
  directives and `exclude from` / `include from` file directives.

- **CLI:** `filter_rules/arguments.rs` tests verify `-F` / `-FF`
  expansion and interleaving with `--filter`.

### Test gaps

| Gap | Description |
|---|---|
| T-1 | No test for per-directory merge file pickup during recursive directory walk (end-to-end with actual `.rsync-filter` files on disk). |
| T-2 | No test for anchored pattern re-anchoring in per-directory merge files. |
| T-3 | No test for `clear` (`!`) directive in a per-directory merge file clearing only the current merge file's inherited rules (not all rules). |
| T-4 | No test for receiver-side per-directory merge file pickup during `--delete`. |
| T-5 | No test for nested dir-merge rules (a `.rsync-filter` containing `: .local-filter`). |
| T-6 | No test for daemon filter list access control on merge file paths. |
| T-7 | No interop test exercising per-directory merge files against upstream rsync. |

## 12. Recommendations

### P1 - Security

1. **G-5:** Add daemon filter list access control to merge file opening.
   When the daemon filter list is active, check the merge file path
   against `daemon_filter_list` before reading it. Mirror upstream's
   `parse_filter_file()` guard (exclude.c lines 1458-1466).

### P2 - Correctness

2. **G-1/G-4:** Implement receiver-side per-directory merge file
   scanning for `--delete` operations. During the deletion pass, the
   receiver should read `.rsync-filter` files from the destination tree
   and apply their rules to deletion decisions. This requires
   `change_local_filter_dir()` equivalent logic in the receiver.

3. **G-3:** Re-anchor patterns with leading `/` in per-directory merge
   files to be relative to the merge file's directory, not the transfer
   root. When a rule is loaded from `/some/dir/.rsync-filter`, a pattern
   `/foo` should match `some/dir/foo` rather than the top-level `foo`.

4. **G-6:** Refactor `FilterChain` to preserve the positional
   relationship between global rules and per-dir merge rules. Per-dir
   rules should be evaluated at the position where the `:` directive
   appears in the filter list, not before all global rules.

### P3 - Quality

5. **G-2:** Fix `has_matching_rule()` in `FilterChain` to detect include
   matches. An include-only per-dir scope should produce a definitive
   "matched as include" result rather than falling through.

6. **T-1 through T-7:** Add integration and interop tests for per-dir
   merge file scenarios listed in section 11.
