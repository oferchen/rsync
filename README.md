[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.1 (protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** - installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.6.1 (alpha) - Wire-compatible drop-in replacement for rsync 3.4.1 (protocols 28-32).

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
| **Incremental recursion** | Pull and push directions, enabled by default |
| **Batch** | `--write-batch` / `--read-batch` roundtrip |
| **Daemon** | Negotiation, auth, modules, chroot, syslog, pre/post-xfer exec |
| **Filtering** | `--filter`, `--exclude`, `--include`, `.rsync-filter`, `--files-from` |
| **Reference dirs** | `--compare-dest`, `--link-dest`, `--copy-dest` |
| **Options** | `--delay-updates`, `--inplace`, `--partial`, `--iconv`, fuzzy matching |
| **I/O** | io_uring (Linux 5.6+), `copy_file_range`, `clonefile` (macOS), adaptive buffers |
| **Platforms** | Linux, macOS (full); Windows (ACLs, xattrs via NTFS ADS, IOCP socket I/O; no symlinks/devices) |

### Platform Support

The primary platform is Linux. macOS is well-supported with parity for all metadata, ACL, and xattr features, including AppleDouble (`._foo`) resource-fork preservation. Windows builds and runs core transfer modes with native ACLs (via `windows-rs` `GetSecurityInfo`/`SetSecurityInfo`), xattrs (via NTFS Alternate Data Streams), and IOCP socket I/O (`WSARecv`/`WSASend`); symlinks and POSIX device nodes remain stubbed.

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Permissions (`-p`) | ✓ | ✓ | ⚠ | Windows preserves only the read-only flag; POSIX mode bits are not applicable. |
| Times (`-t`) | ✓ | ✓ | ✓ | Nanosecond precision via the `filetime` crate on all platforms. |
| File ownership (`-o`/`-g`, uid/gid) | ✓ | ✓ | ✗ | `apply_ownership_from_entry` is a no-op on non-Unix; uid/gid mapping is Unix-only. |
| ACLs (`-A`) | ✓ | ✓ | ✓ | Uses `exacl` on Linux/macOS/FreeBSD; Windows uses `windows-rs` `GetSecurityInfo`/`SetSecurityInfo` for native NTFS ACL round-trip. |
| Extended attributes (`-X`) | ✓ | ✓ | ✓ | Linux/macOS via the `xattr` crate (macOS adds AppleDouble resource-fork support); Windows stores xattrs as NTFS Alternate Data Streams. |
| Hardlinks (`-H`) | ✓ | ✓ | ✓ | Uses portable `std::fs::hard_link`; works on NTFS. |
| Symbolic links | ✓ | ✓ | ✗ | `create_symlinks` is `cfg(not(unix))` no-op; symlink entries are skipped on Windows. |
| Devices/specials (`-D`) | ✓ | ✓ | ✗ | `create_fifo` and `create_device_node` are no-ops on non-Unix. |
| Sparse files (`-S`) | ✓ | ✓ | ⚠ | Uses portable `seek` + `set_len`; depends on filesystem (NTFS supports sparse but is not explicitly marked via `FSCTL_SET_SPARSE`). |
| Async I/O backend | ✓ io_uring | ⚠ standard I/O | ⚠ IOCP compiled, not wired | io_uring runtime-detected on Linux 5.6+; IOCP is implemented in `fast_io` but not yet consumed by the transfer pipeline (#1868). |
| Reflink / clone copy | ✓ FICLONE | ✓ clonefile | ⚠ ReFS reflink | Linux Btrfs/XFS/bcachefs via `FICLONE`; macOS via `clonefile`; Windows via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS only). |
| Optimized file copy | ✓ `copy_file_range` | ✓ `fcopyfile` | ✓ `CopyFileExW` | All three are wired into the local-copy executor with standard-I/O fallback. |

Legend: ✓ supported, ⚠ partial or not yet wired, ✗ not implemented.

### What's New (v0.6.1)

**Protocol & interop**
- INC_RECURSE sender enabled by default for both push and pull directions
- Rolling checksum now sign-extends bytes to match upstream's `schar` semantics, fixing block-match parity at protocol >= 30
- `--jump-host` for multi-hop SSH transports, with a dedicated proxy-jump interop test against upstream 3.4.1

**Charset translation (`--iconv`)**
- End-to-end iconv pipeline: file list ingest (receiver) and emit (sender), symlink targets, `--files-from`, and secluded args all flow through `FilenameConverter`
- Client-side resolver now uses UTF-8 as the wire charset, matching upstream `rsync.c:130-140`
- `--iconv` forwarded to remote rsync server args (SSH) and to daemon args (`rsync://`), mirroring upstream `options.c:2716-2723`
- Server-mode flag parser recognises `--iconv=` and `--timeout=` so they no longer leak into positional args and corrupt the destination path
- Daemon module `charset =` directive wires per-module iconv into the runtime
- Golden byte tests for iconv-converted filenames; live interop test against upstream 3.4.1

**Daemon**
- `fake super = yes` module directive consumed by daemon and transfer paths
- `charset =` module directive wired into the iconv runtime

**Windows platform**
- Native NTFS ACL round-trip (`-A`) via `windows-rs` `GetSecurityInfo` / `SetSecurityInfo`
- Extended attributes (`-X`) stored as NTFS Alternate Data Streams
- IOCP socket I/O via `WSARecv` / `WSASend` for daemon and SSH transports

**macOS platform**
- AppleDouble (`._` resource-fork sidecars) wire-compatible with upstream rsync

**Compatibility**
- `--fake-super` mode for unprivileged metadata preservation via xattrs, with `am_root` forced false so ownership is encoded in `user.rsync.%stat` rather than applied
- `-oBatchMode=yes` injection now gated on `is_ssh_program()` so non-OpenSSH transports (e.g. `rsh`) are no longer corrupted

**Performance**
- io_uring shared submission ring across reader and writer worker pools
- io_uring SEND-path deadlock eliminated under sustained TCP back-pressure

**Security & code quality**
- `SensitiveBytes` uses the `zeroize` crate to scrub daemon credentials and auth secrets on drop
- Safe `syslog` crate replaces hand-rolled FFI in the logging-sink crate
- `setsockopt` FFI consolidated into the `fast_io` crate per the unsafe-code policy

**Testing & CI**
- Property tests added for compress codec round-trip, rolling-checksum SIMD/scalar parity, FilterChain precedence/anchoring, and bwlimit rate/burst pacing
- CI fail-fast guard rejects stale `Cargo.lock`
- `standalone:delta-stats` and `iconv-local-ssh` cleared from `KNOWN_FAILURES`

**Documentation**
- SSH transport timeout coverage matrix
- Eliminate-path matrix for `tools/ci/known_failures.conf`
- Audit confirming filter rules match upstream's iconv policy

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
- **Daemon TLS.** Native TLS is not built into the daemon. Deploy `oc-rsync --daemon` behind `stunnel`, `ssh -L`, or a reverse proxy that terminates TLS. See [`docs/deployment/daemon-tls.md`](./docs/deployment/daemon-tls.md) for runnable terminator configs, hardened systemd units, and host-firewall rules; see [SECURITY.md](./SECURITY.md) for the broader hardening note.
- **Windows IOCP scope.** IOCP is wired for socket I/O (daemon and SSH transports); file-system reads and writes still use standard buffered I/O on Windows. Tracking work to extend IOCP to the file pipeline is in flight (#1868).
- **`.rsync-filter` per-directory inheritance.** Inheritance semantics match upstream for the common cases tested in the interop suite, but exhaustive parity against upstream's filter-tree corner cases (deeply nested merges, anchored vs unanchored interactions) is still being validated.
- **`--checksum-seed` / `--fuzzy`.** These flags are accepted and exercised in the common path; deeper conformance audits against upstream rsync 3.4.1 are tracked separately.

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

All crates enforce `#![deny(unsafe_code)]`. Targeted `#[allow(unsafe_code)]` is permitted only in crates that wrap platform FFI or SIMD intrinsics:

- **checksums** - SIMD intrinsics (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM) with scalar fallbacks
- **fast_io** - io_uring, `copy_file_range`, sendfile, mmap, IOCP, `WSARecv`/`WSASend`, `setsockopt` with standard I/O fallbacks
- **metadata** - UID/GID lookup, timestamps, ownership, xattrs, ACLs
- **platform** - Signal handlers, chroot, daemonize, process management
- **engine** - Buffer pool atomics, deferred fsync, clonefile, `CopyFileExW`
- **protocol** - One isolated allow in `multiplex::helpers` for frame parsing
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
