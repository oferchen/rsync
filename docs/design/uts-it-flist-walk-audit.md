# UTS-IT.11/12/13 - Flist walk audit for implicit directory itemize emission

Status: audit only. Implementation tracked separately under UTS-IT.14.

## Scope

The direction-arrow fix (UTS-IT.1-.10) closed the per-file `<`/`>` glyph
divergence. The remaining gap is the **implicit directory itemize row** that
upstream emits once per directory entry in the flist on initial transfers
under `-v` recursive runs:

```
cd+++++++++ ./
cd+++++++++ subdir/
cd+++++++++ subdir/nested/
```

These rows are part of the upstream `itemize.test` / `dirs.test` / `delete.test`
goldens and are emitted from the receiver's generator pass over directory
entries, not from any source-side walk.

## Upstream emission sites (3.4.4 source under `target/interop/upstream-src/rsync-3.4.4/`)

### Per-directory itemize call

- `generator.c:1480-1483` - inside `recv_generator()` directory branch:

  ```c
  if (itemizing && f_out != -1) {
      itemize(fnamecmp, file, ndx, statret, &sx,
              statret ? ITEM_LOCAL_CHANGE : 0, 0, NULL);
  }
  ```

  When the destination directory is absent (`statret < 0`), `itemize()` ORs
  in `ITEM_IS_NEW`, producing the `cd+++++++++` glyph at `generator.c:578`.
  When the directory already exists with matching attributes, `iflags == 0`
  and the row is suppressed by the standard gate at `generator.c:574-576`.

### Root-dir special case

The root entry `./` always reaches `recv_generator()` with `statret < 0` when
the receiver pre-flight-created the destination root via `setup_basis_dirs()`
because:

- `main.c:803-805` - `flist->files[0]->flags |= FLAG_DIR_CREATED` is set after
  the mkdir succeeds.
- `generator.c:1464-1465` - `if (file->flags & FLAG_DIR_CREATED) statret = -1;`
  forces the root row to fire even though stat succeeded.

### "created directory $todir" notice

- `main.c:807-808` - emitted once per receiver run, gated on
  `INFO_GTE(NAME, 1) || stdout_format_has_i`:

  ```c
  if (INFO_GTE(NAME, 1) || stdout_format_has_i)
      rprintf(FINFO, "created directory %s\n", dest_path);
  ```

  This is **not** an itemize row - it is a one-shot FINFO line that precedes
  the `cd+++++++++ ./` row.

## oc-rsync sites

### Recursive walk (UTS-IT.11)

- `crates/transfer/src/generator/file_list/walk.rs:159-282` -
  `walk_path_with_metadata()` is the **sender-side** flist builder, not the
  receiver's recv_generator analog. Directory entries are produced here at
  `walk.rs:90` (`metadata.is_dir()`) and `walk.rs:277`
  (`should_recurse = metadata.is_dir() && self.config.flags.recursive`).
  No itemize emission happens at the sender; itemize is a receiver-role
  output. The walk site is correctly silent.

- `crates/transfer/src/receiver/directory/creation.rs:38-76` -
  `create_directories()` is the receiver's analog of upstream's directory
  branch in `recv_generator()`. It iterates every `is_dir()` flist entry
  sequentially and calls `mkdir_at` / `apply_metadata`. **This is the
  walk site where the per-directory itemize row should fire.**

### Root-dir gap (UTS-IT.12)

- `crates/transfer/src/receiver/mod.rs:843-886` - `emit_itemize()` already
  has root-dir compensation logic (added by UTS-DD-itemize.3):

  ```rust
  let is_root_dir = entry.is_dir() && entry.path().as_os_str() == ".";
  if !is_root_dir && !iflags.has_significant_flags() {
      return Ok(());
  }
  let effective_iflags = if is_root_dir && !iflags.has_significant_flags() {
      ItemFlags::from_raw(
          ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW,
      )
  } else {
      *iflags
  };
  ```

  **Gap:** `emit_itemize()` is only invoked from per-file paths
  (`pipeline.rs:369`, `pipelined_incremental.rs:73`,
  `candidates.rs:267`). It is **not** invoked from
  `create_directories()`. As a result, even with the root-dir
  compensation logic in place, the `cd+++++++++ ./` row never fires for
  the root entry, and neither do the intermediate-dir rows.

  **Proposed insertion point:** at the end of the `create_directories()`
  per-entry loop body (around `creation.rs:115-130` after `mkdir_at` /
  `apply_metadata` completes), invoke `self.emit_itemize(writer, &iflags,
  entry)` with `iflags = ITEM_LOCAL_CHANGE | ITEM_IS_NEW` when the
  directory was newly created (`!dir_path.exists()` arm taken), or
  `iflags = 0` when it already existed (letting the existing gate
  suppress the row when nothing changed). The root entry will be picked
  up naturally because `dir_entries` already includes `relative_path ==
  "."`.

### Intermediate-dir gap (UTS-IT.13)

Same `create_directories()` site. The per-subdir rows (`cd+++++++++
subdir/`, `cd+++++++++ subdir/nested/`) come from the **same loop** as the
root row - upstream's `recv_generator()` is invoked once per flist entry
including directories, so all directory rows funnel through the same
`itemize()` call. The per-subdir gap is therefore a subset of the same
fix: once `emit_itemize()` is called from inside the
`create_directories()` loop body, every directory entry (root and
subdirs) gets its row.

```rust
// crates/transfer/src/receiver/directory/creation.rs around line 100+:
if !dir_path.exists() {
    // [existing mkdir_at logic]
    // PROPOSED: build iflags = ITEM_LOCAL_CHANGE | ITEM_IS_NEW and call
    // self.emit_itemize(writer, &iflags, entry).
}
```

### "created directory $todir" notice site

- `crates/transfer/src/receiver/transfer/setup.rs:246-267` - already wired
  by UTS-DD-itemize.3:

  ```rust
  if created_dest_root {
      if self.config.flags.info_flags.itemize && self.config.connection.client_mode {
          println!("created directory {}", dest_dir.display());
      }
  }
  ```

  The site is correct and matches upstream `main.c:807-808` semantics; the
  client-mode gate prevents server-mode receivers (SSH/daemon) from
  injecting the line into the multiplex stream. No follow-up audit needed
  for this site.

## Sequencing for UTS-IT.14 implementation

1. Plumb a `writer` reference (`MsgInfoSender + Write`) into
   `create_directories()` - currently the fn signature does not carry one.
   The existing per-file callers (`pipeline.rs`, `pipelined_incremental.rs`,
   `candidates.rs`) all hold a `writer` of the same trait bound; the
   call-site that invokes `create_directories()` (in `setup.rs` /
   `pipelined_incremental.rs`) already owns one.
2. Inside the `for (_, relative_path, dir_path)` loop in `create_directories()`,
   compute `iflags` per upstream `generator.c:1481-1482`:
   - if `!dir_path.exists()` was true at loop entry: `ITEM_LOCAL_CHANGE`
     (which `emit_itemize` will OR with `ITEM_IS_NEW` via the existing
     root-dir branch path, generalized to all dirs)
   - else: `ITEM_LOCAL_CHANGE` only if attributes diverge per quick-check
     semantics; else `0` (suppressed by the standard gate).
3. Generalize the `is_root_dir` branch in `emit_itemize()` from
   `path == "."` to `entry.is_dir()` so that any `is_dir() &&
   !has_significant_flags()` entry under FLAG_DIR_CREATED equivalence
   emits the full `cd+++++++++` glyph.
4. Regression tests (UTS-IT.15/.16) assert both the root `./` row and
   per-subdir rows fire on initial transfer; the existing
   `emit_itemize_root_directory_emits_creation_glyph_when_iflags_zero`
   test in `tests/symlinks_and_devices.rs:106` is the template.
5. Upstream-testsuite reruns (UTS-IT.18-.21) close the loop on
   `itemize.test`, `dirs.test`, `delete.test`, `output-options.test`.

## Cross-references

- Upstream sources read from `target/interop/upstream-src/rsync-3.4.4/`:
  `log.c`, `generator.c`, `main.c`.
- oc-rsync sites:
  `crates/transfer/src/generator/file_list/walk.rs`
  (sender-side walk, no itemize),
  `crates/transfer/src/receiver/directory/creation.rs` (the gap),
  `crates/transfer/src/receiver/mod.rs` (`emit_itemize`),
  `crates/transfer/src/receiver/transfer/setup.rs`
  ("created directory" notice).
- Related shipped audit work: UTS-DD-itemize.1-.4, UTS-IT.1-.10.
