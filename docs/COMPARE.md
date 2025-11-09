# Parity Checkpoints vs Upstream rsync 3.4.1

This log captures the observable parity checkpoints that the current Rust
workspace satisfies today. Each entry is backed by automated tests living in
the repository so results remain reproducible. Items will expand as additional
layers (CLI, core, engine, daemon) land. The checkpoints were generated using
the branded **oc-rsync 3.4.1-rust** entrypoint (invoked as `oc-rsync --daemon`)
configured via `/etc/oc-rsyncd/oc-rsyncd.conf`, matching the packaging
layout enforced by the workspace metadata.

## Negotiation Detection & Replay

- **Binary vs. legacy sniffing** – `transport::sniff_negotiation_stream` mirrors
  upstream negotiation detection by replaying the sniffed bytes before handing
  control to higher layers. Unit tests exercise both branches and verify that
  the buffered prefix remains byte-identical regardless of the negotiation
  style.【F:crates/transport/src/negotiation.rs†L1131-L1165】【F:crates/transport/src/negotiation.rs†L1540-L1587】
- **Legacy daemon greeting parsing** – `NegotiatedStream::read_and_parse_legacy_
  daemon_greeting` forwards the buffered greeting into the protocol parser and
  round-trips through `ProtocolVersion`, demonstrating compatibility with the
  ASCII `@RSYNCD:` flow.【F:crates/transport/src/negotiation.rs†L342-L405】【F:crates/transport/src/negotiation.rs†L2388-L2412】
- **Session orchestration** – `transport::session::negotiate_session` selects
  the correct handshake style, clamps the protocol, and exposes unified access
  to the replaying transport. Tests cover binary, legacy, shared sniffer reuse,
  and mapping the inner transport, confirming that the helper matches upstream
  orchestration semantics for the implemented scope.【F:crates/transport/src/session/handshake.rs†L1-L237】【F:crates/transport/src/session/tests.rs†L1-L342】

## Checksums

- **Rolling checksum (`rsum`)** – The implementation reproduces upstream
  `sum1`/`sum2` semantics and exposes safe rolling updates. Property tests ensure
  equivalence with a reference recomputation, covering sliding-window updates
  and chunked ingestion patterns.【F:crates/checksums/src/rolling.rs†L1-L206】【F:crates/checksums/src/rolling.rs†L209-L312】
- **Strong digests** – Streaming wrappers over MD4, MD5, and XXH64 match known
  vectors from RFC test suites and the upstream xxhash reference. The tests
  validate multi-chunk updates plus one-shot helpers for each algorithm.【F:crates/checksums/src/strong/md4.rs†L1-L60】【F:crates/checksums/src/strong/md5.rs†L1-L58】【F:crates/checksums/src/strong/xxhash.rs†L1-L48】

## User-Facing Diagnostics

- **Diagnostic formatter** – The `core::message::Message` facade reconstructs
  upstream `rsync error:`/`rsync warning:` output, normalises Rust
  `file!()`/`line!()` metadata to repo-relative paths, and appends role trailers
  with the `3.4.1-rust` suffix. The byte-oriented renderer streams the message
  through vectored writes when available and falls back to sequential copies if
  a writer reports a partial write. Unit tests cover the path normalisation,
  newline handling, and vectored-write fallback to guarantee parity for the
  current diagnostic surface.【F:crates/core/src/message.rs†L99-L209】【F:crates/core/src/message.rs†L569-L708】【F:crates/core/src/message.rs†L940-L1261】
- **Streaming sinks** – `rsync_logging::MessageSink` wraps arbitrary writers,
  reuses `MessageScratch` buffers, and exposes newline controls that mirror
  upstream logging behaviour. Batch helpers keep newline policy stable while
  emitting progress updates or mixed severities, with tests exercising
  owned/borrowed message batches and explicit mode overrides.【F:crates/logging/src/lib.rs†L163-L220】【F:crates/logging/src/lib.rs†L994-L1078】

## Outstanding Areas

The following parity milestones remain unmet and are tracked in
`docs/differences.md` and `docs/gaps.md`:

- CLI, core orchestration, and daemon server binaries.
- Delta-transfer engine integration with transports, expanded filter grammar,
  and end-to-end compression for networked transfers.
- Golden parity harness, exit-code oracle, SSH transport coverage, and
  automated installation verification. CI now produces multi-platform
  packages/tarballs and exercises upstream interoperability, but these
  remaining gates still require manual review.

These items will transition into the sections above once implemented and backed
by reproducible parity evidence.
