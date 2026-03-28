# Parallel Chunk Transfers - Design Document

**Date:** 2026-03-28
**Status:** Approved
**Scope:** Buffer pool redesign + vstring-negotiated intra-file parallel delta application

## Problem

Single large files are processed sequentially by one rayon worker. A 100 GB file
is transferred by a single thread regardless of available cores. Upstream rsync
has the same limitation - this is parity, not a regression - but it is a ceiling
on single-large-file throughput.

## Solution Overview

Two changes, independently shippable:

1. **Buffer pool redesign** (Phase 0) - Replace `SegQueue + AtomicUsize` with
   `thread_local!` fast path + `Mutex<Vec>` central pool. Zero-sync hot path,
   std-only, cross-platform.

2. **Chunk-aware transfers** (Phases 1-6) - Negotiate a `parallel-chunks-v1`
   capability via vstring exchange (same mechanism as checksums/compression).
   When negotiated, the sender emits chunk envelope tokens (CHUNK_HDR/CHUNK_END)
   around groups of delta tokens. The receiver dispatches chunks to rayon workers
   that apply them in parallel via pwrite/pread.

## Wire Protocol Compatibility

| Component                  | Impact   |
|----------------------------|----------|
| Protocol version           | None (stays 32) |
| Compat flags               | None consumed |
| Token format inside chunks | Identical to current |
| Negotiation mechanism      | New "chunking" vstring list |
| Upstream rsync interop     | Automatic fallback to sequential |

The chunking vstring is exchanged after checksums and compression during
the existing algorithm negotiation phase (protocol >= 30). Old peers that
don't send a chunking list default to `"none"` (sequential). No wire
format changes occur when sequential mode is selected.

## Phase 0: Buffer Pool Redesign

### Current Design

```
BufferPool
  buffers: SegQueue<Vec<u8>>       // lock-free MPMC queue
  pool_len: AtomicUsize            // approximate length counter
  soft_capacity: usize             // TOCTOU race on capacity check
```

### New Design

```
Thread-local fast path (zero sync):
  thread_local! { RefCell<Option<Vec<u8>>> }   // 1 slot per thread

Central pool (rare access):
  buffers: Mutex<Vec<Vec<u8>>>                  // exact capacity check
  soft_capacity: usize                          // enforced under lock
```

**Rationale:** Research across jemalloc, mimalloc, Go sync.Pool, Linux SLUB,
and Netty converges on the same two-level pattern: thread-local fast path +
shared slow path. The thread-local cache absorbs 95%+ of operations, making
lock-free central storage unnecessary. `Mutex<Vec>` is simpler, exact, and
cache-friendly. No external dependencies - std only.

### Acquire Path

```
1. thread_local_cache::try_take()    -> hit: zero sync, ~2 ns
2. buffers.lock().pop()              -> miss: Mutex, ~20 ns
3. allocator.allocate(buffer_size)   -> cold: heap alloc, ~50 ns
```

### Return Path

```
1. thread_local_cache::try_store()   -> slot empty: zero sync
2. buffers.lock().push()             -> slot occupied: Mutex
3. allocator.deallocate()            -> pool at capacity
```

## Phase 1: Chunking Negotiation

Add a third vstring negotiation round after checksums and compression:

```
Client: "parallel-chunks-v1 none"
Server: "parallel-chunks-v1 none"   (oc-rsync)
   or:  (no chunking list)          (upstream rsync)

Result: first mutual match, defaulting to "none"
```

`ChunkingAlgorithm` enum: `None` | `ParallelChunksV1 { chunk_size }`.
Wired through `NegotiatedParams` -> `CoreConfig` -> `TransferConfig`.

## Phase 2: Chunk Envelope Wire Format

When `parallel-chunks-v1` is negotiated, the delta stream gains framing:

```
Sequential (current):
  [DATA] [BLOCK_REF] [DATA] [BLOCK_REF] ... [END] [CHECKSUM]

Chunked (new):
  [CHUNK_HDR offset=0 len=64MB]
    [DATA] [BLOCK_REF] [DATA] ...
  [CHUNK_END chunk_checksum]
  [CHUNK_HDR offset=64MB len=64MB]
    [DATA] [BLOCK_REF] ...
  [CHUNK_END chunk_checksum]
  [END] [WHOLE_FILE_CHECKSUM]
```

- CHUNK_HDR: sentinel i32 + output_offset u64 LE + byte_count u64 LE
- CHUNK_END: sentinel i32 + per-chunk checksum bytes
- Tokens inside chunks: identical encoding to current format
- Per-chunk checksums enable early error detection

## Phase 3: Cross-Platform Positional I/O

```rust
pub trait PositionalWriter {
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize>;
    fn write_all_at(&self, buf: &[u8], offset: u64) -> io::Result<()>;
}

pub trait PositionalReader {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}
```

- Unix: `std::os::unix::fs::FileExt` (wraps pwrite/pread)
- Windows: `std::os::windows::fs::FileExt` (wraps seek_write/seek_read)
- `&self` not `&mut self` - safe for concurrent non-overlapping access

## Phase 4: Parallel Delta Application

Strategy pattern with two implementations:

```rust
pub trait ChunkProcessor {
    fn process_file(&self, tokens: TokenStream, basis: &File,
                    output: &File, file_size: u64) -> Result<()>;
}
```

- `SequentialProcessor`: current token_loop behavior (refactored to trait)
- `ParallelProcessor`: accumulates decoded chunks, dispatches via rayon par_iter

Each chunk worker:
1. Acquires buffer from pool (TLS fast path)
2. Reads basis blocks via PositionalReader
3. Writes output via PositionalWriter at chunk's offset
4. Verifies per-chunk checksum
5. Checks shared abort flag between tokens

Error recovery: any chunk failure sets AtomicBool abort flag, all workers
exit early, temp file discarded, file retried in phase 2 redo with
SequentialProcessor.

## Phase 5: CLI & Activation

```
--parallel-chunks[=SIZE]           Enable chunked transfers (default: 64 MB)
--parallel-chunks-threshold=SIZE   Min file size for parallel (default: 256 MB)
```

- Off by default - upstream parity when not specified
- File size threshold gates activation per-file
- Factory function selects processor: mode + file_size -> Sequential or Parallel

## Phase 6: Correctness & Validation

- Property tests: sequential == parallel output (proptest)
- Interop tests: oc-rsync chunked e2e, upstream fallback
- Benchmarks: throughput scaling, TLS hit rate, memory overhead
- Regression tests: small file throughput unaffected

## Task Decomposition

112 atomic tasks across 7 phases. 4 parallel start points (P0, P1, P3).
See task list for full dependency graph.

## Design Patterns Applied

| Pattern | Usage |
|---------|-------|
| Strategy | ChunkProcessor trait: Sequential vs Parallel |
| Decorator | Thread-local cache wrapping central pool |
| Factory | select_chunk_processor() based on mode + threshold |
| Dependency Inversion | BufferAllocator trait, PositionalWriter/Reader traits |
| RAII | BufferGuard auto-returns buffer on drop |
