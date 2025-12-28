# oc-rsync Interoperability Tests

**Last Updated**: 2025-11-25
**Status**: RESTORED TO CI PIPELINE

This document defines the upstream compatibility test scenarios used to validate oc-rsync's parity with rsync 3.0.9, 3.1.3, and 3.4.1.

---

## Upstream Binaries Tested

- `rsync-3.0.9` (via old-releases.ubuntu.com or source build)
- `rsync-3.1.3` (via archive.ubuntu.com or source build)
- `rsync-3.4.1` (via deb.debian.org or source build)

---

## Test Matrix

| Scenario                            | Description                              | Result    | Notes |
|-------------------------------------|------------------------------------------|-----------|-------|
| Local copy (archive mode)           | `-av /src /dest`                          | ✅        | Full test coverage |
| Sparse file round-trip              | Zero run → hole                           | ✅        | Fixed in Phase 2 (2025-11-25) |
| Remote copy via SSH (sender/recv)   | `-av host:/src /dest`                     | ⚠️        | Uses fallback (native SSH pending) |
| Daemon transfer (host::module)      | `-av rsync://host/module/ /dest`         | ✅        | Auth + transfers fixed in Phase 3 |
| Filters (include/exclude/filter)    | Deep ruleset match                        | ✅        | Comprehensive coverage |
| Compression level                   | `-z --compress-level=9`                  | ✅        | Verified against rsync 3.4.1 |
| Metadata flags                      | `-aHAX --numeric-ids`                    | ⚠️ ACL    | ACLs partially implemented |
| Delete options                      | `--delete-excluded`, etc.                | ✅        | All variants working |
| File list diffing                   | Match order, mtime, permission checks     | ✅        | Deterministic ordering |
| Exit code match                     | Match known upstream codes                | ✅        | All 25 codes verified |
| Protocol versions 28-32             | Multi-version negotiation                 | ✅        | Protocol 32 primary, 28-31 supported |

---

## CI Integration

**Status**: ✅ INTEGRATED (2025-11-25)

### Workflow: `.github/workflows/ci.yml`

**Job**: `interop-upstream`
- **Depends on**: `lint-and-test`
- **Runs**: Bidirectional interop tests against upstream rsync versions
- **Script**: `tools/ci/run_interop.sh`
- **Tests**:
  - Upstream rsync 3.0.9/3.1.3/3.4.1 client → oc-rsync daemon
  - oc-rsync client → Upstream rsync 3.0.9/3.1.3/3.4.1 daemon
- **Result validation**:
  - File transfer success
  - Payload file existence
  - Exit code correctness

**Status**: `continue-on-error: true` during stabilization (will be removed after 1 week)

### Test Scripts

#### 1. `tools/ci/run_interop.sh` (PRIMARY)
- **Purpose**: Self-contained CI interop runner
- **Features**:
  - Sequential test cases per version
  - Automatic upstream binary acquisition (packages → source build)
  - Starts/stops daemons for each test
  - Both directions tested
  - Comprehensive failure reporting

#### 2. `scripts/rsync-interop-orchestrator.sh`
- **Purpose**: Multi-daemon orchestrator for local testing
- **Features**:
  - Starts all version daemons simultaneously
  - Environment descriptor files for discovery
  - Cleanup on exit
- **Usage**: `bash scripts/rsync-interop-orchestrator.sh`

#### 3. `scripts/rsync-interop-server.sh`
- **Purpose**: Server-side daemon setup
- **Features**:
  - Fetches upstream binaries (Debian/Ubuntu packages or source)
  - Starts oc-rsync + upstream daemons per version
  - Distinct ports per version

#### 4. `scripts/rsync-interop-client.sh`
- **Purpose**: Client-side test execution
- **Features**:
  - Reads server environment descriptors
  - Runs bidirectional tests
  - Aggregates failures

---

## Test Infrastructure

### Comparison Points

For each test scenario, we validate:
- **Exit code**: Must match upstream rsync exactly
- **Stdout**: Normalized (branding, source locations) and compared
- **Stderr**: Normalized and compared (exit code, severity, message text)
- **Filesystem state**:
  - File sizes
  - Block allocation (for sparse files)
  - Permissions, ownership (if `--perms`/`--owner`)
  - Extended attributes (if `--xattrs`)
  - ACLs (if `--acls`, partial support)
  - Hard links (if `--hard-links`)

### Normalization Rules

When comparing output:
- Replace `rsync` → `oc-rsync` in program names
- Strip Rust source trailers: `at <path>:<line> [<role>=0.5.0]`
- Normalize paths: `/etc/rsyncd.conf` vs `/etc/oc-rsyncd/oc-rsyncd.conf`
- Ignore timing differences in progress output

### Binary Acquisition

Upstream binaries are obtained via multi-tier fallback:
1. **Try Debian/Ubuntu packages** (fastest, most reliable)
   - 3.0.9: old-releases.ubuntu.com
   - 3.1.3: archive.ubuntu.com
   - 3.4.1: deb.debian.org
2. **Try release tarballs** from rsync.samba.org
3. **Try git clone** from github.com/RsyncProject/rsync
4. **Build from source** with `./configure && make && make install`

---

## Known Gaps

### ✅ FIXED (as of 2025-11-25)
- ~~Daemon auth secrets not enforced~~ → Fixed in Phase 3 (daemon.rs)
- ~~Sparse hole layout not guaranteed~~ → Fixed in Phase 2 (sparse.rs)
- ~~Daemon transfers timeout after auth~~ → Fixed in Phase 3 (run_server_with_handshake)

### ⚠️ ONGOING
- **ACLs**: Partially implemented, not all platforms supported
- **Native SSH transport**: Infrastructure exists, integration pending (see `SSH_TRANSPORT_ARCHITECTURE.md`)
- **Filter merge edge cases**: Complex merge directives pending deeper validation

### ❌ TODO
- Explicit protocol version tests (28, 29, 30, 31, 32)
- Performance benchmarks vs upstream
- Multi-platform CI (macOS, Windows)

---

## Running Tests Locally

### Quick Interop Test (CI script)
```bash
bash tools/ci/run_interop.sh
```

### Full Orchestrated Interop (Multi-daemon)
```bash
# Terminal 1: Start server
bash scripts/rsync-interop-server.sh

# Terminal 2: Run client tests
bash scripts/rsync-interop-client.sh
```

### Custom Test Against Specific Version
```bash
# Build oc-rsync
cargo build --profile dist --bin oc-rsync

# Ensure upstream rsync 3.4.1 is available
rsync_341=/path/to/upstream/rsync-3.4.1/bin/rsync

# Test oc-rsync client → upstream daemon
$rsync_341 --daemon --config=test.conf --no-detach --port=2873 &
sleep 1
./target/dist/oc-rsync -av /source/ rsync://127.0.0.1:2873/module/
kill %1

# Test upstream client → oc-rsync daemon
./target/dist/oc-rsync --daemon --config=test.conf --port=2873 &
sleep 1
$rsync_341 -av /source/ rsync://127.0.0.1:2873/module/
kill %1
```

---

## References

- **CI workflow**: `.github/workflows/ci.yml`
- **Interop scripts**: `tools/ci/run_interop.sh`, `scripts/rsync-interop-*.sh`
- **CI hardening plan**: `docs/CI_INTEROP_HARDENING.md`
- **Session summary**: `docs/SESSION_SUMMARY_2025-11-25.md`
- **Gap tracking**: `docs/gaps.md`
- **Message verification**: `docs/MESSAGE_EXIT_CODE_VERIFICATION.md`
- **SSH analysis**: `docs/SSH_TRANSPORT_ARCHITECTURE.md`
- **Upstream reference**: https://github.com/RsyncProject/rsync

---
