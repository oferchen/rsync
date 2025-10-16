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
| Transport | Negotiation stream wrappers with prefix replay | Implemented | `NegotiatedStream` preserves the sniffed bytes, exposes `Read`/`BufRead`, and returns the underlying reader for continued use. | `crates/transport/src/negotiation.rs` |
| Workspace | CLI front-end (`bin/rsync`) | Missing | No CLI crate or binary exists yet; command-line parsing and help parity are outstanding. | _n/a_ |
| Workspace | Daemon server (`bin/rsyncd`) | Missing | Daemon crate, config parser, and transport loop have not been implemented. | _n/a_ |
| Workspace | Core transfer/engine/meta/filter/compress crates | Missing | No crates beyond `protocol` exist; delta transfer, metadata application, and compression remain to be written. | _n/a_ |
| Quality | Golden parity harness & interop tests | Missing | The repository does not yet build or execute the upstream rsync comparison matrix. | _n/a_ |
| Quality | Packaging (deb/rpm), SBOM, systemd unit | Missing | Packaging artifacts are absent pending higher-layer implementation. | _n/a_ |

Status legend: **Implemented** — behavior is present and backed by tests in this
repository. **Missing** — code has not been written yet; entries remain until the
corresponding crates/binaries land and parity is verified.
