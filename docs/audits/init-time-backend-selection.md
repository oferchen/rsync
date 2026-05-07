# Init-Time Backend Selection Audit

Tracking issue: #2116. No code changes.

## 1. Goal

Identify hot-path branches in I/O, checksum, compression, and platform code
that re-evaluate the same backend selection on every call, then propose
init-time selection patterns that cache the choice once and dispatch through
a stable handle for the lifetime of the operation.

The branches surveyed fall into three classes:

- **A. Cached probe re-read** - the probe is `OnceLock`-cached, so each call
  is one atomic load plus one or more conditionals. Cheap individually, but
  re-evaluated millions of times in tight loops.
- **B. Variant match per call** - a `OnceLock` cached *enum* tag is matched
  on every entry, with each arm gated by `#[cfg]`. Branch is correctly
  predicted but defeats inlining of the chosen backend.
- **C. Try-fallback per call** - the dispatch attempts the optimised
  backend, catches a syscall error, then retries through a slower path.
  Expensive on filesystems where the optimised backend is permanently
  unavailable (FICLONE on ext4, clonefile on HFS+, ReFS reflink on NTFS).

Class C is the highest-impact category because every retry is a kernel
round-trip, not an atomic load.

## 2. Hot-path summary

| # | Subsystem | Hot path | Class | Per-call cost | Init-time fix |
|---|-----------|----------|-------|---------------|---------------|
| 1 | rolling checksum | `accumulate_chunk_dispatch` -> `try_accumulate_chunk` -> `cpu_features()` | A | atomic load + 2 bool tests + branch | `RollingChecksum` stores `accumulate_fn: fn(...)` resolved once at construction |
| 2 | strong checksum | `ChecksumVerifier::update` enum match | B | enum match + indirect call | trait object `Box<dyn StrongDigest>` stored in verifier |
| 3 | zero-run detection | `find_first_nonzero` -> `dispatch()` -> `OnceLock::get_or_init` | A | atomic load + indirect call | already cached; expose handle and store on `SparseWriter` |
| 4 | io_uring file reader | `IoUringOrStdReader::read` enum match | B | atomic load (via factory) and enum match per `read` | trait-object reader stored on transfer state |
| 5 | io_uring file writer | `IoUringOrStdWriter::write` enum match | B | enum match per `write`/`flush`/`seek` | same |
| 6 | iocp file writer | `IocpOrStdWriter::write` enum match | B | enum match per write | same |
| 7 | platform copy chain | `platform_copy_impl` Linux/macOS/Windows try-then-fallback | C | extra `open` + ioctl + `unlink` on every retry | per-mount probe cache keyed by `dev_t`/volume id |
| 8 | windows refs detection | `is_refs_filesystem(dst.parent())` per `platform_copy_impl` call | A+ | `GetVolumeInformationW` syscall every call | mount-id keyed cache; today this is *not* `OnceLock` cached, it is re-queried |
| 9 | sha256 capability query | `sha256_hardware_acceleration_available()` | A- | 4 `is_x86_feature_detected!` calls (cached internally by `cpufeatures`) but no top-level cache | wrap in `OnceLock<bool>` so the four-feature AND-chain runs once |
| 10 | md5 batch dispatch | `Dispatcher::digest_batch` `match self.backend` | B | enum match per batch call | already init-time cached `Backend` tag; inline the batch loop with a function pointer |
| 11 | md4 batch dispatch | `Md4Dispatcher::digest_batch` `match self.backend` | B | as above | same |
| 12 | rolling SIMD parity test path | `simd_available()` -> `cpu_features()` | A | one atomic load per query | rare, not hot |
| 13 | splice availability | `is_splice_available()` -> `OnceLock` | A | one atomic load | inline into pipeline state |

## 3. Specific call sites

### 3.1 rolling checksum

- `crates/checksums/src/rolling/checksum/mod.rs:165` -- `update` body
  invokes `accumulate_chunk_dispatch`.
- `crates/checksums/src/rolling/checksum/mod.rs:520` -- the dispatcher
  calls `accumulate_chunk_arch`.
- `crates/checksums/src/rolling/checksum/mod.rs:542` -- arch dispatcher
  calls `x86::try_accumulate_chunk`.
- `crates/checksums/src/rolling/checksum/x86.rs:101` --
  `try_accumulate_chunk` calls `cpu_features()` for every chunk.
- `crates/checksums/src/rolling/checksum/x86.rs:81-86` -- `cpu_features`
  is `OnceLock`-cached (fast) but still re-queried per call.
- `crates/checksums/src/rolling/checksum/neon.rs:67` -- `simd_available`
  queries `NEON_AVAILABLE` per call.
- Sender hot-loop callers:
  - `crates/match/src/generator.rs:142` -- `rolling.roll(...)` per byte.
  - `crates/match/src/generator.rs:156` -- `rolling.update_byte(byte)`.
  - `crates/engine/src/local_copy/context_impl/delta_transfer.rs:48` --
    `let mut rolling = RollingChecksum::new();` (window-fed update path).

### 3.2 strong checksum / `ChecksumVerifier`

- `crates/transfer/src/delta_apply/checksum.rs:115-125` -- `update`
  matches the variant on every byte chunk.
- `crates/transfer/src/delta_apply/checksum.rs:86-96` -- `for_algorithm`
  constructs the variant once per file; the branch survives into every
  subsequent `update` and `finalize` call.
- Receiver hot callers:
  - `crates/transfer/src/receiver/transfer.rs:306,316,326,368` -- four
    `checksum_verifier.update(...)` sites inside the per-token loop.
  - `crates/transfer/src/transfer_ops/response.rs:183,193,203,244` --
    matching sender-side update sites.
  - `crates/transfer/src/receiver/quick_check.rs:242` -- streaming hash
    over the existing destination file.

### 3.3 zero-run detection

- `crates/fast_io/src/zero_detect.rs:83-88` -- `DISPATCH: OnceLock<FindFn>`
  with `dispatch()` re-reading the cell on every call.
- `crates/fast_io/src/zero_detect.rs:411-423` -- second dispatch in
  `find_zero_run_end`.
- `crates/fast_io/src/zero_detect.rs:77` -- `is_all_zeros` invokes the
  dispatcher per call.

### 3.4 io_uring / iocp readers and writers

- `crates/fast_io/src/io_uring/file_factory.rs:73-79` -- `Read` impl for
  `IoUringOrStdReader` matches the variant per call.
- `crates/fast_io/src/io_uring/file_factory.rs:189-201` -- `Write` impl
  for `IoUringOrStdWriter` matches per call (`write`, `flush`).
- `crates/fast_io/src/io_uring/file_factory.rs:204-209` -- `Seek` impl
  matches per call.
- `crates/fast_io/src/io_uring/file_factory.rs:115-128` -- factory
  consults `is_io_uring_available()` on every `open`, but caller side
  this is a per-file decision, not per-byte (acceptable).
- `crates/fast_io/src/io_uring/file_factory.rs:46-48,161-163` --
  `will_use_io_uring()` re-queried by every callee that branches on it.
- `crates/fast_io/src/io_uring/socket_factory.rs:71-101` -- `match
  policy` and `is_io_uring_available` repeated for reader and writer
  factories. Per-connection cost; acceptable today but couples policy
  evaluation with construction.
- `crates/fast_io/src/iocp/file_factory.rs:290` -- equivalent dispatch
  for `IocpOrStdWriter`.

### 3.5 platform copy chain (try-then-fallback)

- `crates/fast_io/src/platform_copy/dispatch.rs:18-54` -- Linux:
  `try_ficlone_impl`, then `copy_file_range::copy_file_contents`, then
  `std::fs::copy`. Each fallback opens a fresh fd pair.
- `crates/fast_io/src/platform_copy/dispatch.rs:62-95` -- macOS:
  `clonefile_impl`, then `fcopyfile_impl`, then `std::fs::copy`. Each
  retry calls `remove_file(dst)` and re-opens the destination.
- `crates/fast_io/src/platform_copy/dispatch.rs:104-132` -- Windows:
  `is_refs_filesystem(dst.parent())` *per call*, then
  `try_refs_reflink_impl`, then `try_copy_file_ex`, then
  `std::fs::copy`. The ReFS detection issues `GetVolumeInformationW` for
  every file copied.

### 3.6 sha256 capability query

- `crates/checksums/src/strong/sha256.rs:142-158` --
  `sha256_hardware_acceleration_available()` runs four
  `is_x86_feature_detected!` calls every invocation. The `cpufeatures`
  crate caches each individually, but the AND-chain is not memoised at
  the wrapper layer.

### 3.7 batched md4/md5

- `crates/checksums/src/simd_batch/md5_dispatcher.rs:214-286` -- match
  on `self.backend` for every `digest_batch` call. Backend is fixed at
  `Dispatcher::detect`; the match remains.
- `crates/checksums/src/simd_batch/md4/mod.rs:74-90` -- equivalent for
  MD4.

### 3.8 splice / kernel probes

- `crates/fast_io/src/splice.rs:60-92` -- `is_splice_available()`
  re-checks the cached `OnceLock` on every transfer call.
- `crates/fast_io/src/io_uring/linkat.rs:60-67`,
  `crates/fast_io/src/io_uring/renameat2.rs:58-65` -- analogous cached
  probes invoked per metadata operation.

## 4. Pattern proposal

### 4.1 Function-pointer specialisation (class A)

Resolve the chosen kernel implementation once and store a function pointer
beside the state. Replaces the `OnceLock::get_or_init` atomic load + bool
tests on the hot path with a direct indirect call (single instruction).

```text
struct RollingChecksum {
    s1: u32,
    s2: u32,
    len: usize,
    accumulate: fn(u32, u32, usize, &[u8]) -> (u32, u32, usize),
}

impl RollingChecksum {
    pub fn new() -> Self {
        Self {
            s1: 0,
            s2: 0,
            len: 0,
            accumulate: select_accumulate(),
        }
    }
}

fn select_accumulate() -> fn(u32, u32, usize, &[u8]) -> (u32, u32, usize) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            return accumulate_avx2;
        }
        if std::arch::is_x86_feature_detected!("sse2") {
            return accumulate_sse2;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return accumulate_neon;
        }
    }
    accumulate_scalar
}
```

The same shape applies to `find_first_nonzero` (zero detection), splice
gating in the disk-pipeline, and `sha256_hardware_acceleration_available`.

### 4.2 Trait-object specialisation (class B)

Replace the variant-dispatching enum with a single trait object created at
init time. The match disappears entirely; the indirect call replaces it.

```text
trait StrongDigestState: Send {
    fn update(&mut self, data: &[u8]);
    fn finalize(self: Box<Self>) -> Vec<u8>;
    fn algorithm(&self) -> ChecksumAlgorithm;
    fn digest_len(&self) -> usize;
}

struct ChecksumVerifier {
    inner: Box<dyn StrongDigestState>,
}

impl ChecksumVerifier {
    pub fn for_algorithm(algorithm: ChecksumAlgorithm) -> Self {
        let inner: Box<dyn StrongDigestState> = match algorithm {
            ChecksumAlgorithm::None => Box::new(NoopState),
            ChecksumAlgorithm::MD4 => Box::new(Md4State::new()),
            ChecksumAlgorithm::MD5 => Box::new(Md5State::new()),
            ChecksumAlgorithm::SHA1 => Box::new(Sha1State::new()),
            ChecksumAlgorithm::XXH64 => Box::new(Xxh64State::new()),
            ChecksumAlgorithm::XXH3 => Box::new(Xxh3State::new()),
            ChecksumAlgorithm::XXH128 => Box::new(Xxh128State::new()),
        };
        Self { inner }
    }
}
```

The match pays its construction cost once per file. The
`update`/`finalize` hot path becomes a direct vtable call.

For `IoUringOrStdReader`, the equivalent is to surface a
`Box<dyn FileReader>` from the factory rather than the
two-arm enum, so per-buffer `read` is a vtable call instead of a `match`
plus inner `read`.

### 4.3 Mount-keyed cache (class C)

Probe the optimised backend once per mount and remember the outcome.

```text
struct MountId(u64); // dev_t on Unix, volume serial on Windows

static REFLINK_CACHE: Mutex<HashMap<MountId, ReflinkCapability>> =
    Mutex::new(HashMap::new());

enum ReflinkCapability {
    Supported,
    Unsupported,
    Probing,
}

fn copy_with_reflink(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    let dst_mount = mount_id(dst.parent().unwrap_or(dst))?;
    match reflink_capability(dst_mount) {
        ReflinkCapability::Supported => {
            // Try the fast path; on failure mark mount as Unsupported.
            try_reflink_or_demote(src, dst, dst_mount)
        }
        ReflinkCapability::Unsupported => fallback_copy(src, dst, size_hint),
        ReflinkCapability::Probing => probe_then_dispatch(src, dst, dst_mount, size_hint),
    }
}
```

This removes the open/ioctl/unlink retry chain on systems where the
optimised path is permanently unavailable (ext4, HFS+, NTFS). The first
file pays the probe; every subsequent file on the same mount goes
straight to `copy_file_range` (Linux), `fcopyfile` (macOS), or
`CopyFileExW` (Windows). The same cache subsumes
`is_refs_filesystem` so it stops calling `GetVolumeInformationW` per file.

## 5. Per-platform / per-feature switch matrix

| Hot path | Linux | macOS | Windows | Other |
|----------|-------|-------|---------|-------|
| rolling checksum | AVX2 / SSE2 / scalar | NEON / scalar | AVX2 / SSE2 / scalar | scalar |
| strong checksum (MD4/MD5/SHA1) | SIMD batch (AVX-512/AVX2/SSE4.1/SSSE3/SSE2) | NEON | SIMD batch (matches Linux x86) | scalar |
| sha256 | SHA-NI on x86_64; sha2 NEON on aarch64 | sha2 NEON on M1/M2/M3 | SHA-NI when available | scalar |
| crc32c | SSE4.2 / aarch64 CRC / scalar | aarch64 CRC | SSE4.2 / scalar | scalar |
| zero-run | AVX2 / SSE2 / scalar | NEON / scalar | AVX2 / SSE2 / scalar | scalar |
| platform copy | FICLONE -> `copy_file_range` -> `std::fs::copy` | `clonefile` -> `fcopyfile` -> `std::fs::copy` | ReFS reflink -> `CopyFileExW` -> `std::fs::copy` | `std::fs::copy` |
| splice/sendfile | yes (kernel-version probed) | no | no | no |
| io_uring | yes (kernel >= 5.6) | no | no | no |
| iocp | no | no | yes | no |
| compression zstd | feature `zstd` | feature `zstd` | feature `zstd` | feature `zstd` |
| compression lz4 | feature `lz4` | feature `lz4` | feature `lz4` | feature `lz4` |

The matrix shows that the runtime branches collapse to two or three
plausible backends per platform after `#[cfg]` gating. The class A and
class B hot paths therefore have at most three live arms; replacing them
with a function-pointer or trait object pre-bound at init is sound.

## 6. Estimated branch-elimination opportunity

Counts below assume one rolling checksum window-roll per byte transferred
and one strong-checksum `update` per delta token. They are upper bounds
that drop with kernel/SIMD-friendly batches but illustrate scale.

| Hot path | Calls per 1 GB transfer | Per-call cost today | Cost after init-time selection | Notes |
|----------|------------------------:|---------------------|--------------------------------|-------|
| rolling checksum (sender) | ~10^9 | atomic load + 2 bool tests + indirect call | indirect call only | ~3 cycles/iter on uncontended core; eliminates two cmov per byte |
| `ChecksumVerifier::update` (receiver) | ~10^4 (per token) | enum match (jump table) + variant deref | vtable call | per-token cost, not per byte; speedup small absolute, large relative |
| zero-run detection | one per 16 KB sparse-write block, ~64k/GB | atomic load + indirect call | indirect call only | reduction ~50% of the dispatch cost |
| io_uring `Read::read` | one per 128 KB buffer, ~8k/GB | atomic load (`is_io_uring_available`) is paid in factory; per-buffer is enum match | trait dispatch via vtable | enum vs vtable is a wash; benefit is removing the second branch in `Seek` and `FileReader::size` paths |
| platform copy try-fallback | one per file | up to 3 syscalls + 1 unlink on permanent-fallback mounts | one syscall (cached) | dominant cost is the syscall, not the branch; this is the biggest absolute win |
| ReFS detection | one per Windows file copy | `GetVolumeInformationW` syscall | mount-cache hash lookup | removes a syscall per file |
| sha256 capability query | varies; often once per session but called from public API | 4 `is_x86_feature_detected!` calls | one atomic load | tiny but cleans up the API |

The largest win is #7/#8 (mount-keyed cache for platform copy and ReFS
detection). The second is #1 (rolling checksum), because the volume of
calls is staggering. The remaining branches are correctness-preserving
cleanups whose individual savings are modest but whose collective effect
is to make the dispatch shape uniform across the codebase: every
backend selection lives at construction, and every hot path is a direct
or vtable call.

## 7. Out of scope

- Compression backends. `CompressionStrategySelector::for_algorithm`
  returns a `Box<dyn CompressionStrategy>` that the caller stores and
  reuses. The hot path is already a single vtable call with no
  per-call backend re-evaluation. The only branch is the
  `match kind` *inside* `for_algorithm_kind`, which runs once per
  session.
- Deferred fsync, buffer-pool selection, sparse-writer commit. These
  are governed by configuration, not runtime probes; the configuration
  is captured at session construction.
- Metadata feature negotiation (xattr, ACL). `#[cfg]` gating handles
  the platform split at compile time; runtime branches are limited to
  graceful-degrade paths whose hot-loop frequency is low.
