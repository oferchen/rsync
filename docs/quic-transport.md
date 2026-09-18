# QUIC transport (operator guide)

`oc-rsync` can carry the rsync daemon protocol over QUIC instead of a plain
TCP connection. QUIC replaces only the transport: the bytes inside the tunnel
are the unmodified daemon protocol, so a QUIC session and a TCP session move
the same wire format and negotiate the same protocol version. QUIC adds a
mandatory TLS 1.3 layer, a UDP substrate, and modern congestion control on top
of that stream.

## Status and support tier

QUIC is an **oc-rsync extension** with no counterpart in upstream `rsync`, and
it is **Tier 2**: opt-in, **off by default**, and compiled only when the `quic`
Cargo feature is enabled. A default build does not link the QUIC stack and does
not understand `quic://` operands or the `--quic` family of flags. Every flag,
directive, and behaviour in this guide requires a build produced with the
feature turned on.

Because it is an extension, QUIC only works oc-rsync-to-oc-rsync: an upstream
`rsync` peer speaks neither QUIC nor the daemon protocol over UDP. Nothing in
this guide changes the behaviour of a TCP or SSH transfer.

## Building with QUIC

Enable the `quic` feature at build time:

```sh
cargo build --release --features quic
```

The feature propagates through the crate graph (`cli` -> `core` ->
`rsync_io`, and `daemon` for the listener side), pulling in the QUIC stack
(`quinn-proto` for the sans-I/O QUIC state machine, `rustls` with the ring
crypto provider for TLS 1.3, and `rustls-native-certs` for the system trust
store; `rcgen` generates self-signed certificates for the QUIC test suite
only, not at runtime). A build without the feature is byte-for-byte unaffected
on every other transport.

## Invoking QUIC (client)

There are two ways to ask a client to use QUIC, and both target an rsync
**daemon** endpoint (QUIC does not apply to SSH `host:path` transfers):

- **The `quic://` URI scheme** — the QUIC analogue of `rsync://`:

  ```sh
  oc-rsync -a quic://host/module/path/ /local/dest/
  oc-rsync -a quic://user@host:8730/module/path/ /local/dest/
  ```

- **The `--quic` modifier** — upgrades an ordinary daemon target to QUIC
  without changing the operand spelling:

  ```sh
  oc-rsync -a --quic host::module/path/ /local/dest/
  oc-rsync -a --quic rsync://host/module/path/ /local/dest/
  ```

Both select `Transport::Quic` for the connection. The default port is **873**,
shared with the TCP daemon port (`--port` overrides it); a `quic://` URI may
name its own port after the host. QUIC dials over **UDP**, so the daemon must be
listening on `873/udp` (or the chosen port).

### No silent fallback

When QUIC is requested but the connection cannot be established, the client
**hard-fails**. It never silently downgrades to plaintext TCP. This is a
deliberate security property: an operator who asked for the encrypted transport
gets an error rather than an unnoticed cleartext transfer.

If a `quic://` operand is given to a build that lacks the `quic` feature, the
client fails fast with an actionable diagnostic ("quic:// URLs require the QUIC
transport, which is not compiled into this build") rather than misparsing the
URL as a host named `quic`.

## Trust model (TLS 1.3)

Every QUIC connection runs TLS 1.3 with the ALPN protocol identifier `rsync`;
there is no unencrypted QUIC mode. The client verifies the daemon's
certificate, and the verification source is chosen by a fixed precedence:

1. **Private CA (`--quic-ca <PATH>`)** — highest precedence. Verifies the
   daemon's certificate chain against the PEM CA bundle at `PATH` instead of the
   platform trust store. Use this when the daemon's certificate is anchored by
   an internal or corporate CA.

   ```sh
   oc-rsync -a --quic-ca /etc/oc-rsync/corp-ca.pem quic://host/module/ dest/
   ```

2. **System roots (default)** — when `--quic-ca` is not given, the certificate
   is verified against the live platform trust store. This is the zero-config
   path for a daemon whose certificate is signed by a publicly trusted CA.

3. **TOFU `quic_known_hosts`** — the trust-on-first-use path for a self-signed
   daemon, mirroring SSH's `known_hosts`. On first contact the server
   certificate's SHA-256 fingerprint is pinned into a known-hosts file
   (recorded as a `SHA256:<base64>` token, the same form SSH prints). A later
   connection to the same authority whose fingerprint has **changed** aborts the
   handshake loudly. There is no blanket "insecure" escape hatch that accepts any
   certificate.

Because a changed fingerprint aborts a TOFU-pinned connection, rotating the
daemon's certificate invalidates every client's pin for that authority: each
client trips the loud "host key changed" abort until its stale
`quic_known_hosts` line is removed. Keep the daemon's certificate and key
stable across restarts, and plan a coordinated re-pin when you rotate them.

### Mutual TLS (client certificate)

The client can additionally present its own certificate so the daemon can
authenticate it (mutual TLS), the reverse direction of the server verification
above. It is opt-in and off by default: with neither flag set the client
presents no certificate and the handshake is unchanged.

```sh
oc-rsync -a --quic-cert /etc/oc-rsync/client.pem \
            --quic-key  /etc/oc-rsync/client.key \
            quic://host/module/ dest/
```

- `--quic-cert <PATH>` is the PEM certificate chain (leaf first) the client
  presents; `--quic-key <PATH>` is its PEM private key (PKCS#8, PKCS#1, or SEC1).
- The pair is **all-or-nothing**: naming only one of the two is an error, since a
  client certificate needs its private key (and vice versa).
- Whether a client certificate is *required* is the daemon's decision (see
  `quic client ca file` below); a daemon that does not request one ignores a
  presented certificate.

## Daemon configuration

The QUIC listener runs alongside the daemon's TCP listener and is configured
with global directives in `oc-rsyncd.conf`. Setting **any** QUIC directive
(`quic cert file`, `quic key file`, `quic client ca file`, or `quic port`) marks
QUIC as requested; a config with no QUIC directives leaves the listener off, so a
default `--features quic` daemon stays TCP-only until you configure it.

The QUIC daemon listener is **Unix-only**. On other platforms a `quic`-feature
build can still dial a QUIC daemon as a client, but cannot open a local QUIC
listener.

"Requested" is not "serviceable". Because there is no ephemeral fallback (see
below), a QUIC request without **both** a certificate and a key is a fatal
misconfiguration: the daemon fails loudly at startup with

```
QUIC listener requested but no certificate configured: set both
`quic cert file` and `quic key file` (there is no ephemeral fallback)
```

rather than synthesizing an identity or silently skipping the listener.

### Certificate identity

```
quic cert file = /etc/oc-rsync/quic/server.pem
quic key file  = /etc/oc-rsync/quic/server.key
```

- `quic cert file` / `quic key file` name the PEM certificate and private key
  the daemon presents. These sit naturally beside `pid file` / `secrets file`;
  like those directives they only resolve and store a path — the files are read
  when the listener is built, not at parse time.
- The pair is **all-or-nothing**: naming only `quic cert file` or only
  `quic key file` leaves QUIC requested but unserviceable, and the daemon
  refuses to start (see the startup error above). The listener needs both to
  present an identity.

Both directives are **global-only** and their paths resolve relative to the
config file (the same handling as `pid file` / `lock file`), with any `%`
tokens left verbatim for expansion at listener-bind time. A module section
that sets either one is rejected.

### Requiring a client certificate (mutual TLS)

```
quic client ca file = /etc/oc-rsync/quic/clients-ca.pem
```

- `quic client ca file` names a PEM CA bundle. When set, the QUIC listener
  **requires** every connecting client to present a certificate and verifies its
  chain against this bundle; a client that presents no certificate, or one not
  anchored by this CA, is refused in the TLS handshake before any `@RSYNCD:`
  byte is exchanged. This is the daemon-side mirror of the client's `--quic-ca`
  server verification.
- It is **off by default**: unset, the listener requests no client certificate,
  so existing configurations behave exactly as before.
- Global-only and path-handled exactly like `quic cert file` / `quic key file`
  (a per-module use is a configuration error). It requires a configured
  `quic cert file` / `quic key file`, since the listener still needs its own
  identity to present.

### No ephemeral fallback

QUIC has **no auto-generated or in-memory certificate**. The listener presents
only the operator-supplied `quic cert file` / `quic key file` pair; if that
pair is absent the daemon fails loudly at startup rather than minting a
throwaway self-signed identity. (Auto-generating a daemon certificate was
considered and dropped: it created more problems than it solved, chiefly the
unstable identity that would trip every TOFU pin on each restart.)

A bad or unreadable certificate does not take the whole daemon down: the
affected QUIC socket is logged and skipped while the TCP listener keeps
serving, degrading to TCP-only rather than a total outage.

### Port selection

```
quic port = 8873
```

- The `quic port` global directive selects the UDP port the QUIC listener
  binds. When unset, the QUIC listener **shares the daemon `port`** (873 by
  default, or whatever `port` is set to) — the same port number, on UDP.
- `quic port = 0` coerces to the well-known rsync port **873** (matching the
  `port = 0` / `--port 0` coercion), not a kernel-assigned ephemeral port.
- `quic port` is a **global** directive: the QUIC listener is shared across all
  modules, so a per-module `quic port` is a configuration error.

## Tuning: congestion control and flow-control window

Two client-side flags tune QUIC's pacing and back-pressure. They are pure
performance knobs: they change only how fast bytes leave and how many may be in
flight before an acknowledgement, never a wire byte or a protocol semantic. A
peer running any setting interoperates with a peer running any other, so a
mismatch is only ever a throughput difference, never a compatibility problem.
Both flags live in the oc-rsync extensions help group and require the `quic`
feature.

### `--quic-cc` — congestion controller

Selects the client endpoint's congestion-control algorithm:

| Value     | Algorithm | Character                                             |
|-----------|-----------|-------------------------------------------------------|
| `bbr`     | BBR       | Model-based; the **default**, the high-BDP fit.       |
| `cubic`   | CUBIC     | Loss-based; the modern TCP default.                   |
| `newreno` | NewReno   | Loss-based; the QUIC baseline.                        |

```sh
oc-rsync -a --quic-cc cubic quic://host/module/ dest/
```

- **Resolution order:** `--quic-cc` (CLI) > `OC_RSYNC_QUIC_CC` (environment) >
  the built-in default, **BBR**.
- An unrecognised controller name is a **hard error**, not a silent fallback —
  on the flag and on the environment variable alike.

**When to pick which:** BBR (the default) is the right choice on high
bandwidth-delay-product links (long fat networks — high throughput and high
latency), where it fills the pipe without waiting on loss signals. CUBIC or
NewReno are loss-based and behave like traditional TCP; prefer them on links
where BBR is undesirable, e.g. where fairness with competing loss-based flows
matters.

### `--quic-window` — flow-control window

Sizes the client endpoint's QUIC flow-control window in bytes, with an optional
`K`/`M`/`G` binary (1024-based) suffix:

```sh
oc-rsync -a --quic-window 128M quic://host/module/ dest/
```

- **Resolution order:** `--quic-window` (CLI) > `OC_RSYNC_QUIC_WINDOW`
  (environment) > the built-in default of **64 MiB**.
- The window must cover the link's bandwidth-delay product (BDP =
  `bandwidth * round-trip-time`), or the sender stalls waiting for
  acknowledgements before the pipe is full. The 64 MiB default saturates a
  single stream up to roughly 5 Gbit/s at a 100 ms RTT — a generous envelope
  that still bounds per-connection receive memory. It deliberately overshoots
  the QUIC library's own ~1.25 MiB default, which would cap a 100 ms-RTT stream
  near 100 Mbit/s.

**When to size the window:** raise it on high-BDP links (high bandwidth *and*
high latency) where the 64 MiB default is smaller than `bandwidth * RTT`; the
ideal value is the BDP of the target link. On ordinary LAN or low-latency links
the default is already ample. The default is a defensible starting point, not a
per-link optimum — confirm any specific value with a real throughput
measurement on the target path before treating it as tuned.

## Environment variable reference

| Variable              | Flag equivalent  | Default | Notes                                         |
|-----------------------|------------------|---------|-----------------------------------------------|
| `OC_RSYNC_QUIC_CC`    | `--quic-cc`      | `bbr`   | `bbr` \| `cubic` \| `newreno`; unknown = error |
| `OC_RSYNC_QUIC_WINDOW`| `--quic-window`  | 64 MiB  | Byte count, optional `K`/`M`/`G` suffix        |

The CLI flag takes precedence over the environment variable, which takes
precedence over the built-in default, resolved at one site so the order is
consistent between the client and the daemon endpoints.

## See also

- [`ssh-transport.md`](ssh-transport.md) — the SSH transport.
- Design notes: [`design/quic-transport-policy.md`](design/quic-transport-policy.md)
  (certificate/ALPN/scheme policy) and
  [`design/quic-transport-concurrency-model.md`](design/quic-transport-concurrency-model.md).
