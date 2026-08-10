# Hardware Acceleration Priority Order

Tracks issue #2121.

## 1. Goal

oc-rsync auto-selects between hardware fast paths for SIMD checksum
computation, asynchronous I/O backends, platform copy mechanisms,
zero-copy network paths, sparse-file detection, and SSH cipher choice.
This document is the single canonical reference for the precedence each
subsystem follows so operators can predict which backend will be picked
on their host - and override it via CLI flags when the default is wrong
(debugging, benchmarking, regression bisects).

Selection runs once per process at startup. Results are cached in a
`OnceLock`, `AtomicBool`, or `AtomicU8` so repeated calls share the
probe. CLI overrides bypass the probe entirely and pin a specific
backend; an unsupported override is a hard error rather than a silent
fallback.

Related audits and design docs (consolidated below):

- [`docs/audits/macos-fastio-fallback.md`](../audits/macos-fastio-fallback.md)
  - macOS fast-I/O fallback inventory, including `MacosWriter` wiring.
- [`docs/audits/windows-iocp-file-write-status.md`](../audits/windows-iocp-file-write-status.md)
  - IOCP disk-commit wiring on Windows.
- [`docs/audits/should-inject-aes-gcm-ciphers.md`](../audits/should-inject-aes-gcm-ciphers.md)
  - AES-NI / ARMv8 AES detection that gates AES-GCM cipher injection.
- [`docs/design/io-uring-ring-pool.md`](./io-uring-ring-pool.md) -
  io_uring per-session ring pool architecture.
- [`docs/design/macos-fnocache-writev-fallback.md`](./macos-fnocache-writev-fallback.md)
  - `MacosWriter` design and threshold rationale.
- [`docs/design/windows-refs-reflink.md`](./windows-refs-reflink.md) -
  ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` mechanics.

---

## 2. Linux I/O Priority

The Linux chain prefers kernel-resident transfer primitives before
falling back to a userspace `read`/`write` loop. The chain is broken up
across three concerns - whole-file copy, file-to-socket transfer, and
socket-to-file transfer - because the optimal primitive differs by
direction.

### 2.1 Whole-file copy (file-to-file)

Dispatch lives in `platform_copy_impl`
(`crates/fast_io/src/platform_copy/dispatch.rs:18`).

| Priority | Backend | Requirement | Cost |
|----------|---------|-------------|------|
| 1 | `FICLONE` ioctl | Btrfs, XFS (reflink), bcachefs; same device | O(1), shares extents |
| 2 | `copy_file_range(2)` | Linux 4.5+ same-fs, 5.3+ cross-fs; size >= 64 KB | Zero-copy in kernel |
| 3 | `std::fs::copy` | Always | Userspace buffered |

The `FICLONE` call is dispatched via `rustix::fs::ioctl_ficlone` (see
`crates/fast_io/src/platform_copy/dispatch.rs:24`); failure cleans up
the partial destination and falls through. `copy_file_range` is gated by
a 64 KB threshold (`CFR_THRESHOLD`,
`crates/fast_io/src/platform_copy/dispatch.rs:34`) because the syscall's
setup cost exceeds the userspace path below that size.

### 2.2 File-to-socket (sender)

Dispatch lives in `send_file_to_fd`
(`crates/fast_io/src/sendfile.rs:147`).

| Priority | Backend | Requirement |
|----------|---------|-------------|
| 1 | `sendfile(2)` | Linux, length >= 64 KB (`SENDFILE_THRESHOLD`, `crates/fast_io/src/sendfile.rs:48`) |
| 2 | Buffered `libc::write` loop | Unix |
| 3 | `std::io::copy` via `BufReader`/`BufWriter` | All platforms |

Each `sendfile` invocation is capped at `SENDFILE_CHUNK_SIZE` (~2 GB,
`crates/fast_io/src/sendfile.rs:54`) so a signal cannot truncate a
single transfer. The `ZeroCopyPolicy::Disabled` variant
(`crates/fast_io/src/policy.rs`) routes through
`send_file_to_fd_with_policy`
(`crates/fast_io/src/sendfile.rs:176`) and forces the buffered path.

### 2.3 Socket-to-file (receiver)

Dispatch goes through `splice_to_file`
(`crates/fast_io/src/splice.rs:725`) which calls
`splice_fd_to_file_via_pipe`
(`crates/fast_io/src/splice.rs:221`).

| Priority | Backend | Requirement |
|----------|---------|-------------|
| 1 | `splice(2)` via pipe pair | Linux 2.6.17+, length >= 64 KB (`SPLICE_THRESHOLD`, `crates/fast_io/src/splice.rs:88`) |
| 2 | Buffered `libc::write` loop | Unix |
| 3 | `std::io::copy` | All platforms |

Splice availability is probed once - a zero-length splice to a throwaway
pipe pair - and cached in `SPLICE_SUPPORTED.get_or_init`
(`crates/fast_io/src/splice.rs:106`). The pipe capacity is set via
`fcntl(F_SETPIPE_SZ)` to `DEFAULT_PIPE_CAPACITY` (1 MiB,
`crates/fast_io/src/splice.rs:97`). Splice uses
`SPLICE_F_MOVE | SPLICE_F_MORE` for optimal page migration.

### 2.4 io_uring (substrate)

When io_uring is selected as the disk-commit backend, the operations
above are issued as SQEs rather than blocking syscalls. The submission
backend selection sits on top of the chain in 2.1-2.3:

`is_io_uring_available`
(`crates/fast_io/src/io_uring/config.rs:179`) returns `true` when all
three hold:

1. Running on Linux.
2. `uname().release` parses as >= 5.6 (`MIN_KERNEL_VERSION`,
   `crates/fast_io/src/io_uring/config.rs:19`).
3. A minimal `IoUring::new(4)` succeeds - rules out seccomp / container
   policy blocks.

The result is cached in two `AtomicBool`s
(`IO_URING_AVAILABLE` and `IO_URING_CHECKED`,
`crates/fast_io/src/io_uring/config.rs:22-23`) so steady-state calls
are a single relaxed load.

Ring construction follows its own internal fallback chain inside
`IoUringConfig::build_ring`:

| Priority | Ring type | Requirement |
|----------|-----------|-------------|
| 1 | SQPOLL ring | `CAP_SYS_NICE` (silent fallback flag at `crates/fast_io/src/io_uring/config.rs:30`) |
| 2 | Regular io_uring ring | Linux 5.6+, no seccomp block |
| 3 | Standard buffered I/O | Always |

Optional opcode probes (each cached in a `OnceLock` or atomic):

| Feature | Minimum kernel | Probe |
|---------|---------------|-------|
| Basic read / write / `IORING_REGISTER_FILES` | 5.6 | Implicit in `IoUring::new` |
| `IORING_SETUP_SQPOLL` | 5.6 (requires `CAP_SYS_NICE`) | Setup-time, falls back silently |
| `IORING_OP_SEND` | 5.6 | Implicit |
| `IORING_OP_LINKAT` | varies | `linkat_supported` (re-exported from `crates/fast_io/src/lib.rs:184`) |
| `IORING_OP_RENAMEAT` | varies | `renameat2_supported` |
| Provided-buffer ring (PBUF_RING) | 5.19 | `pbuf_ring_supported` |
| `statx` | varies | `statx_supported` (`STATX_MIN_KERNEL` at `crates/fast_io/src/lib.rs:181`) |

CLI control: `--io-uring` (force), `--no-io-uring` (disable),
`--io-uring-depth=N` (SQ depth, power of two, 1-32768; range constants
at `crates/fast_io/src/io_uring_depth.rs`).

Full Linux chain summary:

```
io_uring (if available, kernel 5.6+, depth N)
   v
FICLONE / copy_file_range / sendfile / splice (per-op selection above)
   v
std::fs::copy or std::io::copy (buffered, always)
```

---

## 3. macOS I/O Priority

### 3.1 Whole-file copy

Dispatch lives in `platform_copy_impl` for macOS
(`crates/fast_io/src/platform_copy/dispatch.rs:63`).

| Priority | Backend | Mechanism | Requirement |
|----------|---------|-----------|-------------|
| 1 | `clonefile(2)` | APFS CoW clone | APFS only, same device; O(1) |
| 2 | `fcopyfile(3)` | Kernel-accelerated copy via fds (`COPYFILE_DATA`) | All macOS filesystems |
| 3 | `std::fs::copy` | Buffered read/write | Always |

`clonefile_impl` (`crates/fast_io/src/platform_copy/dispatch.rs:151`)
wraps the libc FFI. `fcopyfile_impl` is the kernel-accelerated path
(`crates/fast_io/src/platform_copy/dispatch.rs:186`).

### 3.2 Streaming writer (disk-commit path)

For streamed writes that do not go through `platform_copy_impl` (the
disk-commit path used by the receiver), the chain is:

| Priority | Backend | Mechanism | Threshold |
|----------|---------|-----------|-----------|
| 1 | `MacosWriter` with `F_NOCACHE` + `writev` | `fcntl(F_NOCACHE, 1)` plus scatter-gather `writev` | `size_hint >= F_NOCACHE_THRESHOLD` (1 MiB, `crates/fast_io/src/macos_io.rs:34`) |
| 2 | `MacosWriter` (`writev` only) | Scatter-gather `writev` without `F_NOCACHE` | size below threshold |
| 3 | Standard buffered `std::io::Write` | `BufWriter` | Always (fallback when `MacosWriter` is not wired) |

`MacosWriter::create` (`crates/fast_io/src/macos_io.rs:84`) applies the
`F_NOCACHE` hint only when `size_hint >= F_NOCACHE_THRESHOLD`; below
that threshold the unified buffer cache is beneficial. `writev` batches
up to `MAX_IOV_COUNT` (64, `crates/fast_io/src/macos_io.rs:41`)
buffers per syscall. The macOS audit
([`macos-fastio-fallback.md`](../audits/macos-fastio-fallback.md))
documents which call sites have `MacosWriter` wired and which still use
plain `BufWriter`.

Full macOS chain summary:

```
clonefile (APFS, same device)
   v
fcopyfile (all macOS filesystems)
   v
MacosWriter F_NOCACHE + writev (size >= 1 MiB)
   v
std::fs::copy or BufWriter (always)
```

---

## 4. Windows I/O Priority

### 4.1 Whole-file copy

Dispatch lives in `platform_copy_impl` for Windows
(`crates/fast_io/src/platform_copy/dispatch.rs:104`).

| Priority | Backend | Mechanism | Requirement |
|----------|---------|-----------|-------------|
| 1 | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | ReFS block-level CoW clone | ReFS volume, same volume; O(1) |
| 2 | `CopyFileExW` + `COPY_FILE_NO_BUFFERING` | Win32 system copy with unbuffered hint | Files > 4 MiB (`NO_BUFFERING_THRESHOLD`, `crates/fast_io/src/platform_copy/dispatch.rs:106`) |
| 3 | `CopyFileExW` (default flags) | Win32 system copy | All volumes |
| 4 | `std::fs::copy` | Buffered read/write | Always |

ReFS detection runs through `is_refs_filesystem`
(`crates/fast_io/src/refs_detect.rs:55`) which calls
`GetVolumePathNameW` and `GetVolumeInformationByHandleW`, with results
cached per volume root in a `Mutex<Option<HashMap<PathBuf, bool>>>`
(`crates/fast_io/src/refs_detect.rs:79`).

### 4.2 Streaming I/O (disk-commit path)

The disk-commit `Writer` enum on Windows dispatches to IOCP via the
`fast_io::iocp` module
([`windows-iocp-file-write-status.md`](../audits/windows-iocp-file-write-status.md)).

| Priority | Backend | Requirement |
|----------|---------|-------------|
| 1 | `IocpDiskBatch` (IOCP writer pump) | Vista+; available when `is_iocp_available` returns `true` (`crates/fast_io/src/iocp/config.rs:91`) |
| 2 | Standard buffered `std::io::Write` | Always; used below the IOCP threshold or when `IocpPolicy::Disabled` |

`is_iocp_available` probes by creating a test `CreateIoCompletionPort`
and caches the outcome in an `AtomicU8` (`IOCP_STATUS` /
`AVAILABLE` / `UNAVAILABLE` sentinels,
`crates/fast_io/src/iocp/config.rs:92-94`). The optional
`FILE_SKIP_SET_EVENT_ON_HANDLE` flag is probed independently
(`skip_event_optimization_available`,
`crates/fast_io/src/iocp/config.rs:105`) and the status string is
emitted by `iocp_availability_reason`
(`crates/fast_io/src/iocp/config.rs:113`). Files below
`IOCP_MIN_FILE_SIZE` (64 KiB) use standard I/O regardless of the
policy because the async overhead exceeds the benefit. The
`IocpPolicy` enum (`Auto` / `Enabled` / `Disabled`) lives in
`crates/fast_io/src/policy.rs`.

Full Windows chain summary:

```
ReFS FSCTL_DUPLICATE_EXTENTS_TO_FILE (ReFS volume only)
   v
CopyFileExW + COPY_FILE_NO_BUFFERING (files > 4 MiB)
   v
CopyFileExW (default flags)
   v
IocpDiskBatch (streamed writes, files >= 64 KiB)
   v
std::fs::copy or BufWriter (always)
```

---

## 5. CPU SIMD Priority

### 5.1 Rolling checksum (`rsum`)

Per-architecture dispatch in
`crates/checksums/src/rolling/checksum/x86.rs` and
`crates/checksums/src/rolling/checksum/neon.rs`. Feature probes are
gated through the `feature_allowed` policy
(`crates/checksums/src/cpu_features.rs:225`).

**x86 / x86_64** (dispatch table at
`crates/checksums/src/rolling/checksum/x86.rs:79-86`):

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | AVX2 | 32 | `is_x86_feature_detected!("avx2")` + `feature_allowed(SimdFeature::Avx2)` |
| 2 | SSE2 | 16 | `is_x86_feature_detected!("sse2")` + `feature_allowed(SimdFeature::Sse2)` |
| 3 | Scalar (unrolled) | 4 | Always |

**aarch64** (dispatch at
`crates/checksums/src/rolling/checksum/neon.rs:63-67`):

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | NEON | 16 | `is_aarch64_feature_detected!("neon")` (mandatory on aarch64) |
| 2 | Scalar (unrolled) | 4 | Always |

**Other architectures:** scalar only.

All SIMD paths tail-call the scalar implementation for trailing bytes
that do not fill a full SIMD lane, preserving byte-for-byte parity with
upstream `checksum.c:get_checksum1()` on every input length.

### 5.2 MD5 batch hashing

`crates/checksums/src/simd_batch/md5_dispatcher.rs` selects the widest
backend the host CPU supports, capped by the `SimdLevel` override.

| Priority | Backend | Parallel lanes | Guard |
|----------|---------|---------------|-------|
| 1 | AVX-512 | 16 | `avx512f` + `avx512bw` (x86_64 only) |
| 2 | AVX2 | 8 | `avx2` (x86_64 only) |
| 3 | SSE4.1 | 4 | `sse4.1` (x86_64 only) |
| 4 | SSSE3 | 4 | `ssse3` (x86_64 only) |
| 5 | SSE2 | 4 | `sse2` (always present on x86_64) |
| 6 | NEON | 4 | always present on aarch64 |
| 7 | WASM SIMD | 4 | wasm32 with simd128 |
| 8 | Scalar | 1 | Always |

The override layer
(`crates/checksums/src/cpu_features.rs:225-241`) intersects the
dispatch decision with the `--simd` CLI flag.

### 5.3 Zero-byte detection (sparse support)

`crates/fast_io/src/zero_detect.rs` uses SIMD to scan buffers for
leading zero runs.

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | AVX2 | 32 | `is_x86_feature_detected!("avx2")` (x86/x86_64) |
| 2 | SSE2 | 16 | `is_x86_feature_detected!("sse2")` (x86/x86_64) |
| 3 | NEON | 16 | `is_aarch64_feature_detected!("neon")` (aarch64) |
| 4 | Scalar via `u128` | 16 | Always |

### 5.4 SHA-256

The `sha2` crate's `cpufeatures` integration automatically uses SHA-NI
on x86_64 and ARMv8 crypto extensions on aarch64. Detection is
transparent. `sha256_hardware_acceleration_available`
(`crates/checksums/src/strong/sha256.rs:142`) reports whether the
running CPU exposes SHA-256 hardware support.

### 5.5 SIMD override matrix

Stored in a process-global `AtomicU8` (`OVERRIDE` at
`crates/checksums/src/cpu_features.rs:126`). The `--simd` CLI flag
installs the override at startup; subsequent calls succeed only when
they request the same level (`set_simd_override`,
`crates/checksums/src/cpu_features.rs:164`).

| `--simd` | AVX-512 | AVX2 | SSE4.1 | SSSE3 | SSE2 | NEON |
|----------|:-------:|:----:|:------:|:-----:|:----:|:----:|
| `auto` | yes | yes | yes | yes | yes | yes |
| `avx512` | yes | yes | yes | yes | yes | yes |
| `avx2` | no | yes | yes | yes | yes | no |
| `sse4` | no | no | yes | yes | yes | no |
| `neon` | no | no | no | no | no | yes |
| `none` | no | no | no | no | no | no |

A pinned level wider than the host CPU silently degrades to the next
available backend.

---

## 6. Crypto Priority

oc-rsync does not implement its own SSH transport - it spawns OpenSSH
and relies on OpenSSH for cipher negotiation. The only hardware-aware
choice oc-rsync makes is whether to *prefer* AES-GCM by injecting a
`-c aes128-gcm@openssh.com,aes256-gcm@openssh.com` argument into the
SSH argv.

See [`docs/audits/should-inject-aes-gcm-ciphers.md`](../audits/should-inject-aes-gcm-ciphers.md)
for the full analysis (issue #1627).

### 6.1 Hardware AES detection

`has_hardware_aes`
(`crates/rsync_io/src/ssh/builder.rs:601`) returns `true` when the
host CPU exposes AES instructions:

| Architecture | Probe |
|--------------|-------|
| x86 / x86_64 | `is_x86_feature_detected!("aes")` (AES-NI: Intel Westmere 2010+, AMD Bulldozer 2011+) |
| aarch64 | `is_aarch64_feature_detected!("aes")` (ARMv8 Cryptography Extensions: Apple M-series, AWS Graviton, ARMv8.1+) |
| Other | Always `false` |

The probe result is cached in a function-local `OnceLock<bool>`
(`HAS_AES` at `crates/rsync_io/src/ssh/builder.rs:602`) so repeated
calls do not re-issue feature-detection syscalls.

### 6.2 Cipher selection chain

`should_inject_aes_gcm_ciphers`
(`crates/rsync_io/src/ssh/builder.rs:500`) decides per invocation:

| Priority | Cipher family | Selected when |
|----------|---------------|---------------|
| 1 | AES-GCM (`aes128-gcm@openssh.com,aes256-gcm@openssh.com`) | `prefer_aes_gcm != Some(false)` AND `has_hardware_aes()` AND program is `ssh[.exe]` AND no existing `-c` cipher |
| 2 | ChaCha20-Poly1305 (or OpenSSH default) | Any precondition above is false |

The argv injection lands in `command_parts`
(`crates/rsync_io/src/ssh/builder.rs:417-422`).

Rationale: AES-NI / ARMv8 AES deliver 2-4x the throughput of software
ChaCha20-Poly1305 on hardware that supports them
(`crates/rsync_io/src/ssh/builder.rs:209`). On CPUs without AES
instructions, OpenSSH's `chacha20-poly1305@openssh.com` is faster
because it is a pure software cipher optimised for CPUs lacking AES
pipelines (`crates/rsync_io/src/ssh/builder.rs:195`).

### 6.3 CLI overrides

| Flag | Effect | `prefer_aes_gcm` value |
|------|--------|-----------------------|
| `--aes` | Force AES-GCM injection even if hardware detection returns `false` | `Some(true)` |
| `--no-aes` | Suppress AES-GCM injection regardless of hardware | `Some(false)` |
| (default) | Auto-detect via `has_hardware_aes` | `None` |

The flag flows from
`crates/cli/src/frontend/arguments/parser/mod.rs:279` through
`ParsedArgs::prefer_aes_gcm` into `ClientConfig`
(`crates/core/src/client/config/builder/network.rs:45`) and is applied
in `build_ssh_connection`
(`crates/core/src/client/remote/ssh_transfer.rs:286`).

---

## 7. Probe Caching Pattern

Every runtime detection in oc-rsync follows the same shape: probe once,
cache the result, never re-probe. The cache primitive differs by
subsystem but the contract is identical - readers see a constant value
after the first call.

| Subsystem | Cache | Reset for tests |
|-----------|-------|-----------------|
| Rolling checksum CPU features | `OnceLock<FeatureLevel>` (`crates/checksums/src/rolling/checksum/x86.rs:79`) | n/a |
| NEON availability | `OnceLock<bool>` (`crates/checksums/src/rolling/checksum/neon.rs:63`) | n/a |
| SIMD level override | `AtomicU8` (`crates/checksums/src/cpu_features.rs:126`) | `reset_simd_override_for_tests` |
| SHA-256 hardware | Implicit per-call (cheap CPUID) (`crates/checksums/src/strong/sha256.rs:142`) | n/a |
| io_uring availability | Pair of `AtomicBool` (`crates/fast_io/src/io_uring/config.rs:22`) | n/a |
| SQPOLL fallback flag | `AtomicBool` (`crates/fast_io/src/io_uring/config.rs:30`) | n/a |
| `splice(2)` availability | `OnceLock<bool>` via `SPLICE_SUPPORTED` (`crates/fast_io/src/splice.rs:103`) | n/a |
| IOCP availability | `AtomicU8` (`crates/fast_io/src/iocp/config.rs:91`) | n/a |
| `FILE_SKIP_SET_EVENT_ON_HANDLE` | `AtomicBool` (`crates/fast_io/src/iocp/config.rs:105`) | n/a |
| ReFS per-volume detection | `Mutex<Option<HashMap<PathBuf, bool>>>` (`crates/fast_io/src/refs_detect.rs:79`) | `clear_refs_cache` (`crates/fast_io/src/refs_detect.rs:64`) |
| AES hardware | Function-local `OnceLock<bool>` (`crates/rsync_io/src/ssh/builder.rs:602`) | n/a |

`OnceLock` and `AtomicU8` are preferred over `Mutex` for read-heavy
caches; the ReFS cache uses a `Mutex<HashMap>` because the key space is
unbounded (one entry per volume root encountered during a session) and
entries are written exactly once. All caches survive for the process
lifetime.

CLI overrides bypass the probe entirely - they set the cache directly
or pin the dispatcher at the requested level. An unsupported override
fails fast with a diagnostic naming the missing capability rather than
silently degrading, except for `--simd` levels wider than the host CPU
which always degrade silently because the override is a cap, not a
floor.

---

## 8. Flowchart: which backend wins?

For a given (platform, kernel, CPU) tuple, the dispatch resolves as
follows. Each box represents an independent decision; the outputs in
`<angle brackets>` are the cached value(s) consulted at runtime.

### 8.1 Disk write (receiver / disk-commit thread)

```
platform == Linux ?
  yes -> is_io_uring_available()                      [config.rs:179]
            yes -> IoUringDiskBatch                   (--io-uring=auto/force)
            no  -> std::fs::File + BufWriter
  no  -> platform == Windows ?
            yes -> is_iocp_available()                [iocp/config.rs:91]
                     yes -> IocpDiskBatch
                     no  -> std::fs::File + BufWriter
            no  -> platform == macOS ?
                     yes -> size_hint >= 1 MiB ?
                              yes -> MacosWriter (F_NOCACHE + writev)
                                                      [macos_io.rs:84]
                              no  -> BufWriter (writev only)
                     no  -> std::fs::File + BufWriter
```

### 8.2 Whole-file copy

```
platform == Linux ?
  yes -> try FICLONE                                  [dispatch.rs:24]
            ok  -> done (O(1) reflink)
            err -> size >= 64 KiB ?
                     yes -> try copy_file_range       [dispatch.rs:39]
                              ok  -> done
                              err -> std::fs::copy
                     no  -> std::fs::copy
  no  -> platform == macOS ?
            yes -> try clonefile                      [dispatch.rs:69]
                     ok  -> done (APFS CoW)
                     err -> try fcopyfile             [dispatch.rs:82]
                              ok  -> done
                              err -> std::fs::copy
            no  -> platform == Windows ?
                     yes -> is_refs_filesystem(dst) ? [refs_detect.rs:55]
                              yes -> try FSCTL_DUPLICATE_EXTENTS
                                       ok  -> done
                                       err -> CopyFileExW
                              no  -> CopyFileExW (+ NO_BUFFERING if > 4 MiB)
                     no  -> std::fs::copy
```

### 8.3 SIMD rolling checksum

```
arch == x86_64 ?
  yes -> feature_allowed(Avx2) && cpuid("avx2") ?    [cpu_features.rs:225]
            yes -> AVX2 path (32 bytes/iter)         [x86.rs:79]
            no  -> feature_allowed(Sse2) && cpuid("sse2") ?
                     yes -> SSE2 path (16 bytes/iter)
                     no  -> scalar (4 bytes/iter)
  no  -> arch == aarch64 ?
            yes -> feature_allowed(Neon) ?           [cpu_features.rs:225]
                     yes -> NEON path (16 bytes/iter) [neon.rs:63]
                     no  -> scalar (4 bytes/iter)
            no  -> scalar (4 bytes/iter)
```

### 8.4 SSH cipher selection

```
program is ssh[.exe] && no existing -c flag ?
  yes -> prefer_aes_gcm == Some(false) ?              [builder.rs:500]
            yes -> let OpenSSH default (likely ChaCha20)
            no  -> has_hardware_aes() ?               [builder.rs:601]
                     yes -> inject -c aes128-gcm,aes256-gcm
                     no  -> let OpenSSH default (likely ChaCha20)
  no  -> let OpenSSH default
```

---

## 9. Platform Availability Matrix

### 9.1 Compute (SIMD)

| Feature | Linux x86_64 | Linux aarch64 | macOS x86_64 | macOS aarch64 | Windows x86_64 |
|---------|:------------:|:-------------:|:------------:|:-------------:|:--------------:|
| AVX-512 (MD5 batch) | CPUID | - | CPUID | - | CPUID |
| AVX2 (rolling + MD5 + zero) | CPUID | - | CPUID | - | CPUID |
| SSE4.1 / SSSE3 (MD5 batch) | CPUID | - | CPUID | - | CPUID |
| SSE2 (rolling + MD5 + zero) | CPUID | - | CPUID | - | CPUID |
| NEON (rolling + MD5 + zero) | - | always | - | always | - |
| SHA-NI (SHA-256) | CPUID | - | CPUID | - | CPUID |
| ARMv8 SHA (SHA-256) | - | CPUID | - | CPUID | - |
| AES-NI (cipher pref) | CPUID | - | CPUID | - | CPUID |
| ARMv8 AES (cipher pref) | - | CPUID | - | CPUID | - |

### 9.2 I/O backends

| Feature | Linux | macOS | Windows |
|---------|:-----:|:-----:|:-------:|
| io_uring | kernel >= 5.6 | - | - |
| IOCP | - | - | Vista+ (always) |
| `copy_file_range` | 4.5+ same-fs, 5.3+ cross-fs | - | - |
| `sendfile` | yes | - | - |
| `splice` | 2.6.17+ | - | - |
| `FICLONE` (reflink) | Btrfs / XFS / bcachefs | - | - |
| `clonefile` (reflink) | - | APFS | - |
| `fcopyfile` | - | yes | - |
| `MacosWriter` (`F_NOCACHE` + `writev`) | - | yes (size >= 1 MiB) | - |
| `CopyFileExW` | - | - | yes |
| ReFS reflink | - | - | ReFS volume |
| `O_TMPFILE` | 3.11+ (ext4 / Btrfs / XFS) | - | - |
| `SEEK_HOLE` / `SEEK_DATA` | 3.1+ | APFS | - |
| FIEMAP | yes | - | - |

---

## 10. Diagnosing the Active Path

### 10.1 `--version` output

Running `oc-rsync --version` prints sections relevant to hardware
acceleration:

1. **Optimizations** - lists compile-time and runtime capabilities:
   `SIMD-roll`, `asm-roll`, `openssl-crypto`, `asm-MD5`, the active
   allocator (`jemalloc` on Unix, `mimalloc` on Windows),
   `copy-file-range`, `io-uring`, `parallel`, `mmap`.
2. **Platform I/O** - runtime-detected fast I/O paths, e.g.
   `copy_file_range, sendfile, splice, FICLONE, O_TMPFILE, io_uring`.
3. **io_uring** - detailed availability line, e.g.
   `io_uring: compiled in, available (kernel 6.1, 48 ops)` or
   `io_uring: compiled in, unavailable (kernel 4.19, requires >= 5.6)`.
4. **IOCP** (Windows builds) - detailed availability, e.g.
   `compiled in, available, FILE_SKIP_SET_EVENT_ON_HANDLE active`.

Source: `crates/core/src/version/report/renderer.rs`,
`crates/core/src/version/report/config.rs`.

### 10.2 Runtime diagnostic functions

For programmatic inspection (integration tests, benchmark harnesses):

- `checksums::simd_acceleration_available()` - rolling SIMD active
- `checksums::simd_batch::active_backend()` - MD5 batch backend name
- `checksums::simd_batch::parallel_lanes()` - SIMD lane count
- `checksums::strong::sha256_hardware_acceleration_available()` -
  SHA-NI / ARMv8 SHA active (`crates/checksums/src/strong/sha256.rs:142`)
- `fast_io::is_io_uring_available()` -
  (`crates/fast_io/src/io_uring/config.rs:179`)
- `fast_io::io_uring_availability_reason()` - human-readable reason
- `fast_io::io_uring_kernel_info()` - structured kernel info
- `fast_io::sqpoll_fell_back()` -
  (`crates/fast_io/src/io_uring/config.rs:45`)
- `fast_io::is_iocp_available()` -
  (`crates/fast_io/src/iocp/config.rs:91`)
- `fast_io::skip_event_optimization_available()` -
  (`crates/fast_io/src/iocp/config.rs:105`)
- `fast_io::iocp_status_detail()` - IOCP status string
- `fast_io::platform_io_capabilities()` - list of available fast I/O
- `fast_io::is_splice_available()` -
  (`crates/fast_io/src/splice.rs:103`)

---

## 11. CLI Override Summary

| Flag | Subsystem | Values | Default |
|------|-----------|--------|---------|
| `--simd` | SIMD checksum dispatch | `auto`, `avx512`, `avx2`, `sse4`, `neon`, `none` | `auto` |
| `--io-uring` / `--no-io-uring` | io_uring backend | enable / disable | auto |
| `--io-uring-depth` | io_uring SQ depth | power-of-two integer 1-32768 | 64 |
| `--cow` / `--no-cow` | FS-level reflink (FICLONE, clonefile, ReFS) | enable / disable | `--cow` |
| `--zero-copy` / `--no-zero-copy` | I/O-level zero-copy (sendfile, splice, cfr) | enable / disable | auto |
| `--sparse-detect` | Hole detection strategy | `auto`, `seek`, `map`, `none` | `auto` |
| `--aes` / `--no-aes` | Prefer AES-GCM cipher injection for SSH | enable / disable | auto |

**Independence of `--cow` and `--zero-copy`:** the two policies are
orthogonal. `--cow` controls filesystem extent sharing (reflinks).
`--zero-copy` controls I/O-level kernel-side data movement. A transfer
can use reflink without sendfile (whole-file CoW clone) or sendfile
without reflink (network send of a file). Disabling either does not
affect the other.

**Override semantics:** `auto` runs the precedence chains above. Any
other value pins that backend. For SIMD, a pinned level wider than the
host CPU degrades silently to the next available backend. For I/O
backends, a pinned backend that is not available fails fast with a
diagnostic naming the missing capability. For `--aes`, the override
forces the cipher request even when hardware detection would otherwise
suppress it; whether OpenSSH accepts the cipher is then OpenSSH's
decision.
