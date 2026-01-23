[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Build-cross](https://github.com/oferchen/rsync/actions/workflows/release-cross.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/release-cross.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
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
- [Security](#security)
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
- **oc-rsync release:** **0.5.3**.
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
- 10,285+ tests passing (100% pass rate)
- Comprehensive integration tests for delta transfer
- Error scenario tests (cleanup, categorization, edge cases)
- Protocol version interoperability validated (protocols 28-32)
- Content integrity and metadata preservation validated
- Edge cases covered (empty files, large files, size mismatches, binary data)
- Wire protocol compatibility verified against upstream rsync 3.4.1

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

Prebuilt artifacts are published on the GitHub **Releases** page for multiple platforms, architectures, and Rust toolchains.

1. Go to: <https://github.com/oferchen/rsync/releases>
2. Download the asset for your OS/arch (e.g., `.deb`, `.rpm`, or `.tar.*`).
3. Install it using your platform's package manager or by extracting the tarball.

The packaging pipeline installs `oc-rsync` under dedicated paths so that the system `rsync` can remain installed in parallel.

#### Toolchain Variants

Each release provides artifacts built with three Rust toolchains:

| Toolchain | Naming Convention | Recommended For |
|-----------|-------------------|-----------------|
| **stable** | `oc-rsync-VERSION-PLATFORM.EXT` | Production use (default) |
| **beta** | `oc-rsync-VERSION-PLATFORM-beta.EXT` | Testing upcoming Rust features |
| **nightly** | `oc-rsync-VERSION-PLATFORM-nightly.EXT` | Experimental/bleeding-edge |

**Examples:**
- Stable: `oc-rsync_0.5.3-1_amd64.deb`, `oc-rsync-0.5.3-darwin-x86_64.tar.gz`
- Beta: `oc-rsync_0.5.3-1-beta_amd64.deb`, `oc-rsync-0.5.3-darwin-x86_64-beta.tar.gz`
- Nightly: `oc-rsync_0.5.3-1-nightly_amd64.deb`, `oc-rsync-0.5.3-darwin-x86_64-nightly.tar.gz`

#### Linux Package Compatibility

Each release provides multiple package variants to ensure compatibility across different Linux distributions.

##### Debian/Ubuntu Packages (.deb)

| Package | glibc | Target Distributions |
|---------|-------|---------------------|
| `oc-rsync_*_amd64.deb` | ≥ 2.35 | Ubuntu 22.04+, Debian 12+ |
| `oc-rsync_*_arm64.deb` | ≥ 2.35 | Ubuntu 22.04+, Debian 12+ (ARM64) |

##### RPM Packages (.rpm)

| Package | glibc | Target Distributions |
|---------|-------|---------------------|
| `oc-rsync-*.x86_64.rpm` | ≥ 2.35 | Fedora 36+, RHEL 9+, Rocky 9+, Alma 9+ |
| `oc-rsync-*.aarch64.rpm` | ≥ 2.35 | Fedora 36+, RHEL 9+, Rocky 9+, Alma 9+ (ARM64) |

##### Static musl Tarballs (Portable)

For maximum portability, statically-linked musl binaries are available. These have **no glibc dependency** and work on any Linux distribution:

| Tarball | Linking | Architecture |
|---------|---------|--------------|
| `oc-rsync-*-linux-x86_64-musl.tar.gz` | Static (musl) | x86_64 |
| `oc-rsync-*-linux-aarch64-musl.tar.gz` | Static (musl) | ARM64/AArch64 |

The musl builds include full feature parity with glibc builds, including ACL and extended attribute support.

##### Selecting the Right Package

| Your Distribution | Recommended Package |
|-------------------|---------------------|
| Ubuntu 22.04+ / Debian 12+ | `.deb` packages |
| Fedora 36+ / RHEL 9+ / Rocky 9+ / Alma 9+ | `.rpm` packages |
| RHEL 8 / Rocky 8 / Alma 8 | musl static tarballs |
| Older glibc systems | musl static tarballs |
| Alpine Linux | musl static tarballs |
| Any Linux (portable) | musl static tarballs |

**Why musl static builds?**

The musl tarballs are fully statically linked, meaning they have zero runtime dependencies. This makes them ideal for:
- Older distributions with outdated glibc
- Containers and minimal environments
- Systems where you cannot install packages
- Maximum portability across Linux distributions

---

### Homebrew 🍺

You can install `oc-rsync` via Homebrew from the custom tap:

```bash
# Add the tap (one-time)
brew tap oferchen/rsync https://github.com/oferchen/rsync

# Install oc-rsync (stable - recommended)
brew install oferchen/rsync/oc-rsync

# Or install beta/nightly toolchain builds
brew install oferchen/rsync/oc-rsync@beta
brew install oferchen/rsync/oc-rsync@nightly
```

| Formula | Toolchain | Description |
|---------|-----------|-------------|
| `oc-rsync` | stable | Production-ready, recommended for most users |
| `oc-rsync@beta` | beta | Latest beta Rust features, good for testing |
| `oc-rsync@nightly` | nightly | Bleeding-edge, may include experimental optimizations |

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

CI publishes artifacts across three Rust toolchains (stable, beta, nightly) for:

| Platform | Architectures | Formats |
|----------|---------------|---------|
| Linux GNU | x86_64, aarch64 | `.deb`, `.rpm` |
| Linux musl | x86_64, aarch64 | `.tar.gz` (static, portable) |
| macOS | x86_64, aarch64 | `.tar.gz` |
| Windows | x86_64 | `.tar.gz`, `.zip` |

**Total artifacts per release:** 30 files (27 unique binaries + 3 Windows zips)

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

## Security

oc-rsync is designed with security as a core principle, leveraging Rust's memory safety guarantees to eliminate entire classes of vulnerabilities that have affected the C rsync implementation.

### Memory Safety

- **No unsafe code in protocol handling**: All wire protocol parsing crates (`protocol`, `batch`, `signature`, `matching`) enforce `#![deny(unsafe_code)]`, ensuring memory safety through Rust's type system.
- **Bounds-checked operations**: All buffer accesses use Rust's safe indexing, eliminating buffer overflows.
- **No uninitialized memory**: Rust's initialization requirements prevent information leaks from uninitialized stack/heap memory.

### CVE Immunity

oc-rsync is **not vulnerable** to known rsync CVEs due to its Rust implementation:

| CVE | Description | oc-rsync Status |
|-----|-------------|-----------------|
| CVE-2024-12084 | Heap buffer overflow in checksum parsing | **Not vulnerable** - Vec<u8> handles sizing |
| CVE-2024-12085 | Info leak via uninitialized stack buffer | **Not vulnerable** - No uninitialized memory |
| CVE-2024-12086 | Server leaks arbitrary client files | **Not vulnerable** - Strict path validation |
| CVE-2024-12087 | Path traversal via `--inc-recursive` | **Not vulnerable** - Path sanitization |
| CVE-2024-12088 | `--safe-links` bypass | **Mitigated** - Rust path handling |
| CVE-2024-12747 | Symlink race condition | **Mitigated** - See note below |

**Note on symlink races**: While Rust eliminates memory corruption, TOCTOU races are an OS-level concern. The `symlink_target_is_safe()` function validates paths, but atomic operations depend on filesystem support.

### Fuzzing

The `crates/protocol/fuzz` directory contains cargo-fuzz targets for critical parsing functions:
- Variable-length integer decoding (`fuzz_varint`)
- Delta wire protocol (`fuzz_delta`)
- Multiplex frame parsing (`fuzz_multiplex_frame`)
- Legacy greeting parsing (`fuzz_legacy_greeting`)

### Reporting Security Issues

For security vulnerabilities, please see [SECURITY.md](./SECURITY.md) or email the maintainer directly rather than opening a public issue.

---

## License

This project is licensed under the **GNU GPL v3.0 or later**.
See [`LICENSE`](./LICENSE) for the full text.

---

## Acknowledgements

Inspired by the original [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team, and by the broader Rust ecosystem that made this re-implementation feasible.
Thanks to **Pieter** for his heroic patience in enduring months of my rsync commentary.
