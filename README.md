[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** — installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.5.5 (beta)

Core transfer, delta algorithm, daemon mode, and SSH transport are complete. Interoperability tested against upstream rsync 3.4.1.

### Performance

v0.5.5 vs upstream rsync 3.4.1 on Linux x86_64 (110 MB, 1130 files):

| Workload | oc-rsync | upstream | Speedup |
|----------|----------|----------|---------|
| Initial sync | 114 ms | 127 ms | 1.1x |
| No-change sync | 115 ms | 131 ms | 1.1x |
| Checksum sync (-c) | 229 ms | 566 ms | **2.5x** |
| Incremental (10% changed) | 115 ms | 129 ms | 1.1x |
| Large files (100 MB) | 89 ms | 126 ms | 1.4x |
| Small files (1000x1KB) | 112 ms | 129 ms | 1.2x |
| Compressed (-z) | 113 ms | 127 ms | 1.1x |

18% faster on average; up to 2.5x faster for checksum-intensive workloads.

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
crates/core/            # Shared types, error model, config
crates/protocol/        # Wire protocol (v28-32)
crates/transfer/        # Generator, receiver, delta transfer
crates/engine/          # File list and data pump pipelines
crates/daemon/          # Daemon mode and module access control
crates/checksums/       # Rolling and strong checksums (SIMD)
crates/filters/         # Include/exclude pattern engine
crates/fast_io/         # Platform I/O (mmap, io_uring, copy_file_range)
crates/compress/        # zstd, lz4, zlib compression
```

See `cargo doc --workspace --no-deps --open` for API documentation.

---

## Security

Protocol parsing crates enforce `#![deny(unsafe_code)]`. Not vulnerable to known upstream rsync CVEs (CVE-2024-12084 through CVE-2024-12088, CVE-2024-12747).

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
