# Feature Matrix — Rust rsync vs Upstream 3.4.1

The table below enumerates the major capability areas described in the
`Codex Mission Brief` and records the current implementation status in this
repository. Every entry is backed by code that exists today; missing
functionality explicitly calls out the absence of the relevant crate or
binary so documentation never overstates parity.

| Area | Feature | Status | Notes | Source |
|------|---------|--------|-------|--------|
| Protocol | Protocol version constants, selection helpers, and iteration utilities | Implemented | `ProtocolVersion` exposes `SUPPORTED_PROTOCOLS`, range helpers, and mutual selection logic used for negotiation parity. | `crates/protocol/src/version.rs` |
| Protocol | Legacy ASCII daemon greeting parsing (`@RSYNCD:`) | Implemented | Structured parsers cover banners, authentication prompts, and error/warning lines with exhaustive tests. | `crates/protocol/src/legacy/` |
| Protocol | Multiplexed message envelope (MSG_* tags, vectored writes) | Implemented | Envelope encoding/decoding mirrors upstream layouts and is fuzz/property tested. | `crates/protocol/src/envelope.rs`, `crates/protocol/src/multiplex.rs` |
| Protocol | Negotiation prologue sniffing (legacy vs binary) | Implemented | `NegotiationPrologueDetector` and sniffer utilities reconstruct buffered prefixes for replay. | `crates/protocol/src/negotiation/` |
| Transport | Negotiation stream wrappers with prefix replay | Implemented | `NegotiatedStream` preserves the sniffed bytes, exposes `Read`/`BufRead`, returns the underlying reader for continued use, and provides helpers to parse legacy daemon messages/errors/warnings. | `crates/transport/src/negotiation.rs` |
| Transport | Legacy daemon handshake orchestration | Implemented | `daemon::negotiate_legacy_daemon_session` reads the ASCII greeting, selects the mutual protocol, emits the client banner, and returns the replaying stream together with the parsed metadata. | `crates/transport/src/daemon.rs` |
| Checksums | Rolling checksum (`rsum`) implementation | Implemented | Streaming `RollingChecksum` mirrors upstream `sum1`/`sum2` semantics and exposes safe rolling updates. | `crates/checksums/src/rolling.rs` |
| Checksums | Strong digests (MD4/MD5/XXH64) | Implemented | Streaming wrappers over RustCrypto hashes and `xxhash-rust` provide the strong checksum variants negotiated by rsync. | `crates/checksums/src/strong/` |
| Workspace | CLI front-end (`bin/rsync`) | Missing | No CLI crate or binary exists yet; command-line parsing and help parity are outstanding. | _n/a_ |
| Transport | Binary negotiation orchestration | Missing | Remote-shell style binary negotiation remains unimplemented; higher layers do not yet drive the multiplexed handshake after the ASCII greeting. | _n/a_ |
| Workspace | Daemon server (`bin/rsyncd`) | Missing | Daemon crate, config parser, and transport loop have not been implemented. | _n/a_ |
| Workspace | Core transfer/engine/meta/filter/compress crates | Missing | Crates beyond `protocol`, `transport`, and `checksums` remain absent; delta transfer, metadata application, and compression still need to be written. | _n/a_ |
| Quality | Golden parity harness & interop tests | Missing | The repository does not yet build or execute the upstream rsync comparison matrix. | _n/a_ |
| Quality | Packaging (deb/rpm), SBOM, systemd unit | Missing | Packaging artifacts are absent pending higher-layer implementation. | _n/a_ |

Status legend: **Implemented** — behavior is present and backed by tests in this
repository. **Missing** — code has not been written yet; entries remain until the
corresponding crates/binaries land and parity is verified.
