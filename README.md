[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** — installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.5.8 (alpha)

Local, SSH, and daemon transfers are fully functional with native delta algorithm, metadata preservation, and all core options. Daemon mode handles negotiation, authentication, module access control, and file transfers natively. Interoperability tested against upstream rsync 3.0.9, 3.1.3, and 3.4.1.

| Area | Status |
|------|--------|
| Local copy | Complete |
| SSH transfer | Complete |
| Daemon negotiation & auth | Complete |
| Daemon file transfer | Complete |
| Delta algorithm | Complete |
| Filter rules | Complete |
| --delete (all timings) | Complete |
| --delay-updates | Complete |
| Sparse files, hardlinks, symlinks | Complete |
| ACLs (-A), xattrs (-X) | Unix only |
| --compress (zlib, zstd, lz4) | Complete |
| Batch files | Local only; remote replay pending |
| Incremental recursion | Complete |
| Daemon daemonization (--detach) | Complete |
| Daemon syslog | Pending |
| SIMD checksums (AVX2, SSE2, NEON) | Complete |
| Linux, macOS | Full support |
| Windows | Partial (no ACLs/xattrs) |

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Threaded architecture replaces upstream's fork-based pipeline for local transfers, reducing syscall overhead and context switches.

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
| Linux (x86_64, aarch64) | `.deb`, `.rpm` (with OpenSSL), static musl `.tar.gz` |
| macOS (x86_64, aarch64) | `.tar.gz` |
| Windows (x86_64) | `.tar.gz`, `.zip` |

Linux static tarballs are available in two checksum variants:

| Variant | Filename | Description |
|---------|----------|-------------|
| **Pure Rust** (recommended) | `*-musl.tar.gz` | Pure-Rust checksums, zero system dependencies |
| **OpenSSL** | `*-musl-openssl.tar.gz` | OpenSSL-accelerated MD4/MD5 checksums (vendored, statically linked) |

Each release also includes three toolchain variants: **stable** (recommended, no suffix), **beta** (`-beta`), and **nightly** (`-nightly`).

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
crates/fast_io/         # Platform I/O (io_uring, copy_file_range, sendfile)
crates/compress/        # zstd, lz4, zlib compression codecs
crates/bandwidth/       # Bandwidth limiting and rate control
crates/signature/       # Signature layout and block-size calculations
crates/match/           # Delta matching and block search
crates/flist/           # File list generation and traversal
crates/logging/         # Logging macros and verbosity control
crates/logging-sink/    # Message sink and output formatting
crates/batch/           # Batch file read/write support
crates/branding/        # Binary naming and version metadata
crates/embedding/       # Programmatic entry points for library usage
crates/apple-fs/        # macOS filesystem operations (clonefile, FSEvents)
crates/windows-gnu-eh/  # Windows GNU exception handling shims
```

See `cargo doc --workspace --no-deps --open` for API documentation.

---

## Security

Protocol parsing crates enforce `#![deny(unsafe_code)]`. Unsafe code is limited to:

- SIMD-accelerated checksums (with scalar fallbacks)
- Platform I/O operations (sendfile, io_uring, mmap -- with fallbacks)
- Metadata/ownership FFI (UID/GID lookup, chroot, setuid/setgid)
- Windows GNU exception handling

Not vulnerable to known upstream rsync CVEs (CVE-2024-12084 through CVE-2024-12088, CVE-2024-12747). OS-level race conditions (TOCTOU) remain possible at filesystem boundaries.

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
