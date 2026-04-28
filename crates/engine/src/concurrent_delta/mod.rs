//! Concurrent delta computation pipeline for parallel file processing.
//!
//! Parallelizes the per-file receive loop that upstream rsync executes
//! sequentially in `receiver.c:recv_files()`. Each file becomes a [`DeltaWork`]
//! item dispatched through a bounded [`work_queue`] to rayon worker threads,
//! producing a [`DeltaResult`] that is reordered for in-sequence delivery.
//!
//! # Components
//!
//! | Submodule | Type | Role |
//! |-----------|------|------|
//! | `types` | [`DeltaWork`], [`DeltaResult`] | Per-file work item (NDX, paths, size) and outcome (stats, redo/fail status) |
//! | [`work_queue`] | `WorkQueueSender` / `WorkQueueReceiver` | Bounded `crossbeam_channel` (2x thread count) with backpressure |
//! | [`strategy`] | [`DeltaStrategy`] trait | Dispatches to [`WholeFileStrategy`] or [`DeltaTransferStrategy`] based on [`DeltaWorkKind`] |
//! | [`reorder`] | [`ReorderBuffer`] | Ring-buffer (`Box<[Option<T>]>`) that yields results in submission order with O(1) insert and drain |
//! | [`consumer`] | [`DeltaConsumer`] | Background thread that drains `WorkQueue` via `drain_parallel` into `ReorderBuffer` for in-order delivery |
//!
//! # Production Pipeline
//!
//! ```text
//! Receiver (producer thread)
//!   |  assigns monotonic sequence number
//!   |  creates DeltaWork (whole-file or delta)
//!   v
//! WorkQueue ──bounded channel──► rayon::scope workers
//!   blocks when full               |
//!   (backpressure to receiver)      |  select_strategy(&work)
//!                                   |  strategy.process(&work)
//!                                   v
//!                              DeltaResult (with sequence stamp)
//!                                   |
//!                                   v
//!                              ReorderBuffer
//!                                   |  drain_ready() yields contiguous run
//!                                   v
//!                              Consumer (in file-list order)
//!                                   - checksum verification
//!                                   - temp-file commit / rename
//!                                   - metadata application
//!                                   - redo collection for phase 2
//! ```
//!
//! The bounded queue caps in-flight items at `2 * rayon::current_num_threads()`,
//! preventing OOM for million-file transfers. The reorder buffer ensures
//! post-processing sees files in the same order as upstream's sequential loop.
//!
//! # Upstream Reference
//!
//! - `receiver.c:recv_files()` - sequential per-file loop this module parallelizes
//! - `receiver.c:receive_data()` - literal vs delta data path (`fd2 == -1` selects whole-file)
//!
//! # Rayon Ordering Audit
//!
//! rsync's wire protocol requires strictly monotonic NDX (file index) ordering.
//! Every `par_iter` site in the codebase was audited for potential ordering
//! violations. Each site is classified as:
//!
//! - **SAFE** - output order is irrelevant (computation-only, no wire output)
//! - **GUARDED** - parallel execution feeds ordered output, but a reordering
//!   mechanism (sort, `ReorderBuffer`, `enumerate`-indexed collect) ensures
//!   correctness
//! - **RISK** - parallel execution could violate monotonic NDX ordering
//!
//! ## GUARDED Sites
//!
//! ### `engine::concurrent_delta` (this module)
//! The core concurrent pipeline. The producer assigns monotonic sequence
//! numbers, rayon workers process files out of order, and `ReorderBuffer`
//! (ring-buffer-backed, `Box<[Option<T>]>`) yields results in submission order
//! before wire output, with O(1) insert and drain. **GUARDED** by
//! `ReorderBuffer`.
//!
//! ### `transfer::receiver::transfer::pipeline` (`pipeline.rs:180`)
//! Parallel signature computation in the pipelined receiver. Results are
//! collected into a `Vec<_>` via `par_iter().map().collect()` which preserves
//! input order (rayon's `IndexedParallelIterator::collect` guarantee). The
//! batch is then iterated sequentially to send signatures in NDX order.
//! **GUARDED** by indexed collect preserving order.
//!
//! ### `transfer::parallel_io` (`parallel_io.rs:121`)
//! Generic `map_blocking` helper using `into_par_iter().map(f).collect()`.
//! Rayon's indexed parallel iterator preserves input order in the collected
//! `Vec`. Used for stat/chmod/chown batches where results map 1:1 back to
//! the input file list. **GUARDED** by indexed collect preserving order.
//!
//! ### `signature::parallel` (`parallel.rs:137`)
//! Parallel signature block computation via `par_chunks().enumerate()
//! .flat_map_iter()`. Each chunk tracks its `base_index` from `enumerate`,
//! and blocks within each chunk are produced in order via `flat_map_iter`
//! (not `flat_map`). The final `collect()` assembles blocks in correct
//! index order. **GUARDED** by chunk-indexed enumeration.
//!
//! ### `flist::parallel` - metadata functions (`parallel.rs:80,102,128,323,389`)
//! Five `par_iter` sites for metadata fetching (`stat` syscalls). All
//! collect into `Vec` and then call `sort_file_entries()` before returning,
//! or use `par_iter().map(f).collect()` which preserves input order.
//! File list ordering is established by the post-collection sort, not by
//! iteration order. **GUARDED** by post-collection sort or indexed collect.
//!
//! ### `flist::batched_stat::cache` (`cache.rs:128`)
//! `BatchedStatCache::stat_batch` parallelizes stat syscalls. Returns
//! results via `par_iter().map().collect()` preserving input order.
//! **GUARDED** by indexed collect preserving order.
//!
//! ### `flist::batched_stat::dir_stat` (`dir_stat.rs:150`)
//! `DirStatHandle::stat_batch_relative` parallelizes per-directory stat
//! calls. Returns via `par_iter().map().collect()` preserving input order.
//! **GUARDED** by indexed collect preserving order.
//!
//! ### `engine::local_copy::executor::directory::support` (`support.rs:105`)
//! Parallel metadata fetching for directory entries via `into_par_iter()
//! .map().collect()`. Results are sorted by filename after collection
//! (`sort_unstable_by`). **GUARDED** by post-collection sort.
//!
//! ## SAFE Sites
//!
//! ### `match::index` (`index/mod.rs:135,208`)
//! `find_any` on candidate block indices during delta matching. Returns
//! the first matching block index found by any thread. Order of candidate
//! evaluation does not matter - any match is equally valid. Output is a
//! single `Option<usize>` block index, not a sequence. **SAFE**.
//!
//! ### `engine::local_copy::executor::directory::parallel_planner` (`parallel_planner.rs:99`)
//! Parallel prefetch of symlink targets and device metadata. Results are
//! collected into a `Vec` indexed by `enumerate()`, used only for local
//! file operations (not wire protocol). **SAFE**.
//!
//! ### `engine::local_copy::executor::directory::parallel_checksum` (`parallel_checksum.rs:95`)
//! Parallel checksum computation for local copy quick-check. Results go
//! into a `HashMap` keyed by path. No wire protocol involvement - purely
//! local comparison logic. **SAFE**.
//!
//! ### `checksums::parallel::blocks` (`blocks.rs:114,142,166,201,242,270`)
//! Six `par_iter` sites computing rolling checksums, strong digests, and
//! block signatures from in-memory data blocks. All return `Vec` via
//! indexed `par_iter().map().collect()` preserving input order. These are
//! pure computation functions with no direct wire protocol interaction.
//! **SAFE**.
//!
//! ### `checksums::parallel::files` (`files.rs:160,194,300`)
//! Three `par_iter` sites hashing files in parallel. Return `Vec` via
//! indexed `par_iter().map().collect()` preserving input order. Pure
//! computation with no direct wire protocol interaction. **SAFE**.
//!
//! ### `fast_io::parallel` (`parallel.rs:133,186`)
//! Generic parallel file processing (`process` and `process_files`). Uses
//! `fold/reduce` which does not preserve order, but results are returned
//! as unordered success/error lists for batch reporting - not used for
//! wire protocol sequencing. **SAFE**.
//!
//! ### `fast_io::cached_sort` (`cached_sort.rs:118`)
//! Parallel key extraction for cached sorting. Keys are collected via
//! indexed `par_iter().map().collect()`, then used to build a permutation
//! applied in-place. Sorting utility with no wire protocol involvement.
//! **SAFE**.
//!
//! ### `engine::benches::parallel_checksum` (`parallel_checksum.rs:52`)
//! Benchmark-only code. Not part of the production binary. **SAFE**.
//!
//! ## RISK Sites
//!
//! None identified. All `par_iter` usage either operates on data that
//! does not feed into wire protocol ordering, or has an explicit
//! reordering mechanism (sort, `ReorderBuffer`, or rayon's indexed
//! collect guarantee) that restores monotonic order before wire output.
//!
//! # See Also
//!
//! - [`crate::delta`] for block-matching primitives
//! - `transfer::pipeline` for the pipelined receiver architecture

pub mod adaptive;
pub mod consumer;
#[cfg(test)]
mod multi_producer_audit;
pub mod reorder;
pub mod strategy;
mod types;
pub mod work_queue;

pub use adaptive::{AdaptiveCapacityPolicy, ReorderStats};
pub use consumer::DeltaConsumer;
pub use reorder::ReorderBuffer;
pub use strategy::{DeltaStrategy, DeltaTransferStrategy, WholeFileStrategy};
pub use types::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind, FileNdx};
