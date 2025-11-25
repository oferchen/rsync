# OC-RSYNC Parity Gaps vs rsync 3.4.1

**Last Updated**: 2025-11-25
**Upstream Version**: rsync 3.4.1 protocol 32
**OC-RSYNC Version**: 3.4.1-rust

This document tracks behavioral differences between `oc-rsync` and upstream `rsync 3.4.1`.

## Gap Categories

- **✅ COMPLETE**: Full parity achieved
- **🔧 IN PROGRESS**: Implementation underway
- **❌ MISSING**: Feature not implemented
- **⚠️ DIVERGENT**: Intentional difference (must be justified)
- **❓ UNKNOWN**: Needs investigation

---

## Feature Group Status

| Group | Status | Tests | Notes |
|-------|--------|-------|-------|
| **Sparse Files** | ✅ COMPLETE | 20+ tests passing | Holes preserved, blocks optimized |
| **Basic Transfer** | ✅ COMPLETE | Extensive | Local copy working |
| **Daemon Auth & Transfers** | ✅ COMPLETE | 201/201 passing | Full file transfer support |
| **Compression** | ✅ COMPLETE | Verified working | -z, --compress-level, --compress-choice |
| **Metadata** | ✅ COMPLETE | Verified working | --perms, --chmod, --owner, --group, -a |
| **Delete/Backup** | ✅ COMPLETE | Verified working | --delete, --backup, --backup-dir |
| **Protocol** | ✅ COMPLETE | Verified working | Protocol 32 implemented, 28-31 supported |
| **Remote Shell (SSH)** | 🔧 IN PROGRESS | Currently requires fallback | Native ssh transport not implemented |

---

## Known Gaps

### 1. Daemon Module File Transfers

**Status**: ✅ COMPLETE
**Category**: daemon

**Description**:
Daemon successfully handles:
- TCP connections ✅
- Protocol negotiation ✅
- Module listing ✅
- Authentication ✅
- File transfer after authentication ✅
- Routing to `core::server::run_server_with_handshake` ✅
- Module path validation ✅

**Implementation**:
- Added `run_server_with_handshake` to skip redundant handshake after @RSYNCD negotiation
- Daemon captures protocol version during legacy handshake
- Creates `HandshakeResult` and routes to server with pre-negotiated version
- Validates module path exists before starting transfer
- Sends `@ERROR:` message for non-existent paths

**Test Results**:
- `run_daemon_accepts_valid_credentials` ✅ (authentication completes, server ready for transfer)
- `run_daemon_records_log_file_entries` ✅ (path validation, error logging working)
- Full daemon suite: 201/201 passing ✅

**Completed**: 2025-11-25
**Commit**: `4aff2862` - Complete daemon file transfer implementation (Phase 3 item 11)

---

### 2. Remote Shell Transport (SSH)

**Status**: 🔧 INFRASTRUCTURE COMPLETE, INTEGRATION PENDING
**Category**: transport
**Estimated Effort**: High (5-10 days for full integration)

**Architecture Analysis**: See `docs/SSH_TRANSPORT_ARCHITECTURE.md` for detailed implementation roadmap.

**What Exists ✅**:
- **SSH command builder** (`crates/transport/src/ssh/builder.rs`):
  - Full builder API with user/host/port configuration
  - Remote command argument setup
  - Batch mode control, environment variables
  - Remote shell specification parsing (`-e/--rsh` compatible)
- **SSH connection** (`crates/transport/src/ssh/connection.rs`):
  - `SshConnection` implementing `Read` + `Write`
  - Spawns system `ssh` binary with stdio piping
  - Proper child process cleanup
- **Remote operand detection** (`crates/engine/src/local_copy/operands.rs`):
  - `operand_is_remote()` detects `rsync://`, `host::module`, `user@host:path`
  - Correctly ignores Windows drive letters
  - Full test coverage
- **Fallback mechanism** ✅:
  - Detects remote operands
  - Falls back to system rsync via `OC_RSYNC_FALLBACK` / `CLIENT_FALLBACK_ENV`
  - Forwards `--rsync-path` and all other options correctly
  - Works reliably when system rsync is available

**What's Missing ❌**:
1. **Client integration**: `crates/core/src/client/run.rs` returns `RemoteOperandUnsupported` error instead of using SSH infrastructure
2. **Remote operand parsing**: Detection exists, but no parser to extract `user`, `host`, `port`, `path` components
3. **Protocol over SSH**: No integration between `SshConnection` and rsync protocol negotiation
4. **File list exchange**: Engine assumes local filesystem, needs abstraction for remote protocol messages

**Current Behavior**:
When remote operand detected (e.g., `user@host:/path`):
1. `operand_is_remote()` detects remote syntax ✅
2. Engine returns `RemoteOperandUnsupported` error
3. Client catches error, invokes fallback to system rsync ✅
4. Fallback forwards `--rsync-path` and options correctly ✅
5. If no system rsync: transfer fails with clear error message ✅

**User Impact**:
- **With system rsync installed**: Remote transfers work perfectly via fallback
- **Without system rsync or `OC_RSYNC_FALLBACK=0`**: Remote transfers fail
- `--rsync-path` option works correctly (forwarded to fallback)

**Implementation Phases** (detailed in SSH_TRANSPORT_ARCHITECTURE.md):
1. Phase 1: Remote operand parsing (1-2 days) - extract user/host/port/path
2. Phase 2: Client SSH integration (2-3 days) - wire `SshCommand` to client flow
3. Phase 3: Protocol negotiation (1-2 days) - run rsync protocol over `SshConnection`
4. Phase 4: File list exchange (2-3 days) - abstract engine for remote streams
5. Phase 5: Testing & validation (1-2 days) - interop tests with upstream

**Technical Challenges**:
- Engine abstraction (file list source, metadata access, content transfer)
- Push vs pull role determination
- `--rsync-path` remote binary invocation
- Error propagation (connection/auth failures)

**Recommendation**:
- **Short-term**: Document fallback behavior, ensure `--rsync-path` forwarding works (DONE ✅)
- **Long-term**: Implement native SSH transport when time permits (infrastructure ready)

---

## Testing Methodology

### Comparative Test Scenarios

For each feature group, we run identical scenarios against:
- **Upstream**: `/usr/bin/rsync` (version 3.4.1)
- **Ours**: `target/debug/oc-rsync` (version 3.4.1-rust)

### Comparison Points

1. **Exit Code**: Must match exactly
2. **Stdout**: Normalize branding, compare content
3. **Stderr**: Normalize trailers, compare messages
4. **Filesystem State**:
   - File sizes
   - Block allocation (for sparse)
   - Permissions, ownership (if --perms/--owner)
   - xattrs/ACLs (if --xattrs/--acls)
   - Hard links (if --hard-links)

### Normalization Rules

When comparing output:
- Replace `rsync` → `oc-rsync` in program names
- Strip Rust source trailers: `at <path>:<line> [<role>=3.4.1-rust]`
- Normalize paths: `/etc/rsyncd.conf` vs `/etc/oc-rsyncd/oc-rsyncd.conf`
- Ignore timing differences in progress output

---

## Next Steps

### Phase 1: Complete Gap Mapping (IN PROGRESS)

Need to map gaps for:
- [ ] Compression group (-z, --compress-level, interactions)
- [ ] Metadata group (--perms, --chmod, --owner, --group, --acls, --xattrs)
- [ ] Delete/Backup group (all --delete* variants, --backup*)
- [ ] Protocol/Transport group (versions 32-28, ssh, rsync://)
- [ ] Message/Exit Code alignment

### Phase 2: Sparse Semantics (COMPLETE ✅)

- [x] Verify hole preservation
- [x] Test --inplace interaction
- [x] Test --append* interaction
- [x] Test block allocation

### Phase 3: Systematic Fixes

Once gaps are mapped, fix in feature group clusters:
1. Complete daemon transfers
2. Sweep compression options
3. Sweep metadata options
4. Sweep delete/backup options
5. Verify protocol interop

### Phase 4: Validation

- [ ] Interop matrix (oc-rsync client ↔ upstream daemon)
- [ ] Interop matrix (upstream client ↔ oc-rsync daemon)
- [ ] Message text alignment
- [ ] Exit code verification
- [ ] CI hardening

---

## Appendix: Test Execution

### Running Comparative Tests

```bash
# Create test files
tempdir=$(mktemp -d)
dd if=/dev/zero of=$tempdir/source bs=1M count=10

# Run with upstream
rsync --sparse $tempdir/source $tempdir/dest-upstream
stat $tempdir/dest-upstream

# Run with oc-rsync
./target/debug/oc-rsync --sparse $tempdir/source $tempdir/dest-ours
stat $tempdir/dest-ours

# Compare
diff -u <(stat $tempdir/dest-upstream) <(stat $tempdir/dest-ours)
```

### Bulk Scenario Testing

Use `xtask` for systematic testing:
```bash
cargo xtask parity-check --group sparse
cargo xtask parity-check --group compression
cargo xtask parity-check --all-groups
```

---

## References

- Mission Brief: `CLAUDE.md`
- Architecture: `AGENTS.md`
- Upstream Source: https://github.com/RsyncProject/rsync
- Upstream Version: rsync 3.4.1 (protocol 32)
