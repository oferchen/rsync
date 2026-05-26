# BPF-9: Per-crate EnvGuard migration plan

## 1. Summary

BPF-9 migrates BufferPool capacity-touching tests off the fragile
`EnvGuard` + `OnceLock` singleton pattern and onto the per-test
`BufferPool::isolated()` factory API (BPF-6 design, BPF-8 implementation).
Migration proceeds crate-by-crate, starting with the `engine` crate
(which owns the BufferPool implementation) and finishing with `transfer`
(the only downstream crate with cap-touching tests).

The end state: no `EnvGuard` references remain in BufferPool capacity
tests. The five env-var contract tests that validate
`GlobalBufferPoolConfig::default()` parsing retain `EnvGuard` permanently
because they test production code that reads `OC_RSYNC_BUFFER_POOL_SIZE`.

## 2. Prerequisites

BPF-9 depends on BPF-8 being merged. The factory API must be available:

- `BufferPool::isolated() -> BufferPoolBuilder` exists and is public.
- `BufferPoolBuilder` supports `with_max_buffers`, `with_buffer_size`,
  `with_memory_cap`, `with_byte_budget`, `with_throughput_tracking`,
  `with_throughput_tracking_alpha`, `with_adaptive_resizing`,
  `with_buffer_controller`, `with_allocator`, and `build()`.
- Feature flag `bufferpool-isolated-factory` is default-ON in
  `crates/engine/Cargo.toml`.
- `BufferPoolBuilder` is re-exported through `crates/engine/src/lib.rs`
  so downstream crates can use it.

## 3. Crates containing cap-touching tests

Two crates contain tests that construct `BufferPool` instances with
explicit capacity parameters or touch the global singleton:

| Crate | Test location | Cap-touching tests | EnvGuard tests |
|-------|---------------|-------------------|----------------|
| `engine` | `crates/engine/src/local_copy/buffer_pool/tests/` | ~80+ across 9 submodules | 5 env-var + 7 singleton (BPF-3) |
| `engine` | `crates/engine/src/local_copy/buffer_pool/global.rs` | 12 in `mod tests` | 5 env-var + 1 LOW + 4 MEDIUM + 1 HIGH |
| `transfer` | `crates/transfer/tests/buffer_pool_cross_crate.rs` | 4 tests | 1 MEDIUM (post-BPF-3 inline) |

No other crates construct `BufferPool` instances in tests. The `core`,
`cli`, and `daemon` crates use `global_buffer_pool()` in production code
but have no cap-touching test coverage.

## 4. Test classification

Tests fall into three categories for migration purposes:

### 4.1 Category A - migrate to factory (bulk of work)

Tests that construct a private `BufferPool` via `BufferPool::new(N)`,
`BufferPool::with_buffer_size(N, S)`, or
`BufferPool::with_allocator(N, S, A)` and then chain `.with_*()` methods.
These tests do not touch the global singleton or environment variables.
They are safe today but should migrate for consistency and to establish
the factory as the canonical test construction pattern.

**Mechanical transformation:**

```rust
// Before
let pool = BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096);

// After
let pool = BufferPool::isolated()
    .with_max_buffers(4)
    .with_buffer_size(1024)
    .with_memory_cap(4096)
    .build();
```

For `BufferPool::new(N)` (uses default buffer size):

```rust
// Before
let pool = BufferPool::new(4);

// After
let pool = BufferPool::isolated()
    .with_max_buffers(4)
    .build();
```

For custom allocator:

```rust
// Before
let pool = BufferPool::with_allocator(4, 1024, TrackingAllocator::new());

// After
let pool = BufferPool::isolated()
    .with_max_buffers(4)
    .with_buffer_size(1024)
    .with_allocator(TrackingAllocator::new())
    .build();
```

### 4.2 Category B - migrate and remove EnvGuard

Tests that touch the global singleton (`global_buffer_pool()` or
`init_global_buffer_pool`) and currently hold `EnvGuard` for env-var
protection (added by BPF-3). These tests can be split: the
capacity-exercise portion migrates to an isolated pool; the
singleton-identity portion (if any) stays on the global path.

Four `global_pool_*` tests in `global.rs` fall here:

- `global_pool_returns_arc` - asserts `pool.max_buffers() > 0`. Can
  migrate to isolated pool (the assertion is about pool construction,
  not singleton identity).
- `global_pool_is_thread_safe` - exercises concurrent `acquire_from` on
  a shared pool. Can migrate entirely to an isolated pool wrapped in
  `Arc`.
- `global_pool_buffers_are_reusable` - exercises acquire/release on a
  shared pool. Can migrate to isolated pool.
- `global_pool_returns_same_instance` - asserts `Arc::ptr_eq` between
  two calls to `global_buffer_pool()`. This MUST stay on the global
  path because it tests singleton identity.

One test in `transfer`:

- `global_pool_accessible_cross_crate` - calls `global_buffer_pool()`
  and reads `buffer_size`/`max_buffers`. This tests cross-crate
  accessibility of the global pool, so it MUST stay on the global path.

### 4.3 Category C - retain EnvGuard permanently

Five env-var contract tests in `global.rs` that test the production
`GlobalBufferPoolConfig::default()` env-var parsing path. These
intentionally mutate `OC_RSYNC_BUFFER_POOL_SIZE` and must keep
`EnvGuard`:

- `env_var_overrides_pool_size`
- `env_var_zero_ignored`
- `env_var_non_numeric_ignored`
- `env_var_negative_ignored`
- `env_var_unset_uses_auto`

These tests also retain the `global-pool-serial` nextest group.

## 5. Per-crate migration plan

### 5.1 Phase 1: `crates/engine/src/local_copy/buffer_pool/tests/`

**PR scope:** One PR covering all 9 test submodules.

**Order within the PR** (by migration complexity, simplest first):

#### 5.1.1 `tests/pool_basic.rs`

10 tests use `BufferPool::new(N)` or `BufferPool::with_buffer_size(N, S)`.

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `test_acquire_returns_buffer` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `test_buffer_reuse` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `test_pool_capacity_limit` | `BufferPool::new(2)` | `.isolated().with_max_buffers(2).build()` |
| `test_concurrent_access` | `BufferPool::new(8)` | `.isolated().with_max_buffers(8).build()` |
| `test_buffer_guard_deref` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `test_buffer_guard_as_mut_slice` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `test_custom_buffer_size` | `BufferPool::with_buffer_size(4, 1024)` | `.isolated().with_max_buffers(4).with_buffer_size(1024).build()` |
| `test_default_pool` | `BufferPool::default()` | No change - tests the `Default` impl, not capacity |
| `test_buffer_length_restored_on_return` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `acquire_adaptive_from_*` (5 tests) | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |

All are Category A. No EnvGuard removal. Pure mechanical rename.

**Sites:** 15 construction sites.

#### 5.1.2 `tests/telemetry.rs`

11 tests. All use `BufferPool::new(N)` or
`BufferPool::with_buffer_size(N, S).with_memory_cap(M)`.

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `telemetry_starts_at_zero` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `telemetry_first_acquire_is_miss` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `telemetry_tls_reuse_is_hit` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `telemetry_hit_rate_calculation` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `telemetry_cumulative_across_many_acquires` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `telemetry_concurrent_counting` | `BufferPool::new(8)` | `.isolated().with_max_buffers(8).build()` |
| `telemetry_with_adaptive_resizing` | `BufferPool::new(4).with_adaptive_resizing()` | `.isolated().with_max_buffers(4).with_adaptive_resizing(true).build()` |
| `telemetry_try_acquire_counts_hits` | `BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096)` | `.isolated().with_max_buffers(4).with_buffer_size(1024).with_memory_cap(4096).build()` |
| `telemetry_try_acquire_from_counts_hits` | Same as above | Same as above |
| `stats_returns_snapshot` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `stats_growths_*` (2 tests) | `BufferPool::new(2)` / `BufferPool::with_buffer_size(2, 1024).with_adaptive_resizing()` | Equivalent factory |

`stats_hit_rate_*` and `stats_debug_and_clone` construct
`BufferPoolStats` directly (no pool) - no migration needed.

**Sites:** 12 construction sites.

#### 5.1.3 `tests/memory_cap.rs`

13 tests. All construct pools with `with_memory_cap()`.

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `no_memory_cap_by_default` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `memory_cap_is_set` | `BufferPool::with_buffer_size(4, 1024).with_memory_cap(4096)` | `.isolated().with_max_buffers(4).with_buffer_size(1024).with_memory_cap(4096).build()` |
| `memory_usage_tracks_outstanding_buffers` | Same pattern | Same pattern |
| `allocation_under_cap_succeeds` | `with_buffer_size(8, 1024).with_memory_cap(4096)` | Equivalent factory |
| `try_acquire_returns_none_at_cap` | `with_buffer_size(4, 1024).with_memory_cap(2048)` | Equivalent factory |
| `try_acquire_succeeds_after_return` | Same | Same |
| `try_acquire_from_returns_none_at_cap` | `with_buffer_size(4, 1024).with_memory_cap(1024)` | Equivalent factory |
| `acquire_blocks_then_succeeds_on_return` | `with_buffer_size(4, 1024).with_memory_cap(1024)` | Equivalent factory |
| `memory_cap_with_concurrent_pressure` | `with_buffer_size(8, 1024).with_memory_cap(4096)` | Equivalent factory |
| `memory_cap_with_builder_chain` | `with_allocator(4, 512, TrackingAllocator::new()).with_memory_cap(2048)` | `.isolated().with_max_buffers(4).with_buffer_size(512).with_allocator(TrackingAllocator::new()).with_memory_cap(2048).build()` |
| `memory_cap_zero_panics` | `BufferPool::new(4).with_memory_cap(0)` | `.isolated().with_max_buffers(4).with_memory_cap(0).build()` |
| `memory_usage_without_cap_is_zero` | `BufferPool::new(4)` | Equivalent factory |
| `memory_cap_backpressure_multiple_waiters` | `with_buffer_size(4, 1024).with_memory_cap(1024)` | Equivalent factory |

**Sites:** 13 construction sites.

#### 5.1.4 `tests/byte_budget.rs`

10 tests. All construct pools with `with_byte_budget()`.

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `byte_budget_default_is_none` | `BufferPool::with_buffer_size(4, 1024)` | `.isolated().with_max_buffers(4).with_buffer_size(1024).build()` |
| `byte_budget_is_set_via_builder` | `with_buffer_size(4, 1024).with_byte_budget(8192)` | `.isolated().with_max_buffers(4).with_buffer_size(1024).with_byte_budget(8192).build()` |
| `byte_budget_allows_returns_below_cap` | `with_buffer_size(8, 1024).with_byte_budget(8 * 1024)` | Equivalent factory |
| `byte_budget_falls_through_to_direct_alloc_at_cap` | `with_buffer_size(8, 1024).with_byte_budget(1024)` | Equivalent factory |
| `byte_budget_overflow_counter_accumulates` | Same | Same |
| `byte_budget_capacity_recycles_after_acquire` | Same | Same |
| `byte_budget_with_count_cap_is_min_of_both` | `with_buffer_size(1, 1024).with_byte_budget(4 * 1024)` | Equivalent factory |
| `byte_budget_stats_field_exposed` | `with_buffer_size(8, 1024).with_byte_budget(1024)` | Equivalent factory |
| `byte_budget_zero_panics` | `with_buffer_size(4, 1024).with_byte_budget(0)` | Equivalent factory |
| `byte_budget_does_not_block_acquires` | `with_buffer_size(8, 1024).with_byte_budget(1024)` | Equivalent factory |

**Sites:** 10 construction sites.

#### 5.1.5 `tests/throughput.rs`

9 tests. Mix of `BufferPool::new(N)` and chained `with_*` calls.

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `no_throughput_tracker_by_default` | `BufferPool::new(4)` | `.isolated().with_max_buffers(4).build()` |
| `throughput_tracking_enabled` | `BufferPool::new(4).with_throughput_tracking()` | `.isolated().with_max_buffers(4).with_throughput_tracking(true).build()` |
| `throughput_tracking_custom_alpha` | `BufferPool::new(4).with_throughput_tracking_alpha(0.5)` | `.isolated().with_max_buffers(4).with_throughput_tracking_alpha(0.5).build()` |
| `record_transfer_noop_without_tracking` | `BufferPool::new(4)` | Equivalent factory |
| `record_transfer_updates_throughput` | `BufferPool::new(4).with_throughput_tracking()` | Equivalent factory |
| `recommended_buffer_size_adapts_to_throughput` | `BufferPool::new(4).with_throughput_tracking_alpha(0.5)` | Equivalent factory |
| `recommended_buffer_size_respects_memory_cap` | `with_buffer_size(4, 4096).with_memory_cap(32 * 1024).with_throughput_tracking_alpha(0.5)` | `.isolated().with_max_buffers(4).with_buffer_size(4096).with_memory_cap(32 * 1024).with_throughput_tracking_alpha(0.5).build()` |
| `throughput_tracking_with_builder_chain` | `with_buffer_size(4, 1024).with_memory_cap(8192).with_throughput_tracking()` | Equivalent factory |
| `concurrent_throughput_recording` | `BufferPool::new(4).with_throughput_tracking()` | Equivalent factory |

**Sites:** 9 construction sites.

#### 5.1.6 `tests/controller.rs`

19 tests. All construct pools with `with_buffer_controller()`.

Most follow the pattern:

```rust
BufferPool::new(4).with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024))
```

which becomes:

```rust
BufferPool::isolated()
    .with_max_buffers(4)
    .with_buffer_controller(ControllerConfig::new(100 * 1024 * 1024))
    .build()
```

One test chains multiple features:

```rust
// buffer_controller_with_builder_chain
BufferPool::with_buffer_size(4, 1024)
    .with_memory_cap(8192)
    .with_adaptive_resizing()
    .with_buffer_controller(ControllerConfig::new(50 * 1024 * 1024))
```

becomes:

```rust
BufferPool::isolated()
    .with_max_buffers(4)
    .with_buffer_size(1024)
    .with_memory_cap(8192)
    .with_adaptive_resizing(true)
    .with_buffer_controller(ControllerConfig::new(50 * 1024 * 1024))
    .build()
```

**Sites:** 19 construction sites.

#### 5.1.7 `tests/adaptive_pool.rs`

10 tests. All construct pools with `with_adaptive_resizing()`.

Pattern:

```rust
BufferPool::with_buffer_size(2, 1024).with_adaptive_resizing()
```

becomes:

```rust
BufferPool::isolated()
    .with_max_buffers(2)
    .with_buffer_size(1024)
    .with_adaptive_resizing(true)
    .build()
```

Custom allocator tests:

```rust
BufferPool::with_allocator(2, 512, AdaptiveTrackingAllocator::new())
    .with_adaptive_resizing()
```

becomes:

```rust
BufferPool::isolated()
    .with_max_buffers(2)
    .with_buffer_size(512)
    .with_allocator(AdaptiveTrackingAllocator::new())
    .with_adaptive_resizing(true)
    .build()
```

**Sites:** 10 construction sites.

#### 5.1.8 `tests/contention.rs`

13 tests. Mix of `BufferPool::new(N)`,
`BufferPool::with_buffer_size(N, S)`, and
`BufferPool::with_allocator(N, S, A)`.

**Sites:** 13 construction sites.

#### 5.1.9 `tests/thread_cache.rs`

7 tests. Mix of `BufferPool::new(N)`,
`BufferPool::with_buffer_size(N, S)`, and
`BufferPool::with_allocator(N, S, A)`.

**Sites:** 8 construction sites.

#### 5.1.10 `tests/slab.rs`

6 tests. All use `BufferPool::new(N)`.

**Sites:** 6 construction sites.

#### 5.1.11 Phase 1 totals

| Submodule | Construction sites | Notes |
|-----------|-------------------|-------|
| `pool_basic.rs` | 15 | Includes 1 `BufferPool::default()` (no change) |
| `telemetry.rs` | 12 | Plus 4 direct `BufferPoolStats` constructions (no change) |
| `memory_cap.rs` | 13 | |
| `byte_budget.rs` | 10 | |
| `throughput.rs` | 9 | |
| `controller.rs` | 19 | |
| `adaptive_pool.rs` | 10 | |
| `contention.rs` | 13 | |
| `thread_cache.rs` | 8 | |
| `slab.rs` | 6 | |
| **Total** | **115** | All Category A (mechanical) |

### 5.2 Phase 2: `crates/engine/src/local_copy/buffer_pool/global.rs`

**PR scope:** Separate PR from Phase 1 because this phase modifies
singleton-touching tests and interacts with the `global-pool-serial`
nextest group.

**Tests to migrate (Category B):**

| Test | Action |
|------|--------|
| `config_default_matches_hardware_parallelism` | Remove `EnvGuard::remove`. Migrate to isolated pool: `BufferPool::isolated().with_max_buffers(expected).build()`. Assert `pool.max_buffers() == expected`. No longer reads `OC_RSYNC_BUFFER_POOL_SIZE`. |
| `global_pool_returns_arc` | Remove `EnvGuard::remove`. Split: create isolated pool, assert `max_buffers > 0` and `buffer_size > 0`. Keep a separate assertion on `global_buffer_pool()` if singleton-identity matters, or remove if redundant with `global_pool_returns_same_instance`. |
| `global_pool_is_thread_safe` | Remove `EnvGuard::remove`. Rewrite to spawn threads against an isolated `Arc<BufferPool>` instead of the global singleton. |
| `global_pool_buffers_are_reusable` | Remove `EnvGuard::remove`. Rewrite to exercise acquire/release on an isolated pool. |

**Tests to keep on global path (Category B, partial):**

| Test | Action |
|------|--------|
| `global_pool_returns_same_instance` | Keep on global path (tests `Arc::ptr_eq`). Remove `EnvGuard::remove` added by BPF-3 - the test does not read env vars; it only asserts pointer identity. Retain in `global-pool-serial` group. |
| `global_pool_init_after_lazy_init_returns_err` | Keep on global path (tests `OnceLock` double-init rejection). Remove `EnvGuard::remove` added by BPF-3. Retain in `global-pool-serial` group. |

**Tests to keep permanently (Category C):**

| Test | Action |
|------|--------|
| `env_var_overrides_pool_size` | No change. Keeps `EnvGuard::set`. |
| `env_var_zero_ignored` | No change. Keeps `EnvGuard::set`. |
| `env_var_non_numeric_ignored` | No change. Keeps `EnvGuard::set`. |
| `env_var_negative_ignored` | No change. Keeps `EnvGuard::set`. |
| `env_var_unset_uses_auto` | No change. Keeps `EnvGuard::remove`. |

**Remaining non-env tests (no migration needed):**

| Test | Reason |
|------|--------|
| `config_custom_values` | Constructs `GlobalBufferPoolConfig` directly. No pool. |
| `memory_cap_field_round_trips` | Same. |
| `byte_budget_field_round_trips` | Same. |
| `byte_budget_zero_is_treated_as_unbounded` | Tests filter logic, no pool. |
| `memory_cap_zero_is_treated_as_unbounded` | Same. |

**Nextest update:**

After migration, the `global-pool-serial` filter narrows. Tests that
migrate off the global singleton no longer need the serial group. Update
the filter to match only the tests that remain on the global path:

```toml
# Before (current)
filter = "test(global_pool) | test(env_var)"

# After BPF-9 Phase 2
filter = "test(global_pool_returns_same_instance) | test(global_pool_init_after_lazy_init) | test(env_var)"
```

Alternatively, rename the migrated tests to drop the `global_pool`
prefix (e.g., `global_pool_returns_arc` becomes `pool_returns_arc`),
so the existing broad filter naturally excludes them. This is the
preferred approach - it avoids enumeration fragility.

### 5.3 Phase 3: `crates/transfer/tests/buffer_pool_cross_crate.rs`

**PR scope:** Separate PR from Phases 1-2.

**Tests to migrate (Category A):**

| Test | Current construction | Factory replacement |
|------|---------------------|---------------------|
| `acquire_and_return_via_public_api` | `BufferPool::with_buffer_size(4, 64)` | `.isolated().with_max_buffers(4).with_buffer_size(64).build()` |
| `borrowed_guard_via_public_api` | `BufferPool::with_buffer_size(4, 32)` | `.isolated().with_max_buffers(4).with_buffer_size(32).build()` |
| `stats_accessible_cross_crate` | `BufferPool::with_buffer_size(2, 128)` | `.isolated().with_max_buffers(2).with_buffer_size(128).build()` |

**Tests to keep on global path (Category B):**

| Test | Action |
|------|--------|
| `global_pool_accessible_cross_crate` | Keep on global path. Tests cross-crate accessibility of the global singleton. Remove the inline `EnvGuard` added by BPF-3. |

**Additional cleanup:**

- Remove the inline `EnvGuard` struct from this file (added by BPF-3).
  After migration, only `global_pool_accessible_cross_crate` remains,
  and it no longer needs env-var protection.
- Add `BufferPoolBuilder` to the import list (re-exported by `engine`).

**Import update:**

```rust
// Before
use engine::{BufferPool, BufferPoolStats, DefaultAllocator, global_buffer_pool};

// After
use engine::{BufferPool, BufferPoolBuilder, BufferPoolStats, DefaultAllocator, global_buffer_pool};
```

**New cross-crate canary test:**

Add one test that exercises the factory from `transfer` to verify the
re-export works:

```rust
#[test]
fn factory_accessible_cross_crate() {
    let pool = BufferPool::isolated()
        .with_max_buffers(2)
        .with_buffer_size(64)
        .build();
    assert_eq!(pool.max_buffers(), 2);
    assert_eq!(pool.buffer_size(), 64);
}
```

**Sites:** 3 migration + 1 retain + 1 new = 5 total.

## 6. How the factory API replaces EnvGuard

The factory eliminates three fragility sources:

### 6.1 No environment variable coupling

`BufferPool::isolated().with_max_buffers(N).build()` never reads
`OC_RSYNC_BUFFER_POOL_SIZE`. Capacity is explicit in the builder call.
Tests no longer need `EnvGuard::remove` to defend against ambient env
state.

### 6.2 No `OnceLock` singleton coupling

The factory returns an owned `BufferPool` that is not registered with
`GLOBAL_BUFFER_POOL`. Two concurrent tests each get their own pool
instance with independent state - no serialization needed.

### 6.3 No nextest filter fragility

Tests that use isolated pools do not need the `global-pool-serial`
nextest group. Renaming a test function cannot break its serialization
because there is no serialization to break.

**Contrast with EnvGuard:**

| Concern | EnvGuard pattern | Factory pattern |
|---------|-----------------|-----------------|
| Env-var mutation | `unsafe { set_var / remove_var }` | None |
| Process-wide state | `OnceLock<Arc<BufferPool>>` shared | Per-test owned instance |
| Serialization | `global-pool-serial` (max-threads=1) | Full parallelism |
| Rename safety | Filter must match test name | No filter dependency |
| `unsafe` blocks | 3 per `EnvGuard` (set/remove/drop) | Zero |

## 7. Verification criteria

### 7.1 Per-phase CI gates

Each phase PR must pass all CI checks:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings`
- `cargo nextest run --workspace --all-features` on Linux, macOS, Windows
- No new `EnvGuard` usage in migrated tests (enforced by BPF-4/BPF-5
  CI lint until BPF-10 retires it)

### 7.2 Post-migration verification

After all three phases are merged:

1. **No EnvGuard in capacity tests.** Grep
   `crates/engine/src/local_copy/buffer_pool/tests/` for `EnvGuard` -
   must return zero matches.

2. **EnvGuard only in env-var contract tests.** Grep
   `crates/engine/src/local_copy/buffer_pool/global.rs` for `EnvGuard`
   - must match only the five `env_var_*` tests and the `EnvGuard`
   struct/impl definitions themselves.

3. **No inline EnvGuard in transfer.** Grep
   `crates/transfer/tests/buffer_pool_cross_crate.rs` for `EnvGuard` -
   must return zero matches.

4. **Nextest serial group narrowed.** Run
   `cargo nextest list --all-features -E 'test(global_pool) | test(env_var)'`
   and confirm only the retained singleton tests and env-var tests
   appear.

5. **Parallel stress test.** Run the full buffer pool test suite 20
   times under maximum parallelism:

   ```sh
   for i in $(seq 1 20); do
     cargo nextest run -p engine --all-features \
       -E 'test(/buffer_pool/)' \
       --color never --no-fail-fast 2>&1 | tail -1
   done
   ```

   All 20 runs must report 0 failures.

### 7.3 Quantitative criteria

| Metric | Before BPF-9 | After BPF-9 |
|--------|--------------|-------------|
| `EnvGuard` usages in `tests/` submodules | 0 | 0 (unchanged) |
| `EnvGuard` usages in `global.rs` (test code) | 12 (5 env-var + 7 BPF-3) | 5 (env-var only) |
| `EnvGuard` usages in `transfer` | 1 (inline struct + usage) | 0 |
| Tests in `global-pool-serial` group | ~12 | ~7 (5 env-var + 2 singleton) |
| Tests using `BufferPool::isolated()` | 0 (BPF-8 only) | ~118 |
| `unsafe` blocks in buffer pool tests | 3 (EnvGuard impls) | 3 (retained for env-var tests) |

## 8. Rollback plan

### 8.1 Per-phase revert

Each phase is a single PR. If a phase introduces test failures:

1. Revert the merge commit: `git revert <merge-sha>`.
2. The factory API (BPF-8) is unaffected - it is additive.
3. Tests fall back to the pre-migration `BufferPool::new(N).with_*()`
   pattern.
4. `EnvGuard` wrappers (BPF-3) remain in place.

### 8.2 Factory API regression

If the factory itself produces pools with different semantics than
direct construction:

1. The BPF-8 property test (`isolated_pool_cap_matches_direct_construction`)
   should catch this. If it does not, add a targeted regression test.
2. Fix the factory builder's `build()` method - it delegates to the same
   `with_*` chain that direct construction uses.
3. If the fix is non-trivial, disable the feature flag
   (`bufferpool-isolated-factory`) in `crates/engine/Cargo.toml` by
   removing it from `default`. All `#[cfg(feature = ...)]`-gated code
   compiles out. Existing tests revert to `BufferPool::new(N).with_*()`
   automatically.

### 8.3 TLS cross-pool interference

If two isolated pools with the same `buffer_size` interfere via the
thread-local cache (e.g., a capacity test sees a buffer from another
test's pool):

1. Use distinct `buffer_size` values in the conflicting tests (e.g.,
   1024 vs 1025). TLS is keyed by buffer length, so different sizes
   never share.
2. Long-term fix: add a `pool_id` discriminator to the TLS cache key
   (forward fix in the factory, not a revert).

### 8.4 Nextest filter regression

If narrowing the `global-pool-serial` filter causes a race between
a retained singleton test and a newly-parallel test:

1. Widen the filter back to the original
   `test(global_pool) | test(env_var)`.
2. Investigate which migrated test still touches the singleton
   (programming error in the migration - fix the test, not the filter).

## 9. Migration ordering rationale

Phase 1 (engine tests/) before Phase 2 (engine global.rs) because:

- Phase 1 is purely mechanical (Category A). No behavior change, no
  nextest config change, no EnvGuard removal. Low risk, high volume.
  Building confidence in the factory before touching singleton tests.
- Phase 2 changes test semantics (moving off the global pool) and
  modifies the nextest config. Review requires more scrutiny.

Phase 3 (transfer) last because:

- It is the only cross-crate consumer. If the factory re-export is
  broken, Phase 3 catches it.
- It has the fewest tests (4 + 1 new), so the blast radius is smallest.

## 10. Cross-references

- BPF-1 inventory: `docs/audit/bufferpool-cap-tests-inventory.md`
- BPF-2 gap list: `docs/audit/bufferpool-envguard-gap-list.md`
- BPF-3 remediation: `docs/design/bpf-3-envguard-gap-remediation.md`
- BPF-4 CI lint spec: `docs/design/bpf-4-envguard-ci-lint-spec.md`
- BPF-6 factory API design: `docs/design/bpf-6-buffer-pool-factory-api.md`
- BPF-7 back-compat: `docs/design/bpf-7-buffer-pool-factory-back-compat.md`
- BPF-8 factory implementation: `docs/design/bpf-8-buffer-pool-factory-impl.md`
- Global pool singleton: `crates/engine/src/local_copy/buffer_pool/global.rs:30`
- Env var const: `crates/engine/src/local_copy/buffer_pool/global.rs:65`
- Nextest serial group: `.config/nextest.toml:36-45`
- Inline EnvGuard: `crates/engine/src/local_copy/buffer_pool/global.rs:230-269`
- Cross-crate tests: `crates/transfer/tests/buffer_pool_cross_crate.rs`
- Memory notes: `project_bufferpool_test_serialization_fragile`,
  `project_bufferpool_count_cap`
