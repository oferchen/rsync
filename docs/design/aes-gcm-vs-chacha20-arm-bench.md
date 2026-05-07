# AES-GCM vs ChaCha20 on ARM Without Hardware AES (#1632)

Focused bench plan for the no-crypto-extension aarch64 case. Companion to
`aes-gcm-vs-chacha20-bench-plan.md`; this note pins the ARM-specific
methodology and decision table.

## 1. Current cipher selection in the SSH path

The OpenSSH-program path injects an AES-GCM cipher list only when the CPU
advertises hardware AES. Selection lives in `rsync_io`:

- Guard: `should_inject_aes_gcm_ciphers` at
  `crates/rsync_io/src/ssh/builder.rs:500`. Four conditions: caller did not
  opt out (`prefer_aes_gcm != Some(false)`), `has_hardware_aes()` is true,
  the program basename is `ssh`/`ssh.exe`, and no `-c` was already supplied.
- Call site at `crates/rsync_io/src/ssh/builder.rs:417` prepends
  `aes128-gcm@openssh.com,aes256-gcm@openssh.com` when the guard fires.
- Detection in `has_hardware_aes()` at
  `crates/rsync_io/src/ssh/builder.rs:601`. On x86/x86_64 it calls
  `is_x86_feature_detected!("aes")` (AES-NI). On aarch64 it calls
  `is_aarch64_feature_detected!("aes")`, which on Linux reads HWCAP_AES
  (sourced from the kernel's parse of `ID_AA64ISAR0_EL1.AES` bits 4..7),
  on macOS/iOS reads `hw.optional.arm.FEAT_AES` via sysctl, and on Windows
  uses `IsProcessorFeaturePresent(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE)`.
  The result is cached in a `OnceLock`. All other architectures return
  `false`.
- Embedded russh path mirrors the same boolean through `default_ciphers()`
  at `crates/rsync_io/src/ssh/embedded/cipher.rs:51`, which orders AES-GCM
  first when hardware AES is present and ChaCha20-Poly1305 first otherwise.

## 2. ARM target classes

| Class | Core | Crypto Ext | `has_hardware_aes` |
|---|---|---|---|
| Apple Silicon (M1-M4) | Firestorm/Avalanche/+ | yes | true |
| Cortex-A55 / A75 / A76 / A77+ | ARMv8.2-A+ | yes | true |
| AWS Graviton 2/3/4 | Neoverse N1/V1/V2 | yes | true |
| Raspberry Pi 4 | Cortex-A72 | no | false |
| Raspberry Pi 3 | Cortex-A53 | no | false |
| Early Graviton (a1) | Cortex-A72 | no | false |
| Cortex-A53 SBC | Cortex-A53 | no | false |

Hardware-AES rows are controls; AES-GCM should win. No-crypto-ext rows
are the targets where ChaCha20 is hypothesised to win.

## 3. Bench plan

Stream a 1 GiB random-filled file end-to-end over loopback SSH, criterion
timing the full `oc-rsync push`. Per-cipher loopback `sshd` configured with
exactly one `Ciphers` line so the negotiated cipher is forced. Driver:

- Bench harness at `crates/rsync_io/benches/ssh_cipher_arm.rs`, gated on
  `embedded-ssh` for the russh control cell. Workspace bench profile is
  already opted in.
- Criterion settings: `Throughput::Bytes(1 << 30)`, `sample_size = 10`,
  `measurement_time = 60s`, `warm_up_time = 3s`, paired benchmark IDs
  `(cipher, payload=1GiB)`.
- Three ciphers under test, exercised explicitly through OpenSSH:
  `aes256-gcm@openssh.com`, `aes128-gcm@openssh.com`,
  `chacha20-poly1305@openssh.com`. Forced via `-e "ssh -c <cipher>"`,
  which short-circuits the auto-injection guard so the cipher under test
  is the dependent variable, not the policy under test.
- Each cell measures encrypt+decrypt round-trip: source on tmpfs, sshd
  on `127.0.0.1:2222`, receiver writes to a second tmpfs. tmpfs keeps
  storage out of the measurement.
- Provenance line at startup records `cpu`, `has_hardware_aes()`,
  `default_ciphers()` first entry, and the `aes` crate's runtime backend
  selection (open question 1 of the companion plan).
- Differential metric: 1 GiB cell minus 256 MiB cell isolates per-byte
  cipher cost from handshake overhead.

## 4. Pass / fail thresholds

- **No-crypto-ext aarch64 (RPi 3, RPi 4, Graviton a1, Cortex-A53 SBC).**
  ChaCha20-Poly1305 must beat the faster AES-GCM variant by 2.0x or more
  on the differential metric. 1.5x to 2.0x = "weak", record and watch.
  Below 1.5x = hypothesis fails, open follow-up to retune defaults.
- **Crypto-ext aarch64 (Apple, RPi 5, Graviton 2+, Cortex-A55+).**
  AES-128-GCM must beat ChaCha20-Poly1305 by 4.0x or more. Below 4.0x =
  "hardware AES regression", investigate whether the russh/`aes` crate
  path engaged the ARMv8 crypto instructions.

Two non-conforming hosts in either direction trigger a policy revisit.
A single host on the boundary does not flip the global default.

## 5. Decision table

| Detected hardware | First cipher | Fallback | Source |
|---|---|---|---|
| x86/x86_64 with AES-NI | `aes128-gcm@openssh.com` | `chacha20-poly1305@openssh.com` | `has_hardware_aes` true |
| aarch64 with Crypto Ext | `aes128-gcm@openssh.com` | `chacha20-poly1305@openssh.com` | `has_hardware_aes` true |
| aarch64 without Crypto Ext | `chacha20-poly1305@openssh.com` | `aes128-gcm@openssh.com` | `has_hardware_aes` false |
| 32-bit ARM, MIPS, RISC-V, PPC | `chacha20-poly1305@openssh.com` | `aes128-gcm@openssh.com` | `has_hardware_aes` false |
| User passed `-c <cipher>` | user-specified | user-specified | guard short-circuits |
| `--no-aes` set | system default | system default | `prefer_aes_gcm = Some(false)` |

This table reflects the current policy. The bench validates the rows in
bold-equivalent positions: the no-crypto-ext aarch64 row is the row under
test, the crypto-ext rows are the controls. A failure in section 4 changes
the table; a pass leaves it as-is and closes #1632 as "policy validated".

## References

- `crates/rsync_io/src/ssh/builder.rs:417` (call site),
  `crates/rsync_io/src/ssh/builder.rs:500`
  (`should_inject_aes_gcm_ciphers`),
  `crates/rsync_io/src/ssh/builder.rs:601` (`has_hardware_aes`).
- `crates/rsync_io/src/ssh/embedded/cipher.rs:19` (`has_aes_ni`),
  `crates/rsync_io/src/ssh/embedded/cipher.rs:51` (`default_ciphers`).
- Companion full plan: `docs/design/aes-gcm-vs-chacha20-bench-plan.md`.
