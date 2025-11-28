# OC-RSYNC Parity Gap Analysis vs rsync 3.4.1

**Generated**: 2025-11-28  
**Upstream Version**: rsync 3.4.1 (protocol 32)  
**OC-RSYNC Version**: 3.4.1-rust  
**Analysis Phase**: Phase 1 - Comprehensive Mapping

This document provides a detailed analysis of behavioral gaps between `oc-rsync` and upstream `rsync 3.4.1` across all major feature groups. Each section identifies specific scenarios, expected vs actual behavior, and categorizes the gap type.

---

## Gap Categories

- **✅ COMPLETE**: Full parity achieved, tests passing
- **🔧 PARTIAL**: Feature exists but incomplete or untested
- **❌ MISSING**: Feature not implemented
- **⚠️ DIVERGENT**: Intentional difference (must be justified)
- **❓ UNKNOWN**: Needs investigation

---

## 1. SPARSE FILES GROUP

### Status: ✅ COMPLETE (Infrastructure) / 🔧 PARTIAL (Parity Validation)

### Implementation Status

**What Exists**:
- `crates/engine/src/local_copy/executor/file/sparse.rs`: Full sparse writer with SIMD zero-run detection
- `SparseWriteState`: Tracks pending zero runs, flushes via seeks
- `write_sparse_chunk`: Batches zero detection, accumulates runs, writes non-zero data
- `leading_zero_run` / `trailing_zero_run`: u128-based fast path (16-byte chunks)
- Tests: `execute_with_sparse_enabled_creates_holes`, `execute_inplace_disables_sparse_writes`

**Parity Validation Needed**:

#### Scenario 1: Basic Sparse Copy
```bash
# Create sparse source
dd if=/dev/zero of=source bs=1M count=100 seek=50

# Upstream
rsync --sparse source dest-upstream
stat dest-upstream  # Check: size, blocks allocated

# Oc-rsync
oc-rsync --sparse source dest-ours
stat dest-ours  # Check: size, blocks allocated

# Compare: blocks should be similar for typical filesystems
```

**Expected**: Both should have same file size, similar block allocation (accounting for filesystem variance)  
**Actual**: Test exists but needs upstream comparison baseline  
**Gap Type**: 🔧 PARTIAL - Infrastructure complete, parity validation pending

#### Scenario 2: Sparse with --inplace
```bash
# Upstream
rsync --sparse --inplace source dest-upstream

# Oc-rsync
oc-rsync --sparse --inplace source dest-ours

# Behavior: --inplace should disable sparse (write zeros explicitly)
```

**Expected**: With --inplace, zeros written explicitly (no holes), blocks == file size / blocksize  
**Actual**: Test `execute_inplace_disables_sparse_writes` exists, needs upstream verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Sparse with --append
```bash
# Initial partial file
dd if=/dev/zero of=dest-partial bs=1M count=50

# Upstream
rsync --sparse --append source dest-partial-upstream

# Oc-rsync
oc-rsync --sparse --append source dest-partial-ours

# Behavior: Should preserve existing holes, append new data
```

**Expected**: Existing holes preserved, new data appended with sparse detection  
**Actual**: ❓ UNKNOWN - Interaction not explicitly tested  
**Gap Type**: ❓ UNKNOWN

#### Scenario 4: Sparse with --append-verify
```bash
# Upstream
rsync --sparse --append-verify source dest-upstream

# Oc-rsync
oc-rsync --sparse --append-verify source dest-ours
```

**Expected**: Checksum verification before append, sparse detection for new data  
**Actual**: ❓ UNKNOWN - Interaction not explicitly tested  
**Gap Type**: ❓ UNKNOWN

#### Scenario 5: Sparse with --partial
```bash
# Simulated interrupted transfer
rsync --sparse --partial source dest

# Resume
rsync --sparse --partial source dest
```

**Expected**: Partial file preserved, sparse detection resumes from partial point  
**Actual**: ❓ UNKNOWN - Interaction not explicitly tested  
**Gap Type**: ❓ UNKNOWN

#### Scenario 6: Sparse with --partial-dir
```bash
rsync --sparse --partial-dir=.rsync-partial source/ dest/
```

**Expected**: Partial files stored in separate directory, sparse detection active  
**Actual**: ❓ UNKNOWN - Interaction not explicitly tested  
**Gap Type**: ❓ UNKNOWN

#### Scenario 7: Multiple Sparse Patterns
```bash
# Pattern: data-hole-data-hole-data
dd if=/dev/urandom of=source bs=1M count=10
dd if=/dev/zero of=source bs=1M count=20 seek=10
dd if=/dev/urandom of=source bs=1M count=10 seek=30
dd if=/dev/zero of=source bs=1M count=20 seek=40
dd if=/dev/urandom of=source bs=1M count=10 seek=60

rsync --sparse source dest
```

**Expected**: Multiple holes preserved, data blocks allocated  
**Actual**: SIMD batching should handle this, needs upstream comparison  
**Gap Type**: 🔧 PARTIAL

---

## 2. COMPRESSION GROUP

### Status: 🔧 PARTIAL (Infrastructure exists, interactions untested)

### Implementation Status

**What Exists**:
- `crates/compress/`: zlib (default), zstd (feature), lz4 (feature)
- `algorithm.rs`: `CompressionAlgorithm` enum (Zlib, Zstd, Lz4)
- `zlib.rs`: Level 1-9, default 6, RFC1950 framing
- `zstd.rs`: Level 1-22, default 3
- `lz4.rs`: Level 1-12, default 1
- CLI parsing: `--compress`, `--compress-level`, `--compress-choice`, `--skip-compress`

**Parity Validation Needed**:

#### Scenario 1: Basic Compression (-z)
```bash
# Upstream
rsync -z source dest-upstream

# Oc-rsync
oc-rsync -z source dest-ours

# Check: compression active, correct default level (6 for zlib)
```

**Expected**: Default zlib level 6, data compressed during transfer  
**Actual**: Infrastructure exists, needs protocol-level verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 2: Compression Level (--compress-level)
```bash
# Upstream
rsync --compress-level=9 source dest-upstream

# Oc-rsync
oc-rsync --compress-level=9 source dest-ours
```

**Expected**: Level 9 compression applied  
**Actual**: Parsing exists, needs protocol verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Compression Choice (--compress-choice)
```bash
# Upstream
rsync --compress-choice=zstd source dest-upstream

# Oc-rsync
oc-rsync --compress-choice=zstd source dest-ours
```

**Expected**: zstd negotiation, fallback to zlib if peer doesn't support  
**Actual**: Infrastructure exists, negotiation needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 4: Compression with --whole-file
```bash
# Upstream
rsync -z --whole-file source dest-upstream

# Oc-rsync
oc-rsync -z --whole-file source dest-ours
```

**Expected**: Compression applied to whole file transfer  
**Actual**: ❓ UNKNOWN - Interaction needs testing  
**Gap Type**: ❓ UNKNOWN

#### Scenario 5: Compression with --inplace
```bash
# Upstream
rsync -z --inplace source dest-upstream

# Oc-rsync
oc-rsync -z --inplace source dest-ours
```

**Expected**: Compressed delta applied in place  
**Actual**: ❓ UNKNOWN - Interaction needs testing  
**Gap Type**: ❓ UNKNOWN

#### Scenario 6: Compression with --append
```bash
# Upstream
rsync -z --append source dest-upstream

# Oc-rsync
oc-rsync -z --append source dest-ours
```

**Expected**: New data compressed during append  
**Actual**: ❓ UNKNOWN - Interaction needs testing  
**Gap Type**: ❓ UNKNOWN

#### Scenario 7: Skip Compress (--skip-compress)
```bash
# Upstream
rsync -z --skip-compress=jpg/png/zip source/ dest-upstream/

# Oc-rsync
oc-rsync -z --skip-compress=jpg/png/zip source/ dest-ours/
```

**Expected**: Listed extensions skip compression  
**Actual**: Parsing exists, needs protocol verification  
**Gap Type**: 🔧 PARTIAL

---

## 3. METADATA GROUP

### Status: ✅ COMPLETE (Basic) / 🔧 PARTIAL (Advanced)

### Implementation Status

**What Exists**:
- `crates/metadata/`: Full metadata crate with apply/options/mapping
- `--perms`, `--chmod`, `--owner`, `--group`, `--numeric-ids`: Implemented
- `--acls`: Implemented (feature gate `acl`)
- `--xattrs`: Implemented (feature gate `xattr`)
- `-a` (archive): Implies `-rlptgoD`

**Parity Validation Needed**:

#### Scenario 1: Basic Permissions (--perms)
```bash
# Upstream
rsync --perms source dest-upstream

# Oc-rsync
oc-rsync --perms source dest-ours

# Compare: stat -c '%a' dest-*
```

**Expected**: Permissions preserved exactly  
**Actual**: Infrastructure exists, needs upstream comparison  
**Gap Type**: 🔧 PARTIAL

#### Scenario 2: Chmod Directive (--chmod)
```bash
# Upstream
rsync -a --chmod=D755,F644 source/ dest-upstream/

# Oc-rsync
oc-rsync -a --chmod=D755,F644 source/ dest-ours/

# Check: directories 755, files 644
```

**Expected**: Permissions modified according to directive  
**Actual**: `crates/metadata/src/chmod/` exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Ownership (--owner --group)
```bash
# Upstream (as root)
rsync -a --owner --group source dest-upstream

# Oc-rsync (as root)
oc-rsync -a --owner --group source dest-ours

# Compare: stat -c '%u:%g'
```

**Expected**: UID/GID preserved  
**Actual**: Infrastructure exists, needs root test  
**Gap Type**: 🔧 PARTIAL

#### Scenario 4: Numeric IDs (--numeric-ids)
```bash
# Upstream
rsync -a --numeric-ids source dest-upstream

# Oc-rsync
oc-rsync -a --numeric-ids source dest-ours

# Behavior: Skip name lookups, use numeric IDs directly
```

**Expected**: No user/group name resolution  
**Actual**: Option parsed, needs verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 5: ACLs (--acls)
```bash
# Set ACLs on source
setfacl -m u:alice:rwx source/file1

# Upstream
rsync -a --acls source/ dest-upstream/

# Oc-rsync
oc-rsync -a --acls source/ dest-ours/

# Compare: getfacl dest-*/file1
```

**Expected**: ACLs preserved exactly  
**Actual**: `crates/metadata/src/acl_support.rs` exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 6: Extended Attributes (--xattrs)
```bash
# Set xattrs on source
setfattr -n user.foo -v bar source/file1

# Upstream
rsync -a --xattrs source/ dest-upstream/

# Oc-rsync
oc-rsync -a --xattrs source/ dest-ours/

# Compare: getfattr -d dest-*/file1
```

**Expected**: xattrs preserved (respecting namespace rules)  
**Actual**: `crates/metadata/src/xattr.rs` exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 7: ACLs Implies Perms
```bash
# Upstream
rsync --acls source dest-upstream

# Oc-rsync
oc-rsync --acls source dest-ours

# Behavior: --acls should imply --perms
```

**Expected**: `--acls` automatically enables `--perms`  
**Actual**: Needs verification in option parsing  
**Gap Type**: ❓ UNKNOWN

#### Scenario 8: ACL Unsupported Diagnostic
```bash
# On filesystem without ACL support
rsync --acls source dest

# Expected error: "rsync: --acls: not supported on this system"
```

**Expected**: Upstream-style error when ACL unavailable  
**Actual**: Needs diagnostic alignment  
**Gap Type**: ❓ UNKNOWN

---

## 4. DELETE & BACKUP GROUP

### Status: 🔧 PARTIAL (CLI exists, engine needs testing)

### Implementation Status

**What Exists**:
- `crates/engine/src/local_copy/options/deletion.rs`: Deletion options
- CLI parsing: `--delete`, `--delete-before`, `--delete-during`, `--delete-delay`, `--delete-after`, `--delete-excluded`
- `--backup`, `--backup-dir`, `--suffix`
- `--max-delete`: Delete limit enforcement

**Parity Validation Needed**:

#### Scenario 1: Delete Before (--delete-before)
```bash
# Upstream
rsync -av --delete-before source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete-before source/ dest-ours/

# Behavior: Deletions happen before transfer starts
```

**Expected**: Old files deleted before new files transferred  
**Actual**: Option parsed, execution order needs verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 2: Delete During (--delete-during / --delete)
```bash
# Upstream
rsync -av --delete source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete source/ dest-ours/

# Behavior: Default delete timing, happens during transfer
```

**Expected**: Deletions interleaved with transfers  
**Actual**: Option parsed, needs timing verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Delete Delay (--delete-delay)
```bash
# Upstream
rsync -av --delete-delay source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete-delay source/ dest-ours/

# Behavior: Deletions deferred until all transfers complete
```

**Expected**: All transfers finish, then deletions occur  
**Actual**: Option parsed, needs timing verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 4: Delete After (--delete-after)
```bash
# Upstream
rsync -av --delete-after source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete-after source/ dest-ours/

# Behavior: Deletions happen after all transfers complete
```

**Expected**: Same as --delete-delay  
**Actual**: Option parsed, needs verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 5: Delete Excluded (--delete-excluded)
```bash
# Upstream
rsync -av --delete --delete-excluded --exclude='*.tmp' source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete --delete-excluded --exclude='*.tmp' source/ dest-ours/

# Behavior: Excluded files also deleted from dest
```

**Expected**: *.tmp files deleted from destination  
**Actual**: Option parsed, needs filter interaction test  
**Gap Type**: 🔧 PARTIAL

#### Scenario 6: Max Delete (--max-delete)
```bash
# Upstream
rsync -av --delete --max-delete=10 source/ dest-upstream/

# Oc-rsync
oc-rsync -av --delete --max-delete=10 source/ dest-ours/

# Behavior: Stop if more than 10 files would be deleted
```

**Expected**: Error if deletion count exceeds limit  
**Actual**: `--max-delete` parsed, enforcement needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 7: Backup (--backup)
```bash
# Upstream
rsync -av --backup source/ dest-upstream/

# Oc-rsync
oc-rsync -av --backup source/ dest-ours/

# Behavior: Files renamed with ~ suffix before replacement
```

**Expected**: Old files renamed to filename~  
**Actual**: Option parsed, backup mechanism needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 8: Backup Dir (--backup --backup-dir)
```bash
# Upstream
rsync -av --backup --backup-dir=../backup source/ dest-upstream/

# Oc-rsync
oc-rsync -av --backup --backup-dir=../backup source/ dest-ours/

# Behavior: Replaced files moved to backup directory
```

**Expected**: Old files moved to ../backup/ with directory structure  
**Actual**: Option parsed, backup dir mechanism needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 9: Backup Suffix (--backup --suffix)
```bash
# Upstream
rsync -av --backup --suffix=.old source/ dest-upstream/

# Oc-rsync
oc-rsync -av --backup --suffix=.old source/ dest-ours/

# Behavior: Custom backup suffix instead of ~
```

**Expected**: Old files renamed to filename.old  
**Actual**: Option parsed, suffix mechanism needs testing  
**Gap Type**: 🔧 PARTIAL

---

## 5. DAEMON & PROTOCOL GROUP

### Status: ✅ COMPLETE (Basic) / 🔧 PARTIAL (Advanced)

### Implementation Status

**What Exists**:
- `crates/daemon/`: Full daemon implementation
- `--daemon` mode: TCP listen, module serving
- Protocol 32 negotiation: `crates/protocol/`
- Legacy `@RSYNCD:` handshake
- Module authentication: secrets file, challenge/response
- `hosts allow/deny`: Access control
- `use chroot`: Chroot enforcement
- Protocol 28-31: Backward compatibility

**Parity Validation Needed**:

#### Scenario 1: Basic Daemon Start (--daemon)
```bash
# Upstream
rsync --daemon --config=/etc/rsyncd.conf

# Oc-rsync
oc-rsync --daemon --config=/etc/oc-rsyncd/oc-rsyncd.conf

# Behavior: Listen on port 873, accept connections
```

**Expected**: Daemon starts, accepts connections  
**Actual**: ✅ COMPLETE - Tested  
**Gap Type**: ✅ COMPLETE

#### Scenario 2: Module Listing
```bash
# Upstream
rsync rsync://localhost/

# Oc-rsync
rsync rsync://localhost/

# Behavior: List available modules with comments
```

**Expected**: Module list with descriptions  
**Actual**: ✅ COMPLETE - Tested  
**Gap Type**: ✅ COMPLETE

#### Scenario 3: Secrets File Authentication
```bash
# Config: auth users = alice:bob, secrets file = /etc/rsyncd.secrets

# Upstream
rsync rsync://alice@localhost/module/ dest/

# Oc-rsync
rsync rsync://alice@localhost/module/ dest/

# Behavior: Challenge/response auth
```

**Expected**: Password prompt, authentication  
**Actual**: ✅ COMPLETE - Tested  
**Gap Type**: ✅ COMPLETE

#### Scenario 4: Secrets File Permissions Check
```bash
# Config: secrets file = /etc/rsyncd.secrets
# File permissions: 0644 (wrong)

# Upstream
rsync --daemon
# Error: "secrets file must not be other-accessible (see strict modes option)"

# Oc-rsync
oc-rsync --daemon
# Expected: Same error
```

**Expected**: Error if secrets file not 0600  
**Actual**: Permission check exists, needs diagnostic alignment  
**Gap Type**: 🔧 PARTIAL

#### Scenario 5: Hosts Allow/Deny
```bash
# Config:
#   hosts allow = 192.168.1.0/24
#   hosts deny = 192.168.1.50

# Connection from 192.168.1.50
# Expected: Denied
```

**Expected**: Access control enforced  
**Actual**: Infrastructure exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 6: Use Chroot
```bash
# Config: use chroot = yes

# Behavior: Daemon chroots to module path
```

**Expected**: Chroot enforced, non-absolute paths rejected  
**Actual**: `use chroot` parsed, enforcement needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 7: UID/GID Drop
```bash
# Config: uid = nobody, gid = nogroup

# Behavior: Daemon drops privileges
```

**Expected**: Process runs as specified user/group  
**Actual**: UID/GID drop exists, needs verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 8: Max Connections
```bash
# Config: max connections = 5

# Behavior: Reject 6th connection
```

**Expected**: Connection refused when limit reached  
**Actual**: `max connections` parsed, enforcement exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 9: Protocol 32 Negotiation
```bash
# Client: rsync 3.4.1 (protocol 32)
# Server: oc-rsync (protocol 32)

# Behavior: Negotiate protocol 32
```

**Expected**: Protocol 32 selected  
**Actual**: ✅ COMPLETE - Protocol negotiation tested  
**Gap Type**: ✅ COMPLETE

#### Scenario 10: Protocol 28-31 Backward Compatibility
```bash
# Client: rsync 3.0.9 (protocol 28)
# Server: oc-rsync (protocol 32)

# Behavior: Negotiate protocol 28
```

**Expected**: Fall back to protocol 28  
**Actual**: Protocol range 28-32 supported, needs interop test  
**Gap Type**: 🔧 PARTIAL

#### Scenario 11: rsync:// Syntax
```bash
# Upstream
rsync rsync://host/module/path dest/

# Oc-rsync
oc-rsync rsync://host/module/path dest/

# Behavior: Connect to daemon, transfer
```

**Expected**: TCP connection to port 873, protocol negotiation  
**Actual**: Infrastructure exists, needs end-to-end test  
**Gap Type**: 🔧 PARTIAL

---

## 6. REMOTE SHELL (SSH) GROUP

### Status: 🔧 INFRASTRUCTURE COMPLETE / ❌ INTEGRATION MISSING

### Implementation Status

**What Exists**:
- `crates/transport/src/ssh/`: SSH infrastructure
  - `builder.rs`: SSH command builder
  - `connection.rs`: SshConnection (Read + Write)
- `operand_is_remote()`: Remote syntax detection
- Fallback mechanism: Delegates to system rsync

**What's Missing**:
- Client integration: Native SSH transport
- Remote operand parsing: Extract user/host/port/path
- Protocol over SSH: Run rsync protocol over SshConnection
- File list exchange: Abstract engine for remote streams

**Gap Type**: 🔧 INFRASTRUCTURE COMPLETE, ❌ INTEGRATION MISSING (see gaps.md SSH section)

---

## 7. FILTER ENGINE GROUP

### Status: ✅ COMPLETE (Basic) / 🔧 PARTIAL (Advanced)

### Implementation Status

**What Exists**:
- `crates/filters/`: Filter rule engine
- `--include`, `--exclude`: Basic patterns
- `--filter`: Rule syntax
- `--files-from`: File list input
- `.rsync-filter`: Per-directory rules

**Parity Validation Needed**:

#### Scenario 1: Include/Exclude
```bash
# Upstream
rsync -av --include='*.txt' --exclude='*' source/ dest-upstream/

# Oc-rsync
oc-rsync -av --include='*.txt' --exclude='*' source/ dest-ours/

# Behavior: Only *.txt files transferred
```

**Expected**: Filter rules applied correctly  
**Actual**: Infrastructure exists, needs pattern matching verification  
**Gap Type**: 🔧 PARTIAL

#### Scenario 2: Filter Rule Syntax
```bash
# Upstream
rsync -av --filter='+ *.txt' --filter='- *' source/ dest-upstream/

# Oc-rsync
oc-rsync -av --filter='+ *.txt' --filter='- *' source/ dest-ours/
```

**Expected**: Same as include/exclude  
**Actual**: Filter parsing exists, needs testing  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Per-Directory Rules
```bash
# Create .rsync-filter in subdirectories
echo '- *.tmp' > source/subdir/.rsync-filter

# Upstream
rsync -av --filter='dir-merge .rsync-filter' source/ dest-upstream/

# Oc-rsync
oc-rsync -av --filter='dir-merge .rsync-filter' source/ dest-ours/

# Behavior: Per-directory rules applied
```

**Expected**: .rsync-filter rules merged at each directory level  
**Actual**: Dir-merge parsing exists, needs testing  
**Gap Type**: 🔧 PARTIAL

---

## 8. MESSAGES & EXIT CODES GROUP

### Status: 🔧 PARTIAL (Infrastructure exists, alignment needed)

### Implementation Status

**What Exists**:
- `crates/core/src/message/`: Message formatting with trailers
- `Role` enum: Sender, Receiver, Generator, Server, Client, Daemon
- Error trailer format: `(code N) at <repo-rel-path>:<line> [<role>=3.4.1-rust]`
- `strings.rs`: Centralized message strings

**Parity Validation Needed**:

#### Message Alignment

Need to compare all error/warning/info messages with upstream:

1. **Sparse errors**: "failed to seek in sparse file"
2. **Permission errors**: "cannot change ownership/permissions"
3. **Protocol errors**: "protocol mismatch", "unknown message tag"
4. **Daemon errors**: "secrets file not secure", "access denied"
5. **Connection errors**: "connection refused", "connection reset"
6. **Checksum errors**: "checksum mismatch"

**Gap Type**: 🔧 PARTIAL - Infrastructure exists, wording needs alignment

#### Exit Code Alignment

Need to map all exit codes to upstream:

- `0`: Success
- `1`: Syntax or usage error
- `2`: Protocol incompatibility
- `3`: Errors selecting input/output files
- `4`: Requested action not supported
- `5`: Error starting client-server protocol
- `6`: Daemon unable to append to log-file
- `10`: Error in socket I/O
- `11`: Error in file I/O
- `12`: Error in rsync protocol data stream
- `13`: Errors with program diagnostics
- `20`: Received SIGUSR1 or SIGINT
- `21`: Some error returned by waitpid()
- `22`: Error allocating core memory buffers
- `23`: Partial transfer due to error
- `24`: Partial transfer due to vanished source files
- `25`: The --max-delete limit stopped deletions
- `30`: Timeout in data send/receive
- `35`: Timeout waiting for daemon connection

**Gap Type**: 🔧 PARTIAL - Exit codes need systematic verification

---

## 9. LOGGING & OUTPUT GROUP

### Status: 🔧 PARTIAL (Infrastructure exists, format alignment needed)

### Implementation Status

**What Exists**:
- `crates/logging/`: Message sinks
- `--msgs2stderr`: Stderr routing
- `--info`, `--debug`: Log level control
- `--out-format`: Output templating

**Parity Validation Needed**:

#### Scenario 1: Info Flags (--info)
```bash
# Upstream
rsync -av --info=progress2,stats source/ dest-upstream/

# Oc-rsync
oc-rsync -av --info=progress2,stats source/ dest-ours/

# Compare output format
```

**Expected**: Progress and stats output matching upstream  
**Actual**: Info parsing exists, output format needs alignment  
**Gap Type**: 🔧 PARTIAL

#### Scenario 2: Debug Flags (--debug)
```bash
# Upstream
rsync -av --debug=ALL source/ dest-upstream/

# Oc-rsync
oc-rsync -av --debug=ALL source/ dest-ours/

# Compare debug output
```

**Expected**: Debug output matching upstream categories  
**Actual**: Debug parsing exists, output needs alignment  
**Gap Type**: 🔧 PARTIAL

#### Scenario 3: Output Format (--out-format)
```bash
# Upstream
rsync -av --out-format='%n %l %b' source/ dest-upstream/

# Oc-rsync
oc-rsync -av --out-format='%n %l %b' source/ dest-ours/

# Compare format string expansion
```

**Expected**: Format specifiers expanded identically  
**Actual**: Template parsing exists, expansion needs verification  
**Gap Type**: 🔧 PARTIAL

---

## SUMMARY

### By Status

- **✅ COMPLETE**: Daemon basic, Protocol negotiation, Message infrastructure
- **🔧 PARTIAL**: Sparse (validation needed), Compression, Metadata, Delete/Backup, Filters, Logging
- **❌ MISSING**: SSH integration (infrastructure complete, wiring needed)
- **❓ UNKNOWN**: Many option interactions need explicit testing

### Priority Order (Mission Brief Phase Order)

1. **Phase 2**: Sparse semantics - Complete validation, add interaction tests
2. **Phase 3**: Compression group - Test all levels/choices/interactions
3. **Phase 4**: Metadata group - Test all preservation modes, ACL/xattr edge cases
4. **Phase 5**: Delete/Backup group - Test all timing modes, backup mechanisms
5. **Phase 6**: Daemon/Protocol group - Test all access control, protocol versions
6. **Phase 7**: Messages/Exit codes - Align wording, verify all exit scenarios

### Test Coverage Gaps

Need interop test harness that:
1. Builds upstream rsync 3.0.9, 3.1.3, 3.4.1
2. Runs identical scenarios with both binaries
3. Compares: exit codes, stdout/stderr (normalized), filesystem state
4. Tests: sparse, compression, metadata, delete/backup, daemon, filters
5. Validates: message wording, exit codes, option interactions

---

## NEXT STEPS

### Immediate Actions

1. **Create interop harness**: `tools/interop/harness.sh`
   - Download/build upstream versions
   - Run scenario matrix
   - Compare outcomes

2. **Sparse validation**: Add upstream comparison to existing tests
   - Measure blocks allocated
   - Verify hole layout
   - Test all interactions (--inplace, --append*, --partial*)

3. **Compression testing**: Add protocol-level compression tests
   - Verify negotiation
   - Test levels 1-9
   - Test algorithm selection
   - Test interactions

4. **Metadata testing**: Add preservation verification tests
   - Compare stat output
   - Verify ACLs (getfacl)
   - Verify xattrs (getfattr)
   - Test --chmod directives

5. **Delete/Backup testing**: Add timing and mechanism tests
   - Verify deletion timing
   - Verify backup file creation
   - Test --max-delete limit
   - Test --backup-dir structure

### Documentation

- Keep this document updated as gaps are closed
- Mark each scenario ✅ when parity verified
- Document any intentional divergences (⚠️)
- Track test coverage for each feature group

---

**Analysis Complete**: 2025-11-28  
**Next Review**: After Phase 2 (Sparse) completion
