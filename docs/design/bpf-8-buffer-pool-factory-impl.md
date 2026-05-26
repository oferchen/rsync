# BPF-8: BufferPool factory implementation spec

## 1. Summary

BPF-8 implements the per-test `BufferPool` factory API designed in BPF-6 and
validated for back-compat by BPF-7. The factory provides
`BufferPool::isolated() -> BufferPoolBuilder` - a builder that constructs
self-contained, non-global pool instances for test isolation. Tests that
exercise capacity semantics can use the factory instead of the fragile
`EnvGuard` + `OnceLock` singleton pattern.

The implementation lands behind the feature flag
`bufferpool-isolated-factory` (default ON) as a one-release revert switch.

## 2. Feature flag

- **Name:** `bufferpool-isolated-factory`
- **Crate:** `engine`
- **Default:** ON (included in the `default` feature list)
- **Purpose:** Precautionary kill switch. If BPF-9 migration surfaces a
  regression, CI can flip `default-features = false` on downstream test
  crates to disable the factory without reverting the implementation.
- **Removal:** BPF-10 removes the flag after two stable releases with no
  opt-out reports.

The flag gates:

1. The `BufferPool::isolated()` inherent method.
2. The `BufferPoolBuilder` type and its `impl` block.
3. The re-export of `BufferPoolBuilder` from `mod.rs` and `lib.rs`.

Production code never references these items - it continues to use
`global_buffer_pool()` / `init_global_buffer_pool` unconditionally.

## 3. Files to create

| Path | Purpose |
|------|---------|
| `crates/engine/src/local_copy/buffer_pool/builder.rs` | `BufferPoolBuilder` struct, fluent setters, `build()` method |
| `crates/engine/src/local_copy/buffer_pool/tests/isolated_factory.rs` | Property test (cap parity) and concurrency test (isolation) |

## 4. Files to modify

| Path | Change |
|------|--------|
| `crates/engine/Cargo.toml` | Add `bufferpool-isolated-factory = []` to `[features]`; add to `default` list |
| `crates/engine/src/local_copy/buffer_pool/mod.rs` | Add `#[cfg(feature = "bufferpool-isolated-factory")] mod builder;` and conditional re-export of `BufferPoolBuilder` |
| `crates/engine/src/local_copy/buffer_pool/pool.rs` | Add `#[cfg(feature = "bufferpool-isolated-factory")] pub fn isolated() -> BufferPoolBuilder` on `impl BufferPool` |
| `crates/engine/src/lib.rs` | Add conditional re-export of `BufferPoolBuilder` alongside existing `BufferPool` re-export |
| `crates/engine/src/local_copy/buffer_pool/tests/mod.rs` | Add `#[cfg(feature = "bufferpool-isolated-factory")] mod isolated_factory;` |

## 5. Factory API

```rust
// crates/engine/src/local_copy/buffer_pool/builder.rs

use super::allocator::{BufferAllocator, DefaultAllocator};
use super::buffer_controller::ControllerConfig;
use super::pool::BufferPool;
use super::throughput::ThroughputTracker;
use super::super::COPY_BUFFER_SIZE;

/// Builder for an isolated [`BufferPool`] instance.
///
/// Constructed via [`BufferPool::isolated()`]. The builder mirrors the
/// existing fluent `with_*` methods on `BufferPool` so migration is
/// mechanical. `build()` returns an owned pool that is NOT registered
/// with the global `OnceLock` singleton - safe for concurrent test use
/// without serialization.
pub struct BufferPoolBuilder<A: BufferAllocator = DefaultAllocator> {
    max_buffers: Option<usize>,
    buffer_size: usize,
    allocator: A,
    memory_cap: Option<usize>,
    byte_budget: Option<usize>,
    throughput_tracking: bool,
    throughput_alpha: Option<f64>,
    adaptive_resizing: bool,
    controller_config: Option<ControllerConfig>,
}

impl BufferPoolBuilder {
    /// Creates a new builder with default settings.
    ///
    /// Defaults: no memory cap, no byte budget, no throughput tracking,
    /// no adaptive resizing, `DefaultAllocator`, `buffer_size =
    /// COPY_BUFFER_SIZE`. `max_buffers` is required at `build()`.
    pub(super) fn new() -> Self {
        Self {
            max_buffers: None,
            buffer_size: COPY_BUFFER_SIZE,
            allocator: DefaultAllocator,
            memory_cap: None,
            byte_budget: None,
            throughput_tracking: false,
            throughput_alpha: None,
            adaptive_resizing: false,
            controller_config: None,
        }
    }
}

impl<A: BufferAllocator> BufferPoolBuilder<A> {
    /// Sets the soft maximum number of buffers retained centrally.
    #[must_use]
    pub fn with_max_buffers(mut self, max_buffers: usize) -> Self {
        self.max_buffers = Some(max_buffers);
        self
    }

    /// Sets each buffer's byte length.
    #[must_use]
    pub fn with_buffer_size(mut self, bytes: usize) -> Self {
        self.buffer_size = bytes;
        self
    }

    /// Sets the hard memory cap on outstanding (checked-out) buffers.
    ///
    /// # Panics
    ///
    /// Panics at `build()` if `bytes` is zero.
    #[must_use]
    pub fn with_memory_cap(mut self, bytes: usize) -> Self {
        self.memory_cap = Some(bytes);
        self
    }

    /// Sets the soft retention byte budget.
    ///
    /// # Panics
    ///
    /// Panics at `build()` if `bytes` is zero.
    #[must_use]
    pub fn with_byte_budget(mut self, bytes: usize) -> Self {
        self.byte_budget = Some(bytes);
        self
    }

    /// Enables throughput tracking.
    #[must_use]
    pub fn with_throughput_tracking(mut self, enabled: bool) -> Self {
        self.throughput_tracking = enabled;
        self
    }

    /// Enables throughput tracking with a custom EMA alpha.
    ///
    /// # Panics
    ///
    /// Panics at `build()` if `alpha` is not in `(0.0, 1.0]`.
    #[must_use]
    pub fn with_throughput_tracking_alpha(mut self, alpha: f64) -> Self {
        self.throughput_alpha = Some(alpha);
        self.throughput_tracking = true;
        self
    }

    /// Enables adaptive resizing.
    #[must_use]
    pub fn with_adaptive_resizing(mut self, enabled: bool) -> Self {
        self.adaptive_resizing = enabled;
        self
    }

    /// Enables the PID buffer controller. Implicitly enables throughput
    /// tracking.
    #[must_use]
    pub fn with_buffer_controller(mut self, config: ControllerConfig) -> Self {
        self.controller_config = Some(config);
        self.throughput_tracking = true;
        self
    }

    /// Substitutes a custom allocator.
    #[must_use]
    pub fn with_allocator<B: BufferAllocator>(
        self,
        allocator: B,
    ) -> BufferPoolBuilder<B> {
        BufferPoolBuilder {
            max_buffers: self.max_buffers,
            buffer_size: self.buffer_size,
            allocator,
            memory_cap: self.memory_cap,
            byte_budget: self.byte_budget,
            throughput_tracking: self.throughput_tracking,
            throughput_alpha: self.throughput_alpha,
            adaptive_resizing: self.adaptive_resizing,
            controller_config: self.controller_config,
        }
    }

    /// Consumes the builder and returns an owned, isolated `BufferPool`.
    ///
    /// Never touches `GLOBAL_BUFFER_POOL` or any `OnceLock` state.
    ///
    /// # Panics
    ///
    /// Panics if `with_max_buffers` was not called (max_buffers is
    /// required).
    #[must_use]
    pub fn build(self) -> BufferPool<A> {
        let max_buffers = self
            .max_buffers
            .expect("BufferPoolBuilder requires with_max_buffers()");

        let mut pool =
            BufferPool::with_allocator(max_buffers, self.buffer_size, self.allocator);

        if let Some(cap) = self.memory_cap {
            pool = pool.with_memory_cap(cap);
        }
        if let Some(budget) = self.byte_budget {
            pool = pool.with_byte_budget(budget);
        }
        if let Some(alpha) = self.throughput_alpha {
            pool = pool.with_throughput_tracking_alpha(alpha);
        } else if self.throughput_tracking {
            pool = pool.with_throughput_tracking();
        }
        if self.adaptive_resizing {
            pool = pool.with_adaptive_resizing();
        }
        if let Some(config) = self.controller_config {
            pool = pool.with_buffer_controller(config);
        }

        pool
    }
}
```

The entry point on `BufferPool`:

```rust
// In crates/engine/src/local_copy/buffer_pool/pool.rs

#[cfg(feature = "bufferpool-isolated-factory")]
use super::builder::BufferPoolBuilder;

impl BufferPool {
    /// Constructs an isolated, non-global `BufferPool` builder.
    ///
    /// The returned instance is self-contained: its lifetime is owned by
    /// the caller, and it is NOT registered with `GLOBAL_BUFFER_POOL`.
    /// Safe to instantiate from concurrent tests without serialization.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::isolated()
    ///     .with_max_buffers(4)
    ///     .with_memory_cap(4096)
    ///     .build();
    /// ```
    #[cfg(feature = "bufferpool-isolated-factory")]
    #[must_use]
    pub fn isolated() -> BufferPoolBuilder {
        BufferPoolBuilder::new()
    }
}
```

## 6. How the factory replaces global OnceLock in test contexts

The global pool path remains unchanged for production:

```
CLI/daemon start
  -> init_global_buffer_pool(GlobalBufferPoolConfig { ... })
  -> GLOBAL_BUFFER_POOL.set(Arc::new(pool))
  -> engine context reads global_buffer_pool()
```

For tests that need to exercise capacity semantics:

```
#[test]
fn memory_cap_is_enforced() {
    // No EnvGuard. No OnceLock. No nextest serial group.
    let pool = Arc::new(
        BufferPool::isolated()
            .with_max_buffers(4)
            .with_memory_cap(4096)
            .build(),
    );
    let _buf = BufferPool::acquire_from(Arc::clone(&pool));
    assert!(pool.memory_cap_bytes().is_some());
}
```

The factory path:

1. Does NOT read `GLOBAL_BUFFER_POOL`.
2. Does NOT write `GLOBAL_BUFFER_POOL`.
3. Does NOT read `OC_RSYNC_BUFFER_POOL_SIZE` or any environment variable.
4. Produces a `BufferPool` identical in internal structure to one built via
   `BufferPool::new(...).with_*()` - the same `ArrayQueue`, counters,
   optional subsystems.
5. The caller owns the pool. Drop deallocates it with no shared-state
   side effects.

## 7. cfg-gated dispatch

The feature flag gates only the API surface, not runtime dispatch. There
is no `#[cfg]` branch on the hot acquisition path. The factory simply
produces the same `BufferPool<A>` type that production code uses. The
difference is purely construction-time:

| Context | Construction | Pool identity |
|---------|--------------|---------------|
| Production | `init_global_buffer_pool(cfg)` -> `OnceLock` singleton | Process-wide, shared via `Arc` |
| Test (old) | `BufferPool::new(N).with_*()` directly | Caller-owned, but requires `EnvGuard` if cap depends on env vars |
| Test (new) | `BufferPool::isolated().with_max_buffers(N).with_*().build()` | Caller-owned, no env coupling, no serialization required |

When `bufferpool-isolated-factory` is disabled:

- `BufferPool::isolated()` does not exist.
- `BufferPoolBuilder` type is not compiled.
- All existing code paths continue to work unchanged.
- Tests fall back to the current `BufferPool::new(N).with_*()` pattern.

## 8. Migration path for existing cap-touching tests (BPF-9 prep)

BPF-9 rewrites cap-touching tests crate-by-crate. The mechanical
transformation:

### Before (current pattern)

```rust
#[test]
fn byte_budget_is_enforced() {
    let pool = BufferPool::with_buffer_size(4, 64).with_byte_budget(256);
    // ...
}
```

### After (factory pattern)

```rust
#[test]
fn byte_budget_is_enforced() {
    let pool = BufferPool::isolated()
        .with_max_buffers(4)
        .with_buffer_size(64)
        .with_byte_budget(256)
        .build();
    // ...
}
```

### Migration ordering (BPF-9)

1. `crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs` - 9 sites
2. `crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs` - 11 sites
3. `crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs` - 2 sites
4. `crates/engine/src/local_copy/buffer_pool/tests/controller.rs` - cap-touching cases
5. `crates/engine/src/local_copy/buffer_pool/tests/throughput.rs` - cap-touching cases
6. `crates/engine/src/local_copy/buffer_pool/global.rs:273-318` - 5 env-var tests
7. `crates/transfer/tests/buffer_pool_cross_crate.rs` - 4 `with_buffer_size` cases

Tests that cover singleton semantics (`global_pool_returns_arc`,
`global_pool_returns_same_instance`, `init_after_lazy_init_returns_err`)
remain on the global path. They still need the `global-pool-serial`
nextest group.

## 9. Thread-safety guarantees for isolated pools

Each isolated pool is self-contained. No shared mutable state exists
between two pools constructed via `BufferPool::isolated().build()`:

| Component | Shared? | Guarantee |
|-----------|---------|-----------|
| `ArrayQueue<Vec<u8>>` (central free-list) | Per-instance | Each pool owns its own queue allocation |
| `AtomicUsize` (central_count, soft_capacity) | Per-instance | Atomics are per-struct-instance |
| `AtomicU64` (total_hits, total_misses, total_growths) | Per-instance | Same |
| `MemoryCap` (Semaphore-based backpressure) | Per-instance | Each pool's cap is independent |
| `ByteBudget` (retention accounting) | Per-instance | Each pool's budget is independent |
| `ThroughputTracker` (EMA state) | Per-instance | Atomic-based, per-pool |
| `PressureTracker` (hit/miss counters) | Per-instance | Per-pool |
| `AdaptiveBufferController` (PID state) | Per-instance | Per-pool |
| Thread-local cache | Process-wide, keyed by buffer_size | Benign cross-pool sharing (see below) |

**Thread-local cache interaction:** The TLS cache at
`thread_local_cache.rs` is a process-global single-slot cache. When two
isolated pools use the same `buffer_size`, a buffer returned to one may
be picked up by another via TLS. This is benign:

- TLS does not carry capacity-cap semantics.
- The MemoryCap semaphore is decremented on return regardless of whether
  the buffer goes to TLS or the central queue.
- The ByteBudget is not debited until the buffer reaches the central pool
  (TLS bypass is a pre-existing optimization).

Two tests with different `buffer_size` values never share via TLS because
the cache key is buffer length. Tests that need strict isolation from TLS
can use distinct buffer sizes.

**`Send + Sync`:** `BufferPool<A>` auto-derives `Send + Sync` from its
field composition (`ArrayQueue`, atomics, owned subsystems). The factory
produces the same struct - no new unsafe impls needed. The cross-crate
test in `crates/transfer/tests/buffer_pool_cross_crate.rs` already
validates `Arc<BufferPool>` across threads.

## 10. Test strategy

BPF-8 ships two test modules (not migrating existing tests - that is BPF-9):

### 10.1 Property test: cap parity

Verifies that an isolated pool with `with_memory_cap(N)` honours the cap
identically to the existing `BufferPool::new(M).with_memory_cap(N)` path.

```rust
#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn isolated_pool_cap_matches_direct_construction(
            max_buffers in 1_usize..=32,
            buffer_size in 64_usize..=4096,
            memory_cap in 1_usize..=65536,
        ) {
            let direct = BufferPool::with_buffer_size(max_buffers, buffer_size)
                .with_memory_cap(memory_cap);
            let isolated = BufferPool::isolated()
                .with_max_buffers(max_buffers)
                .with_buffer_size(buffer_size)
                .with_memory_cap(memory_cap)
                .build();

            // Structural equivalence.
            prop_assert_eq!(direct.max_buffers(), isolated.max_buffers());
            prop_assert_eq!(direct.buffer_size(), isolated.buffer_size());
            prop_assert_eq!(
                direct.memory_cap_bytes(),
                isolated.memory_cap_bytes()
            );
        }
    }
}
```

### 10.2 Concurrency test: isolation

Asserts that two isolated pools on two threads do not share state.

```rust
#[test]
fn isolated_pools_do_not_interfere() {
    use std::sync::Arc;
    use std::thread;

    let pool_a = Arc::new(
        BufferPool::isolated()
            .with_max_buffers(2)
            .with_buffer_size(64)
            .with_memory_cap(128)
            .build(),
    );
    let pool_b = Arc::new(
        BufferPool::isolated()
            .with_max_buffers(8)
            .with_buffer_size(256)
            .with_memory_cap(8192)
            .build(),
    );

    let a = Arc::clone(&pool_a);
    let b = Arc::clone(&pool_b);

    let handle_a = thread::spawn(move || {
        // Exhaust pool_a's cap (2 buffers * 64 bytes = 128 bytes).
        let _g1 = BufferPool::acquire_from(Arc::clone(&a));
        let _g2 = BufferPool::acquire_from(Arc::clone(&a));
        // try_acquire should fail - cap exhausted.
        assert!(BufferPool::try_acquire_from(Arc::clone(&a)).is_none());
        a.stats()
    });

    let handle_b = thread::spawn(move || {
        // pool_b has much larger cap - should succeed many times.
        let _g1 = BufferPool::acquire_from(Arc::clone(&b));
        let _g2 = BufferPool::acquire_from(Arc::clone(&b));
        let _g3 = BufferPool::acquire_from(Arc::clone(&b));
        let _g4 = BufferPool::acquire_from(Arc::clone(&b));
        // All four succeed because cap is 8192 bytes and each is 256.
        b.stats()
    });

    let stats_a = handle_a.join().unwrap();
    let stats_b = handle_b.join().unwrap();

    // Counter independence: pool_a saw 3 acquires, pool_b saw 4.
    assert_eq!(stats_a.total_acquires(), 3);
    assert_eq!(stats_b.total_acquires(), 4);

    // Cap-accounting independence: pool_a is at cap, pool_b is not.
    assert_eq!(pool_a.memory_cap_bytes(), Some(128));
    assert_eq!(pool_b.memory_cap_bytes(), Some(8192));
}
```

### 10.3 No-global-touch test

Verifies the factory does not initialise the singleton as a side effect.

```rust
#[test]
fn isolated_does_not_touch_global() {
    // Build an isolated pool.
    let _pool = BufferPool::isolated()
        .with_max_buffers(4)
        .with_buffer_size(64)
        .build();

    // The global pool OnceLock may or may not be initialized by other
    // tests (nextest process isolation handles this). The key invariant
    // is that isolated() itself does not call global_buffer_pool() or
    // init_global_buffer_pool(). This is verified structurally by code
    // review: builder.rs has no import of global.rs symbols.
}
```

## 11. Rollback plan

If the factory introduces regressions during BPF-9 migration:

1. **Immediate:** Disable the feature flag in `crates/engine/Cargo.toml`
   by removing `bufferpool-isolated-factory` from `default`. All
   `#[cfg(feature = ...)]`-gated code compiles out. Existing tests revert
   to the `BufferPool::new(...).with_*()` pattern automatically because
   BPF-9 PRs are independent per-crate commits.
2. **If regression is in the builder logic:** Fix the builder. The pool
   returned by `build()` delegates to the same `with_*` chain that direct
   construction uses. A builder bug is a configuration bug, not a pool
   internals bug.
3. **If regression is in TLS cross-pool sharing:** Add a `buffer_pool_id`
   discriminator to the TLS cache key so isolated pools never share via
   TLS. This is a forward fix, not a revert.
4. **If regression is in Send/Sync bounds:** The cross-crate test fails
   to compile. Revert the BPF-8 PR and refactor the builder to not hold
   any field that drops the auto-derived bound.
5. **Full revert:** `git revert <BPF-8 merge commit>`. The factory is
   purely additive - removing it breaks no production code and no test
   code (until BPF-9 migrates tests to depend on it).

## 12. Acceptance criteria

- [ ] `BufferPool::isolated() -> BufferPoolBuilder` compiles and is
  accessible from `crates/transfer/tests/` (cross-crate).
- [ ] `BufferPoolBuilder` mirrors all existing `with_*` builder methods
  on `BufferPool`.
- [ ] `build()` produces a pool with identical semantics to direct
  construction.
- [ ] Property test (section 10.1) passes under `proptest`.
- [ ] Concurrency test (section 10.2) passes.
- [ ] No existing test breaks (CI green across all platforms).
- [ ] The factory never reads `GLOBAL_BUFFER_POOL` or any env var.
- [ ] Feature flag `bufferpool-isolated-factory` is default-ON.
- [ ] Disabling the flag (via `--no-default-features`) compiles cleanly.
- [ ] No BPF-9 migration in this PR - existing tests remain unchanged.

## 13. Cross-references

- BPF-6 design: `docs/design/bpf-6-buffer-pool-factory-api.md`
- BPF-7 back-compat: `docs/design/bpf-7-buffer-pool-factory-back-compat.md`
- BufferPool struct: `crates/engine/src/local_copy/buffer_pool/pool.rs:102`
- Global singleton: `crates/engine/src/local_copy/buffer_pool/global.rs:30`
- Engine re-exports: `crates/engine/src/lib.rs:215-217`
- Nextest serial group: `.config/nextest.toml:44`
- Memory notes: `[[project_bufferpool_test_serialization_fragile]]`,
  `[[project_bufferpool_count_cap]]`
