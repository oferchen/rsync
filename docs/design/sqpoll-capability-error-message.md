# SQP-3: Explicit error message for SQPOLL capability failure

Status: design spec.

Tracking: SQP-3 (#3297). Parent tracker: SQP-1..6 (#3295-#3300).
Related: IKV-F.2 (io_uring restriction detection in `crates/fast_io/src/status.rs`).

## 1. Problem statement

When SQPOLL setup fails due to missing `CAP_SYS_NICE`, the current code
silently falls back to a regular io_uring ring. The only observable signal
is:

1. The process-wide `SQPOLL_FALLBACK` atomic is set to `true`.
2. A `debug_log!(Io, 1, ...)` fires in `build_ring()` for the mmap-basis
   defensive refusal path - but NOT for the `EPERM` path from the kernel.
3. `--io-uring-status` reports `sqpoll fell back: yes (CAP_SYS_NICE likely
   missing)` - but only if the user explicitly requests the capability
   matrix.

There is no warning or info-level message at the point of failure. An
operator deploying oc-rsync in a container with `--io-uring=enabled`
(expecting SQPOLL for its throughput benefit) gets the fallback silently
and has no indication until they query `--io-uring-status` after the fact.

## 2. Current code path

File: `crates/fast_io/src/io_uring/config.rs`, lines 357-386.

```rust
impl IoUringConfig {
    pub(crate) fn build_ring(&self) -> io::Result<RawIoUring> {
        // ... mmap_basis_active check (has a debug_log on refusal) ...

        if sqpoll_safe {
            let mut builder = io_uring::IoUring::builder();
            builder.setup_sqpoll(self.sqpoll_idle_ms);
            match builder.build(self.sq_entries) {
                Ok(ring) => return Ok(ring),
                Err(_) => {
                    // SQPOLL requires CAP_SYS_NICE on most kernels. Record
                    // the fallback so callers can surface it in diagnostics.
                    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
                    // <-- NO log message here currently
                }
            }
        }
        RawIoUring::new(self.sq_entries)
            .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
    }
}
```

The `EPERM` branch sets the fallback flag but emits no log line. The only
post-hoc diagnostic is `sqpoll_fell_back()` consumed by
`io_uring_capability_matrix()` in `status.rs`.

## 3. Relationship to IKV-F.2

IKV-F.2 (implemented in `crates/fast_io/src/status.rs`) provides the
`IoUringRestriction` enum and `detect_io_uring_restriction()`. It covers
the case where io_uring *itself* is blocked (seccomp, container, old
kernel) - **not** the case where io_uring works but SQPOLL specifically
is denied.

IKV-F.2 answers: "Is io_uring available at all?"
SQP-3 answers: "Is the SQPOLL performance tier available within io_uring?"

These are complementary, not overlapping. IKV-F.2 does not log anything
about SQPOLL; it only surfaces io_uring base availability. SQP-3 adds
the SQPOLL-specific diagnostic.

## 4. Design

### 4.1 When to emit the message

| Scenario | Policy | Action |
|----------|--------|--------|
| `--io-uring=auto` + SQPOLL fails | Auto | `info!` level log once |
| `--io-uring=enabled` + SQPOLL fails | Explicit | `warn!` level log once |
| `--io-uring=disabled` | N/A | No SQPOLL attempt; no message |
| SQPOLL succeeds | Any | No message (happy path) |
| mmap-basis defensive refusal | Any | Existing `debug_log!` unchanged |

The log level escalation under `Enabled` policy reflects the operator's
explicit intent: they asked for the fast path and did not get it.

### 4.2 Message content

For the `Auto` (info) case:

```
io_uring: SQPOLL setup failed (EPERM) - falling back to regular submission.
  Hint: grant CAP_SYS_NICE to enable kernel-side SQ polling.
  Container: podman run --cap-add=SYS_NICE ...
  Kernel: SQPOLL unprivileged since Linux 5.13; current kernel is {major}.{minor}.
```

For the `Enabled` (warn) case:

```
io_uring: SQPOLL setup failed (EPERM) - falling back to regular submission.
  --io-uring=enabled was requested but SQPOLL could not be activated.
  Hint: grant CAP_SYS_NICE to enable kernel-side SQ polling.
  Container: podman run --cap-add=SYS_NICE ...
  Kernel: SQPOLL unprivileged since Linux 5.13; current kernel is {major}.{minor}.
```

Key design choices:

- **Single emission**: use a `static AtomicBool` (like the existing
  `DOWNGRADE_WARNED` in `sqpoll_basis.rs`) to fire only once per process.
  Repeated ring builds (session pool, long-lived daemon) must not flood
  the log.
- **Include errno**: the actual errno from `builder.build()` is surfaced
  so operators can distinguish `EPERM` (capability) from `ENOMEM`
  (resource exhaustion) or other unexpected failures.
- **Actionable hint**: the `podman run --cap-add=SYS_NICE` command is the
  exact fix for the most common deployment (rootless container). Docker
  equivalent: `docker run --cap-add=SYS_NICE`.
- **Kernel version context**: SQPOLL became unprivileged in Linux 5.13.
  On 5.13+ `EPERM` is unexpected and may indicate seccomp restriction
  rather than missing capability. The message includes the running kernel
  version so the operator can distinguish the two cases.

### 4.3 Integration with `--io-uring-status`

The `io_uring_capability_matrix()` function in `status.rs` already
reports `sqpoll fell back: yes (CAP_SYS_NICE likely missing)`. SQP-3
enhances this line to also include:

- The actual errno that caused the fallback (stored alongside the bool).
- Whether the kernel is >= 5.13 (SQPOLL should be unprivileged).
- The `--cap-add` hint inline.

This makes `--io-uring-status` a self-contained diagnostic without
requiring the operator to cross-reference kernel docs.

### 4.4 Implementation location

The message emission belongs inside `build_ring()` at the existing
`SQPOLL_FALLBACK.store(true, ...)` site. The function needs:

1. Access to the `IoUringPolicy` in effect (to choose info vs warn).
   This requires threading the policy through `IoUringConfig` (it already
   has `sqpoll: bool`; adding `policy: IoUringPolicy` is a field
   addition).

2. Access to the kernel version for the hint. The cached
   `check_io_uring_reason()` result provides this - it is already called
   once per process and the major/minor are available.

3. A `static SQPOLL_EPERM_WARNED: AtomicBool` to gate single-fire
   emission.

### 4.5 Proposed API change to `IoUringConfig`

```rust
pub struct IoUringConfig {
    // ... existing fields ...

    /// The operator's I/O policy (Auto/Enabled/Disabled).
    /// Used to select log level when SQPOLL falls back.
    pub policy: IoUringPolicy,
}
```

Callers that construct `IoUringConfig` already know the policy (it flows
from `CoreConfig` -> `TransferConfig` -> ring builder). The addition is
a single-field plumbing change.

## 5. Scope boundaries

What SQP-3 does:

- Adds a log message (info or warn) on SQPOLL EPERM fallback.
- Stores the errno alongside the fallback flag for richer diagnostics.
- Enriches `--io-uring-status` with the errno and kernel-version hint.

What SQP-3 does NOT do:

- Does not change fallback behaviour. The ring still falls back silently
  in terms of functionality - only observability improves.
- Does not make SQPOLL failure fatal even under `Enabled` policy. The
  `Enabled` policy means "use io_uring, error if io_uring is unavailable"
  - SQPOLL is a performance tier within io_uring, not a hard requirement.
- Does not add CAP_SYS_NICE detection at process startup. The message is
  emitted at the point of failure (lazy), not eagerly at init.
- Does not interact with the mmap-basis path (SQM-3). That path has its
  own `debug_log!` and is orthogonal.

## 6. Test plan

1. **Unit test**: mock `build_ring` SQPOLL failure path with
   `IoUringPolicy::Enabled` and verify the warn-level message is emitted
   (capture via test log subscriber).
2. **Unit test**: same with `IoUringPolicy::Auto` - verify info level.
3. **Unit test**: verify single-fire semantics - two consecutive
   `build_ring` failures produce exactly one log line.
4. **Integration test**: on an unprivileged CI runner, request SQPOLL
   and verify `sqpoll_fell_back()` returns `true` AND the fallback errno
   is stored (new accessor: `sqpoll_fallback_errno() -> Option<i32>`).
5. **`--io-uring-status` golden test**: verify the enhanced capability
   matrix includes the errno and kernel-version hint when SQPOLL fell
   back.

## 7. Dependencies

- No new crate dependencies.
- Uses existing `logging::debug_log!` macro infrastructure (which
  supports info/warn/error levels via the level parameter).
- Relies on `check_io_uring_reason()` for kernel version - already
  cached per-process.
