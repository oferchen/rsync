# Transport Crate TLS Insertion Points for Client Connections (TLS-3)

Audit of the client-side daemon connection path to identify where a native
TLS layer (e.g. rustls) would wrap `TcpStream` into `TlsStream` for
`rsync://` connections.

Prerequisite: TLS-1 documented upstream rsync's stunnel/SSL model. TLS-2
audited the daemon (server) side and found the insertion point at
`spawn_connection_worker` with `try_clone()` as the biggest obstacle. This
audit covers the complementary client side - when oc-rsync connects to an
rsync daemon over a network.

## 1. Architecture - No Transport Crate

There is no dedicated `transport` crate. Client-side daemon connection code
lives entirely in `crates/core/src/client/`:

| Path | Responsibility |
|------|---------------|
| `module_list/connect/direct.rs` | TCP socket creation, DNS resolution, `connect_direct()` |
| `module_list/connect/proxy.rs` | HTTP CONNECT proxy tunneling |
| `module_list/connect/program.rs` | `RSYNC_CONNECT_PROG` child process transport |
| `module_list/connect/mod.rs` | `DaemonStream` enum, `open_daemon_stream()` dispatcher |
| `module_list/listing.rs` | Module listing handshake (`run_module_list()`) |
| `remote/daemon_transfer/connection/mod.rs` | Transfer-path handshake (`perform_daemon_handshake()`) |
| `remote/daemon_transfer/orchestration/transfer.rs` | Pull/push transfer execution |
| `remote/daemon_transfer/orchestration/arguments.rs` | Daemon argument building |
| `remote/daemon_transfer/mod.rs` | `run_daemon_transfer()` entry point |

## 2. Client Connection Lifecycle

Two distinct paths exist for daemon connections - module listing and data
transfer. Both begin with TCP connection setup but diverge in how the stream
is consumed.

### Module listing path

```
run_module_list_with_password_and_options()         [listing.rs]
  -> open_daemon_stream()                           [connect/mod.rs]
     -> load_daemon_connect_program() / load_daemon_proxy()
     -> connect_direct() -> TcpStream               [connect/direct.rs]
     -> DaemonStream::tcp(stream)
  -> configure_daemon_stream()                      [listing.rs]
     -> apply_socket_options() on DaemonStream::Tcp  [socket_options/apply.rs]
  -> negotiate_legacy_daemon_session(stream, proto)  [rsync_io crate]
     -> sniff_negotiation_stream() -> NegotiatedStream<DaemonStream>
     -> parse greeting, send client greeting
     -> LegacyDaemonHandshake<DaemonStream>
  -> handshake.into_stream() -> NegotiatedStream<DaemonStream>
  -> BufReader::new(negotiated_stream)
  -> write "#list\n", read module entries
```

### Data transfer path

```
run_daemon_transfer()                               [daemon_transfer/mod.rs]
  -> connect_direct() -> TcpStream                  [connect/direct.rs]
  -> apply_socket_options(&stream, sockopts)         [socket_options/apply.rs]
  -> perform_daemon_handshake(&mut stream, ...)      [connection/mod.rs]
     -> stream.try_clone() -> BufReader<TcpStream>   ** TcpStream-specific **
     -> read greeting, send client greeting
     -> module selection, MOTD, authentication
  -> send_daemon_arguments(&mut stream, ...)         [arguments.rs]
  -> run_pull_transfer(config, stream, ...) or
     run_push_transfer(config, stream, ...)          [transfer.rs]
     -> configure_transfer_socket(&stream, ...)      ** TcpStream methods **
     -> stream.try_clone()                           ** TcpStream-specific **
     -> run_server_with_handshake(config, hs, &mut reader, &mut stream, ...)
```

## 3. Stream Type Usage

### Concrete type dependencies

The data transfer path is **hardcoded to `std::net::TcpStream`** throughout.
Key evidence:

| Location | Concrete type usage |
|----------|-------------------|
| `connect/direct.rs:15` | `fn connect_direct(...) -> Result<TcpStream, ClientError>` |
| `connect/proxy.rs:17` | `fn connect_via_proxy(...) -> Result<TcpStream, ClientError>` |
| `connect/mod.rs:64` | `enum DaemonStream { Tcp(TcpStream), Program(...) }` |
| `connection/mod.rs:9` | `use std::net::TcpStream;` in perform_daemon_handshake |
| `connection/mod.rs:150` | `fn perform_daemon_handshake(stream: &mut TcpStream, ...)` |
| `transfer.rs:32` | `fn run_pull_transfer(..., mut stream: TcpStream, ...)` |
| `transfer.rs:86` | `fn run_push_transfer(..., mut stream: TcpStream, ...)` |
| `arguments.rs:29` | `fn send_daemon_arguments(stream: &mut TcpStream, ...)` |
| `socket_options/apply.rs:29` | `fn apply_socket_options(stream: &TcpStream, ...)` |

### `try_clone()` calls - the TLS obstacle

Three locations call `TcpStream::try_clone()` to create independent
read/write handles from a single socket:

1. **`connection/mod.rs:159`** - `perform_daemon_handshake()` clones the
   stream to create a `BufReader` for reading the greeting while retaining
   the original for writing the client greeting. The `BufReader<TcpStream>`
   reads lines while the raw `TcpStream` writes responses.

2. **`transfer.rs:112`** - `run_push_transfer()` clones the stream to create
   separate read and write halves for `run_server_with_handshake()`.

3. **`transfer.rs:227`** - `run_server_with_handshake_over_stream()` (used by
   `run_pull_transfer()`) clones the stream for the same read/write split.

`TlsStream` from rustls/native-tls does not support `try_clone()` because
TLS state is shared between read and write directions. This is the same
obstacle identified in TLS-2 for the daemon side.

### Module listing path - already generic

The module listing path is more TLS-friendly because:

- `open_daemon_stream()` returns `DaemonStream` (already an enum, trivially
  extensible with a `Tls(TlsStream)` variant)
- `negotiate_legacy_daemon_session<R: Read + Write>()` is generic
- `NegotiatedStream<R>` is generic over the inner transport
- `LegacyDaemonHandshake<R>` is generic and has `map_stream_inner()` for
  post-handshake transport wrapping
- No `try_clone()` in the listing path - the `BufReader` wraps the
  `NegotiatedStream` directly

### Transfer engine - already generic

The downstream transfer engine (`crates/transfer/src/lib.rs`) already accepts
generic I/O:

```rust
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    handshake: HandshakeResult,
    stdin: &mut dyn Read,     // trait object, any reader
    stdout: W,                // generic writer
    ...
)
```

The concrete `TcpStream` dependency exists only in the ~200 lines between
`connect_direct()` and the `run_server_with_handshake()` call.

## 4. Primary TLS Insertion Point

### Where to wrap

The TLS handshake should occur **immediately after TCP connection
establishment and before any rsync protocol exchange**. The exact insertion
point differs between the two paths:

#### Module listing path

In `open_daemon_stream()` (`connect/mod.rs:20-46`), after
`connect_direct()` returns a `TcpStream` and before wrapping in
`DaemonStream::tcp()`:

```
let stream = connect_direct(...)?;
// TLS insertion point:
// let stream = if tls_enabled {
//     let connector = build_tls_connector()?;
//     let tls_stream = connector.connect(hostname, stream)?;
//     DaemonStream::Tls(tls_stream)
// } else {
//     DaemonStream::tcp(stream)
// };
Ok(DaemonStream::tcp(stream))
```

The `DaemonStream` enum would gain a `Tls` variant. Since `DaemonStream`
already implements `Read + Write` via delegation, adding a TLS variant is
mechanical.

#### Data transfer path

In `run_daemon_transfer()` (`daemon_transfer/mod.rs:103-109`), after
`connect_direct()` and `apply_socket_options()`, before
`perform_daemon_handshake()`:

```
let mut stream = connect_direct(&request.address, ...)?;
apply_socket_options(&stream, sockopts)?;
// TLS insertion point:
// let mut stream = if tls_enabled {
//     tls_wrap(stream, &request.address.host)?
// } else {
//     stream
// };
perform_daemon_handshake(&mut stream, ...)?;
```

This path requires more work because `perform_daemon_handshake()`,
`send_daemon_arguments()`, `run_pull_transfer()`, and `run_push_transfer()`
all take concrete `TcpStream`. The entire chain from connection to transfer
needs generalization.

## 5. Required Changes Catalog

### Module listing path (lower effort)

| # | File | Change |
|---|------|--------|
| L1 | `connect/mod.rs` | Add `DaemonStream::Tls(TlsStream)` variant |
| L2 | `connect/mod.rs` | Add `Read`/`Write` delegation for new variant |
| L3 | `connect/mod.rs` | Extend `open_daemon_stream()` with TLS flag parameter |
| L4 | `listing.rs:376` | Handle `DaemonStream::Tls` in `configure_daemon_stream()` (socket options are TCP-level and must apply before TLS wrapping - already handled if TLS wraps after `apply_socket_options`) |

### Data transfer path (higher effort)

| # | File | Change |
|---|------|--------|
| T1 | `daemon_transfer/mod.rs` | Wrap `TcpStream` with TLS after `connect_direct()` |
| T2 | `connection/mod.rs` | Generalize `perform_daemon_handshake()` from `&mut TcpStream` to `&mut S where S: Read + Write` |
| T3 | `connection/mod.rs:159` | Eliminate `try_clone()` - restructure to use single stream for both read and write during handshake |
| T4 | `arguments.rs` | Generalize `send_daemon_arguments()` from `&mut TcpStream` to `&mut S where S: Write` |
| T5 | `transfer.rs:32` | Generalize `run_pull_transfer()` from `TcpStream` to generic stream |
| T6 | `transfer.rs:86` | Generalize `run_push_transfer()` from `TcpStream` to generic stream |
| T7 | `transfer.rs:112,227` | Eliminate `try_clone()` - use `ReadHalf`/`WriteHalf` adapter or similar split |
| T8 | `transfer.rs:158` | Generalize `configure_transfer_socket()` - `set_nodelay()`, `set_read_timeout()`, `set_write_timeout()` are `TcpStream`-specific methods not available on `TlsStream` |
| T9 | `socket_options/apply.rs` | `apply_socket_options()` takes `&TcpStream` - must apply before TLS wrapping or expose the underlying socket |

### Cross-cutting concerns

| # | Area | Change |
|---|------|--------|
| C1 | URL parsing | `DaemonTransferRequest::parse_rsync_url()` currently only recognizes `rsync://`. Must add `rsyncs://` or `rsync+tls://` scheme detection |
| C2 | Port handling | Default port 873 for `rsync://`, 874 for `rsyncs://` / `rsync+tls://` |
| C3 | CLI flag | Add `--ssl` flag to `ClientConfig` (mirrors upstream rsync-ssl behavior) |
| C4 | Feature gate | New `tls` Cargo feature, following `async-daemon` / `sd-notify` pattern |

## 6. `try_clone()` Elimination Strategy

The three `try_clone()` calls are the central obstacle for TLS support. TLS
streams cannot be cloned because the encryption state machine is shared
between read and write. Two strategies:

### Option A: Trait-based read/write split (preferred)

Define a trait that produces independent read and write halves:

```rust
trait SplittableStream: Read + Write {
    type ReadHalf: Read;
    type WriteHalf: Write;
    fn split(self) -> (Self::ReadHalf, Self::WriteHalf);
}
```

For `TcpStream`, `split()` calls `try_clone()`. For `TlsStream`, `split()`
uses the crate's native split mechanism (e.g., `tokio_rustls::TlsStream::split()`
or a `ReadHalf`/`WriteHalf` wrapper with `Arc<Mutex<TlsStream>>`).

### Option B: Single-stream sequential I/O

Eliminate the read/write split entirely. Use a single `&mut stream` for both
reading and writing, alternating direction as needed. The rsync protocol is
half-duplex during the handshake phase and only goes full-duplex during the
transfer phase (which already uses `&mut dyn Read` + `W: Write`).

For `perform_daemon_handshake()` specifically, the `try_clone()` at line 159
creates a `BufReader` for reading while keeping the raw stream for writing.
This can be replaced by passing a `BufReader<&mut S>` and using
`reader.get_mut()` for writes - exactly the pattern already used in the
module listing path.

### Recommendation

Use **Option B for the handshake** (trivial refactor, the listing path
already does this) and **Option A for the transfer phase** (the transfer
engine needs simultaneous bidirectional I/O).

## 7. Hostname Verification and SNI

### SNI (Server Name Indication)

The TLS connector must send the hostname via SNI. The hostname is available
from `DaemonAddress.host` (a `String`), which is the DNS name or IP address
the user specified. For IP addresses, SNI should be omitted per RFC 6066.

### Certificate verification

By default, rustls verifies the server certificate against the system trust
store and checks that the certificate's Subject Alternative Name matches the
hostname. This is the correct default for production.

For testing and migration, an `--ssl-verify=no` flag (or environment
variable) would allow disabling certificate verification, mirroring
upstream's `rsync-ssl` script which defaults to `--verify-hostname=0` with
OpenSSL backend.

### Self-signed certificates

Users with self-signed daemon certificates would need either:

- `--ssl-ca=<path>` to specify a custom CA certificate
- System trust store configuration (OS-dependent)

## 8. `--ssl` Flag and URL Scheme Interaction

### Activation model

TLS should be activated by any of:

1. **URL scheme**: `rsyncs://host/module` or `rsync+tls://host/module`
2. **CLI flag**: `--ssl` with a plain `rsync://` URL
3. **Environment variable**: `RSYNC_SSL=1` (matches upstream rsync-ssl script)

### Port resolution

| Scheme | Default port | Notes |
|--------|-------------|-------|
| `rsync://` (no `--ssl`) | 873 | Current behavior, unchanged |
| `rsync://` + `--ssl` | 874 | Explicit `--ssl` implies TLS port |
| `rsyncs://` | 874 | TLS implied by scheme |
| `rsync+tls://` | 874 | Alternate TLS scheme |
| Any + `--port=N` | N | Explicit port always wins |

### Proxy interaction

When `RSYNC_PROXY` is set, the HTTP CONNECT tunnel is established first
through the proxy, then TLS wraps the tunneled connection. The TLS
handshake occurs after `establish_proxy_tunnel()` succeeds. This is the
standard HTTPS-through-proxy pattern.

### Connect program interaction

When `RSYNC_CONNECT_PROG` is set, the connect program provides the transport
(stdin/stdout of the child process). TLS wrapping a child process stdio pair
is possible but unusual. The `--ssl` flag should be rejected when a connect
program is active, or the connect program should be expected to handle TLS
itself (e.g., `openssl s_client`).

## 9. Implementation Order

Recommended sequence, from lowest to highest effort:

1. **C4**: Add `tls` feature gate to `core` crate Cargo.toml
2. **C1, C2**: Add `rsyncs://` URL parsing and port 874 default
3. **C3**: Add `--ssl` flag to CLI and `ClientConfig`
4. **L1-L4**: TLS support in module listing path (extend `DaemonStream` enum)
5. **T2-T4**: Generalize handshake and argument functions (eliminate concrete
   `TcpStream` parameters)
6. **T3, T7**: Eliminate `try_clone()` calls (the hard part)
7. **T1, T5, T6, T8, T9**: Complete transfer path generalization

Steps 1-4 are self-contained and independently useful (module listing over
TLS). Steps 5-7 are the bulk of the work and mirror the daemon-side changes
identified in TLS-2.

## 10. Comparison with Daemon Side (TLS-2)

| Aspect | Daemon (TLS-2) | Client (TLS-3) |
|--------|---------------|----------------|
| Primary insertion point | `spawn_connection_worker` | `run_daemon_transfer` / `open_daemon_stream` |
| Concrete type | `TcpStream` throughout | `TcpStream` in transfer, `DaemonStream` enum in listing |
| `try_clone()` count | 2 (in `setup_transfer_streams`) | 3 (handshake + transfer) |
| Transfer engine compatibility | Already generic (`&mut dyn Read` + `W: Write`) | Same engine, same generics |
| Functions to generalize | ~15 | ~8 |
| Existing abstraction | None | `DaemonStream` enum (listing path only) |
| Effort estimate | Medium-high | Medium (listing path is nearly free; transfer path similar to daemon) |

The client side is slightly easier because:

- The `DaemonStream` enum already exists as a two-variant abstraction
- The module listing path is already generic via `negotiate_legacy_daemon_session<R>`
- Fewer functions in the call chain between connection and transfer engine

## 11. Crate Dependency

Adding TLS to the `core` crate requires a TLS library dependency:

| Crate | License | Pros | Cons |
|-------|---------|------|------|
| `rustls` + `tokio-rustls` | MIT/Apache-2.0 | Pure Rust, no OpenSSL dep, async-ready | Larger API surface |
| `native-tls` | MIT/Apache-2.0 | Uses OS TLS (Schannel/Security.framework/OpenSSL) | Links to C libraries |
| `rustls` (sync only) | MIT/Apache-2.0 | No async runtime needed for sync path | Need `rustls-connector` or manual `StreamOwned` |

Recommendation: `rustls` with `ring` or `aws-lc-rs` backend. Pure Rust
avoids cross-compilation issues on all three CI platforms (Linux, macOS,
Windows). The sync daemon path uses blocking I/O, so `rustls::StreamOwned`
wrapping a `TcpStream` is sufficient without tokio. Feature-gate behind
`tls` to keep default builds lean.

## 12. Files Audited

- `crates/core/src/client/module_list/connect/direct.rs`
- `crates/core/src/client/module_list/connect/proxy.rs`
- `crates/core/src/client/module_list/connect/program.rs`
- `crates/core/src/client/module_list/connect/mod.rs`
- `crates/core/src/client/module_list/listing.rs`
- `crates/core/src/client/module_list/socket_options/apply.rs`
- `crates/core/src/client/module_list/types.rs`
- `crates/core/src/client/module_list/mod.rs`
- `crates/core/src/client/remote/daemon_transfer/connection/mod.rs`
- `crates/core/src/client/remote/daemon_transfer/orchestration/transfer.rs`
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs`
- `crates/core/src/client/remote/daemon_transfer/orchestration/mod.rs`
- `crates/core/src/client/remote/daemon_transfer/mod.rs`
- `crates/rsync_io/src/daemon/negotiate.rs`
- `crates/rsync_io/src/daemon/types/handshake.rs`
- `crates/rsync_io/src/negotiation/stream/base.rs`
- `crates/rsync_io/src/negotiation/stream/traits.rs`
- `crates/transfer/src/lib.rs`
