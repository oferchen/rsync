# NET-KTLS.2 - rustls -> Kernel TLS Key Handoff Design

Status: Design only. No code lands in this PR. Parent: NET-KTLS (#4257).
Predecessor: NET-KTLS.1 audit at `docs/design/net-ktls-audit.md`.
Companion specs: `docs/design/ktls-handoff.md` (broader architecture),
`docs/design/ktls-key-extraction.md` (extraction + zeroization in NET-KTLS.3).

This document narrowly scopes the **handoff contract** between the rustls
post-handshake state and the Linux kernel TLS ULP: the exact API path, the
cipher-suite gate, the `setsockopt` ordering, and the failure-mode matrix.
Implementation lands under NET-KTLS.3 (extract), NET-KTLS.4 (TX wire-up),
NET-KTLS.5 (RX wire-up), NET-KTLS.6 (bench).

## 1. Goal

After a `daemon-tls` handshake completes on a `TcpStream`, install the
negotiated session keys into the kernel so subsequent `write(2)` /
`sendfile(2)` / `splice(2)` calls emit AEAD-sealed TLS records without a
userspace copy. Userspace rustls remains the fallback path on any failure
along the chain. See NET-KTLS.1 audit section 1 for motivation and section
2.1 for the Linux kernel support matrix.

## 2. Insertion site in the current daemon

The `daemon-tls` integration lives at `crates/daemon/src/tls.rs`. The
relevant call sites on `master`:

- Acceptor build: `crates/daemon/src/tls.rs:87` (`build_tls_acceptor`).
  ServerConfig is constructed here; NET-KTLS.3 will set
  `enable_secret_extraction = true` next to the existing
  `with_safe_default_protocol_versions` chain.
- Per-connection handshake: `crates/daemon/src/tls.rs:131` (`wrap_stream`).
  Returns a `rustls::StreamOwned<ServerConnection, TcpStream>` today;
  NET-KTLS.3 expands this to also expose the extracted secrets when
  enabled.
- Dispatch seam: `crates/daemon/src/daemon/sections/server_runtime/connection.rs:297`
  (`wrap_accepted_stream`). The branch that calls `wrap_stream` is where
  NET-KTLS.4 inserts the kTLS attach attempt before producing the
  per-connection `DaemonStream`.

These sites are confirmed against the audit doc's Section 5 site map.

## 3. Key-extraction API path

rustls 0.23 (workspace pin in `Cargo.toml`) exposes session secrets via
the explicit, intentionally awkward `dangerous_extract_secrets` method on
`ConnectionCommon`, gated by the `secret_extraction` Cargo feature.

```rust
// gated by rustls feature `secret_extraction`
let extracted: rustls::ExtractedSecrets =
    server_conn.dangerous_extract_secrets()?;
let rustls::ExtractedSecrets { tx, rx } = extracted;
// tx and rx are (u64, ConnectionTrafficSecrets)
```

`ConnectionTrafficSecrets` is an enum keyed on the negotiated cipher,
carrying the AEAD `key`, the `iv`, and (GCM only) the 4-byte `salt`
implicit-nonce prefix. The companion `u64` carries the starting record
sequence number for that direction; the kernel takes over counting from
that point.

`dangerous_extract_secrets` consumes the secrets - subsequent rustls
`Read`/`Write` on the same connection error. The handoff therefore
happens exactly once per connection. The full feature-flag wiring and
zero-on-drop staging belong to NET-KTLS.3 and are specified in
`docs/design/ktls-key-extraction.md`.

## 4. Cipher-suite eligibility gate

Linux kTLS accepts a strict subset of TLS 1.2 / 1.3 AEAD suites; rustls
can negotiate ciphers outside that subset. The handoff path must enumerate
the extracted `ConnectionTrafficSecrets` variant and skip the attempt for
anything off the kernel allow-list:

| rustls variant         | Kernel cipher_type             | Min kernel |
| ---------------------- | ------------------------------ | ---------- |
| `Aes128Gcm`            | `TLS_CIPHER_AES_GCM_128`       | 4.13       |
| `Aes256Gcm`            | `TLS_CIPHER_AES_GCM_256`       | 4.17       |
| `Chacha20Poly1305`     | `TLS_CIPHER_CHACHA20_POLY1305` | 5.11       |

Any other variant (future post-quantum AEADs, AES-CCM-8, ARIA, etc.) is a
hard skip: log once at `INFO`, leave the userspace rustls path in place,
continue the connection.

The TLS protocol version is also gated. The first cut programs only
TLS 1.2 to dodge TLS 1.3 KeyUpdate and 0-RTT complications - see NET-KTLS.1
Risk Register entry 1 and `docs/design/ktls-handoff.md` Section 3.2 / 5.

## 5. Handoff sequence

The kernel attach is a strict three-step `setsockopt` sequence. Any step
that fails reverts the connection to the userspace rustls path; once
`TLS_TX` succeeds the connection is committed.

```text
1. setsockopt(fd, SOL_TCP, TCP_ULP, "tls", 4)
     - attach the kTLS Upper-Layer Protocol; must succeed first.

2. setsockopt(fd, SOL_TLS, TLS_TX, &crypto_info_tx, sizeof(*tx))
     - program the send-side AEAD key, iv, salt, and starting rec_seq.
     - after this, write(2) / sendfile(2) emit native TLS records.

3. setsockopt(fd, SOL_TLS, TLS_RX, &crypto_info_rx, sizeof(*rx))   [NET-KTLS.5]
     - program the receive-side keys; read(2) returns plaintext.
```

The crypto-info struct layout (`tls12_crypto_info_aes_gcm_128` and
variants) is fully specified in `docs/design/ktls-handoff.md` Section 2.2
and the audit doc Section 3.2; this doc does not duplicate it.

Critical invariant before step 2: rustls MUST have no buffered outbound
plaintext. NET-KTLS.4 will flush the rustls writer and call
`complete_io(tcp_stream)` until `conn.wants_write() == false` before the
`TLS_TX` `setsockopt`. A TLS record straddling the user/kernel boundary
would corrupt the sequence counter and abort the peer with a fatal alert.

## 6. Error handling

Every `setsockopt` call is best-effort and reversible until a direction is
programmed. The dispatch contract is:

| Stage                       | errno                | Action                                                                         |
| --------------------------- | -------------------- | ------------------------------------------------------------------------------ |
| `TCP_ULP="tls"`             | `ENOENT`/`ENOPROTOOPT` | Kernel < 4.13 or `tls.ko` not loaded. Log once at WARN. Userspace fallback.    |
| `TCP_ULP="tls"`             | `EBUSY`/`EEXIST`     | ULP already attached. Log at WARN. Userspace fallback.                        |
| `TCP_ULP="tls"`             | `EACCES`             | Module autoload denied (locked-down kernel). Log INFO. Userspace fallback.    |
| `dangerous_extract_secrets` | `HandshakeNotComplete` | Programming error. Abort the connection.                                       |
| `dangerous_extract_secrets` | other                | Unsupported / failed extraction. Log INFO. Userspace fallback.                |
| `TLS_TX`                    | `EINVAL`             | Cipher-mapping bug. `debug_assert!`. Abort.                                    |
| `TLS_TX`                    | `EOPNOTSUPP`/`ENOTSUPP` | Kernel built without selected cipher. Log INFO. Userspace fallback.        |
| `TLS_RX` (after `TLS_TX`)   | any                  | Already committed to kTLS TX. Abort connection - cannot roll back cleanly.    |

Any error BEFORE the first direction commits is recoverable. Any error
AFTER terminates the session. Detailed errno mapping mirrors
`docs/design/ktls-key-extraction.md` Section 6.

## 7. Failure-mode matrix (kernel coverage)

Drawn directly from NET-KTLS.1 audit Section 2.1:

| Kernel range  | TLS_TX | TLS_RX | Behaviour                                                |
| ------------- | ------ | ------ | -------------------------------------------------------- |
| < 4.13        | no     | no     | `TCP_ULP` fails `ENOENT`. Userspace rustls path only.    |
| 4.13 - 4.16   | yes    | no     | NET-KTLS.4 wins; NET-KTLS.5 skipped, kernel rejects RX.  |
| 4.17 - 5.18   | yes    | yes    | Both NET-KTLS.4 and .5 attach. AES-GCM only pre-5.11.    |
| 5.19+         | yes    | yes    | All three audited ciphers; full feature set.             |

Detection is per-process: NET-KTLS.4 will `OnceLock`-cache the result of a
probe `setsockopt(TCP_ULP="tls")` on a throwaway socket. The probe runs
once at first daemon connection; the cached verdict guards every
subsequent attach attempt.

## 8. Cross-platform stance

Linux is the only target. FreeBSD has an ABI-compatible kTLS (audit
Section 2.2) but a different `SOL_TLS` value and is deferred. macOS and
Windows have no equivalent (audit Section 2.3) - the userspace rustls
path remains the only option, with zero observable behavioural change.
The module is gated `#[cfg(target_os = "linux")]`; non-Linux builds get a
no-op stub returning `KtlsUnsupported` so call sites compile unchanged.

## 9. Home module decision

**Chosen: `crates/fast_io/src/ktls.rs`**, exposing a safe `attach_ktls(fd,
secrets) -> io::Result<KtlsSocket>` API consumed from
`crates/daemon/src/tls.rs`.

Justification:

- **Unsafe-code policy.** The workspace policy (CLAUDE.md unsafe section,
  `crates/daemon/Cargo.toml` deny-list) forbids unsafe in `daemon`.
  `setsockopt` with `repr(C)` crypto-info structs is unavoidably raw FFI.
  `fast_io` already hosts kernel-platform glue (io_uring, sendfile,
  splice, IOCP), is listed as a permitted unsafe site, and has the
  established pattern of safe wrappers around raw socket syscalls.
- **Shape match.** `fast_io::sendfile` and `fast_io::splice` already own
  the file-fd-to-socket fast path that kTLS exists to unlock. Co-locating
  `fast_io::ktls` lets NET-KTLS.4 chain `ktls::attach` directly into the
  existing sendfile dispatch without an extra crate boundary.
- **Daemon stays high-level.** `crates/daemon/src/tls.rs` keeps the
  rustls integration and the `enable_secret_extraction` config flag.
  After extraction, daemon hands the `ExtractedSecrets` to
  `fast_io::ktls::attach_ktls`. The split mirrors how `daemon` calls
  `fast_io::sendfile` today without knowing about raw FDs.

The alternative `crates/daemon/src/tls/ktls_handoff.rs` was considered and
rejected: it would require adding `#[allow(unsafe_code)]` to the daemon
crate, breaking the unsafe-code policy. Refactoring would only push the
unsafe back into `fast_io` later. Better to land it there from day one.

Concretely the new layout under NET-KTLS.3/.4 is:

- `crates/fast_io/src/ktls/mod.rs` - public `attach_ktls`, `KtlsSocket`,
  `KtlsError`. Linux-gated; non-Linux stub returns `KtlsUnsupported`.
- `crates/fast_io/src/ktls/sys.rs` - `#[repr(C)]` `tls12_crypto_info_*`
  structs, `setsockopt` FFI, SOL_TLS / TCP_ULP constants.
- `crates/fast_io/src/ktls/attach.rs` - cipher-suite gate, rustls
  `ConnectionTrafficSecrets` -> `crypto_info` mapping, attach sequence.
- `crates/fast_io/src/ktls/probe.rs` - one-shot `OnceLock` kernel probe.
- `crates/daemon/Cargo.toml` - new `daemon-tls-ktls` feature enabling
  `rustls/secret_extraction` and `fast_io/ktls`.

## 10. Follow-up tasks

- **NET-KTLS.3** - rustls secret extraction wiring. Add
  `enable_secret_extraction` in `build_tls_acceptor`; expand `wrap_stream`
  to return `ExtractedSecrets`. Detailed in
  `docs/design/ktls-key-extraction.md`.
- **NET-KTLS.4** - TX wire-up. Land `fast_io::ktls::attach_ktls`, the
  `DaemonStream::KTls(TcpStream)` variant, and the dispatch decision at
  `connection.rs:297`. Drain rustls writer first.
- **NET-KTLS.5** - RX wire-up. Program `TLS_RX` after `TLS_TX` succeeds.
  Adds the only abort-on-failure edge.
- **NET-KTLS.6** - Benchmark plan. Measure userspace-rustls vs kTLS on
  `bulk-tls-pull`, `mixed-tls-pull`, `daemon-handshake`. Acceptance
  criteria specified in `docs/design/ktls-handoff.md` Section 7.

## 11. References

- NET-KTLS.1 audit: `docs/design/net-ktls-audit.md`.
- Broader handoff architecture: `docs/design/ktls-handoff.md`.
- Extraction + zeroization: `docs/design/ktls-key-extraction.md`.
- Linux kernel: `Documentation/networking/tls.rst`,
  `include/uapi/linux/tls.h`.
- rustls 0.23: `ConnectionCommon::dangerous_extract_secrets`,
  `ExtractedSecrets`, `ConnectionTrafficSecrets`.
- RFC 5288 (AES-GCM), RFC 7905 (ChaCha20-Poly1305), RFC 8446 (TLS 1.3).
