# RSS-6: backward-compat audit for the pool-allocator `FileEntry` shape

Task: RSS-6. Branch: `docs/rss-6-fileentry-backcompat-audit`.
Prerequisites: `docs/audits/rss-3-fileentry-size-breakdown.md` (per-field
cost), `docs/design/rss-5-fileentry-pool-shape.md` (target struct, accessor
contract, lifecycle). Downstream: RSS-7 (`name: PathBuf -> Spur`) and
RSS-8 (`dirname: Arc<Path> -> Spur`).

## Summary

Every public path-returning accessor on `FileEntry`
(`name() -> &str`, `path() -> &PathBuf`, `dirname() -> &Arc<Path>`,
`name_bytes() -> Cow<'_, [u8]>`, `link_target() -> Option<&PathBuf>`)
plus every direct constructor (`new_file`, `new_directory`, `new_symlink`,
`new_block_device`, `new_char_device`, `new_fifo`, `new_socket`,
`from_raw_bytes`, the test-only `from_raw`) and the mutation helpers
(`prepend_dir`, `strip_leading_slashes`, `set_dirname`) was inspected
against every workspace consumer.

Counts (production code only, excluding `_tests.rs` and bench files):

- **A. SHIM-COMPATIBLE**: 11 sites. Mostly leaf consumers that take
  `&str`, `&Path`, or `&[u8]` and never store the borrow past the
  immediate expression. Threading a `&PathInterner` arg in is a one-token
  change at every call site because the parent `FileList` / `Vec<FileEntry>`
  is already in scope.
- **B. SIGNATURE BREAK**: 9 distinct symbols. Constructors and mutators
  in `crates/protocol/src/flist/entry/{constructors,accessors}.rs` plus
  the cohort/plan builders in `crates/engine/src/delete/plan.rs`. Each
  must grow an `&mut PathInterner` parameter (constructors) or be
  rerouted through a wrapping interner-aware helper (cohort/plan).
- **C. INTERNAL ONLY**: 4 sites. Tests, debug formatting, `extract_dirname`
  helper - not on the public crate boundary.

Total touched files: ~26 production files. Hot consumer area is
`crates/transfer/src/receiver` (10 sites), then
`crates/protocol/src/flist/{name_cmp,sort,incremental,trace,write}` (8),
`crates/batch/src/replay` (4), `crates/engine/src/delete` (3).

The wire-byte parity check (`name_bytes()` -> wire encoder) sits on the
critical path and is the single load-bearing accessor: its `Cow` return
and `path_bytes_to_wire` invariant must survive interner round-trip
byte-for-byte. RSS-5's byte-keyed rodeo (open question #8) is the
specific design choice that keeps this site untouched.

## Inventory of public path-returning surface

From `crates/protocol/src/flist/entry/accessors.rs` and
`constructors.rs`:

| Symbol | Today's signature | Returns |
|---|---|---|
| `FileEntry::name` | `&self -> &str` | `&str` borrowed from `self.name: PathBuf` |
| `FileEntry::path` | `&self -> &PathBuf` | `&PathBuf` direct field borrow |
| `FileEntry::dirname` | `&self -> &Arc<Path>` | `&Arc<Path>` direct field borrow |
| `FileEntry::name_bytes` | `&self -> Cow<'_, [u8]>` | wire-form bytes (borrowed on Unix, owned on Windows) |
| `FileEntry::link_target` | `&self -> Option<&PathBuf>` | `extras.link_target` field |
| `FileEntry::prepend_dir` | `&mut self, &Path -> ()` | mutates `self.name` and `self.dirname` |
| `FileEntry::strip_leading_slashes` | `&mut self -> ()` | mutates `self.name` and `self.dirname` |
| `FileEntry::set_dirname` | `&mut self, Arc<Path> -> ()` | replaces `self.dirname` |
| `FileEntry::new_file` / `new_directory` / `new_symlink` / `new_block_device` / `new_char_device` / `new_fifo` / `new_socket` | `PathBuf, ... -> Self` | constructors |
| `FileEntry::from_raw_bytes` | `Vec<u8>, ... -> Self` | wire-decode constructor |
| `FileEntry::from_raw` (cfg(test)) | `PathBuf, ... -> Self` | test-only |

`link_target` stays boxed inside `FileEntryExtras` per RSS-5 non-goal
#2; its `Option<&PathBuf>` return is unchanged. Listed for completeness.

## Consumer-call-site table

Conventions: `crate::path:line | symbol used | classification | notes`.
Call sites in `tests.rs`, `*_tests.rs`, `tests/`, and `benches/` are
collapsed into "(tests)" rows because they all rewrite together when
the constructor changes. Production sites are enumerated.

### Group 1: `name() -> &str`

| Call site | Classification | Notes |
|---|---|---|
| `crates/protocol/src/flist/sort.rs:331` (`flist_clean` dedupe) | A | takes `&str` to compare; `Vec<FileEntry>` and its parent context can pass `&interner` |
| `crates/protocol/src/flist/incremental/mod.rs:148,152,153,184,185,360` (`IncrementalFileList::push`, `release_pending_children`, debug trace) | A | each takes `&str` then converts to `String` for the `created_dirs`/`pending` maps; `IncrementalFileList` becomes the natural owner of the interner |
| `crates/protocol/src/flist/incremental/ready_entry.rs:130,169` (`process_ready_entry` filter dispatch) | A | takes `&str` for the `is_excluded`/`failed_ancestor` closure params; closure callers can co-own the interner |
| `crates/protocol/src/flist/read/mod.rs:728` (debug log in `read_file_entry`) | A | inline `debug_log!`, single expression, no storage |
| `crates/protocol/src/flist/trace.rs:179` (`output_flist_entry`) | A | inline display; signature already takes `&FileEntry`, add `&PathInterner` |
| `crates/batch/src/replay/mod.rs:165,169,237` and `replay/delta_phase.rs:405` (replay dest path join + verbose print) | A | `dest_root.join(entry.name())` - takes `&str`, immediate consumer; passes the batch reader's interner through `replay_*` |
| `crates/transfer/src/generator/file_list/inc_recurse.rs:72` (debug trace) | A | inline log |
| `crates/transfer/src/receiver/directory/creation.rs:330,336,340,382` (`failed_dirs.failed_ancestor` / `failed_dirs.mark_failed`) | A | takes `&str`, maps it into an owned String key in `FailedDirs`; `FailedDirs` owns its own `HashSet<String>` so interner just borrows |
| `crates/transfer/src/receiver/directory/links.rs:255` (debug log) | A | inline format |
| (tests) `entry/tests.rs`, `read/tests.rs`, `write/tests/*.rs`, `incremental/tests.rs`, `batched_writer/tests.rs` (~120 sites) | A | all rewrite together when constructors take `&mut PathInterner` |

### Group 2: `path() -> &PathBuf`

| Call site | Classification | Notes |
|---|---|---|
| `crates/protocol/src/flist/name_cmp.rs:106` (`basename_bytes` helper) | A | takes `&Path` via `as_path()`; `f_name_cmp` already takes `&FileEntry, &FileEntry` - signature grows by one ref |
| `crates/protocol/src/flist/read/mod.rs:654` (decoder dirname extraction) | A | `entry.path().parent()`; this is *inside* `FileListReader::read_file_entry` which already owns `self.dirname_interner` |
| `crates/transfer/src/pipeline/async_dispatch.rs:42` (`dest_dir.join(entry.path())`) | A | needs `&Path`; immediate join, no storage |
| `crates/transfer/src/generator/itemize.rs:223` (`format_itemize_line`) | A | needs `&Path` for `display()`; takes `&FileEntry` already |
| `crates/transfer/src/generator/transfer/transfer_loop.rs:298,437,447` (sender debug + request build) | A | inline format and `&Path` arg to request struct |
| `crates/transfer/src/receiver/file_list/sanitize.rs:39,107` (path checks + `strip_leading_slashes`) | A | needs `&Path`; immediate `has_root()` / `path_contains_dot_dot` check |
| `crates/transfer/src/receiver/quick_check.rs:147` (`relative_path = entry.path()`) | A | `&Path`, immediate `join` |
| `crates/transfer/src/receiver/transfer/sync.rs:105` | A | `&Path`, immediate `join` |
| `crates/transfer/src/receiver/transfer/candidates.rs:58,119,138` (verbose + `dest_dir.join(entry.path())`) | A | immediate join, no storage |
| `crates/transfer/src/receiver/transfer/pipeline.rs:196,215,234,261,362,450` (basis-find + info logs) | A | immediate borrow into `BasisFileConfig.relative_path` (lifetime-bound to the loop iteration) and into `info_log!` |
| `crates/transfer/src/receiver/transfer/pipelined.rs:128,162` and `pipelined_incremental.rs:141` | A | identical pattern |
| `crates/transfer/src/receiver/directory/creation.rs:65,244,322` and `directory/deletion.rs:72` | A | identical pattern |
| `crates/transfer/src/receiver/directory/links.rs:53,221,243,272` | A | identical pattern; line 243 uses `entry.path().display().to_string()` which owns immediately |
| `crates/engine/src/delete/cohort_index.rs:224` (`basename_of(entry.path())`) | A | `&Path -> Option<OsString>`, owns immediately |
| `crates/engine/src/delete/extras.rs:135` (`entry.path().file_name()`) | A | `&Path -> Option<&OsStr>`, owned by `to_os_string()` |
| `crates/flist/src/parallel.rs:446` | A | `&Path`, immediate join |

Pattern is uniform: every production `path()` consumer takes `&Path` for
one immediate `join`, `display()`, `has_root()`, `parent()`, or
`file_name()` and drops the borrow. None stores `&PathBuf` across an
`await` or a `match` arm outliving the interner. RSS-5's
`path(&self, paths: &PathInterner) -> &Path` is mechanical at every site.

### Group 3: `dirname() -> &Arc<Path>`

| Call site | Classification | Notes |
|---|---|---|
| `crates/protocol/src/flist/name_cmp.rs:61,62,89,90,107` (`f_name_cmp` / `name_cmp_eq` / `basename_bytes`) | A | each call site immediately feeds `path_bytes_to_wire(&Path)` - changing return type to `&Path` is no-op |
| (tests in `entry/tests.rs`, `intern.rs` tests) | A | tests assert `Arc::ptr_eq`; under the new scheme those become `Spur ==` |

Cleanest collapse: zero production consumers depend on the `Arc<Path>`
*type*, only on the byte view. Returning `&Path` per RSS-5 preserves
every existing call.

### Group 4: `name_bytes() -> Cow<'_, [u8]>`

| Call site | Classification | Notes |
|---|---|---|
| `crates/protocol/src/flist/write/mod.rs:376` (`write_entry`) | A (with care) | wire encoder; must preserve byte-for-byte parity. RSS-5's byte-keyed rodeo (open question #8) is precisely the design choice that keeps this site untouched |

Single load-bearing site. The `name_bytes()` return type does **not**
change - the body routes through `paths.resolve_bytes(self.name)` and
re-wraps in `Cow::Borrowed`. Signature gains one `&PathInterner` arg,
matching RSS-5's `C` (Change-with-wrapper) classification.

### Group 5: `link_target() -> Option<&PathBuf>`

| Call site | Classification | Notes |
|---|---|---|
| `crates/protocol/src/flist/write/encoding.rs:113,383` (write symlink target + stats accumulation) | C | extras stay boxed (RSS-5 non-goal #2); no change |
| `crates/transfer/src/generator/itemize.rs:236` | C | unchanged |
| `crates/transfer/src/receiver/directory/links.rs:48` | C | unchanged |
| `crates/batch/src/replay/mod.rs:185` and `delta_phase.rs:412` | C | unchanged |
| `crates/core/src/client/summary/metadata.rs:79` (calls `metadata.symlink_target()`, not `FileEntry::link_target()`) | C | not the same symbol; ignored |

### Group 6: constructors (`new_file`, `from_raw_bytes`, etc.)

| Symbol | Call sites (production) | Classification |
|---|---|---|
| `FileEntry::new_file` | `crates/engine/src/delete/plan.rs:249` (synth FileEntry for delete plan), `crates/engine/src/delete/traversal.rs:172` (synth FileEntry pair for `f_name_cmp`), `crates/transfer/src/generator/file_list/entry.rs:*` (walker builds entries from `fs::Metadata`) | **B** |
| `FileEntry::new_directory` | `crates/engine/src/delete/plan.rs:246,172` and walker | **B** |
| `FileEntry::new_symlink` | `crates/engine/src/delete/plan.rs:247` and walker | **B** |
| `FileEntry::new_block_device` / `new_char_device` / `new_fifo` / `new_socket` | walker entry builder | **B** |
| `FileEntry::from_raw_bytes` | `crates/protocol/src/flist/read/mod.rs:642` (wire decoder) | **B** |
| `FileEntry::from_raw` (cfg(test)) | tests only | C |
| `FileEntry::prepend_dir` | `crates/protocol/src/flist/incremental/*` (INC_RECURSE join) | **B** (`&mut PathInterner` add) |
| `FileEntry::strip_leading_slashes` | `crates/transfer/src/receiver/file_list/sanitize.rs:108` | **B** (`&mut PathInterner` add) |
| `FileEntry::set_dirname` | `crates/protocol/src/flist/read/mod.rs:659` | **B** (takes a `Spur` instead of `Arc<Path>`) |

Surprise: the synthetic-FileEntry sites in
`engine/src/delete/{plan,traversal}.rs` build transient `FileEntry`
values just to feed `f_name_cmp` or wrap a `DeleteEntry`. After RSS-7/8
they need an interner. Cleanest mitigation: keep the synthetic builder
local to `engine/src/delete/` with a single-shot `Rodeo` (two
`get_or_intern` calls plus `Rodeo` allocation, dominated by existing
heap work).

## Invariants check

### 1. `Path::as_os_str()` / `as_encoded_bytes()` round-trip

`from_raw_bytes(Vec<u8>, ...)` preserves arbitrary non-UTF-8 bytes on
Unix via `OsStr::from_bytes(&name)`. The new shape stores the basename
as `Spur` into a byte-keyed rodeo (RSS-5 open question #8). **Verdict:
preserved** as long as the rodeo is `[u8]`-keyed. If lasso 0.7 lacks
the byte-key variant the `bumpalo::Bump + HashMap<&[u8], Spur>`
fallback gives identical semantics. `name_bytes()` must round-trip
identical bytes through any `intern -> resolve` pair - the single most
important wire-format invariant.

### 2. Equality and hashing

Today's `PartialEq for FileEntry` byte-compares `self.name`
(`PathBuf` deep compare). Same-interner becomes `Spur ==` (u32);
cross-interner is meaningless. RSS-5 already proposes removing the
hand-written `PartialEq` and exposing `entry.eq(other, &paths)`.

`FileEntry` has **no `Hash` derive** and **no `serde::Serialize` impl**
in the workspace (zero matches in `crates/protocol/src/flist`). No
hash- or serialization-stability migration step. **Verdict: equality
migration is the only ergonomic break, already captured in RSS-5.**

### 3. Wire-byte parity

`name_bytes()` -> `path_bytes_to_wire(&self.name)` is the only path
into the wire encoder (`write/mod.rs:376`). The byte-keyed rodeo
returns the exact `&[u8]` that was interned; `path_bytes_to_wire` then
applies platform `/`-vs-`\` normalisation. **Verdict: byte parity holds**
provided the rodeo is keyed on the same bytes the wire encoder reads.
RSS-7 should add a golden test that pumps a non-UTF-8 basename through
`intern -> from_raw_bytes -> write_entry -> read_file_entry ->
name_bytes()` and asserts byte equality.

### 4. `Arc::ptr_eq` semantics

Test-only callers in `entry/tests.rs:275,290` and `intern.rs` tests
assert `Arc::ptr_eq(entry.dirname(), &shared)`. Under the new shape
this becomes `entry.dirname_spur() == shared_spur`. **Verdict: test
rewrite required; no production impact.**

### 5. `Default::default()` and drop ordering

`FileEntry` does not derive `Default`; `Spur` does not impl `Default`
either - no new construction path needs an interner arg. RSS-5
§"Failure modes" #1 confirms the borrow checker refuses stale-`Spur`
use, so drop ordering is type-system-enforced. No runtime check.

### 6. Wire-protocol stability

`name_bytes()` is the only `FileEntry` accessor on wire-write;
`from_raw_bytes` the only constructor on wire-read. Both preserve
identical byte semantics under a byte-keyed rodeo. **Verdict: wire
format unchanged; golden byte tests stay green at every staged step.**

## Migration plan signal: which step lands first per area

Counting **B**-classified symbols by owning area to drive the
RSS-7 / RSS-8 sequencing:

| Area | B-symbol count | First-touch task |
|---|---|---|
| `crates/protocol/src/flist/read/` | 2 (`from_raw_bytes`, `set_dirname`) | RSS-7 (the `name: Spur` flip happens here; `set_dirname` becomes a `Spur` setter under RSS-8) |
| `crates/protocol/src/flist/entry/` | 9 constructors + `prepend_dir` + `strip_leading_slashes` | RSS-7 (constructors); RSS-8 (`prepend_dir` / `strip_leading_slashes` re-intern joined path) |
| `crates/protocol/src/flist/incremental/` | 1 (`prepend_dir` consumer) | RSS-8 |
| `crates/transfer/src/receiver/file_list/sanitize.rs` | 1 (`strip_leading_slashes` consumer) | RSS-8 |
| `crates/transfer/src/generator/file_list/entry.rs` | 7 (walker builds entries) | RSS-7 (walker gains its own `Rodeo`) |
| `crates/engine/src/delete/{plan,traversal}.rs` | 5 synthetic constructor calls | RSS-7 (synthetic builders gain a one-shot `Rodeo` local helper) |

**RSS-7 must land first**, because the larger field (`name: PathBuf -> Spur`)
also pulls in every constructor, and the constructor signature change is
the costliest API break (B-class). RSS-8's `dirname: Arc<Path> -> Spur`
flip is structurally lighter: only `set_dirname`, `prepend_dir`,
`strip_leading_slashes`, and the `read_file_entry` decoder call site
need updating; every other `dirname()` consumer is A-class because they
already drop down to `&Path` immediately.

### Top 3 B-sites that drive migration order

1. **`FileEntry::from_raw_bytes` in `crates/protocol/src/flist/read/mod.rs:642`** -
   the wire-decode hot path. Every received file list entry funnels
   through it. Signature changes from `(Vec<u8>, ...) -> Self` to
   `(&mut PathInterner, &[u8], ...) -> Self`. The decoder
   (`FileListReader`) already owns its `dirname_interner`; RSS-7
   retypes that field to `Rodeo` and threads `&mut self.dirname_interner`
   into the call. One PR.
2. **`FileEntry::new_file` + companions in
   `crates/transfer/src/generator/file_list/entry.rs`** - the sender
   walker that constructs entries from `fs::Metadata`. Today it hits
   `extract_dirname()` -> `Arc::from(parent)` per entry (RSS-3 "smoking
   gun"). RSS-7 makes the walker own a `Rodeo`; every `new_*` call
   becomes `new_*(&mut interner, ...)`. This is the second-largest
   surface area after constructors themselves.
3. **`FileEntry::set_dirname` in
   `crates/protocol/src/flist/read/mod.rs:659`** - the post-construct
   interning step in the decoder. Today it accepts an `Arc<Path>`;
   RSS-8 makes it accept a `Spur` and replaces the `dirname_interner.intern(p)`
   chain with a single `get_or_intern` call directly inside
   `read_file_entry`, eliding the separate `set_dirname` step entirely.
   Worth flagging early so RSS-7 does not freeze a transitional API
   that RSS-8 immediately rewrites.

## Open questions for the RSS-7 author

1. **Should `FileEntry::from_raw_bytes` keep the `Vec<u8>` argument or
   switch to `&[u8]`?** Today's `Vec<u8>` lets the constructor reuse
   the buffer as the PathBuf backing on Unix. Under interning the buffer
   is hashed-and-copied into the rodeo anyway, so taking `&[u8]` avoids
   a needless `Vec` allocation on the caller side. RSS-3's smoking gun
   counts this as part of the per-entry allocation budget.
2. **Should `set_dirname` survive RSS-8 or be folded into the decoder?**
   The only production caller is in
   `crates/protocol/src/flist/read/mod.rs:659`, immediately after
   `from_raw_bytes`. If RSS-7 already threads `&mut PathInterner` into
   the constructor, the decoder can intern dirname inline and `set_dirname`
   becomes vestigial. Killing it is one less B-site for RSS-8 to migrate.
3. **Where should the synthetic-FileEntry builder in
   `crates/engine/src/delete/{plan,traversal}.rs` live?** Three options:
   (a) inline a per-call `Rodeo`; (b) thread the receiver's `&PathInterner`
   down into the delete pipeline; (c) keep a private static `Rodeo` behind
   a `Mutex` (rejected: contention + lifetime mismatch). Recommendation:
   (a), because the delete pipeline already pays a per-entry allocation
   for the `PathBuf`. Cost is two `get_or_intern` calls.
4. **`prepend_dir` and `strip_leading_slashes` re-intern the joined
   path.** Under per-segment interner ownership (RSS-5 §INC_RECURSE),
   the segment's `Rodeo` is borrowed mutably for the prepend - confirm
   no read borrow is live at the call site. The known call sites are
   `crates/protocol/src/flist/incremental/*` (INC_RECURSE re-build) and
   `crates/transfer/src/receiver/file_list/sanitize.rs:108` (post-decode
   path cleanup); both run before consumers acquire read borrows.
5. **`f_name_cmp` is called from `engine/src/delete/plan.rs` and
   `traversal.rs` on entries from different sources** (segment entries
   vs synthetic builders). Under per-segment interners those entries can
   carry handles from *different rodeos*. Solution: `f_name_cmp` already
   resolves to byte arrays via `path_bytes_to_wire`, so the comparison
   is byte-based, not handle-based, and survives cross-interner mixing.
   Confirm in RSS-7 that no comparator anywhere shortcuts on `Spur ==`
   without resolving first.
6. **The `Cow<'_, [u8]>` return of `name_bytes()`** today borrows from
   the entry's owned `PathBuf` on Unix. Under interning it borrows from
   the rodeo. The borrow checker will require `name_bytes(&self,
   paths: &'a PathInterner) -> Cow<'a, [u8]>` - one fresh lifetime
   parameter. Production callers do not store this `Cow` across yields,
   so the lifetime tightening is mechanical.

## Cross-references

- `docs/audits/rss-3-fileentry-size-breakdown.md` - field-by-field
  cost basis for the migration.
- `docs/design/rss-4-arena-allocator-eval.md` - rationale for `lasso`.
- `docs/design/rss-5-fileentry-pool-shape.md` §"API shape" - the
  accessor-with-interner-param signature this audit is verifying.
- `crates/protocol/src/flist/entry/core.rs:32-83` - canonical struct.
- `crates/protocol/src/flist/entry/accessors.rs:11-481` - every public
  accessor in scope of this audit.
- `crates/protocol/src/flist/entry/constructors.rs:18-173` - every
  constructor in scope.
- `crates/protocol/src/flist/intern.rs:42-114` - current `PathInterner`
  to be retyped to `Rodeo` / `RodeoReader`.
- `crates/protocol/src/flist/read/mod.rs:116,168,642,654-659` -
  decoder ownership, `from_raw_bytes` call, `set_dirname` call.
- `crates/protocol/src/flist/name_cmp.rs:60-124` - byte-based comparator
  that survives interner migration unchanged.
- `crates/protocol/src/flist/write/mod.rs:376` and
  `write/encoding.rs:118` - the wire-byte critical path
  (`name_bytes()` and `link_target()`).
- `crates/engine/src/delete/plan.rs:243-249` and
  `traversal.rs:172` - synthetic `FileEntry` builders that need a
  one-shot `Rodeo` helper post RSS-7.
- `crates/transfer/src/generator/file_list/entry.rs` - sender walker
  constructors (the second-largest constructor consumer after the
  decoder).
