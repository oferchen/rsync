# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.6.x   | :white_check_mark: (current: 0.6.4) |
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

Crates that enforce `#![deny(unsafe_code)]` with no allow-listed exceptions in production code:
- `daemon`, `cli`, `core`, `transfer`, `batch`, `filters`, `signature`, `matching`, `bandwidth`, `logging`, `logging-sink`, `branding`, `rsync_io`, `compress`, `apple-fs`, `flist`, `embedding`, `test-support` - business logic, parsers, orchestration, and high-level I/O wrappers. `embedding` carries one `#[cfg(test)]`-only `#[allow(unsafe_code)]` on a `tests::EnvGuard` helper that wraps `std::env::set_var` under a process-wide mutex.

Crates with `#![deny(unsafe_code)]` and targeted `#[allow(unsafe_code)]` for documented FFI/SIMD boundaries:
- `metadata` - Ownership and privilege FFI (UID/GID lookup via `getpwuid_r`/`getgrnam_r`, `setuid`/`setgid`, `setattrlist`)
- `protocol` - One isolated `#[allow]` in `multiplex::helpers` for performance-critical frame parsing
- `engine` - Denies unsafe outside tests (`#![cfg_attr(not(test), deny(unsafe_code))]`) with targeted `#[allow(unsafe_code)]` on platform FFI (prefetch, buffer pool, `CopyFileExW`)
- `platform` - Daemonization, name resolution (`getpwnam_r`/`getgrnam_r`), privilege transitions (`setuid`/`setgid`/`initgroups`), and chroot syscalls. Signal-handler installation has been hoisted out into `fast_io::signal::install_signal_handler`; the handlers themselves are defined in `core::signal` under a plain `#![deny(unsafe_code)]`.
- `checksums` - SIMD intrinsics for MD4/MD5 and rolling checksums (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM), with scalar fallbacks and parity tests
- `fast_io` - Platform I/O syscalls (sendfile, io_uring, mmap, `copy_file_range`, IOCP, `WSARecv`/`WSASend`, `setsockopt`) and the `signal::install_signal_handler` FFI wrapper, with standard I/O / no-op fallbacks
- `windows-gnu-eh` - Windows GNU exception handling shims (properly documented)

**Long-term direction.** Unsafe code is being consolidated into `fast_io` as the single crate permitted to wrap platform FFI directly; new unsafe code goes there and is exposed via safe public APIs. New `#[allow(unsafe_code)]` annotations in any other crate require explicit review.

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
| CVE-2026-29518 | TOCTOU symlink race in daemon receiver (`use chroot = no`) | **Fixed** | All path-based syscalls have been migrated to `*at` variants routed through `DirSandbox` with `openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` runtime detection (SEC-1.a..q2). Device/FIFO node creation uses `mknodat`/`mkfifoat` (SEC-MK.a..h). A Landlock LSM defense-in-depth layer (SEC-1.p, PR #4702) allowlists `module.path` on the daemon receiver via Landlock 0.4 (v1/v2/v3 best-effort, kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43617 / GHSA-rjfm-3w2m-jf4f | Reverse-DNS lookup after daemon chroot causes hostname ACL bypass | **Fixed** | Hostname resolution runs before chroot at two levels: session-level `resolve_peer_hostname()` in `session_runtime.rs::handle_session` (at accept time, before any module selection), and module-level `module_peer_hostname()` in `module_access::request.rs::respond_with_module_request` / `listing.rs::respond_with_module_list` (during ACL evaluation). Per-module chroot is applied later in `transfer.rs::apply_privilege_restrictions_with_upstream_errors` (after auth and argument reading). The `daemon chroot` global directive is applied once at startup in `accept_loop.rs::serve_connections` (before the accept loop), so per-connection DNS post-chroot can fail when the chroot lacks NSS configuration. To close that path without depending on the chroot containing `/etc/resolv.conf`/`/etc/nsswitch.conf`/`/etc/hosts`/NSS shared objects, `ModuleDefinition::permits` fails closed when reverse DNS returns `None` and any `hosts deny` rule is hostname-based - an attacker who controls their PTR record (or simply blackholes reverse DNS) cannot bypass a hostname-pattern deny rule. Regression tests: `module_peer_hostname_resolution_before_chroot_denies_unknown` (allow-side fail-closed), `module_hostname_deny_fails_closed_when_dns_unresolved` (GHSA scenario A: deny-side fail-closed under daemon chroot), `module_ip_deny_unaffected_by_dns_failure` (scope guard: IP-only rules retain their original semantics). |
| CVE-2026-43618 | Integer overflow in compressed-token decoder causes memory disclosure | **Fixed** | The upstream C vulnerability uses a multiplexed signed-integer return from `recv_deflated_token()` where a negative `rx_token` is misinterpreted as a literal length, leaking memory via a stale `*data` pointer. oc-rsync's decoder returns a typed `CompressedToken` enum (`Literal(Vec<u8>)` / `BlockMatch(u32)` / `End`), structurally eliminating the return-value misinterpretation vector. The residual risk - a malicious sender injecting a negative absolute token via `TOKEN_LONG` that wraps to a valid-looking block index after `as u32` cast - is closed by an explicit sign-check (`if self.rx_token < 0 { return Err(...) }`) in all three wire decoders: `zlib_codec.rs:419`, `zstd_codec.rs:460`, `lz4_codec.rs:346`. Regression tests: `zlib_decoder_rejects_negative_absolute_token`, `zstd_decoder_rejects_negative_absolute_token`, `lz4_decoder_rejects_negative_absolute_token`, `zlib_decoder_rejects_i32_min_token` in `crates/protocol/src/wire/compressed_token/tests.rs`. Audit doc: `docs/audits/upstream-3.4.2-token-decoder-parity.md`. |
| CVE-2026-43619 | Symlink races on chmod/lchown/utimes/rename/unlink/mkdir/symlink/mknod/link/rmdir/lstat | **Fixed** | Same root cause as CVE-2026-29518. All `*at` helpers shipped and receiver call sites fully wired: `lstat` / `unlink` / `rmdir` / `mkdir` / `symlink` / `link` migrated to `fstatat` / `unlinkat` / `mkdirat` / `symlinkat` / `linkat`; `chmod` / `lchown` / `utimes` migrated to `fchmodat` / `fchownat` / `utimensat` (SEC-1.i, PR #4690); `rename` migrated to `renameat` (SEC-1.j, PR #4693); `mknod` / `mkfifo` migrated to `mknodat` / `mkfifoat` (SEC-MK.a..h). Receiver wiring for SEC-1.i/j helpers completed (SEC-1.q/q2). The `recursive_unlinkat` helper shipped (SEC-1.s). The SEC-1.p Landlock LSM defense-in-depth layer (PR #4702) confines the daemon receiver to the configured `module.path` via Landlock 0.4 (kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43620 | OOB read in `recv_files` via negative `parent_ndx` → client SIGSEGV | Not vulnerable | oc-rsync consumes the parent reference as `Option<usize>` and indexes into a bounds-checked `Vec` (`crates/protocol/src/flist/dir_tree.rs`). The validating entry point `DirectoryTree::try_add_directory` returns `DirTreeError::OutOfBoundsParent` on a malformed wire index; the unchecked `add_directory` aborts via Rust's bounds-check panic. Regression coverage: `try_add_directory_rejects_out_of_range_parent_idx`, `try_add_directory_rejects_boundary_off_by_one`, `add_directory_panics_safely_on_oob_parent_idx` in `crates/protocol/src/flist/dir_tree.rs`. SEC-4 closed. |
| CVE-2026-45232 | Off-by-one stack write in HTTP CONNECT proxy response handler | **Fixed** | `read_proxy_line()` in `crates/core/src/client/module_list/connect/proxy.rs` reads byte-by-byte into a heap `Vec<u8>` and explicitly caps the response line at `MAX_PROXY_LINE_BYTES = 1023` bytes, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer (socket.c:86). The C off-by-one stack-write is structurally impossible (bounds-checked `Vec::push`), and indefinite buffering is bounded by the explicit cap. Audit: SEC-2.a (PR #4609); upstream-parity alignment SEC-2.b (PR #4812). |

### Upstream rsync 3.4.3 audits (2026-05-20)

rsync 3.4.3 (released 2026-05-20) is a major security release closing six CVEs and a defense-in-depth batch. Per-CVE applicability is captured in the table above (CVE-2026-29518 / 43617 / 43618 / 43619 / 43620 / 45232). The defense-in-depth items were audited as follows:

- **Bounded wire-supplied counts and lengths** in flist/io/acls/xattrs - oc-rsync already validates these at decode (`crates/protocol/src/flist/read/`, `xattr/cache.rs:123,141`, `acl/`). Re-audit confirmed no path accepts an unbounded length without a `MAX_*` ceiling.
- **Length-underflow guard in cumulative `snprintf()` callers** - oc-rsync uses `format!()`/`write!()` which do not underflow; the equivalent risk is `usize` subtraction, audited cleanly.
- **Parent block-index bounds check on receiver** - addressed by CVE-2026-43620 entry above.
- **NULL check in `read_delay_line()`** - oc-rsync uses `Option<&str>` so the C null-dereference is impossible.
- **Lower ceiling on `MAX_WIRE_DEL_STAT`** - audit confirmed our delete-stats reader (`crates/protocol/src/flist/delete_stats.rs` and surrounding) uses bounded `u32` varints capped well below the upstream lowered ceiling.
- **Reject hyphen-prefixed remote-shell hostnames** - tracked under SEC-3 (`crates/rsync_io/src/ssh/operand.rs` + `parse.rs` already had hostname validation; verify it includes leading-hyphen rejection).
- **NULL-check on `localtime_r()` in `timestring()`** - oc-rsync uses `chrono`/`time` for timestamp formatting; out-of-range timestamps return `Err` rather than dereferencing a null pointer.

Open follow-ups:
- **SEC-1** (TOCTOU on path-based daemon syscalls under `use_chroot=false`) - **Fixed.** Umbrella issue #2516, decomposed into SEC-1.a..s. All `*at` helpers shipped (SEC-1.a..n), receiver call-site wiring completed (SEC-1.q/q2), `DeleteFs` trait sandbox refactor shipped (SEC-1.q), `recursive_unlinkat` helper shipped (SEC-1.s), and `mknodat`/`mkfifoat` migration completed (SEC-MK.a..h). The SEC-1.p Landlock LSM defense-in-depth layer shipped (PR #4702).
- **SEC-2.b** (align proxy-line cap to upstream's ceiling) - **Fixed** (PR #4812). SEC-2.a confirmed the structural mitigation (bounds-checked `Vec::push`); SEC-2.b tightened the numeric cap to `MAX_PROXY_LINE_BYTES = 1023` at `connect/proxy.rs:344`, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer.
- **SEC-3** (confirm hyphen-prefixed hostname rejection in SSH operand parse) - **Fixed.** SEC-3.a audit, SEC-3.b validation, and SEC-3.c regression coverage all completed.
- **SEC-4** (regression test for malformed `parent_node_idx` per CVE-2026-43620 mitigation) - closed. `DirectoryTree::try_add_directory` validates the wire-supplied parent index and returns `DirTreeError::OutOfBoundsParent`; three regression tests in `crates/protocol/src/flist/dir_tree.rs` pin down both the graceful-reject path and the worst-case controlled-panic path (no SIGSEGV).

#### SEC-1 progress (CVE-2026-29518 / CVE-2026-43619)

Shipped:
- **SEC-1.a/b/c/d/e**: `DirSandbox` carrier with in-tree dirfd cache, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime detection, and receiver pipeline wiring (PRs #4643, #4650 and prior).
- **SEC-1.f** (PR #4668): receiver `lstat` / `symlink_metadata` path resolves via `fstatat(AT_SYMLINK_NOFOLLOW)` routed through `DirSandbox`.
- **SEC-1.g** (PR #4671): receiver `remove_file` / `remove_dir` path uses `unlinkat` routed through `DirSandbox`.
- **SEC-1.h** (PR #4683): receiver `mkdir` / `symlink` / `hard_link` creation paths use `mkdirat` / `symlinkat` / `linkat` routed through `DirSandbox`.
- **SEC-1.i** (PR #4690): `fchmodat` / `fchownat` / `utimensat` sandbox helpers replace path-based `chmod` / `lchown` / `utimes`.
- **SEC-1.j** (PR #4693): `renameat` sandbox helper replaces path-based `rename`.
- **SEC-1.k**: macOS verified - the `*at` syscall family is available and behaves consistently with the Linux migration.
- **SEC-1.l**: Windows audited - NTFS handle-based APIs naturally sidestep the TOCTOU window, so Windows is not affected by either CVE.
- **SEC-1.m** (PR #4675): comprehensive symlink-swap attack regression coverage against the daemon receiver.
- **SEC-1.n** (PR #4678): interop regression coverage confirming legitimate symlinks still transfer correctly under the new `*at` paths.
- **SEC-1.p** (PR #4702, shipped 2026-05-22): Landlock LSM defense-in-depth for the daemon receiver. `crates/fast_io/src/landlock.rs` wraps Landlock 0.4 (v1/v2/v3 best-effort, kernel 5.13+); `crates/daemon/src/daemon/sections/module_access/transfer.rs::engage_landlock_sandbox` allowlists `module.path` immediately before the receiver pipeline starts, so any residual unconverted path-based syscall is bounded by a kernel-enforced filesystem allowlist.

Additionally shipped since the last update:
- **SEC-1.q** (DeleteFs trait sandbox refactor): deletion operations route through the `DirSandbox`-backed `DeleteFs` trait.
- **SEC-1.q2** (Receiver-deletion sandbox wiring): all receiver deletion call sites fully wired through `DirSandbox`.
- **SEC-1.s** (`recursive_unlinkat` helper): recursive directory removal uses `unlinkat` throughout, closing the last TOCTOU window in directory tree deletion.
- **SEC-MK.a..h** (`mknodat`/`mkfifoat` sandbox migration): device and FIFO node creation migrated to `*at` variants routed through `DirSandbox`. Previously deferred (closure doc #4694); now complete.
- **SEC-1.p Landlock LSM defense-in-depth** - Linux 5.13+ kernel-side allowlist over the module root, engaged per-connection after `apply_module_privilege_restrictions` returns. Even a future regression that calls a path-based syscall directly (bypassing `DirSandbox`) is rejected by the kernel with `EACCES`. Client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir` / `--compare-dest` / `--copy-dest` / `--link-dest` paths that resolve outside the module root are rejected at the wire-protocol layer (PR #5568, URV-5.b.1); the in-module subset is admitted to the Landlock allowlist alongside the module root so a default-on Landlock posture (URV-5.c.5) does not EACCES legitimate writes (URV-5.b.REOPEN). Best-effort ABI downgrade picks the highest level the running kernel exposes (v3 on 6.2+, v2 on 5.19+, v1 on 5.13+). Stub returns `Unavailable` on non-Linux targets so the SEC-1 `*at` chain remains the sole defense there.
- **SEC-1.t** (receiver pre-flight dest_root symlink refusal): `ensure_dest_root_exists` in `crates/transfer/src/receiver/mod.rs` uses `symlink_metadata()` (lstat) rather than `metadata()` (stat) so a symlink at the destination root is observed directly. The helper refuses with `InvalidInput` for any symlinked dest - broken or pointing at an existing directory, inside or outside the module - because `create_dir_all` against a stat-NotFound result would otherwise resolve through the link and materialize the directory at the symlink target, sidestepping the SEC-1 `*at` chain that protects every subsequent per-entry write. The receiver never auto-creates through a symlink; operators that genuinely need a symlinked dest must materialize the real directory themselves. Follow-up to PR #5567 which added the pre-flight mkdir path.

**Status: Fixed.** All receiver call sites are wired through `DirSandbox`, and the SEC-1.m / SEC-1.n regression suites pass against the fully-wired pipeline. The SEC-1.p Landlock layer provides defense-in-depth.

CI integration: as of 2026-05-21 the interop job (`.github/workflows/_interop.yml`) runs upstream rsync's own `testsuite/*.test` corpus against oc-rsync as `$RSYNC`, pinned to upstream 3.4.4 by default. The known-failures roster lives at `tools/ci/upstream_testsuite_known_failures.conf`.

### Upstream rsync 3.4.2 audits

In v0.6.2 the codebase was audited against every fix that landed in upstream rsync 3.4.2 (released 2026). The equivalent code paths were verified safe in oc-rsync:

- Compressed-stream negative-token decoder bounds (#2225)
- Xattr `qsort` element-count parity (#2226)
- `clean_fname()` buffer-underflow parity (#2227)
- Allocator zeroing pattern (calloc + realloc-expand) (#2228)
- Y2038 safety in syscall paths (Int32x32To64 equivalent) (#2229)
- ACL ID mapping for non-root users (#2230, closes #618)
- FreeBSD many-xattrs handling parity (#2231)
- "Directory has vanished" error path (#2232)
- Removal of multiple leading slashes (#2233)
- Daemon `chrono::Local` pre-init before `chroot` (#2234)
- `--open-noatime` propagation through sender source-file opens (#2236)
- AVX2 `get_checksum1` `mul_one` uninitialised-regression audit (#2222)
- MD4 `get_checksum2` `buf1` uninitialised-regression audit (#2223)
- SIMD vs scalar self-test that cross-validates AVX2/SSE2/NEON paths at startup (#2224)

### Monitoring Process

1. **Subscribe to rsync-announce**: https://lists.samba.org/mailman/listinfo/rsync-announce
2. **Monitor NVD**: https://nvd.nist.gov/vuln/search?query=rsync
3. **GitHub Security Advisories**: Watch this repository for security advisories
4. **Scheduled CI watcher**: `tools/ci/check_upstream_release.sh` runs weekly via GitHub Actions and opens a tracking issue when a new upstream rsync release ships, so new CVEs are surfaced automatically

### When New CVEs Are Published

For each new upstream rsync CVE:
1. Analyze the root cause (memory corruption, logic error, etc.)
2. Check if oc-rsync has equivalent code paths
3. Verify Rust's safety guarantees apply
4. Document the analysis in this file
5. If vulnerable, issue a security advisory and patch

## Fuzzing

The repo ships 24 cargo-fuzz targets covering security-critical parsing, SIMD parity, and differential fuzzing against upstream rsync:

**Core protocol parsing:**
- `varint_decode` - variable-length integer codec
- `multiplex_frame_parse` - multiplex `MSG_*` frame parsing
- `legacy_greeting` - daemon `@RSYNCD:` greeting parser
- `ndx_codec` - file-list index codec
- `protocol_wire` - generic protocol wire format
- `flist_entry_decode` - file-list entry decoder
- `incremental_flist` - incremental file-list segments
- `capability_flags` (FCV-10) - negotiation prologue capability flags
- `vstring` (FCV-14) - vstring parser
- `auth_response` - daemon authentication response

**Daemon and configuration:**
- `daemon_greeting` - daemon greeting generation
- `rsyncd_conf` (FCV-16) - `rsyncd.conf` line parser
- `batch_reader` (FCV-12) - rsync batch file header
- `bwlimit` (FCV-15) - bwlimit CLI string parser

**Metadata and extensions:**
- `acl_xattr_wire` (FCV-11) - ACL/xattr wire format
- `filter_list_wire` - filter list wire format

**Compression:**
- `decompressor_zlib` - zlib decompression
- `decompressor_zstd` - zstd decompression

**SIMD parity:**
- `simd_checksum_parity` - cross-validates AVX2, SSE2, NEON, and scalar rolling/strong checksum paths against random inputs (see #2103)

**Filter rules:**
- `filter_rules_vs_upstream` - filter rule evaluation vs upstream behavior
- `filter_differential` - differential filter testing

**Differential fuzzing (upstream wire parity):**
- `differential_outcome` (WDF-3) - outcome-based differential fuzzing against upstream rsync
- `differential_multiplex` (WDF-4) - multiplex frame differential fuzzing
- `differential_flist` (WDF-5) - flist wire format differential fuzzing

See `fuzz/README.md` for detailed fuzzing instructions.

## Hardening Notes

These cover operationally relevant trade-offs in the current code base and how to mitigate them.

### Buffer pool bounds checks

`recycle_buffer(buf_id)` in the io_uring path validates that `buf_id` falls within the registered buffer pool using `debug_assert!`. In release builds the assertion compiles out, so a corrupted or attacker-influenced `buf_id` reaching this code path would index out of range and either produce undefined behaviour inside the io_uring crate or — more likely — be caught by the kernel as an invalid SQE. A defense-in-depth fix to upgrade the check to a release-mode bound is tracked; until it lands, do not expose the io_uring path to untrusted protocol input.

### io_uring buffer-group ID namespace

io_uring buffer-group IDs (`bgid`) live in a 16-bit namespace. The provided-buffer ring helpers in `fast_io` cap allocation at this bound, and exhaustion returns an error rather than wrapping. Long-running daemons that churn ring groups should monitor for the bounded error and recycle.

### SSH double compression

If the SSH transport itself compresses the stream (`Compression yes` in `ssh_config` or a cipher with built-in compression), running `oc-rsync -z` will compress payloads twice. The amplification surface is small in practice but adds CPU and can mask compressor-specific bugs. Disable one layer; the canonical choice is to leave compression to rsync (`-z` / `--compress`) and disable it in SSH.

### Daemon encryption

The daemon protocol is plaintext, matching upstream rsync: the daemon provides authentication but not encryption. To expose a daemon over an untrusted network, deploy it behind one of:

- **SSH tunnel** (`ssh -L` to a localhost-bound daemon), or use the ssh transport directly
- **stunnel** in front of `rsync://`-style daemon traffic
- **A reverse proxy** that performs TLS termination (e.g., HAProxy in TCP mode, or nginx)

Bind the daemon to `127.0.0.1` (or a private VPC interface) and route external clients exclusively through the TLS terminator. Clients reach an SSL-proxied daemon with `--ssl` (requires `--features client-tls`), matching upstream `rsync-ssl`.

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
