# Async/Tokio Migration Roadmap

This document outlines the blocking I/O patterns identified across the oc-rsync codebase
and the strategy for converting to async using tokio.

## Executive Summary

| Crate | Blocking Patterns | Priority | Complexity |
|-------|-------------------|----------|------------|
| daemon | 40+ | CRITICAL | High |
| engine | 60+ | HIGH | Very High |
| transport | 15+ | HIGH | Medium |
| core | 10+ | MEDIUM | Medium |

**Total estimated blocking I/O operations: 125+**

---

## Phase 1: Foundation (Feature-Gated)

All async code should be gated behind a feature flag to maintain backwards compatibility:

```toml
[features]
default = []
async = ["tokio", "tokio-util"]
```

### 1.1 Add Dependencies

```toml
# Root Cargo.toml workspace dependencies
[workspace.dependencies]
tokio = { version = "1.45", features = ["rt-multi-thread", "io-util", "net", "fs", "sync", "time", "process"] }
tokio-util = { version = "0.7", features = ["codec", "io"] }
```

---

## Phase 2: Daemon Crate Migration

### Critical Blocking Patterns

#### TCP Listener Accept Loop
**Location:** `crates/daemon/src/daemon/sections/server_runtime.rs:341-525`

```rust
// Current (blocking)
let listener = TcpListener::bind(addr)?;
loop {
    let (stream, peer) = listener.accept()?;  // BLOCKS
    thread::spawn(move || handle_connection(stream, peer));
}

// Target (async)
let listener = tokio::net::TcpListener::bind(addr).await?;
loop {
    let (stream, peer) = listener.accept().await?;
    tokio::spawn(async move { handle_connection(stream, peer).await });
}
```

#### Dual-Stack Listener Polling
**Location:** `crates/daemon/src/daemon/sections/server_runtime.rs:471-518`

Current implementation uses `mpsc::channel` with 100ms polling timeout:
```rust
match rx.recv_timeout(Duration::from_millis(100)) { ... }
```

Target: Use `tokio::select!` for efficient multi-listener waiting:
```rust
tokio::select! {
    result = ipv6_listener.accept() => { /* handle IPv6 */ }
    result = ipv4_listener.accept() => { /* handle IPv4 */ }
}
```

#### Session Detection via peek()
**Location:** `crates/daemon/src/daemon/sections/session_runtime.rs:92-119`

`TcpStream::peek()` has no async equivalent. Strategy:
1. Use `BufReader::fill_buf()` to inspect without consuming
2. Or spawn dedicated detection task with channel communication

#### Thread-Based Reader/Writer Pairs
**Location:** `crates/daemon/src/daemon/sections/delegation.rs:105-113`

```rust
// Current
let reader_thread = thread::spawn(|| forward_client_to_child(...));
let writer_thread = thread::spawn(|| io::copy(&mut child_stdout, &mut downstream));

// Target
let reader_task = tokio::spawn(async { forward_client_to_child(...).await });
let writer_task = tokio::spawn(async { tokio::io::copy(&mut child_stdout, &mut downstream).await });
```

### Synchronization Primitives

| Current | Location | Tokio Equivalent |
|---------|----------|------------------|
| `Arc<Mutex<File>>` | daemon.rs:116 | `tokio::sync::Mutex` or mpsc channel |
| `mpsc::channel` | server_runtime.rs | `tokio::sync::mpsc` |
| `Arc<AtomicBool>` | delegation.rs | `tokio::sync::Notify` or `CancellationToken` |

---

## Phase 3: Engine Crate Migration

### File I/O Operations

#### File Open/Create
**Locations:**
- `batch/writer.rs:28` - `File::create()`
- `batch/reader.rs:27` - `File::open()`
- `local_copy/executor/file/copy/transfer.rs:198-259` - Various open modes

All convert to `tokio::fs::File::open()` / `tokio::fs::File::create()`.

#### Buffered I/O
**Locations:**
- `batch/reader.rs:17` - `BufReader<File>`
- `batch/writer.rs:18` - `BufWriter<File>`

Options:
1. `tokio::io::BufReader` / `tokio::io::BufWriter`
2. Custom async buffer implementation

#### Read/Write Trait Bounds
**Location:** `delta/script.rs:97-104`

```rust
// Current
pub fn apply_delta<R: Read + Seek, W: Write>(
    basis: &mut R,
    delta: &[DeltaToken],
    output: &mut W,
) -> io::Result<()>

// Target
pub async fn apply_delta<R: AsyncRead + AsyncSeek + Unpin, W: AsyncWrite + Unpin>(
    basis: &mut R,
    delta: &[DeltaToken],
    output: &mut W,
) -> io::Result<()>
```

### Directory Traversal

#### read_dir() Iteration
**Locations:**
- `local_copy/executor/directory/support.rs:19-29`
- `local_copy/executor/cleanup.rs:24-26`

```rust
// Current (sync iterator)
for entry in fs::read_dir(path)? {
    let entry = entry?;
    // process entry
}

// Target (async stream)
let mut entries = tokio::fs::read_dir(path).await?;
while let Some(entry) = entries.next_entry().await? {
    // process entry
}
```

### Critical Mutex Issue

**Location:** `local_copy/executor/directory/recursive.rs:69`

```rust
// Current - WILL PANIC in async context
let mut writer = batch_writer_arc.lock().unwrap();

// Target
let mut writer = batch_writer_arc.lock().await;
```

The `Arc<std::sync::Mutex<BatchWriter>>` in `context_impl/options.rs:447` must become
`Arc<tokio::sync::Mutex<BatchWriter>>`.

### fsync Handling

**Location:** `local_copy/executor/file/copy/transfer.rs:302,667`

Options:
1. `tokio::task::block_in_place(|| file.sync_all())` - Less ideal
2. Make fsync configurable/optional - Better
3. Batch fsync operations - Best but complex

---

## Phase 4: Transport Crate Migration

### SSH Connection Wrapper

**Location:** `crates/rsync_io/src/ssh/connection.rs`

```rust
// Current
impl Read for SshConnection { ... }
impl Write for SshConnection { ... }

// Target
impl AsyncRead for SshConnection { ... }
impl AsyncWrite for SshConnection { ... }
```

With `tokio::process::Command` for subprocess spawning.

### Stream Transformations

**Location:** `crates/rsync_io/src/negotiation/parts/stream_parts/transform.rs`

Generic helpers work for both sync and async with appropriate trait bounds.

---

## Phase 5: Core Crate Migration

### Server Setup Generic Bounds

**Location:** `crates/core/src/server/setup.rs:7-8`

```rust
// Current
pub fn setup_protocol<R: Read, W: Write>(...)

// Target (breaking change)
pub async fn setup_protocol<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(...)
```

---

## Migration Strategy

### Incremental Approach

1. **Add feature flag** - All async code behind `#[cfg(feature = "async")]`
2. **Start with daemon** - Highest impact, clearest async boundaries
3. **Add async trait variants** - `AsyncRead`/`AsyncWrite` alongside `Read`/`Write`
4. **Convert file operations** - `tokio::fs` for file I/O
5. **Update CLI entry point** - `#[tokio::main]` when feature enabled

### Breaking Changes

- All public APIs with `Read`/`Write` bounds need async variants
- Error handling changes (timeout semantics)
- Test infrastructure requires `#[tokio::test]`

### Testing Strategy

1. Keep sync tests working (default feature set)
2. Add `#[cfg(feature = "async")]` async test variants
3. Run both in CI: `cargo test` and `cargo test --features async`

---

## Estimated Effort

| Phase | Scope | Estimated LOC Changes |
|-------|-------|----------------------|
| Dependencies | Cargo.toml files | ~50 |
| Daemon async | server_runtime, session, delegation | ~500 |
| Engine async | batch, local_copy, delta | ~800 |
| Transport async | ssh, negotiation | ~200 |
| Core async | server setup, client | ~300 |
| Tests | async test variants | ~400 |
| **Total** | | **~2250** |

---

## Files Requiring Major Changes

### Critical Priority
1. `crates/daemon/src/daemon/sections/server_runtime.rs`
2. `crates/daemon/src/daemon/sections/session_runtime.rs`
3. `crates/daemon/src/daemon/sections/delegation.rs`
4. `crates/engine/src/batch/writer.rs`
5. `crates/engine/src/batch/reader.rs`
6. `crates/engine/src/local_copy/executor/file/copy/transfer.rs`

### High Priority
7. `crates/engine/src/delta/script.rs`
8. `crates/engine/src/local_copy/executor/directory/recursive.rs`
9. `crates/engine/src/local_copy/executor/cleanup.rs`
10. `crates/rsync_io/src/ssh/connection.rs`

### Medium Priority
11. `crates/core/src/server/setup.rs`
12. `crates/engine/src/local_copy/executor/file/sparse.rs`
13. `crates/engine/src/local_copy/dir_merge/load.rs`
14. `crates/engine/src/signature.rs`

---

## Next Steps

1. Create `async` feature flag in workspace Cargo.toml
2. Add tokio dependencies (gated)
3. Start with daemon TCP listener conversion
4. Add async test infrastructure
5. Incrementally convert remaining modules

This migration should be done in small, reviewable commits with feature flag
allowing gradual rollout and easy rollback if issues arise.
