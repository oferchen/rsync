# Parallelism Audit - Rayon and Channel Usage

Audit of all rayon parallel iterators, `rayon::scope`/`rayon::join`, `std::sync::mpsc` channels, and crossbeam channels across the codebase.

## Rayon `par_iter` / `into_par_iter` Sites

All `par_iter().map().collect()` chains preserve input order (rayon guarantees positional correspondence). The `fold().reduce()` pattern does NOT preserve order.

| File | Function | Pattern | Ordering | Reorder Downstream | Potential Issues |
|------|----------|---------|----------|-------------------|-----------------|
| `crates/flist/src/parallel.rs:83` | `process_entries_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 positional | None |
| `crates/flist/src/parallel.rs:105` | `filter_entries_indices` | `par_iter().enumerate().filter_map().collect()` | Indices preserved, output order arbitrary | No - returns indices | None |
| `crates/flist/src/parallel.rs:133` | `collect_paths_then_metadata_parallel` | `into_par_iter().map().collect()` | Preserved by collect | Yes - `sort_file_entries()` post-sort for wire order | None |
| `crates/flist/src/parallel.rs:331` | `collect_paths_chunked_parallel` | `par_iter().map().collect()` per chunk | Preserved within each chunk | Yes - `sort_file_entries()` post-sort | None |
| `crates/flist/src/parallel.rs:400` | `resolve_metadata_parallel` | `into_par_iter().map().collect()` | Preserved by collect | Yes - `sort_file_entries()` post-sort | None |
| `crates/flist/src/batched_stat/dir_stat.rs:153` | `DirStatHandle::stat_batch_relative` | `par_iter().map().collect()` | Preserved | No - 1:1 positional | Feature-gated (`parallel`) |
| `crates/flist/src/batched_stat/cache.rs:131` | `BatchedStatCache::stat_batch` | `par_iter().map().collect()` | Preserved | No - 1:1 positional | Feature-gated (`parallel`) |
| `crates/checksums/src/parallel/blocks.rs:117` | `compute_digests_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with blocks | None |
| `crates/checksums/src/parallel/blocks.rs:145` | `compute_digests_with_seed_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with blocks | None |
| `crates/checksums/src/parallel/blocks.rs:169` | `compute_rolling_checksums_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with blocks | None |
| `crates/checksums/src/parallel/blocks.rs:207` | `compute_block_signatures_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with blocks | None |
| `crates/checksums/src/parallel/blocks.rs:248` | `process_blocks_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with blocks | None |
| `crates/checksums/src/parallel/blocks.rs:276` | `filter_blocks_by_checksum` | `par_iter().enumerate().filter_map().collect()` | Indices preserved | No - returns indices | None |
| `crates/checksums/src/parallel/files.rs:163` | `hash_files_parallel_with_config` | `par_iter().map().collect()` | Preserved | No - 1:1 with paths | I/O contention on HDD |
| `crates/checksums/src/parallel/files.rs:197` | `hash_files_with_seed_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with paths | I/O contention on HDD |
| `crates/checksums/src/parallel/files.rs:306` | `compute_file_signatures_parallel` | `par_iter().map().collect()` | Preserved | No - 1:1 with paths | I/O contention on HDD |
| `crates/match/src/index/mod.rs:135` | `find_match_parallel` | `par_iter().find_any()` | N/A - returns first match | No | Feature-gated (`parallel`), non-deterministic match selection when duplicates exist |
| `crates/match/src/index/mod.rs:208` | `find_match_slices_parallel` | `par_iter().find_any()` | N/A - returns first match | No | Feature-gated (`parallel`), non-deterministic match selection when duplicates exist |
| `crates/fast_io/src/cached_sort.rs:120` | `cached_sort_by_parallel` | `par_iter().map().collect()` | Preserved (keys) | Permutation sort applied after | None |
| `crates/fast_io/src/parallel.rs:136` | `ParallelExecutor::process` | `par_iter().fold().reduce()` | **NOT preserved** | No - errors carry index | Callers must not assume positional order of successes |
| `crates/fast_io/src/parallel.rs:191` | `ParallelExecutor::process_files` | `par_iter().fold().reduce()` | **NOT preserved** | No - errors carry index | Callers must not assume positional order of successes |
| `crates/transfer/src/parallel_io.rs:124` | `map_blocking` | `into_par_iter().map().collect()` | Preserved | No - 1:1 positional | Sequential fallback below threshold |
| `crates/transfer/src/receiver/transfer/pipeline.rs:182` | `run_pipeline_loop_decoupled` (signature batch) | `par_iter().map().collect()` | Preserved | No - zipped with batch for sequential send | None |
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:101` | `prefetch_entry_data` | `par_iter().enumerate().map().collect()` | Preserved | No - 1:1 with entries | None |
| `crates/engine/src/local_copy/executor/directory/support.rs:107` | directory metadata fetch | `into_par_iter().map().collect()` | Preserved by collect | Yes - `sort_unstable_by()` post-sort | None |
| `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:95` | `prefetch_checksums` | `par_iter().map().collect()` | Preserved | No - collected into HashMap by source path | I/O contention on HDD |

## `rayon::scope` Sites

| File | Function | Purpose | Ordering | Reorder Downstream | Potential Issues |
|------|----------|---------|----------|-------------------|-----------------|
| `crates/engine/src/concurrent_delta/work_queue.rs:208` | `WorkQueueReceiver::drain_parallel` | Spawns one rayon task per `DeltaWork` item from bounded channel | **Arbitrary** - results land in sharded Vecs by thread index | Caller must use `ReorderBuffer` | Mutex contention on per-shard Vecs (mitigated by N shards) |
| `crates/engine/src/concurrent_delta/work_queue.rs:283` | `WorkQueueReceiver::drain_parallel_into` | Streams results via `SyncSender` as workers complete | **Arbitrary** - completion order | Yes - `ReorderBuffer` in consumer thread | `SyncSender` provides backpressure; silent drop on receiver gone |

## `rayon::join` Sites

| File | Function | Purpose | Ordering | Potential Issues |
|------|----------|---------|----------|-----------------|
| `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:110` | `prefetch_checksums` (inner) | Parallel source + destination file checksums | N/A - two independent results returned as tuple | None |

## `std::sync::mpsc` Channel Sites

### `mpsc::channel` (unbounded)

| File | Function | Purpose | Ordering | Potential Issues |
|------|----------|---------|----------|-----------------|
| `crates/daemon/src/daemon/sections/server_runtime/connection.rs:288` | `run_dual_stack_loop` | Multiplexes accepted TCP connections from N listener threads | Arrival order (non-deterministic) | Unbounded - could grow if accept rate exceeds processing rate; mitigated by OS connection backlog |
| `crates/signature/src/async_gen.rs:219-220` | `AsyncSignatureGenerator::new` | Request channel (single producer to N workers) + result channel (N workers to single consumer) | Results arrive in completion order | Unbounded request channel - producer can outrun workers; request receiver wrapped in `Arc<Mutex>` adds contention |
| `crates/core/src/client/remote/remote_to_remote.rs:265-266` | `run_remote_to_remote` | Two relay thread completion notification channels (s2d, d2s) | N/A - single item per channel | None - each channel carries exactly one `Result` |
| `crates/checksums/src/pipelined/reader.rs:75` | `PipelinedReader::new` | I/O thread sends prefetched blocks to compute thread | Sequential - single producer, FIFO | Unbounded - fast reader could buffer many blocks ahead of slow consumer |
| `crates/checksums/src/pipeline/pipelined.rs:30` | `pipelined_checksum` | I/O worker sends chunks to compute worker | Sequential - single producer, FIFO | Unbounded - same concern as above |
| `crates/engine/src/concurrent_delta/consumer.rs:130` | `DeltaConsumer::spawn` | Reorder thread sends in-order results to consumer | Sequential (reordered) | Unbounded - reorder thread can outrun consumer; mitigated by bounded upstream channel |

### `mpsc::sync_channel` (bounded)

| File | Function | Capacity | Purpose | Ordering | Potential Issues |
|------|----------|----------|---------|----------|-----------------|
| `crates/engine/src/concurrent_delta/work_queue.rs:383` | `bounded_with_capacity` | `2 * num_threads` (default) | Bounded work queue for delta pipeline | FIFO from single producer | Backpressure: producer blocks at capacity - correct by design |
| `crates/engine/src/concurrent_delta/consumer.rs:135` | `DeltaConsumer::spawn` | `max(reorder_capacity, 2 * num_threads)` | Stream channel between rayon workers and reorder thread | Arbitrary (worker completion order) | Backpressure: workers block when reorder thread falls behind - correct by design |

### `tokio::sync::mpsc::channel` (async, bounded)

| File | Function | Capacity | Purpose | Ordering | Potential Issues |
|------|----------|----------|---------|----------|-----------------|
| `crates/transfer/src/pipeline/async_pipeline.rs:164` | `create_pipeline` | Configurable (default clamped) | File job dispatch from producer task to consumer task | Sequential - single async producer | Backpressure via bounded channel; consumer processes sequentially |

## Crossbeam Channel Sites

| File | Function | Type | Purpose | Ordering | Potential Issues |
|------|----------|------|---------|----------|-----------------|
| `crates/transfer/src/pipeline/spsc.rs` | `spsc::channel` | Lock-free SPSC ring buffer (`crossbeam_queue::ArrayQueue`) | Network-to-disk pipeline (`FileMessage` items) | FIFO - single producer, single consumer | **Spin-wait**: both `send` and `recv` spin-loop when full/empty; CPU-intensive under imbalanced producer/consumer rates. Bounded capacity prevents unbounded growth. |

### SPSC Channel Usage Sites

| File | Purpose | Capacity |
|------|---------|----------|
| `crates/transfer/src/disk_commit/thread.rs:43` | File messages: network ingest to disk commit | Configurable |
| `crates/transfer/src/disk_commit/thread.rs:44` | Commit results: disk thread to pipeline | 2x file capacity |
| `crates/transfer/src/disk_commit/thread.rs:45` | Buffer return: disk thread returns reusable buffers | 2x file capacity |

## Summary of Findings

### Ordering Patterns

1. **Order-preserving**: 22 of 25 `par_iter` sites use `par_iter().map().collect()` which preserves input order by rayon's guarantee. These are safe.
2. **Post-sort**: 4 sites (flist metadata collection, directory support) discard order during parallel processing and apply `sort_file_entries()` or `sort_unstable_by()` afterward. Correct and documented.
3. **Unordered by design**: 2 sites use `fold().reduce()` (`ParallelExecutor`). Documented as not preserving order; errors carry original indices.
4. **Non-deterministic match**: 2 sites use `find_any()` for candidate verification. Returns first-found match, which is non-deterministic but acceptable since all candidates are valid matches.
5. **Explicit reorder**: The concurrent delta pipeline (`work_queue` + `consumer`) uses `ReorderBuffer` with sequence numbers to restore wire order after parallel dispatch.

### Potential Issues

1. **Spin-wait CPU usage (SPSC channel)**: The `spsc::channel` in `crates/transfer/src/pipeline/spsc.rs` uses `spin_loop()` for both send and recv. Under producer/consumer rate imbalance, this burns CPU. Acceptable for the high-throughput network-to-disk path where items flow continuously, but would waste CPU if either side stalls for extended periods.

2. **Unbounded `mpsc::channel` growth**: Three sites use unbounded channels where a fast producer could outpace a slow consumer:
   - `AsyncSignatureGenerator` request channel - producer can queue unlimited signature requests.
   - `PipelinedReader` / `pipelined_checksum` - fast I/O thread could buffer many blocks.
   - `DeltaConsumer` result channel - mitigated by bounded upstream `sync_channel`.

3. **I/O contention on parallel file hashing**: File hashing functions (`hash_files_parallel`, `compute_file_signatures_parallel`, `prefetch_checksums`) use rayon's full thread pool for file I/O. On rotational drives (HDD), parallel random reads cause seek storms. Mitigated by `ParallelExecutor`'s configurable `thread_count` for I/O-bound workloads, but the checksum functions do not offer this knob.

4. **Mutex in `AsyncSignatureGenerator`**: The request receiver is wrapped in `Arc<Mutex<Receiver>>` for sharing across worker threads. Under high request rates with many workers, this becomes a contention point since only one worker can dequeue at a time.

5. **`find_any` non-determinism**: `DeltaSignatureIndex::find_match_parallel` and `find_match_slices_parallel` use `find_any()` which returns an arbitrary matching candidate. If multiple blocks share the same rolling + strong checksum (hash collision), different runs may select different blocks. This is functionally correct since any match is valid, but makes debugging harder.

### No Deadlock Risks Identified

- All bounded channels have clear producer/consumer separation.
- The `WorkQueue` SPMC pattern uses `rayon::scope` which guarantees all spawned tasks complete before the scope exits, preventing orphaned senders.
- The `DeltaConsumer` two-thread architecture (drain + reorder) has a clear shutdown chain: drain thread finishes when `WorkQueueSender` drops, reorder thread finishes when drain thread's `SyncSender` drops, consumer iterator finishes when reorder thread's `Sender` drops.
- The `force_insert` fallback in `DeltaConsumer::spawn` (line 165) prevents a potential livelock when the reorder buffer is full but the next-expected item has not arrived yet.
