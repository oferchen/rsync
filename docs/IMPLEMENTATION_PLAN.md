# Implementation Plan for Remaining Components

**Date**: 2025-12-17
**Status**: Planning Phase
**Reference**: MISSING_COMPONENTS.md

---

## Overview

This document provides a prioritized implementation plan for remaining missing components, organized by priority level with specific implementation steps for each component.

---

## HIGH Priority - Compression Configuration

### Task 1: Add --compress-level CLI Flag

**Estimated Effort**: 4-6 hours
**Dependencies**: None (compression infrastructure complete)
**Files to Modify**:
- `crates/cli/src/frontend/arguments/parser.rs` - Add flag parsing
- `crates/core/src/server/mod.rs` - Use configured level instead of Default
- `crates/daemon/src/config.rs` - Add daemon configuration option

**Implementation Steps**:
1. Add `--compress-level=NUM` flag to CLI parser
   - Accept values 0-9 for zlib (0=none, 1=fast, 9=best)
   - Map to `compress::zlib::CompressionLevel` enum
   - Default to level 6 (matches upstream)

2. Thread compression level through HandshakeResult
   - Add `compression_level: Option<CompressionLevel>` field
   - Pass from configuration to setup

3. Use configured level in activation
   - Replace hardcoded `CompressionLevel::Default`
   - Apply in `run_server_with_handshake()`

4. Add daemon configuration support
   - `compress-level = 6` in rsyncd.conf
   - Per-module override capability

**Testing**:
- Test each compression level (0-9)
- Verify compression ratio changes
- Test daemon configuration

**Acceptance Criteria**:
- CLI flag accepted and validated
- Compression level propagates to encoders
- Daemon configuration works
- Upstream parity for default behavior

---

### Task 2: Implement Skip-Compress Patterns

**Estimated Effort**: 6-8 hours
**Dependencies**: Task 1 (compression level)
**Files to Modify**:
- `crates/engine/src/local_copy/skip_compress.rs` - Pattern matching
- `crates/core/src/server/generator.rs` - File-level compression decision
- `crates/cli/src/frontend/arguments/parser.rs` - Add --skip-compress flag

**Implementation Steps**:
1. Extend skip_compress module
   - Add pattern matching for file extensions
   - Default patterns: `.gz`, `.zip`, `.bz2`, `.xz`, `.7z`, `.rar`, etc.
   - Support custom patterns via --skip-compress

2. Add per-file compression control
   - Check file extension before activating compression
   - Disable compression for matched files
   - Re-enable for next file

3. Add CLI flag
   - `--skip-compress=LIST` (comma-separated patterns)
   - Parse and store in configuration

4. Integration
   - Generator checks pattern before sending each file
   - Communicate compression state to receiver

**Testing**:
- Test default skip patterns
- Test custom patterns
- Verify compression toggling works
- Test with mixed file types

**Acceptance Criteria**:
- Default patterns match upstream
- Custom patterns work
- Compression correctly toggled per-file
- No performance regression

---

### Task 3: Add Compression Integration Tests

**Estimated Effort**: 4-6 hours
**Dependencies**: Tasks 1-2
**Files to Create**:
- `crates/core/tests/compression_streams.rs` - Integration tests

**Test Scenarios**:
1. Full session with zlib compression
   - Negotiate compression
   - Transfer multiple files
   - Verify data integrity
   - Measure compression ratio

2. Full session with no compression
   - Negotiate None algorithm
   - Verify plain multiplex used

3. Compression level variation
   - Test levels 1, 6, 9
   - Verify different ratios

4. Skip-compress patterns
   - Transfer mix of compressible/incompressible files
   - Verify selective compression

5. Algorithm negotiation
   - Test zlib, LZ4, zstd (if features enabled)
   - Verify correct algorithm activated

**Acceptance Criteria**:
- All test scenarios pass
- Tests run in CI
- Coverage for all compression paths

---

## MEDIUM Priority - Compat Flags Behavioral Implementation

### Task 4: Implement CHECKSUM_SEED_FIX Flag

**Estimated Effort**: 2-4 hours
**Priority**: HIGH (infrastructure already exists)
**Files to Modify**:
- `crates/core/src/server/setup.rs` - Conditional seed ordering

**Implementation Steps**:
1. Check flag in `setup_protocol()`
   ```rust
   if compat_flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX) {
       // New order: seed before file list
   } else {
       // Old order: seed after file list
   }
   ```

2. Adjust seed transmission timing
   - Current implementation sends seed
   - Make timing conditional on flag

3. Add tests
   - Test with flag set
   - Test with flag not set
   - Verify interop with old clients

**Acceptance Criteria**:
- Seed order matches upstream for each case
- Tests verify both orderings
- Interop with old/new clients works

---

### Task 5: Implement SAFE_FILE_LIST Flag

**Estimated Effort**: 4-6 hours
**Priority**: MEDIUM (security improvement)
**Files to Modify**:
- `crates/protocol/src/flist/read.rs` - Enhanced validation
- `crates/walk/src/validation.rs` - Path validation

**Implementation Steps**:
1. Add enhanced validation mode
   ```rust
   if let Some(flags) = compat_flags {
       if flags.contains(CompatibilityFlags::SAFE_FILE_LIST) {
           validate_safe_file_list(entry)?;
       }
   }
   ```

2. Implement validation checks
   - Reject path traversal attempts (`..`)
   - Validate field ranges
   - Check for malicious patterns

3. Add error messages
   - Match upstream error format
   - Include file path in error

4. Add tests
   - Test malicious paths
   - Test valid paths
   - Verify flag behavior

**Acceptance Criteria**:
- Path traversal rejected when flag set
- Valid paths accepted
- Error messages match upstream
- Tests cover attack scenarios

---

### Task 6: Implement SYMLINK_TIMES Flag

**Estimated Effort**: 6-8 hours
**Priority**: MEDIUM (platform-specific)
**Files to Modify**:
- `crates/metadata/src/symlink.rs` - New module
- `crates/metadata/src/apply.rs` - Conditional behavior
- `crates/walk/src/entry.rs` - Include symlink mtime

**Implementation Steps**:
1. Detect symlink vs regular file
   ```rust
   if entry.is_symlink() && preserve_times {
       if let Some(flags) = compat_flags {
           if flags.contains(CompatibilityFlags::SYMLINK_TIMES) {
               set_symlink_times(path, mtime)?;
           }
       }
   }
   ```

2. Platform-specific implementation
   - Linux: `utimensat()` with `AT_SYMLINK_NOFOLLOW`
   - BSD/macOS: `lutimes()`
   - Windows: Skip (not applicable)

3. Include symlink mtime in file list
   - Read mtime for symlinks
   - Encode in file list flags

4. Add tests
   - Create symlink with specific mtime
   - Transfer and verify mtime preserved
   - Test on multiple platforms

**Acceptance Criteria**:
- Symlink times preserved when flag set
- Platform-specific code works
- Falls back gracefully on unsupported platforms
- Tests pass on Linux/macOS

---

### Task 7: Implement INC_RECURSE Flag

**Estimated Effort**: 16-24 hours
**Priority**: MEDIUM (significant complexity)
**Files to Modify**:
- `crates/walk/src/` - Streaming traversal
- `crates/core/src/server/generator.rs` - Incremental sending
- `crates/core/src/server/receiver.rs` - Incremental receiving

**Implementation Steps**:
1. Design streaming file walker
   - Yield entries as directories discovered
   - Maintain partial state
   - Handle parent-child relationships

2. Modify generator
   ```rust
   if let Some(flags) = compat_flags {
       if flags.contains(CompatibilityFlags::INC_RECURSE) {
           send_incremental_file_list()?;
       } else {
           send_complete_file_list()?;
       }
   }
   ```

3. Protocol changes
   - Send directory entry before children
   - Mark directory completion
   - Handle recursive subdirectory requests

4. Receiver coordination
   - Process entries as they arrive
   - Track completion state
   - Send acknowledgments

5. Add tests
   - Test deep directory trees
   - Test early file processing
   - Test memory usage reduction

**Acceptance Criteria**:
- Incremental recursion works
- Memory usage reduced for large trees
- Transfer begins sooner
- Tests verify correctness

**Note**: This is the most complex compat flag implementation. Consider deferring until other flags are complete.

---

### Tasks 8-11: Low Priority Compat Flags

**SYMLINK_ICONV** (Task 8):
- Requires iconv integration
- Character set conversion for symlink targets
- Platform and locale dependent

**AVOID_XATTR_OPTIMIZATION** (Task 9):
- Requires xattr implementation first
- Disables shortcuts in xattr handling

**INPLACE_PARTIAL_DIR** (Task 10):
- Requires partial transfer handling
- Allows --inplace with --partial-dir

**ID0_NAMES** (Task 11):
- Requires ownership preservation
- Sends user/group names for UID/GID 0

**Implementation**: Defer until higher priority items complete.

---

## LOW Priority - Batch Mode Completion

### Task 12: Complete Batch Mode

**Estimated Effort**: 8-12 hours
**Priority**: LOW (rarely used feature)
**Files to Modify**:
- `crates/core/src/client/run.rs` - Batch application
- `crates/engine/src/batch/` - Batch processing

**Implementation Steps**:
1. Handle directories
   - Create directories from batch
   - Set directory metadata

2. Handle symlinks
   - Create symlinks from batch
   - Set symlink metadata (if SYMLINK_TIMES)

3. Handle devices
   - Create device nodes (requires root)
   - Set device metadata

4. Full metadata preservation
   - Apply all preservation flags
   - Match batch header settings

5. Add end-to-end tests
   - Create batch
   - Apply batch
   - Verify result matches source

**Acceptance Criteria**:
- All file types handled
- Metadata fully preserved
- Tests verify completeness
- Upstream parity

---

## LOW Priority - Automation

### Task 13: Capture-Handshakes XTask Command

**Estimated Effort**: 6-8 hours
**Priority**: LOW (manual process works)
**Files to Create**:
- `xtask/src/commands/capture_handshakes/` - New module

**Implementation Steps**:
1. Create module structure
   - `mod.rs` - Command entry point
   - `pcap.rs` - Pcap parsing
   - `extract.rs` - Handshake extraction
   - `save.rs` - Golden file writing

2. Integrate with xtask
   - Add to `InteropCommand` enum
   - Add CLI argument parsing

3. Implement pcap parsing
   - Use tshark or rust pcap library
   - Extract TCP streams
   - Identify handshake sequences

4. Extract handshakes
   - Find rsync protocol exchanges
   - Extract binary/text sequences
   - Validate format

5. Save golden files
   - Write to appropriate directories
   - Name files by protocol version
   - Update README

**Acceptance Criteria**:
- `cargo xtask capture-handshakes all` works
- Golden files generated correctly
- Tests pass with new fixtures
- Documentation updated

---

## Implementation Order Recommendation

### Phase 1: Compression Configuration (1-2 weeks)
1. Task 1: --compress-level flag
2. Task 2: Skip-compress patterns
3. Task 3: Compression integration tests

**Rationale**: Completes compression feature, highest user value.

### Phase 2: High-Value Compat Flags (1 week)
4. Task 4: CHECKSUM_SEED_FIX
5. Task 5: SAFE_FILE_LIST
6. Task 6: SYMLINK_TIMES

**Rationale**: Quick wins, improves protocol correctness and security.

### Phase 3: Complex Compat Flag (2-3 weeks)
7. Task 7: INC_RECURSE

**Rationale**: Most complex, significant memory/performance benefit.

### Phase 4: Low Priority Items (as needed)
8-13. Remaining tasks based on user demand

**Rationale**: Nice-to-have features, lower impact.

---

## Success Metrics

- All tests passing (currently 3346/3346)
- Clippy clean with -D warnings
- Upstream interoperability maintained
- Documentation updated
- No performance regressions

---

## References

- MISSING_COMPONENTS.md - Component status tracking
- COMPAT_FLAGS_GUIDE.md - Compatibility flags documentation
- COMPRESSION_ARCHITECTURE.md - Compression design
- Upstream rsync 3.4.1 source code

---

**Document Version**: 1.0
**Last Updated**: 2025-12-17
**Author**: Implementation Planning
**Status**: Ready for Execution
