# Daemon Crate TLS Insertion Points (TLS-2)

Audit of `crates/daemon/src/` to identify where a native TLS layer (e.g.
rustls) would be inserted into the listener and connection handler code.

Prerequisite: TLS-1 confirmed that upstream rsync has no native TLS -
encryption is delegated to stunnel, rsync-ssl, or a reverse proxy.
oc-rsync already works with all three approaches because TLS terminates
before the rsync protocol starts. This audit documents the internal
insertion points should native TLS support be added in the future.

## 1. Connection Lifecycle Overview

The daemon crate has two listener paths:

| Path | Feature gate | Status | Entry point |
|------|-------------|--------|-------------|
| Sync (thread-per-connection) | default | Production | `run_daemon()` -> `serve_connections()` |
| Hybrid async (tokio accept + sync workers) | `async-daemon` | Skeleton | `run_async_daemon()` -> `run_hybrid_listener()` |
| Full async (tokio throughout) | `async` | Skeleton | `AsyncDaemonListener::serve()` |

### Sync path lifecycle

```
TcpListener::bind / bind_with_backlog
  -> set_nonblocking(true) on listener
  -> accept loop (run_single_listener_loop or run_dual_stack_loop)
     -> listener.accept() -> (TcpStream, SocketAddr)
     -> stream.set_nonblocking(false)
     -> apply_client_options (TCP_NODELAY, SO_KEEPALIVE, etc.)
     -> refuse_if_at_capacity (writes @ERROR on raw stream if cap hit)
     -> spawn_connection_worker (std::thread::spawn)
        -> handle_session(stream: TcpStream, peer_addr, params)
           -> configure_stream (set read/write timeouts)
           -> parse_proxy_header (if proxy_protocol enabled, reads raw bytes)
           -> handle_legacy_session(stream: TcpStream, ...)
              -> BufReader::new(stream)
              -> write greeting bytes to reader.get_mut()
              -> read client version, module request
              -> authentication challenge/response
              -> setup_transfer_streams (try_clone x2 for read/write split)
              -> run_server_with_handshake(config, handshake, &mut read, &mut write, ...)
```

### Hybrid async path lifecycle

```
tokio::net::TcpListener::bind
  -> accept().await -> (tokio::net::TcpStream, SocketAddr)
  -> stream.into_std() + set_nonblocking(false)
  -> tokio::task::spawn_blocking(|| worker(std_stream, peer))
  -> (worker is currently a no-op skeleton)
```

### Full async path lifecycle

```
tokio::net::TcpListener::bind
  -> accept().await -> (tokio::net::TcpStream, SocketAddr)
  -> stream.into_split() -> (OwnedReadHalf, OwnedWriteHalf)
  -> BufReader/BufWriter wrappers
  -> async greeting/handshake (skeleton only, no transfer)
```

## 2. Stream Type Usage

### Concrete type dependency

The daemon crate is **hardcoded to `std::net::TcpStream`** throughout. There
is no generic stream abstraction. Key evidence:

- `handle_session(stream: TcpStream, ...)` - concrete `TcpStream` parameter
- `handle_legacy_session(stream: TcpStream, ...)` - concrete `TcpStream`
- `handle_binary_session(stream: TcpStream, ...)` - concrete `TcpStream`
- `BufReader<TcpStream>` - used in `ModuleRequestContext`, `perform_module_authentication`
- `setup_transfer_streams` calls `stream.try_clone()` - a `TcpStream`-specific method
- `write_limited(stream: &mut TcpStream, ...)` - concrete `TcpStream`
- `send_error_and_exit(stream: &mut TcpStream, ...)` - concrete `TcpStream`
- `deny_module(stream: &mut TcpStream, ...)` - concrete `TcpStream`
- `advertise_capabilities(stream: &mut TcpStream, ...)` - concrete `TcpStream`
- `refuse_if_at_capacity(stream: &mut TcpStream, ...)` - concrete `TcpStream`

### Trait bounds required

The greeting, auth, and module-select phases use:
- `std::io::Read` (via `BufReader<TcpStream>`)
- `std::io::Write` (via `reader.get_mut()` -> `&mut TcpStream`)
- `TcpStream::try_clone()` for read/write split in transfer phase
- `TcpStream::set_nodelay(true)` before transfer
- `TcpStream::set_read_timeout()` / `set_write_timeout()` for handshake
- `TcpStream::peek()` in `detect_session_style` (currently dead code)
- `TcpStream::set_nonblocking()` in `detect_session_style` (currently dead code)

The transfer engine (`run_server_with_handshake`) accepts:
- `stdin: &mut dyn Read`
- `stdout: W` where `W: Write`

This means the transfer engine is **already generic over the stream type**.
Only the daemon's pre-transfer code is hardcoded.

### `TracingStream` as precedent

`crates/daemon/src/daemon/tracing_stream.rs` contains a `TracingStream`
wrapper that implements `Read + Write` around a `TcpStream`. This wrapper
is not used in production - it is a debugging utility. However, it
demonstrates the wrapping pattern: a newtype struct holding `inner:
TcpStream` and delegating `Read`/`Write` implementations.

## 3. Primary TLS Insertion Point

### Where

The TLS handshake must occur **after TCP accept and before any rsync protocol
bytes**. The exact insertion point is in `spawn_connection_worker` (file:
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`, line 185),
between the socket option application and the `handle_session` call:

```
apply_client_options(&stream, ...)       // <-- existing
// === TLS HANDSHAKE HERE ===            // <-- insertion point
handle_session(stream, peer_addr, ...)   // <-- existing
```

More precisely, the TLS acceptor would wrap the `TcpStream` before it enters
`handle_session`. The wrapped `TlsStream<TcpStream>` would then be passed
through the session lifecycle.

### Why this location

1. **After socket options.** `TCP_NODELAY`, `SO_KEEPALIVE`, buffer sizes must
   be set on the raw TCP socket before TLS negotiation - they govern the
   transport layer beneath TLS.

2. **Before PROXY protocol.** The PROXY protocol header is sent by the load
   balancer *inside* the TLS tunnel (stunnel terminates TLS, then HAProxy adds
   the PROXY header, then oc-rsync reads it). So PROXY protocol parsing must
   happen on the TLS-wrapped stream, not the raw TCP stream.

3. **Before the `@RSYNCD:` greeting.** The greeting is the first application
   data - it must flow over the encrypted channel.

4. **Per-connection, not per-listener.** TLS negotiation is connection-scoped.
   The `TlsAcceptor` config (certificate, key, ALPN) is daemon-wide, but the
   `accept()` call is per-connection.

### Thread model impact

Each connection already gets its own OS thread via `std::thread::spawn` in
`spawn_connection_worker`. The TLS handshake would run synchronously on that
worker thread, **not blocking the accept loop**. This matches upstream
rsync's model where each connection is an independent process/thread.

The rustls `Acceptor::accept(stream)` call is blocking (when using
`rustls::StreamOwned`) and takes roughly 1-3 ms on modern hardware for
TLS 1.3. This is negligible compared to the thread spawn overhead.

## 4. Code Changes Required

### 4a. Stream type generalization

To avoid duplicating every function signature, the session handler chain
should be made generic over a stream trait. Two options:

**Option A: Type parameter** (zero-cost, compile-time monomorphization)

```rust
fn handle_session<S: Read + Write>(stream: S, peer_addr: SocketAddr, ...) -> io::Result<()>
```

Requires propagating the type parameter through `handle_legacy_session`,
`ModuleRequestContext`, `perform_module_authentication`, `write_limited`,
`send_error_and_exit`, `deny_module`, `advertise_capabilities`, and all
helper functions that currently take `&mut TcpStream`.

**Option B: Trait object** (simpler, minor dynamic dispatch overhead)

```rust
fn handle_session(stream: Box<dyn ReadWrite>, peer_addr: SocketAddr, ...) -> io::Result<()>
```

Where `ReadWrite: Read + Write` is a helper trait. The dynamic dispatch cost
is negligible relative to network I/O latency.

### 4b. `try_clone()` elimination

`setup_transfer_streams` calls `TcpStream::try_clone()` twice to create
separate read and write handles. `TlsStream` does not support `try_clone()`
because TLS state (encryption context, sequence numbers) is shared between
read and write directions.

Resolution: the transfer engine already accepts `&mut dyn Read` and
`W: Write` as separate parameters. A TLS-aware split can be achieved via:

- `rustls::StreamOwned` split into reader/writer halves using the crate's
  built-in `reader()`/`writer()` accessors on `Connection`.
- Or a single `TlsStream` passed as both `&mut dyn Read` and `&mut dyn Write`
  since `Read` and `Write` are implemented independently and the transfer
  engine does not use them concurrently from different threads (the multiplexer
  sequences reads and writes on the same thread).

### 4c. Socket-specific calls

The following `TcpStream`-specific methods are called on the stream after
accept:

| Method | Location | TLS impact |
|--------|----------|------------|
| `set_read_timeout` | `configure_stream` | Must be called on the **inner** `TcpStream`, not the TLS wrapper |
| `set_write_timeout` | `configure_stream` | Same - inner `TcpStream` |
| `set_nodelay` | `setup_transfer_streams` | Inner `TcpStream` |
| `try_clone` | `setup_transfer_streams` | **Not available on TlsStream** - see 4b |
| `peek` | `detect_session_style` | Dead code - unused in production |
| `set_nonblocking` | `detect_session_style` | Dead code - unused in production |

Resolution: the TLS wrapper should expose a `get_ref() -> &TcpStream` method
(rustls provides this) so socket-level configuration can target the inner
transport. Alternatively, configure timeouts and TCP options *before* wrapping
in TLS.

### 4d. Greeting, auth, and module-select

These phases use the stream exclusively through `BufReader<TcpStream>` and
`reader.get_mut() -> &mut TcpStream`. Since `BufReader<S>` is generic over
`S: Read`, switching to `BufReader<TlsStream<TcpStream>>` (or
`BufReader<Box<dyn Read>>`) requires no logic changes. The greeting bytes,
auth challenge, module listing, and error messages are all plain ASCII text
written with `write_all` and `flush` - TLS is transparent to them.

### 4e. `refuse_if_at_capacity` race

When the connection cap is hit, `refuse_if_at_capacity` writes an `@ERROR`
message directly to the raw `TcpStream` *before* the connection enters the
session handler. With TLS, this message must be sent over the encrypted
channel, which means the TLS handshake must complete before the refusal can
be delivered. This changes the refusal behavior: under TLS, the daemon would
perform the full TLS handshake even for connections it intends to refuse.

Two options:
1. Accept the cost - TLS handshake is 1-3 ms, and hitting the cap is rare.
2. Close the raw TCP socket without a TLS handshake - the client sees a
   connection reset rather than a readable error message. This matches the
   behavior of web servers under connection pressure.

## 5. Feature Flag Structure

### Existing feature flags

The daemon crate already has a well-established feature flag pattern:

```toml
[features]
default = []
sd-notify = ["dep:sd-notify", "core/sd-notify"]
async = ["dep:tokio", "core/async"]
async-daemon = ["dep:tokio"]
concurrent-sessions = ["dep:dashmap"]
xattr = ["core/xattr"]
tracing = ["dep:tracing", "core/tracing"]
acl = ["core/acl", "metadata/acl"]
iconv = ["core/iconv", "protocol/iconv"]
```

### Proposed TLS feature flag

```toml
tls = ["dep:rustls", "dep:rustls-pemfile", "dep:webpki-roots"]
```

- `rustls` - pure Rust TLS implementation, no OpenSSL dependency
- `rustls-pemfile` - PEM certificate/key parsing
- `webpki-roots` - Mozilla root CA bundle for client certificate validation

The flag would gate:
- The `TlsAcceptor` construction in `serve_connections`
- The stream wrapping logic in `spawn_connection_worker`
- Configuration directives in `rsyncd.conf` parsing (`ssl cert`, `ssl key`,
  `ssl ca`, `ssl port`)
- The `--ssl` CLI flag

When the `tls` feature is disabled (the default), the daemon operates
identically to today - no TLS dependencies are pulled in, no code changes
are visible.

## 6. Async Path Considerations

### Hybrid async listener (`async-daemon`)

The hybrid listener converts `tokio::net::TcpStream` back to
`std::net::TcpStream` via `into_std()` before handing off to the sync
worker. With TLS, the handshake could be performed either:

- **Async TLS** (via `tokio-rustls`): perform the handshake on the tokio
  runtime before `into_std()`, then convert the negotiated TLS stream to a
  sync wrapper. This is cleaner but adds a tokio-rustls dependency.
- **Sync TLS** (via plain `rustls`): convert to `std::net::TcpStream` first,
  then perform the blocking TLS handshake on the `spawn_blocking` pool. This
  is simpler and keeps the TLS path identical between sync and hybrid modes.

### Full async listener (`async`)

Uses `tokio::net::TcpStream` with `AsyncBufReadExt` / `AsyncWriteExt`. TLS
would use `tokio-rustls::TlsAcceptor::accept(stream).await` to produce a
`tokio_rustls::server::TlsStream<TcpStream>`, which implements
`AsyncRead + AsyncWrite`. No conversion to std needed.

## 7. Configuration Surface

A native TLS daemon would need these `rsyncd.conf` directives, mirroring
the conventions established by upstream's stunnel wrapper:

| Directive | Type | Description |
|-----------|------|-------------|
| `ssl cert` | path | Server certificate file (PEM) |
| `ssl key` | path | Private key file (PEM) |
| `ssl ca` | path | CA certificate for client auth (optional) |
| `ssl port` | integer | TLS listener port (default 874, mirrors rsync-ssl) |
| `ssl only` | boolean | Refuse non-TLS connections (default false) |

These would be parsed by the existing `rsyncd.conf` parser in
`crates/daemon/src/daemon/sections/config_parsing/` and stored in
`RuntimeOptions`.

## 8. Summary of Insertion Points

| # | Location | File | What changes |
|---|----------|------|-------------|
| 1 | `spawn_connection_worker` | `server_runtime/connection.rs:185` | Wrap `TcpStream` in `TlsStream` after socket options, before `handle_session` |
| 2 | `handle_session` | `sections/session_runtime.rs:44` | Change signature from `TcpStream` to generic `S: Read + Write` |
| 3 | `handle_legacy_session` | `sections/session_runtime.rs:206` | Change `BufReader<TcpStream>` to `BufReader<S>` |
| 4 | `ModuleRequestContext` | `module_access/request.rs:17` | Change `BufReader<TcpStream>` field to generic or trait object |
| 5 | `perform_module_authentication` | `module_access/authentication.rs:33` | Change `BufReader<TcpStream>` parameter |
| 6 | `setup_transfer_streams` | `module_access/transfer.rs:226` | Replace `try_clone()` with TLS-compatible split |
| 7 | `write_limited` | `sections/session_runtime.rs:176` | Change `&mut TcpStream` to `&mut dyn Write` |
| 8 | Helper functions | `request.rs`, `listing.rs` | Change `&mut TcpStream` params to `&mut dyn Write` |
| 9 | `refuse_if_at_capacity` | `server_runtime/connection.rs:120` | Decide: TLS handshake before refusal, or raw TCP close |
| 10 | `AcceptLoopState` / `serve_connections` | `server_runtime/accept_loop.rs` | Hold `Arc<TlsAcceptor>` in loop state |
| 11 | `rsyncd.conf` parser | `sections/config_parsing/` | Add SSL directive parsing |
| 12 | `RuntimeOptions` | `runtime_options/types.rs` | Add TLS config fields |
| 13 | `async_listener` | `async_listener.rs` | Insert `tokio-rustls` accept before `into_std()` |
| 14 | `AsyncDaemonListener::serve` | `async_session/listener.rs` | Insert `tokio-rustls` accept after TCP accept |

## 9. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| `try_clone()` removal | High | Transfer engine already accepts `dyn Read` + `W: Write`; no concurrent usage |
| Type parameter explosion | Medium | Use `Box<dyn Read + Write>` trait objects to contain generics |
| Async TLS dependency | Low | `tokio-rustls` is mature, widely used, pure Rust |
| Performance overhead | Low | TLS 1.3 adds ~1-3 ms handshake; negligible for daemon workloads |
| Certificate hot-reload | Medium | `TlsAcceptor` is `Arc`-wrapped; rebuild on SIGHUP alongside config reload |
| Feature flag complexity | Low | Follows established pattern (`sd-notify`, `async`, `xattr`) |

## 10. Recommendation

The primary insertion point at `spawn_connection_worker` is clean and
non-disruptive. The biggest implementation task is generalizing the session
handler chain from concrete `TcpStream` to a `Read + Write` trait bound -
approximately 15-20 function signatures across 5 files. The `try_clone()`
elimination in `setup_transfer_streams` requires the most care but is
straightforward since the transfer engine's interface is already generic.

A feature-gated `tls` flag with `rustls` (sync path) and `tokio-rustls`
(async path) would add native TLS without any default-build impact. The
existing external approaches (stunnel, rsync-ssl, reverse proxy) remain
fully functional and are the recommended path for production use.
