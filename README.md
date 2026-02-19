[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** — installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.5.8 (beta)

Core transfer, delta algorithm, daemon mode, and SSH transport are complete. Interoperability tested against upstream rsync 3.0.9, 3.1.3, and 3.4.1.

### Performance

![Benchmark: oc-rsync vs upstream rsync](docs/assets/benchmark.svg)

Single-process architecture eliminates fork overhead: 22% fewer syscalls, 36 context switches vs upstream's 92 per transfer.

---

## Installation

### Homebrew

```bash
brew tap oferchen/rsync https://github.com/oferchen/rsync
brew install oferchen/rsync/oc-rsync
```

### Prebuilt packages

Download from the [Releases](https://github.com/oferchen/rsync/releases) page:

| Platform | Formats |
|----------|---------|
| Linux (x86_64, aarch64) | `.deb`, `.rpm`, static musl `.tar.gz` |
| macOS (x86_64, aarch64) | `.tar.gz` |
| Windows (x86_64) | `.tar.gz`, `.zip` |

Each release includes three build variants:

| Variant | Description |
|---------|-------------|
| **stable** (recommended) | Built with Rust stable. No filename suffix. |
| **beta** | Built with Rust beta. Filename includes `-beta`. |
| **nightly** | Built with Rust nightly. Filename includes `-nightly`. |

Most users should download the **stable** variant.

### Build from source

Requires Rust **1.88+**.

```bash
git clone https://github.com/oferchen/rsync.git
cd rsync
cargo build --workspace --release
```

---

## Usage

Works like `rsync`:

```bash
# Local sync
oc-rsync -av ./source/ ./dest/

# Remote pull
oc-rsync -av user@host:/remote/path/ ./local/

# Remote push
oc-rsync -av ./local/ user@host:/remote/path/

# Daemon mode
oc-rsync --daemon --config=/etc/oc-rsyncd/oc-rsyncd.conf
```

For supported options: `oc-rsync --help`

---

## Development

### Prerequisites

- Rust 1.88.0 (managed via `rust-toolchain.toml`)
- [`cargo-nextest`](https://nexte.st/): `cargo install cargo-nextest --locked`

### Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
cargo nextest run --workspace --all-features
```

### Project layout

```text
src/bin/oc-rsync.rs     # Entry point
crates/cli/             # CLI flags, help, output formatting
crates/core/            # Orchestration facade, session management, config
crates/protocol/        # Wire protocol (v28-32), multiplex framing
crates/transfer/        # Generator (sender), receiver, delta transfer
crates/engine/          # Local copy executor, sparse writes, temp-file commit
crates/daemon/          # Daemon mode, module access control, systemd
crates/checksums/       # Rolling and strong checksums (MD4, MD5, XXH3, SIMD)
crates/filters/         # Include/exclude pattern engine, .rsync-filter
crates/metadata/        # Permissions, uid/gid, mtime, ACLs, xattrs
crates/rsync_io/        # SSH stdio, rsync:// TCP transport, handshake
crates/fast_io/         # Platform I/O (io_uring, copy_file_range)
crates/compress/        # zstd, lz4, zlib compression codecs
crates/bandwidth/       # Bandwidth limiting and rate control
crates/signature/       # Signature layout and block-size calculations
crates/match/           # Delta matching and block search
crates/flist/           # File list generation and traversal
crates/logging/         # Logging macros and verbosity control
crates/batch/           # Batch file read/write support
crates/branding/        # Binary naming and version metadata
```

See `cargo doc --workspace --no-deps --open` for API documentation.

---

## Security

Protocol parsing crates enforce `#![deny(unsafe_code)]`. Unsafe code is limited to SIMD-accelerated checksums and platform I/O, both with safe fallbacks. Not vulnerable to known upstream rsync CVEs (CVE-2024-12084 through CVE-2024-12088, CVE-2024-12747).

For security issues, see [SECURITY.md](./SECURITY.md).

---

## Contributing

1. Fork and create a feature branch
2. Run `cargo fmt`, `cargo clippy`, and `cargo nextest run`
3. Open a PR describing behavioral changes and interop impact

---

## License

GNU GPL v3.0 or later. See [`LICENSE`](./LICENSE).

---

## Acknowledgements

Inspired by [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team.
Thanks to **Pieter** for his heroic patience in enduring months of my rsync commentary.
