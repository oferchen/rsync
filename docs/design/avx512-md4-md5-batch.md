# AVX-512 batch MD4 / MD5 design (#1763)

Tracking issue: oc-rsync task #1763.

This document is design-only. No code lands in this PR. The file paths,
function signatures, and dispatch sites named here describe how an
AVX-512F multi-buffer kernel is intended to slot into the existing
`simd_batch` ladder so the CLI keeps a single calling convention across
vector widths.

## 1. Today's MD4 / MD5 dispatch

Strong-checksum batch hashing is exposed by `crates/checksums/src/strong/`
and implemented in `crates/checksums/src/simd_batch/`.

- `crates/checksums/src/strong/mod.rs` re-exports the per-algorithm
  batch helpers as `md4_digest_batch` and `md5_digest_batch`.
- `crates/checksums/src/strong/md4.rs` and
  `crates/checksums/src/strong/md5.rs` provide the streaming hashers
  plus thin `digest_batch` shims that delegate to the batch module.
- `crates/checksums/src/simd_batch/md4/mod.rs` owns `Md4Dispatcher`,
  which probes the host once via `OnceLock` and selects from the
  ladder Avx512 -> Avx2 -> Sse2 -> Neon -> Wasm -> Scalar. MD4's
  simpler round functions do not benefit from `pshufb` or `blendv`,
  so the SSSE3 and SSE4.1 rungs collapse into the SSE2 lane on x86.
- `crates/checksums/src/simd_batch/md5_dispatcher.rs` owns the MD5
  `Dispatcher`, with the wider ladder Avx512 -> Avx2 -> Sse41 ->
  Ssse3 -> Sse2 -> Neon -> Wasm -> Scalar. Each rung is gated by both
  `is_x86_feature_detected!` (or the architecture-specific equivalent)
  and the `--simd` CLI override surfaced through
  `crate::cpu_features::feature_allowed`.
- The actual SIMD kernels sit under
  `crates/checksums/src/simd_batch/md4/simd/` and
  `crates/checksums/src/simd_batch/md5_simd/`. Each rung exposes a
  `digest_xN` entry point: SSE2 / NEON / WASM at four lanes, AVX2 at
  eight lanes, AVX-512 at sixteen lanes. The MD5 AVX-512 file
  (`md5_simd/avx512.rs`) is the existing reference for the multi-buffer
  pattern this design generalises.

Parity is enforced by `crates/checksums/src/simd_parity_tests.rs`. The
`md5_simd_parity` module covers RFC 1321 vectors, lane-boundary sweeps,
partial batches, large inputs up to 100 KiB, and proptest-driven random
byte vectors against the scalar reference. The `md4_simd_parity` module
mirrors that coverage against RFC 1320 vectors. Both modules call
through `simd_batch::digest_batch` so the active backend on the test
host is the one that gets compared to scalar.

## 2. AVX-512F multi-buffer pattern

MD4 and MD5 are 32-bit ARX (add / rotate / xor) constructions over a
sixteen-word message schedule. AVX-512F's `__m512i` register holds
sixteen `u32` lanes, giving a natural one-hash-per-lane mapping with no
cross-lane traffic during the compression rounds. The kernel keeps each
state word (A, B, C, D for MD4 / MD5) as a single `__m512i` whose lanes
are the per-message values. The 16x16 block schedule is a `[__m512i;
16]` loaded by transposing the inputs so lane `i` of `M[j]` holds the
`j`-th 32-bit word of message `i`.

Three AVX-512 instructions carry the round work:

- `vpaddd` performs lane-wise 32-bit addition for the `A + F + K[i] +
  M[g]` step in both MD4 and MD5.
- `vprold` (or `vprolvd` for variable shifts) executes the per-round
  rotate in a single instruction, replacing the
  shift-or-shift-or pattern that AVX2 has to emit. MD4 uses three
  rotate amounts per round; MD5 uses sixteen rotate constants stored
  per-round.
- `vpternlogd` evaluates any three-input bitwise function in one
  micro-op via an 8-bit lookup. MD4's `F = (X & Y) | (!X & Z)`,
  MD5's identical `F`, and MD5's `H = X ^ Y ^ Z` and `I = Y ^ (X |
  !Z)` all map to single `vpternlogd` calls with truth-table immediates
  `0xCA`, `0x96`, and `0x39` respectively. MD4's `G = (X & Y) | (X & Z)
  | (Y & Z)` (majority) is also one `vpternlogd` (immediate `0xE8`).

The kernel processes one 64-byte block per outer-loop iteration. In
pseudocode:

```text
for each message i in 0..16:
    pad inputs[i] to (len + 1 + zeros + 8) bytes following RFC 1320 / 1321
    transpose into block[0..16] as 16x __m512i lanes
state = (init_a, init_b, init_c, init_d)              // four __m512i
for block in blocks:
    a, b, c, d = state
    for round in 0..rounds:
        f = vpternlogd(b, c, d, round.lut)            // F / G / H / I
        a = vpaddd(a, f)
        a = vpaddd(a, block[round.g])                 // M[g]
        a = vpaddd(a, K[round.i])                     // splat K[i]
        a = vprold(a, round.s)
        a = vpaddd(a, b)
        rotate (a, b, c, d)
    state = vpaddd(state, (a, b, c, d))
return transpose(state) -> [Digest; 16]
```

The MD5 path uses sixty-four rounds split across four function
constants; MD4 uses forty-eight rounds split across three. The K
constants are loaded as broadcasted `__m512i` immediates from a
read-only table; on Skylake-X and later they hit the L1 cache after the
first call and stay resident for the lifetime of the dispatcher.

`vmovdqa64` writes / reads use 64-byte alignment via the
`#[repr(C, align(64))]` `Aligned512` helper that already exists in
`md5_simd/avx512.rs`. The MD4 kernel reuses the same aligned scratch
type.

The Rust 1.88 toolchain pinned in `rust-toolchain.toml` does not yet
expose stable AVX-512 intrinsics, so the kernels keep the inline-asm
approach used by `md5_simd/avx512.rs` (`use std::arch::asm`). The MD4
file lands at `crates/checksums/src/simd_batch/md4/simd/avx512.rs` and
exports `unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16]`,
mirroring the existing AVX2 / SSE2 / NEON entry points.

## 3. Block boundary, tail handling, length encoding

MD4 and MD5 share padding: the message is suffixed with `0x80`, then
zero bytes until the length modulo 64 equals 56, then an 8-byte
little-endian bit length. Each lane in the AVX-512 batch can have a
distinct input length, so padding is a per-lane operation done up front
in a scalar staging step.

The kernel allocates a single contiguous `Vec<u32>` sized to
`16 * max_padded_words`, where `max_padded_words` is the longest padded
input rounded up to sixteen `u32`s. Shorter inputs are zero-extended in
the staging buffer; the staging cost is dominated by `memcpy` and
amortised across the sixteen lanes. Inputs larger than the
`MAX_INPUT_SIZE = 1 MiB` cap defined in the existing
`md5_simd/avx512.rs` fall back to scalar to keep staging memory
bounded; the new MD4 kernel inherits the same threshold.

The ARX rounds operate on whole 64-byte blocks, so once padding is
applied there is no "tail" for the SIMD core to handle. Lanes whose
padded length is shorter than the longest lane simply consume their
zero-extended trailer; the final state words for those lanes are
captured at the round in which they would have terminated by tracking
per-lane block counts in an opmask register `k1`. This is the same
pattern that the existing MD5 AVX-512 kernel uses, and it generalises
without change to MD4.

The length field is the final 8 bytes of the padded message and stores
`bit_length = byte_length * 8` as little-endian `u64`. Per-lane bit
lengths are written into the staging buffer at the correct offset for
each lane before the SIMD loop starts; nothing about the length
encoding changes between MD4 and MD5.

## 4. Detection and dispatch wiring

The CPU probe lives in
`crates/checksums/src/simd_batch/md4/mod.rs::Md4Dispatcher::has_avx512`
and
`crates/checksums/src/simd_batch/md5_dispatcher.rs::Dispatcher::has_avx512`.
Both call `is_x86_feature_detected!("avx512f") &&
is_x86_feature_detected!("avx512bw")`, gated by `feature_allowed(
SimdFeature::Avx512)` so the `--simd` CLI override can pin dispatch
below AVX-512 even on capable hosts.

The probe is only evaluated on `x86_64`; the `cfg(not(target_arch =
"x86_64"))` arm always returns `false`. The `OnceLock`-backed
dispatcher caches the result for the process lifetime, so the probe
cost is one `cpuid` per program rather than per batch.

Dispatch into the kernel is already in place in both
`Md4Dispatcher::digest_batch_avx512` and
`Dispatcher::digest_batch_avx512`. Each splits inputs into
`chunks_exact(16)` plus a trailing partial chunk that is padded with
empty `&[]` slices to a full sixteen-lane batch; the padded results
are truncated to `chunk.len()` before being appended to the output
vector. No changes to the dispatcher layer are required by this design;
all the work is in the new MD4 AVX-512 kernel.

## 5. Risks

AVX-512 carries non-trivial deployment risk on older silicon:

- **Frequency throttling on Skylake-X / Cascade Lake.** Intel's first
  three AVX-512 generations drop the all-core turbo by one or two bins
  while ZMM registers are live, and impose a brief "warm-up" period
  before full-width execution begins. A short batch can finish entirely
  within the warm-up window, leaving the rest of the process running
  at the lower clock without ever amortising the AVX-512 win.
- **Asymmetric ZMM execution units.** Skylake-X client SKUs have only
  one 512-bit FMA unit, so the per-cycle throughput is half the eight-
  or sixteen-lane theoretical peak. AMD Zen 4 splits each 512-bit op
  across two 256-bit pipes ("double-pumped"), giving close to AVX2
  throughput at twice the latency. Both effects mean AVX-512 is not
  unconditionally faster than the AVX2 rung for short batches.
- **Heterogeneous cores.** Intel Alder Lake and Raptor Lake disable
  AVX-512 entirely on retail BIOSes; the probe correctly reports
  `false` there, but mixed P-core / E-core scheduling can migrate a
  thread from a capable to an incapable core mid-execution on hacked
  firmware. The dispatcher's once-per-process probe is sufficient
  protection because affinity migrations cannot turn a feature off
  underneath running code; the CPU will fault rather than silently
  miscompute. A sigill handler is out of scope for this design.

Mitigation: gate the AVX-512 rung behind a CPU model allow-list in
`crate::cpu_features`. Hosts that report Ice Lake-SP, Sapphire Rapids,
or any Zen 4-or-later AMD part take the AVX-512 path. Older Skylake-X
and Cascade Lake parts fall through to AVX2. The model check piggy-
backs on the existing `feature_allowed` plumbing so the `--simd avx2`
override remains the canonical way for users to opt out manually.

A second mitigation is the `MAX_INPUT_SIZE = 1 MiB` ceiling already
present in `md5_simd/avx512.rs`. Very large inputs are processed by the
streaming hasher in `crates/checksums/src/strong/md5.rs`, which uses
the scalar `md5` crate; this avoids long ZMM-resident sections that
would expose the warm-up penalty.

## 6. Verification

Two layers of testing protect the AVX-512 rung.

- **Per-kernel unit tests** live alongside `md5_simd/avx512.rs` and
  the new `md4/simd/avx512.rs`. Each file checks RFC 1320 / 1321
  vectors at every batch position, all-empty batches, and lane-length
  permutations that exercise the per-lane opmask logic.
- **Cross-backend parity** is enforced by
  `crates/checksums/src/simd_parity_tests.rs::md4_simd_parity` and
  `::md5_simd_parity`. These call through `simd_batch::digest_batch`,
  so on a host that selects the AVX-512 rung the parity tests compare
  AVX-512 output against the scalar reference for RFC vectors,
  partial batches, lane-boundary sweeps, large (up to 100 KiB) inputs,
  and proptest-generated random byte vectors. A second pass with the
  `--simd avx2` and `--simd sse2` overrides (already wired through
  `feature_allowed`) re-runs the same suite against the narrower
  rungs, giving end-to-end cross-backend agreement.

CI runs the parity suite on every supported target via
`cargo nextest run -p checksums --all-features`. AVX-512 hosts are not
yet in the CI matrix, so the AVX-512 rung is also exercised by an
opt-in `qemu-x86_64 -cpu Skylake-Server-v5,+avx512f,+avx512bw` job in
`tools/ci/run_interop.sh` that runs the parity tests under emulation
before any AVX-512 code is merged. The QEMU run is gated by a workflow
input rather than every PR so the CI baseline cost stays flat.

A short benchmark harness
(`scripts/benchmark_simd_md5.sh`, MD4 sibling to follow) compares
sixteen-lane AVX-512 throughput against the AVX2 and scalar rungs on
the same host so the model allow-list from section 5 can be tuned with
measured numbers rather than vendor whitepapers alone.
