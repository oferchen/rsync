[![RepoGrade](https://www.repo-grade.com/api/badge/oferchen/rsync)](https://www.repo-grade.com/report/oferchen/rsync)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/oferchen/rsync)

[![CI](https://github.com/oferchen/rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/ci.yml)
[![Interop Validation](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/interop-validation.yml)
[![Upstream Testsuite 3.5.0 (nonroot, pipe)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite.yml)
[![Upstream Testsuite 3.5.0 (root, pipe)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-root.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-root.yml)
[![Upstream Testsuite 3.5.0 (nonroot, tcp)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-tcp.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-tcp.yml)
[![Upstream Testsuite 3.5.0 (root, tcp)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-root-tcp.yml/badge.svg)](https://github.com/oferchen/rsync/actions/workflows/upstream-testsuite-root-tcp.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/rsync?include_prereleases)](https://github.com/oferchen/rsync/releases)

# oc-rsync

`rsync` re-implemented in Rust. Wire-compatible with upstream rsync 3.5.0 and the 3.4.x series (protocol 32). Works as a drop-in replacement.

Binary name: **`oc-rsync`**. It installs alongside the system `rsync` without conflict.

---

## Status

**Release:** 0.6.4. **Upstream reference:** rsync 3.5.0, protocol 32, with back-negotiation to protocol 28.

All transfer modes (local, SSH, daemon), the delta algorithm, metadata preservation and compression are complete.

**rsync 3.5.0.** Upstream released 3.5.0 on 13 Aug 2026. It keeps `PROTOCOL_VERSION` 32 and `SUBPROTOCOL_VERSION` 0, so wire compatibility carries over from 3.4.4. The changes are behavioural: 33 CVE fixes in path handling and the daemon, and new options and directives. All five new options (`--confine-root`, `--drop-D`, `--no-drop-D`, `--insecure-links`, `--no-insecure-links`) and all three new daemon directives (`proxy protocol hosts`, `auth digest`, `insecure links`) are implemented. The per-CVE audit trail is in [`SECURITY.md`](./SECURITY.md).

**rsync 3.5.1.** Upstream 3.5.1 advertises protocol 33. oc-rsync does not advertise 33. A peer that advertises a newer protocol is negotiated down to 32 instead of refused (#7916); that path is covered by unit tests. Moving the reference to 3.5.1 is in progress and not yet on master.

| Component | Status |
|-----------|--------|
| **Transfer** | Local, SSH, daemon push/pull, daemon over remote shell (`host::module`) |
| **Delta** | Rolling + strong checksums, block matching, parallel receive-delta pipeline |
| **Metadata** | Permissions, timestamps, ownership, ACLs (`-A`), xattrs (`-X`) |
| **File handling** | Sparse, hardlinks, symlinks, devices, FIFOs |
| **Deletion** | `--delete` (before/during/after/delay), `--delete-excluded` |
| **Compression** | zlib, zstd, lz4 with level control and negotiation |
| **Checksums** | MD4, MD5, XXH3/XXH128 with SIMD (AVX2, SSE2, NEON) |
| **Incremental recursion** | Advertised when oc-rsync sends. An oc-rsync client that receives (a pull) does not advertise it yet, so pulls use a full up-front file list |
| **Batch** | `--write-batch` / `--read-batch` round trip |
| **Daemon** | Negotiation, auth, modules, chroot, syslog, pre/post-xfer exec |
| **Filtering** | `--filter`, `--exclude`, `--include`, `.rsync-filter`, `--files-from` |
| **Reference dirs** | `--compare-dest`, `--link-dest`, `--copy-dest` |
| **Options** | `--delay-updates`, `--inplace`, `--partial`, `--iconv`, fuzzy matching |
| **3.5.0 surface** | `--confine-root`, `--drop-D` / `--no-drop-D`, `--insecure-links` / `--no-insecure-links`; daemon `auth digest`, `insecure links`, `proxy protocol hosts` |
| **I/O** | io_uring (Linux 5.6+), `copy_file_range`, `clonefile` (macOS), adaptive buffers |

### Upstream testsuite

Upstream's own 3.5.0 test suite runs against `oc-rsync` as `$RSYNC` on every pull request, in **eight legs**: platform {Linux, macOS} x daemon transport {stdio pipe, loopback TCP} x privilege {non-root, root}. The pipe legs run the whole 345-test corpus. The TCP legs add `--daemon-tests-only` and run the 155 tests that start a daemon.

The four Linux legs are required status checks. The four macOS legs run on every PR and gate on their own manifests, but are not required contexts.

Outcomes, counted from each leg's committed manifest (`tools/ci/upstream-3.5.0-expect.*.txt`):

| leg | pass | fail | skip | corpus |
|---|---:|---:|---:|---:|
| Linux, non-root, pipe | 261 | 0 | 84 | 345 |
| Linux, root, pipe | 290 | 0 | 55 | 345 |
| Linux, non-root, tcp | 119 | 4 | 32 | 155 |
| Linux, root, tcp | 137 | 4 | 14 | 155 |
| macOS, non-root, pipe | 238 | 2 | 105 | 345 |
| macOS, root, pipe | 267 | 1 | 77 | 345 |
| macOS, non-root, tcp | 116 | 4 | 35 | 155 |
| macOS, root, tcp | 132 | 4 | 19 | 155 |

Re-derive any row:

```sh
awk '!/^#/ && NF {c[$NF]++; t++} END {print t, c["pass"], c["fail"], c["skip"]}' \
  tools/ci/upstream-3.5.0-expect.nonroot.txt
```

No test fails on either full-corpus Linux leg. **Six distinct tests** fail across all eight manifests (`awk '!/^#/ && $NF=="fail" {print $1}' tools/ci/upstream-3.5.0-expect.*.txt | sort -u`). Four are the `proto-*` cluster, which fails on every TCP leg. The other two (`chmod-setid`, `partial-protected-regular-retry-policy`) fail only on macOS, where the real upstream 3.5.0 binary lands on the same outcome. Only a *change* in outcome turns a leg red, including an unexpected pass, so a divergence cannot be re-baselined silently.

### Platform support

| Platform | Tier | Notes |
|---|---|---|
| Linux x86_64 / aarch64 | **Tier 1** | io_uring, `splice`, `vmsplice`, Landlock. Required CI runs the full nextest workspace. A seccomp syscall allowlist for daemon workers is available with `--features daemon-seccomp`; it is off in default builds and in released binaries. |
| macOS x86_64 / aarch64 | **Tier 1** | `clonefile`, `fcopyfile`, full metadata, ACL and xattr support including AppleDouble (`._foo`) resource forks. Required CI runs a crate-scoped subset (core, engine, cli, metadata, apple-fs, fast_io). |
| Windows x86_64 | **Tier 2** | IOCP file and socket I/O, `CopyFileExW`, ReFS reflink, NTFS DACLs (partial), xattrs via NTFS Alternate Data Streams. No POSIX device nodes or FIFOs. Required CI tests the core, engine and cli crates. |

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

### Interop testing

Interop scenarios run in CI against the upstream releases listed in [`tools/ci/run_interop.sh`](./tools/ci/run_interop.sh): `versions=` for the scenario matrix, `extra_build_versions=` for build-only peers, and `extended_matrix_versions=` for the extended matrix. Read the script for the current list. Push and pull are both covered, across transfer modes, deletion, compression, metadata, reference dirs, file selection, batch round trip, path handling, device nodes and daemon auth. See the [interop compatibility matrix](./docs/user/interop-compatibility-matrix.md) for detail.

| Protocol | Upstream versions | oc-rsync status | Coverage |
|----------|-------------------|-----------------|----------|
| 32 | 3.4.x, 3.5.0 | Full support (default) | Interop matrix against 3.4.4 and 3.5.0 |
| 31 | 3.1.x - 3.3.x | Full support | Interop matrix against 3.1.3 |
| 30 | 3.0.x | Full support | Interop matrix against 3.0.9 |
| 29 | 2.6.9 | Full support | Non-blocking daemon push/pull cells against 2.6.9, plus golden-byte tests |
| 28 | 2.6.0 - 2.6.8 | Wire-level support | Golden-byte tests in `crates/protocol/tests/` |
| <= 27 | <= 2.5.x | Not supported | |

A peer that advertises a protocol newer than 32 is negotiated down to 32. Per-version behaviour is implemented as `protocol_version` gates in the wire codecs, for example [`zlib_codec.rs`](./crates/protocol/src/wire/compressed_token/zlib_codec.rs).

### Linux io_uring

oc-rsync uses io_uring when the kernel and probed opcodes allow it. Otherwise it falls back to standard `read(2)`/`write(2)`. The kernel floor is Linux 5.6 (`MIN_KERNEL_VERSION` in [`crates/fast_io/src/io_uring/config.rs`](./crates/fast_io/src/io_uring/config.rs)). Provided buffer rings need 5.19+. `SEND_ZC` needs Linux 6.0+ and the `iouring-send-zc` cargo feature, which is not in the default set. The per-opcode kernel table is in [`docs/audit/iouring-opcode-kernel-floor.md`](./docs/audit/iouring-opcode-kernel-floor.md).

Three policies: *auto* (default; probe and fall back), `--io-uring` (require it), `--no-io-uring` (never use it). The active backend is shown in `--version` output.

### SSH transports

- `host:path` and `user@host:path` spawn the system `ssh`, like upstream. `-e`/`--rsh` and `RSYNC_RSH` choose another remote shell. This path needs no cargo feature.
- `ssh://[user@]host[:port]/path` is an oc-rsync extension. It uses an embedded client built on [`russh`](https://crates.io/crates/russh) and spawns no subprocess. It needs the `embedded-ssh` feature (on by default). Without the feature an `ssh://` operand fails with a diagnostic. Combining `ssh://` with `-e`/`--rsh` or `RSYNC_RSH` is rejected.

The embedded client supports key-based (RSA, ED25519, ECDSA) and password authentication, and `SSH_AUTH_SOCK` agents. It reads `~/.ssh/config` and `/etc/ssh/ssh_config` with an in-house parser (`ssh-config-parse` feature, on by default): `Host` and `Match` blocks, `Include`, first-obtained-wins precedence, `ProxyCommand` / `ProxyJump`, and the host-key and authentication families. A differential harness checks it against `ssh -G` in CI.

Limitations: some SSH features (for example certificate-based auth) may be missing; please open an issue. The embedded client bridges a synchronous transfer pipeline to async russh through `spawn_blocking`; see [`docs/design/russh-async-native-path.md`](./docs/design/russh-async-native-path.md) for the planned async-native path.

See also the `SSH TRANSPORT` section of `oc-rsync(1)`.

### QUIC transport

oc-rsync can carry the rsync protocol over QUIC (RFC 9000/9001, TLS 1.3 over UDP) instead of a plain `rsync://` TCP socket. Upstream rsync has no QUIC transport. It is behind the `quic` cargo feature and is **off by default**; release builds do not carry it.

```sh
cargo build --release --features quic -p cli -p daemon
```

QUIC always runs TLS 1.3 with the ALPN token `rsync`. The client verifies the daemon certificate against the system roots, a private CA (`--quic-ca`), or a trust-on-first-use pin in `quic_known_hosts`. Mutual TLS is optional. Selecting QUIC is hard-fail: if the feature or listener is unavailable, the transfer errors out rather than falling back to TCP. The cipher defaults to AES-GCM with hardware AES and ChaCha20-Poly1305 without; `--quic-cipher aes|chacha20` overrides it.

Create a self-signed daemon certificate (the SubjectAltName must match the host clients connect to):

```sh
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout quic-key.pem -out quic-cert.pem -days 365 \
  -subj "/CN=rsync.example.com" \
  -addext "subjectAltName=DNS:rsync.example.com"
```

For mutual TLS, also create a client CA and sign a client certificate:

```sh
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout ca-key.pem -out ca-cert.pem -subj "/CN=oc-rsync client CA"
openssl req -newkey rsa:2048 -nodes \
  -keyout client-key.pem -out client.csr -subj "/CN=alice"
openssl x509 -req -in client.csr -CA ca-cert.pem -CAkey ca-key.pem \
  -CAcreateserial -out client-cert.pem -days 365
```

Daemon configuration (`oc-rsyncd.conf`, global section):

```ini
quic cert file = /etc/oc-rsync/quic-cert.pem
quic key file  = /etc/oc-rsync/quic-key.pem
# quic port defaults to the module `port` (873):
# quic port = 1873
# For mutual TLS:
quic client ca file = /etc/oc-rsync/ca-cert.pem
```

Client usage:

```sh
oc-rsync -a quic://host/module/ dest/
oc-rsync -a --quic host::module/ dest/
oc-rsync -a --quic-ca ca-cert.pem quic://host/module/ dest/
oc-rsync -a --quic --quic-cert client-cert.pem --quic-key client-key.pem host::module/ dest/
oc-rsync -a --quic --quic-cipher chacha20 quic://host/module/ dest/
```

`--quic-cc` selects the congestion controller and `--quic-window` the flow-control window. Full guide: [`docs/quic-transport.md`](./docs/quic-transport.md).

### Performance

![Benchmark: oc-rsync vs upstream rsync](https://github.com/oferchen/rsync/releases/latest/download/benchmark.png)

Each tagged release runs [`.github/workflows/benchmark.yml`](./.github/workflows/benchmark.yml) against upstream rsync 3.5.0 and 3.4.4 across local, SSH and daemon modes. It reports elapsed time (median, with run-to-run spread), peak RSS and corpus rate. Results are attached to the [GitHub release](https://github.com/oferchen/rsync/releases/latest) as `benchmark.png`, `benchmark_report.md` and `benchmark_results.json`.

Releases up to and including v0.6.4 predate this harness. Their published report compares against 3.4.4 only, without peak RSS, corpus rate or spread. The chart above comes from whichever release is latest, so read that release's `benchmark_report.md` for what it measured.

oc-rsync uses threads where upstream forks, while keeping the same protocol. I/O buffers are sized by file size.

### Performance tuning

**Avoid double compression over SSH.** SSH compression (`ssh -C`, or `Compression yes` in `ssh_config`) and rsync's own `-z` compress the same bytes twice, costing CPU for little gain. Pick one. Usually prefer rsync's `-z`: oc-rsync negotiates zstd when both peers support it, falling back to zlib, and honours `--skip-compress`.

```sh
# Good: rsync compresses, SSH carries the bytes as-is.
oc-rsync -avz user@host:/src/ /dst/

# Bad: -C on ssh re-compresses what rsync already compressed.
oc-rsync -avz -e 'ssh -C' user@host:/src/ /dst/
```

oc-rsync warns when it sees `-C` or `-o Compression=yes` in the SSH argv, and (with `ssh-config-parse`) when a `Compression yes` directive applies from `ssh_config`. It does not disable either layer.

**`--zero-copy` and `SEND_ZC`.** `--zero-copy` may use `sendfile`, `splice`, `copy_file_range` and io_uring `SEND_ZC` on Linux. `SEND_ZC` dispatch needs the `iouring-send-zc` feature (Linux 6.0+), which is not in the default set. See [`docs/design/iouring-send-zc.md`](./docs/design/iouring-send-zc.md).

**SSH stderr socketpair.** The `ssh-socketpair-stderr` feature drains the SSH child's stderr through a `socketpair(AF_UNIX, SOCK_STREAM)` instead of a pipe, with event-driven shutdown. It helps with chatty remote shells and many parallel transfers. Linux is the recommended target. See [`docs/ssh-transport.md`](./docs/ssh-transport.md) and [`docs/design/socketpair-stderr-channel.md`](./docs/design/socketpair-stderr-channel.md). When the runtime cannot honour the feature, one of these warnings appears once per process:

- `SSH stderr async drain unavailable on this platform` - `socketpair` failed; the session falls back to a pipe.
- `SSH stderr socketpair partially set up` - `dup(2)` on the parent half failed; shutdown relies on a 50 ms timeout.
- `SSH stderr async drain falling back to Stdio::inherit()` - stderr goes straight to the parent terminal and is not captured.

### Known limitations

- **Two buffer pools.** `OC_RSYNC_BUFFER_POOL_SIZE`, `OC_RSYNC_BUFFER_POOL_MEMORY_CAP` and `OC_BUFFER_POOL_BLOCK_SIZE` tune the engine `BufferPool` only. The io_uring registered buffer pool is sized statically and does not adapt. Its cost has not been measured separately.
- **io_uring buffer-group IDs** are a 16-bit namespace and allocation returns `BgidAllocError::Exhausted` rather than wrapping. No transfer path allocates one today. See [`docs/audits/bgid-lifecycle.md`](./docs/audits/bgid-lifecycle.md).
- **Delta work is single-threaded per file by default.** Opt in with `--parallel-delta-scan` and `--checksum-threads=N`.
- **Daemon traffic is plaintext**, like upstream (authentication, no encryption). There is no built-in TLS client. Use SSH, or put the daemon behind a TLS proxy (`stunnel`, HAProxy, nginx) and connect with a wrapper such as `rsync-ssl` or `stunnel`.
- **Windows IOCP** covers sockets and disk writes. The IOCP file reader is only selected under the experimental `adaptive-basis-dispatch` feature.

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
| `zstd` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol`, `batch` | yes | zstd codec and negotiation. | stable |
| `lz4` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol` | yes | LZ4 codec. | stable |
| `zlib-ng` | workspace, `core`, `engine`, `transfer`, `compress`, `protocol` | no | SIMD `zlib-ng` C backend instead of pure-Rust zlib. | stable |
| `xattr` | workspace, `cli`, `core`, `transfer`, `daemon`, `metadata` | yes | Extended attributes (`-X`); NTFS ADS on Windows. | stable |
| `acl` | workspace, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata` | yes | ACLs (`-A`) via `exacl`; NTFS ACLs via `windows-rs`. | stable |
| `iconv` | workspace, `cli`, `core`, `transfer`, `daemon`, `protocol` | yes | Filename charset conversion (`--iconv`). | stable |
| `parallel` | workspace, `cli`, `engine`, `checksums` | yes | Rayon-based parallel file and checksum work. | stable |
| `io_uring` | workspace, `transfer`, `fast_io` | yes | Linux 5.6+ batched I/O with runtime fallback. | stable |
| `iocp` | workspace, `transfer`, `fast_io` | yes | Windows I/O Completion Ports. | stable |
| `copy_file_range` | workspace, `fast_io` | yes | Compatibility alias; the syscall is always compiled with runtime detection. | stable |
| `async` | workspace, `core`, `engine`, `daemon` | yes | tokio for async I/O paths. | stable |
| `embedded-ssh` | workspace, `core`, `rsync_io` | yes | Built-in SSH client for `ssh://` operands. | stable |
| `openssl` | workspace, `checksums` | no | MD4/MD5 through system OpenSSL. | stable |
| `openssl-vendored` | workspace, `checksums` | no | As `openssl`, statically linked. | stable |
| `sd-notify` | workspace, `core`, `daemon` | no | systemd `sd-notify` for the daemon. | stable |
| `quic` | workspace, `cli`, `core`, `daemon` | no | QUIC transport (see above). | opt-in |
| `iouring-send-zc` | workspace, `fast_io` | no | io_uring `SEND_ZC` dispatch (Linux 6.0+). | opt-in |
| `daemon-seccomp` | workspace, `daemon` | no | seccomp allowlist for daemon workers. Once compiled in, it is on at runtime; opt out with `OC_RSYNC_NO_SECCOMP=1`. | opt-in |
| `incremental-flist` | `transfer` | no | Incremental file-list processing (not forwarded into the default binary). | stable |
| `lazy-metadata` | `engine` | yes | Defers `stat()` until metadata is needed. | stable |
| `multi-producer` | `engine` | no | Relaxes the single-producer invariant on `WorkQueueSender`. | experimental |
| `thread-slab-pool` | `engine` | no | Per-thread slab in front of `BufferPool`. | experimental |
| `vmsplice` | `fast_io`, `transfer` | no | Linux `vmsplice(2)` + `splice(2)` writer. | experimental |
| `async-ssh` | `core`, `rsync_io` | no | Async SSH transport; enable at runtime with `OC_RSYNC_ASYNC_SSH=1`. | experimental |
| `ssh-socketpair-stderr` | `rsync_io` | no | SSH stderr over a socketpair (see above). | experimental |
| `async-daemon` | `daemon` | no | tokio accept loop dispatching sync workers. | experimental |
| `concurrent-sessions` | `daemon` | no | Shared session state for multi-session daemons. | experimental |
| `tracing` | `core`, `engine`, `transfer`, `daemon` | no | Structured `tracing` instrumentation. | stable |

#### Receiver spill

The concurrent-delta receiver keeps its reorder buffer in memory by default. Set `OC_RSYNC_SPILL_THRESHOLD_BYTES` (for example `64M`) to spill to disk, and optionally `OC_RSYNC_SPILL_DIR`. The `--spill-threshold-bytes` and `--spill-dir` flags override the variables. See [`docs/design/spill-policy-public-api.md`](./docs/design/spill-policy-public-api.md) and [`docs/operator-migration-guide-vNEXT.md`](./docs/operator-migration-guide-vNEXT.md).

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

For all options: `oc-rsync --help`. Release history: [CHANGELOG](./CHANGELOG.md).

---

## Development

### Prerequisites

- Rust 1.89.0 (pinned in `rust-toolchain.toml`)
- [`cargo-nextest`](https://nexte.st/): `cargo install cargo-nextest --locked`

### Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings
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
crates/platform/        # Process, identity, environment and signal FFI
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

API documentation: `cargo doc --workspace --no-deps --open`.

### Architecture

```text
cli -> core -> engine, daemon, rsync_io, logging
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
                                                                            -> platform
```

---

## Security

Every crate sets `#![deny(unsafe_code)]` at its root. Production `#[allow(unsafe_code)]` sites exist only in crates that wrap platform FFI or SIMD intrinsics: `fast_io`, `metadata`, `checksums`, `platform`, `engine`, `protocol` (one site) and `windows-gnu-eh`. Other crates allow unsafe only in tests.

Upstream CVE status, in short:

- **2024 batch** (CVE-2024-12084 to CVE-2024-12088, CVE-2024-12747): not vulnerable or mitigated.
- **rsync 3.4.3 batch** (CVE-2026-29518, 43617, 43618, 43619, 43620, 45232): fixed or not vulnerable. Receiver filesystem calls go through `*at` syscalls anchored on a directory fd, with a Landlock layer for the daemon on Linux.
- **rsync 3.5.0 batch** (33 CVEs): partially assessed. 14 of the 33 ids have a row in [`SECURITY.md`](./SECURITY.md), some still under audit. The other 19 are listed there by id as untriaged.
- **rsync 3.5.1**: its security fixes are being tracked. No disposition is claimed yet.

See [`SECURITY.md`](./SECURITY.md) for the per-CVE detail and how to report a vulnerability.

---

## Contributing

1. Fork and create a feature branch.
2. Run `cargo fmt --all` locally; CI runs `clippy` and `nextest`.
3. Open a PR with a conventional-commit prefix describing behavioural changes and interop impact.

See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the full workflow.

---

## License

GNU GPL v3.0 or later. See [`LICENSE`](./LICENSE).

---

## Acknowledgements

Inspired by [`rsync`](https://rsync.samba.org/) by Andrew Tridgell and the Samba team.

Internal matching-engine optimisations adapted from [`zsync`](http://zsync.moria.org.uk/) by Colin Phipps (in-memory only; wire format stays pure rsync).

Thanks to **Pieter** for his heroic patience in enduring months of my rsync commentary.
Thanks to **Elad** for his endless patience hearing rsync protocol commentary as I'm introduced to it.
