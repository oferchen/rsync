[![RepoGrade](https://www.repo-grade.com/api/badge/oferchen/rsync)](https://www.repo-grade.com/report/oferchen/rsync)
[![Ask DeepWiki](https://img.shields.io/badge/Ask-DeepWiki-blue)](https://deepwiki.com/oferchen/rsync)

[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg?branch=master&event=push)](https://github.com/oferchen/rsync/actions/workflows/ci.yml?query=branch%3Amaster)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Upstream Rsync Testsuite 3.5.1](https://img.shields.io/github/actions/workflow/status/oferchen/rsync/upstream-testsuite-3.5.1.yml?branch=master&event=schedule&label=Upstream%20Rsync%20Testsuite%203.5.1)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-3.5.1.yml?query=branch%3Amaster+event%3Aschedule)

[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)


# oc-rsync

`rsync` re-implemented in Rust. The reference version is upstream rsync 3.5.1. oc-rsync speaks protocols 28 through 33 and aims to be a drop-in replacement. Known divergences are listed per test in the [upstream testsuite](#upstream-testsuite) manifests.

The binary is named **`oc-rsync`**, so it installs alongside the system `rsync` without conflict.

---

## Status

**Release:** 0.6.4 (2026-07-18). Changes merged since then are listed under *Unreleased* in the [CHANGELOG](./CHANGELOG.md). All transfer modes (local, SSH, daemon), the delta algorithm, metadata preservation and compression are implemented; the table below lists the gaps.

**Upstream reference.** Behaviour, source citations and the testsuite gate all track rsync 3.5.1 (`upstream_version` in `Cargo.toml`). 3.5.1 raised the protocol to 33. oc-rsync speaks 33, including `MSG_BLOCK_STATS` and the `--stats` touched-blocks line (#8003). It negotiates down to 28 for older peers and caps a newer peer at 33 instead of refusing it (#7916, #8209). The options and daemon directives added in 3.5.0 are implemented (see *Path confinement* below). The per-CVE audit is in [`SECURITY.md`](./SECURITY.md).

| Component | Status |
|-----------|--------|
| **Transfer** | Local, SSH, daemon push/pull, daemon over remote shell (`host::module`) |
| **Delta** | Rolling + strong checksums, block matching |
| **Metadata** | Permissions, timestamps, ownership, ACLs (`-A`), xattrs (`-X`) |
| **File handling** | Sparse, hardlinks, symlinks, devices, FIFOs |
| **Deletion** | `--delete` (before/during/after/delay), `--delete-excluded` |
| **Compression** | zlib, zstd, lz4 with level control and negotiation |
| **Checksums** | MD4, MD5, XXH3/XXH128 with SIMD (AVX2, SSE2, NEON) |
| **Incremental recursion** | Negotiated whenever oc-rsync sends, when an oc-rsync server or daemon receives a push (#8169), and on client pulls by default (#8062, #8091, #8217). `OC_RSYNC_PULL_INC_RECURSE=0` opts a pull out. `--delete-before` and `--delete-after` need the whole list and withhold it as in upstream. |
| **Batch** | `--write-batch` / `--read-batch` round trip |
| **Daemon** | Negotiation, auth, modules, chroot, syslog, pre/post-xfer exec |
| **Host names** | Internationalized host names converted to IDNA A-labels, as upstream 3.5.1 does (#8104) |
| **Filtering** | `--filter`, `--exclude`, `--include`, `.rsync-filter`, `--files-from` |
| **Reference dirs** | `--compare-dest`, `--link-dest`, `--copy-dest` |
| **Options** | `--delay-updates`, `--inplace`, `--partial`, `--iconv`, fuzzy matching |
| **Path confinement** | `--confine-root`, `--drop-D` / `--no-drop-D`, `--insecure-links` / `--no-insecure-links`; daemon `auth digest`, `insecure links`, `proxy protocol hosts` |
| **I/O** | io_uring (Linux 5.6+), `copy_file_range`, `clonefile` (macOS), adaptive buffers |

### Upstream testsuite

Upstream's own 3.5.1 test suite runs against `oc-rsync` as `$RSYNC` on every pull request and nightly on master, in **eight legs**: platform {Linux, macOS} x daemon transport {stdio pipe, loopback TCP} x privilege {non-root, root}. The pipe legs run the whole 360-test corpus. The TCP legs add `--daemon-tests-only` and run the 161 tests that start a daemon. The legs run from [`upstream-testsuite-3.5.1.yml`](./.github/workflows/upstream-testsuite-3.5.1.yml), behind the 3.5.1 badge at the top.

All eight legs are required status checks.

Outcomes, counted from each leg's committed manifest:

| leg | pass | fail | skip |
|---|---:|---:|---:|
| Linux, non-root, pipe | 274 | 0 | 86 |
| Linux, root, pipe | 305 | 0 | 55 |
| Linux, non-root, tcp | 129 | 0 | 32 |
| Linux, root, tcp | 149 | 0 | 12 |
| macOS, non-root, pipe | 248 | 0 | 112 |
| macOS, root, pipe | 278 | 0 | 82 |
| macOS, non-root, tcp | 126 | 0 | 35 |
| macOS, root, tcp | 144 | 0 | 17 |

Re-derive any cell, and list any failing tests (the outcome is the second field; a `fail` row carries its cause and owner in a trailing comment):

```sh
awk '!/^#/ && NF {c[$2]++; t++} END {print t, c["pass"], c["fail"], c["skip"]}' \
  tools/ci/upstream-3.5.1-expect.nonroot.txt
awk '!/^#/ && $2=="fail" {print $1}' tools/ci/upstream-3.5.1-expect.*.txt | sort -u
```

**No 3.5.1 test fails on any leg.** The last one, `proto-subflist-freed`, passes since the server receiver refuses a sub-list that arrives after the final file list was freed, as upstream `flist.c` does (#8193). Every remaining non-pass row is a `skip` with a reason. Only a *change* in outcome turns a leg red, including an unexpected pass, so a divergence cannot be re-baselined silently: the PR that fixes a cell must also flip its row to `pass`.

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
| Linux (x86_64, aarch64) | `.deb`, `.rpm`, Alpine `.apk`, static musl `.tar.gz` |
| macOS (x86_64, aarch64) | `.tar.gz` |
| Windows (x86_64) | `.tar.gz`, `.zip` |

Linux static tarballs come in two checksum variants:

| Variant | Filename | Description |
|---------|----------|-------------|
| **Pure Rust** (recommended) | `*-musl.tar.gz` | No system dependencies |
| **OpenSSL** | `*-musl-openssl.tar.gz` | OpenSSL MD4/MD5, vendored and statically linked |

Each release also ships three toolchain builds: **stable** (no suffix, recommended), **beta** (`-beta`) and **nightly** (`-nightly`).

### Build from source

Requires Rust **1.89** or newer. The repository pins 1.89.0 in `rust-toolchain.toml`.

```bash
git clone https://github.com/oferchen/rsync.git
cd rsync
cargo build --workspace --release
```

### Cargo features

| Feature | Crate(s) | Default | Purpose | Status |
|---------|----------|:-------:|---------|--------|
| `zstd` | workspace, `cli`, `core`, `engine`, `transfer`, `compress`, `protocol`, `batch` | yes | zstd codec and negotiation. | stable |
| `zstdmt` | `compress` | no | Multithreaded zstd, needed to honour `--compress-threads=N` above 0. | stable |
| `lz4` | workspace, `cli`, `core`, `engine`, `transfer`, `compress`, `protocol` | yes | LZ4 codec. | stable |
| `zlib-ng` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol` | yes | SIMD `zlib-ng` C backend for zlib. `compress` and `protocol` enable it by default, so every build that links `core` uses it. | stable |
| `zlib-rs` | `compress`, `protocol` | no | Pure-Rust `zlib-rs` backend for zlib. | stable |
| `xattr` | workspace, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata` | yes | Extended attributes (`-X`); NTFS ADS on Windows. | stable |
| `acl` | workspace, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata` | yes | ACLs (`-A`) via `exacl`; NTFS ACLs via `windows-rs`. | stable |
| `iconv` | workspace, `cli`, `core`, `engine`, `transfer`, `daemon`, `protocol` | yes | Filename charset conversion (`--iconv`). | stable |
| `idn` | workspace, `core` | yes | IDNA A-label conversion of host names (upstream `SUPPORT_IDN`). Without it, hosts are used as typed and `--version` reports `no IDN`. | stable |
| `parallel` | workspace, `cli`, `engine`, `checksums`, `signature` | yes | Rayon-based parallel file and checksum work. | stable |
| `io_uring` | workspace, `transfer`, `fast_io` | yes | Linux 5.6+ batched I/O with runtime fallback. | stable |
| `sqpoll-mlock-basis` | `fast_io` | yes | Pins the mmap'd basis window with `mlock(2)` so an SQPOLL ring can stay on beside an mmap basis reader. | stable |
| `iocp` | workspace, `transfer`, `fast_io` | yes | Windows I/O Completion Ports. | stable |
| `copy_file_range` | workspace, `fast_io` | yes | Compatibility alias; the syscall is always compiled with runtime detection. | stable |
| `async` | workspace, `core`, `engine`, `daemon`, `bandwidth`, `protocol` | yes | tokio for async I/O paths. | stable |
| `embedded-ssh` | workspace, `cli`, `core`, `rsync_io` | yes | Built-in SSH client for `ssh://` operands. | stable |
| `ssh-config-parse` | `rsync_io` | yes | `ssh_config` reader for the embedded client and the SSH compression warning. | stable |
| `openssl` | workspace, `checksums` | no | MD4/MD5 through system OpenSSL. | stable |
| `openssl-vendored` | workspace, `checksums` | no | As `openssl`, statically linked. | stable |
| `sd-notify` | workspace, `core`, `daemon` | no | systemd `sd-notify` for the daemon. | stable |
| `quic` | workspace, `cli`, `core`, `daemon`, `rsync_io`, `transfer`, `test-support` | no | QUIC transport (see [QUIC transport](#quic-transport)). | opt-in |
| `iouring-send-zc` | workspace, `fast_io` | yes | io_uring `SEND_ZC` socket writes (Linux 6.0+). Built in, but used only when the local node asks for zero-copy (see [Performance tuning](#performance-tuning)). | stable |
| `daemon-zero-copy` | `daemon` | yes | Lets a daemon sender use `SEND_ZC` when zero-copy is requested. | stable |
| `daemon-seccomp` | workspace, `daemon` | no | seccomp allowlist for daemon workers. Once compiled in, it is on at runtime; opt out with `OC_RSYNC_NO_SECCOMP=1`. | opt-in |
| `incremental-flist` | `transfer` | yes | Gates tests only. The incremental receiver is chosen at runtime from the negotiated flags (#7968). | stable |
| `lazy-metadata` | `engine` | yes | Defers `stat()` until metadata is needed. | stable |
| `mmap-free-basis` | workspace, `engine`, `fast_io` | no | Reads the basis without mmap, through io_uring where available or buffered I/O. | experimental |
| `multi-producer` | `engine` | no | Relaxes the single-producer invariant on `WorkQueueSender`. | experimental |
| `thread-slab-pool` | `engine`, `transfer` | no | Per-thread slab in front of `BufferPool`. | experimental |
| `parallel-delete-consumer` | `engine` | no | Parallel consumer for delete operations that keeps the wire order. | experimental |
| `vmsplice` | `fast_io`, `transfer` | no | Linux `vmsplice(2)` + `splice(2)` writer. | experimental |
| `dontcache` | `fast_io`, `transfer` | no | Linux 6.14+ `RWF_DONTCACHE` bulk writer that does not evict the page cache. | experimental |
| `macos-gcd` | `fast_io`, `transfer` | no | macOS Grand Central Dispatch `dispatch_io` disk writer. | experimental |
| `macos-kqueue` | `daemon` | no | macOS daemon accept loop on one `kqueue(2)` wait over all listeners. | experimental |
| `transmitfile` | `fast_io` | no | Windows `TransmitFile()` file-to-socket primitive. | experimental |
| `iouring-data-reads` / `iouring-data-writes` | `engine`, `fast_io` | no | Route large file reads or writes through io_uring. | experimental |
| `adaptive-basis-dispatch` | `fast_io` | no | Per-file basis-read dispatch between mmap and io_uring by recent throughput; on Windows it also selects the IOCP file reader. | experimental |
| `async-ssh` | `core`, `rsync_io` | no | Async SSH transport; enable at runtime with `OC_RSYNC_ASYNC_SSH=1`. | experimental |
| `async-daemon` | `cli`, `daemon` | no | tokio accept loop dispatching sync workers. | experimental |
| `concurrent-sessions` | `daemon` | no | Shared session state for multi-session daemons. | experimental |
| `tokio-transfer` | `protocol` | no | Async multiplex read leaf; not wired into a transfer path. | experimental |
| `tracing` | `core`, `engine`, `transfer`, `daemon`, `protocol`, `filters`, `logging`, `matching`, `signature` | no | Structured `tracing` instrumentation. | stable |

Not listed: `landlock` (`fast_io`), which `cli` and `daemon` always enable on Linux; `syscall_batch` (`fast_io`), an empty alias; `block2` (`fast_io`), the `macos-gcd` dependency; and the internal `serde`, `bench-internal` and `test-support` features.

---

## Usage

Works like `rsync`:

```bash
# Local sync
oc-rsync -av ./source/ ./dest/

# Remote pull / push over SSH
oc-rsync -av user@host:/remote/path/ ./local/
oc-rsync -av ./local/ user@host:/remote/path/

# Daemon pull / push
oc-rsync -av rsync://host/module/ ./local/
oc-rsync -av ./local/ rsync://host/module/

# Daemon over remote shell
oc-rsync -av host::module/path/ ./local/

# Run as a daemon
oc-rsync --daemon --config=/etc/oc-rsync/oc-rsyncd.conf

# Compression, checksum-based sync with deletion
oc-rsync -avz --compress-level=3 ./source/ ./dest/
oc-rsync -avc --delete ./source/ ./dest/

# Batch mode (record and replay)
oc-rsync -av --write-batch=changes ./source/ ./dest/
oc-rsync -av --read-batch=changes ./other-dest/
```

For all options: `oc-rsync --help`.

### SSH transports

- `host:path` and `user@host:path` spawn the system `ssh`, like upstream. `-e`/`--rsh` and `RSYNC_RSH` choose another remote shell. As in upstream, the child's stdin and stdout are `AF_UNIX` socketpairs, with a fallback to pipes if the kernel refuses one (#8183). This path needs no cargo feature.
- `ssh://[user@]host[:port]/path` is an oc-rsync extension. It uses an embedded client built on [`russh`](https://crates.io/crates/russh) and spawns no subprocess. It needs the `embedded-ssh` feature (on by default). Without the feature an `ssh://` operand fails with a diagnostic. Combining `ssh://` with `-e`/`--rsh` or `RSYNC_RSH` is rejected.

The embedded client supports key-based (RSA, ED25519, ECDSA) and password authentication, and `SSH_AUTH_SOCK` agents. It reads `~/.ssh/config` and `/etc/ssh/ssh_config` with an in-house parser (`ssh-config-parse` feature, on by default): `Host` and `Match` blocks, `Include`, first-obtained-wins precedence, `ProxyCommand` / `ProxyJump`, and the host-key and authentication families. A differential harness checks it against `ssh -G` in CI.

Some SSH features (for example certificate-based auth) may be missing; please open an issue. The embedded client bridges a synchronous transfer pipeline to async russh through `spawn_blocking`; [`docs/design/russh-async-native-path.md`](./docs/design/russh-async-native-path.md) describes the planned async-native path. See also the `SSH TRANSPORT` section of `oc-rsync(1)`.

### QUIC transport

oc-rsync can carry the rsync daemon protocol over QUIC (TLS 1.3 over UDP) instead of a plain TCP socket. Upstream rsync has no QUIC transport, so both ends must be oc-rsync. It needs the `quic` cargo feature, which is not a default; release artifacts for Linux, macOS and Windows enable it from the first release after v0.6.4. Windows builds run the QUIC client only: a Windows daemon with a QUIC directive refuses to start.

```sh
cargo build --release --features quic
```

The daemon needs a certificate and key in the global section of `oc-rsyncd.conf`, and UDP open on its port (873 unless `quic port` says otherwise):

```ini
quic cert file = /etc/oc-rsync/server-cert.pem
quic key file  = /etc/oc-rsync/server-key.pem
# Optional: require client certificates (mutual TLS)
# quic client ca file = /etc/oc-rsync/client-ca.pem
```

Clients must name the CA that signed the daemon certificate. There is no default trust and no trust-on-first-use, and the system trust store is not consulted. A failed QUIC connection exits 5 and never falls back to TCP.

```sh
oc-rsync -av --quic-ca ca-cert.pem quic://host/module/ dest/
oc-rsync -av --quic --quic-ca ca-cert.pem host::module/ dest/
oc-rsync -av --quic-ca ca-cert.pem --quic-cert client-cert.pem --quic-key client-key.pem quic://host/module/ dest/
```

The ALPN identifier is `rsync`. The congestion controller defaults to BBR; `--quic-cc` selects `cubic` or `newreno`, and `--quic-window` sizes the flow-control window. The daemon leaf certificate must be `CA:FALSE`, which a plain `openssl req -x509` does not produce. [`docs/quic-transport.md`](./docs/quic-transport.md) has the full guide: certificate generation, every directive, flag and environment variable, a walkthrough, error messages and limitations.

### Linux io_uring

oc-rsync uses io_uring when the kernel and probed opcodes allow it, and otherwise falls back to standard `read(2)`/`write(2)`. The kernel floor is Linux 5.6 (`MIN_KERNEL_VERSION` in [`crates/fast_io/src/io_uring/config.rs`](./crates/fast_io/src/io_uring/config.rs)). Provided buffer rings need 5.19+. `SEND_ZC` needs Linux 6.0+; the `iouring-send-zc` feature is in the default set, but `SEND_ZC` runs only when zero-copy is requested. The per-opcode kernel table is in [`docs/audit/iouring-opcode-kernel-floor.md`](./docs/audit/iouring-opcode-kernel-floor.md).

Three policies: *auto* (default; probe and fall back), `--io-uring` (require it), `--no-io-uring` (never use it). `--version` shows the active backend.

### Performance tuning

**Avoid double compression over SSH.** SSH compression (`ssh -C`, or `Compression yes` in `ssh_config`) and rsync's own `-z` compress the same bytes twice, costing CPU for little gain. Pick one, usually rsync's `-z`: oc-rsync negotiates zstd when both peers support it, falls back to zlib, and honours `--skip-compress`.

```sh
# Good: rsync compresses, SSH carries the bytes as-is.
oc-rsync -avz user@host:/src/ /dst/

# Bad: -C on ssh re-compresses what rsync already compressed.
oc-rsync -avz -e 'ssh -C' user@host:/src/ /dst/
```

oc-rsync warns when it sees `-C` or `-o Compression=yes` in the SSH argv, and (with `ssh-config-parse`) when a `Compression yes` directive applies from `ssh_config`. It does not disable either layer.

**`--zero-copy` and `SEND_ZC`.** Zero-copy socket writes (io_uring `SEND_ZC`, Linux 6.0+) are off by default. On a loopback benchmark they made a 2 GiB daemon pull slower, so they stay opt-in. The choice is local to each node and is never forwarded to the peer (#8183). A client opts in with `--zero-copy`. A server opts in with `oc-rsync --daemon --zero-copy`, with `--rsync-path='oc-rsync --zero-copy'`, or with `OC_RSYNC_ZERO_COPY=on|off|auto`. The flag wins over the variable, and the variable over the default. See [`docs/design/iouring-send-zc.md`](./docs/design/iouring-send-zc.md).

**Parallel delta work.** The sender scans each file's delta on one thread unless `--parallel-delta-scan` is given; it applies to local transfers of large, duplicate-free basis files. `--checksum-threads=N` sets the threads that hash basis signatures; the default uses the available cores above a size threshold, and `1` forces sequential hashing.

**SSH stderr.** The remote shell inherits oc-rsync's own stderr, as upstream `piped_child()` arranges (`pipe.c:48-97`): its messages reach the terminal byte for byte, as soon as they are written, and exactly once. Only stdin and stdout are socketpairs. See [`docs/ssh-transport.md`](./docs/ssh-transport.md).

### Known limitations

- **Two buffer pools.** `OC_RSYNC_BUFFER_POOL_SIZE`, `OC_RSYNC_BUFFER_POOL_MEMORY_CAP` and `OC_BUFFER_POOL_BLOCK_SIZE` tune the engine `BufferPool` only. The io_uring registered buffer pool is sized statically and does not adapt. Its cost has not been measured separately.
- **io_uring buffer-group IDs** are a 16-bit namespace, and allocation returns `BgidAllocError::Exhausted` rather than wrapping. No transfer path allocates one today. See [`docs/audits/bgid-lifecycle.md`](./docs/audits/bgid-lifecycle.md).
- **Daemon traffic is plaintext**, like upstream (authentication, no encryption). There is no built-in TLS client. Use SSH, or put the daemon behind a TLS proxy (`stunnel`, HAProxy, nginx) and connect with a wrapper such as `rsync-ssl` or `stunnel`.
- **Windows IOCP** covers sockets and disk writes. The IOCP file reader is only selected under the experimental `adaptive-basis-dispatch` feature.

---

## Compatibility

### Protocol and interop

| Protocol | Upstream versions | oc-rsync status | Coverage |
|----------|-------------------|-----------------|----------|
| 33 | 3.5.1 | Full support (default) | Unit tests, interop matrix against 3.5.1, and the 3.5.1 testsuite legs |
| 32 | 3.4.x, 3.5.0 | Full support | Interop matrix against 3.4.4 and 3.5.0 |
| 31 | 3.1.x - 3.3.x | Full support | Interop matrix against 3.1.3 |
| 30 | 3.0.x | Full support | Interop matrix against 3.0.9 |
| 29 | 2.6.4 - 2.6.9 | Full support | Interop matrix against 2.6.9, plus golden-byte tests |
| 28 | 2.6.1 - 2.6.3 | Full support | Interop matrix against 2.6.3, plus golden-byte tests in `crates/protocol/tests/` |
| <= 27 | <= 2.5.x | Not supported | |

oc-rsync advertises protocol 33 and uses the lower of its own and the peer's version, so a 3.5.0 or 3.4.x peer runs at 32. A 3.5.1 peer runs at 33. Per-version behaviour is implemented as `protocol_version` gates in the wire codecs, for example [`zlib_codec.rs`](./crates/protocol/src/wire/compressed_token/zlib_codec.rs).

Interop scenarios run in CI against the upstream releases listed in [`tools/ci/run_interop.sh`](./tools/ci/run_interop.sh): `versions=` for the scenario matrix, `extra_build_versions=` for build-only peers, and `extended_matrix_versions=` for the extended matrix. Push and pull are both covered, across transfer modes, deletion, compression, metadata, reference dirs, file selection, batch round trip, path handling, device nodes and daemon auth. See the [interop compatibility matrix](./docs/user/interop-compatibility-matrix.md) for detail.

### Platform support

| Platform | Tier | Notes |
|---|---|---|
| Linux x86_64 / aarch64 | **Tier 1** | io_uring, `splice`, `vmsplice`, Landlock. Required CI runs the full nextest workspace. A seccomp syscall allowlist for daemon workers is available with `--features daemon-seccomp`; it is off in default builds and in released binaries. |
| macOS x86_64 / aarch64 | **Tier 1** | `clonefile`, `fcopyfile`, full metadata, ACL and xattr support including AppleDouble (`._foo`) resource forks. Required CI runs a crate-scoped subset (core, engine, cli, metadata, apple-fs, fast_io). |
| Windows x86_64 | **Tier 2** | IOCP file and socket I/O, `CopyFileExW`, ReFS reflink, NTFS DACLs (partial), xattrs via NTFS Alternate Data Streams. No POSIX device nodes or FIFOs. Required CI tests the core, engine, cli, metadata, fast_io and transfer crates. |

Tier definitions and criteria: [Platform support tiers](docs/design/platform-tiers.md). Windows detail: [Windows support matrix](docs/user/windows-support-matrix.md) and the [Windows Tier 2 stub inventory](docs/audits/win-tier2-stub-inventory.md).

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Permissions (`-p`) | ✓ | ✓ | ⚠ | Windows preserves only the read-only flag. |
| Times (`-t`) | ✓ | ✓ | ✓ | Nanosecond precision. |
| Ownership (`-o`/`-g`) | ✓ | ✓ | ✗ | uid/gid mapping is Unix-only. |
| ACLs (`-A`) | ✓ | ✓ | ⚠ | `exacl` on Linux/macOS. Windows round-trips NTFS DACLs; deny ACEs, inherited ACEs, the SACL, non-`rwx` bits and unresolvable SIDs are dropped with a warning. See [`docs/design/windows-ntfs-acl-support.md`](docs/design/windows-ntfs-acl-support.md). |
| Xattrs (`-X`) | ✓ | ✓ | ✓ | Windows stores xattrs as NTFS Alternate Data Streams. |
| Hardlinks (`-H`) | ✓ | ✓ | ✓ | |
| Symlinks | ✓ | ✓ | ⚠ | Windows: directory links fall back to a junction when unprivileged; file links need Administrator or Developer Mode, otherwise they are skipped with a warning (exit 23). |
| Devices/specials (`-D`) | ✓ | ✓ | ✗ | |
| Sparse files (`-S`) | ✓ | ✓ | ⚠ | Windows does not set `FSCTL_SET_SPARSE`. |
| Async I/O | ✓ io_uring | ⚠ standard I/O | ⚠ IOCP | io_uring is detected at runtime on Linux 5.6+. Windows uses IOCP for disk writes and sockets; file reads use buffered I/O. |
| Reflink / clone | ✓ `FICLONE` | ✓ `clonefile` | ⚠ ReFS only | |
| Optimized copy | ✓ `copy_file_range` | ✓ `fcopyfile` | ✓ `CopyFileExW` | Each falls back to standard I/O. |

Legend: ✓ supported, ⚠ partial, ✗ not implemented.

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Each tagged release runs [`.github/workflows/benchmark.yml`](./.github/workflows/benchmark.yml) against upstream rsync 3.5.1, 3.5.0 and 3.4.4 across local, SSH and daemon modes (#8106). It reports elapsed time (median, with run-to-run spread), peak RSS and corpus rate. Results are attached to the [GitHub release](https://github.com/oferchen/rsync/releases/latest) as `benchmark.png`, `benchmark_report.md` and `benchmark_results.json`.

Releases up to and including v0.6.4 predate this harness: their report compares against 3.4.4 only, without peak RSS, corpus rate or spread. The chart above comes from the latest release, so read that release's `benchmark_report.md` for what it measured.

oc-rsync uses threads where upstream forks, while keeping the same protocol. I/O buffers are sized by file size.

---

## Testing and CI

Branch protection requires fourteen checks: `fmt + clippy`, `nextest (stable)`, `Windows (stable)`, `macOS (stable)`, `Linux musl (stable)`, `interop / interop with upstream rsync`, and the eight [upstream testsuite](#upstream-testsuite) legs (`upstream-testsuite / upstream testsuite` and its `(root)` variant, the same two under `upstream-testsuite-tcp /`, and `upstream testsuite (macOS)` and `(macOS, root)` under `upstream-testsuite-macos /` and `upstream-testsuite-macos-tcp /`). [`docs/contributing/TESTING.md`](./docs/contributing/TESTING.md) has the command that re-derives the list from the ruleset.

The SSH throughput check (SSR-1) compares a release build of oc-rsync against upstream in its own `SSH throughput (SSR-1, release)` job. It runs on every pull request and reports its result, but it is not a required check. Benchmarks run only when a release is tagged (see [Performance](#performance)). Interop coverage is described under [Protocol and interop](#protocol-and-interop).

---

## Security

Of the 33 CVEs fixed upstream in rsync 3.5.0, 27 are fixed in oc-rsync, 1 is partially fixed and 5 do not apply. The partial one is CVE-2026-53796: every non-daemon receiver pins its destination for data, but metadata calls after a destination swap are still path-based. The 2024 and rsync 3.4.3 CVEs are fixed, mitigated or do not affect oc-rsync. rsync 3.5.1 names no CVE; of its nine security-relevant fixes, seven are mirrored and two do not apply. Unsafe code is confined to a few crates that wrap platform FFI and SIMD.

[`SECURITY.md`](./SECURITY.md) has the per-CVE evidence, the unsafe-code policy, fuzzing and how to report a vulnerability. [`docs/audits/cve-uts351-matrix.md`](./docs/audits/cve-uts351-matrix.md) maps each CVE to its upstream fix, its upstream test cells and the Rust tests that pin it.

---

## Contributing

1. Fork and create a feature branch.
2. Install Rust 1.89.0 (pinned in `rust-toolchain.toml`) and [`cargo-nextest`](https://nexte.st/), then run the checks CI runs:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings
   cargo nextest run --workspace --all-features
   ```
3. Open a PR with a conventional-commit prefix describing behavioural changes and interop impact.

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the full workflow and [`docs/contributing/ONBOARDING.md`](./docs/contributing/ONBOARDING.md) for a guided start.

### Project layout

```text
src/bin/oc-rsync.rs     # Entry point
crates/cli/             # CLI flags, help, output formatting
crates/core/            # Orchestration facade, session management, config
crates/protocol/        # Wire protocol (v28-33), multiplex framing
crates/transfer/        # Generator, receiver, delta transfer pipeline
crates/engine/          # Local copy executor, sparse writes, temp-file commit
crates/daemon/          # Daemon mode, module access control, systemd
crates/checksums/       # Rolling and strong checksums (MD4, MD5, XXH3, SIMD)
crates/filters/         # Include/exclude pattern engine, .rsync-filter
crates/metadata/        # Permissions, uid/gid, mtime, ACLs, xattrs
crates/platform/        # Process, identity, environment and signal FFI
crates/rsync_io/        # SSH stdio, rsync:// TCP transport, handshake
crates/fast_io/         # Platform I/O (io_uring, copy_file_range, sendfile)
crates/compress/        # zstd, lz4, zlib compression codecs
crates/bandwidth/       # Bandwidth limiting and rate control
crates/signature/       # Signature layout and block-size calculations
crates/matching/        # Delta matching and block search
crates/logging/         # Logging macros and verbosity control
crates/logging-sink/    # Message sink and output formatting
crates/batch/           # Batch file read/write support
crates/branding/        # Binary naming and version metadata
crates/embedding/       # Programmatic entry points for library usage
crates/apple-fs/        # macOS filesystem operations (clonefile, FSEvents)
crates/windows-gnu-eh/  # Windows GNU exception handling shims
crates/test-support/    # Shared test utilities (dev-dependency only)
```

API documentation: `cargo doc --workspace --no-deps --open`.

### Architecture

```text
cli -> core -> engine, daemon, rsync_io, logging
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
                                                                            -> platform
```

---

## License

GNU GPL v3.0 or later. See [`LICENSE`](./LICENSE).

---

## Acknowledgements

Inspired by [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team.

Internal matching-engine optimisations adapted from [`zsync`](http://zsync.moria.org.uk/) by Colin Phipps. They change only in-memory lookups, so the default wire format stays pure rsync.

Thanks to **Pieter** for his heroic patience in enduring months of my rsync commentary.
Thanks to **Elad** for his endless patience hearing rsync protocol commentary as I'm introduced to it.
