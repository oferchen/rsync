# Architecture Rationale

## 1. Goal

This document explains *why* oc-rsync's architecture diverges from upstream
rsync's C implementation in the places that matter. Component-level "what
each crate does" is documented elsewhere (`docs/ARCHITECTURE.md` and the
project agent reference). The focus here is the design intent behind the
divergences: the pressures that justified them, the alternatives rejected,
and the trade-offs accepted.

The guiding rule is narrow: oc-rsync mirrors upstream at the protocol and
CLI surface, and freely diverges below that surface where Rust idioms,
modern syscalls, and clean module boundaries deliver concrete wins.

## 2. Single binary, multiple modes (no separate `oc-rsyncd`)

Upstream ships `rsync` and `rsyncd` as the same executable selected by
argv[0] or `--daemon`. We keep that property: `oc-rsync` is the only
binary, with `--daemon` switching mode. Rationale:

- **Simpler distribution.** One executable to package, sign, ship, and
  install. Distros, container images, and the Homebrew formula track a
  single artifact. Operators upgrade once.
- **Shared codepath verification.** Client, server, and daemon all flow
  through `core::client::run_client` / `daemon::run`. Bugs surface in CI
  against a single binary instead of two that drift. The `--server`
  self-exec path is exercised by the same test matrix as `--daemon`.
- **Wire-compat preservation.** Upstream's symlink/argv0 contract is
  preserved: invoking the binary as `rsync` or `oc-rsyncd` does the
  expected thing, so existing ssh `rsync_path=` configurations and
  service files continue to work unchanged.

## 3. Crate decomposition (cli -> core -> engine|daemon|transport)

The workspace is split along a single axis: each crate owns one concern
and exposes a stable, testable API to its consumers. Two design forces
drove the split:

- **Unsafe-boundary isolation (closed in #1101).** Crates that touch the
  network or the command line (`cli`, `core`, `daemon`, `transfer`,
  `filters`, `protocol`) declare `#![deny(unsafe_code)]`. Platform FFI
  is consolidated in `fast_io`, with the small remaining `unsafe`
  surface confined to `metadata`, `checksums`, and `engine` for SIMD
  intrinsics, POSIX id lookups, and reflink ioctls. A memory-safety bug
  in the daemon or CLI is a compiler error, not a CVE.
- **Testability.** Every crate has its own unit and property tests.
  `checksums` parity tests pin SIMD against scalar; `protocol` golden
  byte tests pin wire output; `filters` snapshot tests pin rule
  evaluation. None of those tests need a network or a temp directory.
  Refactors that stay inside one crate do not recompile the rest of the
  workspace.

The layering reads top-down: `cli -> core -> {engine, daemon, transport,
protocol, filters, metadata, ...}`. There are no upward edges; cycles are
structurally prevented and detected by `cargo check`.

## 4. Wire-compat is not source-port

We are wire-compatible with upstream rsync 3.4.1 (protocol 32). We are
not a line-for-line C-to-Rust port. The two contracts are:

- **Protocol/CLI surface mirrors upstream exactly.** Same byte streams,
  same flag bits, same exit codes, same capability strings (`-e.LsfxCIvu`),
  same error message format. Verified by golden byte tests, exit-code
  parity tests, and the interop harness against upstream 3.0.9, 3.1.3,
  and 3.4.1.
- **Internals diverge freely.** Rayon for parallel stat batching,
  signature generation, and directory walks. io_uring on Linux 5.6+
  with automatic fallback. IOCP on Windows. SIMD checksums (AVX2, SSE2,
  NEON). Strategy pattern for checksum and codec dispatch. Builder
  pattern for `CoreConfig`, `TransferConfigBuilder`, and `FilterChain`.
  State machines for the daemon and transfer lifecycles. None of these
  appear on the wire; all are observable only through speed.

The rule: if changing an internal pattern would change a byte on the
wire, we don't do it without an upstream-compatible negotiation path.

## 5. Async-by-default trade-off

The default code path is synchronous: blocking I/O with rayon for CPU
work, threads for daemon concurrency. Tokio is opt-in via the `async`
feature on `core` and `daemon`, and the daemon uses tokio when that
feature is enabled.

We deliberately keep async out of the transfer hot loops. Per the
investigation tracked in #1779 / #1751, the generator/receiver/sender
pipelines are CPU- and syscall-bound, not concurrency-bound. The wire
protocol is in-order; adding async at that layer adds runtime overhead
and obscures the syscall profile we tune against upstream's. The async
feature exists for the daemon's accept loop and for embedders that need
many concurrent connections; the transfer path stays synchronous so
profiling, strace traces, and upstream comparisons remain meaningful.

## 6. Three-platform parity (Linux, macOS, Windows)

Every feature ships on every supported OS. The rule is enforced two
ways:

- **No `unimplemented!` / `todo!` on any platform.** A feature either
  has a real implementation or a documented stub that returns an
  upstream-compatible error or a graceful no-op. `cargo clippy
  --all-features` runs in CI for Linux musl, macOS, and Windows; any
  unimplemented path fails the build.
- **Alternative paths, not missing paths.** When a Linux syscall is not
  available, the same operation has a fallback: `copy_file_range` falls
  back to read/write, io_uring falls back to standard I/O, sendfile
  falls back to `std::io::copy`, POSIX ACLs map to Windows ACLs via
  `windows-rs`, AppleDouble has both real (macOS) and stub
  (Linux/Windows) implementations.

The result: a single source tree, three production targets, no
"Linux-only" features that quietly degrade on macOS or Windows.

## 7. Closing

For component-level details (per-crate responsibilities, public APIs,
dependency graph, external dependency table), see `docs/ARCHITECTURE.md`
and the project agent reference at the repo root. For deeper rationale
on specific subsystems, see `docs/architecture-rationale.md`,
`docs/DAEMON_PROCESS_MODEL.md`, and `docs/design-patterns-catalog.md`.
