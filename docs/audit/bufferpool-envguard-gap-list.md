# BufferPool EnvGuard gap list (BPF-2)

## Scope

Derived from the BPF-1 inventory at
`docs/audit/bufferpool-cap-tests-inventory.md`. This document classifies the
tests that touch BufferPool capacity state without an `EnvGuard` (or otherwise
miss the `global-pool-serial` nextest serialisation), so BPF-3 can wrap or
re-filter them with surgical changes.

Serialisation today: `.config/nextest.toml` defines test-group
`global-pool-serial` with `max-threads = 1` and filter
`test(global_pool) | test(env_var)`. Tests whose names do not match that filter
run with default concurrency.

`EnvGuard` itself is the inline RAII helper at
`crates/engine/src/local_copy/buffer_pool/global.rs:230-269` (also reproduced
in `crates/engine/src/concurrent_delta/spill/env.rs` and
`crates/engine/tests/spill_env_e2e.rs`). BPF-8 may extract a shared helper.

Risk-level definitions:

- **HIGH** - mutates singleton or env state AND misses the
  `global-pool-serial` nextest filter; can race with parallel tests today.
- **MEDIUM** - mutates singleton state but is covered by the
  `global-pool-serial` filter; safe today but relies on the filter staying in
  sync with test names (rename-fragile).
- **LOW** - reads or constructs only a private `BufferPool` instance; no
  process-wide coupling.

## Tests that already use EnvGuard (confirmed from BPF-1)

| File | test_name | EnvGuard scope |
|------|-----------|----------------|
| crates/engine/src/local_copy/buffer_pool/global.rs:273 | env_var_overrides_pool_size | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "42")` |
| crates/engine/src/local_copy/buffer_pool/global.rs:280 | env_var_zero_ignored | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "0")` |
| crates/engine/src/local_copy/buffer_pool/global.rs:291 | env_var_non_numeric_ignored | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "not_a_number")` |
| crates/engine/src/local_copy/buffer_pool/global.rs:301 | env_var_negative_ignored | `EnvGuard::set(ENV_BUFFER_POOL_SIZE, "-5")` |
| crates/engine/src/local_copy/buffer_pool/global.rs:311 | env_var_unset_uses_auto | `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)` |

BPF-1 captured all five. No additions needed.

## Gap list: tests that touch singleton without EnvGuard

| file | test_name | what_it_mutates | risk_level | proposed_fix |
|------|-----------|-----------------|------------|--------------|
| crates/engine/src/local_copy/buffer_pool/global.rs:321 | init_after_lazy_init_returns_err | Forces lazy init of `GLOBAL_BUFFER_POOL` `OnceLock`, then calls `init_global_buffer_pool` expecting `Err`. Mutates singleton init state; does NOT match `test(global_pool) | test(env_var)` filter, so runs in parallel with everything else. | HIGH | Rename to `global_pool_init_after_lazy_init_returns_err` so it matches `test(global_pool)`, OR widen the nextest filter in `.config/nextest.toml` to include `test(init_after_lazy_init)`. BPF-8 factory migration would remove the singleton dependency entirely. |
| crates/engine/src/local_copy/buffer_pool/global.rs:182 | global_pool_returns_arc | Calls `global_buffer_pool()`; triggers lazy init of singleton on first run. Capacity comes from `GlobalBufferPoolConfig::default()` which reads `OC_RSYNC_BUFFER_POOL_SIZE`. | MEDIUM | Covered by `global-pool-serial` filter today. Add EnvGuard pin to a known value to defend against future env-var tests in other crates, or migrate to BPF-8 factory pool. |
| crates/engine/src/local_copy/buffer_pool/global.rs:192 | global_pool_returns_same_instance | Calls `global_buffer_pool()` twice; asserts `Arc::ptr_eq`. Reads (and may init) the singleton. | MEDIUM | Same as above. Filter-covered; rename-fragile. Pin via EnvGuard or migrate in BPF-8. |
| crates/engine/src/local_copy/buffer_pool/global.rs:200 | global_pool_is_thread_safe | Spawns 8 threads calling `global_buffer_pool()` + `acquire_from`. Exercises singleton init under concurrency. | MEDIUM | Filter-covered. Add EnvGuard for env-var pin, or migrate to BPF-8 factory. |
| crates/engine/src/local_copy/buffer_pool/global.rs:217 | global_pool_buffers_are_reusable | Calls `global_buffer_pool()`, acquires/releases a buffer; mutates pool free-list state. Sensitive to other tests racing on the same Arc. | MEDIUM | Filter-covered. Pin env via EnvGuard, or migrate to a private pool in BPF-8. |
| crates/transfer/tests/buffer_pool_cross_crate.rs:54 | global_pool_accessible_cross_crate | Calls `global_buffer_pool()` from the `transfer` crate; reads `buffer_size`, `max_buffers`. Triggers lazy init if not yet initialised. | MEDIUM | Filter-covered (name matches `test(global_pool)`). Confirm nextest test-group `global-pool-serial` applies across crates (it does - filter is global). BPF-8 factory migration would remove cross-crate singleton dependency. |

## Findings

1. **One HIGH-risk gap**: `init_after_lazy_init_returns_err` is the only
   singleton-touching test that misses the `global-pool-serial` filter. Its
   name contains neither `global_pool` nor `env_var`. Today nextest runs it
   in parallel with the env-var tests; if `GlobalBufferPoolConfig::default()`
   in an env-var test runs while this test is mid-`init_global_buffer_pool`,
   the env-var test sees the racing singleton state. Fix is a one-line rename
   or filter widening - both are surgical.
2. **Five MEDIUM-risk gaps**: the four `global_pool_*` tests in `global.rs`
   plus `global_pool_accessible_cross_crate` in
   `crates/transfer/tests/buffer_pool_cross_crate.rs`. All are filter-covered
   today via name match, but none defend against a future rename or against
   another crate's tests setting `OC_RSYNC_BUFFER_POOL_SIZE` without using
   the filter group. Adding an `EnvGuard` pin (e.g.
   `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)` at the top of each) would make
   them robust without changing the filter; BPF-8 factory migration is the
   long-term fix.
3. **No EnvGuard usages missed by BPF-1**: the 5 env-var tests in `global.rs`
   are exhaustive. No further additions to the "already uses EnvGuard"
   list.

## Recommended BPF-3 work units

In order of urgency:

1. Rename `init_after_lazy_init_returns_err` -> `global_pool_init_after_lazy_init_returns_err`
   so it matches the existing `test(global_pool)` filter. Single-line change
   in `crates/engine/src/local_copy/buffer_pool/global.rs:321`.
2. Add `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)` (or `set` to a deterministic
   value) at the top of each of the five MEDIUM-risk tests so they no longer
   depend on ambient env state. The inline `EnvGuard` is already in scope in
   `global.rs`; `buffer_pool_cross_crate.rs` either inlines its own copy or
   waits for BPF-8 to expose a shared helper.
3. Defer the factory-pool migration to BPF-8 as already planned; BPF-3 is
   scoped to EnvGuard wrappers only.

## Cross-references

- BPF-1 inventory: `docs/audit/bufferpool-cap-tests-inventory.md`
- Nextest serialisation: `.config/nextest.toml` lines 36-45
- Inline EnvGuard: `crates/engine/src/local_copy/buffer_pool/global.rs:230-269`
- Sibling EnvGuard implementations:
  `crates/engine/src/concurrent_delta/spill/env.rs`,
  `crates/engine/tests/spill_env_e2e.rs`
