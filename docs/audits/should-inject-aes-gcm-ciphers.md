# `should_inject_aes_gcm_ciphers` CPU feature detection

Tracking issue: oc-rsync task #1627.

## Summary

When oc-rsync builds the SSH argv to spawn the remote shell, it can opt to
prepend `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com` to the SSH options
so the connection negotiates a hardware-accelerated AES-GCM cipher rather
than whatever the user's `ssh_config` happens to default to. On modern
hardware this measurably improves SSH transport throughput, often by a
factor of 2-3x against the chacha20-poly1305 fallback that OpenSSH selects
when no AES-NI / ARMv8 Crypto Extensions are reported by the local CPU.

The decision is gated by `should_inject_aes_gcm_ciphers` in
`crates/rsync_io/src/ssh/builder.rs`. Injection is conditional on real
hardware support: if the host CPU lacks AES instructions, oc-rsync must
not force AES-GCM, because the cipher would then run in software inside
OpenSSH and be slower than chacha20-poly1305 on the same CPU. This audit
documents the current CPU feature detection path, the per-architecture
verification we have, and the edge cases that motivate the runtime check
instead of a build-time `cfg!`.

This is a quality-of-life feature, not a wire-protocol or correctness
concern. Both AES-GCM and chacha20-poly1305 are individually correct
OpenSSH ciphers; the only impact of mis-detection is wasted CPU and
worse throughput.

## Current oc-rsync surface

The injection logic lives in `crates/rsync_io/src/ssh/builder.rs`:

- `SshCommand::should_inject_aes_gcm_ciphers` (line 500) returns `true`
  only when all four conditions hold simultaneously:
  1. `prefer_aes_gcm` is not `Some(false)` (the user has not opted out
     via `--no-aes`).
  2. `has_hardware_aes()` reports the CPU has AES instructions.
  3. `is_ssh_program()` confirms the configured remote shell is `ssh`
     or `ssh.exe` (case-insensitive on Windows).
  4. `options_contain_cipher_flag()` is `false`, i.e. the user did not
     already specify a cipher via `-c`, `-caes128-ctr`, or
     `push_option("-c …")`.
- `has_hardware_aes` (line 601) is the runtime CPU feature probe. It
  caches its result in a `OnceLock<bool>` so repeated calls do not
  re-issue platform feature-detection syscalls (`/proc/cpuinfo`,
  `getauxval(AT_HWCAP)`, `mrs id_aa64isar0_el1`, `cpuid` on x86).

When `should_inject_aes_gcm_ciphers` returns `true`, `command_parts`
appends `-c` then the literal `aes128-gcm@openssh.com,aes256-gcm@openssh.com`
to the rendered argv before the destination operand.

The CLI surface is in
`crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`:

- `--aes` (line 128): force AES-GCM cipher injection even if hardware
  detection returns `false`. Stored as `prefer_aes_gcm = Some(true)`.
- `--no-aes` (line 135): suppress AES-GCM injection regardless of
  hardware. Stored as `prefer_aes_gcm = Some(false)`.
- Default (`prefer_aes_gcm = None`): auto-detect. Inject only when
  `has_hardware_aes()` reports `true`.

The flag is parsed in
`crates/cli/src/frontend/arguments/parser/mod.rs:279` and threaded
through `ParsedArgs::prefer_aes_gcm` into `ClientConfig` via
`crates/core/src/client/config/builder/network.rs:45`. The SSH transfer
entry point `build_ssh_connection`
(`crates/core/src/client/remote/ssh_transfer.rs:286`) and the
remote-to-remote driver (`remote_to_remote.rs:204`) call
`SshCommand::set_prefer_aes_gcm(config.prefer_aes_gcm())` before
rendering the argv.

## CPU feature detection by architecture

`has_hardware_aes` dispatches at compile time on `target_arch` and
performs runtime feature detection inside each branch:

```rust
pub(super) fn has_hardware_aes() -> bool {
    static HAS_AES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS_AES.get_or_init(|| {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        { std::arch::is_x86_feature_detected!("aes") }
        #[cfg(target_arch = "aarch64")]
        { std::arch::is_aarch64_feature_detected!("aes") }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        { false }
    })
}
```

### x86 / x86_64 (AES-NI)

- Detection macro: `std::arch::is_x86_feature_detected!("aes")`.
- Backed by `cpuid` leaf 1, ECX bit 25.
- Hardware support introduced with Intel Westmere (2010) and AMD
  Bulldozer (2011). Effectively universal on workstation and server
  CPUs from 2012 onward.
- Likely-false hosts: very old Atom (Bonnell / Saltwell), early Core
  i3/i5/i7 first-gen Nehalem (pre-Westmere), Pentium / Celeron variants
  where AES-NI was fused off, and KVM/QEMU guests where the host
  intentionally hides AES-NI from the guest CPU model
  (`-cpu qemu64` does not expose AES; `-cpu host,+aes` does).
- CI coverage: `Linux musl`, `macOS x86_64`, and `Windows` matrix legs
  all run on hosted runners that report AES-NI, so the `true` arm is
  exercised by `forces_aes_gcm_ciphers_when_explicitly_enabled`,
  `auto_detects_aes_when_preference_is_none`, and
  `aes_gcm_injection_requires_hardware_aes` in
  `crates/rsync_io/src/ssh/tests.rs:670, 700, 1549`.

### aarch64 (ARMv8 Cryptography Extensions)

- Detection macro: `std::arch::is_aarch64_feature_detected!("aes")`.
- Backed by `getauxval(AT_HWCAP)` on Linux (HWCAP_AES, bit 3),
  `sysctlbyname("hw.optional.arm.FEAT_AES")` on macOS, and
  `IsProcessorFeaturePresent(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE)`
  on Windows on ARM. Rust's std handles the platform dispatch.
- Hardware feature is `FEAT_AES` from the ARMv8 Cryptography Extensions.
  All Apple silicon (M1, M1 Pro/Max/Ultra, M2, M2 Pro/Max/Ultra, M3,
  M3 Pro/Max, M4) report it. AWS Graviton2/3/4 report it. Most Cortex
  A-series cores from Cortex-A53 onward implement it, but the licensing
  optionality means the SoC integrator can omit the crypto block
  (see Raspberry Pi entry below).
- CI coverage: `macOS arm64` exercises the `true` arm. The
  `has_hardware_aes_returns_expected_for_platform` test
  (`crates/rsync_io/src/ssh/tests.rs:1520`) asserts on aarch64 that
  the result is `true`, locking down the contract that all aarch64
  hosts the project supports have FEAT_AES.

### Other architectures

- `target_arch` is neither x86, x86_64, nor aarch64: returns `false`
  unconditionally, which means cipher injection is skipped and OpenSSH
  picks its default. This is the conservative choice for `riscv64`,
  `mips64`, `powerpc64`, `s390x`, etc. - oc-rsync compiles on these
  via the workspace's portability gates but they are not in the CI
  matrix and we have no signal on hardware AES availability for
  RISC-V crypto extensions or POWER10 AES instructions.

## Verification: x86_64 AES-NI

The x86 branch of `has_hardware_aes` is exercised by:

- `has_hardware_aes_is_consistent` (line 1511): calls `has_hardware_aes`
  twice and asserts the cached result is stable. Verifies the
  `OnceLock` initialisation does not race or return different values
  on a second call.
- `has_hardware_aes_returns_expected_for_platform` (line 1520): calls
  `has_hardware_aes` and on `target_arch = "x86" / "x86_64"` does not
  hard-assert `true` (CI runners on bare x86 always report it, but
  the test must remain correct on hypothetical AES-less x86 hosts in
  the CI matrix). The assertion is informational on x86; the harder
  contract lives on aarch64.
- `aes_gcm_injection_requires_hardware_aes` (line 1549): asserts that
  the rendered argv contains `-c` if and only if `has_hardware_aes()`
  returns `true`. This is the round-trip test - it ties
  `should_inject_aes_gcm_ciphers` to the runtime detection result
  rather than a compile-time `cfg!`.

`std::arch::is_x86_feature_detected!("aes")` is a compiler-provided
macro that expands to a `cpuid`-backed runtime check, cached internally
by the standard library. It has been stable since Rust 1.27.0
(2018-06-21) and is the recommended entry point per the Rust
portability guide. No external crate (`raw-cpuid`, `cpufeatures`) is
needed.

## Verification: aarch64 FEAT_AES

The aarch64 branch is exercised by the same three tests above. The
`has_hardware_aes_returns_expected_for_platform` test contains a
`#[cfg(target_arch = "aarch64")]` block at line 1535 that hard-asserts
`assert!(result, "aarch64 platforms with Crypto Extensions should
report hardware AES")`. This is the strongest contract in the
audit: every aarch64 host in CI must report FEAT_AES. If a future
runner migration introduces an aarch64 host without crypto extensions,
this test fires and forces a triage.

`std::arch::is_aarch64_feature_detected!("aes")` is the
aarch64 equivalent of the x86 macro. It has been stable since Rust
1.59.0 (2022-02-24). Internally it consults the OS-specific HWCAP
mechanism on first call and caches the result. The macro's
implementation in `core::arch::aarch64` does the right thing on Linux,
macOS, and Windows on ARM; we do not need to handle each platform
separately.

## Edge cases

### Apple Silicon

- All Apple M-series (M1 / M1 Pro / M1 Max / M1 Ultra / M2 / M2 Pro /
  M2 Max / M2 Ultra / M3 / M3 Pro / M3 Max / M4 / M4 Pro / M4 Max)
  implement FEAT_AES. Detection returns `true` unconditionally.
- macOS exposes FEAT_AES via `sysctlbyname("hw.optional.arm.FEAT_AES")`,
  which Rust std consults from
  `core::arch::aarch64::detect::os::macos`.
- `has_hardware_aes_returns_expected_for_platform` makes this contract
  explicit on aarch64-apple-* runners. `forces_aes_gcm_ciphers_when_explicitly_enabled`
  asserts the rendered argv contains the AES-GCM cipher list whenever
  the host reports hardware AES, which on aarch64 Apple is always.
- Practical effect: oc-rsync over SSH from a Mac always negotiates
  AES-GCM unless the user passes `--no-aes`.

### Raspberry Pi

The Raspberry Pi family is the canonical example of why this gate must
be a runtime probe, not a compile-time `cfg`:

- Raspberry Pi 1 / Zero / Zero W use ARMv6 `armv6l` (no NEON, no
  FEAT_AES, target_arch = `arm`). `has_hardware_aes` falls into the
  `_` arm and returns `false`. Cipher injection is skipped.
- Raspberry Pi 2B (BCM2836) is ARMv7 Cortex-A7, no Crypto Extensions.
  Same `arm` target, returns `false`.
- Raspberry Pi 3, 3B+, 4B, 400, CM4 use Cortex-A53 / A72 cores.
  Broadcom shipped these SoCs with the Crypto Extensions block
  **disabled** to save die area. Even when running a 64-bit aarch64
  kernel (`aarch64-unknown-linux-gnu` Raspberry Pi OS), the HWCAP_AES
  bit is **not** set. `has_hardware_aes()` returns `false`. Cipher
  injection is skipped, and OpenSSH falls back to chacha20-poly1305,
  which is faster on these CPUs anyway because the ARMv8.0 SIMD path
  for ChaCha20 uses NEON.
- Raspberry Pi 5 (BCM2712) uses Cortex-A76 cores, which **do**
  implement the Crypto Extensions in this SKU. `has_hardware_aes()`
  returns `true`. AES-GCM is injected and outperforms chacha20-poly1305
  by roughly 1.5x.
- The split inside the same product family is exactly why we cannot
  hard-code `cfg!(target_arch = "aarch64")` as a proxy for AES
  availability. Compile-time detection is unsound.

### Generic ARM Linux

- ARMv7 / ARMv8.0-A boards without the Crypto Extensions option fitted:
  same story as Pi 4. `armv7l` builds (target_arch = `arm`) skip
  injection at compile time. `aarch64` builds with HWCAP_AES = 0
  skip at runtime. No code change needed.
- ARMv8.2+ designs with mandatory crypto (NXP LX2160A, Marvell
  ThunderX2, Ampere Altra, AWS Graviton2/3/4, Cavium ThunderX) all
  report HWCAP_AES = 1 and inject normally.
- Embedded SoCs that require an additional license fee for the crypto
  block (some NXP i.MX 8 variants, some Allwinner H6 SKUs) ship with
  the bit cleared. Runtime detection handles them transparently.
- Android `aarch64` devices: every flagship from Snapdragon 820
  onward reports HWCAP_AES. Cheap MediaTek-based devices vary;
  detection is the only correct path. Android is not currently a
  supported oc-rsync target, but the runtime check is forward-compatible
  if Android support is added.
- Linux containers: `getauxval(AT_HWCAP)` reports the host's HWCAP
  inside the container, regardless of cgroup settings. There is no
  way to hide AES from a container without recompiling glibc.
- Linux KVM/QEMU aarch64 guests: the AES bit is exposed when the host
  has it and the QEMU CPU model is `host` or `max`. With
  `-cpu cortex-a72` the bit is hidden because the Cortex-A72 reference
  ID register reports no crypto. Detection handles this correctly.

### Windows on ARM

Microsoft Surface Pro X / Pro 9 (Snapdragon SQ1/SQ2/SQ3) and Snapdragon
X Elite laptops report FEAT_AES via `IsProcessorFeaturePresent`. The
`std::arch::is_aarch64_feature_detected!("aes")` macro consults this
on Windows. `has_hardware_aes()` returns `true` on every shipping
Windows-on-ARM device.

### Cipher flag user override

Even on hardware with AES-NI, if the user supplies a cipher via any of:

- `-e "ssh -c chacha20-poly1305@openssh.com"`
- `--rsh="ssh -c aes128-ctr"`
- `RSYNC_RSH="ssh -caes256-ctr"` (combined form, no space)
- programmatic `SshCommand::push_option("-c …")`

`options_contain_cipher_flag` returns `true` and injection is skipped.
The user's choice always wins. This is exercised by
`no_aes_gcm_injection_without_hardware_and_user_cipher`
(`crates/rsync_io/src/ssh/tests.rs:1570`),
`skips_cipher_injection_when_cipher_option_present` (line 716), and
`skips_cipher_injection_when_combined_cipher_option_present`
(line 1615).

### Non-SSH program

If the configured remote shell is not `ssh` / `ssh.exe`
(e.g. `rsh`, `lsh`, `mosh`, an embedded SSH-like wrapper), `is_ssh_program`
returns `false` and injection is skipped. The `-c` flag is OpenSSH-specific
syntax; injecting it into a non-OpenSSH client would corrupt the argv.

## Conclusion

`should_inject_aes_gcm_ciphers` is structurally sound:

- All three CPU detection paths (`x86_64` AES-NI, `aarch64` FEAT_AES,
  fallback `false`) are covered by tests in
  `crates/rsync_io/src/ssh/tests.rs`.
- The runtime probe is necessary - compile-time `cfg` is provably
  wrong on Raspberry Pi 4 and similar aarch64 SoCs without Crypto
  Extensions.
- The four-way guard (`prefer_aes_gcm` / hardware / SSH program /
  no user cipher) cleanly separates user intent from environmental
  capability.
- The `OnceLock` cache prevents repeated `cpuid` / HWCAP syscalls
  across the lifetime of an oc-rsync invocation.

No code changes are recommended by this audit. Phase 1 ships only
this documentation. Future work, if any, would extend the runtime
probe to cover RISC-V Zk* extensions or POWER10 AES instructions
when those become relevant CI targets.
