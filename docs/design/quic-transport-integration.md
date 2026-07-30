# QUIC Transport: Integration Design (CLI, bootstrap, exit codes)

Tracking tasks: QUIC-5 .. QUIC-11. Status: design note awaiting owner review.
No code is introduced by this note. It sequences the wiring that connects the
already-shipped transport primitive to the CLI, the daemon listener, and the
unmodified Protocol 32 handshake, resolving the last open build details against
the decisions already fixed in:

- `quic-transport-concurrency-model.md` (sans-IO driver; `QuicStream` as
  blocking `Read`/`Write`; `quinn`/tokio dropped from the `quic` feature).
- `quic-transport-policy.md` (server identity, client verification, ALPN,
  scheme/flag, out-of-scope).

## 0. Current state (build vs wire)

Built and tested (`crates/rsync_io/src/quic`, `tests/quic_loopback.rs`), behind
the off-by-default `quic` cargo feature:

- `QuicAcceptor::bind` / `accept` - UDP listener, self-signed cert (rcgen),
  TLS 1.3, ALPN `rsync`; blocks for one bidirectional stream.
- `QuicConnector::new(cert)` / `connect(addr, server_name)` - pins a server
  certificate; returns one bidirectional stream.
- `QuicStream` - one bidi stream as **blocking `std::io::Read` + `Write`**
  with `finish()` / `close()` teardown barriers.

Not yet wired (this note's scope): CLI `--quic` / `quic://`, the daemon
`quic cert file` / `quic key file` directives and ephemeral default, client
trust (`--quic-ca`, TOFU `quic_known_hosts`), the bootstrap that hands a
`QuicStream` to the `@RSYNCD:` handshake, and the quinn-proto error ->
exit-code mapping. `QuicConnector`/`QuicAcceptor` are referenced only from the
crate's own loopback test today.

## 1. Premise corrections (why some of the "obvious" plan does not apply)

- **Not `quinn`; `quinn-proto` sans-IO.** The concurrency-model note already
  chose the sans-IO state machine driven from one dedicated I/O thread over a
  std `UdpSocket`. There is no tokio runtime in the `quic` feature and none is
  to be added. "Integrate the `quinn` crate" is superseded.
- **No `AsyncRead`/`AsyncWrite` layer.** The Protocol 32 codecs
  (`crates/protocol/src/wire/**`) are synchronous `Read`/`Write`. `QuicStream`
  is already blocking `Read`/`Write`, so it is a 1:1 drop-in for `TcpStream`
  with zero adapter. The `AsyncRead/AsyncWrite` wrapper in the original plan is
  unnecessary and would re-introduce the tokio dependency the design removed.
- **io_uring is not in the path.** io_uring is measured-harmful here and is
  default-off; the UDP driver owns its own std socket on its own thread. There
  is no disk ring to bridge or contend with (original Phase 4 is moot).
- **GSO/GRO deferred.** The driver uses a portable std `UdpSocket`
  (`allow_mtud = false`, since a std socket cannot set don't-fragment). GSO/GRO
  are a throughput follow-up (revisit trigger: a measured LAN/WAN bulk-transfer
  regression versus TCP), not part of first integration.

## 2. Phase mapping

### Phase 1 - CLI and URI (QUIC-8)

- Add the `quic://[user@]host[:port]/module[/path]` scheme parsed exactly
  beside `rsync://` in the CLI target parser, and a `--quic` modifier that
  upgrades an `rsync://` / `host::module` daemon target to QUIC. Both resolve
  to the same internal "daemon-over-QUIC" transport selection.
- Default port `873/udp` (policy D). `--port` overrides.
- **Hard failure, never silent TCP fallback** (policy D): a `quic://` target or
  `--quic` that cannot establish a QUIC connection exits non-zero, it does not
  retry over TCP. Absent `--quic`/`quic://`, behaviour is byte-for-byte the
  current TCP/SSH path (backward compatibility preserved; the `quic` feature
  compiled out removes the flag entirely).
- Files: `crates/cli` target/URI parsing and the daemon-target config that
  currently records `rsync://` host/port/module; add a transport enum
  (`Tcp | Quic`) rather than a bool, so the value threads cleanly to core.

### Phase 2 - Transport trait (QUIC-1, DONE)

Satisfied by `QuicStream`'s blocking `Read`/`Write`. No new trait work; the
integration consumes the existing type. The one addition is a thin transport
selector in core so the connect path yields `Box<dyn Read+Write>` (or an enum)
that is either a `TcpStream` or a `QuicStream`.

### Phase 3 - Bootstrap (QUIC-5 client, QUIC-6 daemon)

Client connect path (core): resolve trust, then
`QuicConnector::connect(addr, server_name)` -> `QuicStream` -> hand the stream
to the **unchanged** `@RSYNCD: 32.0` greeting + module negotiation + auth. The
handshake code does not learn it is on QUIC.

- Trust resolution (policy B), in order: `--quic-ca <pem>` (private CA) ->
  system roots -> TOFU `quic_known_hosts` (first contact pins the cert
  fingerprint keyed by host:port, mirroring SSH `known_hosts`; a changed
  fingerprint aborts loudly). The current `QuicConnector::new(cert)` explicit
  pin becomes the concrete backend these paths configure (generalise `new` to
  accept a verifier/roots source; keep the pin form for tests).

Daemon listener (daemon): build a `QuicAcceptor` from `quic cert file` /
`quic key file` (or the ephemeral in-memory self-signed default, policy A),
`accept()`
-> `QuicStream` -> existing daemon session (`@RSYNCD:` greeting, module select,
auth, transfer) with no protocol changes. The QUIC listener runs alongside the
TCP listener; enabling it is a config/opt-in, not a replacement.

### Phase 4 - Performance (deferred)

io_uring bridge: not applicable (see 1). GSO/GRO: deferred follow-up. First
integration targets correctness and the QUIC-7 differential oracle, not peak
throughput.

## 3. Error / exit-code mapping (QUIC-9)

Map at the transport boundary into the existing `ExitCode` schema so callers
above are unchanged:

| Source | Condition | ExitCode (rsync RERR) |
|--------|-----------|-----------------------|
| ALPN mismatch / TLS handshake refusal | peer is not an oc QUIC endpoint | 5 (protocol-incompat, policy C) |
| `quinn_proto::ConnectionError::TimedOut` / handshake timeout | idle/keepalive/connect timeout | 35 `RERR_CONTIMEOUT` |
| connect/UDP bind/route failure | cannot reach endpoint | 10 `RERR_SOCKETIO` |
| stream reset / `WriteError` / mid-transfer connection loss | reliable stream broke after handshake | 12 `RERR_STREAMIO` |
| clean peer close with unfinished protocol state | greeting/negotiation aborted | 2 `RERR_PROTOCOL` |

`QuicStream`'s `Read`/`Write` already surface driver terminal states as
`io::Error`; the mapping is a small classifier in core's connect+drive path,
tested against forced failures (bad cert, unreachable port, mid-stream kill).

## 4. Invariants and test gates

- **Wire untouched.** `crates/protocol/src/wire/**` is not modified; QUIC is a
  reliable byte pipe below the codec. QUIC-7 differential oracle: a QUIC daemon
  session transcript must be byte-identical to the TCP daemon session for the
  same inputs (extend the interop harness with a QUIC cell).
- **Feature-gated, default-off.** All wiring compiles only under `--features
  quic`; default builds and the upstream-parity surface are unchanged. This is
  the sole sanctioned TLS site (amends the NO-TLS policy narrowly).
- **No new copies.** The connect path yields the `QuicStream` directly to the
  existing buffered reader/writer; no staging buffer between the driver and the
  token parser beyond the driver's existing condvar-guarded buffer pair.

## 5. Implementation sequence

1. QUIC-8: CLI `quic://` + `--quic`, transport enum threaded to core (no
   connect yet; parse + config only, unit-tested).
2. QUIC-5: client trust resolution + `QuicConnector` connect -> handshake;
   `--quic-ca`, then TOFU `quic_known_hosts`.
3. QUIC-6: daemon `quic cert file`/`key file` directives + ephemeral default +
   `QuicAcceptor` listener -> daemon session.
4. QUIC-9: error -> exit-code classifier + failure tests.
5. QUIC-7: differential-oracle interop cell (QUIC transcript == TCP transcript).
6. QUIC-10/11: docs (Tier-2 note that TLS exists only under `quic`), packaging.
