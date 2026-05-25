# BPF-7: Back-compat assessment for the BufferPool per-test factory API

## 1. Scope

BPF-7 enumerates every observable consequence of the BPF-6 API addition
(`BufferPool::isolated() -> BufferPoolBuilder`, plus the
`BufferPoolBuilder` type and any helper traits) on existing call sites.
The goal is to certify that BPF-8 can land the new surface as a pure
addition, with no behavioural change for any current caller, and to flag
any rename or refactor that must precede BPF-8.

In-scope:

- Public items exported from
  `crates/engine/src/local_copy/buffer_pool/` and re-exported through
  `crates/engine/src/lib.rs`.
- Internal callers across every workspace crate that constructs or
  references a `BufferPool`.
- External (cargo-published) crate-level publish posture and what it
  implies for SemVer obligations.
- Test sites that touch `BufferPool` capacity state (the BPF-2 audit
  output at `docs/audit/bufferpool-envguard-gap-list.md`).

Out of scope:

- The shape of the BPF-6 builder methods themselves; BPF-6 owns that
  design. This document only verifies BPF-6's chosen names do not
  collide with the existing surface.
- The BPF-9 migration of individual cap-tests; only the ordering
  constraint is captured here.

Memory note: `[[project_bufferpool_test_serialization_fragile]]` -
the global `OnceLock` pool forces cap-tests to wrap mutations in
`EnvGuard`. BPF-7 is the back-compat sign-off that lets the BPF-6
factory replace that pattern without breaking any current consumer.

Prior art in this series:

- BPF-1 (#2819) - inventoried every cap-touching test.
- BPF-2 (#2820) - classified the inventory by `EnvGuard` coverage.
- BPF-3 (#2821) - wrapped the BPF-2 gap list with `EnvGuard`.
- BPF-4 (#4916) - CI lint spec for `EnvGuard` coverage.
- BPF-5 (#2823) - implemented the lint.
- BPF-6 (#2824) - factory API design (parent of this assessment).
- BPF-8 (#2826) - factory implementation.
- BPF-9 (#2827) - cap-test migration.
- BPF-10 - removes `EnvGuard` lint and inline duplicates.
- BPF-11 - test-author docs.

## 2. Public surface inventory

Grepping
`crates/engine/src/local_copy/buffer_pool/**/*.rs` (excluding
`/tests/`) for `pub fn`, `pub struct`, `pub enum`, `pub trait`
produces the following surface. The "BPF-6 collision?" column records
whether the planned addition (`BufferPool::isolated()` returning a
`BufferPoolBuilder`) shadows or replaces an existing item.

### 2.1 Module `buffer_pool/mod.rs`

Re-exports (see `crates/engine/src/local_copy/buffer_pool/mod.rs:109-115`):

| Re-exported item | Source module | BPF-6 collision? |
|------------------|---------------|------------------|
| `BufferAllocator` (trait) | `allocator` | no |
| `DefaultAllocator` (struct) | `allocator` | no |
| `AdaptiveBufferController` (struct) | `buffer_controller` | no |
| `ControllerConfig` (struct) | `buffer_controller` | no |
| `GlobalBufferPoolConfig` (struct) | `global` | no |
| `global_buffer_pool` (fn) | `global` | no |
| `init_global_buffer_pool` (fn) | `global` | no |
| `BorrowedBufferGuard` (struct) | `guard` | no |
| `BufferGuard` (struct) | `guard` | no |
| `PageAlignedBufferGuard` (struct) | `page_aligned` | no |
| `PageAlignedBufferPool` (struct) | `page_aligned` | no |
| `BufferPool` (struct) | `pool` | adds inherent method `isolated`; new symbol |
| `BufferPoolStats` (struct) | `pool` | no |
| `ThroughputTracker` (struct) | `throughput` | no |

Plus the `adaptive_buffer_size` free function and the
`ADAPTIVE_BUFFER_*` constants. None of those collide with the BPF-6
additions.

### 2.2 Inherent items on `BufferPool`

`crates/engine/src/local_copy/buffer_pool/pool.rs`:

| Line | Item | Kind | BPF-6 impact |
|------|------|------|--------------|
| 102 | `pub struct BufferPool<A: BufferAllocator = DefaultAllocator>` | struct | none; BPF-6 adds inherent method, not a wrapper type |
| 205 | `pub fn new(max_buffers: usize) -> Self` | ctor | none |
| 232 | `pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self` | ctor | none; BPF-6 builder will mirror the name as a builder method |
| 264 | `pub fn with_allocator(max_buffers, buffer_size, allocator: A) -> Self` | ctor | none |
| 298 | `pub fn with_memory_cap(mut self, max_bytes: usize) -> Self` | builder fluent | none; BPF-6 mirrors this name on `BufferPoolBuilder` |
| 327 | `pub fn with_byte_budget(mut self, max_bytes: usize) -> Self` | builder fluent | none; mirrored by BPF-6 |
| 343 | `pub fn with_throughput_tracking(mut self) -> Self` | builder fluent | none; mirrored by BPF-6 |
| 356 | `pub fn with_throughput_tracking_alpha(mut self, alpha: f64) -> Self` | builder fluent | none; mirrored by BPF-6 |
| 377 | `pub fn with_adaptive_resizing(mut self) -> Self` | builder fluent | none; mirrored by BPF-6 |
| 409 | `pub fn with_buffer_controller(mut self, config: ControllerConfig) -> Self` | builder fluent | none; mirrored by BPF-6 |
| 427 | `pub fn record_transfer(&self, bytes, duration)` | accessor | none |
| 453 | `pub fn recommended_buffer_size(&self) -> usize` | accessor | none |
| 472..1056 | accessors and stats getters | accessors | none |
| 501..689 | `acquire_from`, `try_acquire_from`, `acquire_adaptive_from`, `acquire_controlled_from`, `acquire_controlled`, `acquire`, `try_acquire` | acquire APIs | none |

**No existing `BufferPool::isolated` method.** Verified by:
`grep -n "fn isolated\|::isolated\|BufferPoolBuilder" crates/` returns
zero hits.

### 2.3 Trait impls on `BufferPool`

`crates/engine/src/local_copy/buffer_pool/pool.rs`:

- `impl BufferPool` (188)
- `impl<A: BufferAllocator> BufferPool<A>` (251)
- `impl Default for BufferPool<DefaultAllocator>` (1094)
- `impl<A: BufferAllocator> Drop for BufferPool<A>` (1109)

Auto-derived: `Debug` (via `#[derive(Debug)]` at line 101).

No `From`, `Into`, `Clone`, `Send`, `Sync`, or `Serialize` impls. The
struct relies on auto-derived `Send`/`Sync` from its field types
(`ArrayQueue`, `AtomicUsize`, `AtomicU64`, owned data), which is the
standard pattern.

### 2.4 Visibility flips

BPF-6 needs no private-to-public visibility flip. `BufferPool::new` and
the `with_*` chain that the factory composes are already `pub`. The
new `BufferPoolBuilder` type is a fresh public symbol in `pool.rs` (or
a sibling module BPF-6 may carve out). The existing `BufferPool` impl
gains one new inherent method `isolated`; no field visibility change.

## 3. Internal caller inventory

Source: `grep -rn "BufferPool::\|init_global_buffer_pool\|global_buffer_pool" crates/`.

### 3.1 Production code (non-test, non-bench)

| Crate | File:line | Usage |
|-------|-----------|-------|
| `core` | `crates/core/src/client/run/mod.rs:66, 346, 385` | Imports `init_global_buffer_pool` and `GlobalBufferPoolConfig`; `apply_max_alloc` calls `init_global_buffer_pool(cfg)` once at CLI start to apply `--max-alloc`. Singleton path. |
| `cli` | `crates/cli/src/frontend/server/run.rs:231-235` | Server-mode path constructs `engine::local_copy::GlobalBufferPoolConfig { byte_budget: Some(limit_usize), .. }` and calls `engine::local_copy::init_global_buffer_pool(cfg)`. Singleton path. |
| `engine` (internal) | `crates/engine/src/local_copy/context_impl/state.rs:36, 731` | Reads `global_buffer_pool()` into context; exposes `pub(super) fn buffer_pool(&self) -> Arc<BufferPool>`. Singleton path; no construction. |
| `engine` (re-export) | `crates/engine/src/lib.rs:214-218` | Public re-export of `BufferPool`, `BufferPoolStats`, `BufferGuard`, `BorrowedBufferGuard`, `BufferAllocator`, `DefaultAllocator`, `GlobalBufferPoolConfig`, `ThroughputTracker`, `global_buffer_pool`, `init_global_buffer_pool`. |
| `transfer` | (no production references) | `transfer` only references `BufferPool` from its integration test (see 3.2). |
| `daemon`, `protocol`, `checksums`, `filters`, `metadata`, `signature`, `bandwidth`, `logging`, `branding`, `rsync_io`, `compress`, `batch`, `flist`, `matching`, `apple-fs`, `platform`, `windows-gnu-eh`, `embedding` | none | No `BufferPool` references in production code. |
| `fast_io` | (`RegisteredBufferPool` only) | `fast_io` defines and consumes its own `RegisteredBufferPool` type for io_uring fixed buffers. It does not depend on `engine`'s `BufferPool`. No collision with BPF-6 because the names are crate-disjoint. |

**Singleton vs. fresh-instance breakdown for production code:**

- Singleton (`init_global_buffer_pool` + `global_buffer_pool`):
  `core::client::run::apply_max_alloc`,
  `cli::frontend::server::run`, `engine::local_copy::context_impl::state`.
- Fresh `BufferPool::with_*` instance: none in production. Every
  production transfer path goes through the singleton.

**Builder-method exposure for BPF-6:** BPF-6 mirrors the names
`with_buffer_size`, `with_memory_cap`, `with_byte_budget`,
`with_throughput_tracking`, `with_throughput_tracking_alpha`,
`with_adaptive_resizing`, `with_buffer_controller`, `with_allocator`
on the new `BufferPoolBuilder` type. These names already exist on
`BufferPool` itself. Because the new methods live on
`BufferPoolBuilder` (a distinct nominal type), there is no inherent
method-resolution ambiguity; method call syntax dispatches on the
receiver type. No production caller breaks.

### 3.2 Tests and benches

| Crate | File:line | Usage | Migrated by BPF-9? |
|-------|-----------|-------|--------------------|
| `engine` (bench) | `crates/engine/benches/buffer_pool_benchmark.rs:68` | `BufferPool::with_buffer_size(POOL_CAPACITY, BUFFER_SIZE)` | benches are out of BPF-9 scope; no `EnvGuard` usage |
| `engine` (bench) | `crates/engine/benches/adaptive_bufferpool.rs:122, 135, 146, 168` | `BufferPool::with_buffer_size`, `acquire_adaptive_from`, `acquire_controlled_from` | benches; out of BPF-9 scope |
| `engine` (bench) | `crates/engine/benches/optimizations_benchmark.rs:29, 41, 46, 75, 82, 100, 109` | `BufferPool::new`, `acquire_from` | benches; out of BPF-9 scope |
| `engine` (bench) | `crates/engine/benches/buffer_pool_contention.rs:27, 45, 70, 124, 181, 197` | `BufferPool::new`, `with_buffer_size`, `acquire_from` | benches; out of BPF-9 scope |
| `engine` | `crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:17, 25, 51, 84, 116, 163, 186, 206, 214` | `BufferPool::with_buffer_size(...).with_byte_budget(...)` | yes; BPF-9 swaps to `BufferPool::isolated().with_buffer_size(...).with_byte_budget(...).build()` |
| `engine` | `crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:17, 23, 42, 59, 71, 91, 102, 127, 150, 164, 179` | `BufferPool::with_buffer_size(...).with_memory_cap(...)` and `BufferPool::with_allocator(...).with_memory_cap(...)` | yes |
| `engine` | `crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:111, 126` | `BufferPool::with_buffer_size(...).with_memory_cap(...)` | yes |
| `engine` | `crates/engine/src/local_copy/buffer_pool/tests/controller.rs`, `throughput.rs`, `thread_cache.rs`, `adaptive_pool.rs`, `pool_basic.rs`, `contention.rs`, `slab.rs` | constructor + builder method use | yes (those that touch caps); leaf tests that only acquire/return need no migration |
| `engine` | `crates/engine/src/local_copy/buffer_pool/global.rs:273-318` | Five `env_var_*` tests using inline `EnvGuard` + `GlobalBufferPoolConfig::default()` | yes; BPF-9 replaces with `BufferPool::isolated().build()` directly, deleting the env-var dependency |
| `engine` | `crates/engine/src/local_copy/buffer_pool/global.rs:321-339` | `init_after_lazy_init_returns_err` (singleton init test) | retained; tests singleton semantics, not isolated factory |
| `engine` | `crates/engine/src/local_copy/buffer_pool/global.rs:182-228` | `global_pool_*` tests | retained; cover singleton API surface |
| `engine` | `crates/engine/src/local_copy/context_impl/state.rs:36` | Production caller (not a test) | not migrated; the singleton path continues to exist for production code |
| `transfer` | `crates/transfer/tests/buffer_pool_cross_crate.rs:8, 19, 22, 31, 40, 54, 62, 65, 69, 78` | Cross-crate integration test for the re-exported types | yes for the `acquire_and_return` / `stats` cases; the `global_pool_*` case stays on the singleton |

All of the above will continue to compile after BPF-6 lands because:

- Adding a new inherent method (`BufferPool::isolated`) is a non-breaking
  addition.
- Adding a new public type (`BufferPoolBuilder`) is a non-breaking
  addition.
- No existing method signature changes.
- No existing type signature changes.

BPF-9 then rewrites the tests in the "yes" rows to use the new factory;
that is a deliberate change, not a regression.

## 4. External (cargo-published) surface

The workspace is published as a binary, not as libraries on crates.io.
Evidence:

- Top-level `Cargo.toml` lines 1-11: the `bin` package
  (`name = "bin"`, binary `oc-rsync`) has `publish = true`. This is
  the only publish target.
- Crates with explicit `publish = false`:
  - `crates/bandwidth/Cargo.toml:11`
  - `crates/platform/Cargo.toml:10`
  - `crates/daemon/Cargo.toml:8`
  - `crates/test-support/Cargo.toml:9`
  - `crates/filters/fuzz/Cargo.toml:5`
  - `crates/protocol/fuzz/Cargo.toml:5`
- Crates without an explicit `publish = ` setting (including
  `engine`, `core`, `cli`, `transfer`, `protocol`, `checksums`,
  `compress`, `metadata`, `flist`, `matching`, `signature`,
  `logging`, `logging-sink`, `branding`, `rsync_io`, `fast_io`,
  `apple-fs`, `embedding`, `windows-gnu-eh`, `batch`): no
  `[workspace.package] publish = ` default is set in the top-level
  `Cargo.toml` (lines 208-214 list only `edition`, `rust-version`,
  `authors`, `license`, `version`, `repository`). Cargo's default for
  a missing `publish` field is "publishable", but none of these crates
  are listed on crates.io as of v0.6.2.

Conclusion: external SemVer back-compat is **N/A**. The BPF-6 API
addition is internal-only. The only consumer of the `engine` public
surface across workspace boundaries is `core` (via
`crates/core/Cargo.toml`'s path dependency) and `transfer` (via the
cross-crate test). Both are workspace-local and migrate atomically
with BPF-8.

If BPF-12 (hypothetical) eventually publishes `engine` to crates.io,
BPF-6's pure-addition posture remains SemVer-minor-compatible. No
breaking-change concern exists today or in any planned downstream
publish.

## 5. Test caller inventory

Derived from BPF-2's gap list
(`docs/audit/bufferpool-envguard-gap-list.md`) plus a fresh grep over
`crates/engine/src/local_copy/buffer_pool/tests/`.

### 5.1 Tests that touch BufferPool capacity state today

| File:line | Test name | Current API | Compiles after BPF-6? | Touched by BPF-9? |
|-----------|-----------|-------------|-----------------------|-------------------|
| `crates/engine/src/local_copy/buffer_pool/global.rs:273` | `env_var_overrides_pool_size` | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "42")` + `GlobalBufferPoolConfig::default()` | yes | yes; replace with `BufferPool::isolated().with_buffer_size(42, ...).build()` |
| `crates/engine/src/local_copy/buffer_pool/global.rs:280` | `env_var_zero_ignored` | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "0")` | yes | yes; behaviour becomes a config-validation test on the builder, not an env-var test |
| `crates/engine/src/local_copy/buffer_pool/global.rs:291` | `env_var_non_numeric_ignored` | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "not_a_number")` | yes | yes (same) |
| `crates/engine/src/local_copy/buffer_pool/global.rs:301` | `env_var_negative_ignored` | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "-5")` | yes | yes (same) |
| `crates/engine/src/local_copy/buffer_pool/global.rs:311` | `env_var_unset_uses_auto` | `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)` | yes | yes (same) |
| `crates/engine/src/local_copy/buffer_pool/global.rs:321` | `init_after_lazy_init_returns_err` | `global_buffer_pool()` + `init_global_buffer_pool(cfg)` expecting `Err` | yes | no; this test exists specifically to cover singleton init semantics |
| `crates/engine/src/local_copy/buffer_pool/global.rs:182` | `global_pool_returns_arc` | `global_buffer_pool()` | yes | no; singleton coverage |
| `crates/engine/src/local_copy/buffer_pool/global.rs:192` | `global_pool_returns_same_instance` | `global_buffer_pool()` twice + `Arc::ptr_eq` | yes | no; singleton coverage |
| `crates/engine/src/local_copy/buffer_pool/global.rs:200` | `global_pool_is_thread_safe` | 8 threads acquiring from singleton | yes | no; singleton coverage |
| `crates/engine/src/local_copy/buffer_pool/global.rs:217` | `global_pool_buffers_are_reusable` | singleton acquire/release | yes | no; singleton coverage |
| `crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:7-220` | nine builder-fluent tests | `BufferPool::with_buffer_size(N, M).with_byte_budget(B)` | yes | yes; route through `BufferPool::isolated()` to make intent explicit and let BPF-10 drop the `global-pool-serial` filter dependency |
| `crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:1-200` | eleven builder-fluent tests | `BufferPool::with_buffer_size(...).with_memory_cap(...)` (line 150 also `with_allocator`) | yes | yes |
| `crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:111, 126` | two tests | `BufferPool::with_buffer_size(...).with_memory_cap(...)` | yes | yes |
| `crates/engine/src/local_copy/buffer_pool/tests/controller.rs`, `throughput.rs`, `thread_cache.rs`, `adaptive_pool.rs`, `pool_basic.rs`, `contention.rs`, `slab.rs` | various | constructor + builder fluent use | yes | yes for cap-touching cases |
| `crates/transfer/tests/buffer_pool_cross_crate.rs:18-78` | five tests | mix of `BufferPool::with_buffer_size`, `acquire_from`, `BufferPool::stats`, plus one `global_buffer_pool` call | yes | yes for the four `with_buffer_size` cases; `global_pool_accessible_cross_crate` stays on the singleton |

### 5.2 Why every row "compiles after BPF-6"

BPF-6 adds:

1. `BufferPool::isolated(&self) -> BufferPoolBuilder` (or static
   `BufferPool::isolated() -> BufferPoolBuilder`; BPF-6 decides).
2. `pub struct BufferPoolBuilder` with mirror-named fluent setters.
3. `BufferPoolBuilder::build(self) -> BufferPool`.

None of these collide with existing inherent methods (section 2.2 and
2.3). The builder type is in a fresh namespace. Existing call sites
keep compiling unchanged because Rust resolves
`BufferPool::with_buffer_size(...)` to the existing inherent method on
`BufferPool`, not to a method on `BufferPoolBuilder`.

## 6. Risk analysis

### 6.1 Method name collision

**Result: no collision.** `grep -rn "fn isolated\|::isolated\|BufferPoolBuilder" crates/`
returns zero hits in the buffer_pool subtree or anywhere in the
workspace. BPF-6 is free to claim:

- `BufferPool::isolated`
- `BufferPoolBuilder` (the type)
- `BufferPoolBuilder::with_buffer_size`
- `BufferPoolBuilder::with_memory_cap`
- `BufferPoolBuilder::with_byte_budget`
- `BufferPoolBuilder::with_throughput_tracking`
- `BufferPoolBuilder::with_throughput_tracking_alpha`
- `BufferPoolBuilder::with_adaptive_resizing`
- `BufferPoolBuilder::with_buffer_controller`
- `BufferPoolBuilder::with_allocator`
- `BufferPoolBuilder::build`

The mirrored `with_*` names on `BufferPoolBuilder` shadow nothing
because Rust dispatches inherent methods on the receiver type.
`BufferPool::with_buffer_size(...)` still resolves to the existing
inherent method on `BufferPool` itself; only
`builder.with_buffer_size(...)` reaches the new code.

### 6.2 Trait coherence

`BufferPool` has no `From`, `Into`, `Clone`, `PartialEq`, `Eq`, `Hash`,
`Serialize`, `Deserialize`, or third-party-trait impls (section 2.3).
Only auto-derived `Debug` plus inherent `Default` and `Drop`. The new
`BufferPoolBuilder` type is unconstrained by any existing impl.

The new builder must not implement `Drop` with side effects (the
caller can drop a half-configured builder and expect a no-op). The
existing `BufferPool::Drop` (telemetry on
`OC_RSYNC_BUFFER_POOL_STATS=1`) is per-instance and applies to any
factory-built pool the same way it applies to the singleton; no
coupling change.

### 6.3 Send / Sync bounds

`BufferPool` derives `Send + Sync` from its field composition:
`ArrayQueue<Vec<u8>>` (`Send + Sync`), `AtomicUsize`, `AtomicU64`,
owned `MemoryCap`/`ByteBudget`/`ThroughputTracker`/`PressureTracker`/
`AdaptiveBufferController`. The factory must produce a pool with
identical bounds because `BufferPoolBuilder::build` calls the
existing `BufferPool::with_*` chain, which already returns
`BufferPool<DefaultAllocator>` (when no custom allocator is supplied)
or `BufferPool<A: BufferAllocator>` (when one is). No new
`unsafe impl Send`/`unsafe impl Sync` is needed; the auto-derived
bounds carry through.

Verification: the cross-crate test
`crates/transfer/tests/buffer_pool_cross_crate.rs:18-35` already
asserts `Arc<BufferPool>` flows across crate boundaries and is shared
between threads (via `Arc::clone` + acquire). Auto-Send/Sync is
established by the build; BPF-6 inherits it.

### 6.4 Drop / RAII semantics

`crates/engine/src/local_copy/buffer_pool/pool.rs:1109-1128` shows the
existing `Drop for BufferPool<A>` only emits a stderr telemetry line
when `OC_RSYNC_BUFFER_POOL_STATS=1`. It touches no shared state, no
process-wide allocators, no static. An isolated `BufferPool` instance
created via `BufferPool::isolated().build()` drops cleanly without
affecting the `GLOBAL_BUFFER_POOL` `OnceLock` at
`crates/engine/src/local_copy/buffer_pool/global.rs:30`.

The factory must not lazily initialise the singleton as a side effect
of `isolated()` (some early-draft factory APIs in other codebases have
done this). Verified by BPF-6's stated contract: `isolated()` returns
a builder; `build()` constructs a new `BufferPool::new(...)` chain
with no `OnceLock` interaction.

### 6.5 Allocator generic preservation

`BufferPool` carries a `BufferAllocator` generic parameter
(`<A: BufferAllocator = DefaultAllocator>`). The factory must preserve
this generic when callers want a custom allocator (the
`tests/memory_cap.rs:150` path uses
`BufferPool::with_allocator(4, 512, TrackingAllocator::new())`). BPF-6
needs either:

- a generic `BufferPoolBuilder<A: BufferAllocator = DefaultAllocator>`, or
- a `with_allocator<A>(self, allocator: A) -> BufferPoolBuilder<A>`
  builder transition method.

Both are non-breaking additions. The risk is API-surface design, not
back-compat; flagged here so BPF-6 picks one explicitly.

### 6.6 Feature-flag interactions

The `thread-slab-pool` feature
(`crates/engine/src/local_copy/buffer_pool/mod.rs:104-105`) compiles in
an extra `thread_slab` module. The cross-crate test at
`crates/transfer/tests/buffer_pool_cross_crate.rs:6-16` already gates
some assertions behind `#[cfg(not(feature = "thread-slab-pool"))]`.
The factory must build pools that honour the same feature gating; the
simplest path is to have `BufferPoolBuilder::build` produce the same
`BufferPool` configuration the existing constructors do, with feature
gates carrying through transparently. No new feature-flag surface.

## 7. Migration plan

Ordering constraints for BPF-8 / BPF-9 / BPF-10 / BPF-11.

### Step 1: BPF-8 lands the factory (#2826)

- New file:
  `crates/engine/src/local_copy/buffer_pool/builder.rs` (or extend
  `pool.rs`).
- New items: `BufferPoolBuilder` struct, `BufferPool::isolated()`
  inherent method, `BufferPoolBuilder::with_*` fluent setters mirroring
  the existing `BufferPool::with_*` names,
  `BufferPoolBuilder::build()`.
- Re-export from
  `crates/engine/src/local_copy/buffer_pool/mod.rs` and
  `crates/engine/src/lib.rs`.
- Gate behind a `bufferpool-isolated-factory` cargo feature, default
  on. The gate is precautionary: if a downstream surface bug emerges,
  CI can flip it off without reverting the implementation.
- No call-site change. All current production and test code keeps
  using the same APIs. Test count unchanged. CI green.

### Step 2: BPF-9 migrates cap-tests crate-by-crate (#2827)

Order:

1. `crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs` -
   nine call sites; pure builder-fluent migrations.
2. `crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs` -
   eleven call sites; one (`line 150`) also uses `with_allocator`.
3. `crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs` - two
   call sites.
4. `crates/engine/src/local_copy/buffer_pool/global.rs:273-318` - five
   `env_var_*` tests. Each migration commit removes the `EnvGuard`
   wrap, replaces `GlobalBufferPoolConfig::default()` with
   `BufferPool::isolated().with_buffer_size(N, ...).build()`, and
   drops the inline `EnvGuard` duplicate when no remaining test in
   the file needs it.
5. `crates/transfer/tests/buffer_pool_cross_crate.rs:18-78` - four
   `with_buffer_size` cases migrate; the singleton tests remain on
   `global_buffer_pool()`.

Each commit is independent and surgical (per project Rule 3). Each
commit must pass `cargo nextest run -p <crate> --all-features` in CI;
no `EnvGuard` removed until that crate's migration commit lands.

### Step 3: BPF-10 deletes EnvGuard scaffolding

Preconditions:

- Every BPF-9 commit merged.
- `tools/ci/check_envguard.sh --list-tracked` reports zero
  unguarded cap-touching test bodies.

Actions:

- Delete `tools/ci/check_envguard.sh`.
- Delete the workflow step that invokes it.
- Delete the inline `EnvGuard` duplicates listed in
  `docs/design/bpf-4-envguard-ci-lint-spec.md:83-94`. Tests that still
  need env-var manipulation for non-buffer-pool reasons (e.g.,
  `crates/branding/src/branding/tests.rs`) keep their local
  `EnvGuard`; BPF-10 only deletes the ones the factory replaces.
- Remove the `global-pool-serial` group entry from
  `.config/nextest.toml` if no remaining test needs serial execution.

### Step 4: BPF-11 documents the factory for test authors

- New section in `docs/test-plans/` or `docs/user/` (BPF-11 decides):
  "Per-test BufferPool isolation pattern."
- Cite the BPF-6 design doc and this BPF-7 assessment.
- Provide a copy-paste template for cap-touching tests.

### Step 5: Feature flag removal (follow-up release)

After two releases with `bufferpool-isolated-factory` defaulting on
and no opt-out reports, remove the `#[cfg(feature = ...)]` gates and
the feature entry from `Cargo.toml`. Single follow-up PR.

## 8. Rollback criteria

If BPF-6 or BPF-8 surfaces a problem, the rollback decisions are:

1. **Public-surface collision** (section 6.1). If a future BPF-6
   revision adds a method name that this assessment did not flag, the
   resolution is to rename the BPF-6 method before BPF-8 lands. Refresh
   the section 2 inventory and re-run the grep
   (`grep -rn "fn <new_name>" crates/`) to certify.
2. **Hidden coupling to OnceLock global** (section 6.4). If
   `BufferPool::isolated().build()` ever touches `GLOBAL_BUFFER_POOL`
   (e.g., through a misplaced `lazy_static!` initialiser inside one of
   the `with_*` builder steps), block BPF-8 merge. The factory must
   provide an instance that is observationally indistinguishable from
   a direct `BufferPool::new(...)` construction; any singleton write
   is a coupling bug that defeats BPF-9's purpose.
3. **Migration uncovers missing knob** (section 7 step 2). If a
   cap-test migration in BPF-9 reveals that the factory cannot
   reproduce the test's intent (e.g., the test depends on
   `OC_RSYNC_BUFFER_POOL_SIZE` parsing semantics that the factory
   does not expose), the response is to revisit BPF-6's API surface
   and add the missing builder method. Until that ships, the affected
   test keeps its `EnvGuard` wrapping; BPF-10 cannot delete the lint
   until every cap-test migrates.
4. **Send/Sync regression** (section 6.3). If
   `BufferPoolBuilder::build()` produces a pool whose `Send + Sync`
   bound is no longer auto-derived (e.g., because the builder holds a
   field that loses the bound), the cross-crate test in
   `crates/transfer/tests/buffer_pool_cross_crate.rs` fails to
   compile. Revert BPF-8 and refactor the builder to drop the
   offending field before re-landing.
5. **Drop side-effect regression** (section 6.4). If a
   factory-produced pool's `Drop` causes interference between parallel
   tests (e.g., the telemetry stderr write deadlocks under specific
   capacity settings), revert BPF-8 and revisit telemetry hook in the
   factory path.

Rollback is the BPF-8 PR's responsibility, not BPF-7's. This section
documents the criteria so the BPF-8 reviewer has a checklist.

## 9. Cross-references

- BPF-6 design doc (parent of this assessment, in flight under
  #2824): `docs/design/bpf-6-buffer-pool-factory-api.md` when it
  lands.
- BPF-4 `EnvGuard` lint specification:
  `docs/design/bpf-4-envguard-ci-lint-spec.md` (PR #4916).
- BPF-2 gap list (source of the section 5 test inventory):
  `docs/audit/bufferpool-envguard-gap-list.md`.
- BPF-1 inventory (source of BPF-2):
  `docs/audit/bufferpool-cap-tests-inventory.md`.
- Implementation tracker: #2826 (BPF-8), #2827 (BPF-9).
- Memory note: `[[project_bufferpool_test_serialization_fragile]]`.
- Memory note: `[[project_bufferpool_count_cap]]` - byte-cap
  regression coverage already shipped; the factory migration
  preserves that coverage.
