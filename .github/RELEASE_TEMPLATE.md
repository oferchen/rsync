## oc-rsync {{VERSION}}

Wire-compatible with upstream rsync 3.4.4 (protocol 32).

### Install

**Homebrew:**
```bash
brew install oferchen/rsync/oc-rsync
```

**Binary:** Download the asset for your platform below.

| Platform | Formats |
|----------|---------|
| Linux (x86_64, aarch64) | `.deb`, `.rpm` (with OpenSSL), static musl `.tar.gz`, `*-openssl.tar.gz` |
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

See the [CHANGELOG](https://github.com/oferchen/rsync/blob/master/CHANGELOG.md) for the full, per-category list of changes in this release.

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

See `SECURITY.md` for the canonical mitigation roster and `docs/design/sec-1-completion-summary-2026-05-22.md` for the SEC-1 sub-task (`.a`-`.p`) ship state.

<!-- Copy rows that changed this cycle from the SECURITY.md CVE table; drop unaffected rows. -->

**Active mitigations**

| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2026-29518 | Path traversal via symlink race | Fixed | SEC-1.a-q2, `SECURITY.md` |
| CVE-2026-43619 | Privilege escalation via TOCTOU | Fixed | SEC-1.a-q2, `SECURITY.md` |

<!-- Status values: "Fixed", "Mostly fixed", "Mitigated", "Not vulnerable". -->
<!-- Use "Mostly fixed" when the primary syscall surface is migrated but deferred callers are tracked separately. -->

**Defense-in-depth.** This release ships the SEC-1 `*at` syscall chain (`fstatat`, `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `fchmodat`, `fchownat`, `utimensat`, `renameat`) routed through a per-transfer `DirSandbox` carrier with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime detection. The optional `landlock` Cargo feature (Linux 5.13+) layers kernel-enforced `PathBeneath` allowlisting on top; daemons on older kernels run with the `*at` chain as the sole TOCTOU defense, which is itself sufficient against CVE-2026-29518 / CVE-2026-43619.

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
| `IORING_OP_SEND_ZC` (`iouring-send-zc` feature) | Linux 6.0+ recommended | Opt-in; default builds use plain `IORING_OP_SEND`. |
| macOS `*at` chain | macOS 10.10+ | BSD `*at` family verified at parity with Linux (SEC-1.k). |
| Windows IOCP | Windows 10 1809+ | NTFS handle-based APIs sidestep path TOCTOU structurally (SEC-1.l). |

Distro packagers: see `docs/packaging/landlock-feature-guidance.md` for the per-distro `--features landlock` recommendation, runtime ABI matrix, and build-time dependency notes.

### Linux io_uring requirements

oc-rsync uses Linux io_uring for high-throughput I/O on supported kernels. The minimum supported kernel for the io_uring path is **Linux 5.6**; older kernels (including RHEL 8 / CentOS Stream 8 at 4.18) fall back to standard `read(2)` / `write(2)` automatically - no user action required.

For the full performance tier (zero-copy `SEND_ZC`), Linux **6.0 or newer** is required AND the binary must be built with the `iouring-send-zc` cargo feature enabled. Default release builds advertise `--zero-copy` but downgrade to non-zero-copy `SEND` silently on older kernels or builds without the feature.

Full per-opcode kernel-floor matrix: see `docs/audit/iouring-opcode-kernel-floor.md`.

### Supported rsync protocol versions

oc-rsync continues to support protocol versions 28-32 inclusive, matching upstream rsync 2.6.x through 3.4.x. Protocol back-negotiation to 28 is exercised by wire-byte regression tests and a periodic CI matrix against rsync 2.6.9 (built from source). Protocols `<= 27` (rsync 2.5.x and earlier) remain unsupported. See `docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md` for the decision rationale and `docs/design/rp28-k-2-execution-record.md` for the execution record.

---

**Full changelog:** https://github.com/oferchen/rsync/compare/{{PREV_VERSION}}...{{VERSION}}
