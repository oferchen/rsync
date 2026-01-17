# Remote-to-Remote Transfer Design

## Overview

Enable transfers between two remote hosts via the local machine as a proxy:

```
rsync user@source:/path user@dest:/path
```

The local machine spawns two SSH connections and relays rsync protocol messages between them.

## Architecture

### Current State

The current SSH transfer (`ssh_transfer.rs`) works like this:

```
Local (Client)              Remote (Server)
┌─────────────────┐  SSH   ┌─────────────────┐
│ Generator/      │◄──────►│ rsync --server  │
│ Receiver        │        │ (opposite role) │
└─────────────────┘        └─────────────────┘
```

The local side runs server infrastructure in the opposite role of the remote.

### Remote-to-Remote Design

For remote-to-remote, we become a pure relay:

```
Source Host                  Local (Proxy)                 Dest Host
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│ rsync --server  │   SSH   │                 │   SSH   │ rsync --server  │
│ --sender        │◄───────►│  Protocol       │◄───────►│ (receiver)      │
│ (generator)     │         │  Relay          │         │                 │
└─────────────────┘         └─────────────────┘         └─────────────────┘
```

The local machine:
1. Spawns SSH to source with `rsync --server --sender` (generator role)
2. Spawns SSH to destination with `rsync --server` (receiver role)
3. Relays protocol messages between the two connections

## Data Flow

### Phase 1: Protocol Negotiation

Both sides negotiate protocol versions independently:

```
1. Source sends version greeting → Proxy reads and stores
2. Proxy relays greeting → Destination
3. Destination sends version greeting → Proxy reads and stores
4. Proxy relays greeting → Source
5. Both connections now have negotiated protocols
```

The proxy must ensure both sides agree on a compatible protocol version. If versions differ, select the minimum common version.

### Phase 2: Transfer Execution

The rsync protocol flows in phases:
1. **File list phase**: Source sends file list → Proxy relays → Destination
2. **Checksum phase**: Destination sends checksums → Proxy relays → Source
3. **Data phase**: Source sends deltas → Proxy relays → Destination
4. **Completion phase**: Both sides exchange statistics

### Relay Strategy

The relay operates in a bidirectional streaming mode:

```rust
// Simplified relay loop
loop {
    select! {
        data = source.read() => dest.write(data),
        data = dest.read() => source.write(data),
    }
}
```

## Key Components

### 1. RemoteToRemoteTransfer

New struct in `crates/core/src/client/remote/`:

```rust
pub struct RemoteToRemoteTransfer {
    source_conn: SshConnection,
    dest_conn: SshConnection,
    config: ClientConfig,
}

impl RemoteToRemoteTransfer {
    pub fn new(
        source: ParsedRemoteOperand,
        dest: ParsedRemoteOperand,
        config: &ClientConfig,
    ) -> Result<Self, ClientError>;

    pub fn run(self) -> Result<ClientSummary, ClientError>;
}
```

### 2. ProtocolRelay

Handles bidirectional protocol message relay:

```rust
pub struct ProtocolRelay<S, D> {
    source: S,
    dest: D,
    source_protocol: ProtocolVersion,
    dest_protocol: ProtocolVersion,
}

impl<S: Read + Write, D: Read + Write> ProtocolRelay<S, D> {
    pub fn relay_until_complete(self) -> Result<RelayStats, Error>;
}
```

### 3. Modified determine_transfer_role

Currently returns an error for remote-to-remote. Change to:

```rust
match (has_remote_source, dest_is_remote) {
    (true, true) => {
        // Remote-to-remote: return new Proxy role
        Ok((RemoteRole::Proxy, sources, destination))
    }
    // ... existing cases
}
```

### 4. New RemoteRole Variant

```rust
pub enum RemoteRole {
    Sender,    // Local → Remote (push)
    Receiver,  // Remote → Local (pull)
    Proxy,     // Remote → Remote (relay)
}
```

## Deadlock Prevention

The main risk is deadlock when both streams block on read/write simultaneously.

### Strategy: Async I/O with Select

Use tokio or async-std to handle bidirectional streams:

```rust
async fn relay(
    source: &mut TcpStream,
    dest: &mut TcpStream,
) -> io::Result<()> {
    let mut source_buf = [0u8; 8192];
    let mut dest_buf = [0u8; 8192];

    loop {
        tokio::select! {
            result = source.read(&mut source_buf) => {
                let n = result?;
                if n == 0 { break; }
                dest.write_all(&source_buf[..n]).await?;
            }
            result = dest.read(&mut dest_buf) => {
                let n = result?;
                if n == 0 { break; }
                source.write_all(&dest_buf[..n]).await?;
            }
        }
    }
    Ok(())
}
```

### Alternative: Thread-Based Relay

If async is not desired, use two threads:

```rust
fn relay_sync(
    mut source: SshConnection,
    mut dest: SshConnection,
) -> io::Result<()> {
    let (source_read, source_write) = source.split();
    let (dest_read, dest_write) = dest.split();

    let s2d = thread::spawn(move || {
        io::copy(&mut source_read, &mut dest_write)
    });

    let d2s = thread::spawn(move || {
        io::copy(&mut dest_read, &mut source_write)
    });

    s2d.join()??;
    d2s.join()??;
    Ok(())
}
```

## Protocol Version Handling

Both remote hosts may support different protocol versions. The proxy must:

1. Negotiate with source to get `source_version`
2. Negotiate with destination to get `dest_version`
3. Use `min(source_version, dest_version)` as the relay protocol

If the versions are incompatible (e.g., one is v30 and other is v27), the transfer fails with a clear error.

## Error Handling

### Connection Errors

- If source connection fails: abort with "failed to connect to source"
- If destination connection fails: abort with "failed to connect to destination"
- If either connection drops mid-transfer: abort with "connection lost to [source|destination]"

### Protocol Errors

- Protocol mismatch: "source and destination have incompatible protocol versions"
- Relay error: "protocol relay error: [details]"

### Statistics

The proxy tracks:
- Bytes relayed source → destination
- Bytes relayed destination → source
- Transfer time
- Files transferred (parsed from protocol messages if possible)

## Implementation Plan

### Phase 1: Infrastructure
1. Add `RemoteRole::Proxy` variant
2. Modify `determine_transfer_role` to return `Proxy` for r2r
3. Create `remote_to_remote.rs` module stub

### Phase 2: Connection Management
1. Implement dual SSH connection spawning
2. Add protocol version negotiation for both connections
3. Implement version compatibility check

### Phase 3: Relay Implementation
1. Implement basic bidirectional relay (thread-based first)
2. Add deadlock prevention
3. Add statistics tracking

### Phase 4: Integration
1. Wire into `run.rs` dispatch
2. Add error handling and recovery
3. Add progress reporting

### Phase 5: Testing
1. Unit tests for relay logic
2. Integration tests with mock SSH
3. End-to-end tests with real SSH (if available)

## Limitations

1. **No protocol inspection**: The proxy relays bytes without parsing. It cannot:
   - Report per-file progress
   - Filter files during transfer
   - Modify transfer behavior

2. **Double network overhead**: Data traverses two network hops instead of one direct transfer.

3. **Local machine as bottleneck**: Transfer speed limited by local machine's bandwidth.

## Future Enhancements

1. **Protocol-aware relay**: Parse protocol messages to provide better progress reporting
2. **Compression optimization**: Compress on source, decompress locally, recompress to dest
3. **Parallel streams**: Use multiple connections for higher throughput
4. **Direct connection fallback**: If source and dest can reach each other, facilitate direct transfer
