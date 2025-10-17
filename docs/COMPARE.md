# Parity Checkpoints vs Upstream rsync 3.4.1

This log captures the observable parity checkpoints that the current Rust
workspace satisfies today. Each entry is backed by automated tests living in
the repository so results remain reproducible. Items will expand as additional
layers (CLI, core, engine, daemon) land.

## Negotiation Detection & Replay

- **Binary vs. legacy sniffing** – `transport::sniff_negotiation_stream` mirrors
  upstream negotiation detection by replaying the sniffed bytes before handing
  control to higher layers. Unit tests exercise both branches and verify that
  the buffered prefix remains byte-identical regardless of the negotiation
  style.【F:crates/transport/src/negotiation.rs†L860-L921】
- **Legacy daemon greeting parsing** – `NegotiatedStream::read_and_parse_legacy_
  daemon_greeting` forwards the buffered greeting into the protocol parser and
  round-trips through `ProtocolVersion`, demonstrating compatibility with the
  ASCII `@RSYNCD:` flow.【F:crates/transport/src/negotiation.rs†L218-L235】【F:crates/transport/src/negotiation.rs†L1046-L1099】
- **Session orchestration** – `transport::session::negotiate_session` selects
  the correct handshake style, clamps the protocol, and exposes unified access
  to the replaying transport. Tests cover binary, legacy, shared sniffer reuse,
  and mapping the inner transport, confirming that the helper matches upstream
  orchestration semantics for the implemented scope.【F:crates/transport/src/session.rs†L1-L214】【F:crates/transport/src/session.rs†L220-L342】

## Checksums

- **Rolling checksum (`rsum`)** – The implementation reproduces upstream
  `sum1`/`sum2` semantics and exposes safe rolling updates. Property tests ensure
  equivalence with a reference recomputation, covering sliding-window updates
  and chunked ingestion patterns.【F:crates/checksums/src/rolling.rs†L1-L206】【F:crates/checksums/src/rolling.rs†L209-L312】
- **Strong digests** – Streaming wrappers over MD4, MD5, and XXH64 match known
  vectors from RFC test suites and the upstream xxhash reference. The tests
  validate multi-chunk updates plus one-shot helpers for each algorithm.【F:crates/checksums/src/strong/md4.rs†L1-L60】【F:crates/checksums/src/strong/md5.rs†L1-L58】【F:crates/checksums/src/strong/xxhash.rs†L1-L48】

## Outstanding Areas

The following parity milestones remain unmet and are tracked in
`docs/differences.md` and `docs/gaps.md`:

- CLI, core orchestration, and daemon server binaries.
- Delta-transfer engine, metadata/xattr/ACL handling, filter grammar, and
  compression layers.
- Golden parity harness, interop matrix, packaging artifacts, and coverage gate
  defined in the production scope.

These items will transition into the sections above once implemented and backed
by reproducible parity evidence.
