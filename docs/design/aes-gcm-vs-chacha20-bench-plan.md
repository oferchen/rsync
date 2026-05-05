# AES-GCM vs ChaCha20 Benchmark Plan on ARM Without Hardware AES (#1632)

## Summary

The cipher selection logic in `crates/rsync_io/src/ssh/embedded/cipher.rs`
and the cipher injection guard in `crates/rsync_io/src/ssh/builder.rs`
both branch on a single boolean: does the running CPU advertise hardware
AES? On x86/x86_64 the boolean comes from `is_x86_feature_detected!("aes")`
(AES-NI, completed in #1364, hardened in #1788). On aarch64 it comes
from `is_aarch64_feature_detected!("aes")` (the ARMv8 Cryptography
Extensions, completed in #1628). Everywhere else the boolean is `false`
by construction. The four-condition guard in
`should_inject_aes_gcm_ciphers()` (#1629) consumes that boolean and
either prepends `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com`
to the OpenSSH argv or leaves the system default alone. The embedded
russh path consumes the same boolean through `default_ciphers()`,
which orders AES-GCM first when hardware AES is present and ChaCha20
first when it is absent.

This design note plans the empirical benchmark that justifies the
ChaCha20-first ordering on ARM hosts without hardware AES, and that
gives the project a falsifiable threshold for changing the policy if
the data ever contradicts the assumption. No production code lands in
this PR. The implementation lands in a follow-up tracked off this note.

The audit task #1627 is the parent for this work; #1630 already
verified that the no-hardware-AES code path picks ChaCha20-first on a
software fallback target. The piece this plan adds is an absolute
throughput comparison rather than an ordering assertion.

## 1. Hypothesis

On any aarch64 (or 32-bit ARM) target where
`is_aarch64_feature_detected!("aes")` returns `false`, the
`chacha20-poly1305@openssh.com` cipher will deliver strictly higher
sustained SSH bulk-transfer throughput than `aes128-gcm@openssh.com`
and `aes256-gcm@openssh.com` for payloads in the 1 MiB to 1 GiB range.
Specifically:

- **Throughput.** ChaCha20-Poly1305 throughput exceeds AES-GCM
  throughput by at least 1.5x at the 1 GiB payload size, measured as
  median of 10 runs after a 3-run warmup, on every host class in the
  test matrix.
- **CPU efficiency.** ChaCha20-Poly1305 consumes fewer CPU-cycles per
  transferred byte (`perf stat -e cycles,instructions` on Linux
  hosts) than either AES-GCM variant on the same host.
- **Latency.** ChaCha20-Poly1305 first-byte latency is no worse than
  AES-GCM (we expect parity here; the cost of a russh handshake
  dominates first-byte time).

The expected mechanism: AES without hardware acceleration is a
software bit-sliced or table-driven implementation. Both forms cost
significantly more CPU per byte than ChaCha20's ARX construction,
which maps cleanly to general-purpose registers and is competitive on
ARM cores even without NEON. ChaCha20-Poly1305 is the cipher the IETF
selected as the mobile-friendly alternative to AES-GCM precisely
because of this property.

If the hypothesis holds, the current default ordering in
`default_ciphers()` at `crates/rsync_io/src/ssh/embedded/cipher.rs:51`
is correct and #1632 closes as "policy validated, no change". If the
hypothesis fails on any host class, the pass/fail decision tree in
section 7 governs the policy update.

## 2. Why It Matters

SSH transport is one of the three transport modes oc-rsync supports.
For aarch64 hosts the cipher choice has measurable user impact across
three distinct populations:

- **Apple Silicon (M1, M2, M3, M4) macOS hosts.** All Apple M-series
  cores ship the ARMv8 Cryptography Extensions. On these hosts
  `has_hardware_aes()` at
  `crates/rsync_io/src/ssh/builder.rs:601` returns `true`. AES-GCM
  is the right default. This benchmark must confirm that and rule
  out a regression where M-series ChaCha20 happens to be even faster
  in software (a real possibility on cores with very wide pipelines).
- **Raspberry Pi class hardware.** Raspberry Pi 4 (Cortex-A72) and
  earlier do not ship AES Crypto Extensions. Raspberry Pi 5
  (Cortex-A76) does. Many embedded NAS and CI runner deployments
  pin to RPi 4 class boards. For those hosts `has_aes_ni()` at
  `crates/rsync_io/src/ssh/embedded/cipher.rs:19` returns `false`,
  the current policy picks ChaCha20-first, and the benchmark must
  confirm that picking ChaCha20-first is the correct call on this
  exact CPU.
- **ARM server cores without AES.** AWS Graviton (1, 2, 3, 4) all
  ship AES extensions. But the broader fleet of aarch64 servers
  available from secondary cloud providers includes Cortex-A53
  and Cortex-A55 cores in low-cost SBCs and edge hardware. Tasks
  #1628 and #1788 collectively guarantee that the runtime detection
  is honest on these hosts; #1632 tests that the cipher we route
  these hosts to is the fast one.

A wrong default on a no-hardware-AES ARM host costs the user real
throughput. On a Cortex-A55 at 1.5 GHz, the difference between a
software AES-GCM and a software ChaCha20-Poly1305 implementation
can be 3x to 5x in raw cipher throughput, which on a saturated
gigabit link translates to either staying CPU-bound or going
network-bound. For a 100 GiB transfer that is the difference
between a 15-minute and a 60-minute run.

## 3. Test Matrix

### Host classes

| Class | CPU | Hardware AES | Expected cipher |
|------|-----|--------------|-----------------|
| Apple M1 | aarch64, ARMv8.4-A + Crypto | yes | AES-128-GCM |
| Apple M2 | aarch64, ARMv8.6-A + Crypto | yes | AES-128-GCM |
| Raspberry Pi 4 | Cortex-A72, ARMv8-A no Crypto | no | ChaCha20 |
| Raspberry Pi 5 | Cortex-A76, ARMv8.2-A + Crypto | yes | AES-128-GCM |
| AWS Graviton 2 | Neoverse N1, ARMv8.2-A + Crypto | yes | AES-128-GCM |
| Cortex-A55 SBC | Cortex-A55, ARMv8.2-A no Crypto | no | ChaCha20 |
| Cortex-A53 SBC | Cortex-A53, ARMv8-A no Crypto | no | ChaCha20 |
| x86_64 reference | Intel Skylake or later, AES-NI | yes | AES-128-GCM |

The Apple, Graviton, and x86_64 rows are control hosts. They are
expected to favour AES-GCM and the benchmark must confirm that.
The Pi 4, Cortex-A55, and Cortex-A53 rows are the hosts where the
hypothesis matters.

### Payload sizes

Five payload classes, each as a single file fed to oc-rsync over
SSH:

- 64 KiB - tickles handshake-dominated regime.
- 1 MiB - small bulk transfer.
- 64 MiB - typical photo or document bundle.
- 256 MiB - typical video or archive.
- 1 GiB - sustained bulk transfer where cipher cost dominates.

All payloads are random-filled to defeat ZFS and btrfs
deduplication and to force the compressor (when invoked) into its
worst case. Source data is generated once per host with
`dd if=/dev/urandom` and cached on local disk to keep storage cost
out of the cipher measurement.

### Cipher pairs

Three SSH ciphers under test, applied via explicit `-c` to the SSH
command (which short-circuits the auto-injection guard at
`crates/rsync_io/src/ssh/builder.rs:417`):

- `aes128-gcm@openssh.com`
- `aes256-gcm@openssh.com`
- `chacha20-poly1305@openssh.com`

Each cipher runs against each payload size on each host. That is
8 hosts x 5 payloads x 3 ciphers = 120 cells. Each cell takes the
median of 10 runs after a 3-run warmup. With realistic per-run
times (5 s on small payloads, 60 s on 1 GiB at 200 MB/s) one host
finishes in roughly 90 minutes plus per-payload generation and
warmup overhead.

## 4. Measurement Methodology

### Bench harness location

The benchmark goes in `crates/rsync_io/benches/ssh_cipher.rs`. The
crate already opts in to criterion through the workspace bench
profile but does not have a benches directory yet. Adding one
follows the same pattern as `crates/checksums/benches/`.

The bench file is gated behind the `embedded-ssh` feature so the
russh dependency is only pulled in when this feature is on. The
file imports `default_ciphers()` and `has_aes_ni()` from
`rsync_io::ssh::embedded::cipher` and the builder's
`has_hardware_aes()` so the benchmark runs the same selection
logic the production path runs (requirement of section 6).

### Criterion configuration

Criterion is already a dev-dependency for `checksums`, `cli`,
`core`, `daemon`, `engine`, `fast_io`, `match`, `protocol`, and
`transfer`; we add it to `crates/rsync_io/Cargo.toml` under
`[dev-dependencies]` with the existing `version = "0.8"` and
`features = ["html_reports"]` to keep the workspace consistent.

Group settings:

- `Throughput::Bytes(payload_size)` so criterion reports MB/s.
- `sample_size = 10` for the 256 MiB and 1 GiB cells. Default 100
  for the smaller cells.
- `measurement_time = 60s` for 1 GiB cells, default 5s elsewhere.
- `warm_up_time = 3s` to populate disk caches and warm the JIT-style
  branch predictors on aarch64.
- `BenchmarkId::new(cipher, payload_size)` gives criterion the
  `(cipher, payload)` axis pair so the HTML report renders one
  comparison plot per payload.

### Runtime feature reporting

The bench harness prints a one-line provenance header at startup:

    cpu: aarch64, hardware_aes=false, cipher_default_first=chacha20-poly1305@openssh.com

This line comes from calling `has_aes_ni()` and `default_ciphers()`
directly (see section 6). The CI archive of the bench output uses
this line to bucket results by host class.

### Wire-level transfer driver

Each iteration runs an oc-rsync push of one source file to a
loopback SSH server. The loopback target is a `sshd` instance on
`127.0.0.1:2222` configured with `Ciphers <single-cipher>` so the
server cannot accept anything else. The criterion timer wraps the
whole `Command::new("oc-rsync").args(...).status()` call so
timer measurements include the russh handshake on the embedded
path and the OpenSSH client startup on the OpenSSH path.

## 5. Confound: russh negotiation cost vs raw cipher cost

The single-end-to-end `oc-rsync push` measurement fuses three
costs into one number:

1. SSH key exchange and cipher negotiation (russh client setup,
   russh::client::connect, host-key verification).
2. Authentication (key file read, signature, server reply).
3. Bulk cipher work over the transferred payload.

Cost 1 and cost 2 are payload-independent. Cost 3 scales with
payload size. The hypothesis in section 1 is about cost 3. To
keep the inference clean, the benchmark must report all three
cells per cipher:

- **Total wall time** (the criterion-measured cell).
- **Handshake-only time** computed by running the same SSH command
  with a 0-byte payload and subtracting from the small-payload
  measurement. This is reported as a separate criterion benchmark
  group `ssh_handshake` keyed only by cipher, no payload axis.
- **Throughput at 1 GiB minus 256 MiB** as the differential
  measure of pure cipher cost. Because handshake cost cancels in
  the subtraction, the residual is dominated by per-byte cipher
  work. The pass/fail decision tree in section 7 uses this
  residual rather than the raw 1 GiB number.

Why the differential matters: russh on a Cortex-A55 may take
500 ms to complete its handshake while shovelling 256 MiB through
the cipher takes 1500 ms. Reporting only the wall time would
attribute 500 ms of handshake to the cipher and inflate the
slow-cipher penalty. Reporting the differential isolates the
per-byte cost.

## 6. Reuse of the existing cipher selection path

The benchmark must not duplicate cipher selection logic. The
selection logic is the artefact under test. Concretely, the bench
harness:

- Calls `rsync_io::ssh::embedded::cipher::default_ciphers()` at
  `crates/rsync_io/src/ssh/embedded/cipher.rs:51` to get the
  preference list and asserts the first entry matches the
  hypothesis for the host class.
- Calls `rsync_io::ssh::embedded::cipher::has_aes_ni()` at
  `crates/rsync_io/src/ssh/embedded/cipher.rs:19` to record
  the hardware AES detection result in the provenance line.
- Imports `has_hardware_aes()` from
  `crates/rsync_io/src/ssh/builder.rs:601` (the function is
  `pub(super)` today; we expose it as `pub` only if the bench
  needs it directly, otherwise `default_ciphers()` is sufficient).

For the per-cipher cells, the bench harness invokes oc-rsync with
explicit `-c <cipher>` rather than implicitly trusting the auto
inject. This isolates the cipher under test from the policy under
test. The cipher under test is the dependent variable; the policy
is the hypothesis. Conflating the two would defeat the experiment.

The "policy" cell - one extra benchmark per host that runs
oc-rsync with no `-c` and lets `should_inject_aes_gcm_ciphers()`
at `crates/rsync_io/src/ssh/builder.rs:500` decide - is reported
separately. It must equal the per-cipher cell of whichever cipher
the policy selected. If it does not, the policy and the bench are
not running the same code path and the bench is invalid.

## 7. Pass/fail decision tree

For each no-hardware-AES host class (Pi 4, Cortex-A55,
Cortex-A53):

1. **Compute** the cipher-cost residual described in section 5
   for ChaCha20-Poly1305, AES-128-GCM, and AES-256-GCM.
2. **If** ChaCha20 residual is at least 1.5x faster than the
   faster of the two AES-GCM residuals, the hypothesis holds for
   this host. Record "validated" and move on. No code change.
3. **If** ChaCha20 residual is between 1.0x and 1.5x faster, the
   hypothesis is weakly supported. Record "weak", do not change
   the policy, and add the host to a regression watchlist.
4. **If** ChaCha20 residual is between 0.9x and 1.0x faster (i.e.
   AES-GCM is slightly faster), record "policy boundary" and open
   a follow-up issue to re-bench with a tuned ChaCha20
   implementation. Do not change the policy yet; one host on the
   margin is not enough to flip the default for the whole
   no-hardware-AES population.
5. **If** ChaCha20 residual is more than 10% slower than AES-GCM
   on two or more no-hardware-AES hosts, the hypothesis fails.
   Open a follow-up issue to either:
   - Promote AES-GCM to first place even without hardware AES, or
   - Make the cipher default itself a build-time or runtime knob
     keyed off a richer CPU class enum than the binary
     `has_aes_ni`.
6. **For each hardware-AES host** (M1, M2, RPi 5, Graviton 2,
   x86_64), AES-128-GCM must be at least 1.2x faster than
   ChaCha20. If not, record "hardware AES regression" and open
   a follow-up. The current default ordering with hardware AES
   first is the high-throughput choice; if it is not, something
   is wrong (likely the russh AES-GCM implementation does not
   actually engage the hardware path).

The decision tree explicitly does not flip the policy on any
single host's results. The `default_ciphers()` function is a
global default applied to many hosts; flipping it requires
evidence across at least two CPU families.

## 8. Run instructions and expected duration

### Per-host run

On each target host:

    git clone https://github.com/oc-rsync/oc-rsync && cd oc-rsync
    cargo build --release -p oc-rsync --features embedded-ssh
    cargo bench -p rsync_io --features embedded-ssh --bench ssh_cipher

The bench writes its HTML output to
`target/criterion/ssh_cipher/` and a JSON copy to
`target/criterion/ssh_cipher/<cell>/new/sample.json`. Both are
collected into the run archive.

### Loopback SSH server

The bench requires a loopback `sshd`. On Linux hosts it is
configured via a per-cipher `sshd_config` (one file per cipher,
each with a single `Ciphers` line). On macOS hosts the system
`sshd` is reused with a per-run `Match Address 127.0.0.1` block.
The exact sshd_config snippets and the start-and-tear-down script
live next to the bench file as `tools/bench/ssh-cipher/`.

### Expected duration

Per host, end-to-end:

- 8 cells x 60 s for 1 GiB at 200 MB/s = 8 minutes.
- 8 cells x 15 s for 256 MiB = 2 minutes.
- 24 cells x 5 s for the three smaller payloads = 2 minutes.
- 4 handshake-only cells x 10 runs x 1 s = 0.7 minutes.
- Warmup, criterion overhead, sshd setup or teardown - 5 minutes.

Total per host: roughly 18 to 25 minutes on a fast core, up to
60 minutes on Cortex-A53 class silicon where the 1 GiB cell
saturates at 30 MB/s and runs for 30 s instead of 5 s.

Total across the matrix: 8 hosts at 30 minutes each = 4 hours of
wall time. This is comfortably one CI nightly job, gated behind
a manual workflow_dispatch label so it does not run on every PR.

### Result reporting

Each host's `target/criterion/` tree is rsync'd back to a single
collection host and processed by a small Python script that
emits one CSV per host with columns:

    cpu_model, hardware_aes, payload_bytes, cipher, median_seconds, throughput_mbps

Plus a per-host summary line stating the decision-tree outcome
(validated, weak, boundary, fail) for that host.

## 9. Open questions

1. **russh AES-GCM hardware path on aarch64.** russh 0.60.x
   delegates AES-GCM to the `aes-gcm` crate, which uses the
   `aes` crate. `aes` on aarch64 picks the ARMv8 Crypto path
   through `cpufeatures::new!(aes_token, "aes")` when the
   runtime feature is detected. If `aes` runtime detection
   disagrees with our `is_aarch64_feature_detected!("aes")`,
   a host that `has_aes_ni()` reports `true` could still hit
   the software AES path inside russh. Action: add a one-time
   runtime print of the `aes` backend selection to the bench
   harness to record which backend was chosen per host.
2. **OpenSSH vs russh path coverage.** The auto-inject path at
   `crates/rsync_io/src/ssh/builder.rs:417` only fires when the
   program basename is `ssh` (OpenSSH), not the embedded russh
   path. Both paths share `has_hardware_aes` detection but
   exercise different cipher implementations (OpenSSH C vs
   russh Rust). The bench matrix as specified covers OpenSSH;
   adding a parallel embedded matrix for the 3 no-hardware-AES
   hosts is a recommended follow-up.
3. **AES-256 vs AES-128 choice.** The default list prepends
   `aes128-gcm@openssh.com` then `aes256-gcm@openssh.com`.
   AES-128 is roughly 25% faster than AES-256 in software, 15%
   in hardware. The benchmark covers both. If AES-256 is more
   than 30% slower than AES-128 on hardware-AES hosts, that
   does not change ordering but suggests a doc update.
4. **NEON-accelerated ChaCha20.** The `chacha20` crate uses
   NEON on aarch64 by default. NEON ChaCha20 is roughly 3x
   faster than the scalar ARX path. None of the benchmark hosts
   proposed here lack NEON, but the policy should be documented
   as "no-hardware-AES with NEON" rather than the more general
   "no-hardware-AES" claim.
5. **Filesystem write amplification.** The receiver writes the
   payload to disk inside the criterion timer. On Cortex-A55 SBC
   class, an SD-card-backed filesystem can dominate the 256 MiB
   and 1 GiB cells. The bench writes to tmpfs on Linux and to a
   RAM-disk on macOS to keep storage out of the measurement.
   Open: whether to also emit a storage-backed cell for realism.
6. **Compression interaction.** This bench fixes compression off
   (`--no-compression`) so cipher cost is the only variable. If
   compression CPU dominates cipher CPU on Cortex-A55, cipher
   choice matters less than compression algorithm choice. A
   follow-up bench with `--zc=zstd` and `--zc=zlib` is in scope
   only if the 1 GiB cipher residual on Cortex-A55 turns out
   smaller than the compression residual.
7. **CI host availability.** GitHub-hosted runners are x86_64
   only. The aarch64 cells require self-hosted runners or
   manual runs on physical hardware, gated behind
   workflow_dispatch. Open: whether the project owns the four
   aarch64 hosts (M1, RPi 4, RPi 5, Cortex-A55 SBC) needed to
   make this routine, or whether each run is manual. This is a
   logistics question that gates how often policy gets
   revalidated.

## References

- Cipher selection (embedded path):
  `crates/rsync_io/src/ssh/embedded/cipher.rs:19`
  (`has_aes_ni`),
  `crates/rsync_io/src/ssh/embedded/cipher.rs:51`
  (`default_ciphers`).
- Cipher injection guard (OpenSSH path):
  `crates/rsync_io/src/ssh/builder.rs:417`
  (call site),
  `crates/rsync_io/src/ssh/builder.rs:500`
  (`should_inject_aes_gcm_ciphers`),
  `crates/rsync_io/src/ssh/builder.rs:601`
  (`has_hardware_aes`).
- Tests covering the four-condition guard and the no-hardware
  fallback: `crates/rsync_io/src/ssh/tests.rs:1511`
  (`has_hardware_aes_is_consistent`),
  `crates/rsync_io/src/ssh/tests.rs:1549`
  (`aes_gcm_injection_requires_hardware_aes`),
  `crates/rsync_io/src/ssh/tests.rs:1570`
  (`no_aes_gcm_injection_without_hardware_and_user_cipher`).
- Related tasks: #1364 (auto-detect AES-NI, completed),
  #1627 (audit pending), #1628 (aarch64 detect, completed),
  #1629 (CPU feature detect, completed),
  #1630 (test no-HW path, completed),
  #1631 (docs, completed),
  #1632 (this design),
  #1788 (AES-NI runtime detect, completed).
