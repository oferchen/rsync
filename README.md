[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.4.4 (and back-compat with 3.4.3 / 3.4.2 / 3.4.1, protocol 32), works as a drop-in replacement.

Binary name: **`oc-rsync`** - installs alongside system `rsync` without conflict.

---

## Status

**Release:** 0.6.3 - Wire-compatible drop-in replacement for rsync 3.4.4 (and 3.4.3 / 3.4.2 / 3.4.1, protocols 28-32).

All transfer modes (local, SSH, daemon), delta algorithm, metadata preservation, incremental recursion, and compression are complete. Interop tested against upstream rsync 2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3, and 3.4.4. Upstream rsync's own `testsuite/*.test` corpus runs in CI against `oc-rsync` as `$RSYNC` - all tests now pass (known-failures roster is empty).

| Component | Status |
|-----------|--------|
| **Transfer** | Local, SSH, daemon push/pull, daemon-over-remote-shell (`host::module`) |
| **Delta** | Rolling + strong checksums, block matching, parallel receive-delta pipeline |
| **Metadata** | Permissions, timestamps, ownership, ACLs (`-A`), xattrs (`-X`) |
| **File handling** | Sparse, hardlinks, symlinks, devices, FIFOs |
| **Deletion** | `--delete` (before/during/after/delay), `--delete-excluded` |
| **Compression** | zlib, zstd, lz4 with level control and auto-negotiation |
| **Checksums** | MD4, MD5, XXH3/XXH128 with SIMD (AVX2, SSE2, NEON) |
| **Incremental recursion** | Pull and push directions, enabled by default |
| **Batch** | `--write-batch` / `--read-batch` roundtrip |
| **Daemon** | Negotiation, auth, modules, chroot, syslog, pre/post-xfer exec |
| **Filtering** | `--filter`, `--exclude`, `--include`, `.rsync-filter`, `--files-from` |
| **Reference dirs** | `--compare-dest`, `--link-dest`, `--copy-dest` |
| **Options** | `--delay-updates`, `--inplace`, `--partial`, `--iconv`, fuzzy matching |
| **I/O** | io_uring (Linux 5.6+), `copy_file_range`, `clonefile` (macOS), adaptive buffers |
| **Memory** | Flat file list with arena allocation for efficient scaling at high file counts |
| **Platforms** | Linux, macOS (full); Windows (NTFS DACL partial, xattrs via NTFS ADS, IOCP file + socket I/O, symlinks with junction fallback; no POSIX device nodes) |

### Platform Support

| Platform | Tier | Notes |
|---|---|---|
| Linux x86_64 / aarch64 | **Tier 1** | Full `io_uring` + `splice` + `vmsplice` + Landlock + seccomp. Every required CI cell runs the full nextest workspace. Production deployment target. |
| macOS x86_64 / aarch64 | **Tier 1** | `kqueue` + `sendfile` + `clonefile`. Every required CI cell runs the full nextest workspace. Full metadata, ACL, and xattr parity including AppleDouble (`._foo`) resource-fork preservation. |
| Windows x86_64 | **Tier 2** | IOCP file I/O, `TransmitFile`, ReFS reflink, `CopyFileExW`, `FILE_FLAG_DELETE_ON_CLOSE`. `splice` / `vmsplice` / `io_uring` are Linux-only and intentionally not implemented; the receiver uses IOCP-batched `WriteFile`, which is faster than the upstream Cygwin `read`/`write` fallback. NTFS DACL preservation, xattrs via NTFS Alternate Data Streams, and IOCP socket I/O (`WSARecv` / `WSASend`) are shipped; POSIX symlinks are materialized (directory links fall back to a junction when unprivileged, unprivileged file symlinks are skipped with a warning), while POSIX device nodes / FIFOs remain stubbed in line with NTFS limits. Required CI cells test the `core`, `engine`, and `cli` crates. See [Windows support matrix](docs/user/windows-support-matrix.md) and the [Windows Tier 2 stub inventory](docs/audits/win-tier2-stub-inventory.md). |

Tier definitions: **Tier 1** means every required CI cell runs the full nextest workspace and the platform is a primary production target. **Tier 2** means the platform builds and runs core transfer modes, required CI cells run a crate-scoped subset of the workspace, and some upstream-testsuite tests may be expected to fail under Cygwin-equivalent feature gaps. Tier 2 is a deliberate choice, not a defect: see `docs/audits/win-tier2-stub-inventory.md` for the structural rationale and the path to Tier 1 promotion.

The primary platform is Linux. macOS is well-supported with parity for all metadata, ACL, and xattr features, including AppleDouble (`._foo`) resource-fork preservation. Windows builds and runs core transfer modes with NTFS DACL preservation (via `windows-rs` `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW`, currently Tier 1C partial - see `docs/platform-notes.md` for the Windows ACL behavior summary, the **--acls** entry in `docs/oc-rsync.1.md`, and `docs/design/windows-ntfs-acl-support.md` for the documented lossy cases), xattrs (via NTFS Alternate Data Streams), and IOCP socket I/O (`WSARecv`/`WSASend`); symlinks are materialized (directory links fall back to junctions when unprivileged), while POSIX device nodes remain stubbed.

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Permissions (`-p`) | ✓ | ✓ | ⚠ | Windows preserves only the read-only flag; POSIX mode bits are not applicable. |
| Times (`-t`) | ✓ | ✓ | ✓ | Nanosecond precision via the `filetime` crate on all platforms. |
| File ownership (`-o`/`-g`, uid/gid) | ✓ | ✓ | ✗ | `apply_ownership_from_entry` is a no-op on non-Unix; uid/gid mapping is Unix-only. |
| ACLs (`-A`) | ✓ | ✓ | ⚠ | Uses `exacl` on Linux/macOS/FreeBSD. Windows uses `windows-rs` `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW` for NTFS DACL round-trip (Tier 1C partial): deny ACEs, inherited ACEs, the SACL, non-`rwx` access bits, and unresolvable SIDs are dropped with a one-time warning. SDDL fidelity payload, `--audit-acls`, `--fail-on-windows-acl-loss`, and `--windows-acls` are planned. See `docs/user-guide/acl-id-mapping.md` for receiver-side UID/GID mapping behaviour (matching upstream 3.4.2), `docs/platform-notes.md` for the Windows ACL behavior summary, `docs/design/windows-ntfs-acl-support.md` for the full mapping matrix, and the **--acls** entry in `docs/oc-rsync.1.md`. |
| Extended attributes (`-X`) | ✓ | ✓ | ✓ | Linux/macOS via the `xattr` crate (macOS adds AppleDouble resource-fork support); Windows stores xattrs as NTFS Alternate Data Streams. |
| Hardlinks (`-H`) | ✓ | ✓ | ✓ | Uses portable `std::fs::hard_link`; works on NTFS. |
| Symbolic links | ✓ | ✓ | ⚠ | Windows materializes symlinks via `create_windows_symlink`: directory links fall back to a junction (`FSCTL_SET_REPARSE_POINT`) when unprivileged, file links require Administrator or Developer Mode and are otherwise skipped with a warning (soft exit 23). See `crates/fast_io/src/win_symlink.rs`. |
| Devices/specials (`-D`) | ✓ | ✓ | ✗ | `create_fifo` and `create_device_node` are no-ops on non-Unix. |
| Sparse files (`-S`) | ✓ | ✓ | ⚠ | Uses portable `seek` + `set_len`; depends on filesystem (NTFS supports sparse but is not explicitly marked via `FSCTL_SET_SPARSE`). |
| Async I/O backend | ✓ io_uring | ⚠ standard I/O | ⚠ IOCP (writes + sockets) | io_uring runtime-detected on Linux 5.6+. IOCP is wired into the disk-write pipeline (`transfer::disk_commit::Writer::Iocp`) and into the socket transports (daemon and SSH); file reads still use standard buffered I/O. |
| Reflink / clone copy | ✓ FICLONE | ✓ clonefile | ⚠ ReFS reflink | Linux Btrfs/XFS/bcachefs via `FICLONE`; macOS via `clonefile`; Windows via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS only). |
| Optimized file copy | ✓ `copy_file_range` | ✓ `fcopyfile` | ✓ `CopyFileExW` | All three are wired into the local-copy executor with standard-I/O fallback. |

Legend: ✓ supported, ⚠ partial or not yet wired, ✗ not implemented.

### What's New (v0.6.3)

**Daemon features**
- `pre-xfer exec` / `post-xfer exec` directives with `RSYNC_ARG#` env vars and stdout capture (#5503)
- `--password-command` option for daemon authentication (#5500)
- Parse missing upstream `rsyncd.conf` directives and warn on unknown keys (#5489)
- `host::module` syntax routes through SSH to a remote daemon (`--server --daemon` mode) (#5353, #5364)
- `RSYNC_CONNECT_PROG` support for custom connection programs (#5317)

**Transfer options**
- Forward `--stop-at` deadline to remote server in SSH transfers (#5499)
- Forward `--remote-option` (`-M`) args to remote rsync process (#5498)
- Wire `--compress-threads` through transfer pipeline to zstd encoder (#5496)
- Embed filter rules in batch replay scripts (#5495)
- Wire `--info` subcategory dispatch to thread-local verbosity config (#5494)

**Performance**
- Optimize generator no-change scan path for 100K-file scale - pre-computed config flags, skip metadata/ACL/xattr when disabled, avoid per-file allocations (#5466, #5468)
- Eliminate redundant stat calls in metadata no-change path (#5492)
- Compact `FileEntry` from 88 to 80 bytes per entry (#5481)
- Unify multiplex flush discipline across transfer roles (#5464)
- Reduce per-file overhead in SSH push/pull paths (#5469, #5470, #5471)
- Reclaim completed INC_RECURSE flist segments to reduce RSS (#5467)
- Increase checksum read buffer from 64KB to 256KB (#5460)
- Tune mimalloc arena reservation and purge delay for lower RSS (#5488)
- Reuse readdir buffer and replace `Path::join` with `PathBuf::push/pop` in traversal (#5483, #5484)
- Tune russh client config for faster SSH handshake (#5490)

**Bug fixes**
- Align daemon `@ERROR` responses with upstream rsync wording (#5504)
- Forward `--trust-sender` and `--checksum-seed` to remote server (#5501)
- Wire `--contimeout` to embedded SSH (russh) connection path (#5497)
- Increase default daemon listen backlog from 5 to 128 (#5487)
- Daemon module listing protocol aligned with upstream behavior (#5366)
- Socketpair used instead of pipes for `RSYNC_CONNECT_PROG` child stdin (#5363)

**Upstream testsuite parity**
- All upstream `testsuite/*.test` scripts pass with zero known failures (#5342, #5346, #5355, #5358)
- Dedicated CI workflow runs the upstream testsuite on every push with UPASS detection (#5342)

**Security**
- SEC-1 TOCTOU sandbox promoted from MOSTLY FIXED to COMPLETE - all path-based daemon syscalls now use `*at` dirfd-scoped equivalents (#4693, #4690, #4683)

**CI improvements**
- All GitHub Actions pinned to SHA hashes for supply chain safety (#5333)
- Nextest profiles, `--locked` builds, and standardized cache keys (#5335, #5332, #5341)

### What's New (v0.6.2)

**Bug fixes**
- `--include='*/'` followed by `--exclude='*'` now allows directory traversal while excluding leaf files, matching upstream byte-for-byte (#4107). Slash modifiers are stripped before wire serialisation and rebuilt on the receiver.
- `--relative` single-source daemon-push path stripping aligned with upstream (#4074)

**`--info` producer emissions (upstream parity)**
- `--info=NONREG` (default-on) reports skipped non-regular source files (`generator.c:1687`)
- `--info=MOUNT` reports `--one-file-system` mount-boundary skips (`flist.c:1319`, `generator.c:325`)
- `--info=SYMSAFE` reports unsafe symlink rejections (`backup.c:291`, `flist.c:216`)
- `--info=BACKUP` reports `--backup` rename successes (`backup.c:352`)
- `--info=COPY` reports `--copy-dest`/`--link-dest`/`--compare-dest` alt-base resolution (`generator.c:919`)
- `--info=STATS` level 1/2/3 distinction restored - level 1 emits only the trailing summary, level 2 adds the file-count detail block, level 3 surfaces heap stats (`main.c:416-465`)

**`--debug` producer emissions (upstream parity)**
- `--debug=ACL` daemon-side default-ACL probes (`acls.c:1133-1134`)
- `--debug=BACKUP` rename/replace/cross-device internal decisions
- `--debug=BIND` `socket()`/`bind()` failure diagnostics per address family (`socket.c:432-470`)
- `--debug=CHDIR` post-`chroot` `change_dir` notice in the daemon (`util1.c:1168-1169`)
- `--debug=CMD` SSH command construction and secluded-args transmission (`util1.c:98-117`, `pipe.c:54-55`)
- `--debug=FUZZY` fuzzy basis selection (`generator.c`)
- `--debug=GENR` generator entry points
- `--debug=HASH` delta-signature hashtable lifecycle (`hashtable.c:45-103`)
- `--debug=HLINK` (previously merged) hardlink resolution
- `--debug=ICONV` iconv setup/probe lines (`rsync.c:99-145`)
- `--debug=OWN` uid/gid lookup and `chown` diagnostics (`rsync.c:537-545`, `uidlist.c:287-291`)
- `--info=help` / `--debug=help` golden output now byte-matches upstream (`options.c:499-509`)

**INC_RECURSE instrumentation (sender-side)**
- First-byte latency in `send_file_list`
- `wire_to_flat_ndx` / `flat_to_wire_ndx` partition_point counters
- `writer.flush()` call-rate on the transfer hot path
- `prepare_pending_acl` per-segment call count and elapsed time
- `encode_and_send_segment` per-segment dispatch counter

**Compression**
- `--compress-threads=N` flag wires through to `ZSTD_c_nbWorkers` for multi-threaded zstd (3.4.2 parity)

**Checksums**
- SIMD vs scalar self-test added (cargo-fuzz target + unit test) cross-validating AVX2, SSE2, NEON, and scalar implementations at startup (3.4.2 parity)

**Upstream interop**
- Pinned upstream interop matrix simplified to rsync **3.4.4** as the sole 3.4.x cell (alongside 2.6.9, 3.0.9, 3.1.3); 3.4.1/3.4.2/3.4.3 share the same wire protocol and are superseded by 3.4.4
- All upstream `testsuite/*.test` tests now pass - known-failures roster is empty
- Wire differential fuzzing validates protocol-level byte equivalence against upstream
- Scheduled GitHub Actions watcher for new upstream releases

**Code quality**
- Comment audit pass across all 25 workspace crates: restating comments removed, public-item `///` rustdoc applied, every `// upstream:` source reference preserved
- Removed redundant `#[must_use]` from `Result`/`Option` returning functions across the workspace (#2123)
- Filter-rule sender-side directive support
- Compressed-stream negative-token decoder hardened against 3.4.2 regression pattern

**Daemon & metadata**
- Daemon `chrono::Local` pre-initialised before `chroot` so timezone-aware log timestamps survive jail entry (3.4.2 parity)
- `--open-noatime` properly propagated through sender source-file opens (3.4.2 parity)
- ACL ID mapping audited against 3.4.2 non-root mapping fix (#618)
- `clean_fname()` buffer underflow, xattr qsort parity, allocator-zeroing, and Y2038 paths all audited against 3.4.2

**Documentation**
- Deployment / TLS guide expanded
- Filter-flags audit, info-flags audit, and debug-flags-verbosity matrix kept current

### Interop Testing

Tested against upstream rsync **2.6.9**, **3.0.9**, **3.1.3**, and **3.4.4** in CI across protocols 28-32. The 3.4.x series shares protocol 32 and is represented in the matrix by 3.4.4, the latest conservative regression-fix release; 3.4.1/3.4.2/3.4.3 cells are subsumed because they run identical wire scenarios. Both push and pull directions verified for 30+ scenarios covering transfer modes, deletion, compression, metadata, reference dirs, file selection, batch roundtrip, path handling, device nodes, and daemon auth. Wire differential fuzzing against upstream rsync validates protocol-level byte equivalence. See the [full interop compatibility matrix](./docs/user/interop-compatibility-matrix.md) for per-version, per-feature, and per-platform detail.

### Supported rsync protocol versions

oc-rsync negotiates `protocol_version` per upstream, defaults to 32, and supports back-negotiation to 28 inclusive. Protocols 28-29 are exercised via wire-byte regression tests; 30-32 are exercised via the daemon and client interop matrices in CI.

| Protocol | Upstream rsync version | Status in oc-rsync | Notes |
|----------|------------------------|--------------------|-------|
| 32       | 3.4.x (current)        | Full support        | Primary target; all features negotiated |
| 31       | 3.2.x - 3.3.x          | Full support        | Verified via interop matrix |
| 30       | 3.1.x                  | Full support        | Verified via interop matrix |
| 29       | 3.0.x                  | Full support        | Verified via interop matrix; sort.rs t_PATH/t_ITEM gate (RP28.h) |
| 28       | 2.6.x                  | Wire-level support  | Validated via wire-byte regression tests (RP28.g, RP28.h); full interop with upstream 2.6.9 daemon/client tracked under RP28 series |
| <= 27    | <= 2.5.x               | Not supported       | Pre-dates protocol cleanup; not tested |

Per-version dispatch is implemented as `protocol_version` gates in the wire codecs. See [`crates/protocol/src/wire/compressed_token/zlib_codec.rs`](./crates/protocol/src/wire/compressed_token/zlib_codec.rs) and the sibling [`zstd_codec.rs`](./crates/protocol/src/wire/compressed_token/zstd_codec.rs) / [`lz4_codec.rs`](./crates/protocol/src/wire/compressed_token/lz4_codec.rs) for representative examples of the gates that switch on `protocol_version`.

### Supported rsync wire protocol versions

| upstream rsync version | protocol | mode (push/pull/daemon)  | status (CI-verified) |
|------------------------|----------|--------------------------|----------------------|
| 2.6.9                  | 29       | push (daemon)            | non-blocking (RP28.c) |
| 2.6.9                  | 29       | pull (daemon)            | non-blocking (RP28.d) |
| 3.0.9                  | 30       | push, pull, daemon       | gating |
| 3.1.3                  | 31       | push, pull, daemon       | gating |
| 3.4.4                  | 32       | push, pull, daemon, SSH  | gating |

Wire format is verified byte-identical to upstream rsync via CI golden-byte tests for the listed versions. Wire differential fuzzing validates protocol-level byte equivalence against upstream. Other versions may work but are not regression-tested.

### Linux io_uring kernel-tier support

oc-rsync uses io_uring on Linux when the kernel and probed opcodes allow it; below the floor (or on any non-Linux platform) it falls back to standard `read(2)`/`write(2)` and platform-specific paths (IOCP on Windows). The hard kernel floor is Linux 5.6, gated by `MIN_KERNEL_VERSION = (5, 6)` in [`crates/fast_io/src/io_uring/config.rs`](./crates/fast_io/src/io_uring/config.rs); on older kernels the io_uring path is disabled entirely.

| Kernel version | Tier | Opcodes available | Notes |
|----------------|------|-------------------|-------|
| < 5.6          | Unsupported | none - io_uring path disabled | Standard I/O fallback only |
| 5.6 - 5.10     | Basic | READ, WRITE, READ_FIXED, WRITE_FIXED, SEND, RECV, FSYNC, NOP, POLL_ADD, ASYNC_CANCEL | Hard floor; data path enabled |
| 5.11 - 5.14    | Extended | + STATX, RENAMEAT | Metadata fast paths enabled |
| 5.15 - 5.18    | Mature | + LINKAT | Hardlink fast path enabled |
| 5.19 - 6.0     | PBUF-ring | + register_pbuf_ring opcodes | Provided-buffer-ring optimisation |
| >= 6.0         | Full | + SEND_ZC (with `iouring-send-zc` feature) | Zero-copy send tier |

The full tier requires Linux 6.0+ together with the `iouring-send-zc` cargo feature for `SEND_ZC` dispatch; default builds downgrade to plain `SEND` even on 6.0+ kernels. See [`docs/audit/iouring-opcode-kernel-floor.md`](./docs/audit/iouring-opcode-kernel-floor.md) for the full per-opcode dispatch-site inventory.

### SSH transport (russh)

oc-rsync uses the Rust [`russh`](https://crates.io/crates/russh) crate for SSH transport, embedded directly in the binary. The default code path does not spawn an external `ssh` subprocess. Authentication uses key-based (RSA, ED25519, ECDSA) and password methods compatible with OpenSSH, and per-host settings are read from `~/.ssh/config` via the [`ssh2-config`](https://crates.io/crates/ssh2-config) integration (SSC-3 series).

What this changes versus upstream rsync, which shells out to the system `ssh` binary:

- No external `ssh` binary dependency at runtime; the SSH client lives inside the oc-rsync process.
- All SSH state (connection, channel, auth context) lives in the oc-rsync process rather than crossing a pipe to a child.
- `~/.ssh/config` `Match` blocks are honored for the limited subset implemented under the SSC-4 series.
- SSH agent forwarding via `SSH_AUTH_SOCK` is honored when set.

Current limitations:

- Some exotic SSH features (for example SSH-2 keepalive intervals and certificate-based auth) may not be fully supported; please open an issue if you hit one.
- The current `spawn_blocking` thread-pool bridge between the synchronous transfer pipeline and the async russh client throttles daemon concurrency at hundreds of concurrent sessions. The RUSSH-9..14 work moves the SSH transport to a fully async-native path; see [`docs/design/russh-async-native-path.md`](./docs/design/russh-async-native-path.md) for the planned evolution and [`docs/design/russh-async-native-back-compat-shim.md`](./docs/design/russh-async-native-back-compat-shim.md) for the back-compat shim.

See also the `SSH TRANSPORT` section of `oc-rsync(1)` for the man-page summary.

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Threaded architecture replaces upstream's fork-based pipeline while keeping full protocol compatibility, reducing syscall overhead and context switches. Adaptive I/O buffers scale from 8KB to 1MB based on file size. Optional io_uring on Linux 5.6+ with three policies: *auto* (default; probe kernel and fall back to standard I/O), `--io-uring` (require io_uring; error if unavailable), `--no-io-uring` (always use standard buffered I/O). The active backend is reported by `--version` and `-vv` output. See `oc-rsync(1)` for details.

### Performance tuning

#### Avoid SSH + rsync double-compression

SSH stream compression (`-C` on the `ssh` command line, or `Compression yes` in `~/.ssh/config` or `/etc/ssh/ssh_config`) compresses every byte the SSH session carries. The rsync wire protocol has its own compression layer, enabled with `--compress` / `-z` (and tuned with `--compress-choice`, `--compress-level`, `--compress-threads`). Running both at once feeds already-compressed bytes back into a second compressor: the second pass adds CPU on both sides and frame overhead while shrinking the stream by almost nothing, and on CPU-bound hosts throughput typically drops by 20-40%.

Pick one layer. For most workloads prefer rsync's own compression: `oc-rsync` negotiates zstd when both peers support it (falling back to zlib), which is usually faster and tighter than SSH's zlib stream, and it can be skipped per file via `--skip-compress`. If you have a reason to keep SSH compression on (for example, a shared SSH config you cannot edit), drop `-z` from the rsync invocation instead.

```sh
# Good: rsync compresses, SSH carries the bytes as-is.
oc-rsync -avz user@host:/src/ /dst/

# Bad: -C on ssh re-compresses what rsync already compressed.
oc-rsync -avz -e 'ssh -C' user@host:/src/ /dst/
```

`oc-rsync` emits a one-line warning when it spots `-C` (or `-o Compression=yes`) in the `--rsh` / `-e` argv it builds for the SSH child, but it does **not** parse `~/.ssh/config` or `/etc/ssh/ssh_config`, so a `Compression yes` directive set there is invisible to the warning. If throughput looks CPU-bound on an SSH transfer, check those files as well.

#### `--zero-copy` and io_uring `SEND_ZC`

`--zero-copy` advertises io_uring `SEND_ZC` as one of the zero-copy primitives it may dispatch on Linux, alongside `sendfile`, `splice`, and `copy_file_range`. The `SEND_ZC` dispatch itself is gated behind the `iouring-send-zc` cargo feature, which is **not** in the default feature set; the gate is documented as "Disabled by default pending kernel/workload benchmarks" in `crates/fast_io/Cargo.toml`. Default distro builds therefore use plain io_uring `SEND` even when `--zero-copy` is set; the other zero-copy primitives still kick in where the kernel supports them.

To get `SEND_ZC` dispatch, build with `cargo build --features iouring-send-zc` (requires Linux 5.16+). See [`docs/design/iouring-send-zc.md`](./docs/design/iouring-send-zc.md) for the full rationale and the path to flipping this default on.

#### SSH stderr socketpair channel

Default builds drain the SSH child's stderr through an anonymous pipe on a dedicated reader thread. The `ssh-socketpair-stderr` cargo feature swaps that pipe for a `socketpair(AF_UNIX, SOCK_STREAM)` and, on the async transport, hands the parent end to an epoll/kqueue-integrated tokio drain. The wake-up and shutdown paths become event-driven instead of timeout-bounded, and the larger socket buffer absorbs bursty remote shells without dropping lines.

Operators benefit when SSH stderr is chatty (banners, MOTDs, `ssh -v`), the network is high-latency, or many parallel transfers are in flight. In those workloads the pipe-based drain delays or truncates diagnostic output and can leave drain threads parked past child exit. The socketpair path keeps stderr capture deterministic at session boundaries.

Build with:

```sh
cargo build --features ssh-socketpair-stderr
```

Linux is the recommended target; the Windows shim is tracked separately (SSE-5). See [`docs/ssh-transport.md`](./docs/ssh-transport.md) and [`docs/design/socketpair-stderr-channel.md`](./docs/design/socketpair-stderr-channel.md).

Three one-shot warnings may appear on stderr (sync path) or via `tracing` target `ssh::stderr` (async path) when the runtime cannot honour the feature. Each fires at most once per process; the substrings are the operator-grep contract:

- `SSH stderr async drain unavailable on this platform` - the kernel rejected `socketpair(AF_UNIX, SOCK_STREAM, 0)` (typically `EMFILE`, `ENFILE`, `EPERM`, or `ENOSYS` under seccomp). The session falls back to `Stdio::piped()`. Raise the per-process fd limit or relax the sandbox if you want the socketpair drain back.
- `SSH stderr socketpair partially set up` - the socketpair allocated but `dup(2)` on the parent half failed (usually `EMFILE`). The drain still reads from the socketpair, but `shutdown_read` becomes a no-op and the drain thread relies on a 50 ms timeout at child exit. Investigate fd pressure in the parent process.
- `SSH stderr async drain falling back to Stdio::inherit()` - the async transport could not stand up the socketpair, so the SSH child's stderr is wired straight to the parent terminal. `stderr_capture()` returns empty for this session; consume diagnostics from the parent's own stderr instead.

### Known Limitations / Architectural Trade-offs

oc-rsync is wire-compatible with upstream rsync 3.4.4, but a few architectural choices and unfinished surfaces are worth calling out for operators planning a deployment:

- **io_uring kernel requirement.** Provided buffer rings (PBUF_RING) require Linux **5.19+**; older 5.6-5.18 kernels fall back to standard buffered I/O via runtime probing.
- **Fixed io_uring buffer pool.** The registered buffer pool is sized at compile time (1024 × 4 KiB = 4 MiB) and does not adapt under sustained I/O pressure. Workloads with very high concurrent file fan-out may see throughput plateau before saturating the device.
- **bgid namespace.** io_uring buffer-group IDs are a 16-bit namespace; the buffer ring helpers cap at this bound. Long-running daemons that recycle thousands of distinct ring groups should monitor for exhaustion.
- **Single-thread delta computation.** The delta sender is sequential per file. Rolling-hash fan-out across files is not yet parallelised; large-file workloads fully utilise one CPU per transfer rather than scaling delta CPU horizontally.
- **SSH compression interaction.** When the SSH cipher already performs compression (e.g., `Compression yes` in `ssh_config`), running `oc-rsync -z` will compress payloads twice. There is currently no auto-detection / auto-disable path; operators should pick one layer.
- **Daemon encryption.** The daemon protocol is plaintext, matching upstream rsync (authentication only, no encryption). Encrypt with the ssh transport, or place the daemon behind an SSL proxy (`stunnel`, HAProxy, nginx) and connect with `--ssl` (behind the `client-tls` feature flag, rustls-based), matching upstream `rsync-ssl`.
- **Windows IOCP scope.** IOCP is wired for socket I/O (daemon and SSH transports) and for the receive-side disk-write pipeline (`transfer::disk_commit` dispatches `Writer::Iocp` when the IOCP backend is selected on Windows). File reads still use standard buffered I/O; extending IOCP to the read path is tracked in WPG-1.
- **`.rsync-filter` per-directory inheritance.** Inheritance semantics match upstream for the common cases tested in the interop suite, but exhaustive parity against upstream's filter-tree corner cases (deeply nested merges, anchored vs unanchored interactions) is still being validated.
- **`--checksum-seed` / `--fuzzy`.** These flags are accepted and exercised in the common path; deeper conformance audits against upstream rsync 3.4.1 are tracked separately.

---

## Installation

### Homebrew

```bash
brew tap oferchen/rsync https://github.com/oferchen/rsync
brew install oferchen/rsync/oc-rsync
```

### Prebuilt packages

Download from the [Releases](https://github.com/oferchen/rsync/releases) page:

| Platform | Formats |
|----------|---------|
| Linux (x86_64, aarch64) | `.deb`, `.rpm` (with OpenSSL), static musl `.tar.gz` |
| macOS (x86_64, aarch64) | `.tar.gz` |
| Windows (x86_64) | `.tar.gz`, `.zip` |

Linux static tarballs are available in two checksum variants:

| Variant | Filename | Description |
|---------|----------|-------------|
| **Pure Rust** (recommended) | `*-musl.tar.gz` | Pure-Rust checksums, zero system dependencies |
| **OpenSSL** | `*-musl-openssl.tar.gz` | OpenSSL-accelerated MD4/MD5 checksums (vendored, statically linked) |

Each release also includes three toolchain variants: **stable** (recommended, no suffix), **beta** (`-beta`), and **nightly** (`-nightly`).

### Build from source

Requires Rust **1.88+**.

```bash
git clone https://github.com/oferchen/rsync.git
cd rsync
cargo build --workspace --release
```

### Cargo features

The workspace exposes the following opt-in features. Defaults are tuned for the
release binary on modern Linux/macOS/Windows hosts; everything marked
`experimental` is wired but not yet promoted to the default set.

| Feature | Crate(s) | Default | Purpose | Status |
|---------|----------|:-------:|---------|--------|
| `zstd` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol`, `batch` | yes | Enables zstd compression codec and wire negotiation. | stable |
| `lz4` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol` | yes | Enables LZ4 compression codec. | stable |
| `zlib-ng` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol` | no | Selects the SIMD-accelerated `zlib-ng` C backend instead of pure-Rust zlib. | stable |
| `xattr` | workspace, `cli`, `core`, `transfer`, `daemon`, `metadata` | yes | Preserves extended attributes (`-X`) on Unix and NTFS ADS on Windows. | stable |
| `acl` | workspace, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata` | yes | Preserves POSIX/NFSv4 ACLs (`-A`) via `exacl`, NTFS ACLs via `windows-rs`. | stable |
| `iconv` | workspace, `cli`, `core`, `transfer`, `daemon`, `protocol` | yes | Filename and symlink-target charset transcoding (`--iconv`). | stable |
| `parallel` | workspace, `cli`, `engine`, `checksums` | yes | Rayon-based multi-core file and checksum operations. | stable |
| `io_uring` | workspace, `transfer`, `fast_io` | yes | Linux 5.6+ batched async I/O with runtime fallback. | stable |
| `iocp` | workspace, `transfer`, `fast_io` | yes | Windows I/O Completion Ports for overlapped file and socket I/O. | stable |
| `copy_file_range` | workspace, `fast_io` | yes | Compat alias; the `copy_file_range` syscall is now always compiled with runtime detection. | stable |
| `async` | workspace, `core`, `engine`, `transfer`, `daemon` | yes | Brings in tokio for async I/O paths across the orchestrator stack. | stable |
| `openssl` | workspace, `checksums` | no | Routes MD4/MD5 through the system OpenSSL build. | stable |
| `openssl-vendored` | workspace, `checksums` | no | Same as `openssl` but statically links a vendored OpenSSL. | stable |
| `embedded-ssh` | workspace, `core`, `rsync_io` | no | Pure-Rust SSH client via `russh`; removes the runtime dependency on system `ssh`. | stable |
| `sd-notify` | workspace, `core`, `daemon` | no | systemd `sd-notify` integration for the daemon. | stable |
| `client-tls` | workspace, `core` | no | Native TLS connector for `rsync://` client connections via rustls. Adds the `--ssl` flag. | stable |
| `incremental-flist` | `transfer` | yes | Incremental file-list processing with failed-directory tracking. | stable |
| `lazy-metadata` | `engine` | yes | Defers `stat()` calls until metadata is needed. | stable |
| `multi-producer` | `engine` | no | Relaxes the single-producer compile-time invariant on `WorkQueueSender`. | experimental |
| `thread-slab-pool` | `engine` | no | Per-thread bounded LIFO slab in front of `BufferPool`; pays off above ~32 workers (#1271, #1370). | experimental |
| `vmsplice` | `fast_io`, `transfer` | no | Linux `vmsplice(2)` + `splice(2)` zero-copy writer for large page-aligned chunks. | experimental |
| `async-ssh` | `core`, `rsync_io` | no | Wires `AsyncSshTransport` into the client remote path; opt-in at runtime via `OC_RSYNC_ASYNC_SSH=1` (#1593, #1796, #1805, #1806). | experimental |
| `ssh-socketpair-stderr` | `rsync_io` | no | Constructs the SSH child's stderr over a `socketpair(AF_UNIX, SOCK_STREAM)` instead of an anonymous pipe, enabling epoll/kqueue-integrated async drain and a larger default buffer to absorb chatty remote shells (Linux recommended; Windows shim pending SSE-5). See [`docs/ssh-transport.md`](./docs/ssh-transport.md) and [`docs/design/socketpair-stderr-channel.md`](./docs/design/socketpair-stderr-channel.md) (#2371, #2372). | experimental |
| `async-daemon` | `daemon` | no | Hybrid tokio accept loop dispatching sync workers via `spawn_blocking` (#1935). | experimental |
| `concurrent-sessions` | `daemon` | no | Shared `dashmap` session state for multi-session daemons. | experimental |
| `tracing` | `core`, `engine`, `transfer`, `daemon` | no | Structured `tracing` instrumentation for diagnostics. | stable |

#### Receiver memory tuning: `SpillPolicy`

The concurrent-delta receiver bounds its in-memory `ReorderBuffer` through a
process-wide `SpillPolicy` knob. The default policy keeps everything in memory
(byte-equivalent to prior releases). Opt in to disk-backed spill by setting
`OC_RSYNC_SPILL_THRESHOLD_BYTES` (e.g. `64M`) and, optionally,
`OC_RSYNC_SPILL_DIR` to point at a fast scratch directory. CLI flags
`--spill-dir` and `--spill-threshold-bytes` are planned for STN-11 and will
shadow the env vars when present. Full surface (env vars, reclaim mode,
granularity, compression, validation rules, defaults table) is in
[`docs/design/spill-policy-public-api.md`](./docs/design/spill-policy-public-api.md);
the underlying buffer is documented in
[`docs/design/reorderbuffer-spill-to-tempfile.md`](./docs/design/reorderbuffer-spill-to-tempfile.md).
Operator-facing tuning guidance is in
[`docs/operator-migration-guide-vNEXT.md`](./docs/operator-migration-guide-vNEXT.md)
under *Receiver spill tunability*.

#### Choosing features for your build

```bash
# Default release build (recommended starting point).
cargo build --workspace --release

# Enable the async network paths end-to-end. async-ssh and async-daemon are
# crate-level features, so build the affected crates directly.
cargo build --release -p core --features async-ssh
cargo build --release -p daemon --features async-daemon

# Squeeze out maximum parallelism on a fat machine: keep the workspace
# defaults and add the experimental engine slab and vmsplice writer.
cargo build --workspace --release \
    --features "parallel async io_uring" \
  && cargo build --release -p engine --features thread-slab-pool \
  && cargo build --release -p fast_io --features vmsplice
```

---

## Usage

Works like `rsync` - drop-in compatible:

```bash
# Local sync
oc-rsync -av ./source/ ./dest/

# Remote pull (SSH)
oc-rsync -av user@host:/remote/path/ ./local/

# Remote push (SSH)
oc-rsync -av ./local/ user@host:/remote/path/

# Daemon pull
oc-rsync -av rsync://host/module/ ./local/

# Daemon push
oc-rsync -av ./local/ rsync://host/module/

# Daemon over remote shell (SSH-based daemon access)
oc-rsync -av host::module/path/ ./local/

# Run as daemon
oc-rsync --daemon --config=/etc/oc-rsyncd/oc-rsyncd.conf

# Delta transfer with compression
oc-rsync -avz --compress-level=3 ./source/ ./dest/

# Checksum-based sync with deletion
oc-rsync -avc --delete ./source/ ./dest/

# Batch mode (record and replay)
oc-rsync -av --write-batch=changes ./source/ ./dest/
oc-rsync -av --read-batch=changes ./other-dest/
```

For supported options: `oc-rsync --help`

---

## Development

### Prerequisites

- Rust 1.88.0 (managed via `rust-toolchain.toml`)
- [`cargo-nextest`](https://nexte.st/): `cargo install cargo-nextest --locked`

### Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
cargo nextest run --workspace --all-features
```

### Project layout

```text
src/bin/oc-rsync.rs     # Entry point
crates/cli/             # CLI flags, help, output formatting
crates/core/            # Orchestration facade, session management, config
crates/protocol/        # Wire protocol (v28-32), multiplex framing
crates/transfer/        # Generator, receiver, delta transfer pipeline
crates/engine/          # Local copy executor, sparse writes, temp-file commit
crates/daemon/          # Daemon mode, module access control, systemd
crates/checksums/       # Rolling and strong checksums (MD4, MD5, XXH3, SIMD)
crates/filters/         # Include/exclude pattern engine, .rsync-filter
crates/metadata/        # Permissions, uid/gid, mtime, ACLs, xattrs
crates/platform/        # Platform-specific unsafe code isolation (signals, chroot)
crates/rsync_io/        # SSH stdio, rsync:// TCP transport, handshake
crates/fast_io/         # Platform I/O (io_uring, copy_file_range, sendfile)
crates/compress/        # zstd, lz4, zlib compression codecs
crates/bandwidth/       # Bandwidth limiting and rate control
crates/signature/       # Signature layout and block-size calculations
crates/matching/        # Delta matching and block search
crates/flist/           # File list generation and traversal
crates/logging/         # Logging macros and verbosity control
crates/logging-sink/    # Message sink and output formatting
crates/batch/           # Batch file read/write support
crates/branding/        # Binary naming and version metadata
crates/embedding/       # Programmatic entry points for library usage
crates/apple-fs/        # macOS filesystem operations (clonefile, FSEvents)
crates/windows-gnu-eh/  # Windows GNU exception handling shims
crates/test-support/    # Shared test utilities (dev-dependency only)
```

See `cargo doc --workspace --no-deps --open` for API documentation.

### Architecture

```text
cli -> core -> engine, daemon, rsync_io, logging
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
                                                                            -> platform
```

Key crates: **cli** (Clap v4), **core** (orchestration facade), **protocol** (wire v28-32, multiplex framing), **transfer** (generator/receiver, delta pipeline), **engine** (local copy, sparse writes, buffer pool), **checksums** (MD4/MD5/XXH3, SIMD), **daemon** (TCP, auth, modules), **platform** (unsafe code isolation).

---

## Security

All crates enforce `#![deny(unsafe_code)]`. Targeted `#[allow(unsafe_code)]` is permitted only in crates that wrap platform FFI or SIMD intrinsics:

- **checksums** - SIMD intrinsics (AVX2, AVX-512, SSE2, SSSE3, SSE4.1, NEON, WASM) with scalar fallbacks
- **fast_io** - io_uring, `copy_file_range`, sendfile, mmap, IOCP, `WSARecv`/`WSASend`, `setsockopt`, and the `signal::install_signal_handler` FFI wrapper, with standard I/O / no-op fallbacks
- **metadata** - UID/GID lookup, timestamps, ownership, xattrs, ACLs
- **platform** - chroot, daemonize, name resolution, privilege transitions (signal-handler installation moved to `fast_io::signal`)
- **engine** - Buffer pool atomics, deferred fsync, clonefile, `CopyFileExW`
- **protocol** - One isolated allow in `multiplex::helpers` for frame parsing
- **windows-gnu-eh** - Windows GNU exception handling shims

Not vulnerable to known upstream rsync CVEs (CVE-2024-12084 through CVE-2024-12088, CVE-2024-12747). All CVE mitigations complete (SEC-1, SEC-2, SEC-3, SEC-MK series). TOCTOU path-based syscalls replaced with `*at` variants throughout.

### Upstream rsync 3.4.3 hardening

- **TOCTOU mitigation for path-based daemon syscalls** (CVE-2026-29518, CVE-2026-43619): the receiver routes every mutating filesystem call through a `DirSandbox` carrier anchored on an `O_DIRECTORY | O_NOFOLLOW` root dirfd, with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` runtime detection. Coverage spans the full `*at` syscall family: `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `fchmodat`, `fchownat`, `utimensat`, `renameat`, and `fstatat`. macOS provides the same `*at` semantics and is verified; Windows uses NTFS handle-based APIs, where the path TOCTOU window does not apply.
- **Defense-in-depth (Linux only, cargo feature `landlock`)**: the daemon engages a kernel-side allowlist over the resolved module root via the Landlock LSM. A per-connection `restrict_self()` runs immediately after `apply_module_privilege_restrictions` returns, so any filesystem syscall that resolves a path outside the module root is rejected with `EACCES` regardless of which syscall the userspace code chose. Requires Linux 5.13+ (ABI v1), 5.19+ (v2 adds `REFER` for cross-directory renames), or 6.2+ (v3 adds `TRUNCATE`). Best-effort ABI downgrade picks the highest level the running kernel exposes; on pre-5.13 kernels the `*at` helpers remain the sole defense. See [`docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`](./docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md).
- **CONNECT proxy bounded-read** (CVE-2026-45232): the HTTP CONNECT response-line parser caps line length at the upstream-aligned ceiling, so the C off-by-one stack write is structurally impossible against a heap `Vec<u8>` push path.
- **Hyphen-prefixed remote-shell hostname rejection** (rsync 3.4.3 hardening): the SSH operand parser rejects hostnames that begin with `-`, blocking the `-oProxyCommand=`-style argument-injection class.

See [`SECURITY.md`](./SECURITY.md) for full status, advisory cross-references, and known-pending follow-ups.

---

## Contributing

1. Fork and create a feature branch.
2. Run `cargo fmt --all` locally; CI handles `clippy` and `nextest`.
3. Open a PR with a conventional-commit prefix describing behavioural changes and interop impact.

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the full local-vs-CI workflow, feature-flag conventions, and PR guidelines.

---

## License

GNU GPL v3.0 or later. See [`LICENSE`](./LICENSE).

---

## Acknowledgements

Inspired by [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team.

Internal matching-engine optimisations adapted from [`zsync`](http://zsync.moria.org.uk/) by Colin Phipps (in-memory only; wire format stays pure rsync).

Thanks to **Pieter** for his heroic patience in enduring months of my rsync commentary.
Thanks to **Elad** for his endless patience hearing rsync protocol commentary as I'm introduced to it.
