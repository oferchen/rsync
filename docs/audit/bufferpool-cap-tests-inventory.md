# BufferPool capacity-touching tests inventory (BPF-1)

## Scope

`BufferPool` is initialised as an `OnceLock` global singleton (`GLOBAL_BUFFER_POOL`
in `crates/engine/src/local_copy/buffer_pool/global.rs`). Tests that touch
capacity behaviour - constructing pools, mutating `OC_RSYNC_BUFFER_POOL_*` env
vars, or exercising the global pool - must serialise via the EnvGuard pattern
when they could race with each other through the singleton or shared process
env.

The serialisation today is enforced by `.config/nextest.toml`:

```
filter = "test(global_pool) | test(env_var)"
test-group = "global-pool-serial"
[test-groups.global-pool-serial]
max-threads = 1
```

That filter targets test names containing `global_pool` or `env_var`. The
inline `EnvGuard` (RAII set/restore) lives in
`crates/engine/src/local_copy/buffer_pool/global.rs` lines 230-269 and is only
used by the env-var tests in that file.

This inventory enumerates every test that touches:

- the `OC_RSYNC_BUFFER_POOL_SIZE` env var (read or mutated)
- the process-wide `global_buffer_pool()` / `init_global_buffer_pool` singleton
- the `with_memory_cap(...)` builder (hard byte cap with backpressure)
- the `with_byte_budget(...)` builder (soft retention budget)
- per-pool construction that exercises capacity semantics
  (`BufferPool::new`, `BufferPool::with_buffer_size`, `BufferPool::with_allocator`,
  `BufferPool::default`)

`set_capacity` does not exist in the current API.

EnvGuard column:

- `yes` - test (or its module fixture) constructs / drops an `EnvGuard`
- `no`  - test mutates capacity-relevant state but does not use EnvGuard
- `n/a` - test does not mutate the env var or the singleton; capacity is
          scoped to a private `BufferPool` instance only

## Inventory

| File | test_name | what_it_does | EnvGuard_present |
|------|-----------|--------------|------------------|
| crates/engine/src/local_copy/buffer_pool/global.rs:157 | config_default_matches_hardware_parallelism | reads `GlobalBufferPoolConfig::default()`, which reads `OC_RSYNC_BUFFER_POOL_SIZE` | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:169 | config_custom_values | constructs `GlobalBufferPoolConfig` struct only; no env / singleton access | n/a |
| crates/engine/src/local_copy/buffer_pool/global.rs:182 | global_pool_returns_arc | calls `global_buffer_pool()`; lazily initialises (or reads) the OnceLock singleton | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:192 | global_pool_returns_same_instance | two `global_buffer_pool()` calls; relies on Arc::ptr_eq on the singleton | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:200 | global_pool_is_thread_safe | spawns 8 threads that each call `global_buffer_pool()` + acquire/release | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:217 | global_pool_buffers_are_reusable | calls `global_buffer_pool()` + acquire/release | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:273 | env_var_overrides_pool_size | mutates `OC_RSYNC_BUFFER_POOL_SIZE` via `EnvGuard::set("42")` then reads `GlobalBufferPoolConfig::default()` | yes |
| crates/engine/src/local_copy/buffer_pool/global.rs:280 | env_var_zero_ignored | mutates env var via `EnvGuard::set("0")`; asserts fallback to auto-detected | yes |
| crates/engine/src/local_copy/buffer_pool/global.rs:291 | env_var_non_numeric_ignored | mutates env var via `EnvGuard::set("not_a_number")`; asserts fallback | yes |
| crates/engine/src/local_copy/buffer_pool/global.rs:301 | env_var_negative_ignored | mutates env var via `EnvGuard::set("-5")`; asserts fallback | yes |
| crates/engine/src/local_copy/buffer_pool/global.rs:311 | env_var_unset_uses_auto | mutates env var via `EnvGuard::remove`; asserts fallback to auto-detected | yes |
| crates/engine/src/local_copy/buffer_pool/global.rs:321 | init_after_lazy_init_returns_err | calls `global_buffer_pool()` to force lazy init, then calls `init_global_buffer_pool` and expects Err | no |
| crates/engine/src/local_copy/buffer_pool/global.rs:342 | memory_cap_field_round_trips | constructs `GlobalBufferPoolConfig` struct only; no env / singleton / pool | n/a |
| crates/engine/src/local_copy/buffer_pool/global.rs:353 | byte_budget_field_round_trips | constructs `GlobalBufferPoolConfig` struct only; no env / singleton / pool | n/a |
| crates/engine/src/local_copy/buffer_pool/global.rs:364 | byte_budget_zero_is_treated_as_unbounded | filter logic on `Option<usize>` only; no pool / env | n/a |
| crates/engine/src/local_copy/buffer_pool/global.rs:372 | memory_cap_zero_is_treated_as_unbounded | filter logic on `Option<usize>` only; no pool / env | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:9 | no_memory_cap_by_default | constructs `BufferPool::new(4)`; asserts cap defaults | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:16 | memory_cap_is_set | builds pool with `with_memory_cap(4096)`; reads `memory_cap()` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:22 | memory_usage_tracks_outstanding_buffers | builds capped pool, acquires/drops buffers, asserts `memory_usage()` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:40 | allocation_under_cap_succeeds | builds capped pool, acquires up to cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:57 | try_acquire_returns_none_at_cap | builds capped pool, exhausts cap, asserts `try_acquire()` returns None | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:69 | try_acquire_succeeds_after_return | exhausts cap, releases, re-acquires | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:89 | try_acquire_from_returns_none_at_cap | Arc-based variant of try_acquire-at-cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:97 | acquire_blocks_then_succeeds_on_return | spawns a thread that blocks on `acquire_from` until main drops the held buffer | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:124 | memory_cap_with_concurrent_pressure | 8 threads compete on a 4-buffer cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:148 | memory_cap_with_builder_chain | combines `with_allocator` + `with_memory_cap` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:162 | memory_cap_zero_panics | asserts `with_memory_cap(0)` panics | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:167 | memory_usage_without_cap_is_zero | no-cap pool, asserts `memory_usage()` returns 0 | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/memory_cap.rs:175 | memory_cap_backpressure_multiple_waiters | two threads share a 1-buffer cap; verifies wakeup ordering | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:7 | byte_budget_default_is_none | constructs pool, asserts `byte_budget()` defaults | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:16 | byte_budget_is_set_via_builder | builds pool with `with_byte_budget(8192)`; reads accessor | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:22 | byte_budget_allows_returns_below_cap | builds budgeted pool; asserts no overflows under-cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:47 | byte_budget_falls_through_to_direct_alloc_at_cap | gated `not(thread-slab-pool)`; exercises central-pool admission past cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:83 | byte_budget_overflow_counter_accumulates | gated `not(thread-slab-pool)`; multi-thread overflow counting | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:115 | byte_budget_capacity_recycles_after_acquire | gated `not(thread-slab-pool)`; budget release on central acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:159 | byte_budget_with_count_cap_is_min_of_both | gated `not(thread-slab-pool)`; count cap vs byte budget interaction | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:185 | byte_budget_stats_field_exposed | gated `not(thread-slab-pool)`; asserts `stats().total_byte_overflows` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:205 | byte_budget_zero_panics | asserts `with_byte_budget(0)` panics | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/byte_budget.rs:210 | byte_budget_does_not_block_acquires | full-budget acquire must fall through to fresh alloc without blocking | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:8 | telemetry_starts_at_zero | constructs pool; asserts counters | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:17 | telemetry_first_acquire_is_miss | constructs pool; one acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:26 | telemetry_tls_reuse_is_hit | constructs pool; acquire/return/acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:40 | telemetry_hit_rate_calculation | constructs pool; two acquires | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:56 | telemetry_cumulative_across_many_acquires | constructs pool; 100 acquires | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:69 | telemetry_concurrent_counting | constructs pool; 8 threads, 200 acquires each | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:100 | telemetry_with_adaptive_resizing | constructs adaptive pool; 100 acquires | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:110 | telemetry_try_acquire_counts_hits | constructs pool with `with_memory_cap(4096)`; try_acquire path | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:125 | telemetry_try_acquire_from_counts_hits | Arc variant of above; capped pool | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:140 | stats_returns_snapshot | constructs pool; asserts stats snapshot | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:159 | stats_growths_zero_without_adaptive | constructs pool; holds 128 buffers; asserts no growth | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:171 | stats_growths_incremented_on_adaptive_grow | adaptive pool; holds 128 to force grow | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:197 | stats_hit_rate_empty | `BufferPoolStats` struct only | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:209 | stats_hit_rate_all_hits | `BufferPoolStats` struct only | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:220 | stats_hit_rate_all_misses | `BufferPoolStats` struct only | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/telemetry.rs:232 | stats_debug_and_clone | `BufferPoolStats` struct only | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:8 | no_buffer_controller_by_default | constructs pool; reads controller accessors | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:15 | buffer_controller_enabled_via_builder | builds pool with `with_buffer_controller` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:22 | buffer_controller_enables_throughput_tracking | builds pool with controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:30 | buffer_controller_preserves_existing_throughput_tracker | builds pool with throughput + controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:41 | buffer_controller_with_builder_chain | builds pool with `with_memory_cap(8192)` + adaptive + controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:53 | recommended_buffer_size_returns_controller_value_when_enabled | builds pool with controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:63 | record_transfer_feeds_controller | builds pool with controller; records samples | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:89 | controller_recommended_size_supersedes_tracker | builds pool with throughput + controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:112 | controller_convergence_through_pool_api | builds pool with controller; convergence test | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:153 | controller_setpoint_matches_config | builds pool with controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:160 | controller_reset_preserves_recommended_size | builds pool with controller | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:182 | controller_concurrent_record_and_recommend | builds pool with controller; concurrent reads/writes | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:227 | acquire_controlled_from_uses_controller_size | builds pool with controller; controlled acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:255 | acquire_controlled_from_falls_back_to_adaptive_without_controller | constructs pool; controlled acquire fallback | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:268 | acquire_controlled_uses_pool_for_matching_size | builds pool with controller pinned to default | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:285 | acquire_controlled_borrowed_variant | builds pool with controller; borrowed acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:299 | controlled_acquire_size_grows_when_throughput_below_setpoint | builds pool with controller; grows under low throughput | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:330 | controlled_acquire_size_shrinks_when_throughput_above_setpoint | builds pool with controller; shrinks under high throughput | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:361 | controlled_acquire_returned_buffer_resized_to_pool_default | builds pool with controller; verifies return resize | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:385 | controlled_acquire_concurrent_safety | builds pool with controller; concurrent bound check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/controller.rs:430 | controlled_acquire_end_to_end_feedback_loop | builds pool with controller; full feedback loop | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:8 | no_throughput_tracker_by_default | constructs pool; asserts default | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:16 | throughput_tracking_enabled | builds pool with throughput tracking | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:23 | throughput_tracking_custom_alpha | builds pool with custom alpha | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:29 | record_transfer_noop_without_tracking | constructs pool; record no-op | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:36 | record_transfer_updates_throughput | builds pool with tracking; records sample | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:45 | recommended_buffer_size_adapts_to_throughput | builds pool with tracking; asserts recommended size | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:68 | recommended_buffer_size_respects_memory_cap | builds pool with `with_memory_cap(32 KiB)` + throughput; asserts size clamp | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:88 | throughput_tracking_with_builder_chain | builds pool with `with_memory_cap(8192)` + throughput | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/throughput.rs:99 | concurrent_throughput_recording | builds pool with tracking; 8 threads | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:9 | adaptive_resizing_disabled_by_default | constructs pool; asserts default | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:15 | adaptive_resizing_enabled_via_builder | builds adaptive pool | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:21 | adaptive_resizing_with_builder_chain | builds pool with `with_memory_cap(8192)` + adaptive | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:31 | adaptive_pool_grows_under_pressure | adaptive pool; forces growth via held buffers | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:68 | adaptive_pool_shrinks_when_idle | gated `not(thread-slab-pool)`; adaptive shrink integration | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:129 | adaptive_pool_holds_steady_under_balanced_load | adaptive pool; balanced load; asserts no resize | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:154 | adaptive_pool_concurrent_growth | adaptive pool; concurrent acquires force growth | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:192 | adaptive_pool_does_not_grow_without_feature | non-adaptive pool; capacity stays fixed | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:206 | adaptive_pool_shrink_respects_minimum | adaptive pool; lower bound check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:220 | adaptive_pool_grow_respects_maximum | adaptive pool; upper bound check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:236 | adaptive_pool_with_custom_allocator | adaptive pool with TrackingAllocator | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/adaptive_pool.rs:254 | adaptive_pool_deallocates_on_shrink | adaptive pool; verifies shrink dealloc count | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:8 | test_acquire_returns_buffer | constructs pool; one acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:15 | test_buffer_reuse | constructs pool; acquire/return/acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:35 | test_pool_capacity_limit | gated `not(thread-slab-pool)`; count-cap retention check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:51 | test_concurrent_access | constructs pool; 16 threads; asserts `available <= 8` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:73 | test_buffer_guard_deref | constructs pool; guard deref | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:88 | test_buffer_guard_as_mut_slice | constructs pool; guard as_mut_slice | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:99 | test_custom_buffer_size | constructs pool with `with_buffer_size`; reads `buffer_size()` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:107 | test_default_pool | calls `BufferPool::default()`; reads `max_buffers()`, `buffer_size()` | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/pool_basic.rs:114 | test_buffer_length_restored_on_return | constructs pool; verifies length on re-acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:10 | pool_reuses_buffers_under_sequential_pressure | constructs pool; 1000 sequential acquire/return | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:26 | pool_size_stays_bounded_under_burst_allocation | constructs pool; 64 concurrent guards; asserts `available == 4` after drop | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:44 | empty_pool_allocates_fresh_buffer | constructs pool; fresh alloc when empty | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:57 | drop_returns_buffer_to_pool | constructs pool; guard Drop returns buffer | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:76 | borrowed_guard_drop_returns_buffer_to_pool | constructs pool; borrowed guard Drop | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:93 | concurrent_checkout_return_from_multiple_threads | constructs pool; 16 threads x 500 ops | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:124 | concurrent_mixed_guard_types | constructs pool; Arc + borrowed guards | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:157 | concurrent_held_buffers_force_new_allocations | constructs pool capacity 2; holds 2; spawns threads | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:197 | adaptive_buffers_returned_under_concurrent_pressure | constructs pool; adaptive acquires of varied sizes | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:247 | repeated_acquire_release_cycle_reuses_same_buffers | gated `not(thread-slab-pool)`; capacity-bound cycle | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:281 | zero_capacity_pool_never_retains_buffers | constructs `BufferPool::new(0)`; asserts no retention | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:301 | single_capacity_pool_reuses_one_buffer | gated `not(thread-slab-pool)`; capacity 1 reuse | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:337 | with_allocator_uses_custom_allocator | constructs pool via `with_allocator`; one acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:349 | custom_allocator_deallocate_called_on_overflow | gated `not(thread-slab-pool)`; cap overflow dealloc | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:373 | custom_allocator_with_arc_guards | constructs pool with custom allocator; Arc guards | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:391 | custom_allocator_adaptive_acquire | constructs pool with custom allocator; adaptive acquire | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:405 | allocator_accessor_returns_reference | constructs pool with custom allocator; accessor | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/contention.rs:412 | lock_free_acquire_release_under_scoped_concurrency | constructs pool; 16 scoped threads x 500 iter; asserts soft-cap not exceeded | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:9 | concurrent_burst_returns_respect_capacity | constructs pool; 32 threads burst-return; asserts soft cap | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:65 | sequential_returns_respect_soft_capacity | gated `not(thread-slab-pool)`; soft-cap retention check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:78 | tls_absorbs_first_return | constructs pool; TLS absorption | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:95 | tls_overflow_routes_to_central_pool | gated `not(thread-slab-pool)`; TLS overflow path | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:110 | tls_provides_fast_path_acquire | constructs pool with allocator; TLS fast-path | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:132 | tls_wrong_size_buffer_discarded | constructs two pools with different `buffer_size`; TLS size check | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/thread_cache.rs:149 | tls_per_thread_isolation | constructs pool; per-thread TLS isolation | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:18 | slab_thread_teardown_releases_buffers | constructs pool; slab teardown | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:42 | cross_thread_return_routes_through_overflow | constructs pool; cross-thread return | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:66 | slab_bounds_per_thread_memory | constructs pool; asserts slab per-thread caps | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:102 | periodic_donation_drains_long_lived_buffers | constructs pool; slab donation cadence | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:154 | slab_lifo_order_warmest_first | constructs pool; slab LIFO order | n/a |
| crates/engine/src/local_copy/buffer_pool/tests/slab.rs:181 | many_threads_share_pool_without_panic | constructs pool; 16 threads stress | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:126 | new_records_limit | `ByteBudget::new(4096)` only; no pool | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:135 | zero_limit_panics | `ByteBudget::new(0)` panic | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:140 | try_reserve_under_limit_succeeds | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:148 | try_reserve_at_exact_limit_succeeds | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:156 | try_reserve_over_limit_fails_and_counts | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:165 | release_returns_capacity | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:175 | release_saturates_on_underflow | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:182 | overflow_counter_accumulates | `ByteBudget` unit test | n/a |
| crates/engine/src/local_copy/buffer_pool/byte_budget.rs:192 | saturating_add_guards_against_wraparound | `ByteBudget` unit test | n/a |
| crates/transfer/tests/buffer_pool_cross_crate.rs:18 | acquire_and_return_via_public_api | gated `not(thread-slab-pool)`; constructs `BufferPool::with_buffer_size(4, 64)` via cross-crate re-export | n/a |
| crates/transfer/tests/buffer_pool_cross_crate.rs:39 | borrowed_guard_via_public_api | gated `not(thread-slab-pool)`; constructs pool, two guards, asserts `available == 1` | n/a |
| crates/transfer/tests/buffer_pool_cross_crate.rs:54 | global_pool_accessible_cross_crate | calls `global_buffer_pool()` from downstream crate; reads `buffer_size`, `max_buffers` | no |
| crates/transfer/tests/buffer_pool_cross_crate.rs:61 | stats_accessible_cross_crate | constructs pool; two acquires; reads `stats()` | n/a |
| crates/transfer/tests/buffer_pool_cross_crate.rs:77 | default_allocator_is_accessible | constructs `DefaultAllocator` only; no pool | n/a |

## Summary

- Tests that use EnvGuard: 5, all in `crates/engine/src/local_copy/buffer_pool/global.rs`
  (env_var_overrides_pool_size, env_var_zero_ignored, env_var_non_numeric_ignored,
  env_var_negative_ignored, env_var_unset_uses_auto).
- Tests that touch the singleton without EnvGuard: 6
  (global_pool_returns_arc, global_pool_returns_same_instance, global_pool_is_thread_safe,
  global_pool_buffers_are_reusable, init_after_lazy_init_returns_err - all in global.rs -
  plus global_pool_accessible_cross_crate in transfer/tests/buffer_pool_cross_crate.rs).
  All six rely on the nextest `global-pool-serial` test group (filter
  `test(global_pool) | test(env_var)`) for serialisation, except
  init_after_lazy_init_returns_err whose name matches neither filter.
- Tests that touch `with_memory_cap` or `with_byte_budget` on a private pool:
  18 in `tests/memory_cap.rs`, 10 in `tests/byte_budget.rs`, plus 2 in
  `tests/telemetry.rs`, 1 in `tests/controller.rs`, 2 in `tests/throughput.rs`,
  1 in `tests/adaptive_pool.rs`. None require env serialisation since each
  builds its own `BufferPool` instance independent of the singleton.
- Internal `ByteBudget` unit tests (`byte_budget.rs`): 9. Pure type-level
  tests, no pool / env coupling.

## Notable observations for BPF-2 / BPF-3

1. `init_after_lazy_init_returns_err` (global.rs:321) does NOT match the
   nextest filter `test(global_pool) | test(env_var)`. It calls
   `global_buffer_pool()` to force lazy init, then `init_global_buffer_pool`
   expecting Err. If nextest schedules it concurrently with an env-var test,
   the env-var test's `GlobalBufferPoolConfig::default()` read can race with
   this test's lazy-init path. Either rename it to include `global_pool` or
   widen the nextest filter.
2. `global_pool_accessible_cross_crate`
   (transfer/tests/buffer_pool_cross_crate.rs:54) matches the filter, so the
   `global-pool-serial` test group already covers it. Confirm BPF-2 keeps the
   cross-crate test under the same gate.
3. The `EnvGuard` type is duplicated inline in `global.rs`. The same pattern
   exists in `crates/engine/src/concurrent_delta/spill/env.rs` and
   `crates/engine/tests/spill_env_e2e.rs`. BPF-3 may want to extract a
   single shared helper (already noted in `spill_env_e2e.rs` line 52: "Mirrors
   `platform::env::EnvGuard` but is inlined to avoid adding a workspace
   dependency").
4. `recommended_buffer_size_respects_memory_cap` (throughput.rs:68) and
   `throughput_tracking_with_builder_chain` (throughput.rs:88) build pools
   with `with_memory_cap` but interact only with their own pool instance -
   safe.
5. Capacity-bound assertions like `assert_eq!(pool.available(), N)` cluster
   inside `#[cfg(not(feature = "thread-slab-pool"))]` blocks because the
   slab feature changes return routing; the slab-equivalent coverage lives
   in `tests/slab.rs`. This is orthogonal to env serialisation but worth
   noting for BPF-2 when classifying.
