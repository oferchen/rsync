# BPF-6: Per-test BufferPool factory API design

## 1. Scope

BPF-6 designs the per-test `BufferPool` factory API. The goal is to let tests
construct an isolated `BufferPool` instance with explicit capacity and config,
without touching environment variables or holding a process-wide guard. This
obsoletes the `EnvGuard` pattern for capacity-touching tests.

This task only produces the design. The follow-on work is:

- BPF-7 (#2825) - assess back-compat of the new API against existing callers.
- BPF-8 (#2826) - implement the factory behind a feature flag.
- BPF-9 - migrate cap-tests crate-by-crate to the new API.
- BPF-10 - retire the `EnvGuard` CI lint (BPF-5) once migration is complete.
- BPF-11 - write test-author docs on the new isolation pattern.

Cross-references (memory notes):

- `[[project_bufferpool_test_serialization_fragile]]` - the global `OnceLock`
  pool forces cap-tests to use a fragile `EnvGuard` pattern; the factory API
  designed here is the long-term fix.
- `[[project_bufferpool_count_cap]]` - byte-cap regression coverage is binding
  and must remain green through the migration.

Prior art in this series:

- BPF-1 (#2819) - inventoried every cap-touching test
  (`docs/audit/bufferpool-cap-tests-inventory.md`).
- BPF-2 (#2820) - classified BPF-1 results by `EnvGuard` coverage
  (`docs/audit/bufferpool-envguard-gap-list.md`).
- BPF-3 (#2821) - wraps the BPF-2 gap list with `EnvGuard`.
- BPF-4 (#2822) - spec for the EnvGuard CI lint
  (`docs/design/bpf-4-envguard-ci-lint-spec.md`, PR #4916).
- BPF-5 (#2823) - implements the BPF-4 lint.

## 2. Current state

The global pool and the env-var coupling that BPF-6 must replace:

- The singleton lives at
  `crates/engine/src/local_copy/buffer_pool/global.rs`. `GLOBAL_BUFFER_POOL`
  is a `static OnceLock<Arc<BufferPool>>` (line 30). `init_global_buffer_pool`
  (line 132) installs a `GlobalBufferPoolConfig`; `global_buffer_pool()`
  (line 106) lazily defaults the pool on first access.
- The env var `OC_RSYNC_BUFFER_POOL_SIZE` is read inside
  `GlobalBufferPoolConfig::default()` (line 65 const definition, line 77
  read). It is the only cap-affecting variable today.
- `BufferPool` constructors and builder methods live in
  `crates/engine/src/local_copy/buffer_pool/pool.rs`:
  - `BufferPool::new(max_buffers)` - line 205.
  - `BufferPool::with_buffer_size(max_buffers, buffer_size)` - line 232.
  - `BufferPool::with_allocator(max_buffers, buffer_size, allocator)` - line 264.
  - `BufferPool::with_memory_cap(max_bytes)` - line 298.
  - `BufferPool::with_byte_budget(max_bytes)` - line 327.
  - `BufferPool::with_throughput_tracking()` - line 343.
  - `BufferPool::with_throughput_tracking_alpha(alpha)` - line 356.
  - `BufferPool::with_adaptive_resizing()` - line 377.
  - `BufferPool::with_buffer_controller(config)` - line 409.
- The canonical `EnvGuard` lives at `crates/platform/src/env.rs:19`. Across
  the workspace there are 12 additional inline duplicate `EnvGuard` structs
  (in `crates/branding`, `crates/core` x2, `crates/embedding`, `crates/cli`
  x3, `crates/fast_io`, `crates/engine` x4, `crates/rsync_io`, plus
  `tests/integration_daemon_server.rs`). The duplicate at
  `crates/engine/src/local_copy/buffer_pool/global.rs:231` is the one the
  five existing cap-tests use.
- The `OnceLock` makes the global pool single-init per process. Once any
  test calls `global_buffer_pool()` or `init_global_buffer_pool()`, the
  singleton is frozen. Tests that mutate `OC_RSYNC_BUFFER_POOL_SIZE` and
  then re-read `GlobalBufferPoolConfig::default()` must serialise via
  `EnvGuard` + the nextest `global-pool-serial` group; otherwise a
  concurrent reader sees the wrong default.

## 3. Factory API surface

The new public API on `BufferPool` is additive: existing constructors and
builder methods stay in place. The factory entry point is a single
associated function that returns a `BufferPoolBuilder`, and the builder
mirrors the existing instance-builder methods one-to-one so call sites can
migrate without learning a new vocabulary.

```rust
impl BufferPool {
    /// Constructs an isolated, non-global `BufferPool` builder with the
    /// supplied config. The returned instance is self-contained: its
    /// lifetime is owned by the caller, and it is NOT registered with
    /// `GLOBAL_BUFFER_POOL`. Safe to instantiate from concurrent tests
    /// without serialisation.
    #[must_use]
    pub fn isolated() -> BufferPoolBuilder { ... }
}

/// Builder for an isolated `BufferPool` instance.
///
/// Mirrors the existing per-instance `with_*` builder methods on
/// `BufferPool` so migration is mechanical. `build()` returns an owned
/// `BufferPool` with the configured knobs; the builder never touches
/// `GLOBAL_BUFFER_POOL`.
pub struct BufferPoolBuilder { ... }

impl BufferPoolBuilder {
    /// Sets the soft maximum number of buffers retained centrally.
    /// Mirrors the `max_buffers` parameter of `BufferPool::new`.
    pub fn with_max_buffers(self, max_buffers: usize) -> Self { ... }

    /// Sets each buffer's byte length. Mirrors `BufferPool::with_buffer_size`.
    pub fn with_buffer_size(self, bytes: usize) -> Self { ... }

    /// Sets the hard memory cap on outstanding (checked-out) buffers.
    /// Mirrors `BufferPool::with_memory_cap`. Panics on zero (same as
    /// the existing method).
    pub fn with_memory_cap(self, bytes: usize) -> Self { ... }

    /// Sets the soft retention byte budget. Mirrors
    /// `BufferPool::with_byte_budget`. Panics on zero (same as the
    /// existing method).
    pub fn with_byte_budget(self, bytes: usize) -> Self { ... }

    /// Enables throughput tracking. `enabled = true` matches
    /// `BufferPool::with_throughput_tracking()`; `false` is the no-op
    /// default. A separate `with_throughput_tracking_alpha(f64)` keeps
    /// parity with the existing alpha-tuned variant.
    pub fn with_throughput_tracking(self, enabled: bool) -> Self { ... }
    pub fn with_throughput_tracking_alpha(self, alpha: f64) -> Self { ... }

    /// Enables adaptive resizing. Mirrors `BufferPool::with_adaptive_resizing`.
    pub fn with_adaptive_resizing(self, enabled: bool) -> Self { ... }

    /// Enables the PID buffer controller. Mirrors
    /// `BufferPool::with_buffer_controller`. As today, enabling the
    /// controller implicitly enables throughput tracking.
    pub fn with_buffer_controller(self, config: ControllerConfig) -> Self { ... }

    /// Substitutes a custom allocator. Mirrors
    /// `BufferPool::with_allocator`'s `allocator` parameter; the generic
    /// allocator type is threaded through `BufferPoolBuilder<A>`.
    pub fn with_allocator<A: BufferAllocator>(self, allocator: A)
        -> BufferPoolBuilder<A>
    { ... }

    /// Consumes the builder and returns an owned, isolated `BufferPool`.
    /// Never touches `GLOBAL_BUFFER_POOL` or any other `OnceLock` state.
    #[must_use]
    pub fn build(self) -> BufferPool { ... }
}
```

Constraints:

- The factory MUST NOT read, write, or initialise `GLOBAL_BUFFER_POOL`.
- The factory MUST NOT read any `OC_RSYNC_BUFFER_POOL_*` environment
  variable. Capacity comes from the explicit builder calls, not ambient
  state.
- `build()` returns an owned `BufferPool` by value. The caller can wrap it
  in `Arc::new` for `acquire_from`-style use, or hold it on the stack /
  inside a `OnceCell` local to the test fixture.
- Defaults when a knob is unset on the builder match the per-instance
  builder defaults today (no memory cap, no byte budget, no throughput
  tracking, no controller, no adaptive resizing, `DefaultAllocator`,
  `buffer_size = COPY_BUFFER_SIZE`, `max_buffers` required at `build()`).

## 4. Concurrency and isolation guarantee

What the API promises and how that is enforced:

- Two threads (or two `nextest` workers) calling `BufferPool::isolated()
  .with_memory_cap(...).build()` concurrently must NOT contend on any
  shared mutex, atomic, or `OnceLock`. The only shared state they touch is
  the heap allocator, which is already concurrency-safe.
- `GLOBAL_BUFFER_POOL` continues to exist for production use. It is the
  singleton consumed by `engine`, `transfer`, `core`, daemon, and the
  parallel-checksum path via `global_buffer_pool()` / `init_global_buffer_pool`.
  Production code does not change; only test code migrates.
- The isolated path reuses the same `BufferPool` struct internals defined
  in `pool.rs` (free-list `ArrayQueue`, per-instance `central_count`,
  per-instance `total_hits` / `total_misses` / `total_growths`, optional
  `MemoryCap`, `ByteBudget`, `ThroughputTracker`, `PressureTracker`,
  `AdaptiveBufferController`). The only behaviour difference is that
  `build()` does not call `GLOBAL_BUFFER_POOL.set(...)`.
- Every `BufferPool` instance already owns its `Mutex<Vec<Vec<u8>>>`-style
  state (the `ArrayQueue` free-list, the per-instance counters, the
  per-instance controller state). Two isolated pools cannot poison each
  other because they share no state.
- The thread-local cache (`thread_local_cache.rs`) is a process-global
  cache keyed by buffer length. When two isolated pools use the same
  `buffer_size`, a buffer returned by one may be reused by the other via
  TLS. This is a pre-existing property of the cache and is benign: TLS
  entries do not carry capacity-cap semantics.

## 5. Tradeoffs

Pros:

- Removes `EnvGuard` from the cap-test surface entirely. Cap-tests become
  idiomatic Rust: build, exercise, drop.
- Cap-tests can run under `cargo nextest` parallelism without the
  `global-pool-serial` group, eliminating the rename-fragile coupling
  flagged by BPF-2.
- The factory API matches the Builder pattern used elsewhere in the
  codebase (`FileEntryBuilder`, `CoreConfig`, `TransferConfigBuilder`,
  `FilterChain`), so it is consistent with the project's design-pattern
  policy in CLAUDE.md.
- Eliminates the only remaining reason for tests outside the buffer-pool
  module to take a process-wide env-var lock for cap concerns.

Cons:

- Requires touching `BufferPool` to expose `BufferPoolBuilder` as a public
  type; this is a public-API surface addition that has to be maintained
  going forward.
- Existing production code still constructs the pool via
  `init_global_buffer_pool` + `GlobalBufferPoolConfig`. The two paths must
  stay in lockstep on config interpretation - if a future knob is added
  (say, `with_pid_target_throughput`), it must land on both the factory
  builder and the global config in the same PR, or the two pools will
  diverge.
- `BufferPoolBuilder<A>`'s generic-allocator slot adds a small amount of
  type complexity. Most tests will stick with `DefaultAllocator` and never
  call `with_allocator`, so the friction is paid only by the few tests
  that already use a custom allocator (see `tests/contention.rs:337`
  onward in the BPF-1 inventory).

## 6. Tests that will migrate

The BPF-2 gap list at `docs/audit/bufferpool-envguard-gap-list.md`
classifies the singleton-touching tests. BPF-9 will migrate the following
representative targets first (cited from BPF-1 inventory at
`docs/audit/bufferpool-cap-tests-inventory.md`):

| File | test_name | current env / singleton coupling | post-migration builder methods |
|------|-----------|----------------------------------|-------------------------------|
| `crates/engine/src/local_copy/buffer_pool/global.rs:273` | `env_var_overrides_pool_size` | mutates `OC_RSYNC_BUFFER_POOL_SIZE` via `EnvGuard::set("42")`; reads `GlobalBufferPoolConfig::default()` | retire env-var path entirely in cap tests; this test continues to assert config parsing of `OC_RSYNC_BUFFER_POOL_SIZE` and remains under `EnvGuard` because it is the env-var contract test for the production global config, not a cap-behaviour test |
| `crates/engine/src/local_copy/buffer_pool/global.rs:182` | `global_pool_returns_arc` | reads `global_buffer_pool()`; depends on `OC_RSYNC_BUFFER_POOL_SIZE` via the singleton's lazy default | migrate to `BufferPool::isolated().with_max_buffers(N).build()`; the singleton-identity property being asserted is exclusive to the global path and stays as a separate test |
| `crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:16` | `memory_cap_is_set` | builds pool with `BufferPool::new(4).with_memory_cap(4096)` | `BufferPool::isolated().with_max_buffers(4).with_memory_cap(4096).build()` |
| `crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:16` | `byte_budget_is_set_via_builder` | builds pool with `BufferPool::new(4).with_byte_budget(8192)` | `BufferPool::isolated().with_max_buffers(4).with_byte_budget(8192).build()` |
| `crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:68` | `recommended_buffer_size_respects_memory_cap` | builds pool with `with_memory_cap(32 KiB)` + `with_throughput_tracking()` | `BufferPool::isolated().with_max_buffers(N).with_memory_cap(32 * 1024).with_throughput_tracking(true).build()` |
| `crates/engine/src/local_copy/buffer_pool/tests/controller.rs:41` | `buffer_controller_with_builder_chain` | chains `with_memory_cap(8192)` + adaptive + controller | `BufferPool::isolated().with_max_buffers(N).with_memory_cap(8192).with_adaptive_resizing(true).with_buffer_controller(cfg).build()` |
| `crates/transfer/tests/buffer_pool_cross_crate.rs:18` | `acquire_and_return_via_public_api` | builds `BufferPool::with_buffer_size(4, 64)` via cross-crate re-export | `BufferPool::isolated().with_max_buffers(4).with_buffer_size(64).build()` |

Notes:

- The env-var contract tests in `global.rs:273..311` (five tests) stay
  under `EnvGuard` because they test the global config's parser behaviour,
  not cap semantics. BPF-10 retires the lint, not the env-var tests.
- All ~50 tests under `crates/engine/src/local_copy/buffer_pool/tests/*.rs`
  classified as `LOW` risk in BPF-2 already construct private
  `BufferPool` instances. Their migration is a mechanical method rename
  (`BufferPool::new(...).with_X(...)` -> `BufferPool::isolated().with_max_buffers(...).with_X(...).build()`).
- The cross-crate test in `crates/transfer/tests/buffer_pool_cross_crate.rs`
  is the canary that the factory works from outside the `engine` crate.

The full migration target list is the BPF-1 inventory; BPF-9 will track it
crate-by-crate.

## 7. Public API stability

- `BufferPool::isolated()` and `BufferPoolBuilder` are public API
  additions. Once landed, removal is a breaking change. BPF-8 lands them
  behind a feature flag (see section 8) so that any unexpected coupling
  can be backed out within one release window.
- The builder method names mirror the existing per-instance `with_*`
  methods on `BufferPool`. If a future config knob is renamed on
  `BufferPool` (for example, `with_buffer_controller(ControllerConfig)`
  evolves a typed input), both the per-instance method, the
  `GlobalBufferPoolConfig` field, and the `BufferPoolBuilder` method must
  update in lockstep. The CI lint specified in BPF-4 already enforces
  that any new `OC_RSYNC_BUFFER_POOL_*` env var receives `EnvGuard`
  wrapping; a parallel guard for the factory parity is unnecessary
  because the factory does not read env state.
- The isolated factory is NOT feature-gated for downstream callers.
  Production code never opts in to the isolated path - it continues to
  use `global_buffer_pool()` / `init_global_buffer_pool` - so the
  factory's existence imposes zero overhead on the hot path. BPF-8's
  feature flag (`bufferpool-isolated-factory`) exists only as a one-
  release revert switch in case the migration uncovers a problem; it
  defaults to ON and is removed by BPF-10.

## 8. BPF-7 and BPF-8 acceptance criteria

These are the success bars for the follow-up tasks:

- **BPF-7 (#2825): back-compat assessment.**
  - Audit confirms no public callers of `BufferPool::new`,
    `BufferPool::with_buffer_size`, or `BufferPool::with_allocator` break
    when `BufferPool::isolated()` and `BufferPoolBuilder` are added.
  - Audit lists every external (non-test) caller of the per-instance
    `with_*` builder methods and confirms each remains source-compatible.
  - Audit produces a back-compat report at
    `docs/audit/bpf-7-buffer-pool-factory-backcompat.md` enumerating any
    name conflicts (none expected) and downstream re-exports
    (`crates/transfer` is the only known re-exporter).

- **BPF-8 (#2826): implement the factory.**
  - Adds `BufferPool::isolated() -> BufferPoolBuilder` and
    `BufferPoolBuilder` per section 3.
  - Gates the new surface behind the workspace feature flag
    `bufferpool-isolated-factory`, default ON. The flag exists for one
    release so the feature can be reverted without a major-version bump
    if BPF-9 migration surfaces a regression.
  - Includes a property test asserting that an isolated pool with
    `with_memory_cap(N)` honours the cap identically to the existing
    `BufferPool::new(M).with_memory_cap(N)` path.
  - Includes a concurrency test asserting that two isolated pools built
    on two threads do not share state (counter independence,
    cap-accounting independence).
  - Does NOT migrate any existing tests yet - that is BPF-9.

- **BPF-9: migrate cap-tests.**
  - One PR per crate, starting with `crates/engine/src/local_copy/buffer_pool/tests/`,
    then `crates/transfer/tests/`, then any `crates/core/` cap-test that
    surfaces during BPF-7 audit.
  - Each PR retires the corresponding `EnvGuard` usage and the
    `global-pool-serial` test-group dependency for the migrated tests.
  - Cap-test names containing `global_pool` or `env_var` are renamed only
    when the test no longer touches the singleton; otherwise they keep
    the name (and the nextest filter) until BPF-10.

- **BPF-10: retire the EnvGuard CI lint.**
  - Once BPF-9 declares the migration complete (last cap-test moved
    off `EnvGuard`), delete the BPF-5 lint script and remove the
    `OC_RSYNC_BUFFER_POOL_SIZE` tokens from the lint's cap-touching set.
  - Update `.config/nextest.toml`: keep the `global-pool-serial` group
    only for the env-var contract tests that legitimately remain under
    `EnvGuard`.

- **BPF-11: docs.**
  - Write a test-author guide at
    `docs/development/buffer-pool-test-isolation.md` describing the
    `BufferPool::isolated()` pattern, when to use it vs the global
    singleton, and how to write new cap-tests.
  - Update CLAUDE.md or the engine crate's rustdoc to point at the new
    guide.

## 9. Cross-references

- BPF-4 spec: `docs/design/bpf-4-envguard-ci-lint-spec.md` (PR #4916).
- BPF-1 inventory: `docs/audit/bufferpool-cap-tests-inventory.md`.
- BPF-2 gap list: `docs/audit/bufferpool-envguard-gap-list.md`.
- Canonical `EnvGuard`: `crates/platform/src/env.rs:19`.
- Global pool: `crates/engine/src/local_copy/buffer_pool/global.rs`.
- `BufferPool` constructors and builders: `crates/engine/src/local_copy/buffer_pool/pool.rs`
  lines 188-416.
- Nextest serialisation group: `.config/nextest.toml` (`global-pool-serial`).
- Memory notes: `[[project_bufferpool_test_serialization_fragile]]`,
  `[[project_bufferpool_count_cap]]`.
