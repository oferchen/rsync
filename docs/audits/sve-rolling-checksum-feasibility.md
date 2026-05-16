# SVE rolling checksum on aarch64: stable-Rust feasibility

Tracking task: oc-rsync #2099.

## Summary

The existing design note at `docs/design/sve-rolling-checksum-aarch64.md`
sketches a width-agnostic SVE accumulator that would replace the fixed
16-byte NEON loop in `crates/checksums/src/rolling/checksum/neon.rs`.
This audit is the implementation-readiness review that asks the
narrower question: can that sketch land today on the workspace
toolchain, and if not, what concretely unblocks it?

Verdict: **not yet, on stable Rust 1.88**. The audit catalogs the four
independent gates that each block landing real SVE code, names the
upstream tracking issues for each, and lays out the two intermediate
shapes (no-op stub plus inline asm) that the project could land
without waiting for any of them. It then recommends staying on the
current NEON path until the intrinsic surface stabilizes, because
neither intermediate shape pays for the maintenance burden it adds.

Audit is docs-only. No Rust source, `Cargo.toml`, or dispatch wiring
is touched.

## 1. The four gates

Every gate below must clear before the design-note sketch compiles on
the workspace toolchain. They are independent: closing three of four
still leaves a broken build.

### 1.1 Gate A: stable SVE intrinsics in `core::arch::aarch64`

`core::arch::aarch64` exposes the NEON family on stable, but the SVE
family (`svld1_u8`, `svptrue_b8`, `svwhilelt_b8`, `svaddv_s16`,
`svindex_s16`, `svdup_s16`, `svunpklo_s16`, `svunpkhi_s16`,
`svmul_s16_x`, `svreinterpret_s8_u8`, `svcntb`, `svuzp1_s16`,
`svuzp2_s16`) is gated behind the unstable feature flag
`stdarch_aarch64_sve`. The flag is tracked under
rust-lang/rust#94830 ("Tracking issue for the
`stdarch_aarch64_sve` API"). As of Rust 1.88.0 (the workspace pin in
`rust-toolchain.toml`), the gate is still nightly-only.

Concrete evidence of the gate: the upstream `core_arch` crate in the
Rust source tree marks every SVE intrinsic with

```text
#[unstable(feature = "stdarch_aarch64_sve", issue = "94830")]
```

Any attempt to call one of those intrinsics from a stable crate fails
with E0658 ("use of unstable library feature"). The workspace pins
`channel = "1.88.0"` and stipulates "no nightly-only code - it would
break the entire workspace build", so the design-note sketch is
literally uncompilable today.

The companion stdarch test crate
(`library/stdarch/crates/core_arch/src/aarch64/sve.rs` upstream) is
also nightly-only, meaning the parity tests proposed in the design
document cannot run on stable even if the production code were
guarded.

The design note's claim that "SVE intrinsics live in
`core::arch::aarch64` behind the `stdsimd` feature on stable as of
Rust 1.85" is incorrect. Rust 1.85 stabilized portions of `stdsimd`
that covered new x86 and wasm intrinsics; the aarch64 SVE surface
stayed behind its own dedicated nightly feature and remains there.
This audit supersedes that note.

### 1.2 Gate B: stable `#[target_feature(enable = "sve")]`

Even if the intrinsics were stable, calling them from a
`#[target_feature(enable = "sve")]` function on stable requires that
the `"sve"` target feature itself be in the stable allow-list. The
allow-list lives in `compiler/rustc_target/src/target_features.rs`.
On Rust 1.88, the aarch64 entries flagged as stable include `"neon"`,
`"aes"`, `"sha2"`, `"sha3"`, `"crc"`, `"lse"`, `"rdm"`, `"dotprod"`,
`"fp16"`, `"rcpc"`, and `"rcpc2"`. The entries for `"sve"`,
`"sve2"`, `"sve2-aes"`, `"sve2-sha3"`, `"sve2-sm4"`, and
`"sve2-bitperm"` are tagged with `Unstable(sym::aarch64_target_feature)`
and surface only on nightly.

A stable function written as

```rust
#[target_feature(enable = "sve")]
unsafe fn accumulate_chunk_sve_impl(...) { ... }
```

fails to compile on Rust 1.88 with E0658 against the
`aarch64_target_feature` gate, regardless of whether the body actually
references an SVE intrinsic. This gate is conceptually independent
from Gate A: SIMDe-style scalar emulation hidden behind
`#[target_feature(enable = "sve")]` would still be blocked here.

Tracking: rust-lang/rust#44839 ("Tracking issue for the
`aarch64_target_feature` flag set").

### 1.3 Gate C: stable `is_aarch64_feature_detected!("sve")`

`std::arch::is_aarch64_feature_detected!` is stable, but its argument
table is the same allow-list as Gate B. Passing `"sve"` to the macro
on Rust 1.88 emits E0658:

```
error[E0658]: the target feature `sve` is currently unstable
   help: add `#![feature(aarch64_target_feature)]`
```

This is the macro the dispatcher would call from safe code to decide
whether to enter the SVE arm. Because the dispatcher is in
`crates/checksums/src/rolling/checksum/mod.rs` (which is
`#![deny(unsafe_code)]` at the workspace level for non-FFI crates,
and even in `checksums` is only `#[allow(unsafe_code)]` per-function
under the workspace's unsafe-code policy), the project cannot work
around Gate C by hiding the macro inside `unsafe`.

The two raw-syscall fallbacks the design note proposes
(`getauxval(AT_HWCAP) & HWCAP_SVE` on Linux,
`sysctlbyname("hw.optional.arm.FEAT_SVE")` on macOS) sidestep Gate C
specifically. They do not help with Gates A or B.

### 1.4 Gate D: cross-platform parity tests

The workspace SIMD policy requires that SIMD and scalar
implementations stay in lockstep with parity tests as a precondition
for landing any new vector path.
The current NEON parity tests
(`crates/checksums/src/rolling/checksum/tests.rs`) run on every
aarch64 host because NEON is mandatory in ARMv8-A and therefore
always advertised by Apple Silicon CI runners
(`macos-14` / `macos-15`) and the aarch64 Linux runners in the
Linux musl matrix.

SVE has no comparable mandatory baseline:

- Apple M1, M2, M3 do not implement SVE. The macOS aarch64 CI runner
  pool, which today rides on M1/M2 hardware, advertises
  `hw.optional.arm.FEAT_SVE = 0`.
- The default GitHub-hosted `ubuntu-22.04` and `ubuntu-24.04` aarch64
  runners run on Graviton 2 (no SVE) and Cobalt 100 (SVE-capable,
  `VL = 16`) hosts respectively, but the exact mix is opaque and not
  contracted.
- The Windows-on-Arm runner (`windows-11-arm`) executes on Qualcomm
  Snapdragon X parts which advertise SVE through the ARM ISA but
  surface no kernel hook the dispatcher can probe; Windows 11 24H2
  has no `PF_*` constant for SVE in `IsProcessorFeaturePresent`.

Without a contracted SVE-bearing CI runner, parity coverage requires
emulation. The supported emulation paths are:

- `qemu-aarch64-static` with `-cpu max,sve=on,sve256=on` (or any
  documented `sve<N>` width). Linux-only; requires
  `binfmt_misc` registration on the host runner.
- The Arm Instruction Emulator (`armie`) and the Arm Statistical
  Profiling Extension simulator: licensed Arm tools, not freely
  redistributable, blocked for CI use.

The QEMU path is achievable but adds a new CI matrix axis (vector
length sweep across 128, 256, 384, 512, 1024, 2048 bits) and a new
toolchain dependency. The cost is real, and it lands before any user
benefit is observable. This is the gate the design note acknowledged
as "out of scope for this design"; this audit confirms it as a hard
prerequisite, not an optimisation.

## 2. Intermediate shapes the project could land today

Two shapes could land on stable Rust 1.88 without clearing Gates A,
B, or C. Both have been considered and both are rejected for the
reasons in their respective subsection.

### 2.1 Inline-asm SVE loop

Rust stable supports inline assembly for aarch64 via
`core::arch::asm!`, and `asm!` does not consult the
`aarch64_target_feature` allow-list - the assembler interprets the
instructions. A hand-written SVE accumulator could therefore land
today, gated by a runtime `getauxval`/`sysctl` probe and a manual
`#[allow(unsafe_code)]`.

Rejected on three counts:

1. **Maintainability.** The NEON loop is 35 lines of intrinsics with
   one-to-one mapping to the upstream `checksum.c` semantics; the
   equivalent inline-asm body is ~120 lines of ARM assembly with
   manual register allocation, manual lane index materialisation,
   and a separate prologue per vector width. Future drive-by changes
   to the accumulator (e.g. signed/unsigned reinterpretation tweaks
   chasing a future `schar` change in upstream) cannot be made
   without re-validating the assembly.
2. **Safety surface.** The workspace audit policy calls for thorough
   safety comments and parity tests on every unsafe block in
   `checksums`. Inline asm dodges the type system entirely,
   so the safety argument must be written by hand against the ARM
   ARM (Architecture Reference Manual) for each instruction used.
   The argument is non-trivial - SVE memory operands have lane-level
   fault semantics that intrinsics elide.
3. **Parity testing.** Inline asm cannot be exercised by the
   `simd_parity_tests.rs` table - which compares scalar against a
   compiled-in SIMD implementation - on a host that lacks SVE,
   because the asm sequence will SIGILL on first execution. Without
   Gate D's CI path, the parity test is effectively unrunnable.

### 2.2 Stub module compiled out on stable

A second option is to land
`crates/checksums/src/rolling/checksum/sve.rs` as a no-op stub that
exports `simd_available() -> false` and `accumulate_chunk` returning
`accumulate_chunk_scalar_raw`, gated `#[cfg(target_arch = "aarch64")]`
but otherwise inert. The dispatcher in `mod.rs` would gain the SVE
arm and immediately fall through to NEON because the stub always
reports `false`.

Rejected on two counts:

1. **No observable value.** The dispatcher does not change behaviour;
   the only effect is a new module, a new dispatch arm, and a new
   line in the documentation - all of which are reset to zero the
   moment Gates A and B clear and real code lands.
2. **Misleading capability surface.** Code reading the dispatch
   ladder in `mod.rs` would see an SVE arm and assume it does
   something. The codebase already practises this pattern
   judiciously (`fast_io::io_uring_stub`), but those stubs hide
   platform-specific FFI; an SVE stub hides nothing the dispatcher
   could not express as a comment.

## 3. What clearing each gate looks like

The recommended posture is to wait. To make the wait actionable, each
gate has an observable signal that the project can watch without
polling:

| Gate | Cleared when | Signal |
|------|--------------|--------|
| A | `stdarch_aarch64_sve` stabilizes | rust-lang/rust#94830 closed as stabilized; SVE intrinsics appear without a `#[feature(...)]` gate in `library/stdarch/crates/core_arch/src/aarch64/sve.rs` |
| B | `"sve"` and `"sve2"` enter the stable target-feature allow-list | rust-lang/rust#44839 closed; `compiler/rustc_target/src/target_features.rs` lists the `"sve"` entry without `Unstable(...)` |
| C | `is_aarch64_feature_detected!("sve")` accepts `"sve"` on stable | Same signal as Gate B; the macro consults the same allow-list |
| D | CI gains an SVE-bearing target | Either GitHub publishes an SVE-bearing aarch64 runner SKU, or the project commits to running `qemu-aarch64-static -cpu max,sve=on` for the rolling-checksum test crate under a new CI job |

Gates A and B are coupled in practice - stabilization batches in
rust-lang/rust tend to ship the intrinsics and target feature
together - but they are independent in principle and either could
ship first. Gate D is independent of all three; the project could
proactively land a QEMU CI job today and have it test only the NEON
and scalar paths, with the SVE arm activating automatically when
Gates A through C clear.

## 4. Recommendation

Hold the SVE accumulator. The NEON path delivers full 128-bit
saturation on every aarch64 host the project supports, including the
macOS CI runners. The cost of waiting is bounded: SVE adoption is
gated by hardware (Graviton 3 onward, Apple M4 onward, ARMv9 phones),
the population is still small relative to the NEON-only fleet, and
the rolling checksum is a small share of the wall-clock budget on
hosts where the network or disk dominates. The cost of landing
inline asm now is unbounded: it commits the project to a maintenance
burden that compounds with every aarch64 microarchitecture revision.

Track the four gates above. When Gates A and B clear in the same
Rust release, revisit the design note at
`docs/design/sve-rolling-checksum-aarch64.md`, correct the toolchain
claim in section 6 of that document, and land the sketch as written
with the parity tests guarded behind a QEMU CI job.

## 5. Related references

- `docs/design/sve-rolling-checksum-aarch64.md` - the design sketch
  this audit reviews and conditionally endorses.
- `docs/design/avx512-vpdpbusd-adler32.md` - the parallel design note
  on x86_64 that uses already-stable intrinsics and therefore faces
  no analogous toolchain gate.
- `crates/checksums/src/rolling/checksum/neon.rs` - the in-tree NEON
  accumulator the SVE work would supplement, not replace.
- `crates/checksums/src/rolling/checksum/mod.rs` - the dispatcher
  that would gain the SVE arm.
- `crates/checksums/src/cpu_features.rs` - the SIMD-level override
  surface that the SVE arm would extend with a new `SimdFeature::Sve`
  variant once Gates B and C clear.
- rust-lang/rust#94830 - `stdarch_aarch64_sve` tracking issue.
- rust-lang/rust#44839 - `aarch64_target_feature` tracking issue.
- ARM ARM DDI 0487 Chapter F1 - SVE ISA reference, the source of
  truth for the intrinsic semantics referenced in the design note.
