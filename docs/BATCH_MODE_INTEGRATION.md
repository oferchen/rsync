# Batch Mode Integration - Implementation Roadmap

## Current Status: Phase 2b File List Capture Complete ✅

**Completed Work:**
- Phase 1: Configuration plumbing (Commit: e2429bde)
- Phase 2a: BatchWriter infrastructure (Commit: 476b812d)
- Phase 2b Part 1: File list capture (Current session)

**What Works:**
- Batch file is created successfully
- Batch header written with stream flags before transfer
- File list entries captured during directory walk
- File metadata recorded (path, mode, size, mtime, uid, gid)
- All file types supported (files, directories, symlinks, devices, FIFOs)
- Batch file now contains header + file entries (tested: 85 bytes for 2 files)
- Transfer completes normally with batch mode enabled
- All 2734 tests passing

**Current Limitation:**
- Delta operations (COPY/LITERAL) not yet captured during file transfers
- Batch finalization and .sh script generation not yet implemented
- Batch reader/replay not yet implemented

---

## Phase 2b: Data Capture Hooks (Pending)

### Architecture Overview

The engine execution flow follows these stages:

```
LocalCopyPlan::execute_with_options()
  → copy_sources() [executor/sources.rs]
    → For each source:
      → copy_directory_recursive() [for directories]
        → Walk filesystem
        → Apply filters
        → Process each file/dir/link
      → copy_file() [for regular files]
        → Read source
        → Apply deltas (if incremental)
        → Write destination
      → copy_symlink() [for symlinks]
      → copy_device() [for devices]
      → copy_fifo() [for FIFOs]
```

### Integration Points

#### 1. File List Capture (6-8 hours)

**Location:** `crates/engine/src/local_copy/executor/directory.rs`
**Function:** `copy_directory_recursive()` and walk helpers

**What to Hook:**
- Each file entry discovered during walk phase
- File metadata (size, mtime, permissions, etc.)
- File type (regular, directory, symlink, device, FIFO)
- Relative path information

**Implementation Approach:**
```rust
// In copy_directory_recursive() or similar walk function:
if let Some(batch_writer) = context.options().batch_writer() {
    let mut writer = batch_writer.lock().unwrap();
    writer.write_file_entry(
        relative_path,
        &metadata,
        file_type,
    )?;
}
```

**Data Format:**
Must match upstream rsync's file list format:
- Path (variable length string)
- Mode (u32)
- Size (u64)
- Mtime (timestamp)
- Flags bitmap
- Additional metadata based on options (uid, gid, xattrs, ACLs)

**Files to Modify:**
- `crates/engine/src/local_copy/executor/directory.rs`
- `crates/engine/src/local_copy/context.rs` (add accessor for batch_writer)
- `crates/engine/src/batch/writer.rs` (add `write_file_entry()` method)
- `crates/engine/src/batch/format.rs` (define file list entry format)

**Testing:**
- Verify file list written in correct format
- Test with various file types
- Test with filters and exclusions
- Compare format with upstream batch files

---

#### 2. Delta Operation Capture (6-8 hours)

**Location:** `crates/engine/src/local_copy/executor/file/copy/mod.rs`
**Function:** Delta generation and application during file transfers

**What to Hook:**
- COPY operations (copy from basis file at offset)
- LITERAL operations (write new data)
- Block checksums
- Delta script sequence

**Implementation Approach:**
```rust
// During delta application:
if let Some(batch_writer) = context.options().batch_writer() {
    let mut writer = batch_writer.lock().unwrap();

    match operation {
        DeltaOp::Copy { offset, length } => {
            writer.write_copy_operation(offset, length)?;
        }
        DeltaOp::Literal { data } => {
            writer.write_literal_data(data)?;
        }
    }
}
```

**Data Format:**
Must match upstream rsync's delta format:
- Operation type (COPY vs LITERAL)
- For COPY: source offset (u64), length (u32)
- For LITERAL: data bytes (variable length)
- Checksums for verification

**Files to Modify:**
- `crates/engine/src/local_copy/executor/file/copy/mod.rs`
- `crates/engine/src/delta/mod.rs` (delta script generation)
- `crates/engine/src/batch/writer.rs` (add delta operation methods)
- `crates/engine/src/batch/format.rs` (define delta operation format)

**Complexity:**
- Must capture operations in exact order
- Handle whole-file copies (no delta)
- Handle sparse files correctly
- Thread-safety for Arc<Mutex<BatchWriter>>

**Testing:**
- Verify delta operations captured correctly
- Test with various file sizes
- Test with --whole-file mode
- Test with sparse files
- Compare format with upstream batch files

---

#### 3. Batch Finalization (2-3 hours)

**Location:** `crates/core/src/client/run.rs`
**Function:** After successful transfer completion

**Implementation:**
```rust
// After plan.execute_with_options() succeeds:
if let Some(batch_writer) = batch_writer_arc {
    let mut writer = batch_writer.lock().unwrap();

    // Write final statistics
    writer.write_statistics(&summary)?;

    // Finalize and flush batch file
    writer.finalize()?;

    // Generate .sh replay script
    let script_path = engine::batch::script::generate_script(
        &config.batch_config().unwrap(),
        &parsed_args, // Need to preserve original args
    )?;

    // Set executable permissions on .sh file
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }
}
```

**Script Generation:**
The `.sh` file should contain:
```bash
#!/bin/sh
oc-rsync --read-batch=mybatch /destination/path/
```

**Handle Only-Write-Batch Mode:**
```rust
if config.batch_config().unwrap().mode == BatchMode::OnlyWrite {
    // Skip actual transfer, only write batch file
    // Return early after batch file is complete
}
```

**Files to Modify:**
- `crates/core/src/client/run.rs`
- `crates/engine/src/batch/script.rs` (implement script generation)
- `crates/engine/src/batch/writer.rs` (add `write_statistics()` method)

**Testing:**
- Verify batch file finalized correctly
- Verify .sh script created with correct content
- Verify .sh script is executable
- Test only-write-batch mode

---

#### 4. Batch Reader Implementation (4-6 hours)

**Location:** `crates/core/src/client/run.rs`
**Function:** Replay batch file instead of normal transfer

**Implementation:**
```rust
if batch_config.is_read_mode() {
    let reader = BatchReader::new(batch_config.clone())?;

    // Read header and validate
    let header = reader.read_header()?;

    // Read file list
    let file_list = reader.read_file_list()?;

    // Apply delta operations for each file
    for file_entry in file_list {
        // Read delta operations for this file
        let operations = reader.read_delta_operations()?;

        // Apply operations to destination
        apply_batch_operations(
            &file_entry,
            &operations,
            plan.destination(),
        )?;
    }

    return Ok(ClientSummary::from_batch_replay(reader.statistics()));
}
```

**Files to Modify:**
- `crates/core/src/client/run.rs` (replay flow)
- `crates/engine/src/batch/reader.rs` (complete implementation)
- `crates/engine/src/batch/replay.rs` (new module for applying operations)

**Testing:**
- Write/read round-trip tests
- Verify operations applied correctly
- Test error handling (corrupt batch, missing data)
- Interop with upstream batch files

---

## Testing Strategy

### Unit Tests
- Batch file format serialization/deserialization
- File list entry encoding/decoding
- Delta operation encoding/decoding
- Script generation logic

### Integration Tests
```rust
#[test]
fn test_batch_write_read_roundtrip() {
    // Create source files
    // Write batch
    // Read batch and apply
    // Verify destination matches
}

#[test]
fn test_batch_interop_with_upstream() {
    // Create batch with oc-rsync
    // Apply with upstream rsync
    // Verify result

    // Create batch with upstream rsync
    // Apply with oc-rsync
    // Verify result
}
```

### Test Files to Create
- `tests/batch_mode.rs` - Main integration tests
- `crates/engine/src/batch/tests.rs` - Already exists, extend with format tests

---

## Estimated Effort Breakdown

| Task | Hours | Complexity |
|------|-------|------------|
| File list capture | 6-8 | High - must match upstream format exactly |
| Delta operation capture | 6-8 | High - complex threading, exact ordering |
| Batch finalization | 2-3 | Medium - script generation, permissions |
| Batch reader/replay | 4-6 | High - inverse of writer operations |
| Testing & debugging | 3-4 | Medium - interop verification |
| **Total** | **21-29** | **Very High** |

---

## Key Challenges

### 1. Format Compatibility
Must match upstream rsync's binary format exactly for interoperability.
**Mitigation:** Study upstream source code, test against multiple rsync versions.

### 2. Thread Safety
BatchWriter wrapped in Arc<Mutex<>> accessed from multiple execution paths.
**Mitigation:** Minimize lock hold times, handle poisoned locks gracefully.

### 3. Error Handling
Batch file corruption must not crash; graceful degradation required.
**Mitigation:** Comprehensive validation, clear error messages.

### 4. Performance
Writing to batch file must not significantly slow transfers.
**Mitigation:** Buffered writes, batch write operations when possible.

---

## Success Criteria

- ✅ `--write-batch` creates valid batch file
- ✅ `--only-write-batch` creates batch without transferring
- ✅ `--read-batch` replays batch file correctly
- ✅ Round-trip test: write → read → verify
- ✅ Interop test: oc-rsync batch ↔ upstream rsync
- ✅ Batch files work across protocol versions 28-32
- ✅ All existing tests still pass
- ✅ Performance impact < 5% for batch writes

---

## Next Steps for Implementation

1. **Start with File List Capture** (simplest, no delta complexity)
   - Add `write_file_entry()` to BatchWriter
   - Hook into directory walking
   - Test with simple file trees
   - Verify format matches upstream

2. **Add Delta Operation Capture** (most complex)
   - Study delta generation code carefully
   - Add operation capture hooks
   - Test with various file scenarios
   - Verify format matches upstream

3. **Implement Finalization** (straightforward)
   - Add statistics writing
   - Generate .sh script
   - Test script execution

4. **Implement Batch Reader** (inverse of writer)
   - Read file list
   - Replay operations
   - Test round-trip
   - Test interop

5. **Comprehensive Testing** (validation)
   - Round-trip tests
   - Interop tests
   - Error handling tests
   - Performance tests

---

## References

- Upstream rsync batch.c: https://github.com/RsyncProject/rsync/blob/master/batch.c
- Upstream rsync generator.c: File list generation
- Upstream rsync sender.c: Delta operation generation
- `crates/engine/src/batch/`: Current implementation (format, reader, writer)
- `crates/engine/src/local_copy/`: Execution engine

---

## Status: Ready for Phase 2b Implementation

All infrastructure is in place. The remaining work is time-consuming but
straightforward: find the right hook points, add batch writer calls, test
thoroughly. Estimated 21-29 hours of focused implementation work.
