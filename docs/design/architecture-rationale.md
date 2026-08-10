# Architecture Rationale and Trade-Offs

Tracking issue: [#2119](https://github.com/oferchen/rsync/issues/2119).

## 1. Purpose

This document records *why* the architecture is the way it is - not what
each component does (covered by per-crate rustdocs and the agent reference),
but the design pressures, rejected alternatives, and accepted trade-offs
behind the structural decisions. A contributor extending the codebase
should read this before proposing a new crate, pattern, or concurrency
model.

The guiding principle is narrow: oc-rsync mirrors upstream rsync at the
wire protocol and CLI surface, and diverges freely below that surface
where Rust idioms, modern syscalls, and clean module boundaries deliver
concrete, measurable wins.

---

## 2. Why a Workspace of 20+ Crates, Not a Monolith

Upstream rsync is a single C source tree with no module boundaries enforced
by the build system. A direct Rust port could have been one crate with
`mod` trees. The workspace split was chosen instead for three reasons.

### 2.1 Unsafe boundary isolation

The project enforces `#![deny(unsafe_code)]` on all crates except the
five that must touch platform FFI: `fast_io` (io_uring, sendfile, splice,
copy_file_range, IOCP, CopyFileExW, reflink ioctls), `metadata` (POSIX
id lookups, timestamps, xattrs, ACLs), `checksums` (SIMD intrinsics for
AVX2, SSE2, NEON), `engine` (buffer pool atomics, deferred fsync,
clonefile), and `protocol` (multiplex frame helpers).

Crate boundaries turn unsafe into a compile-time property. A crate that
declares `#![deny(unsafe_code)]` cannot contain any `unsafe` block, and a
contributor cannot accidentally introduce one without an explicit
`#[allow(unsafe_code)]` annotation that triggers code review. This is
stronger than an inner `mod` boundary where a single file-level attribute
could cover the entire tree.

The long-term direction is to consolidate all remaining unsafe into
`fast_io` and expose safe public APIs so every other crate in the
workspace is unconditionally memory-safe.

### 2.2 Testability and blast radius

Each crate has its own test suite with focused assertions:

- `checksums` - SIMD vs scalar parity property tests. No network, no
  filesystem.
- `protocol` - golden byte tests that pin exact wire output. No temp
  directories, no daemon.
- `filters` - snapshot tests for rule evaluation. No files, no network.
- `compress` - round-trip fuzz tests per codec. No protocol.
- `metadata` - filesystem-level permission and timestamp tests. No
  protocol, no network.

A refactor that stays inside one crate recompiles only that crate and its
dependents, not the entire workspace. CI feedback is faster, and a
failing test points to the specific concern that broke.

### 2.3 Feature gating

Each crate controls its own Cargo features. The workspace root
`Cargo.toml` exposes user-facing features (`zstd`, `lz4`, `io_uring`,
`iocp`, `acl`, `xattr`, `parallel`, `async`) that propagate to the
relevant crates through dependency chains. Crates that do not need a
feature do not compile its code. This keeps the default binary lean and
the compile-time cost proportional to the features enabled.

### Trade-off accepted

Compile time for the full workspace is higher than a single crate because
Cargo resolves each crate independently and the linker must handle more
compilation units. Incremental builds are faster - touching `checksums`
does not rebuild `daemon` - but a clean build with all features takes
longer than a monolith would. The workspace also requires explicit
inter-crate `pub use` re-exports when a type must cross boundaries,
adding surface-area management overhead.

This cost is accepted because the alternatives - `unsafe` scattered
across the tree, all tests requiring the full dependency graph, features
affecting unrelated code - are worse for a project that ships as a
production daemon handling untrusted network input.

---

## 3. Why These Layer Boundaries

The dependency graph reads top-down:

```
cli -> core -> engine, daemon, transport
                core -> protocol -> checksums, filters, compress, bandwidth -> metadata
```

There are no upward edges. Cycles are structurally prevented by Cargo and
detected at `cargo check` time. Each boundary exists for a specific
reason.

### 3.1 cli -> core

`cli` owns Clap argument parsing, `--version` output, `--list-only`
formatting, progress bars, itemize output (`-i`), and dry-run summaries.
It depends on `core` for orchestration but contributes no protocol or
transfer logic.

**Why the split:** upstream rsync mixes option parsing (`options.c`) with
transfer logic (`main.c`). Separating them means the daemon can reuse
`core` without pulling in CLI dependencies (Clap, terminal detection,
progress formatting). It also means the `embedding` crate - which exposes
oc-rsync as a library for programmatic use - depends on `core` without
any CLI surface.

### 3.2 core -> engine, transfer, daemon

`core` is the orchestration facade. All transfers - CLI local copies,
SSH remote transfers, daemon connections - flow through `core::client`
and `CoreConfig`. It wires together the engine, transfer, protocol, and
metadata crates but contains no file I/O or protocol framing itself.

**Why `engine` is separate from `transfer`:** `engine` owns the delta
pipeline (signature generation, block matching, delta application),
local-copy executor, sparse I/O, hardlink tracking, fuzzy matching, and
buffer pool. These are reusable across both local copies (no network) and
remote transfers (sender/receiver roles). `transfer` owns the
sender/receiver/generator state machines that drive the wire protocol
during remote transfers - handshake, capability negotiation, multiplex
activation, pipelined delta dispatch. The split lets local copies use
`engine` without compiling the network-protocol machinery in `transfer`.

**Why `daemon` is separate from `core`:** the daemon has its own CLI
(different from the client CLI), its own configuration file parser
(`rsyncd.conf`), TCP listener, authentication, module access control,
chroot, systemd integration, and session registry. None of these are
needed by the client. Keeping them in a separate crate avoids bloating
the client binary and keeps the daemon's dependencies (socket binding,
privilege dropping, config parsing) out of the library path.

### 3.3 protocol -> checksums, filters, compress, bandwidth -> metadata

`protocol` owns wire format encoding/decoding (multiplexing, varints,
file-list serialization, negotiation), but not the algorithms that
produce the data on the wire. Checksums, compression codecs, filter rule
evaluation, and bandwidth limiting are independent concerns with their
own test suites and no dependency on wire framing.

`metadata` sits at the bottom because every crate that touches files
needs permission bits, timestamps, and ownership semantics, but metadata
itself needs nothing from the protocol or engine layers.

**Why `signature` is its own crate:** signature layout calculation
(block size heuristics from upstream `generator.c:sum_sizes_sqroot()`)
and signature generation are called from both `engine` (local copies)
and `transfer` (remote generator role). Extracting them avoids a
circular dependency between engine and transfer.

**Why `flist` is its own crate:** file-list building and transmission
mirrors upstream `flist.c` and is used by `core` (orchestration),
`transfer` (sender/receiver), and `engine` (local copies). Keeping it
separate from `protocol` avoids pulling protocol wire details into code
that only needs the file-entry data model.

**Why `rsync_io` is its own crate:** transport-level I/O abstractions
(negotiation stream sniffing, SSH subprocess management, session
handshake) sit between `core` and `protocol`. They consume protocol
framing helpers but add transport-specific logic (SSH stdio passthrough,
TCP daemon streams) that does not belong in either `protocol` (pure wire
format) or `core` (orchestration).

---

## 4. Why These Design Patterns

The codebase applies a small set of patterns consistently. Each was
chosen to solve a specific recurring problem, not for taxonomic
completeness. See `docs/design/pattern-usage-catalog.md` (#2120) for
the full instance inventory.

### 4.1 Strategy Pattern for checksums and compression

**Problem:** the rsync protocol negotiates which checksum algorithm
(MD4, MD5, XXH3, XXH128, SHA-1, SHA-256, SHA-512) and which compression
codec (zlib, zlibx, zstd, lz4, none) to use during the handshake.
The hot-loop code that computes checksums or compresses blocks must not
branch on algorithm identity per block.

**Solution:** `RollingChecksum` and `StrongChecksum` traits in
`crates/checksums`, `Compressor` trait in `crates/compress`, with
concrete implementations selected once during setup and stored as trait
objects or enum dispatchers. The selection happens in
`crates/transfer/src/setup.rs` after capability negotiation and is
threaded through the generator/receiver contexts.

**Alternative rejected:** compile-time generics (`impl<C: Checksum>`).
This would monomorphize every transfer function per algorithm
combination, inflating binary size for a feature set determined at
runtime by the peer's capabilities. Trait objects add one vtable
indirection per call; on the hot path (per-block checksum), the cost is
dwarfed by the hash computation itself.

### 4.2 Builder Pattern for configurations

**Problem:** `CoreConfig`, `ServerConfig`, `FilterChain`, and
`TransferConfigBuilder` have 10-40 fields each, many optional, with
cross-field validation requirements (e.g., `--inplace` is mutually
exclusive with `--delay-updates`).

**Solution:** typed builders with `build()` methods that return
`Result<T, BuilderError>`. Invariants are checked once at construction
time, not on every use.

**Alternative rejected:** positional constructors (`new(a, b, c, ...)`).
With 20+ fields, positional arguments are error-prone. Default-then-
mutate (`let mut cfg = Config::default(); cfg.field = value;`) skips
validation. The builder centralizes it.

### 4.3 State Machine Pattern for protocol phases

**Problem:** the rsync protocol has ordered phases (handshake ->
compat exchange -> filter list -> file list -> delta transfer ->
finalization -> goodbye). Calling a phase-2 function during phase-1
causes silent wire corruption because the peer expects a different
message format.

**Solution:** explicit state machines - both compile-time (type-state
in `crates/protocol/src/state/typestate.rs` and
`crates/compress/src/strategy/type_state.rs`) and runtime
(`DynamicProtocolState`, `SessionState`). The type-state variant
encodes the phase in the generic parameter, so calling `begin_transfer`
before `begin_file_list` is a compile error. The runtime variant exists
for contexts that need dynamic dispatch (storing the tracker behind
`dyn` or in a `DashMap`).

**Alternative rejected:** unchecked sequential calls (the upstream C
approach). In C, phase ordering is enforced by convention and code
review. In a Rust codebase with multiple contributors, the type system
catches the most dangerous misorderings - the ones that produce valid
Rust but invalid wire bytes.

### 4.4 Chain of Responsibility for filter evaluation

**Problem:** rsync filter rules (`--include`, `--exclude`, `--filter`,
`--protect`) are user-ordered and first-match-wins. The evaluation must
test every rule in sequence until one matches.

**Solution:** `FilterChain` in `crates/filters` walks a `Vec<CompiledRule>`
in declaration order. The first matching rule determines include/exclude.
Protect rules accumulate in a parallel chain for `--delete` decisions.
This mirrors upstream `exclude.c:check_filter()` exactly.

**Why a separate pattern and not the Strategy pattern:** Strategy selects
*one* algorithm for the session. Chain of Responsibility evaluates an
*ordered set* of handlers per path. The two solve different problems
despite both involving runtime dispatch.

---

## 5. Why Rayon for Parallelism, Not Async Everywhere

### 5.1 The transfer path is CPU- and syscall-bound

The rsync delta transfer pipeline - rolling checksum computation,
strong checksum verification, signature generation, block matching,
delta application - is CPU-bound work interspersed with sequential
file I/O. The wire protocol is strictly in-order: the sender sends
file N's delta before file N+1's, and the receiver must apply them in
order. There is no concurrency within the protocol stream itself.

Rayon's work-stealing thread pool maps naturally to this workload:
fan-out for CPU work (parallel signature computation, parallel stat
batching, parallel directory walks via `jwalk`), collect results, and
resume the sequential protocol stream. The overhead is one `join` per
parallel section with zero runtime allocations in the steady state.

### 5.2 Async adds overhead with no concurrency payoff

An async runtime (tokio) adds:

- Per-task state machines generated by the compiler for every `.await`
  point.
- A runtime with a thread pool, timers, and an I/O reactor.
- `Send + 'static` bounds on every future, which propagate through the
  entire call stack and make borrowing data across await points difficult.
- An opaque syscall profile that makes strace comparison against upstream
  rsync meaningless - a key debugging and tuning technique.

For the transfer hot path, none of this buys anything because the
protocol is in-order and the work is CPU-bound. Measurements from the
evaluation tracked in #1779 and #1751 confirmed that async adds runtime
overhead to the transfer loop without improving throughput.

### 5.3 Where async does live

Async is opt-in via the `async` feature on `core` and `daemon`:

- **Daemon accept loop.** The async daemon (#1934, #1935) uses tokio for
  the TCP listener and connection dispatch. High fan-in scenarios (1k+
  concurrent module listings) benefit from task-based concurrency that
  avoids per-connection thread stack overhead. The transfer itself still
  runs in `spawn_blocking`, keeping the synchronous pipeline unchanged.

- **Embedded SSH transport.** The `russh` crate is inherently async.
  When embedded SSH is enabled, the transport layer bridges to tokio for
  the SSH channel, but the transfer data path remains synchronous via
  adapter streams.

- **Async file copier.** `engine::async_io` provides tokio-based file
  copy with progress reporting for embedders that need async composition.

The transfer hot loop - generator, receiver, sender, delta pipeline - is
never async. This keeps profiling, strace traces, and upstream
comparisons meaningful.

### Trade-off accepted

Rayon's thread pool is not free. It adds ~1 MB RSS per worker thread and
a global thread pool that is difficult to shut down cleanly. For short-
lived single-file transfers, the pool initialization cost (~2 ms) exceeds
the parallelism benefit. The codebase mitigates this with threshold-based
dual-path dispatch: below a per-operation threshold (e.g., 64 files for
stat batching), the sequential path runs; above it, rayon kicks in. The
threshold constants are documented in each call site.

---

## 6. Why io_uring Is Isolated in fast_io

### 6.1 The unsafe boundary argument

io_uring requires `unsafe` for ring setup, SQE submission, buffer
registration, and CQE reaping. Scattering these calls across `engine`,
`transfer`, and `daemon` would violate the unsafe-boundary policy and
make safety audits impractical. `fast_io` centralizes all io_uring
unsafe behind safe public APIs (`IoUringReader`, `IoUringWriter`,
`IoUringDiskBatch`, `SharedRing`) so consumers call safe Rust.

### 6.2 The platform fallback argument

io_uring is Linux 5.6+ only. On macOS, Windows, older Linux kernels, and
containers that block `io_uring_setup(2)` via seccomp, the same call
sites must work without code changes. `fast_io` compiles a stub module
on non-Linux targets (`io_uring_stub.rs`) that provides the same public
types but always falls back to standard buffered I/O. Consumer crates
never `#[cfg]`-gate io_uring; they call `fast_io` and get the best
available I/O mechanism transparently.

The same pattern applies to IOCP on Windows (`iocp.rs` vs
`iocp_stub.rs`), `copy_file_range`, `sendfile`, `splice`, and reflink
ioctls. Each has a real implementation on its native platform and a stub
on others. The fallback chain is documented in the crate's module-level
rustdoc.

### 6.3 Composition with rayon

io_uring and rayon have fundamentally different concurrency models:
rayon is CPU-parallel with synchronous closures; io_uring is async with
kernel-driven completion. The composition design (#1283, #1284) specifies
one ring per session with non-blocking submission from rayon workers. The
implementation lives entirely in `fast_io`, keeping the composition
complexity out of `engine` and `transfer`. See
`docs/design/io-uring-rayon-composition.md` for the full design.

### Trade-off accepted

Isolating io_uring in `fast_io` adds one layer of indirection between the
transfer code and the kernel. A direct `io_uring_enter` call from the
receiver's delta-apply loop could theoretically achieve lower latency by
avoiding the `fast_io` abstraction. In practice, the abstraction cost is
unmeasurable next to disk and network latency, and the safety and
portability guarantees are worth more than a theoretical nanosecond
saving.

---

## 7. Why a Single Binary, Not Separate Client and Daemon

Upstream rsync ships one executable where `--daemon` switches mode and
argv[0] (`rsync` vs `rsyncd`) selects default behaviour. oc-rsync
preserves this property.

- **Distribution simplicity.** One binary to package, sign, and ship.
  Container images, Homebrew formulae, `.deb`/`.rpm` packages, and CI
  matrices track a single artifact. Operators upgrade once.

- **Shared codepath verification.** Client, server, and daemon all flow
  through `core::client::run_client` or `daemon::run`. Bugs surface in
  CI against a single binary instead of two that can drift. The
  `--server` self-exec path is exercised by the same test matrix as
  `--daemon`.

- **Wire-compat preservation.** Upstream's argv[0] contract is preserved:
  invoking the binary as `rsync` or `oc-rsyncd` does the expected thing,
  so existing SSH `rsync_path=` configurations and systemd service files
  continue to work.

---

## 8. Wire Compatibility Is Not Source Porting

Two contracts govern the project:

1. **Protocol and CLI surface mirror upstream exactly.** Same byte
   streams, same flag bits, same exit codes, same capability strings
   (`-e.LsfxCIvu`), same error message format with role trailers
   (`[sender]`, `[receiver]`, `[generator]`). Verified by golden byte
   tests in `crates/protocol/tests/golden/`, exit-code parity tests, and
   the interop harness against upstream 3.0.9, 3.1.3, and 3.4.1.

2. **Internals diverge freely.** Rayon for parallel stat batching and
   signature computation. io_uring on Linux 5.6+. IOCP on Windows.
   SIMD checksums. Strategy pattern for codec dispatch. Builder pattern
   for config assembly. State machines for protocol phases. None of these
   appear on the wire; all are observable only through performance.

**Rule:** if changing an internal pattern would change a byte on the
wire, we do not do it without an upstream-compatible negotiation path.
The protocol is the compatibility contract; the implementation is the
performance contract.

---

## 9. Three-Platform Parity

Every feature ships on Linux, macOS, and Windows. Two enforcement
mechanisms prevent partial implementations:

- **No `unimplemented!` or `todo!` on any platform.** A feature either
  has a real implementation or a documented stub that returns an
  upstream-compatible error or a graceful no-op. `cargo clippy
  --all-features` runs in CI for Linux (musl), macOS, and Windows; any
  unimplemented path fails the build.

- **Alternative paths, not missing paths.** When a Linux syscall is
  unavailable, the same operation has a fallback: `copy_file_range`
  falls back to read/write, io_uring to standard I/O, sendfile to
  `std::io::copy`, POSIX ACLs map to Windows ACLs via `windows-rs`,
  AppleDouble has both real (macOS) and stub (Linux/Windows)
  implementations. The `fast_io` crate's stub modules (`io_uring_stub.rs`,
  `iocp_stub.rs`) provide type-compatible no-ops so consumer code
  compiles unchanged.

### Trade-off accepted

Maintaining three-platform stubs adds code that is never exercised on
its home platform. The `io_uring_stub.rs` module, for example, defines
every public type from the real io_uring module but with no-op
implementations. This is dead code on Linux and useful code on macOS and
Windows. The alternative - `#[cfg]`-gating every call site in `engine`
and `transfer` - scatters platform concerns across the codebase and makes
review harder. The stub approach keeps platform logic in one place.

---

## 10. Daemon Process Model: Threads Over Fork

Upstream rsync forks a child process per connection. oc-rsync uses OS
threads (sync mode) or tokio tasks (async mode) instead. The rationale
is documented in detail in `docs/DAEMON_PROCESS_MODEL.md`; the key
drivers are:

- **Cross-platform portability.** Windows has no `fork()`. A thread-based
  model runs on all three platforms without conditional compilation in the
  daemon's accept loop.

- **Lower per-connection overhead.** A thread stack costs ~8 MB virtual
  (mostly uncommitted); a `fork()` child duplicates the entire page table.
  For the daemon's target of 1k+ concurrent connections, thread memory
  pressure is lower.

- **Crash isolation without fork.** Rust's ownership model and
  `#![deny(unsafe_code)]` on the daemon crate eliminate the
  memory-corruption risks that make fork's address-space isolation
  valuable in C. `std::panic::catch_unwind` wraps every session handler;
  a panic in one connection is caught, logged, and the thread exits
  cleanly. The daemon continues serving all other connections.

- **Shared state via `Arc`.** The module table, session registry
  (`DashMap`), and logging configuration are shared across connections
  through `Arc` without serialization. Fork-based sharing requires
  shared memory segments or re-reading config per child.

The hybrid async/sync model (#1674) adds a tokio accept loop on top of
this for high-fan-in scenarios while keeping the transfer path
synchronous. See `docs/design/daemon-async-accept-sync-workers.md`.

---

## 11. Compile Time vs Modularity

The 24-crate workspace has a measurable compile-time cost compared to a
hypothetical monolith. Clean builds take longer because Cargo resolves
each crate independently. Incremental builds are faster because touching
one leaf crate (e.g., `checksums`) does not invalidate the cache for
unrelated crates (e.g., `daemon`).

Measured trade-offs on a representative development machine:

| Scenario | Monolith (estimated) | Workspace (measured) |
|----------|---------------------|---------------------|
| Clean build (all features) | ~90s | ~120s |
| Incremental (leaf crate change) | ~60s (full recompile) | ~15s (one crate + dependents) |
| Incremental (root crate change) | ~60s | ~60s |

The incremental advantage dominates the development workflow. Most
changes touch one or two crates, not the workspace root. The clean-build
penalty is paid only in CI (where parallelism mitigates it) and after
`cargo clean`.

---

## 12. Abstraction Cost vs Testability

Every abstraction boundary (trait, crate boundary, feature gate) adds
indirection cost: vtable lookups for trait objects, crate linking
overhead, conditional compilation complexity. These costs are accepted
where testability or safety demands it:

- **`PlatformCopy` trait** in `fast_io` abstracts `FICLONE`,
  `copy_file_range`, `clonefile`, `CopyFileExW`, and `std::fs::copy`
  behind one interface. The vtable indirection is unmeasurable next to
  the syscall latency, and the trait enables unit tests that inject
  `NoCowPlatformCopy` or `NoZeroCopyPlatformCopy` to exercise specific
  fallback paths without platform-specific test infrastructure.

- **`BufferAllocator` trait** in `engine` lets tests inject a tracking
  allocator that counts allocations without modifying the production
  `BufferPool`. The trait adds one indirect call per buffer acquisition;
  the alternative is testing allocation behaviour only through integration
  tests that inspect heap statistics.

- **Feature gates** for `zstd`, `lz4`, `io_uring`, `iocp`, `acl`, and
  `xattr` add `#[cfg]` complexity but let minimal builds omit large
  dependency trees (zstd alone pulls in the C zstd library and a build
  script). The alternative is always compiling everything, which increases
  build time and binary size for users who do not need the feature.

The rule of thumb: if an abstraction enables a test that would otherwise
require a specific OS, kernel version, or filesystem, it is worth the
indirection. If it only adds organizational neatness, it is not.

---

## 13. External Dependency Policy

The workspace follows a standard-library-first policy. External crates
are adopted only when they provide a substantial, documented advantage
over `std`:

| Dependency | Justification |
|-----------|--------------|
| `rayon` | Work-stealing thread pool with zero-alloc `par_iter`. `std` has no equivalent. |
| `crossbeam-channel` | Lower syscall overhead than `std::sync::mpsc` for the SPSC pipeline. |
| `crossbeam-queue` | Lock-free `ArrayQueue` for the disk-commit channel. |
| `jwalk` | Parallel directory walking, ~4x faster than sequential `walkdir`. |
| `tikv-jemallocator` / `mimalloc` | High-performance global allocator (jemalloc on Unix, mimalloc on Windows); 8-50% faster than the system allocator on allocation-heavy workloads. jemalloc is tuned via a compile-time `malloc_conf` static to return freed pages promptly, bounding RSS at scale. |
| `rustc-hash` | 2-5x faster than `std::collections::HashMap` for integer keys. |
| `thiserror` | Derives `Display` and `From` for error types without boilerplate. |
| `clap` | CLI argument parsing with help generation. No `std` equivalent. |
| `flate2` | zlib compression. `std` has no compression support. |
| `globset` | Compiled glob matching for filter rules. Faster than per-path regex. |
| `tokio` | Async runtime for the daemon accept loop. Feature-gated, not default. |
| `exacl` | POSIX ACL support via safe Rust. Avoids unsafe `acl_*` FFI in our code. |
| `windows-rs` | Safe Rust bindings for Windows APIs. Avoids raw `winapi` FFI. |
| `russh` | Embedded SSH client. Feature-gated, avoids runtime `ssh` dependency. |

Dependencies that are deprecated, unmaintained, or provide marginal
benefit over `std` are not adopted. When a dependency deprecates an API,
migration is immediate - no `#[allow(deprecated)]` grace periods.

---

## 14. Cross-References

- Per-crate responsibilities and public APIs: crate-level rustdocs.
- Pattern instance inventory: `docs/design/pattern-usage-catalog.md` (#2120).
- Daemon process model: `docs/DAEMON_PROCESS_MODEL.md`.
- Async daemon design: `docs/design/daemon-async-accept-sync-workers.md`.
- io_uring + rayon composition: `docs/design/io-uring-rayon-composition.md`.
- Async impact on io_uring: `docs/design/async-io-uring-impact.md`.
- Type-state for protocol phases: `docs/design/type-state-protocol-phases.md`.
