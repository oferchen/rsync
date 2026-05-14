# Codebase Navigation and Design Rationale

A landing page for new contributors. The "if I want to change X, where do
I start?" map, followed by short notes on the highest-leverage design
decisions and pointers to the deeper docs that explain each one.

This document is intentionally a roadmap, not a textbook. Each section
links out to the canonical source instead of restating it. When in doubt,
the upstream rsync 3.4.1 C source at
`target/interop/upstream-src/rsync-3.4.1/` is the authoritative reference
for protocol behaviour; the Rust code mirrors it.

Related top-level documents:

- [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) - flat reference of crate
  responsibilities and dependency graph.
- [`docs/architecture-rationale.md`](../architecture-rationale.md) - why each
  crate exists, why the boundaries fall where they do, why specific external
  dependencies were chosen over alternatives.
- [`docs/design/architecture-rationale.md`](architecture-rationale.md) - the
  longer-form discussion of trade-offs (workspace vs monolith, rayon vs
  async, single binary vs split, three-platform parity, compile time).
- [`docs/design/pattern-usage-catalog.md`](pattern-usage-catalog.md) -
  catalogued instances of each design pattern in the workspace.
- [`docs/UPSTREAM_COMPARISON.md`](../UPSTREAM_COMPARISON.md) - point-by-point
  comparison with upstream rsync 3.4.1.

---

## 1. How to navigate the codebase

Most contributor questions reduce to "I want to change X - what do I read
first?" Use this table as an entry point. Each row points at the smallest
surface area where the change belongs and the doc that explains why.

| If you want to change ...                                  | Start here                                                                                          | Why (rationale)                                                              |
|-----------------------------------------------------------|----------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------|
| A CLI flag, help text, or exit-code routing                | `crates/cli/src/frontend/`                                                                          | [`architecture-rationale.md#3.1`](architecture-rationale.md)                |
| The orchestration of a transfer (client lifecycle)         | `crates/core/src/client/run/mod.rs`, `crates/core/src/client/config/builder/mod.rs`                  | [`architecture-rationale.md#3.1`](architecture-rationale.md), facade section |
| Wire protocol bytes (multiplex frame, varint, file-list)   | `crates/protocol/src/`, golden tests at `crates/protocol/tests/golden/`                             | [`docs/PROTOCOL.md`](../PROTOCOL.md)                                         |
| Sender / receiver / generator state machine                | `crates/transfer/src/setup/mod.rs`, `crates/transfer/src/handshake.rs`                              | [`architecture-rationale.md#4.3`](architecture-rationale.md)                |
| Capability negotiation (`-e.LsfxCIvu`)                     | `crates/transfer/src/setup/capability.rs`, `crates/transfer/src/setup/negotiator.rs`                | [`docs/UPSTREAM_COMPARISON.md`](../UPSTREAM_COMPARISON.md)                  |
| Rolling or strong checksum, SIMD dispatch                  | `crates/checksums/src/rolling/`, `crates/checksums/src/strong/`                                     | [`architecture-rationale.md#4.1`](architecture-rationale.md), SIMD section  |
| Add or change a compression codec                          | `crates/compress/src/` (one module per codec), feature-gated                                        | [`architecture-rationale.md#4.1`](architecture-rationale.md)                |
| Filter rule evaluation (include/exclude/dir-merge)         | `crates/filters/src/chain.rs`, `crates/filters/src/rule.rs`, `crates/filters/src/merge/`            | [`architecture-rationale.md#4.4`](architecture-rationale.md)                |
| Local-copy executor, delta apply, sparse handling          | `crates/engine/src/local_copy/`, `crates/engine/src/delta/`                                         | [`docs/architecture-rationale.md#1`](../architecture-rationale.md)         |
| Buffer pool sizing and reuse                               | `crates/engine/src/local_copy/buffer_pool/`                                                         | [`docs/audits/buffer-pool-capacity-sizing.md`](../audits/buffer-pool-capacity-sizing.md) |
| Directory walking                                          | `crates/engine/src/walk/`                                                                            | [`docs/architecture/parallelization.md`](../architecture/parallelization.md) |
| File list construction (`flist.c` equivalent)              | `crates/flist/src/builder.rs`, `crates/protocol/src/flist/`                                         | [`docs/architecture-rationale.md#1`](../architecture-rationale.md)         |
| Transport (SSH stdio, `rsync://` TCP, embedded SSH)        | `crates/rsync_io/src/`                                                                              | [`docs/audits/async-ssh-transport-evaluation.md`](../audits/async-ssh-transport-evaluation.md) |
| Daemon `@RSYNCD:` greeting, auth, module config            | `crates/daemon/src/`                                                                                | [`docs/DAEMON_PROCESS_MODEL.md`](../DAEMON_PROCESS_MODEL.md)               |
| io_uring, IOCP, `copy_file_range`, `sendfile`, mmap        | `crates/fast_io/src/` (one module per backend)                                                      | [`architecture-rationale.md#6`](architecture-rationale.md), fast_io section |
| Permissions, ACLs, xattrs, timestamps, ownership           | `crates/metadata/src/`                                                                              | [`docs/architecture-rationale.md#5`](../architecture-rationale.md)        |
| Rate limiter / bandwidth shaping                           | `crates/bandwidth/src/limiter/core/limiter.rs`                                                      | [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md)                                |
| Batch-mode capture/replay                                  | `crates/batch/src/format/`, `crates/batch/src/script.rs`                                            | [`docs/BATCH_MODE.md`](../BATCH_MODE.md)                                    |
| Logging output format, verbosity, info/debug categories    | `crates/logging/src/`, `crates/logging-sink/src/`                                                   | upstream `log.c`                                                            |
| macOS resource forks, AppleDouble                          | `crates/apple-fs/src/`, `crates/fast_io/src/macos_io.rs`                                            | [`docs/design/macos-fnocache-writev-fallback.md`](macos-fnocache-writev-fallback.md) |
| Windows-specific paths, ACLs, IOCP wiring                  | `crates/fast_io/src/iocp_stub.rs` (stub), platform-gated IOCP modules in `crates/fast_io/src/`     | [`docs/audits/windows-iocp-file-write-status.md`](../audits/windows-iocp-file-write-status.md) |
| Embedded SSH (russh) transport                             | `crates/rsync_io/src/`, feature-gated `embedded-ssh`                                                | [`docs/design/async-ssh-transport.md`](async-ssh-transport.md)              |
| Add a new design audit, benchmark, or RFC                  | `docs/audits/` or `docs/design/`                                                                    | this document                                                               |

Rules that apply across the table:

1. **Cross a crate boundary only if you must.** Most changes belong inside
   one crate. If a change requires editing two crates, ask whether the
   abstraction in between is wrong (an unnecessary indirection) or right
   (the change really is cross-cutting).
2. **The dependency graph is one-directional.** Lower-level crates never
   import from higher-level ones; `cargo check` enforces this. The order
   from top to bottom is roughly:
   `cli -> core -> {engine, transfer, daemon, rsync_io} -> {protocol, filters, compress, bandwidth, signature, match, flist} -> {checksums, metadata, fast_io, logging, branding}`.
3. **Wire bytes are the compatibility contract.** Any change visible to a
   peer must be byte-equivalent to upstream rsync 3.4.1. Internal
   refactors are free; wire format changes need an upstream-compatible
   negotiation path.

---

## 2. The highest-leverage design decisions

A new contributor benefits more from understanding *why* each major
decision was taken than from a complete tour of the source. The following
sections capture the six or seven highest-leverage architectural choices.
Each is one paragraph (**what**, **why**, **where to look**) plus a
cross-reference to the canonical doc.

### 2.1 Crate decomposition over a monolithic source tree

**What.** The workspace splits into ~25 crates layered top-down
(`cli -> core -> engine, daemon, transport -> protocol -> checksums,
filters, compress, bandwidth -> metadata`), with no upward edges.
**Why.** Three pressures drove the split: turning `unsafe` into a
compile-time property (most crates declare `#![deny(unsafe_code)]`),
enabling per-crate test suites that do not need the full dependency
graph, and feature gating at the crate boundary so a minimal build can
drop `zstd`, `lz4`, `io_uring`, `iocp`, `acl`, `xattr`, `parallel`, or
`async` without touching consumer code. The cost is a slower clean build;
the incremental advantage dominates the development workflow.
**Where to look.** Crate layout: [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md).
Full rationale and trade-offs:
[`docs/architecture-rationale.md`](../architecture-rationale.md) and
[`docs/design/architecture-rationale.md`](architecture-rationale.md).

### 2.2 Strategy pattern for checksums and compression

**What.** `StrongDigest` trait in
`crates/checksums/src/strong/mod.rs`, `ChecksumStrategy` trait in
`crates/checksums/src/strong/strategy/trait_def.rs`, and
`CompressionStrategy` / `CompressionNegotiator` traits in
`crates/compress/src/strategy/`. Concrete implementations
(Adler32, MD4, MD5, XXH3, XXH128, SHA-1/256/512; zlib, zlibx, zstd, lz4)
are selected once during handshake and threaded through the
generator/receiver contexts.
**Why.** The rsync protocol negotiates checksum and compression
algorithms during the handshake. Compile-time generics would
monomorphize every transfer function per algorithm combination and
inflate binary size for a runtime-determined feature set. Trait objects
add one vtable indirection per call; on the hot path the cost is dwarfed
by the hash or compression itself. The selection happens in
`crates/transfer/src/setup/capability.rs` after capability negotiation.
**Where to look.** Implementation files in
`crates/checksums/src/{rolling,strong}/`,
`crates/compress/src/strategy/`. Rationale:
[`docs/design/architecture-rationale.md#4.1`](architecture-rationale.md).
Pattern instances: [`docs/design/pattern-usage-catalog.md`](pattern-usage-catalog.md).

### 2.3 Builder pattern for configuration assembly

**What.** Typed builders for `ClientConfig`
(`crates/core/src/client/config/builder/mod.rs`), `CoreConfig`,
`TransferConfigBuilder`, `FilterChain`, and the remote-invocation builder
in `crates/core/src/client/remote/invocation/builder.rs`. Each `build()`
returns `Result<T, BuilderError>`.
**Why.** These configs have 10-40 fields, many optional, with
cross-field validation (e.g., `--inplace` is mutually exclusive with
`--delay-updates`; `--append` is mutually exclusive with
`--partial-dir`). Positional constructors are error-prone at this width;
default-then-mutate skips validation. The builder centralises it,
checking invariants once at construction instead of on every use.
**Where to look.** Builder files cited above. Rationale:
[`docs/design/architecture-rationale.md#4.2`](architecture-rationale.md).

### 2.4 State machine for protocol phases

**What.** The protocol has ordered phases (handshake -> compat exchange
-> filter list -> file list -> delta transfer -> finalization ->
goodbye). The code encodes this explicitly: a compile-time type-state
variant in `crates/protocol/src/state/typestate.rs` and a runtime variant
(`DynamicProtocolState`, `SessionState`) for contexts that need dynamic
dispatch. The daemon has its own connection lifecycle
(`Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing`).
**Why.** Calling a phase-2 function during phase-1 produces valid Rust
but invalid wire bytes - the peer expects a different message format and
the stream silently corrupts. The type system catches the most dangerous
misorderings before they reach the wire. Upstream rsync enforces ordering
by C-level convention and code review; the Rust port adds compile-time
guarantees.
**Where to look.** `crates/protocol/src/state/`,
`crates/daemon/src/` for the session state machine.
Rationale: [`docs/design/architecture-rationale.md#4.3`](architecture-rationale.md).

### 2.5 Chain of responsibility for filter evaluation

**What.** `FilterChain` in `crates/filters/src/chain.rs` walks a
`Vec<CompiledRule>` in declaration order; first match wins. Protect
rules accumulate in a parallel chain for `--delete` decisions. Dir-merge
files (`.rsync-filter`) splice their rules into the chain when a
directory is entered.
**Why.** rsync filter rules are user-ordered and first-match-wins; the
evaluation must test rules in sequence until one matches. This mirrors
upstream `exclude.c:check_filter()` exactly. Strategy would be wrong
here: strategy selects *one* algorithm for the session, whereas chain of
responsibility evaluates an *ordered set* of handlers per path.
**Where to look.** `crates/filters/src/{chain,rule,set,merge}/`.
Rationale: [`docs/design/architecture-rationale.md#4.4`](architecture-rationale.md).
Coverage matrix: [`docs/filter-coverage-matrix.md`](../filter-coverage-matrix.md).

### 2.6 Dependency inversion via traits

**What.** High-level modules depend on abstractions, not concrete types.
The transfer layer asks for `dyn ChecksumStrategy` and `dyn
CompressionStrategy`, not `MD5` or `Zlib`. The local-copy executor asks
for `dyn PlatformCopy`, not `std::fs::copy`. Tests inject
`NoCowPlatformCopy` or `NoZeroCopyPlatformCopy` to exercise specific
fallback paths without platform-specific test infrastructure. The
`BufferAllocator` trait in `engine` lets tests inject a tracking
allocator that counts allocations without modifying the production
`BufferPool`.
**Why.** Trait objects make per-OS, per-kernel-version, per-filesystem
test paths reachable from cross-platform unit tests. Without them, every
fallback-path test would need a Linux kernel without `copy_file_range`,
a filesystem without reflink, or a macOS without `clonefile`.
**Where to look.** Trait definitions in
`crates/checksums/src/strong/strategy/trait_def.rs`,
`crates/compress/src/strategy/traits.rs`, and the platform-copy traits
in `crates/fast_io/src/`. Trade-off discussion:
[`docs/design/architecture-rationale.md#12`](architecture-rationale.md).

### 2.7 SIMD with runtime feature detection cached in `OnceLock`

**What.** Rolling and strong checksums have SIMD fast paths (AVX2, SSE2,
NEON) with scalar fallbacks. Feature detection uses
`std::arch::is_x86_feature_detected!` or
`std::arch::is_aarch64_feature_detected!`, and the result is cached in a
`OnceLock<bool>` per backend so subsequent dispatches are a single atomic
load.
**Why.** CPUID probing on every call would dominate the hot loop. A
`OnceLock` provides thread-safe lazy initialization without lock
contention. SIMD and scalar paths must stay in lockstep; mandatory parity
tests in `crates/checksums/src/simd_parity_tests.rs` keep them honest.
The CLI override (`--simd=<level>`) hooks in through `SimdLevel` in
`crates/checksums/src/cpu_features.rs` so benchmarks and tests can pin
the dispatcher to a specific backend.
**Where to look.**
`crates/checksums/src/rolling/checksum/{x86,neon}.rs` for the cached
detection. `crates/checksums/src/cpu_features.rs` for the runtime
override. Rationale:
[`docs/design/architecture-rationale.md#3.3`](architecture-rationale.md).

### 2.8 `fast_io` as the unsafe-code containment crate

**What.** All io_uring, IOCP, `copy_file_range`, `sendfile`, `splice`,
mmap, `CopyFileExW`, reflink ioctls, and statx batching live in
`crates/fast_io/src/`. Consumer crates call safe Rust APIs
(`IoUringReader`, `IoUringWriter`, `PlatformCopy`) and never see an
`unsafe` block or a `#[cfg(target_os = "linux")]` gate.
**Why.** io_uring requires `unsafe` for ring setup, SQE submission,
buffer registration, and CQE reaping. Scattering these across `engine`,
`transfer`, and `daemon` would violate the unsafe-boundary policy and
make safety audits impractical. Centralising them lets a reviewer read
every `# Safety` block in one pass, keeps the fallback discipline (every
optimization has a safe fallback) enforced in one place, and means the
daemon/CLI/protocol surfaces - which see untrusted network input -
remain `#![deny(unsafe_code)]`. The long-term direction is to migrate
the remaining `#[allow(unsafe_code)]` permissions in `metadata`,
`checksums`, `engine`, `protocol`, and `embedding` into `fast_io` as well.
**Where to look.** `crates/fast_io/src/` and its module-level rustdoc.
Stubs for non-native platforms:
`crates/fast_io/src/{io_uring_stub.rs,iocp_stub.rs}`.
Composition with rayon:
[`docs/design/io-uring-rayon-composition.md`](io-uring-rayon-composition.md).
Trade-offs and rationale:
[`docs/design/architecture-rationale.md#6`](architecture-rationale.md) and
[`docs/architecture-rationale.md#5`](../architecture-rationale.md).

### 2.9 Upstream rsync as the source of truth

**What.** The wire protocol, CLI surface, exit-code table, and error
message format (including role trailers `[sender]`, `[receiver]`,
`[generator]`) all mirror upstream rsync 3.4.1 exactly. Internals
(rayon, io_uring, IOCP, SIMD, strategy/builder/state-machine patterns)
diverge freely below that surface.
**Why.** Compatibility is the value proposition: oc-rsync drops into
existing SSH `rsync_path=` configurations, systemd service files, and
backup scripts because the wire bytes, flag bits, and exit codes match.
Two enforcement mechanisms guard this: golden byte tests in
`crates/protocol/tests/golden/` that pin exact wire output, and an
interop harness (`tools/ci/run_interop.sh`) that runs the binary against
upstream 3.0.9, 3.1.3, and 3.4.1 in CI. The rule is: if changing an
internal pattern would change a byte on the wire, do not change it
without an upstream-compatible negotiation path.
**Where to look.** Upstream source at
`target/interop/upstream-src/rsync-3.4.1/`. Golden tests at
`crates/protocol/tests/golden/`. Comparison:
[`docs/UPSTREAM_COMPARISON.md`](../UPSTREAM_COMPARISON.md). Rationale:
[`docs/design/architecture-rationale.md#8`](architecture-rationale.md).

---

## 3. Concurrency model in one paragraph

The default transfer path is synchronous I/O with rayon for CPU-bound
work (parallel signature computation, parallel stat batching, parallel
directory walks via `jwalk`) interspersed with sequential file I/O. The
wire protocol itself is strictly in-order - file N's delta before file
N+1's - so there is no concurrency within the protocol stream and an
async runtime adds overhead without throughput. Async is opt-in via the
`async` feature on `core` and `daemon` for two specific cases: the
daemon accept loop in high fan-in scenarios (1k+ concurrent connections)
and the embedded SSH transport (russh is inherently async). The transfer
hot loop is never async; this keeps strace traces and upstream
comparisons meaningful. The full evaluation is in
[`docs/design/architecture-rationale.md#5`](architecture-rationale.md)
and [`docs/architecture/parallelization.md`](../architecture/parallelization.md).

---

## 4. Three-platform parity in one paragraph

Every feature ships on Linux, macOS, and Windows. The mechanism is
*alternative paths, not missing paths*: when a Linux syscall is
unavailable, the same operation has a fallback (`copy_file_range` ->
read/write, io_uring -> standard I/O, sendfile -> `std::io::copy`,
POSIX ACLs -> Windows ACLs via `windows-rs`, AppleDouble has both real
macOS and stub Linux/Windows implementations). The `fast_io` crate ships
type-compatible stubs (`io_uring_stub.rs`, `iocp_stub.rs`) so consumer
code compiles unchanged on every target. There is no
`unimplemented!`/`todo!` anywhere - any unimplemented path fails the CI
build on its home platform.

Cross-platform audit references:

- [`docs/audits/fast-io-fallback-macos-vs-linux.md`](../audits/fast-io-fallback-macos-vs-linux.md)
- [`docs/audits/macos-dispatch-io.md`](../audits/macos-dispatch-io.md)
- [`docs/audits/windows-iocp-file-write-status.md`](../audits/windows-iocp-file-write-status.md)
- [`docs/audits/windows-gnu-vs-msvc.md`](../audits/windows-gnu-vs-msvc.md)
- [`docs/audits/windows-acl-xattr-ci-matrix.md`](../audits/windows-acl-xattr-ci-matrix.md)
- [`docs/audits/buffer-pool-capacity-sizing.md`](../audits/buffer-pool-capacity-sizing.md)

---

## 5. Where the deeper docs live

Inventory of canonical documents by topic. This document deliberately
does not duplicate them.

### Protocol and wire format

- [`docs/PROTOCOL.md`](../PROTOCOL.md) - wire format details.
- [`docs/UPSTREAM_COMPARISON.md`](../UPSTREAM_COMPARISON.md) - upstream
  comparison.
- [`docs/INTEROP.md`](../INTEROP.md) - interop test matrix.
- [`docs/parity-options.yml`](../parity-options.yml) - per-flag parity
  status.
- Golden byte tests: `crates/protocol/tests/golden/`.

### Daemon

- [`docs/DAEMON_PROCESS_MODEL.md`](../DAEMON_PROCESS_MODEL.md) -
  thread-per-connection rationale.
- [`docs/design/daemon-async-accept-sync-workers.md`](daemon-async-accept-sync-workers.md)
- [`docs/design/daemon-tokio-async-listener-impl.md`](daemon-tokio-async-listener-impl.md)

### Parallelism and concurrency

- [`docs/architecture/parallelization.md`](../architecture/parallelization.md)
- [`docs/design/io-uring-rayon-composition.md`](io-uring-rayon-composition.md)
- [`docs/design/async-io-uring-impact.md`](async-io-uring-impact.md)
- [`docs/design/buffer-pool-sharding.md`](buffer-pool-sharding.md)
- [`docs/parallelism_audit.md`](../parallelism_audit.md)

### Platform-specific I/O

- [`docs/platform-io-fast-paths.md`](../platform-io-fast-paths.md)
- [`docs/design/macos-fnocache-writev-fallback.md`](macos-fnocache-writev-fallback.md)
- [`docs/design/macos-kqueue-fast-io.md`](macos-kqueue-fast-io.md)
- [`docs/design/iocp-transfer-pipeline-wiring.md`](iocp-transfer-pipeline-wiring.md)
- [`docs/design/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
- [`docs/design/io-uring-ring-pool.md`](io-uring-ring-pool.md)

### Pattern catalogue and rationale

- [`docs/design/pattern-usage-catalog.md`](pattern-usage-catalog.md) -
  every instance of each pattern.
- [`docs/design-patterns-catalog.md`](../design-patterns-catalog.md) -
  pattern overview.
- [`docs/architecture-rationale.md`](../architecture-rationale.md) -
  rationale for crate boundaries and dependencies.
- [`docs/design/architecture-rationale.md`](architecture-rationale.md) -
  longer-form trade-off discussion.

### Operations

- [`docs/BATCH_MODE.md`](../BATCH_MODE.md)
- [`docs/PROFILING.md`](../PROFILING.md)
- [`docs/platform-support.md`](../platform-support.md)
- [`docs/feature_matrix.md`](../feature_matrix.md)
- [`docs/filter-coverage-matrix.md`](../filter-coverage-matrix.md)

### Audits

195 audit notes live in [`docs/audits/`](../audits/). Each captures a
specific investigation: a kernel quirk, a benchmark result, a fallback
decision, a CI matrix gap. New investigations land here; the cross-links
above point at the load-bearing ones.

---

## 6. Conventions worth knowing before the first PR

- **Conventional commit prefixes are required.** `feat:`, `fix:`,
  `perf:`, `docs:`, `chore:`, `style:`, `test:`, `refactor:`, `ci:`. The
  same prefix is required on PR titles; a labeler workflow uses it for
  release-note categorization.
- **No placeholders.** `todo!`, `unimplemented!`, `FIXME`, `XXX`, stub
  functions, or commented-out code do not ship. Every change is
  production-ready.
- **No deprecated APIs.** Migration is immediate when a dependency
  deprecates anything; no `#[allow(deprecated)]` grace periods.
- **Tests run in CI only.** Locally, run `cargo fmt --all -- --check`
  and `cargo clippy --workspace --all-targets --all-features --no-deps
  -- -D warnings`. Full `cargo nextest` is reserved for CI. Targeted
  tests during development: `cargo nextest run -p <crate>
  --all-features -E 'test(<pattern>)'`.
- **Three-platform CI is non-negotiable.** Required checks: fmt+clippy,
  nextest (stable), Windows (stable), macOS (stable), Linux musl
  (stable). A change is not done until every required check is green.
- **Branding scrubs.** Documentation, commit messages, and PR text are
  written as human-authored prose. References to authoring tools do not
  appear in shipped files or in git history.

---

## 7. Reading order for a new contributor

If you are arriving fresh, the suggested order is:

1. [`README.md`](../../README.md) for the project elevator pitch.
2. This document for the navigation map.
3. [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) for the crate-by-crate
   responsibilities table.
4. [`docs/architecture-rationale.md`](../architecture-rationale.md) for
   the per-crate "why" - the dependency-choice and facade-design
   rationale.
5. [`docs/design/architecture-rationale.md`](architecture-rationale.md)
   for the longer-form trade-off discussion (rayon vs async, single
   binary vs split, compile-time vs modularity).
6. [`docs/PROTOCOL.md`](../PROTOCOL.md) and
   [`docs/UPSTREAM_COMPARISON.md`](../UPSTREAM_COMPARISON.md) before
   touching the wire.
7. The relevant audit in [`docs/audits/`](../audits/) or design note in
   [`docs/design/`](.) before touching a contentious subsystem.

After that, the table in section 1 takes over.
