# SQP-4 - Rootless container detection at io_uring initialization

Tracking issue: #3298

Related:

- SQP-3 (#3297) - SQPOLL capability error message improvements
- SQP-6 (#3300) - Deployment guide for container environments
- IKV-F (`feat/ikv-f-io-uring-fallback-observability`) - Fallback observability
- `crates/fast_io/src/io_uring/config.rs` - `build_ring()` SQPOLL fallback
- `docs/design/ikv-3-runtime-probe-matrix.md` - Runtime probe design

## 1. Problem statement

When oc-rsync runs inside a rootless container (Podman rootless, Docker with
user namespaces, or similar), SQPOLL setup via `IORING_SETUP_SQPOLL` always
fails with `EPERM` because `CAP_SYS_NICE` is not available in the user
namespace. The current code path in `build_ring()` handles this failure by
falling back to a regular ring, but:

1. The SQPOLL probe attempt costs a failed syscall (`io_uring_setup` with
   SQPOLL flags) plus associated kernel bookkeeping, repeated per ring
   construction.
2. The `EPERM` error from a container environment is indistinguishable from
   a host system lacking `CAP_SYS_NICE` - both produce the same fallback,
   but the container case is structural (will never succeed for the lifetime
   of the process) whereas the host case might be resolved by granting the
   capability.
3. Operators debugging container deployments see generic "SQPOLL fell back"
   messages without context about *why* - missing the container-environment
   detail that would immediately explain the situation.

Detecting the container environment proactively allows the init path to skip
the SQPOLL probe entirely, log context-rich diagnostics, and avoid wasted
syscalls on every ring construction.

## 2. Detection algorithm

Three signals are checked in order; the first match is sufficient:

### 2.1 User namespace detection (`/proc/self/uid_map`)

The most reliable signal. Inside a user namespace (which is the mechanism
underlying rootless containers), `/proc/self/uid_map` contains a non-trivial
mapping - typically `0 1000 1` or similar, where the inner UID 0 maps to an
outer UID. On the host, the mapping is always the identity `0 0 4294967295`.

**Detection rule:** Read `/proc/self/uid_map`. If the file:
- exists and is readable, AND
- does NOT contain a single line matching `^\s*0\s+0\s+4294967295\s*$`

...then the process is running inside a user namespace.

This catches:
- Podman rootless
- Docker with `--userns=host` disabled (user-remapped)
- Kubernetes with user namespace isolation
- Any nested user namespace

### 2.2 Podman marker (`/run/.containerenv`)

Podman (both rootless and rootful) creates `/run/.containerenv` inside the
container mount namespace. Its presence confirms a Podman container regardless
of user namespace configuration.

**Detection rule:** Check `Path::new("/run/.containerenv").exists()`.

### 2.3 Docker marker (`/.dockerenv`)

Docker creates `/.dockerenv` in the container root filesystem. This file
exists in both rootful and rootless Docker containers.

**Detection rule:** Check `Path::new("/.dockerenv").exists()`.

### 2.4 Combined result

```rust
/// Container environment detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContainerContext {
    /// Not running in a detectable container environment.
    None,
    /// Running inside a user namespace (rootless container or explicit userns).
    UserNamespace,
    /// Running inside a Podman container (detected via /run/.containerenv).
    Podman,
    /// Running inside a Docker container (detected via /.dockerenv).
    Docker,
}

impl ContainerContext {
    /// Returns `true` when SQPOLL is structurally unavailable due to the
    /// container environment lacking `CAP_SYS_NICE`.
    pub(crate) fn sqpoll_unavailable(&self) -> bool {
        // User namespace containers never have CAP_SYS_NICE in the initial
        // namespace. Podman/Docker markers alone don't guarantee rootless
        // (could be rootful with full caps), but in practice rootful containers
        // with CAP_SYS_NICE are rare enough that the false-positive cost
        // (skipping a single SQPOLL attempt) is negligible vs the common case.
        *self != ContainerContext::None
    }
}
```

The ordering (uid_map first, then Podman, then Docker) ensures the most
precise signal wins. A Podman rootless container matches both 2.1 and 2.2;
the uid_map check fires first and is reported as `UserNamespace`.

## 3. Insertion point in the init path

The detection runs once per process, cached alongside the existing io_uring
availability check in `config.rs`. The natural insertion point is inside
`IoUringConfig::build_ring()`, before the SQPOLL attempt:

```
build_ring()
  |
  +-- [NEW] detect_container_context()  // cached in OnceLock
  |     |
  |     +-- returns ContainerContext
  |
  +-- if sqpoll_requested && container.sqpoll_unavailable()
  |     |
  |     +-- log container context, set SQPOLL_FALLBACK, skip SQPOLL attempt
  |     +-- proceed directly to RawIoUring::new(sq_entries)
  |
  +-- else if sqpoll_requested
  |     |
  |     +-- [existing path] attempt SQPOLL, fall back on EPERM
  |
  +-- else
        |
        +-- [existing path] plain ring
```

### 3.1 Caching

The container context is immutable for the lifetime of a process. Store the
result in a `OnceLock<ContainerContext>`:

```rust
use std::sync::OnceLock;

static CONTAINER_CONTEXT: OnceLock<ContainerContext> = OnceLock::new();

pub(crate) fn container_context() -> &'static ContainerContext {
    CONTAINER_CONTEXT.get_or_init(detect_container_context)
}
```

This function is called from `build_ring()` on the first ring construction.
All subsequent calls are a single pointer deref with no I/O.

### 3.2 File: `crates/fast_io/src/io_uring/container_detect.rs`

A new internal module alongside `config.rs`. Not public - this is an
implementation detail of the SQPOLL init path:

```rust
// crates/fast_io/src/io_uring/container_detect.rs
//! Rootless container environment detection for SQPOLL skip logic.
//!
//! Checks /proc/self/uid_map, /run/.containerenv, and /.dockerenv to
//! determine whether SQPOLL probes will structurally fail with EPERM.
//! Results are cached process-wide in a OnceLock.
```

Register in `mod.rs`:

```rust
mod container_detect;
```

## 4. Logging

When a container is detected and SQPOLL is skipped, emit a single
informational log line via the existing `logging::debug_log!` macro at
verbosity level 1 (same as other io_uring init diagnostics):

```
io_uring: detected rootless container (user namespace), skipping SQPOLL probe
io_uring: detected container (Podman via /run/.containerenv), skipping SQPOLL probe
io_uring: detected container (Docker via /.dockerenv), skipping SQPOLL probe
```

When container detection finds nothing, no log line is emitted (quiet path).

Additionally, the `container_context()` result is available to the
`io_uring_availability_reason()` function so that `--version` output can
include container context when relevant:

```
io_uring: enabled (kernel 5.15, 42 ops supported, pbuf_ring=yes, container=podman-rootless, sqpoll=skipped)
```

## 5. SQPOLL skip behaviour

When `container_context().sqpoll_unavailable()` returns `true` and
`IoUringConfig::sqpoll` is `true`:

1. Set `SQPOLL_FALLBACK.store(true, Ordering::Relaxed)` - preserves the
   existing contract that `sqpoll_fell_back()` returns `true` when SQPOLL
   was requested but not used.
2. Log the container-specific message (section 4).
3. Skip the `io_uring::IoUring::builder().setup_sqpoll()` call entirely.
4. Proceed to `RawIoUring::new(self.sq_entries)` for a plain ring.

This eliminates one failed `io_uring_setup` syscall per ring construction in
container environments. On a daemon handling many concurrent connections, each
spawning its own per-thread ring, this removes N wasted syscalls where N is
the connection count.

## 6. Edge cases

### 6.1 Rootful containers with CAP_SYS_NICE

A rootful Podman/Docker container that has `--cap-add=SYS_NICE` will match
the Podman/Docker marker files but SQPOLL would actually work. The
`uid_map` check does NOT fire for rootful containers (the mapping is trivial
inside the initial namespace). Since the marker-file checks (2.2, 2.3)
run after the uid_map check:

- Rootful + trivial uid_map -> markers still fire, SQPOLL skipped
- Cost: one lost optimization opportunity (SQPOLL not attempted)
- Mitigation: add a refinement check for `CAP_SYS_NICE` presence

To handle this, add a `prctl(PR_CAPBSET_READ, CAP_SYS_NICE)` check after
a Podman/Docker marker fires. If the capability is in the bounding set, the
container context is downgraded to `None` and SQPOLL proceeds normally:

```rust
fn has_cap_sys_nice_in_bounding_set() -> bool {
    // prctl(PR_CAPBSET_READ, CAP_SYS_NICE) returns 1 if present, 0 if not
    // CAP_SYS_NICE = 23
    unsafe { libc::prctl(libc::PR_CAPBSET_READ, 23) == 1 }
}
```

### 6.2 /proc not mounted

Some hardened containers unmount `/proc`. If `/proc/self/uid_map` is
unreadable, the uid_map check returns `false` (no detection) and falls
through to the marker-file checks. This is correct: without `/proc` there
is no definitive user-namespace signal, and the marker files provide the
container signal separately.

### 6.3 Flat namespaces (LXC/LXD unprivileged)

LXC unprivileged containers use user namespaces. The uid_map check catches
these correctly. No LXC-specific marker file is needed.

### 6.4 Kubernetes with user namespaces

Kubernetes 1.25+ supports user namespace isolation. The uid_map check
detects this without requiring any container-runtime-specific markers.

### 6.5 WSL2

Windows Subsystem for Linux 2 is not a container; its uid_map is trivial
(`0 0 4294967295`), and neither marker file exists. No false positive.

## 7. Testing strategy

### 7.1 Unit tests

- Parse various `/proc/self/uid_map` contents:
  - `"         0          0 4294967295\n"` -> `None` (host identity)
  - `"         0       1000          1\n"` -> `UserNamespace`
  - `"         0     100000      65536\n"` -> `UserNamespace` (subuid)
  - Empty/unreadable -> falls through to marker checks

- Mock file existence for marker checks:
  - Both absent -> `None`
  - `/run/.containerenv` present -> `Podman`
  - `/.dockerenv` present -> `Docker`

### 7.2 Integration tests

- Run in a real Podman rootless container: verify `container_context()`
  returns `UserNamespace` or `Podman`, and `sqpoll_fell_back()` is `true`
  when SQPOLL was requested.
- Verify that `--version` output includes container context string.

### 7.3 Existing test compatibility

- On the host, all tests continue to pass unchanged; `container_context()`
  returns `None` and the existing SQPOLL attempt path is unaffected.
- The `OC_RSYNC_DISABLE_IOURING=1` env override still works orthogonally
  (disables all io_uring, not just SQPOLL).

## 8. Performance impact

| Path | Syscalls saved | Notes |
|------|---------------|-------|
| Container, SQPOLL requested | 1 per ring construction | Eliminates failed `io_uring_setup` with SQPOLL flags |
| Container, SQPOLL not requested | 0 | Detection cached but not on the critical path |
| Host, SQPOLL requested | 0 | Existing path unchanged; detection cost is one `OnceLock` init |
| Host, no SQPOLL | 0 | Detection never queried if SQPOLL not requested |

The detection itself costs (first call only):
- 1 `open` + `read` of `/proc/self/uid_map` (~200 bytes)
- Up to 2 `stat` calls for marker files
- Total: < 5 microseconds, once per process lifetime

## 9. Relationship to existing work

- **IKV-F fallback observability** introduced structured logging for io_uring
  feature degradation. SQP-4 adds a new pre-condition signal (container
  detection) to that observability surface.
- **SQP-3** improved SQPOLL error messages. SQP-4 avoids the error entirely
  by detecting the futility upfront.
- **SQP-6** deployment guide documents the CAP_SYS_NICE requirement. SQP-4
  makes the runtime self-documenting: operators see container context in logs
  without consulting the guide.
- **`mmap_basis_active` defensive disable** (config.rs:336-373) is a
  compile-time/config-time decision about SQPOLL safety. SQP-4 is a
  runtime-environment decision about SQPOLL feasibility. Both set
  `SQPOLL_FALLBACK` and both log their reason.

## 10. Implementation plan

1. Add `crates/fast_io/src/io_uring/container_detect.rs` with the detection
   functions, `ContainerContext` enum, and the `OnceLock` cache.
2. Register the module in `crates/fast_io/src/io_uring/mod.rs`.
3. Modify `IoUringConfig::build_ring()` in `config.rs` to call
   `container_context()` before the SQPOLL attempt.
4. Extend `IoUringProbeResult::reason()` to include container context when
   non-`None`.
5. Add unit tests for uid_map parsing and marker detection.
6. Add an integration test using Podman rootless (guarded by
   `#[cfg_attr(not(feature = "container-tests"), ignore)]`).
