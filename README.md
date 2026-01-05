[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync – Classic rsync implementation in pure Rust

Classic `rsync` re-implementation in **pure Rust**, targeting wire-compatible **protocol 32** and near drop-in CLI/daemon behavior, while taking advantage of Rust’s safety guarantees and modern tooling.

> Binary name: **`oc-rsync`** (client and daemon in one binary). System packages can keep the original `rsync` installed side-by-side.

---

## Table of Contents

- [Features](#features)
- [Status](#status)
  - [Implementation Status](#implementation-status)
- [Installation](#installation)
  - [Prebuilt packages](#prebuilt-packages)
  - [Homebrew 🍺](#homebrew-)
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
- [Feature Flags](#feature-flags)
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

- **Single native binary**

  - Client, server (`--server`), and daemon (`--daemon`) roles are implemented in Rust within the `oc-rsync` binary.
  - No delegation to a system `rsync` binary is required or supported for normal operation.

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

## Status

- **Upstream baseline:** tracking `rsync` **3.4.1** (protocol 32).
- **oc-rsync release:** **0.5.1**.
- **Stability:** beta; core transfer functionality is complete with full protocol interoperability. Ongoing work focuses on edge cases, performance optimization, and production hardening.

### Implementation Status

**Daemon Interoperability**: ✅ **Full protocol 28-32 compatibility with upstream rsync clients**

The daemon mode (`oc-rsync --daemon`) is fully interoperable with upstream rsync clients across all supported protocol versions:

- ✅ **Protocol 28** - rsync 3.0.x clients
- ✅ **Protocol 29** - rsync 3.1.x clients
- ✅ **Protocol 30** - rsync 3.2.x clients
- ✅ **Protocol 31** - rsync 3.3.x clients
- ✅ **Protocol 32** - rsync 3.4.x clients

**Server Delta Transfer**: ✅ **Complete with metadata preservation**

The native Rust server (`--server` mode) fully implements rsync's delta transfer algorithm:

- ✅ **Signature generation** - Receiver generates rolling and strong checksums from basis files
- ✅ **Delta generation** - Generator creates efficient delta operations (copy references + literals)
- ✅ **Delta application** - Receiver reconstructs files from deltas with atomic operations
- ✅ **Metadata preservation** - Permissions, timestamps, and ownership with nanosecond precision
- ✅ **Wire protocol integration** - Full protocol 32 compatibility
- ✅ **SIMD acceleration** - AVX2/NEON for rolling checksums on x86_64/aarch64
- ✅ **Error handling** - RAII cleanup, error categorization, ENOSPC detection

**Test Coverage**:
- 8,300+ tests passing (100% pass rate)
- Comprehensive integration tests for delta transfer
- Error scenario tests (cleanup, categorization, edge cases)
- Protocol version interoperability validated (protocols 28-32)
- Content integrity and metadata preservation validated
- Edge cases covered (empty files, large files, size mismatches, binary data)

**Production Readiness**:
- ✅ Core delta transfer: Production-ready
- ✅ Protocol interoperability: Tested with protocols 28-32
- ✅ Metadata preservation: Complete and tested
- ✅ End-to-end validation: Comprehensive integration tests
- ✅ Error handling: Complete with categorization and cleanup

For detailed implementation documentation:
```bash
# Generate and browse the documentation
cargo doc --workspace --no-deps --open
```

**Key documentation modules**:
- `transfer` - Server-side transfer implementation (generator, receiver, delta transfer)
- `protocol` - Wire protocol, negotiation, file list encoding
- `checksums` - Rolling and strong checksum implementations
- `daemon` - Rsync daemon mode and module configuration

---

## Installation

### Prebuilt packages

Prebuilt artifacts are (or will be) published on the GitHub **Releases** page (Deb/RPM packages and tarballs, across multiple platforms/architectures).

1. Go to: <https://github.com/oferchen/rsync/releases>  
2. Download the asset for your OS/arch (e.g., `.deb`, `.rpm`, or `.tar.*`).  
3. Install it using your platform’s package manager or by extracting the tarball.

The packaging pipeline installs `oc-rsync` under dedicated paths so that the system `rsync` can remain installed in parallel.

#### Linux Package Compatibility

Each release provides multiple package variants to ensure compatibility across different Linux distributions.

##### Debian/Ubuntu Packages (.deb)

| Package | glibc | Target Distributions |
|---------|-------|---------------------|
| `oc-rsync_*_amd64.deb` | ≥ 2.35 | Ubuntu 22.04+, Debian 12+ |
| `oc-rsync_*_arm64.deb` | ≥ 2.35 | Ubuntu 22.04+, Debian 12+ (ARM64) |
| `oc-rsync_*_amd64_focal.deb` | ≥ 2.31 | Ubuntu 20.04+, Debian 11+ |
| `oc-rsync_*_arm64_focal.deb` | ≥ 2.31 | Ubuntu 20.04+, Debian 11+ (ARM64) |

##### Linux Tarballs

| Tarball | glibc | Architecture |
|---------|-------|--------------|
| `oc-rsync-*-linux-amd64.tar.gz` | ≥ 2.35 | x86_64 |
| `oc-rsync-*-linux-aarch64.tar.gz` | ≥ 2.35 | ARM64/AArch64 |

##### Understanding glibc Compatibility

Linux binaries are dynamically linked against the GNU C Library (glibc). A binary built on a newer system may require a glibc version that older systems don't have, causing errors like:

```
/lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.34' not found
```

**Check your system's glibc version:**

```bash
ldd --version | head -1
# Example output: ldd (Ubuntu GLIBC 2.31-0ubuntu9.18) 2.31
```

**Selecting the right package:**

| Your Distribution | Recommended Package |
|-------------------|---------------------|
| Ubuntu 24.04 (Noble) | Standard packages (no suffix) |
| Ubuntu 22.04 (Jammy) | Standard packages (no suffix) |
| Ubuntu 20.04 (Focal) | `_focal` packages |
| Debian 12 (Bookworm) | Standard packages (no suffix) |
| Debian 11 (Bullseye) | `_focal` packages |
| RHEL/Rocky/Alma 9 | Standard tarballs |
| RHEL/Rocky/Alma 8 | `_focal` packages or build from source |

**Why separate focal builds?**

The `_focal` packages are built in an Ubuntu 20.04 container environment, ensuring they only depend on glibc 2.31 symbols. This provides broader compatibility at the cost of missing some newer glibc optimizations. For most use cases, there is no functional difference between the standard and focal builds.

---

### Homebrew 🍺

You can install `oc-rsync` via Homebrew from the custom tap:

```bash
# Add the tap (one-time)
brew tap oferchen/rsync https://github.com/oferchen/rsync

# Install oc-rsync
brew install oferchen/rsync/oc-rsync
```

To upgrade to the latest released version:

```bash
brew update
brew upgrade oferchen/rsync/oc-rsync
```

After installation, confirm that the binary is available and reports the expected Rust-branded version:

```bash
oc-rsync --version
```

---

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
```

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
src/bin/oc-rsync.rs     # Client + daemon entry point
crates/cli/             # CLI: flags, help, UX parity
crates/core/            # Core types, error model, shared utilities
crates/protocol/        # Protocol v32: negotiation, tags, framing, IO
crates/transfer/        # Server-side transfer: generator, receiver, delta
crates/engine/          # File list & data pump pipelines
crates/daemon/          # Rsync-like daemon behaviors
crates/filters/         # Include/exclude pattern engine
crates/checksums/       # Rolling & strong checksum implementations
crates/bandwidth/       # Throttling/pacing primitives
crates/flist/           # File list construction and serialization
crates/metadata/        # File metadata handling (permissions, xattr, ACL)
docs/                   # Design notes, internal docs
tools/                  # Dev utilities (e.g., enforce_limits.sh)
xtask/                  # Developer tasks: docs validation, packaging helpers
```

---

## Development

### Prerequisites

* Rust toolchain 1.88.0 (managed via `rust-toolchain.toml`).
* [`cargo-nextest`](https://nexte.st/) for running the test suite:

  ```bash
  cargo install cargo-nextest --locked
  ```

  A helper script (`scripts/install-nextest.sh`) installs the locked
  dependency set for environments that don't have a recent toolchain.

* [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) if you want to
  generate coverage reports locally.

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
cargo nextest run --workspace --all-features

# Convenience wrapper (falls back to `cargo test` if `cargo-nextest` is missing)
cargo xtask test
```

Example coverage flow (LLVM based):

```bash
cargo llvm-cov clean
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

Where enabled, CI may enforce coverage gates to avoid regressions.

### Interop & compliance harness

An interop harness exercises both directions against upstream `rsync` releases (e.g., 3.0.9, 3.1.3, 3.4.1):

* Upstream `rsync` client → `oc-rsync --daemon`
* `oc-rsync` client → upstream `rsync` daemon

Look under `tools/ci/` for the relevant scripts and pinned upstream versions.

Example smoke test:

```bash
# Replace host/module with your upstream rsyncd endpoint
cargo run --bin oc-rsync -- -av rsync://host/module/ /tmp/sync-test
```

### XTask & docs validation

Documentation and policy checks are wired via `xtask`:

```bash
# Validate README and docs (anchors, headings, fenced blocks, link sets as configured)
cargo xtask docs

# Source/policy limits
bash tools/enforce_limits.sh

# One-liner: fmt + clippy + tests + docs
cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings \
  && cargo nextest run --workspace --all-features \
  && cargo xtask docs
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

## Feature Flags

The workspace uses Cargo feature flags to enable optional functionality. The following features are available:

### Core Features (enabled by default)

| Feature | Crate | Description |
|---------|-------|-------------|
| `zstd` | core, cli, engine, compress | Zstandard compression algorithm support |
| `lz4` | core, cli, engine, compress | LZ4 compression algorithm support |
| `acl` | core, cli, engine, metadata | POSIX ACL (Access Control List) preservation |
| `xattr` | core, cli, engine, metadata | Extended attribute preservation |
| `iconv` | core, protocol | Filename encoding conversion (via encoding_rs) |
| `xxh3-simd` | checksums | Runtime SIMD detection for XXH3 (AVX2/NEON) |

### Optional Features

| Feature | Crate | Description |
|---------|-------|-------------|
| `parallel` | checksums, flist | Parallel processing using rayon (checksum computation, file list building) |
| `async` | core, protocol, engine, daemon | Tokio-based async I/O for concurrent operations |
| `tracing` | core, engine, daemon | Structured logging instrumentation for performance analysis |
| `concurrent-sessions` | daemon | Thread-safe session/connection tracking with DashMap |
| `sd-notify` | daemon, core | systemd notification support for service integration |
| `serde` | logging, protocol, flist | Serialization support for configuration and protocol types |
| `test-support` | bandwidth | Test utilities for bandwidth limiting |
| `openssl` | checksums | Use OpenSSL for hash implementations (adds C dependency) |
| `openssl-vendored` | checksums | Use vendored OpenSSL build (adds C dependency) |
| `zlib-sys` | compress | Use system zlib instead of pure-Rust miniz_oxide |

### Building with Features

```bash
# Default features (recommended for most users)
cargo build --workspace

# All features (for development/testing)
cargo build --workspace --all-features

# Minimal build (no optional features)
cargo build --workspace --no-default-features

# Specific feature combinations
cargo build --workspace --features "parallel,tracing"
cargo build -p daemon --features "concurrent-sessions,sd-notify"
cargo build -p checksums --features "parallel"
```

### Feature Dependencies

Some features have cross-crate dependencies:

- `core/zstd` enables `engine/zstd` and `compress/zstd`
- `core/async` enables `engine/async`
- `daemon/tracing` enables `core/tracing`
- `daemon/concurrent-sessions` requires DashMap for lock-free concurrent data structures

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
    cargo nextest run --workspace --all-features
    cargo xtask docs
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
