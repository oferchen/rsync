# QUIC transport (operator guide)

`oc-rsync` can carry the rsync daemon protocol over QUIC (RFC 9000/9001, TLS 1.3
over UDP) instead of a plain TCP socket. QUIC replaces only the transport. The
bytes inside the tunnel are the unmodified daemon protocol, so a QUIC session
and a TCP session move the same wire format and negotiate the same protocol
version.

## 1. What it is and when to use it

QUIC is an **oc-rsync extension**. Upstream `rsync` has no QUIC transport, so
it works only between two oc-rsync builds that both carry the `quic` feature.

| Transport | Encryption | Authentication | Use it when |
|---|---|---|---|
| `rsync://` / `host::module` (TCP) | none, like upstream | daemon `auth users` | trusted networks, or behind a TLS proxy |
| SSH (`host:path`, `ssh://`) | SSH | SSH keys or passwords | you already have shell access to the remote host |
| QUIC (`quic://`, `--quic`) | TLS 1.3, always | daemon certificate, plus optional client certificate | you run an oc-rsync daemon and want it encrypted without shell accounts, or on high-latency links where a tunable congestion controller helps |

QUIC applies only to daemon targets. It never changes an SSH or TCP transfer.

## 2. Build and availability

The cargo feature is `quic`, and it is in the default feature set, so a plain
`cargo build --release` includes it. Being compiled in changes nothing at
runtime: the client uses QUIC only for a `quic://` operand or `--quic`, and the
daemon opens its UDP listener only when `quic cert file` and `quic key file`
are set. To build without QUIC, turn the defaults off and name the features you
need:

```sh
cargo build --release --no-default-features --features zstd,lz4,acl,xattr,iconv,parallel
```

Such a build refuses `quic://` and `--quic` with a "not compiled into this
build" error.

It pulls in `quinn-proto` (the QUIC state machine), `quinn-udp`, and `rustls`
with the `ring` provider for TLS 1.3. `rcgen` is also linked, for the library's
ephemeral test identity. The daemon and the client never generate a
certificate at runtime.

Release artifacts built by `.github/workflows/release-cross.yml` enable `quic`
for Linux (glibc and musl), macOS and Windows. Releases up to and including
v0.6.4 predate this and do not carry it. Check a binary with
`oc-rsync --help | grep quic://`.

| Platform | Client | Daemon listener |
|---|---|---|
| Linux | yes | yes |
| macOS | yes | yes |
| Windows | yes | **no**: any QUIC directive in the config refuses to start (see [Troubleshooting](#8-troubleshooting)) |

## 3. Server setup

The QUIC listener runs beside the TCP listener. Five global directives in
`oc-rsyncd.conf` configure it:

| Directive | Default | Meaning |
|---|---|---|
| `quic cert file` | unset | PEM certificate chain the daemon presents, leaf first |
| `quic key file` | unset | PEM private key for that certificate (PKCS#8, PKCS#1 or SEC1) |
| `quic client ca file` | unset (no client certificate requested) | PEM CA bundle. When set, every client must present a certificate that chains to it (mutual TLS) |
| `quic port` | the daemon `port` (873) | UDP port of the QUIC listener. `quic port = 0` means 873, as `port = 0` does |
| `quic retry` | `auto` | When the listener answers a new client with a stateless Retry: `auto` once 100 handshakes are in flight, `always` for every client, `never` for none. Any other value fails to parse |

Rules:

- Setting any of the five turns the listener on. A config without them stays
  TCP-only.
- All five are global-only. A module section that sets one fails to parse.
- Paths resolve relative to the config file, like `pid file`.
- `quic cert file` and `quic key file` go together. Naming only one fails at
  startup.
- There is no auto-generated certificate. A listener with no certificate and key
  refuses to start.
- The daemon loads the certificate and key at startup, before it binds any
  listener. An unreadable or malformed file, or a key that does not match the
  certificate, stops the daemon with exit 1. It never serves TCP alone while
  QUIC is silently down.
- The listener binds the same addresses as the TCP listener, IPv4 and IPv6.
- `quic client ca file` gates only the QUIC listener. The TCP listener still
  serves the same modules without a certificate.

**Retry and connection limits.** A Retry (RFC 9000 section 8.1.2) makes a
client prove it can receive at its source address before the daemon keeps any
state for it, at the cost of one extra round trip. The client follows it
without any flag. With `quic retry = auto` an idle daemon never sends one, so
normal transfers pay nothing; under a handshake flood every new client is
validated first. A client accepted after a Retry is logged as `QUIC client
<address> validated by Retry`. Retry tokens are valid for 15 seconds and are
signed with a key generated at startup, so they never outlive the daemon
process. The daemon also bounds what it holds for connection attempts it has not
yet accepted: at most `max connections` attempts (128 when unlimited, the
listener's accept backlog), each buffering at most 64 KiB.

**Firewall.** QUIC uses UDP. Open `873/udp` (or your `quic port`) as well as
`873/tcp`.

**Process model.** Each QUIC session runs in its own forked child, as a TCP
session does. The daemon binds the UDP sockets while it holds its startup
privileges, then forks one QUIC front process. The front process drops to
`nobody` if it is still root. On Linux it also sets `no_new_privs` and applies
a Landlock ruleset with no filesystem access. It terminates TLS and hands each stream to the parent. The
parent applies `hosts allow` / `hosts deny` and `max connections` (counted
across TCP and QUIC), then forks the session child, which applies `use chroot`,
`uid` and `gid` as it would for TCP. If the front process exits, the daemon
keeps serving TCP and logs it.

## 4. Client usage

Two forms reach a QUIC daemon:

```sh
# The quic:// scheme (port 873 unless the URL names one)
oc-rsync -av --quic-ca ca-cert.pem quic://host/module/path/ dest/
oc-rsync -av --quic-ca ca-cert.pem quic://user@host:8873/module/path/ dest/

# --quic upgrades an rsync:// or host::module target
oc-rsync -av --quic --quic-ca ca-cert.pem host::module/path/ dest/
oc-rsync -av --quic --quic-ca ca-cert.pem rsync://host/module/path/ dest/
```

| Flag | Meaning |
|---|---|
| `--quic` | Use QUIC for a daemon target |
| `--quic-ca=PATH` | PEM CA bundle that must sign the daemon certificate. **Required** |
| `--quic-cert=PATH` | Client certificate chain (PEM) for mutual TLS. Needs `--quic-key` |
| `--quic-key=PATH` | Private key for `--quic-cert`. Needs `--quic-cert` |
| `--quic-cc=ALGORITHM` | Client congestion controller: `bbr` (default), `cubic` or `newreno` |
| `--quic-window=SIZE` | Client flow-control window in bytes, with an optional `K`/`M`/`G` (1024-based) suffix. Default 64 MiB |
| `--quic-cipher=CIPHER` | `aes` or `chacha20`. Default: AES-GCM on CPUs with hardware AES, otherwise ChaCha20-Poly1305 |

| Environment variable | Default | Meaning |
|---|---|---|
| `OC_RSYNC_QUIC_CC` | `bbr` | Congestion controller when `--quic-cc` is not given. An unknown name is an error |
| `OC_RSYNC_QUIC_WINDOW` | 64 MiB | Flow-control window when `--quic-window` is not given |

The flags tune the client endpoint only. A daemon's endpoint takes
`OC_RSYNC_QUIC_CC` and `OC_RSYNC_QUIC_WINDOW` from the daemon's own
environment, or the defaults. A mismatch between the two ends changes only
throughput, never compatibility.

**No fallback.** If QUIC cannot be established, the client exits 5. It never
retries over plaintext TCP.

## 5. Trust model

Every connection runs TLS 1.3 with the ALPN identifier `rsync`. There is no
unencrypted QUIC mode.

- **No default trust.** The client trusts only the bundle named by `--quic-ca`.
  It does not consult the system trust store. Without `--quic-ca` it refuses
  before sending a packet, with exit 5.
- **No trust-on-first-use.** There is no known-hosts file, and an unknown
  certificate is never accepted or remembered.
- **Strict verification.** The daemon chain must verify against the bundle, the
  certificate must name the dialled host in its SAN (DNS name, or IP address
  when you dial an IP), it must be inside its validity period, and the leaf must
  be `CA:FALSE`. Any failure ends the handshake with exit 5.
- **Address validation.** QUIC limits a daemon's replies to an unvalidated
  address to three times what it received, so it cannot be used to amplify a
  spoofed flood. Under load the daemon also sends a stateless Retry, so a
  flood of spoofed handshakes cannot make it hold state (`quic retry`, section
  3).

**Trusting a private CA.** Issue the daemon certificate from your CA and give
clients the CA certificate:

```sh
oc-rsync -av --quic-ca /etc/oc-rsync/ca-cert.pem quic://host/module/ dest/
```

Rotating the daemon certificate under the same CA needs no client change.

**Pinning one certificate.** Give a client the daemon's own self-signed
certificate as `--quic-ca`. It then trusts exactly that certificate. The
certificate must still be `CA:FALSE` and name the host. Replacing it means
redistributing the new file. There is no fingerprint flag.

The rows below are pinned by
`crates/transfer/tests/quic_certificate_validation_matrix.rs`, which mints every
certificate at test time and runs the real daemon and client in both
directions. If this table and the test disagree, the test wins.

| Server certificate or client setup | Result | Exit |
|---|---|---|
| Leaf signed by the `--quic-ca` CA: CA:FALSE, `serverAuth`, DNS SAN of the dialled name | accepted | 0 |
| IP SAN, dialled by that IP address | accepted | 0 |
| Leaf plus intermediate in `quic cert file`, root in `--quic-ca` | accepted | 0 |
| Leaf key P-256, P-384, Ed25519 or RSA-2048 | accepted | 0 |
| Leaf with no extended key usage extension | accepted | 0 |
| Self-signed CA:FALSE leaf named directly as `--quic-ca` | accepted | 0 |
| Leaf marked CA:TRUE | refused, `CaUsedAsEndEntity` | 5 |
| SAN does not name the dialled host (including an IP-only SAN dialled by name) | refused, `certificate not valid for name` | 5 |
| Leaf whose extended key usage lacks `serverAuth` | refused | 5 |
| Expired, or not yet valid | refused, `certificate expired` / `certificate not valid yet` | 5 |
| Self-signed leaf, or a leaf from another CA, not anchored by `--quic-ca` | refused, `UnknownIssuer` | 5 |
| No `--quic-ca` | refused before dialling, names `--quic-ca` | 5 |
| `--quic-ca` empty, not a certificate, or unreadable | refused before dialling, names the file | 5 |
| `--quic-cert` without `--quic-key` | refused before dialling | 5 |

With `quic client ca file` set on the daemon (mutual TLS):

| Client certificate | Result | Exit |
|---|---|---|
| Signed by the client CA, `clientAuth` | accepted | 0 |
| None presented | refused, `peer sent no certificates` | 5 |
| Signed by another CA | refused, `UnknownIssuer` | 5 |
| Expired | refused, `certificate expired` | 5 |
| Extended key usage lacks `clientAuth` | refused | 5 |

A client certificate offered to a daemon without `quic client ca file` is
ignored, and the transfer succeeds.

## 6. Generating test certificates

This creates a CA, a daemon certificate for `localhost` and `127.0.0.1`, and a
client certificate for mutual TLS. `openssl req -x509` marks its output
`CA:TRUE`, which is right for the CA and wrong for a leaf, so the leaves are
signed from CSRs with explicit extensions.

```sh
mkdir -p pki && cd pki

# CA
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout ca-key.pem -out ca-cert.pem -days 3650 -subj "/CN=oc-rsync test CA"

# Daemon certificate. The SAN must name the host clients dial.
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server-key.pem -out server.csr -subj "/CN=localhost"
printf 'basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n' > server.ext
openssl x509 -req -in server.csr -CA ca-cert.pem -CAkey ca-key.pem -CAcreateserial \
  -out server-cert.pem -days 365 -extfile server.ext

# Client certificate for mutual TLS
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout client-key.pem -out client.csr -subj "/CN=alice"
printf 'basicConstraints=critical,CA:FALSE\nextendedKeyUsage=clientAuth\n' > client.ext
openssl x509 -req -in client.csr -CA ca-cert.pem -CAkey ca-key.pem -CAcreateserial \
  -out client-cert.pem -days 365 -extfile client.ext

openssl verify -CAfile ca-cert.pem server-cert.pem client-cert.pem
```

For a single pinned certificate instead of a CA, keep the leaf `CA:FALSE`:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server-key.pem -out server-cert.pem -days 365 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "extendedKeyUsage=serverAuth"
```

Clients then pass `--quic-ca server-cert.pem`.

## 7. Walkthrough

Run from the directory that holds `pki/`. Port 18873 avoids needing root.

```sh
mkdir -p mod src dst && echo hello > src/a.txt && echo world > mod/m.txt

cat > oc-rsyncd.conf <<EOF
port = 18873
pid file = $PWD/oc-rsyncd.pid
log file = $PWD/oc-rsyncd.log
use chroot = no
quic cert file = $PWD/pki/server-cert.pem
quic key file = $PWD/pki/server-key.pem

[data]
    path = $PWD/mod
    read only = no
EOF

oc-rsync --daemon --config=$PWD/oc-rsyncd.conf
grep QUIC oc-rsyncd.log        # QUIC listener bound on [::]:18873 and 0.0.0.0:18873

# Pull
oc-rsync -av --quic-ca pki/ca-cert.pem quic://localhost:18873/data/ dst/

# Push
oc-rsync -av --quic-ca pki/ca-cert.pem src/ quic://localhost:18873/data/pushed/
```

For mutual TLS, add this line to the global section and restart the daemon:

```ini
quic client ca file = /full/path/to/pki/ca-cert.pem
```

```sh
# Refused: no client certificate (exit 5, "peer sent no certificates")
oc-rsync -av --quic-ca pki/ca-cert.pem quic://localhost:18873/data/ dst/

# Accepted
oc-rsync -av --quic-ca pki/ca-cert.pem \
  --quic-cert pki/client-cert.pem --quic-key pki/client-key.pem \
  quic://localhost:18873/data/ dst/
```

Stop the daemon with `kill $(cat oc-rsyncd.pid)`.

## 8. Troubleshooting

Client messages. All exit 5 unless noted.

| Message | Meaning |
|---|---|
| `quic: no trust anchor configured; pass --quic-ca <PATH> naming the CA that signed the daemon certificate (QUIC has no default trust)` | Add `--quic-ca` |
| `quic: reading --quic-ca file PATH: ...` | The bundle cannot be read |
| `quic: no certificates found in --quic-ca file PATH` | The file holds no PEM certificate |
| `quic: parsing certificate #N in --quic-ca file PATH: ...` / `... is not a usable trust anchor: ...` | A certificate in the bundle is malformed |
| `--quic-cert requires --quic-key (client certificate needs its private key)` | Pass both flags, or neither. The reverse message names `--quic-cert` |
| `QUIC connection to HOST:PORT failed: DETAIL; refusing to fall back to plaintext TCP` | The handshake failed. DETAIL says why, below |
| DETAIL `timed out` | No QUIC listener answered: wrong port, UDP blocked, or the daemon has no QUIC directives |
| DETAIL `... UnknownIssuer` | The daemon certificate does not chain to `--quic-ca` |
| DETAIL `... certificate not valid for name ...` | The SAN does not name the host you dialled |
| DETAIL `... CaUsedAsEndEntity` | The daemon leaf is marked CA:TRUE. Reissue it as CA:FALSE (section 6) |
| DETAIL `... certificate expired` / `certificate not valid yet` | Check the certificate dates and both clocks |
| DETAIL `... peer sent no certificates` | The daemon requires a client certificate. Add `--quic-cert` and `--quic-key` |
| `unrecognized QUIC congestion controller "X"; expected one of: bbr, cubic, newreno` | Bad `--quic-cc` or `OC_RSYNC_QUIC_CC` |
| `invalid OC_RSYNC_QUIC_WINDOW value "X"; expected a byte count with an optional K, M, or G suffix` | Bad window size |
| `quic:// URLs require the QUIC transport, which is not compiled into this build (rebuild with the 'quic' feature)...` | This binary has no `quic` feature. `--quic` reports `--quic requires the QUIC transport, ...` |

Daemon startup messages. All exit 1.

| Message | Meaning |
|---|---|
| ``QUIC listener requested but no certificate configured: set both `quic cert file` and `quic key file` (there is no ephemeral fallback)`` | `quic port` or `quic client ca file` is set without a certificate and key |
| `'quic cert file' requires 'quic key file' to be set as well` | Only half the pair is set. The reverse message names `quic key file` |
| `'quic cert file' directive must not be empty` | Empty value. Same for the other path directives |
| `'quic port' is a global-only directive and cannot appear in a module section` | Move it to the global section. Same for the other three |
| `failed to load the QUIC server identity: the private key does not match the certificate` | `quic key file` belongs to another certificate |
| `failed to load the QUIC server identity: ...` | The certificate or key cannot be read or parsed |
| `'quic cert file' is set, but the QUIC daemon is not supported on Windows` | Windows daemons cannot serve QUIC. Remove the QUIC directives |

Log lines: `QUIC listener bound on ADDR and ADDR` and `QUIC listener serving on
ADDR` mean the listener is up. `failed to start QUIC listener on ADDR: ...`
means the front process could not start the listener on that socket.

## 9. Congestion control

QUIC uses `quinn-proto`'s congestion controllers. The default is **BBR**
(`BbrConfig`), a model-based controller suited to high bandwidth-delay links.
CUBIC and NewReno, both loss-based, can be selected with `--quic-cc` or
`OC_RSYNC_QUIC_CC`. Prefer them where fairness with competing loss-based
flows matters.

The flow-control window must cover the link's bandwidth-delay product
(`bandwidth * round-trip time`), or the sender stalls waiting for
acknowledgements. The 64 MiB default fills a single stream up to roughly
5 Gbit/s at a 100 ms round trip. Raise it with `--quic-window` on faster or
longer links, and confirm a value by measuring on the real path.

## 10. Limitations

- oc-rsync to oc-rsync only, and daemon targets only.
- No Windows daemon listener (the client works).
- No multipath congestion control. This is a known, noted gap.
- No per-module client CA. `quic client ca file` is global, and the TCP
  listener serves the same modules without a certificate.
- An unreachable QUIC daemon exits 5, where TCP exits 10. `--contimeout`,
  `RSYNC_PROXY` and `RSYNC_CONNECT_PROG` do not apply to a QUIC connection.
- No permission check on `quic key file` or `--quic-key`. Protect the key files
  yourself.
- No certificate fingerprint pinning flag. Pin by passing the certificate as
  `--quic-ca` (section 5).

ALPN is `rsync` today. Today's congestion controller is BBR (section 9).
Additional performance-oriented ALPNs and congestion control algorithms will follow.

## See also

- [`ssh-transport.md`](ssh-transport.md): the SSH transport.
- [`oc-extension-env-reference.md`](oc-extension-env-reference.md): every oc-only environment variable.
- Design notes: [`design/quic-transport-policy.md`](design/quic-transport-policy.md),
  [`design/quic-security-model.md`](design/quic-security-model.md) and
  [`design/quic-transport-concurrency-model.md`](design/quic-transport-concurrency-model.md).
