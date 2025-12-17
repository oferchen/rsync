# Compatibility Flags Implementation Guide

**Date**: 2025-12-17
**Status**: VARINT_FLIST_FLAGS complete, others documented for future work

---

## Overview

This document tracks the implementation status of rsync protocol compatibility flags
and provides guidance for implementing flag-dependent behaviors. All flags are defined
in `crates/protocol/src/compatibility/flags.rs` and are exchanged during Protocol 30+
capability negotiation.

## Flag Status Summary

| Flag | Bit | Status | Notes |
|------|-----|--------|-------|
| `INC_RECURSE` | 0 | ❌ Not implemented | Incremental recursion mode |
| `SYMLINK_TIMES` | 1 | ❌ Not implemented | Preserve symlink timestamps |
| `SYMLINK_ICONV` | 2 | ❌ Not implemented | Character set conversion for symlinks |
| `SAFE_FILE_LIST` | 3 | ❌ Not implemented | Alternative file list validation |
| `AVOID_XATTR_OPTIMIZATION` | 4 | ❌ Not implemented | Disable xattr shortcuts |
| `CHECKSUM_SEED_FIX` | 5 | ❌ Not implemented | Seed order handling |
| `INPLACE_PARTIAL_DIR` | 6 | ❌ Not implemented | Allow --inplace with --partial-dir |
| `VARINT_FLIST_FLAGS` | 7 | ✅ **Implemented** | Varint encoding for file list flags |
| `ID0_NAMES` | 8 | ❌ Not implemented | Send user/group names for ID 0 |

---

## Implemented Flags

### VARINT_FLIST_FLAGS (bit 7, 0x80)

**Status**: ✅ Complete
**Location**: `crates/protocol/src/flist/{read,write}.rs`
**Implementation Strategy**: Protocol version check

#### Upstream Behavior

Upstream rsync automatically sets `VARINT_FLIST_FLAGS` for all protocol 30+ sessions
during capability negotiation in `compat.c:setup_protocol()`. This makes the flag
equivalent to a protocol version check.

#### Our Implementation

We mirror upstream by checking `protocol >= 30` rather than testing the compat flag:

**Write side** (`crates/protocol/src/flist/write.rs:119`):
```rust
if self.protocol.as_u8() >= 30 {
    write_varint(writer, xflags_to_write as i32)?;
} else {
    writer.write_all(&[xflags_to_write as u8])?;
}
```

**Read side** (`crates/protocol/src/flist/read.rs:67`):
```rust
let flags_value = if self.protocol.as_u8() >= 30 {
    read_varint(reader)?
} else {
    let mut flags_byte = [0u8; 1];
    reader.read_exact(&mut flags_byte)?;
    flags_byte[0] as i32
};
```

#### Rationale

1. **Upstream equivalence**: Protocol 30+ always has this flag set
2. **Performance**: Upstream uses protocol version checks in hot paths (`flist.c`)
3. **Correctness**: The flag is part of protocol definition, not optional behavior

#### Testing

- ✅ File list round-trip tests pass for protocol 30+
- ✅ Both varint and byte encodings tested
- ✅ All 3339 tests passing

---

## Flags Pending Implementation

### INC_RECURSE (bit 0, 0x01) - Incremental Recursion

**Status**: ❌ Not implemented
**Upstream**: `CF_INC_RECURSE`
**Location for implementation**: `crates/walk/`, `crates/core/src/server/`

#### Purpose

Enables incremental directory traversal where file list entries are sent as directories
are discovered, rather than collecting the entire tree before transmission begins.

#### Benefits

- Reduces memory usage for large directory trees
- Allows transfer to begin sooner
- Better progress reporting for deep hierarchies

#### Implementation Requirements

1. **File walker changes**:
   - Stream entries as directories are scanned
   - Maintain partial file list state
   - Handle parent-child relationships incrementally

2. **Generator changes**:
   - Process entries as they arrive
   - Track which directories have been fully scanned
   - Coordinate with receiver for acknowledgments

3. **Protocol changes**:
   - Send directory entry before children
   - Mark directory completion with special entry
   - Handle recursive subdirectory requests

#### Upstream References

- `generator.c:recv_generator()` - Incremental processing
- `flist.c:send_implied_dirs()` - Directory handling
- `flist.c:flist_expand()` - Dynamic list growth

---

### SYMLINK_TIMES (bit 1, 0x02) - Symlink Timestamp Preservation

**Status**: ❌ Not implemented
**Upstream**: `CF_SYMLINK_TIMES`
**Location for implementation**: `crates/metadata/`

#### Purpose

Preserves modification times on symbolic links themselves (not their targets) when
`--times` is specified.

#### Platform Support

- **Linux**: `lutimes()`, `utimensat()` with `AT_SYMLINK_NOFOLLOW`
- **BSD/macOS**: `lutimes()`
- **Windows**: Not applicable (symlinks uncommon)

#### Implementation Requirements

1. **Metadata operations**:
   - Detect symlink vs. regular file
   - Use `lutimes()` or equivalent for symlinks
   - Fall back gracefully on unsupported platforms

2. **File list encoding**:
   - Include mtime for symlink entries
   - Set appropriate flags for symlink type

3. **Testing**:
   - Create symlink with specific mtime
   - Transfer and verify mtime preserved
   - Test on multiple platforms

#### Upstream References

- `syscall.c:do_lutimes()` - Platform-specific symlink time setting
- `t_unsafe.c:do_lchown()` - Similar symlink-specific operation

---

### SAFE_FILE_LIST (bit 3, 0x08) - Alternative File List Validation

**Status**: ❌ Not implemented
**Upstream**: `CF_SAFE_FLIST`
**Location for implementation**: `crates/protocol/src/flist/`

#### Purpose

Requests stricter validation of file list entries to detect malicious or malformed data.

#### Implementation Requirements

1. **Enhanced validation**:
   - Check for path traversal attempts (`..`)
   - Validate entry field ranges
   - Reject suspicious patterns

2. **Error handling**:
   - Fail fast on validation errors
   - Report specific violations
   - Match upstream error messages

#### Upstream References

- `flist.c:check_for_malicious_names()` - Path validation
- `flist.c:flist_expand()` - List integrity checks

---

### CHECKSUM_SEED_FIX (bit 5, 0x20) - Checksum Seed Ordering

**Status**: ❌ Not implemented
**Upstream**: `CF_CHKSUM_SEED_FIX`
**Location for implementation**: `crates/core/src/server/setup.rs`

#### Purpose

Controls the order in which checksum seeds are transmitted during protocol setup.
Older protocols had a bug in seed ordering that this flag corrects.

#### Current Implementation

Checksum seeds are already generated and transmitted (`crates/core/src/server/setup.rs`),
but the order is not yet conditional on this flag.

#### Implementation Requirements

1. **Conditional ordering**:
   ```rust
   if compat_flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX) {
       // New order: seed before file list
   } else {
       // Old order: seed after file list
   }
   ```

2. **Testing**:
   - Verify both orderings work
   - Test interop with old clients
   - Confirm XXHash seed usage

#### Upstream References

- `compat.c:setup_protocol()` - Seed transmission order
- `authenticate.c:get_secret()` - Seed generation

---

### Other Flags

#### SYMLINK_ICONV (bit 2, 0x04)

Character set conversion for symlink targets. Requires iconv integration.

#### AVOID_XATTR_OPTIMIZATION (bit 4, 0x10)

Disables shortcuts in extended attribute handling. Requires xattr implementation first.

#### INPLACE_PARTIAL_DIR (bit 6, 0x40)

Allows `--inplace` with `--partial-dir`. Requires partial transfer handling.

#### ID0_NAMES (bit 8, 0x100)

Sends user/group names for UID/GID 0. Requires ownership preservation.

---

## Implementation Priorities

### HIGH Priority

1. **INC_RECURSE** - Significant memory and performance benefits
2. **CHECKSUM_SEED_FIX** - Already have infrastructure, just needs ordering

### MEDIUM Priority

3. **SYMLINK_TIMES** - Platform-specific but commonly used
4. **SAFE_FILE_LIST** - Security and robustness improvement

### LOW Priority

5. **SYMLINK_ICONV**, **AVOID_XATTR_OPTIMIZATION**, **INPLACE_PARTIAL_DIR**, **ID0_NAMES**
   - Depend on other features being implemented first
   - Less commonly used
   - Platform or configuration specific

---

## Adding New Flag Behavior

When implementing a new compatibility flag:

### 1. Check Flag Availability

In role contexts (ReceiverContext, GeneratorContext):

```rust
if let Some(flags) = self.compat_flags() {
    if flags.contains(CompatibilityFlags::YOUR_FLAG) {
        // Flag-specific behavior
    } else {
        // Fallback behavior
    }
} else {
    // Protocol < 30 or no compat exchange - use default behavior
}
```

### 2. Add Tests

- Unit tests for flag detection
- Integration tests for flag-dependent behavior
- Interop tests with upstream rsync

### 3. Document Behavior

- Update this guide with implementation details
- Add comments explaining flag usage
- Document any deviations from upstream

### 4. Update PROTOCOL30_STATUS.md

- Mark flag as implemented
- Add to changelog
- Update test counts

---

## References

- **Upstream source**: `rsync/compat.c` - Flag definitions and negotiation
- **Upstream source**: `rsync/flist.c` - File list flag usage
- **Our implementation**: `crates/protocol/src/compatibility/flags.rs`
- **Our implementation**: `crates/core/src/server/{receiver,generator}.rs`

---

## Changelog

- **2025-12-17**: Created guide documenting VARINT_FLIST_FLAGS implementation
- **2025-12-17**: Added detailed descriptions for all 9 flags
- **2025-12-17**: Documented implementation priorities and patterns
