[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync)](https://github.com/oferchen/rsync/releases)

# oc-rsync – Classic rsync implementation in pure Rust

Classic `rsync` re-implementation in **pure Rust**, targeting wire-compatible **protocol 32** and near drop-in CLI/daemon behavior, while taking advantage of Rust’s safety guarantees and modern tooling.

> Binary name: **`oc-rsync`** (client and daemon in one binary). System packages can keep the original `rsync` installed side-by-side.

---

## About

`oc-rsync` is in the final stages of stabilization: the core feature set is implemented and working, and the focus is now on polishing edge cases, and ergonomics.

At the moment, Linux CI builds are temporarily failing due to a `ubuntu-latest` GitHub Actions issue; once that is resolved, official Linux packages (Deb/RPM and tarballs) will be published on the Releases page.

---

## Table of Contents

- [Features](#features)
- [Status](#status)
- [Installation](#installation)
  - [Prebuilt packages](#prebuilt-packages)
  - [Build from source](#build-from-source)
- [Usage](#usage)
  - [Basic examples](#basic-examples)
  - [Daemon mode](#daemon-mode)
- [Project layout](#project-layout)
- [Development](#development)
  - [Linting and formatting](#linting-and-formatting)
  - [Testing and coverage](#testing-and-coverage)
  - [Interop & compliance harness](#interop--compliance-harness)
  - [XTask & docs validation](#xtask--docs-validation)
  - [Release & packaging](#release--packaging)
- [Configuration & environment](#configuration--environment)
- [Logging](#logging)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgements](#acknowledgements)

---

## Features

- **Protocol 32 parity**

  - Negotiation, tags, framing, and multiplexing modeled closely after upstream `rsync` (3.4.x).
  - Designed to interoperate with existing `rsync` daemons and clients (both directions).

- **CLI & UX parity**

  - Command-line surface modeled after `rsync`, including exit codes and user-facing messages.
  - `--version` and `--help` outputs are structured to closely match upstream while exposing Rust-specific details.

- **Rust safety & performance**

  - Memory-safe implementation using idiomatic Rust.
  - Hot path I/O and checksum operations are structured for future SIMD/vectorization work.

- **Composed workspace**

  - Multiple crates separate concerns:
    - `cli` for argument parsing and user experience.
    - `core` for shared types, error model, and utilities.
    - `protocol` for wire format, tags, and negotiation.
    - `engine` for file list and data pump pipelines.
    - `daemon` for rsync-style daemon behavior.
    - `filters`, `checksums`, `bandwidth` for independent subsystems.

- **Strict hygiene**

  - `cargo fmt` and `cargo clippy` enforced in CI with `-D warnings`.
  - Documentation and README validation via `xtask` to keep public docs in sync with code.

---

## Installation

### Prebuilt packages

Prebuilt artifacts are (or will be) published on the GitHub **Releases** page (Deb/RPM packages and tarballs, across multiple platforms/architectures).

1. Go to: <https://github.com/oferchen/rsync/releases>
2. Download the asset for your OS/arch (e.g., `.deb`, `.rpm`, or `.tar.*`).
3. Install it using your platform’s package manager or by extracting the tarball.

The packaging pipeline installs `oc-rsync` under dedicated paths so that the system `rsync` can remain installed in parallel.

### Build from source

Requirements:

- Rust toolchain **1.88** (or newer compatible with the workspace).

Clone and build:

```bash
git clone https://github.com/oferchen/rsync.git
cd rsync

# Debug build
cargo build --workspace

# Optimized build
cargo build --workspace --release
````

To match the documented toolchain:

```bash
rustup toolchain install 1.88.0
rustup default 1.88.0
cargo build --workspace --all-features
```

The primary entrypoint binary is `oc-rsync` (client and daemon).

---

## Usage

`oc-rsync` is designed to behave like `rsync`, so existing `rsync` muscle memory should mostly apply.

### Basic examples

Local directory sync:

```bash
oc-rsync -av ./source/ ./dest/
```

Remote pull:

```bash
oc-rsync -av user@host:/remote/path/ ./local/path/
```

Remote push:

```bash
oc-rsync -av ./local/path/ user@host:/remote/path/
```

Stats, progress, and other flags follow upstream semantics wherever possible.

### Daemon mode

Run as a daemon:

```bash
oc-rsync --daemon --config=/etc/oc-rsyncd/oc-rsyncd.conf
```

Default daemon configuration example path:

```text
/etc/oc-rsyncd/oc-rsyncd.conf
```

The daemon mode is intended to interoperate with upstream `rsync` clients and daemons.

---

## Project layout

High-level workspace structure:

```text
bin/oc-rsync/           # Client + daemon entry (binary crate)
crates/cli/             # CLI: flags, help, UX parity
crates/core/            # Core types, error model, shared utilities
crates/protocol/        # Protocol v32: negotiation, tags, framing, IO
crates/engine/          # File list & data pump pipelines
crates/daemon/          # Rsync-like daemon behaviors
crates/filters/         # Include/exclude pattern engine
crates/checksums/       # Rolling & strong checksum implementations
crates/bandwidth/       # Throttling/pacing primitives
docs/                   # Design notes, internal docs
tools/                  # Dev utilities (e.g., enforce_limits.sh)
xtask/                  # Developer tasks: docs validation, packaging helpers
AGENTS.md               # Internal agent roles & conventions
```

---

## Development

### Linting and formatting

```bash
# Format (check mode)
cargo fmt --all -- --check

# Clippy (deny warnings)
cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
```

These are enforced in CI to keep the codebase consistent and warning-free.

### Testing and coverage

```bash
# Unit + integration tests
cargo nextest --workspace --all-features
```

Example coverage flow (LLVM based):

```bash
cargo llvm-cov clean
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

Where enabled, CI may enforce coverage gates to avoid regressions.

### XTask & docs validation

Documentation and policy checks are wired via `xtask`:

```bash
# Validate README and docs (anchors, headings, fenced blocks, link sets as configured)
cargo xtask doc-validate

# Source/policy limits
bash tools/enforce_limits.sh

# One-liner: fmt + clippy + tests + docs
cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets --all-features --no-deps -D warnings \
  && cargo nextest --workspace --all-features \
  && cargo xtask doc-validate
```

This keeps public docs and internal invariants in sync with the codebase.

### Release & packaging

Release builds use a dedicated `dist` profile and `xtask` packaging helpers:

```bash
# Build optimized binaries (dist profile)
cargo build --workspace --profile dist --locked

# Produce Debian, RPM, and tarball artifacts for the host platform
cargo xtask package --release

# Only tarballs (e.g., for macOS cross-build hosts)
cargo xtask package --release --tarball

# Restrict tarball generation to a specific target triple
cargo xtask package --release --tarball --tarball-target x86_64-apple-darwin
```

CI publishes artifacts across:

* Linux (`x86_64` / `aarch64`) `.deb` / `.rpm`
* macOS and Windows tarballs (for supported targets)
* CycloneDX SBOM built from the same `dist` binaries.

---

## Configuration & environment

Defaults aim to mirror upstream `rsync` semantics wherever possible. Flags and environment variables follow upstream names when feasible.

For a full overview of supported options:

```bash
# Client options
oc-rsync --help

# Daemon options
oc-rsync --daemon --help
```

---

## Logging

* Structured logs with standard levels: `error`, `warn`, `info`, `debug`, `trace`.
* Human-oriented progress output follows `rsync` UX conventions.
* Diagnostics reference Rust module/function paths instead of C file/line pairs while preserving the same level of precision.

---

## Contributing

Contributions, bug reports, and interop findings are very welcome.

1. Fork the repository and create a feature branch.

2. Run the full hygiene pipeline:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
   cargo nextest --workspace --all-features
   cargo xtask doc-validate
   ```

3. Open a pull request with a clear description of:

   * Behavioral changes (especially vs upstream `rsync`)
   * New flags or configuration knobs
   * Any interop impact or protocol changes

Please keep changes focused and align with the existing crate split and error handling patterns.

---

## License

This project is licensed under the **GNU GPL v3.0 or later**.
See [`LICENSE`](./LICENSE) for the full text.

---

## Acknowledgements

Inspired by the original [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team, and by the broader Rust ecosystem that made this re-implementation feasible.
```
