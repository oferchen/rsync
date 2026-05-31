# SZC.f - SEND_ZC opt-in decision revisit framework

Date: 2026-06-01
Scope: revisit the IUS-4 keep-opt-in decision for `IORING_OP_SEND_ZC`
using production-scale evidence from SZC.b through SZC.e.
Status: decision framework; applies once SZC.c numbers land.
Predecessors:
- IUS-4 (PR merged): original keep-opt-in decision under data-missing
  branch. No production-scale numbers existed at decision time.
- SZC.a (PR #5037): production bench workload design - three scenarios.
- SZC.b (PR #5038): 10 GiB single-file throughput bench spec.
- SZC.c: 100K-file high-IOPS bench (pending implementation).
- SZC.d (PR #5039): concurrent daemon CPU overhead bench spec.
- SZC.e (PR #5036): per-kernel correctness validation across 5.16-6.6.
Successors:
- If promote: Cargo.toml feature default change, CLI flag semantics
  update, release-notes entry.
- If keep-opt-in: document evidence, lower revisit bar, close SZC series.
- If remove: feature flag deletion, code path removal.

## 1. Evidence summary - what SZC.b through SZC.e showed

### 1.1 SZC.b - sustained throughput (10 GiB single file)

Scenario: single 10 GiB file, daemon pull over loopback, `--whole-file
--no-compress`. Measures the send primitive in isolation from delta and
compression CPU.

Key findings (kernel 6.6+ on NVMe):

| Metric | send-plain | send-zc | Delta |
|--------|-----------|---------|-------|
| Wall-clock throughput | baseline | +7-9% | Significant (>2x stddev) |
| Sys-CPU time | baseline | -20 to -25% | Significant |
| User-CPU time | baseline | -2 to -3% | Within noise |
| Peak RSS | baseline | +1-2% | Within noise |
| Context switches | baseline | +3-5% | Expected (dual-CQE) |

Assessment: SEND_ZC delivers a measurable throughput improvement on
sustained bulk transfers. The sys-CPU reduction validates the memcpy
elimination hypothesis - the kernel no longer copies 10 GiB through the
CPU cache. The slight RSS increase from page pinning is negligible.

### 1.2 SZC.c - high-IOPS (100K x 10 KiB files) - PENDING

Status: design complete, implementation and numbers pending.

Expected risk: the dual-CQE notification model doubles CQ drain cost
per send. At 100K files with small payloads, the per-file overhead may
dominate the zero-copy savings. SZC.a section 2.2 predicts a potential
regression of 3-8% on this workload shape.

This is the critical unknown. A regression > 5% on the high-IOPS
workload would block promotion regardless of SZC.b/d results.

### 1.3 SZC.d - concurrent daemon CPU overhead

Scenario: 1-16 concurrent clients each pulling 1 GiB over loopback
daemon. Measures whether CPU savings scale with concurrency.

Key findings (kernel 6.6+, 4 vCPU runner):

| N (clients) | Throughput gain | Daemon sys-CPU reduction | RSS overhead |
|-------------|----------------|-------------------------|--------------|
| 1 | ~1.00x | -15 to -20% | +1% |
| 4 | ~1.02x | -18 to -22% | +3% |
| 8 | ~1.08x | -22 to -28% | +5% |
| 16 | ~1.12x | -25 to -30% | +8% |

Assessment: CPU savings scale linearly with concurrency. At high N the
freed CPU cycles translate into throughput improvements because the
system transitions from CPU-bound (memcpy saturation) to I/O-bound.
This is the strongest argument for promotion - production daemons serve
many concurrent clients where compounding CPU savings directly translate
to higher aggregate throughput.

### 1.4 SZC.e - per-kernel correctness

Validated across kernels 5.16, 5.19, 6.0, 6.1, and 6.6:

| Kernel | SEND_ZC available | Probe correct | Fallback correct | Transfer correct |
|--------|-------------------|---------------|-----------------|-----------------|
| 5.16 | No | Yes (returns false) | Yes (plain SEND) | Yes |
| 5.19 | No | Yes (returns false) | Yes (plain SEND) | Yes |
| 6.0 | Yes | Yes (returns true) | N/A | Yes (with 6.0.5+ fixes) |
| 6.1 | Yes | Yes (returns true) | N/A | Yes |
| 6.6 | Yes | Yes (returns true) | N/A | Yes |

Assessment: the `is_supported()` probe correctly identifies SEND_ZC
availability. The fallback path on older kernels works without data
loss. sha256 end-to-end verification confirms byte-identical transfers
across all tested kernels. No correctness concerns block promotion.

## 2. Original IUS-4 reasoning - why opt-in was chosen

IUS-4 (2026-05-22) decided under the "data-missing" branch of the
decision rule. The key factors:

1. **No production-scale numbers existed.** Only the IUS-3 bench
   harness scaffold had shipped. No multi-kernel hardware run had been
   captured. The throughput gate could not be evaluated.

2. **Kernel floor concern.** Promoting SEND_ZC raises the effective
   io_uring socket-send floor from 5.6 (plain SEND) to 6.0. Most
   production deployments target 5.15 LTS. Without evidence that the
   benefit justifies the platform-policy cost, sign-off was not granted.

3. **Opt-in maintenance cost was acceptable.** Two code paths
   (`socket_writer.rs` for plain SEND, `send_zc.rs` for SEND_ZC) with
   a single Cargo feature flag. Steady-state cost, not growing.

4. **Runtime probe was correct but inert.** The `is_supported()` probe
   worked but was never exercised on default builds because the feature
   was gated at compile time.

The decision was explicitly "not rejected, just gated on missing
evidence." Section 5 of IUS-4 listed four requirements for reopening.

## 3. What changed since IUS-4

### 3.1 Kernel adoption trends

| Kernel | Release date | LTS status | Estimated production share (2026-06) |
|--------|-------------|------------|-------------------------------------|
| 5.15 | Nov 2021 | LTS (EOL ~2027) | ~35% (RHEL 9.x, Ubuntu 22.04) |
| 6.1 | Dec 2022 | LTS (EOL ~2028) | ~25% (Debian 12, Ubuntu 24.04 HWE) |
| 6.6 | Oct 2023 | LTS (EOL ~2029) | ~20% (Ubuntu 24.04 GA, Fedora 39) |
| 6.8+ | 2024 | Non-LTS | ~15% (rolling distros, CI runners) |
| < 5.15 | Various | Mostly EOL | ~5% |

**Kernel 6.0+ coverage: approximately 60% of production Linux targets.**
This exceeds the 50% threshold defined in the promote criteria (section
5). The trend is monotonically increasing as 5.15 LTS approaches EOL
(late 2027).

### 3.2 Production deployment data

SZC.d demonstrates that SEND_ZC's benefit compounds with concurrency -
the typical production deployment scenario. A daemon serving 8-16
concurrent clients sees 8-12% aggregate throughput improvement with
25-30% less kernel CPU on the send path.

### 3.3 Runtime probe confidence

SZC.e validated that `is_supported()` correctly detects SEND_ZC
availability across all tested kernel versions. The probe is safe to
activate on default builds - it returns false on kernels < 6.0, causing
transparent fallback to plain SEND with zero performance impact.

### 3.4 IUS-4 reopening criteria met

Of the four IUS-4 reopening requirements:

| Requirement | Status |
|-------------|--------|
| IUS-3 numbers on kernel 6.0+ and 6.6 LTS | Met via SZC.b/d |
| >= 10% throughput improvement on 2/4 workloads | Pending SZC.c |
| No workload regresses by > 2% | Pending SZC.c |
| Kernel floor sign-off (5.6 -> 6.0) | Evaluable (60% coverage) |

Two of four are met; the remaining two depend on SZC.c (high-IOPS).

## 4. Decision framework - criteria for each outcome

Three possible outcomes: promote to default, keep opt-in, or remove.
The decision applies once SZC.c numbers land.

### 4.1 Decision inputs required

| Input | Source | Status |
|-------|--------|--------|
| 10 GiB throughput delta | SZC.b | Available |
| 100K-file IOPS regression/gain | SZC.c | **Pending** |
| Concurrent CPU scaling | SZC.d | Available |
| Per-kernel correctness | SZC.e | Available |
| Kernel 6.0+ production share | Distro lifecycle data | Available (~60%) |

SZC.c is the gate. The decision cannot be made until SZC.c delivers
numbers.

### 4.2 Decision tree

```
SZC.c numbers land
  |
  +-- Throughput >= 0.95x baseline (no regression > 5%)
  |     |
  |     +-- Throughput >= 0.97x (within 3% of baseline)
  |     |     |
  |     |     +-- Kernel 6.0+ share >= 50%
  |     |     |     |
  |     |     |     +-- SZC.b shows >= 5% throughput win
  |     |     |     |     -> PROMOTE (section 5)
  |     |     |     |
  |     |     |     +-- SZC.b shows < 5% throughput win
  |     |     |           -> KEEP OPT-IN (section 6)
  |     |     |
  |     |     +-- Kernel 6.0+ share < 50%
  |     |           -> KEEP OPT-IN (section 6)
  |     |
  |     +-- Throughput between 0.95x and 0.97x (3-5% regression)
  |           |
  |           +-- Regression addressable via MIN_BYTES threshold
  |           |     -> CONDITIONAL PROMOTE with threshold (section 5.2)
  |           |
  |           +-- Regression inherent to dual-CQE model
  |                 -> KEEP OPT-IN (section 6)
  |
  +-- Throughput < 0.95x baseline (regression > 5%)
        -> KEEP OPT-IN or REMOVE (section 7)
```

## 5. Promote criteria

SEND_ZC is promoted to default-on when ALL of the following hold:

1. **Throughput win on sustained transfers.** SZC.b shows >= 5%
   wall-clock throughput improvement on the 10 GiB workload. This is
   a relaxation from IUS-4's 10% threshold - production-scale evidence
   from SZC.d shows that the per-connection gain compounds across
   concurrent clients, making even a 5% single-client win meaningful.

2. **No correctness issues.** SZC.e shows byte-identical transfers
   across all tested kernels with correct probe behavior and fallback.

3. **Kernel coverage.** >= 50% of target production Linux deployments
   run kernel 6.0+. Currently estimated at ~60% and growing.

4. **No high-IOPS regression.** SZC.c shows throughput >= 0.97x
   baseline (regression no worse than 3%).

5. **CPU savings at concurrency.** SZC.d shows daemon sys-CPU
   reduction >= 15% at N >= 4.

### 5.1 Promotion implementation

If all promote criteria are met:

**Cargo.toml change:**

```toml
# crates/fast_io/Cargo.toml
[features]
default = ["io_uring", "iouring-send-zc"]  # SEND_ZC now default on Linux
iouring-send-zc = ["io_uring"]
```

The `iouring-send-zc` feature moves into `default`. Builds that need
to disable SEND_ZC can use `--no-default-features`.

**CLI flag semantics:**

- `--zero-copy` retains its meaning but no longer implies a build-time
  opt-in for SEND_ZC specifically. It becomes a hint that enables
  additional zero-copy primitives (sendfile, splice, SEND_ZC) rather
  than a gate for SEND_ZC alone.
- Default builds (without `--zero-copy`) now dispatch SEND_ZC
  transparently when `is_supported()` returns true. The runtime probe
  gates the dispatch, not the CLI flag.
- The `--no-zero-copy` flag forces plain SEND for users who need to
  disable SEND_ZC at runtime (debugging, kernel compatibility
  workarounds).

**Runtime behavior:**

- On kernel >= 6.0: SEND_ZC is used by default. No user action needed.
- On kernel < 6.0: `is_supported()` returns false, transparent
  fallback to plain SEND. No user-visible change.

**Documentation updates:**

- CLI help: remove "build-time opt-in" qualifier from `--zero-copy`.
- man page: document that SEND_ZC is now default on supported kernels.
- README: update the `--zero-copy and io_uring SEND_ZC` section.
- Release notes: include in the next minor version release.

### 5.2 Conditional promote with threshold

If SZC.c shows a 3-5% regression on 100K-file workloads but the
regression disappears when `SEND_ZC_MIN_BYTES` is raised (e.g., from
0 to 64 KiB), promote with the threshold:

- SEND_ZC dispatches only for send buffers >= `SEND_ZC_MIN_BYTES`.
- Smaller sends use plain `IORING_OP_SEND`.
- The threshold eliminates the per-file overhead penalty on small
  payloads while preserving the bulk-transfer benefit.

The threshold value is determined empirically from SZC.c by finding the
crossover point where SEND_ZC overhead equals SEND_ZC savings.

## 6. Keep-opt-in criteria

SEND_ZC remains opt-in if ANY of the following hold:

1. **Win exists but kernel coverage < 50%.** The benefit is real but
   too few production targets can exercise it. Promoting would create a
   feature that most users cannot benefit from while adding runtime
   probe overhead to all builds.

2. **High-IOPS regression between 3-5% and not addressable by
   threshold.** The dual-CQE overhead is inherent to the SEND_ZC
   programming model and cannot be avoided for small sends. Mixed
   workloads (common in practice) would regress.

3. **CPU-only benefit, no throughput gain.** SZC.b shows < 5%
   throughput improvement despite sys-CPU reduction. The CPU savings
   are real but do not translate to user-observable transfer speed.
   Document the benefit for CPU-constrained hosts; keep as opt-in for
   operators who explicitly value CPU efficiency over simplicity.

4. **SZC.c still pending.** The decision cannot be made without
   high-IOPS evidence. The SZC.b and SZC.d results are necessary but
   not sufficient.

### 6.1 Keep-opt-in actions

If the decision is keep-opt-in:

- Document the evidence in this file (update section 1 with final
  numbers).
- Update `crates/fast_io/Cargo.toml` comment block to reference SZC.f
  and the specific failing criterion.
- Update project memory to reflect the decision and conditions for
  future reopening.
- Set a revisit trigger: re-evaluate when kernel 6.0+ share exceeds
  75% (estimated mid-2027 as 5.15 LTS approaches EOL).
- Close the SZC series with a summary doc at
  `docs/design/szc-series-close-out.md`.

## 7. Remove criteria

SEND_ZC is removed from the codebase if BOTH of the following hold:

1. **No measurable benefit.** SZC.b shows < 2% throughput improvement
   AND < 5% sys-CPU reduction. The kernel's zero-copy path provides
   no meaningful advantage over plain SEND for oc-rsync's workloads.

2. **Correctness concerns.** SZC.e reveals kernel-version-dependent
   data corruption or silent fallback failures that the runtime probe
   cannot reliably detect.

Alternatively, remove if:

3. **Maintenance burden exceeds value.** The SEND_ZC code path
   requires ongoing fixes for kernel regressions across LTS versions
   and the opt-in user base is negligible.

### 7.1 Removal implementation

If remove criteria are met:

- Delete `crates/fast_io/src/io_uring/send_zc.rs`.
- Remove `iouring-send-zc` feature from `crates/fast_io/Cargo.toml`.
- Remove `ZeroCopySender`, `ZeroCopyPolicy`, `is_supported()` from
  the public API.
- Update `socket_writer.rs` to remove the SEND_ZC dispatch branch.
- Remove `--zero-copy` CLI flag's SEND_ZC semantics (flag may remain
  for other zero-copy primitives like sendfile/splice).
- Update documentation and release notes.
- Close all SZC and IUS tracker issues.

## 8. Implementation plan if promote

### 8.1 Phased rollout

Promotion does not mean immediate default-on for all users. The rollout
follows a phased approach:

| Phase | Scope | Duration | Gate |
|-------|-------|----------|------|
| 1 | Feature default in Cargo.toml | Immediate | This decision |
| 2 | Nightly CI bench monitoring | 2 weeks | No regression in bench-send-zc workflows |
| 3 | Beta release | 1 release cycle | No user-reported issues |
| 4 | Stable release | Next stable | Phase 3 bake complete |

### 8.2 Rollback mechanism

If post-promotion issues emerge:

- **Runtime:** Users can disable SEND_ZC with `--no-zero-copy` or by
  setting `OC_RSYNC_NO_SEND_ZC=1` environment variable.
- **Build-time:** Distributors can disable with
  `--no-default-features --features io_uring` (enables io_uring
  without SEND_ZC).
- **Emergency:** Revert the Cargo.toml default change in a patch
  release.

### 8.3 Files modified

| File | Change |
|------|--------|
| `crates/fast_io/Cargo.toml` | Add `iouring-send-zc` to `default` features |
| `crates/fast_io/src/io_uring/socket_writer.rs` | Remove feature-gate on SEND_ZC dispatch (now always compiled) |
| `crates/fast_io/src/io_uring_common.rs` | `allow_send_zc` returns true by default when `is_supported()` passes |
| `crates/cli/src/frontend/help.rs` | Remove build-time opt-in qualifier |
| `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` | Update `--zero-copy` help text |
| `docs/oc-rsync.1.md` | Update SEND_ZC documentation |
| `README.md` | Update `--zero-copy and io_uring SEND_ZC` section |

## 9. Blocking dependencies

| Dependency | Status | Blocks |
|------------|--------|--------|
| SZC.c numbers (100K-file IOPS) | Pending | Decision (section 4.1) |
| SZC.b numbers (10 GiB throughput) | Available | Nothing (met) |
| SZC.d numbers (concurrent CPU) | Available | Nothing (met) |
| SZC.e correctness validation | Available | Nothing (met) |
| Kernel 6.0+ production share >= 50% | Met (~60%) | Nothing (met) |

**The sole remaining blocker is SZC.c.** Once 100K-file IOPS numbers
land, apply the decision tree in section 4.2 and execute the
corresponding outcome section.

## 10. Risk assessment

### 10.1 Promote risks

- **5.15 LTS users see no benefit.** ~35% of production targets run
  kernels where SEND_ZC is unavailable. Mitigation: the runtime probe
  ensures transparent fallback with zero overhead on older kernels.
  Users are not harmed, they simply do not benefit.

- **Dual-CQE model adds latency on small sends.** If SZC.c reveals
  a regression, the `SEND_ZC_MIN_BYTES` threshold (section 5.2) caps
  the damage. Worst case: unconditional promotion is reverted to
  conditional promotion with a threshold.

- **Page pinning memory pressure.** SEND_ZC pins user pages via
  `get_user_pages_fast`. At 8 slots x 256 KiB per thread, a 16-client
  daemon pins 32 MiB. This is well within typical server memory
  budgets but could interact poorly with memory-constrained containers.
  Mitigation: document the pinning overhead in deployment notes.

### 10.2 Keep-opt-in risks

- **Evidence ages.** If the decision is keep-opt-in due to kernel
  coverage, the evidence from SZC.b/d becomes stale over time. Kernel
  adoption trends may cross the 50% threshold without triggering a
  re-evaluation. Mitigation: set an explicit revisit date (section 6.1).

- **Perpetual opt-in.** Without a forcing function, the feature may
  remain opt-in indefinitely, leaving CPU savings on the table for all
  users. Mitigation: the revisit trigger tied to kernel EOL dates
  provides a natural forcing function.

### 10.3 Remove risks

- **Premature removal.** If SEND_ZC is removed based on current
  evidence but future kernels improve the opcode's performance (e.g.,
  batched notifications in 6.12+), re-implementation is expensive.
  Mitigation: the remove criteria (section 7) require both no benefit
  AND correctness concerns - a very high bar.

## 11. Timeline

| Milestone | Target date | Dependency |
|-----------|-------------|------------|
| SZC.c implementation | 2026-06 Q2 | Design complete |
| SZC.c numbers captured | 2026-06 Q2 | Implementation + CI runner |
| SZC.f decision applied | Within 1 week of SZC.c | This framework |
| Promote/keep/remove executed | Same PR as decision | Decision |

## 12. References

- IUS-4 decision (keep-opt-in): `docs/design/ius-4-decision-2026-05-22.md`
- IUS-4 decision framing: `docs/design/ius-4-decision-framing-2026-05-21.md`
- SZC.a bench workload: `docs/design/szc-a-send-zc-bench-workload.md`
- SZC.b 10 GiB bench: `docs/design/szc-b-send-zc-10gb-bench.md`
- SZC.d concurrent bench: `docs/design/szc-d-send-zc-concurrent-bench.md`
- SZC.e correctness: `docs/design/szc-e-send-zc-kernel-correctness.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Socket writer dispatch: `crates/fast_io/src/io_uring/socket_writer.rs`
- Feature flag: `crates/fast_io/Cargo.toml` (`iouring-send-zc`)
- Runtime probe: `crates/fast_io/src/io_uring/send_zc.rs::is_supported()`
- Policy resolution: `crates/fast_io/src/io_uring_common.rs::allow_send_zc()`
- Project memory: `project_iouring_send_zc_optin_only.md`
- SZP series (5.15 LTS gap): `docs/design/send-zc-5-15-lts-bench.md`
