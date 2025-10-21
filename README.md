# oc-rsync (Rust rsync implementation, protocol 32)

This workspace hosts a Rust rsync implementation supporting protocol version 32
under the **oc-rsync** and **oc-rsyncd** binaries. The long-term goal is
byte-for-byte parity with upstream behaviour while modernising the
implementation in Rust. The project follows the requirements outlined in the
repository's Codex Mission Brief and implements modules as cohesive crates so
both binaries reuse the same core logic.

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
- `crates/engine` — the transfer engine facade. The current
  [`local_copy`](crates/engine/src/local_copy.rs) module provides deterministic
  local filesystem copies for regular files, directories, symbolic links, and
  named pipes (FIFOs) while preserving permissions and timestamps.
- `crates/walk` — deterministic filesystem traversal that emits ordered file
  lists while enforcing relative-path safety and optional symlink following.
- `crates/cli` — the command-line front-end that exposes `--help`, `--version`,
  `--dry-run`, and local copy support (regular files, directories, symbolic
  links, and FIFOs) by delegating to `rsync_core::client`.

## Binaries

- `bin/oc-rsync` — thin wrapper that locks standard streams and invokes
  [`rsync_cli::run`](crates/cli/src/lib.rs) before converting the resulting exit
  status into `std::process::ExitCode`.
- `bin/oc-rsyncd` — daemon wrapper that binds the requested TCP socket,
  performs the legacy `@RSYNCD:` handshake, lists configured in-memory modules
  for `#list` requests, and reports that full module transfers are still under
  development.

Higher-level crates such as `daemon` remain under development. The new engine
module powers the local copy mode shipped by `oc-rsync`, but delta transfer,
remote transports, xattr/ACL handling, filters, and compression are
still pending. Current gaps and parity status are tracked in
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
