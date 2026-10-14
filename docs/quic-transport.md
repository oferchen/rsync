# QUIC transport (operator guide)

`oc-rsync` can carry the rsync daemon protocol over QUIC instead of a plain
TCP connection. QUIC replaces only the transport: the bytes inside the tunnel
are the unmodified daemon protocol, so a QUIC session and a TCP session move
the same wire format and negotiate the same protocol version. QUIC adds a
mandatory TLS 1.3 layer, a UDP substrate, and its own congestion control on top
of that stream.

## Status and support tier

QUIC is an **oc-rsync extension** with no counterpart in upstream `rsync`, and
it is **Tier 2**: opt-in, **off by default**, and compiled only when the `quic`
Cargo feature is enabled. A default build does not link the QUIC stack and does
not understand `quic://` operands or the `--quic` family of flags. Every flag,
directive, and behaviour in this guide requires a build produced with the
feature turned on.

The release artifacts are built without the feature, so to use QUIC you build
`oc-rsync` yourself (see [Building with QUIC](#building-with-quic)).

Because it is an extension, QUIC only works oc-rsync-to-oc-rsync: an upstream
`rsync` peer speaks neither QUIC nor the daemon protocol over UDP. Nothing in
this guide changes the behaviour of a TCP or SSH transfer.

## Quick start

This walkthrough builds a QUIC-enabled binary, creates a private CA with one
server and one client certificate, starts a daemon on an unprivileged port,
and pulls and pushes over QUIC. Every command below was run on Linux (aarch64)
with OpenSSL 3.5 against the binary built in step 1.

1. Build:

   ```sh
   cargo build --release --locked --features quic --bin oc-rsync
   ```

2. Create a CA and a server certificate (see [Certificates](#certificates)
   for what each extension is for):

   ```sh
   openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key
   openssl req -x509 -new -key ca.key -sha256 -days 365 \
     -subj "/CN=oc-rsync QUIC CA" \
     -addext "basicConstraints=critical,CA:TRUE" \
     -addext "keyUsage=critical,keyCertSign,cRLSign" \
     -out ca.pem

   cat > server.ext <<'EOF'
   basicConstraints = critical,CA:FALSE
   keyUsage = critical,digitalSignature
   extendedKeyUsage = serverAuth
   subjectAltName = DNS:localhost,IP:127.0.0.1
   EOF
   openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out server.key
   openssl req -new -key server.key -subj "/CN=localhost" -out server.csr
   openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
     -sha256 -days 365 -extfile server.ext -out server.pem
   ```

3. Write a daemon configuration. The QUIC listener shares the TCP `port`
   number on UDP unless `quic port` is set:

   ```ini
   port = 18873
   pid file = /path/to/oc-rsyncd.pid
   log file = /path/to/oc-rsyncd.log
   quic cert file = /path/to/server.pem
   quic key file = /path/to/server.key

   [data]
       path = /path/to/module
       use chroot = no
       read only = false
   ```

4. Start the daemon. It now listens on 18873/tcp and 18873/udp:

   ```sh
   oc-rsync --daemon --no-detach --config oc-rsyncd.conf
   ```

5. Pull and push over QUIC:

   ```sh
   oc-rsync -av --quic-ca ca.pem quic://localhost:18873/data/ pulled/
   oc-rsync -av --quic-ca ca.pem src/ quic://localhost:18873/data/incoming/
   ```

6. List the daemon's modules over QUIC:

   ```sh
   oc-rsync --quic-ca ca.pem quic://localhost:18873/
   ```

## Building with QUIC

Enable the `quic` feature at build time:

```sh
cargo build --release --locked --features quic --bin oc-rsync
```

The feature propagates through the crate graph (`cli` -> `core` ->
`rsync_io`, and `daemon` for the listener side). It pulls in `quinn-proto` (the
sans-I/O QUIC state machine), `quinn-udp`, and `rustls` with the ring crypto
provider for TLS 1.3, so there is no system OpenSSL requirement for QUIC. The
feature also links `rcgen`, which only the tests use. A build without the
feature is unchanged on every other transport.

A release build with the feature is larger than a default one. Measured on
aarch64 Linux: 13,770,128 bytes with `quic`, 12,328,320 bytes without
(+1.4 MB, +11.7%), and 11 extra crates in the dependency tree.

## Certificates

QUIC verifies the daemon's certificate with the WebPKI rules of `rustls`, which
are stricter than many TLS stacks:

- The daemon certificate must name the host the client dials in its
  `subjectAltName` (`DNS:` for names, `IP:` for addresses). The `CN` is not
  consulted.
- The daemon certificate must be an end-entity certificate
  (`basicConstraints = CA:FALSE`). A certificate marked `CA:TRUE` is refused
  with `CaUsedAsEndEntity`, even when it is self-signed and passed as its own
  `--quic-ca`.
- The issuing CA must be marked `CA:TRUE` with `keyCertSign`.

A private CA, as in the [Quick start](#quick-start), is the recommended setup:
clients trust the CA once, and the daemon certificate can be rotated without
touching them.

### Self-signed daemon certificate

A single self-signed certificate also works, provided it is an end-entity
certificate. `openssl req -x509` adds `CA:TRUE` by default, so override it:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server.key -out server.pem -days 365 \
  -subj "/CN=rsync.example.com" \
  -addext "subjectAltName=DNS:rsync.example.com" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=serverAuth"
```

Clients then pass `server.pem` itself as `--quic-ca`.

### Client certificate (mutual TLS)

For mutual TLS, sign a client certificate with a CA the daemon trusts. This can
be the same CA as above or a separate one:

```sh
cat > client.ext <<'EOF'
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
EOF
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out client.key
openssl req -new -key client.key -subj "/CN=backup-client" -out client.csr
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
  -sha256 -days 365 -extfile client.ext -out client.pem
```

Private keys may be PKCS#8, PKCS#1 or SEC1 PEM.

## Daemon configuration

The QUIC listener runs alongside the daemon's TCP listener and is configured
with global directives in `oc-rsyncd.conf`:

| Directive | Default | Meaning |
|---|---|---|
| `quic cert file` | unset | PEM certificate chain the daemon presents (leaf first). |
| `quic key file` | unset | PEM private key for `quic cert file`. |
| `quic client ca file` | unset (no client certificate requested) | PEM CA bundle; when set, every client must present a certificate anchored by it. |
| `quic port` | the daemon `port` (873) | UDP port of the QUIC listener. `0` means 873. |

Setting **any** of these marks QUIC as requested. A config with none of them
leaves the listener off, so a `quic` build stays TCP-only until you configure
it.

All four are **global-only**: a module section that sets one is rejected at
startup (exit 1), for example:

```text
'quic port' is a global-only directive and cannot appear in a module section (line 7)
```

The certificate, key and CA paths resolve relative to the config file (the same
handling as `pid file` / `lock file`), and any `%` tokens are left verbatim for
expansion when the listener is bound. The files are read when the listener
starts, not at parse time.

### Certificate and key are required together

There is no auto-generated or in-memory certificate. The daemon presents only
the operator-supplied `quic cert file` / `quic key file` pair, and refuses to
start (exit 1) when QUIC is requested without both:

```text
'quic cert file' requires 'quic key file' to be set as well
QUIC listener requested but no certificate configured: set both `quic cert file` and `quic key file` (there is no ephemeral fallback)
```

The first message appears when only one of the pair is set, the second when
QUIC is requested (for example by `quic port`) with neither.

A certificate or key that is present but unusable, such as a key that does not
match the certificate, does **not** stop the daemon. It logs
`failed to load the QUIC server identity: ...`, skips the QUIC listener and
keeps serving TCP. QUIC clients then time out. Check the daemon log after any
certificate change.

### Requiring a client certificate (mutual TLS)

```ini
quic client ca file = /etc/oc-rsync/quic/clients-ca.pem
```

When set, the listener requires every client to present a certificate that
chains to this bundle. A client that presents none, or one from another CA, is
refused in the TLS handshake before any `@RSYNCD:` byte is exchanged. The
listener still needs its own `quic cert file` / `quic key file`.

### Process model

A QUIC session runs in its own forked child, exactly like a TCP session. The
daemon parent never serves one itself:

1. The parent binds the UDP sockets while it still holds its startup
   privileges, then forks a QUIC front process before it starts any thread.
2. The front process drops to `nobody` if it is still root. On Linux it also
   dies with the parent, sets `no_new_privs`, and installs a Landlock ruleset
   that grants no filesystem access. Only then does it start the QUIC threads.
   It terminates TLS for every QUIC connection and never forks or runs a
   session.
3. For each accepted stream, the front process hands the parent one end of a
   socket pair together with the peer and local addresses.
4. The parent admits the relayed stream through the same path as a TCP
   connection: `hosts allow` / `hosts deny`, `max connections` (counted across
   TCP and QUIC together), then a forked per-session child. That child applies
   `use chroot`, `uid` and `gid` for its module, as upstream does for TCP.

So stock modules with `use chroot = yes` and `uid = nobody` work over QUIC, and
a QUIC session cannot chroot or drop privileges for the whole daemon. If the
front process exits, the parent logs it and keeps serving TCP; QUIC stays down
until the daemon restarts.

`auth users` and `secrets file` work over QUIC unchanged, inside the tunnel.

## Client usage

There are two ways to ask a client to use QUIC, and both target an rsync
**daemon** endpoint (QUIC does not apply to SSH `host:path` transfers):

- **The `quic://` URL scheme**, the QUIC form of `rsync://`:

  ```sh
  oc-rsync -a --quic-ca ca.pem quic://host/module/path/ dest/
  oc-rsync -a --quic-ca ca.pem quic://user@host:8730/module/path/ dest/
  ```

- **The `--quic` modifier**, which moves an ordinary daemon target to QUIC
  without changing the operand:

  ```sh
  oc-rsync -a --quic --quic-ca ca.pem rsync://host/module/path/ dest/
  oc-rsync -a --quic --quic-ca ca.pem --port 8730 host::module/path/ dest/
  ```

The default port is **873**, over **UDP**. `--port` overrides it, and a
`quic://` or `rsync://` URL may name its own port after the host.

### No silent fallback

When QUIC is requested but the connection cannot be established, the client
**fails**. It never falls back to plaintext TCP: every QUIC failure message ends
with `refusing to fall back to plaintext TCP`.

### Trust model

Every QUIC connection runs TLS 1.3 with the ALPN protocol identifier `rsync`;
there is no unencrypted QUIC mode. The client verifies the daemon's certificate
against exactly one trust source, the PEM CA bundle named by `--quic-ca <PATH>`:

- **`--quic-ca` is required.** Nothing is trusted by default. Without it the
  client refuses before sending a packet and exits 5:

  ```text
  QUIC connection to localhost:18873 failed: quic: no trust anchor configured; pass --quic-ca <PATH> naming the CA that signed the daemon certificate (QUIC has no default trust)
  ```

- **`--quic-ca` is strict.** The daemon's chain must verify against the bundle:
  a certificate from another CA, one whose `subjectAltName` does not name the
  host being dialed, or one outside its validity period fails the handshake
  (exit 5).
- **No trust-on-first-use, no system trust store.** The client keeps no
  known-hosts file and does not consult the operating system's CA store.

### Client certificate

```sh
oc-rsync -a --quic-ca ca.pem --quic-cert client.pem --quic-key client.key \
  quic://host/module/ dest/
```

`--quic-cert` is the PEM certificate chain (leaf first) the client presents and
`--quic-key` its private key. The two are all-or-nothing: naming only one is an
error (exit 5). A daemon without `quic client ca file` ignores a presented
certificate.

### Cipher selection

`--quic-cipher aes|chacha20` fixes the TLS 1.3 cipher family. The default picks
AES-GCM on CPUs with hardware AES and ChaCha20-Poly1305 otherwise. `--debug=connect`
prints the negotiated suite, for example
`quic: negotiated cipher suite TLS13_CHACHA20_POLY1305_SHA256`.

### Congestion control and flow-control window

Two client-side flags tune QUIC's pacing and back-pressure. They change only
how fast bytes leave and how many may be in flight, never a wire byte, so any
setting interoperates with any other.

`--quic-cc` selects the congestion controller:

| Value | Algorithm | Notes |
|---|---|---|
| `bbr` | BBR | Model-based; the default, suited to high bandwidth-delay links. |
| `cubic` | CUBIC | Loss-based; the usual TCP default. |
| `newreno` | NewReno | Loss-based; the QUIC baseline. |

`--quic-window` sizes the flow-control window in bytes, with an optional
`K`/`M`/`G` (1024-based) suffix. The default, 64 MiB, covers a single stream
up to roughly 5 Gbit/s at 100 ms RTT. Raise it when `bandwidth * RTT` of the
link is larger, and confirm a chosen value with a measurement on the real path.

| Variable | Flag | Default | Notes |
|---|---|---|---|
| `OC_RSYNC_QUIC_CC` | `--quic-cc` | `bbr` | `bbr`, `cubic` or `newreno` |
| `OC_RSYNC_QUIC_WINDOW` | `--quic-window` | 64 MiB | Byte count, optional `K`/`M`/`G` suffix |

The flag takes precedence over the variable, which takes precedence over the
default. An unknown value is an error: exit 1 on the flag, exit 5 on the
variable.

## Platform support

| Platform | Client | Daemon listener | CI coverage |
|---|---|---|---|
| Linux | yes | yes | build, `rsync_io` QUIC tests, oc client to oc daemon interop cells |
| macOS | yes | yes | build and `rsync_io` QUIC tests |
| Windows | yes | no | build and `rsync_io` QUIC tests |
| FreeBSD | not tested | not tested | none |

The daemon listener is Unix-only. On Windows, a `quic` build parses the QUIC
directives but starts no listener and does not report an error. A loud refusal
is planned. The macOS build uses the portable UDP path.

## Troubleshooting

Exit codes below were observed with the walkthrough setup.

| Symptom | Exit | Cause and fix |
|---|---|---|
| `quic:// URLs require the QUIC transport, which is not compiled into this build` | 1 | The binary was built without the `quic` feature. Rebuild with `--features quic`. |
| `--quic requires the QUIC transport, which is not compiled into this build` | 1 | Same as above, for the `--quic` flag. |
| `quic: no trust anchor configured; pass --quic-ca <PATH> ...` | 5 | Add `--quic-ca` naming the CA that signed the daemon certificate. |
| `invalid peer certificate: UnknownIssuer` | 5 | `--quic-ca` names a different CA than the one that signed the daemon certificate. |
| `certificate not valid for name "..."` | 5 | The dialed host or address is not in the certificate's `subjectAltName`. Dial a listed name or reissue the certificate. |
| `invalid peer certificate: ... CaUsedAsEndEntity` | 5 | The daemon certificate is marked `CA:TRUE`. Reissue it with `basicConstraints=critical,CA:FALSE`. |
| `peer sent no certificates` | 5 | The daemon requires a client certificate (`quic client ca file`). Pass `--quic-cert` and `--quic-key`. |
| `--quic-cert requires --quic-key` | 5 | Pass both halves of the client identity. |
| `QUIC connection to ... failed: timed out` after about 30 s | 5 | Nothing answers on that UDP port: wrong port, a firewall dropping UDP, a daemon without QUIC directives, or a daemon whose QUIC identity failed to load (check its log). |
| Daemon exits at startup naming `quic cert file` / `quic key file` | 1 | Set both directives, in the global section. |

## Known limitations

- **Throughput.** QUIC is slower than TCP. On a 512 MiB single-file transfer
  over loopback (aarch64 Linux VM, release build, `--whole-file`), QUIC moved
  480 to 690 MiB/s against 2,700 to 3,400 MiB/s for TCP, so 4.5 to 6 times
  slower, and the QUIC push client used about 120 MB of RSS against 14 MB for
  TCP.
- **No QUIC in release artifacts.** QUIC requires a source build with the
  feature.
- **oc-rsync only.** There is no QUIC interop with upstream `rsync`.
- **Daemon listener is Unix-only**, and a Windows daemon silently ignores QUIC
  directives (see [Platform support](#platform-support)).
- **An unusable certificate or key does not fail daemon startup**; the daemon
  keeps serving TCP and logs the QUIC error.
- **FreeBSD is untested.**

## Future work

Additional performance-oriented ALPN protocols are planned. They will be
selectable through a command-line option, and the current ALPN, `rsync`, will
stay the default for now.

## See also

- [`ssh-transport.md`](ssh-transport.md): the SSH transport.
- Design notes: [`design/quic-transport-policy.md`](design/quic-transport-policy.md)
  (certificate, ALPN and scheme policy) and
  [`design/quic-transport-concurrency-model.md`](design/quic-transport-concurrency-model.md).
