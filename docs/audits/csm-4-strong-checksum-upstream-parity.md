# CSM-4 - Strong-checksum upstream-parity audit

Date: 2026-05-23
Scope: read-only research, no `.rs` edits
Tracked under: CSM-4 (parent CSM track: `--checksum` mode is ~1.5-1.7x slower
than upstream rsync 3.4.1; upstream issue #970)

## Goal

Compare oc-rsync's strong-checksum implementations against upstream rsync 3.4.1
(`lib/md4.c`, `lib/md5.c`, `lib/md5-asm-x86_64.S`, `checksum.c` for XXH usage)
and surface every algorithm, block-size, seed, finalize, and SIMD divergence
that could plausibly contribute to the `--checksum` performance gap. CSM-7 will
synthesize the findings; CSM-8 will land the fix(es).

This is a docs-only audit. The questions answered here are:

1. Do oc-rsync's MD4/MD5/XXH3/XXH128 produce byte-identical output to upstream?
2. Where do the two implementations exchange data differently (chunk size,
   buffer reuse, allocation pattern, syscall pattern)?
3. Where does upstream get a real CPU win that oc-rsync misses (OpenSSL EVP +
   AES-NI/SHA-NI in default builds, MD5 hand-rolled assembly, static digest
   contexts, mmap'd input)?
4. How is the per-session checksum seed threaded through both implementations,
   and is the ordering byte-identical for the `--checksum` path?

## 1. Code paths under audit

| Concern | oc-rsync | Upstream rsync 3.4.1 |
| --- | --- | --- |
| MD4 streaming | `crates/checksums/src/strong/md4.rs` (delegates to `md4` crate or `openssl::hash::Hasher` when `openssl` feature is on) | `lib/mdfour.c:mdfour_begin/update/result` |
| MD5 streaming | `crates/checksums/src/strong/md5.rs` (delegates to `md-5` crate or `openssl::hash::Hasher`) | `lib/md5.c:md5_begin/update/result`, optionally `lib/md5-asm-x86_64.S` |
| XXH64 / XXH3-64 / XXH3-128 | `crates/checksums/src/strong/xxhash.rs` (delegates to `xxhash-rust` for streaming, `xxh3` crate for one-shot) | `lib/xxhash.c` (vendor copy of upstream xxHash) called from `checksum.c` |
| Per-block dispatch (`get_checksum2`) | `crates/transfer/src/delta_apply/checksum.rs::ChecksumVerifier` | `checksum.c:304 get_checksum2()` |
| Whole-file dispatch (`-c` mode) | sender side absent today (see CSP audit `docs/audits/checksum-mode-computation-cost.md`); receiver side `crates/transfer/src/receiver/quick_check.rs:262 file_checksum_matches` and `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs::hash_file_contents` | `checksum.c:402 file_checksum()` |
| Streaming accumulator (sender `sum_*` API) | composed ad-hoc per call site via `ChecksumVerifier::for_algorithm` | `checksum.c:559 sum_init / sum_update / sum_end` (static contexts) |
| SIMD batch MD4/MD5 | `crates/checksums/src/simd_batch/{md4,md5_simd}/{avx2,avx512,sse2,sse41,ssse3,neon,wasm}.rs` (4/8/16-lane batch) | none for MD4; MD5 has scalar `lib/md5.c` plus optional single-stream x86_64 asm (`lib/md5-asm-x86_64.S`, guarded by `USE_MD5_ASM`) |
| OpenSSL EVP fast path | `crates/checksums/src/strong/openssl_support.rs` (feature `openssl`, **off by default**, MD4/MD5 only) | `checksum.c` via `USE_OPENSSL`, default-on when `EVP_get_digestbyname()` resolves; covers MD4/MD5/SHA1/SHA256/SHA512 |
| Rolling SIMD | `crates/checksums/src/rolling*` (AVX2/SSE2/NEON) - **not exercised by `-c`** | `simd-checksum-x86_64.cpp` + `simd-checksum-avx2.S` (`USE_ROLL_SIMD`) - **not exercised by `-c`** |

## 2. Algorithm-by-algorithm side-by-side

### 2.1 MD5 (default for `--checksum` on protocol 30+)

| Attribute | oc-rsync | upstream | divergence | perf impact |
| --- | --- | --- | --- | --- |
| Initial state | `md-5` crate uses RFC 1321 IV `A=0x67452301, B=0xEFCDAB89, C=0x98BADCFE, D=0x10325476` | `lib/md5.c:25-28` identical IV | none | n/a (correctness) |
| Round constants | `md-5` crate uses RFC 1321 table | `lib/md5.c:65-140` same table | none | n/a |
| Padding | `md-5` crate appends `0x80`, zeros, then 64-bit LE length | `lib/md5.c:200-219` `0x80`, zeros, then 64-bit LE length | none | n/a |
| Block size fed to compress | 64-byte blocks, internally buffered by `md-5` crate | 64-byte blocks (`CSUM_CHUNK = 64`), explicit `ctx->buffer` | none | n/a |
| **Chunk size fed to digest** (`-c` receiver path) | 64 KiB stack buffer (`let mut buf = [0u8; 64 * 1024]`) in `quick_check.rs:272` | 32 KiB (`CHUNK_SIZE = 32*1024`, `rsync.h:158`) walked over `map_file()`-backed mmap (`MAX_MAP_SIZE = 256 KiB`, `rsync.h:159`) | **chunk size differs** (64 KiB userspace vs 32 KiB mapped) and oc-rsync uses `read()`-into-stack-buffer whereas upstream uses `map_ptr()` over `map_file()` | **HIGH** - extra copy per chunk, extra syscall per chunk |
| **Allocation pattern** | fresh `Md5::new()` per file in both `quick_check.rs:271` (via `ChecksumVerifier::for_algorithm`) and `parallel_checksum.rs:184` | `checksum.c:494 file_checksum` re-uses local `md_context m5` on the stack; `sum_init` uses `static md_context ctx_md` so per-block path has zero per-file allocations | matches structurally (both fresh per file) for `file_checksum`, but oc-rsync's enum-dispatch `ChecksumVerifier` adds a small per-file branch the upstream call site does not | **LOW** |
| **Backend selection** | pure-Rust `md-5` crate by default; OpenSSL only when the `openssl` cargo feature is enabled (not the default) | OpenSSL EVP by default if compiled with `USE_OPENSSL` (Debian/Fedora/macOS Homebrew builds), giving AES-NI / SHA-NI / vendor-tuned MD5 | **default build of oc-rsync is pure-Rust MD5; default upstream is OpenSSL EVP MD5** | **CRITICAL** - this is the single biggest expected delta. Pure-Rust `md-5` is ~500 MB/s on x86_64; OpenSSL MD5 is ~1 GB/s. Receiver hashes both basis-file (for `-c`) and freshly received data; 2x backend speed maps almost 1:1 to throughput |
| **MD5 ASM fast path** | none | `lib/md5-asm-x86_64.S` via `USE_MD5_ASM`; processes multiple 64-byte chunks per call. Note: upstream ships it but only enables it when configured with `--enable-md5-asm`, and even then `checksum.c:152-156 csum_evp_md` *disables* EVP MD5 to force the ASM path | mismatched, oc-rsync has no equivalent | **MEDIUM** when upstream is built with `--enable-md5-asm` (uncommon on distros, common in benchmark suites) |
| Seed (file-checksum mode) | not applied; `quick_check.rs:271 ChecksumVerifier::for_algorithm` calls with no seed; `parallel_checksum.rs:184 SignatureAlgorithm::Md5 { seed_config }` plumbs `Md5Seed::default()` (`none()`) - upstream does the same | `checksum.c:402 file_checksum()` calls plain `md5_begin` + `md5_update`, no seed (only `get_checksum2()` mixes the seed for *per-block* hashing) | none for `-c` mode | n/a |
| Finalize | 16-byte LE output via `md-5` crate | `lib/md5.c:221-224 SIVALu` (LE) writes A,B,C,D as 4 LE u32s = 16 bytes | none | n/a |

### 2.2 MD4 (legacy default for protocol < 30)

| Attribute | oc-rsync | upstream | divergence | perf impact |
| --- | --- | --- | --- | --- |
| Initial state | `md4` crate (RFC 1320) IV `0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476` | `lib/mdfour.c:107-110` same IV | none | n/a |
| Round constants | `md4` crate uses RFC 1320 | `lib/mdfour.c:38-40` constants `0x5A827999, 0x6ED9EBA1` and identity for round 1 | none | n/a |
| Block size fed to compress | 64-byte blocks, internally buffered by `md4` crate | 64-byte blocks (`mdfour64` over `M[16] = 4-byte words`) | none | n/a |
| **Total bit count** (`totalN`/`totalN2`) | `md4` crate maintains 64-bit message length per RFC 1320 | `lib/mdfour.c:113` two `uint32` fields; `lib/mdfour.c:142,154 protocol_version >= 27` gates whether `totalN2` is written into the padding tail | none structurally (oc-rsync only targets protocol 27+; `ChecksumVerifier` does not implement `MD4_BUSTED` / `MD4_ARCHAIC` semantics) | n/a (those variants are negotiation-only quirks for ancient peers) |
| Padding | `md4` crate `0x80`, zeros, 64-bit LE length | `lib/mdfour.c:115-160` two-pass tail handler (length <= 55 in current block vs spillover) | none | n/a |
| **Backend selection** | pure-Rust `md4` crate by default, OpenSSL when feature-gated and OpenSSL build still exposes MD4 (legacy provider on OpenSSL 3.0+) | OpenSSL EVP when `USE_OPENSSL` and `EVP_get_digestbyname("md4")` resolves (OpenSSL 3 puts MD4 behind the legacy provider; upstream falls back to built-in C if EVP returns NULL) | matches structurally; both have a built-in fallback | **LOW** for `-c` mode on protocol 30+ (MD4 only used by very old peers) |
| Seed (block path) | `Md4::digest_with_seed(seed, data)` appends `seed.to_le_bytes()` to the data buffer before finalize | `checksum.c:376-380` `memcpy(buf1, buf, len); if (checksum_seed) { SIVAL(buf1,len,checksum_seed); len += 4; }` then `mdfour_update` | identical wire bytes; oc-rsync skips upstream's `buf1` cache (single hot allocation grown by the longest block ever seen, freed at process end) | **NEGLIGIBLE** - upstream's `buf1` is allocated once per process, oc-rsync's `update(&seed.to_le_bytes())` adds one extra `mdfour_update` of 4 bytes which is still cheaper than `memcpy(buf1, ..., len)` |
| Finalize | 16-byte LE output via `md4` crate | `lib/mdfour.c:97-103 copy4()` LE | none | n/a |

### 2.3 XXH3-64 and XXH3-128 (negotiated for modern peers)

| Attribute | oc-rsync | upstream | divergence | perf impact |
| --- | --- | --- | --- | --- |
| One-shot path | `xxh3` crate (`hash64_with_seed` / `hash128_with_seed`), runtime SIMD detection for AVX2 / NEON / scalar | `XXH3_64bits_withSeed` / `XXH3_128bits_withSeed` from `lib/xxhash.c` (vendor copy), compile-time SIMD (AVX2/SSE2/NEON) selected via xxHash's own dispatcher | both use SIMD; oc-rsync's runtime detect is portable, upstream's is compile-time | none |
| **Streaming path** | `xxhash-rust::xxh3::Xxh3` (separate crate from one-shot `xxh3` crate; `xxhash-rust` uses compile-time SIMD selection only) | `XXH3_state_t` with `XXH3_64bits_update` / `_digest` over 32 KiB chunks | oc-rsync uses two different XXH3 backends - the fast `xxh3` crate for one-shot and `xxhash-rust` for streaming. `xxhash-rust` is slower than the reference `libxxhash` on AVX2 hardware (typically ~10-20%) | **MEDIUM** when `--checksum` files exceed `max_memory_file_size` (default 1 MiB in `FileHashConfig`) and the streaming path engages |
| **Chunk size fed to digest** (`-c` receiver) | 64 KiB read into stack buffer (`quick_check.rs:272`); for engine path, `BufferPool` chunks (default 64 KiB) | 32 KiB mapped chunks via `map_ptr()` | divergent | **LOW** for XXH3 (the digest is so fast - 15 GB/s - that the chunk size barely matters; I/O dominates) |
| **State reuse** | fresh `Xxh3::new(seed)` per file | `checksum.c:457-460` `static XXH3_state_t* state = NULL;` + `XXH3_64bits_reset(state)` per file - one allocation for the entire process | divergent | **LOW** - XXH3 state alloc is ~600 bytes one-shot; per-file alloc cost is in the noise vs hashing throughput |
| Seed | `xxh3::hash64_with_seed(data, seed)` accepts a `u64` seed | `XXH3_64bits_withSeed(buf, len, checksum_seed)` accepts a `XXH64_hash_t = uint64_t` | none (oc-rsync passes session seed through; **see seed-threading note below**) | n/a |
| Finalize | 8 or 16-byte LE | `SIVAL64(sum, 0, ...)` LE | none | n/a |

### 2.4 XXH64

| Attribute | oc-rsync | upstream | divergence | perf impact |
| --- | --- | --- | --- | --- |
| One-shot | `xxhash-rust::xxh64::xxh64` (compile-time SIMD only; XXH64 has fewer SIMD opportunities than XXH3) | `XXH64(buf, len, checksum_seed)` from vendored xxHash | none algorithmically; both backends are competitive at ~6-7 GB/s | none |
| Streaming | `xxhash-rust::xxh64::Xxh64` | `XXH64_state_t` + `XXH64_reset/update/digest` | none | none |
| **State reuse** | fresh per file | `static XXH64_state_t* state` per process | divergent | **LOW** |

### 2.5 SHA1, SHA256, SHA512

`--checksum=sha1`/`sha256`/`sha512` is rare in production (upstream only registers
SHA1 in the negotiation table; SHA256/512 are auth-only). oc-rsync ships
`crates/checksums/src/strong/{sha1,sha256,sha512}.rs` backed by pure-Rust
`sha1`/`sha2` crates; **no hardware SHA-NI fast path**. Upstream goes through
OpenSSL EVP which uses SHA-NI on x86_64 Cooper Lake+ and ARMv8 Crypto Extensions
when available. If a benchmark suite explicitly uses `--checksum-choice=sha1`
the gap will be ~3-4x (300 MB/s scalar vs 1 GB/s SHA-NI), but this is **not
relevant to the default `--checksum` mode** which negotiates MD5/MD4/XXH3.

## 3. Seed-threading comparison

This is historically the most common source of `--checksum` slowdowns when the
seed is mis-threaded: a wrong seed makes every basis-file digest miss, every
file falls through to the delta-transfer pipeline, and `--checksum` effectively
becomes "transfer everything", which dominates the perf measurement.

### 3.1 What gets seeded where in upstream

Upstream rsync uses **one** session-wide `checksum_seed` (`checksum.c:40 extern
int checksum_seed`). It is exchanged early in the protocol and applied as
follows:

| Site | Seed applied? | Order |
| --- | --- | --- |
| `checksum.c:285 get_checksum1` (rolling) | **no** - rolling has no seed | n/a |
| `checksum.c:304 get_checksum2` (per-block strong, MD4 path) | **yes**, appended as `SIVAL(buf1, len, checksum_seed); len += 4;` (after data) | data, then seed |
| `checksum.c:304 get_checksum2` (per-block strong, MD5 path) | **yes**, ordering depends on `proper_seed_order` (compat flag `CHECKSUM_SEED_FIX`): protocol 30+ -> seed then data; pre-30 -> data then seed | varies |
| `checksum.c:304 get_checksum2` (per-block strong, XXH64/XXH3) | **yes**, native seed argument to xxHash one-shot | xxHash takes a 64-bit seed; checksum_seed is a 32-bit int promoted |
| `checksum.c:402 file_checksum` (whole-file `-c` mode, all algorithms) | **NO SEED** - this is the critical point. The `-c` checksum is the *unseeded* hash of the file contents. Upstream calls `md5_begin / md5_update(map_ptr) / md5_result` with no seed mix-in (`checksum.c:494-507`). Same for MD4 (`:512-530`), XXH64 (`:437-452`), XXH3 (`:455-491`). | n/a |

The `-c` mode's whole-file digest is intentionally unseeded so that the same
file on two different sessions (different `checksum_seed`) produces the same
digest. This is by design: `--checksum` compares the file's content identity,
not a session-bound value.

### 3.2 What oc-rsync does

oc-rsync's `-c` mode also computes the whole-file digest unseeded. The two
relevant sites:

- `crates/transfer/src/receiver/quick_check.rs:262 file_checksum_matches`
  calls `ChecksumVerifier::for_algorithm(algorithm)` (note: **not**
  `for_algorithm_seeded`). The verifier is constructed without a seed and the
  file is hashed end-to-end. `ChecksumVerifier::for_algorithm_seeded` exists
  but is only used by the delta-apply path
  (`crates/transfer/src/delta_apply/checksum.rs:74`).
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:155-238
  hash_file_contents` dispatches on `SignatureAlgorithm` (NOT `ChecksumAlgorithm`).
  - For `SignatureAlgorithm::Md5 { seed_config }` it builds `Md5::with_seed(seed_config)`,
    but every caller passes `Md5Seed::default()` (= `Md5Seed::none()`), so the
    seed contributes no bytes to the digest. Verified in
    `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:184`.
  - For `SignatureAlgorithm::Md4Seeded { seed }` the seed bytes ARE appended,
    but this variant is only used by the engine's per-block delta-transfer
    path, not by `-c` mode.
  - For XXH64/XXH3/XXH3_128 the seed is `0` in the `-c` caller, matching
    upstream's `XXH*_reset(state)` (which also seeds with 0).

**Verdict: no functional seed divergence for the `-c` whole-file path.** The
seed comparison is unseeded on both sides, so the digests should match
byte-for-byte and the `-c` quick-check decision will be the same as upstream's.

### 3.3 Subtle gotcha worth flagging for CSM-7

Upstream's `checksum.c:609-611 sum_init` for `CSUM_MD4_OLD / BUSTED / ARCHAIC`
(protocol 27-29 only) prepends the seed via:

```c
SIVAL(s, 0, seed);
sum_update(s, 4);
```

The seed for these archaic MD4 variants is prepended **as the first 4 bytes of
the digest stream**. oc-rsync mirrors this in
`crates/transfer/src/delta_apply/checksum.rs:74-81 for_algorithm_seeded`. This
is only the per-block path, not `-c`. **No divergence**, but it is worth
calling out because (a) BR-3i.b had a similar bug class and (b) if `-c` were
ever wired to `for_algorithm_seeded` by accident, all protocol 27-29 transfers
would silently produce wrong digests.

## 4. Other plausible perf contributors (not algorithm parity, but adjacent)

These are not "divergences in the strong-checksum core" - they are deltas in
how the per-file hashing job is set up, which the `--checksum` benchmark
attributes to the algorithm path. CSM-7 should pick from this list along with
the algorithm divergences above.

| # | Item | Where | Why it matters | Estimated impact |
| --- | --- | --- | --- | --- |
| O1 | Read syscall pattern | `quick_check.rs:272` uses 64 KiB stack buffer + `read_exact`; upstream uses `map_file()` + `map_ptr(buf, i, CHUNK_SIZE=32 KiB)` (no syscall after initial `mmap`) | extra `read()` per chunk vs zero syscalls after `mmap`; userspace bounce buffer vs direct slice into mapped pages | **HIGH** for warm-cache benchmarks; less for cold-cache (kernel paginates either way) |
| O2 | Buffer reuse | `quick_check.rs` allocates the 64 KiB buffer on the stack per call (re-zeroed); engine path uses `BufferPool` (good) | per-file stack frame setup is cheap; not the dominant cost | LOW |
| O3 | Digest backend per-call dispatch | `ChecksumVerifier` is an enum-dispatch; the inner `Md5` carries a `Md5Backend` enum with `OpenSsl` (feature-gated) and `Rust` variants; OpenSSL is OFF in the default build | one extra match per `update` call; per-block path (~700 byte blocks) pays this often | LOW |
| O4 | `simd_batch` not wired | `crates/checksums/src/simd_batch/` provides 4/8/16-lane batched MD4/MD5 (AVX2/AVX-512/NEON) but neither the `-c` receiver nor the engine prefetcher calls it; only used by isolated test code (`md5_backend` re-export hidden from rustdoc) | when hashing many small files (the case `--checksum` cares about), batching different files' MD5 onto SIMD lanes ~4-16x the per-thread throughput | **HIGH** for trees of small/medium files; **null** for one large file |
| O5 | mmap not used by receiver path | `quick_check.rs:262` and `parallel_checksum.rs:138` both go through `File::open` + `read`; the standalone `parallel/files.rs:hash_file_internal` does use `MmapReader` above `MMAP_THRESHOLD` (default 16 MiB in `fast_io`) but is not the `-c` callee | matches upstream's `map_file()` only for the standalone code path, not the `-c` quick-check | **MEDIUM** for large basis files |
| O6 | Pipelined I/O not engaged | `crates/checksums/src/pipelined/` provides a `DoubleBufferedReader` that overlaps `read()` with hashing; not wired into `-c` | hashing is CPU-bound on pure-Rust MD5 (~500 MB/s) vs I/O-bound on OpenSSL MD5 (~1 GB/s on a fast SSD); pipeline win is 20-40% on cold cache | MEDIUM |
| O7 | Pure-Rust `md-5`/`md4` defaults | crate-level default features do not enable `openssl` | the upstream comparison binary on Linux/macOS is almost always OpenSSL-backed | **CRITICAL** - this single switch is most of the gap |
| O8 | `xxhash-rust` vs reference `libxxhash` for streaming | one-shot uses fast `xxh3` crate; streaming falls back to `xxhash-rust` which is ~10-20% slower than reference on AVX2 | only matters for files above `max_memory_file_size` | MEDIUM (for large files when XXH3 is negotiated) |

## 5. Ranked divergence list for CSM-7

Sorted by expected `--checksum` perf impact, highest first. CSM-7 should pick
from the top of this list when scoping CSM-8.

| Rank | ID | Divergence | Expected gain on `-c` |
| --- | --- | --- | --- |
| 1 | O7 / 2.1 backend | Default-build oc-rsync uses pure-Rust `md-5` and `md4` crates. Default-build upstream uses OpenSSL EVP MD5 (and MD4 via the legacy provider). | ~1.8-2x on MD5-heavy `-c` workloads |
| 2 | O4 | `simd_batch` (4-16 lane SIMD MD5/MD4) is implemented but not wired into the `-c` receiver prefetch path | ~4-16x per-core for many-small-files trees, capped by I/O |
| 3 | O1 + O5 | Receiver path reads via `read_exact` into a 64 KiB stack buffer instead of `mmap`-then-walk like upstream (`map_file` + `map_ptr` over 32 KiB chunks within a 256 KiB mapping window) | ~10-20% on warm cache, ~5% on cold |
| 4 | O6 | `DoubleBufferedReader` overlap not engaged for `-c` | ~20-40% on hashes where compute >= I/O (pure-Rust MD5 case) |
| 5 | 2.3 streaming XXH3 backend | streaming XXH3 uses `xxhash-rust` (slower); one-shot uses faster `xxh3` crate. Only matters for files > `max_memory_file_size` | ~10-20% on large-file XXH3 transfers |
| 6 | O3 / 2.1 dispatch | Per-call enum-dispatch and per-file fresh `Md5::new()` allocation; upstream uses static `md_context`/`XXH*_state_t` | <5%; in the noise vs the items above |

Items 1, 2, and 3 are wire-compatible and require no protocol change. Item 1
is a build-config / feature-gate decision (enable `openssl` by default, or
adopt a hardware-accelerated pure-Rust backend such as `md-5` with the
`asm` feature or the `sha2-asm`-style approach for MD5). Item 2 is a wiring
job: change `quick_check.rs::file_checksum_matches` to gather N candidate
files first and dispatch through `simd_batch::digest_batch` when the algorithm
is MD4 or MD5.

## 6. Key non-divergences (confirmed parity)

- MD4 and MD5 produce **byte-identical output** to upstream when given the
  same input. RFC test vectors pass on both sides
  (`crates/checksums/src/strong/md4.rs::md4_streaming_matches_rfc_vectors`,
  `crates/checksums/src/strong/md5.rs::md5_streaming_matches_rfc_vectors`).
- XXH64 / XXH3-64 / XXH3-128 produce byte-identical output to reference
  xxHash for all tested seeds and lengths
  (`crates/checksums/src/strong/xxhash.rs::xxh3_*_oneshot_matches_reference`).
- Seed threading for the `-c` whole-file path is correct: no seed is mixed,
  matching upstream `checksum.c:402-507`.
- Seed threading for the per-block path (`get_checksum2`) is also correct:
  MD4 appends seed after data; MD5 with `proper_seed_order=true` prepends
  seed before data; XXH* takes the seed natively. Verified in
  `crates/transfer/src/delta_apply/checksum.rs` and pinned by tests in
  `crates/checksums/src/strong/md{4,5}.rs`.
- Finalize byte order is little-endian for all algorithms on both sides
  (rsync's `SIVALu` macro is little-endian; the Rust crates emit LE bytes
  for raw digest output).
- The `-c` path is **not** affected by any of the protocol-27-29 MD4 quirks
  (`CSUM_MD4_OLD/BUSTED/ARCHAIC`) because those only fire on the per-block
  `get_checksum2` path when the negotiated protocol is < 30, and `-c` uses
  `file_checksum` which is the same plain MD4 regardless of variant.

## 7. Next steps

- **CSM-7** (synthesis): pick top 2-3 items from the ranked list above and
  produce an implementation plan. Item 1 (OpenSSL EVP default) is a build-time
  / Cargo-feature decision and should be evaluated against the unsafe-code
  policy and license posture; the `openssl` crate is permitted from
  `checksums` (the crate exposes the feature today). Item 2 (`simd_batch`
  wiring) is a pure-Rust receiver-side change with no new dependencies.
- **CSM-8** (implementation): land the chosen fix. Both items 1 and 2 are
  wire-compatible and need no compat-flag negotiation.
- Adjacent: `docs/audits/checksum-mode-computation-cost.md` (CSP audit) already
  proposes wiring `parallel/files.rs`-style mmap + `BufferPool` reuse into the
  receiver path; CSM-7 should reconcile that audit's proposals with items 3 and
  5 above.

## References

- `crates/checksums/src/strong/md4.rs`
- `crates/checksums/src/strong/md5.rs`
- `crates/checksums/src/strong/xxhash.rs`
- `crates/checksums/src/strong/openssl_support.rs`
- `crates/checksums/src/simd_batch/mod.rs`
- `crates/checksums/src/parallel/files.rs`
- `crates/checksums/src/pipelined/mod.rs`
- `crates/transfer/src/delta_apply/checksum.rs`
- `crates/transfer/src/receiver/quick_check.rs`
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`
- `target/interop/upstream-src/rsync-3.4.1/checksum.c`
- `target/interop/upstream-src/rsync-3.4.1/lib/md5.c`
- `target/interop/upstream-src/rsync-3.4.1/lib/md5-asm-x86_64.S`
- `target/interop/upstream-src/rsync-3.4.1/lib/mdfour.c`
- `target/interop/upstream-src/rsync-3.4.1/rsync.h` (`CHUNK_SIZE`, `MAX_MAP_SIZE`, `CSUM_CHUNK`)
- `docs/audits/checksum-mode-computation-cost.md` (CSP audit; complementary)
