# QUIC Transport: Security Model and Authentication Review

Status: design review, awaiting owner decisions (section 7). No code changes
are made by this note. Base: `origin/master` at `fc7ac47e4`. Test-cell
references to PR #8060 (`ci/interop-quic-oc-daemon`, open) point at that
branch.

Scope: the oc-only QUIC transport (`quic` cargo feature). Upstream rsync has
no QUIC and terminates no TLS in the daemon, so there is no upstream behaviour
to mirror for the transport layer itself. Everything inside the tunnel
(`@RSYNCD:` greeting, module selection, `auth users`, the transfer protocol)
is upstream's and is out of scope except where it interacts with QUIC.

Dependency versions reviewed (from `Cargo.lock`): `quinn-proto` 0.11.18,
`rustls` 0.23.45, `rustls-native-certs` 0.8.4, `rcgen` 0.14.10. Library
defaults are cited against those crate sources in the cargo registry.

Every claim about current behaviour carries a `file:line` citation. Claims
that need a live run to confirm are marked **unverified**.

## 1. Summary

- **Critical defect (confirmed by PR #8060 interop cells).** `--quic-ca` does
  not make CA verification mandatory. A chain that fails verification against
  the supplied CA - untrusted issuer, wrong SAN, expired - falls through to
  trust-on-first-use, is pinned, and the transfer exits 0. This contradicts
  `docs/design/quic-transport-policy.md:127-129` and `:180-181`.
- **High: the no-flag default gives no protection to CA-anchored daemons.** A
  chain that validates against the system roots is accepted but never pinned,
  so a later MITM presenting any other certificate is treated as a first
  contact and pinned.
- **High (availability, unverified at runtime): no QUIC Retry.** Spoofed
  Initial packets can occupy all 128 unaccepted-connection slots for the idle
  timeout, locking every real client out of the QUIC listener.
- **Medium:** a daemon whose QUIC identity cannot load starts anyway and
  serves only TCP; mutual TLS gates QUIC only, so the same modules stay
  reachable over TCP without a certificate; no permission checks on either
  private-key file; the promised `--quic-known-key` pin does not exist.
- **Premise correction.** The earlier concern that the QUIC listener runs in
  the daemon parent and that one session's chroot/setuid can affect the whole
  daemon is stale on master. Since #8027 (which squashed #8028), a dedicated
  QUIC front process terminates QUIC and relays each stream to the
  single-threaded parent, which forks a per-session child exactly as for TCP
  (`crates/daemon/src/daemon/sections/server_runtime/quic_listener.rs:10-14`,
  `:123-181`, `accept_loop.rs:393-403`). The residual risk is narrower and is
  listed as G16.

The proposed model (section 5) has explicit server-trust modes, fail-closed
defaults, and a small PR sequence that ships the `--quic-ca` fix first
(section 6).

## 2. Inventory of current controls

Paths are relative to the repository root. `trust.rs`, `mod.rs`, `driver.rs`,
`cipher.rs` and `tuning.rs` are in `crates/rsync_io/src/quic/`. `connect.rs`
means `crates/core/src/client/module_list/connect/mod.rs`. `quic_listener.rs`,
`accept_loop.rs` and `reload.rs` are in
`crates/daemon/src/daemon/sections/server_runtime/`. `dispatch.rs` is
`crates/daemon/src/daemon/sections/config_parsing/global_directives/dispatch.rs`,
`module_directives.rs` is in `crates/daemon/src/daemon/sections/config_parsing/`,
`quic_identity.rs` is in `crates/daemon/src/daemon/runtime_options/`,
`fd_pass.rs` and `secrets.rs` are in `crates/platform/src/`, and
`landlock_stub.rs` is in `crates/fast_io/src/`. Library paths such as
`quinn-proto-0.11.18/src/...` and `rustls-0.23.45/src/...` are the crate
sources in the cargo registry. Upstream paths (`authenticate.c`,
`clientserver.c`, `socket.c`) are rsync 3.5.1.

### 2.1 Client surface

| Flag | Defined | Behaviour |
|------|---------|-----------|
| `--quic` | `crates/cli/src/frontend/command_builder/sections/build_base_command/network.rs:138-143` | Selects `Transport::Quic` for a daemon target (`crates/cli/src/frontend/execution/drive/config.rs:370-374`) |
| `--quic-ca PATH` | `network.rs:144-153` | PEM bundle loaded by `load_private_ca` (`trust.rs:518-551`), passed to `resolve` (`connect.rs:433-441`) |
| `--quic-cert PATH` / `--quic-key PATH` | `network.rs:154-173` | Client certificate for mutual TLS (`connect.rs:408-426`, `trust.rs:602-618`) |
| `--quic-cipher aes\|chacha20` | `network.rs:194-203` | Restricts the TLS 1.3 suite family (`cipher.rs:125-148`) |
| `--quic-cc`, `--quic-window` | `network.rs:174-193` | Transport tuning only, no security role |
| `--quic-known-key` | **does not exist** | Named in `quic-transport-policy.md:145-149` and in the client notice (`trust.rs:459-460`), but no flag is defined (no match under `crates/cli/src`) |

There is no flag for the known-hosts path. `resolve` is always called with
`known_hosts = None` (`connect.rs:441`), so the location comes from
`default_known_hosts_path` (`trust.rs:646-652`).

### 2.2 Server authentication (client side)

`open_quic_daemon_stream` (`connect.rs:393-474`) always builds trust via
`resolve(ca, "host:port", None)` (`connect.rs:439-441`). `resolve` always
returns `TrustPolicy::AcceptNew` (`trust.rs:761-772`), which materialises as
`AcceptNewVerifier` (`trust.rs:721-742`). The TLS server name is `addr.host()`
(`connect.rs:456`, `:465`).

`AcceptNewVerifier::verify_server_cert` (`trust.rs:382-402`):

1. If a root layer exists and validates, accept and return (`:393-398`). The
   root layer is the `--quic-ca` store when given (`:735`), otherwise the
   platform store (`:736`, `system_root_verifier` at `:448-453`). An empty
   platform store silently removes the layer (`:448-453`).
2. Otherwise call `TofuVerifier` (`:400-401`), whatever the reason the root
   layer failed.

`TofuVerifier::verify_server_cert` (`trust.rs:259-293`) ignores
`_intermediates`, `_server_name`, `_ocsp_response` and `_now` (`:262-266`).
It hashes the whole certificate DER, not the SPKI (`trust.rs:63-71`).

| Case | Result today | Message | Exit |
|------|--------------|---------|------|
| Chain valid for `--quic-ca` or system roots, name and time valid | Accept, nothing pinned (`trust.rs:393-398`) | none | 0 |
| No valid chain, authority unknown | Pin and accept (`trust.rs:282-286`) | `oc-rsync: quic: pinning new host key for H:P (SHA256:...); add --quic-ca or --quic-known-key to verify out of band` (`trust.rs:456-462`) | 0 |
| No valid chain, pin matches | Accept (`trust.rs:274`) | none | 0 |
| No valid chain, pin differs | Refuse (`trust.rs:275-281`) | SSH-style banner (`trust.rs:466-479`) then `QUIC connection to H:P failed: ... quic host key mismatch for H:P: pinned ... got ...; refusing to fall back to plaintext TCP` (`connect.rs:486-497`) | 5 |
| `--quic-ca` given, chain from another CA / wrong SAN / expired | **Pin and accept** (falls to TOFU at `trust.rs:400-401`) | pinning notice, which also tells the user to "add --quic-ca" although it is set | **0** |
| known-hosts unreadable | Refuse (`trust.rs:270-273`) | `quic: reading known-hosts for H:P: ...` | 5 |
| known-hosts not writable on first contact | Refuse (`trust.rs:287-290`) | `quic: pinning host key for H:P: ...` | 5 |
| No HOME / config dir | Refuse (`trust.rs:726-730`) | `cannot resolve a quic_known_hosts path (no HOME/config dir)` | 5 |
| `--quic-ca` missing, unparsable, or no certificate | Refuse before dialling (`trust.rs:518-551`, `connect.rs:433-438`) | `quic: reading --quic-ca file ...`, `quic: parsing certificate #N in --quic-ca file ...`, `quic: no certificates found in --quic-ca file ...` | 5 |

The CA-validated row is the only one where the SAN (`ServerName`, IP or DNS)
and the validity period are checked, by rustls `WebPkiServerVerifier`
(`trust.rs:431-440`). Revocation is not checked: the OCSP response is
ignored and no CRLs are configured.

The unit test `quic_nonmatching_ca_falls_through_to_accept_new`
(`connect.rs:949-1011`) asserts the defective behaviour, and
`resolve_yields_accept_new_verifier_with_and_without_ca` (`trust.rs:1077-1107`)
asserts that `--quic-ca` composes into the TOFU verifier.

`QuicTrust::Pinned` (`mod.rs:689-694`, built at `mod.rs:776-780`) installs one
certificate as the only WebPKI root. It still checks the name and time, so it
is not a key pin. Production code uses it only in tests.

### 2.3 Known-hosts file

| Property | Current | Cite |
|----------|---------|------|
| Location | `$XDG_CONFIG_HOME/oc-rsync/quic_known_hosts`, else `$HOME/.config/oc-rsync/...` (Unix), `%APPDATA%\oc-rsync\...` (Windows) | `trust.rs:646-671` |
| Format | `host:port SHA256:<base64 of cert DER hash>`, plain text, unhashed | `trust.rs:73-78`, `:163`, `:170-181` |
| Key | The authority as typed (`host:port`), not the resolved address | `connect.rs:439` |
| Create mode | dir `0700`, file `0600`, atomic rename; no-op on non-Unix | `trust.rs:150-165`, `:185-220` |
| Checked on read | No mode or owner check | `trust.rs:139-148` |
| Multiple keys per host | First matching line wins; no rotation support | `trust.rs:145-147` |
| Concurrency | Read-modify-rename without a lock; a concurrent pin can be lost (benign) | `trust.rs:150-165` |

### 2.4 Client authentication

| Control | Where | Behaviour |
|---------|-------|-----------|
| mTLS, client side | `connect.rs:408-426`, `mod.rs:787-795` | Both flags: cert presented. One flag only: refused before dialling, `--quic-cert requires --quic-key ...` / `--quic-key requires --quic-cert ...`, exit 5. Unreadable or mismatched key: `ClientAuth::from_pem_files` or rustls `with_client_auth_cert` error, exit 5 |
| mTLS, daemon side | `quic client ca file` (`crates/daemon/src/daemon/sections/config_parsing/global_directives/dispatch.rs:225-249`); verifier at `mod.rs:443-461` | `WebPkiClientVerifier` with default settings: a certificate is **mandatory** for every QUIC client, verified for chain and time against the CA. No name or identity check |
| Scope | `module_directives.rs:391-415` | Global only. A module-level `quic client ca file` is a hard config error |
| Identity to session | `quic_listener.rs:313-317` | `RelayRecord.client_identity` is always `None`. The session never learns who the certificate named |
| TCP bypass | `accept_loop.rs:393-398` | The TCP listener serves the same modules with no certificate requirement |
| `auth users` / `secrets file` | Unchanged daemon session code | Runs inside the tunnel as on TCP. Secrets-file strict modes are enforced by `platform::secrets::check_secrets_file_permissions` (`crates/platform/src/secrets.rs:26-58`), mirroring upstream `authenticate.c:172-179` |

mTLS and `auth users` are independent today: when both are configured, a
client must pass both, but nothing maps a certificate to an rsync user.

### 2.5 TLS parameters

| Item | Current | Cite |
|------|---------|------|
| TLS versions | TLS 1.3 only, both sides | `mod.rs:440-442`, `:773-775` |
| Provider | ring | `mod.rs:110-112`, `cipher.rs:125-148` |
| Suites | AES-256-GCM, AES-128-GCM, ChaCha20-Poly1305 (TLS 1.3 AEADs only). Client order is CPU-adaptive or forced by `--quic-cipher`; AES-128-GCM is always offered for QUIC Initial protection | `cipher.rs:31-47`, `:85-89`, `:125-148` |
| Server suite order | ring default; the server honours the client's order | `cipher.rs:74-75` |
| ALPN | `rsync`, both sides | `mod.rs:92`, `:465`, `:796` |
| ALPN mismatch | Handshake failure, exit 5 | `connect.rs:1063-1073` |
| 0-RTT | Off, **by library default only**: rustls client `enable_early_data: false` (`rustls-0.23.45/src/client/builder.rs:180`), server `max_early_data_size: 0` (`rustls-0.23.45/src/server/builder.rs:121`). The code uses `QuicServerConfig::try_from` / `QuicClientConfig::try_from` (`mod.rs:467-469`, `:798-800`), which keep those values; quinn's own `QuicServerConfig::new` would set `u32::MAX` (`quinn-proto-0.11.18/src/crypto/rustls.rs:487`) | |
| Resumption | Client: rustls in-memory cache (`client/builder.rs:172`), one per process, so no cross-process resumption. Server: stateful cache of 256 (`server/builder.rs:113`), no stateless ticketer (`:116`), 2 tickets per handshake (`:123`) | |
| Key logging | None. rustls `NoKeyLog` on both sides (`server/builder.rs:119`, `client/builder.rs:178`); no `KeyLogFile` or `SSLKEYLOGFILE` anywhere under `crates/` | |

### 2.6 QUIC transport controls

| Item | Current | Cite |
|------|---------|------|
| Retry / address validation | Never used. Every admitted Initial goes straight to `endpoint.accept`; there is no `endpoint.retry` call | `driver.rs:531-552` |
| Amplification | Only the protocol's built-in 3x anti-amplification limit for unvalidated paths (quinn-proto, RFC 9000 section 8.1) | library |
| Handshake backlog | 128 connections not yet handed to `accept()`, **including those still handshaking**. Beyond that, `endpoint.refuse` | `driver.rs:29-35`, `:509-516`, `:531-551` |
| Handshake timeout | None of its own. Bounded by quinn's default `max_idle_timeout` of 30 s (`quinn-proto-0.11.18/src/config/transport.rs:375`), because `build_transport_config` starts from `TransportConfig::default()` (`tuning.rs:201`). Client `--contimeout` is not applied: the QUIC arm receives no timeouts (`connect.rs:371-372`) | |
| Stream limits | quinn defaults: 100 peer-initiated bidi and 100 uni streams (`transport.rs:372-373`). Only one bidi stream is used (`driver.rs:299-313`) | |
| Flow-control window | 64 MiB per stream and per connection by default (`tuning.rs:55`, `:201-206`) | |
| Connection migration | Enabled (quinn `ServerConfig` default `migration: true`, `quinn-proto-0.11.18/src/config/mod.rs:244`); never changed (`mod.rs:467-472`) | |
| Stateless reset key | Random 64-byte HMAC key per endpoint (`quinn-proto-0.11.18/src/config/mod.rs:179-189`), from `EndpointConfig::default()` (`mod.rs:595-600`, `:829-840`) | |
| MTU discovery | Off (`mod.rs:593-599`) | |
| `--timeout` on QUIC reads | The QUIC reader has no readiness descriptor (`connect.rs:49-59`). Whether `--timeout` is enforced on a stalled QUIC session is **unverified** | |

### 2.7 TCP fallback

None. The QUIC arm never touches TCP (`connect.rs:365-373`). Every QUIC error
is mapped to `QUIC connection to H:P failed: <detail>; refusing to fall back
to plaintext TCP` with exit 5 (`connect.rs:476-497`), including an
unreachable daemon. `RSYNC_PROXY` and `RSYNC_CONNECT_PROG` are not consulted
on the QUIC arm (`connect.rs:371-372` passes neither), and nothing tells the
user they were ignored.

### 2.8 Daemon identity, startup, and privilege

| Step | Current | Cite |
|------|---------|------|
| Directives | `quic cert file`, `quic key file`, `quic client ca file`, `quic port`; global only; relative paths resolve against the config file's directory | `dispatch.rs:189-265`, `module_directives.rs:391-415` |
| QUIC requested | Any `quic *` directive set (there is no `quic = yes`) | `quic_identity.rs:59-65` |
| Cert or key directive missing | Refuse to start, `QUIC listener requested but no certificate configured: ...`, exit 1 | `quic_identity.rs:75-85`, `accept_loop.rs:51-57`, `crates/daemon/src/daemon.rs:106` |
| Ephemeral cert | Removed for the daemon (`quic_identity.rs:1-9`, `quic_listener.rs:19-33`). `QuicServerIdentity::Ephemeral` survives in `rsync_io` for tests (`mod.rs:348-392`) | |
| Startup order | bind TCP (`accept_loop.rs:220`), bind UDP (`:249`), daemonize (`:297`), pid file (`:331`), `daemon chroot` (`:349`), `daemon uid/gid` drop (`:366`), then fork the QUIC front (`:393-403`) | |
| Identity load | In the daemon **parent**, after chroot and privilege drop, just before the fork (`quic_listener.rs:139`, `:189-204`). So the paths resolve inside `daemon chroot`, and the files must be readable by the daemon's runtime uid | |
| Cert file missing / key unreadable / key does not match cert | `load_quic_setup` fails (rustls `CertifiedKey::from_der` rejects a mismatched key, `rustls-0.23.45/src/server/builder.rs:70`). The error is logged: `failed to load the QUIC server identity: ...` (`quic_listener.rs:202-203`). `start_quic_front` returns `None` and **the daemon keeps serving TCP only** (`quic_listener.rs:131-145`). The UDP sockets are dropped, so clients see a handshake timeout (PR #8060 `run_interop.sh:12711-12714`) | |
| Key-file permissions | None. `load_cert_chain_and_key` reads the PEM with no mode or owner check (`trust.rs:575-591`) | |
| Front process | Drops to `nobody` if still root (`quic_listener.rs:284-294`), dies with the parent (`:241-243`), removes all filesystem access with Landlock on Linux (`:244-252`; best effort on older kernels, `crates/fast_io/src/landlock.rs:233-245`; a no-op elsewhere, `landlock_stub.rs:67-69`). It never forks or serves a session (`:216-223`) | |
| Sessions | Each relayed stream reaches the parent over a socketpair with the real peer address, so `hosts allow` / `hosts deny` and `max connections` apply as for TCP (`quic_listener.rs:334-351`, `accept_loop.rs:393-398`) | |
| Certificate reload | None. SIGHUP re-reads modules (`reload.rs:1-61`) but the front process keeps the identity and client CA it loaded at start. Rotation needs a restart | |

## 3. Threat model

Assets: the transferred data, module credentials (`auth users` secrets), the
daemon's private key, the client's private key, the integrity of the client's
known-hosts file, and daemon availability.

| # | Adversary | Capability | Current exposure |
|---|-----------|-----------|------------------|
| T1 | Network MITM, on path | Intercept and answer UDP to the daemon address | **Wins against `--quic-ca` users** (G1). Wins against default-mode users of a CA-anchored daemon at any contact (G2). Wins against a self-signed daemon only on the very first contact (inherent to TOFU). Loses against an already-pinned self-signed daemon (`trust.rs:275-281`) |
| T2 | Impostor daemon | Runs its own QUIC endpoint at a name the victim dials (DNS spoofing, typo, stolen IP) | Same as T1. Pins are keyed by the typed authority (`connect.rs:439`), so DNS spoofing of a pinned name is caught; a new name is always first contact |
| T3 | Malicious client | Reaches the daemon over UDP | Parses attacker QUIC/TLS in a sandboxed front process (`quic_listener.rs:216-252`). Then the upstream daemon protocol, as on TCP. mTLS, if configured, stops it before `@RSYNCD:` (`mod.rs:443-461`), but only on QUIC (G5) |
| T4 | Credential theft | Obtains a module password or a client key | Password: same as TCP. Client key: no permission check on `--quic-key` (G6). A stolen client cert is valid until expiry; no revocation (G12) |
| T5 | Replay | Replays captured packets | 0-RTT is off (2.5), so no replayable early data. Replayed 1-RTT packets fail AEAD. Protected only by library defaults (G15) |
| T6 | Downgrade | Forces a weaker transport or cipher | No TCP fallback (2.7). TLS 1.3 only, AEAD suites only (2.5). The trust downgrade from CA to TOFU is G1/G2 |
| T7 | DoS / amplification | Spoofed or real UDP floods | Spoofed Initials can fill the 128-slot backlog for up to 30 s each (G3, unverified). Amplification limited to 3x by protocol. Per-connection memory up to the 64 MiB window across up to 200 peer streams (G9, unverified) |
| T8 | Local attacker reading keys | Another local user or a compromised session process | Daemon key must be readable by the daemon runtime uid (G7). No permission checks on either key file (G6). Key bytes pass through the parent's heap before session children are forked; whether they remain readable there is **unverified** (G7) |
| T9 | Cross-session leakage | One session reads or influences another | Premise of a shared parent is stale (section 1). All QUIC sessions still share one front process that holds every connection's traffic keys and plaintext in memory (G16) |

## 4. Gap analysis

Comparison points: OpenSSH (`StrictHostKeyChecking=yes|accept-new|no`,
`HashKnownHosts`, `UpdateHostKeys`, `StrictModes`, private-key mode checks),
curl (`--cacert` is strict and never falls back; `--pinnedpubkey
sha256//...` pins the SPKI and combines with CA verification), and
rustls/quinn practice (Retry under load, explicit stream limits, migration off
when unused, 0-RTT off unless replay-safe, no key logging in production).

| ID | Gap | Compared with | Severity |
|----|-----|---------------|----------|
| G1 | `--quic-ca` falls back to TOFU; untrusted CA, wrong SAN and expired certificates are pinned and served with exit 0 (`trust.rs:393-401`, `:761-772`, `:259-266`) | curl `--cacert` never falls back; policy `:127-129`, `:180-181` | **Critical** |
| G2 | Default mode: a CA-validated daemon is never pinned (`trust.rs:390-398`), so any later non-CA certificate for that authority is a "first contact" and is pinned. A public-CA certificate for another name is also pinned | OpenSSH `accept-new` refuses a changed key for a known host; here a CA-anchored host is never "known" | **High** |
| G3 | No Retry. Spoofed Initials hold backlog slots (handshaking counts toward 128, `driver.rs:29-35`, `:509-516`) until idle timeout, then real clients get `refuse` | quinn `Incoming::retry` / address validation; RFC 9000 section 8.1 | **High** (availability; runtime **unverified**) |
| G4 | Daemon with an unusable identity (missing cert file, unreadable key, key mismatch) starts and serves TCP only (`quic_listener.rs:131-145`); only a missing *directive* is fatal (`accept_loop.rs:51-57`) | sshd refuses to start without a loadable host key; fail-loud policy | **Medium** |
| G5 | mTLS gates only the QUIC listener; TCP serves the same modules with no certificate. mTLS is global only and the verified identity never reaches the session (`quic_listener.rs:316`) | An operator who sets `quic client ca file` reasonably expects the data to require a certificate | **Medium** |
| G6 | No permission checks on `quic key file` or `--quic-key` (`trust.rs:575-591`) | Upstream strict modes for the secrets file (`authenticate.c:172-179`) and the password file (`authenticate.c:258-265`, exit `RERR_SYNTAX`); OpenSSH refuses group/other-readable private keys | **Medium** |
| G7 | The daemon key is read after `daemon chroot` and uid drop, by the parent (`accept_loop.rs:349-403`, `quic_listener.rs:139`). It must be readable by the runtime uid and live inside the chroot, and its bytes pass through the heap that later session children inherit | sshd, nginx: read keys as root before dropping privileges | **Medium** |
| G8 | No pin-only mode. `--quic-known-key` is documented (`quic-transport-policy.md:145-149`, `:326`) and advertised by the notice (`trust.rs:460`) but not implemented | curl `--pinnedpubkey`; OpenSSH `StrictHostKeyChecking=yes` with a provisioned `known_hosts` | **Medium** |
| G9 | Server allows 100+100 peer-initiated streams with a 64 MiB window it never reads (`transport.rs:372-373`, `tuning.rs:55`, `driver.rs:299-313`) | Set peer stream limits to what the protocol needs | **Low** (runtime **unverified**) |
| G10 | Connection migration on (`quinn-proto config/mod.rs:244`) although the policy defers it (`quic-transport-policy.md:299-301`); `hosts allow` is judged once, at hand-off (`quic_listener.rs:310-317`) | Disable features not in use | **Low** |
| G11 | The pinning notice says "add --quic-ca" even when `--quic-ca` is set, and names a flag that does not exist (`trust.rs:456-462`) | Accurate diagnostics | **Low** |
| G12 | known-hosts: whole-certificate hash, not the SPKI the policy specifies (`trust.rs:54-71` vs `quic-transport-policy.md:131-133`); no hashing of host names; no permission check on read; no multi-key rotation; no revocation checking anywhere | OpenSSH `HashKnownHosts`, `UpdateHostKeys` | **Low** |
| G13 | Unreachable QUIC daemon exits 5, TCP exits 10 (`connect.rs:486-497` vs upstream `clientserver.c:163-165`); `--contimeout`, `RSYNC_PROXY` and `RSYNC_CONNECT_PROG` are silently not applied on the QUIC arm (`connect.rs:371-372`) | Exit-code parity; fail loud | **Low** |
| G14 | No certificate or client-CA reload on SIGHUP (`reload.rs`, 2.8) | nginx/sshd reload | **Low** (documentation) |
| G15 | 0-RTT off and key logging off only by library default; no test pins either (2.5) | A dependency upgrade or a switch to `QuicServerConfig::new` would silently enable 0-RTT | **Low** |
| G16 | One front process holds keys and plaintext for every QUIC session; Landlock is Linux only (`landlock_stub.rs:67-69`) | Per-connection isolation | **Low** (residual, mitigated) |

## 5. Proposed model

Principles: every mode is explicit; anything the user names is enforced or
the connection is refused; defaults fail closed; nothing is added without a
threat it addresses.

### 5.1 Server trust (client side)

Exactly one mode per connection, derived from the flags. No new mode flag is
needed.

| Flags | Mode | Accepts when | Checks | Writes known-hosts |
|-------|------|--------------|--------|--------------------|
| `--quic-ca FILE` | `ca` | Chain verifies to FILE | chain, SAN (DNS or IP), validity period | never |
| `--quic-known-key FP` (repeatable) | `pin` | Leaf fingerprint equals one of the FPs | fingerprint only; handshake signature | never |
| `--quic-ca FILE --quic-known-key FP` | `ca+pin` | Both of the above | all of the above | never |
| none | `tofu` (accept-new) | See below | See below | yes |

Rules:

- **`--quic-ca` never degrades.** `resolve` returns a strict WebPKI verifier
  when a CA is given. A failure is fatal with the policy's text:
  `quic certificate verify failed for H:P: <rustls reason>` (exit 5).
- **`pin` is a dedicated verifier**, not `QuicTrust::Pinned` (which checks
  name and time, `mod.rs:776-780`). It reuses `Fingerprint` and the
  handshake-signature methods of `TofuVerifier`. The fingerprint format is the
  existing `SHA256:<base64>` token of the certificate DER, the same token the
  pinning notice prints and known-hosts stores, so an operator can copy it
  (decision D3).
- **`tofu` default with a sticky CA marker (decision D2).** When the system
  roots validate the chain, the client accepts and records `H:P @ca` in
  known-hosts. When they do not validate:
  - authority marked `@ca`: refuse (`quic certificate verify failed ...;
    H:P previously verified against the system trust store`, exit 5);
  - pin present: require a match (today's behaviour);
  - nothing recorded: pin and accept with the notice.
  This keeps the task 134 zero-config experience for self-signed daemons and
  closes G2. A CA-anchored daemon can renew its certificate freely, because
  renewal still validates.
- **Notice text** (G11): `pinning new host key for H:P (SHA256:...); verify
  it out of band, or use --quic-ca or --quic-known-key`. It is printed only in
  `tofu` mode.

### 5.2 Client authentication (daemon side)

Three independent mechanisms, combined with AND:

| Mechanism | Configured by | Scope |
|-----------|---------------|-------|
| none | default | - |
| mTLS | global `quic client ca file` (unchanged: a certificate is mandatory on the QUIC listener when set) | transport |
| rsync auth | `auth users` / `secrets file` (unchanged, upstream) | module |

- **The daemon can require both.** It already can: the TLS layer refuses a
  QUIC client without a valid certificate, then the module asks for its
  password.
- **New per-module directive `require client cert = yes|no`** (default `no`,
  oc extension, decision D7). With `yes`, the module is refused unless the
  session arrived through the QUIC front with a verified client certificate.
  This is the only way to close the TCP bypass (G5) without adding a listener
  switch. The refusal uses the existing module-refusal path and text style,
  for example `@ERROR: access denied to MODULE from HOST (client certificate
  required)`, so the client exits 5 as for any daemon refusal.
- **No mapping from certificate to module user** in this round. The
  certificate is a transport gate; `auth users` stays the rsync identity. The
  front fills `RelayRecord.client_identity` (`fd_pass.rs:249`) with the leaf
  certificate so the parent can enforce `require client cert` and log the
  certificate's SHA-256 fingerprint in the session log line. No new `%` log
  tokens are added, in keeping with the upstream-only `%` expansion policy.
- **Per-module CA** stays unsupported. One QUIC listener presents one
  identity and one client-CA policy (`module_directives.rs:391-415`).

### 5.3 Key files and daemon startup

- **Fail-closed startup (G4, decision D4).** Any failure to load the QUIC
  identity or client CA - missing or unreadable file, bad PEM, key that does
  not match the certificate, bad permissions - stops the daemon before it
  serves anything, exit 1 (`RERR_SYNTAX`), the code the daemon already uses
  for a missing QUIC directive (`accept_loop.rs:51-57`) and upstream uses for a
  config it cannot load (`clientserver.c:1762-1764`). A UDP bind failure keeps
  its socket-error path (exit 10, upstream `socket.c:706-707`).
- **Load keys before dropping privileges (G7, decision D5).** Fork the QUIC
  front after the pid file is written and before `daemon chroot` / uid drop
  (between `accept_loop.rs:331` and `:345`). The front reads the identity as
  the startup user, reports success or the error text to the parent over a
  one-shot status channel, then drops to `nobody` and applies Landlock as
  today. The parent waits for that status and exits 1 on failure. The parent
  then never holds the private key, so later session children cannot inherit
  it, and the key can be `root:root 0600` outside the chroot, like an sshd
  host key. Paths keep resolving against the config file directory
  (`dispatch.rs:194-195`), no longer inside `daemon chroot`.
- **Permission checks (G6, decision D6).**
  - Daemon `quic key file`: the upstream secrets-file rule via
    `platform::secrets::check_secrets_file_permissions`
    (`crates/platform/src/secrets.rs:26-58`): not other-accessible, and
    root-owned when running as root, gated by the global `strict modes`
    value. Failure: `quic key file must not be other-accessible (see strict
    modes option): 'PATH'`, exit 1 at startup.
  - Client `--quic-key`: the upstream password-file rule
    (`authenticate.c:258-265`): refuse if other-accessible, with
    `ERROR: --quic-key file must not be other-accessible`, exit 1.
  - Both are no-ops on Windows, as for secrets files (`secrets.rs:59-60`).
- **Reload:** unchanged. Document that the identity and client CA are read at
  start only.

### 5.4 Transport settings

- **0-RTT off, pinned by test (G15).** Keep `try_from` and add unit tests that
  assert `enable_early_data == false` on the client config and
  `max_early_data_size == 0` on the server config before conversion. Key
  logging stays absent; a test asserts no `KeyLogFile` is installed.
- **Retry (G3, decision D10).** Answer every Initial without a validated
  address with a Retry (`endpoint.retry`) in `driver.rs:531-551`, so
  handshake state is allocated only for addresses that can receive. Cost: one
  extra round trip per connection.
- **Stream limits (G9).** Server: peer-initiated bidi 0, uni 0 (the server
  opens the only stream, `driver.rs:299-313`). Client: bidi 1, uni 0. Set in
  `build_transport_config` per role (`tuning.rs:183-208`).
- **Migration off (G10, decision D11).** `server_config.migration(false)`
  (`mod.rs:467-472`), matching the policy's deferral and TCP's fixed 5-tuple.
- **Timeouts (G13).** Apply `--contimeout` to the QUIC handshake (the time
  from dial to `wait_stream` returning, `mod.rs:315-339`). Keep quinn's 30 s
  idle timeout as the upper bound when `--contimeout` is unset.

### 5.5 TCP fallback (decision D8)

Keep the current rule: never fall back, and no opt-in fallback flag. An
opt-in fallback would be a scripted downgrade, the exact attack the policy
forbids (`quic-transport-policy.md:236-245`). Refuse `--quic` when
`RSYNC_PROXY` or `RSYNC_CONNECT_PROG` is set, instead of silently dialling
direct UDP: `--quic cannot be combined with RSYNC_PROXY` (exit 1).

### 5.6 Error texts and exit codes

| Condition | Text (after `QUIC connection to H:P failed: `) | Exit | Why |
|-----------|------|------|-----|
| No response, unreachable, handshake timeout | `no QUIC response` / io detail | **10** `RERR_SOCKETIO` | Same class as a failed TCP connect (upstream `clientserver.c:163-165`); decision D9 |
| CA verify failure (`ca`, `ca+pin`, `tofu` with `@ca`) | `quic certificate verify failed for H:P: <reason>` | 5 `RERR_STARTCLIENT` | Refusal before the protocol starts; matches the policy's text (`:180-181`) |
| Pin mismatch (`pin`, `ca+pin`, `tofu`) | `quic host key mismatch for H:P: pinned ... got ...` | 5 | Unchanged |
| ALPN mismatch, mTLS rejected by daemon | rustls detail | 5 | Unchanged |
| Bad `--quic-ca` / `--quic-known-key` value, `--quic-cert` without `--quic-key`, key-file permissions, `--quic` with proxy env | specific text | **1** `RERR_SYNTAX` | Usage or local-configuration errors; upstream uses 1 for the password-file checks (`authenticate.c:258-265`). Today the cert/key pairing errors exit 5 (`connect.rs:414-425`) |
| Daemon: identity or client CA cannot load, key permissions | `failed to load the QUIC server identity: ...` | 1 | Config error at start (`clientserver.c:1762-1764`) |
| Daemon: UDP bind failed | existing `bind_error` text | 10 | `socket.c:706-707` |

## 6. Implementation plan

Small PRs, the security fix first. Each maps to files and test cells.
"XFAIL rows" are the rows carried by the PR #8060 rule in
`tools/ci/known_failures.conf:235-252` (on that branch).

| # | PR | Gaps | Files | Tests and cells |
|---|----|------|-------|-----------------|
| 1 | `fix(quic): make --quic-ca verification strict` | G1, G11 | `trust.rs` (`resolve`, notice text, rustdoc at `:21-32`, `:330-349`, `:747-760`), `connect.rs:376-391`, `:427-441` (comments and error text), `docs/quic-transport.md` trust section | Invert `quic_nonmatching_ca_falls_through_to_accept_new` (`connect.rs:949-1011`) into a refusal test with exit 5 and no known-hosts write. Change `resolve_yields_accept_new_verifier_with_and_without_ca` (`trust.rs:1077-1107`) so the CA case yields a strict verifier. Add unit tests for wrong SAN and expired leaf against the CA. **Flips 6 XFAIL rows**: `quic-ca/untrusted-ca/{push,pull}`, `quic-ca/wrong-san/{push,pull}`, `quic-ca/expired/{push,pull}`; delete the rule. If #8060 has not landed, #8060 drops the rule instead |
| 2 | `fix(daemon): refuse to start when the QUIC identity cannot load` | G4, G7 | `accept_loop.rs` (fork the front between `:331` and `:345`, wait for its status), `quic_listener.rs` (`start_quic_front`, `run_quic_front`: load, report, then drop) | Daemon unit tests for missing cert file, key mismatch, unreadable key: exit 1 and the log text. Cell `daemon/key-mismatch` in `run_interop.sh` (PR #8060 `:12711-12736`) moves from its "handshake timeout" branch to its "refused to start" branch; tighten it to require a non-zero exit and the log text |
| 3 | `fix(quic): enforce key-file permissions` | G6 | `quic_listener.rs` (daemon, via `platform::secrets`), `trust.rs` `load_cert_chain_and_key` caller for the client | Unit tests: `0644` key refused on both sides, `0600` accepted. New cells `daemon/key-perms` (daemon exits 1) and `mtls/client-key-perms` (client exits 1) |
| 4 | `feat(quic): add --quic-known-key pin mode` | G8 | `network.rs` (flag, repeatable), `drive/config.rs`, `connect.rs` (`QuicDialParams`), `trust.rs` (pin verifier, `resolve` modes) | Unit tests for match, mismatch, and `ca+pin` with a CA-valid but unpinned leaf. Cells `pin/match/{push,pull}`, `pin/mismatch/{push,pull}`, `ca+pin/wrong-pin/push` |
| 5 | `fix(quic): refuse a non-CA certificate for a CA-verified authority` | G2 | `trust.rs` (`KnownHostsStore` `@ca` record, `AcceptNewVerifier`) | Unit test: CA-validated contact records `@ca`; a later self-signed leaf for that authority is refused. A new interop cell needs a daemon cert trusted by the client's system roots, which CI does not have; cover it with the unit test and say so in the PR |
| 6 | `fix(quic): harden the listener transport` | G3, G9, G10, G15 | `driver.rs:531-551` (Retry), `tuning.rs` (per-role stream limits), `mod.rs:467-472` (migration off) | Unit tests: server config has migration off and zero peer streams; early data off on both configs; a loopback test that a client-opened stream is refused. Existing positive cells must stay green (they exercise Retry on every connection) |
| 7 | `feat(daemon): add per-module require client cert` | G5 | `module_directives.rs` (directive), `quic_listener.rs:313-317` (fill `client_identity`), `mod.rs` (`QuicStream` peer certificate accessor), module-access check in the session code | Unit tests for the directive parser. Cells `mtls/require-cert-over-tcp` (TCP client to that module is refused, exit 5) and `mtls/require-cert-over-quic` (accepted) |
| 8 | `fix(quic): exit 10 for an unreachable daemon; honour --contimeout` | G13 | `connect.rs:476-497` (split socket errors from TLS refusals), `mod.rs` (`connect_with` deadline) | Update `quic_alpn_mismatch_maps_to_startclient` (`connect.rs:1063-1073`); add an unreachable-port test (exit 10) and a `--contimeout` test. `daemon/key-mismatch` on the old path would have seen 10; after PR 2 it no longer reaches the client |
| 9 | `docs: reconcile the QUIC policy with the shipped model` | G12, G14 | `docs/design/quic-transport-policy.md` (Decision A: no ephemeral daemon cert; Decision B: modes of section 5.1, cert-DER fingerprint; Decision E: mTLS shipped), `docs/quic-transport.md` | Doc-only |

Ordering: PR 1 is independent and should land first. PRs 2 and 3 touch the
same startup path and can land in either order. PRs 4 and 5 both touch
`trust.rs` but are otherwise independent; PR 5 waits for decision D2. PR 7
depends on PR 2 (the front must carry the certificate). PR 9 lands last.

## 7. Owner decisions

These change user-visible behaviour or reverse the task 134 design.

| # | Decision | Options | Recommendation |
|---|----------|---------|----------------|
| D1 | Make `--quic-ca` strict. Reverses task 134's composition of a given CA with the TOFU fallback (`trust.rs:21-32`, `connect.rs:949-959`) | (a) strict: a CA failure is fatal; (b) keep the fallback | **(a)**. An explicit trust anchor that is silently ignored is the worst failure mode; the policy already requires it. Ship as PR 1 |
| D2 | Default mode when no trust flag is given | (a) keep accept-new, add the sticky `@ca` marker; (b) system roots only, TOFU only on request (pre-task-134 behaviour); (c) leave as is | **(a)**. Keeps task 134's zero-config self-signed path and closes the downgrade of CA-anchored daemons. (b) is simpler but reverses task 134 outright. (c) leaves G2 open |
| D3 | Add `--quic-known-key`, and its fingerprint format | (a) cert-DER `SHA256:` token, repeatable; (b) SPKI hash as the policy says (`:131-133`) | **(a)**. One token everywhere (notice, known-hosts, flag), no migration of existing pins. Update the policy to match. Revisit SPKI only if key-preserving renewals become a real need |
| D4 | Daemon behaviour when the QUIC identity cannot load | (a) refuse to start, exit 1; (b) serve TCP only (today) | **(a)**. A configured listener that silently does not exist is a fail-quiet |
| D5 | Where the daemon reads the QUIC key | (a) in the front process, before `daemon chroot` and uid drop; (b) in the parent after the drop (today) | **(a)**. Keys can be root-only and outside the chroot; the parent never holds them. Changes where relative paths resolve for chrooted daemons, so note it in the changelog |
| D6 | Key-file permission enforcement | (a) enforce as upstream does for secrets and password files, exit 1; (b) warn only; (c) none | **(a)**, gated by `strict modes` on the daemon, matching upstream semantics |
| D7 | Client-auth model | (a) mTLS and `auth users` independent (AND), per-module `require client cert`, no cert-to-user mapping; (b) also map a certificate field to an `auth users` name; (c) no change | **(a)**. Closes the TCP bypass with one boolean. Mapping (b) adds a second credential system the policy rejected (`:302-307`); defer until an operator asks |
| D8 | TCP fallback | (a) never, and refuse `--quic` with proxy env vars; (b) opt-in fallback flag | **(a)**. A fallback flag is a scripted downgrade |
| D9 | Exit code for an unreachable QUIC daemon | (a) 10, as for TCP; (b) 5 (today) | **(a)**. Parity with `clientserver.c:163-165`; keeps 5 for refusals so scripts can tell "down" from "rejected" |
| D10 | Retry policy | (a) always Retry; (b) Retry only when the backlog is over half full | **(a)**. One code path, no tuning knob; the extra round trip is negligible for rsync sessions |
| D11 | Connection migration | (a) off; (b) on (today, by library default) | **(a)**. Matches the policy's deferral and TCP semantics |
| D12 | Reconcile the policy note | (a) update `quic-transport-policy.md` to the shipped model; (b) leave it as a historical record | **(a)**. The note is cited as the authority by code comments and by `known_failures.conf`, so it must be accurate |

## 8. Unverified items

These need a live run before being treated as facts:

- G3: that 128 spoofed Initials lock the listener for about 30 s. Derived from
  `driver.rs:29-35`, `:509-551` and quinn's default idle timeout; the
  handshake-phase timeout in quinn-proto 0.11.18 was not traced.
- G9: the per-connection memory a client can pin with unread streams.
- Whether `--timeout` bounds a stalled QUIC session (2.6).
- Whether freed key bytes remain readable in the parent heap inherited by
  session children (G7). PR 2 removes the question either way.
