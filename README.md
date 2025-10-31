# rsync (Rust rsync implementation, protocol 32)

This workspace hosts a Rust rsync implementation that targets upstream rsync
**3.4.1** (protocol version **32**) under the branded version string
**3.4.1-rust**. The canonical distribution ships the **oc-rsync** and
**oc-rsyncd** binaries, which wrap the shared CLI/daemon front-ends provided by
this repository. The command-line interface still inspects the invoked program
name so operators can provide compatibility symlinks (`rsync` / `rsyncd`) when
required, but the workspace no longer maintains separate wrapper crates. The
long-term goal is byte-for-byte parity with upstream behaviour while
modernising the implementation in Rust. The project follows the requirements
outlined in the repository's Codex Mission Brief and implements modules as
cohesive crates so both binaries reuse the same core logic.

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
  local filesystem copies for regular files, directory trees, symbolic links,
  hard links, character/block devices, and named pipes (FIFOs) while preserving
  permissions, timestamps, sparse extents, optional ownership metadata, and (when
  compiled in) extended attributes and POSIX ACLs.
- `crates/walk` — deterministic filesystem traversal that emits ordered file
  lists while enforcing relative-path safety and optional symlink following.
- `crates/cli` — the command-line front-end that exposes `--help`, `--version`,
  `--dry-run`, and local copy support (regular files, directories, symbolic
  links, and FIFOs) by delegating to `rsync_core::client`.

## Binaries

- `src/bin/oc-rsync.rs` — canonical client wrapper that locks standard streams
  and invokes [`rsync_cli::run`](crates/cli/src/lib.rs) before converting the
  resulting status into `std::process::ExitCode`. Supplying `--daemon` delegates
  argument parsing to the daemon front-end so `oc-rsync --daemon ...` behaves
  like invoking the dedicated daemon binary. Local copies honour the full
  metadata matrix (`--owner`, `--group`, `--perms`, `--times`, sparse files,
  hard links, devices, FIFOs, and optional ACL/xattr preservation) together with
  deletion, partial transfer, reference directory, bandwidth, progress, and
  stats flags.
- `src/bin/oc-rsyncd.rs` — canonical daemon wrapper that binds the requested TCP
  socket, performs the legacy `@RSYNCD:` handshake, lists configured in-memory
  modules for `#list` requests, and reports that full module transfers are still
  under development.

## Branding and configuration defaults

The project ships branded binaries (`oc-rsync` and `oc-rsyncd`). Branding
details—including the program names rendered in `--version` output and the
filesystem locations for packaged configuration—are centralised in
[`rsync_core::branding`](crates/core/src/branding/mod.rs). Consumers can query the
module directly to discover the canonical installation paths:

```rust
use std::path::Path;

let config_dir = rsync_core::branding::oc_daemon_config_dir();
let config = rsync_core::branding::oc_daemon_config_path();
let secrets = rsync_core::branding::oc_daemon_secrets_path();

assert_eq!(config_dir, Path::new("/etc/oc-rsyncd"));
assert_eq!(config, Path::new("/etc/oc-rsyncd/oc-rsyncd.conf"));
assert_eq!(secrets, Path::new("/etc/oc-rsyncd/oc-rsyncd.secrets"));
```

For callers that need a complete snapshot, the
[`rsync_core::branding::manifest()`](crates/core/src/branding/manifest.rs)
helper caches the branded and upstream profiles together with the workspace
version metadata:

```rust
let manifest = rsync_core::branding::manifest();

assert_eq!(manifest.oc().daemon_program_name(), "oc-rsyncd");
assert_eq!(manifest.upstream().daemon_program_name(), "rsyncd");
assert_eq!(manifest.rust_version(), "3.4.1-rust");
assert_eq!(manifest.source_url(), "https://github.com/oferchen/rsync");
```

The packaging metadata installs example files at the same locations so new
deployments pick up sane defaults out of the box. The binaries rely on the
shared branding facade, ensuring help text, diagnostics, and configuration
searches remain consistent across entry points regardless of the executable
name that launched the process.

Automation can serialise the same snapshot without reimplementing parsing
logic by calling [`rsync_core::branding::manifest_json`] or the pretty-printed
variant [`rsync_core::branding::manifest_json_pretty`]:

```rust
let json = rsync_core::branding::manifest_json_pretty();
assert!(json.contains("\"rust_version\": \"3.4.1-rust\""));
```

Both helpers cache their output for the lifetime of the process, keeping
command-line tooling lightweight while guaranteeing that downstream consumers
observe the same metadata as the binaries themselves.

Workspace automation consumes the same identifiers via the
`[workspace.metadata.oc_rsync]` section in the top-level `Cargo.toml`. The
metadata records the canonical program names, configuration locations, source
URL, and supported protocol version so packaging tasks and CI workflows can
validate branding before producing release artifacts. CI now invokes
`cargo xtask branding` to surface the metadata without relying on external
tools. Pass `--json` to emit the same information in machine-readable form,
allowing release automation to consume the data directly. Local operators can
run the command in either mode to confirm that binaries, documentation, and
packaging assets share a consistent brand profile.

Higher-level crates such as `daemon` remain under development. The new engine
module powers the local copy mode shipped by `oc-rsync` (and therefore any
compatibility symlink that targets it), but delta transfer, remote transports,
ACL handling, advanced filter grammar, and compression are still pending. Current
gaps and parity status are tracked in `docs/differences.md` and `docs/gaps.md`.

## Getting started

The workspace targets Rust 2024, requires `rustc` 1.87 or newer, and denies
unsafe code across all crates. To run the existing unit and property tests:

```bash
cargo test
```

Rebuild the API documentation and run doctests via the workspace helper:

```bash
cargo run -p xtask -- docs
```

Run the aggregated release-readiness checks before cutting a build:

```bash
cargo run -p xtask -- release
```

Generate a CycloneDX SBOM for packaging via the workspace helper:

```bash
cargo run -p xtask -- sbom
```

Collect code coverage reports (enforced at ≥95% line coverage in CI) by first
installing the `cargo-llvm-cov` subcommand and the LLVM tooling component. The
binary name is **`cargo-llvm-cov`**, not `llvm-cov`:

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
cargo llvm-cov --workspace --lcov --output-path coverage.lcov --fail-under-lines 95
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
