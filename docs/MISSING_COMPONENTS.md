# Missing Components Analysis

**Date**: 2025-12-17
**Status**: Survey Complete

---

## Overview

This document catalogs incomplete functionality in the oc-rsync codebase, prioritized by impact on core functionality.

## HIGH Priority - Testing Infrastructure

### 1. Golden Handshake Test Fixtures
**Status**: Infrastructure ready, fixtures need generation
**Impact**: Cannot validate wire-level protocol compatibility with upstream rsync

**Files Needed**:
```
tests/protocol_handshakes/
├── protocol_28_legacy/
│   ├── client_greeting.txt
│   └── server_response.txt
├── protocol_29_legacy/
│   ├── client_greeting.txt
│   └── server_response.txt
├── protocol_30_binary/
│   ├── client_hello.bin
│   └── server_response.bin
├── protocol_31_binary/
│   ├── client_hello.bin
│   └── server_response.bin
└── protocol_32_binary/
    ├── client_hello.bin
    ├── server_response.bin
    └── compatibility_exchange.bin
```

**Solution**: Manual capture script created at `tools/capture-handshakes.sh`
- Captures pcap files from upstream rsync daemons
- Manual extraction of handshake bytes required (tshark/wireshark)
- Automated extraction via xtask command not yet implemented

**Next Steps**:
1. Run `tools/capture-handshakes.sh all` to capture pcap files
2. Extract handshake sequences using tshark or Wireshark
3. Save as golden files in appropriate directories
4. Verify tests pass with `cargo test -p protocol --test golden_handshakes`

---

## MEDIUM Priority - Protocol Implementation

### 2. Negotiated Algorithms Wiring
**Status**: Negotiation works, results not used
**Location**: `crates/core/src/server/mod.rs:184`
**Impact**: Defaults used regardless of negotiation outcome

**Current Behavior**:
```rust
let negotiated = setup::setup_protocol(...)?;

// TODO: Wire negotiated algorithms through to transfer engine
if let Some(result) = negotiated {
    // Algorithms negotiated: result.checksum and result.compression
    // These should be stored in context and used by the transfer engine
    let _ = result; // Suppress unused warning for now
}
```

**Required Architecture**:
1. Add `negotiated_algorithms: Option<NegotiationResult>` to `ServerConfig` or create new `ServerContext`
2. Pass negotiated algorithms to role contexts:
   - `GeneratorContext`
   - `ReceiverContext`
   - `SenderContext`
3. Use negotiated checksum algorithm when creating `StrongDigest` instances
4. Use negotiated compression algorithm when creating compressors
5. Update checksum selection logic in:
   - `crates/checksums/src/strong/mod.rs`
   - Delta generation/application code
6. Update compression selection logic in:
   - `crates/compress/src/`
   - Transfer pipeline

**Design Considerations**:
- Backward compatibility: Default to MD4/zlib for protocols < 30
- Thread safety: Multiple roles may access algorithms concurrently
- Testing: Unit tests for algorithm selection at each protocol version

### 3. Compat Flags Storage
**Status**: Exchanged but not stored for runtime use
**Location**: `crates/core/src/server/setup.rs:253`
**Impact**: Protocol-specific behavior can't check negotiated flags

**Current**:
```rust
// TODO: Store our_flags for use by role handlers
// Upstream stores these in global variables, but we'll need to pass them through
// the HandshakeResult or ServerConfig
```

**Solution**:
- Add `compat_flags: CompatibilityFlags` to `HandshakeResult` or `ServerConfig`
- Pass flags to role contexts for runtime checks
- Examples where flags affect behavior:
  - `INC_RECURSE`: Enables incremental recursion
  - `SAFE_FILE_LIST`: Changes file list validation
  - `AVOID_XATTR_OPTIMIZATION`: Disables xattr shortcuts

---

## LOW Priority - Specialized Features

### 4. Batch Mode Completion
**Status**: Read works, application incomplete
**Location**: `crates/core/src/client/run.rs:690-705`
**Impact**: Limited - batch mode is an advanced feature

**Current Implementation**:
- ✅ Parse batch file format
- ✅ Read delta operations (COPY/LITERAL)
- ✅ Validate batch structure
- ❌ Apply operations to all file types (only basic files)
- ❌ Set complete metadata (partial implementation)
- ❌ Handle directories, symlinks, devices

**Remaining Work**:
```rust
// TODO: Full implementation would:
// 1. For each file entry:
//    a. Read delta operations (COPY/LITERAL) from batch
//    b. Apply operations to destination directory
//    c. Set file metadata (mode, mtime, uid, gid)
// 2. Handle directories, symlinks, devices
// 3. Apply preservation flags from batch header
```

**Priority**: LOW - Batch mode is rarely used in production

### 5. Capture-Handshakes XTask Command
**Status**: Script exists, xtask integration pending
**Location**: `xtask/src/commands/`
**Impact**: Manual process works, automation would improve workflow

**Manual Process**: `tools/capture-handshakes.sh` (working)
**Automated Process**: `cargo xtask capture-handshakes` (not implemented)

**Implementation Path**:
1. Create `xtask/src/commands/capture_handshakes/` module
2. Add as variant to `InteropCommand` enum
3. Implement automated pcap parsing with tshark
4. Extract handshake sequences programmatically
5. Save as golden files with proper naming

**Benefit**: Streamlines golden file regeneration after protocol changes

---

## Platform-Specific Limitations

### 6. ACL Support (macOS/BSD)
**Status**: Intentional stub - platform API limitation
**Location**: `crates/metadata/src/acl_stub.rs`
**Impact**: ACLs not preserved on Apple platforms

**Reason**: Apple's libSystem lacks `acl_from_mode` helper present in Linux glibc

**Behavior**: Operations succeed but ACLs are not copied (matches rsync without ACL support)

**Future**: Would require platform-specific ACL API implementation for each BSD variant

---

## Testing Coverage

### Missing Test Categories
1. **Protocol Interoperability**: Golden handshake fixtures (HIGH)
2. **Algorithm Selection**: Verify negotiated algorithms are used (MEDIUM)
3. **Compat Flags**: Runtime behavior based on negotiated flags (MEDIUM)
4. **Batch Mode**: End-to-end batch application (LOW)

### Current Test Status
- ✅ Unit tests: Good coverage in most crates
- ✅ Integration tests: Basic transfer scenarios work
- ✅ Property tests: Checksums, filters have property tests
- ❌ Golden tests: Protocol handshakes need fixtures
- ⚠️ Interop tests: Exit codes and messages validated, handshakes pending

---

## Implementation Roadmap

### Phase 1: Testing Infrastructure (1-2 days)
1. Capture golden handshake files for protocols 28-32
2. Verify golden tests pass
3. Document handshake capture process

### Phase 2: Algorithm Wiring (2-3 days)
1. Design context structure for negotiated algorithms
2. Pass negotiation results through role contexts
3. Update checksum selection in delta operations
4. Update compression selection in transfer pipeline
5. Add tests for algorithm selection

### Phase 3: Runtime Flags (1-2 days)
1. Store compat flags in server context
2. Pass flags to role contexts
3. Implement flag-dependent behavior
4. Test with various flag combinations

### Phase 4: Automation (1 day)
1. Implement capture-handshakes xtask command
2. Automate pcap parsing with tshark
3. Integrate into CI pipeline

### Phase 5: Batch Mode (optional, 2-3 days)
1. Complete directory/symlink/device handling
2. Full metadata preservation
3. End-to-end batch tests

---

## References

- Golden handshake README: `tests/protocol_handshakes/README.md`
- Protocol negotiation: `crates/protocol/src/negotiation/`
- Checksum implementations: `crates/checksums/src/strong/`
- Compression implementations: `crates/compress/src/`
- Server setup: `crates/core/src/server/setup.rs`
