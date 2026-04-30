# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.6.x   | :white_check_mark: |
| 0.5.x   | :warning: critical fixes only |
| < 0.5   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in oc-rsync, please report it responsibly:

1. **Do not** open a public GitHub issue for security vulnerabilities
2. **Email** the maintainer directly at: skewers.irises.3b@icloud.com
3. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact assessment
   - Any suggested fixes (optional)

You can expect:
- Initial acknowledgment within 48 hours
- Regular updates on the fix progress
- Credit in the security advisory (unless you prefer anonymity)

## Security Design

oc-rsync leverages Rust's memory safety to eliminate entire vulnerability classes:

### Memory Safety Guarantees

- **No buffer overflows**: Rust's bounds checking prevents out-of-bounds memory access
- **No use-after-free**: Rust's ownership system prevents dangling pointer access
- **No uninitialized memory**: All memory must be initialized before use
- **No data races**: Rust's type system prevents concurrent memory access bugs

### Unsafe Code Policy

Protocol-handling crates enforce `#![deny(unsafe_code)]`:
- `protocol` - Wire format parsing
- `batch` - Batch file format
- `signature` - File signatures
- `matching` - Delta generation

Crates with targeted unsafe code:
- `checksums` - SIMD intrinsics for MD4/MD5 and rolling checksums (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM), with scalar fallbacks and parity tests
- `fast_io` - Platform I/O syscalls (sendfile, io_uring, mmap, copy_file_range), with standard I/O fallbacks
- `metadata` - Ownership/privilege FFI (UID/GID lookup via getpwuid_r/getgrnam_r, setuid/setgid, setattrlist), with `#![deny(unsafe_code)]` at crate level and targeted `#[allow(unsafe_code)]` per module
- `flist` - Batched stat syscalls (fstatat, statx) for file list generation
- `engine` - Denies unsafe outside tests (`#![cfg_attr(not(test), deny(unsafe_code))]`) with targeted `#[allow(unsafe_code)]` on platform FFI functions (prefetch, buffer pool, CopyFileExW)
- `windows-gnu-eh` - Windows GNU exception handling shims (properly documented)

**Note:** OS-level race conditions (TOCTOU) remain possible at filesystem boundaries; Rust's memory safety does not prevent them.

## CVE Monitoring

### Upstream rsync CVEs

oc-rsync monitors upstream rsync CVEs to verify continued non-applicability. Recent CVEs and our status:

| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2024-12084 | Heap overflow in checksum parsing | Not vulnerable | Rust Vec<u8> handles dynamic sizing |
| CVE-2024-12085 | Uninitialized stack buffer leak | Not vulnerable | Rust requires initialization |
| CVE-2024-12086 | Server leaks client files | Not vulnerable | Strict path validation |
| CVE-2024-12087 | Path traversal via --inc-recursive | Not vulnerable | Path sanitization |
| CVE-2024-12088 | --safe-links bypass | Mitigated | Rust path handling |
| CVE-2024-12747 | Symlink race condition | Mitigated | TOCTOU is OS-level |

### Monitoring Process

1. **Subscribe to rsync-announce**: https://lists.samba.org/mailman/listinfo/rsync-announce
2. **Monitor NVD**: https://nvd.nist.gov/vuln/search?query=rsync
3. **GitHub Security Advisories**: Watch this repository for security advisories

### When New CVEs Are Published

For each new upstream rsync CVE:
1. Analyze the root cause (memory corruption, logic error, etc.)
2. Check if oc-rsync has equivalent code paths
3. Verify Rust's safety guarantees apply
4. Document the analysis in this file
5. If vulnerable, issue a security advisory and patch

## Fuzzing

The protocol crate includes cargo-fuzz targets for security-critical parsing:

```bash
cd crates/protocol/fuzz
cargo +nightly fuzz run fuzz_varint
cargo +nightly fuzz run fuzz_delta
cargo +nightly fuzz run fuzz_multiplex_frame
cargo +nightly fuzz run fuzz_legacy_greeting
```

See `crates/protocol/fuzz/README.md` for detailed fuzzing instructions.

## Hardening Notes

These cover operationally relevant trade-offs in the current code base and how to mitigate them.

### Buffer pool bounds checks

`recycle_buffer(buf_id)` in the io_uring path validates that `buf_id` falls within the registered buffer pool using `debug_assert!`. In release builds the assertion compiles out, so a corrupted or attacker-influenced `buf_id` reaching this code path would index out of range and either produce undefined behaviour inside the io_uring crate or — more likely — be caught by the kernel as an invalid SQE. A defense-in-depth fix to upgrade the check to a release-mode bound is tracked; until it lands, do not expose the io_uring path to untrusted protocol input.

### io_uring buffer-group ID namespace

io_uring buffer-group IDs (`bgid`) live in a 16-bit namespace. The provided-buffer ring helpers in `fast_io` cap allocation at this bound, and exhaustion returns an error rather than wrapping. Long-running daemons that churn ring groups should monitor for the bounded error and recycle.

### SSH double compression

If the SSH transport itself compresses the stream (`Compression yes` in `ssh_config` or a cipher with built-in compression), running `oc-rsync -z` will compress payloads twice. The amplification surface is small in practice but adds CPU and can mask compressor-specific bugs. Disable one layer; the canonical choice is to leave compression to rsync (`-z` / `--compress`) and disable it in SSH.

### Daemon TLS

`oc-rsync --daemon` does not terminate TLS natively. To expose the daemon over an untrusted network, deploy it behind one of:

- **stunnel** in front of `rsync://`-style daemon traffic
- **SSH tunnel** (`ssh -L` to a localhost-bound daemon)
- **A reverse proxy** that performs TLS termination (e.g., HAProxy in TCP mode)

Bind the daemon itself to `127.0.0.1` (or a private VPC interface) and route external clients exclusively through the TLS terminator.

See `docs/deployment/daemon-tls.md` for runnable stunnel / SSH tunnel / HAProxy recipes.

### Daemon module hardening

In addition to `use chroot = yes`, prefer:

- `numeric ids = yes` so uid/gid mapping does not depend on the daemon's `passwd`/`group`
- `refuse options = delete *` for read-only mirrors
- `hosts allow` / `hosts deny` ACLs at the daemon layer (these run before authentication)
- `secrets file` permissions of `0600`, owned by the daemon user only

## Security Best Practices for Users

### Daemon Mode

When running `oc-rsync --daemon`:

1. **Use chroot**: Configure `use chroot = yes` in rsyncd.conf
2. **Restrict modules**: Only expose necessary paths
3. **Authentication**: Use `auth users` and `secrets file` for access control
4. **Network security**: Run behind a firewall, use SSH tunneling for remote access
5. **Read-only modules**: Use `read only = yes` where possible

### Client Mode

1. **Verify server identity**: Use SSH for transport when possible
2. **Careful with --delete**: Ensure you're syncing to the intended destination
3. **Review exclude patterns**: Avoid accidentally transferring sensitive files

## Acknowledgments

Security researchers who have contributed to oc-rsync's security:
- (Your name could be here - report responsibly!)
