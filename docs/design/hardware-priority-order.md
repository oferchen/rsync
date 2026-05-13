# Hardware Acceleration Priority Order

Tracks issue #2121.

## 1. Goal

At runtime, oc-rsync auto-selects between hardware fast paths for SIMD
checksum computation, asynchronous I/O backends, platform copy
mechanisms, zero-copy network paths, and sparse-file detection. This
document captures the precedence each subsystem follows so operators can
predict which backend will be picked on their host - and override it via
CLI flags when the default is wrong (debugging, benchmarking, regression
bisects).

Selection runs once per process at startup. Results are cached in
`OnceLock` or `AtomicU8` so repeated calls share the probe. CLI
overrides bypass the probe entirely and pin a specific backend; an
unsupported override is a hard error rather than a silent fallback.

---

## 2. SIMD Checksum Acceleration

### 2.1 Rolling Checksum (rsum)

The rolling checksum (`crates/checksums/src/rolling/checksum/`) mirrors
upstream `checksum.c:get_checksum1()` with architecture-specific SIMD
fast paths. Feature detection uses `is_x86_feature_detected!` and
`is_aarch64_feature_detected!`, cached in a `OnceLock` on first use.

**x86 / x86_64 dispatch ladder:**

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | AVX2 | 32 | `is_x86_feature_detected!("avx2")` |
| 2 | SSE2 | 16 | `is_x86_feature_detected!("sse2")` |
| 3 | Scalar | 4 (unrolled) | always |

**aarch64 dispatch ladder:**

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | NEON | 16 | `is_aarch64_feature_detected!("neon")` (mandatory on aarch64) |
| 2 | Scalar | 4 (unrolled) | always |

**Other architectures:** scalar only.

All SIMD paths tail-call into the scalar implementation for trailing
bytes that do not fill a full SIMD lane, preserving byte-for-byte
parity with upstream on every input length.

**Source:** `crates/checksums/src/rolling/checksum/x86.rs`,
`crates/checksums/src/rolling/checksum/neon.rs`,
`crates/checksums/src/rolling/checksum/mod.rs`

### 2.2 MD5 Batch Hashing

The `simd_batch` module (`crates/checksums/src/simd_batch/`) computes
MD5 digests for multiple independent inputs in parallel using multi-lane
SIMD. The dispatch ladder is wider than the rolling checksum because
md5-simd crate backends exploit narrower instruction sets.

**Dispatch ladder (all architectures):**

| Priority | Backend | Parallel Lanes | Guard |
|----------|---------|---------------|-------|
| 1 | AVX-512 | 16 | `avx512f` + `avx512bw` (x86_64 only) |
| 2 | AVX2 | 8 | `avx2` (x86_64 only) |
| 3 | SSE4.1 | 4 | `sse4.1` (x86_64 only, blendv optimization) |
| 4 | SSSE3 | 4 | `ssse3` (x86_64 only, pshufb optimization) |
| 5 | SSE2 | 4 | `sse2` (always true on x86_64) |
| 6 | NEON | 4 | always true on aarch64 |
| 7 | WASM SIMD | 4 | wasm32 with simd128 |
| 8 | Scalar | 1 | always |

Each capability check is gated by the runtime SIMD override
(`cpu_features::feature_allowed`) so the `--simd` CLI flag can pin
dispatch to a specific level even on hosts that advertise wider support.

**Source:** `crates/checksums/src/simd_batch/md5_dispatcher.rs`

### 2.3 Zero-Byte Detection (Sparse Support)

The `zero_detect` module (`crates/fast_io/src/zero_detect.rs`) uses
SIMD to scan buffers for leading zero runs - the core operation for
sparse file hole detection.

**Dispatch ladder:**

| Priority | Backend | Bytes/iter | Guard |
|----------|---------|-----------|-------|
| 1 | AVX2 | 32 | `is_x86_feature_detected!("avx2")` (x86/x86_64) |
| 2 | SSE2 | 16 | `is_x86_feature_detected!("sse2")` (x86/x86_64) |
| 3 | NEON | 16 | `is_aarch64_feature_detected!("neon")` (aarch64) |
| 4 | Scalar | 16 (via `u128`) | always |

**Source:** `crates/fast_io/src/zero_detect.rs`

### 2.4 SHA-256 Hardware Acceleration

The `sha2` crate's `cpufeatures` integration automatically uses SHA-NI
on x86_64 and ARMv8 crypto extensions on aarch64. Detection is
transparent to oc-rsync - the crate handles it internally. The
`sha256_hardware_acceleration_available()` function reports whether the
running CPU exposes SHA-256 hardware support.

**Source:** `crates/checksums/src/strong/sha256.rs`

### 2.5 SIMD Override Mechanism

The `cpu_features` module (`crates/checksums/src/cpu_features.rs`)
provides a process-global override stored in an `AtomicU8`. The `--simd`
CLI flag installs this override at startup. The override is intersected
with host CPUID - requesting a backend wider than the host's capabilities
silently degrades to the next available backend.

**Override permission matrix:**

| `--simd` | AVX-512 | AVX2 | SSE4.1 | SSSE3 | SSE2 | NEON |
|----------|:-------:|:----:|:------:|:-----:|:----:|:----:|
| `auto` | yes | yes | yes | yes | yes | yes |
| `avx512` | yes | yes | yes | yes | yes | yes |
| `avx2` | no | yes | yes | yes | yes | no |
| `sse4` | no | no | yes | yes | yes | no |
| `neon` | no | no | no | no | no | yes |
| `none` | no | no | no | no | no | no |

---

## 3. Asynchronous I/O Backends

### 3.1 io_uring (Linux)

**Crate:** `fast_io` (`crates/fast_io/src/io_uring/`)

**Compile-time gate:** `#[cfg(all(target_os = "linux", feature = "io_uring"))]`

**Runtime detection** (cached in a process-wide atomic):

1. Parse `uname().release` for kernel major.minor >= 5.6
2. Attempt `IoUring::new(4)` to verify the syscall is not blocked by
   seccomp, container runtime, or permission restriction
3. On first actual I/O, construct the real ring via `IoUringConfig::build_ring`

**Fallback chain for ring creation:**

| Priority | Ring Type | Requirement |
|----------|----------|-------------|
| 1 | SQPOLL ring | `CAP_SYS_NICE` or root |
| 2 | Regular io_uring ring | Linux 5.6+, no seccomp block |
| 3 | Standard buffered I/O | always |

**Optional feature probes:**

| Feature | Minimum Kernel | Notes |
|---------|---------------|-------|
| Basic read/write | 5.6 | `io_uring_setup`, `io_uring_enter` |
| `IORING_REGISTER_FILES` | 5.6 | ~50ns/SQE savings |
| `IORING_SETUP_SQPOLL` | 5.6 | Needs `CAP_SYS_NICE`, silent fallback |
| `IORING_OP_SEND` | 5.6 | Socket writer batching |
| Provided-buffer ring (PBUF_RING) | 5.19 | Ring-mapped supplied buffers |
| `IORING_OP_LINKAT` | varies | Detected at runtime |
| `IORING_OP_RENAMEAT` | varies | `RENAME_NOREPLACE` / `RENAME_EXCHANGE` |

**CLI control:** `--io-uring` (force), `--no-io-uring` (disable),
`--io-uring-depth=N` (SQ depth, power of two, 1-32768). Default: auto.

**Source:** `crates/fast_io/src/io_uring/mod.rs`,
`crates/fast_io/src/lib.rs`

### 3.2 IOCP (Windows)

**Crate:** `fast_io` (`crates/fast_io/src/iocp/`)

**Compile-time gate:** `#[cfg(all(target_os = "windows", feature = "iocp"))]`

**Runtime detection** (cached after first probe):
Creates a test completion port via `CreateIoCompletionPort`. On Windows
Vista+, IOCP is always available. Files smaller than 64 KB
(`IOCP_MIN_FILE_SIZE`) use standard I/O regardless of this policy since
the async overhead exceeds the benefit.

**Optional optimization:** `FILE_SKIP_SET_EVENT_ON_HANDLE` - reduces
per-operation overhead when available. Detected at runtime.

**CLI control:** mirrors `IoUringPolicy` pattern but is not currently
exposed as a separate CLI flag; controlled implicitly by the I/O backend
selection.

**Source:** `crates/fast_io/src/iocp/mod.rs`,
`crates/fast_io/src/iocp/config.rs`

### 3.3 Standard Buffered I/O

The universal fallback on all platforms. Uses `BufReader`/`BufWriter`
with 64 KB default buffers. Always available.

---

## 4. Platform Copy Dispatch

The `platform_copy` module (`crates/fast_io/src/platform_copy/`)
abstracts file-to-file copying behind the `PlatformCopy` trait. Each
platform has its own dispatch chain, with the strongest method tried
first and progressively weaker fallbacks.

### 4.1 Linux

| Priority | Method | Mechanism | Requirement |
|----------|--------|-----------|-------------|
| 1 | `FICLONE` | `ioctl(FICLONE)` via `rustix::fs::ioctl_ficlone` | Btrfs, XFS (reflink), bcachefs. Same device. O(1). |
| 2 | `copy_file_range` | Kernel zero-copy file-to-file | Linux 4.5+ (same-fs), 5.3+ (cross-fs). Files >= 64 KB. |
| 3 | `std::fs::copy` | Buffered read/write | always |

### 4.2 macOS

| Priority | Method | Mechanism | Requirement |
|----------|--------|-----------|-------------|
| 1 | `clonefile` | CoW clone | APFS only. O(1). |
| 2 | `fcopyfile` | Kernel-accelerated data copy via fd | All macOS filesystems. |
| 3 | `std::fs::copy` | Buffered read/write | always |

### 4.3 Windows

| Priority | Method | Mechanism | Requirement |
|----------|--------|-----------|-------------|
| 1 | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | ReFS block-level CoW clone | ReFS volume, same volume. O(1). |
| 2 | `CopyFileExW` | System copy API | All volumes. `COPY_FILE_NO_BUFFERING` for files > 4 MB. |
| 3 | `std::fs::copy` | Buffered read/write | always |

### 4.4 CLI Control

- `--cow` (default) / `--no-cow` - controls filesystem-level CoW
  reflink cloning (`FICLONE`, `clonefile`, ReFS `FSCTL_DUPLICATE_EXTENTS`).
  When disabled, forces `std::fs::copy` for whole-file copies. Mapped to
  `CowPolicy::Auto` / `CowPolicy::Disabled`.

**Source:** `crates/fast_io/src/platform_copy/dispatch.rs`,
`crates/fast_io/src/platform_copy/types.rs`

---

## 5. Zero-Copy Network Paths

The `fast_io` crate provides zero-copy data movement between file
descriptors and sockets, orthogonal to filesystem-level CoW.

### 5.1 sendfile (file-to-socket)

| Priority | Backend | Requirement |
|----------|---------|-------------|
| 1 | `sendfile(2)` | Linux, file >= 64 KB |
| 2 | Buffered `read`/`write` via `libc::write` | Unix |
| 3 | Buffered I/O via `std::io` | All platforms |

Transfers data in chunks up to ~2 GB to avoid signal interruption.

**Source:** `crates/fast_io/src/sendfile.rs`

### 5.2 splice (socket-to-file)

| Priority | Backend | Requirement |
|----------|---------|-------------|
| 1 | `splice(2)` via pipe intermediary | Linux 2.6.17+, transfer >= 64 KB |
| 2 | Buffered `read`/`write` via `libc` | Unix |
| 3 | `std::io::copy` | All platforms |

Splice availability is probed once (zero-length splice to a test pipe
pair) and cached in a `OnceLock`. Uses `SPLICE_F_MOVE | SPLICE_F_MORE`
flags for optimal page migration.

**Source:** `crates/fast_io/src/splice.rs`

### 5.3 CLI Control

- `--zero-copy` / `--no-zero-copy` - controls I/O-level zero-copy
  syscalls (`sendfile`, `splice`, `copy_file_range`, io_uring
  `IORING_OP_SEND_ZC`). Orthogonal to `--cow`. Mapped to
  `ZeroCopyPolicy::Auto` / `ZeroCopyPolicy::Enabled` /
  `ZeroCopyPolicy::Disabled`.

When `--no-zero-copy` is set, all zero-copy syscalls are bypassed and
data flows through userspace read/write loops.

---

## 6. Sparse File Detection

The `--sparse` flag controls *whether* holes are punched. The
`--sparse-detect` flag controls *how* holes are found in source files.

| `--sparse-detect` | Strategy | Platform Notes |
|--------------------|----------|---------------|
| `auto` (default) | Prefer `SEEK_HOLE`/`SEEK_DATA`, fall back to byte scan | Linux, macOS (APFS) |
| `seek` | Force `SEEK_HOLE`/`SEEK_DATA` | No holes on unsupported FS |
| `map` | FIEMAP extent mapping | Linux only; degrades to seek on other platforms |
| `none` | Disabled; zero runs written verbatim | All platforms |

Zero-run scanning itself uses the SIMD-accelerated `zero_detect` module
(section 2.3).

**Source:** `crates/engine/src/local_copy/executor/file/sparse/mod.rs`

---

## 7. Platform Availability Matrix

### 7.1 Compute (SIMD)

| Feature | Linux x86_64 | Linux aarch64 | macOS x86_64 | macOS aarch64 | Windows x86_64 |
|---------|:------------:|:-------------:|:------------:|:-------------:|:--------------:|
| AVX-512 (MD5 batch) | CPUID | - | CPUID | - | CPUID |
| AVX2 (rolling + MD5 + zero) | CPUID | - | CPUID | - | CPUID |
| SSE4.1/SSSE3 (MD5 batch) | CPUID | - | CPUID | - | CPUID |
| SSE2 (rolling + MD5 + zero) | CPUID | - | CPUID | - | CPUID |
| NEON (rolling + MD5 + zero) | - | always | - | always | - |
| SHA-NI (SHA-256) | CPUID | - | CPUID | - | CPUID |
| ARMv8 crypto (SHA-256) | - | CPUID | - | CPUID | - |

### 7.2 I/O Backends

| Feature | Linux | macOS | Windows |
|---------|:-----:|:-----:|:-------:|
| io_uring | kernel >= 5.6 | - | - |
| IOCP | - | - | Vista+ (always) |
| `copy_file_range` | 4.5+ (same-fs), 5.3+ (cross) | - | - |
| `sendfile` | yes | - | - |
| `splice` | 2.6.17+ | - | - |
| `FICLONE` (reflink) | Btrfs/XFS/bcachefs | - | - |
| `clonefile` (reflink) | - | APFS | - |
| `fcopyfile` | - | yes | - |
| `CopyFileExW` | - | - | yes |
| ReFS reflink | - | - | ReFS volume |
| `O_TMPFILE` | 3.11+ (ext4/Btrfs/XFS) | - | - |
| `SEEK_HOLE`/`SEEK_DATA` | 3.1+ | APFS | - |
| FIEMAP | yes | - | - |
| mmap | yes | yes | - |

---

## 8. Diagnosing the Active Path

### 8.1 `--version` Output

Running `oc-rsync --version` prints three sections relevant to hardware
acceleration:

1. **Optimizations** - lists compile-time and runtime capabilities:
   `SIMD-roll`, `asm-roll`, `openssl-crypto`, `asm-MD5`, `mimalloc`,
   `copy-file-range`, `io-uring`, `parallel`, `mmap`. Each is `yes`/`no`
   based on runtime detection.

2. **Platform I/O** - lists runtime-detected fast I/O paths. Example:
   `Platform I/O: copy_file_range, sendfile, splice, FICLONE, O_TMPFILE, io_uring`

3. **io_uring** - detailed availability line. Examples:
   - `io_uring: compiled in, available (kernel 6.1, 48 ops)`
   - `io_uring: compiled in, unavailable (kernel 4.19, requires >= 5.6)`
   - `io_uring: not available (platform is not Linux)`

4. **IOCP** - detailed availability (Windows builds):
   - `compiled in, available, FILE_SKIP_SET_EVENT_ON_HANDLE active`
   - `not available (platform is not Windows)`

**Source:** `crates/core/src/version/report/renderer.rs`,
`crates/core/src/version/report/config.rs`

### 8.2 Verbose Output (`-vv`)

The transfer summary printed at the end of a `-vv` run includes an
**I/O backend** line showing the active backend for the session:
`standard I/O`, `io_uring`, or `io_uring (SQPOLL)`.

**Source:** `crates/cli/src/frontend/progress/render.rs`

### 8.3 Runtime Diagnostic Functions

For programmatic inspection (useful in integration tests or
benchmarking harnesses):

- `checksums::simd_acceleration_available()` - rolling checksum SIMD
- `checksums::simd_batch::active_backend()` - MD5 batch backend name
- `checksums::simd_batch::parallel_lanes()` - number of SIMD lanes
- `fast_io::is_io_uring_available()` - io_uring availability
- `fast_io::io_uring_availability_reason()` - human-readable reason
- `fast_io::io_uring_kernel_info()` - structured kernel info
- `fast_io::is_iocp_available()` - IOCP availability
- `fast_io::iocp_status_detail()` - IOCP status string
- `fast_io::platform_io_capabilities()` - list of available fast I/O
- `fast_io::is_splice_available()` - splice(2) availability

---

## 9. CLI Override Summary

| Flag | Subsystem | Values | Default |
|------|-----------|--------|---------|
| `--simd` | SIMD checksum dispatch | `auto`, `avx512`, `avx2`, `sse4`, `neon`, `none` | `auto` |
| `--io-uring` / `--no-io-uring` | io_uring backend | enable / disable | auto |
| `--io-uring-depth` | io_uring SQ depth | power-of-two integer 1-32768 | 64 |
| `--cow` / `--no-cow` | FS-level reflink (FICLONE, clonefile, ReFS) | enable / disable | `--cow` |
| `--zero-copy` / `--no-zero-copy` | I/O-level zero-copy (sendfile, splice, cfr) | enable / disable | auto |
| `--sparse-detect` | Hole detection strategy | `auto`, `seek`, `map`, `none` | `auto` |

**Independence of `--cow` and `--zero-copy`:** the two policies are
orthogonal. `--cow` controls filesystem extent sharing (reflinks).
`--zero-copy` controls I/O-level kernel-side data movement. A transfer
can use reflink without sendfile (whole-file CoW clone) or sendfile
without reflink (network send of a file). Disabling either does not
affect the other.

**Override semantics:** `auto` runs the precedence chains documented
above. Any other value pins that backend. For SIMD, a pinned level that
is wider than the host CPU degrades silently to the next available
backend. For I/O backends, a pinned backend that is not available fails
fast with a diagnostic naming the missing capability.

---

## 10. Runtime Detection Mechanisms

| Mechanism | Subsystem | How |
|-----------|-----------|-----|
| `is_x86_feature_detected!` | SIMD (x86) | CPUID instruction, cached in `OnceLock` |
| `is_aarch64_feature_detected!` | SIMD (aarch64) | Kernel-exposed HWCAP, cached in `OnceLock` |
| `uname().release` parse | io_uring | Parse major.minor from kernel version string |
| `IoUring::new(4)` probe | io_uring | Attempt minimal ring creation to detect seccomp blocks |
| `pipe2` + `splice(0 bytes)` | splice | Zero-length splice to test pipe pair |
| `CreateIoCompletionPort` | IOCP | Create test completion port |
| `ioctl(FICLONE)` | reflink | Attempted at copy time; failure triggers fallback |
| `clonefile()` | reflink (macOS) | Attempted at copy time; failure triggers fallback |
| `GetDiskFreeSpaceW` + `DeviceIoControl` | ReFS reflink | Cluster size query + FSCTL_DUPLICATE_EXTENTS |
| `is_refs_filesystem()` | ReFS detection | Volume filesystem type check, cached |

All detection results are cached for the process lifetime to avoid
repeated syscall overhead.
