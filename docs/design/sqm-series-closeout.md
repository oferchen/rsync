# SQM series close-out criteria (SQM-4.b)

Tracking task: SQM-4.b. Parent: SQM-4. Series: SQM-1 through SQM-4.b.

Predecessor: SQM-4.a (`docs/design/sqpoll-nvme-rebench.md`, PR #5275) -
specifies the bench plan that quantifies the defensive-disable cost.

This document does not change source. It defines the criteria under
which the SQM series is considered complete, enumerates the possible
bench outcomes and their follow-up actions, and specifies the memory
note update to perform at closure.

## 1. Series summary

The SQM series investigated and resolved a correctness hazard where
the io_uring SQPOLL kernel thread races with userspace mmap page-fault
handling on the same basis file descriptor. The outcome is a defensive
SQPOLL disable that is production-safe at the cost of 10-15% estimated
NVMe throughput on affected workloads.

### Task inventory

| Task | Description | Status | Artifact |
|------|-------------|--------|----------|
| SQM-1.a | Reproducer for SQPOLL + mmap race | Done | `crates/fast_io/tests/repro_sqpoll_mmap.rs` |
| SQM-1.b | Symptom documentation | Done | `docs/design/sqpoll-mmap-race-symptoms.md` |
| SQM-1.c | Workaround specification (3 candidates) | Done | `docs/design/sqm-1c-workaround-spec.md` |
| SQM-2.a | Candidate scoring matrix | Done | `docs/design/sqm-2a-workaround-scoring.md` |
| SQM-2.b | Implementation design (mlock + dispatch) | Done | `docs/design/sqm-2b-implementation-design.md` |
| SQM-3 | Implementation - per-basis dispatch with defensive SQPOLL disable | Done | Production code in `crates/fast_io/src/io_uring/config.rs` |
| SQM-4.a | NVMe rebench design | Done | `docs/design/sqpoll-nvme-rebench.md` (PR #5275) |
| SQM-4.b | Close-out criteria | Done | This document |

### Candidate evaluated

Three mitigation candidates were scored (SQM-2.a):

1. **Candidate 1 - `MADV_WILLNEED` prefetch.** Hint-based; kernel may
   ignore under memory pressure. Non-deterministic safety guarantee.
2. **Candidate 2 - `mlock` per-slide basis window.** Deterministic
   (pages pinned, no fault possible). Selected as preferred fix.
3. **Candidate 3 - Per-basis dispatch (disable SQPOLL when mmap active).**
   Conservative fallback. Zero race risk. Costs ~10-15% NVMe throughput.

SQM-3 shipped Candidate 3 as the unconditional default because it
requires zero kernel cooperation and zero `RLIMIT_MEMLOCK` headroom.
Candidate 2 (`mlock`) was designed but deferred pending bench evidence
that the throughput cost of Candidate 3 justifies the `RLIMIT_MEMLOCK`
operational burden.

## 2. Current state

- The defensive SQPOLL disable (Candidate 3) is shipped and
  production-safe. No user-facing incidents or correctness issues.
- SQPOLL is active for all non-mmap workloads (network I/O, file
  writes, metadata ops) - no regression on those paths.
- The 10-15% throughput estimate for mmap-basis workloads is derived
  from architectural analysis (syscall overhead * submission rate),
  not from direct measurement on NVMe hardware.
- SQM-4.a specifies the bench plan. Execution requires Linux NVMe
  hardware meeting the criteria in SQM-4.a section 7.

## 3. Close-out criteria

The SQM series is complete when ALL of the following hold:

| # | Criterion | How verified |
|---|-----------|--------------|
| 1 | SQM-4.a bench executed on qualifying NVMe hardware | Raw data collected per SQM-4.a section 8 |
| 2 | Throughput delta measured with statistical significance (Welch's t-test, alpha = 0.05) | 100-sample Arm A vs 100-sample Arm B |
| 3 | Decision criteria applied (section 4 below) | Outcome documented with raw numbers |
| 4 | Follow-up task filed or series explicitly closed | GitHub issue or close-out annotation |
| 5 | Memory note updated | `project_sqpoll_disabled_with_mmap.md` reflects measured (not estimated) cost |

### Pre-closure without bench hardware

If NVMe bench hardware is unavailable for an extended period (> 30
days from SQM-4.a merge), the series may be closed with a
"provisional accept" status:

- Document that the 10-15% estimate is unverified.
- Keep the defensive disable as the permanent default.
- Mark the memory note with "estimated, not measured" qualifier.
- Close the series. Reopen only if a user reports measurable
  throughput degradation on NVMe + mmap-basis workloads.

## 4. Possible outcomes and follow-up actions

These mirror SQM-4.a section 6, expanded with series-level actions:

### Outcome A: measured delta < 5%

The defensive disable is nearly free on production NVMe hardware.

| Action | Detail |
|--------|--------|
| Series disposition | **Closed - no follow-up.** |
| Code change | None. Candidate 3 remains the permanent default. |
| Memory note | Update cost from "~10-15% estimated" to measured value (e.g., "< 5% measured on Gen3x4 NVMe"). |
| Candidate 2 (mlock) | Archived. Not worth the `RLIMIT_MEMLOCK` operational cost. |
| Candidate 1 (MADV) | Archived. |

### Outcome B: measured delta 5-15%

The defensive disable has a real but bounded cost. Acceptable as the
safe default given the race it prevents.

| Action | Detail |
|--------|--------|
| Series disposition | **Closed - optional follow-up.** |
| Code change | None immediately. |
| Memory note | Update with measured value and "acceptable tradeoff" annotation. |
| Candidate 1 (MADV) | Open a new task (MADV-1) to evaluate `MADV_WILLNEED` prefetch as an additive optimisation. This is an independent optimisation - not a SQM series item. |
| Candidate 2 (mlock) | Remains available if MADV evaluation shows insufficient recovery. |
| Timeline | MADV evaluation is P2 (nice-to-have). Not gating any release. |

### Outcome C: measured delta > 15%

The defensive disable is expensive. The estimated 10-15% was
optimistic or the test hardware exhibits higher SQPOLL benefit than
expected.

| Action | Detail |
|--------|--------|
| Series disposition | **Open - remediation required.** |
| Immediate action | Evaluate Candidate 1 (`MADV_WILLNEED`) as SQM-4.c. |
| If MADV brings delta below 5% | Implement as additive layer, close series. |
| If MADV insufficient | Evaluate Candidate 2 (mlock) on same hardware as SQM-4.d. |
| If neither recovers | Accept the cost; document as a known platform limitation for NVMe + mmap workloads. Close series with measured data. |

### Outcome D: Arm B produces EFAULT or hangs

The race is confirmed real on the test hardware under the bench
workload. This is the strongest possible validation of SQM-3.

| Action | Detail |
|--------|--------|
| Series disposition | **Closed unconditionally.** |
| Code change | None. Defensive disable is mandatory. |
| Memory note | Update with "race confirmed on hardware X; defensive disable is non-negotiable". |
| Throughput delta | Irrelevant - correctness trumps performance. |

## 5. Memory note update

Upon series closure, `project_sqpoll_disabled_with_mmap.md` should be
updated to reflect:

### Fields to update

| Field | Current value | Updated value (template) |
|-------|---------------|--------------------------|
| Throughput cost | "~10-15% estimated" | "X% measured on [hardware description]" or "< 5% measured; effectively free" |
| Series status | Implicitly open (references SMR-3a/b/c) | "SQM series closed [date]. Defensive disable is permanent default." |
| Follow-up | "Future work would benchmark..." | "Bench completed [date], [outcome]. No follow-up required." or "MADV-1 filed for optional recovery." |
| Related notes | References SMR-3a/b/c | Add reference to this close-out doc and SQM-4.a bench results |

### Template for Outcome A/B

```markdown
**Series status (closed [date]):** SQM-1 through SQM-4.b completed.
Defensive SQPOLL disable confirmed as permanent default. Measured
throughput cost: X% on [hardware]. Acceptable tradeoff for race
elimination. No remediation planned.
```

### Template for Outcome C (remediation shipped)

```markdown
**Series status (closed [date]):** SQM-1 through SQM-4.d completed.
Original defensive disable cost was X%. Remediation via
[MADV_WILLNEED/mlock] reduced cost to Y%. Both layers are production
defaults.
```

## 6. Relationship to other series

### SQP (SQPOLL rootless container detection)

Tracked by SQP-1 through SQP-6 (`#3295-#3300`). Concerns the
`CAP_SYS_NICE` requirement for SQPOLL in rootless Podman containers.

- **Intersection:** SQP affects whether SQPOLL is available at all;
  SQM affects whether SQPOLL is *used* when available and mmap is
  active. They are orthogonal in the decision tree:
  1. Is SQPOLL available? (SQP series - capability/container detection)
  2. Is SQPOLL safe? (SQM series - mmap-basis guard)
- **Dependency:** None. SQP can ship independently. SQM-3's guard
  applies after SQP's availability check.
- **Combined effect:** In a rootless container with an mmap'd basis,
  both guards fire (SQP denies SQPOLL due to missing capability,
  SQM would also deny it due to mmap). The SQP check fires first
  and the SQM check is never reached. No conflict.

### IUR (io_uring per-thread rings)

Tracked by `project_io_uring_shared_ring_bottleneck.md`. Concerns the
`Arc<Mutex>` serialization of a single shared ring across parallel
submissions.

- **Intersection:** Per-thread rings would give each thread its own
  SQPOLL configuration. Currently SQM-3's `mmap_basis_active` flag
  is global (per-process). With per-thread rings, the disable could
  be scoped to only the thread handling the mmap'd basis, while other
  threads retain SQPOLL for non-mmap I/O.
- **Dependency:** IUR is a prerequisite for per-thread SQPOLL
  granularity. SQM-3's global disable is the correct conservative
  choice until IUR ships.
- **Future opportunity:** If IUR lands and the SQM-4.a bench shows
  Outcome B or C, per-thread SQPOLL scoping becomes a natural
  recovery path - disable SQPOLL only on the mmap-basis ring while
  keeping it active on the write/network rings.

### IUM (io_uring benefit model)

Tracked by IUM-1 through IUM-4 (`#3186-#3189`). Predicts io_uring
win per use site before building.

- **Intersection:** SQM-4.a's bench produces a data point for the
  benefit model: the measured cost of losing SQPOLL submission mode.
  IUM can consume this as an input parameter for its per-site
  prediction framework.
- **Dependency:** None. IUM is a planning framework; SQM is a
  correctness series that happened to produce performance data.

## 7. Timeline

| Event | Expected date | Notes |
|-------|---------------|-------|
| SQM-4.a design merged | 2026-06-01 | PR #5275 |
| SQM-4.b close-out criteria | 2026-06-01 | This document |
| Bench hardware provisioned | TBD | Requires Linux NVMe host |
| Bench execution | TBD | Depends on hardware availability |
| Series closure | TBD or +30 days (provisional) | Per section 3 criteria |

If the 30-day provisional-close window is reached (2026-07-01) without
bench execution, apply the provisional-accept path from section 3.
