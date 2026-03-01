# Incremental Recursion File List Exchange

## Problem

Our implementation sends the complete file list in one batch before starting
transfers. Upstream rsync with `INC_RECURSE` streams sub-lists per directory
during the transfer, reducing memory and improving time-to-first-transfer.

The codebase has extensive infrastructure (`INC_RECURSE` flag, `IncrementalFileList`,
`NDX_FLIST_EOF`, `IncrementalFileListReceiver`) but negotiation is hardcoded off
at every entry point.

## Upstream Protocol Semantics

### Negotiation
1. Client includes `'i'` in `-e` capability string
2. Server checks conditions (`recurse && !use_qsort && compatible_delete_mode`)
3. Server sets `CF_INC_RECURSE` (bit 0) in compat flags byte
4. Both sides derive `inc_recurse` from compat flags

### Wire Protocol
1. **Initial file list**: Top-level entries only. Directories diverted to `dir_flist`.
   ID lists NOT sent (deferred).
2. **Sub-lists on demand**: Sender writes `NDX_FLIST_OFFSET - dir_ndx` followed by
   file entries followed by zero byte. Interleaved with transfer data.
3. **Completion**: Sender writes `NDX_FLIST_EOF` when all directories exhausted.
4. **Traversal**: Depth-first (first-child, next-sibling, parent-backtrack).

### Index Space
- First sub-list: `ndx_start = 1`
- Each subsequent: `ndx_start = prev.ndx_start + prev.used + 1`
- Gap at `ndx_start - 1` reserved for parent directory entry

### Lookahead
- Sender maintains 1000-file lookahead (`MIN_FILECNT_LOOKAHEAD`)
- Sends sub-lists until enough files are queued ahead of processing

## Architecture

### New Types (protocol crate)

**`FileListSegment`** — One sub-list with its own index range:
- `ndx_start: i32` — global NDX offset
- `parent_dir_ndx: i32` — parent directory's global NDX
- `entries: Vec<FileEntry>` — files in this segment

**`SegmentedFileList`** — Collection of segments with global NDX lookup:
- `segments: Vec<FileListSegment>` — ordered by ndx_start
- `dir_entries: Vec<(i32, FileEntry)>` — (global_ndx, dir_entry) for tree
- `flist_eof: bool`
- Methods: `add_segment()`, `get_by_ndx()`, `total_files()`, `is_complete()`

**`DirectoryTree`** — Depth-first traversal state for sender:
- Tree nodes with parent/first_child/next_sibling indices
- `send_dir_ndx: Option<usize>` — current position
- `send_dir_depth: usize`
- Methods: `add_directory()`, `next_directory()` (depth-first), `is_exhausted()`

### Negotiation Changes (transfer crate — setup.rs)

1. Add `allow_inc_recurse()` function implementing upstream conditions:
   - Requires recursive mode
   - Not `use_qsort` / `--no-inc-recursive`
   - Receiver: not `delete_before`, `delete_after`, `delay_updates`, `prune_empty_dirs`
   - Server: client must advertise `'i'` capability
2. Pass result to `build_compat_flags_from_client_info()`
3. Remove client-side `INC_RECURSE` masking (line 582)
4. Add `'i'` to daemon capability string

### Sender Changes (transfer crate — generator.rs)

1. `build_file_list()` → shallow mode when INC_RECURSE:
   - Scan only top-level + root directory contents
   - Divert directories to `DirectoryTree`
2. `send_extra_file_list()` — new method:
   - Pop next directory from tree (depth-first)
   - Write `NDX_FLIST_OFFSET - dir_ndx`
   - Scan directory, write entries, write end marker
   - Add discovered subdirectories to tree
   - Repeat until lookahead satisfied or tree exhausted
   - Write `NDX_FLIST_EOF` when tree empty
3. `run_transfer_loop()` — call `send_extra_file_list()` before each NDX read

### Receiver Changes (transfer crate — receiver.rs)

1. NDX reading: handle `ndx <= NDX_FLIST_OFFSET`:
   - Decode `dir_ndx = NDX_FLIST_OFFSET - ndx`
   - Call `receive_sub_file_list(dir_ndx)` to read entries
   - Add segment to `SegmentedFileList`
   - Add new files to transfer pipeline
2. Handle `NDX_FLIST_EOF` → set `flist_eof = true`
3. Adapt pipeline to work with growing file list

## Implementation Phases

### Phase 1: Data structures (protocol crate)
- `FileListSegment`, `SegmentedFileList`, `DirectoryTree`
- Unit tests for each

### Phase 2: Negotiation (transfer crate)
- `allow_inc_recurse()` function
- Un-hardcode `false` at all call sites
- Add `'i'` to daemon capability string

### Phase 3: Sender (transfer crate — generator)
- Shallow `build_file_list()` mode
- `send_extra_file_list()` with depth-first traversal
- Integration into transfer loop

### Phase 4: Receiver (transfer crate — receiver)
- Mid-stream sub-list reception
- `SegmentedFileList` integration
- Pipeline adaptation

### Phase 5: Tests
- Unit tests for data structures
- Golden byte tests for wire format
- Integration test with known sub-list sequence
