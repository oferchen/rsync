# Missing Components Analysis

**Date**: 2025-12-18
**Status**: Phase 1 & Phase 4 Complete - Golden Handshake Fixtures & Compatibility Flags

---

## Overview

This document catalogs incomplete functionality in the oc-rsync codebase, prioritized by impact on core functionality.

## HIGH Priority - Testing Infrastructure

### 1. ✅ Golden Handshake Test Fixtures (COMPLETED)
**Status**: COMPLETE - All golden files captured and validated
**Commit**: 955761f5
**Impact**: Wire-level protocol compatibility with upstream rsync fully validated

**Files Created**:
```
tests/protocol_handshakes/
├── protocol_28_legacy/
│   ├── client_greeting.txt  ✅
│   └── server_response.txt  ✅
├── protocol_29_legacy/
│   ├── client_greeting.txt  ✅
│   └── server_response.txt  ✅
├── protocol_30_binary/
│   ├── client_hello.bin     ✅
│   └── server_response.bin  ✅
├── protocol_31_binary/
│   ├── client_hello.bin     ✅
│   └── server_response.bin  ✅
└── protocol_32_binary/
    ├── client_hello.bin             ✅
    ├── server_response.bin          ✅
    └── compatibility_exchange.bin   ✅
```

**Capture Tools Created**:
- `tools/strace-capture.sh` - Automated handshake capture using strace (no root required)
- `crates/protocol/examples/generate_golden_handshakes.rs` - Programmatic baseline generator
- `tools/simple-capture.sh` - Alternative tcpdump approach (requires sudo)

**Validation Results**:
- All 12 golden handshake tests passing
- Byte-level compatibility verified with upstream rsync 3.4.1
- Legacy protocols (28-29): ASCII `@RSYNCD:` format with checksum algorithm lists
- Binary protocols (30-32): varint-encoded version negotiation
- Test suite: 3382 tests passed

---

## MEDIUM Priority - Protocol Implementation

### 2. ✅ Negotiated Checksum Algorithms (COMPLETED)
**Status**: COMPLETE - Checksum algorithms fully wired and tested
**Commits**: f9d22b2c, db41d62e, 350c88dd, c57ae371
**Impact**: Protocol 30+ checksum negotiation working correctly

**Completed Work**:
1. ✅ Added `negotiated_algorithms: Option<NegotiationResult>` to `HandshakeResult`
2. ✅ Pass negotiated algorithms to role contexts:
   - `GeneratorContext` - stores and uses negotiated checksum
   - `ReceiverContext` - stores and uses negotiated checksum
3. ✅ Checksum selection with proper fallback chain:
   - Negotiated algorithm (Protocol 30+ with 'v' capability)
   - MD5 default (Protocol 30+ without negotiation)
   - MD4 default (Protocol < 30)
4. ✅ Checksum seed generation and transmission (all protocols)
5. ✅ XXHash variants support with seed propagation
6. ✅ Integration tests: 14 tests validating algorithm usage

**Remaining Work**:
- ❌ Compression algorithm application (negotiated but not yet applied to streams)
  - Requires creating compression wrapper layers in ServerWriter/ServerReader
  - Architectural work to add Plain → Multiplex → Compress stream stack

### 3. ✅ Compression Stream Implementation (COMPLETED)
**Status**: COMPLETE - Compression streams fully integrated
**Commits**: 78b69abe, e6f7dae6
**Impact**: Protocol 30+ compression working end-to-end

**Completed Work**:
1. ✅ Created `CompressedWriter` and `CompressedReader` wrappers (Commit: 78b69abe)
   - EncoderVariant/DecoderVariant for zlib, LZ4, zstd
   - Proper stream lifecycle (init, write, flush, finish)
   - Control message bypass for protocol compatibility
   - 7 comprehensive tests for compression streams
2. ✅ Extended `ServerWriter` and `ServerReader` enums (Commit: 78b69abe)
   - Added Compressed variants
   - activate_compression() methods
   - Updated Write/Read trait implementations
3. ✅ Integrated into server stream stack (Commit: e6f7dae6)
   - Writer compression in `run_server_with_handshake()`
   - Reader compression in ReceiverContext and GeneratorContext
   - Activated AFTER multiplex (matches upstream)
   - Protocol conversion method for algorithm enums
4. ✅ Full stream stack implemented:
   ```
   Plain → Multiplex → Compress (for protocol 30+)
   ```

**Remaining Work**: None - Phase 3 COMPLETE
- ✅ Configuration infrastructure for compression level (Commits: 36954c8b, 2214a80c)
  - ServerConfig has compression_level: Option<CompressionLevel>
  - run_server_with_handshake uses configured level or defaults to 6
  - Unit tests verify configuration plumbing (3 tests)
- ⏸️ Daemon configuration parsing (--compress-level in rsyncd.conf) - deferred
- ✅ Skip-compress patterns support (Commit: 3e398602)
  - Already implemented in crates/engine/src/local_copy/skip_compress.rs
  - CLI flag --skip-compress exists and works
  - Default patterns: gz, zip, bz2, xz, 7z, mp4, etc. (73 extensions)
  - Status in parity-options.yml: implemented
- ✅ Compression integration tests (Commit: 3e398602)
  - 6 end-to-end tests verify compression with data integrity
  - Tests cover compressible/incompressible data, multiple files, large files
  - Skip-compress patterns tested

### 4. Compat Flags Usage
**Status**: Accessible but not used for runtime behavior
**Location**: `crates/core/src/server/{receiver,generator}.rs`
**Impact**: Protocol-specific optimizations and behaviors disabled

**Current State**:
- ✅ Compat flags exchanged during Protocol 30+ setup
- ✅ Stored in `HandshakeResult.compat_flags: Option<CompatibilityFlags>`
- ✅ Passed to role contexts (ReceiverContext, GeneratorContext)
- ✅ Accessor methods: `ctx.compat_flags()` returns Option<CompatibilityFlags> (Commit: 52201448)
- ✅ Integration tests verify flag accessibility and individual flag checks
- ❌ Not used to control protocol behaviors

**Required Implementation**:
Use flags to control protocol behaviors in role contexts:
- `INC_RECURSE` - Enable incremental recursion mode
- `SAFE_FILE_LIST` - Change file list validation rules
- `AVOID_XATTR_OPTIMIZATION` - Disable xattr shortcuts
- `CHECKSUM_SEED_FIX` - Handle seed order variations
- `SYMLINK_TIMES` - Preserve symlink timestamps
- `SYMLINK_ICONV` - Character set conversion for symlinks
- `INPLACE_PARTIAL_DIR` - Allow in-place with partial-dir
- `VARINT_FLIST_FLAGS` - Use varint encoding for file list flags
- `ID0_NAMES` - Send user/group names for ID 0

**Example Usage**:
```rust
if let Some(flags) = &self.compat_flags {
    if flags.has(CompatFlag::INC_RECURSE) {
        // Use incremental recursion
    }
}
```

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
2. ✅ **Algorithm Selection**: Verify negotiated algorithms are used (COMPLETED)
3. **Compat Flags**: Runtime behavior based on negotiated flags (MEDIUM)
4. **Batch Mode**: End-to-end batch application (LOW)
5. **Compression Streams**: Verify compressed data flow (MEDIUM)

### Current Test Status
- ✅ Unit tests: Good coverage in most crates
- ✅ Integration tests: Basic transfer scenarios work
- ✅ Property tests: Checksums, filters have property tests
- ✅ Algorithm tests: 14 integration tests for negotiated checksums
- ✅ Compat flags tests: 3 integration tests for flag accessibility
- ✅ Compression streams: 7 tests for compression wrappers
- ✅ Compression config: 3 tests for ServerConfig.compression_level
- ✅ Compression integration: 6 tests for end-to-end compression
- ✅ Compat flags behavior: 14 tests for flag implementation (Phase 4)
- ✅ Golden tests: Protocol handshakes complete (12 tests passing)
- ✅ Interop tests: Exit codes and messages validated, handshakes complete
- **Total**: 3382/3382 tests passing ✅ (as of commit 3887d08a)

---

## Implementation Roadmap

### Phase 1: ✅ Testing Infrastructure (COMPLETED)
**Status**: COMPLETE
**Commit**: 955761f5, 3887d08a
**Priority**: HIGH
1. ✅ Capture golden handshake files for protocols 28-32
2. ✅ Verify golden tests pass (12 tests, all passing)
3. ✅ Document handshake capture process (tools/strace-capture.sh)

### Phase 2: ✅ Checksum Algorithm Wiring (COMPLETED)
**Status**: COMPLETE
**Commits**: f9d22b2c, db41d62e, 350c88dd, c57ae371
1. ✅ Design context structure for negotiated algorithms
2. ✅ Pass negotiation results through role contexts
3. ✅ Update checksum selection in delta operations
4. ✅ Add tests for algorithm selection (14 integration tests)

### Phase 3: ✅ Compression Stream Implementation (COMPLETED)
**Status**: COMPLETE
**Commits**: 78b69abe, e6f7dae6, 36954c8b, 2214a80c, 3e398602
1. ✅ Created CompressedWriter and CompressedReader wrappers
2. ✅ Integrated into server stream stack (Plain → Multiplex → Compress)
3. ✅ Wired negotiated compression algorithm to wrappers
4. ✅ Handled compression lifecycle (init, write, flush, finish)
5. ✅ Added 7 comprehensive tests for compressed data flow
6. ✅ Added compression level configuration (ServerConfig.compression_level)
7. ✅ Verified skip-compress patterns already implemented
8. ✅ Added 6 integration tests for end-to-end compression

### Phase 4: ✅ Runtime Flags Usage (SUBSTANTIALLY COMPLETE)
**Status**: COMPLETE (except complex deferred items)
**Priority**: MEDIUM
**Commits**: 52201448, aa735a3f, 16d65c53, b40b235d, d1554f23, 1f5142c1
1. ✅ Add accessor methods for compat_flags (Commit: 52201448)
2. ✅ Remove `#[allow(dead_code)]` annotations (Commit: 52201448)
3. ✅ Add tests for flag accessibility (3 tests)
4. ✅ Use compat flags in role contexts for protocol behaviors
5. ✅ Implement flag-dependent behavior (Commits: aa735a3f, 16d65c53, b40b235d, d1554f23)
   - ✅ CHECKSUM_SEED_FIX - MD5 seed ordering (aa735a3f)
   - ✅ SAFE_FILE_LIST - I/O error transmission (16d65c53)
   - ✅ SYMLINK_TIMES - Platform-conditional timestamps (b40b235d)
   - ✅ ID0_NAMES - uid/gid 0 name transmission (d1554f23)
   - ✅ INPLACE_PARTIAL_DIR - --inplace with --partial-dir (d1554f23)
   - ✅ AVOID_XATTR_OPTIMIZATION - xattr hardlink control (d1554f23)
   - ✅ VARINT_FLIST_FLAGS - Protocol handles automatically
   - ⏸️ INC_RECURSE - Analyzed, 16-24hr task, deferred (1f5142c1)
   - ⏸️ SYMLINK_ICONV - Requires iconv integration, deferred
6. ✅ Test with various flag combinations (14 new tests across commits)

### Phase 5: Automation (optional)
**Status**: PENDING
**Priority**: LOW
1. Implement capture-handshakes xtask command
2. Automate pcap parsing with tshark
3. Integrate into CI pipeline

### Phase 6: Batch Mode (optional)
**Status**: PENDING
**Priority**: LOW
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

---

## Update 2025-12-18: Recent Completions

### Exit Code Parity - 100% Complete
**Status**: ✅ COMPLETE
**Commit**: b6c56305
**Impact**: Perfect compatibility with upstream rsync error handling

All 15 exit code scenarios now match upstream rsync behavior:
- daemon_connection_refused: Fixed rsync:// URL parsing and connection attempts
- permission_denied_write: Added early destination directory validation  
- remote_not_found: Mapped SSH spawn failures to IPC_EXIT_CODE (14)

**Validation**: `cargo xtask interop exit-codes --impl oc-rsync --version 3.4.1` - 15/15 passing

### Daemon Client Data Transfer - In Progress
**Status**: 🚧 IMPLEMENTATION STARTED
**Impact**: Enable client-side daemon file push/pull (rsync:// URLs)

**Current State**:
- ✅ Daemon server-side works (201 tests passing)
- ✅ Module listing works (run_module_list)
- ✅ Daemon handshake protocol implemented
- 🚧 Client-side data transfer skeleton created
- ❌ Server execution with daemon stream not wired

**New Files**:
- `crates/core/src/client/remote/daemon_transfer.rs` - Daemon transfer orchestration

**Remaining Work**:
1. Complete daemon handshake → server execution wiring
2. Build ServerConfig from daemon transfer request
3. Execute transfer using run_server_stdio with daemon stream
4. Add integration tests for daemon push/pull
5. Wire daemon transfer into run.rs instead of "not implemented" error

### Protocol Message Duplication - Documented
**Status**: ✅ DOCUMENTED AS EXPECTED
**File**: `docs/design/PROTOCOL_MESSAGE_DUPLICATION.md`

The protocol_version_mismatch scenario shows message duplication differences with
upstream 3.0.9/3.1.3. This is expected behavior due to architectural differences:
- Upstream: Fork-based (separate sender/receiver processes → messages twice)
- oc-rsync: Single-process architecture (→ message once)

Exit codes are identical; only message count differs. This is acceptable and not a bug.

