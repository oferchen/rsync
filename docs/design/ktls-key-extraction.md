# NET-KTLS.3 - TLS Session Key Extraction for Kernel TLS Handoff

Status: Design (docs-only). Parent: `docs/design/ktls-handoff.md`.

## 1. Goal

Once the TLS handshake completes on a `daemon-tls` connection, extract the
per-direction record-protection keys from rustls and program them into the
Linux kernel's TLS ULP so that subsequent record framing, encryption, and
decryption happen in kernel context. After programming, the daemon may use
`sendfile(2)` / `splice(2)` / `MSG_ZEROCOPY` on the TLS socket and the kernel
emits properly framed TLS records without a userspace copy.

Concretely the post-handshake sequence is:

1. `setsockopt(fd, IPPROTO_TCP, TCP_ULP, "tls", 4)` to attach the TLS ULP.
2. `setsockopt(fd, SOL_TLS, TLS_TX, &tls12_crypto_info_*, sizeof(...))` for
   the send direction (using the rustls TX secret).
3. `setsockopt(fd, SOL_TLS, TLS_RX, &tls12_crypto_info_*, sizeof(...))` for
   the receive direction (using the rustls RX secret) - NET-KTLS.5.

Until NET-KTLS.4 lands the daemon continues to drive records through rustls
in userspace; this document scopes only the extraction and the safe handoff
contract.

## 2. rustls API Path

rustls 0.23 exposes the extraction entry point on the post-handshake
connection:

```rust
// gated by the `dangerous_extract_secrets` Cargo feature.
let extracted: rustls::ExtractedSecrets =
    conn.dangerous_extract_secrets()?;
let rustls::ExtractedSecrets { tx, rx } = extracted;
```

`tx` and `rx` are `(u64 /* sequence */, ConnectionTrafficSecrets)` pairs.
`ConnectionTrafficSecrets` is an enum tagged by negotiated cipher
(`Aes128Gcm`, `Aes256Gcm`, `Chacha20Poly1305`, ...). Each variant carries
the raw `key`, `iv`/`salt`, and any implicit-nonce material the kernel
expects.

### Cargo feature wiring

The workspace currently does not enable extraction. NET-KTLS.3 requires:

- `crates/daemon/Cargo.toml`: add a new `daemon-tls-ktls` feature that
  depends on `daemon-tls` and turns on rustls' `dangerous_extract_secrets`.
  Example:

  ```toml
  [features]
  daemon-tls = ["dep:rustls", "dep:tokio-rustls"]
  daemon-tls-ktls = ["daemon-tls", "rustls/dangerous_extract_secrets"]
  ```

- The opt-in is intentional. The rustls feature name carries an explicit
  `dangerous_` prefix because extraction defeats rustls' built-in defence
  against accidental secret exposure; only the kTLS path should compile it
  in.

### Safety implications

- Extracted secrets are live key material. They MUST NOT be logged, sent
  to telemetry, written to disk, or copied into wider-lifetime buffers.
- The connection must be at post-handshake state. Calling extraction
  mid-handshake or on a 0-RTT-only path returns an error; the daemon must
  treat that as a hard fallback to userspace TLS.
- After successful kernel handoff the userspace rustls `Connection` MUST
  be either dropped or marked "drained" so it never re-encrypts a record
  that the kernel has also emitted - mismatched record sequence numbers
  would terminate the session with a fatal alert.

## 3. Cipher Suite Restrictions

Linux's kTLS only programs a fixed set of AEADs. The daemon enumerates the
negotiated `ConnectionTrafficSecrets` variant and rejects anything outside
the kernel's allow-list before touching `setsockopt`:

| rustls variant              | Kernel cipher_type           | Kernel struct                          | Min kernel |
| --------------------------- | ---------------------------- | -------------------------------------- | ---------- |
| `Aes128Gcm`                 | `TLS_CIPHER_AES_GCM_128`     | `tls12_crypto_info_aes_gcm_128`        | 4.13       |
| `Aes256Gcm`                 | `TLS_CIPHER_AES_GCM_256`     | `tls12_crypto_info_aes_gcm_256`        | 5.1        |
| `Chacha20Poly1305`          | `TLS_CIPHER_CHACHA20_POLY1305` | `tls12_crypto_info_chacha20_poly1305` | 5.11       |
| `Aes128Ccm`                 | `TLS_CIPHER_AES_CCM_128`     | `tls12_crypto_info_aes_ccm_128`        | 5.2        |

Any other variant (future post-quantum AEAD, AES-CCM-8, ARIA, ...) is a
hard reject - log once at `INFO`, leave the ULP unattached, and continue
in userspace.

The TLS protocol version is also encoded in the crypto-info struct
(`TLS_1_2_VERSION` / `TLS_1_3_VERSION`). The daemon picks the value from
`rustls::ProtocolVersion` and refuses to program if rustls reports
anything older than TLS 1.2.

## 4. Programming the Kernel - Field Mapping

Per `linux/tls.h` the AES-GCM-128 record structure is:

```c
struct tls12_crypto_info_aes_gcm_128 {
    struct tls_crypto_info info;          /* version, cipher_type */
    unsigned char iv[TLS_CIPHER_AES_GCM_128_IV_SIZE];          /* 8  */
    unsigned char key[TLS_CIPHER_AES_GCM_128_KEY_SIZE];        /* 16 */
    unsigned char salt[TLS_CIPHER_AES_GCM_128_SALT_SIZE];      /* 4  */
    unsigned char rec_seq[TLS_CIPHER_AES_GCM_128_REC_SEQ_SIZE];/* 8  */
};
```

Mapping from rustls:

- `info.version`: `TLS_1_3_VERSION` or `TLS_1_2_VERSION`.
- `info.cipher_type`: from the table above.
- `key`: AEAD key material from `ConnectionTrafficSecrets::Aes128Gcm.key`.
- `salt`: the 4-byte implicit-nonce prefix from rustls (TLS 1.3 derives this
  from the traffic secret; rustls hands it back already split).
- `iv`: the 8-byte explicit nonce / per-record counter seed.
- `rec_seq`: the 8-byte starting record sequence number, taken from the
  `u64` returned alongside the `ConnectionTrafficSecrets`. Endianness is
  network byte order (big-endian) - the kernel performs no swap.

The struct is `repr(C)` in Rust via `libc`; the daemon must zero-init,
populate, hand it to `setsockopt` as `&T as *const _ as *const c_void`,
then explicitly `zeroize()` the local copy before drop.

## 5. Sequence Diagram

```mermaid
sequenceDiagram
    participant App as oc-rsync daemon
    participant Rust as rustls Connection
    participant Probe as kTLS probe
    participant Kern as Linux kernel (TLS ULP)

    App->>Rust: complete handshake (TLS 1.3 / 1.2)
    Rust-->>App: handshake done, suite known
    App->>App: gate on suite in allow-list?
    alt suite unsupported
        App->>Rust: continue userspace TLS
    else suite supported
        App->>Probe: TCP_ULP="tls" supported & loaded?
        Probe-->>App: yes (or load tls.ko via module_request)
        App->>Rust: dangerous_extract_secrets()
        Rust-->>App: ExtractedSecrets{tx, rx}
        App->>Kern: setsockopt(TCP_ULP, "tls")
        App->>Kern: setsockopt(SOL_TLS, TLS_TX, &info_tx)
        App->>Kern: setsockopt(SOL_TLS, TLS_RX, &info_rx)
        Kern-->>App: 0 (ok)
        App->>App: zeroize ExtractedSecrets, drop rustls record layer
        App->>Kern: sendfile(payload_fd, sock)
        Kern-->>Kern: encrypt + frame in kernel; emit on wire
    end
```

## 6. Error Handling

Every kernel call is best-effort and fully reversible until `TLS_TX` /
`TLS_RX` succeed. Once one direction is programmed the daemon is
committed: rustls must not re-encrypt that direction or sequence numbers
will diverge.

| Stage                      | errno / cause                  | Action                                                                                          |
| -------------------------- | ------------------------------ | ----------------------------------------------------------------------------------------------- |
| `TCP_ULP="tls"`            | `ENOPROTOOPT`                  | Kernel < 4.13 or `tls.ko` not loaded. Log once at `INFO`, fall back to userspace TLS.            |
| `TCP_ULP="tls"`            | `EBUSY` / `EEXIST`             | Another ULP already attached. Surface as `WARN`, fall back.                                     |
| `TCP_ULP="tls"`            | `EACCES`                       | Module autoload denied (locked-down kernel). Log once at `INFO`, fall back.                     |
| `dangerous_extract_secrets`| `Error::HandshakeNotComplete`  | Bug - extraction called too early. Abort connection, no fallback.                                |
| `dangerous_extract_secrets`| `Error::General` (suite gap)   | rustls negotiated a suite the extractor cannot lower. Log + fall back.                          |
| `TLS_TX` / `TLS_RX`        | `EINVAL` (bad key length)      | BUG in cipher-mapping code. `debug_assert!`, abort the connection, never silently fall back.    |
| `TLS_TX` / `TLS_RX`        | `ENOTSUPP` / `EOPNOTSUPP`      | Kernel built without selected cipher. Fall back (TX not yet committed) or abort (RX after TX).  |
| `TLS_TX` / `TLS_RX`        | `EBADMSG`                      | rec_seq mismatch. Abort connection with TLS fatal alert; this is unrecoverable.                 |

Fallback rule of thumb: any error BEFORE the first successful direction is
programmed is recoverable; any error AFTER terminates the session.

## 7. Key Zeroization

- `rustls::ExtractedSecrets` and its `ConnectionTrafficSecrets` payload
  already implement zero-on-drop (rustls uses `zeroize::Zeroizing` for
  the inner byte arrays). The daemon must not `Clone` them; move them
  directly into the `setsockopt` builder.
- The per-direction `tls12_crypto_info_*` value built in Rust is a
  short-lived local. It MUST be wrapped in `zeroize::Zeroizing<T>` or
  explicitly zeroed with `core::ptr::write_volatile` before the stack
  frame returns. Compiler reordering is the realistic threat - relying
  on `Drop` alone is insufficient for `[u8; N]` arrays.
- Page-locking the secret (`mlock`) is out of scope. Daemons that need
  swap-resilient keys should run with `RLIMIT_MEMLOCK` and call `mlockall`
  at startup; this is a deployment concern, not part of NET-KTLS.3.
- Logging redaction: the `Debug` impl on the wrapper struct must print
  `"<redacted>"`. Tests assert this with a `format!("{:?}", info)` check.

## 8. Roadmap

- **NET-KTLS.4** - wire the TX handoff into the daemon-tls send path.
  Once `TLS_TX` is programmed, replace the `tokio_rustls` writer with a
  direct `sendfile`/`writev` on the underlying TCP socket. Userspace
  rustls remains responsible for control records (alerts, close-notify)
  via `KTLS_SET_TX_SEND_CTRL_MSG` semantics.
- **NET-KTLS.5** - program `TLS_RX` and switch the read path to plain
  `recv()`/`splice()` against the TLS-decrypted stream. Requires
  resolving rustls' control-record draining and graceful close-notify
  handling.
- **NET-KTLS.6** - opportunistic key rotation (`TLS_TX` reprogramming on
  TLS 1.3 KeyUpdate). rustls surfaces the new secret through
  `ExtractedSecrets`; the daemon re-programs `TLS_TX` in-place.

## 9. References

- Linux kernel: `Documentation/networking/tls.rst`
  (kernel.org `Documentation/networking/tls.rst`).
- Linux header: `include/uapi/linux/tls.h` (cipher_type constants,
  `tls12_crypto_info_*` structs).
- rustls: `rustls::ConnectionCommon::dangerous_extract_secrets`,
  `rustls::ExtractedSecrets`, `rustls::ConnectionTrafficSecrets`.
- Parent design: `docs/design/ktls-handoff.md` (NET-KTLS.1/.2).
