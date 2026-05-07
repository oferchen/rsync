# Hardware Priority Order and Fallback Chains

This document records the runtime dispatch order, detection points, and scalar
fallbacks for every hardware-accelerated subsystem in oc-rsync. Each chain
prefers the widest backend the host advertises, intersects that with the active
SIMD override (`--simd=<level>`) where applicable, and degrades gracefully to a
portable scalar path on architectures without specialised support.

## Conventions

- File paths are repository-relative.
- "Detection point" identifies the function that probes the capability.
- "Cache" identifies the storage that memoises the probe result so the per-call
  cost is a single relaxed atomic load.
- Scalar fallback is always present, byte-for-byte equivalent to upstream
  rsync, and exercised by parity tests under
  `crates/checksums/src/simd_parity_tests.rs` and
  `crates/checksums/src/rolling/checksum/tests.rs`.

---

## 1. Rolling checksum (Adler-32 style `rsum`)

Used by the delta-transfer matcher to slide a weak hash across a basis file.

### Dispatch chain

| Order | Backend | Lane width | Architecture |
|-------|---------|------------|--------------|
| 1 | AVX2 | 32 bytes/iter | x86 / x86_64 |
| 2 | SSE2 | 16 bytes/iter | x86 / x86_64 |
| 3 | NEON | 16 bytes/iter | aarch64 |
| last | Scalar | 4-byte unrolled | all architectures |

### Detection and dispatch

- Architecture-neutral entry: `accumulate_chunk_dispatch()` in
  `crates/checksums/src/rolling/checksum/mod.rs`.
- x86/x86_64 probe: `cpu_features()` in
  `crates/checksums/src/rolling/checksum/x86.rs` evaluates
  `is_x86_feature_detected!("avx2")` and `is_x86_feature_detected!("sse2")`,
  caches the pair in `static FEATURES: OnceLock<FeatureLevel>`, and is
  intersected with the CLI override via `effective_features()`.
- aarch64 probe: `neon_available()` in
  `crates/checksums/src/rolling/checksum/neon.rs` evaluates
  `is_aarch64_feature_detected!("neon")` and caches the answer in
  `static NEON_AVAILABLE: OnceLock<bool>`.
- Scalar fallback: `accumulate_chunk_scalar_raw()` in
  `crates/checksums/src/rolling/checksum/mod.rs`. Mirrors upstream
  `checksum.c:get_checksum1()` with sign-extended bytes (`schar` semantics).

### Override surface

The CLI flag `--simd=<auto|avx512|avx2|sse4|neon|none>` is wired through
`set_simd_override()` in `crates/checksums/src/cpu_features.rs`. The override
byte is stored in `static OVERRIDE: AtomicU8` and consulted on every dispatch
through `feature_allowed()`, which intersects the requested cap with the host's
CPUID-detected feature set.

---

## 2. Strong checksum (MD4 / MD5 batch)

Used to verify candidate block matches and finalise whole-file digests.

### Dispatch chain

| Order | Backend | Lanes | Detection |
|-------|---------|-------|-----------|
| 1 | AVX-512 | 16 | `is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw")` |
| 2 | AVX2 | 8 | `is_x86_feature_detected!("avx2")` |
| 3 | SSE4.1 | 4 | `is_x86_feature_detected!("sse4.1")` |
| 4 | SSSE3 | 4 | `is_x86_feature_detected!("ssse3")` |
| 5 | SSE2 | 4 | baseline on x86_64 |
| 6 | NEON | 4 | mandatory on aarch64 |
| 7 | WASM SIMD | 4 | `target_feature = "simd128"` |
| last | Scalar | 1 | portable |

### Detection and dispatch

- Dispatcher: `Dispatcher::detect_backend()` in
  `crates/checksums/src/simd_batch/md5_dispatcher.rs`.
- Each capability probe (`Dispatcher::has_avx512`, `has_avx2`, `has_sse41`,
  `has_ssse3`, `has_sse2`, `has_neon`, `has_wasm_simd`) gates the runtime
  feature detection behind `feature_allowed(SimdFeature::*)` so the CLI
  override clamps the selected level.
- Cache: `static DISPATCHER: OnceLock<Dispatcher>` returned by `global()` in
  the same file. The dispatcher is built once, on first call to
  `digest_batch()`.
- MD4 mirrors the same ladder. Entry point:
  `crates/checksums/src/simd_batch/md4/mod.rs` (`detect_backend`).
- Lane backends live under `crates/checksums/src/simd_batch/md5_simd/`
  (`avx512.rs`, `avx2.rs`, `sse41.rs`, `ssse3.rs`, `sse2.rs`, `neon.rs`,
  `wasm.rs`). MD4 lane backends live under
  `crates/checksums/src/simd_batch/md4/simd/`.
- Scalar fallback: `crates/checksums/src/simd_batch/md5_scalar.rs`
  (`scalar::digest`). MD4 scalar fallback:
  `crates/checksums/src/simd_batch/md4/scalar.rs`. Both paths are exercised
  whenever the dispatcher resolves to `Backend::Scalar`, when an input batch
  is partial, and by `simd_parity_tests` over RFC vectors plus property
  inputs up to 100 KiB.

---

## 3. SHA-256 (daemon authentication, high-security strong digests)

The `Sha256` wrapper in `crates/checksums/src/strong/sha256.rs` delegates to
the `sha2` crate, which performs its own runtime detection through
`cpufeatures`. oc-rsync exposes the result via
`sha256_hardware_acceleration_available()` so the version banner and
diagnostics match the active backend.

### Dispatch chain

| Order | Backend | Detection |
|-------|---------|-----------|
| 1 | Intel SHA-NI / AMD Zen `sha256rnds2` | `is_x86_feature_detected!("sha")` plus the `sse2 / ssse3 / sse4.1` baseline required by `sha2` |
| 2 | ARMv8 `sha2` crypto extension | `is_aarch64_feature_detected!("sha2")` |
| 3 | `sha2-asm` hand-tuned assembly | Unix targets where the assembler succeeds |
| 4 | Pure-Rust scalar | Windows builds (no NASM) and architectures without HW SHA |

### Detection and dispatch

- Public probe: `sha256_hardware_acceleration_available()` in
  `crates/checksums/src/strong/sha256.rs`. The function inspects CPUID at
  call time; the underlying selection inside `sha2` happens once and is
  cached by `cpufeatures` itself.
- Streaming/one-shot parity: `streaming_random_buffer_matches_one_shot` and
  `streaming_chunk_sizes_match_one_shot` in the same file.

---

## 4. AES (embedded SSH cipher selection)

The embedded SSH transport orders AES-GCM ahead of ChaCha20-Poly1305 only when
the host advertises hardware AES.

### Dispatch chain

| Order | Backend | Detection |
|-------|---------|-----------|
| 1 | x86 / x86_64 AES-NI | `is_x86_feature_detected!("aes")` |
| 2 | aarch64 ARMv8 AES extension | `is_aarch64_feature_detected!("aes")` |
| last | Software cipher (ChaCha20-Poly1305 preferred) | other architectures |

### Detection and dispatch

- Probe: `has_aes_ni()` in `crates/rsync_io/src/ssh/embedded/cipher.rs`.
- Architecture branches: `detect_aes_ni()` in the same file (one body per
  arch, the third returns `false`).
- Cache: `static HAS_AES_NI: OnceLock<bool>`.
- Consumer: `default_ciphers()` returns
  `aes128-gcm@openssh.com -> aes256-gcm@openssh.com -> chacha20-poly1305@openssh.com`
  with hardware AES, and reverses the first/last entries otherwise so that
  software ChaCha20-Poly1305 leads.

---

## 5. Disk I/O (read / write)

### Linux

| Order | Mechanism | Detection |
|-------|-----------|-----------|
| 1 | io_uring (kernel >= 5.6) | `is_io_uring_available()` |
| 2 | Standard buffered I/O via `BufReader` / `BufWriter` | factory fallback |

- Probe: `is_io_uring_available()` in
  `crates/fast_io/src/io_uring/config.rs`. Steps:
  1. `parse_kernel_version()` on `uname().release` (defined in
     `crates/fast_io/src/kernel_version.rs`).
  2. Compares against `MIN_KERNEL_VERSION = (5, 6)`.
  3. Calls `IoUring::new(4)` to confirm `io_uring_setup(2)` is not blocked
     by seccomp or container policy.
- Cache: `IO_URING_AVAILABLE: AtomicBool` plus `IO_URING_CHECKED: AtomicBool`.
  Hot path is a single relaxed atomic load.
- Fallback: factories in `crates/fast_io/src/io_uring/file_factory.rs` and
  `socket_factory.rs` return the standard `BufReader` / `BufWriter` variants
  when the probe fails or `cfg(not(feature = "io_uring"))`.
- Stub for non-Linux / feature off:
  `crates/fast_io/src/io_uring_stub.rs` (`is_io_uring_available()` always
  returns `false`).

### macOS

Always synchronous. Reads use buffered `std::fs::File`; writes flow through
the platform-copy chain below. There is no async equivalent of io_uring on
macOS in this codebase.

### Windows

| Order | Mechanism | Detection |
|-------|-----------|-----------|
| 1 | IOCP (overlapped I/O for files >= 64 KiB) | `is_iocp_available()` |
| 2 | Standard buffered I/O | factory fallback |

- Probe: `is_iocp_available()` in `crates/fast_io/src/iocp/config.rs`. Calls
  `probe_iocp()` which creates a standalone completion port via
  `CreateIoCompletionPort(INVALID_HANDLE_VALUE, ...)`.
- Cache: `IOCP_STATUS: AtomicU8` (with `AVAILABLE` / `UNAVAILABLE` /
  unprobed states).
- Optional `FILE_SKIP_SET_EVENT_ON_HANDLE` optimisation is reported via
  `skip_event_optimization_available()`.
- Threshold: files smaller than `IOCP_MIN_FILE_SIZE` (64 KiB) bypass IOCP.
- Stub: `crates/fast_io/src/iocp_stub.rs` (`is_iocp_available()` always
  returns `false` on non-Windows).

---

## 6. Sparse hole detection

Used by the delta executor to recreate file holes instead of writing zero
runs. Strategy is selected by `SparseDetectStrategy` in
`crates/engine/src/local_copy/executor/file/sparse/mod.rs`.

### Dispatch chain

| Order | Mechanism | Detection point |
|-------|-----------|-----------------|
| 1 | `lseek(SEEK_HOLE)` / `lseek(SEEK_DATA)` | Linux only, via `rustix::fs::seek(SeekFrom::Hole/Data)` |
| 2 | FIEMAP-equivalent map mode | currently routes through `SEEK_HOLE` (placeholder for direct ioctl) |
| 3 | Read-and-scan fallback | byte-level scan via `find_first_nonzero` |
| last | Single-data-region (no holes) | other platforms / non-sparse files |

### Detection and dispatch

- Linux primary path: `SparseReader::detect_holes_linux()` in
  `crates/engine/src/local_copy/executor/file/sparse/reader.rs`. Walks the
  file with alternating `SeekFrom::Data` / `SeekFrom::Hole` calls.
- Errors other than `ENXIO` defer to `detect_holes_fallback()` (read +
  scan) in the same file.
- The byte scanner uses `fast_io::zero_detect::find_first_nonzero`. Its own
  dispatch chain (`crates/fast_io/src/zero_detect.rs::select_impl`) is:
  - x86 / x86_64: AVX2 (32 B/iter) -> SSE2 (16 B/iter) -> scalar.
  - aarch64: NEON (16 B/iter) -> scalar.
  - other: scalar `u128` 16-byte loop.
  Probe results are cached in `static DISPATCH: OnceLock<FindFn>`.
- Non-Linux: `detect_holes_map()` and `detect_holes_disabled()` collapse to
  `detect_holes_single_data()` so destinations replicate the source verbatim.

---

## 7. Copy-on-write / reflink

Used by `PlatformCopy::copy_file` for instant clones when the destination
filesystem supports block sharing.

### Dispatch chain by platform

#### Linux

| Order | Mechanism | Detection |
|-------|-----------|-----------|
| 1 | `FICLONE` ioctl | first attempt, succeeds on Btrfs / XFS-with-reflink / bcachefs |
| 2 | `copy_file_range(2)` | files >= 64 KiB |
| 3 | `std::fs::copy` | universal fallback |

Entry: `platform_copy_impl` (Linux arm) in
`crates/fast_io/src/platform_copy/dispatch.rs`. Inner helpers:
`try_ficlone_impl()` (calls `rustix::fs::ioctl_ficlone`) and
`crate::copy_file_range::copy_file_contents()`. Failure of FICLONE removes
the partial destination before falling through.

#### macOS

| Order | Mechanism | Detection |
|-------|-----------|-----------|
| 1 | `clonefile(2)` | APFS reflink (zero data copied) |
| 2 | `fcopyfile(3)` with `COPYFILE_DATA` | kernel-accelerated descriptor copy |
| 3 | `std::fs::copy` | universal fallback |

Entry: `platform_copy_impl` (macOS arm) in the same dispatch file. Inner
helpers: `clonefile_impl()` and `fcopyfile_impl()`.

#### Windows

| Order | Mechanism | Detection |
|-------|-----------|-----------|
| 1 | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | `crate::refs_detect::is_refs_filesystem` reports ReFS |
| 2 | `CopyFileExW` (with `COPY_FILE_NO_BUFFERING` for files > 4 MiB) | always available on Windows Vista+ |
| 3 | `std::fs::copy` | universal fallback |

Entry: `platform_copy_impl` (Windows arm) in the same dispatch file. Inner
helpers: `try_refs_reflink_impl()` (queries cluster size via
`GetDiskFreeSpaceW`, issues `DeviceIoControl(FSCTL_DUPLICATE_EXTENTS_TO_FILE)`)
and `try_copy_file_ex()`.

#### Other platforms

`platform_copy_impl` (default arm) calls `std::fs::copy` directly. Capability
queries `platform_supports_reflink()` and `platform_preferred_method()`
return `false` / `CopyMethod::StandardCopy`.

---

## Summary table

| Subsystem | Primary detection function | Cache | Scalar fallback |
|-----------|----------------------------|-------|-----------------|
| Rolling checksum | `cpu_features()` (x86), `neon_available()` (aarch64) in `crates/checksums/src/rolling/checksum/{x86,neon}.rs` | `OnceLock<FeatureLevel>` / `OnceLock<bool>` | `accumulate_chunk_scalar_raw` (`mod.rs`) |
| MD5 / MD4 batch | `Dispatcher::detect_backend()` in `crates/checksums/src/simd_batch/md5_dispatcher.rs` | `OnceLock<Dispatcher>` (`global()`) | `md5_scalar::digest`, `md4/scalar.rs` |
| SHA-256 | `sha256_hardware_acceleration_available()` in `crates/checksums/src/strong/sha256.rs` | `cpufeatures` internal cache | `sha2` pure-Rust path |
| SSH AES cipher order | `has_aes_ni()` in `crates/rsync_io/src/ssh/embedded/cipher.rs` | `OnceLock<bool>` | ChaCha20-Poly1305 software cipher |
| io_uring | `is_io_uring_available()` in `crates/fast_io/src/io_uring/config.rs` | `AtomicBool` + `AtomicBool` | `BufReader` / `BufWriter` factory |
| IOCP | `is_iocp_available()` in `crates/fast_io/src/iocp/config.rs` | `AtomicU8` | `BufReader` / `BufWriter` factory |
| Sparse detection | `SparseReader::detect_holes_linux()` in `crates/engine/src/local_copy/executor/file/sparse/reader.rs` | none (per-call); zero-scan in `fast_io::zero_detect` is `OnceLock<FindFn>` | `detect_holes_fallback` -> `find_first_nonzero` -> scalar `u128` loop |
| Reflink / CoW | `platform_copy_impl` arms in `crates/fast_io/src/platform_copy/dispatch.rs` | none (probed every copy; ReFS detection cached by `refs_detect`) | `std::fs::copy` |
