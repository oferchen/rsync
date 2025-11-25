# SSH Transport Architecture Analysis

**Date**: 2025-11-25
**Status**: Infrastructure exists, integration pending
**Effort Estimate**: High (5-10 days)

## Summary

Native SSH transport infrastructure is **fully implemented** in `crates/transport/src/ssh` but **not integrated** into the client execution path. Remote transfers currently fall back to system rsync, which works correctly but requires system rsync to be installed.

## Current Architecture

### What Exists ✅

1. **SSH Command Builder** (`crates/transport/src/ssh/builder.rs`):
   - `SshCommand` struct with full builder API
   - User/host/port configuration
   - Remote command arguments
   - Batch mode control
   - Environment variable passing
   - Remote shell specification parsing (`-e/--rsh` compatible)

2. **SSH Connection** (`crates/transport/src/ssh/connection.rs`):
   - `SshConnection` implementing `Read` + `Write`
   - Spawns system `ssh` binary
   - Stdio piping to remote rsync process
   - Proper child process cleanup

3. **Remote Operand Detection** (`crates/engine/src/local_copy/operands.rs`):
   - `operand_is_remote()` function detecting:
     - `rsync://` URLs
     - `host::module` daemon syntax
     - `user@host:path` SSH syntax
     - Correctly ignores Windows drive letters (`C:`)
   - Full test coverage

4. **Integration Tests**:
   - `SshCommand` builder tests
   - IPv6 host handling
   - User/port combinations
   - Remote shell parsing

### What's Missing ❌

1. **Client Integration**:
   - `crates/core/src/client/run.rs` returns `RemoteOperandUnsupported` error
   - No code path to create `SshCommand` from detected remote operand
   - No protocol negotiation over SSH streams

2. **Remote Operand Parsing**:
   - Detection exists, but no parser to extract `user`, `host`, `port`, `path` components
   - Need to handle:
     - `host:path`
     - `user@host:path`
     - `host:port:path`
     - `user@host:port:path`
     - IPv6 literals: `[2001:db8::1]:path`

3. **Protocol Over SSH**:
   - No integration between `SshConnection` and rsync protocol negotiation
   - Need to determine client vs server role based on push/pull
   - Need to handle `--rsync-path` to invoke correct remote binary

4. **File List Exchange**:
   - Engine currently assumes local filesystem access
   - Need to wire protocol messages for remote file list exchange

## Implementation Roadmap

### Phase 1: Remote Operand Parsing (1-2 days)

Add parser to extract components from remote operand:

```rust
// crates/transport/src/ssh/parse.rs or new module
pub struct RemoteOperand {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
}

pub fn parse_remote_operand(operand: &OsStr) -> Result<RemoteOperand, ParseError> {
    // Parse user@host:path syntax
    // Handle IPv6 [::1]:path
    // Extract port from host:port
}
```

### Phase 2: Client SSH Integration (2-3 days)

Modify `crates/core/src/client/run.rs` to handle remote operands natively:

```rust
// Instead of returning RemoteOperandUnsupported:
if operand_is_remote(&operand) {
    let remote = parse_remote_operand(&operand)?;
    let mut ssh_cmd = SshCommand::new(&remote.host);

    if let Some(user) = remote.user {
        ssh_cmd.set_user(user);
    }
    if let Some(port) = remote.port {
        ssh_cmd.set_port(port);
    }

    // Configure remote rsync invocation
    ssh_cmd.push_remote_arg("rsync");
    ssh_cmd.push_remote_arg("--server");
    // ... add other flags

    let connection = ssh_cmd.spawn()?;

    // Run protocol negotiation over connection
    // Execute transfer
}
```

### Phase 3: Protocol Negotiation (1-2 days)

Wire `SshConnection` to existing protocol negotiation:

```rust
// Use existing transport::negotiate_session
let negotiated = negotiate_session(connection)?;

// Determine role (sender vs receiver)
let role = determine_role(is_push_transfer, &operands);

// Run appropriate side of protocol
match role {
    Role::Sender => run_sender_protocol(negotiated, ...),
    Role::Receiver => run_receiver_protocol(negotiated, ...),
}
```

### Phase 4: File List Exchange (2-3 days)

Modify engine to work with remote streams instead of assuming local filesystem:

- Abstract file list source (local walk vs protocol messages)
- Wire protocol file list messages to engine
- Handle both push and pull scenarios
- Maintain existing local copy fast path

### Phase 5: Testing & Validation (1-2 days)

- Integration tests with actual SSH connections
- Interop tests with upstream rsync
- Error handling (connection failures, auth failures, etc.)
- Edge cases (symlinks, permissions, etc.)

## Technical Challenges

### 1. Engine Abstraction

Current `LocalCopyPlan` assumes local filesystem. Need to abstract:
- File list source (local walk vs protocol exchange)
- Metadata access (stat() vs protocol messages)
- Content transfer (local read/write vs delta protocol)

### 2. Push vs Pull

Determine which side is sender/receiver:
- Push: `oc-rsync local user@host:remote` → we're sender
- Pull: `oc-rsync user@host:remote local` → we're receiver
- Both: `oc-rsync user@host1:path user@host2:path` → ??

### 3. --rsync-path Handling

When client connects via SSH, it needs to invoke remote rsync:
- Default: `rsync --server ...`
- With `--rsync-path=/path/to/oc-rsync`: use that instead
- Must forward flags correctly

### 4. Error Propagation

SSH failures need proper error messages:
- Connection refused
- Authentication failure
- Remote binary not found
- Protocol version mismatch

## Current Workaround

The fallback mechanism works well:
```bash
# With system rsync installed:
oc-rsync -arv ~/src user@host:/dest
# → detects remote operand
# → falls back to system rsync
# → forwards --rsync-path and other options
# → works correctly
```

Users can use `OC_RSYNC_FALLBACK` env var to control fallback binary location.

## Recommendation

Given the effort required and that the fallback mechanism works correctly:

**Short-term**: Document the fallback behavior clearly, ensure `--rsync-path` forwarding works perfectly

**Long-term**: Implement native SSH transport as described above when time permits

## References

- Upstream rsync source: `client.c`, `pipe.c`, `main.c`
- Existing infrastructure:
  - `crates/transport/src/ssh/` - complete SSH spawning
  - `crates/transport/src/session/` - protocol negotiation
  - `crates/engine/src/local_copy/` - local transfers
