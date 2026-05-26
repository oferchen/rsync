# Daemon TLS Strategy - Stunnel vs Native rustls vs Both (TLS-4)

Status: Design decision
Audience: daemon and transport maintainers, operators, contributors
Scope: strategy selection for daemon TLS support
Prerequisites: TLS-1 (upstream stunnel/SSL audit), TLS-2 (daemon insertion
points audit), TLS-5 (stunnel wrapping user guide)

---

## 1. Problem Statement

oc-rsync daemon mode listens on TCP port 873 and speaks the rsync wire protocol
in cleartext - identical to upstream rsync. Neither upstream rsync nor oc-rsync
ships native TLS. Operators who need encrypted daemon connections must deploy an
external TLS terminator (stunnel, HAProxy, nginx) in front of the daemon.

This works but carries operational cost: two processes to configure, monitor,
and upgrade; certificate management split across configs; and a deployment
surface that discourages casual adoption of encryption. Users who expect a
single-binary solution - common in modern Rust tooling - find the external
wrapper pattern surprising.

This document evaluates three strategies for TLS support in oc-rsync and
recommends a path forward.

---

## 2. Prior Work

| Item | PR | Key Finding |
|------|----|-------------|
| TLS-1: upstream stunnel/SSL audit | #5052 | Upstream has no native TLS. `rsync-ssl` wraps with openssl/gnutls/stunnel. Reverse proxy recommended since 3.2.0. |
| TLS-2: daemon insertion points audit | #5056 | Primary insertion at `spawn_connection_worker`. 14 code locations need changes. `try_clone()` is the main obstacle. Transfer engine already generic. |
| TLS-5: stunnel wrapping user guide | #5055 | External TLS works today with zero code changes. Covers stunnel, rsync-ssl, HAProxy, nginx. |
| Daemon TLS-in-front recipes | #3518 | Deployment recipes for stunnel, SSH tunnel, HAProxy with systemd units. |

---

## 3. Strategy A: External-Only (stunnel / reverse proxy)

### Description

No code changes. The daemon continues to speak plaintext on port 873.
Encryption is handled by an external TLS terminator (stunnel, HAProxy, nginx)
that accepts TLS on port 874, terminates the handshake, and forwards plaintext
bytes to the daemon on loopback.

This is the upstream model. TLS-5 documents the complete setup.

### Advantages

- **Zero implementation cost.** No code to write, test, or maintain.
- **Matches upstream behavior.** Wire-compatible with `rsync-ssl` clients
  connecting to a stunnel-wrapped daemon. No feature divergence.
- **Battle-tested at scale.** stunnel and HAProxy handle millions of TLS
  connections daily in production environments unrelated to rsync.
- **Separation of concerns.** The TLS terminator can be independently upgraded,
  audited, and configured by a team that specializes in PKI.
- **Certificate rotation without daemon restart.** HAProxy and nginx reload
  certificates on SIGHUP without dropping connections. stunnel requires a
  process restart.

### Disadvantages

- **Deployment complexity.** Two processes, two configs, two log streams. The
  operator must understand both oc-rsync and the TLS terminator.
- **No integrated certificate management.** Certificates are configured in the
  stunnel/proxy config, not in `oc-rsyncd.conf`. No unified error messages
  when certificates expire or are missing.
- **Discourages encryption adoption.** Users who want a quick encrypted daemon
  must install and configure a second tool. Many will skip encryption entirely.
- **No single-binary deployment.** Containerized and embedded deployments must
  bundle both binaries and both configs.

### Cost

Zero engineering effort. Documentation already exists (TLS-5).

---

## 4. Strategy B: Native rustls Behind Feature Flag

### Description

Add a `tls` Cargo feature that embeds a rustls-based TLS acceptor in the
daemon listener and a TLS connector in the client transport layer. When
enabled, the daemon can accept TLS connections directly on port 874 (or a
configured port) without any external tooling.

### Architecture

```
Client (TLS 1.3) --> oc-rsyncd:874 [rustls acceptor] --> rsync protocol
```

The TLS handshake runs on the per-connection worker thread after TCP socket
options are applied and before the `@RSYNCD:` greeting. This matches the
insertion point identified in TLS-2 at `spawn_connection_worker`.

### Dependencies

| Crate | Version | License | Purpose |
|-------|---------|---------|---------|
| `rustls` | 0.23.x | Apache-2.0 / ISC / MIT | TLS implementation (no OpenSSL linking) |
| `rustls-pemfile` | 2.x | Apache-2.0 / ISC / MIT | PEM certificate and key parsing |
| `webpki-roots` | 0.26.x | MPL-2.0 | Mozilla root CA bundle for server verification |

All dependencies are pure Rust with no C FFI, no platform-specific linking,
and GPLv3-compatible licenses. The `rustls` crate is audited, maintained by
the rustls team, and used by Hyper, reqwest, and Tokio - the most widely
deployed Rust networking stack.

### Feature flag structure

Following the established pattern in the daemon crate (`sd-notify`, `async`,
`xattr`, `acl`, `iconv`):

```toml
# In crates/daemon/Cargo.toml
[features]
tls = ["dep:rustls", "dep:rustls-pemfile", "dep:webpki-roots"]

[dependencies]
rustls = { version = "0.23", optional = true, default-features = false, features = ["std", "tls12"] }
rustls-pemfile = { version = "2", optional = true }
webpki-roots = { version = "0.26", optional = true }
```

When the `tls` feature is disabled (the default), zero TLS code is compiled
and no dependencies are pulled. The binary size, startup time, and runtime
behavior are identical to today.

### The try_clone() obstacle and its solution

TLS-2 identified `setup_transfer_streams` as the main obstacle: it calls
`TcpStream::try_clone()` twice to create separate read and write handles.
`rustls::StreamOwned<ServerConnection, TcpStream>` does not support
`try_clone()` because TLS encryption state (sequence numbers, key material)
is shared between read and write directions.

**Solution:** A single `TlsStream` serves as both the `Read` and `Write`
handle. This works because the transfer engine (`run_server_with_handshake`)
already accepts `&mut dyn Read` and `W: Write` as separate parameters, and
the multiplexer sequences reads and writes on the same thread - they are never
used concurrently from different threads.

For the plaintext (non-TLS) path, `try_clone()` continues to work as it does
today. The TLS path simply passes a single owned `TlsStream` that implements
both traits. The conditional dispatch is feature-gated:

```rust
#[cfg(feature = "tls")]
{
    // Single TlsStream serves both Read and Write
    let mut tls_stream = rustls::StreamOwned::new(server_conn, tcp_stream);
    run_server_with_handshake(config, handshake, &mut tls_stream, &mut tls_stream, ...);
}
#[cfg(not(feature = "tls"))]
{
    // Existing try_clone() path unchanged
    let read_stream = stream.try_clone()?;
    let write_stream = stream.try_clone()?;
    run_server_with_handshake(config, handshake, &mut read_stream, &mut write_stream, ...);
}
```

### Stream type generalization

The daemon crate is hardcoded to `std::net::TcpStream` across 14 locations
(TLS-2, section 8). Two options exist for generalization:

**Option A: Type parameter** (zero-cost, compile-time monomorphization)
```rust
fn handle_session<S: Read + Write>(stream: S, peer: SocketAddr, ...) -> io::Result<()>
```
Propagates through ~15 function signatures across 5 files. Adds generic noise
but zero runtime overhead.

**Option B: Trait object** (simpler signatures, minor dispatch overhead)
```rust
fn handle_session(stream: &mut dyn ReadWrite, peer: SocketAddr, ...) -> io::Result<()>
```
Dynamic dispatch cost is negligible relative to network I/O latency. Simpler
diffs and fewer lines changed.

**Recommendation:** Option B (trait objects). The daemon's pre-transfer
greeting, auth, and module-select phases are I/O-bound at network speed -
a vtable lookup per read/write call adds no measurable overhead. The simpler
signatures reduce review burden and future maintenance.

### Socket-specific method access

Methods like `set_read_timeout`, `set_write_timeout`, and `set_nodelay` must
target the inner `TcpStream`, not the TLS wrapper. rustls provides
`StreamOwned::get_ref() -> &TcpStream` for this purpose. The approach:
configure all socket options on the raw `TcpStream` *before* wrapping it in
the TLS layer. This matches the insertion point order identified in TLS-2.

### Configuration surface

New `oc-rsyncd.conf` directives, parsed by the existing config parser:

| Directive | Type | Default | Description |
|-----------|------|---------|-------------|
| `ssl cert` | path | (none) | Server certificate file (PEM) |
| `ssl key` | path | (none) | Private key file (PEM) |
| `ssl ca` | path | (none) | CA certificate for client verification (mTLS) |
| `ssl port` | integer | 874 | TLS listener port |
| `ssl only` | boolean | false | Refuse plaintext connections on port 873 |

These follow the naming convention established by upstream's stunnel config
template (`stunnel-rsyncd.conf`).

### CLI flags

| Flag | Description |
|------|-------------|
| `--ssl` | Client-side: connect via TLS instead of plaintext |
| `--ssl-daemon` | Server-side: enable TLS acceptor (alternative to conf directives) |
| `--ssl-cert PATH` | Server certificate (overrides `ssl cert` in config) |
| `--ssl-key PATH` | Private key (overrides `ssl key` in config) |

### Advantages

- **Single-binary deployment.** No external tooling required. One binary, one
  config file, encrypted by default if certificates are provided.
- **Simpler operator experience.** Certificate paths in `oc-rsyncd.conf` next
  to module definitions. Unified error messages when certs expire.
- **Competitive advantage.** No other rsync implementation offers native TLS.
  This differentiates oc-rsync for security-conscious deployments.
- **Container-friendly.** A single container image with one binary and one
  config serves encrypted rsync. No sidecar stunnel container needed.
- **rsync-ssl client compatibility.** The upstream `rsync-ssl` script works
  against any standard TLS listener on port 874. A native rustls acceptor is
  indistinguishable from stunnel from the client's perspective.
- **Certificate hot-reload.** `TlsAcceptor` is `Arc`-wrapped. On SIGHUP
  (alongside existing config reload), rebuild the acceptor with fresh
  certificates. No connection interruption.

### Disadvantages

- **Maintenance burden.** rustls updates, certificate parsing edge cases,
  cipher suite configuration, ALPN handling. Estimated 15-20 functions need
  stream generalization (TLS-2 audit).
- **Feature divergence from upstream.** Upstream deliberately avoids TLS in the
  binary. Native TLS is a feature that upstream does not have and will not have.
- **Binary size increase.** rustls adds approximately 1-2 MB to the binary when
  the `tls` feature is enabled. Zero impact when disabled.
- **Testing surface.** TLS adds a new failure mode: expired certificates,
  mismatched hostnames, incompatible cipher suites, client verification
  failures. Each needs test coverage.
- **Security audit scope.** A native TLS implementation is a security-critical
  surface. While rustls is well-audited upstream, the integration code (config
  parsing, certificate loading, error handling) is new attack surface.

### Cost

- **Stream generalization:** ~15-20 function signatures across 5 daemon source
  files. Medium complexity, low risk. Can be landed independently of TLS.
- **TLS acceptor wiring:** ~200-300 lines of feature-gated code in
  `spawn_connection_worker` and `serve_connections`. Straightforward.
- **Config parsing:** ~100 lines for `ssl cert`, `ssl key`, `ssl ca`,
  `ssl port`, `ssl only` directives. Follows existing parser patterns.
- **CLI flags:** ~50 lines in the clap argument definitions.
- **Tests:** Certificate generation in test fixtures, TLS handshake round-trip
  tests, rejection tests (expired cert, wrong hostname, no client cert when
  required). ~300-400 lines.
- **Total estimate:** 700-1000 lines of new code, feature-gated.

---

## 5. Strategy C: Both (External + Native)

### Description

External TLS wrapping continues to work exactly as it does today - zero cost,
zero code changes. Additionally, native rustls adds `--ssl` / `--ssl-daemon`
flags and `oc-rsyncd.conf` directives for integrated TLS. The native path is
feature-gated behind `--features tls`, so default builds carry no TLS overhead.

This is not a compromise between A and B - it is A (which is free) plus B
(which is opt-in). Operators choose the approach that fits their deployment:

| Deployment | Recommended approach |
|------------|---------------------|
| Enterprise with existing PKI team | External proxy (HAProxy/nginx) |
| Container / single-binary | Native rustls |
| Quick testing / development | Native rustls with self-signed cert |
| Upstream rsync-ssl interop | Either (both present standard TLS on 874) |
| Air-gapped / minimal deps | External stunnel (proven, no rebuild) |

### Phased Rollout

The native TLS implementation is split into three phases to limit blast radius
and allow incremental validation:

#### Phase 1: Daemon TLS acceptor

Scope: the daemon can accept TLS connections on a configured port.

- Add `tls` feature flag to `crates/daemon/Cargo.toml`.
- Generalize stream types in the session handler chain (Option B: trait
  objects). This is a prerequisite that can land independently.
- Wire `rustls::ServerConfig` and `TlsAcceptor` in `serve_connections`.
- Insert TLS handshake in `spawn_connection_worker` between socket options
  and `handle_session`.
- Solve `try_clone()` with single-stream approach (section 4).
- Parse `ssl cert`, `ssl key`, `ssl port` from `oc-rsyncd.conf`.
- Tests: TLS handshake round-trip, greeting over TLS, auth over TLS,
  transfer over TLS, certificate rejection.

Validation: upstream `rsync-ssl` client connects to native TLS daemon and
completes a file transfer.

#### Phase 2: Client TLS connector

Scope: the client can connect to TLS-wrapped daemons natively.

- Add `--ssl` flag to the CLI.
- Wire `rustls::ClientConfig` in the client transport layer.
- `webpki-roots` for default certificate verification against Mozilla CAs.
- Environment variables for override: `RSYNC_SSL_CA_CERT`, `RSYNC_SSL_CERT`,
  `RSYNC_SSL_KEY` (matching upstream `rsync-ssl` conventions).
- Tests: client connects to native TLS daemon, client connects to
  stunnel-wrapped daemon, certificate pinning, hostname verification.

Validation: oc-rsync client with `--ssl` connects to both a native TLS daemon
and a stunnel-wrapped upstream rsync daemon.

#### Phase 3: Advanced configuration

Scope: production hardening and policy controls.

- `ssl only` directive to refuse plaintext connections.
- `ssl ca` for mutual TLS (client certificate verification).
- Certificate hot-reload on SIGHUP.
- TLS session resumption for connection-heavy workloads.
- Cipher suite and TLS version constraints in config (rustls defaults are
  already secure - TLS 1.2+ only, no RC4, no 3DES).
- Metrics: TLS handshake latency, certificate expiry warnings in logs.

### Compatibility matrix

| Client | Server | Works? | Notes |
|--------|--------|--------|-------|
| rsync-ssl (upstream) | oc-rsyncd + native TLS | Yes | rsync-ssl opens a standard TLS connection on 874 |
| rsync-ssl (upstream) | oc-rsyncd + stunnel | Yes | Existing behavior, documented in TLS-5 |
| oc-rsync --ssl | oc-rsyncd + native TLS | Yes | End-to-end native, single binary |
| oc-rsync --ssl | upstream rsyncd + stunnel | Yes | Standard TLS client connecting to stunnel |
| oc-rsync (no --ssl) | oc-rsyncd (no TLS) | Yes | Plaintext, existing behavior unchanged |
| rsync (upstream, no ssl) | oc-rsyncd + native TLS | No | Plaintext client cannot connect to TLS port |

The last row is expected - a plaintext client cannot speak TLS. If
`ssl only = false` (the default), the daemon continues to accept plaintext
on port 873 alongside TLS on port 874.

### Advantages

- **All advantages of A and B combined.** External wrapping is free.
  Native TLS is opt-in.
- **Graceful migration.** Operators already using stunnel can keep it.
  New deployments can use native TLS. No forced migration.
- **Feature-gated default safety.** Default builds are identical to today.
  The `tls` feature adds ~1-2 MB binary size only when explicitly enabled.
- **Phased risk.** Phase 1 alone delivers the highest-value feature (daemon
  acceptor) with the smallest blast radius. Phases 2-3 build incrementally.

### Disadvantages

- **Documentation for two paths.** Operators must understand that both external
  and native TLS exist. Clear guidance on when to use which mitigates this.
- **Same maintenance cost as B.** The native TLS code exists regardless of
  whether external wrapping also works.

---

## 6. Decision Criteria

| Criterion | A (External) | B (Native) | C (Both) |
|-----------|-------------|-----------|---------|
| Deployment complexity | Higher (two processes) | Lower (single binary) | Operator chooses |
| Maintenance cost | Zero | Medium (~1000 LoC, ongoing rustls updates) | Same as B |
| Time to production | Immediate (already works) | 2-3 sprints | Phase 1 in 1-2 sprints |
| Security audit scope | External tool (stunnel/HAProxy) | Integration code (~1000 LoC) | Same as B |
| Performance overhead | Negligible (stunnel is fast) | Negligible (rustls is fast) | Same as B |
| Binary size impact | None | ~1-2 MB when enabled | Same as B |
| Default build impact | None | None (feature-gated) | None (feature-gated) |
| Upstream compatibility | Perfect | Perfect (TLS terminates before protocol) | Perfect |
| rsync-ssl interop | Yes (stunnel endpoint) | Yes (standard TLS endpoint) | Yes (either path) |
| Container deployment | Needs sidecar | Single image | Operator chooses |
| Certificate management | Split across configs | Unified in oc-rsyncd.conf | Operator chooses |
| Competitive advantage | None (matches upstream) | Unique in rsync ecosystem | Same as B |

### Performance comparison

Both stunnel and rustls add negligible overhead relative to the file transfer
workload:

- **TLS 1.3 handshake:** 1-3 ms (one round-trip).
- **Symmetric encryption throughput:** AES-256-GCM on modern x86 with AES-NI
  exceeds 5 GB/s. ChaCha20-Poly1305 on ARM exceeds 1 GB/s. Both are orders
  of magnitude faster than network I/O or disk I/O.
- **Per-record overhead:** 5 bytes (TLS record header) + 16 bytes (AEAD tag)
  per ~16 KB record. Less than 0.2% bandwidth overhead.

stunnel uses OpenSSL; rustls uses ring (or aws-lc-rs). Both are
hardware-accelerated on x86 and ARM. The performance difference between them
is not measurable in an rsync workload where disk I/O and network latency
dominate.

---

## 7. Recommendation

**Strategy C (Both) with phased rollout.**

### Rationale

1. **External wrapping is free.** It works today (TLS-5). There is no reason
   to remove or discourage it. Operators with existing stunnel/HAProxy
   infrastructure should keep using it.

2. **Native TLS removes the largest adoption barrier.** Users who skip
   encryption because stunnel is "one more thing to deploy" will encrypt
   when it is a single flag in the config they already have.

3. **Feature gating eliminates risk to default builds.** The `tls` feature
   follows the established pattern (`sd-notify`, `async`, `xattr`, `acl`).
   Default builds compile and behave identically to today.

4. **rustls is the right choice for native TLS.** Pure Rust, no C FFI, no
   OpenSSL linking, GPLv3-compatible license, hardware-accelerated
   cryptography, maintained by a dedicated team with a strong security
   track record. It avoids the GPL/OpenSSL license conflict that historically
   blocked native TLS in upstream rsync (TLS-1, section 5).

5. **The insertion point is clean.** TLS-2 confirmed that `spawn_connection_worker`
   is the right place. The transfer engine is already generic over Read + Write.
   The `try_clone()` obstacle has a known solution. The stream generalization
   (trait objects) can land as a standalone refactor before any TLS code.

6. **Phased rollout limits blast radius.** Phase 1 (daemon acceptor) delivers
   the highest value - operators can point `rsync-ssl` at a native TLS
   daemon - with the smallest code footprint. Phases 2-3 are independent and
   can be deferred or reprioritized based on user demand.

### Suggested implementation order

1. **Pre-TLS refactor:** Generalize the session handler chain from
   `TcpStream` to `&mut dyn Read + Write`. This is a pure refactor with no
   behavioral change and no new dependencies. It unblocks Phase 1 and
   improves code quality independently.

2. **Phase 1:** Daemon TLS acceptor. Feature-gated, ~500 lines. Validates
   with upstream `rsync-ssl`.

3. **Phase 2:** Client TLS connector. Feature-gated, ~300 lines. Validates
   with both native and stunnel-wrapped servers.

4. **Phase 3:** Production hardening. mTLS, hot-reload, metrics. Driven by
   operator feedback.

### What not to do

- **Do not make TLS the default.** The daemon should remain plaintext-capable
  on port 873 by default. TLS is opt-in via config or CLI flags.
- **Do not drop external wrapping support.** External proxies are the right
  answer for large deployments with dedicated PKI teams.
- **Do not implement PROXY protocol as part of TLS.** PROXY protocol
  (rsyncd.conf `proxy protocol` directive) is orthogonal to TLS and should
  be tracked separately. It works with both external and native TLS.
- **Do not add OpenSSL as a dependency.** rustls covers all requirements
  without the licensing complexity, build complexity, or platform-specific
  linking that OpenSSL entails.

---

## 8. Open Questions

| Question | Status | Notes |
|----------|--------|-------|
| Should the `tls` feature be enabled in release binaries? | Deferred to Phase 1 | Likely yes for convenience, but adds ~1-2 MB |
| Should `ssl only = true` disable the plaintext listener entirely? | Design needed | Implies a single-port daemon; simplifies firewall rules |
| Should the daemon auto-detect TLS vs plaintext on a single port? | Probably not | TLS ClientHello starts with 0x16; rsync greeting starts with `@`. Detection is possible but adds complexity for marginal benefit |
| How should certificate errors be reported? | Phase 1 | Log at ERROR level with the cert path, expiry date, and error. Exit with a clear message if the cert is unreadable at startup |
| Should we support ACME (Let's Encrypt) natively? | Out of scope | Use certbot or a proxy. Native ACME is a large surface area |

---

## Appendix A: Dependency License Summary

| Crate | License | GPLv3 compatible | Notes |
|-------|---------|-----------------|-------|
| `rustls` | Apache-2.0 / ISC / MIT | Yes | Core TLS implementation |
| `rustls-pemfile` | Apache-2.0 / ISC / MIT | Yes | PEM parsing |
| `webpki-roots` | MPL-2.0 | Yes | Mozilla CA bundle |
| `ring` (rustls dep) | ISC-style | Yes | Cryptographic primitives |
| `aws-lc-rs` (alt rustls backend) | Apache-2.0 / ISC | Yes | AWS-maintained crypto, optional |

No dependency introduces a GPL-incompatible license. No dependency requires
C compilation or OpenSSL linking. All are pure Rust or include pre-compiled
ASM via `ring`.

## Appendix B: Upstream rsync TLS Timeline

| Year | Event |
|------|-------|
| ~2008 | `openssl-support.diff` patch attempted; abandoned as non-functional |
| 2013 | rsync 3.1.0 ships `rsync-ssl` helper script and `stunnel-rsyncd.conf` |
| 2020 | rsync 3.2.0 adds OpenSSL and GnuTLS backends to `rsync-ssl`; PROXY protocol support; reverse proxy recommended |
| 2025 | rsync 3.4.x: no change to TLS strategy; proxy approach remains recommended |

Upstream's position is clear and stable: TLS belongs outside the rsync binary.
oc-rsync respects this by keeping external wrapping as the documented default
while offering native TLS as an opt-in enhancement for deployments that
benefit from single-binary simplicity.

## Appendix C: Related Documents

- [TLS-1: Upstream stunnel/SSL integration model](../audit/upstream-stunnel-integration.md)
- [TLS-2: Daemon crate TLS insertion points](../audit/daemon-tls-insertion-points.md)
- [TLS-5: Stunnel/TLS wrapping user guide](../user/daemon-tls-wrapping.md)
- [Daemon TLS-in-front deployment recipes](../deployment/daemon-tls.md)
- [Daemon async accept loop design](daemon-async-accept-sync-workers.md)
