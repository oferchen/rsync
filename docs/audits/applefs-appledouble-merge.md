# AppleDouble (`._foo`) filter and xattr-merge design audit

Task: #1907. Branch: `docs/applefs-appledouble-1907`. No code changes.

## Scope

Audit the current AppleDouble support shipped by oc-rsync and design two
follow-on capabilities that are still missing:

1. A first-class CLI surface that skips AppleDouble (`._foo`) sidecar files
   on the sender when the destination is already collecting Apple metadata
   natively (xattrs, resource forks). Today the rule exists as
   `--apple-double-skip` but its semantics are unconditional and it is not
   linked to whether xattrs are being transferred.
2. A merge mode that takes an incoming `._foo` AppleDouble container,
   parses it, and re-projects the contained payloads back into the parent
   file's native extended attributes (`com.apple.ResourceFork`,
   `com.apple.FinderInfo`, plus any vendor-specific entries). After a
   successful merge the sidecar is removed from the destination tree.

This is the audit-and-design phase for follow-up F-2 of
[`apple-fs-roundtrip.md`](apple-fs-roundtrip.md). No code is shipped here.

Source files inspected (all paths repository-relative):

- `crates/apple-fs/src/lib.rs`
- `crates/apple-fs/src/apple_double.rs`
- `crates/apple-fs/src/resource_fork.rs`
- `crates/apple-fs/README.md`
- `crates/apple-fs/tests/apple_double_round_trip.rs`
- `crates/filters/src/apple_double.rs`
- `crates/filters/src/lib.rs`
- `crates/filters/src/set.rs`
- `crates/filters/tests/apple_double.rs`
- `crates/cli/src/frontend/filter_rules/apple_double.rs`
- `crates/cli/src/frontend/defaults.rs`
- `crates/cli/src/frontend/execution/drive/filters.rs`
- `crates/cli/src/frontend/execution/drive/workflow/run.rs`
- `crates/cli/src/frontend/help.rs`
- `crates/cli/src/frontend/tests/parse_args_recognises_apple_double.rs`
- `crates/cli/src/frontend/tests/transfer_request_with_apple_double.rs`
- `crates/cli/tests/argument_defaults_special.rs`
- `crates/metadata/src/xattr.rs`
- `crates/protocol/src/xattr/wire/{encode,decode}.rs`

Upstream references (rsync 3.4.1, `target/interop/upstream-src/rsync-3.4.1/`):

- `xattrs.c` - xattr listing, sending, and applying. No `AppleDouble` or
  `._` literal anywhere in the file.
- `exclude.c` - filter machinery; the codebase has no built-in AppleDouble
  pattern.
- `flist.c` - file-list scanner; no AppleDouble pairing logic.
- `options.c` - no `--apple-double-*` long option.
- `rsync.h`, `rsync.1.md` - silent on AppleDouble.

Mainline upstream rsync 3.4.1 has no AppleDouble awareness whatsoever. Every
behaviour proposed below is therefore an oc-rsync extension. Per project
policy this is acceptable for opt-in features that do not require wire
protocol changes (see "Wire-protocol invariants" below).

## TL;DR

- The AppleDouble container parser/encoder, resource-fork accessors, and the
  `is_apple_double_name` / `apple_double_companion` lexical helpers are
  already in place under `crates/apple-fs/`. They are pure data and FFI-free.
- `--apple-double-skip` already wires `._*` as a perishable exclude rule
  through `crates/cli/.../filter_rules/apple_double.rs`. It is unconditional
  and direction-agnostic.
- The two missing pieces are: (a) automatic, xattr-aware skipping (only skip
  `._foo` when xattrs are being transferred natively) so we do not double-
  ship Apple metadata, and (b) an opt-in merge mode that recovers Apple
  metadata from an incoming `._foo` and writes it as native xattrs on the
  partner file before deleting the sidecar.
- Both pieces sit entirely above the rsync wire protocol. No new caps, no
  new file-list flags, no protocol bump.

## Current state (post PR b1c754cf3)

### Building blocks already shipped

`crates/apple-fs/src/lib.rs`:

- `APPLE_DOUBLE_PREFIX = "._"` constant.
- `is_apple_double_name(name)` - lexical predicate for `._foo` filenames.
- `apple_double_companion(path)` - lexical pairing helper that maps `foo`
  to `._foo` and `._foo` back to `foo` in either direction.

`crates/apple-fs/src/apple_double.rs`:

- AppleDouble v2 (RFC 1740) parser and encoder.
- `EntryId` enum with the standard entry ids (`DataFork = 1`,
  `ResourceFork = 2`, `RealName = 3`, `Comment = 4`, `IconBw = 5`,
  `IconColor = 6`, `FileDatesInfo = 8`, `FinderInfo = 9`,
  `MacFileInfo = 10`, `ProDosFileInfo = 11`, `MsDosFileInfo = 12`,
  `ShortName = 13`, `AfpFileInfo = 14`, `DirectoryId = 15`).
- `AppleDouble::decode` validates magic (`0x0005_1607` for AppleDouble,
  `0x0005_1600` for AppleSingle), version (`0x0002_0000`), entry-table
  bounds, and per-entry offset/length bounds.
- `AppleDouble::encode` emits id-sorted entries with byte-stable output.

`crates/apple-fs/src/resource_fork.rs`:

- `read_resource_fork`, `write_resource_fork`, `remove_resource_fork`.
- `read_finder_info`, `write_finder_info`, `remove_finder_info`.
- All accessors: macOS uses the third-party `xattr` crate (the only
  unsafe-bearing dependency); every non-macOS target is an `Ok(None)` /
  `Ok(())` stub. The crate itself stays `#![deny(unsafe_code)]`.

### Filter rule already shipped

`--apple-double-skip` is parsed in `crates/cli` and threaded into the filter
chain by `crates/cli/src/frontend/filter_rules/apple_double.rs`. The
underlying pattern (`._*`) is sourced from
`crates/cli/src/frontend/defaults.rs::APPLE_DOUBLE_EXCLUDE_PATTERNS` and
also exposed by `crates/filters/src/apple_double.rs::default_patterns()`.
`FilterSet::from_rules_with_apple_double` lets library callers obtain a
ready-to-use rule set without touching the CLI layer.

The rule is marked perishable (`with_perishable(true)`), matching the
precedence semantics of `--cvs-exclude`: an explicit include rule placed
earlier in the chain wins under first-match-wins evaluation.

### What this gives us today

| Direction          | Default       | `--apple-double-skip` |
|--------------------|---------------|-----------------------|
| macOS -> macOS     | `._foo` copied verbatim alongside `foo`. xattrs are also synced via `metadata::xattr`, leading to double-ship if a sidecar exists. | `._foo` excluded; xattr pipeline still ships `com.apple.*`. Correct outcome. |
| macOS -> Linux     | `._foo` copied verbatim. Linux has no resource fork concept; user sees a litter of `._foo` files. | `._foo` excluded; the equivalent metadata is still preserved on Linux as `user.com.apple.ResourceFork` (via `wire_to_local`) when `-X` is in effect. |
| Linux -> macOS     | Any `._foo` files imported earlier are copied verbatim and remain dormant - macOS does not auto-merge. | `._foo` excluded entirely. If the only Apple metadata lives inside `._foo`, the user loses it. |
| Either -> Windows  | `._foo` copied as plain files (Windows xattr stub silently drops `com.apple.*`). | `._foo` excluded. |

The Linux -> macOS row is where we leak data: if a user previously
exported their Mac files through a non-xattr transport (FAT USB stick,
older NFS share) the resource forks live inside `._foo` containers. Today
oc-rsync either copies them verbatim (no merge) or skips them (data loss).

### What is missing

1. **Xattr-aware filtering.** `--apple-double-skip` is unconditional. There
   is no mode that ties skipping to the presence of xattr transfer
   (`-X` / `--xattrs`). When a transfer is shipping `com.apple.*` xattrs
   natively, the sidecar is redundant and should be skipped automatically;
   when xattrs are not being shipped, the sidecar is the only carrier and
   must not be skipped.
2. **Merge mode.** No code path takes an incoming `._foo`, parses it via
   `AppleDouble::decode`, and writes the contents back as native xattrs on
   the partner file. The container format support exists; the merge policy
   does not.
3. **Cleanup after merge.** No code removes the sidecar after a successful
   merge. Without cleanup the destination ends up with both the native
   xattrs and a now-stale `._foo` next to the file.
4. **Filter directive equivalents.** The skip rule is only reachable through
   the long option. There is no `apple-double-skip` filter directive token,
   so `.rsync-filter` files cannot encode the policy per directory.

## Upstream prior art

There is none in mainline rsync 3.4.1. Confirmed by inspecting:

- `xattrs.c` - reads `com.apple.ResourceFork` and `com.apple.FinderInfo`
  exactly like any other xattr, with no AppleDouble awareness.
- `exclude.c` and `default.h` - no built-in `._*` pattern.
- `options.c` and the `rsync.1.md` man page - no AppleDouble-aware option.
- `flist.c` - no sibling-file pairing.

Some out-of-tree rsync forks (notably Apple's macOS-bundled rsync, which is
a fork of `rsync 2.6.9` with `--extended-attributes` patches) detect a
specific subset of AppleDouble payloads when copying onto/off HFS+. None of
those patches are in the upstream tree we mirror; we do not follow them.

The closest analogues in mainline are `--cvs-exclude` and the various
`backup-by-suffix` heuristics. The proposed AppleDouble surface mirrors the
shape of `--cvs-exclude` (perishable built-in exclude pattern) and the
shape of `--no-implied-dirs` (named flag that toggles a synthetic file-list
post-processing step).

## Wire-protocol invariants

The proposal does not touch the rsync wire. Specifically:

- File list flags. `flist.c::send_file_list` advertises a fixed set of
  per-file flags. We add no new flag bits.
- xattr abbreviation. `protocol::xattr::wire` already abbreviates `>= 32`
  byte payloads to 16-byte MD5 references and only sends full payloads on
  request. The merge step is performed exclusively on the receiver side
  after the file is materialized; it generates no wire traffic.
- Filter exchange. The `[merge]` rule list is exchanged before the file
  list begins (`crates/protocol/.../filter_exchange.rs`). The new directive
  proposed below is purely a sender-side filter pattern, equivalent to
  inserting `- ._*` in a filter file. No new wire token.

A receiver running an unpatched oc-rsync (or upstream rsync) will simply
ignore the new flag because the corresponding sender-side rule was already
applied locally before the file list was sent.

## Design proposal

### CLI surface

Two new long options on top of the existing `--apple-double-skip`:

| Flag | Behaviour | Implies | Default |
|------|-----------|---------|---------|
| `--apple-double-skip` (existing) | Append `- ._*` (perishable) to the filter chain. Unconditional. | none | off |
| `--apple-double-auto` (new) | Equivalent to `--apple-double-skip` only when xattrs are also being transferred (`-X`/`--xattrs`). When xattrs are off, the flag is a no-op. | implied by `-X` only when the user opts in via `--apple-double-auto` | off |
| `--apple-double-merge` (new) | On the receiver, after each file is materialized, scan its directory for the matching `._foo` companion. If found, parse the AppleDouble container, project its entries onto the partner file's xattrs, and unlink the sidecar. | `--xattrs` (errors if `-X` is not also set) | off |

`--apple-double-auto` and `--apple-double-merge` are independent: a Linux
sender can pair them with `-X` to ship metadata as native xattrs and drop
sidecars; a macOS receiver pulling from a FAT-resident snapshot can pair
`--apple-double-merge` with `-X` to recover stranded resource forks.

`--no-apple-double-skip`, `--no-apple-double-auto`, and
`--no-apple-double-merge` negate the corresponding flags, mirroring the
`--no-FOO` style already used by `--cvs-exclude` / `--no-cvs-exclude`.

### Filter directive

Add a new short directive token: `apple-double-skip` (and its short form
`A`, mirroring `C` for `cvs-exclude`). Inside a `.rsync-filter` file or a
`--filter` argument, writing:

```
:A
```

inserts the same `- ._*` perishable rule that `--apple-double-skip`
contributes to the global chain. The directive is local to the directory
(and its descendants, per merge semantics). This lets a user enable the
filter for `~/Pictures` without polluting the global chain.

### Merge algorithm

Receiver-side, scoped to `--apple-double-merge` and only meaningful when
`-X` is in effect:

```
for each materialized regular file `path` after metadata application:
    let companion = apple_fs::apple_double_companion(path);
    if companion does not exist on the destination filesystem: continue;
    let blob = read(companion);
    match apple_fs::apple_double::AppleDouble::decode(&blob):
        Err(_) -> log warning, leave both files untouched, continue;
        Ok(container) -> for each entry in container:
            match entry.id:
                ResourceFork (2):
                    if !path.has_xattr(RESOURCE_FORK_XATTR)
                       or !options.preserve_existing_xattrs:
                        write_xattr(path, RESOURCE_FORK_XATTR, entry.data);
                FinderInfo (9):
                    if entry.data.len() != FINDER_INFO_LEN: skip with warning;
                    else if !path.has_xattr(FINDER_INFO_XATTR)
                            or !options.preserve_existing_xattrs:
                        write_xattr(path, FINDER_INFO_XATTR, entry.data);
                RealName | Comment | FileDatesInfo | MacFileInfo
                | AfpFileInfo | DirectoryId:
                    project to com.apple.<lowercased>.<id> as user xattr;
                IconBw | IconColor | ProDosFileInfo | MsDosFileInfo
                | ShortName | DataFork:
                    log "ignored AppleDouble entry id=N" at debug level;
                _ (vendor-specific id):
                    project to com.apple.appledouble.unknown.<hex_id>;
    if all known entries were applied successfully: unlink(companion);
    else: leave companion in place, log warning.
```

Notes:

- The merge runs after all xattrs that arrived through the native wire
  pipeline have been applied. A native `com.apple.ResourceFork` always
  wins over a stale AppleDouble payload.
- The unlink step is skipped if any entry failed to project, so the sidecar
  remains as a recoverable carrier.
- The `DataFork` (id 1) entry is intentionally ignored: AppleDouble
  containers should not contain a DataFork (that lives in the partner
  file). AppleSingle containers do, but oc-rsync only ever encounters them
  if the user explicitly placed one there; the existing partner data is
  trusted.
- Vendor-specific ids (anything not in `EntryId`) are preserved in the
  user xattr namespace `com.apple.appledouble.unknown.<hex>` so a future
  re-export can reconstruct the container with `AppleDouble::encode`.

### Skip-when-shipping-xattrs algorithm (`--apple-double-auto`)

Sender-side. When the option is set, the filter chain receives `- ._*`
exactly as in `--apple-double-skip`, but only after the option-resolution
step confirms `-X` (or `--fake-super`, which also exercises the user xattr
namespace) is enabled. Otherwise the flag is a no-op and a warning is
emitted at the verbose level explaining that the sidecar is the only
carrier of Apple metadata in this configuration.

This is the only place we mutate the filter chain conditionally; the
existing `--apple-double-skip` semantics are preserved unchanged.

## Edge cases

The cases below must be covered by integration tests before merge. Each is
captured as a row in the future test matrix; this audit only enumerates
them.

### E-1 - Orphan `._foo` with no partner file

Scenario: the destination tree contains `._note.txt` but no `note.txt`.

Behaviour: `apple_double_companion("._note.txt")` returns `Some("note.txt")`
but the partner does not exist. `--apple-double-merge` must skip the
sidecar with a warning ("no partner for ._note.txt; left as-is") and not
attempt to materialize an empty data file. The sidecar is preserved so the
user can recover it manually.

Rationale: silently creating an empty `note.txt` to host the merged xattrs
would surprise the user and could clobber a deferred write.

### E-2 - Partner file has both native xattrs AND a `._foo`

Scenario: the destination has `note.txt` with `com.apple.ResourceFork`
already set (received via the native wire), plus a sidecar `._note.txt`
that decodes to a different `ResourceFork` payload.

Behaviour: native xattrs win. The merge step inspects each entry and only
writes when the destination xattr is absent (default policy), or always
overwrites when the user passes `--apple-double-merge=overwrite`.

Rationale: the native xattr came from the up-to-date sender; the sidecar is
historical. Overwriting silently is data loss in the common case.

### E-3 - `._foo` with malformed AppleDouble bytes

Scenario: the sidecar exists but `AppleDouble::decode` returns
`InvalidData` (truncated header, bad magic, descriptor table off the end,
entry length overflow).

Behaviour: log at warning level (`apple-double-merge: <path>: <error>`),
leave both files alone, increment a session counter exposed through
`--itemize-changes` and the final stats. Do not fail the transfer.

Rationale: a non-AppleDouble file that happens to start with `._` (some
build systems emit such names) must not abort the run.

### E-4 - Sidecar with vendor-specific entry ids

Scenario: container contains a payload at `id = 0x4647 ("FG")` that is not
in our `EntryId` enum.

Behaviour: project to `com.apple.appledouble.unknown.4647` as a user xattr
on the partner. Round-trip preserves the payload bytes exactly.

Rationale: keeps oc-rsync forward-compatible with future Apple entries
without requiring an enum bump.

### E-5 - Sidecar bigger than `MAX_FULL_DATUM` xattr abbreviation threshold

Scenario: sidecar is 5 MiB; one of its entries (resource fork) is 4.9 MiB.

Behaviour: read the file in one shot via `std::fs::read`. The merge step
writes the unpacked entries directly via `xattr::set`, which calls
`setxattr(2)`. macOS imposes no per-call cap on `com.apple.ResourceFork`
size. Linux ext4/xfs will reject payloads larger than the per-attribute
limit (xfs ~64 KiB, ext4 ~4 KiB inline / unlimited with `large_xattr`); the
underlying `io::Error` is propagated unchanged.

Rationale: matches the existing native-xattr pipeline. No new behaviour.

### E-6 - Read-only destination filesystem

Scenario: destination is mounted read-only or the user lacks write
permission.

Behaviour: `xattr::set` returns `EROFS` / `EACCES`. The merge step does not
unlink the sidecar in this case, surfaces the error, and leaves the
sidecar in place.

Rationale: idempotent retry from a writable mount completes the job.

### E-7 - Sender enables `--apple-double-skip` but receiver lacks `-X`

Scenario: macOS sender drops `._foo` files; Linux receiver does not request
xattrs.

Behaviour: the receiver applies no Apple metadata at all. This is the
documented "user opted out" case and is not a bug, but the CLI must emit a
single-line warning when both `--apple-double-skip` and absence of `-X`
are detected on the same end of the transfer ("Apple metadata is being
skipped without xattrs being preserved; metadata will be lost").

### E-8 - Sender uses `--apple-double-auto` but xattrs were dropped silently

Scenario: a transport in the middle (older daemon, Windows receiver) does
not advertise xattr support during capability negotiation.

Behaviour: the sender already disables `-X` on the wire after capability
negotiation. The auto-mode logic must inspect the post-negotiation flag,
not the pre-parse flag, so the filter is not silently inserted when the
receiver cannot consume xattrs. If the negotiation result is observed only
after the filter chain is locked, the auto-mode must fail with a clear
error rather than corrupting the destination.

### E-9 - Symlink partner

Scenario: the destination has a symlink `note.txt -> /elsewhere/note.txt`
plus a `._note.txt` sidecar.

Behaviour: the merge step calls `xattr::set_deref` for symlinks (which on
macOS still uses `XATTR_NOFOLLOW=0` and the link target's xattrs). For
oc-rsync's purposes, applying the merged xattrs to the link target is the
right behaviour because that is where the resource fork conceptually lives.
On Linux symlink xattrs are restricted to the `trusted.*` and `security.*`
namespaces only, so writes to `user.*` on a symlink would fail; in that
case skip with a warning.

### E-10 - Hard-linked partner

Scenario: `note.txt` and `notes/_note.txt` are hard links to the same
inode, both materialized in the same transfer; `._note.txt` is also
materialized.

Behaviour: applying xattrs to one inode mutates them for every alias, so
the merge runs once per sidecar - not once per partner alias. The cleanup
step unlinks only the sidecar, never the partner aliases.

### E-11 - Concurrent transfers writing the same destination

Scenario: two oc-rsync processes write the same destination at once
(unsupported by upstream rsync, but possible with operator error).

Behaviour: out of scope. Document that `--apple-double-merge` assumes the
caller follows upstream's serialization expectations.

### E-12 - Filesystem that lacks xattr support entirely

Scenario: destination is a FAT volume with no xattr support.

Behaviour: the merge step's first `xattr::set` call returns
`io::ErrorKind::Unsupported`. Log a single warning and disable the merge
step for the remainder of the transfer to avoid a per-file error spam.
Sidecars are left in place.

## Recommended task breakdown

Each step is a separate PR sized to fit the workspace's conventional commit
style. Tests come with each step, no separate test-only PRs.

1. **T-1 - Wire `--apple-double-auto` flag.**
   - Add the option to `crates/cli/src/frontend/parser.rs`.
   - Extend `FilterInputs` in
     `crates/cli/src/frontend/execution/drive/filters.rs` with
     `apple_double_auto` and apply the rule only when `-X` is also set.
   - Tests: parser recognises the flag; filter chain receives `._*` only
     when `-X` is on; warning emitted when `-X` is off.
   - Estimated change: ~120 LoC + ~80 LoC tests.

2. **T-2 - Filter directive `:A` / `apple-double-skip`.**
   - Extend `crates/filters/src/parse/directive.rs` to recognise the new
     directive token.
   - Reuse `crates/filters/src/apple_double.rs::default_patterns` so the
     directive expands to the same `._*` rule.
   - Tests: golden parse round-trips; precedence with explicit `+ keep.txt`
     placed earlier; nested merge files inherit the rule.
   - Estimated change: ~80 LoC + ~120 LoC tests.

3. **T-3 - Receiver-side merge skeleton.**
   - Add `crates/transfer/src/receiver/apple_double.rs` (or extend
     `crates/transfer/src/receiver/directory/mod.rs`) with a function
     `merge_apple_double_companion(path) -> io::Result<()>`.
   - Use `apple_fs::apple_double::AppleDouble::decode` and
     `apple_fs::resource_fork::write_*` plus `xattr::set` for everything
     else.
   - Gated behind a new `CoreConfig::apple_double_merge: bool`.
   - Tests: round-trip with a real `._foo` produced by macOS (or by the
     parser/encoder pair); E-1 through E-6 unit-level coverage.
   - Estimated change: ~250 LoC + ~300 LoC tests.

4. **T-4 - CLI plumbing for `--apple-double-merge`.**
   - Add the option to the parser, validate that `-X` is also set
     (otherwise return a clean error from `argument-parsing`).
   - Thread the flag into `CoreConfig` via the existing
     `ClientConfigBuilder` chain.
   - Tests: parse + validation, integration smoke test with a fixture
     sidecar.
   - Estimated change: ~150 LoC + ~150 LoC tests.

5. **T-5 - Cleanup, itemize, and stats.**
   - Emit `* (apple-double-merged)` in `--itemize-changes` for files whose
     xattrs were rebuilt from a sidecar, and `* (apple-double-removed)`
     for the sidecar that was unlinked.
   - Add three counters to `crates/transfer/src/stats.rs`:
     `apple_double_merged`, `apple_double_skipped`,
     `apple_double_warnings`. Surface them in the final-stats footer.
   - Tests: itemize golden; stats footer golden.
   - Estimated change: ~100 LoC + ~120 LoC tests.

6. **T-6 - Documentation.**
   - Update `docs/oc-rsync.1.md`, `docs/platform-notes.md` (add the
     "AppleDouble interop" section flagged in F-2 of the round-trip audit),
     and `crates/apple-fs/README.md` to remove the "future merge
     implementation" caveat.
   - Add a section to `docs/cross-platform-parity-matrix.md` covering the
     four direction rows of the matrix above.
   - Tests: man page renders; spell-check passes.
   - Estimated change: ~200 LoC of docs only.

7. **T-7 - End-to-end interop harness.**
   - Add a macOS-only nextest that creates a real source tree with mixed
     `com.apple.*` xattrs and AppleDouble sidecars, transfers via the
     in-process daemon harness, and verifies the destination has the
     correct xattrs and no leftover `._foo`.
   - Add a Linux receiver counterpart that verifies the merge step lifts
     resource forks out of `._foo` and into `user.com.apple.ResourceFork`.
   - Estimated change: ~200 LoC of test fixtures.

T-1 and T-2 are independent and can be merged in either order. T-3 can
land as a library-only crate addition (no CLI exposure) before T-4. T-5
through T-7 only need the merge code path to exist. None of these steps
require a wire-protocol change; the order respects the workspace's
"merge-as-you-go" cadence.

## Out of scope

- **Bidirectional sync.** Replacing rsync's one-way model with a merge
  that walks both ends of the link is not part of this design.
- **AppleSingle production.** oc-rsync only consumes AppleSingle when
  decoding (the parser already accepts the magic). Emitting AppleSingle
  from a Linux source remains a separate feature request.
- **Spotlight metadata (`com.apple.metadata:_kMDItemUserTags`, etc.)**
  These are already covered by the generic xattr pipeline; no AppleDouble-
  specific handling needed.
- **HFS+ compression decorators (`com.apple.decmpfs`).** Decorating
  compressed forks back to live data is a kernel-level operation the user-
  space rsync cannot perform; outside this scope.
- **macOS bundle (.app/.framework) directory awareness.** The proposal
  acts purely on regular files. Bundles are already preserved correctly
  by the regular file-list machinery.

## Conclusion

The data structures, name-pairing helpers, and resource-fork accessors
needed for AppleDouble-aware behaviour have all landed in PR b1c754cf3.
The remaining work is policy, not parsing: pick when to skip the sidecar,
pick when to absorb the sidecar back into native xattrs, and surface the
result in CLI output.

Implementing the seven-step plan above turns oc-rsync into the only mainline-
compatible rsync that recovers stranded macOS metadata when copying a FAT-
or SMB-resident snapshot back onto an Apple filesystem, without requiring a
wire-protocol change or a deviation from upstream's filter precedence rules.
