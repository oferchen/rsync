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
