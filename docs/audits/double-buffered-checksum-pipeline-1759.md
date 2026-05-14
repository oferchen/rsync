# Double-buffered retained checksum pipeline audit

Tracks task #1759. Confirms that the retained checksum pipeline -
the path that reads a basis file and folds it into rolling + strong
checksums - already overlaps I/O with computation through a strict
two-buffer producer-consumer arrangement. No code change is needed;
this note records where the implementation lives, how it satisfies
the double-buffering invariant, and which call sites use it.

## 1. Where the pipeline lives

The double-buffered reader and the checksum loops that drive it
share a single module:

- `crates/checksums/src/pipelined/reader.rs` -
  `DoubleBufferedReader`. Owns the I/O thread and the two
  pre-allocated buffers.
- `crates/checksums/src/pipelined/checksums.rs` -
  `compute_checksums_pipelined()` (batch) and
  `PipelinedChecksumIterator` (streaming) wrap the reader and
  feed each block through rolling + strong hashing.
- `crates/checksums/src/pipelined/config.rs` - `PipelineConfig`
  with `block_size`, `min_file_size`, and an `enabled` switch.
  Defaults: 64 KiB blocks, 256 KiB minimum file size,
  pipelining on.
- `crates/checksums/src/pipelined/mod.rs` - public exports plus
  unit tests and `proptest` properties.

## 2. Two-buffer invariant

`reader.rs` enforces a strict `2 * block_size` memory footprint by
construction:

1. The constructor allocates buffer A on the calling thread and
   reads block 0 into it synchronously, so the first
   `next_block()` returns without crossing a thread boundary.
2. It allocates buffer B and pushes it down the recycle channel
   before spawning the I/O thread. The first `recycle_rx.recv()`
   inside `io_thread_main()` therefore returns immediately and
   block 1 is read while the caller is still hashing block 0.
3. The data channel is `mpsc::sync_channel(1)`, so at most one
   filled block sits in flight between threads.
4. On every subsequent `next_block()` call, the buffer the caller
   just consumed is returned to the I/O thread via the recycle
   channel. The I/O thread blocks on `recycle_rx.recv()` until
   that recycled buffer arrives, so it can never run ahead and
   allocate a third buffer.

The two buffers keep swapping roles until EOF or error. There is
no unbounded read-ahead, no fallback allocation path, and no
heap traffic per block - exactly the pattern the task calls for.

## 3. Pipelining vs. synchronous fallback

`DoubleBufferedReader::with_size_hint()` skips the worker thread
when one of the following holds:

- `config.enabled` is false.
- A size hint is supplied and is below `config.min_file_size`
  (default 256 KiB).
- The first synchronous read returns 0 bytes (empty file).

In those cases the reader degrades to a single reusable buffer
on the calling thread (`sync_buffer`). The synchronous path keeps
the same public API, so callers do not need to branch on size.

## 4. Wiring into the rest of the codebase

The pipelined reader is the single source for "fold a basis file
through rolling + strong checksums" in oc-rsync:

- `crates/signature/src/pipelined_gen.rs::generate_signature_pipelined()`
  drives `DoubleBufferedReader` directly to build a
  `FileSignature`. This is the production retained-checksum
  generator used during delta transfers.
- `crates/checksums/benches/pipelined_benchmark.rs` exercises
  `compute_checksums_pipelined()` and `DoubleBufferedReader`
  in microbenchmarks.
- `crates/checksums/tests/comprehensive_tests.rs` covers the
  public surface end-to-end with the same reader.

`grep -r DoubleBufferedReader\|compute_checksums_pipelined\|PipelinedChecksumIterator`
returns hits only in the module itself, in `signature::pipelined_gen`,
in the comprehensive test crate, and in the benchmark - so any
new caller that wants overlapped I/O for retained checksums can
go through the existing entry points without copying logic.

## 5. Test coverage

`crates/checksums/src/pipelined/mod.rs` and
`crates/checksums/tests/comprehensive_tests.rs` already cover the
two requirements from task #1759:

- **Functional parity with serialized hashing.** Tests such as
  `pipelined_matches_sequential_various_sizes`,
  `compute_checksums_pipelined_matches_sequential`, and the
  `pipelined_equals_sequential` proptest run the same input through
  the pipelined config and through `with_enabled(false)` and assert
  block-by-block equality of `rolling`, `strong`, and `len`.
- **Edge cases.** Empty input, single byte, exact block boundary,
  partial last block, very small block sizes, sync mode with
  buffer reuse, and pipelined mode with 100-block recycling are
  each pinned by dedicated tests.
- **Lifecycle.** `reader_thread_cleanup_on_drop` exercises the
  `Drop` implementation that closes the channels and joins the
  I/O thread.

A timing-based "pipelined completes in <= serial wall time" test
is intentionally absent. Such a test would either need a mocked
reader that injects deterministic sleeps or it would race with
scheduler jitter in CI. The microbenchmark in
`crates/checksums/benches/pipelined_benchmark.rs` carries that
signal in a venue (criterion) where variance can be measured
without flaking CI.

## 6. Conclusion

The retained checksum pipeline has been double-buffered since
the `pipelined` module landed. The constructor pre-allocates
buffer A, seeds buffer B into the recycle channel, and the
`sync_channel(1)` between the I/O thread and the main thread
keeps in-flight data to exactly one block. Functional parity
with the serialized hasher is enforced by both unit and
property tests. Task #1759 needs no code change; future work
on this path should extend `DoubleBufferedReader` rather than
introduce a second pipelining primitive.
