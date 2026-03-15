[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** - installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.5.9 (alpha)

Local, SSH, and daemon transfers are fully functional with native delta algorithm, metadata preservation, and all core options. Daemon mode handles negotiation, authentication, module access control, and file transfers natively. Interoperability tested against upstream rsync 3.0.9, 3.1.3, and 3.4.1 across protocols 28-32.

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
| Batch files (write + read) | Complete |
| Incremental recursion (push + pull) | Complete |
| Daemon daemonization (--detach) | Complete |
| Daemon syslog | Complete |
| Daemon chroot & privilege drop | Complete |
| Daemon pre/post-xfer exec | Complete |
| SIMD checksums (AVX2, SSE2, NEON) | Complete |
| Hardlink preservation | Complete |
| --files-from (local, stdin, remote) | Complete |
| --compare-dest, --link-dest, --copy-dest | Complete |
| --iconv charset conversion | Complete |
| Fuzzy matching (level 1 & 2) | Complete |
| Protocol 28-32 wire compatibility | Complete |
| io_uring (Linux 5.6+, optional) | Complete |
| Linux, macOS | Full support |
| Windows | Partial (no ACLs/xattrs) |

### Interop Testing

Tested against upstream rsync **3.0.9**, **3.1.3**, and **3.4.1** in CI across protocols 28-32. Both push (oc-rsync client to upstream daemon) and pull (upstream client to oc-rsync daemon) directions are verified for over 30 scenarios:

- Transfer modes: `--checksum`, `--whole-file`, `--ignore-times`, `--update`, `--append`
- Deletion: `--delete`, `--delete-during`, `--delete-after`, `--delete-excluded`
- Compression: `--compress`, `--compress-level`, zlib/zlibx negotiation
- Metadata: `--hard-links`, `--numeric-ids`, `--acls`, `--xattrs`, `--sparse`
- Reference dirs: `--compare-dest`, `--link-dest`, `--copy-dest`
- File selection: `--files-from`, `--exclude`, `--include`, `--filter`
- Special modes: `--inplace`, `--delay-updates`, `--partial`, `--partial-dir`
- Path handling: `--relative`, `--one-file-system`, `--safe-links`, `--copy-links`
- Batch: `--write-batch` / `--read-batch` roundtrip (oc-rsync and upstream)
- Output: `--itemize-changes`, `--dry-run`, `--bwlimit`
- Protocol: forced `--protocol=28` through `--protocol=32`
- Devices: device nodes, special files (`-D`)
- Auth: daemon module authentication

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Threaded architecture replaces upstream's fork-based pipeline for local transfers, reducing syscall overhead and context switches. Optional io_uring support on Linux 5.6+ for async file I/O (`--io-uring` / `--no-io-uring`).

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

Works like `rsync` - drop-in compatible:

```bash
# Local sync
oc-rsync -av ./source/ ./dest/

# Remote pull (SSH)
oc-rsync -av user@host:/remote/path/ ./local/

# Remote push (SSH)
oc-rsync -av ./local/ user@host:/remote/path/

# Daemon pull
oc-rsync -av rsync://host/module/ ./local/

# Daemon push
oc-rsync -av ./local/ rsync://host/module/

# Run as daemon
oc-rsync --daemon --config=/etc/oc-rsyncd/oc-rsyncd.conf

# Delta transfer with compression
oc-rsync -avz --compress-level=3 ./source/ ./dest/

# Checksum-based sync with deletion
oc-rsync -avc --delete ./source/ ./dest/

# Batch mode (record and replay)
oc-rsync -av --write-batch=changes ./source/ ./dest/
oc-rsync -av --read-batch=changes ./other-dest/
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

### Architecture

```text
cli -> core -> engine, daemon, transport, logging
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
```

- **cli** - CLI parsing (Clap v4), help text, output formatting
- **core** - Orchestration facade; all transfers go through `core::session()` and `CoreConfig`
- **protocol** - Wire protocol (v28-32), multiplex MSG_* framing, version negotiation
- **transfer** - Generator (sender) and receiver roles, delta transfer pipeline
- **engine** - Local copy executor, sparse writes, temp-file commit, buffer pool
- **checksums** - Rolling rsum + strong checksums (MD4/MD5/XXH3) with SIMD fast paths
- **daemon** - TCP listener, @RSYNCD: negotiation, auth, module access control

Design patterns used throughout: Strategy (checksum/compression selection), Builder (config objects), State Machine (connection lifecycle), Chain of Responsibility (filter rules), Dependency Inversion (trait-based abstractions).

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
