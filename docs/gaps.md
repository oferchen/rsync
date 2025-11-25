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
| **Daemon Auth** | 🔧 IN PROGRESS | 199/201 passing | File transfers pending |
| **Compression** | ❓ UNKNOWN | Need mapping | -z, --compress-level |
| **Metadata** | ❓ UNKNOWN | Need mapping | --perms, --chmod, --owner, --acls, --xattrs |
| **Delete/Backup** | ❓ UNKNOWN | Need mapping | --delete*, --backup* |
| **Protocol** | ❓ UNKNOWN | Need mapping | Versions 32-28 |
| **Remote Shell** | ❓ UNKNOWN | Need mapping | ssh transport |

---

## Known Gaps

### 1. Daemon Module File Transfers

**Status**: 🔧 IN PROGRESS
**Category**: daemon

**Description**:
Daemon successfully handles:
- TCP connections ✅
- Protocol negotiation ✅
- Module listing ✅
- Authentication ✅

Daemon does NOT handle:
- File transfer after authentication ❌
- Routing to `core::server::run_server_stdio` ❌

**Evidence**:
```bash
# Test: run_daemon_accepts_valid_credentials
# Expected: Error message after auth
# Actual: 10s timeout (SOCKET_TIMEOUT)
```

**Impact**: Blocks daemon-to-client file transfer scenarios

**Location**: `crates/daemon/src/daemon/sections/session_runtime.rs`

**Estimated Effort**: Medium

**Blocked Tests**:
1. `run_daemon_accepts_valid_credentials`
2. `run_daemon_records_log_file_entries`

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
