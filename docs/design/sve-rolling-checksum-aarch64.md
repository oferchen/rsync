# SVE rolling checksum on aarch64 (#2099)

Tracking issue: oc-rsync task #2099.

Related code and design notes:

- `crates/checksums/src/rolling/checksum/neon.rs` - the existing NEON
  (Advanced SIMD, fixed 128-bit) implementation that this design extends.
- `crates/checksums/src/rolling/checksum/mod.rs:50-66` - the
  `simd_available_arch()` dispatcher that selects between aarch64 NEON,
  x86 AVX2/SSE2, and the scalar fallback.
- `crates/checksums/src/rolling/checksum/x86.rs` - the AVX2/SSE2 ladder
  whose layered probing pattern (widest vector first, narrower vector
  next, scalar last) this design mirrors for aarch64.

This document is design-only. No code lands in this PR. The entry
points, type stubs, and dispatch sites named here are sketches that
follow the existing `neon::accumulate_chunk` shape so the rolling
checksum dispatch keeps a single calling convention across vector
widths.

## 1. Motivation

The rolling checksum (`get_checksum1` upstream, `RollingChecksum::update`
in oc-rsync) is on the hot path of every delta transfer: the receiver
slides it byte-by-byte over the basis file, and the sender computes one
sum per block during signature emission. On aarch64 today the work is
done by `accumulate_chunk_neon_impl`
(`crates/checksums/src/rolling/checksum/neon.rs:87`) at a fixed 16
bytes per iteration, because NEON registers are exactly 128 bits wide.

Arm's Scalable Vector Extension (SVE) lifts that ceiling. SVE registers
have an implementation-defined width between 128 and 2048 bits, in
multiples of 128, and the ISA is written so the same instruction stream
runs unmodified on a 128-bit core and a 512-bit core. The goal of this
design is to add an SVE accumulator that lets oc-rsync ride that width
on hardware that supplies it (AWS Graviton 3 and 4 at 256 bits, Fujitsu
A64FX at 512, Apple's "M-series" cores starting with M4 at 128 bits but
with cleaner predication, and a growing population of ARMv9 servers and
phones) without forking the source for each width.

### 1.1 Why SVE, not "wider NEON"

NEON is structurally fixed at 128 bits. Adding a second SIMD path with
the same instruction set but a wider register file is not expressible
in the ISA - the only forward path on aarch64 for vectors above 128
bits is SVE (or SVE2). Two SVE properties matter here, both absent
from NEON:

- **Width-agnostic vectors.** SVE code uses "vector-length agnostic"
  (VLA) intrinsics: `svcntb()` reports the byte count of the live
  vector, `svptrue_b8()` builds an all-true predicate over it, and
  `svld1_u8(pg, ptr)` loads exactly that many bytes. The same source
  produces a 16, 32, 64, ... 256-byte loop body depending on the host's
  `VL` (vector length). NEON cannot express this.
- **Predicate masking.** Every SVE memory and arithmetic instruction
  takes a governing predicate. Tail handling - the last `len % VL`
  bytes of a chunk - is a single masked load with `svwhilelt_b8(0,
  remaining)`, not a scalar fall-through. This eliminates the small-
  tail penalty that the NEON path pays through
  `accumulate_chunk_scalar_raw` (`neon.rs:124-129`).

### 1.2 Why not block on SVE2

SVE2 (ARMv9-A) extends SVE with bit-permute, complex-arithmetic, and
crypto primitives (AES, SHA3, SM4). The rolling checksum needs only
`add`, `sub`, `mul`, widening reductions, and predicated load - all
present in baseline SVE. Targeting SVE2 would shrink the addressable
hardware set (Graviton 3 is SVE-only; Graviton 4 and Apple M4 add
SVE2) without unlocking any instruction the rolling checksum uses.
The strong checksum ladder may revisit SVE2 separately for crypto
acceleration; that is out of scope here.

## 2. Hardware availability

| Platform                | Vector length | SVE | SVE2 |
|-------------------------|---------------|-----|------|
| AWS Graviton 3 / 3E     | 256 bit       | yes | no   |
| AWS Graviton 4          | 128 bit       | yes | yes  |
| Fujitsu A64FX (Fugaku)  | 512 bit       | yes | no   |
| NVIDIA Grace            | 128 bit       | yes | yes  |
| Microsoft Cobalt 100    | 128 bit       | yes | yes  |
| Ampere AmpereOne        | 128 bit       | yes | yes  |
| Apple M4                | 128 bit       | yes | yes  |
| Apple M1 / M2 / M3      | -             | no  | no   |
| Raspberry Pi 5 (BCM2712)| -             | no  | no   |

The minimum useful SVE host advertises `VL = 16` bytes, identical to
NEON in lane count, but with predicated tail handling. The peak
hardware (A64FX, 64 bytes per iteration) processes four NEON loops
worth of bytes per cycle issue. A representative cloud host
(Graviton 3, 32 bytes per iteration) doubles NEON's per-iteration
work.

## 3. Detection

aarch64 SVE detection is a two-step probe analogous to the NEON path
in `neon.rs:62-67` but split between compile-time hint and runtime
fact, because Linux distributions still ship aarch64 toolchains
defaulting to `-march=armv8-a` (no SVE).

```rust
static SVE_AVAILABLE: OnceLock<bool> = OnceLock::new();

#[inline]
fn sve_available() -> bool {
    *SVE_AVAILABLE.get_or_init(detect_sve)
}

fn detect_sve() -> bool {
    // Compile-time short-circuit: if the toolchain knows the target
    // never has SVE, skip the runtime probe entirely. This keeps the
    // detection cost off platforms like Raspberry Pi 5 and Apple M1-M3.
    if cfg!(not(target_feature = "sve")) && cfg!(not(target_feature = "neon")) {
        return false;
    }
    runtime_sve_probe()
}
```

The runtime probe must avoid issuing an SVE instruction speculatively;
on a non-SVE core that traps as `SIGILL`. Two safe sources of truth
exist:

- **Linux:** `getauxval(AT_HWCAP)` and test `HWCAP_SVE`
  (`/usr/include/asm/hwcap.h`, value `1 << 22`). The `libc` crate
  exposes both. This is the same channel the kernel uses to publish
  feature bits to dynamic linkers.
- **macOS (Apple silicon):** `sysctlbyname("hw.optional.arm.FEAT_SVE")`
  returns 1 on M4 and later. Apple does not expose SVE through
  `getauxval`; the kernel auxiliary vector is Linux-only.
- **FreeBSD / NetBSD:** `elf_aux_info(AT_HWCAP, ...)` mirrors the Linux
  contract and is wrapped by `libc::elf_aux_info`.
- **Windows on Arm:** SVE is not exposed by Windows 11 23H2 or earlier;
  the dispatcher returns `false` unconditionally on
  `target_os = "windows"` until Microsoft surfaces an
  `IsProcessorFeaturePresent` constant for it.

```rust
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
fn runtime_sve_probe() -> bool {
    // upstream: glibc sysdeps/aarch64/multiarch/init-arch.h reads HWCAP
    // through __getauxval; we follow the same convention.
    const HWCAP_SVE: libc::c_ulong = 1 << 22;
    // SAFETY: getauxval has no preconditions; the only failure mode is
    // a zero return for unknown types, which we treat as "not present".
    let hwcap = unsafe { libc::getauxval(libc::AT_HWCAP) };
    hwcap & HWCAP_SVE != 0
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn runtime_sve_probe() -> bool {
    // sysctl_arm_optional("FEAT_SVE") - returns 1 on M4 and later.
    sysctl_int(b"hw.optional.arm.FEAT_SVE\0").unwrap_or(0) != 0
}

#[cfg(all(target_arch = "aarch64",
          not(any(target_os = "linux", target_os = "macos"))))]
fn runtime_sve_probe() -> bool {
    false
}
```

The probe runs once per process and is memoized by the same `OnceLock`
pattern used for NEON. The cost is one `getauxval` (no syscall on
glibc; the kernel writes `AT_HWCAP` into the auxiliary vector at
process start, and `getauxval` reads it from memory) or one `sysctl`
call.

## 4. Implementation sketch

The SVE accumulator goes in a new file
`crates/checksums/src/rolling/checksum/sve.rs`, with the same shape
as `neon.rs`:

```rust
#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;

#[inline]
pub(super) fn simd_available() -> bool {
    sve_available()
}

#[inline]
pub(super) fn accumulate_chunk(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    if !sve_available() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }
    // SAFETY: SVE is available (checked above). The implementation is
    // vector-length agnostic and uses predicated load for the tail.
    unsafe { accumulate_chunk_sve_impl(s1, s2, len, chunk) }
}

#[target_feature(enable = "sve")]
unsafe fn accumulate_chunk_sve_impl(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    // svcntb() == implementation VL in bytes (16, 32, 48, 64, ...).
    let vl = svcntb();
    let mut offset: usize = 0;
    let n = chunk.len();

    // Lane index vector [0, 1, 2, ..., vl-1] used to derive per-lane
    // weights (vl - i) for the prefix-sum contribution to s2. Computed
    // once, outside the loop.
    let lane_idx = svindex_s16(0, 1);
    let vl_s16 = svdup_s16(vl as i16);
    let weights = svsub_s16_x(svptrue_b16(), vl_s16, lane_idx);

    while offset < n {
        let remaining = n - offset;
        let pg = svwhilelt_b8(0u64, remaining as u64);

        // Predicated load - safe past the end of the buffer because
        // inactive lanes do not perform memory access.
        let bytes_u = svld1_u8(pg, chunk.as_ptr().add(offset));
        // Reinterpret as i8 to match upstream's `schar *buf` signedness
        // (checksum.c:285, mirroring neon.rs:97).
        let bytes_s = svreinterpret_s8_u8(bytes_u);

        // Widen low and high halves to i16 so weighted multiplies fit.
        let lo = svunpklo_s16(bytes_s);
        let hi = svunpkhi_s16(bytes_s);

        // Block byte sum (s1 contribution) - widening reduction to i64
        // mirrors NEON's vaddlvq_s16 choice; no overflow risk.
        let block_sum_lo = svaddv_s16(svptrue_b16(), lo) as i64;
        let block_sum_hi = svaddv_s16(svptrue_b16(), hi) as i64;
        let block_sum = (block_sum_lo + block_sum_hi) as u32;

        // Block prefix sum (s2 contribution): sum_i (vl - i) * b_i.
        let weighted_lo = svmul_s16_x(svptrue_b16(),
                                      lo,
                                      svuzp1_s16(weights, weights));
        let weighted_hi = svmul_s16_x(svptrue_b16(),
                                      hi,
                                      svuzp2_s16(weights, weights));
        let block_prefix = (svaddv_s16(svptrue_b16(), weighted_lo) as i64
                            + svaddv_s16(svptrue_b16(), weighted_hi) as i64)
                            as u32;

        let consumed = remaining.min(vl);
        s2 = s2.wrapping_add(block_prefix);
        s2 = s2.wrapping_add(s1.wrapping_mul(consumed as u32));
        s1 = s1.wrapping_add(block_sum);
        len = len.saturating_add(consumed);
        offset += consumed;
    }

    (s1, s2, len)
}
```

Three properties of this sketch deserve emphasis:

- **No scalar tail.** The `svwhilelt_b8` predicate on the last
  iteration is exactly `min(vl, remaining)` lanes wide; inactive
  lanes are guaranteed not to fault and contribute zero to the
  reductions. Compare against `neon.rs:124-129`, which calls
  `accumulate_chunk_scalar_raw` for the trailing 0..15 bytes.
- **Width-driven weight vector.** The weight pattern that NEON
  hardcodes as `HIGH_WEIGHTS = [16, 15, 14, 13, 12, 11, 10, 9]` and
  `LOW_WEIGHTS = [8, 7, 6, 5, 4, 3, 2, 1]` (`neon.rs:59-60`) is
  computed at runtime here as `vl - lane_idx`, identical in semantics
  but valid at any `vl`.
- **i16 product safety.** Maximum lane magnitude is `127 * vl`, and
  `vl <= 256` on the largest documented SVE implementation (A64FX,
  2048-bit). `127 * 256 = 32512` fits in i16 with margin, matching
  the analysis in `neon.rs:110-111`.

## 5. Dispatch ladder

`simd_available_arch()` (`mod.rs:50-66`) gains an SVE arm above NEON:

```rust
#[cfg(target_arch = "aarch64")]
#[inline]
fn simd_available_arch() -> bool {
    sve::simd_available() || neon::simd_available()
}
```

`accumulate_chunk_dispatch` follows the same order:

| Architecture | Order | Path                                              |
|--------------|-------|---------------------------------------------------|
| aarch64      | 1     | SVE (vl bytes/iter) when `HWCAP_SVE`              |
| aarch64      | 2     | NEON (16 bytes/iter) when `HWCAP_NEON` (always on aarch64) |
| All          | last  | Scalar 4-byte unrolled loop                       |

The first call evaluates both `OnceLock`s; subsequent calls are
straight-line atomic loads. The fallback chain SVE -> NEON -> scalar
preserves the byte-for-byte parity guarantee already enforced by
`rolling::tests::checksum::simd`: every existing test runs against
the chosen path on the host, and the new SVE path is added to the
parity matrix.

## 6. Build and test

- The new module is gated `#[cfg(target_arch = "aarch64")]` (same as
  `neon`) and additionally `#[cfg(target_feature = "sve")]` is
  **not** required at compile time - the host probe is the gate. This
  matches how `x86.rs` ships AVX2 code unconditionally and only enters
  it when `is_x86_feature_detected!("avx2")` returns true.
- Toolchain: SVE intrinsics live in `core::arch::aarch64` behind the
  `stdsimd` feature on stable as of Rust 1.85; the workspace already
  pins 1.88. No new nightly requirement.
- Cross-compilation needs `qemu-user-static` with `cpu=max,sve=on,sve512=on`
  for parity tests, since neither the GitHub aarch64 macOS runner nor
  the standard Linux runners advertise SVE. CI changes are tracked
  separately under the SIMD test-matrix issue and are out of scope
  for this design.
- The parity test file
  `crates/checksums/src/rolling/checksum/tests.rs` is extended with
  an `accumulate_chunk_sve_for_tests` companion to
  `accumulate_chunk_neon_for_tests`
  (`neon.rs:135-146`); the existing property tests then iterate
  over `[scalar, neon, sve]` on aarch64 hosts and assert byte
  equality across all three.

## 7. Risks and open questions

- **VL changes across migration.** Live migration of an SVE VM
  between hosts with different `VL` is documented as forbidden by
  Arm but supported in practice on some hypervisors. Because the
  weight vector is rebuilt every call from `svcntb()`, a `VL`
  change between calls is correct but expensive (the hot path
  recomputes `weights`). Hoisting `svcntb()` and `weights` into
  thread-local state is a small follow-up if profiling shows the
  recompute is measurable.
- **i16 product margin on hypothetical 4096-bit SVE.** Arm reserves
  `VL` up to 2048 bits (256 bytes). At 256 bytes, max product is
  `127 * 256 = 32512`, still in i16. If a future Arm revision lifts
  `VL` past 2048 bits the multiply must widen to i32; the parity
  test will catch the overflow before it ships.
- **macOS sysctl name stability.** Apple's `hw.optional.arm.FEAT_*`
  family is documented but not contractually stable. The probe
  treats a missing key as "no SVE", which is the safe direction.
- **Strong-checksum SVE2 path.** Out of scope. Tracked separately
  under the strong-checksum acceleration epic.
