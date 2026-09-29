# Upstream security audit history

Audit records moved out of [`SECURITY.md`](../../SECURITY.md), which keeps the current per-CVE status. The task identifiers below (SEC-n) name the tracking items of each audit.

## Upstream rsync 3.4.3 defense-in-depth audit (2026-05-20)

Per-CVE applicability for the six 3.4.3 CVEs is in the table in [`SECURITY.md`](../../SECURITY.md#2024-and-rsync-343-cves). The defense-in-depth items were audited as follows:

- **Bounded wire-supplied counts and lengths** in flist/io/acls/xattrs - oc-rsync already validates these at decode (`crates/protocol/src/flist/read/`, `xattr/cache.rs:123,141`, `acl/`). Re-audit confirmed no path accepts an unbounded length without a `MAX_*` ceiling.
- **Length-underflow guard in cumulative `snprintf()` callers** - oc-rsync uses `format!()`/`write!()` which do not underflow; the equivalent risk is `usize` subtraction, audited cleanly.
- **Parent block-index bounds check on receiver** - addressed by the CVE-2026-43620 entry in [`SECURITY.md`](../../SECURITY.md#2024-and-rsync-343-cves).
- **NULL check in `read_delay_line()`** - oc-rsync uses `Option<&str>` so the C null-dereference is impossible.
- **Lower ceiling on `MAX_WIRE_DEL_STAT`** - the delete-stats reader lives at `crates/protocol/src/stats/delete.rs`, reads each category as a varint, and caps every one at `MAX_WIRE_DEL_STAT = 1 << 28` - the same value upstream lowered to (`rsync.h:187`, unchanged in 3.4.4 and 3.5.0), rejecting anything above it rather than clamping.
- **Reject hyphen-prefixed remote-shell hostnames** - `crates/rsync_io/src/ssh/operand.rs` rejects a leading `-` (SEC-3: audit, validation and regression coverage completed).
- **NULL-check on `localtime_r()` in `timestring()`** - oc-rsync uses `chrono`/`time` for timestamp formatting; out-of-range timestamps return `Err` rather than dereferencing a null pointer.

Follow-ups, all closed:

- **SEC-1** (TOCTOU on path-based daemon syscalls, CVE-2026-29518 / CVE-2026-43619) - fixed; see the implementation record below.
- **SEC-2** (proxy-line cap) - SEC-2.a confirmed the structural mitigation (bounds-checked `Vec::push`); SEC-2.b (PR #4812) tightened the cap to 1023 bytes, matching upstream's 1024-byte `establish_proxy_connection()` stack buffer. The cap is **derived** rather than typed: `connect/proxy.rs` names `PROXY_BUF_SIZE = 1024` after socket.c:52 and defines `MAX_PROXY_LINE_BYTES = PROXY_BUF_SIZE - 1`, so the two cannot drift apart (PR #7650).
- **SEC-3** (hyphen-prefixed hostname rejection in SSH operand parse) - fixed.
- **SEC-4** (malformed `parent_node_idx`, CVE-2026-43620) - `DirectoryTree::try_add_directory` validates the wire-supplied parent index and returns `DirTreeError::OutOfBoundsParent`; three regression tests in `crates/protocol/src/flist/dir_tree.rs` pin both the graceful-reject path and the worst-case controlled-panic path (no SIGSEGV).

### SEC-1 implementation record (CVE-2026-29518 / CVE-2026-43619)

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

## Upstream rsync 3.4.2 audits

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
