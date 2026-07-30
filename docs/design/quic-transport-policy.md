# QUIC Transport: Certificate Strategy, ALPN, and Scheme Policy

Tracking task: QUIC-4. Status: decision note awaiting owner review. No code, no
CLI flags, no config directives are introduced by this note - it fixes the
policy surface so the build-out tasks (QUIC-5 through QUIC-11) implement one
agreed design rather than discovering it.

## 0. Framing: where QUIC sits relative to upstream

Upstream rsync deliberately does **not** terminate TLS inside the daemon. Its
own answer to "encrypt an rsync-daemon connection" is a front-end proxy
(haproxy/nginx) plus the `rsync-ssl` client helper script; `rsyncd.conf(5)` is
explicit that the daemon "only encrypts ... if you put rsync behind an SSL
proxy" and documents no in-daemon certificate parameters. The client helper
speaks to that proxy over stunnel/openssl/gnutls and exposes exactly four
knobs, all environment variables: `RSYNC_SSL_CERT`, `RSYNC_SSL_KEY`,
`RSYNC_SSL_CA_CERT`, and `RSYNC_SSL_PORT` (default 874).

A native QUIC transport is therefore an **oc extension that adds reach** - a
new way to carry the *unmodified* daemon wire protocol - and must not fork
upstream policy. Two consequences bound every decision below:

1. **The bytes inside the tunnel are upstream's.** QUIC replaces the TCP
   socket, nothing above it. Version selection, module negotiation, auth, and
   framing remain the `@RSYNCD:`/binary protocol exactly as on TCP (this is the
   QUIC-7 differential-oracle invariant: the QUIC session transcript must be
   byte-identical to the TCP daemon session). No decision here may introduce a
   second version-negotiation channel or any wire feature beyond what upstream
   defines.
2. **Vocabulary parity beats novelty.** Where upstream `rsync-ssl` already names
   a concept (cert, key, CA cert, the "one higher than 873" port convention),
   we mirror the name so an operator arriving from `rsync-ssl` finds the
   familiar concept. We only invent a name where upstream has none (it has none
   for in-daemon identity, because it has no in-daemon TLS).

This note also keeps faith with the project's stated Tier-2 documentation
(QUIC-10 / WIN-4), which records the *deliberate absence* of daemon TLS to
date. QUIC does not contradict that; it is an explicit, feature-gated,
opt-in addition, off by default, compiled only under the `quic` cargo feature
(the sole place the TLS amendment permits rustls).

## 1. Decision summary

| # | Decision | Recommendation |
|---|----------|----------------|
| A | Server identity | `quic cert file` / `quic key file` global directives; zero-config default is an ephemeral per-start self-signed cert generated in memory at bind time (not persisted; rotates each restart) |
| B | Client verification | System roots by default; `--quic-ca <pem>` for private CAs; **TOFU known-hosts** (`quic_known_hosts`) as the primary self-signed path, mirroring SSH; **no** blanket insecure flag |
| C | ALPN | Single fixed token `rsync` (bytes `0x05 r s y n c`); mismatch = TLS-layer connection refusal mapped to exit 5 |
| D | Scheme/flag | New `quic://host[:port]/module` scheme **and** `--quic` modifier on a daemon target; hard failure (never silent TCP fallback) on a non-QUIC endpoint; default `873/udp` |
| E | Out-of-scope | 0-RTT, connection migration, mTLS/client certs, MASQUE - all deferred with revisit triggers |

## 2. Decision A - Server identity

### Directives

Two `oc-rsyncd.conf` **global** directives (they describe the listener, not a
module):

```
quic cert file = /etc/oc-rsync/quic/server.pem
quic key file  = /etc/oc-rsync/quic/server.key
```

Naming follows the existing parser convention exactly: directives are written
as space-separated words and matched after whitespace-normalisation (`pid
file`, `lock file`, `use chroot`, `secrets file`). `quic cert file` /
`quic key file` sit naturally beside `pid file` / `secrets file` and read as
obvious extensions of the same family. They parse as global-only; appearing
inside a `[module]` header is a config error (identity is per-listener, not
per-module - a module cannot present a different certificate on a shared UDP
socket).

*Alternative considered - reuse a single `quic cert file` holding a combined
PEM (cert+key), as haproxy's `crt` does.* Rejected: upstream `rsync-ssl` keeps
`RSYNC_SSL_CERT` and `RSYNC_SSL_KEY` distinct, and separate files match the
overwhelmingly common Let's Encrypt layout (`fullchain.pem` +
`privkey.pem`). Mirroring that split is the lower-surprise choice. A combined
file can be added later as an ergonomic shortcut without breaking the split
form.

### Zero-config LAN default

When neither directive is set, the daemon generates a self-signed certificate
via `rcgen` **in memory at bind time** (exactly as the QUIC-1 skeleton's
`QuicAcceptor::bind` already does) and never writes it to disk. The certificate
is **ephemeral**: it is regenerated on every start, so the daemon's identity
rotates each restart.

Identity resolution, in priority order:

1. If `quic cert file` / `quic key file` are set, use them verbatim (operator
   owns identity; nothing is generated). This is the path an operator picks for
   a stable, persistent identity - a Let's Encrypt pair, a private-CA-signed
   cert, or a hand-generated self-signed cert kept on disk.
2. Else the listener mints a fresh in-memory self-signed cert at bind. No state
   directory is created, no files are written, and the key never touches disk.

**The tradeoff, stated plainly:** an ephemeral per-start identity means every
daemon restart presents a new certificate. A TOFU client (Decision B,
`quic_known_hosts`) that pinned the previous fingerprint will therefore see a
changed fingerprint after a restart and refuse to connect until the operator
clears the stale pin. Operators who want a stable TOFU identity **must** set
`quic cert file` / `quic key file` so the certificate survives restarts; the
zero-config default trades that stability for a genuinely zero-state daemon that
writes nothing to disk. This keeps the zero-config path simple and side-effect
free (no directory to create, permission-check, secure, or clean up) at the cost
of TOFU stability, which the explicit-cert path recovers.

*Alternative considered - persist the generated pair under a `quic/`
subdirectory of the config directory (`/etc/oc-rsync/quic/self-signed.{pem,key}`,
`0700`/`0600`).* Rejected as the default: it makes the zero-config daemon write
private-key state to disk (with the attendant permission, read-only-filesystem,
and cleanup concerns) purely to smooth TOFU. An operator who values a stable
identity can express it directly and unambiguously with the two directives,
which also documents the intent in the config. Keeping the default ephemeral
means the daemon has no hidden on-disk identity to reason about.

## 3. Decision B - Client verification ladder

The ladder, in the order the client applies it:

1. **System root store** (default). A cert chaining to a platform CA - e.g. a
   Let's Encrypt cert on a public daemon - verifies with zero flags, hostname
   checked against the `quic://` authority. This is the upstream
   `RSYNC_SSL_CA_CERT`-unset behaviour ("default CA set AND verify") carried
   forward.
2. **`--quic-ca <pem>`** for a private CA. Direct analogue of
   `RSYNC_SSL_CA_CERT=<file>` ("use CA AND verify"). Replaces the system store
   with the supplied bundle; hostname still checked.
3. **TOFU known-hosts** for self-signed identities (the zero-config server
   default). On first connect to an unknown authority the client pins the
   server's SPKI (SHA-256 of the SubjectPublicKeyInfo) in a known-hosts file
   and proceeds; on every later connect it requires the same key.

### The hard one: what replaces `--quic-insecure`

The transport spec's placeholder `--quic-insecure` (accept any cert, verify
nothing) is the option we most want to avoid shipping as-is: a blanket
"verify nothing" flag is precisely the MITM-open posture, and once it exists in
scripts it never leaves. Three replacements were weighed:

| Option | UX | Security | Verdict |
|--------|----|---------| --------|
| (1) blanket `--quic-insecure` + loud once-per-run stderr warning | trivial | none - accepts any key silently after the warning; warnings get scripted away | reject as primary |
| (2) explicit pin `--quic-known-key=SHA256:<b64>` | must obtain the key out of band | strong - pins exactly one key | keep as escape hatch |
| (3) TOFU known-hosts file | first-connect prompt, then invisible | strong after first connect; first connect is trust-on-first-use, exactly SSH's model | **recommend as primary** |

**Recommendation: (3) TOFU as primary, (2) explicit pin as the single escape
hatch. No blanket insecure flag.**

Rationale: this is *the* SSH workflow users already trust for exactly this
shape of problem - an unknown host presenting a self-signed key on first
contact. Reusing that mental model (a `quic_known_hosts` file that behaves like
`~/.ssh/known_hosts`) means the security property and the UX are both things
operators have internalised for decades. Option (2) covers the automation case
where a human can transport a fingerprint once (CI pinning a known server)
without a writable known-hosts file. Option (1) is refused because a flag whose
entire meaning is "disable the check" cannot be made safe by a warning; the
warning is the thing scripts delete first.

Known-hosts file location: `$XDG_CONFIG_HOME/oc-rsync/quic_known_hosts`,
falling back to `~/.config/oc-rsync/quic_known_hosts`, one `authority
SHA256:<base64-spki>` record per line. This mirrors SSH's per-user trust store
placement while staying inside oc-rsync's own config namespace (it is an
oc-extension file; it must not collide with or resemble an upstream path).

### Failure messages

All failures use the daemon-verification exit code (Decision C maps the code)
and the established upstream diagnostic format,
`rsync error: <text> (code N) at <file>(<line>) [client=<version>]`
(`logging::error_format`, mirroring `log.c:rwrite()`):

- Unknown host, TOFU would pin (informational, not fatal):
  `oc-rsync: quic: pinning new host key for example.com:873 (SHA256:AbC…); add --quic-ca or --quic-known-key to verify out of band`
- Key changed vs. pinned (fatal, MITM-shaped):
  `rsync error: quic host key mismatch for example.com:873: pinned SHA256:AbC… got SHA256:XyZ… (code 5) at <file>(<line>) [client=<version>]`
  followed by the SSH-style remediation line naming the exact known-hosts line
  to remove.
- Cert chain rejected under `--quic-ca`/system roots (fatal):
  `rsync error: quic certificate verify failed for example.com:873: <rustls reason> (code 5) at <file>(<line>) [client=<version>]`

The key-mismatch message deliberately reads like SSH's
`REMOTE HOST IDENTIFICATION HAS CHANGED` because the threat is identical and the
operator's muscle memory should transfer.

## 4. Decision C - ALPN

**Recommendation: a single fixed ALPN token, `rsync`** - the exact
`ALPN_RSYNC = b"rsync"` constant the QUIC-1 skeleton already advertises on both
endpoints. On the wire that is the ALPN protocol-list entry `0x05 0x72 0x73
0x79 0x6e 0x63` (length-prefixed `rsync`).

The conflict, stated explicitly: ALPN *can* carry a version token
(`rsync/32`, `rsync/31`, …) and some designs use it to pre-select a protocol
before the first application byte. But the daemon protocol **already owns
version selection** in-band (`@RSYNCD: <ver>` greeting / binary version
exchange). Encoding the version a second time in ALPN creates two sources of
truth that can skew: an ALPN token could advertise a version the in-band
handshake then contradicts, and we would have to define which wins. That skew
is a wire-fidelity hazard for no benefit - the differential oracle (QUIC-7)
requires the in-band bytes to be identical to TCP, so the in-band handshake is
authoritative by construction. A version-bearing ALPN token would be redundant
at best and contradictory at worst.

Therefore ALPN carries exactly one job: "the application inside this QUIC
connection is the rsync protocol," so a QUIC endpoint that is not an oc-rsync
daemon (or a shared-port service) refuses at the TLS layer instead of feeding
garbage into the rsync framer.

**Mismatch behaviour:** if the peer does not offer `rsync`, the TLS handshake
fails with `no_application_protocol`; the connection never reaches the rsync
framing layer. This is a setup-time failure and maps to exit **5**
(`RERR_STARTCLIENT`, "error starting client-server protocol ... the initial
handshake with the daemon fails") - the same class as any other pre-transfer
handshake failure, distinct from exit 10 (`SocketIo`) which is reserved for I/O
errors *after* a connection is established.

## 5. Decision D - Scheme and flag

### How a QUIC transfer is requested

**Recommendation: introduce a `quic://` URL scheme *and* a `--quic` modifier**,
matching how upstream exposes daemon transport two ways (`rsync://host/mod` URL
and `host::mod` shorthand):

- `quic://host[:port]/module/path` - explicit scheme, parallel to
  `rsync://`.
- `--quic host::module/path` - the `--quic` flag reinterprets an otherwise
  ordinary daemon target as QUIC, parallel to how `rsync-ssl` wraps a
  `host::module` target without changing its syntax. This is the lower-friction
  form for users converting an existing `rsync://`/`::` invocation.

Both are equivalent; `quic://` is the canonical form documentation leads with.

### No silent downgrade (hard requirement)

A QUIC-selected transfer to an endpoint that does not speak QUIC **must fail
loudly**, never fall back to TCP. Falling back would silently downgrade an
encrypted transport to plaintext - the classic downgrade attack, and a
violation of the user's explicit intent. Concretely: if the UDP endpoint does
not complete a QUIC handshake (no response, or a non-QUIC service), the client
reports a connection failure and exits 5; it does **not** retry on TCP 873.
This is the one place the design is deliberately less "helpful" than a
fallback would be, and it is non-negotiable.

### Port

**Default `873/udp`.** QUIC runs over UDP, a distinct namespace from TCP, so
sharing the number with the TCP daemon port (873) carries no conflict and keeps
one number for "the rsync daemon" regardless of transport. This diverges from
`rsync-ssl`'s 874 convention - but 874 exists only because stunnel/TLS-over-TCP
needed a *second TCP port* beside 873; QUIC has no such collision because
`873/udp` and `873/tcp` are independent. Choosing `873/udp` is the honest
mapping of "same service, different transport." An operator running both TCP
and QUIC daemons binds `873/tcp` and `873/udp` with no clash.

*Alternative considered - default `874/udp` to echo rsync-ssl.* Rejected: 874
is a TCP-collision workaround, not a semantic "secure rsync" number; carrying
it to UDP would propagate an accident. Documented clearly so rsync-ssl users
understand why the number differs.

Port precedence mirrors the existing daemon path: an explicit `:port` in the
`quic://` authority wins, then `--port`, then the `873/udp` default.
`--address`, `--ipv4`/`-4`, `--ipv6`/`-6` apply unchanged to the UDP socket;
IPv6 literals use the standard bracketed authority `quic://[2001:db8::1]:873/mod`.

### Daemon-side config

The daemon needs to know whether to open a UDP/QUIC listener and on what
address/port. Two shapes were weighed:

| Shape | Config | Verdict |
|-------|--------|---------|
| Dedicated directives | `quic = yes` (enable), `quic port = 873`, `quic listen = <addr>` | **recommend** |
| Reuse `port` + toggle | `port = 873` shared, `quic = yes` flips transport | reject |

**Recommendation: dedicated directives.** `quic = yes` enables the listener;
`quic port` and `quic listen` set the UDP bind (defaulting to `873` and the
global `address`). Reusing the single `port` directive is rejected because an
operator will legitimately want the TCP daemon on `873/tcp` *and* QUIC on
`873/udp` simultaneously (staged rollout, mixed-client fleet); one shared
`port` cannot express two listeners, and forcing an either/or would make QUIC
adoption a cutover instead of an addition. Dedicated directives let the two
listeners coexist behind one config file - the operator-ergonomics win that
matters for real deployments. `quic port` naturally defaults to the same 873 as
the transport default, so the common case is still `quic = yes` plus the two
cert directives from Decision A.

## 6. Decision E - Out-of-scope ledger

Each deferred, with the one thing that would reopen it:

- **0-RTT / early data** - deferred. 0-RTT application data is replayable by a
  network attacker; rsync's first bytes drive module selection and would need
  replay-safety analysis. Revisit only if a measured connection-setup latency
  problem on high-RTT links (the QUIC-8 benchmark) justifies the replay-hardening
  work.
- **Connection migration** - deferred. Surviving a client IP change is a real
  QUIC advantage for mobile/roaming clients, but rsync sessions are short and
  path-pinned in practice. Revisit if a concrete roaming use case appears.
- **Client certificates / mTLS** - deferred. The daemon authenticates with its
  existing `auth users` / `secrets file` mechanism *inside* the tunnel;
  duplicating auth at the TLS layer adds a second credential system for no
  upstream-parity reason. Revisit if an operator needs transport-layer client
  identity independent of module auth. The skeleton's `with_no_client_auth()`
  encodes this default.
- **MASQUE / HTTP-tunnelled QUIC** - deferred. Proxy traversal is a genuine
  reach feature but a large surface with no upstream analogue. Revisit only on
  explicit demand, as a separate feature behind its own gate.

## 7. Precedent conflicts surfaced

1. **Port number: `873/udp` vs rsync-ssl's `874`.** Documented in Decision D.
   The note chooses `873/udp` on transport-namespace grounds and explains the
   divergence so migrating rsync-ssl users are not surprised.
2. **In-daemon TLS vs upstream's proxy-only stance.** Upstream `rsyncd.conf`
   documents *no* in-daemon cert parameters by design; the `quic cert file` /
   `quic key file` directives are genuinely new. They are justified as an
   oc-extension (Section 0) and named to sit inside the existing directive
   family, but there is no upstream directive to mirror - only the `rsync-ssl`
   *environment* vocabulary (`RSYNC_SSL_CERT`/`_KEY`/`_CA_CERT`), which the
   directive and flag names deliberately echo.
3. **Verification: env vars vs flags.** rsync-ssl exposes CA/cert/key as
   environment variables; this note exposes client-side verification as flags
   (`--quic-ca`, `--quic-known-key`) plus a TOFU file, because oc-rsync's client
   surface is CLI flags, not the shell-script env-var interface `rsync-ssl`
   inherited from wrapping openssl/stunnel. The *concepts* map 1:1; the delivery
   mechanism matches oc-rsync's own conventions rather than the wrapper's.
