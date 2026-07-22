# Architecture Rationale

This document explains the *why* behind the oc-rsync workspace layout: why
each crate exists, why the boundaries fall where they do, why specific
external dependencies were chosen over alternatives, why `core` is a facade,
why `fast_io` is the only crate permitted to contain unsafe code, and which
trade-offs were accepted along the way.

For a flat reference of crate responsibilities and the dependency graph, see
[`ARCHITECTURE.md`](ARCHITECTURE.md). For deeper architectural topics, see
[`architecture/parallelization.md`](architecture/parallelization.md) and
[`DAEMON_PROCESS_MODEL.md`](DAEMON_PROCESS_MODEL.md).

---

## 1. Why each crate exists

The workspace is divided along a single axis: each crate owns one well-defined
concern and exposes a stable, testable API to its consumers. The split lets
the test matrix exercise concerns in isolation, keeps compile times low for
incremental work, and prevents cross-cutting changes from rippling through
unrelated subsystems.

### Front-end and orchestration

- **`cli`** - Clap v4 argument parsing, help, exit-code routing, output
  formatting. Owns everything the user sees on stdout/stderr. Contains no
  transfer logic; it normalizes flags into a `ClientConfig` and delegates.
- **`core`** - Orchestration facade. Owns the public entry points
  (`run_client`, `ClientConfig`, `run_module_list`) consumed by `cli` and
  `embedding`. Re-exports `transfer` as `server` and `flist`/`rsync_io` so
  downstream binaries depend on a single crate name. See section 4 for why
  this layer exists.
- **`embedding`** - Self-exec orchestration for the `--server` flow.
  Encapsulates the only boundary that needs an `unsafe` `setenv` shim,
  isolating it from the rest of the workspace.

### Transfer pipeline

- **`transfer`** - Generator/receiver/sender pipelines, handshake, multiplex
  pump, network-to-disk SPSC channel, parallel stat batching. The serial wire
  protocol lives here because it must own thread topology decisions.
- **`engine`** - Local-copy executor, delta apply pipeline, directory walk,
  buffer pool, sparse handling, deferred fsync, temp-file commit. Operates on
  filesystem state without touching the wire.
- **`flist`** - Upstream-equivalent `flist.c`: file list construction and
  traversal. Split from `transfer` because the same data structure is used in
  daemon, client, and batch modes.
- **`signature`** - Block layout and rolling/strong checksum generation for
  the basis side of the delta algorithm.
- **`match`** - Block matching against incoming signatures. Kept separate so
  the matcher can be benchmarked, fuzzed, and replaced (zsync-inspired
  variants) without touching signature generation.
- **`batch`** - `--write-batch` / `--read-batch` recording and replay. Reuses
  the wire format but bypasses the transport layer.

### Wire protocol and transport

- **`protocol`** - Versions 28-32, multiplex framing, varint codec, `MSG_*`
  tags, file-list flag bits, NDX delta encoding. Pure parsing/encoding, no
  I/O. Wire format is verified by golden byte tests.
- **`rsync_io`** - Transport adapters: SSH subprocess, `rsync://` TCP,
  negotiation sniffing, embedded-ssh feature gate. Sits between `transfer`
  and the OS so that the pipeline does not care whether bytes travel through
  a `TcpStream`, a piped child, or russh.
- **`daemon`** - `oc-rsyncd.conf`, `@RSYNCD:` greeting, module dispatch, auth,
  thread-per-connection serve loop, optional async/tokio runtime, signal
  handling, sd-notify. Mode of `oc-rsync`, not a separate binary.

### Algorithms and data layer

- **`checksums`** - Adler32-style rolling sum and strong checksum dispatch
  (MD4/MD5/XXH3/XXH128/SHA*). SIMD fast paths (AVX2, SSE2, NEON) with scalar
  fallbacks. Owns runtime feature detection cached in `OnceLock`.
- **`compress`** - zlib/zstd/lz4 codec abstraction behind a `Compressor`
  trait. Codecs are feature-gated so a build can drop any combination.
- **`filters`** - `FilterChain`, include/exclude pattern compilation,
  `.rsync-filter` parsing, dir-merge handling. Pure, no I/O at evaluation
  time.
- **`bandwidth`** - Token-bucket limiter. Single-purpose crate so the same
  limiter can be wired into network and disk paths without shared state.
- **`metadata`** - Permissions, uid/gid, ns-mtime, atime/crtime, devices,
  FIFOs, symlinks, ACLs (POSIX via `exacl`, Windows via `windows-rs`),
  xattrs. The only Unix/Windows split that needs unsafe wrappers, contained
  to one crate.
- **`apple-fs`** - macOS-specific resource forks and AppleDouble. Isolated so
  the rest of the tree never sees `#[cfg(target_os = "macos")]`.
- **`platform`** - Cross-platform helpers (user/group lookup, signal mask).
  Crate-level `#![deny(unsafe_code)]` with per-function `#[allow]` only where
  unavoidable.

### I/O and infrastructure

- **`fast_io`** - High-performance I/O: io_uring, IOCP, `copy_file_range`,
  `sendfile`, `splice`, mmap, `CopyFileExW`, statx batching, parallel result
  fan-in. The single crate that wraps platform FFI behind safe APIs. See
  section 5.
- **`logging`** - Output formatting, verbosity flags, info/debug categories
  matching upstream `--info` / `--debug`.
- **`logging-sink`** - Newline policy, scratch-buffer reuse, log rotation.
  Split from `logging` because the daemon and the client wire sinks
  differently and the policy code is non-trivial.
- **`branding`** - Version strings, program names, packaging metadata.
  Single-source-of-truth so renaming the binary stays a one-line change.
- **`windows-gnu-eh`** - Windows GNU exception-handling shim, only ever
  pulled in for the `windows-gnu` target.
- **`test-support`** - Shared fixtures (`setup_test_dirs`, `EnvGuard`,
  golden harnesses). Lives in the workspace so unit tests in every crate can
  use it without duplication.

---

## 2. Why crate boundaries fall where they do

Three rules drove the split:

1. **One concern per crate.** A crate owns a single subsystem (checksums,
   filtering, compression, transport). Mixing concerns produces monolithic
   modules that are hard to test and slow to compile.
2. **No upward dependencies.** Lower-level crates never depend on higher-level
   ones. `protocol` does not know about `transfer`; `metadata` does not know
   about `engine`. Cycles are detected by `cargo check` and structurally
   prevented by the layering. This is what makes `core` a true facade rather
   than a god-crate.
3. **Feature gating at the boundary.** Compression backends, async runtime,
   ACL, xattr, embedded-ssh, io_uring, IOCP, and SIMD all flip at the crate
   boundary. A consumer can switch zlib for zstd without recompiling
   `transfer`, and the daemon can be built without async by toggling a
   single feature on `core`.

The dependency layering reads top-down:

```
cli  -> core  -> {engine, transfer, daemon, protocol, filters, ...}
core -> fast_io, rsync_io, branding, logging
engine, transfer -> protocol, checksums, compress, metadata, fast_io
protocol -> checksums, filters, compress, bandwidth
daemon -> core, protocol, metadata, logging-sink
```

A practical consequence: edits in `checksums` or `metadata` recompile a
fraction of the workspace. Edits in `protocol` recompile the wire stack but
not `cli`. Only edits in `core` or `cli` recompile the binary entry point.

---

## 3. Why we chose certain crates over alternatives

External dependencies are chosen on three criteria: measured advantage over
the standard library, audit / maintenance posture, and feature gating so a
release can drop any of them without code changes.

### `crossbeam-channel` over `std::sync::mpsc`

Used by `engine` and `transfer` for the network-to-disk pipeline.

- The pipeline runs at line rate: tens of thousands of `FileMessage` items
  per second cross the channel. `std::sync::mpsc` uses a `Mutex` plus
  `Condvar` per send/recv, which adds futex syscalls on contention.
- `crossbeam-channel`'s bounded variant is implemented over an
  `ArrayQueue` and avoids syscalls on the fast path. The disk-commit
  channel additionally uses `crossbeam_queue::ArrayQueue` directly for a
  pure-userspace SPSC ring with `std::hint::spin_loop` waits.
- Profiling on Linux x86_64 showed measurable reduction in per-message
  syscall overhead under the small-files workload that dominates real
  rsync traffic.

### `rayon` over a custom thread pool

Used by `flist`, `signature`, `transfer`, `engine`, `checksums`, `fast_io`.

- A custom pool would have to reimplement work-stealing, adaptive
  sizing, and panic propagation - all features `rayon` already provides
  and that are exercised by its production users.
- `rayon`'s `par_iter` integrates with the threshold-based dual-path
  pattern (sequential below `PARALLEL_STAT_THRESHOLD`, parallel above)
  with no glue code.
- The `parallel` feature can disable rayon entirely for embedded or
  single-core targets.

A custom pool would be on the table only if `rayon` became a
compile-time bottleneck or if it pulled in deps incompatible with `musl`,
neither of which is the case.

### `tokio` only behind a feature

Default builds are synchronous. Tokio is opt-in via the `async` feature
on `core` and `daemon`, and via `embedded-ssh` for russh.

- The synchronous path is the production path; it is simpler, has no
  runtime initialization cost, and matches upstream rsync's process
  model more closely.
- Async mode exists for operators who run thousands of concurrent
  connections on a single daemon process, where stack-per-thread becomes
  expensive. Most deployments do not need it.
- Gating `tokio` behind a feature keeps the dependency tree, binary
  size, and audit surface small for the default build. Workloads that
  do not opt in never link the runtime.

### `russh` (`embedded-ssh`) only behind a feature

The default SSH transport spawns the system `ssh` binary. `russh` is
behind the `embedded-ssh` feature so:

- Operators retain control over `ssh_config`, `ControlMaster`, agent
  forwarding, and key handling.
- The default binary works on systems without a Rust SSH stack and
  matches upstream rsync's behaviour exactly.
- Embedded-SSH is available when an in-process implementation is
  required (sandboxed environments, builds without `/usr/bin/ssh`).

### `exacl` and `windows-rs` over hand-rolled FFI

ACL handling is platform-specific and historically a source of unsafe
code. We use:

- `exacl` for POSIX ACLs on Linux, macOS, FreeBSD - audited, single
  trait surface across platforms.
- `windows-rs` (Microsoft's official binding generator) for Windows
  ACLs, registry, IOCP, and console APIs. Replaces hand-written
  `windows-sys` calls that would otherwise live in `daemon` and `cli`.

Both crates eliminate `unsafe` blocks from `metadata` and `cli`, leaving
only the small set of FFI calls that must live in `fast_io` or
`metadata`'s permitted blocks.

### jemalloc / mimalloc as the global allocator

Linked at the binary level, selected per platform: jemalloc on Unix,
mimalloc on Windows (which lacks comparable jemalloc support). Measured
8-50% throughput improvement over the system allocator on small-file
workloads (where allocator contention is the bottleneck). On Unix,
jemalloc is configured at allocator init via a compile-time
`malloc_conf` static (`dirty_decay_ms:0,muzzy_decay_ms:0`) so freed
pages are returned to the OS promptly, bounding resident memory at
scale (~45 MB -> ~30 MB on a 100k-file local copy) without a process
re-exec. The allocator is selected at the binary boundary so embedders
can swap it out.

### `jwalk`, `rustc-hash`, `dashmap`

Each replaces a standard-library type only where measured to be faster:

- `jwalk` is ~4x faster than `walkdir` for the directory-walk hot path
  in `flist`.
- `rustc-hash` (FxHash) is 2-5x faster than `SipHash` for integer keys
  in inode-deduplication and hardlink tables. Not used for any
  attacker-controlled input.
- `dashmap` provides concurrent shared state in the daemon without the
  reader-writer-lock contention of `RwLock<HashMap>`.

---

## 4. Why `core` is a facade

`core` exposes a small set of orchestration entry points and re-exports the
crates a binary needs: `run_client`, `ClientConfig`, `run_module_list`, plus
`pub use ::transfer as server`, `pub use ::flist`, `pub use rsync_io as io`.
Everything else is implementation detail.

The reasons for the facade:

- **Single integration point.** Both `cli` and `embedding` go through
  `core::client::run_client`. There is exactly one place to wire a new
  flag, one place to plumb a new transport, one place to match upstream's
  `start_client()` lifecycle.
- **Stable surface for embedders.** External callers (Homebrew formula,
  Docker entry points, the embedding API) link against `core`. Internal
  refactors of `transfer` or `engine` do not break them.
- **Re-exports flatten the dependency graph for callers.** A binary that
  depends on `core` does not need to also depend on `transfer`,
  `flist`, or `rsync_io`. The version pinning lives in one place.
- **Error and exit-code centralization.** `ClientError` and the exit-code
  table live in `core`. Every transfer path produces the same error
  envelope, mapped 1:1 to upstream rsync exit codes.
- **No business logic.** `core` orchestrates. It does not parse the wire
  protocol, walk the tree, or apply metadata - those belong to
  `protocol`, `flist`, and `metadata`. The facade pattern is honoured:
  `core` is a thin layer over the crates that do the work.

If `core` ever grows transfer logic itself, that is a smell to factor out.

---

## 5. Why `fast_io` is the only crate permitted to contain unsafe

The workspace unsafe-code policy points in one direction: consolidate all
unsafe code into `fast_io`. Today a small set of crates retain
`#[allow(unsafe_code)]` on specific functions for historical reasons
(`metadata` for POSIX id lookups, `checksums` for SIMD intrinsics, `engine`
for clonefile and atomic buffer-pool counters, `protocol` for multiplex
frame helpers, `embedding` for the `setenv` shim). New unsafe code goes to
`fast_io`.

The reasoning:

- **Auditability.** Every `unsafe` block is justified with a safety comment
  citing which invariants hold. Containing unsafe to one crate means a
  reviewer can read every justification in one pass.
- **Test isolation.** `fast_io` has both an optimized path and a safe
  fallback for every operation (`copy_file_range` falls back to
  read/write, io_uring falls back to standard I/O, mmap falls back to
  buffered reads). Tests cover both paths. Containing unsafe here means
  the fallback discipline is enforced in one place.
- **Cross-platform discipline.** Platform-specific code (`io-uring`,
  `windows-sys`, `memmap2`, `rustix`, `libc`) is gated by `#[cfg(...)]`
  inside `fast_io`. Consumers see a single safe API and never write a
  `#[cfg(target_os = "linux")]` block themselves.
- **Daemon and CLI safety.** `daemon`, `cli`, `core`, `transfer`,
  `filters`, `protocol`, and the rest declare `#![deny(unsafe_code)]`.
  These are the surfaces an attacker reaches first (TCP, command-line,
  filter rules, wire frames). Keeping them unsafe-free means a memory
  safety bug in one of them is a compiler error, not a CVE.
- **One place to change FFI.** When a Rust release adds a safe wrapper
  for an unsafe API, only `fast_io` needs an update. When a platform
  drops a syscall, only `fast_io` needs a fallback path.

The invariants `fast_io` upholds are documented in its crate-level
rustdoc: valid file descriptors, proper buffer lifetimes, no data races on
`Send`-implemented fd wrappers, graceful fallback on every optimization.

---

## 6. Trade-offs accepted

Every architectural choice has costs. The ones we accepted:

### Per-process daemon model (no fork)

oc-rsync's daemon spawns a thread per connection (sync mode) or a
`tokio::spawn` task per connection (async mode), not a `fork()` child like
upstream. Trade-offs:

- Threads share a single address space. A memory-corruption bug could in
  principle propagate across sessions. Mitigated by `#![deny(unsafe_code)]`
  in the daemon crate, by routing all platform FFI through `fast_io`, and by
  wrapping each session in `catch_unwind` so a panic does not abort the
  process.
- No per-session PID. Operators cannot `kill <session-pid>`. Mitigated by
  graceful shutdown via SIGTERM and per-session cancellation in the
  daemon's session table.
- No automatic OS resource cleanup on session crash. Mitigated by Rust's
  `Drop` impls and by reaping `SshChildHandle` to prevent zombies.

In return: cross-platform portability (Windows has no `fork`), lower
per-connection overhead (no page-table copy), and shared `Arc` state
without IPC. See [`DAEMON_PROCESS_MODEL.md`](DAEMON_PROCESS_MODEL.md) for
the full comparison.

### Wire protocol throughput does not scale linearly with cores

The rsync wire protocol is in-order: file indices are sequential, the
sender emits deltas in that order, the receiver acknowledges in that
order. The network-facing parts of generator, receiver, and sender are
single-threaded by design. Adding cores helps only the CPU-bound work
(checksum computation, signature generation, parallel stat) that runs
off the wire critical path. On a 32-core box transferring many small
files, scaling stops at ~3 threads. Out-of-order transfer would require
a wire-protocol extension; we deliberately do not break wire
compatibility with upstream 3.4.4. See
[`architecture/parallelization.md`](architecture/parallelization.md).

### One SSH process per transfer

The default SSH transport spawns one OS process per transfer, matching
upstream's `do_cmd()`. Two concurrent transfers to the same host do two
handshakes and two authentications. Users who care can enable
OpenSSH `ControlMaster` outside of oc-rsync. The `embedded-ssh` feature
offers an in-process alternative behind russh, accepting the larger
dependency footprint as a cost.

### Synchronous default, async opt-in

The default code path is synchronous I/O with rayon for CPU work.
`tokio` is behind a feature flag. This keeps the default binary small,
the dependency graph short, and the runtime predictable. Operators who
need thousands of concurrent daemon connections enable `async` and pay
the runtime startup cost; everyone else gets the lean path.

### Buffer pool uses a `Mutex<Vec<Vec<u8>>>`

The buffer pool in `engine` uses a mutex around a vector of vectors.
Under extreme concurrency this could become a contention point.
Measured throughput shows it is not the bottleneck today; if it ever
becomes one, the path forward is per-thread pools or a lock-free
alternative. We accepted simplicity now, traded for a known migration
path.

### `core` re-exports widen the public API surface

Because `core` re-exports `transfer`, `flist`, and `rsync_io`, those
crates' public APIs are visible through `core`. A breaking change in
`transfer` is a breaking change for embedders. We accepted this in
exchange for a single integration point; the alternative (a fully
hand-rolled facade that duplicates types) would have been worse for
maintenance.

### Crate count is high (~25 crates)

A larger workspace has more `Cargo.toml` files to keep in sync and
more boundaries to cross when refactoring. We accepted this in
exchange for compile-time isolation, parallel build of independent
crates, and the ability to publish individual crates if needed. The
boundaries are the value, not the cost.

---

## See also

- [`ARCHITECTURE.md`](ARCHITECTURE.md) - flat reference of crate
  responsibilities and dependency graph.
- [`DAEMON_PROCESS_MODEL.md`](DAEMON_PROCESS_MODEL.md) - thread-per-connection
  rationale and isolation guarantees.
- [`architecture/parallelization.md`](architecture/parallelization.md) -
  what runs in parallel and what is intentionally serial.
- [`PROTOCOL.md`](PROTOCOL.md) - wire format details.
- [`UPSTREAM_COMPARISON.md`](UPSTREAM_COMPARISON.md) - point-by-point
  comparison with upstream rsync 3.4.4.
