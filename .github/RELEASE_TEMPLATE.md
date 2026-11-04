## oc-rsync {{VERSION}}

Wire-compatible with upstream rsync 3.5.1 (protocol 33).

### Install

**Homebrew:**
```bash
brew install oferchen/rsync/oc-rsync
```

**Binary:** Download the asset for your platform below.

| Platform | Formats |
|----------|---------|
| Linux (x86_64, aarch64) | `.deb`, `.rpm` (with OpenSSL), static musl `.tar.gz`, `*-openssl.tar.gz` |
| Alpine Linux (x86_64, aarch64) | `.apk` (stable toolchain only) |
| macOS (x86_64, aarch64) | `.tar.gz` |
| Windows (x86_64) | `.tar.gz`, `.zip` |

Linux static tarballs: `*-musl.tar.gz` (pure Rust) or `*-musl-openssl.tar.gz` (OpenSSL-accelerated checksums).

### Toolchain variants

Each release ships three Rust-toolchain variants of every binary:

| Suffix | Rust toolchain | Recommended use |
|--------|----------------|-----------------|
| _(none)_ | stable | Default. Use this. |
| `-beta` | Rust beta | Early adopters validating the next Rust release. |
| `-nightly` | Rust nightly | Performance experiments and unstable-feature testing. |

The `-beta` / `-nightly` suffix denotes the **Rust toolchain** the artifact was built with - it is **not** an indicator of oc-rsync's own release maturity (that is set in the release title and `README.md`).

---

### Highlights

See the [CHANGELOG](https://github.com/oferchen/rsync/blob/v{{VERSION}}/CHANGELOG.md) for the full, per-category list of changes in this release.

<!-- Optional per-release: add 1-2 sentence summaries per category above the
     CHANGELOG link. Delete any category with no changes. Keep the CHANGELOG
     link so the Highlights section is never empty.
**Daemon features.** e.g. new directives, auth changes, config parsing
**Transfer options.** e.g. new CLI flags, option wiring, batch mode
**Performance.** e.g. hot-path optimizations, memory reductions, I/O changes
**Bug fixes.** e.g. count + notable fixes
**Testing.** e.g. new test coverage, interop additions
-->

<!-- Note: the release workflow sets generate_release_notes=false to avoid
     overflowing GitHub's 125000-char release-body limit on large releases;
     the CHANGELOG link above is the canonical full change list. -->



---

### Security

The Security section of the [CHANGELOG](https://github.com/oferchen/rsync/blob/v{{VERSION}}/CHANGELOG.md) opens with a CVE index: each CVE's status and the PRs that fixed it. [`SECURITY.md`](https://github.com/oferchen/rsync/blob/v{{VERSION}}/SECURITY.md) has the evidence for each CVE.

<!-- Per release: one line with the counts from the CHANGELOG CVE index,
     e.g. "Of the 33 rsync 3.5.0 CVEs, 27 are fixed, 1 is partially fixed
     and 5 do not apply." Drop this comment. -->

**Defense in depth.** Every path a peer names is resolved one component at a time beneath a held directory descriptor (`openat2(RESOLVE_BENEATH)` where the kernel has it, the `*at` syscall family elsewhere), so a symlink swapped in mid-transfer cannot redirect a read or write. On Linux 5.13+ the daemon and `--server` processes also run under Landlock; the opt-in `daemon-seccomp` filter adds a syscall allowlist.

### Platform support tiers

| Platform | Tier | Notes |
|---|---|---|
| Linux x86_64 / aarch64 | **Tier 1** | Full `io_uring` + `splice` + `vmsplice` + Landlock + seccomp. Every required CI cell runs the full nextest workspace. |
| macOS x86_64 / aarch64 | **Tier 1** | `kqueue` + `sendfile` + `clonefile`. Every required CI cell runs the full nextest workspace. |
| Windows x86_64 | **Tier 2** | IOCP file and socket I/O, `TransmitFile`, ReFS reflink, `CopyFileExW`. `splice` / `vmsplice` / `io_uring` are Linux-only and intentionally not implemented; the IOCP receive path is faster than the upstream Cygwin `read`/`write` fallback. Required CI cells test the `core`, `engine`, and `cli` crates. See [Windows support matrix](https://github.com/oferchen/rsync/blob/master/docs/user/windows-support-matrix.md) and the [Windows Tier 2 stub inventory](https://github.com/oferchen/rsync/blob/master/docs/audits/win-tier2-stub-inventory.md). |

Tier 2 is a deliberate choice (no Win32 equivalent for `splice` / `vmsplice` / `io_uring`), not a defect. The IOCP backend matches or beats the upstream Cygwin baseline on the same workload.

### Kernel / platform compatibility

| Layer | Minimum | Notes |
|-------|---------|-------|
| io_uring data path | Linux 5.6+ | Runtime-probed; falls back to standard buffered I/O. |
| Provided buffer rings (PBUF_RING) | Linux 5.19+ | Runtime-probed; falls back to standard buffered I/O. |
| Landlock LSM (`landlock` feature) | Linux 5.13+ | Best-effort ABI downgrade; no-op on older kernels. |
| `IORING_OP_SEND_ZC` (`iouring-send-zc` feature) | Linux 6.0+ | Compiled in by default; dispatched only under an explicit `--zero-copy`, runtime-probed. |
| macOS `*at` chain | macOS 10.10+ | BSD `*at` family verified at parity with Linux (SEC-1.k). |
| Windows IOCP | Windows 10 1809+ | NTFS handle-based APIs sidestep path TOCTOU structurally (SEC-1.l). |

Distro packagers: see `docs/packaging/landlock-feature-guidance.md` for the per-distro `--features landlock` recommendation, runtime ABI matrix, and build-time dependency notes.

### QUIC transport

QUIC (`quic://`, `--quic`) is in the default build on Linux, macOS and Windows and is used only when a transfer asks for it. Both ends must be oc-rsync. A daemon listens on UDP only when `quic cert file` and `quic key file` are set, and the client trusts only the CA named by `--quic-ca`. Windows builds run the QUIC client only. See [`docs/quic-transport.md`](https://github.com/oferchen/rsync/blob/v{{VERSION}}/docs/quic-transport.md).

### Linux io_uring requirements

oc-rsync uses Linux io_uring for high-throughput I/O on supported kernels. The minimum supported kernel for the io_uring path is **Linux 5.6**; older kernels (including RHEL 8 / CentOS Stream 8 at 4.18) fall back to standard `read(2)` / `write(2)` automatically - no user action required.

For the full performance tier (zero-copy `SEND_ZC`), Linux **6.0 or newer** is required. Default release builds include the `iouring-send-zc` cargo feature, but dispatch `SEND_ZC` on daemon socket sends only when the client passes `--zero-copy` and the kernel advertises the opcode; the default path keeps plain socket writes. `--io-uring-status` reports whether the opcode is available.

Full per-opcode kernel-floor matrix: see `docs/audit/iouring-opcode-kernel-floor.md`.

### Supported rsync protocol versions

oc-rsync supports protocol versions 28-33 inclusive, matching upstream rsync 2.6.x through 3.5.1. A peer that advertises a newer protocol is negotiated down to 33. Native rsync 2.6.3 (protocol 28) and 2.6.9 (protocol 29) peers gate the interop matrix on every pull request, alongside 3.0.9, 3.1.3, 3.4.1, 3.4.4, 3.5.0 and 3.5.1. Protocols `<= 27` (rsync 2.5.x and earlier) remain unsupported. See `docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md` for the decision rationale and `docs/design/rp28-k-2-execution-record.md` for the execution record.

---

**Full changelog:** https://github.com/oferchen/rsync/compare/v{{PREV_VERSION}}...v{{VERSION}}
