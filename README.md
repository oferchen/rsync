# rsync (Rust reimplementation)

This workspace hosts a from-scratch Rust port of rsync 3.4.1 (protocol 32)
with the long-term goal of matching upstream behaviour byte-for-byte. The
project follows the requirements outlined in the repository's Codex Mission
Brief and implements modules as cohesive crates so both the future CLI and
rsync daemon reuse the same core logic.

## Repository layout

The workspace currently contains the following published crates:

- `crates/protocol` — protocol version negotiation helpers, legacy `@RSYNCD:`
  parsing, multiplexed message envelopes, and stream-sniffing utilities.
- `crates/transport` — transport-level negotiation wrappers that preserve the
  sniffed handshake bytes and expose helpers for replaying legacy daemon
  greetings and control messages.
- `crates/checksums` — the rolling rsync checksum (`rsum`) together with
  streaming MD4/MD5/XXH64 digests used for strong block verification.
- `crates/core` — shared infrastructure such as the centralized message
  formatting utilities that attach role trailers and normalized source
  locations to user-facing diagnostics.
- `crates/logging` — newline-aware message sinks that reuse
  `MessageScratch` buffers when streaming diagnostics into arbitrary
  writers, mirroring upstream `rsync`'s logging pipeline.
- `crates/cli` — the command-line front-end that exposes `--help`, `--version`,
  and local copy support by delegating to `rsync_core::client`.

Higher-level crates such as `daemon`, `engine`, and `meta` have not been
implemented yet. The `core` crate currently ships a deterministic local copy
helper that mirrors `rsync SOURCE DEST` for regular files and directories, but
delta compression, metadata preservation, filters, and remote transports remain
to be written. Current gaps and parity status are tracked in
`docs/differences.md` and `docs/gaps.md`.

## Getting started

The workspace targets Rust 2024 and denies unsafe code across all crates. To
run the existing unit and property tests:

```bash
cargo test
```

The top-level documents provide additional context:

- `docs/production_scope_p1.md` freezes the scope that must be green before the
  project is considered production ready.
- `docs/feature_matrix.md` summarises implemented features and the remaining
  work items.
- `docs/differences.md` and `docs/gaps.md` enumerate observable gaps versus
  upstream rsync 3.4.1.

## License

This project is licensed under the terms of the GPL-3.0-or-later. See
[`LICENSE`](LICENSE) for details.
