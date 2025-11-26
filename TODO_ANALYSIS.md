# TODO Task Analysis for rsync Project

**Generated**: 2025-11-25
**Workspace Status**: All 2796 tests passing ✅
**Windows Build**: Fixed ✅

---

## Executive Summary

- **Total Tasks Identified**: 19
- **Completed**: 5 (26%)
- **Blocked**: 3 (waiting on daemon tests fix)
- **Pending**: 11 (58%)
- **Critical Issues**: 1 (daemon tests module not running)

---

## ✅ COMPLETED TASKS

### 1. Fix compilation error in delta.rs
- **Status**: ✅ Complete
- **Commit**: e3506029
- **Description**: Fixed format string interpolation error
- **Impact**: Build compilation restored

### 2. Wire core::server to daemon module handler
- **Status**: ✅ Complete
- **Commit**: e3506029
- **Location**: `crates/daemon/src/daemon/sections/module_access.rs:373-452`
- **Description**:
  - Implemented ServerConfig construction from module metadata
  - Determined role (Generator vs Receiver) based on module.read_only flag
  - Wired up TcpStream I/O to run_server_stdio
  - Added error handling and logging
- **Impact**: Daemon can now route file transfers through core server implementation

### 3. Add ServerConfig tests for daemon mode
- **Status**: ✅ Complete
- **Commit**: 923dd0b8
- **Location**: `crates/core/src/server/tests.rs`
- **Tests Added**:
  - `config_accepts_empty_flag_string_with_args` - Daemon mode uses empty flags
  - `config_receiver_role_with_module_path` - Receiver role configuration
  - `config_generator_role_with_module_path` - Generator role configuration
  - `config_preserves_role_for_daemon_transfers` - Role preservation verification
- **Impact**: ServerConfig validated for daemon use cases

### 4. Fix all workspace test failures
- **Status**: ✅ Complete
- **Commit**: 0247cba1
- **Fixes**:
  - Fixed `fallback_binary_available_respects_path_changes` - Added cache clearing
  - Fixed PATH semantics - Distinguished unset vs empty PATH (Unix execvp behavior)
  - Renamed test to `fallback_binary_candidates_search_cwd_when_path_empty`
- **Impact**: Full test suite (2796 tests) now passes

### 5. Fix Windows build errors
- **Status**: ✅ Complete
- **Commits**: 8ad03134, afa5f593
- **Fixes**:
  - Made `std::path::Path` import conditional on unix (protocol/wire/file_entry.rs)
  - Added Windows stub for `daemon_mode_arguments` (returns None)
- **Impact**: Windows CI builds will succeed

---

## 🔴 CRITICAL ISSUE

### Daemon Tests Module Not Running

**Severity**: HIGH
**Documentation**: `crates/daemon/DAEMON_TESTS_TODO.md`

**Problem**:
- The daemon crate has **189 test files** in `src/tests/chunks/`
- These tests have **NEVER been executed** because the tests module was never wired up in `src/lib.rs`
- This represents a massive gap in test coverage

**Impact**:
- Daemon functionality regressions may be undetected
- New features (like server transfer wiring) lack integration test coverage
- CI passes while daemon-specific functionality could be broken

**Root Causes**:
1. `src/lib.rs` does not contain `#[cfg(test)] mod tests;`
2. Missing imports when tests module is enabled:
   - `branding` module
   - `ModuleDefinition` type
   - `HostPattern` type
   - Other daemon-internal types
3. Incorrect filename reference:
   - Current: `delegate_system_daemon_fallback_env_triggers_delegation.rs`
   - Correct: `delegate_system_rsync_daemon_fallback_env_triggers_delegation.rs`

---

## 🔲 HIGH PRIORITY TASKS

### 6. Fix daemon tests module wiring ⚠️ BLOCKS OTHERS

**Status**: 🔲 Pending
**Priority**: CRITICAL
**Blocks**: Tasks #7, #8, #9
**Documentation**: `crates/daemon/DAEMON_TESTS_TODO.md`

**Steps**:
1. Add `#[cfg(test)] mod tests;` to `daemon/src/lib.rs`
2. Fix imports in `src/tests.rs` or `src/tests/support.rs`:
   ```rust
   use crate::daemon::ModuleDefinition;
   use core::branding;
   // ... other missing imports
   ```
3. Fix filename mismatch in `src/tests.rs`:
   - Line 42: Update include! path
4. Compile and verify all 189+ tests pass

**Estimated Effort**: 2-4 hours
**Risk**: May uncover failing tests that reveal bugs

---

### 7. Integration tests for daemon receiver role (client push)

**Status**: 🔲 Blocked (by #6)
**Priority**: High
**File**: `daemon/src/tests/chunks/daemon_receiver_accepts_file_push.rs`

**Description**:
- Tests client push to writable daemon module
- Verifies authentication challenge flow
- Confirms ServerConfig with Receiver role
- Status: Written but not yet running

**Scope**:
- ✅ File created
- 🔲 Module wiring needed
- 🔲 Execute and verify

---

### 8. Integration tests for daemon generator role (client pull)

**Status**: 🔲 Blocked (by #6)
**Priority**: High
**File**: `daemon/src/tests/chunks/daemon_generator_accepts_file_pull.rs`

**Description**:
- Tests client pull from read-only daemon module
- No authentication required for read-only modules
- Confirms ServerConfig with Generator role
- Status: Written but not yet running

**Scope**:
- ✅ File created
- 🔲 Module wiring needed
- 🔲 Execute and verify

---

### 9. Test daemon authentication with actual file transfers

**Status**: 🔲 Blocked (by #6, #7, #8)
**Priority**: High
**Depends on**: Daemon tests running + basic integration tests passing

**Scope**:
- End-to-end authentication flow
- Challenge-response mechanism
- Secrets file validation
- Invalid credential rejection
- Module access control

**Estimated Effort**: 4-6 hours

---

### 10. Verify bandwidth limiting in daemon mode

**Status**: 🔲 Pending
**Priority**: Medium
**Location**: Module `bandwidth_limit` configuration

**Scope**:
- Test daemon-level bandwidth caps
- Test module-level bandwidth overrides
- Verify bandwidth limiting during actual transfers
- Test burst handling

**Related Code**:
- `crates/daemon/src/daemon/sections/module_definition.rs`
- `crates/bandwidth/`

**Estimated Effort**: 3-4 hours

---

### 11. Test module timeout application

**Status**: 🔲 Pending
**Priority**: Medium
**Location**: Module `timeout` configuration

**Scope**:
- Verify timeout enforcement during transfers
- Test timeout expiry behavior
- Confirm graceful connection termination

**Estimated Effort**: 2-3 hours

---

### 12. Test connection guard enforcement (max_connections)

**Status**: 🔲 Pending
**Priority**: Medium
**Location**: Module `max_connections` setting

**Scope**:
- Verify connection limits work correctly
- Test concurrent connection handling
- Confirm connection rejection when limit reached
- Test connection counting accuracy

**Estimated Effort**: 2-3 hours

---

### 13. End-to-end tests with real rsync clients

**Status**: 🔲 Pending
**Priority**: Medium-High
**Scope**: Interop testing with upstream rsync

**Target Versions**:
- rsync 3.0.9
- rsync 3.1.3
- rsync 3.4.1

**Test Scenarios**:
- Client push to daemon (receiver role)
- Client pull from daemon (generator role)
- Authentication flows
- Protocol negotiation (versions 28-32)
- Module listing

**Location**: Integration test suite
**Estimated Effort**: 6-8 hours

---

## 🔲 MEDIUM PRIORITY TASKS

### 14. Expand read-only module test coverage

**Status**: 🔲 Pending
**Priority**: Medium
**Current**: Basic test exists (#8)

**Additional Scenarios**:
- Large file transfers
- Directory tree transfers
- Symbolic link handling
- Permission preservation
- Multiple concurrent clients

**Estimated Effort**: 3-4 hours

---

### 15. Expand read-write module test coverage

**Status**: 🔲 Pending
**Priority**: Medium
**Current**: Basic test exists (#7)

**Additional Scenarios**:
- File uploads with various attributes
- Directory creation
- File overwrites
- Partial transfers (--partial)
- Checksum verification
- Multiple concurrent uploads

**Estimated Effort**: 3-4 hours

---

## 🔲 OPTIONAL REFINEMENTS

### 16. Add structured logging for daemon transfers

**Status**: 🔲 Pending
**Priority**: Low
**Location**: `daemon/src/daemon/sections/module_access.rs`

**Current State**:
- Basic info logging exists (lines 373-452)
- Logs transfer start and completion

**Enhancements**:
- Structured log events (JSON/key-value)
- Transfer duration tracking
- Bytes transferred metrics
- Success/failure categorization
- Client identification

**Estimated Effort**: 2-3 hours

---

### 17. Document daemon-to-server integration

**Status**: 🔲 Pending
**Priority**: Low
**Location**: `CLAUDE.md`

**Content to Add**:
- How daemon routes to core::server
- ServerRole determination logic
- TcpStream handling architecture
- Error propagation flow
- Module-to-ServerConfig mapping

**Estimated Effort**: 1-2 hours

---

### 18. Metrics/telemetry for daemon transfers

**Status**: 🔲 Pending
**Priority**: Low
**Implementation**: Optional feature flag

**Metrics to Track**:
- Transfer duration
- Bytes transferred
- Success/failure rates
- Authentication attempts
- Module access patterns
- Protocol version distribution

**Estimated Effort**: 4-6 hours

---

### 19. Performance testing for daemon under load

**Status**: 🔲 Pending
**Priority**: Low
**Scope**: Concurrent connection handling

**Test Scenarios**:
- Multiple simultaneous clients
- Large file transfers
- Small file transfers (many files)
- Mixed workloads
- Connection churn

**Tools Needed**:
- Load testing framework
- Benchmarking harness
- Performance profiling

**Estimated Effort**: 8-10 hours

---

## Immediate Next Steps (Priority Order)

1. **Fix daemon tests module** (#6) - CRITICAL, unblocks everything
2. **Verify new integration tests run** (#7, #8) - Depends on #6
3. **Test daemon authentication** (#9) - High priority for security
4. **Test bandwidth limiting** (#10) - Important functionality validation
5. **Document daemon integration** (#17) - Helps future contributors

---

## Long-Term Roadmap

### Phase 1: Foundation (Current)
- ✅ Wire core::server to daemon
- ✅ Basic ServerConfig tests
- 🔲 Enable all daemon tests (#6)
- 🔲 Basic integration tests (#7, #8)

### Phase 2: Core Functionality
- 🔲 Authentication testing (#9)
- 🔲 Bandwidth limiting (#10)
- 🔲 Timeout handling (#11)
- 🔲 Connection limits (#12)

### Phase 3: Interoperability
- 🔲 Upstream rsync interop (#13)
- 🔲 Extended test coverage (#14, #15)

### Phase 4: Production Readiness
- 🔲 Structured logging (#16)
- 🔲 Metrics/telemetry (#18)
- 🔲 Performance testing (#19)
- 🔲 Documentation (#17)

---

## Risk Assessment

### High Risk
- **Daemon tests not running**: Could hide critical bugs ⚠️
- **Authentication untested**: Security-critical functionality 🔴

### Medium Risk
- **Bandwidth limiting untested**: Performance feature may be broken
- **Interop untested**: May not work with real rsync clients

### Low Risk
- **Missing metrics**: Nice-to-have, not critical
- **Documentation gaps**: Can be filled incrementally

---

## Notes

- All placeholders (TODO/FIXME/XXX) have been eliminated from codebase ✅
- no_placeholders check is enforced via `tools/no_placeholders.sh`
- Current test suite: 2796 tests, all passing
- Windows build compatibility maintained
- Code follows upstream rsync semantics where applicable

---

**Last Updated**: 2025-11-25
**Next Review**: After daemon tests module is fixed (#6)
