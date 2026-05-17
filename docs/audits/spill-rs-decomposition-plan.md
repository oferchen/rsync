# spill.rs Decomposition Plan (SPL-1, #2323)

`crates/engine/src/concurrent_delta/spill.rs` is 1229 lines (44.3 KB),
substantially over the 650 LoC cap enforced by `tools/enforce_limits.sh`.
This document maps the file's existing structure to the six-way submodule
split targeted by tasks SPL-2 through SPL-8.

The goal is a pure decomposition: no behaviour change, no API change.
Public re-exports stay at `crates/engine/src/concurrent_delta/spill/mod.rs`
so downstream callers (`consumer.rs`, `parallel_apply.rs`,
`strategy.rs`) keep working unchanged.

## Submodule Map

| Target module                                  | Source line range | Key items                                                                                                                            | Depends on                          | Follow-up task |
|------------------------------------------------|-------------------|--------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------|----------------|
| `spill/error.rs`                               | 70-133            | `SpillError` enum, `Display`, `Error`, `From<CapacityExceeded>`, `From<io::Error>`, `io_error`, `is_out_of_space`                    | `reorder::CapacityExceeded`         | SPL-2          |
| `spill/codec.rs`                               | 135-161           | `SpillCodec` trait                                                                                                                   | std `io::{Read, Write}`             | SPL-3          |
| `spill/tempfile.rs` (backend + ENOSPC + retry) | 163-190, 564-671, 674-687 | `SpillBackend`, `ReadWriteSeek`, `open_backend`, `spill_item`, `write_record`, `reload_item`, `recreate_spill_dir` | `error`, `codec`                    | SPL-4          |
| `spill/policy.rs`                              | 57-68, 509-562    | `DEFAULT_SPILL_THRESHOLD`, `HOT_ZONE`, `spill_excess` (hot-zone + reverse-order candidate selection)                                 | `tempfile` (calls `spill_item`)     | SPL-5          |
| `spill/stats.rs`                               | 258-273           | `SpillStats` struct                                                                                                                  | (none)                              | SPL-6          |
| `spill/buffer.rs`                              | 192-507           | `SpillableReorderBuffer<T>` struct + `Debug` + `new` / `with_spill_dir` / `with_default_threshold` / `insert` / `force_insert` / `next_in_order` / `drain_ready` / `next_expected` / `buffered_count` / `is_empty` / `capacity` / `spill_stats` / `threshold` / `spill_dir` | `error`, `codec`, `tempfile`, `policy`, `stats` | SPL-7          |
| `spill/tests/*` (per-module)                    | 689-1228          | See "Test split plan" below                                                                                                          | (each test follows its module)      | SPL-8          |

Line ranges include leading rustdoc on each item.

## Recommended Split Order

Pull leaves first to minimise churn in `buffer.rs`, which is the busiest
consumer of every other submodule.

1. **SPL-2 `error.rs`** - zero internal callers in this file, only the
   public `SpillError` surface. Smallest and safest extraction. Validates
   the `pub use` re-export pattern in `spill/mod.rs` end-to-end before
   touching anything bigger.
2. **SPL-3 `codec.rs`** - trait-only, no implementations live in spill.rs
   (the production `DeltaResult` impl lives in
   `concurrent_delta/types.rs`). Trivially independent.
3. **SPL-6 `stats.rs`** - plain data carrier, no methods. Independent.
4. **SPL-4 `tempfile.rs`** - lifts `SpillBackend`, `ReadWriteSeek`,
   `open_backend`, and the three I/O helpers (`spill_item`,
   `write_record`, `reload_item`, `recreate_spill_dir`) into a single
   `tempfile` module. These four methods form the disk-facing surface and
   are the only place ENOSPC and temp-dir-vanish handling lives (added in
   #2247). Buffer becomes a thin caller. To keep them out of an
   `impl SpillableReorderBuffer` block, refactor them to take an explicit
   `&mut SpillState { backend, dir, write_pos, spill_index,
   dir_recreate_count }` borrow.
5. **SPL-5 `policy.rs`** - extracts `spill_excess` plus the two
   thresholds. Depends on SPL-4 because it calls `spill_item` and
   manipulates `spill_count`/`memory_used`. Either move
   `spill_excess` as a free function over the same `SpillState`-style
   borrow, or keep it as an `impl SpillableReorderBuffer` method that
   delegates to free helpers in `policy::`. Free-function form is
   preferred because it leaves `buffer.rs` doing only orchestration.
6. **SPL-7 `buffer.rs`** - last. By the time this runs, `buffer.rs`
   imports `error::SpillError`, `codec::SpillCodec`, `stats::SpillStats`,
   and calls into `tempfile::*` and `policy::*`. The public type and
   surface stay unchanged.
7. **SPL-8 `tests/`** - tests move alongside their module after the code
   moves. Mechanical step.

This order keeps every intermediate PR compilable and CI-green; each step
removes 50-400 lines from `spill.rs` without rewriting any logic.

## Cross-Cutting Helpers and Risks

The following items do not split cleanly along the six axes and need
explicit handling:

- **`write_record` and `reload_item` mutate `spill_file`, `spill_write_pos`,
  `spill_index`, `dir_recreate_count`** all of which currently live on
  `SpillableReorderBuffer`. The proposed `SpillState` borrow groups them
  so the tempfile module can own the disk-side mutation surface without
  reaching into `buffer.rs`. Without this borrow, the tempfile module
  collapses back into `impl SpillableReorderBuffer` methods, defeating
  the split.
- **`spill_excess` (policy) calls `spill_item` (tempfile) and adjusts
  `memory_used` / `spill_count`** owned by the buffer. Either expose
  these counters on `SpillState`, or have `spill_excess` return a
  `SpillReport { spilled_bytes, spill_events_delta }` that buffer applies.
  The report shape avoids a circular dependency between `policy` and
  `tempfile`.
- **`SpillBackend::file()` returns `&mut dyn ReadWriteSeek`** with a
  private trait. The trait must move into `tempfile.rs` with the enum;
  no other module needs it.
- **`open_backend` is a free function** outside any impl block; it sits
  naturally in `tempfile.rs` next to `SpillBackend`.
- **The `Debug` impl for `SpillableReorderBuffer`** (lines 243-256)
  reads `inner.capacity()`, `memory_used`, `threshold`,
  `inner.buffered_count()`, `spill_index.len()`, `spill_count`,
  `reload_count`, `dir_recreate_count`. Keep it next to the struct in
  `buffer.rs`; it is the natural owner.
- **Doc examples on the struct** (lines 206-217) reference
  `engine::concurrent_delta::spill::SpillableReorderBuffer`. The
  re-export in `spill/mod.rs` must preserve that path verbatim or every
  rustdoc example fails compilation.
- **Module-level `//!` rustdoc** (lines 1-48) belongs in `spill/mod.rs`
  unchanged. Submodules get their own focused `//!` headers.

No item resists splitting outright; the only friction is the shared
mutable state, addressed by the `SpillState` borrow described above.

## Test Split Plan (SPL-8)

Tests are currently a single `#[cfg(test)] mod tests` block spanning
lines 689-1228. After the split they break out as follows:

| Source test (line)                                                | Destination test module      |
|-------------------------------------------------------------------|------------------------------|
| `spill_error_display_and_source` (1220)                           | `spill/error.rs` `#[cfg(test)]` |
| `delta_result_spill_codec_roundtrip` (957)                        | `spill/codec.rs` `#[cfg(test)]` (codec contract via `DeltaResult`) |
| `delta_result_needs_redo_codec_roundtrip` (974)                   | `spill/codec.rs` `#[cfg(test)]` |
| `delta_result_failed_codec_roundtrip` (989)                       | `spill/codec.rs` `#[cfg(test)]` |
| `partial_write_surfaces_as_write_zero` (1077)                     | `spill/codec.rs` `#[cfg(test)]` (exercises `SpillCodec::encode` write-loop contract) |
| `enospc_during_spill_propagates_as_io_error` (1039)               | `spill/tempfile.rs` `#[cfg(test)]` |
| `temp_dir_vanish_recreates_when_no_prior_spills` (1111)           | `spill/tempfile.rs` `#[cfg(test)]` |
| `temp_dir_vanish_after_prior_spills_returns_error` (1142)         | `spill/tempfile.rs` `#[cfg(test)]` |
| `dir_recreate_failure_surfaces_io_error` (1183)                   | `spill/tempfile.rs` `#[cfg(test)]` |
| `directory_backed_spill_round_trip` (1202)                        | `spill/tempfile.rs` `#[cfg(test)]` |
| `exact_threshold_boundary` (858)                                  | `spill/policy.rs` `#[cfg(test)]` |
| `spill_triggers_when_threshold_exceeded` (773)                    | `spill/policy.rs` `#[cfg(test)]` |
| `spill_stats_tracking` (911)                                      | `spill/stats.rs` `#[cfg(test)]` |
| `no_spill_under_threshold` (752)                                  | `spill/buffer.rs` `#[cfg(test)]` |
| `correct_delivery_order_after_spill_and_reload` (795)             | `spill/buffer.rs` `#[cfg(test)]` |
| `cleanup_on_drop` (816)                                           | `spill/buffer.rs` `#[cfg(test)]` |
| `interleaved_spill_and_deliver` (832)                             | `spill/buffer.rs` `#[cfg(test)]` |
| `empty_buffer_operations` (882)                                   | `spill/buffer.rs` `#[cfg(test)]` |
| `force_insert_with_spill` (893)                                   | `spill/buffer.rs` `#[cfg(test)]` |
| `large_scale_spill_and_drain` (935)                               | `spill/buffer.rs` `#[cfg(test)]` |
| `spillable_buffer_with_delta_results` (1004)                      | `spill/buffer.rs` `#[cfg(test)]` |

The two test fixtures (`impl SpillCodec for u64`, `FailingCodec`) plus
the `drain_all` helper are shared across multiple destinations. Move
them into `spill/tests_support.rs` as `#[cfg(test)] pub(super)` items
that each test module re-imports. This avoids duplicating fixtures and
keeps the integration-level tests (`buffer.rs`) honest about what they
exercise.

## Out of Scope

- No new public API. The `spill/mod.rs` re-exports cover everything
  `consumer.rs`, `parallel_apply.rs`, and `strategy.rs` import today.
- No behavioural change to ENOSPC handling, temp-dir recovery, hot zone
  sizing, or memory accounting. Those land in their target modules
  verbatim.
- No change to `concurrent_delta/types.rs`, where the production
  `SpillCodec for DeltaResult` impl lives - it keeps importing from
  `super::spill::SpillCodec`, which now resolves to `spill::codec::SpillCodec`
  via the `pub use` re-export.
