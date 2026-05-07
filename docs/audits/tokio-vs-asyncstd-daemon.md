# Tokio vs async-std for Daemon Connection Handling

Tracking: oc-rsync task #1590.

## Summary

This audit is the historical record for the tokio-vs-async-std evaluation that
preceded the daemon's async runtime decision. No live decision is needed: the
workspace has been pinned to `tokio` since task #1779 landed, and the
async-only daemon connection design captured in #1934 builds on that pin. The
purpose here is to document why async-std was rejected, why no other async
runtime (smol, glommio) was chosen as a hedge, and why `tokio` remains the
correct single-runtime choice for the daemon and `core` crate.

The recommendation is unchanged: stay on `tokio`. Async-std is unmaintained
since 2022, smol fragments the runtime story without solving a problem the
daemon has, and glommio's thread-per-core model is incompatible with the
daemon's per-connection isolation requirement.

## Current state

The workspace pins `tokio = { version = "1.45", features = ["rt-multi-thread",
"io-util", "net", "fs", "sync", "time", "process", "macros"] }` at
`Cargo.toml:188`, with the doc comment "Async runtime support - enables
tokio-based async I/O throughout the codebase" at `Cargo.toml:106`. `tokio-util
= { version = "0.7", features = ["codec", "io"] }` follows at `Cargo.toml:189`.

Two crates consume the runtime, both behind feature gates:

- `crates/core/Cargo.toml:44` declares `tokio = { workspace = true, optional =
  true }`. The `async` feature at `crates/core/Cargo.toml:93` reads `async =
  ["dep:tokio", "engine/async", "transfer/async"]`. The `embedded-ssh` feature
  at `crates/core/Cargo.toml:90` also pulls `tokio` in: `embedded-ssh =
  ["dep:tokio", "rsync_io/embedded-ssh"]`.
- `crates/daemon/Cargo.toml:45` declares `tokio = { workspace = true, optional
  = true, features = ["net", "io-util", "sync", "rt", "time"] }`. The `async`
  feature at `crates/daemon/Cargo.toml:20` reads `async = ["dep:tokio",
  "core/async"]`. The dev-dependency at `crates/daemon/Cargo.toml:67` brings in
  `["rt-multi-thread", "macros"]` for tests.

The async daemon listener scaffold lives in
`crates/daemon/src/daemon/async_session/` and is gated behind the `async`
feature plus a `#[cfg(test)]` re-export until production wiring is complete
(see #1934 and the parallel audit `daemon-event-loop-multiplexing.md`). The
synchronous accept loop in
`crates/daemon/src/daemon/sections/server_runtime/` is the production path
today.

The dependency tree audit performed under #1780 confirmed that `async-std`,
`smol`, and `glommio` are not transitive dependencies of any workspace crate.
A grep across `Cargo.toml`, `Cargo.lock`, and the `crates/` tree returns zero
matches for `async-std`, `async_std`, `smol`, or `glommio`. The workspace is
single-runtime today.

## Async-std maintenance status

Async-std 1.x has not received a feature release since v1.12.0 in
December 2022. The last published 1.x patch (1.13.x) shipped routine
dependency bumps; the upstream repository
(`https://github.com/async-rs/async-std`) has been effectively dormant for
issue triage and has not produced a 2.x line. The crate is stable but
unmaintained: new Rust toolchain regressions, security fixes in transitive
dependencies, and ecosystem alignment work (for example, the move to MSRV >=
1.70 across the wider crate ecosystem) are not being driven by an active
maintainer.

For a production daemon expected to ship for years and accept patches across
the supported toolchain matrix (Rust 1.88 today, future MSRV bumps), an
unmaintained runtime is disqualifying on three counts:

1. Security advisories in `async-std` or its private dependencies cannot be
   relied on to receive timely upstream fixes. Contrast tokio, which has a
   documented security policy and has shipped patch releases within days for
   advisories such as `RUSTSEC-2024-0019`.
2. Bug fixes specific to async-std's executor or `async-io` reactor would
   require either forking the crate or migrating off it under time pressure.
3. New `async fn in trait`, `async closures`, and `Send` bound stabilisations
   in 2024-edition Rust are landing first in tokio's API surface; async-std's
   trait definitions lag.

## Comparison: smol and glommio

Two other single-threaded-friendly runtimes were considered as alternatives
to async-std, neither as replacements for tokio.

### smol

smol is a thin executor over `async-io` and `async-channel`, the same
ecosystem async-std builds on. It is actively maintained, small (the core
runtime is < 2 kLoC), and binary-compatible with `futures` traits. Its
strengths are dependency footprint and the ability to embed an executor in a
larger application without taking ownership of the main loop.

Trade-offs versus tokio for the daemon path:

- Smol does not provide a `spawn_blocking` thread pool out of the box. The
  daemon's design plan (#1934, F4 in `daemon-event-loop-multiplexing.md`)
  reuses the existing synchronous `handle_session` body via
  `tokio::task::spawn_blocking`. Replicating this on smol requires explicit
  `blocking::Unblock` integration and a separately tuned thread pool, which
  duplicates infrastructure tokio already ships.
- Smol's `async-io` reactor is single-threaded by default. The multi-threaded
  variant via `smol::Executor` plus `async-global-executor` is supported but
  is the path async-std built on, sharing the same maintenance burden risk
  if `async-io` upstream slows.
- The ecosystem surface around smol (TLS adapters, rustls integration,
  systemd notifier, cancellation) is substantially smaller than tokio's. Each
  daemon wiring step in `accept_loop.rs:11-285` would need a smol-compatible
  alternative or a custom shim.
- No precedent for smol in the workspace. Adopting it would create a
  two-runtime situation - tokio in `core/embedded-ssh`, smol in `daemon` -
  for no operational benefit.

### glommio

glommio is a thread-per-core, share-nothing async runtime built on
`io_uring`. It is designed for storage- and networking-heavy workloads where
each core owns a fixed slice of state and there is no work-stealing.

Trade-offs versus tokio for the daemon path:

- Glommio is Linux-only. The daemon must build and run on macOS, Windows,
  and Linux musl per the project compatibility matrix. Single-runtime
  glommio fails this constraint immediately; a hybrid glommio-on-Linux,
  tokio-elsewhere arrangement doubles the daemon's accept-loop
  implementations.
- Glommio's thread-per-core model assumes work that is naturally
  partitionable by core. The daemon's per-connection isolation requirement
  (a faulting session never tears down the daemon, mirroring upstream's
  fork-per-connection guarantee) maps poorly onto thread-per-core: a panic
  in a connection task would tear down the executor for that core and
  therefore for all other connections pinned to it.
- Glommio's `io_uring`-only I/O model overlaps with the workspace's existing
  `fast_io` crate, which already provides Linux 5.6+ `io_uring` support with
  graceful fallback. Adopting glommio in the daemon would not extend
  `io_uring` coverage; it would create a second `io_uring` integration with
  different policy semantics.
- Glommio's API surface is small and the maintainer pool narrow. The crate
  is best suited to specialised single-tenant workloads (DataDog's `agent`,
  scylla-rs); a general-purpose daemon is not the target audience.

Neither smol nor glommio offers a property the daemon needs that tokio does
not provide. Both would introduce additional runtimes into the workspace
contrary to the single-runtime preference verified under #1780.

## Single-runtime preference rationale

The workspace operates under a single-runtime preference for three reasons,
all verified by the #1780 dependency-tree audit:

1. **Binary size.** Each async runtime brings its own reactor, timer wheel,
   and channel implementations. Two runtimes in the same binary roughly
   double the runtime overhead (~300 KiB stripped tokio runtime + comparable
   smol/async-std cost).
2. **Build complexity.** Multiple runtimes mean separate feature flags,
   separate test harnesses (`#[tokio::test]` vs `#[async_std::test]` vs
   `#[smol::main]`), and separate cancellation models (`tokio::select!` vs
   `futures::select!`). Cross-runtime futures interoperability via
   `async-compat` is possible but introduces context-switch overhead and
   another point of failure.
3. **Operational mental model.** Staff debugging a tokio panic, a tokio
   `JoinError`, or a tokio task-budget stall needs one runtime's diagnostic
   surface. Two runtimes mean two `tokio-console`-equivalent toolchains and
   two sets of tracing conventions.

The #1780 audit confirmed `cargo tree` returns no async-std, smol, or
glommio entries; tokio is the sole async runtime in the workspace.

## Recommendation

Stay on tokio for daemon connection handling. The decision is recorded as
final and supports the design captured in #1934. No migration work is
warranted because:

1. Tokio is already the workspace's pinned async runtime (#1779), used by
   `core/embedded-ssh`, `core/async`, `daemon/async`, `engine/async`, and
   `transfer/async`.
2. The async daemon listener scaffold
   (`crates/daemon/src/daemon/async_session/`) targets tokio idioms
   (`tokio::select!`, `tokio::sync::Semaphore`, `tokio::spawn`,
   `tokio::signal::unix`).
3. Async-std is unmaintained since 2022 and disqualified on security and
   maintenance grounds.
4. Smol and glommio do not solve a problem the daemon has and would
   fragment the runtime story.
5. The workspace's single-runtime preference, verified by #1780, is upheld.

This audit closes #1590 as a documentation-only task. Future async runtime
decisions should reference this record before re-opening the question.

## Follow-up tasks

None. #1590 closes on merge. The active follow-ups belong to the daemon
event-loop multiplexing work (`daemon-event-loop-multiplexing.md`,
tasks #1676-#1683) and to the async daemon connection design (#1934).

## References

- Workspace tokio pin: `Cargo.toml:106` (comment), `Cargo.toml:188-189`
  (`tokio` and `tokio-util` declarations).
- Daemon tokio feature: `crates/daemon/Cargo.toml:19-20` (`async` feature),
  `crates/daemon/Cargo.toml:45` (optional dependency),
  `crates/daemon/Cargo.toml:67` (dev-dependency for tests).
- Core tokio feature: `crates/core/Cargo.toml:44` (optional dependency),
  `crates/core/Cargo.toml:90` (`embedded-ssh` feature),
  `crates/core/Cargo.toml:92-93` (`async` feature).
- Async daemon listener scaffold: `crates/daemon/src/daemon/async_session/`.
- Synchronous accept loop (status quo):
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `connection.rs`, `listener.rs`, `workers.rs`.
- Companion audit (event-loop choice):
  `docs/audits/daemon-event-loop-multiplexing.md`.
- Process-model rationale: `docs/DAEMON_PROCESS_MODEL.md`.
- Tokio 1.x: <https://docs.rs/tokio/1>.
- async-std 1.x (unmaintained): <https://docs.rs/async-std/1>.
- smol: <https://docs.rs/smol/2>.
- glommio: <https://docs.rs/glommio/0.9>.
