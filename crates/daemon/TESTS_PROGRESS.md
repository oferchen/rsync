# Daemon Tests Module - Progress Report

**Status**: Partially Complete (estimated 60% done)
**Date**: 2025-11-25

## ✅ Completed

1. ✅ Added `#[cfg(test)] mod tests;` to daemon/src/lib.rs
2. ✅ Fixed filename mismatch: `delegate_system_daemon_fallback` → `delegate_system_rsync_daemon_fallback`
3. ✅ Added protocol imports to tests.rs (MessageCode, MessageFrame, ProtocolVersion)
4. ✅ Added core imports (Brand, fallback envs)
5. ✅ Made visible for tests:
   - `ModuleDefinition` struct and all fields
   - `HostPattern` enum
   - `ModuleRuntime` struct and fields
   - `TestSecretsEnvOverride` struct
   - `TEST_SECRETS_CANDIDATES` thread_local
   - `TEST_SECRETS_ENV` thread_local
   - `advertised_capability_lines` function
   - `HANDSHAKE_ERROR_PAYLOAD` const
   - `FEATURE_UNAVAILABLE_EXIT_CODE` const
   - `clap_command` function
   - `permits` method on ModuleDefinition

## 🔲 Remaining Work

**289 compilation errors remain** - all are visibility issues.

The following items need to be made `pub(crate)`:

### Functions
- `apply_module_bandwidth_limit`
- `clear_test_hostname_overrides`
- `configured_fallback_binary`
- `default_secrets_path_if_present`
- `first_existing_config_path`
- `format_bandwidth_rate`
- `format_connection_status`
- `legacy_daemon_greeting`
- `log_module_bandwidth_change`
- `module_peer_hostname`
- `open_log_sink`
- `parse_auth_user_list`
- `parse_boolean_directive`
- `parse_config_modules`
- `parse_max_connections_directive`
- `parse_numeric_identifier`
- `parse_refuse_option_list`
- `parse_timeout_seconds`
- `read_trimmed_line`
- `render_help`
- `sanitize_module_identifier`
- And approximately 20-30 more...

### Types
- `AddressFamily` enum
- `ConnectionLimiter` struct
- `ModuleConnectionError` enum
- `ProgramName` enum
- `RuntimeOptions` struct
- And several more...

### Constants
- `BRANDED_CONFIG_ENV`
- `LEGACY_CONFIG_ENV`
- And several more...

## Approach to Complete

### Option 1: Manual (Tedious but Safe)
Go through each compilation error and add `pub(crate)` to the relevant item.

Estimated time: 2-3 hours

### Option 2: Automated Script
Create a script that:
1. Extracts all error messages mentioning "not accessible"
2. Parses out the function/type/const names
3. Finds their definitions using grep
4. Adds `pub(crate)` to each definition

Estimated time: 1 hour to write script + 30 min to verify

### Option 3: Blanket Approach
Make all private items in daemon.rs and its sections `pub(crate)` by default.

**Risk**: Exposes internals that shouldn't be accessible
**Benefit**: Fast, comprehensive

## Files Modified So Far

- `crates/daemon/src/lib.rs` - Added tests module
- `crates/daemon/src/tests.rs` - Fixed filename, added imports
- `crates/daemon/src/tests/support.rs` - Added daemon type imports
- `crates/daemon/src/daemon.rs` - Made 2 constants pub(crate)
- `crates/daemon/src/daemon/module_state.rs` - Made ModuleDefinition, ModuleRuntime, test helpers pub(crate)
- `crates/daemon/src/daemon/sections/config_helpers.rs` - Made HostPattern pub(crate)
- `crates/daemon/src/daemon/sections/delegation.rs` - Made advertised_capability_lines pub(crate)
- `crates/daemon/src/daemon/sections/cli_args.rs` - Made clap_command pub(crate)

## Impact

Once complete, this will:
- Enable **189 daemon test files** to run for the first time ever
- Reveal any hidden bugs in daemon implementation
- Provide proper test coverage for daemon functionality
- Unblock the 2 new integration tests for file transfers

## Recommendation

**Continue with Option 1 (Manual)** to maintain code quality and intentional visibility control.

Each item should be reviewed to ensure it's appropriate to expose to tests.

Alternative: Pair-program or batch-fix in 30-item chunks to make progress manageable.
