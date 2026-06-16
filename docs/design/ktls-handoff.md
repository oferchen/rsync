# Linux kTLS Key Handoff for `daemon-tls` (NET-KTLS.1 / NET-KTLS.2)

Status: Design
Audience: daemon and transport maintainers
Scope: zero-copy file-serving path for the `daemon-tls` feature on Linux
Prerequisites: `daemon-tls` feature shipped (see `docs/design/daemon-tls-strategy.md`)
Related: NET-KTLS.3..6 (implementation tasks - see roadmap below)

---

## 1. Motivation

The `daemon-tls` feature (PR #3625, behind Cargo flag) uses rustls to terminate
TLS in userspace. Every record on the daemon side traverses this path on send:

1. `DaemonStream::Tls::write(buf)` -> rustls `StreamOwned::write`
2. rustls AEAD seal in userspace (AES-GCM or ChaCha20-Poly1305) - `memcpy` plus
   AES round computation per record
3. `TcpStream::write(ciphertext)` - one `write(2)` per ~16 KiB record

For the file-serving daemon path the input is already a file fd. `fast_io::
sendfile::send_file_to_fd_with_policy` (`crates/fast_io/src/sendfile/`) lets
the daemon push file bytes straight from the page cache to a socket via
`sendfile(2)`, skipping userspace entirely - but only for **plain TCP**. With
the TLS variant, every byte must round-trip through rustls so AEAD can run.

Linux 4.13 introduced **kTLS** (kernel TLS): once TLS handshake has produced
session keys, the application calls `setsockopt(IPPROTO_TLS, TLS_TX, &key)`
and subsequent `write(2)` / `sendfile(2)` / `splice(2)` calls are AEAD-sealed
inside the kernel. The userspace path collapses to:

1. `DaemonStream::KTls::write_file(file_fd, len)` -> `sendfile(file_fd, sock_fd, len)`
2. Kernel reads from page cache, seals into TLS records, writes to socket -
   no user/kernel data copy

Expected gains on the daemon read-only file-serving workload, based on
published kTLS benchmarks (Netflix, Cloudflare):

- CPU: ~30-50% reduction on the daemon process for large-file serving
- Throughput: line-rate possible on 10 GbE/25 GbE NICs that previously
  bottlenecked on userspace AES
- Memory: one fewer ~16 KiB ciphertext buffer per write

**Upstream baseline:** upstream rsync 3.4.x has no native TLS - the
`rsync-ssl` wrapper invokes stunnel/openssl externally. kTLS is an
oc-rsync-specific perf improvement and is **wire-compatible** (peers see
the same TLS records and the same rsync protocol underneath; only our
send path changes).

**Scope guard - what kTLS is *not* for:**

- The control-channel paths (greeting, motd, module list, auth) stay on the
  rustls path. They are short, mixed read/write, and never hit `sendfile`.
- Receive side (`TLS_RX`) is lower priority and gated behind NET-KTLS.5.
  Daemons mostly *serve* data; the high-volume direction is TX.
- Non-Linux platforms get no kTLS - macOS/BSD/Windows continue to use the
  userspace rustls path. The fallback is mandatory, not optional.

---

## 2. Linux kTLS kernel API

### 2.1 Socket setup

```text
setsockopt(fd, SOL_TCP, TCP_ULP, "tls", 4)         // attach kTLS ULP
setsockopt(fd, SOL_TLS, TLS_TX, &crypto_info, len) // install TX keys
setsockopt(fd, SOL_TLS, TLS_RX, &crypto_info, len) // optional, install RX keys
```

`TCP_ULP` must succeed before either `TLS_TX` or `TLS_RX`. After `TLS_TX`
is installed, every `write(2)` / `writev(2)` / `sendfile(2)` / `splice(2)`
on the socket emits TLS records.

### 2.2 Crypto info structs

From `<linux/tls.h>`. Each ciphersuite has its own struct prefixed by a
common header. Layouts are stable kernel ABI - we encode them ourselves
to avoid a `libc` dependency on the exact set of constants.

```c
struct tls_crypto_info {
    __u16 version;     // TLS_1_2_VERSION (0x0303) or TLS_1_3_VERSION (0x0304)
    __u16 cipher_type; // TLS_CIPHER_AES_GCM_128, etc.
};

struct tls12_crypto_info_aes_gcm_128 {
    struct tls_crypto_info info;
    unsigned char iv[8];       // TLS_CIPHER_AES_GCM_128_IV_SIZE
    unsigned char key[16];     // TLS_CIPHER_AES_GCM_128_KEY_SIZE
    unsigned char salt[4];     // TLS_CIPHER_AES_GCM_128_SALT_SIZE
    unsigned char rec_seq[8];  // TLS_CIPHER_AES_GCM_128_REC_SEQ_SIZE
};
```

We will mirror these as `#[repr(C)]` Rust structs inside `fast_io::ktls`
(the only crate permitted to host the `setsockopt` FFI under the workspace
unsafe-code policy). Cipher / version constants:

| Constant | Value | Notes |
|---|---|---|
| `TLS_1_2_VERSION` | 0x0303 | |
| `TLS_1_3_VERSION` | 0x0304 | |
| `TLS_CIPHER_AES_GCM_128` | 51 | TLS 1.2 + 1.3 |
| `TLS_CIPHER_AES_GCM_256` | 52 | TLS 1.2 + 1.3 |
| `TLS_CIPHER_CHACHA20_POLY1305` | 54 | Linux 5.11+ |
| `SOL_TLS` | 282 | Linux ABI |
| `TLS_TX` | 1 | |
| `TLS_RX` | 2 | |

### 2.3 Kernel version feature matrix

| Kernel | TLS_TX | TLS_RX | ChaCha20 | TLS 1.3 | Notes |
|---|---|---|---|---|---|
| 4.13 | yes | no | no | no | TX only, TLS 1.2 AES-GCM-128 |
| 4.17 | yes | yes | no | no | RX added |
| 5.1 | yes | yes | no | yes | TLS 1.3 TX |
| 5.2 | yes | yes | no | yes | TLS 1.3 RX |
| 5.11 | yes | yes | yes | yes | ChaCha20-Poly1305 |
| 6.x | yes | yes | yes | yes | + TLS 1.2 / 1.3 AES-CCM, NIC offload |

We target Linux 5.1+ for the first cut (TLS 1.3 + AES-GCM-{128,256}) and
graceful fall-through to userspace rustls everywhere else. ChaCha20 and RX
land behind their own gates (NET-KTLS.5).

### 2.4 Failure modes

- `ENOENT` on `TCP_ULP="tls"` -> kernel built without `CONFIG_TLS`. Fall
  back to userspace rustls. Log once per daemon process at WARN.
- `EBUSY` on `TLS_TX` -> ULP already installed (e.g. reattach attempt).
  Treat as a programming error - abort the connection.
- `EINVAL` on `TLS_TX` -> unsupported ciphersuite or version. Fall back to
  userspace rustls for *this* connection only.
- After kTLS is installed, an `EBADMSG` from `read(2)` indicates AEAD
  authentication failure: tear down the connection.

---

## 3. rustls key extraction API

rustls (>=0.21) exposes session keys via `dangerous_extract_secrets`:

```rust
// Cargo: rustls = { version = "0.23", features = ["secret_extraction"] }
let conn: rustls::ServerConnection = /* post-handshake */;
let extracted: rustls::ExtractedSecrets = conn.dangerous_extract_secrets()?;

// ExtractedSecrets holds: (tx_seq, tx_keys) and (rx_seq, rx_keys)
let (tx_seq, tx_keys) = extracted.tx;
let (rx_seq, rx_keys) = extracted.rx;

// tx_keys / rx_keys is a rustls::ConnectionTrafficSecrets enum:
//   Aes128Gcm { key, iv } | Aes256Gcm { key, iv } | Chacha20Poly1305 { key, iv }
```

### 3.1 Cargo wiring

The workspace pin (`Cargo.toml:314`) currently has:

```toml
rustls = { version = "0.23", default-features = false,
           features = ["ring", "logging", "std", "tls12"] }
```

NET-KTLS.3 will add `"secret_extraction"` to the daemon-tls feature only,
keeping default builds free of the dangerous API:

```toml
# crates/daemon/Cargo.toml
daemon-tls       = ["dep:rustls", "dep:rustls-pemfile"]
daemon-tls-ktls  = ["daemon-tls", "rustls/secret_extraction",
                    "fast_io/ktls"]    # Linux-only at the build layer
```

### 3.2 Safety of `dangerous_extract_secrets`

rustls names it `dangerous_*` because:

1. **Single use.** The method consumes the secrets; rustls can no longer
   encrypt or decrypt on the same connection after extraction. This is
   exactly the contract we want: rustls hands the keys to the kernel,
   then we never call rustls `write`/`read` on this connection again.
2. **No rekey.** TLS 1.3 KeyUpdate is *not* delivered through the
   extracted-secrets path. NET-KTLS.4 will disable rustls KeyUpdate by
   configuring `ServerConfig::send_tls13_tickets = 0` and ignore the
   theoretical case for our daemon traffic profile (short-lived
   connections, no resumption requirement). If a connection ever needs
   rekey, we abort it.
3. **Key material leaves userspace into the kernel.** This is the same
   threat model the kernel kTLS code itself was built around. The keys
   end in kernel slab memory, are zeroed on socket close, and are not
   reachable from other userspace processes.

The workspace unsafe-code policy scopes raw FFI to `fast_io`. The
`fast_io::ktls` module will own the `setsockopt` calls; `daemon::tls`
calls only the safe `KtlsSocket::install_tx(..)` wrapper.

---

## 4. Architecture

### 4.1 Current path (today)

```
TcpStream  --accept-->  rustls::ServerConnection
                         |  handshake, AEAD in userspace
                         v
                  rustls::StreamOwned
                         |  Read + Write
                         v
                  DaemonStream::Tls   --[per-connection worker]-->
                         |  every byte copied + AEAD'd in user
                         v
                  TcpStream  -- write(2) -- NIC
```

### 4.2 kTLS path (proposed)

```
TcpStream  --accept-->  rustls::ServerConnection
                         |  handshake (TLS 1.3 ClientHello/ServerHello/Finished)
                         v
            +------------+------------+
            |                         |
            |  rustls::dangerous_extract_secrets()
            |                         |
            v                         v
   ExtractedSecrets             pending pre-kTLS plaintext from rustls
   (tx, rx, seq)                (drained to TcpStream BEFORE keys installed)
            |
            v
   fast_io::ktls::install_tx(&TcpStream, ExtractedSecrets.tx)
            |  setsockopt TCP_ULP="tls" + TLS_TX(crypto_info)
            v
   DaemonStream::KTls(TcpStream)   <-- post-kTLS, rustls is OUT of the path
            |
            v
   write(2) / sendfile(2) -- kernel AEAD -- NIC
```

### 4.3 Sequence

```mermaid
sequenceDiagram
  participant C as Client
  participant A as Accept loop
  participant R as rustls::ServerConnection
  participant K as fast_io::ktls
  participant S as TcpStream / kernel
  participant F as File fd

  C->>A: TCP SYN, ClientHello
  A->>R: ServerConnection::new(cfg)
  R-->>C: ServerHello, certs, Finished
  C-->>R: client Finished
  R-->>A: is_handshaking() == false
  A->>R: dangerous_extract_secrets()
  R-->>A: ExtractedSecrets { tx, rx, seq }
  A->>S: drain any pending rustls plaintext to socket
  A->>K: install_tx(sock_fd, tx, seq.tx)
  K->>S: setsockopt(TCP_ULP="tls"), setsockopt(TLS_TX)
  K-->>A: KtlsSocket
  Note over A,S: rustls dropped; DaemonStream::KTls wraps sock
  A->>F: open data file
  A->>S: sendfile(sock_fd, file_fd, len)
  S->>C: TLS records (kernel-sealed)
```

### 4.4 Integration points

| File | Change |
|---|---|
| `crates/fast_io/src/lib.rs` | add `pub mod ktls;` (Linux only, behind `ktls` feature) |
| `crates/fast_io/src/ktls/` | new: `mod.rs`, `crypto_info.rs`, `setsockopt.rs`, `probe.rs`, `tests.rs` |
| `crates/daemon/Cargo.toml` | add `daemon-tls-ktls` feature wiring `fast_io/ktls` and `rustls/secret_extraction` |
| `crates/daemon/src/tls.rs` | new `try_upgrade_to_ktls(server_conn, tcp) -> KtlsOrFallback` |
| `crates/daemon/src/daemon_stream.rs` | add `KTls(TcpStream)` variant gated on `daemon-tls-ktls` |
| `crates/daemon/src/daemon/sections/server_runtime/connection.rs` | call `try_upgrade_to_ktls` after `wrap_stream`; drop rustls on success, keep on fallback |

The `DaemonStream::KTls` variant implements `Read + Write` straight on
`TcpStream`. For the sendfile path, the per-connection worker will check
`stream.is_ktls()` and dispatch through `fast_io::sendfile::
send_file_to_fd_with_policy(file, sock_fd, len)` directly - this is
already on the daemon hot path for plain TCP; kTLS lets us reuse it
unchanged.

### 4.5 Error handling and fallback

Three boundaries, three behaviours:

1. **Build time.** `daemon-tls-ktls` is Linux-only (`#[cfg(target_os
   = "linux")]` on the entire module). Non-Linux builds compile the
   feature to a no-op stub returning `KtlsUnsupported` so callers can
   share the same code shape across platforms.
2. **Probe time (per daemon process).** On first connection we
   `OnceLock`-cache the result of a probe `setsockopt(TCP_ULP="tls")`
   on a throwaway socket. On `ENOENT` we mark kTLS unavailable for the
   whole process and skip the upgrade. Logged once at WARN.
3. **Per connection.** If `dangerous_extract_secrets` returns an
   unsupported ciphersuite, or `setsockopt(TLS_TX)` fails for any
   reason other than `ENOENT`, log at INFO, leave the connection on
   the userspace rustls path, continue.

Critical invariant: rustls MUST have no buffered outbound plaintext when
we hand off keys. NET-KTLS.4 will drain rustls write buffers with
`conn.writer().flush()` and call `complete_io(tcp_stream)` until
`conn.wants_write() == false` before invoking `setsockopt(TLS_TX)`. A
single TLS record straddling the boundary would corrupt the sequence
number and abort the connection on the peer.

---

## 5. Security considerations

- **Key material leaves the rustls allocator.** `dangerous_extract_secrets`
  is `dangerous` because rustls can no longer rotate, KeyUpdate, or close
  the session cryptographically. We accept this: the kernel takes over.
  The keys do not appear in any other userspace allocation - we move the
  `ExtractedSecrets` struct directly into the `setsockopt` call and drop
  it. NET-KTLS.4 will `zeroize` the staging struct on drop.
- **No KeyUpdate.** TLS 1.3 KeyUpdate is incompatible with extracted
  secrets. The daemon will not advertise support; if a client sends one,
  the kernel TLS layer returns `EBADMSG` and we tear down. Rsync sessions
  are short-lived and below the 2^36 byte rekey threshold of AES-GCM.
- **No 0-RTT.** Early data flows through rustls before secrets are
  available; we explicitly disable it (`ServerConfig::max_early_data_size
  = 0`, already the default in our build).
- **Audit surface.** Adding `setsockopt(SOL_TLS, ...)` is a new kernel
  attack surface, but it is the same one Netflix, Cloudflare and nginx
  already use in production. We do not introduce new C code.
- **Unsafe code budget.** All FFI lives in `fast_io::ktls` (the workspace
  unsafe-code policy already permits `fast_io` to host raw FFI).
  `daemon::tls` stays safe.
- **Heap residue.** rustls 0.23 does not implement `Zeroize` on
  `ConnectionTrafficSecrets`. NET-KTLS.4 wraps the extracted struct in
  a small `KeyMaterial(zeroize::Zeroizing<[u8; ..]>)` and clears it on
  drop.

---

## 6. Implementation roadmap

| Task | Scope | Effort |
|---|---|---|
| **NET-KTLS.3** | rustls `secret_extraction` wiring + `KeyMaterial` wrapper with `Zeroize`. Drain rustls write buffers. Unit test against a real handshake. | S |
| **NET-KTLS.4** | `fast_io::ktls` module: `crypto_info` repr-C structs, `install_tx`, `probe`, `OnceLock` capability cache. `DaemonStream::KTls` variant. Wire `try_upgrade_to_ktls` from `connection.rs`. | M |
| **NET-KTLS.5** | `TLS_RX` install. Use only when daemon is in receive-heavy mode (`--write-batch` over TLS, push uploads). Same fallback policy. | M |
| **NET-KTLS.6** | Benchmark plan (section 7), publish results, decide on default-on-Linux gating. | S |

Each task is one PR with `feat:` or `perf:` prefix as appropriate. The
feature stays opt-in via `daemon-tls-ktls` through NET-KTLS.5 and is
considered for default-on under NET-KTLS.6 based on bench data and
kernel-version field reports.

---

## 7. Benchmark plan (NET-KTLS.6)

Goal: prove the userspace AES cost is the bottleneck and that kTLS
removes it, *without* regressing small-message control traffic.

### 7.1 Workloads

| Name | Corpus | Daemon role | Expected effect |
|---|---|---|---|
| `bulk-tls-pull` | 4 GiB single file, page-cache hot | sender | kTLS dominant - target -30% CPU, +throughput |
| `mixed-tls-pull` | 1k files, 4 KiB-2 MiB mix, real distribution | sender | partial gain; sendfile fraction matters |
| `daemon-handshake` | 1k short pulls of a 1 KiB file | sender | baseline; should not regress |
| `bulk-tls-push` | 4 GiB upload (NET-KTLS.5) | receiver | RX kTLS gain |

### 7.2 Harness

Reuse `scripts/benchmark.sh` and `scripts/benchmark_hyperfine.sh`. Both
sides on the same host (loopback), then repeat on the `rsync-profile`
container against a remote host with 10 GbE. Compare three builds:

1. `daemon-tls` (userspace rustls, today)
2. `daemon-tls-ktls` (new path)
3. plaintext daemon (lower bound)

Metrics: wall time, peak RSS, daemon-process CPU time (`getrusage`),
sendfile call count from `strace -c`, NIC throughput from `ifstat`.

### 7.3 Acceptance

- `bulk-tls-pull`: >=20% CPU reduction, >=10% throughput gain vs userspace
  TLS, within 5% of plaintext throughput.
- `daemon-handshake`: no regression beyond noise (1%).
- All workloads on a 4.18 (RHEL 8 - no TLS_TX) kernel: fall-through
  succeeds, results identical to userspace TLS path.

---

## 8. Out of scope (deferred)

- NIC TLS offload (Mellanox/Intel hardware encrypt) - kernel exposes the
  same `setsockopt` API; we get it for free if the NIC advertises it,
  but we will not validate or recommend.
- BoringSSL / OpenSSL kTLS variants - we are rustls-only.
- macOS Network Framework TLS offload - non-Linux platforms keep
  userspace rustls. No equivalent on macOS today.
- Windows Schannel kTLS - no equivalent.
- TLS 1.2 AES-CBC modes - not supported by Linux kTLS; not in rustls
  default ciphersuites either.

---

## 9. Decision log

- **2026-06-16**: design accepted, implementation tracked under NET-KTLS.3..6.
  Default-on policy deferred until NET-KTLS.6 bench data is in hand.
