# SQPOLL rootless-container detection - landing status audit

Audit task: SQP-LAND.1 (#3647).
Audit date: 2026-06-10.
Scope: oc-rsync production code under `crates/fast_io/src/`, comparing the
SQP-1..6 design (`docs/design/sqpoll-rootless-container-detection.md`,
`docs/design/sqpoll-capability-error-message.md`,
`docs/audit/sqpoll-capability-requirements.md`) against what is actually
compiled into the binary.

## Headline

**Rootless-container detection is NOT shipped.** SQP-1..6 are marked
"completed" in the project task list, but no production code in
`crates/fast_io/` reads `/proc/self/uid_map`, checks for
`/run/.containerenv` or `/.dockerenv`, or otherwise distinguishes a
rootless container from a host that lacks `CAP_SYS_NICE`. Only the
SQM-3 mlock primitive (`WiredBasisWindow`) and the silent reactive
EPERM-fallback in `build_ring` are in the binary.

## The single decision site that owns SQPOLL setup

`crates/fast_io/src/io_uring/config.rs:357-386` -
`IoUringConfig::build_ring()`.

Key lines:

- `:358` `let sqpoll_requested = self.sqpoll;` - reads caller's request.
- `:359` `let mlock_basis_enabled = cfg!(feature = "sqpoll-mlock-basis");` -
  the only "SQM-3 signal" used in the decision is a compile-time feature
  flag, not a runtime mlock attempt.
- `:360` `let sqpoll_safe = sqpoll_requested && (!self.mmap_basis_active || mlock_basis_enabled);`
- `:373-376` `builder.setup_sqpoll(self.sqpoll_idle_ms); builder.build(...)` -
  unconditional optimistic attempt when `sqpoll_safe` is true.
- `:377-381` `Err(_) => { SQPOLL_FALLBACK.store(true, Ordering::Relaxed); }` -
  generic catch-all on any kernel rejection. The errno is discarded; EPERM is
  not distinguished from ENOMEM or anything else, and no log line fires
  here (the only log line in the function fires on the `mmap_basis_active`
  refusal branch).

This is the file:line SQP-LAND.2 must land work in.

Second SQPOLL decision site, for completeness:
`crates/fast_io/src/io_uring/session_pool.rs:272-283` -
`build_ring(config: &SessionPoolConfig)`. This path has **no** EPERM
fallback at all - it propagates the error directly. SQP-LAND.2 should
either teach this path the same fallback or document why session-pool
rings are exempt.

## Per-file findings

### `crates/fast_io/src/io_uring/config.rs`

| Element | Line | Shipped? | Notes |
|---|---|---|---|
| `SQPOLL_FALLBACK: AtomicBool` | 53 | yes | Process-wide flag, set on any setup error after fallback. |
| `sqpoll_fell_back()` | 67-70 | yes | Public read-only accessor for `--io-uring-status`. |
| `build_ring()` | 357-386 | partial | Optimistic try-then-fallback. Silent on the EPERM branch (no `debug_log!`/`warn!`). |
| `ContainerContext` enum | - | **NO** | Designed in `docs/design/sqpoll-rootless-container-detection.md` section 2.4; zero references in code. |
| `detect_container_context()` | - | **NO** | Designed insertion point at `build_ring` top; not implemented. |
| `/proc/self/uid_map` parse | - | **NO** | Not referenced anywhere in `crates/fast_io/`. |
| `/run/.containerenv` check | - | **NO** | Not referenced anywhere in `crates/fast_io/`. |
| `/.dockerenv` check | - | **NO** | Not referenced anywhere in `crates/fast_io/`. |

### `crates/fast_io/src/sqpoll_basis.rs` (SQM-3 mlock primitive)

| Element | Line | Shipped? | Notes |
|---|---|---|---|
| `WiredBasisWindow::new` | 189-226 | yes | RAII guard around `mlock(2)`/`munlock(2)`. Production-ready. |
| `MlockError::{Downgrade,Fatal}` | 97-106 | yes | Typed error classifies `EAGAIN`/`EPERM`/`ENOMEM` as downgrade. |
| `mlock_attempts()` / `mlock_downgrades()` | 81-94 | yes | Counter pair for SQM-2.b rollback ratio. |
| Production callers of `WiredBasisWindow::new` | - | **NO** | The only call sites are tests under `crates/fast_io/tests/sqpoll_mlock_fault_injection.rs`. No submission path constructs the guard. |

Implication: SQM-3's mlock approach is plumbing waiting for a consumer.
The "wiring" referenced in `config.rs:359` is a compile-time `cfg!`
boolean, not a runtime mlock guard around submissions.

### `crates/fast_io/src/io_uring/session_pool.rs`

| Element | Line | Shipped? | Notes |
|---|---|---|---|
| `IORING_SETUP_SQPOLL = 1 << 1` | 270 | yes | Local constant. |
| `build_ring` SQPOLL path | 277-279 | partial | Sets up SQPOLL when `flags` requests it; **no EPERM/ENOMEM fallback**; error propagates to caller as `io_uring init failed`. |

### `crates/fast_io/src/status.rs`

| Element | Line | Shipped? | Notes |
|---|---|---|---|
| `IoUringRestriction::SyscallBlocked` | 50-55 | yes | Covers io_uring base availability blocked by seccomp / container. |
| `IoUringRestriction::SqpollUnavailable` | - | **NO** | No SQPOLL-specific variant. `SyscallBlocked` is for io_uring as a whole, not the SQPOLL tier. |
| `sqpoll fell back` status line | 187-194 | yes | Reports `"yes (CAP_SYS_NICE likely missing)"` post-hoc; speculative wording because no detection runs. |

### `crates/fast_io/src/linux_capabilities.rs`

Module exists for `openat2_supported()` (SEC-1) only. Despite the name,
it does not probe `CAP_SYS_NICE`, `CAP_IPC_LOCK`, or any other capability
relevant to SQPOLL.

### `crates/fast_io/src/io_uring_common.rs`

`IoUringConfig::sqpoll: bool` field (line 103) and `mmap_basis_active`
flag (line 114) are inputs to the decision; no rootless-detection flag
exists on the struct.

## SQP-3 typed-error path

The SQP-3 spec (`docs/design/sqpoll-capability-error-message.md`)
specifies an `info!`/`warn!` log line at the EPERM branch keyed by
`--io-uring=auto` vs `--io-uring=enabled`. Status:

- The error branch at `config.rs:377-381` matches `Err(_)` and discards
  the errno. EPERM is not distinguishable from any other failure.
- No typed `SqpollSetupError` enum exists.
- No log line is emitted at the EPERM branch. The only `debug_log!` in
  the function fires on the mmap-basis refusal branch (`:362-369`).
- `--io-uring=enabled` is not threaded into this decision; the function
  has no policy input distinguishing auto from explicit.

SQP-3 is not shipped.

## How SQM-3 mlock relates to rootless detection

The SQM-3 mlock approach (`WiredBasisWindow`) and SQP-2/4 rootless
detection are **complementary, not substitutes**:

- SQM-3 closes the SQPOLL + mmap **kernel page-fault race** by wiring
  basis pages before submission. It still needs `CAP_IPC_LOCK` to
  succeed and still requires SQPOLL to be available in the first place.
- SQP-2/4 detects whether SQPOLL is structurally unavailable
  (rootless / user namespace, no `CAP_SYS_NICE`) so the init path can
  skip the doomed `IORING_SETUP_SQPOLL` syscall, emit a context-rich
  diagnostic, and avoid the SQM-3 mlock path when it cannot help.

Crucially, SQM-3 today is gated only by the `sqpoll-mlock-basis` Cargo
feature; no caller ever constructs a `WiredBasisWindow`. Even if SQP-2
detection landed, there is no submission-time consumer to wire it to.
SQP-LAND.2 should consider whether to:

1. Add rootless detection at `build_ring` top and short-circuit before
   the SQPOLL probe, AND
2. Wire `WiredBasisWindow::new` into the actual SQE-batch submission
   path so the SQM-3 plumbing is exercised in production.

## Recommended detection signal

Per the SQP-4 design spec, with no changes to the chosen ordering:

1. `/proc/self/uid_map` - read once; if its single line does NOT match
   `^\s*0\s+0\s+4294967295\s*$` then the process is inside a user
   namespace and `CAP_SYS_NICE` will not be available. Most precise
   signal; covers Podman rootless, Docker with user-remap, Kubernetes
   userns isolation.
2. `/run/.containerenv` - presence indicates a Podman container.
   Secondary signal because rootful Podman can have `CAP_SYS_NICE`,
   but false-positive cost is one skipped SQPOLL attempt.
3. `/.dockerenv` - presence indicates a Docker container. Same
   trade-off as `/run/.containerenv`.

Cache in a `OnceLock<ContainerContext>` adjacent to the existing
`IO_URING_AVAILABLE` / `IO_URING_CHECKED` atomics in
`crates/fast_io/src/io_uring/config.rs:22-23`. Insert the check at the
top of `build_ring()` (currently `:358`) before
`sqpoll_requested = self.sqpoll`.

## Recommended landing footprint for SQP-LAND.2

- New module: `crates/fast_io/src/container_context.rs` exposing
  `pub(crate) enum ContainerContext` + `pub(crate) fn detect() -> ContainerContext`
  with a `OnceLock` cache. Linux-gated; non-Linux returns
  `ContainerContext::None`.
- Edit `crates/fast_io/src/io_uring/config.rs:357-386` to consult the
  cache before calling `builder.setup_sqpoll(...)`. On a detected
  container, skip the syscall, set `SQPOLL_FALLBACK`, and emit a
  `debug_log!(Io, 1, ...)` with the `ContainerContext` variant
  embedded.
- Add a typed `IoUringRestriction::SqpollUnavailable { reason }`
  variant in `crates/fast_io/src/status.rs:33-58` so
  `--io-uring-status` distinguishes "SQPOLL skipped because rootless
  container" from "SQPOLL fell back after EPERM".
- Add EPERM-specific error message at the existing `Err(_)` branch in
  `config.rs:377-381` per SQP-3 spec section 4.1.
- Wire `WiredBasisWindow::new` into at least one SQE-batch submission
  site (likely `crates/fast_io/src/io_uring_ops.rs` around basis-window
  reads) so the SQM-3 mlock plumbing has a production consumer.

## Honest statement on prior task accounting

Memory notes `project_sqpoll_rootless_container.md` describe SQP-1..6
as "marked completed" with tracking issues #3295-#3300. This audit
contradicts that accounting:

- SQP-1 (audit) - shipped: `docs/audit/sqpoll-capability-requirements.md`.
- SQP-2 (probe design) - shipped as design only: docs exist, code does not.
- SQP-3 (typed error / log) - **not shipped in production code**.
- SQP-4 (detection landing) - **not shipped in production code**.
- SQP-5 (rootless test harness) - not in scope of this audit.
- SQP-6 (deployment guide) - not in scope of this audit.

What is shipped is the SQM-3 mlock primitive (`WiredBasisWindow`) and
the reactive try-then-fallback EPERM handling. Both exist; neither is
the SQP-2/4 proactive detection that the design specifies.
