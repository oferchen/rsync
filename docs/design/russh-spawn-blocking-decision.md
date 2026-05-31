# RUSSH-14: Decision Framework - Keep spawn_blocking vs Flip Default to Async-Native

**Tracking:** RUSSH-14
**Status:** Decision framework. Pre-commits the criteria before RUSSH-4..8 bench data arrives.
**Dependencies:** RUSSH-11 (async-native impl), RUSSH-12 (wire-byte parity), RUSSH-4..8 (bench at 64/128/256/512 sessions).
**Feature flag:** `russh-async-native` (Cargo feature on `rsync_io`, default OFF today).

Cross-links: [[project_russh_spawn_blocking_ceiling]], [[project_no_async_threaded_only]], [[project_ssh_push_russh_v062]], [[project_ssh_stderr_socketpair_silent_fallback]].

## 1. Current State

| Aspect | spawn_blocking (default) | async-native (feature-gated) |
|--------|--------------------------|------------------------------|
| Maturity | Battle-tested since v0.6.2 russh migration | Implemented under RUSSH-11; wire-byte parity verified (RUSSH-12) |
| Concurrency ceiling | ~256 sessions per process (2 blocking-pool slots per session) | Low-thousands (zero blocking-pool slots; one OS thread + two tokio tasks per session) |
| Single-stream throughput | Baseline | Expected 5-10% slower due to `blocking_send`/`blocking_recv` per-chunk overhead |
| Runtime selection | `OC_RSYNC_SSH_DISPATCH=spawn_blocking` (default) | `OC_RSYNC_SSH_DISPATCH=async_native` (opt-in) |
| Rollback | N/A (current default) | Env var or one-line code change reverts |

The goal of this document is to define - before benchmark data arrives - the exact conditions under which the default flips from `spawn_blocking` to `async_native`, the conditions under which we keep `spawn_blocking`, and the hybrid option that auto-selects based on observed connection count.

## 2. Evidence Needed: RUSSH-4..8 Bench Numbers

RUSSH-4..8 run the same bench harness (from RUSSH-3, #2806) at escalating concurrent session counts against both dispatch backends on identical hardware.

### 2.1 Metrics collected per run

| Metric | Method |
|--------|--------|
| Per-session throughput (MB/s) | Wall-clock for 100 MiB fixed payload |
| Per-session p99 latency | Time from session start to first byte received |
| Blocking-pool slot count | `RuntimeMetrics::num_blocking_threads()` at 1 Hz |
| OS thread count | `/proc/self/status Threads:` (Linux) or `task_info` (macOS) at 1 Hz |
| Peak RSS | `VmRSS` (Linux) or `mach_task_basic_info` (macOS) |
| Session failure rate | Errors / total sessions |

### 2.2 Session counts

| Task | Concurrent sessions |
|------|-------------------|
| RUSSH-4 | 64 |
| RUSSH-5 | 128 |
| RUSSH-6 | 256 |
| RUSSH-7 | 512 |
| RUSSH-8 | 1024 (stretch) |

Each task reports the full metric table for both `spawn_blocking` and `async_native` backends. The decision matrix below consumes these numbers directly.

## 3. Decision Criteria: When to Flip the Default

The default flips to `async_native` when ALL of the following hold:

### 3.1 High-concurrency win (mandatory)

Async-native must demonstrate a sustained throughput improvement of **> 20% at 128+ concurrent sessions** relative to spawn_blocking at the same session count. This is measured as aggregate MB/s across all sessions completing within a fixed wall-clock window.

Rationale: the blocking-pool ceiling is the architectural motivation. If async-native does not measurably outperform at the session counts where the ceiling binds, the complexity is unjustified for a default flip.

### 3.2 Low-concurrency non-regression (mandatory)

Async-native must not regress single-stream throughput by **more than 15%** at 1-4 concurrent sessions. The 5-10% slowdown from `blocking_send`/`blocking_recv` overhead (projected in RUSSH-9 Section 4) is acceptable.

Rationale: the common deployment is a single SSH transfer or a small handful of concurrent sessions. A > 15% throughput regression at this scale negates the high-concurrency win for the majority of users.

### 3.3 Memory non-regression (mandatory)

Peak RSS at 512 concurrent sessions must not exceed the spawn_blocking baseline by **more than 10%**.

Rationale: the async-native path replaces per-session runtime construction with a shared runtime plus per-session `tokio::sync::mpsc` channels. The shared runtime's task state should not dominate. If RSS grows disproportionately, investigate leaked tasks or oversized channel buffers before flipping.

### 3.4 Wire-byte parity (mandatory, gate from RUSSH-12)

Zero divergences between dispatchers in the golden byte test suite and the full interop harness (`tools/ci/run_interop.sh`). Any single divergence blocks the flip regardless of performance numbers.

### 3.5 Stress stability (mandatory)

The stress test suite (`ssh_async_native_stress.rs`) must pass 100% across 1000 rapid open/close cycles, 50 concurrent goodbye-phase completions, and mid-transfer abort scenarios. Any silent hang, exit-code misrouting, or thread/task leak blocks the flip.

### 3.6 Summary decision table

| Condition | Threshold | Source |
|-----------|-----------|--------|
| Throughput win at 128+ sessions | > 20% | RUSSH-5..8 |
| Throughput regression at 1-4 sessions | < 15% | RUSSH-4 |
| RSS regression at 512 sessions | < 10% | RUSSH-7 |
| Wire-byte parity | Zero divergences | RUSSH-12 |
| Stress stability | Zero failures | RUSSH-11 stress tests |

All five must be green simultaneously. Any one failure blocks the flip.

## 4. Keep Criteria: When to Keep spawn_blocking as Default

The default stays at `spawn_blocking` if ANY of the following hold:

### 4.1 Low-concurrency regression exceeds budget

Async-native regresses throughput at **1-4 sessions by > 15%**. This means the per-chunk channel overhead is too high for the common deployment. The async-native path remains available as an opt-in for high-concurrency daemon deployments, but cannot be the default.

### 4.2 Insufficient high-concurrency gain

Async-native fails to improve throughput by **> 20% at 128+ sessions**. If the win is only 10-15%, the ceiling shift from ~256 to thousands is real, but the per-session throughput difference does not justify changing the default for all users. Keep spawn_blocking as default; document async-native as the recommended dispatch for deployments expecting 100+ concurrent sessions.

### 4.3 Memory regression exceeds budget

RSS at 512 sessions grows by **> 10%** vs spawn_blocking. Investigate shared-runtime task state, channel-buffer sizing, or leaked pump tasks before reconsidering.

### 4.4 Stability failure

Any silent hang, exit-code misrouting, truncated transfer, or thread leak in the stress suite. The async-native path is new; the spawn_blocking path has been stable since v0.6.2. New code must earn trust.

### 4.5 Wire divergence

Any byte-level wire-format difference between dispatchers (RUSSH-12 failure). Protocol correctness is non-negotiable.

## 5. Hybrid Option: Auto-Detect Based on Connection Count

If the data shows that async-native wins above a threshold but regresses below it, a hybrid dispatch mode can auto-select the backend per-session based on the daemon's current connection count.

### 5.1 Design

```rust
// crates/rsync_io/src/ssh/dispatch/config.rs
pub(crate) enum DispatchKind {
    SpawnBlocking,
    #[cfg(feature = "russh-async-native")]
    AsyncNative,
    #[cfg(feature = "russh-async-native")]
    Hybrid { threshold: usize },
}
```

In `Hybrid` mode:
- Sessions 1..threshold use `SpawnBlockingDispatch` (optimal single-stream throughput).
- Sessions threshold+1.. use `AsyncNativeDispatch` (optimal high-concurrency scaling).

The threshold is derived from bench data: the session count at which async-native first matches or exceeds spawn_blocking in per-session throughput. Expected range: 32-128 sessions based on RUSSH-9 projections.

### 5.2 When to adopt hybrid

Adopt hybrid if BOTH conditions hold:
1. Async-native regresses at low concurrency (1-4 sessions) by 5-15% (noticeable but within budget).
2. Async-native wins at high concurrency (128+ sessions) by > 20%.

This combination means the crossover point exists within the measured range. The hybrid default eliminates the regression for small deployments while delivering the scaling win for large ones.

### 5.3 Operator override

The env var `OC_RSYNC_SSH_DISPATCH` continues to override. Values:
- `spawn_blocking` - force spawn_blocking regardless of connection count.
- `async_native` - force async_native regardless of connection count.
- `hybrid` or `hybrid:N` - auto-select with explicit threshold N (default from bench data).

### 5.4 Connection count observation

The daemon already tracks active connections for `--max-connections` admission. The hybrid dispatch queries `ActiveConnectionCount::current()` at session-open time to select the backend. Client-side (non-daemon) SSH always uses spawn_blocking under hybrid mode since it runs at most a handful of sessions.

### 5.5 Risks of hybrid

- Two code paths exercised in production simultaneously increases test surface.
- Threshold tuning is hardware-dependent; a single default threshold may not fit all deployments.
- Debug complexity: operators must know which dispatch a given session used when diagnosing failures.

Mitigation: log the dispatch selection at `TRACE` level per session. Include dispatch kind in error messages and the `[role=version]` trailer.

## 6. Risk Assessment

### 6.1 spawn_blocking: known risks (baseline)

| Risk | Severity | Status |
|------|----------|--------|
| Ceiling at ~256 concurrent sessions | Medium (daemon deployments only) | Known, documented, mitigated by `--max-connections` |
| Blocking-pool exhaustion degrades entire process | High (if hit) | Mitigated by ceiling awareness + operator docs |
| Per-session runtime build/drop cost | Low (~sub-ms) | Not binding below 100 sessions/sec arrival rate |

### 6.2 async-native: known risks (new code)

| Risk | Severity | Mitigation |
|------|----------|------------|
| Goodbye-phase drain failure (truncated transfer) | High | Explicit drain barrier + timeout (RUSSH-9 Section 6.2) |
| Shared runtime task starvation | Medium | Per-session deadline on pump tasks; dedicated thread for transfer pipeline |
| `blocking_send`/`blocking_recv` overhead on hot path | Low-Medium | Measured in RUSSH-4; expected 5-10%; acceptable if within 15% budget |
| Exit-status synthesis divergence from subprocess path | Medium | Round-trip validation in RUSSH-12 + stress tests |
| Panic in pump task surfaces as opaque I/O error | Low | Descriptive error wrapping (RUSSH-11 Section 8.1) |
| Tokio runtime shutdown races on process exit | Low | Graceful shutdown sequence; timeout-then-abort |

### 6.3 Maturity asymmetry

spawn_blocking has been the production default since v0.6.2 (months of deployment history). async-native has passed unit tests, stress tests, and wire-byte parity validation, but has zero production hours. This asymmetry means:

- Even if bench numbers favor async-native, the flip should include a bake window.
- The rollback mechanism must be instantaneous (env var, no rebuild).
- Monitoring for the bake period should track session failure rate, RSS growth over time, and goodbye-phase timeout hits.

## 7. Migration Plan

### 7.1 Phase 1: Benchmark and data collection

1. Run RUSSH-4..8 on representative hardware (the CI bench runner or the `oc-rsync-bench` container).
2. Collect the full metric table (Section 2.1) for both backends at all session counts.
3. Evaluate against the decision matrix (Section 3.6).

### 7.2 Phase 2: Default flip (if criteria met)

1. **Code change:** flip `DispatchConfig::from_env()` default from `SpawnBlocking` to `AsyncNative` (or `Hybrid` if Section 5 applies). One-line change in `crates/rsync_io/src/ssh/dispatch/config.rs`.
2. **Feature promotion:** add `russh-async-native` to the workspace default feature set.
3. **Release:** ship in the next minor version with release notes documenting the change and the `OC_RSYNC_SSH_DISPATCH=spawn_blocking` escape hatch.

### 7.3 Phase 3: Bake window

- Duration: **14 days** from the release containing the flip.
- Monitoring: session failure rate, RSS at steady state, goodbye-phase timeout frequency.
- Success criterion: zero P0/P1 issues filed against the async-native dispatch during the bake window.
- If a P0/P1 is filed: revert the default in a patch release within 24 hours.

### 7.4 Phase 4: Rollback mechanism

Three levels of rollback, in escalating severity:

| Level | Action | Requires rebuild? | Scope |
|-------|--------|-------------------|-------|
| Operator | Set `OC_RSYNC_SSH_DISPATCH=spawn_blocking` | No | Per-process |
| Code | Flip default in `DispatchConfig::from_env()` | Yes (one-line) | Next release |
| Feature | Remove `russh-async-native` from default features | Yes | Full revert to opt-in |

Level 1 is the immediate response; level 2 ships in a patch release if the issue is systemic; level 3 is reserved for fundamental design failures.

### 7.5 Phase 5: Cleanup (post-bake)

After a successful bake window:
- Mark `SpawnBlockingDispatch` as legacy in internal docs.
- Do NOT remove it. The spawn_blocking path remains compiled and selectable via env var indefinitely.
- Consider removing it only after two full release cycles with async-native as default and zero rollback events.

## 8. Impact on Tokio Runtime Configuration

### 8.1 spawn_blocking default (today)

- Blocking thread pool sized at 512 (tokio default). Each session holds 2 slots.
- Per-session `Builder::new_current_thread()` runtime for the russh channel lifecycle.
- No contention between sessions on the shared multi-thread runtime (each has its own current-thread runtime).

### 8.2 async-native default (proposed)

- Blocking thread pool is freed from SSH session load; available for file metadata ops (`copier.rs:184`) and other short-lived blocking tasks.
- A single shared multi-thread runtime hosts all russh I/O pump tasks.
- Worker thread count: 2 (for the client-side shared runtime), or inherited from the daemon's existing runtime. Rationale: pump tasks are I/O-bound, not CPU-bound; 2 workers suffice for thousands of sessions.
- `max_blocking_threads` can be lowered from 512 to a value matching actual non-SSH blocking work (likely 64-128), reducing OS thread overhead at high connection counts.

### 8.3 Configuration knobs

| Env var | Default (spawn_blocking) | Default (async-native) | Purpose |
|---------|--------------------------|------------------------|---------|
| `TOKIO_WORKER_THREADS` | N/A (per-session current-thread) | 2 (shared runtime) | I/O pump concurrency |
| `OC_RSYNC_SSH_CHANNEL_CAP` | N/A | 32 | mpsc channel capacity per direction |
| `OC_RSYNC_SSH_CHUNK_BYTES` | N/A | 32768 | Max bytes per channel send |
| `OC_RSYNC_BLOCKING_THREADS` | 512 (tokio default) | 128 (lowered) | Blocking pool for non-SSH tasks |

### 8.4 Daemon runtime sharing

The async-native dispatch reuses the daemon's existing tokio multi-thread runtime via `Handle::current()` when a runtime is already active. This avoids constructing a second runtime in the daemon process. The daemon's worker thread count (typically `num_cpus`) is sufficient for both the TCP listener and the SSH I/O pump tasks.

## 9. Compatibility: Does Async-Native Change Behavior for Existing Callers?

### 9.1 Public API surface

No change. `SshConnection`, `SshChildHandle`, `SshReader`, `SshWriter` retain identical signatures, trait implementations, and observable behavior (per RUSSH-10 back-compat shim spec).

### 9.2 Wire protocol

No change. RUSSH-12 validates byte-for-byte parity. The dispatch backend is invisible to the remote peer.

### 9.3 Exit codes

No change. `map_child_exit_status()` produces identical `ExitCode` values from both real subprocess `ExitStatus` and synthesized `ExitStatus` (per RUSSH-11 Section 8.4).

### 9.4 Error messages

Minor differences in error message text for connection failures (russh-native error messages vs SSH client stderr). The `ErrorKind` is identical; only the human-readable message differs. Callers that match on `ErrorKind` are unaffected. Callers that parse error message strings (not recommended but possible) may observe changes.

### 9.5 Timing characteristics

The async-native path has marginally different timing characteristics:
- Connection establishment may be faster (no fork/exec overhead).
- Per-chunk throughput may be 5-10% slower (channel overhead).
- Goodbye-phase shutdown may be faster (cooperative drain vs subprocess signal).

None of these change correctness. Callers with hard timing assumptions (e.g., expect connection within N ms) should use the connect timeout, not wall-clock assumptions.

### 9.6 Thread visibility

The async-native path creates named threads (`oc-rsync-ssh-{id}`) visible in `/proc/{pid}/task/`. The spawn_blocking path creates unnamed threads in the tokio blocking pool. Operators monitoring thread names will see different names. This is cosmetic and does not affect behavior.

## 10. Decision Timeline

| Milestone | Dependency | Expected |
|-----------|------------|----------|
| RUSSH-4..8 bench data collected | Hardware availability | Pending |
| Decision matrix evaluated | Bench data | Within 1 week of data |
| Default flip PR (if approved) | Decision + code review | Within 1 week of decision |
| Bake window start | Flip PR merged + release | Release day |
| Bake window end | 14 days clean | Release day + 14 |
| Confirmed stable | Zero P0/P1 in bake | Day 15 |

## 11. Cross-Links

- RUSSH-9 (#2812) - [`russh-async-native-path.md`](./russh-async-native-path.md) - parent architecture design.
- RUSSH-10 (#2813) - [`russh-async-native-back-compat-shim.md`](./russh-async-native-back-compat-shim.md) - back-compat shim spec.
- RUSSH-11 (#2814) - [`russh-11-async-native-impl.md`](./russh-11-async-native-impl.md) - implementation spec.
- RUSSH-12 (#2815) - wire-byte parity validation.
- RUSSH-3 (#2806) - N-concurrent-sessions bench harness.
- RUSSH-4..8 (#2807-#2811) - baseline + escalating session count benchmarks.
- [[project_russh_spawn_blocking_ceiling]] - root bottleneck motivating async-native.
- [[project_no_async_threaded_only]] - constraint: transfer pipeline stays threaded.
- [[project_ssh_push_russh_v062]] - prior russh migration (v0.6.2).
- [[project_daemon_10k_conn_ceiling]] - thread-per-connection ceiling context.
- [`ssh-async-default-linux.md`](./ssh-async-default-linux.md) - earlier Linux-first async default design (superseded for the dispatch layer by this framework).
