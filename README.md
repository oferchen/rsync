[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** - installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.6.0 (alpha) - Wire-compatible drop-in replacement for rsync 3.4.1 (protocols 28-32).

All transfer modes (local, SSH, daemon), delta algorithm, metadata preservation, incremental recursion, and compression are complete. Interop tested against upstream rsync 3.0.9, 3.1.3, and 3.4.1.

| Component | Status |
|-----------|--------|
| **Transfer** | Local, SSH, daemon push/pull |
| **Delta** | Rolling + strong checksums, block matching |
| **Metadata** | Permissions, timestamps, ownership, ACLs (`-A`), xattrs (`-X`) |
| **File handling** | Sparse, hardlinks, symlinks, devices, FIFOs |
| **Deletion** | `--delete` (before/during/after/delay), `--delete-excluded` |
| **Compression** | zlib, zstd, lz4 with level control and auto-negotiation |
| **Checksums** | MD4, MD5, XXH3/XXH128 with SIMD (AVX2, SSE2, NEON) |
| **Incremental recursion** | Pull direction; sender-side disabled pending interop validation |
| **Batch** | `--write-batch` / `--read-batch` roundtrip |
| **Daemon** | Negotiation, auth, modules, chroot, syslog, pre/post-xfer exec |
| **Filtering** | `--filter`, `--exclude`, `--include`, `.rsync-filter`, `--files-from` |
| **Reference dirs** | `--compare-dest`, `--link-dest`, `--copy-dest` |
| **Options** | `--delay-updates`, `--inplace`, `--partial`, `--iconv`, fuzzy matching |
| **I/O** | io_uring (Linux 5.6+), `copy_file_range`, `clonefile` (macOS), adaptive buffers |
| **Platforms** | Linux, macOS (full); Windows (partial - no ACLs/xattrs/symlinks/devices) |

### Platform Support

The primary platform is Linux. macOS is well-supported with parity for all metadata, ACL, and xattr features. Windows builds and runs core transfer modes, but several POSIX-specific features are stubbed; ongoing work targets Windows ACL preservation (#1866), Windows xattr (alternate data stream) preservation (#1867), and wiring IOCP into the transfer pipeline (#1868).

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Permissions (`-p`) | ✓ | ✓ | ⚠ | Windows preserves only the read-only flag; POSIX mode bits are not applicable. |
| Times (`-t`) | ✓ | ✓ | ✓ | Nanosecond precision via the `filetime` crate on all platforms. |
| File ownership (`-o`/`-g`, uid/gid) | ✓ | ✓ | ✗ | `apply_ownership_from_entry` is a no-op on non-Unix; uid/gid mapping is Unix-only. |
| ACLs (`-A`) | ✓ | ✓ | ✗ | Uses `exacl` on Linux/macOS/FreeBSD; Windows falls through to a no-op stub with a one-time warning (see #1866). |
| Extended attributes (`-X`) | ✓ | ✓ | ✗ | Wired only behind `cfg(all(unix, feature = "xattr"))`; non-Unix uses a no-op stub with a one-time warning (see #1867). |
| Hardlinks (`-H`) | ✓ | ✓ | ✓ | Uses portable `std::fs::hard_link`; works on NTFS. |
| Symbolic links | ✓ | ✓ | ✗ | `create_symlinks` is `cfg(not(unix))` no-op; symlink entries are skipped on Windows. |
| Devices/specials (`-D`) | ✓ | ✓ | ✗ | `create_fifo` and `create_device_node` are no-ops on non-Unix. |
| Sparse files (`-S`) | ✓ | ✓ | ⚠ | Uses portable `seek` + `set_len`; depends on filesystem (NTFS supports sparse but is not explicitly marked via `FSCTL_SET_SPARSE`). |
| Async I/O backend | ✓ io_uring | ⚠ standard I/O | ⚠ IOCP compiled, not wired | io_uring runtime-detected on Linux 5.6+; IOCP is implemented in `fast_io` but not yet consumed by the transfer pipeline (#1868). |
| Reflink / clone copy | ✓ FICLONE | ✓ clonefile | ⚠ ReFS reflink | Linux Btrfs/XFS/bcachefs via `FICLONE`; macOS via `clonefile`; Windows via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS only). |
| Optimized file copy | ✓ `copy_file_range` | ✓ `fcopyfile` | ✓ `CopyFileExW` | All three are wired into the local-copy executor with standard-I/O fallback. |

Legend: ✓ supported, ⚠ partial or not yet wired, ✗ not implemented.

### What's New (v0.6.0)

**Compression**
- Zstd auto-negotiation - peers exchange supported codecs, first mutual match wins
- Continuous zstd/lz4 stream across files matching upstream session-level codec context
- Per-token compression flush alignment for zlib, zstd, and lz4

**Metadata**
- ACL wire format (`-A`) interop with upstream rsync 3.4.1
- Xattr wire format (`-X`) with abbreviation encoding for repeated namespace prefixes
- Hardlink receiver-side inode/device mapping for daemon push transfers

**Performance**
- Adaptive I/O buffers (8KB-1MB) scaled to file size
- `FileEntry` memory reduced via `Box<FileEntryExtras>` for rarely-used fields
- Lock-free buffer pool (`crossbeam::ArrayQueue`) replaces `Mutex<Vec>`
- Shared file list via `Arc` eliminates per-file clone overhead
- Precomputed sort keys remove per-comparison `memrchr` calls
- Parallel basis file signature computation in pipeline fill
- Work-stealing deque replaces `par_bridge()` for delta dispatch
- Rayon-based parallel stat replaces `tokio::spawn_blocking`

**Batch mode**
- Full batch roundtrip with upstream rsync (write + read in both directions)
- INC_RECURSE interleaving, uid/gid name lists, checksum seed in batch header
- Protocol stream format replaces custom encoding

**Fixes**
- SSH transfer deadlocks and protocol compatibility resolved
- Daemon filter rules applied on receiver side for push transfers
- INC_RECURSE capability direction corrected for daemon push
- `--files-from` daemon flag compatibility with upstream
- 10+ interop known failures resolved across batch, compression, filters, and paths

### Interop Testing

Tested against upstream rsync **3.0.9**, **3.1.3**, and **3.4.1** in CI across protocols 28-32. Both push and pull directions verified for 30+ scenarios covering transfer modes, deletion, compression, metadata, reference dirs, file selection, batch roundtrip, path handling, device nodes, and daemon auth.

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Threaded architecture replaces upstream's fork-based pipeline while keeping full protocol compatibility, reducing syscall overhead and context switches. Adaptive I/O buffers scale from 8KB to 1MB based on file size. Optional io_uring on Linux 5.6+ with three policies: *auto* (default; probe kernel and fall back to standard I/O), `--io-uring` (require io_uring; error if unavailable), `--no-io-uring` (always use standard buffered I/O). The active backend is reported by `--version` and `-vv` output. See `oc-rsync(1)` for details.

### Known Limitations / Architectural Trade-offs

oc-rsync is wire-compatible with upstream rsync 3.4.1, but a few architectural choices and unfinished surfaces are worth calling out for operators planning a deployment:

- **io_uring kernel requirement.** Provided buffer rings (PBUF_RING) require Linux **5.19+**; older 5.6-5.18 kernels fall back to standard buffered I/O via runtime probing.
- **Fixed io_uring buffer pool.** The registered buffer pool is sized at compile time (1024 × 4 KiB = 4 MiB) and does not adapt under sustained I/O pressure. Workloads with very high concurrent file fan-out may see throughput plateau before saturating the device.
- **bgid namespace.** io_uring buffer-group IDs are a 16-bit namespace; the buffer ring helpers cap at this bound. Long-running daemons that recycle thousands of distinct ring groups should monitor for exhaustion.
- **Single-thread delta computation.** The delta sender is sequential per file. Rolling-hash fan-out across files is not yet parallelised; large-file workloads fully utilise one CPU per transfer rather than scaling delta CPU horizontally.
- **SSH compression interaction.** When the SSH cipher already performs compression (e.g., `Compression yes` in `ssh_config`), running `oc-rsync -z` will compress payloads twice. There is currently no auto-detection / auto-disable path; operators should pick one layer.
- **Daemon TLS.** Native TLS is not built into the daemon. Deploy `oc-rsync --daemon` behind `stunnel`, `ssh -L`, or a reverse proxy that terminates TLS. See [SECURITY.md](./SECURITY.md) for hardening recipes. See [`docs/deployment/daemon-tls.md`](./docs/deployment/daemon-tls.md).
- **Windows IOCP not wired.** IOCP is implemented in `fast_io` but not yet consumed by the transfer pipeline (#1868); Windows uses standard buffered I/O for transfers.
- **`.rsync-filter` per-directory inheritance.** Inheritance semantics match upstream for the common cases tested in the interop suite, but exhaustive parity against upstream's filter-tree corner cases (deeply nested merges, anchored vs unanchored interactions) is still being validated.
- **`--checksum-seed` / `--fuzzy` / `--iconv`.** These flags are accepted and exercised in the common path; deeper conformance audits against upstream rsync 3.4.1 are tracked separately.

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
crates/transfer/        # Generator, receiver, delta transfer pipeline
crates/engine/          # Local copy executor, sparse writes, temp-file commit
crates/daemon/          # Daemon mode, module access control, systemd
crates/checksums/       # Rolling and strong checksums (MD4, MD5, XXH3, SIMD)
crates/filters/         # Include/exclude pattern engine, .rsync-filter
crates/metadata/        # Permissions, uid/gid, mtime, ACLs, xattrs
crates/platform/        # Platform-specific unsafe code isolation (signals, chroot)
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
crates/test-support/    # Shared test utilities (dev-dependency only)
```

See `cargo doc --workspace --no-deps --open` for API documentation.

### Architecture

```text
cli -> core -> engine, daemon, rsync_io, logging
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
                                                                            -> platform
```

Key crates: **cli** (Clap v4), **core** (orchestration facade), **protocol** (wire v28-32, multiplex framing), **transfer** (generator/receiver, delta pipeline), **engine** (local copy, sparse writes, buffer pool), **checksums** (MD4/MD5/XXH3, SIMD), **daemon** (TCP, auth, modules), **platform** (unsafe code isolation).

---

## Security

All crates enforce `#![deny(unsafe_code)]`. Unsafe blocks are only permitted in crates that directly wrap platform FFI:

- **checksums** - SIMD intrinsics (AVX2, SSE2, NEON) with scalar fallbacks
- **fast_io** - io_uring, `copy_file_range`, sendfile, mmap with standard I/O fallbacks
- **metadata** - UID/GID lookup, timestamps, ownership, xattrs, ACLs
- **platform** - Signal handlers, chroot, daemonize, process management
- **engine** - Buffer pool atomics, deferred fsync, clonefile
- **windows-gnu-eh** - Windows GNU exception handling shims

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
Thanks to **Elad** for his endless patience hearing rsync protocol commentary as I'm introduced to it.
