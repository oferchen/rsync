# Interop Known Failures Fix Plan

> **For implementers:** Work through this plan task-by-task; complete each before moving to the next.

**Goal:** Eliminate all fixable interop KNOWN_FAILURES, reducing the list from 15 to 5 (environment-dependent only).

**Architecture:** Each failure has existing infrastructure - the fixes are wiring, edge cases, and missing code paths. Tasks are ordered by dependency and difficulty: quick wins first, deeper protocol fixes later.

**Tech Stack:** Rust, upstream rsync C source (https://github.com/RsyncProject/rsync), protocol wire format, daemon/client architecture.

**Upstream source:** `target/interop/upstream-src/rsync-3.4.1/` (local) or https://github.com/RsyncProject/rsync

---

## Environment-dependent failures (CANNOT fix - keep in KNOWN_FAILURES)

| Failure | Reason |
|---------|--------|
| `up:protocol-31` | upstream 3.0.9 does not support protocol 31 |
| `oc:acls`, `up:acls` | upstream daemon build may lack `--enable-acl-support` |
| `oc:xattrs`, `up:xattrs` | upstream daemon build may lack `--enable-xattr-support` |

## Fixable failures (10 items, 8 tasks)

Priority order based on complexity and dependency:

1. **Task 1: file-vanished** - exit code propagation (small fix)
2. **Task 2: info-progress2** - progress output in daemon mode (small fix)
3. **Task 3: itemize** - two sub-issues: client-side output + daemon MSG_INFO (medium)
4. **Task 4: large-file-2gb** - daemon transfer of sparse 3GB file (investigate + fix)
5. **Task 5: hardlinks / hardlinks-relative** - hardlink inode/dev mapping in push (medium)
6. **Task 6: write-batch-read-batch** - batch file format compatibility (investigate + fix)
7. **Task 7: iconv** - charset conversion in daemon transfers (investigate + fix)
8. **Task 8: merge-filter** - wire DirMerge to generator walk_path (large)

---

### Task 1: Fix file-vanished exit code (standalone:file-vanished)

**Root cause:** The interop test creates a `--files-from` list with a non-existent file (`vanished_file.dat`). It expects exit code 23 or 24. The `IOERR_VANISHED` flag and exit code 24 are properly defined (`core/src/exit_code/codes.rs:101-104`) and the detection exists (`generator/protocol_io.rs:128-169`, `generator/file_list/walk.rs:315-330`). The issue is likely that the exit code isn't propagated through the CLI's local copy path when using `--files-from`.

**Files:**
- Investigate: `crates/cli/src/frontend/execution/drive/workflow/run.rs` - files-from exit code handling
- Investigate: `crates/engine/src/local_copy/error.rs:139-154` - vanished error detection
- Investigate: `crates/engine/src/local_copy/executor/sources/orchestration.rs:89-95` - vanished handling
- Test: `tools/ci/run_interop.sh` - remove from KNOWN_FAILURES after fix

**Steps:**
1. Run the standalone test locally to reproduce: extract and run `test_file_vanished()` from `run_interop.sh`
2. Trace exit code propagation: `--files-from` with missing file should set `IOERR_VANISHED` or `IOERR_GENERAL`
3. Fix the exit code propagation path so exit 23 or 24 is returned
4. Remove `standalone:file-vanished` from KNOWN_FAILURES
5. Push and verify CI

---

### Task 2: Fix info-progress2 output (standalone:info-progress2)

**Root cause:** The `--info=progress2` output format is implemented (`cli/src/frontend/progress_format.rs:132-232`, `cli/src/frontend/progress/live.rs:117-150`). The test checks for `[0-9]+%`, `xfr#`, or `to-chk=` patterns in stdout. The issue is likely that progress output goes to stderr but the test captures stdout, OR the progress callback is not wired for daemon transfers.

**Files:**
- Investigate: `crates/cli/src/frontend/execution/drive/summary.rs:77-81` - progress writer routing
- Investigate: `crates/cli/src/frontend/progress/live.rs:117-150` - Overall mode output
- Test: `tools/ci/run_interop.sh` - `test_info_progress2()` at line 1549

**Steps:**
1. Run `test_info_progress2()` locally to reproduce
2. Check whether progress output goes to stdout (captured as `$transfer_log.out`) or stderr
3. If routing issue: fix the output writer selection for `--info=progress2` mode
4. If callback not wired: ensure the progress callback fires during daemon transfers
5. Verify the output format matches `xfr#N` and `to-chk=N/M` patterns
6. Remove `standalone:info-progress2` from KNOWN_FAILURES
7. Push and verify CI

---

### Task 3: Fix itemize output (oc:itemize, up:itemize)

**Root cause:** Two separate issues:

**3a: oc:itemize (client push)** - When oc-rsync is the CLIENT pushing to upstream daemon, `maybe_emit_itemize()` in `generator/protocol_io.rs:174` checks `client_mode` and returns early. This is correct for MSG_INFO (server→client). But the CLIENT should also produce itemize output LOCALLY to stdout, like upstream's `log.c:rwrite()` which writes to FCLIENT when `am_server` is false. This local client-side itemize output path is missing.

**3b: up:itemize (daemon receive)** - When upstream pushes to oc-rsync daemon, the oc-rsync RECEIVER should emit MSG_INFO frames back to the client. The receiver's `emit_itemize()` (`receiver/mod.rs:412`) checks `!client_mode && info_flags.itemize`. The `info_flags.itemize` flag must be set from the client's `--log-format=%i` argument. Verify the daemon parses `--log-format=%i` and sets the flag.

**Files:**
- Modify: `crates/cli/src/frontend/execution/drive/workflow/run.rs` or `crates/core/src/client/summary/mod.rs` - add client-side itemize output callback
- Investigate: `crates/transfer/src/generator/transfer.rs:581+` - generator run() where itemize should be output locally
- Investigate: server arg parsing for `--log-format=%i` → `info_flags.itemize = true`
- Investigate: `crates/transfer/src/receiver/mod.rs:412-424` - receiver emit_itemize
- Investigate: `crates/transfer/src/writer/msg_info.rs:34-44` - MsgInfoSender for ServerWriter
- Reference: upstream `log.c:330-340`, `sender.c:287,430`

**Steps:**
1. Read upstream `log.c:rwrite()` to understand client-side vs server-side output routing
2. For 3a: add client-side itemize output path - when `client_mode=true`, the generator should format itemize lines and write them to a client-visible output (stdout or callback)
3. For 3b: verify that the daemon's server arg parser sets `info_flags.itemize = true` when `--log-format=%i` is received
4. For 3b: verify the receiver's writer is multiplexed so `send_msg_info()` actually sends MSG_INFO
5. Run itemize interop test in both directions
6. Remove `oc:itemize` and `up:itemize` from KNOWN_FAILURES
7. Push and verify CI

---

### Task 4: Fix large-file-2gb (standalone:large-file-2gb)

**Root cause:** The test creates a sparse 3GB file and transfers via daemon. The 64-bit size support exists in the protocol. Likely issues: sparse file handling in daemon mode, timeout during large transfer, or size comparison logic.

**Files:**
- Investigate: `crates/transfer/src/receiver/transfer.rs` - large file receive path
- Investigate: `crates/protocol/src/varint/` - 64-bit size encoding
- Test: `tools/ci/run_interop.sh` - `test_large_file_2gb()` at line 1582

**Steps:**
1. Run `test_large_file_2gb()` locally to reproduce the failure
2. Check error output to identify the specific failure point (transfer failure? size mismatch? checksum?)
3. Fix the identified issue
4. Remove `standalone:large-file-2gb` from KNOWN_FAILURES
5. Push and verify CI

---

### Task 5: Fix hardlinks in daemon push (oc:hardlinks, oc:hardlinks-relative)

**Root cause:** When oc-rsync client pushes with `-H` to upstream daemon, hardlinks are not detected/encoded correctly. The test checks that `hello.txt` and `hardlink.txt` have the same inode at the destination. The hardlink table (`protocol/src/flist/hardlink/`) and detection code exist but the generator's file list building in client/push mode may not be populating hardlink metadata correctly.

**Files:**
- Investigate: `crates/transfer/src/generator/file_list/hardlinks.rs` - hardlink detection during file list build
- Investigate: `crates/protocol/src/flist/hardlink/table.rs` - HardlinkTable usage
- Investigate: `crates/protocol/src/flist/write/mod.rs` - hardlink flags in wire encoding
- Reference: upstream `hlink.c` and `flist.c` hardlink handling

**Steps:**
1. Run hardlinks interop test locally to reproduce
2. Read upstream `hlink.c:init_hard_links()` and `flist.c` to understand how hardlinks are detected and encoded in the file list
3. Compare with oc-rsync's generator `build_file_list()` path - verify inode/dev metadata is collected and written to wire
4. Fix the hardlink detection/encoding in the generator's file list building for client push mode
5. For hardlinks-relative: verify the `-R` flag doesn't interfere with hardlink detection
6. Remove `oc:hardlinks` and `oc:hardlinks-relative` from KNOWN_FAILURES
7. Push and verify CI

---

### Task 6: Fix write-batch/read-batch roundtrip (standalone:write-batch-read-batch)

**Root cause:** The test runs 5 scenarios: upstream-write/oc-read, oc-write/upstream-read, and daemon-mode batch. The batch file format is implemented (`crates/batch/`) with working unit tests. The interop failure suggests format incompatibility with upstream rsync's batch files.

**Files:**
- Investigate: `crates/batch/src/writer.rs` - BatchWriter
- Investigate: `crates/batch/src/reader/mod.rs` - BatchReader
- Investigate: `crates/batch/src/replay.rs:333-507` - replay logic
- Reference: upstream `batch.c` for format specification

**Steps:**
1. Run `test_write_batch_read_batch()` locally to identify which scenario fails
2. Compare batch file header format with upstream `batch.c`
3. If header mismatch: fix the wire format in writer.rs or reader.rs
4. If replay issue: fix the delta application or file list handling in replay.rs
5. Test all 5 scenarios pass
6. Remove `standalone:write-batch-read-batch` from KNOWN_FAILURES
7. Push and verify CI

---

### Task 7: Fix iconv charset conversion (standalone:iconv)

**Root cause:** The `iconv` feature is enabled by default in the workspace. The implementation uses `encoding_rs` crate (`protocol/src/iconv/converter.rs`). The test checks UTF-8 identity conversion and UTF-8→ISO-8859-1 cross-charset conversion. Likely issue: filename encoding not applied during daemon file list transmission, or the `--iconv` argument not forwarded to the server.

**Files:**
- Investigate: `crates/protocol/src/iconv/converter.rs` - FilenameConverter
- Investigate: `crates/protocol/src/flist/write/encoding.rs` - filename encoding during write
- Investigate: `crates/cli/src/frontend/execution/options/iconv.rs` - --iconv parsing
- Investigate: `crates/core/src/client/config/iconv.rs` - IconvSpec

**Steps:**
1. Run `test_iconv()` locally to reproduce
2. Check if identity conversion (UTF-8→UTF-8) passes or fails
3. If identity fails: the converter or filename encoding path isn't wired
4. If only cross-charset fails: check encoding_rs conversion for Latin-1 characters
5. Fix the identified issue
6. Remove `standalone:iconv` from KNOWN_FAILURES
7. Push and verify CI

---

### Task 8: Wire DirMerge to generator walk_path (oc:merge-filter)

**Root cause:** This is the largest fix. Per-directory merge filters (`.rsync-filter` files loaded via `-FF`) work in local copy mode (`engine/src/local_copy/dir_merge/`) but NOT in remote transfers. The generator's `parse_received_filters()` (`transfer/src/generator/filters.rs:172-187`) explicitly skips DirMerge rules with a comment explaining the gap. The generator's `walk_path()` (`transfer/src/generator/file_list/walk.rs`) doesn't read per-directory `.rsync-filter` files.

**Files:**
- Modify: `crates/transfer/src/generator/filters.rs:172-187` - stop skipping DirMerge rules
- Modify: `crates/transfer/src/generator/file_list/walk.rs` - read .rsync-filter files during walk
- Reference: `crates/engine/src/local_copy/dir_merge/` - existing DirMerge parsing infrastructure
- Reference: `crates/engine/src/local_copy/context_impl/transfer.rs:59-153` - `enter_directory()` implementation
- Reference: upstream `exclude.c` per-directory filter handling

**Steps:**
1. Read the existing DirMerge infrastructure in `engine/src/local_copy/dir_merge/`
2. Read upstream `exclude.c` to understand per-directory filter file loading
3. In `generator/filters.rs`: accept DirMerge rules instead of skipping them, store the merge specs
4. In `generator/file_list/walk.rs`: when entering a directory, check for the merge filename (e.g., `.rsync-filter`), parse it using the existing dir_merge parsing infrastructure, and inject rules into the active FilterSet
5. Handle the `no_inherit` modifier to clear parent rules when leaving directories
6. Test with `-FF` flag against upstream daemon
7. Remove `oc:merge-filter` from KNOWN_FAILURES
8. Push and verify CI

---

## Verification

After all tasks, the KNOWN_FAILURES array should contain only:

```bash
KNOWN_FAILURES=(
  "up:protocol-31"
  "oc:acls"
  "up:acls"
  "oc:xattrs"
  "up:xattrs"
)
```

Run the full interop suite to confirm all previously-failing tests now pass:
```bash
bash tools/ci/run_interop.sh
```
