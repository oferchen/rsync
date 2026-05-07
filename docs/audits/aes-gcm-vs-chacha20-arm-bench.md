# AES-GCM vs ChaCha20-Poly1305 on aarch64: benchmark plan

Tracking task: oc-rsync #1632.

## Summary

oc-rsync injects `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com` into the
SSH argv when `prefer_aes_gcm` is set and the host CPU advertises hardware
AES (AES-NI on x86, ARMv8 crypto extensions on aarch64). On aarch64 SoCs that
ship without crypto extensions (older Cortex-A53, low-end Allwinner/Rockchip
boards, some embedded NXP parts) the same injection forces the SSH stack
through a software AES-GCM path that is materially slower than ChaCha20-Poly1305.
This document catalogs where the choice is made today, defines a loopback
benchmark plan to quantify the gap on real aarch64 hardware, and proposes a
data-backed decision rule for when to fall back to ChaCha20.

This audit is docs-only. No Rust, no Cargo.toml, no flag wiring is changed by
landing this file. The benchmark drives a follow-up flag-tuning task, not a
behaviour change here.

## Where SSH cipher selection happens

Two cipher-selection paths live in the workspace; both should be exercised
by the benchmark plan.

### External-OpenSSH path (default)

- Builder: `crates/rsync_io/src/ssh/builder.rs`
  - `SshCommand::set_prefer_aes_gcm` (line 215) records the user preference.
  - `SshCommand::should_inject_aes_gcm_ciphers` (line 500) gates injection on
    three conditions: `prefer_aes_gcm != Some(false)`, `has_hardware_aes()`
    returns `true`, and `options_contain_cipher_flag()` is `false`.
  - `has_hardware_aes` (line 601) caches a `OnceLock<bool>` that wraps
    `is_x86_feature_detected!("aes")` on x86, `is_aarch64_feature_detected!("aes")`
    on aarch64, and returns `false` on every other architecture.
  - Injection itself appends `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com`
    immediately before the target argument (lines 414-422).
- Driver: `crates/core/src/client/remote/ssh_transfer.rs:286` calls
  `ssh.set_prefer_aes_gcm(config.prefer_aes_gcm())` with the value from
  `ClientConfig::prefer_aes_gcm()`.
- CLI plumbing: `--aes` and `--no-aes` are parsed in
  `crates/cli/src/frontend/arguments/parser/mod.rs:279` and exposed on
  `ParsedArgs::prefer_aes_gcm`. They flow through `ClientConfig::prefer_aes_gcm`
  (`crates/core/src/client/config/builder/mod.rs:251,416`) to the SSH builder.

### Embedded-russh path

- `crates/rsync_io/src/ssh/embedded/cipher.rs:51` exposes `default_ciphers()`
  which inverts the order based on `has_aes_ni()` (line 19, the same CPUID/MRS
  probe surfaced under a different name): hardware-AES hosts get
  `aes128-gcm@openssh.com, aes256-gcm@openssh.com, chacha20-poly1305@openssh.com`;
  hosts without hardware AES get `chacha20-poly1305@openssh.com,
  aes128-gcm@openssh.com, aes256-gcm@openssh.com`.
- The embedded path is used only when the system `ssh` binary is unavailable;
  it does not honour `--aes` / `--no-aes` directly because `russh` negotiates
  from a list rather than a forced choice.

The two paths agree on the detection primitive (architecture-feature probe,
cached in a `OnceLock`) but disagree on what they do with a "no hardware AES"
result: the external path simply does not inject `-c`, so OpenSSH's own
default selection (typically ChaCha20-Poly1305 first on builds shipped by
Debian, Ubuntu, RHEL, Alpine, and Homebrew) takes effect. The embedded path
explicitly reorders ChaCha20 first.

## AES-GCM hardware path vs ChaCha20-Poly1305

### Hardware AES-GCM

- **x86 / x86_64**: AES-NI (Intel Westmere 2010+, AMD Bulldozer 2011+) and
  PCLMULQDQ for GHASH multiplication. OpenSSL/wolfSSL/libgcrypt keep AES-GCM
  in a 16-byte-wide pipeline; throughput on a single core comfortably
  exceeds 4 GB/s on a modern desktop part. The cost on the rsync side is
  invisible compared to disk and network.
- **aarch64 with crypto extensions** (ARMv8.0-A optional, ARMv8.4-A
  mandatory): `AESE`, `AESD`, `AESMC`, `AESIMC` for the AES rounds and
  `PMULL`/`PMULL2` for the GHASH multiply. Apple M-series, AWS Graviton 2/3,
  Ampere Altra, Cortex-A55/A75/A76/A77/A78/X1, and most ARMv8.2+ SoCs ship
  this. Throughput is on par with x86 AES-NI per clock.
- **aarch64 without crypto extensions**: Cortex-A53 r0/r1 stepping (early
  Raspberry Pi 3, Allwinner H5/H6, Rockchip RK3328/RK3399 little cluster),
  Cortex-A35, some Marvell Armada 7K/8K SKUs, and any chip where the OEM
  fused off `ID_AA64ISAR0_EL1.AES`. On these parts AES-GCM falls back to a
  T-table software implementation that is 5-10x slower than the same core's
  ChaCha20-Poly1305, because ChaCha20 vectorises cleanly into NEON 128-bit
  lanes and Poly1305 needs only 64x64-to-128 multiplies that NEON also
  delivers.
- **Other architectures** (riscv64, ppc64le, mips, s390x): the
  `has_hardware_aes()` probe returns `false` unconditionally. ppc64le has
  AltiVec AES instructions and s390x has CPACF, but neither is wired into
  the Rust feature-detection macros today, so both arches behave like
  "no hardware AES" from oc-rsync's point of view. They fall back to
  whatever OpenSSH chose at build time.

### ChaCha20-Poly1305

- Pure software stream cipher plus polynomial MAC. Designed by D. J.
  Bernstein for systems without dedicated AES silicon. On NEON-capable
  aarch64 (every ARMv8.0-A core, including the ones missing AES) ChaCha20
  vectorises into four parallel 16-lane permutations and Poly1305 reduces in
  64-bit limbs, so a single Cortex-A53 r0 core delivers 250-400 MB/s of
  authenticated-encryption throughput. That is well above the ~120 MB/s
  software AES-GCM number on the same core.
- Constant-time by construction; no cache-timing leakage and no T-tables
  to mis-prefetch. This is why OpenSSH and TLS 1.3 BoringSSL keep it as the
  default on chips without AES instructions.
- No GHASH dependency; no benefit from PMULL even when present.

### Loopback as the fairness floor

Loopback (`lo`) removes NIC offload, MTU, and RTT variance from the
measurement. The remaining cost is exactly: SSH framing, AEAD cipher,
rsync's own delta engine, and disk I/O. A loopback bench is the cleanest
way to expose the cipher cost on a CPU-bound aarch64 part because it
saturates the cipher before it saturates the network.

## Bench plan

### Test matrix

Two payload sizes (100 MiB, 1 GiB), two ciphers (AES-128-GCM, ChaCha20-Poly1305),
two aarch64 hardware classes (with crypto extensions, without), and two
SSH paths (external OpenSSH, embedded russh) for a total of 16 cells.
Iterate each cell with hyperfine `--warmup 2 --runs 10`.

| Axis | Value 1 | Value 2 |
|------|---------|---------|
| Payload | 100 MiB random | 1 GiB random |
| Cipher | `aes128-gcm@openssh.com` | `chacha20-poly1305@openssh.com` |
| CPU AES | Apple M2 / Graviton 3 (extensions present) | Cortex-A53 r0 (extensions absent) |
| SSH path | system `ssh` via `--rsh ssh` | embedded russh via `--rsh oc-rsync-ssh` |

Use `dd if=/dev/urandom` for the payloads and place them on `tmpfs` to keep
disk I/O out of the picture. Random data defeats the rsync delta engine, so
the measurement reflects pure encryption + framing cost.

### Drivers

- `scripts/benchmark_remote.sh` already hosts the SSH-loopback harness and
  wraps hyperfine. Extend it with two new rows: one parameterising
  `OC_RSYNC_FORCE_CIPHER=aes128-gcm@openssh.com` and one with
  `OC_RSYNC_FORCE_CIPHER=chacha20-poly1305@openssh.com`. The flag should
  inject `-o Ciphers=...` rather than rely on `--aes`, so we get a
  deterministic single-cipher negotiation result.
- `scripts/benchmark.sh` and `scripts/benchmark_hyperfine.sh` cover local
  copies and provide a no-SSH baseline.
- For the aarch64-without-extensions cell, run inside the
  `localhost/oc-rsync-bench:latest` container on a Cortex-A53 r0 host
  (Raspberry Pi 3B+ or NanoPi NEO Plus2). Disable AES at the kernel ABI
  level by booting with `clearcpuid=aes` if QEMU is in the loop; otherwise
  rely on the silicon truly lacking the feature. Confirm via
  `cat /proc/cpuinfo | grep Features` (no `aes` token) and via
  `getauxval(AT_HWCAP) & HWCAP_AES == 0`.

### Metrics

For every cell capture:

- **Throughput (MB/s)**: file size divided by hyperfine wall-clock mean.
  Report mean and 95% confidence interval.
- **CPU%**: aggregate CPU time across both `ssh` (or `russh`) processes and
  both `oc-rsync` processes, sampled by `pidstat -h -u 1` for the duration.
  Report mean utilisation; saturation at 100% on a single-core part means
  the cipher is the bottleneck.
- **Latency (first byte)**: time from `connect()` return on the loopback
  socket to the first decrypted application byte at the receiver. Capture
  with `strace -tt -e trace=connect,read` on both ends and post-process
  with `awk`. Latency matters for many-small-files workloads; throughput
  matters for one-large-file workloads.
- **AEAD CPU share**: profile with `perf record -g -p $PID` for 5 seconds
  mid-transfer on each side. Report the share of cycles inside
  `aes_gcm_*` / `chacha20_*` / `poly1305_*` symbols. This is the single
  number that confirms whether the cipher is the cost driver.

### Expected outcomes

These are predictions to falsify. Hardware behaviour wins ties.

- **aarch64 with extensions, AES-128-GCM**: 600-800 MB/s on 1 GiB; CPU
  saturated at one core; AEAD share near 35%.
- **aarch64 with extensions, ChaCha20-Poly1305**: 350-500 MB/s; CPU
  saturated at one core; AEAD share near 60%. AES-GCM wins.
- **aarch64 without extensions, AES-128-GCM**: 80-130 MB/s; CPU saturated;
  AEAD share above 70%.
- **aarch64 without extensions, ChaCha20-Poly1305**: 250-400 MB/s; CPU
  saturated; AEAD share near 60%. ChaCha20 wins by 2-4x.
- **100 MiB cells**: same ratios as 1 GiB but with higher noise; hyperfine
  warmups absorb cold-cache effects.

## Decision criteria

The benchmark is the source of truth, but the decision rule below codifies
how to read the numbers.

1. **AES instructions present on the local CPU**: keep the current
   behaviour. Inject `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com`.
   Hardware AES-GCM beats software ChaCha20 on every modern chip we have
   measured.
2. **AES instructions absent on the local CPU**: do not inject `-c` at all
   for the external-OpenSSH path. OpenSSH defaults to ChaCha20-Poly1305
   first on every build shipped by mainstream distributions, which is the
   right choice. The embedded russh path already reorders ChaCha20 first
   in `default_ciphers()` (`crates/rsync_io/src/ssh/embedded/cipher.rs:51`)
   and needs no change.
3. **AES instructions absent on the remote CPU but present locally**:
   undetectable from the client side at command-build time. Document this
   as a known limitation. Users on heterogeneous fleets can pin
   `-o Ciphers=chacha20-poly1305@openssh.com` via `--rsh "ssh -o ..."`.
4. **User explicitly passes `--aes`**: honour it. The injection logic
   already defers to `prefer_aes_gcm == Some(true)` regardless of the
   hardware probe, so this case requires no change. Document that on a
   chip without crypto extensions `--aes` will be slower than the default.
5. **User explicitly passes `--no-aes`**: honour it. Suppresses injection
   even on hardware-AES hosts. No change.

The third row of the matrix - `--aes` on a chip without extensions - is the
only place the current code is mis-tuned. The fix is to demote `--aes` from
"force" to "prefer when hardware available" by also honouring the local
hardware probe in the `Some(true)` arm. That is a one-line change in
`should_inject_aes_gcm_ciphers`; it is held until the benchmark numbers
above confirm the slowdown is real on representative aarch64 hardware.

### Ship criteria for the follow-up code change

- ChaCha20-Poly1305 beats software AES-GCM by at least 1.5x on the
  Cortex-A53 r0 cell at both 100 MiB and 1 GiB.
- The win persists at the 95% confidence interval across 10 hyperfine runs.
- AEAD CPU share for AES-GCM exceeds 60% on the no-extensions cell, i.e.
  the cipher is unambiguously the bottleneck.

If any of these fail, leave the current force-AES-on-`--aes` semantics
unchanged and revisit.

## Cross-references

- Task #1628: cipher selection audit (parent).
- Task #1629: AES-NI detection caching.
- Task #1630: ChaCha20-Poly1305 fallback policy.
- Task #1631: SSH cipher injection precedence with user `-c` flags.
- Task #1632 (this file): benchmark plan to quantify aarch64 software AES-GCM
  vs ChaCha20-Poly1305 and feed back into the decision rule above.

Related audits in this tree:

- `docs/audits/ssh-cipher-compression.md` - companion catalog of SSH
  pre-spawn detection signals; same code surface (`SshCommand`,
  `should_inject_*` predicates).
- `docs/audits/async-ssh-transport.md` - async transport plan; cipher
  choice is orthogonal but the bench harness reuses the same loopback rig.

Upstream rsync 3.4.1 does not select SSH ciphers; it inherits whatever the
SSH client negotiates. The behaviour catalogued here is an oc-rsync
enhancement and does not affect the wire protocol.

## References

- `crates/rsync_io/src/ssh/builder.rs` - `SshCommand`,
  `set_prefer_aes_gcm`, `should_inject_aes_gcm_ciphers`,
  `options_contain_cipher_flag`, `has_hardware_aes`.
- `crates/rsync_io/src/ssh/embedded/cipher.rs` - `has_aes_ni`,
  `default_ciphers`.
- `crates/core/src/client/remote/ssh_transfer.rs` - `build_ssh_connection`
  call site at line 286.
- `crates/core/src/client/config/builder/mod.rs` - `prefer_aes_gcm` field
  (line 251) and propagation (line 416).
- `crates/cli/src/frontend/arguments/parser/mod.rs` - `--aes` / `--no-aes`
  parsing at line 279.
- `scripts/benchmark_remote.sh`, `scripts/benchmark_hyperfine.sh`,
  `scripts/benchmark.sh` - existing benchmark harness; the cipher matrix
  above slots in alongside the existing rows.
- OpenSSH manual: `ssh(1)` `-c`, `ssh_config(5)` `Ciphers`. Default cipher
  order on Debian/Ubuntu/RHEL/Alpine/Homebrew is documented as
  `chacha20-poly1305@openssh.com,aes128-ctr,aes192-ctr,aes256-ctr,aes128-gcm@openssh.com,aes256-gcm@openssh.com`.
- ARMv8 ARM section A2.3 (FEAT_AES, FEAT_PMULL): the architectural feature
  bits exposed via `ID_AA64ISAR0_EL1` that the kernel surfaces as
  `HWCAP_AES` / `HWCAP_PMULL` on Linux.

Last verified: 2026-05-07 against master at commit 60e83fd96
("chore(ci): remove standalone:delta-stats from KNOWN_FAILURES").
Spot-checked files:
`crates/rsync_io/src/ssh/builder.rs`,
`crates/rsync_io/src/ssh/embedded/cipher.rs`,
`crates/core/src/client/remote/ssh_transfer.rs`,
`crates/cli/src/frontend/arguments/parser/mod.rs`.
