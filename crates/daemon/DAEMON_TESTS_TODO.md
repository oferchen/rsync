# Daemon Tests Module Issue

## Problem

The daemon crate has 189 test files in `src/tests/chunks/` that are currently **not being run**.

The test module defined in `src/tests.rs` has never been wired up to `src/lib.rs`, meaning these tests have never been compiled or executed.

## Discovery

While adding integration tests for daemon file transfers, I discovered that:

1. `src/lib.rs` does not contain `mod tests;`
2. Adding `mod tests;` reveals multiple compilation issues:
   - Missing imports (branding, ModuleDefinition, HostPattern, etc.)
   - At least one incorrect filename reference:
     - `delegate_system_daemon_fallback_env_triggers_delegation.rs` should be
     - `delegate_system_rsync_daemon_fallback_env_triggers_delegation.rs`

## New Test Files Ready

Two integration test files have been created and are ready to be activated once the tests module is properly wired:

- `src/tests/chunks/daemon_generator_accepts_file_pull.rs` - Tests read-only module access (client pull scenario)
- `src/tests/chunks/daemon_receiver_accepts_file_push.rs` - Tests writable module access with authentication (client push scenario)

These tests verify that the daemon can properly route file transfers through `core::server::run_server_stdio`.

## Required Fixes

To enable the daemon tests:

1. Add `#[cfg(test)] mod tests;` to `src/lib.rs`
2. Fix imports in `src/tests.rs` or `src/tests/support.rs`:
   - Add `use crate::daemon::ModuleDefinition;` (or similar)
   - Add `use core::branding;`
   - Fix other missing type imports
3. Fix the filename mismatch noted above
4. Verify all 189+ tests compile and pass

## Impact

This is a significant gap in test coverage. The daemon module has extensive test infrastructure that has never been exercised, which means:
- Daemon functionality may have regressions that are undetected
- New features (like the server transfer wiring) lack integration test coverage
- CI may be passing while daemon-specific functionality is broken

## Next Steps

1. Create a dedicated task/issue to fix the daemon tests module setup
2. Once fixed, verify all existing tests pass
3. Enable the two new transfer integration tests
4. Consider adding more end-to-end daemon transfer tests
