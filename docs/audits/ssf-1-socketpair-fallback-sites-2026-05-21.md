# SSF-1 - SSH stderr socketpair-to-pipe fallback site inventory

Date: 2026-05-21
Scope: read-only audit of `crates/rsync_io/src/ssh/`
Tracked under: #2578
Sets up: SSF-2 (one-shot operator warning on runtime degradation)

## Goal

Inventory every branch in the SSH stderr layer that picks between the
socketpair-backed drain and the anonymous-pipe fallback so the SSF-2 follow-up
can emit a one-shot operator warning when the degradation happens at runtime.
Today the only `"fallback-to-pipe"` string in the tree is a test fixture
(`aux_channel.rs:614`, asserted at `:628-629`); production has no log line at
the moment a session degrades.

## 1. Inventory

The branching is concentrated in three layers: the channel factory in
`aux_channel.rs`, the standalone primitive `socketpair_stderr.rs`, and the two
consumers (`builder.rs` for the sync path, `async_transport.rs` for the async
path). `connection.rs` and `embedded/sync_bridge.rs` carry no
socketpair-vs-pipe branches themselves - they accept whatever
`BoxedStderrChannel` the factory produced.

| # | File | Lines | Branch | Trigger | Outcome |
|---|------|-------|--------|---------|---------|
| 1 | `crates/rsync_io/src/ssh/aux_channel.rs` | `337-359` | `configure_stderr_channel` Unix arm: `match UnixStream::pair()` | runtime - kernel rejects `socketpair(AF_UNIX, SOCK_STREAM, 0)` (EMFILE, ENFILE, EPERM, ENOSYS) | `Err` arm at `349-358` installs `Stdio::piped()` and emits `debug_log!(Connect, 2, ...)` with the `io::Error`. Returns `None`. |
| 2 | `crates/rsync_io/src/ssh/aux_channel.rs` | `361-365` | `configure_stderr_channel` non-Unix shim under `#[cfg(not(unix))]` | compile-time - target is not Unix (Windows, WASI) | Unconditional `Stdio::piped()`; returns `None`. No diagnostic. |
| 3 | `crates/rsync_io/src/ssh/aux_channel.rs` | `372-382` | `build_stderr_channel` Unix arm | propagation of (1) - `parent_socketpair_end.is_some()` | `Some` -> `SocketpairStderrChannel::spawn(parent)`; `None` -> `PipeStderrChannel::spawn(stderr)`. No diagnostic at this site; it only rewraps the choice made in (1)/(2). |
| 4 | `crates/rsync_io/src/ssh/aux_channel.rs` | `384-390` | `build_stderr_channel` non-Unix shim under `#[cfg(not(unix))]` | compile-time - same as (2) | Always wraps `child_stderr` in `PipeStderrChannel`. |
| 5 | `crates/rsync_io/src/ssh/aux_channel.rs` | `223` | `SocketpairStderrChannel::spawn` - `try_clone().ok()` for `parent_clone` | runtime - `dup(2)` of the parent socketpair half fails | "Half-fallback": drain proceeds normally, but `shutdown_read` becomes a no-op (`244-252`) and the channel relies on the `DRAIN_JOIN_TIMEOUT` (50 ms, `:42`) to abandon a stuck drain. No log. |
| 6 | `crates/rsync_io/src/ssh/socketpair_stderr.rs` | `80-98` | `make_stderr_socketpair_impl` Unix vs non-Unix | compile-time non-Unix returns `io::ErrorKind::Unsupported` (`:93-97`); runtime Unix returns the kernel error from `UnixStream::pair()` | Caller (any future SSE-5 consumer) is expected to fall back. No diagnostic. Note: today only the `socketpair-stderr` feature-gated `pub` API; not on the production spawn path. |
| 7 | `crates/rsync_io/src/ssh/builder.rs` | `339` | `SshCommand::spawn` calls `configure_stderr_channel(&mut command)` | propagation of (1)/(2) | Unconditional - the result drives `build_stderr_channel` at `:358`. No site-local diagnostic. |
| 8 | `crates/rsync_io/src/ssh/async_transport.rs` | `60-63, 141-156` | `#[cfg(all(feature = "ssh-socketpair-stderr", unix))]` block selects socketpair; `#[cfg(not(...))]` arm picks `Stdio::inherit()` (note: **not** `Stdio::piped()`) | compile-time when feature off or non-Unix; runtime when `configure_stderr_channel` returns `None` at `:143` | Distinct from sync path: async transport degrades to `Stdio::inherit()` (`:151,156`) rather than the pipe-drained path. No `stderr_capture()` data is collected post-fallback (`:204-218, 224-228`). No log. |
| 9 | `crates/rsync_io/src/ssh/async_transport.rs` | `177-190` | `match parent_socketpair_end` | propagation of (8) | `Some(parent)` -> `set_nonblocking(true)?` + `tokio::net::UnixStream::from_std(parent)?` + `AsyncStderrDrain::spawn`. `None` -> `stderr_drain = None`. The `?` operators expose a second half-fallback path: a socketpair that was created may still fail to become async (e.g. no tokio runtime, fd flag refusal) and is propagated as `io::Error` rather than degraded. |
| 10 | `crates/rsync_io/src/ssh/connection.rs` | `30-39, 396-422` | `SshConnection` / `SshChildHandle` hold `stderr_drain: Option<BoxedStderrChannel>` | propagation only | These structs are agnostic to which backend produced the trait object. `Drop` (`:492-514`, `:562-585`) calls `join_and_surface_on_error` which works for both backends. No diagnostic. |
| 11 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs` | n/a | none | russh-backed embedded SSH never uses the OS stderr socketpair (the russh channel carries diagnostics in-band), so no branch exists | n/a |

**Total branching sites: 8 in code, plus 2 propagation-only struct fields and 1 confirmed non-site.**

## 2. Conditions table

| Condition | Site(s) | Class |
|-----------|---------|-------|
| Workspace built without `ssh-socketpair-stderr` feature | n/a in `aux_channel.rs` (always compiled), site (8) in `async_transport.rs` | compile-time |
| Target is not Unix (Windows, WASI) | (2), (4), (6), (8) | compile-time |
| Kernel/libc rejects `socketpair(AF_UNIX, SOCK_STREAM, 0)` - ENOSYS (sandboxed), EMFILE (per-process fd cap), ENFILE (system-wide), EPERM (seccomp) | (1) | runtime |
| `try_clone` on the parent socketpair half fails - typically EMFILE post-spawn | (5) | runtime, half-fallback |
| Non-blocking flag flip on parent fd fails | (9), via `set_nonblocking(true)?` at `async_transport.rs:185` | runtime, error - not fallback |
| `tokio::net::UnixStream::from_std` fails (no runtime driver) | (9), at `async_transport.rs:186` | runtime, error - not fallback |
| `Drop` path with already-degraded channel | (10) | propagation; no extra branch |

## 3. Detectability at each site

The information an operator could surface today (had we logged it):

| # | Information available locally | Currently emitted? |
|---|------------------------------|--------------------|
| 1 | `io::Error` (`errno` + `Display`) from `UnixStream::pair()` | only at `debug_log!(Connect, 2, ...)` - requires verbose tracing |
| 2 | conditional-compilation context (target = Windows etc.) | no |
| 5 | `try_clone()` returned `None` (errno-bearing `io::Error` thrown away by `.ok()`) | no - error dropped |
| 6 | `io::ErrorKind::Unsupported` literal message at `:96-97` | returned to caller, not logged |
| 8 (compile-time arm) | feature flag state, target | no |
| 8 (runtime arm) | `parent.is_none()` only - root cause already lost upstream at (1) | no |
| 9 (set_nonblocking / from_std) | full `io::Error` propagated via `?` | propagated to caller as `Err` |

The compile-time fallbacks (2, 4, 6, 8-compile, 11) are by definition known at
build time and do not need runtime detection - documentation in
`docs/design/socketpair-stderr-channel.md` and the feature-gate comment in
`crates/rsync_io/Cargo.toml:34-44` covers them.

The runtime degradations - (1), (5), (8-runtime), and (9) - drop diagnostic
information that an operator cannot recover after the fact.

## 4. Recommended SSF-2 sites

SSF-2 should emit a one-shot warning at the **runtime** sites only. Compile-time
fallbacks are static configuration and should remain documentation-only.

| # | Site | Recommended action |
|---|------|--------------------|
| 1 | `aux_channel.rs:349-358` | Upgrade the existing `debug_log!(Connect, 2, ...)` to `tracing::warn!` (gated on `tracing` being present) with `target = "ssh::stderr"`, fire-once via `std::sync::OnceLock<()>`. The `io::Error` is already in scope. |
| 5 | `aux_channel.rs:223` | Capture the `try_clone` error before `.ok()` and emit one-shot `tracing::warn!` noting that `shutdown_read` is degraded to timeout-only. Half-fallback - keep severity below the (1) warning. |
| 8 (runtime arm) | `async_transport.rs:150-153` | Mirror site (1) here: when `configure_stderr_channel` returns `None` on the async path, warn that capture is unavailable for this transport. Note the async path further degrades to `Stdio::inherit()` rather than `Stdio::piped()` - the warning should call that out so operators understand stderr will appear on the parent's terminal rather than being captured. |
| 9 | `async_transport.rs:185-186` | These are already `Err`-returning paths; they surface to the caller. Do **not** add a warning - SSF-2 is for silent degradations only. |

Recommended channel and level:

- **Channel**: `tracing::warn!(target = "ssh::stderr", ...)`. The async drain
  already uses this target (`async_stderr_drain.rs:258`), so subscribers
  filtering on `ssh::stderr` will get both per-line warnings and the
  degradation notice. Use `eprintln!` only as a fallback when `tracing` is not
  linked (i.e., when the `ssh-socketpair-stderr` feature is off; in that case
  the sync `aux_channel.rs` site has to use `eprintln!` because `tracing` is
  not a default dependency - see `Cargo.toml:44`).
- **One-shot discipline**: `std::sync::OnceLock<()>` per site. Multiple SSH
  invocations in the same process (rsync resumes, push+pull session) should
  not spam logs.
- **Suggested text** (site 1): `"ssh stderr: socketpair unavailable ({error});
  falling back to anonymous pipe drain. Async event-loop polling of ssh
  stderr is degraded for this session."`
- **Suggested text** (site 8 runtime): `"ssh stderr: socketpair unavailable
  ({error}); async transport falling back to Stdio::inherit(). Remote ssh
  diagnostics will appear on the parent process stderr and will not be
  captured by stderr_capture()."`
- **Suggested text** (site 5): `"ssh stderr: socketpair clone failed
  ({error}); shutdown_read degraded to timeout-only ({DRAIN_JOIN_TIMEOUT}).
  The drain thread may be abandoned at child exit rather than woken
  immediately."`

## 5. Edge cases

### 5.1 Half-fallback: socketpair succeeds, secondary step fails

Site (5) is the canonical example: `UnixStream::pair()` succeeded so
`configure_stderr_channel` returned `Some(parent)`, but `try_clone()` at
`aux_channel.rs:223` failed. The channel is socketpair-backed for the drain
read, but `shutdown_read` (`:244-252`) silently becomes a no-op because
`parent_clone` is `None`. The drain then relies entirely on the 50 ms
`DRAIN_JOIN_TIMEOUT` (`aux_channel.rs:42`) - if an ssh helper inherited the
write end, the drain thread is abandoned via `std::mem::forget` at `:54` and
runs until process exit. This is **not** a fallback to pipe; it is a
socketpair channel with degraded wake-up.

Site (9) is the inverse: `UnixStream::pair()` succeeded *and* `try_clone`
succeeded inside (5), but `set_nonblocking(true)?` or
`tokio::net::UnixStream::from_std(parent)?` fails. The `?` operator surfaces
the error to the caller of `execute_remote_rsync` rather than degrading; the
caller sees an `io::Error` from spawn. The child has not yet been spawned
when this fires (lines `:162` and `:177-178`), so there is no leak.

### 5.2 Spawn races

If `command.spawn()` at `builder.rs:341` or `async_transport.rs:162` fails
**after** `configure_stderr_channel` already created a socketpair, the parent
half (`UnixStream` returned to the local `parent_socketpair_end`) is dropped
when the function returns `Err(...)`, which closes the parent fd. The child
half was moved into the `Command` via `Stdio::from(child_fd)`; `Command::spawn`
on failure drops it too. No fd leak. No SSF-2 action needed - the error is
surfaced normally.

### 5.3 Feature-gate matrix sanity

Async transport selects the socketpair path only when both `async-ssh` **and**
`ssh-socketpair-stderr` are enabled (`async_transport.rs:60-63, 141, 177,
196-197, 211, 224`). The matrix:

| `async-ssh` | `ssh-socketpair-stderr` | unix | Async stderr behaviour |
|-------------|-------------------------|------|------------------------|
| off | any | any | async transport not compiled |
| on | off | any | `Stdio::inherit()` only |
| on | on | no | `Stdio::inherit()` only |
| on | on | yes | socketpair, then runtime-degrade to `Stdio::inherit()` per site (8) |

The sync transport is unaffected by `ssh-socketpair-stderr` - it always tries
the socketpair via `aux_channel.rs:337-359` and falls back to `Stdio::piped()`
(not `Stdio::inherit()`) on runtime failure. The feature flag only gates the
**async** drain plumbing and the standalone `socketpair_stderr` primitive
(`mod.rs:87-104`).

### 5.4 `tracing` dependency availability

`tracing` is only a dependency when `ssh-socketpair-stderr` is enabled
(`Cargo.toml:44`). The sync-path warning at site (1) must therefore use
`eprintln!` (matching the existing `eprintln!` at
`aux_channel.rs:129` and `:294`) rather than `tracing::warn!` to avoid forcing
a feature on default builds. Async-path site (8) can safely use
`tracing::warn!` since it is already inside an `#[cfg(all(feature =
"ssh-socketpair-stderr", unix))]` block.

## 6. Out of scope for SSF-2

- Adding `tracing` to the default feature set. Sync-path warnings must use
  `eprintln!` to stay zero-dependency.
- Wiring SSE-5 Windows loopback shim (`socketpair_stderr.rs:80-98`,
  `make_stderr_socketpair_impl` non-Unix arm). Tracked separately under
  SSE-5 / #2374.
- Surfacing the `set_nonblocking` / `from_std` errors at (9) - they are
  already `Result`-propagated; converting them to warnings would mask the
  failure.
- Embedded russh transport - no socketpair branch exists there (site 11).

## 7. References

- `docs/design/socketpair-stderr-channel.md` - SSE design and staging plan
- `docs/audits/ssh-socketpair-vs-anonymous-pipes-verification.md` - data
  socketpair audit (separate concern from this stderr audit)
- `crates/rsync_io/Cargo.toml:29-44` - feature definitions and contract notes
- upstream `pipe.c` - upstream rsync uses inherited stdio for SSH stderr and
  has no socketpair-stderr equivalent; this audit covers an oc-rsync-specific
  capability.
