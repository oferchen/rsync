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
- `daemon`, `cli`, `core`, `transfer`, `batch`, `filters`, `signature`, `matching`, `bandwidth`, `logging`, `logging-sink`, `branding`, `rsync_io`, `compress`, `apple-fs`, `flist`, `embedding`, `test-support` - business logic, parsers, orchestration, and high-level I/O wrappers. Some of these crates carry `#[allow(unsafe_code)]` in test code only, for example to set environment variables. `test-support` is itself a test-only crate.

Crates with `#![deny(unsafe_code)]` and targeted `#[allow(unsafe_code)]` for documented FFI/SIMD boundaries:
- `metadata` - Ownership, identity, xattr, timestamp and Windows ACL FFI (for example `getpwuid_r`/`getgrnam_r`, `setuid`/`setgid` for `--copy-as`, `setattrlist`, NTFS DACLs)
- `protocol` - One isolated `#[allow]` in `multiplex::helpers` for performance-critical frame parsing
- `engine` - Denies unsafe outside tests (`#![cfg_attr(not(test), deny(unsafe_code))]`) with targeted `#[allow(unsafe_code)]` on platform FFI and the buffer pool (`syncfs`, `clonefile`, buffer reuse)
- `platform` - Daemonization, per-connection `fork`/`waitpid`, name and group resolution, privilege transitions (`setuid`/`setgid`/`initgroups`), chroot, environment access, local time, signal disposition, and Windows service dispatch
- `checksums` - SIMD intrinsics for MD4/MD5 and rolling checksums (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM), with scalar fallbacks and parity tests
- `fast_io` - Platform I/O syscalls (sendfile, io_uring, mmap, `copy_file_range`, IOCP, `WSARecv`/`WSASend`, `setsockopt`) and the `signal::install_signal_handler` FFI wrapper, with standard I/O / no-op fallbacks
- `windows-gnu-eh` - Windows GNU exception handling shims

The `oc-rsync` binary (`src/bin/oc-rsync.rs`) also denies unsafe code, with one `#[unsafe(no_mangle)]` static that sets jemalloc's decay options.

**Long-term direction.** Unsafe code is being consolidated into two owning crates: `fast_io` for I/O syscalls and `platform` for process, identity, environment and signals. Both expose safe public APIs. `metadata`, `checksums`, `engine` and `protocol` still hold production unsafe until their sites migrate. New `#[allow(unsafe_code)]` annotations in any other crate require explicit review.

## Upstream CVE Status

The reference version is upstream rsync 3.5.1. Its release notes name no CVE ids; its fixes are tracked in the 3.5.1 table below. Each CVE row carries one of four statuses:

- **Fixed**: the code closes it, and a named test or PR shows it.
- **Open**: oc-rsync is still exposed, or a test that covers it fails.
- **Not applicable**: the affected code has no counterpart in oc-rsync.
- **Unverified**: not yet established either way from the code or a test.

"Passes on every leg that runs it" refers to the upstream 3.5.1 test suite, run against oc-rsync on the legs listed in the [README](./README.md#upstream-testsuite). Some cells skip on some legs, for example when they need root.

| Upstream batch | oc-rsync status |
|---|---|
| 2024 (CVE-2024-12084 to 12088, CVE-2024-12747) | Not vulnerable or mitigated |
| rsync 3.4.2 fixes | Audited in v0.6.2; equivalent paths verified safe |
| rsync 3.4.3 (six CVEs) | Fixed or not vulnerable |
| rsync 3.5.0 (33 CVEs) | One row each below |
| rsync 3.5.1 (no CVE ids) | One row per security-relevant fix below |

### 2024 and rsync 3.4.3 CVEs

| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2024-12084 | Heap overflow in checksum parsing | Not vulnerable | Rust Vec<u8> handles dynamic sizing |
| CVE-2024-12085 | Uninitialized stack buffer leak | Not vulnerable | Rust requires initialization |
| CVE-2024-12086 | Server leaks client files | Not vulnerable | Strict path validation |
| CVE-2024-12087 | Path traversal via --inc-recursive | Not vulnerable | Unless `--trust-sender` is given, the receiver drops every file-list entry with a `..` component, or an absolute path outside `--relative`, before any disk operation (`crates/transfer/src/receiver/file_list/sanitize.rs`, after upstream's `clean_fname(CFN_REFUSE_DOT_DOT_DIRS)`), and a sub-list's parent index is bounds-checked against the directory list (`DirectoryTree::try_add_directory`) |
| CVE-2024-12088 | --safe-links bypass | Mitigated | `--safe-links` uses a port of upstream's `unsafe_symlink()` (`crates/transfer/src/symlink_safety.rs`), including the 3.4.1 rule that rejects an inner `/../`, so a target that could later be redirected through a replaced parent is refused; symlinks are then created with `symlinkat` beneath the pinned destination descriptor |
| CVE-2024-12747 | Symlink race condition | Mitigated | Receiver path syscalls resolve beneath the pinned destination descriptor: temp files are created with `openat(O_CREAT \| O_EXCL \| O_NOFOLLOW)` and committed with `renameat`, so a path component swapped for a symlink cannot redirect the write. The Windows receiver uses the reparse-point-safe equivalents (`crates/fast_io/src/win_atomic_commit.rs`). On Linux the daemon receiver is additionally confined to the module path by Landlock |
| CVE-2026-29518 | TOCTOU symlink race in daemon receiver (`use chroot = no`) | **Fixed** | All path-based syscalls have been migrated to `*at` variants routed through `DirSandbox` with `openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` runtime detection. Device/FIFO node creation uses `mknodat`/`mkfifoat`. A Landlock LSM defense-in-depth layer (PR #4702) allowlists `module.path` on the daemon receiver via Landlock 0.4 (requests up to ABI v5 with best-effort downgrade; v1 on kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43617 / GHSA-rjfm-3w2m-jf4f | Reverse-DNS lookup after daemon chroot causes hostname ACL bypass | **Fixed** | Hostname resolution runs before any per-module chroot: per session in `session_runtime.rs::handle_session` at accept time, and per module in `module_access/request.rs::respond_with_module_request` during ACL evaluation (`module_peer_hostname()`). The per-module chroot is applied later, after authentication (`module_access/transfer/sandbox.rs::apply_privilege_restrictions_with_upstream_errors`). The global `daemon chroot` is applied once at startup, before the accept loop (`accept_loop.rs::serve_connections`), so a lookup inside it can fail when the chroot lacks NSS files. `ModuleDefinition::permits` therefore fails closed when reverse DNS returns nothing and any `hosts deny` rule is hostname-based: a peer that controls or blackholes its PTR record cannot bypass a hostname deny rule. Regression tests: `module_peer_hostname_resolution_before_chroot_denies_unknown` (allow side), `module_hostname_deny_fails_closed_when_dns_unresolved` (deny side under daemon chroot), `module_ip_deny_unaffected_by_dns_failure` (IP-only rules unchanged). |
| CVE-2026-43618 | Integer overflow in compressed-token decoder causes memory disclosure | **Fixed** | The upstream C vulnerability uses a multiplexed signed-integer return from `recv_deflated_token()` where a negative `rx_token` is misinterpreted as a literal length, leaking memory via a stale `*data` pointer. oc-rsync's decoder returns a typed `CompressedToken` enum (`Literal(Vec<u8>)` / `BlockMatch(u32)` / `End`), structurally eliminating the return-value misinterpretation vector. The residual risk - a malicious sender injecting a negative absolute token via `TOKEN_LONG` that wraps to a valid-looking block index after `as u32` cast - is closed by an explicit sign check in all three wire decoders (zlib, zstd, lz4). Regression tests: `zlib_decoder_rejects_negative_absolute_token`, `zstd_decoder_rejects_negative_absolute_token`, `lz4_decoder_rejects_negative_absolute_token`, `zlib_decoder_rejects_i32_min_token` in `crates/protocol/src/wire/compressed_token/tests.rs`. Audit doc: `docs/audits/upstream-3.4.2-token-decoder-parity.md`. |
| CVE-2026-43619 | Symlink races on chmod/lchown/utimes/rename/unlink/mkdir/symlink/mknod/link/rmdir/lstat | **Fixed** | Same root cause as CVE-2026-29518. All `*at` helpers shipped and receiver call sites fully wired: `lstat` / `unlink` / `rmdir` / `mkdir` / `symlink` / `link` migrated to `fstatat` / `unlinkat` / `mkdirat` / `symlinkat` / `linkat`; `chmod` / `lchown` / `utimes` migrated to `fchmodat` / `fchownat` / `utimensat` (PR #4690); `rename` migrated to `renameat` (PR #4693); `mknod` / `mkfifo` migrated to `mknodat` / `mkfifoat`. Deletion routes through the same sandbox, and directory trees are removed with `unlinkat` throughout. The Landlock LSM defense-in-depth layer (PR #4702) confines the daemon receiver to the configured `module.path` via Landlock 0.4 (kernel 5.13+). Umbrella tracking issue #2516. |
| CVE-2026-43620 | OOB read in `recv_files` via negative `parent_ndx` → client SIGSEGV | Not vulnerable | oc-rsync consumes the parent reference as `Option<usize>` and indexes into a bounds-checked `Vec` (`crates/protocol/src/flist/dir_tree.rs`). The validating entry point `DirectoryTree::try_add_directory` returns `DirTreeError::OutOfBoundsParent` on a malformed wire index; the unchecked `add_directory` aborts via Rust's bounds-check panic. Regression coverage: `try_add_directory_rejects_out_of_range_parent_idx`, `try_add_directory_rejects_boundary_off_by_one`, `add_directory_panics_safely_on_oob_parent_idx` in `crates/protocol/src/flist/dir_tree.rs`. |
| CVE-2026-45232 | Off-by-one stack write in HTTP CONNECT proxy response handler | **Fixed** | `read_proxy_line()` in `crates/core/src/client/module_list/connect/proxy.rs` reads byte-by-byte into a heap `Vec<u8>` and explicitly caps the response line at `MAX_PROXY_LINE_BYTES = 1023` bytes, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer (socket.c:86). The C off-by-one stack-write is structurally impossible (bounds-checked `Vec::push`), and indefinite buffering is bounded by the explicit cap. Audit PR #4609; upstream-parity alignment PR #4812. |

### rsync 3.5.0 CVEs (fixed upstream in 3.5.0, 13 Aug 2026)

3.5.0 kept protocol 32, so none of these stem from a wire-format change. They are implementation flaws in code oc-rsync reimplements independently. Re-derive the id list from the 3.5.0 section of the 3.5.1 `NEWS.md` and check that every id has a row (the section also names two 3.4.3 ids, `CVE-2026-43617` and `CVE-2026-43620`, which have rows above):

```sh
awk '/^# NEWS for rsync 3.5.0/,/^# NEWS for rsync 3.4/' \
  target/interop/upstream-src/rsync-3.5.1/NEWS.md \
  | grep -o 'CVE-2026-[0-9]*' | sort -u > /tmp/batch
grep -oE '^\| CVE-2026-[0-9]+' SECURITY.md | cut -c3- | sort -u > /tmp/rows
comm -23 /tmp/batch /tmp/rows      # ids with no row: prints nothing
grep -cE '^\| CVE-2026-(5|7)[0-9]{4} \|' SECURITY.md      # 3.5.0 rows: 33
```

| CVE | Severity | Upstream issue | Status | Evidence |
|---|---|---|---|---|
| CVE-2026-53791 | CRITICAL | `proxy protocol = true` let a direct client forge a PROXY header and spoof its address past `hosts allow` / `hosts deny`. | Fixed | `proxy protocol hosts` mirrors upstream's `allow_proxy_protocol_peer()` (access.c:311-317): an unset or empty list refuses every PROXY header, and the daemon warns at startup as upstream does (PR #7648). The stdio entry points no longer fabricate `127.0.0.1` as the peer (PR #7303). `proxy-protocol-trusted-peer` passes on every leg that runs it. |
| CVE-2026-53783 | HIGH | `rrsync` restricted-directory escape through a TOCTOU window between validation and exec. | Not applicable | oc-rsync ships no `rrsync` wrapper. Upstream's `rrsync-*` cells, which wrap oc-rsync as the rsync binary, pass on every leg that runs them. |
| CVE-2026-53784 | HIGH | Daemon module-root `chdir` followed a planted parent symlink under `use chroot = no`. | Fixed | The module root is entered with plain `chdir`/`openat` and the peer tail through a confined walk (PR #7304). `daemon-module-chdir-symlink` passes on every leg that runs it. |
| CVE-2026-53785 | HIGH | Under `--relative`, implied-parent creation followed a planted parent symlink. | Fixed | `--relative` implied parents resolve through the ownership walk (PR #7393). `relative-implied-symlink`, `relative-mkpath-symlink` and `relative-mkpath-dir-symlink` pass on every leg that runs them. |
| CVE-2026-53786 | MEDIUM | A client `--filter` merge file bypassed the daemon module filter. | Fixed | `daemon-filter-merge-bypass` passes on every leg. |
| CVE-2026-53788 | MEDIUM | A newline in a peer-controlled name injected requests into the name-converter protocol. | Fixed | `daemon-namecvt-newline-token` passes on every leg that runs it. |
| CVE-2026-53789 | MEDIUM | A daemon sender widened `--delete` scope by omitting the "no content dir" flag on an implied parent. | Fixed | `malicious-sender-delete-scope`, `malicious-dot-dir-delete-scope`, `malicious-dot-file-delete-scope` and `peer-legacy-implied-delete-scope` pass on every leg that runs them. |
| CVE-2026-53790 | HIGH | Command and argument injection through unquoted peer- or host-controlled values. | Fixed | Hook-variable expansion refuses shell metacharacters (PR #7465), the `RSYNC_CONNECT_PROG` `%H` substitution is validated and quoted (PR #7430), and the batch replay script quotes its arguments (PR #7445). The `connect-prog-*`, `daemon-exec-*` and `write-batch-quoting` cells pass on every leg that runs them. The `rsync-ssl` arm does not apply: oc-rsync ships no `rsync-ssl`. |
| CVE-2026-53792 | MEDIUM | A checksum header with blocks but a zero block length drove the sender's match arithmetic negative. | Fixed | `checksum-zero-blocklen` passes on every leg that runs it. |
| CVE-2026-53793 | HIGH | A symlinked parent inside a chroot inner module reached a sibling outside it through `/./`. | Fixed | The module identity is resolved before the chroot (PR #7585), and the destination is entered through a port of `secure_relative_dirfd()` (PR #8010). The six `chroot-*-inner-module` cells pass on every leg that runs them. |
| CVE-2026-53794 | MEDIUM | `--max-alloc=0` disabled the per-allocation cap. | Fixed | Follows 3.5.1, which accepts `0` again as the maximum limit rather than as "no limit" (PR #8011). `max-alloc-zero` passes on every leg that runs it. |
| CVE-2026-53795 | HIGH | An absolute `--temp-dir` or `--link-dest` disabled rename and link confinement. | Fixed | The staging family, `--log-file`, the alt-dest basis, `--relative` implied parents and absolute rename endpoints resolve through one ownership walk (PRs #7393, #7398, #7404, #7415, #7419), as do the operator-named auxiliary files (PRs #7421-#7426, #7439, #7441-#7443, #7459, #7463). The `operator-path-*`, `link-dest-*` and `rename-mixed-parent-*` cells pass on every leg that runs them. |
| CVE-2026-53796 | MEDIUM | A non-daemon receiver's `chdir()` into a relative destination was not confined. | Fixed | An untrusted destination symlink is refused with upstream's diagnostic (PR #8071). `symlink-race-dest`, `symlink-race-relative-dest` and `chdir-symlink-race` pass on every leg that runs them. |
| CVE-2026-53797 | MEDIUM | A non-daemon sender opened file content by path, following a raced parent symlink. | Fixed | Explicit source roots are held by dev/ino and content opens run beneath the held root; a replaced root is refused with `ELOOP` (PR #8120). An rsh-server-sender e2e swaps a listed directory mid-scan and is refused with exit 23, and `relative-source-ancestor`, `symlink-race-source` and `chdir-symlink-race` pass on every leg that runs them. |
| CVE-2026-53798 | MEDIUM | The daemon name converter mapped an unknown name to uid/gid 0. | Fixed | oc-rsync reads a converter answer strictly, as upstream's `namecvt_call()` does (clientserver.c:1340-1354): an empty or non-numeric answer is an unknown name, and the receiver keeps the sender's id (uidlist.c:273-276). Id 0 is never resolved by name, matching `recv_add_id()`'s `*name && id` guard (PR #8108). A root e2e test (`tests/daemon_name_converter_unknown_answer.rs`) pushes a `nobody`-owned file through converters that answer empty, `notanumber` and `4242`, and gets the sender's ids, the sender's ids and 4242:4242, the same as the upstream 3.5.1 control; making the parse read those answers as 0 fails both refusal cells. The upstream `daemon-namecvt-empty-response` cell reads a `--fake-super` xattr, which oc-rsync does not implement, so it passes without exercising the mapping. |
| CVE-2026-53799 | MEDIUM | ACL and xattr application followed a symlink race. | Partially fixed | The network receiver writes xattrs through a leaf pinned beneath the destination root and skips them when the pin is refused (`rsync.c:582-597`). ACLs are still applied by path; the Linux fd-based arm (`lib/acl.c:110-145`) is a follow-up. `acl-symlink-race` and `copy-xattrs-symlink-race` pass on every leg that runs them. |
| CVE-2026-53800 | MEDIUM | `--remove-source-files` unlink followed a parent-symlink race. | Fixed | `sender-remove-source-secure` and `sender-remove-source-relative-anchor` pass on every leg that runs them. |
| CVE-2026-53801 | MEDIUM | Directory-scan enumeration escaped the transfer root or module. | Fixed | `sender-scan-dir-escape` and `daemon-scan-dir-escape` pass on every leg that runs them. |
| CVE-2026-53802 | HIGH | Symlinked operator input files: merge files, per-directory merges and `.cvsignore`. | Fixed | Merge and `--*clude-from` files open through the ownership walk (PRs #7421-#7426). `filter-merge-symlink` passes on every leg that runs it. |
| CVE-2026-53803 | HIGH | Symlinked operator output paths: `--log-file`, batch files, and the daemon motd, lock, early-input and config opens. | Fixed | The `--config`, `log file` / `--log-file`, `lock file`, `motd file`, `--write-batch` and its `.sh` script, `--read-batch` and `--early-input` opens go through the ownership walk (PRs #7439, #7441, #7443). The oc-only `--motd-file` / `--motd` flags now take the same walk instead of a plain read, and the `pid file` leaf is opened as upstream's `create_pid_file()` does (clientserver.c:1636-1654): a symlink is replaced, never written through (PR #8108). `log-file-symlink`, `operator-path-write-batch`, `batch-file-symlink`, `daemon-config-symlink` (config, motd and lock file) and `early-input-symlink` pass on every leg that runs them. A root e2e test (`tests/daemon_operator_path_symlinks.rs`) plants a `nobody`-owned symlink at the config, log file, pid file, `--motd-file` and `--read-batch` paths; reverting the motd or pid-file fix fails its cell. |
| CVE-2026-70452 | HIGH | `hosts deny` failed open when a configured hostname did not resolve. | Fixed | Lookup failure and non-match are now distinct results, and matching is case-insensitive as in upstream `iwildmatch` (access.c:57) (PR #7314). `daemon-deny-dns-failopen` passes on every leg that runs it. |
| CVE-2026-70453 | HIGH | Quadratic CPU in `hash_search()` from a long equal-weak-checksum chain. | Fixed | The chain walk is bounded (PR #7293). `hashsearch-chain` passes on every leg that runs it. |
| CVE-2026-70454 | MEDIUM | `rsync-ssl` made unauthenticated TLS connections. | Not applicable | oc-rsync ships no `rsync-ssl`. |
| CVE-2026-70455 | HIGH | A daemon client could request any Zstandard worker count. | Fixed | `daemon-zstd-thread-exhaustion` and `daemon-refuse-compress-threads-alias` pass on every leg that runs them. |
| CVE-2026-70456 | HIGH | Heap write one past the end in `read_args()` at exactly `maxargs`. | Not applicable | The `daemon` crate denies unsafe code, so an out-of-bounds write cannot happen. `scanner-argv-bounds` passes on every leg that runs it. |
| CVE-2026-70457 | MEDIUM | Attacker-chosen-offset write in `parse_size_arg()` error formatting. | Fixed | `daemon-size-arg-overflow` passes on every leg. |
| CVE-2026-70458 | HIGH | Out-of-bounds write from `FLAG_HLINKED` accepted without `-H`. | Not applicable | The file list is decoded in safe Rust (`protocol`, `transfer`), so an out-of-bounds write cannot happen. Whether the receiver refuses the flag like upstream is unverified: `proto-hlink-flag-oob` skips on every leg. |
| CVE-2026-70459 | MEDIUM | Wild-pointer read from a crafted first incremental file list. | Fixed | Parent indexes are bounds-checked (`DirectoryTree::try_add_directory`). `proto-parent-ndx-empty-dirflist` passes on every leg that runs it. |
| CVE-2026-70460 | HIGH | A peer `--partial-dir` or `--backup-dir` resolved by pathname escaped the module. | Fixed | `operator-path-partial-dir-daemon`, `operator-path-backup-dir-daemon`, their `-exclude-daemon` variants and the three `operator-path-traversal-*-daemon` cells pass on every leg. |
| CVE-2026-70461 | HIGH | One-byte heap write in `add_implied_include()` from a trailing backslash. | Not applicable | The `filters` crate denies unsafe code, so an out-of-bounds write cannot happen. Whether the rule is handled like upstream is unverified: `exclude-implied-trailing-backslash` skips on every leg. |
| CVE-2026-70462 | MEDIUM | A peer `MSG_IO_TIMEOUT` overflowed or disabled the client's I/O timeout. | Fixed | `msg-io-timeout-overflow` and `msg-io-timeout-zero` pass on every leg that runs them. |
| CVE-2026-70463 | HIGH | `auth users` split on whitespace despite the leading-comma form. | Fixed | The comma-only form is honoured for `auth users` and `gid` (PR #7345). `daemon-auth-users-comma-only` passes on every leg. |
| CVE-2026-70464 | HIGH | An unauthenticated peer could stall the daemon after the greeting. | Fixed | The pre-authentication phase has a deadline (`crates/daemon/src/daemon/handshake_deadline.rs`), and a peer `@RSYNCD: OPTION` line cannot override daemon parameters: a line sent before the module name is answered as an unknown module, as upstream does (PRs #7754, #8002). `daemon-handshake-timeout` passes on every leg that runs it. |

Count the statuses:

```sh
grep -E '^\| CVE-2026-(5|7)[0-9]{4} \|' SECURITY.md | cut -d'|' -f5 | sort | uniq -c
```

**New options in 3.5.0.** `--confine-root=DIR`, `--drop-D` / `--no-drop-D`, `--insecure-links` / `--no-insecure-links`, and the `auth digest`, `insecure links` and `proxy protocol hosts` daemon directives are implemented (PRs #7396, #7299, #7350, #7484, #7648). Like upstream, `--confine-root` and `--drop-D` are not forwarded to the remote side.

`--drop-D` makes a receiver refuse to create device and special files. It exists because `--no-D` on one end only desynchronises the file list: a `-D` sender writes rdev fields that a `--no-D` receiver never reads. `--drop-D` refuses the creation and leaves the wire format alone.

### rsync 3.5.1 fixes (21 Sep 2026)

3.5.1 raises the protocol to 33 and names no CVE ids. Its security-relevant fixes, from its `NEWS.md`:

| 3.5.1 fix | Status | Evidence |
|---|---|---|
| Explicit sender paths traverse symlinked ancestors without weakening scan confinement; `--files-from` entries are operator paths | Fixed | `--files-from` entries resolve through the ownership walk and are refused outside `--confine-root` (PR #8012). `relative-source-ancestor` passes on every leg that runs it. |
| `/dev/stdin`, `/dev/stdout`, `/dev/stderr` and `/dev/fd/N` work for pipes; `--read-batch` accepts a FIFO | Fixed | `pseudo-paths`, `read-batch-pipe` and `pseudo-paths-daemon` pass on every leg that runs them (PR #8096 flipped the first two). A root daemon already logs to a `/dev/fd/N` or `/dev/stdout` pipe. `pseudo-paths-daemon` failed on the Linux root legs for another reason: the daemon, dropped to `nobody`, re-stamped the mtime of a root-owned module directory and got `EPERM`. The receiver now skips an mtime already within `--modify-window`, as upstream `same_mtime()` does (rsync.c:489). |
| `--max-alloc=0` means the maximum limit, not no limit | Fixed | PR #8011. `max-alloc-zero` passes on every leg that runs it. |
| `rrsync` restricted-root paths | Not applicable | oc-rsync ships no `rrsync`. |
| inetd mode only for a network stream on stdin | Fixed | PR #8011. `daemon-stdin-local-socket` passes on every leg. |
| `--contimeout` applies to daemon connections over `--rsh` only | Fixed | PR #8011. `contimeout-rsh` passes on every leg. |
| Partial-directory state is validated | Fixed | A block match with no basis file errors as upstream does (PR #8016), and a sender `NDX_DONE` ends the phase as in upstream `recv_files()` (PR #8075). `strict-basis` passes on every leg. |
| An alternate-destination leaf symlink is not followed as a basis | Fixed | `alt-dest-symlink-race` passes on every leg. |
| Undefined shifts in the bundled zlib | Not applicable | oc-rsync does not build upstream's bundled zlib. |

The remaining `fail` rows in the 3.5.1 manifests are listed, with cause and owner, by `awk '!/^#/ && $2=="fail" {print $1}' tools/ci/upstream-3.5.1-expect.*.txt | sort -u`.

### Earlier audits

The rsync 3.4.3 defense-in-depth audit, the path-syscall (`*at`) migration record for CVE-2026-29518 / CVE-2026-43619, and the rsync 3.4.2 audit list are kept in [`docs/audits/security-upstream-audit-history.md`](./docs/audits/security-upstream-audit-history.md).

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
- `capability_flags` - negotiation prologue capability flags
- `vstring` - vstring parser
- `auth_response` - daemon authentication response

**Daemon and configuration:**
- `daemon_greeting` - daemon greeting generation
- `batch_reader` - rsync batch file header
- `bwlimit` - bwlimit CLI string parser

**Metadata and extensions:**
- `acl_xattr_wire` - ACL/xattr wire format
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
- `differential_outcome` - outcome-based differential fuzzing against upstream rsync
- `differential_multiplex` - multiplex frame differential fuzzing
- `differential_flist` - flist wire format differential fuzzing

See `fuzz/README.md` for detailed fuzzing instructions.

## Hardening Notes

### Buffer pool bounds checks

`recycle_buffer(buf_id)` in the io_uring path (`crates/fast_io/src/io_uring/buffer_ring/mod.rs`) validates that `buf_id` falls within the registered buffer pool and returns `BufferRingError::BufferIdOutOfRange` when it does not. The check runs in **both debug and release builds**, and the recycle is refused before any state is mutated, so a corrupted or attacker-influenced `buf_id` cannot advance the ring tail or write into kernel-shared memory.

### io_uring buffer-group ID namespace

io_uring buffer-group IDs (`bgid`) live in a 16-bit namespace. Allocation is capped at this bound, and exhaustion returns `BgidAllocError::Exhausted` rather than wrapping. Released ids go back to a free list. No production caller allocates one today: `BufferRing` and `BgidAllocator` appear only inside `fast_io` and its tests, so exhaustion is not a live operational condition (`docs/audits/bgid-lifecycle.md`, section 5). Peak-occupancy telemetry becomes relevant once per-session rings are wired in.

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
