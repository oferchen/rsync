# NET-KTLS.1 - Audit: Linux kTLS kernel x cipher x TX/RX matrix

Status: Audit only. No code lands in this PR. Feeds NET-KTLS.2 (key handoff),
NET-KTLS.3 (rustls key extraction), NET-KTLS.4 (TX wire-up), NET-KTLS.5 (RX
wire-up), NET-KTLS.6 (bench).

Companion docs:

- `docs/design/ktls-handoff.md` - per-cipher crypto_info struct field map and
  setsockopt sequence (drives NET-KTLS.2).
- `docs/design/ktls-key-extraction.md` - rustls `dangerous_extract_secrets`
  surface and key-zeroization plan (drives NET-KTLS.3).

This audit covers ONLY: kernel support matrix, ULP attach ABI, rustls feasibility
assessment, daemon-tls insertion-point map, and TLS-1.2-vs-1.3 sequencing risk.

## 1. Motivation

`daemon-tls` (PR series TLS-1..14, SHIPPED) terminates rustls in userspace.
Every encrypted byte traverses: kernel-read -> userspace-decrypt (rustls AES-GCM)
-> consumer. The mirror path holds for writes. Files served from disk over TLS
cannot use `sendfile(2)` / `splice(2)` because the data is not yet encrypted
when it leaves the page cache.

Kernel TLS (kTLS) moves AES-GCM/ChaCha20-Poly1305 record encryption into the
kernel. After the rustls handshake finishes, the application hands the
negotiated traffic keys + IVs + sequence numbers to the kernel via
`setsockopt(SOL_TLS, ...)`. Subsequent `write(2)` / `sendfile(2)` calls let the
kernel encrypt records in-place, enabling zero-copy file serving over TLS.

## 2. Kernel support matrix

### 2.1 Linux

| Cipher                  | TLS_TX kernel | TLS_RX kernel | Notes                                  |
| ----------------------- | ------------- | ------------- | -------------------------------------- |
| AES-128-GCM             | 4.13+         | 4.17+         | Original kTLS, broadest support        |
| AES-256-GCM             | 4.17+         | 4.17+         | Landed alongside TLS_RX                |
| ChaCha20-Poly1305       | 5.11+         | 5.11+         | RFC 7905; required for no-AES-NI hosts |
| AES-128-CCM             | 5.1+          | 5.1+          | Rarely used; rustls does not offer it  |

Kernel feature gate: `CONFIG_TLS=y` (module `tls`). Most distro kernels ship
this as a loadable module; `modprobe tls` is required on first use. RHEL 8
(4.18) supports AES-GCM-128/256 TX+RX but not ChaCha20.

TLS_HW (NIC offload via Mellanox CX-5+, Chelsio T6) landed in 5.x but is rarely
available in cloud environments and requires explicit NIC + driver support.
Detection: `ethtool -k <iface> | grep tls-hw-`. Out of scope for NET-KTLS.4-6;
software kTLS is the target.

### 2.2 FreeBSD

| Cipher              | TLS_TX | TLS_RX | Notes                                |
| ------------------- | ------ | ------ | ------------------------------------ |
| AES-128-GCM         | 13.0+  | 13.0+  | `kern.ipc.tls.enable=1` opt-in       |
| AES-256-GCM         | 13.0+  | 13.0+  | Same socket option ABI as Linux      |
| ChaCha20-Poly1305   | 13.0+  | 13.0+  |                                      |

Wire-compatible setsockopt surface but `SOL_TLS` constant differs; treat as a
follow-up after Linux ships.

### 2.3 macOS / Windows

No equivalent. macOS Network framework offers TLS in-kernel only as an
opaque transport (no setsockopt handoff). Windows Schannel cannot accept an
externally negotiated session. These are permanent gaps; `daemon-tls` falls
back to the userspace rustls path on these platforms with no observable
behavior change. Documented in NET-KTLS.4 as a tier-2 caveat.

## 3. Linux ULP attach pattern and crypto-info struct

### 3.1 Attach sequence

```
1. setsockopt(fd, SOL_TCP, TCP_ULP, "tls", 4)              // attach kTLS ULP
2. setsockopt(fd, SOL_TLS, TLS_TX, &crypto_info_tx, len)   // program TX keys
3. setsockopt(fd, SOL_TLS, TLS_RX, &crypto_info_rx, len)   // program RX keys
```

Step 1 must succeed before steps 2-3. After step 2, `write(fd, ...)` and
`sendfile(fd, ...)` emit TLS records natively. After step 3, `read(fd, ...)`
returns decrypted plaintext.

### 3.2 Crypto-info structs

The cipher type tag selects the struct shape. All variants begin with
`tls_crypto_info { __u16 version; __u16 cipher_type; }` and then carry:

| Cipher              | C struct                              | Key | IV  | Salt | Seq |
| ------------------- | ------------------------------------- | --- | --- | ---- | --- |
| AES-128-GCM         | `tls12_crypto_info_aes_gcm_128`       | 16  | 8   | 4    | 8   |
| AES-256-GCM         | `tls12_crypto_info_aes_gcm_256`       | 32  | 8   | 4    | 8   |
| ChaCha20-Poly1305   | `tls12_crypto_info_chacha20_poly1305` | 32  | 12  | 0    | 8   |

All sizes in bytes. The `version` field is `TLS_1_2_VERSION` (0x0303) or
`TLS_1_3_VERSION` (0x0304). The same struct layout is reused for TLS 1.3 -
only the version tag differs.

Linux kernel headers: `include/uapi/linux/tls.h`. Rust binding plan in
NET-KTLS.2 uses `libc::TCP_ULP` (already present) plus locally-declared structs
mirroring the kernel headers under `crates/daemon/src/ktls/sys.rs`.

## 4. rustls key extraction feasibility

rustls 0.23 (workspace pin: `Cargo.toml`) exposes session secrets only behind
the `dangerous_configuration` Cargo feature plus the explicit unsafe call
`ConnectionCommon::dangerous_extract_secrets()` -> `ExtractedSecrets`. Each
secret bundle yields the cipher-suite identifier plus the per-direction
`(seq, ConnectionTrafficSecrets)` tuple where `ConnectionTrafficSecrets` is
an enum keyed on the suite, exposing `key`, `iv`, and (GCM only) `salt`.

Feasibility: workable but with constraints.

1. The connection must be opted-in via
   `ServerConfig::enable_secret_extraction = true` at build time (NET-KTLS.2
   adds this beside the existing `build_tls_acceptor` path; see Section 5).
2. After handshake completion, calling `extract_secrets()` consumes the
   `ConnectionCommon`. Subsequent userspace `Read`/`Write` calls would error.
   The handoff therefore happens exactly once, between
   `wrap_stream`/handshake-complete and the connection-handler dispatch in
   `connection.rs:297` (see Section 5).
3. Extracted secrets are sensitive. Zeroize on drop is required - planned
   in NET-KTLS.3 via the `zeroize` crate's `Zeroizing<Vec<u8>>` wrapper around
   the cloned key/iv buffers before they leave the boundary function.
4. Only TLS 1.2 GCM and ChaCha20-Poly1305 cipher suites map cleanly to the
   kernel ABI in 0.23. TLS 1.3 0-RTT (early data) is not supported - rustls
   does not surface 0-RTT secrets through this API; the kernel ABI for 0-RTT
   re-keying is also not stable. NET-KTLS.4 will disable 0-RTT on the
   ServerConfig and stick to TLS 1.2 for the first cut.

## 5. Daemon-tls insertion points

Site map (file:line on `master` at branch base):

| Concern                         | File                                                                     | Line  | Action                                       |
| ------------------------------- | ------------------------------------------------------------------------ | ----- | -------------------------------------------- |
| Acceptor construction           | `crates/daemon/src/tls.rs`                                               | 87    | Enable `secret_extraction` on ServerConfig   |
| Handshake wrap                  | `crates/daemon/src/tls.rs`                                               | 131   | Return secrets alongside `StreamOwned`       |
| Per-connection dispatch         | `crates/daemon/src/connection/server_runtime/connection.rs`              | 285   | Decide kTLS-attach vs userspace-rustls path  |
| TLS wrap call site              | `crates/daemon/src/connection/server_runtime/connection.rs`              | 297   | Handoff seam (NET-KTLS.4 splices here)       |
| Daemon stream variant           | `crates/daemon/src/daemon_stream.rs`                                     | 65    | Add `KTls(TcpStream)` peer to `Tls(...)`     |
| Read/Write dispatch             | `crates/daemon/src/daemon_stream.rs`                                     | 193, 204, 213 | New `KTls` arm calls underlying TCP  |
| Config parser hook              | `crates/daemon/src/rsyncd_config/sections.rs`                            | 190   | Add `ktls = auto|on|off` directive (NET-KTLS.4) |

The file-serving hot path (sender writing flist + file payload) lives in
`crates/transfer/src/sender` and emits chunked plaintext through the
`daemon_stream::Write` impl above. Once the `KTls` variant lands, the
generator can additionally try a `sendfile(2)` fast path for the file
payload phase, bypassing the buffer pool entirely for content that already
sits in the page cache. This is the throughput win NET-KTLS.6 measures.

## 6. Risk register

1. TLS 1.3 0-RTT. rustls does not expose 0-RTT keys, and the kernel ABI for
   re-keying mid-flight is fragile. NET-KTLS.4 will pin TLS 1.2 only for the
   first cut. TLS 1.3 1-RTT (no early data) is supported and works the same
   way; gating on TLS 1.2 keeps the surface predictable.
2. Silent fallback. If `setsockopt(TCP_ULP, "tls")` returns `ENOENT` (kernel
   built without `CONFIG_TLS`) or `EOPNOTSUPP` (unsupported cipher), the
   handoff must fail loudly and fall back to userspace rustls. NET-KTLS.4
   wires a typed error and a one-shot `log::info` mirroring the IKV-F.1
   pattern already used for io_uring.
3. Key zeroization gap. Extracted key material lives in heap allocations
   that must be wiped before drop. NET-KTLS.3 uses `Zeroizing<Vec<u8>>`
   wrappers in the handoff boundary; this audit confirms no rustls API forces
   a residual copy outside that wrapper.
4. Sequence number drift. The kernel advances its sequence counter on every
   record. Userspace fallback after a kTLS attach is impossible without
   resynchronization; we treat kTLS attach as one-way for the connection
   lifetime.
5. Renegotiation. rustls does not renegotiate. Not a concern.
6. NIC offload (TLS_HW). Not pursued in NET-KTLS.4-6. Software kTLS already
   captures the sendfile-through-TLS win. NIC offload is a future task.

## 7. Sequencing into NET-KTLS.2

1. NET-KTLS.2 (handoff design - already drafted in `ktls-handoff.md`) lands
   the `crates/daemon/src/ktls/` module skeleton: `sys.rs` (crypto_info
   structs + setsockopt wrapper), `attach.rs` (cipher-suite -> crypto_info
   mapping), `mod.rs` (one-shot `attach_ktls(fd, secrets) -> io::Result<()>`).
2. NET-KTLS.3 (rustls key extraction - already drafted in
   `ktls-key-extraction.md`) wires `enable_secret_extraction` into
   `build_tls_acceptor` (`tls.rs:87`) and returns extracted secrets from
   `wrap_stream` (`tls.rs:131`).
3. NET-KTLS.4 (TX wire-up) splices the attach call between
   `wrap_stream` completion and connection-handler dispatch
   (`connection.rs:297`), adds the `KTls(TcpStream)` peer to
   `DaemonStream::Tls` (`daemon_stream.rs:65`), and threads the
   `ktls = auto|on|off` config directive (`sections.rs:190`).
4. NET-KTLS.5 (RX wire-up) programs `TLS_RX` after `TLS_TX` succeeds and
   adds a regression test covering both directions.
5. NET-KTLS.6 (bench) measures `daemon-tls` throughput with kTLS off vs on
   against upstream stunnel-fronted rsync 3.4.4.

## 8. Out of scope (deferred)

- macOS / Windows kTLS equivalents (no native ABI; permanent gap).
- TLS_HW NIC offload (Mellanox, Chelsio).
- TLS 1.3 0-RTT.
- Client-side kTLS for `rsync://` connector (NET-KTLS series targets daemon
  serving; client wins are smaller and ride on the same primitives later).
- Re-keying mid-connection (rustls does not surface it; kernel ABI fragile).

## 9. References

- Linux kernel `Documentation/networking/tls.rst`.
- Kernel uapi headers: `include/uapi/linux/tls.h`.
- rustls 0.23 `ConnectionCommon::dangerous_extract_secrets`,
  `ExtractedSecrets`, `ConnectionTrafficSecrets`.
- RFC 5288 (AES-GCM), RFC 7905 (ChaCha20-Poly1305), RFC 8446 (TLS 1.3).
- FreeBSD `ktls(4)` man page.
- Companion design docs: `docs/design/ktls-handoff.md`,
  `docs/design/ktls-key-extraction.md`.
