# Spill Temp-File Failure Mode Audit (SRO-1)

Audit of `SpillableReorderBuffer` temp-file creation and usage paths in
`crates/engine/src/concurrent_delta/spill/`.

## Overview

The `SpillableReorderBuffer` wraps a bounded-capacity `ReorderBuffer` and
adds disk-backed overflow when estimated in-memory size exceeds a
configurable threshold (default 64 MB). Items above the threshold are
serialized to a temp file and transparently reloaded on delivery.

Upstream rsync processes files sequentially in `recv_files()` and never
needs this mechanism. The spill layer exists solely to bound the memory
cost of out-of-order parallel delta dispatch.

## Temp-File Backend Selection

Two backends exist (`spill/tempfile.rs`):

| Backend | Selection | Temp-file method | Auto-cleanup |
|---------|-----------|-----------------|--------------|
| `Spooled` | Default (no dir configured) | `tempfile::SpooledTempFile::new(1 MiB)` | Yes - RAII Drop |
| `Directory` | `with_spill_dir()` or `OC_RSYNC_SPILL_DIR` env | `tempfile::tempfile_in(dir)` | Yes - anonymous file, unlinked at creation |

### How the temp directory is chosen

1. **Default path (no configuration):** `SpooledTempFile` keeps payloads
   under 1 MiB in memory. Over 1 MiB it rolls to a system-default temp
   file via the `tempfile` crate, which uses `std::env::temp_dir()`
   (typically `/tmp` on Unix, `%TEMP%` on Windows).

2. **Explicit directory:** Set via `SpillableReorderBuffer::with_spill_dir()`,
   `OC_RSYNC_SPILL_DIR` env var, or `SpillPolicy::with_dir()`. The
   directory is created eagerly with `fs::create_dir_all()` at
   construction time. An anonymous temp file (`tempfile::tempfile_in()`)
   is opened inside it lazily on first spill.

3. **Precedence:** CLI flag > `OC_RSYNC_SPILL_DIR` env var > default
   spooled. Implemented in `SpillPolicy::apply_cli_overrides()`.

---

## Failure Mode Inventory

### FM-1: Temp-file creation fails (ENOSPC on spill directory)

- **Trigger:** Disk full at the moment `open_backend()` calls
  `tempfile::tempfile_in(dir)` or the spooled backend rolls over to disk
  and `std::env::temp_dir()` is full.
- **Current behavior:** `open_backend()` returns `io::Error`. This
  propagates through `write_record()` to `spill_item()` or
  `spill_candidates_whole_batch()`, which wraps it in `SpillError::Io`.
  Per-item path: item is re-inserted into the in-memory ring via
  `force_insert` so no data is lost from the buffer. Whole-batch path:
  all taken items are restored via `restore_taken()`. The consumer loop
  (`loops.rs`) maps the error to `DeltaResult::failed()`, which the
  receiver maps to exit code 11 (FileIo). Transfer aborts cleanly.
- **Severity:** Transfer failure. No data loss, no corruption.
- **Test coverage:** `hardening::enospc_during_spill_propagates_as_io_error`,
  `enospc_degradation::tier1_spillable_buffer_surfaces_storage_full_during_spill`,
  `enospc_degradation::tier5_real_kernel_enospc_via_full_tmpfs` (Linux
  with `CAP_SYS_ADMIN`).
- **Assessment:** Handled correctly. No fix needed.

### FM-2: Temp-file creation fails (EACCES - permission denied)

- **Trigger:** Spill directory exists but the process lacks write
  permission. Or the default `$TMPDIR` is not writable.
- **Current behavior:** Same propagation as FM-1. `tempfile_in()` returns
  `io::Error(PermissionDenied)`, wrapped in `SpillError::Io`. Item
  preserved in memory, transfer aborts with exit code 11.
- **Severity:** Transfer failure. No data loss.
- **Test coverage:** Not directly tested with EACCES, but the error
  propagation path is identical to FM-1 and is well-exercised.
- **Assessment:** Handled correctly. No fix needed.

### FM-3: Temp-file creation fails (EROFS - read-only filesystem)

- **Trigger:** Spill directory is on a read-only filesystem (e.g.,
  container with read-only tmpfs, snap confinement).
- **Current behavior:** Same propagation as FM-1. `tempfile_in()` returns
  `io::Error(ReadOnlyFilesystem)`, wrapped in `SpillError::Io`. Transfer
  aborts with exit code 11.
- **Severity:** Transfer failure. No data loss.
- **Test coverage:** Not directly tested. Path is identical to FM-1.
- **Assessment:** Handled correctly. The error message will contain the
  OS error text, which is sufficient for diagnosis.

### FM-4: Temp-file write fails mid-spill (ENOSPC)

- **Trigger:** Disk fills up between the initial temp-file open and a
  subsequent `write_all()` inside `write_record()`.
- **Current behavior:** `write_all()` returns `io::Error(StorageFull)`.
  The error propagates through `spill_item()` or
  `spill_candidates_whole_batch()`. **Per-item path:** the item that
  failed to spill is re-inserted via `force_insert()`. However, the
  spill file may contain a partial record (header written, payload
  incomplete). The `spill_write_pos` is **not** advanced on failure
  (the write position only advances on `Ok`), so subsequent writes will
  overwrite the partial record. **Whole-batch path:** all items are
  restored via `restore_taken()`. The payload is encoded into a `Vec`
  before `write_record()`, so a partial header-write leaves partial
  bytes that will be overwritten on retry.
- **Severity:** Transfer failure. No data corruption because
  `spill_write_pos` is not advanced on error, so the partial record is
  logically invisible. No data loss because items are re-inserted.
- **Test coverage:** `enospc_degradation::tier3_*` tests cover
  atomic-fail behavior via `MockEnoSpcWriter`.
  `enospc_degradation::tier2_*` covers sequential spills where the
  second fails.
- **Assessment:** Handled correctly, with one subtlety worth noting.

  **Subtlety:** When `write_record()` calls `file.write_all(header)`
  successfully but `file.write_all(payload)` fails, the header bytes
  are already on disk. The `spill_write_pos` is not advanced, so a
  subsequent successful write would seek back and overwrite from the
  same position. This is correct behavior - the partial data is never
  read because no `spill_index` entry was created. However, if the
  transfer aborts (as it typically would), the partial bytes remain in
  the temp file until it is cleaned up. This is cosmetically imperfect
  but functionally harmless.

### FM-5: Temp-file read-back fails (truncated file, corrupt data)

- **Trigger:** Spill file is truncated by external interference, or the
  on-disk data is corrupted (bit-flip, filesystem error).
- **Current behavior:** `reload_item()` or `reload_batch()` calls
  `file.read_exact()`, which returns `io::Error(UnexpectedEof)` on
  truncation. `T::decode()` returns an I/O error on corrupt data. Both
  propagate as `SpillError::Io` through `next_in_order()`. The consumer
  loop maps it to `DeltaResult::failed()` and exits with exit code 11.
- **Severity:** Transfer failure. The item that was on disk is
  unrecoverable. No silent corruption - the transfer aborts rather than
  delivering garbage.
- **Test coverage:** Not directly tested with corrupt data. The error
  path is exercised indirectly through the `drain_ready` error handling
  in `loops.rs`.
- **Assessment:** Handled correctly. The read path fails loudly on any
  I/O anomaly.

### FM-6: Spill file not initialized on read

- **Trigger:** Code attempts to reload a spilled item but `spill_file`
  is `None`. This could happen if the spill file handle was dropped
  (e.g., during `recreate_spill_dir()`).
- **Current behavior:** Both `reload_item()` and `reload_batch()` check
  `self.spill_file.as_mut()` and return
  `io::Error(NotFound, "spill file not initialized")` when it is `None`.
  This propagates as `SpillError::Io` to the caller.
- **Severity:** Transfer failure. Items on disk are unrecoverable.
- **Test coverage:** Not directly tested, but the guard clause is
  present in both reload paths.
- **Assessment:** Handled correctly. Fails loud.

### FM-7: Spill directory vanishes mid-transfer (no prior spills)

- **Trigger:** Operator or container runtime removes the spill directory
  before any items have been spilled. Next spill attempt gets
  `io::Error(NotFound)`.
- **Current behavior:** Both `spill_item()` and
  `spill_candidates_whole_batch()` detect `ErrorKind::NotFound` when
  `spill_dir.is_some()` and `spill_index.is_empty()`. They call
  `recreate_spill_dir()`, which: drops the stale file handle, runs
  `create_dir_all()`, resets `spill_write_pos` to 0, clears `spill_index`
  and `batch_members`, increments `dir_recreate_count`. A fresh temp file
  is opened on the retry write. Transfer continues.
- **Severity:** None. Transparent recovery, one retry.
- **Test coverage:**
  `hardening::temp_dir_vanish_recreates_when_no_prior_spills`.
- **Assessment:** Handled correctly. Clean recovery with diagnostic
  counter.

### FM-8: Spill directory vanishes mid-transfer (prior spills exist)

- **Trigger:** Operator removes the spill directory after items have
  already been serialized to disk. The next spill attempt gets
  `io::Error(NotFound)`.
- **Current behavior:** The code detects `!self.spill_index.is_empty()`
  and refuses to recreate the directory because the prior spilled items
  are now unrecoverable. **Per-item path:** returns the raw `NotFound`
  error. The caller (`spill_candidates_per_item`) upgrades it to
  `SpillError::PriorSpillsLost { dir, count }`. **Whole-batch path:**
  detects the condition and returns `SpillError::PriorSpillsLost`
  directly. Transfer aborts with exit code 11. The error message
  includes the vanished directory path and the count of unrecoverable
  chunks, enabling operator diagnosis.
- **Severity:** Transfer failure. Previously spilled items are lost.
  No silent corruption - the transfer aborts explicitly.
- **Test coverage:**
  `hardening::temp_dir_vanish_after_prior_spills_returns_error`,
  `hardening::prior_spills_lost_surfaces_typed_variant_on_dir_wipe`.
- **Assessment:** Handled correctly. The typed `PriorSpillsLost` variant
  gives operators an actionable diagnostic.

### FM-9: Spill directory recreation fails

- **Trigger:** After the spill directory vanishes and no prior spills
  exist, `recreate_spill_dir()` calls `fs::create_dir_all()`. This
  fails if the parent directory is also gone, or if the path is blocked
  (parent is a regular file), or if the filesystem is read-only.
- **Current behavior:** `recreate_spill_dir()` returns the `io::Error`
  from `create_dir_all()`. This propagates as `SpillError::Io`. The
  item is re-inserted (per-item path) or restored (whole-batch path).
  Transfer aborts.
- **Severity:** Transfer failure. No data loss from the buffer.
- **Test coverage:** `hardening::dir_recreate_failure_surfaces_io_error`.
- **Assessment:** Handled correctly.

### FM-10: Unsupported compression tag on read

- **Trigger:** A spill file was written with the `spill-compression`
  feature (zstd) but is being read by a build without that feature.
  The on-disk tag byte is `0x01` (SPILL_TAG_ZSTD) but the reader only
  handles `0x00` (SPILL_TAG_RAW).
- **Current behavior:** `decode_payload()` returns
  `SpillError::UnsupportedCompression(0x01)`. The consumer loop
  translates this to `DeltaResult::failed()` with a descriptive message.
  Transfer aborts.
- **Severity:** Transfer failure. No corruption.
- **Test coverage:** `compression` test module and `spill_error_display`
  test.
- **Assessment:** Handled correctly. Build-time feature gates prevent
  this from occurring in normal usage (the compression enum variant is
  only constructable behind the feature flag). This can only happen if a
  spill file is somehow shared across process restarts with different
  builds, which is unlikely given that temp files are anonymous.

### FM-11: Spill record exceeds u32::MAX bytes

- **Trigger:** A single item's encoded payload exceeds 4 GiB. The
  length prefix is a `u32`.
- **Current behavior:** Both `spill_item()` and
  `spill_candidates_whole_batch()` check `payload.len() > u32::MAX as
  usize` and return `io::Error(InvalidData, "spill record exceeds
  u32::MAX bytes")` before any disk write. Item is re-inserted or
  restored.
- **Severity:** Transfer failure. No partial write.
- **Test coverage:** Not directly tested. The guard is trivially
  verifiable by inspection.
- **Assessment:** Handled correctly. The 4 GiB limit is far above any
  realistic per-item payload. `DeltaResult` encodes file-index + stats
  in tens of bytes.

### FM-12: Codec encode failure

- **Trigger:** `item.encode()` returns an error. The `FailingCodec` test
  helper simulates this.
- **Current behavior:** **Per-item path:** `spill_item()` calls
  `item.encode(&mut encoded)` before any disk I/O. On failure, no disk
  bytes are written, and the error propagates. The caller re-inserts
  the item. **Whole-batch path:** `spill_candidates_whole_batch()`
  encodes all items before writing. On failure, `restore_taken()` puts
  every item back. No disk bytes written.
- **Severity:** Transfer failure. No data loss, no partial records.
- **Test coverage:** `enospc_degradation` tests use `FailingCodec` to
  inject encode failures.
- **Assessment:** Handled correctly. The "encode first, write second"
  design prevents partial records.

### FM-13: Codec decode failure on reload

- **Trigger:** `T::decode()` returns an error when reading back a
  spilled item. Could happen from on-disk corruption or a codec bug.
- **Current behavior:** `reload_item()` propagates the `io::Error` from
  `decode()`. `reload_batch()` propagates any error from the per-item
  `T::decode()` loop. Both surface as `SpillError::Io`. Transfer aborts.
- **Severity:** Transfer failure. No silent corruption.
- **Test coverage:** Not directly tested with a corrupt-payload
  scenario. Codec round-trip tests in `mod.rs` verify encode/decode
  parity.
- **Assessment:** Handled correctly. Fails loud.

---

## Cleanup Paths

### Normal completion (RAII)

- **Spooled backend:** `tempfile::SpooledTempFile` is an in-memory buffer
  that rolls to a system temp file. The temp file is anonymous (unlinked
  at creation on Unix) or uses the `FILE_FLAG_DELETE_ON_CLOSE` semantic
  on Windows. When the `SpillableReorderBuffer` is dropped, the
  `SpillBackend` enum drops, the inner `SpooledTempFile` drops, and the
  file descriptor closes. The OS reclaims the disk blocks. No leaked
  files.

- **Directory backend:** `tempfile::tempfile_in()` creates an anonymous
  file (immediately unlinked on Unix). When the buffer drops, the `File`
  handle closes, and the OS reclaims the blocks. No leaked files. The
  spill directory itself is **not** removed on drop - it was either
  created by the user or by `with_spill_dir()`, and removing a
  user-supplied directory would be surprising.

### Abnormal termination

- **Panic:** Rust unwinds and runs destructors. The `SpillBackend` drops
  normally. Anonymous files are reclaimed. **No leaked files on panic.**

- **SIGTERM / SIGINT:** Rust's default signal handling runs destructors
  during the unwind. Anonymous files are reclaimed. Same as panic.

- **SIGKILL / power loss / OOM kill:** The process is terminated without
  running destructors. The file descriptor table is closed by the
  kernel, which reclaims anonymous file blocks. **No leaked files** for
  either backend because `tempfile::tempfile_in()` creates anonymous
  (unlinked) files and `SpooledTempFile` uses the same mechanism when
  it rolls to disk. The only potential leak is the spill **directory**
  itself (created by `with_spill_dir()` or the env var), which remains
  as an empty directory. This is a cosmetic issue only - no data leaks
  and the directory contains no files.

- **Thread panic in consumer loop:** The consumer thread
  (`run_spillable_loop`) owns the `SpillableReorderBuffer`. If the
  thread panics, the buffer is dropped as part of the thread's stack
  unwind. Anonymous files are cleaned up. **No leaked files.**

---

## Summary of Findings

| ID | Failure mode | Current behavior | Severity | Fix needed? |
|----|-------------|-----------------|----------|-------------|
| FM-1 | ENOSPC on temp-file creation | Error propagated, item preserved, transfer aborts with exit 11 | Transfer failure | No |
| FM-2 | EACCES on temp-file creation | Same as FM-1 | Transfer failure | No |
| FM-3 | EROFS on temp-file creation | Same as FM-1 | Transfer failure | No |
| FM-4 | ENOSPC mid-spill write | Partial record overwritable, item re-inserted, transfer aborts | Transfer failure | No |
| FM-5 | Corrupt/truncated read-back | `read_exact` fails, transfer aborts | Transfer failure | No |
| FM-6 | Spill file not initialized on read | Explicit check returns NotFound error | Transfer failure | No |
| FM-7 | Dir vanish, no prior spills | `create_dir_all` recovery, retry succeeds | None - transparent | No |
| FM-8 | Dir vanish, prior spills exist | `PriorSpillsLost` error, transfer aborts | Transfer failure | No |
| FM-9 | Dir recreation fails | Error propagated, item preserved | Transfer failure | No |
| FM-10 | Unknown compression tag | `UnsupportedCompression` error, transfer aborts | Transfer failure | No |
| FM-11 | Record exceeds u32::MAX | Pre-write guard, item preserved | Transfer failure | No |
| FM-12 | Encode failure | Pre-write encode, no partial records | Transfer failure | No |
| FM-13 | Decode failure on reload | Error propagated, transfer aborts | Transfer failure | No |

## Recommendations

The spill layer handles every identified failure mode correctly. No
panics, no silent data loss, no silent corruption across any path. Key
design strengths:

1. **Encode-before-write:** Both per-item and whole-batch paths encode
   payloads into a `Vec` before touching the spill file. Codec failures
   never leave partial records.

2. **Item preservation on failure:** Failed spill attempts re-insert
   items via `force_insert()` (per-item) or `restore_taken()`
   (whole-batch). The in-memory buffer stays consistent.

3. **Write position not advanced on error:** `spill_write_pos` only
   advances on successful `write_record()` returns. Partial disk writes
   are logically invisible and overwritten by any subsequent successful
   write.

4. **Anonymous temp files:** Both backends use unlinked files. SIGKILL
   and OOM-kill leave no orphaned files.

5. **Typed error variants:** `PriorSpillsLost` and
   `UnsupportedCompression` give operators actionable diagnostics
   instead of generic `NotFound` or decode errors.

### Minor observations (no action required)

- **Spill directory not cleaned on drop:** The directory created by
  `with_spill_dir()` survives process exit. This is correct behavior
  (removing user-supplied directories would be surprising), but operators
  may accumulate empty directories over many transfers if they configure
  a per-transfer spill path. Documented in `SpillPolicy::dir`.

- **No retry on transient ENOSPC:** The spill layer treats ENOSPC as
  fatal (item preserved, transfer aborts). It does not wait-and-retry
  in case disk space is freed. This matches upstream rsync's behavior -
  upstream aborts on I/O errors rather than retrying. A retry mechanism
  would add complexity with little benefit for the typical use case.

- **macOS RSS probe stubbed at zero:** The `memory_pressure_bytes` knob
  is effectively disabled on macOS because the RSS probe returns `Ok(0)`.
  Tracked separately under issue #2340.
