[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync)](https://github.com/oferchen/rsync/releases)

# oc-rsync - Classic rsync implementation in pure rust.


- **Parity goal:** CLI UX, negotiation, framing/tags, messages, and exit codes are designed to be **indistinguishable** from rsync@https://rsync.samba.org.
- **Safety & performance:** Memory-safe Rust, explicit error types, and hot-path IO attention.

---

## Table of Contents

- [Features](#features)
- [Status](#status)
- [Project Structure](#project-structure)
- [Quick Start](#quick-start)
- [Build From Source](#build-from-source)
- [Linting, Formatting, and Clippy](#linting-formatting-and-clippy)
- [Testing and Coverage](#testing-and-coverage)
- [Interop & Compliance](#interop--compliance)
- [XTask & Docs Validation](#xtask--docs-validation)
- [CI](#ci)
- [Release & Packaging](#release--packaging)
- [Configuration & Environment](#configuration--environment)
- [Logging](#logging)
- [Security Notes](#security-notes)
- [Design Highlights](#design-highlights)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgments](#acknowledgments)

---

## Features

- **Protocol 32**: Negotiation, tags, framing & multiplexing modeled after rsync@https://rsync.samba.org.
- **Functional parity**: Exit codes, progress/prose, and CLI help emulate upstream behavior.
- **Checksums**: Rolling + strong checksums with clear traits for future SIMD paths.
- **Composed crates**: Separation of protocol, engine, daemon, CLI, filters, checksums, bandwidth.
- **Strict hygiene**: Clippy `-D warnings`, rustfmt checks, and doc validation via `xtask`.

> Note: rsync@https://rsync.samba.org error lines reference C source files. Here, diagnostics map to **Rust module/function paths** with equivalent fidelity.

---

## Status

- **Target:** Full day-to-day parity with rsync@https://rsync.samba.org (protocol 32).
- **Interoperability:** Client and daemon modes are designed to interop with upstream; see [Interop & Compliance](#interop--compliance).
- **Rust edition:** **2024**
- **Toolchain/MSRV:** **1.88** (matches CI)

---

## Project Structure

```

bin/oc-rsync/           # Client entry (binary crate)
bin/oc-rsyncd/          # Daemon entry (binary crate)
crates/cli/             # CLI: flags/help/UX parity
crates/core/            # Core types, error model, shared utils
crates/protocol/        # Protocol v32: negotiation, tags, framing, IO
crates/engine/          # File-list & data pump pipelines
crates/daemon/          # Rsync-like daemon behaviors
crates/filters/         # Include/exclude pattern engine
crates/checksums/       # Rolling & strong checksum implementations
crates/bandwidth/       # Throttling/pacing primitives
tools/                  # Dev utilities (e.g., enforce_limits.sh)
xtask/                  # Developer tasks (e.g., doc validation)

````

> The workspace also includes additional internal crates/utilities as development evolves; the above are stable anchors for docs/CI.

---

## Quick Start

```bash
git clone https://github.com/oferchen/rsync.git
cd rsync

# Build (debug)
cargo build --workspace

# Build (release)
cargo build --workspace --release

# Version/help parity check
cargo run -p oc-rsync -- --version
cargo run -p oc-rsync -- --help
````

---

## Build From Source

```bash
# Ensure Rust 1.88 toolchain
rustup toolchain install 1.88.0
rustup default 1.88.0

# Workspace build
cargo build --workspace --all-features
```

Cross builds and extra packaging live in CI and optional developer flows (see [Release & Packaging](#release--packaging)).

---

## Linting, Formatting, and Clippy

```bash
# Format (check)
cargo fmt --all -- --check

# Clippy (deny warnings)
cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
```

These are enforced in CI.

---

## Testing and Coverage

```bash
# Unit + integration tests
cargo test --workspace --all-features

# Optional coverage (example with llvm-cov)
cargo llvm-cov clean
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

> Where enabled, CI enforces a coverage gate to prevent regressions.

---

## Interop & Compliance

* **Protocol v32:** Message/tag semantics mirror rsync@https://rsync.samba.org.
* **Client/daemon:** Designed to operate with upstream `rsync` daemons and clients.
* **Exit codes/messages:** Map to upstream conventions; differences are documented inline when safety/perf requires.
* **Smoke test example:**

  ```bash
  # Replace host/module with your upstream rsyncd endpoint
  cargo run -p oc-rsync -- -av rsync://host/module/ /tmp/sync-test
  ```

---

## XTask & Docs Validation

Documentation & hygiene are validated via `xtask`:

```bash
# Validate README and docs (anchors, headings, fenced blocks, link sets as configured)
cargo xtask doc-validate

# Source limits / policy checks
bash tools/enforce_limits.sh

# One-liner (fmt + clippy + tests + docs)
cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets --all-features --no-deps -D warnings \
  && cargo test --workspace --all-features \
  && cargo xtask doc-validate
```

This README intentionally uses **stable, simple headings** (e.g., `#xtask--docs-validation`) to keep validators deterministic.

---

## CI

Typical CI stages:

1. **Formatting** (`cargo fmt --check`)
2. **Clippy** (`-D warnings`)
3. **Tests** (workspace, all features)
4. **Doc validation** (`cargo xtask doc-validate`)
5. **(Optional)** Interop & packaging smoke tests

CI is configured with **fail-fast hygiene** to keep `main/master` green.

---

## Release & Packaging

```bash
# Standard release artifacts
cargo build --workspace --release

# Optional (if configured): Zig-based cross artifacts
# cargo zigbuild --release --target x86_64-unknown-linux-gnu
```

Check repository workflows and packaging directories when present for distro-specific outputs (e.g., RPM/DEB).

---

## Configuration & Environment

Defaults aim to **mirror rsync@https://rsync.samba.org** semantics. Flags/envs follow upstream names where feasible.

```bash
cargo run -p oc-rsync -- --help
cargo run -p oc-rsyncd -- --help
```

---

## Logging

* Structured logs with conventional levels: `error`, `warn`, `info`, `debug`, `trace`.
* End-user progress output follows rsync UX; diagnostics emphasize actionable context.

---

## Design Highlights

* **Layered crates** keep protocol, engine, and UX concerns isolated for testability.
* **Streaming IO** minimizes copies and bounds allocations.
* **Checksum traits** allow targeted SIMD acceleration behind stable interfaces.
* **Error handling** uses explicit enums with context—no panics in libraries.

---

## Contributing

1. Create a focused branch off `main/master`.
2. Keep PRs small and well-scoped.
3. Run the full hygiene suite:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
   cargo test --workspace --all-features
   cargo xtask doc-validate
   ```

---

## License

This project is available under the GPL-3.0-or-later license. See `LICENSE` for full terms.

---

## Acknowledgments

* The **rsync@https://rsync.samba.org** project and maintainers.
* The Rust community and ecosystem crates enabling safe, fast systems programming.

