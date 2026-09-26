# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.6.x   | :white_check_mark: (current: 0.6.4) |
| 0.5.x   | :warning: critical fixes only |
| < 0.5   | :x:                |

Fixes land on `master` first and ship in the next 0.6.x release. Changes merged since 0.6.4 are listed under *Unreleased* in [`CHANGELOG.md`](./CHANGELOG.md).

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

### Memory safety

Safe Rust rules out out-of-bounds access, use-after-free, reads of uninitialised memory and data races in the code it covers. It does **not** rule out logic flaws: a fail-open access check, an unconfined path or an unbounded peer-supplied count is as reachable in safe Rust as in C. OS-level races at filesystem boundaries (TOCTOU) are also outside what the language prevents, which is why path handling goes through the confined `*at` walk described below.

### Unsafe code policy

Crates that enforce `#![deny(unsafe_code)]` with no allow-listed exceptions in production code:
- `daemon`, `cli`, `core`, `transfer`, `batch`, `filters`, `signature`, `matching`, `bandwidth`, `logging`, `logging-sink`, `branding`, `rsync_io`, `compress`, `apple-fs`, `flist`, `embedding`, `test-support` - business logic, parsers, orchestration, and high-level I/O wrappers. Some of these crates carry `#[allow(unsafe_code)]` inside `#[cfg(test)]` modules only, for example to set environment variables in tests.

Crates with `#![deny(unsafe_code)]` and targeted `#[allow(unsafe_code)]` for documented FFI/SIMD boundaries:
- `metadata` - Ownership and privilege FFI (UID/GID lookup via `getpwuid_r`/`getgrnam_r`, `setuid`/`setgid`, `setattrlist`)
- `protocol` - One isolated `#[allow]` in `multiplex::helpers` for performance-critical frame parsing
- `engine` - Denies unsafe outside tests (`#![cfg_attr(not(test), deny(unsafe_code))]`) with targeted `#[allow(unsafe_code)]` on platform FFI (prefetch, buffer pool, `CopyFileExW`)
- `platform` - Daemonization, per-connection `fork`/`waitpid`, name and group resolution, privilege transitions (`setuid`/`setgid`/`initgroups`), chroot, environment access, local time, signal disposition, and Windows service dispatch
- `checksums` - SIMD intrinsics for MD4/MD5 and rolling checksums (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM), with scalar fallbacks and parity tests
- `fast_io` - Platform I/O syscalls (sendfile, io_uring, mmap, `copy_file_range`, IOCP, `WSARecv`/`WSASend`, `setsockopt`) and the `signal::install_signal_handler` FFI wrapper, with standard I/O / no-op fallbacks
- `windows-gnu-eh` - Windows GNU exception handling shims (properly documented)

**Long-term direction.** Unsafe code is being consolidated into two owning crates: `fast_io` for I/O syscalls and `platform` for process, identity, environment and signals. Both expose safe public APIs. `metadata`, `checksums`, `engine` and `protocol` still hold production unsafe until their sites migrate. New `#[allow(unsafe_code)]` annotations in any other crate require explicit review.

## Upstream CVE Status

| Upstream batch | oc-rsync status |
|---|---|
| 2024 (CVE-2024-12084 to 12088, CVE-2024-12747) | Not vulnerable or mitigated |
| rsync 3.4.2 fixes | Audited in v0.6.2; equivalent paths verified safe |
| rsync 3.4.3 (six CVEs) | Fixed or not vulnerable |
| rsync 3.5.0 (33 CVEs) | 14 ids assessed, some still under audit; 19 untriaged |
| rsync 3.5.1 | No CVE ids in upstream's notes; hardening fixes tracked below |

### 2024 and rsync 3.4.3 CVEs

| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2024-12084 | Heap overflow in checksum parsing | Not vulnerable | Rust Vec<u8> handles dynamic sizing |
| CVE-2024-12085 | Uninitialized stack buffer leak | Not vulnerable | Rust requires initialization |
| CVE-2024-12086 | Server leaks client files | Not vulnerable | Strict path validation |
| CVE-2024-12087 | Path traversal via --inc-recursive | Not vulnerable | Path sanitization |
| CVE-2024-12088 | --safe-links bypass | Mitigated | Rust path handling |
| CVE-2024-12747 | Symlink race condition | Mitigated | TOCTOU is OS-level |
| CVE-2026-29518 | TOCTOU symlink race in daemon receiver (`use chroot = no`) | **Fixed** | All path-based syscalls have been migrated to `*at` variants routed through `DirSandbox` with `openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` runtime detection (SEC-1.a..q2). Device/FIFO node creation uses `mknodat`/`mkfifoat` (SEC-MK.a..h). A Landlock LSM defense-in-depth layer (SEC-1.p, PR #4702) allowlists `module.path` on the daemon receiver via Landlock 0.4 (requests up to ABI v5 with best-effort downgrade; v1 on kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43617 / GHSA-rjfm-3w2m-jf4f | Reverse-DNS lookup after daemon chroot causes hostname ACL bypass | **Fixed** | Hostname resolution runs before chroot at two levels: session-level `resolve_peer_hostname()` in `session_runtime.rs::handle_session` (at accept time, before any module selection), and module-level `module_peer_hostname()` in `module_access::request.rs::respond_with_module_request` / `listing.rs::respond_with_module_list` (during ACL evaluation). Per-module chroot is applied later in `transfer.rs::apply_privilege_restrictions_with_upstream_errors` (after auth and argument reading). The `daemon chroot` global directive is applied once at startup in `accept_loop.rs::serve_connections` (before the accept loop), so per-connection DNS post-chroot can fail when the chroot lacks NSS configuration. To close that path without depending on the chroot containing `/etc/resolv.conf`/`/etc/nsswitch.conf`/`/etc/hosts`/NSS shared objects, `ModuleDefinition::permits` fails closed when reverse DNS returns `None` and any `hosts deny` rule is hostname-based - an attacker who controls their PTR record (or simply blackholes reverse DNS) cannot bypass a hostname-pattern deny rule. Regression tests: `module_peer_hostname_resolution_before_chroot_denies_unknown` (allow-side fail-closed), `module_hostname_deny_fails_closed_when_dns_unresolved` (GHSA scenario A: deny-side fail-closed under daemon chroot), `module_ip_deny_unaffected_by_dns_failure` (scope guard: IP-only rules retain their original semantics). |
| CVE-2026-43618 | Integer overflow in compressed-token decoder causes memory disclosure | **Fixed** | The upstream C vulnerability uses a multiplexed signed-integer return from `recv_deflated_token()` where a negative `rx_token` is misinterpreted as a literal length, leaking memory via a stale `*data` pointer. oc-rsync's decoder returns a typed `CompressedToken` enum (`Literal(Vec<u8>)` / `BlockMatch(u32)` / `End`), structurally eliminating the return-value misinterpretation vector. The residual risk - a malicious sender injecting a negative absolute token via `TOKEN_LONG` that wraps to a valid-looking block index after `as u32` cast - is closed by an explicit sign check in all three wire decoders (zlib, zstd, lz4). Regression tests: `zlib_decoder_rejects_negative_absolute_token`, `zstd_decoder_rejects_negative_absolute_token`, `lz4_decoder_rejects_negative_absolute_token`, `zlib_decoder_rejects_i32_min_token` in `crates/protocol/src/wire/compressed_token/tests.rs`. Audit doc: `docs/audits/upstream-3.4.2-token-decoder-parity.md`. |
| CVE-2026-43619 | Symlink races on chmod/lchown/utimes/rename/unlink/mkdir/symlink/mknod/link/rmdir/lstat | **Fixed** | Same root cause as CVE-2026-29518. All `*at` helpers shipped and receiver call sites fully wired: `lstat` / `unlink` / `rmdir` / `mkdir` / `symlink` / `link` migrated to `fstatat` / `unlinkat` / `mkdirat` / `symlinkat` / `linkat`; `chmod` / `lchown` / `utimes` migrated to `fchmodat` / `fchownat` / `utimensat` (SEC-1.i, PR #4690); `rename` migrated to `renameat` (SEC-1.j, PR #4693); `mknod` / `mkfifo` migrated to `mknodat` / `mkfifoat` (SEC-MK.a..h). Receiver wiring for SEC-1.i/j helpers completed (SEC-1.q/q2). The `recursive_unlinkat` helper shipped (SEC-1.s). The SEC-1.p Landlock LSM defense-in-depth layer (PR #4702) confines the daemon receiver to the configured `module.path` via Landlock 0.4 (kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43620 | OOB read in `recv_files` via negative `parent_ndx` → client SIGSEGV | Not vulnerable | oc-rsync consumes the parent reference as `Option<usize>` and indexes into a bounds-checked `Vec` (`crates/protocol/src/flist/dir_tree.rs`). The validating entry point `DirectoryTree::try_add_directory` returns `DirTreeError::OutOfBoundsParent` on a malformed wire index; the unchecked `add_directory` aborts via Rust's bounds-check panic. Regression coverage: `try_add_directory_rejects_out_of_range_parent_idx`, `try_add_directory_rejects_boundary_off_by_one`, `add_directory_panics_safely_on_oob_parent_idx` in `crates/protocol/src/flist/dir_tree.rs`. SEC-4 closed. |
| CVE-2026-45232 | Off-by-one stack write in HTTP CONNECT proxy response handler | **Fixed** | `read_proxy_line()` in `crates/core/src/client/module_list/connect/proxy.rs` reads byte-by-byte into a heap `Vec<u8>` and explicitly caps the response line at `MAX_PROXY_LINE_BYTES = 1023` bytes, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer (socket.c:86). The C off-by-one stack-write is structurally impossible (bounds-checked `Vec::push`), and indefinite buffering is bounded by the explicit cap. Audit: SEC-2.a (PR #4609); upstream-parity alignment SEC-2.b (PR #4812). |

### Upstream rsync 3.5.0 (13 Aug 2026) - triage in progress

rsync 3.5.0 is a major security release closing **33 CVEs**, concentrated in path handling and the daemon. That figure is upstream's own: "This release fixes 33 security issues found during a focused audit of rsync's..." (`NEWS.md:37` in the 3.5.0 tarball). Unlike the 3.4.2/3.4.3 batches, this set is **not yet fully audited against oc-rsync**, and this section says so plainly rather than implying coverage that does not exist.

**How much is assessed: 14 of 33.** 14 of the 3.5.0 CVE ids appear in a table row below, and some of those rows are still marked "under audit". The other 19 have **no entry yet**. They are neither claimed fixed nor claimed inapplicable:

```
CVE-2026-53785  CVE-2026-53786  CVE-2026-53788  CVE-2026-53789
CVE-2026-53792  CVE-2026-53794  CVE-2026-53796  CVE-2026-53797
CVE-2026-53798  CVE-2026-53799  CVE-2026-53800  CVE-2026-53801
CVE-2026-53802  CVE-2026-53803  CVE-2026-70454  CVE-2026-70457
CVE-2026-70459  CVE-2026-70460  CVE-2026-70462
```

Re-derive the roster from the pinned tarball rather than trusting this list. An id counts as assessed only when it appears in the first cell of a **table row**, not merely in the text, because the roster above mentions every untriaged id:

```sh
sed -n '1,451p' target/interop/upstream-src/rsync-3.5.0/NEWS.md \
  | grep -o 'CVE-2026-[0-9]*' | sort -u > /tmp/batch
grep '^| CVE-2026-' SECURITY.md | cut -d'|' -f2 \
  | grep -oE '[0-9]{5}' | sed 's/^/CVE-2026-/' | sort -u > /tmp/assessed
comm -23 /tmp/batch /tmp/assessed          # ids with no table row
```

Lines 1-451 are upstream's 3.5.0 section. They name 35 ids: upstream's 33 plus two back-references to 3.4.3 fixes (`CVE-2026-43617`, `CVE-2026-43620`), which have their own rows above. The command prints the 19 ids listed here.

**What is established.** 3.5.0 is wire-identical to 3.4.4 (`PROTOCOL_VERSION` 32, `SUBPROTOCOL_VERSION` 0, unchanged `errcode.h`), so none of these CVEs stem from a protocol change and none require a wire-format response. They are implementation vulnerabilities in areas oc-rsync reimplements independently, so neither "inherited" nor "not applicable" can be assumed for any of them; each needs its own evidence.

**What is measured.** Upstream's 3.5.0 test suite runs against oc-rsync on every pull request in eight legs, the four Linux ones as required checks; the per-leg counts and the command that re-derives them are in the [README](./README.md#upstream-testsuite). The testsuite jobs read their own `UPSTREAM_TESTSUITE_VERSION` (default `3.5.0`) rather than the interop matrix's `UPSTREAM_RSYNC_VERSION`, so retargeting interop cannot move the conformance gate. **Nine distinct tests** carry a `fail` row across the eight manifests (`awk '!/^#/ && $2=="fail" {print $1}' tools/ci/upstream-3.5.0-expect.*.txt | sort -u`). Three of them assert 3.5.0 behaviour that 3.5.1 reverted (`--max-alloc=0` and inetd detection, PR #8011). That set is triage input, not a vulnerability count. A fix flips its manifest rows in the same commit, and the gate fails on an unexpected *pass*, so a row cannot be re-baselined without a fix.

**Highest-severity items and their oc-rsync bearing:**

| CVE | Severity | Upstream issue | oc-rsync bearing |
|-----|----------|----------------|------------------|
| CVE-2026-53791 | CRITICAL | `proxy protocol = true` let a directly-connecting client forge a PROXY header and spoof its source address, defeating `hosts allow`/`hosts deny`. Fixed by a new `proxy protocol hosts` allow-list that fails **closed** when unset. | **Fixed.** `proxy protocol hosts` is parsed into a `ProxyProtocolPolicy` that mirrors upstream's `allow_proxy_protocol_peer()` (access.c:311-317): an empty or unset trusted-proxy list makes the policy reject **every** peer rather than accept any, and the daemon warns at startup on the `proxy protocol = true` with no list combination exactly as upstream does (clientserver.c:1771-1772). A PROXY header from a direct peer that is not on the list is refused (PR #7648). The prerequisite was fixed earlier: the daemon's two stdio entry points fabricated `127.0.0.1` as the peer address, which made every `hosts allow` / `hosts deny` rule evaluate against a synthetic localhost. Both now mirror upstream `client_addr()` - `getpeername` under inetd, the `REMOTE_HOST` / `SSH_CONNECTION` / `SSH_CLIENT` / `SSH2_CLIENT` environment chain under a remote shell - and abort with `RERR_SOCKETIO` rather than inventing a value (PR #7303). |
| CVE-2026-70452 | HIGH | `hosts deny` failed **open** when a configured hostname would not resolve. | **Fixed.** The root cause was a missing distinction, not a missing check: `forward_resolve` collapsed "lookup failed" and "resolved but did not match" into one empty result, which is what made the deny side fail open. The resolver now returns `Option<Vec<IpAddr>>` so the two are separable, and hostname matching is case-insensitive against a lowercased list per upstream `iwildmatch` (access.c:57) (PR #7314). |
| CVE-2026-70464 | HIGH | An unauthenticated peer could complete the `@RSYNCD` handshake. | Under audit. The related `auth digest` minimum-digest floor has **shipped** (PR #7350); `Md4Old` must rank as md4 or the floor locks out the very clients it exists to reason about. Related pre-auth hardening: a peer-supplied `@RSYNCD: OPTION` line can no longer override daemon parameters before authentication (PR #7754). |
| CVE-2026-53784 / 53793 | HIGH | Daemon module-root chdir escape under `use chroot`, and a `/./` inner-module escape via a symlinked path. | Under audit. Related and fixed: the daemon fused the operator module root and the peer-supplied tail into one absolute string and applied `RESOLVE_NO_SYMLINKS` to the whole thing. Upstream keeps the two in different mechanisms - plain `chdir`/`openat` for the root, a confined `RESOLVE_BENEATH` walk for the tail - and oc-rsync now does the same (PR #7304). Since then the module identity is resolved before the chroot (PR #7585), each connection is served from its own forked session so per-module chroot state cannot leak across peers, and the daemon-chroot testsuite cells pass on the gating legs. A daemon now enters the peer's destination beneath the module root through a port of upstream's `secure_relative_dirfd()` walk (PR #8010). The symlinked-path `/./` arm remains under audit. |
| CVE-2026-53795 | HIGH | An absolute `--temp-dir` or `--link-dest` **disabled path confinement entirely**. | Directly applicable, and **largely closed**. oc-rsync's confinement was narrower than 3.5.0's and has been widened onto one shared per-component resolver: the staging family (`--temp-dir`, `--partial-dir`, `--backup-dir`), `--log-file`, the alt-dest basis leaf, `--relative` implied parents, and absolute rename endpoints all now resolve through the ownership walk (PRs #7398, #7404, #7415, #7419, #7393). The operator-named **auxiliary file** family followed: the daemon config, motd, secrets and `lock file`, `--password-file`, `--log-file`, `--files-from`, `--early-input`, the `--*clude-from` and merge files, and the batch files all open through the walk, and the alt-dest basis entry is stat-ed through it rather than opened (PRs #7421-#7426, #7439, #7441-#7443, #7459, #7463). `--files-from` entries on a local sender now resolve through the same walk and are refused outside `--confine-root` (PR #8012). The `operator-path-*` testsuite cluster passes in full: no `operator-path-*` row is `fail` in any committed manifest. |
| CVE-2026-53783 | HIGH | `rrsync` restricted-directory escape - each path was validated, but the validation could be walked out of. | oc-rsync ships no restricted-shell wrapper today; one is being added, enforcing through the same path resolver rather than a separate validator. The upstream rule set is extracted as a spec (PR #7283) - note it forces `--drop-D`, not `--no-D`. |
| CVE-2026-53790 | HIGH | Command / argument injection via unquoted peer- or config-supplied values. | **Largely closed.** Upstream has four sinks and three remedies; oc-rsync has two of the same shape, one structurally different, and one that does not apply. The live sink here is oc-rsync's own single-character config expansion, which upstream does not have - so an upstream-shaped port alone would have been inert, and the refusal was applied at that sink instead: shell metacharacters are now rejected when expanding a hook variable for `pre-xfer exec` / `post-xfer exec` (PR #7465), and the `RSYNC_CONNECT_PROG` `%H` substitution is validated and quoted rather than interpolated verbatim (PR #7430). The batch replay script closed two further injections in its generated `.sh` (PR #7445). |
| CVE-2026-70453 | HIGH | Quadratic CPU exhaustion in `hash_search()` from a long equal-weak-checksum chain. | **Fixed** (PR #7293). oc-rsync's block matcher differs from upstream's, so the bound was derived independently rather than copied. |
| CVE-2026-70463 | HIGH | `auth users` separator handling. | **Fixed** (PR #7345). Upstream's leading-comma separator form is honoured at both affected sites - `auth users` and `gid`, not just the one named in the advisory. |
| CVE-2026-70455 / 70461 / 70458 / 70456 | HIGH | Peer-controlled Zstandard thread count; three out-of-bounds heap writes. | The memory-safety trio has no direct Rust analogue, and the observable halves are checked rather than assumed: the live wire-token decoder rejects the malformed frames only the test-only decoders had guarded against (PR #7297), the AVX2 checksum over-read is disproven by a guard-page harness with a negative control (PR #7294), and the duplicate-suffix merge path collapses where it previously had no collapse at all (PR #7288). The peer-controlled Zstandard thread count is measured non-applicable: the option never reaches a peer-controlled decision in oc-rsync, whose only divergence runs the opposite direction (stricter on the client). |

**New defensive surfaces in 3.5.0.** `--confine-root=DIR`, `--drop-D` / `--no-drop-D`, `--insecure-links` / `--no-insecure-links` and the `auth digest` daemon directive have **shipped** (PRs #7396, #7299, #7350), as have the `insecure links` and `proxy protocol hosts` daemon directives (PRs #7484, #7648). Like upstream, `--confine-root` and `--drop-D` are deliberately **not forwarded** to the remote side: both are meant to be applied to one end of a connection by itself.

`--drop-D` tells a receiver to refuse to create device and special files whatever the transfer requested. It exists because the obvious alternative does not work: `--no-D` also frames the file list's rdev fields, and only one end of a connection receives the option, so a `-D` sender writes rdev that a `--no-D` receiver never reads. That desynchronises the list: a FIFO hangs the transfer at protocol 29 and corrupts it at 30, and a device node breaks every protocol. `--drop-D` refuses the creation while leaving the wire format untouched.

This section is updated per CVE as each is closed or evidenced as non-applicable.

### Upstream rsync 3.5.1 (21 Sep 2026)

rsync 3.5.1 raises the protocol to 33 and names **no CVE ids** in its release notes. Its bug fixes tighten path handling and the daemon on top of 3.5.0. oc-rsync still pins 3.5.0 as its reference and negotiates a protocol-33 peer down to 32 (PR #7916). The 3.5.1 test suite runs on push to master and nightly, not on pull requests, and every `fail` row in its manifests (`tools/ci/upstream-3.5.1-expect.*.txt`) names its cause and owning task.

On master:

- **`--files-from` confinement.** A local sender now stats, opens and enumerates each `--files-from` entry through the ownership walk from the held files-from base, and refuses an entry that resolves outside `--confine-root`, including through an in-root symlink to an outside directory. The walk carries 3.5.1's final flag: an ancestor of the root is allowed only while descending (PR #8012).
- **Destination root attributes.** The destination root's owner, times and mode are applied through its own descriptor, as upstream applies them to `.` after `change_dir()`, instead of by opening its parent, which lay outside the Landlock grant. A daemon enters the peer's destination beneath the module root through a port of upstream's `secure_relative_dirfd()` walk, which follows a relative in-module symlink but refuses an absolute target or a climb out of the module (PR #8010).
- **Basis and stream errors.** A block match with no basis file is a protocol error (exit 2) naming the file, and a daemon stream that closes mid-transfer reports `connection unexpectedly closed` and exits 12, as upstream does (PR #8016). This covers the first two sub-cases of the `strict-basis` cell.
- **Option and startup rules.** `--contimeout` is refused unless there is a daemon connection and bounds a daemon-over-`--rsh` handshake; `--max-alloc=0` is accepted again and resolves to the bounded maximum; the daemon enters inetd mode only for an `AF_INET`/`AF_INET6` stream on stdin (PR #8011).

Still carrying a `fail` row in the 3.5.1 manifests: `strict-basis` (remaining sub-cases), `symlink-race-dest`, `search-only-held-dirfd`, the `/dev/fd/N` cells (`pseudo-paths`, `pseudo-paths-daemon`, `read-batch-pipe`), `batch-file-symlink`, and `write-touched-blocks`, which needs protocol 33. `relative-source-ancestor`, whose row names the `--files-from` escape, passes since PR #8012; PR #8017 flips its rows to `pass`.

### Upstream rsync 3.4.3 defense-in-depth audit (2026-05-20)

Per-CVE applicability for the six 3.4.3 CVEs is in the table above. The defense-in-depth items were audited as follows:

- **Bounded wire-supplied counts and lengths** in flist/io/acls/xattrs - oc-rsync already validates these at decode (`crates/protocol/src/flist/read/`, `xattr/cache.rs:123,141`, `acl/`). Re-audit confirmed no path accepts an unbounded length without a `MAX_*` ceiling.
- **Length-underflow guard in cumulative `snprintf()` callers** - oc-rsync uses `format!()`/`write!()` which do not underflow; the equivalent risk is `usize` subtraction, audited cleanly.
- **Parent block-index bounds check on receiver** - addressed by the CVE-2026-43620 entry above.
- **NULL check in `read_delay_line()`** - oc-rsync uses `Option<&str>` so the C null-dereference is impossible.
- **Lower ceiling on `MAX_WIRE_DEL_STAT`** - the delete-stats reader lives at `crates/protocol/src/stats/delete.rs`, reads each category as a varint, and caps every one at `MAX_WIRE_DEL_STAT = 1 << 28` - the same value upstream lowered to (`rsync.h:187`, unchanged in 3.4.4 and 3.5.0), rejecting anything above it rather than clamping.
- **Reject hyphen-prefixed remote-shell hostnames** - `crates/rsync_io/src/ssh/operand.rs` rejects a leading `-` (SEC-3: audit, validation and regression coverage completed).
- **NULL-check on `localtime_r()` in `timestring()`** - oc-rsync uses `chrono`/`time` for timestamp formatting; out-of-range timestamps return `Err` rather than dereferencing a null pointer.

Follow-ups, all closed:

- **SEC-1** (TOCTOU on path-based daemon syscalls, CVE-2026-29518 / CVE-2026-43619) - fixed; see the implementation record below.
- **SEC-2** (proxy-line cap) - SEC-2.a confirmed the structural mitigation (bounds-checked `Vec::push`); SEC-2.b (PR #4812) tightened the cap to 1023 bytes, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer. The cap is **derived** rather than typed: `connect/proxy.rs` names `PROXY_BUF_SIZE = 1024` after socket.c:52 and defines `MAX_PROXY_LINE_BYTES = PROXY_BUF_SIZE - 1`, so the two cannot drift apart (PR #7650).
- **SEC-3** (hyphen-prefixed hostname rejection in SSH operand parse) - fixed.
- **SEC-4** (malformed `parent_node_idx`, CVE-2026-43620) - `DirectoryTree::try_add_directory` validates the wire-supplied parent index and returns `DirTreeError::OutOfBoundsParent`; three regression tests in `crates/protocol/src/flist/dir_tree.rs` pin both the graceful-reject path and the worst-case controlled-panic path (no SIGSEGV).

#### SEC-1 implementation record (CVE-2026-29518 / CVE-2026-43619)

Umbrella issue #2516. **Status: fixed.** All receiver call sites are wired through `DirSandbox`, and the SEC-1.m / SEC-1.n regression suites pass against the fully wired pipeline.

- **SEC-1.a-e**: `DirSandbox` carrier with in-tree dirfd cache, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime detection, and receiver pipeline wiring (PRs #4643, #4650 and prior).
- **SEC-1.f** (PR #4668): receiver `lstat` / `symlink_metadata` via `fstatat(AT_SYMLINK_NOFOLLOW)`.
- **SEC-1.g** (PR #4671): receiver `remove_file` / `remove_dir` via `unlinkat`.
- **SEC-1.h** (PR #4683): receiver `mkdir` / `symlink` / `hard_link` via `mkdirat` / `symlinkat` / `linkat`.
- **SEC-1.i** (PR #4690): `fchmodat` / `fchownat` / `utimensat` replace `chmod` / `lchown` / `utimes`.
- **SEC-1.j** (PR #4693): `renameat` replaces `rename`.
- **SEC-1.k**: macOS verified - the `*at` family is available and behaves as on Linux.
- **SEC-1.l**: Windows audited - NTFS handle-based APIs sidestep the TOCTOU window, so Windows is not affected by either CVE.
- **SEC-1.m** (PR #4675): symlink-swap attack regression coverage against the daemon receiver.
- **SEC-1.n** (PR #4678): interop coverage confirming legitimate symlinks still transfer under the `*at` paths.
- **SEC-1.p** (PR #4702): Landlock defense in depth. `crates/fast_io/src/landlock.rs` wraps Landlock 0.4 and requests `AccessFs::from_all(ABI::V5)` with best-effort downgrade (v5 and v4 on recent kernels, v3 on 6.2+, v2 on 5.19+, v1 on 5.13+), naming the rights it had to drop. `crates/daemon/src/daemon/sections/module_access/transfer.rs::engage_landlock_sandbox` allowlists `module.path` per connection after `apply_module_privilege_restrictions` returns, so a path-based syscall that bypasses `DirSandbox` is refused by the kernel with `EACCES`. Client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir` / `--compare-dest` / `--copy-dest` / `--link-dest` paths that resolve outside the module root are rejected at the wire-protocol layer (PR #5568); the in-module subset joins the allowlist so legitimate writes are not refused. On non-Linux targets the stub returns `Unavailable` and the `*at` chain is the sole defense.
- **SEC-1.q / q2**: deletion routes through the `DirSandbox`-backed `DeleteFs` trait, and every receiver deletion call site is wired through it.
- **SEC-1.s**: `recursive_unlinkat` removes directory trees with `unlinkat` throughout.
- **SEC-1.t**: `ensure_dest_root_exists` in `crates/transfer/src/receiver/mod.rs` uses `symlink_metadata()` so a symlink at the destination root is seen directly, and refuses any symlinked destination with `InvalidInput` rather than letting `create_dir_all` materialise the directory at the link target (follow-up to PR #5567).
- **SEC-MK.a-h**: device and FIFO creation uses `mknodat` / `mkfifoat` through `DirSandbox`.

### Upstream rsync 3.4.2 audits

In v0.6.2 the codebase was audited against every fix that landed in upstream rsync 3.4.2. The equivalent code paths were verified safe in oc-rsync:

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

## CVE Monitoring Process

Sources:

1. **rsync-announce**: https://lists.samba.org/mailman/listinfo/rsync-announce
2. **NVD**: https://nvd.nist.gov/vuln/search?query=rsync
3. **GitHub Security Advisories** for this repository
4. **Scheduled watcher**: [`upstream-release-watch.yml`](./.github/workflows/upstream-release-watch.yml) runs `tools/ci/check_upstream_release.sh` every Monday and opens a tracking issue when a new upstream rsync release ships

For each new upstream CVE:

1. Analyse the root cause (memory corruption, logic error, etc.)
2. Check whether oc-rsync has an equivalent code path
3. Check whether Rust's guarantees actually cover it (logic flaws are not covered)
4. Record the analysis in this file
5. If oc-rsync is affected, issue a security advisory and a fix

## Fuzzing

The repo ships 26 cargo-fuzz targets covering security-critical parsing, SIMD parity, concurrency, and differential fuzzing against upstream rsync:

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
- `batch_reader` (FCV-12) - rsync batch file header
- `bwlimit` (FCV-15) - bwlimit CLI string parser

**Metadata and extensions:**
- `acl_xattr_wire` (FCV-11) - ACL/xattr wire format
- `filter_list_wire` - filter list wire format
- `buffered_map` - buffered map decoder

**Compression:**
- `decompressor_zlib` - zlib decompression
- `decompressor_zstd` - zstd decompression
- `zlib_token_decode` - zlib compressed-token decoder

**SIMD parity:**
- `simd_checksum_parity` - cross-validates AVX2, SSE2, NEON, and scalar rolling/strong checksum paths against random inputs (see #2103)

**Concurrency:**
- `parallel_receive_delta_adversarial` - adversarial scheduling against the parallel receive-delta pipeline

**Filter rules:**
- `filter_rules_vs_upstream` - filter rule evaluation vs upstream behavior
- `filter_differential` - differential filter testing

**Differential fuzzing (upstream wire parity):**
- `differential_outcome` (WDF-3) - outcome-based differential fuzzing against upstream rsync
- `differential_multiplex` (WDF-4) - multiplex frame differential fuzzing
- `differential_flist` (WDF-5) - flist wire format differential fuzzing

See `fuzz/README.md` for detailed fuzzing instructions.

## Hardening Notes

### Buffer pool bounds checks

`recycle_buffer(buf_id)` in the io_uring path (`crates/fast_io/src/io_uring/buffer_ring/mod.rs`) validates that `buf_id` falls within the registered buffer pool and returns `BufferRingError::BufferIdOutOfRange` when it does not. The check runs in **both debug and release builds**, and the recycle is refused before any state is mutated, so a corrupted or attacker-influenced `buf_id` cannot advance the ring tail or write into kernel-shared memory.

### io_uring buffer-group ID namespace

io_uring buffer-group IDs (`bgid`) live in a 16-bit namespace. Allocation is capped at this bound, and exhaustion returns `BgidAllocError::Exhausted` rather than wrapping. Released ids go back to a free list. No production caller allocates one today: `BufferRing` and `BgidAllocator` appear only inside `fast_io` and its tests, so exhaustion is not a live operational condition (`docs/audits/bgid-lifecycle.md`, section 5). Peak-occupancy telemetry (BGE-3) becomes relevant when per-session rings are wired in.

### SSH double compression

If the SSH transport compresses the stream (`Compression yes` in `ssh_config`), `oc-rsync -z` compresses payloads twice. This costs CPU and can mask compressor-specific bugs. Leave compression to rsync (`-z`) and disable it in SSH.

## Operator Guidance

### Daemon

- **Transport.** The daemon protocol is plaintext, like upstream: it authenticates but does not encrypt. On an untrusted network, bind the daemon to `127.0.0.1` or a private interface and reach it through an SSH tunnel (`ssh -L`) or the SSH transport, or through a TLS terminator such as `stunnel` or HAProxy / nginx in TCP mode. oc-rsync has no built-in TLS client (the former `--ssl` / `client-tls` path was removed to match upstream), so clients use an external wrapper such as `rsync-ssl` or `stunnel`.
- **Confinement.** `use chroot = yes`, and expose only the paths you need. Prefer `read only = yes` where possible.
- **Access control.** `hosts allow` / `hosts deny` run before authentication; `auth users` with a `secrets file` of mode `0600`, owned by the daemon user only. With `proxy protocol = true`, list the trusted proxies in `proxy protocol hosts`; left unset, every PROXY header is refused.
- **Identity mapping.** `numeric ids = yes` keeps uid/gid mapping independent of the daemon's `passwd`/`group`.
- **Options.** `refuse options = delete *` for read-only mirrors.

### Client

1. **Verify the server's identity**: use SSH for transport when possible
2. **Check `--delete` targets**: make sure you are syncing to the intended destination
3. **Review exclude patterns**: avoid transferring sensitive files by accident
4. **Confine untrusted transfers**: `--confine-root=DIR` confines every operator- and peer-supplied path beneath `DIR`, and `--drop-D` makes a receiver refuse device and special files

## Acknowledgments

Security researchers who have contributed to oc-rsync's security:
- (Your name could be here - report responsibly!)
