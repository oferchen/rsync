# Daemon Fallback Behavior

**Date**: 2025-11-25
**Context**: Interop test debugging

## Summary

The oc-rsync daemon has a **default fallback behavior** that delegates incoming connections to the system rsync binary. This is by design for production compatibility, but must be explicitly disabled for interop testing.

## Default Behavior

When oc-rsync daemon receives a connection:
1. Check `OC_RSYNC_DAEMON_FALLBACK` environment variable
2. Check `OC_RSYNC_FALLBACK` environment variable
3. **Default**: Delegate to system `rsync` binary

This is implemented in `crates/daemon/src/daemon/sections/module_parsing.rs`:

```rust
pub(crate) fn configured_fallback_binary() -> Option<OsString> {
    if let Some(selection) = fallback_override(DAEMON_FALLBACK_ENV) {
        return selection.resolve_or_default(OsStr::new(Brand::Upstream.client_program_name()));
    }

    if let Some(selection) = fallback_override(CLIENT_FALLBACK_ENV) {
        return selection.resolve_or_default(OsStr::new(Brand::Upstream.client_program_name()));
    }

    // DEFAULT: Always delegate to "rsync"
    Some(OsString::from(Brand::Upstream.client_program_name()))
}
```

## Why This Design?

The default delegation behavior provides:
1. **Safety**: Production deployments can fall back to system rsync if oc-rsync daemon encounters unsupported features
2. **Gradual migration**: Deployments can test oc-rsync without full feature parity
3. **Compatibility**: Ensures daemon always works, even with missing features

## Disabling Delegation

To force **native handling** (no delegation), set environment variable to a disable value:

```bash
# Any of these will disable delegation:
export OC_RSYNC_DAEMON_FALLBACK=0
export OC_RSYNC_DAEMON_FALLBACK=no
export OC_RSYNC_DAEMON_FALLBACK=false
export OC_RSYNC_DAEMON_FALLBACK=off
```

## Testing Implications

**Interop tests MUST disable delegation** to validate native implementation:

```bash
# Start daemon with native handling
OC_RSYNC_DAEMON_FALLBACK=0 oc-rsync --daemon --config test.conf --port 2873
```

Otherwise, interop tests would just validate that:
- oc-rsync can forward connections to system rsync ✓
- But NOT that oc-rsync natively implements the protocol ✗

## Common Mistake

❌ **Wrong**: Removing environment variable to disable delegation
```bash
# This ENABLES delegation (default behavior)!
unset OC_RSYNC_DAEMON_FALLBACK
oc-rsync --daemon
```

✅ **Right**: Explicitly setting to disable value
```bash
# This DISABLES delegation
OC_RSYNC_DAEMON_FALLBACK=0 oc-rsync --daemon
```

## Environment Variables

| Variable | Scope | Priority | Purpose |
|----------|-------|----------|---------|
| `OC_RSYNC_DAEMON_FALLBACK` | Daemon only | Primary | Controls daemon delegation |
| `OC_RSYNC_FALLBACK` | Client & Daemon | Secondary | Shared fallback control |

**Disable values**: `0`, `no`, `false`, `off` (case-insensitive)
**Enable values**: Any path (e.g., `/usr/bin/rsync`), `auto`, `default`

## Test Configuration

### `tools/ci/run_interop.sh`
```bash
start_oc_daemon() {
    # ...
    OC_RSYNC_DAEMON_FALLBACK=0 \
        "$oc_binary" --daemon --config "$config" --port "$port"
}
```

### `scripts/rsync-interop-server.sh`
```bash
start_oc_daemon() {
    # ...
    OC_RSYNC_DAEMON_FALLBACK=0 \
        "${bin}" --daemon --config "${conf}" --port "${port}"
}
```

## Production Use

In production, the default delegation is **desirable**:
```bash
# Let daemon use fallback if needed
oc-rsync --daemon --config /etc/oc-rsyncd/oc-rsyncd.conf

# Daemon will handle native features, delegate for unsupported ones
```

To force native-only (fail on unsupported features):
```bash
OC_RSYNC_DAEMON_FALLBACK=0 oc-rsync --daemon --config /etc/oc-rsyncd/oc-rsyncd.conf
```

## Related Tests

Tests confirming this behavior:

### `crates/daemon/src/tests/chunks/configured_fallback_binary_defaults_to_rsync.rs`
```rust
#[test]
fn configured_fallback_binary_defaults_to_rsync() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert_eq!(configured_fallback_binary(), Some(OsString::from("rsync")));
}
```

### `crates/daemon/src/tests/chunks/configured_fallback_binary_respects_primary_disable.rs`
```rust
#[test]
fn configured_fallback_binary_respects_primary_disable() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert!(configured_fallback_binary().is_none());
}
```

## Debug Checklist

When debugging daemon delegation issues:

1. ✅ Check daemon logs for "delegating binary session to" message
2. ✅ Verify `OC_RSYNC_DAEMON_FALLBACK` is set to `0` (not just unset)
3. ✅ Check which rsync binary is being invoked (log shows version)
4. ✅ Ensure system rsync binary exists and is executable
5. ✅ Verify test expects native handling, not delegation

## Interop Test Failure Pattern

**Symptom**: Timeout (exit code 30) when upstream client connects to daemon

**Log shows**:
```
oc-rsync info: delegating binary session to 'rsync'
[sender] io timeout after 10 seconds -- exiting
rsync error: timeout in data send/receive (code 30)
```

**Diagnosis**: Daemon delegating to system rsync, which tries to bind same port

**Fix**: Set `OC_RSYNC_DAEMON_FALLBACK=0` in test script

## References

- Implementation: `crates/daemon/src/daemon/sections/module_parsing.rs:561`
- Delegation logic: `crates/daemon/src/daemon/sections/delegation.rs:65`
- Server startup: `crates/daemon/src/daemon/sections/server_runtime.rs:237`
- Test scripts: `tools/ci/run_interop.sh`, `scripts/rsync-interop-server.sh`
- Related doc: `CLAUDE.md` (fallback guardrails section)

---
