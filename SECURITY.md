# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.5.x   | :white_check_mark: |
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

The only crates with unsafe code:
- `engine` - Conditional unsafe for ACL support (`#![cfg_attr(not(feature = "acl"), deny(unsafe_code))]`)
- `windows-gnu-eh` - Required for Windows FFI (properly documented)

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
