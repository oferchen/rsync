## oc-rsync {{VERSION}}

Wire-compatible with upstream rsync 3.4.1 (protocol 32).

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

### Security

See `SECURITY.md` for the canonical mitigation roster and `docs/design/sec-1-completion-summary-2026-05-22.md` for the SEC-1 sub-task (`.a`-`.p`) ship state. Release authors: copy the rows that changed this cycle from the `SECURITY.md` CVE table into the placeholder below; drop unaffected rows.

**Active mitigations**

| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-XXXX-XXXXX | _short upstream description_ | _Fixed / Mostly fixed / Mitigated / Not vulnerable_ | PR #NNNN, `SECURITY.md` |

Status values mirror `SECURITY.md` ("Fixed", "Mostly fixed", "Mitigated", "Not vulnerable"). Use "Mostly fixed" when the primary syscall surface is migrated but deferred callers are tracked separately.

**Defense-in-depth.** This release ships the SEC-1 `*at` syscall chain (`fstatat`, `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `fchmodat`, `fchownat`, `utimensat`, `renameat`) routed through a per-transfer `DirSandbox` carrier with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime detection. The optional `landlock` Cargo feature (Linux 5.13+) layers kernel-enforced `PathBeneath` allowlisting on top; daemons on older kernels run with the `*at` chain as the sole TOCTOU defense, which is itself sufficient against CVE-2026-29518 / CVE-2026-43619.

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

---
