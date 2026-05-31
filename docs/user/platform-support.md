# Platform support tiers

This document describes oc-rsync's platform support model - which targets
are tested, which I/O optimizations are available on each, and what
operators should expect in terms of feature coverage and performance.

---

## Support tiers

oc-rsync classifies its supported platforms into three tiers based on CI
coverage depth, feature completeness, and performance validation.

### Tier 1 - primary development target

Full test suite, interop coverage, benchmarking, and release artifact
generation. Regressions block merge.

| Target | Runner | Notes |
|--------|--------|-------|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | Full workspace nextest (stable/beta/nightly), SSH integration tests, interop against upstream rsync 3.0.9 - 3.4.3, coverage via `cargo-llvm-cov`, benchmarks, fuzzing |

### Tier 2 - first-class platforms

Platform-specific crates tested on native runners. Stable toolchain
failures block merge. Interop smoke coverage. Release artifacts produced.

| Target | Runner | Tested Crates | Notes |
|--------|--------|---------------|-------|
| `x86_64-pc-windows-msvc` | `windows-latest` | core, engine, cli, metadata, fast_io, transfer | Stable/beta/nightly matrix; dedicated IOCP job; ACL/xattr job; interop (best-effort) |
| `x86_64-apple-darwin` | `macos-latest` | core, engine, cli, metadata, apple-fs | Stable/beta/nightly matrix; interop smoke; benchmarks (best-effort) |
| `aarch64-apple-darwin` | `macos-15` | (release build only) | Cross-compiled release artifact; native tests via universal macos-latest runner |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | full workspace | Static binary; stable/beta/nightly matrix; verified static linking |

### Tier 3 - cross-compiled, limited testing

Release artifacts are produced via `cross`. No native test execution in
CI. Correctness inherited from Tier 1 test coverage on the same source.

| Target | Notes |
|--------|-------|
| `aarch64-unknown-linux-gnu` | Cross-compiled on ubuntu-latest via `cross`; NEON SIMD paths validated by scalar-parity unit tests on x86_64 |
| `aarch64-unknown-linux-musl` | Static musl binary; same cross-compilation story |

---

## I/O acceleration matrix

oc-rsync uses platform-specific I/O fast paths where available, with
automatic fallback to standard buffered I/O. The `fast_io` crate owns all
platform-specific I/O dispatch.

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| **io_uring** (batched async I/O) | Yes - kernel 5.6+, feature-gated (`io_uring`) | No (stub) | No (stub) |
| **SQPOLL** (kernel-side submission polling) | Yes - kernel 5.11+, requires `CAP_SYS_NICE` or `IORING_SETUP_SQPOLL` rights | No | No |
| **SEND_ZC** (zero-copy socket send) | Yes - kernel 6.0+, feature-gated (`iouring-send-zc`) | No | No |
| **IOCP** (I/O Completion Ports) | No (stub) | No (stub) | Yes - feature-gated (`iocp`), default-enabled |
| **kqueue** (event-driven I/O) | No (stub) | Yes - used for readiness-driven file/socket events | No (stub) |
| **copy_file_range** (in-kernel file copy) | Yes - kernel 4.5+ same-fs, 5.3+ cross-fs | No | No |
| **sendfile** (file-to-socket zero-copy) | Yes | No | No |
| **splice** (socket-to-file zero-copy) | Yes - kernel 2.6.17+ | No | No |
| **vmsplice** (user-to-pipe zero-copy) | Yes | No | No |
| **F_NOCACHE + writev** (cache bypass, scatter-gather) | No | Yes - files above 1 MB threshold | No |
| **CopyFileExW** (OS-level file copy) | No | No | Yes |
| **FICLONE / reflink** (copy-on-write clone) | Yes - Btrfs, XFS, bcachefs | Yes - via `clonefile(2)` on APFS | Yes - via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS |
| **Landlock LSM** (filesystem sandboxing) | Yes - kernel 5.13+, feature-gated (`landlock`) | No | No |
| **Sparse file support** | Yes - `SEEK_HOLE`/`SEEK_DATA` | Yes - `SEEK_HOLE`/`SEEK_DATA` | Yes - `FSCTL_SET_ZERO_DATA` |
| **mmap reader** | Yes | Yes | Yes (with fallback for locked pages) |

### io_uring kernel version tiers

CI validates io_uring behaviour across kernel versions:

| Kernel | Available Operations | CI Runner |
|--------|---------------------|-----------|
| < 5.6 | None (fallback to standard I/O) | Not directly testable in CI |
| 5.6 - 5.10 | Basic ring, read/write, statx | Approximated via ubuntu-22.04 (5.15) |
| 5.11 - 5.15 | Above + SQPOLL, registered buffers | `ubuntu-22.04` |
| 6.0+ | Above + SEND_ZC, PBUF_RING, full perf tier | `ubuntu-24.04` |

---

## Metadata and permissions matrix

Platform-specific metadata handling lives in the `metadata` crate. The
`apple-fs` crate adds macOS-specific AppleDouble and resource fork support.

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| **POSIX permissions** (`-p`) | Full | Full | Partial - read-only bit mapped; SUID/SGID/sticky ignored |
| **Owner/group** (`-o`, `-g`) | Full (uid/gid via NSS) | Full (uid/gid via Directory Services) | Partial - SID-based via `LookupAccountNameW` with `-A` |
| **Timestamps** (`-t`) | Full (ns resolution) | Full (ns resolution) | Full (100 ns NTFS resolution) |
| **Symlinks** (`-l`) | Full | Full | Partial - requires `SeCreateSymbolicLinkPrivilege` or Developer Mode |
| **Hard links** (`-H`) | Full | Full | Full (same NTFS volume) |
| **POSIX ACLs** (`-A`) | Full via `exacl` | NFSv4 extended ACLs via `exacl` | Mapped to NTFS DACLs |
| **Extended attributes** (`-X`) | Full (`user.*`, `trusted.*`, `security.*`) | Full (flat namespace, `com.apple.*`) | Partial - mapped to NTFS Alternate Data Streams |
| **Devices/FIFOs** (`-D`, `--specials`) | Full | Full | Unsupported (no-op) |
| **Resource forks** | N/A | Full via `apple-fs` crate | N/A |
| **`--fake-super`** | Full | Full | Unsupported |

---

## SIMD acceleration

The `checksums` crate provides SIMD-accelerated rolling and strong checksum
implementations with runtime feature detection cached in a `OnceLock`.

| Architecture | AVX2 | SSE2 | NEON | Scalar fallback |
|--------------|------|------|------|-----------------|
| x86_64 (Linux, macOS, Windows) | Yes | Yes | No | Yes |
| aarch64 (Linux, macOS) | No | No | Yes | Yes |

All SIMD implementations have mandatory parity tests against the scalar
reference to guarantee correctness across code paths.

---

## CI coverage summary

### Required checks (block merge)

| Check | Platform | Scope |
|-------|----------|-------|
| fmt + clippy | Linux | Workspace-wide |
| nextest (stable) | Linux | Full workspace, all features |
| Windows (stable) | Windows | core, engine, cli |
| macOS (stable) | macOS | core, engine, cli, metadata, apple-fs |
| Linux musl (stable) | Linux | Full workspace, static binary |
| interop | Linux | Upstream rsync 3.0.9 - 3.4.3, daemon + SSH |
| interop (macOS) | macOS | Smoke harness against Homebrew rsync |

### Informational checks (do not block merge)

| Check | Platform | Scope |
|-------|----------|-------|
| nextest (beta/nightly) | Linux | Full workspace |
| Windows (beta/nightly) | Windows | core, engine, cli |
| macOS (beta/nightly) | macOS | core, engine, cli, metadata, apple-fs |
| Linux musl (beta/nightly) | Linux | Full workspace |
| Windows IOCP | Windows | fast_io, transfer (explicit `--features iocp`) |
| Windows ACL/xattr | Windows | metadata (explicit `--features acl,xattr`) |
| Windows GNU cross-check | Linux (cross) | Compilation check for `x86_64-pc-windows-gnu` |
| interop (Windows) | Windows | Best-effort smoke against MSYS2 rsync |
| io_uring kernel compat | Linux | Fallback behaviour on 5.15 and 6.8 kernels |
| DG-3 stress | Linux, Windows, macOS | 1000-thread concurrency stress |
| coverage | Linux | `cargo-llvm-cov`, informational threshold |

---

## Performance expectations

Performance targets are measured against upstream rsync C on equivalent
hardware and workloads. Benchmarks run on Linux (primary) and macOS
(best-effort regression sniff).

| Mode | Target vs Upstream | Platform | Notes |
|------|--------------------|----------|-------|
| Local copy | 3x+ faster | Linux | io_uring, copy_file_range, rayon parallelism |
| Local copy | 2x+ faster | macOS | F_NOCACHE, clonefile, rayon parallelism |
| Local copy | 1.5x+ faster | Windows | IOCP, CopyFileExW, rayon parallelism |
| Daemon transfer | 2x+ faster | Linux | Multiplexed I/O, splice, zero-copy paths |
| SSH transfer | On par to 1.2x | All | Wire protocol bound; XXH3 negotiation helps on aarch64 |

### Known performance gaps

- **RSS overhead**: approximately 2.6x upstream at 1M files without
  incremental recursion. Tracked for arena allocator migration.
- **io_uring at small scale**: marginal benefit at < 200 MB corpus;
  payoff materializes at multi-GB / high-IOPS workloads.
- **Windows IOCP**: not hardware-profiled on physical NTFS; numbers are
  from CI virtualized runners only.
- **aarch64 without checksum negotiation**: SSH transfers fall back to
  software MD5 without the `-e.LsfxCIvu` capability string, costing
  approximately 34% CPU on ARM.

---

## Known platform limitations

### Windows

- No daemon mode (`--daemon` is refused on Windows).
- No `--fake-super` or `--copy-as`.
- No FIFOs, sockets, or device special files.
- Symlinks require elevated privileges or Developer Mode.
- `--usermap` / `--groupmap` / `--chown` unsupported (no POSIX-to-SID mapping).
- IOCP path not validated on physical NTFS hardware.
- Interop tests are best-effort (not a merge gate).
- `--list-only` path rendering may differ from Cygwin rsync.

### macOS

- No xattr/ACL interop coverage in CI (differs from Linux semantics).
- No daemon mode testing in CI (no privileged port listener on runners).
- No SSH loopback testing in CI (sshd not enabled on macOS runners).
- Benchmark numbers are informational only (high runner variance).
- `crtime` (creation time) is not exposed on native macOS.

### Linux musl

- No io_uring in default musl builds (feature not included in musl CI feature set).
- ACL support requires Alpine-packaged `libacl` (provided in release cross images).
- Static binary size is larger than glibc-linked equivalent.

### aarch64 (all platforms)

- No native CI test execution - correctness relies on Tier 1 x86_64 coverage.
- NEON SIMD validated via scalar-parity tests, not native ARM execution in CI.
- Performance numbers are from containerized or cross-compiled environments.

---

## Release artifacts

Each tagged release produces artifacts for all supported platforms:

| Platform | Format | Toolchains |
|----------|--------|------------|
| Linux x86_64 (glibc) | `.deb`, `.rpm`, tarball | stable, beta, nightly |
| Linux aarch64 (glibc) | `.deb`, `.rpm`, tarball | stable, beta, nightly |
| Linux x86_64 (musl) | tarball (static), `.apk` | stable, beta, nightly |
| Linux aarch64 (musl) | tarball (static), `.apk` | stable, beta, nightly |
| macOS x86_64 | tarball | stable, beta, nightly |
| macOS aarch64 | tarball | stable, beta, nightly |
| Windows x86_64 | tarball, `.zip` | stable, beta, nightly |

A multi-arch Docker image is also published to GHCR on each release.
