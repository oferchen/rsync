# BufferPool test isolation guide

## 1. Audience

Rust test authors writing cap-affecting tests against
[`BufferPool`](../../crates/engine/src/local_copy/buffer_pool/pool.rs) in any
workspace crate. A cap-affecting test is one that exercises pool capacity
(`memory_cap`, `byte_budget`, slot count, buffer size) or interacts with the
process-wide singleton via `init_global_buffer_pool` /
`global_buffer_pool`.

This document supersedes earlier `EnvGuard`-based guidance once BPF-9 (the
per-test factory migration) and BPF-10 (the `EnvGuard` removal pass) land.
Until then, see the [Transition](#7-transition-period-current-state-pre-bpf-9)
section for what to do today.

## 2. Why isolation matters

`BufferPool` is a process-wide singleton. The pool is initialised lazily on
the first call to `global_buffer_pool()` (see
`crates/engine/src/local_copy/buffer_pool/global.rs:106`) or eagerly via
`init_global_buffer_pool` (`global.rs:132`). The lazy default reads the
`OC_RSYNC_BUFFER_POOL_SIZE` environment variable (`global.rs:65`) to pick
its slot count.

Two consequences follow:

- A test that mutates `OC_RSYNC_BUFFER_POOL_SIZE`, or any other
  `OC_RSYNC_BUFFER_POOL_*` cap-affecting variable, races with every other
  test that touches the singleton. Under `cargo nextest` parallel
  execution, the first test wins the `OnceLock` and the rest observe a pool
  configured by whichever sibling raced ahead.
- A test that calls `init_global_buffer_pool` after the singleton has
  already initialised silently no-ops (the function returns its config back
  in `Err`), so the test's intended configuration is not applied.

Both failure modes present as flaky tests that pass in isolation
(`cargo nextest run -E 'test(my_cap_test)'`) and fail under the full
parallel suite. Historical fragility is tracked in memory note
`[[project_bufferpool_test_serialization_fragile]]`; the byte-cap
regression coverage that motivates strict isolation is tracked in
`[[project_bufferpool_count_cap]]`.

The interim mitigation is to serialise every cap-touching test behind
`EnvGuard` (`crates/platform/src/env.rs:19`), which scopes the env-var
mutation and restores the previous value on drop. The structural fix
(BPF-6..BPF-9) replaces the singleton dependency with per-test isolated
instances so serialisation is no longer required.

## 3. Recommended pattern (post-BPF-9)

Once BPF-9 lands, the recommended pattern for every cap-affecting test is
to construct an isolated `BufferPool` instance via the factory API. The
instance is fully local to the test, holds no global state, and is safe
under `cargo nextest` parallel execution without any guard.

```rust
use engine::local_copy::buffer_pool::BufferPool;

#[test]
fn my_cap_test() {
    // Per-test isolated BufferPool. No env var, no EnvGuard,
    // no interaction with the process-wide singleton.
    let pool = BufferPool::isolated()
        .with_memory_cap(8 * 1024 * 1024)
        .with_byte_budget(2 * 1024 * 1024)
        .with_buffer_size(64 * 1024)
        .build();

    // Exercise the pool directly.
    assert_eq!(pool.memory_cap(), Some(8 * 1024 * 1024));
}
```

The factory entry point (`BufferPool::isolated`) and the terminal `build()`
land in BPF-6. The intermediate knob methods reuse the existing builder
signatures on `BufferPool` itself (see [Configuration knobs](#5-configuration-knobs))
so the migration in BPF-9 is mechanical.

## 4. What to AVOID

The following anti-patterns either cause flakiness today or will be flagged
by the BPF-5 CI lint (`tools/ci/lint_bufferpool_cap_tests.sh`). The lint is
enforced on every PR.

- **Do NOT mutate `OC_RSYNC_BUFFER_POOL_SIZE`** or any other
  `OC_RSYNC_BUFFER_POOL_*` environment variable in test bodies. The
  variable is process-global; the `OnceLock` singleton reads it once and
  caches the result for the rest of the test binary. Cap-affecting env
  mutations without an `EnvGuard` fail the BPF-5 lint.
- **Do NOT call `init_global_buffer_pool` from a test.** The singleton must
  remain singleton. A second `init_*` call after lazy initialisation
  returns its config back in `Err` and silently fails to apply, leaving
  the test running against an unrelated pool. Construct an isolated
  instance instead.
- **Do NOT share a single `BufferPool` across multiple `#[test]` functions
  in the same module** unless every shared user is explicitly serialised
  with `serial_test::serial` or an equivalent guard. Per-test isolation is
  cheap and removes the need to reason about ordering.
- **Do NOT add `#[allow(...)]` to silence the BPF-5 lint.** The lint is a
  signal that the test needs migration, not a stylistic warning.

## 5. Configuration knobs

The factory delegates to the existing `BufferPool` builder methods at
`crates/engine/src/local_copy/buffer_pool/pool.rs:232..409`. The table
below lists every knob currently exposed by the builder and how a test
should reach for it.

| Builder method                                | What it controls                                                                                | Default when unset                                                                                | When a test should override it                                                              |
|-----------------------------------------------|--------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------|
| `BufferPool::new(max_buffers)`                | Soft slot count for the central queue.                                                          | `available_parallelism()`, overridden by `OC_RSYNC_BUFFER_POOL_SIZE` in production paths.         | Always - the factory wraps this constructor; pick a deterministic value.                     |
| `with_buffer_size(max_buffers, buffer_size)`  | Per-buffer byte size; associated function, not chained.                                         | `COPY_BUFFER_SIZE` (128 KiB).                                                                     | Test wants specific small buffers or wants to exercise the size-recommendation path.         |
| `.with_memory_cap(max_bytes)`                 | Hard ceiling on outstanding (checked-out) bytes; acquires block when the cap is reached.        | `None` - no cap.                                                                                  | Test wants to exercise cap-exceeded backpressure or `try_acquire` returning `None`.          |
| `.with_byte_budget(max_bytes)`                | Soft ceiling on retained bytes in the central pool; admissions over the budget deallocate.     | `None` - retained bytes unbounded.                                                                | Test wants to exercise byte-budget rejection (`total_byte_overflows`).                       |
| `.with_throughput_tracking()`                 | Enables the EMA throughput tracker and `recommended_buffer_size`.                               | Disabled - zero-cost when off.                                                                    | Test asserts on throughput stats or recommended buffer size.                                  |
| `.with_throughput_tracking_alpha(alpha)`      | Variant of `with_throughput_tracking` with custom EMA smoothing factor in `(0.0, 1.0]`.        | Disabled.                                                                                         | Test wants a specific smoothing factor for deterministic EMA values.                          |
| `.with_adaptive_resizing()`                   | Enables grow/shrink of the soft slot capacity based on hit/miss pressure.                       | Disabled - capacity fixed at construction time.                                                   | Test wants to exercise pressure-driven resizing; keep disabled when asserting on cap.        |
| `.with_buffer_controller(config)`             | Enables PID-style buffer-size control; auto-enables throughput tracking.                        | Disabled.                                                                                         | Test asserts on controller-driven `recommended_buffer_size` evolution.                       |
| `.with_allocator(max_buffers, size, alloc)`   | Replaces `DefaultAllocator` with a custom allocator (e.g. page-aligned, instrumented).         | `DefaultAllocator`.                                                                               | Test needs deterministic allocation accounting or a fault-injection allocator.              |

Defaults that are not relevant to the test should be left unset rather
than set to "the production value": a per-test isolated pool is meant to
deviate from production defaults intentionally, so an absent knob is
strictly clearer than a magic-number repetition.

## 6. Concurrency model

- Multiple `BufferPool::isolated()` instances are fully independent. Each
  instance owns its own `ArrayQueue`, atomic counters, optional
  `MemoryCap`, optional `ByteBudget`, and optional `ThroughputTracker`.
  There is no shared mutex and no shared atomic state between instances.
- Construction is `O(1)` (the `ArrayQueue` is allocated once at the
  default queue capacity) and acquires no buffers until the first
  `acquire` / `try_acquire` call.
- The isolated path bypasses the `OnceLock` global entirely. Production
  code paths continue to use `global_buffer_pool()` and the singleton;
  the factory is a test-only entry point with the same public surface.
- Each isolated pool maintains its own thread-local cache slot. Tests
  spawning multiple threads against a single isolated pool see the same
  two-level cache hierarchy production gets.

## 7. Transition period (current state, pre-BPF-9)

Until BPF-9 ships the factory migration and BPF-10 removes the residual
`EnvGuard` usage, the rules are:

- **New tests being written today.** If the BPF-6 factory API has landed,
  prefer it. Otherwise, wrap any env-var mutation in `EnvGuard` from
  `crates/platform/src/env.rs:19`. The inline `EnvGuard` duplicates in
  `crates/engine/src/local_copy/buffer_pool/global.rs:231` are equivalent
  and may be used in-place if you are co-locating with existing tests in
  that module, but new tests in other crates should reach for the
  canonical guard.
- **Modifying an existing cap-test.** Leave the existing `EnvGuard`
  serialisation in place; do not migrate ahead of BPF-9. Premature
  migration creates merge conflicts against the BPF-9 sweep.
- **The BPF-5 lint** (`tools/ci/lint_bufferpool_cap_tests.sh`) catches new
  tests that touch `OC_RSYNC_BUFFER_POOL_*` without holding an
  `EnvGuard`. If the lint fires on your PR, the fix is to either add the
  guard or - once BPF-6 has landed - migrate the test to the factory
  pattern.
- **End of transition.** When BPF-10 lands and every cap-test has been
  migrated to `BufferPool::isolated()`, the `OC_RSYNC_BUFFER_POOL_*`
  variables are removed from the supported test surface and this section
  is deleted from the document.

## 8. Examples in the codebase

Pre-migration (until BPF-9), the canonical correct-usage examples are
`EnvGuard`-based:

- `crates/engine/src/local_copy/buffer_pool/global.rs:272..312` -
  `env_var_overrides_pool_size`, `env_var_zero_ignored`,
  `env_var_non_numeric_ignored`, `env_var_negative_ignored`,
  `env_var_unset_uses_auto`. Each test wraps the env mutation
  in the inline `EnvGuard` and asserts on `GlobalBufferPoolConfig::default()`.

Post-migration (added by BPF-9), the canonical factory-pattern examples
will be listed here. The expected entries are the migrations of the
tests above plus the byte-budget cap tests; the exact file/line
references will be filled in when BPF-9 lands.

## 9. Cross-references

- BPF-4 EnvGuard CI lint specification:
  `docs/design/bpf-4-envguard-ci-lint-spec.md` (PR #4916).
- BPF-6 factory API design: `docs/design/bpf-6-buffer-pool-factory-api.md`
  (expected filename; lands with #2824).
- BPF-7 back-compat assessment: tracked by #2825.
- BPF-9 factory migration sweep: tracked by #2827.
- Memory notes:
  - `[[project_bufferpool_test_serialization_fragile]]` - historical
    fragility of the global `OnceLock` pool under cap-test serialisation.
  - `[[project_bufferpool_count_cap]]` - byte-cap regression coverage that
    motivates strict per-test isolation.
